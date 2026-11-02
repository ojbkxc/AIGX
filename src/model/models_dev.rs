//! models.dev 模型元数据补齐（对照 Agent2API `core/model_meta`）。
//!
//! ── 它补什么 ────────────────────────────────────────────────
//! AIGX 的模型元信息（上下文长度 / 能力标签）走三层：管理员覆盖 >
//! 内置推断（`metadata.rs` 的静态表）。内置表只收录主流型号，长尾模型
//! 两个字段都是空。models.dev（<https://models.dev>）是社区维护的模型
//! 能力声明目录，作为**第三层只填空位**：管理员覆盖与内置推断已声明的
//! 值绝不改写。语义与上游一致（用户 > 上游声明 > models.dev > 未声明）。
//!
//! ── 数据来源与降级 ──────────────────────────────────────────
//! - 内嵌快照 `modelsdev.json`（编译期 `include_str!`，与上游同源文件）：
//!   离线部署 / 首次启动的保底目录。
//! - 联网刷新拉 `https://models.dev/models.json`（canonical 目录），
//!   解析成功后**覆盖内存索引 + 落 KV**（`model.modelsdev_catalog`）。
//!   失败保留旧值（KV 里的上一版仍可用），绝不因网络抖动退回空目录。
//! - 与上游的差异：上游还拉 api.json 取 reasoning effort 档位；AIGX 的
//!   ModelMetadata 没有档位字段，不拉（少一个网络依赖）。
//! - 定时刷新挂 cron（24h 一轮，见 main.rs 的 modelsdev-refresh）；
//!   手动刷新走 `POST /api/models/metadata/refresh`，5 秒内合并重复点击
//!   （对齐上游 MANUAL 间隔下限）。
//!
//! 开关 `usage.models_dev_enabled` 默认**开**：关 = 不再联网刷新，
//!   已装载目录也不再参与补值（登记处检查）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

use chrono::Utc;
use parking_lot::RwLock;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::FileStore;

/// canonical 模型目录（models.dev 公开数据）。
pub const MODELS_DEV_URL: &str = "https://models.dev/models.json";
/// 刷新后的目录在 KV 里的存储键。
const KV_CATALOG: &str = "model.modelsdev_catalog";
/// 内嵌保底快照（来源同 `source` 字段声明的 https://models.dev）。
const EMBEDDED_SNAPSHOT: &str = include_str!("modelsdev.json");
/// 手动刷新最小间隔（毫秒）：连点合并，与上游同款下限。
const MIN_REFRESH_INTERVAL_MS: i64 = 5_000;

/// 单模型的能力声明（字段与内嵌快照逐字对应，camelCase）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DevEntry {
    /// 规范名（provider/model 形态，如 `aion-labs/aion-2.0`）
    pub canonical: Option<String>,
    /// 是否支持思考（reasoning 模型）
    pub reasoning: bool,
    /// 是否支持工具调用
    pub tool_call: bool,
    /// 输入模态含图片
    pub images: bool,
    /// 输入模态含视频
    pub video: bool,
    /// 上下文窗口（token）
    pub context: Option<u64>,
    /// 最大输出（token）
    pub output: Option<u64>,
}

/// 目录：索引键（规范化模型名）→ 能力声明。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Catalog {
    models: HashMap<String, DevEntry>,
    /// 目录生成时刻（毫秒；内嵌快照带原值，联网刷新覆盖为拉取时刻）
    generated_at: Option<i64>,
    /// 来源标识
    source: String,
}

static CATALOG: OnceLock<RwLock<Catalog>> = OnceLock::new();

fn catalog() -> &'static RwLock<Catalog> {
    CATALOG.get_or_init(|| RwLock::new(Catalog::default()))
}

/// 上次成功/尝试刷新（毫秒时间戳），供手动刷新合并连点。
static LAST_REFRESH_AT_MS: AtomicI64 = AtomicI64::new(0);

/// 索引键：取模型名最后一段（截掉 provider 前缀 `/` 与 `:`），
/// 只留 ASCII 字母数字并转小写 —— 与上游 `snapshot.rs::key_of` 同口径。
pub fn normalize_key(model: &str) -> String {
    let last = model.rsplit(['/', ':']).next().unwrap_or(model);
    last.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// 按模型名查 models.dev 声明（补值调用方只在「未声明」时采用）。
pub fn lookup(model: &str) -> Option<DevEntry> {
    let key = normalize_key(model);
    if key.is_empty() {
        return None;
    }
    catalog().read().models.get(&key).cloned()
}

/// 启动装载：KV 缓存优先，缺省回落内嵌快照。返回（来源, 条目数）。
pub fn init(store: &FileStore) -> (&'static str, usize) {
    let cached: Option<Catalog> = store.get::<Catalog>(KV_CATALOG).ok().flatten();
    let (loaded, source) = match cached {
        Some(cached) if !cached.models.is_empty() => (cached, "cache"),
        _ => (
            parse_snapshot(EMBEDDED_SNAPSHOT).unwrap_or_default(),
            "embedded",
        ),
    };
    let count = loaded.models.len();
    // 局部名不能叫 `catalog`：会把同名函数遮蔽掉（E0618）
    *catalog().write() = loaded;
    (source, count)
}

/// 目录状态（管理端展示：更新时间 / 条目数 / 来源）。
pub fn stats() -> (Option<i64>, usize, String) {
    let guard = catalog().read();
    (guard.generated_at, guard.models.len(), guard.source.clone())
}

/// 内嵌快照的 `{generatedAt, source, models:{...}}` 结构解析。
fn parse_snapshot(raw: &str) -> Option<Catalog> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let generated_at = value.get("generatedAt").and_then(Value::as_i64);
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("embedded")
        .to_string();
    let entries = value.get("models")?.as_object()?;
    let mut models = HashMap::with_capacity(entries.len());
    for (key, entry) in entries {
        if let Ok(dev) = serde_json::from_value::<DevEntry>(entry.clone()) {
            models.insert(key.clone(), dev);
        }
    }
    Some(Catalog {
        models,
        generated_at,
        source,
    })
}

/// models.json 的单条目 → 内部条目：只收 chat 且文本输出的模型。
fn reduce_entry(entry: &Value) -> Option<DevEntry> {
    let object = entry.as_object()?;
    let kind = object.get("type").and_then(Value::as_str).unwrap_or("chat");
    if kind != "chat" {
        return None;
    }
    let modalities = object.get("modalities");
    let outputs: Vec<&str> = modalities
        .and_then(|m| m.get("output"))
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !outputs.is_empty() && !outputs.contains(&"text") {
        return None;
    }
    let limit = object.get("limit").and_then(Value::as_object);
    let context = limit
        .and_then(|l| l.get("context"))
        .and_then(Value::as_u64)
        .filter(|n| *n >= 1);
    let output = limit
        .and_then(|l| l.get("output"))
        .and_then(Value::as_u64)
        .filter(|n| *n >= 1);
    // 两个数值都没有的条目没有补值意义，直接不收
    if context.is_none() && output.is_none() {
        return None;
    }
    let inputs: Vec<&str> = modalities
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    Some(DevEntry {
        canonical: object.get("id").and_then(Value::as_str).map(String::from),
        reasoning: object
            .get("reasoning")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        tool_call: object
            .get("tool_call")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        images: inputs.contains(&"image"),
        video: inputs.contains(&"video"),
        context,
        output,
    })
}

/// 联网拉取 models.dev 并重建索引 + 落 KV。
/// 失败返回错误且**不动现有目录**（保留上一版）。
pub async fn refresh(client: &Client, store: &FileStore) -> Result<usize, String> {
    let response = client
        .get(MODELS_DEV_URL)
        .send()
        .await
        .map_err(|e| format!("请求失败：{e}"))?;
    if !response.status().is_success() {
        return Err(format!("models.dev 返回 HTTP {}", response.status()));
    }
    let text = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;
    let value: Value = serde_json::from_str(&text).map_err(|e| format!("JSON 解析失败：{e}"))?;
    let entries = value
        .as_object()
        .ok_or_else(|| "models.json 顶层结构异常（应为对象）".to_string())?;
    let mut models = HashMap::with_capacity(entries.len());
    for (id, entry) in entries {
        let Some(dev) = reduce_entry(entry) else {
            continue;
        };
        let key = normalize_key(id);
        if !key.is_empty() {
            models.insert(key, dev);
        }
    }
    if models.is_empty() {
        return Err("未解析出任何可用模型条目".to_string());
    }
    // 局部名不能叫 `catalog`：会把同名函数遮蔽掉（E0618）
    let fresh = Catalog {
        models,
        generated_at: Some(Utc::now().timestamp_millis()),
        source: "https://models.dev".to_string(),
    };
    let count = fresh.models.len();
    // KV 落盘尽力而为：写失败内存仍生效（下次启动回落内嵌快照）
    let _ = store.put(KV_CATALOG, &fresh);
    *catalog().write() = fresh;
    Ok(count)
}

/// 手动刷新入口（管理端 POST）：带 5 秒合并窗；返回 `(status, message, count)`。
pub async fn manual_refresh(client: &Client, store: &FileStore) -> (String, Option<String>, usize) {
    let now = Utc::now().timestamp_millis();
    let last = LAST_REFRESH_AT_MS.swap(now, Ordering::Relaxed);
    if now - last < MIN_REFRESH_INTERVAL_MS {
        return (
            "skipped".to_string(),
            Some("刷新过于频繁，已合并".to_string()),
            0,
        );
    }
    match refresh(client, store).await {
        Ok(count) => ("refreshed".to_string(), None, count),
        Err(message) => ("failed".to_string(), Some(message), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_key_matches_upstream_convention() {
        assert_eq!(normalize_key("openai/gpt-4o"), "gpt4o");
        assert_eq!(normalize_key("aion-labs/aion-2.0"), "aion20");
        assert_eq!(normalize_key("GPT-4.1-mini"), "gpt41mini");
        assert_eq!(normalize_key("provider:model:name"), "name");
        assert_eq!(normalize_key("///"), "");
    }

    #[test]
    fn embedded_snapshot_parses_with_real_entries() {
        let catalog = parse_snapshot(EMBEDDED_SNAPSHOT).expect("内嵌快照必须可解析");
        assert!(catalog.models.len() > 400, "快照应有数百条");
        let entry = catalog.models.get("aion20").expect("aion20 应在内嵌快照中");
        assert_eq!(entry.context, Some(131072));
        assert!(entry.reasoning);
        assert!(entry.tool_call);
    }

    #[test]
    fn reduce_entry_accepts_only_chat_text_models() {
        let chat = serde_json::json!({
            "id": "openai/gpt-4o",
            "type": "chat",
            "reasoning": false,
            "tool_call": true,
            "limit": { "context": 128000, "output": 16384 },
            "modalities": { "input": ["text", "image"], "output": ["text"] },
        });
        let dev = reduce_entry(&chat).expect("chat+text 应收录");
        assert_eq!(dev.context, Some(128000));
        assert!(dev.tool_call);
        assert!(dev.images);
        assert!(!dev.video);

        let embedding = serde_json::json!({ "type": "embedding", "limit": { "context": 8 } });
        assert!(reduce_entry(&embedding).is_none());
        let audio_out = serde_json::json!({
            "type": "chat",
            "limit": { "context": 8 },
            "modalities": { "output": ["audio"] },
        });
        assert!(reduce_entry(&audio_out).is_none());
        let no_limit = serde_json::json!({ "type": "chat" });
        assert!(reduce_entry(&no_limit).is_none());
    }
}
