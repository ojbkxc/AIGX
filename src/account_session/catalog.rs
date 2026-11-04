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

/// KukuAI 目录请求的固定 query 前缀（上游要求先带这串再拼 token 三件套）。
const WEB_QUERY: &str = "webQuery=1";

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
    //
    // 字段名对齐 Agent2API 归一后的目录字段（`core/models/shape.rs`）：
    // `supports_images` / `supports_reasoning` / `supports_tool_call` —— 注意
    // images 是**复数**（A2A 的口径），单数 `supports_image` 是各家的原始
    // 字段名，两者都认。
    let flags: &[(&str, &str)] = &[
        ("supportsImages", "vision"),
        ("supports_images", "vision"),
        ("supports_image", "vision"),
        ("supportsVision", "vision"),
        ("supportsToolCall", "tool"),
        ("supports_tool_call", "tool"),
        ("supportsTools", "tool"),
        ("supportsReasoning", "reasoning"),
        ("supports_reasoning", "reasoning"),
        ("thinking", "reasoning"),
        ("supportsVideo", "video"),
        ("supports_video", "video"),
        ("supportsAudio", "audio"),
    ];
    for (key, label) in flags {
        if value.get(*key).and_then(Value::as_bool).unwrap_or(false)
            && !caps.iter().any(|c| c.eq_ignore_ascii_case(label))
        {
            caps.push((*label).to_string());
        }
    }
    // `input_modalities: ["text","image","video"]` —— Agent2API 归一后的目录
    // 用它表达输入模态。视觉/视频位也在这里（有的上游只给模态、不给布尔位）。
    if let Some(modalities) = value.get("input_modalities").and_then(Value::as_array) {
        for modality in modalities {
            let Some(text) = modality.as_str() else {
                continue;
            };
            let label = match text.trim().to_ascii_lowercase().as_str() {
                "image" | "images" | "vision" => "vision",
                "video" | "videos" => "video",
                "audio" => "audio",
                _ => continue,
            };
            if !caps.iter().any(|c| c.eq_ignore_ascii_case(label)) {
                caps.push(label.to_string());
            }
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
    /// 目录端点的 HTTP 方法（None = GET）。
    method: Option<CatalogMethod>,
    /// 请求体（仅 POST 需要）。
    body: Option<fn() -> Value>,
    /// 响应解析器（None = 按 `openai_shape` 走通用解析）。
    parser: Option<CatalogParser>,
}

/// 目录端点的 HTTP 方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogMethod {
    Post,
}

/// 目录响应的专用解析器。
///
/// 有些上游的目录不是 OpenAI 的 `{data:[{id,…}]}` 形态（Trae 是
/// `{config_info_list:[…]}`、Antigravity 是 `{models:{id:…}}` 字典），
/// 通用解析读不出来 —— 这时给一家一个专用解析器。
///
/// 解析口径逐条对齐 Agent2API 对应 provider 的 `models.rs`（见该文件的
/// `parse_catalog`），**不编造字段**：上游没给的上下文长度就不给。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogParser {
    /// Trae：`{config_info_list:[{config_name, display_config:{display_name}, …}]}`。
    Trae,
    /// Antigravity：`{models:{"<id>":{…}}}` —— **字典**不是数组。
    AntigravityModelsDict,
    /// Accio：`{data:[{provider, modelList:[{modelCode, modelDisplayName,…}]}]}`。
    Accio,
    /// 小浣熊(raccoon)：`{data:{models:[{modelId, modelName,…}]}}`。
    Raccoon,
    /// KukuAI：`{data:{model_list:[…]}}` —— 顶层包络 `status.code` 判成败，
    /// 模型名散落在嵌套结构里（递归收集 `model_name` / `modelname` / `modelid`）。
    Kuku,
}

/// 目录端点的特殊鉴权与解析方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CatalogSign {
    /// COSY 签名（Qoder）：请求头要用账号身份加密签名，与转发链路同一套。
    Cosy,
    /// KukuAI：目录端点要带**按产品签发的** STOKEN 三件套（bdstoken /
    /// uinfo / uk），不是网页登录 Cookie 里那份通行证级 STOKEN ——
    /// 光带 Cookie 的症状是「余额正常、模型拉不下」。
    Kuku,
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
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        "monkeycode" | "monkeycode-intl" => Some(CatalogSpec {
            path: "/api/v1/users/models",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        "commandcode" => Some(CatalogSpec {
            path: "/provider/v1/models",
            openai_shape: true,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        "catpaw" => Some(CatalogSpec {
            path: "/api/agent/maas/model-types",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        "codearts" => Some(CatalogSpec {
            path: "/v1/model/builtin",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        "officeace" => Some(CatalogSpec {
            path: "/v1/models",
            openai_shape: true,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // Qoder 中国版 / 国际版：两地的目录不是同一份（套餐档位不同），兜底表
        // 按地区分开 —— 移植自 Agent2API `providers/qoder/models.rs` 的
        // `fallback(region)`。`upstream_key` 是上游的 `config_name`。
        "qoder" => Some(CatalogSpec {
            path: "/algo/api/v2/model/list?Encode=1",
            openai_shape: false,
            fallback: QODER_CN_FALLBACK,
            sign: Some(CatalogSign::Cosy),
            method: None,
            body: None,
            parser: None,
        }),
        "qoder-intl" => Some(CatalogSpec {
            path: "/algo/api/v2/model/list?Encode=1",
            openai_shape: false,
            fallback: QODER_INTL_FALLBACK,
            sign: Some(CatalogSign::Cosy),
            method: None,
            body: None,
            parser: None,
        }),
        // KukuAI：`GET /wenchain/genflowpro/model/list`，查询串要带按产品
        // 签发的 STOKEN 三件套（见 `CatalogSign::Kuku` 的说明）。
        "kuku" => Some(CatalogSpec {
            path: "/wenchain/genflowpro/model/list",
            openai_shape: false,
            fallback: &[],
            sign: Some(CatalogSign::Kuku),
            method: None,
            body: None,
            parser: Some(CatalogParser::Kuku),
        }),
        // AutoClaw：**上游没有可拉取的模型目录接口** —— 源实现是一份静态映射
        // 表（AutoClaw 设置页目录 + model-provider-config 支持列表）。
        // 移植自 Agent2API `providers/autoclaw/models.rs` 的 `MODELS`。
        "autoclaw" | "autoclaw-intl" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: AUTOCLAW_FALLBACK,
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // OrcaTerm：静态兜底（上游没有可拉的清单接口）——
        // 移植自 Agent2API `providers/orcaterm/models.rs` 的 `MODELS`。
        "orcaterm" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: ORCATERM_FALLBACK,
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // ZCode：静态表（上游没有「列模型」的公开接口）——
        // 移植自 Agent2API `providers/zcode/models.rs` 的 `MODELS`。
        "zcode" | "zcode-intl" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: ZCODE_FALLBACK,
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // Trae SOLO：`POST /api/ide/v1/get_detail_param`。
        // 目录是「按客户端版本给的一张表」—— `X-Ide-Version-Code` 决定拿到
        // 哪一张，所以**没有静态兜底**（抄成常量表等于把一个时间点的读数
        // 当契约，上游加模型我们要改代码发一版）。没刷到就是空清单。
        "trae" => Some(CatalogSpec {
            path: "/api/ide/v1/get_detail_param",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: Some(CatalogMethod::Post),
            body: Some(trae_catalog_body),
            parser: Some(CatalogParser::Trae),
        }),
        // Antigravity：`POST {base}/v1internal:fetchAvailableModels`。
        // 响应是 `{models:{"<id>":{…}}}` —— **字典**不是数组。
        "antigravity" => Some(CatalogSpec {
            path: "/v1internal:fetchAvailableModels",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: Some(CatalogMethod::Post),
            body: Some(antigravity_catalog_body),
            parser: Some(CatalogParser::AntigravityModelsDict),
        }),
        // Accio：`POST {gw}/api/llm/config`，body 带 token。
        // 响应是 `{data:[{provider, modelList:[{modelCode, modelDisplayName,…}]}]}`。
        "accio" | "accio-cn" => Some(CatalogSpec {
            path: "/api/llm/config",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: Some(CatalogMethod::Post),
            body: Some(accio_catalog_body),
            parser: Some(CatalogParser::Accio),
        }),
        // 小浣熊（上游叫 raccoon）：`GET {llmBase}/model_catalog`。
        // 响应与 `/v3/config` 同形态：`{data:{models:[{modelId, modelName,…}]}}`。
        "xiaohuanxiong" => Some(CatalogSpec {
            path: "/model_catalog",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: Some(CatalogParser::Raccoon),
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
            method: None,
            body: None,
            parser: None,
        }),
        _ => None,
    }
}

/// AutoClaw 兜底表（上游无远程目录接口）。
///
/// 移植自 Agent2API `providers/autoclaw/models.rs` 的 `MODELS`：
/// `id` 是路由 id，`route_id` 是上游真名，`reasoning`/`support_image`
/// 是能力位，`context_window` 是上下文长度。
const AUTOCLAW_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "glm-5.3",
        upstream_key: "zaicoding_glm-5.3",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 1_048_576,
    },
    FallbackModel {
        id: "glm-5.3-flash",
        upstream_key: "zai_glm-5.3-flash",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 1_048_576,
    },
];

/// OrcaTerm 兜底表（上游无远程目录接口，客户端内置清单）。
///
/// 移植自 Agent2API `providers/orcaterm/models.rs` 的 `MODELS`。
const ORCATERM_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "hy4-preview",
        upstream_key: "hy4-preview",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "hy3",
        upstream_key: "hy3",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "kimi-k3",
        upstream_key: "kimi-k3",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "glm-5.3-flash",
        upstream_key: "glm-5.3-flash",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "glm-5.3",
        upstream_key: "glm-5.3",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "glm-5.2",
        upstream_key: "glm-5.2",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4.1-flash",
        upstream_key: "deepseek-v4.1-flash",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-flash",
        upstream_key: "deepseek-v4-flash",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-pro",
        upstream_key: "deepseek-v4-pro",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
];

/// ZCode 兜底表（上游无「列模型」接口，客户端内置目录）。
///
/// 移植自 Agent2API `providers/zcode/models.rs` 的 `MODELS`。
/// 视觉两档（`glm-4.6v` / `glm-5v-turbo`）上游**明确不支持思考** ——
/// 声明成 true 会让客户端按「支持思考」发 `thinking`，上游直接 400。
const ZCODE_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "glm-5.3",
        upstream_key: "glm-5.3",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "glm-5.3-flash",
        upstream_key: "glm-5.3-flash",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "glm-5.2",
        upstream_key: "glm-5.2",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "glm-5.1",
        upstream_key: "glm-5.1",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 64_000,
    },
    FallbackModel {
        id: "glm-5",
        upstream_key: "glm-5",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 64_000,
    },
    FallbackModel {
        id: "glm-5-turbo",
        upstream_key: "glm-5-turbo",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 64_000,
    },
    FallbackModel {
        id: "glm-4.7",
        upstream_key: "glm-4.7",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 131_072,
    },
    FallbackModel {
        id: "glm-4.6",
        upstream_key: "glm-4.6",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 131_072,
    },
    FallbackModel {
        id: "glm-4.5-air",
        upstream_key: "glm-4.5-air",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 98_304,
    },
    FallbackModel {
        id: "glm-4.6v",
        upstream_key: "glm-4.6v",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 32_768,
    },
    FallbackModel {
        id: "glm-5v-turbo",
        upstream_key: "glm-5v-turbo",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 131_072,
    },
];

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

/// 通用模型发现：对**任意** API-Key 型供应商拉一次上游 `/models`。
///
/// 与 [`sync_account_catalog`] 的分工：
/// - `sync_account_catalog` 只覆盖 `spec_for` 里那批登录态供应商（含私有
///   解析器 / 签名 / 静态兜底），按账号落缓存；
/// - 本函数覆盖剩下的 OpenAI 兼容 / Anthropic / Gemini 直连供应商——它们
///   的目录端点就是标准形态，不需要逐家特判。用于「账号刚建好、模型白名单
///   还空着」时自动补一份清单，避免账号进了池子却在模型管理里看不见。
///
/// 不落缓存、不编造模型名：拉不到就返回错误，由调用方决定是否忽略。
pub async fn discover_models_plain(
    sessions: &AccountSessionStore,
    session: &AccountSession,
) -> Result<Vec<String>, String> {
    let spec = registry::find(&session.provider)
        .ok_or_else(|| format!("未知供应商 {}", session.provider))?;
    let base = super::adapter::effective_base_url(session)
        .map_err(|e| format!("上游地址不可用：{e}"))?
        .trim()
        .trim_end_matches('/')
        .to_string();
    let secret = session.credential.secret_plain();
    // 免鉴权账号（IP 白名单 / 本地 Ollama）不带 Authorization
    let authed =
        !secret.is_empty() && session.credential.kind != super::model::CREDENTIAL_KIND_NONE;

    let client = sessions
        .client_for(session)
        .map_err(|e| format!("出口客户端不可用：{e}"))?;

    let url = match spec.protocol {
        registry::Protocol::Openai => {
            format!("{}/models", crate::bridge::openai::normalize_base_url(base))
        }
        registry::Protocol::Anthropic => format!("{base}/v1/models"),
        registry::Protocol::Gemini => format!("{base}/models"),
    };

    let mut req = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));
    match spec.protocol {
        registry::Protocol::Anthropic => {
            if authed {
                req = req.header("x-api-key", &secret);
            }
            req = req.header("anthropic-version", "2023-06-01");
        }
        registry::Protocol::Gemini => {
            if authed {
                req = req.header("x-goog-api-key", &secret);
            }
        }
        registry::Protocol::Openai => {
            // 登录态供应商的专用头仍然要带（缺头一律 403 的那几家）
            if let Some(meta) = registry::session_meta(&session.provider) {
                let defaults: Vec<(String, String)> = meta
                    .headers
                    .iter()
                    .map(|(name, template)| {
                        (
                            (*name).to_string(),
                            template.replace("{credential}", &secret),
                        )
                    })
                    .collect();
                // 与对话路径同一份伪装头快照：上游按头做版本门禁，
                // 目录/发现请求与转发请求不能两套身份。
                for (name, value) in
                    crate::config::apply_cline_header_overrides(&session.provider, defaults)
                {
                    req = req.header(name.as_str(), value.as_str());
                }
            }
            for (name, value) in &session.extra_headers {
                req = req.header(name, value.replace("{credential}", &secret));
            }
            if authed {
                req = req.bearer_auth(&secret);
            }
        }
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
    // 三家都是 `{data:[{id}]}` 或 `{models:[{name}]}` 形态，`parse_response`
    // 两种都认（非 OpenAI 形态走 `models` 分支，并剥 `models/` 前缀）。
    let entries = parse_response(&value, true);
    if entries.is_empty() {
        // OpenAI 兼容供应商有时只回 `{models:[...]}`，openai_shape 分支取不到
        let retry = parse_response(&value, false);
        if retry.is_empty() {
            return Err("上游返回的模型目录为空".to_string());
        }
        return Ok(retry.into_iter().map(|e| e.id).collect());
    }
    Ok(entries.into_iter().map(|e| e.id).collect())
}

/// 已支持目录发现的供应商清单（管理面下拉用）。
pub fn supported_providers() -> Vec<&'static str> {
    vec![
        "accio",
        "accio-cn",
        "antigravity",
        "autoclaw",
        "autoclaw-intl",
        "catpaw",
        "cline",
        "cline-free",
        "cline-pass",
        "codearts",
        "commandcode",
        "kuku",
        "loomy",
        "monkeycode",
        "monkeycode-intl",
        "officeace",
        "orcaterm",
        "qoder",
        "qoder-intl",
        "trae",
        "xiaohuanxiong",
        "zcode",
        "zcode-intl",
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
    // KukuAI 的目录路径要带 STOKEN 三件套 —— 得先取 token 才能拼出完整 URL，
    // 所以这里不是简单的 `base + path`。
    let url = if spec.sign == Some(CatalogSign::Kuku) {
        // `client_for` 给的是 `Arc<Client>`，而 Bridge 要 `Client`（值）——
        // 克隆一次 reqwest::Client 是廉价的（内部是 Arc 句柄）。
        let client = sessions
            .client_for(session)
            .map_err(|e| format!("出口客户端不可用：{e}"))?;
        let bridge = crate::bridge::session::kuku::KukuBridge::new(
            base,
            session.credential.secret_plain(),
            client.as_ref().clone(),
        );
        let query = bridge
            .catalog_query()
            .await
            .map_err(|e| format!("取 KukuAI 会话令牌失败：{e}"))?;
        format!("{base}{}?{WEB_QUERY}{query}", spec.path)
    } else {
        format!("{base}{}", spec.path)
    };

    let client = sessions
        .client_for(session)
        .map_err(|e| format!("出口客户端不可用：{e}"))?;

    // 凭证落盘是 `enc:` 编码的，取明文用 `secret_plain()`。
    let secret = session.credential.secret_plain();

    let mut req = match spec.method {
        Some(CatalogMethod::Post) => {
            let mut builder = client
                .post(&url)
                .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));
            if let Some(body) = spec.body {
                builder = builder.json(&body());
            }
            builder
        }
        None => client
            .get(&url)
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)),
    };

    // provider 默认头 + 账号级额外头
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(meta) = registry::session_meta(&session.provider) {
        for (name, template) in meta.headers {
            let value = template.replace("{credential}", &secret);
            headers.push(((*name).to_string(), value));
        }
    }
    // Cline 伪装头覆盖表（与对话路径同一份快照：目录请求也要漂成同一个客户端身份）
    let mut headers = crate::config::apply_cline_header_overrides(&session.provider, headers);
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
    // 各家目录响应形态不同，按家分派解析器。
    //
    // Qoder 的目录是 `chat[]` 私有形态（不是 OpenAI 的 `data[]`），
    // 解析口径见 `parse_qoder_chat`。
    let models = match spec.parser {
        Some(CatalogParser::Trae) => parse_trae_catalog(&value),
        Some(CatalogParser::AntigravityModelsDict) => parse_antigravity_catalog(&value),
        Some(CatalogParser::Accio) => parse_accio_catalog(&value),
        Some(CatalogParser::Raccoon) => parse_raccoon_catalog(&value),
        Some(CatalogParser::Kuku) => parse_kuku_catalog(&value),
        None if spec.sign == Some(CatalogSign::Cosy) => parse_qoder_chat(&value),
        None => parse_response(&value, spec.openai_shape),
    };
    if models.is_empty() {
        return Err("上游返回的模型目录为空".to_string());
    }
    Ok(models)
}

/// Trae 目录请求体。
///
/// 口径抄自 Agent2API `providers/trae/models.rs` 的 `catalog_body`：
/// `function` 恒为 `solo_work_lite`（各家变体都映射到它），其余字段是空值占位。
fn trae_catalog_body() -> Value {
    serde_json::json!({
        "function": "solo_work_lite",
        "config_names": Value::Null,
        "need_prompt": false,
        "current_config_info": Value::Null,
        "poly_prompt": true,
        "mode_type": Value::Null,
        "agent_type": Value::Null,
    })
}

/// Antigravity 目录请求体（上游不接受空体）。
fn antigravity_catalog_body() -> Value {
    serde_json::json!({})
}

/// Accio 目录请求体（token 由请求头带，体里只放空对象）。
fn accio_catalog_body() -> Value {
    serde_json::json!({})
}

/// 解析 Trae 的模型目录（`{config_info_list:[…]}`）。
///
/// 口径抄自 Agent2API `providers/trae/models.rs` 的 `parse_catalog`：
/// - `config_name` 是上游真名（也是路由用的 id），空则跳过；
/// - `display_config.display_name` 是展示名，空则跳过（那是租户自定义
///   模型的**占位模板**，能出现在表里但不是可选模型）；
/// - `config_switch == false`（**显式 false** 才过滤，缺省按启用处理 ——
///   上游删字段时不能把整张表滤空）与 `is_invisible_to_user == true`
///   （内部 subagent / 实验通道）两类要滤掉。
fn parse_trae_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let Some(entries) = payload.get("config_info_list").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = Vec::with_capacity(entries.len());
    let mut seen: Vec<String> = Vec::with_capacity(entries.len());
    for entry in entries {
        let config_name = entry
            .get("config_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if config_name.is_empty() {
            continue;
        }
        let lowered = config_name.to_lowercase();
        if seen.contains(&lowered) {
            continue;
        }
        seen.push(lowered);
        if entry.get("config_switch").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        if entry.get("is_invisible_to_user").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let display_name = entry
            .get("display_config")
            .and_then(|config| config.get("display_name"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if display_name.is_empty() {
            continue;
        }
        // 上下文长度在 `context_window_tokens.dev`
        let context = entry
            .pointer("/context_window_tokens/dev")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        let mut capabilities: Vec<String> = Vec::new();
        if entry.get("supports_reasoning").and_then(Value::as_bool) == Some(true)
            || entry
                .pointer("/thinking_config/enabled")
                .and_then(Value::as_bool)
                == Some(true)
        {
            capabilities.push("reasoning".to_string());
        }
        capabilities.push("tool".to_string());
        out.push(CatalogEntry {
            id: display_name,
            upstream: Some(config_name),
            context_length: context,
            capabilities,
        });
    }
    out
}

/// 解析 Antigravity 的模型目录（`{models:{"<id>":{…}}}` —— 字典）。
fn parse_antigravity_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let Some(models) = payload.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = Vec::with_capacity(models.len());
    for (id, meta) in models {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut capabilities: Vec<String> = Vec::new();
        if meta.get("supportsImages").and_then(Value::as_bool) == Some(true)
            || meta.get("supports_images").and_then(Value::as_bool) == Some(true)
        {
            capabilities.push("vision".to_string());
        }
        if meta.get("supportsReasoning").and_then(Value::as_bool) == Some(true) {
            capabilities.push("reasoning".to_string());
        }
        capabilities.push("tool".to_string());
        let context = meta
            .get("maxInputTokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        out.push(CatalogEntry {
            id: trimmed.to_string(),
            upstream: None,
            context_length: context,
            capabilities,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 解析 Accio 的模型目录（`{data:[{provider, modelList:[…]}]}`）。
///
/// 口径抄自 Agent2API `providers/accio/models.rs`：外层是 **provider 数组**
/// （不是 `{providers:[…]}` 对象 —— 早先按对象解析导致清单恒为空而 HTTP 是
/// 200，前端只看到「上游目录里没有可见模型」），模型在 `modelList` 里。
/// `visible == false` 的条目不列。
fn parse_accio_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let Some(providers) = payload.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for provider in providers {
        let Some(list) = provider.get("modelList").and_then(Value::as_array) else {
            continue;
        };
        for model in list {
            if model.get("visible").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            let code = model
                .get("modelCode")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if code.is_empty() {
                continue;
            }
            let display = model
                .get("modelDisplayName")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let id = if display.is_empty() {
                code.clone()
            } else {
                display.to_string()
            };
            if seen.contains(&id) {
                continue;
            }
            seen.push(id.clone());
            let mut capabilities: Vec<String> = Vec::new();
            if model.get("supportImage").and_then(Value::as_bool) == Some(true) {
                capabilities.push("vision".to_string());
            }
            capabilities.push("tool".to_string());
            out.push(CatalogEntry {
                id,
                upstream: Some(code),
                context_length: None,
                capabilities,
            });
        }
    }
    out
}

/// 解析 KukuAI 的模型目录。
///
/// 口径抄自 Agent2API `providers/kuku/models.rs` 的 `extract_model_names`：
/// 模型名散落在嵌套结构里，所以**递归**收集 `model_name` / `modelname` /
/// `modelid` 三种键的字符串值，排序后去重。
fn parse_kuku_catalog(payload: &Value) -> Vec<CatalogEntry> {
    // 顶层包络：业务错误也在 200 里，判据在 `status.code`。
    if let Some(code) = payload.pointer("/status/code").and_then(Value::as_i64) {
        if code != 0 {
            return Vec::new();
        }
    }
    let mut names: Vec<String> = Vec::new();
    collect_model_names(payload, &mut names);
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|id| CatalogEntry {
            id,
            upstream: None,
            context_length: None,
            capabilities: vec!["tool".to_string()],
        })
        .collect()
}

/// 递归收集 KukuAI 目录里的模型名。
fn collect_model_names(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                if matches!(lower.as_str(), "model_name" | "modelname" | "modelid")
                    && child.is_string()
                {
                    if let Some(text) = child.as_str() {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                            continue;
                        }
                    }
                }
                collect_model_names(child, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_model_names(item, out);
            }
        }
        _ => {}
    }
}

/// 解析小浣熊的模型目录（`{data:{models:[{modelId, modelName,…}]}}`）。
fn parse_raccoon_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let Some(models) = payload
        .pointer("/data/models")
        .or_else(|| payload.get("models"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut out: Vec<CatalogEntry> = Vec::with_capacity(models.len());
    let mut seen: Vec<String> = Vec::with_capacity(models.len());
    for model in models {
        let id = model
            .get("modelId")
            .or_else(|| model.get("id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if id.is_empty() {
            continue;
        }
        if seen.contains(&id) {
            continue;
        }
        seen.push(id.clone());
        let mut capabilities: Vec<String> = Vec::new();
        if model.get("is_reasoning").and_then(Value::as_bool) == Some(true) {
            capabilities.push("reasoning".to_string());
        }
        if model.get("is_vl").and_then(Value::as_bool) == Some(true) {
            capabilities.push("vision".to_string());
        }
        capabilities.push("tool".to_string());
        let context = model
            .get("maxInputTokens")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok());
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: context,
            capabilities,
        });
    }
    out
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
    fn trae_catalog_filters_invisible_and_placeholders() {
        let payload = serde_json::json!({
            "config_info_list": [
                {
                    "config_name": "deepseek-v4-pro",
                    "display_config": {"display_name": "DeepSeek V4 Pro"},
                    "context_window_tokens": {"dev": 128000},
                    "supports_reasoning": true
                },
                // 内部 subagent：不可见
                {
                    "config_name": "browser_use_subagent",
                    "display_config": {"display_name": "Browser Use"},
                    "is_invisible_to_user": true
                },
                // 租户自定义占位模板：展示名为空
                {"config_name": "custom_model_x", "display_config": {"display_name": ""}},
                // 显式下线
                {
                    "config_name": "retired",
                    "display_config": {"display_name": "Retired"},
                    "config_switch": false
                }
            ]
        });
        let entries = parse_trae_catalog(&payload);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "DeepSeek V4 Pro");
        assert_eq!(entries[0].upstream.as_deref(), Some("deepseek-v4-pro"));
        assert_eq!(entries[0].context_length, Some(128000));
        assert!(entries[0].capabilities.iter().any(|c| c == "reasoning"));
    }

    #[test]
    fn antigravity_catalog_reads_models_dict() {
        let payload = serde_json::json!({
            "models": {
                "gemini-3-pro": {"maxInputTokens": 1048576, "supportsImages": true},
                "gemini-3-flash": {}
            }
        });
        let entries = parse_antigravity_catalog(&payload);
        assert_eq!(entries.len(), 2);
        // 按 id 排序，字典顺序不固定
        assert_eq!(entries[0].id, "gemini-3-flash");
        assert_eq!(entries[1].id, "gemini-3-pro");
        assert_eq!(entries[1].context_length, Some(1048576));
        assert!(entries[1].capabilities.iter().any(|c| c == "vision"));
    }

    #[test]
    fn accio_catalog_flattens_provider_model_lists() {
        let payload = serde_json::json!({
            "data": [
                {
                    "provider": "openai",
                    "modelList": [
                        {"modelCode": "code-1", "modelDisplayName": "GPT 6 Astra", "visible": true},
                        {"modelCode": "hidden", "modelDisplayName": "Hidden", "visible": false}
                    ]
                }
            ]
        });
        let entries = parse_accio_catalog(&payload);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "GPT 6 Astra");
        assert_eq!(entries[0].upstream.as_deref(), Some("code-1"));
    }

    #[test]
    fn raccoon_catalog_reads_nested_models() {
        let payload = serde_json::json!({
            "data": {"models": [
                {"modelId": "spark-x", "is_reasoning": true, "is_vl": true, "maxInputTokens": 128000}
            ]}
        });
        let entries = parse_raccoon_catalog(&payload);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "spark-x");
        assert_eq!(entries[0].context_length, Some(128000));
        assert!(entries[0].capabilities.iter().any(|c| c == "vision"));
    }

    #[test]
    fn kuku_catalog_collects_nested_model_names() {
        let payload = serde_json::json!({
            "status": {"code": 0, "msg": "ok"},
            "data": {"model_list": [
                {"model_name": "spark-x", "extra": {"modelId": "nested-id"}}
            ]}
        });
        let entries = parse_kuku_catalog(&payload);
        let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"spark-x"));
        assert!(ids.contains(&"nested-id"));
    }

    #[test]
    fn kuku_catalog_rejects_business_error_envelope() {
        let payload = serde_json::json!({
            "status": {"code": 4001, "msg": "not logged in"},
            "data": {"model_list": [{"model_name": "should-not-appear"}]}
        });
        assert!(parse_kuku_catalog(&payload).is_empty());
    }

    #[test]
    fn capabilities_read_agent2api_normalized_fields() {
        // 样本取自 Agent2API 归一后的真实上游目录（`/v1/models` 实测响应）。
        //
        // 关键点：视觉位在 **`supports_images`（复数）** 与
        // `input_modalities` 里 —— 早先只认单数 `supports_image`，导致
        // 真实支持图像的模型被判成不支持视觉。
        let sample = serde_json::json!({
            "id": "hy3-x",
            "name": "Hy3",
            "owned_by": "workbuddy",
            "supports_images": true,
            "supports_reasoning": true,
            "supports_tool_call": true,
            "supports_video": false,
            "input_modalities": ["text", "image"],
            "max_input_tokens": 192000,
            "max_output_tokens": 64000
        });
        let caps = pick_capabilities(&sample);
        assert!(caps.iter().any(|c| c == "vision"), "应认出视觉位：{caps:?}");
        assert!(
            caps.iter().any(|c| c == "reasoning"),
            "应认出推理位：{caps:?}"
        );
        assert!(caps.iter().any(|c| c == "tool"), "应认出工具位：{caps:?}");
        assert!(!caps.iter().any(|c| c == "video"), "不支持视频：{caps:?}");

        // 只有 input_modalities 带 image（上游不给布尔位）也要认出视觉
        let modalities_only = serde_json::json!({
            "id": "mimo-v2.6-pro",
            "input_modalities": ["text", "image", "video"]
        });
        let caps = pick_capabilities(&modalities_only);
        assert!(caps.iter().any(|c| c == "vision"), "{caps:?}");
        assert!(caps.iter().any(|c| c == "video"), "{caps:?}");

        // 纯文本模型不该被误判成支持视觉
        let text_only = serde_json::json!({
            "id": "glm-5.3",
            "supports_images": false,
            "input_modalities": ["text"]
        });
        let caps = pick_capabilities(&text_only);
        assert!(!caps.iter().any(|c| c == "vision"), "{caps:?}");
    }

    #[test]
    fn static_only_providers_have_no_remote_path() {
        // 上游没有可拉的目录接口的三家：path 为空 → 走静态兜底
        for kind in ["autoclaw", "orcaterm", "zcode"] {
            let spec = spec_for(kind).expect("应有目录描述");
            assert!(spec.path.is_empty(), "{kind} 不该有远程端点");
            assert!(!spec.fallback.is_empty(), "{kind} 应有静态兜底表");
        }
    }

    #[test]
    fn supported_providers_are_consistent() {
        for kind in supported_providers() {
            assert!(supports(kind), "{kind} 在清单里但 spec_for 不认");
        }
    }
}
