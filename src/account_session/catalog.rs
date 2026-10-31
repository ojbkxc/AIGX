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
    /// 静态兜底表。
    fallback: &'static [FallbackModel],
    /// 目录端点的鉴权/解析方式（None = 用 provider 默认头 + 账号凭证）。
    sign: Option<CatalogSign>,
}

/// 目录端点的特殊鉴权与解析方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogSign {
    /// COSY 签名（Qoder）：请求头要用账号身份加密签名，与转发链路同一套。
    Cosy,
}

/// 静态兜底表的一行。
///
/// 字段与 Agent2API 各家 `models.rs` 的兜底表**逐条对应**（见该文件的
/// 模块头）：`id` 是客户端可见名，`upstream_key` 是上游真名（路由时用），
/// `reasoning` / `vision` / `tool` 是能力位，`context_window` 是上下文长度。
///
/// 上游拉回真实目录时这些值以上游为准；兜底表只在断网 / 未登录时兜底。
#[derive(Debug, Clone, Copy)]
pub struct FallbackModel {
    pub id: &'static str,
    /// 上游真名（空 = 与 id 相同）。
    pub upstream_key: &'static str,
    pub reasoning: bool,
    pub vision: bool,
    /// 该家是 agent 形态、工具调用为固有能力时置 true。
    pub tool: bool,
    pub context_window: u32,
}

/// 已接入目录发现的供应商。
///
/// 路径与兜底表移植自 Agent2API 对应 provider 的 `models.rs`（MIT 许可，
/// 见 `Agent2API-main/LICENSE`）。兜底表带能力位与上下文长度 —— 那是从
/// Agent2API 的静态表里逐条抄来的实测值；上游拉回真实目录时以上游为准。
///
/// `path` 为空表示该家**只有静态兜底**（Qoder 的目录端点需要 COSY 签名，
/// 本模块不做逐家签名，与其发一个必然被拒的请求，不如用兜底表）。
fn spec_for(provider: &str) -> Option<CatalogSpec> {
    match provider {
        "loomy" => Some(CatalogSpec {
            path: "/api/v1/models",
            openai_shape: true,
            fallback: &[
            sign: None,
                FallbackModel {
                    id: "spark-x",
                    upstream_key: "",
                    reasoning: true,
                    vision: false,
                    tool: true,
                    context_window: 128_000,
                },
                FallbackModel {
                    id: "spark-x2.5",
                    upstream_key: "",
                    reasoning: false,
                    vision: false,
                    tool: true,
                    context_window: 128_000,
                },
            ],
        }),
        "monkeycode" | "monkeycode-intl" => Some(CatalogSpec {
            path: "/api/v1/users/models",
            openai_shape: false,
            fallback: &[],
            sign: None,
        }),
        "commandcode" => Some(CatalogSpec {
            path: "/provider/v1/models",
            openai_shape: true,
            fallback: &[],
            sign: None,
        }),
        "catpaw" => Some(CatalogSpec {
            path: "/api/agent/maas/model-types",
            openai_shape: false,
            fallback: &[],
            sign: None,
        }),
        "codearts" => Some(CatalogSpec {
            path: "/v1/model/builtin",
            openai_shape: false,
            fallback: &[],
            sign: None,
        }),
        "officeace" => Some(CatalogSpec {
            path: "/v1/models",
            openai_shape: true,
            fallback: &[],
            sign: None,
        }),
        // Qoder 中国版 / 国际版：两地的目录不是同一份（套餐档位不同），兜底表
        // 按地区分开 —— 移植自 Agent2API `providers/qoder/models.rs` 的
        // `fallback(region)`。`upstream_key` 是上游的 `config_name`。
        "qoder" => Some(CatalogSpec {
            path: "/algo/api/v2/model/list?Encode=1",
            openai_shape: false,
            fallback: QODER_CN_FALLBACK,
            sign: Some(CatalogSign::Cosy),
        }),
        "qoder-intl" => Some(CatalogSpec {
            path: "/algo/api/v2/model/list?Encode=1",
            openai_shape: false,
            fallback: QODER_INTL_FALLBACK,
            sign: Some(CatalogSign::Cosy),
        }),
        // Cline 免费池 / 订阅池：兜底表移植自 Agent2API
        // `providers/cline/models.rs` 的 `FALLBACK_MODELS`。上游 id 带池前缀
        // （`cline-free/` / `cline-pass/`），客户端可见名就是同一个 id ——
        // AIGX 侧不做改名，前缀本身就是池归属。
        "cline" | "cline-free" | "cline-pass" => Some(CatalogSpec {
            path: "/ai/cline/recommended-models",
            openai_shape: false,
            fallback: CLINE_FALLBACK,
            sign: None,
        }),
        _ => None,
    }
}

/// Qoder 中国版兜底表。
const QODER_CN_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "Qwen3.8-Flash",
        upstream_key: "qfmodel",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.8-Max",
        upstream_key: "qmodel_38max",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Auto",
        upstream_key: "auto",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.7-Max",
        upstream_key: "qmodel_latest",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.7-Plus",
        upstream_key: "qmodel",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "DeepSeek-V4-Pro",
        upstream_key: "dmodel",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "DeepSeek-Flash",
        upstream_key: "dfmodel",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "GLM-5.3",
        upstream_key: "gmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Kimi-K2.8-Preview",
        upstream_key: "kmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "MiniMax-M3",
        upstream_key: "mmodel",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
];

/// Qoder 国际版兜底表（比中国版多几个模型）。
const QODER_INTL_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "Qwen3.8-Flash",
        upstream_key: "qfmodel",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.8-Max",
        upstream_key: "qmodel_38max",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Auto",
        upstream_key: "auto",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Ultimate",
        upstream_key: "ultimate",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Performance",
        upstream_key: "performance",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Efficient",
        upstream_key: "efficient",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Sonus",
        upstream_key: "smodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Cantus",
        upstream_key: "cmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.7-Max",
        upstream_key: "qmodel_latest",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Qwen3.7-Plus",
        upstream_key: "qmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Kimi-K3",
        upstream_key: "kmodel_latest",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "Kimi-K2.8-Preview",
        upstream_key: "kmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "GLM-5.3",
        upstream_key: "gmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "GLM-5.3-Flash",
        upstream_key: "gfmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "DeepSeek-V4-Pro",
        upstream_key: "dmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "DeepSeek-Flash",
        upstream_key: "dfmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "MiniMax-M3",
        upstream_key: "mmodel",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
];

/// Cline 兜底表（免费池 + 订阅池）。
///
/// 上下文窗口取自 Cline 官方目录（`@cline/llms` 的 `cline` 块实测值）。
const CLINE_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "cline-pass/glm-5.3",
        upstream_key: "cline-pass/glm-5.3",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "cline-pass/kimi-k3",
        upstream_key: "cline-pass/kimi-k3",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 256_000,
    },
    FallbackModel {
        id: "cline-pass/deepseek-v4.1-flash",
        upstream_key: "cline-pass/deepseek-v4.1-flash",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "cline-pass/deepseek-v4-pro",
        upstream_key: "cline-pass/deepseek-v4-pro",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "cline-pass/qwen3.8-max",
        upstream_key: "cline-pass/qwen3.8-max",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 256_000,
    },
    FallbackModel {
        id: "cline-free/deepseek-v4.1-flash",
        upstream_key: "cline-free/deepseek-v4.1-flash",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "cline-free/muse-spark-1.3-contributor",
        upstream_key: "cline-free/muse-spark-1.3-contributor",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "cline-free/solar-pro4",
        upstream_key: "cline-free/solar-pro4",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "z-ai/glm-5.3-flash",
        upstream_key: "z-ai/glm-5.3-flash",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 1_310_720,
    },
    FallbackModel {
        id: "poolside/laguna-s-2.1:free",
        upstream_key: "poolside/laguna-s-2.1:free",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 262_144,
    },
];

/// 该供应商是否支持目录发现（管理面据此决定是否显示「同步模型」）。
pub fn supports(provider: &str) -> bool {
    spec_for(provider).is_some()
}

/// 已支持目录发现的供应商清单（管理面下拉用）。
pub fn supported_providers() -> Vec<&'static str> {
    vec![
        "catpaw",
        "cline",
        "cline-free",
        "cline-pass",
        "codearts",
        "commandcode",
        "loomy",
        "monkeycode",
        "monkeycode-intl",
        "officeace",
        "qoder",
        "qoder-intl",
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
            .map(|m| {
                let mut capabilities: Vec<String> = Vec::new();
                if m.reasoning {
                    capabilities.push("reasoning".to_string());
                }
                if m.vision {
                    capabilities.push("vision".to_string());
                }
                if m.tool {
                    capabilities.push("tool".to_string());
                }
                CatalogEntry {
                    id: m.id.to_string(),
                    upstream: if m.upstream_key.is_empty() {
                        None
                    } else {
                        Some(m.upstream_key.to_string())
                    },
                    context_length: if m.context_window > 0 {
                        Some(m.context_window)
                    } else {
                        None
                    },
                    capabilities,
                }
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

    // COSY 签名（Qoder）：目录端点与对话端点用同一套签名 —— 复用
    // `bridge::session::qoder` 的身份解析，两处口径一致，不会出现
    // 「对话能通、目录 401」的分裂。
    if spec.sign == Some(CatalogSign::Cosy) {
        if secret.is_empty() {
            return Err("账号没有凭证，无法签名目录请求".to_string());
        }
        let identity = crate::bridge::session::qoder::identity_of(&secret);
        let signed = crate::bridge::session::qoder_cosy::build_auth_headers(
            None,
            &url,
            &crate::bridge::session::qoder_cosy::CosyIdentity {
                user_id: &identity.user_id,
                auth_token: &secret,
                name: &identity.name,
                email: &identity.email,
                machine_id: &identity.machine_id,
            },
        )
        .map_err(|e| format!("COSY 签名失败：{e}"))?;
        for (name, value) in signed {
            req = req.header(name, value);
        }
    }

    for (name, value) in &headers {
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
    // Qoder 的目录是 `chat[]` 私有形态（不是 OpenAI 的 `data[]`），
    // 解析口径见 `parse_qoder_chat`。
    if spec.sign == Some(CatalogSign::Cosy) {
        let models = parse_qoder_chat(&value);
        if models.is_empty() {
            return Err("上游返回的模型目录为空".to_string());
        }
        return Ok(models);
    }
    Ok(parse_response(&value, spec.openai_shape))
}

/// 解析 Qoder 的模型目录（`{chat:[{key, display_name, is_vl, ...}]}`）。
///
/// 口径移植自 Agent2API `providers/qoder/models.rs` 的 `parse_catalog`：
/// - `key` 是上游真名（路由时用它），`display_name` 是客户端可见名
///   （**去掉所有空白** —— 上游的展示名里带空格与换行，去掉后才是能
///   当模型名用的形态）；
/// - `is_vl` / `is_reasoning` 是能力位；
/// - 上下文长度取条目里的分档上限，取不到用 200k 兜底。
fn parse_qoder_chat(value: &Value) -> Vec<CatalogEntry> {
    let Some(chat) = value.get("chat").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for item in chat {
        let key = item.get("key").and_then(Value::as_str).unwrap_or("").trim();
        let display = item
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if key.is_empty() || display.is_empty() {
            continue;
        }
        let id: String = display.chars().filter(|ch| !ch.is_whitespace()).collect();
        if id.is_empty() {
            continue;
        }
        let lowered = id.to_lowercase();
        if seen.contains(&lowered) {
            continue;
        }
        seen.push(lowered);

        let mut capabilities: Vec<String> = Vec::new();
        if js_truthy(item.get("is_vl")) {
            capabilities.push("vision".to_string());
        }
        if js_truthy(item.get("is_reasoning")) || js_truthy(item.get("thinking_config")) {
            capabilities.push("reasoning".to_string());
        }
        // Qoder 上游是 agent 形态，工具调用是固有能力
        capabilities.push("tool".to_string());

        out.push(CatalogEntry {
            id,
            upstream: Some(key.to_string()),
            context_length: Some(qoder_context_window(item)),
            capabilities,
        });
    }
    out
}

/// JS 真值判定（`Boolean(x)`）：null/false/0/"" 为假，其余为真。
fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().map(|n| n != 0.0).unwrap_or(false),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// Qoder 条目的上下文长度：取分档上限，取不到用 200k 兜底。
///
/// 上游把上下文按 tier 分档（`context_window` 之类），本函数扫条目里的
/// 数值字段取最大值 —— 与 Agent2API 的 `context_window_of` 同口径
/// （它读 `tiers_from_catalog` 后的 max）。
fn qoder_context_window(item: &Value) -> u32 {
    const DEFAULT_CONTEXT_WINDOW: u32 = 200_000;
    let mut max = 0u32;
    if let Some(obj) = item.as_object() {
        for (key, value) in obj {
            if !key.to_lowercase().contains("context")
                && !key.to_lowercase().contains("token")
                && !key.to_lowercase().contains("window")
            {
                continue;
            }
            // 数字
            if let Some(n) = value.as_u64() {
                if n > 0 && n <= u32::MAX as u64 && n as u32 > max {
                    max = n as u32;
                }
            }
            // 分档数组：{context_window: N} 之类
            if let Some(arr) = value.as_array() {
                for entry in arr {
                    if let Some(n) = entry.as_u64() {
                        if n > 0 && n <= u32::MAX as u64 && n as u32 > max {
                            max = n as u32;
                        }
                    } else if let Some(inner) = entry.as_object() {
                        for v in inner.values() {
                            if let Some(n) = v.as_u64() {
                                if n > 0 && n <= u32::MAX as u64 && n as u32 > max {
                                    max = n as u32;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if max > 0 {
        max
    } else {
        DEFAULT_CONTEXT_WINDOW
    }
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
        .map_err(|e| format!("写入账号失败：{e}"))?;
    Ok(())
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
    fn fallback_tables_carry_metadata() {
        // Qoder 兜底表带上游真名与能力位
        let spec = spec_for("qoder").expect("qoder 应有目录描述");
        assert!(!spec.fallback.is_empty());
        let first = &spec.fallback[0];
        assert_eq!(first.id, "Qwen3.8-Flash");
        assert_eq!(first.upstream_key, "qfmodel");
        assert!(first.reasoning);
        assert!(first.context_window > 0);

        // 兜底目录条目要带上上游真名与能力位
        let snapshot = fallback_catalog(&spec);
        assert_eq!(snapshot.models.len(), spec.fallback.len());
        assert_eq!(snapshot.source, "fallback");
        let entry = &snapshot.models[0];
        assert_eq!(entry.upstream.as_deref(), Some("qfmodel"));
        assert!(entry.capabilities.iter().any(|c| c == "reasoning"));
    }

    #[test]
    fn cline_fallback_keeps_pool_prefix() {
        let spec = spec_for("cline-free").expect("cline 应有目录描述");
        // 表里既有 cline-free/ 也有 cline-pass/ 的条目（两个池共用一张表）
        assert!(spec.fallback.iter().all(|m| m.id.starts_with("cline-free/")
            || m.id.starts_with("cline-pass/")
            || m.id.starts_with("z-ai/")
            || m.id.starts_with("poolside/")));
    }

    #[test]
    fn qoder_chat_parsed_with_upstream_key() {
        let payload = serde_json::json!({
            "chat": [
                {"key": "qfmodel", "display_name": "Qwen3.8 Flash", "is_vl": true},
                {"key": "qmodel", "display_name": "Qwen3.8-Max", "is_reasoning": true},
                {"key": "dup", "display_name": "Qwen3.8 Flash"},
                {"key": "", "display_name": "无效"},
            ]
        });
        let entries = parse_qoder_chat(&payload);
        // 第 3 条是第 1 条的同名（去空白后），按 id 小写去重
        assert_eq!(entries.len(), 2);
        // 可见名去掉所有空白
        assert_eq!(entries[0].id, "Qwen3.8Flash");
        // 上游真名单独记
        assert_eq!(entries[0].upstream.as_deref(), Some("qfmodel"));
        assert!(entries[0].capabilities.iter().any(|c| c == "vision"));
        assert!(entries[1].capabilities.iter().any(|c| c == "reasoning"));
    }

    #[test]
    fn qoder_context_window_defaults_to_200k() {
        let item = serde_json::json!({"key": "x"});
        assert_eq!(qoder_context_window(&item), 200_000);
        let item = serde_json::json!({"max_context_tokens": 131072});
        assert_eq!(qoder_context_window(&item), 131_072);
    }

    #[test]
    fn qoder_spec_uses_cosy_signed_endpoint() {
        let spec = spec_for("qoder").expect("qoder 应有目录描述");
        assert_eq!(spec.sign, Some(CatalogSign::Cosy));
        assert!(spec.path.contains("model/list"));
    }

    #[test]
    fn supported_providers_are_consistent() {
        for kind in supported_providers() {
            assert!(supports(kind), "{kind} 在清单里但 spec_for 不认");
        }
    }
}
