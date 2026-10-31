//! 账号池「按账号自动发现上游模型目录」。
//!
//! ## 为什么需要这一层
//!
//! 改造前账号池的模型只能手填（`AccountSession.models`）。管理员加一个账号
//! 时必须自己知道上游支持哪些模型 —— 抄 Agent2API 的做法，每家供应商的目录
//! 都能从上游拉回来，手填只是离线时的兜底。
//!
//! ## 与 Agent2API 的对应关系
//!
//! A2A 在 `core/providers/<家>/models.rs` 里给每家实现目录（静态兜底表 +
//! 远程刷新），由 `core/providers/catalog_cache.rs` 落盘缓存、
//! `core/providers/catalog_refresh.rs` 做并发闸门。本模块是同一套设计的
//! 精简版：
//!
//! - **静态兜底表** —— 从 A2A 各家 `models.rs` 移植（纯数据，见
//!   [`fallback`]）。断网 / 未登录时目录不至于空着。
//! - **远程刷新** —— 每家一个 [`CatalogSpec`]：目录路径 + 响应解析口径。
//!   与 A2A 不同的是这里不重复实现各家的签名逻辑：目录端点绝大多数只需
//!   带上 provider 默认头 + 账号凭证（`registry::SessionMeta` 已经描述了
//!   头部模板），因此统一走 [`fetch_remote`]。
//! - **缓存** —— 按**账号**粒度落盘（`model_catalog:{provider}:{account_id}`），
//!   比 A2A 的 provider 粒度更细：同一家不同账号的套餐不同、可用模型可能不同。
//!
//! ## 口径原则
//!
//! **不编造模型名**。上游拉不到就如实报失败并保留旧缓存；静态兜底表里
//! 没有的模型不会凭空出现。表外供应商一律「不支持」，管理面据此不显示
//! 「同步模型」按钮，而不是给一个必然失败的入口。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::AccountSession;
use super::registry;
use super::store::AccountSessionStore;

/// 目录缓存的落盘键前缀（按 provider + 账号 id 分格）。
const CACHE_PREFIX: &str = "model_catalog:";

/// 远程目录缓存有效期（秒）。10 分钟 —— 与 Agent2API 的自动路径同档。
const CACHE_TTL_SECS: i64 = 600;

/// 目录请求超时（秒）。目录是短响应，20 秒足够。
const REQUEST_TIMEOUT_SECS: u64 = 20;

/// 目录条目里模型 id 的来源字段。
///
/// 不同上游的命名习惯不同：OpenAI 形态是 `id`，Agent2API 统一成 `id`，
/// 但也有直接用 `name` 的。这里按顺序取第一个非空值。
fn pick_id(value: &Value) -> Option<String> {
    for key in ["id", "name", "model", "model_name", "modelId"] {
        if let Some(s) = value.get(key).and_then(Value::as_str) {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// 目录条目里上下文长度的来源字段。
fn pick_context_length(value: &Value) -> Option<u32> {
    for key in [
        "maxInputTokens",
        "max_input_tokens",
        "context_length",
        "contextLength",
        "max_context_length",
        "context_window",
    ] {
        if let Some(n) = value.get(key).and_then(Value::as_u64) {
            if n > 0 && n <= u32::MAX as u64 {
                return Some(n as u32);
            }
        }
    }
    None
}

/// 目录条目里能力位的来源字段（各家键名不同，统一归一成标签数组）。
///
/// 与 `model::metadata::infer_capabilities` 的标签词表对齐（`vision` /
/// `tool` / `reasoning` / `embedding` / `audio` / `image`），这样模型管理页
/// 的能力徽章对「上游拉来的模型」和「内置推断的模型」是同一套渲染。
fn pick_capabilities(value: &Value) -> Vec<String> {
    let mut caps: Vec<String> = Vec::new();

    // 上游可能直接给字符串数组
    if let Some(arr) = value.get("capabilities").and_then(Value::as_array) {
        for item in arr {
            if let Some(s) = item.as_str() {
                let s = s.trim();
                if !s.is_empty() && !caps.iter().any(|c| c.eq_ignore_ascii_case(s)) {
                    caps.push(s.to_string());
                }
            }
        }
    }

    // 布尔位
    let flags: &[(&str, &str)] = &[
        ("supportsImages", "vision"),
        ("supports_image", "vision"),
        ("supportsVision", "vision"),
        ("supportsToolCall", "tool"),
        ("supports_tool_call", "tool"),
        ("supportsTools", "tool"),
        ("supportsReasoning", "reasoning"),
        ("supports_reasoning", "reasoning"),
        ("thinking", "reasoning"),
        ("supportsVideo", "video"),
        ("supportsAudio", "audio"),
    ];
    for (key, label) in flags {
        if value.get(*key).and_then(Value::as_bool).unwrap_or(false)
            && !caps.iter().any(|c| c.eq_ignore_ascii_case(label))
        {
            caps.push((*label).to_string());
        }
    }

    caps
}

/// 一条目录条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// 模型 id（客户端可见名）。
    pub id: String,
    /// 上游真名（与 id 不同则为别名，路由时用它）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// 上下文长度（上游没给则 None）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u32>,
    /// 能力位标签。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// 一份目录快照。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CatalogSnapshot {
    /// 条目。
    #[serde(default)]
    pub models: Vec<CatalogEntry>,
    /// 拉取时刻（unix 秒）。
    #[serde(default)]
    pub fetched_at: i64,
    /// 来源：`remote`（上游拉回）/ `fallback`（静态兜底）/ `cache`（命中缓存）。
    #[serde(default)]
    pub source: String,
}

/// 一个供应商的目录端点描述。
struct CatalogSpec {
    /// 相对 base_url 的目录路径（空 = 该家只有静态兜底）。
    path: &'static str,
    /// 响应是否是 OpenAI 形态 `{data:[...]}`（false = 需按家解析）。
    openai_shape: bool,
    /// 静态兜底表（模型 id 列表）。
    fallback: &'static [&'static str],
}

/// 已接入目录发现的供应商。
///
/// 路径与兜底表移植自 Agent2API 对应 provider 的 `models.rs`（MIT 许可，
/// 见 `Agent2API-main/LICENSE`）。**兜底表只收模型 id**，能力位与上下文长度
/// 不硬编码 —— 上游拉回来时才有这些信息。
fn spec_for(provider: &str) -> Option<CatalogSpec> {
    match provider {
        "loomy" => Some(CatalogSpec {
            path: "/api/v1/models",
            openai_shape: true,
            fallback: &["spark-x", "spark-x2.5"],
        }),
        "monkeycode" | "monkeycode-intl" => Some(CatalogSpec {
            path: "/api/v1/users/models",
            openai_shape: false,
            fallback: &[],
        }),
        "commandcode" => Some(CatalogSpec {
            path: "/provider/v1/models",
            openai_shape: true,
            fallback: &[],
        }),
        "catpaw" => Some(CatalogSpec {
            path: "/api/agent/maas/model-types",
            openai_shape: false,
            fallback: &[],
        }),
        "codearts" => Some(CatalogSpec {
            path: "/v1/model/builtin",
            openai_shape: false,
            fallback: &[],
        }),
        "officeace" => Some(CatalogSpec {
            path: "/v1/models",
            openai_shape: true,
            fallback: &[],
        }),
        _ => None,
    }
}

/// 该供应商是否支持目录发现（管理面据此决定是否显示「同步模型」）。
pub fn supports(provider: &str) -> bool {
    spec_for(provider).is_some()
}

/// 已支持目录发现的供应商清单（管理面下拉用）。
pub fn supported_providers() -> Vec<&'static str> {
    vec![
        "catpaw",
        "codearts",
        "commandcode",
        "loomy",
        "monkeycode",
        "monkeycode-intl",
        "officeace",
    ]
}

/// 缓存键：按 provider + 账号 id 分格。
fn cache_key(provider: &str, account_id: &str) -> String {
    format!("{CACHE_PREFIX}{provider}:{account_id}")
}

/// 读缓存（不存在或已过期返回 None）。
fn load_cache(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
) -> Option<CatalogSnapshot> {
    let snap: CatalogSnapshot = sessions
        .store()
        .get(&cache_key(provider, account_id))
        .ok()
        .flatten()?;
    if snap.fetched_at > 0 && now_secs() - snap.fetched_at < CACHE_TTL_SECS {
        Some(snap)
    } else {
        None
    }
}

/// 写缓存。
fn save_cache(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
    snapshot: &CatalogSnapshot,
) {
    // 缓存写失败不影响本次结果 —— 目录是可重建的展示数据
    let _ = sessions
        .store()
        .put(&cache_key(provider, account_id), snapshot);
}

/// 静态兜底目录。
fn fallback_catalog(spec: &CatalogSpec) -> CatalogSnapshot {
    CatalogSnapshot {
        models: spec
            .fallback
            .iter()
            .map(|id| CatalogEntry {
                id: (*id).to_string(),
                upstream: None,
                context_length: None,
                capabilities: Vec::new(),
            })
            .collect(),
        fetched_at: 0,
        source: "fallback".to_string(),
    }
}

/// 当前 unix 秒。
fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 把上游响应解析成目录条目。
fn parse_response(value: &Value, openai_shape: bool) -> Vec<CatalogEntry> {
    // OpenAI 形态：{data:[{id,...}]}
    let items: Vec<&Value> = if openai_shape {
        value
            .get("data")
            .and_then(Value::as_array)
            .map(|a| a.iter().collect())
            .unwrap_or_default()
    } else {
        // 常见私有形态：{models:[...]} / {data:[...]} / 顶层数组
        if let Some(arr) = value.get("models").and_then(Value::as_array) {
            arr.iter().collect()
        } else if let Some(arr) = value.get("data").and_then(Value::as_array) {
            arr.iter().collect()
        } else if let Some(arr) = value.as_array() {
            arr.iter().collect()
        } else {
            Vec::new()
        }
    };

    let mut out: Vec<CatalogEntry> = Vec::with_capacity(items.len());
    let mut seen: HashMap<String, ()> = HashMap::new();
    for item in items {
        // 部分上游把条目包在 {model: {...}} 里
        let entry_value = item.get("model").filter(|v| v.is_object()).unwrap_or(item);
        let Some(id) = pick_id(entry_value) else {
            continue;
        };
        // 同 id 去重（大小写不敏感），保留首条
        if seen.insert(id.to_lowercase(), ()).is_some() {
            continue;
        }
        let upstream = entry_value
            .get("upstream_name")
            .or_else(|| entry_value.get("upstreamKey"))
            .or_else(|| entry_value.get("config_name"))
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .filter(|s| s != &id);
        out.push(CatalogEntry {
            id,
            upstream,
            context_length: pick_context_length(entry_value),
            capabilities: pick_capabilities(entry_value),
        });
    }
    out
}

/// 拉取上游目录。
///
/// 目录端点绝大多数只需要 provider 默认头 + 账号凭证，因此统一走
/// `registry::SessionMeta.headers` 模板（`{credential}` 已替换）与
/// `extra_headers` 合并。**不做逐家签名**：需要签名的目录端点（如 Qoder 的
/// COSY 签名）不在本表内 —— 宁可报失败也不发一个必然被拒的请求。
async fn fetch_remote(
    sessions: &AccountSessionStore,
    session: &AccountSession,
    spec: &CatalogSpec,
) -> Result<Vec<CatalogEntry>, String> {
    let base = session.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("账号未配置上游地址".to_string());
    }
    let url = format!("{base}{}", spec.path);

    let client = sessions
        .client_for(session)
        .map_err(|e| format!("出口客户端不可用：{e}"))?;

    // 凭证落盘是 `enc:` 编码的，取明文用 `secret_plain()`。
    let secret = session.credential.secret_plain();

    let mut req = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));

    // provider 默认头 + 账号级额外头
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(meta) = registry::session_meta(&session.provider) {
        for (name, template) in meta.headers {
            let value = template.replace("{credential}", &secret);
            headers.push(((*name).to_string(), value));
        }
    }
    for (name, value) in &session.extra_headers {
        let value = value.replace("{credential}", &secret);
        headers.push((name.clone(), value));
    }
    for (name, value) in headers {
        req = req.header(name, value);
    }
    // 没有默认头的供应商至少带上 Bearer
    if !secret.is_empty()
        && !headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("authorization"))
    {
        req = req.header("Authorization", format!("Bearer {secret}"));
    }

    let resp = req.send().await.map_err(|e| format!("请求失败：{e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("上游返回 {status}"));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("响应不是合法 JSON：{e}"))?;
    Ok(parse_response(&value, spec.openai_shape))
}

/// 同步某个账号的模型目录。
///
/// 语义：
/// - 命中未过期缓存 → 直接返回（`source = "cache"`）；
/// - `force` 为真 → 跳过缓存，强制回源；
/// - 回源失败 → 保留并返回旧缓存（`source = "cache"`）；旧缓存也没有时，
///   退到静态兜底表（`source = "fallback"`）；
/// - 该供应商不支持目录发现 → 返回错误（调用方据此禁用按钮）。
pub async fn sync_account_catalog(
    sessions: &AccountSessionStore,
    session: &AccountSession,
    force: bool,
) -> Result<CatalogSnapshot, String> {
    let Some(spec) = spec_for(&session.provider) else {
        return Err(format!("供应商 {} 不支持模型目录发现", session.provider));
    };

    if !force {
        if let Some(cached) = load_cache(sessions, &session.provider, &session.id) {
            let mut snap = cached;
            snap.source = "cache".to_string();
            return Ok(snap);
        }
    }

    match fetch_remote(sessions, session, &spec).await {
        Ok(models) => {
            let snapshot = CatalogSnapshot {
                models,
                fetched_at: now_secs(),
                source: "remote".to_string(),
            };
            save_cache(sessions, &session.provider, &session.id, &snapshot);
            Ok(snapshot)
        }
        Err(err) => {
            // 回源失败：优先用旧缓存（哪怕过期），再退到静态兜底
            if let Some(old) = sessions
                .store()
                .get::<CatalogSnapshot>(&cache_key(&session.provider, &session.id))
                .ok()
                .flatten()
            {
                let mut snap = old;
                snap.source = "cache".to_string();
                return Ok(snap);
            }
            let fallback = fallback_catalog(&spec);
            if fallback.models.is_empty() {
                return Err(err);
            }
            Ok(fallback)
        }
    }
}

/// 把目录条目写回账号的模型白名单。
///
/// `replace = true` 时覆盖为目录结果；否则只**追加**目录里新增的模型
/// （保留管理员手填的模型）。禁用状态与空白名单都会让写入变成 no-op。
pub fn apply_catalog_to_session(
    sessions: &AccountSessionStore,
    session_id: &str,
    models: &[String],
    replace: bool,
) -> Result<(), String> {
    let Some(session) = sessions.get(session_id) else {
        return Err(format!("账号 {session_id} 不存在"));
    };
    // 运行态才是准的：凭证过期的账号（runtime_status = expired）同样不该写入
    if session.runtime_status(now_secs()) != "enabled" {
        return Err("账号未处于启用状态，无法写入模型清单".to_string());
    }
    let mut next: Vec<String> = if replace {
        Vec::new()
    } else {
        session.models.clone()
    };
    for m in models {
        let m = m.trim();
        if m.is_empty() {
            continue;
        }
        if !next.iter().any(|x| x == m) {
            next.push(m.to_string());
        }
    }
    if next == session.models {
        return Ok(());
    }
    let mut updated = session.clone();
    updated.models = next;
    sessions
        .upsert(updated)
        .map_err(|e| format!("写入账号失败：{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_id_variants() {
        let v = serde_json::json!({"id": "gpt-4o"});
        assert_eq!(pick_id(&v), Some("gpt-4o".to_string()));
        let v = serde_json::json!({"name": "spark-x"});
        assert_eq!(pick_id(&v), Some("spark-x".to_string()));
        let v = serde_json::json!({"nothing": 1});
        assert_eq!(pick_id(&v), None);
    }

    #[test]
    fn parse_openai_shape() {
        let v = serde_json::json!({
            "data": [
                {"id": "a", "max_input_tokens": 128000, "supportsImages": true},
                {"id": "a"},
                {"id": "b", "capabilities": ["reasoning"]}
            ]
        });
        let entries = parse_response(&v, true);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "a");
        assert_eq!(entries[0].context_length, Some(128000));
        assert!(entries[0].capabilities.iter().any(|c| c == "vision"));
        assert!(entries[1].capabilities.iter().any(|c| c == "reasoning"));
    }

    #[test]
    fn parse_private_shape() {
        let v = serde_json::json!({"models": [{"name": "x"}, {"model": {"id": "y"}}]});
        let entries = parse_response(&v, false);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].id, "y");
    }

    #[test]
    fn supported_providers_are_consistent() {
        for kind in supported_providers() {
            assert!(supports(kind), "{kind} 在清单里但 spec_for 不认");
        }
    }
}
