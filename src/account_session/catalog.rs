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
use std::sync::Arc;

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

/// 账号目录的**能力索引**（#40 路由多规则 §1 的账号级精筛数据源）。
///
/// 由 [`CatalogSnapshot`] 归一而来，**只存在于进程内存**：选路要在选号循环里
/// 按模型名查能力位，而目录快照落在 SQLite KV —— 每查一次就是一次 DB 读 +
/// JSON 解析，绝不能让候选循环去读它（#40 性能红线）。
///
/// ## 三态口径（这是本结构最重要的语义）
///
/// | 情形 | 判定 |
/// |---|---|
/// | 条目带标签，且覆盖所需位 | 放行 |
/// | 条目带标签，缺所需位 | **拒绝**（已知不支持） |
/// | 条目不带标签（`capabilities` 字段是后加的，老快照没有） | 放行（未知） |
/// | 模型名不在索引里（该家无目录发现 / 尚未同步过） | 放行（未知） |
///
/// **为什么未知一律放行**：升级后老快照与未同步过的账号全都算「无能力信息」，
/// 若未知即拒绝，这些账号会**无声地**退出候选池（选号层过滤掉的账号按设计
/// 不计错误，用户只看到 502，账号 `error_count` 仍是 0 —— 08-prod P1-1 的
/// 事故形态）。与既有 limiter「本地计数类在计数器接入前恒不命中（宁放勿误伤）」
/// 同口径：**有证据才裁人**。
///
/// ## 名字判定
///
/// 只做**精确相等**：目录条目的 `id`（用户可见名）与 `upstream`（上游真名）
/// 两个键都进表，查询侧用请求的（上游真名, 别名）两个名字去查。不引入第三套
/// 名字等价启发式（前缀 / 大小写 / 别名表）——「这算不算同一个模型」由既有的
/// `AccountSession::supports_model_any` 负责，本索引只回答「覆盖到的那个模型
/// 带什么标签」。
#[derive(Debug, Default)]
pub struct AccountCapabilityIndex {
    /// 是否存在任何「带标签」的条目（粗筛判据：全无证据 = 放行）。
    evidence: bool,
    /// 全部条目能力位的并集（用于「这个账号有没有任何一个模型能满足需求」）。
    any: crate::model::capability::CapabilityMask,
    /// 模型名 → 能力位（`id` 与上游真名两个键都进表，先写入者优先）。
    by_name: HashMap<String, crate::model::capability::CapabilityMask>,
}

impl AccountCapabilityIndex {
    /// 由目录快照构建。
    pub fn from_snapshot(snapshot: &CatalogSnapshot) -> Self {
        let mut index = Self::default();
        for entry in &snapshot.models {
            if entry.capabilities.is_empty() {
                // 老快照（pick_capabilities 上线前写入的）没有能力位：算未知，不进表
                continue;
            }
            let mask = crate::model::capability::CapabilityMask::from_tags(&entry.capabilities);
            index.evidence = true;
            index.any = index.any.union(mask);
            for name in std::iter::once(&entry.id).chain(entry.upstream.as_ref()) {
                index.by_name.entry(name.clone()).or_insert(mask);
            }
        }
        index
    }

    /// 是否有任何能力证据（无 = 该账号的能力位未知）。
    pub fn has_evidence(&self) -> bool {
        self.evidence
    }

    /// 全部条目能力位的并集。
    pub fn any(&self) -> crate::model::capability::CapabilityMask {
        self.any
    }

    /// 按模型名查能力位（未收录返回 `None`）。
    pub fn mask_for(&self, name: &str) -> Option<crate::model::capability::CapabilityMask> {
        self.by_name.get(name).copied()
    }

    /// 判定：这个账号能否服务带 `required` 需求的 `model`（别名兜底）。
    ///
    /// 返回 `None` = **无证据 → 放行**（见上表）；`Some(false)` = 已知不支持。
    /// 两个名字任一命中且满足即放行（与 `supports_model_any` 的「任一命中」
    /// 同形状）；两个名字都已知但都不满足才拒。
    pub fn verdict(
        &self,
        model: &str,
        alias: Option<&str>,
        required: crate::model::capability::CapabilityMask,
    ) -> Option<bool> {
        if required.is_empty() || !self.evidence {
            return None;
        }
        let mut known = false;
        for name in std::iter::once(model).chain(alias) {
            if let Some(mask) = self.mask_for(name) {
                known = true;
                if mask.covers(required) {
                    return Some(true);
                }
            }
        }
        if known {
            Some(false)
        } else {
            None
        }
    }
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
    /// Cline（cline / cline-free / cline-pass）：条目用旧缓存字段
    /// `{id, pool, contextWindow, reasoning, supportsToolCall}`（专用解析见
    /// `parse_cline_catalog`；不改通用字段归一，避免波及其它 provider）。
    Cline,
    /// 小浣熊(raccoon)：`{data:{categories:[{models:[{name,…}]}]}}`（兼容旧
    /// `{data:{models:[{modelId,…}]}}`）。
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
    /// CodeArts：华为云 **SDK-HMAC-SHA256** 签名 + agent/builtin/benefit
    /// 三源合并（不是单 URL；`fetch_remote` 会转交 `fetch_codearts_catalog`）。
    CodeArts,
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
        // CatPaw：`POST /api/agent/maas/model-types`，`{tenant,scene,env}` 是
        // **POST body**（不是 query；见 Agent2API `providers/catpaw/catalog.rs`
        // 的模块头，推翻旧实现「GET」的误读）。旧实现发 GET → 上游 405。
        "catpaw" => Some(CatalogSpec {
            path: "/api/agent/maas/model-types",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: Some(CatalogMethod::Post),
            body: Some(catpaw_catalog_body),
            parser: None,
        }),
        "codearts" => Some(CatalogSpec {
            path: "/v1/model/builtin",
            openai_shape: false,
            fallback: &[],
            // 三源合并 + 每请求签名，交给 `fetch_codearts_catalog`（见下方同名
            // 函数）；这里的 path 只用于「不支持目录」判定与静态兜底判断。
            sign: Some(CatalogSign::CodeArts),
            method: None,
            body: None,
            parser: None,
        }),
        // OfficeAce：`GET {网关}/models`（base 需带 `/v2`，见 `catalog_base_url`
        // 的 officeace 分支 + `officeace_basic_authorization`）。旧实现用
        // `/v1/models` 且把 `secret`(=modelAppKey) 直接塞进 `Basic`，缺
        // `modelAppSecret` 一半 → 上游 400。
        "officeace" => Some(CatalogSpec {
            path: "/models",
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
        // 真实结构 `{data:{categories:[{models:[{name,…}]}]}}`（解析见
        // `parse_raccoon_catalog`）；远程拉不到时用 RACCOON_FALLBACK 兜底。
        "xiaohuanxiong" => Some(CatalogSpec {
            path: "/model_catalog",
            openai_shape: false,
            fallback: RACCOON_FALLBACK,
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
            // 专用解析器：识别 contextWindow / 裸 reasoning / supportsToolCall，
            // 不改通用 pick_*（避免波及其它 provider）。
            parser: Some(CatalogParser::Cline),
        }),
        // ModelScope（魔搭）国内站 / 国际站：OpenAI 兼容的 `/models` 端点，
        // 响应是 `{data:[{id,...}]}`。两个站点各一份清单（账号体系独立）。
        "modelscope" | "modelscope-intl" => Some(CatalogSpec {
            path: "/api/v1/models",
            openai_shape: true,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // 智谱清言（chatglm.cn）：目录端点在插件仓（agent2api-providers），
        // 主仓不再持有端点知识。path 留空 = 只有静态兜底；兜底表也留空
        // （没有上游实测数据，不编造模型名）—— 该家当前显示「不支持目录发现」，
        // 等插件仓端点确定后再补 path 与兜底表。
        "chatglm" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // AtomCode（AtomGit / GitCode）：目录端点在插件仓。同 chatglm，
        // path 与兜底表都留空，等插件仓端点确定后再补。
        "atomcode" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // LobsterAI（网易有道「有道龙虾」）：目录端点在插件仓。同 chatglm，
        // path 与兜底表都留空，等插件仓端点确定后再补。
        "lobsterai" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: &[],
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // DeepSeek 网页版（chat.deepseek.com）：网页版没有 /models 目录端点
        // （ds2api 一系同样只有静态清单）。path 留空 = 只下发静态兜底表；
        // 表内容对齐 ds2api `config/models.go` 的静态模型清单：四个基础档 +
        // 各自的 -nothinking 变体（-search 档强制关思考）。上下文长度上游
        // 未公布，留 0（不编造）。
        "deepseek-web" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: DEEPSEEK_WEB_FALLBACK,
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        // WorkBuddy 国内版 / 国际版：两个地区**共用同一份内置静态目录**
        // （`/v3/config` 远程清单在 Agent2API 插件仓里，主仓只留种子兜底 ——
        // 见 `models/mod.rs::initial_state`，两地兜底共用一份）。`path` 留空 =
        // 没有可拉取的远程目录端点，只下发静态兜底，与 autoclaw / orcaterm /
        // zcode 同一形态。典据：Agent2API `core/models/mod.rs` 的
        // `builtin_models()`（种子清单）。
        "workbuddy" | "workbuddy-intl" => Some(CatalogSpec {
            path: "",
            openai_shape: false,
            fallback: WORKBUDDY_FALLBACK,
            sign: None,
            method: None,
            body: None,
            parser: None,
        }),
        _ => None,
    }
}

/// DeepSeek 网页版兜底表（网页版无目录端点，清单对齐 ds2api 静态表）。
const DEEPSEEK_WEB_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "deepseek-v4-flash",
        upstream_key: "",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-flash-search",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-pro",
        upstream_key: "",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-vision",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-flash-nothinking",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-flash-search-nothinking",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-pro-nothinking",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 0,
    },
    FallbackModel {
        id: "deepseek-v4-vision-nothinking",
        upstream_key: "",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 0,
    },
];

/// AutoClaw 兜底表（上游无远程目录接口）。
///
/// 口径（2026-11-23 实测 `GET …/proxy/autoclaw-model-config` 的 `models[]`）：
/// 目录条目同时给 `id`（路由键，如 `zaicoding_glm-5.3`）与 `name`（展示/模型名，
/// 如 `GLM-5.3`）。**把 `id` 当 model 发上游会被拒（400「非法模型」）**，
/// 故这里 `id`=客户端可见名、`upstream_key`=上游 `name`（不是 `id`/routeId）。
/// `reasoning`/`vision`/`context_window` 仍是能力位与上下文长度。
const AUTOCLAW_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "glm-5.3",
        upstream_key: "GLM-5.3",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 1_048_576,
    },
    FallbackModel {
        id: "glm-5.3-flash",
        upstream_key: "GLM-5.3-FlashX-Legal",
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

/// 小浣熊兜底表（远程目录拉不到时保证离线也有可用清单）。
///
/// id 用上游真名（`/model_catalog` 条目里的 `name`），上下文窗口与能力位取自
/// 上游目录的实测值。远程刷新成功后以远程清单为准，本表只兜底。
const RACCOON_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "raccoon-8c4485",
        upstream_key: "",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "raccoon-19b265",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "raccoon-405a1c",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 1_000_000,
    },
    // 桌面端本地预置的默认对话模型（远程目录通常不含，仅兜底可见）。
    FallbackModel {
        id: "raccoon-chat-ml-5-5",
        upstream_key: "",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 180_000,
    },
    FallbackModel {
        id: "sn-sensenova-6-8-flash-lite",
        upstream_key: "",
        reasoning: true,
        vision: false,
        tool: true,
        context_window: 256_000,
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

/// WorkBuddy（国内版 / 国际版）静态兜底表。
///
/// 移植自 Agent2API `core/models/mod.rs` 的 `builtin_models()`（**WorkBuddy
/// 内置种子目录**；两个地区共用同一份种子，差异只在 `/v3/config` 的远程刷新，
/// 见该文件的 `initial_state`）。WorkBuddy 没有可拉取的 `/models` 目录端点，
/// 故 `spec_for` 的 `path` 留空、只下发本表。
///
/// 字段映射（逐项对齐种子清单）：
/// - `id` ← 种子的 `id`（WorkBuddy 侧即可路由的模型名）；
/// - `upstream_key` 一律空串 —— WorkBuddy 的 id 就是上游真名，路由原样透传；
/// - `reasoning` ← `supportsReasoning`；`vision` ← `supportsImages`；
///   `tool` ← `supportsToolCall`；
/// - `context_window` ← `maxInputTokens`。AIGX 的 [`FallbackModel`] 没有
///   `maxOutputTokens` 字段，本期**不承载输出上限**（见 SPEC §9 的窄口径记录）。
///
/// ## 为什么是 23 条而不是种子的 27 条
///
/// 移植时**按数据裁掉**了被 Agent2API `is_chat_model`（`models/mod.rs:164-186`）
/// 过滤掉的 4 条 —— AIGX 不做运行时过滤（`FallbackModel` 无 `maxOutputTokens`，
/// 硬做只能造假判据）：
/// - `kimi-k2-instruct-taiji` —— 命中前缀黑名单 `kimi-k2-instruct`；
/// - `hunyuan-chat` —— `maxOutputTokens = 8192 < 16000`（网关只服务对话模型）；
/// - `default-1.1` / `default-1.2` —— 命中前缀黑名单 `default-1.`（Claude 内部 id）。
///
/// 这 4 条**不是遗漏**：将来「补全目录」时不要加回来。完整理由见 SPEC §9。
const WORKBUDDY_FALLBACK: &[FallbackModel] = &[
    FallbackModel {
        id: "auto",
        upstream_key: "",
        reasoning: false,
        vision: true,
        tool: true,
        context_window: 168_000,
    },
    FallbackModel {
        id: "default",
        upstream_key: "",
        reasoning: false,
        vision: false,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "deepseek-v4-pro",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "deepseek-v4-flash",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "deepseek-v4.1-flash",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 1_000_000,
    },
    FallbackModel {
        id: "deepseek-v3-2-volc",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 96_000,
    },
    FallbackModel {
        id: "glm-5.1",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "glm-5.0",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "glm-5.0-turbo",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "glm-5v-turbo",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "glm-4.7",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "glm-4.6",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 168_000,
    },
    FallbackModel {
        id: "glm-4.6v",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "minimax-m2.5",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "minimax-m2.7",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 200_000,
    },
    FallbackModel {
        id: "minimax-m3-play",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 512_000,
    },
    FallbackModel {
        id: "kimi-k2.6",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 256_000,
    },
    FallbackModel {
        id: "kimi-k2.5",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 256_000,
    },
    FallbackModel {
        id: "kimi-k2-thinking",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 256_000,
    },
    FallbackModel {
        id: "hy3-preview",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 192_000,
    },
    FallbackModel {
        id: "hy3-preview-agent",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 192_000,
    },
    FallbackModel {
        id: "hunyuan-2.0-instruct",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 128_000,
    },
    FallbackModel {
        id: "hunyuan-2.0-thinking",
        upstream_key: "",
        reasoning: true,
        vision: true,
        tool: true,
        context_window: 128_000,
    },
];

/// 该供应商是否支持目录发现（管理面据此决定是否显示「同步模型」）。
pub fn supports(provider: &str) -> bool {
    spec_for(provider).is_some()
}

/// 无远程目录端点时的静态兜底清单（复用各家的 `CatalogSpec.fallback` 表，
/// 与 raccoon / deepseek-web / autoclaw 等家同一套兜底机制）。
///
/// 返回**空切片** = 该家没有可靠的内置清单 —— 不编造模型名，调用方应提示
/// 「需手动声明模型」。当前 chatglm / codar / qwenwork / tabbit 均属此列。
pub(crate) fn static_models_for(provider: &str) -> &'static [FallbackModel] {
    spec_for(provider).map(|spec| spec.fallback).unwrap_or(&[])
}

/// 这些家在 AIGX 没有可自证的内置模型清单（目录端点在闭源插件 / 家无原生后端），
/// 与其编造模型名，不如明确提示运维手动声明。custom-* 也归此类（用户自填）。
fn provider_needs_manual_models(provider: &str) -> bool {
    matches!(
        provider,
        "chatglm"
            | "codar"
            | "qwenwork"
            | "qwenwork-intl"
            | "tabbit"
            | "tabbit-intl"
            | "atomcode"
            | "lobsterai"
    ) || provider.starts_with("custom-")
}

/// 「需手动声明模型」的统一提示文案（目录无法自动发现且无内置清单时）。
fn manual_declaration_hint(provider: &str) -> String {
    format!("供应商 {provider} 无远程模型目录或内置静态清单，请在账号模型白名单手动声明模型")
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
        "atomcode",
        "autoclaw",
        "autoclaw-intl",
        "catpaw",
        "chatglm",
        "cline",
        "cline-free",
        "cline-pass",
        "codearts",
        "commandcode",
        "kuku",
        "loomy",
        "lobsterai",
        "monkeycode",
        "monkeycode-intl",
        "modelscope",
        "modelscope-intl",
        "officeace",
        "orcaterm",
        "qoder",
        "qoder-intl",
        "trae",
        "workbuddy",
        "workbuddy-intl",
        "xiaohuanxiong",
        "zcode",
        "zcode-intl",
    ]
}

/// 缓存键：按 provider + 账号 id 分格。
pub(crate) fn cache_key(provider: &str, account_id: &str) -> String {
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
    // #40 §1：能力索引随目录一起刷新（本函数是目录缓存的**唯一**写入口，
    // 故索引不存在第二处需要失效的地方）。
    sessions.set_catalog_index(
        account_id,
        Arc::new(AccountCapabilityIndex::from_snapshot(snapshot)),
    );
}

/// 由静态兜底表构造目录快照（[`static_models_for`] 的产物归一）。
fn catalog_from_static(models: &[FallbackModel]) -> CatalogSnapshot {
    CatalogSnapshot {
        models: models
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

/// 解析 Cline 推荐模型目录（专用）。
///
/// Cline 条目用一层独立的旧缓存字段（参考树 `catalog_legacy.rs` 证据）：
/// `id`（含 `cline-free/` / `cline-pass/` 池前缀）、`contextWindow`、裸
/// `reasoning`、`supportsToolCall`。这些键与通用 provider 的归一字段
/// （`maxInputTokens` / `supportsReasoning` / `context_window`）不同，因此在
/// **Cline 分支**用专用解析器，不改通用 `pick_*` —— 通用函数被十多家共用，
/// 直接加裸 `reasoning` / `contextWindow` 有误判风险。
///
/// 顶层条目容器与通用 parser 一致：`{models:[…]}` / `{data:[…]}` / 顶层数组。
fn parse_cline_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let items: Vec<&Value> = payload
        .get("models")
        .and_then(Value::as_array)
        .or_else(|| payload.get("data").and_then(Value::as_array))
        .or_else(|| payload.as_array())
        .map(|items| items.iter().collect())
        .unwrap_or_default();
    let mut out: Vec<CatalogEntry> = Vec::with_capacity(items.len());
    let mut seen: Vec<String> = Vec::with_capacity(items.len());
    for item in items {
        let entry = item
            .get("model")
            .filter(|value| value.is_object())
            .unwrap_or(item);
        let Some(id) = value_text(entry.get("id"))
            .or_else(|| value_text(entry.get("name")))
            .or_else(|| value_text(entry.get("modelId")))
        else {
            continue;
        };
        if seen.iter().any(|known| known.eq_ignore_ascii_case(&id)) {
            continue;
        }
        seen.push(id.clone());

        // 上下文：Cline 旧缓存用 `contextWindow`；兼容更早的归一键名。
        let context = [
            "contextWindow",
            "context_window",
            "maxInputTokens",
            "max_input_tokens",
            "context_length",
            "contextLength",
        ]
        .iter()
        .find_map(|key| value_u32(entry.get(*key)));

        let flag = |keys: &[&str]| {
            keys.iter()
                .any(|key| entry.get(*key).and_then(Value::as_bool) == Some(true))
        };
        let mut capabilities: Vec<String> = Vec::new();
        if flag(&["reasoning", "supportsReasoning", "supports_reasoning"]) {
            capabilities.push("reasoning".to_string());
        }
        if flag(&["supportsImages", "supports_images", "supports_image"]) {
            capabilities.push("vision".to_string());
        }
        if flag(&[
            "supportsToolCall",
            "supports_tool_call",
            "supportsTools",
            "supports_tools",
        ]) {
            capabilities.push("tool".to_string());
        }
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: context,
            capabilities,
        });
    }
    out
}
///
/// 目录端点绝大多数只需要 provider 默认头 + 账号凭证，因此统一走
/// `registry::SessionMeta.headers` 模板（`{credential}` 已替换）与
/// `extra_headers` 合并。**不做逐家签名**：需要签名的目录端点（如 Qoder 的
/// COSY 签名）不在本表内 —— 宁可报失败也不发一个必然被拒的请求。
/// 目录回源用的生效地址：账号级地址优先，空则回落 provider 目录默认值。
///
/// 与对话链路 [`super::adapter::effective_base_url`] **同口径**（同一函数）。
/// 为什么必须走生效地址：登录型账号在控制台里通常留空 `base_url`（默认地址
/// 由 provider 隐含）。若直接读 `session.base_url` 会得到空串 → 回源永远
/// 失败 → 「拉取模型」永远只下发静态兜底，而 UI 却报「已同步」（2026-11-11
/// 生产实测的缺陷）。
fn catalog_base_url(session: &AccountSession) -> Result<String, String> {
    let base =
        super::adapter::effective_base_url(session).map_err(|e| format!("上游地址不可用：{e}"))?;
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("账号未配置上游地址".to_string());
    }
    // OfficeAce 的模型网关固定在 `/v2` 之下（A2A `officeace/chat.rs::normalize_base`：
    // 不以 `/v2` 结尾就补上），目录再拼 `/models`。否则会打成 `/v1/models`
    // 或 `/v2/v1/models` → 上游 400/404。
    if session.provider == "officeace" && !base.ends_with("/v2") {
        return Ok(format!("{base}/v2"));
    }
    Ok(base.to_string())
}

/// 「模型目录为空」错误：带去掉查询串的 URL 与已遮蔽的响应摘要（≤800 字符）。
fn empty_catalog_error(url: &str, value: &Value) -> String {
    let safe_url = url.split('?').next().unwrap_or("");
    // `null` / 空字符串视为「没有响应体」，给出明确标记而不是字面量 `null`。
    let summary = match value {
        Value::Null => String::new(),
        Value::String(text) if text.trim().is_empty() => String::new(),
        _ => body_summary(value, 800),
    };
    let summary = if summary.is_empty() {
        "<空响应体>".to_string()
    } else {
        summary
    };
    format!("上游返回的模型目录为空（{safe_url}；响应摘要: {summary}）")
}

/// 诊断用响应体摘要：遮蔽凭证、压成单行、截断到 `limit` 个字符。
///
/// 仅用于错误文案（目录解析为空时帮运维看清上游真实结构）。JSON 按值递归：
/// 敏感键（token / secret / password / cookie / authorization 等）整值替换为
/// `"[已隐藏]"`；字符串值里疑似 Bearer / JWT / 长 hex / 长 base64 的也遮蔽。
fn body_summary(value: &Value, limit: usize) -> String {
    let masked = mask_json_value(value);
    let text = serde_json::to_string(&masked).unwrap_or_else(|_| masked.to_string());
    truncate_chars(&collapse_whitespace(&text), limit)
}

fn mask_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| {
                    let masked = if is_sensitive_key(key) {
                        Value::String("[已隐藏]".to_string())
                    } else {
                        mask_json_value(child)
                    };
                    (key.clone(), masked)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(mask_json_value).collect()),
        Value::String(text) => Value::String(mask_secret_like(text)),
        other => other.clone(),
    }
}

/// 键名是否属于「整值敏感」（去掉非字母数字后按精确/后缀匹配，覆盖
/// camelCase / snake_case / kebab-case）。涵盖常见 API Key 变体
/// （`secretKey`/`accessKey`/`privateKey`/`x-api-key`）与 `passwd`/`pwd`。
fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "password"
            | "passwd"
            | "pwd"
    ) || normalized.ends_with("token")
        || normalized.ends_with("secret")
        || normalized.ends_with("password")
        || normalized.ends_with("apikey")
        || normalized.ends_with("accesskey")
        || normalized.ends_with("secretkey")
        || normalized.ends_with("privatekey")
}

/// 单个字符串值里的凭证遮蔽：逐 token 判断，覆盖「Bearer <token>」与嵌入在
/// 长文本中的 JWT / 长 hex / 长 base64（整值不像凭证也不放过句子里的片段）。
fn mask_secret_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    for (index, token) in text.split_whitespace().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let bare = token.trim_matches(|ch: char| {
            matches!(
                ch,
                '"' | '\'' | ',' | ';' | ':' | '{' | '}' | '[' | ']' | '(' | ')'
            )
        });
        let masked = after_bearer
            || looks_like_jwt(bare)
            || looks_like_long_hex(bare)
            || looks_like_long_base64(bare)
            || looks_like_api_key(bare);
        after_bearer = bare.eq_ignore_ascii_case("bearer");
        if masked {
            out.push_str("[已隐藏]");
        } else {
            out.push_str(token);
        }
    }
    out
}

/// 常见 API Key 前缀（短值也要遮蔽，例如 `sk-abc123`）。
fn looks_like_api_key(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("sk-") && text.len() > 3)
        || (lower.starts_with("ghp_") && text.len() > 4)
        || (lower.starts_with("xoxb-") && text.len() > 5)
        || (text.starts_with("AKIA") && text.len() >= 12)
}

fn looks_like_jwt(text: &str) -> bool {
    let mut parts = text.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(header), Some(payload), Some(sig), None)
            if header.starts_with("eyJ") && !payload.is_empty() && !sig.is_empty()
    )
}

fn looks_like_long_hex(text: &str) -> bool {
    text.len() >= 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn looks_like_long_base64(text: &str) -> bool {
    text.len() >= 64
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
        })
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let keep = limit.saturating_sub(1);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push('…');
    truncated
}

// ── CodeArts：SDK-HMAC-SHA256 签名 + agent/builtin/benefit 三源目录 ─────────
//
// 对齐 Agent2API `providers/codearts/models.rs`：三个源按 id **先到先得** 合并，
// 任一源失败只记日志不中断；每请求用 bridge 里同一套 `sign_v4`（避免两处算法
// 分叉）。目录端点无路径转义，`Url::path()` 与上游 Go 语义等价。

const CODEARTS_PLUGIN_NAME: &str = "snap_vscode";
const CODEARTS_PLUGIN_VERSION: &str = "26.9.101";
const CODEARTS_LANGUAGE: &str = "en-us";
/// 福利网关（另一台主机、签名带 Host、无 domain/agent 头）。
const CODEARTS_BENEFIT_BASE: &str = "https://opengw.developer.huaweicloud.com";

fn codearts_sdk_date() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// 一组目录请求头（**全部参与签名**）。`sts`/`domain` 非空才带；`include_host`
/// 仅福利网关开（snap-access 不带）。
fn codearts_headers(
    agent_type: &str,
    include_host: bool,
    host: &str,
    sts: &str,
    domain: &str,
    domainless: bool,
) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("X-Language".to_string(), CODEARTS_LANGUAGE.to_string()),
        ("plugin-name".to_string(), CODEARTS_PLUGIN_NAME.to_string()),
        (
            "plugin-version".to_string(),
            CODEARTS_PLUGIN_VERSION.to_string(),
        ),
        (
            "client_version".to_string(),
            format!("Vscode_{CODEARTS_PLUGIN_VERSION}"),
        ),
        ("is_confidential".to_string(), "false".to_string()),
        ("X-Sdk-Date".to_string(), codearts_sdk_date()),
    ];
    if !agent_type.is_empty() {
        headers.push(("Agent-Type".to_string(), agent_type.to_string()));
    }
    if !sts.trim().is_empty() {
        headers.push(("X-Security-Token".to_string(), sts.trim().to_string()));
    }
    if !domainless && !domain.trim().is_empty() {
        headers.push(("X-Domain-Id".to_string(), domain.trim().to_string()));
    }
    if include_host && !host.trim().is_empty() {
        headers.push(("Host".to_string(), host.trim().to_string()));
    }
    headers
}

/// 签名并执行一次 GET，返回解析后的 JSON（GET 恒签空体摘要）。
async fn codearts_get(
    client: &reqwest::Client,
    url: &str,
    headers: Vec<(String, String)>,
    access_key: &str,
    secret_key: &str,
) -> Result<Value, String> {
    let mut headers = headers;
    let authorization = crate::bridge::session::codearts::sign_v4(
        "GET", url, &headers, b"", access_key, secret_key,
    )?;
    headers.push(("Authorization".to_string(), authorization));
    let mut req = client
        .get(url)
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));
    for (name, value) in &headers {
        req = req.header(name.as_str(), value.as_str());
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
    serde_json::from_str(&text).map_err(|e| format!("响应不是合法 JSON：{e}"))
}

fn codearts_text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn codearts_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number > 0)
}

fn codearts_caps(images: bool) -> Vec<String> {
    let mut caps = Vec::new();
    if images {
        caps.push("vision".to_string());
    }
    caps
}

/// `/v1/model/builtin` → 条目（`enable` 显式 false 才跳过）。
fn parse_codearts_builtin(payload: &Value) -> Vec<CatalogEntry> {
    let Some(entries) = payload.get("builtinModels").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        if entry.get("enable").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let model_id = codearts_text(entry, "model_id");
        let id = if model_id.is_empty() {
            codearts_text(entry, "model_name")
        } else {
            model_id
        };
        if id.is_empty() {
            continue;
        }
        let images = entry
            .get("supports_images")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: codearts_u32(entry, "context_window"),
            capabilities: codearts_caps(images),
        });
    }
    out
}

/// `/v1/agent-center/agents/useragents` → agent id 列表（去重）。
fn parse_codearts_agent_ids(payload: &Value) -> Vec<String> {
    let Some(agents) = payload.get("agents").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = Vec::new();
    for agent in agents {
        let agent_id = codearts_text(agent, "agent_id");
        let id = if agent_id.is_empty() {
            codearts_text(agent, "original_id")
        } else {
            agent_id
        };
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// agent detail 的 `gpts.models` → 条目。
///
/// 两条过滤照参考实现：`enabled == false` 跳过；**`display_enabled` 缺失也算
/// 不可见**（实测 trial 账号的 detail 正是如此）。
fn parse_codearts_agent_detail(payload: &Value) -> Vec<CatalogEntry> {
    let Some(entries) = payload.pointer("/gpts/models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        let parameters = entry
            .get("model_parameters")
            .cloned()
            .unwrap_or(Value::Null);
        if parameters.get("enabled").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        if parameters.get("display_enabled").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        // 扩展把 model_parameters 摊在 model_alias 上，嵌套 model_id 优先。
        let nested = codearts_text(&parameters, "model_id");
        let id = if !nested.is_empty() {
            nested
        } else {
            let alias = codearts_text(entry, "model_alias");
            if alias.is_empty() {
                codearts_text(entry, "model_id")
            } else {
                alias
            }
        };
        if id.is_empty() {
            continue;
        }
        let images = parameters
            .get("supports_images")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: codearts_u32(&parameters, "context_window"),
            capabilities: codearts_caps(images),
        });
    }
    out
}

/// `/v1/benefit-gateway-config` → 总开关（缺字段报错，不静默当关）。
fn parse_codearts_benefit_gate(payload: &Value) -> Result<bool, String> {
    payload
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "福利开关响应没有 enabled 字段".to_string())
}

/// 福利网关 `result.models` → 条目（成功看体里 `error_code == "0000"`，不看 HTTP）。
fn parse_codearts_benefit(payload: &Value) -> Result<Vec<CatalogEntry>, String> {
    let code = codearts_text(payload, "error_code");
    if code != "0000" {
        return Err(format!("福利目录未成功（error_code={code:?}）"));
    }
    let Some(entries) = payload.pointer("/result/models").and_then(Value::as_array) else {
        return Err("福利目录缺少 result.models".to_string());
    };
    let mut rows: Vec<(i64, &Value)> = entries
        .iter()
        .map(|entry| {
            (
                entry.get("sort").and_then(Value::as_i64).unwrap_or(0),
                entry,
            )
        })
        .collect();
    rows.sort_by_key(|(sort, _)| *sort);
    let mut out = Vec::new();
    for (_, entry) in rows {
        let id = codearts_text(entry, "model_id");
        if id.is_empty() {
            continue;
        }
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: codearts_u32(entry, "context_window"),
            capabilities: Vec::new(),
        });
    }
    Ok(out)
}

/// 按 id 先到先得合并（前者已报过的 id 不再被后者覆盖）。
fn merge_codearts(mut target: Vec<CatalogEntry>, incoming: Vec<CatalogEntry>) -> Vec<CatalogEntry> {
    for model in incoming {
        if !target.iter().any(|existing| existing.id == model.id) {
            target.push(model);
        }
    }
    target
}

/// 分页拉 agent id（100/页，最多 100 页，与参考实现同口径）。
async fn fetch_codearts_agent_ids(
    client: &reqwest::Client,
    base: &str,
    access_key: &str,
    secret_key: &str,
    sts: &str,
    domain: &str,
) -> Result<Vec<String>, String> {
    const PAGE: usize = 100;
    let mut ids: Vec<String> = Vec::new();
    for page in 0..100usize {
        let offset = page * PAGE;
        let url = format!(
            "{base}/v1/agent-center/agents/useragents?offset={offset}&limit={PAGE}&is_primary_agent=true&supported_client=VSCODE&min_compatible_plugin_version={CODEARTS_PLUGIN_VERSION}"
        );
        let headers = codearts_headers("AgentCenter", false, "", sts, domain, false);
        let payload = codearts_get(client, &url, headers, access_key, secret_key).await?;
        let page_ids = parse_codearts_agent_ids(&payload);
        let count = page_ids.len();
        for id in page_ids {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if count < PAGE {
            break;
        }
    }
    Ok(ids)
}

/// 解析 CodeArts 凭据，兼容两种落库形态：
///   - **导入形态**：`secret` = AK，`secret_access_key`/`security_token`/`domain_id`
///     在具名字段（A2A 迁移来的账号）；
///   - **原生形态**：`secret` = `AK:SK[:STS]`（冒号或换行分隔），字段多为空。
///
/// 返回 `(AK, SK, STS, domain)`；缺 SK 由调用方判为「凭据不完整」。
fn codearts_credentials(session: &AccountSession) -> (String, String, String, String) {
    let raw = session.credential.secret_plain();
    let field_sk = session.credential.field_plain("secret_access_key");
    let field_sts = session.credential.field_plain("security_token");
    let field_domain = session.credential.field_plain("domain_id");
    if !field_sk.trim().is_empty() {
        return (
            raw.trim().to_string(),
            field_sk.trim().to_string(),
            field_sts.trim().to_string(),
            field_domain.trim().to_string(),
        );
    }
    let parts: Vec<&str> = raw
        .split([':', '\n'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let ak = parts.first().copied().unwrap_or("").to_string();
    let sk = parts.get(1).copied().unwrap_or("").to_string();
    let raw_sts = parts.get(2).copied().unwrap_or("");
    let sts = if field_sts.trim().is_empty() {
        raw_sts.to_string()
    } else {
        field_sts.trim().to_string()
    };
    (ak, sk, sts, field_domain)
}

/// CodeArts 目录：agent → builtin → benefit 三源合并（见模块内注释）。
async fn fetch_codearts_catalog(
    sessions: &AccountSessionStore,
    session: &AccountSession,
) -> Result<Vec<CatalogEntry>, String> {
    // 凭证：兼容导入形态（secret=AK + 具名字段）与原生形态（secret=AK:SK:STS）。
    let (access_key, secret_key, sts, domain) = codearts_credentials(session);
    if access_key.trim().is_empty() || secret_key.trim().is_empty() {
        return Err(
            "codearts 凭据不完整（缺 access_key / secret_access_key），无法签名目录请求"
                .to_string(),
        );
    }
    let base = catalog_base_url(session)?;
    let base = base.trim_end_matches('/').to_string();
    let client = sessions
        .client_for(session)
        .map_err(|e| format!("出口客户端不可用：{e}"))?;

    // 1) agent：先列 id，再逐个取 detail。
    let mut agent_models: Vec<CatalogEntry> = Vec::new();
    match fetch_codearts_agent_ids(&client, &base, &access_key, &secret_key, &sts, &domain).await {
        Ok(ids) => {
            for id in ids {
                let query = url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("agent_id", &id)
                    .finish();
                let url = format!("{base}/v1/agent-center/agents/detail?{query}");
                let headers = codearts_headers("AgentCenter", false, "", &sts, &domain, false);
                match codearts_get(&client, &url, headers, &access_key, &secret_key).await {
                    Ok(payload) => agent_models.extend(parse_codearts_agent_detail(&payload)),
                    Err(e) => tracing::warn!("codearts agent detail {id} 失败：{e}"),
                }
            }
        }
        Err(e) => tracing::warn!("codearts agent 列表失败：{e}"),
    }

    // 2) builtin（多模态只在这里出现）。
    let builtin = {
        let url = format!("{base}/v1/model/builtin");
        let headers = codearts_headers("PromptCenter", false, "", &sts, &domain, false);
        match codearts_get(&client, &url, headers, &access_key, &secret_key).await {
            Ok(payload) => parse_codearts_builtin(&payload),
            Err(e) => {
                tracing::warn!("codearts builtin 失败：{e}");
                Vec::new()
            }
        }
    };
    let mut models = merge_codearts(agent_models, builtin);

    // 3) benefit：先判总开关，开了再取福利网关目录（带 Host、无 domain/agent 头）。
    let gate_url = format!("{base}/v1/benefit-gateway-config");
    let gate_headers = codearts_headers("PromptCenter", false, "", &sts, &domain, false);
    match codearts_get(&client, &gate_url, gate_headers, &access_key, &secret_key).await {
        Ok(gate) => match parse_codearts_benefit_gate(&gate) {
            Ok(true) => {
                let url = format!("{CODEARTS_BENEFIT_BASE}/api/v1/gateway/config");
                let host = url::Url::parse(&url)
                    .ok()
                    .and_then(|parsed| parsed.host_str().map(str::to_string))
                    .unwrap_or_default();
                let headers = codearts_headers("", true, &host, &sts, &domain, true);
                match codearts_get(&client, &url, headers, &access_key, &secret_key).await {
                    Ok(payload) => match parse_codearts_benefit(&payload) {
                        Ok(benefit) => models = merge_codearts(models, benefit),
                        Err(e) => tracing::warn!("codearts 福利目录解析失败：{e}"),
                    },
                    Err(e) => tracing::warn!("codearts 福利目录失败：{e}"),
                }
            }
            Ok(false) => {}
            Err(e) => tracing::warn!("codearts 福利开关解析失败：{e}"),
        },
        Err(e) => tracing::warn!("codearts 福利开关失败：{e}"),
    }

    if models.is_empty() {
        return Err("codearts 三个目录源都没有返回可用模型".to_string());
    }
    Ok(models)
}

async fn fetch_remote(
    sessions: &AccountSessionStore,
    session: &AccountSession,
    spec: &CatalogSpec,
) -> Result<Vec<CatalogEntry>, String> {
    // CodeArts 不是单 URL：每请求签名 + 三个模型源合并，走专用路径。
    if spec.sign == Some(CatalogSign::CodeArts) {
        return fetch_codearts_catalog(sessions, session).await;
    }
    let base = catalog_base_url(session)?;
    let base = base.as_str();
    // `path` 为空 = 该家只有静态兜底（workbuddy / autoclaw / orcaterm / zcode
    // 这类没有可拉取远程目录的）。不去 GET 站点根：那是无意义的请求，还可能把
    // SPA 的 HTML 当目录解析，直接失败让调用方走 `fallback`。
    if spec.path.is_empty() {
        return Err(format!("供应商 {} 无远程目录端点", session.provider));
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

    // ── 逐 provider 的目录鉴权/身份头补齐（只影响目录请求，不动 chat 路径）──
    // OfficeAce：`Basic base64(modelAppKey:modelAppSecret)`（缺一半 → 上游 400）。
    if session.provider == "officeace" {
        if let Some(basic) = super::adapter::officeace_basic_authorization(session) {
            headers.retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
            headers.push(("Authorization".to_string(), basic));
        }
    }
    // CatPaw：`M-TRACEID` 每次请求一个新随机 UUID（A2A `catpaw/upstream_http.rs`
    // 每条请求都生成，去掉连字符）。
    if session.provider == "catpaw" {
        headers.push((
            "m-traceid".to_string(),
            uuid::Uuid::new_v4().simple().to_string(),
        ));
    }
    // Trae：uid / machineId / deviceId 是独立请求头（A2A `trae/headers.rs`）；
    // 为空就不发（上游把空值当「另一个身份」）。
    if session.provider == "trae" {
        for (field, header) in [
            ("uid", "x-uid"),
            ("machineId", "x-machine-id"),
            ("deviceId", "x-device-id"),
        ] {
            let value = session.credential.field_plain(field);
            let value = value.trim();
            if !value.is_empty()
                && !headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(header))
            {
                headers.push((header.to_string(), value.to_string()));
            }
        }
    }
    // Antigravity：非 content 方法（含目录）带 `x-goog-user-project`
    // （A2A `antigravity/endpoints.rs::admin_headers`；project 为空则不发）。
    if session.provider == "antigravity" {
        let project = session.credential.field_plain("projectId");
        let project = project.trim();
        if !project.is_empty()
            && !headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("x-goog-user-project"))
        {
            headers.push(("x-goog-user-project".to_string(), project.to_string()));
        }
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
        Some(CatalogParser::Cline) => parse_cline_catalog(&value),
        Some(CatalogParser::Raccoon) => parse_raccoon_catalog(&value),
        Some(CatalogParser::Kuku) => parse_kuku_catalog(&value),
        None if spec.sign == Some(CatalogSign::Cosy) => parse_qoder_chat(&value),
        None => parse_response(&value, spec.openai_shape),
    };
    if models.is_empty() {
        return Err(empty_catalog_error(&url, &value));
    }
    Ok(models)
}

/// CatPaw 目录请求体（A2A `providers/catpaw/catalog.rs::request_body` 逐字）：
/// `tenant/scene/env` 是**客户端身份三常量**，发错拿不到目录。
fn catpaw_catalog_body() -> Value {
    serde_json::json!({
        "tenant": "CatDesk",
        "scene": "CATX_APP",
        "env": "EXTERNAL",
    })
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

/// 解析小浣熊的模型目录。
///
/// 真实形态（实测）：`{data:{categories:[{type, default_model, models:[…]}]}}`，
/// 单条模型的 id 在 `name` 字段；上下文窗口 `context_window` / 输出上限
/// `max_tokens` 可能嵌在 `params` 里、也可能在顶层（两级都要看）；视觉能力由
/// `tags`（vision / image / image-understanding）或 `supportImage` 推断；
/// 上游目录一般不填 `supportThinking`，缺省按 true 处理。
///
/// 兼容旧形态 `{data:{models:[{modelId|id,…}]}}` 与根级 `models`。同名模型
/// 首次出现者胜，空 id 丢弃。
fn parse_raccoon_catalog(payload: &Value) -> Vec<CatalogEntry> {
    let data = payload
        .get("data")
        .filter(|value| value.is_object())
        .unwrap_or(payload);

    // 主形态 data.categories[].models[]；无 categories 时回落到 data.models / 根 models。
    let models: Vec<&Value> = match data.get("categories").and_then(Value::as_array) {
        Some(categories) => categories
            .iter()
            .filter_map(|category| category.get("models").and_then(Value::as_array))
            .flatten()
            .collect(),
        None => data
            .get("models")
            .or_else(|| payload.get("models"))
            .and_then(Value::as_array)
            .map(|items| items.iter().collect())
            .unwrap_or_default(),
    };

    let mut out: Vec<CatalogEntry> = Vec::with_capacity(models.len());
    let mut seen: Vec<String> = Vec::with_capacity(models.len());
    for model in models {
        let Some(id) = value_text(model.get("name"))
            .or_else(|| value_text(model.get("model_name")))
            .or_else(|| value_text(model.get("modelId")))
            .or_else(|| value_text(model.get("id")))
        else {
            continue;
        };
        if seen.iter().any(|known| known == &id) {
            continue;
        }
        seen.push(id.clone());

        let params = model.get("params").filter(|value| value.is_object());
        // 上下文窗口优先 context_window；缺失时退化到 max_tokens（两者都可能嵌套 params）。
        let pick = |keys: &[&str]| -> Option<u32> {
            for key in keys {
                if let Some(number) = value_u32(model.get(*key)) {
                    return Some(number);
                }
                if let Some(number) = value_u32(params.and_then(|params| params.get(*key))) {
                    return Some(number);
                }
            }
            None
        };
        let context = pick(&[
            "context_window",
            "contextWindow",
            "maxInputTokens",
            "contextLength",
            "context_length",
        ])
        .or_else(|| pick(&["max_tokens", "maxTokens"]));

        let tags: Vec<String> = model
            .get("tags")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|tag| tag.trim().to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let vision = ["vision", "image", "image-understanding"]
            .iter()
            .any(|wanted| tags.iter().any(|tag| tag == wanted))
            || model.get("supportImage").and_then(Value::as_bool) == Some(true)
            || model.get("supportsImages").and_then(Value::as_bool) == Some(true)
            || model.get("is_vl").and_then(Value::as_bool) == Some(true);
        // 上游目录一般不填 supportThinking；缺省按 true（与上游 normalize 同口径）。
        let reasoning = model
            .get("supportThinking")
            .and_then(Value::as_bool)
            .or_else(|| model.get("supportsReasoning").and_then(Value::as_bool))
            .or_else(|| model.get("is_reasoning").and_then(Value::as_bool))
            .unwrap_or(true);

        let mut capabilities: Vec<String> = Vec::new();
        if reasoning {
            capabilities.push("reasoning".to_string());
        }
        if vision {
            capabilities.push("vision".to_string());
        }
        capabilities.push("tool".to_string());
        out.push(CatalogEntry {
            id,
            upstream: None,
            context_length: context,
            capabilities,
        });
    }
    out
}

/// 宽松取字符串：字符串去空白、数字转字符串；空串视为 None。
fn value_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => {
            let trimmed = text.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// 宽松取无符号整数（接受 JSON 数字或数字字符串）。
fn value_u32(value: Option<&Value>) -> Option<u32> {
    let value = value?;
    let number = value.as_u64().or_else(|| {
        value
            .as_str()
            .and_then(|text| text.trim().parse::<u64>().ok())
    })?;
    u32::try_from(number).ok()
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
        // 无目录端点、也无内置静态清单（codar/qwenwork/tabbit/custom-* 等）：
        // 与其报「不支持」，不如明确告诉运维该手动声明模型。
        if provider_needs_manual_models(&session.provider) {
            return Err(manual_declaration_hint(&session.provider));
        }
        return Err(format!("供应商 {} 不支持模型目录发现", session.provider));
    };

    if !force {
        if let Some(cached) = load_cache(sessions, &session.provider, &session.id) {
            let mut snap = cached;
            snap.source = "cache".to_string();
            return Ok(snap);
        }
    }

    // 纯静态兜底家（path 空 + 有兜底表，如 deepseek-web）：无需打网络，
    // 直接下发内置清单（对齐 raccoon「远程失败回退静态」的终态）。
    let static_models = static_models_for(&session.provider);
    if spec.path.is_empty() && !static_models.is_empty() {
        return Ok(catalog_from_static(static_models));
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
            let static_models = static_models_for(&session.provider);
            if static_models.is_empty() {
                // path 空 + 无兜底表（chatglm / atomcode / lobsterai）：
                // 没有可自证的清单，明确提示手动声明，而不是抛原始网络错误。
                if spec.path.is_empty() {
                    return Err(manual_declaration_hint(&session.provider));
                }
                return Err(err);
            }
            Ok(catalog_from_static(static_models))
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

/// 账号模型清单的可见性状态（SPEC §2.2 冻结契约）。
///
/// 四态：`whitelist`（白名单非空）/ `catalog`（白名单空但目录缓存有内容）/
/// `none`（两者都空且该家**不支持**目录发现）/ `failed`（支持发现却拿不到目录，
/// `error` 是人类可读原因）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsStatus {
    pub state: String,
    pub count: usize,
    pub error: Option<String>,
}

/// 计算某账号的模型清单可见性状态（管理面账号池展示用）。
///
/// 口径与 `build_view` / `collect_available_models` 的模型回落**一致**：
/// 白名单优先；空则看目录缓存；再空且该家不支持发现 = `none`；支持发现却
/// 拿不到目录 = `failed`。缓存按**存在性**读（不看 TTL）——它是展示兜底，
/// 不是新鲜度判据（新鲜度由 `sync_account_catalog` 自己把关）。
pub fn models_status(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
    whitelist_count: usize,
) -> ModelsStatus {
    if whitelist_count > 0 {
        return ModelsStatus {
            state: "whitelist".to_string(),
            count: whitelist_count,
            error: None,
        };
    }
    let cached = cached_model_ids(sessions, provider, account_id);
    if !cached.is_empty() {
        return ModelsStatus {
            state: "catalog".to_string(),
            count: cached.len(),
            error: None,
        };
    }
    if !supports(provider) {
        // 无目录端点：对已知「无内置清单」的家给明确提示（不假装有模型）；
        // 其余 API-Key 型家仍按「none」静默（它们可走通用 `discover_models_plain`）。
        return ModelsStatus {
            state: "none".to_string(),
            count: 0,
            error: provider_needs_manual_models(provider)
                .then(|| manual_declaration_hint(provider)),
        };
    }
    // 支持发现却两者皆空：最近一次探测失败，或还没探测过。
    let error = last_catalog_error(sessions, provider, account_id)
        .unwrap_or_else(|| "尚未同步模型目录（可点「同步模型」重试）".to_string());
    ModelsStatus {
        state: "failed".to_string(),
        count: 0,
        error: Some(error),
    }
}

/// 读某账号目录缓存里的模型 id 列表（按**存在性**读，不看 TTL）。
///
/// 供 `build_view` / `collect_available_models` 在白名单为空时回落 —— 两处
/// **同一口径**（SPEC §3.3），否则客户 API 与控制台的模型清单又会分叉。
pub fn cached_model_ids(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
) -> Vec<String> {
    sessions
        .store()
        .get::<CatalogSnapshot>(&cache_key(provider, account_id))
        .ok()
        .flatten()
        .map(|snapshot| snapshot.models.into_iter().map(|model| model.id).collect())
        .unwrap_or_default()
}

/// 记录 / 清除某账号最近一次目录探测的失败原因。
///
/// 落盘键 `model_catalog_error:{provider}:{account_id}`（SPEC §3.2 的建议键）。
/// 成功时传 `None` 清除，失败时传原因。写失败不影响主流程（纯展示数据）。
pub fn record_catalog_error(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
    error: Option<&str>,
) {
    let store = sessions.store();
    match error {
        Some(message) if !message.trim().is_empty() => {
            let _ = store.put(&error_key(provider, account_id), &message.to_string());
        }
        _ => {
            let _ = store.delete(&error_key(provider, account_id));
        }
    }
}

/// 读某账号最近一次目录探测的失败原因（无记录返回 `None`）。
pub fn last_catalog_error(
    sessions: &AccountSessionStore,
    provider: &str,
    account_id: &str,
) -> Option<String> {
    sessions
        .store()
        .get::<String>(&error_key(provider, account_id))
        .ok()
        .flatten()
        .filter(|message| !message.trim().is_empty())
}

/// 最近一次目录探测失败原因的落盘键。
fn error_key(provider: &str, account_id: &str) -> String {
    format!("model_catalog_error:{provider}:{account_id}")
}

/// 建号后异步补一次目录发现（**直连建号与登录型建号共用**，唯一实现）。
///
/// 白名单为空的新账号若不做这一步，模型管理里一行都看不到（账号型渠道的模型
/// 只来自 `session.models`，落回 `channel.effective_models()` 也是空）。
/// `sync_account_catalog` 覆盖登录态供应商（含静态兜底）；它不认的 API-Key 型
/// 供应商再退回通用 [`discover_models_plain`]。
///
/// 失败**不再静默**：写 `tracing::warn!`，并把原因落到 `failed` 态
/// （落盘键见 [`error_key`]，由 [`models_status`] 读回）。
pub fn spawn_catalog_probe(sessions: AccountSessionStore, account_id: String) {
    tokio::spawn(async move {
        run_catalog_probe(&sessions, &account_id).await;
    });
}

/// [`spawn_catalog_probe`] 的实际逻辑体。
async fn run_catalog_probe(sessions: &AccountSessionStore, account_id: &str) {
    let Some(session) = sessions.get(account_id) else {
        return;
    };
    let provider = session.provider.clone();
    let found = match sync_account_catalog(sessions, &session, true).await {
        Ok(snapshot) => Ok(snapshot
            .models
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>()),
        Err(_) => discover_models_plain(sessions, &session).await,
    };
    match found {
        Ok(models) if !models.is_empty() => {
            record_catalog_error(sessions, &provider, account_id, None);
            if let Err(e) = apply_catalog_to_session(sessions, account_id, &models, false) {
                tracing::warn!("账号 {account_id} 写入模型清单失败：{e}");
            }
        }
        // 上游确实没有可见模型：不写白名单，也不标失败
        Ok(_) => {}
        Err(e) => {
            record_catalog_error(sessions, &provider, account_id, Some(&e));
            tracing::warn!("账号 {account_id} 自动模型发现失败：{e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 「拉取模型」的回源地址必须走 **provider 默认值回落**。
    ///
    /// 回归护栏（2026-11-11 生产缺陷）：登录型账号在控制台里 base_url 留空，
    /// 若回源直接读 `session.base_url` 就会拿到空串 —— 回源永远失败、只下发
    /// 静态兜底，而 UI 仍报「已同步」。把 `catalog_base_url` 里的
    /// `effective_base_url` 换回 `session.base_url`，本测必红。
    #[test]
    fn catalog_base_url_falls_back_to_provider_default() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "workbuddy".to_string();
        assert_eq!(
            catalog_base_url(&session).unwrap(),
            "https://copilot.tencent.com"
        );

        // 账号级地址优先，且去掉末尾斜杠（拼路径靠它）
        session.base_url = "https://my.proxy/v1/".to_string();
        assert_eq!(catalog_base_url(&session).unwrap(), "https://my.proxy/v1");
    }

    /// CatPaw 目录必须是 **POST + 客户端身份 body**（旧实现 GET → 上游 405）。
    #[test]
    fn catpaw_catalog_is_post_with_client_identity_body() {
        let spec = spec_for("catpaw").expect("catpaw spec");
        assert_eq!(spec.method, Some(CatalogMethod::Post));
        let body = (spec.body.expect("catpaw body"))();
        assert_eq!(body["tenant"], "CatDesk");
        assert_eq!(body["scene"], "CATX_APP");
        assert_eq!(body["env"], "EXTERNAL");
    }

    /// OfficeAce：目录路径 `/models`、base 补 `/v2`、鉴权 = `Basic(base64(key:secret))`。
    #[test]
    fn officeace_catalog_url_and_basic_auth() {
        use base64::Engine;
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "officeace".to_string();
        // base 不带 /v2 → 补上；已带 /v2 → 不重复。
        session.base_url = "https://modelgw.example.com".to_string();
        assert_eq!(
            catalog_base_url(&session).unwrap(),
            "https://modelgw.example.com/v2"
        );
        session.base_url = "https://modelgw.example.com/v2".to_string();
        assert_eq!(
            catalog_base_url(&session).unwrap(),
            "https://modelgw.example.com/v2"
        );
        // 目录路径是 /models（不是 /v1/models）
        assert_eq!(spec_for("officeace").unwrap().path, "/models");
        // Basic = base64(modelAppKey:modelAppSecret)
        session.credential.secret = "APPKEY".to_string();
        session.credential.set_field("modelAppSecret", "APPSECRET");
        let got = super::super::adapter::officeace_basic_authorization(&session).expect("basic");
        let want = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(b"APPKEY:APPSECRET")
        );
        assert_eq!(got, want);
        // 缺一半 → None（不伪造一个空 secret 的 Basic）
        session.credential.set_field("modelAppSecret", "");
        assert!(super::super::adapter::officeace_basic_authorization(&session).is_none());
    }

    /// Trae / Antigravity 的目录仍是 POST + 固定 body（版本头见 registry 测试）。
    #[test]
    fn trae_and_antigravity_catalog_methods() {
        let trae = spec_for("trae").expect("trae spec");
        assert_eq!(trae.method, Some(CatalogMethod::Post));
        assert_eq!(
            (trae.body.expect("trae body"))()["function"],
            "solo_work_lite"
        );
        let antigravity = spec_for("antigravity").expect("antigravity spec");
        assert_eq!(antigravity.method, Some(CatalogMethod::Post));
        assert_eq!(
            (antigravity.body.expect("antigravity body"))(),
            serde_json::json!({})
        );
    }

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

    /// Cline 旧缓存字段（参考树 `catalog_legacy.rs`）：`contextWindow` /
    /// 裸 `reasoning` / `supportsToolCall` —— 由 **Cline 专用 parser** 识别。
    #[test]
    fn cline_legacy_fields_populate_context_and_reasoning() {
        let payload = serde_json::json!({ "data": [
            {
                "id": "cline-pass/some-model",
                "pool": "cline-pass",
                "contextWindow": 262_144,
                "reasoning": true,
                "supportsToolCall": true
            },
            // 兼容更早字段名（不应误伤/误报）
            {
                "id": "cline-free/plain-model",
                "maxInputTokens": 128_000,
                "supportsReasoning": true
            }
        ]});
        let entries = parse_cline_catalog(&payload);
        assert_eq!(entries.len(), 2);

        let first = entries
            .iter()
            .find(|entry| entry.id == "cline-pass/some-model")
            .expect("应解析出 Cline 条目");
        assert_eq!(first.context_length, Some(262_144));
        assert!(first.capabilities.iter().any(|c| c == "reasoning"));
        assert!(first.capabilities.iter().any(|c| c == "tool"));

        let second = entries
            .iter()
            .find(|entry| entry.id == "cline-free/plain-model")
            .expect("应解析出旧字段条目");
        assert_eq!(second.context_length, Some(128_000));
        assert!(second.capabilities.iter().any(|c| c == "reasoning"));
    }

    /// 边界：Cline 专用字段**不得**被通用 parser 采纳（证明改动只落在 Cline 分支）。
    /// 注意 `supportsToolCall` 是通用 parser 既有的 key（原行为），这里只断言本轮
    /// 新增的两个字段（`contextWindow` / 裸 `reasoning`）不被通用 parser 识别。
    #[test]
    fn generic_parser_does_not_consume_cline_specific_fields() {
        let payload = serde_json::json!({ "models": [
            {
                "id": "generic-model",
                "contextWindow": 262_144,
                "reasoning": true,
                "supportsToolCall": true
            }
        ]});
        let entries = parse_response(&payload, false);
        assert_eq!(entries.len(), 1);
        // contextWindow 不被通用 pick_context_length 采纳（本轮已回退）
        assert_eq!(entries[0].context_length, None);
        // 裸 reasoning 不被通用 pick_capabilities 采纳（本轮已回退）
        assert!(!entries[0].capabilities.iter().any(|c| c == "reasoning"));
        // supportsToolCall 是既有通用行为，保持不变
        assert!(entries[0].capabilities.iter().any(|c| c == "tool"));
    }

    /// Cline spec 用专用 parser（不是通用 parse_response）。
    #[test]
    fn cline_spec_uses_dedicated_parser() {
        for provider in ["cline", "cline-free", "cline-pass"] {
            let spec = spec_for(provider).expect("cline 系列应有目录 spec");
            assert_eq!(spec.parser, Some(CatalogParser::Cline), "{provider}");
        }
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
        let snapshot = catalog_from_static(spec.fallback);
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
    fn codearts_three_sources_parse_and_merge() {
        // builtin：enable=false 跳过；多模态 → vision；context_window → context_length。
        let builtin = parse_codearts_builtin(&serde_json::json!({
            "builtinModels": [
                {"model_id": "GLM-5.2", "model_name": "GLM 5.2", "context_window": 131072, "supports_images": false},
                {"model_id": "Qwen3-VL-235B", "context_window": 262144, "supports_images": true},
                {"model_id": "disabled", "enable": false}
            ]
        }));
        assert_eq!(builtin.len(), 2);
        assert_eq!(builtin[0].id, "GLM-5.2");
        assert_eq!(builtin[0].context_length, Some(131072));
        assert!(builtin[1].capabilities.iter().any(|c| c == "vision"));

        // agent detail：display_enabled 缺失（trial）/enabled=false → 跳过；嵌套 model_id 优先。
        let agent = parse_codearts_agent_detail(&serde_json::json!({
            "gpts": {"models": [
                {"model_alias": "glm-5.2-sft", "model_parameters": {"display_enabled": true, "model_id": "glm-5.2-real", "context_window": 128000}},
                {"model_alias": "trial", "model_parameters": {}},
                {"model_alias": "off", "model_parameters": {"display_enabled": true, "enabled": false}}
            ]}
        }));
        assert_eq!(agent.len(), 1);
        assert_eq!(agent[0].id, "glm-5.2-real");
        assert_eq!(agent[0].context_length, Some(128000));

        // benefit：error_code 非 "0000" 报错；成功按 sort 排序。
        assert!(parse_codearts_benefit(&serde_json::json!({"error_code": "1"})).is_err());
        let benefit = parse_codearts_benefit(&serde_json::json!({
            "error_code": "0000",
            "result": {"models": [
                {"model_id": "b-second", "sort": 2},
                {"model_id": "b-first", "sort": 1, "context_window": 65536}
            ]}
        }))
        .expect("benefit parse");
        assert_eq!(benefit[0].id, "b-first");
        assert_eq!(benefit[0].context_length, Some(65536));
        assert_eq!(benefit[1].id, "b-second");

        // merge：先到先得——agent 已报过的 id 不被 builtin 覆盖。
        let merged = merge_codearts(
            vec![CatalogEntry {
                id: "shared".into(),
                upstream: None,
                context_length: Some(1),
                capabilities: vec![],
            }],
            vec![
                CatalogEntry {
                    id: "shared".into(),
                    upstream: None,
                    context_length: Some(2),
                    capabilities: vec![],
                },
                CatalogEntry {
                    id: "only-builtin".into(),
                    upstream: None,
                    context_length: None,
                    capabilities: vec![],
                },
            ],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "shared");
        assert_eq!(merged[0].context_length, Some(1), "先到先得：保留 agent 的");
        assert_eq!(merged[1].id, "only-builtin");

        // agent id 列表：agent_id 优先、original_id 回退、去重。
        let ids = parse_codearts_agent_ids(&serde_json::json!({
            "agents": [{"agent_id": "a1"}, {"original_id": "a2"}, {"agent_id": "a1"}]
        }));
        assert_eq!(ids, vec!["a1".to_string(), "a2".to_string()]);
    }

    #[test]
    fn codearts_credentials_support_import_and_native_shapes() {
        // 原生形态：secret = AK:SK:STS，具名字段为空。
        let mut native = AccountSession::new("ch", "acct");
        native.provider = "codearts".to_string();
        native.credential.secret = "AK-1:SK-1:STS-1".to_string();
        assert_eq!(
            codearts_credentials(&native),
            (
                "AK-1".to_string(),
                "SK-1".to_string(),
                "STS-1".to_string(),
                String::new()
            )
        );
        // 导入形态：secret = AK，SK/STS/domain 在具名字段。
        let mut imported = AccountSession::new("ch", "acct");
        imported.provider = "codearts".to_string();
        imported.credential.secret = "AK-2".to_string();
        imported.credential.set_field("secret_access_key", "SK-2");
        imported.credential.set_field("security_token", "STS-2");
        imported.credential.set_field("domain_id", "DOM-2");
        assert_eq!(
            codearts_credentials(&imported),
            (
                "AK-2".to_string(),
                "SK-2".to_string(),
                "STS-2".to_string(),
                "DOM-2".to_string()
            )
        );
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
    fn body_summary_masks_credentials_and_keeps_structure() {
        let payload = serde_json::json!({
            "data": { "list": [
                {"modelCode": "glm-4", "modelName": "GLM-4"}
            ]},
            "access_token": "sk-should-be-hidden",
            "authorization": "Bearer abc.def.ghi",
            "detail": "upstream said Bearer topsecretvalue / eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2ln / deadbeefdeadbeefdeadbeefdeadbeef"
        });
        let summary = body_summary(&payload, 800);
        // 敏感键整值遮蔽
        assert!(!summary.contains("sk-should-be-hidden"), "{summary}");
        assert!(!summary.contains("abc.def.ghi"), "{summary}");
        // 长文本内嵌的 Bearer / JWT / 长 hex 遮蔽
        assert!(!summary.contains("topsecretvalue"), "{summary}");
        assert!(!summary.contains("eyJhbGciOiJIUzI1NiJ9"), "{summary}");
        assert!(!summary.contains("deadbeefdeadbeef"), "{summary}");
        // 结构（新结构字段）保留，便于判断真实形态
        assert!(summary.contains("modelCode"), "{summary}");
        assert!(summary.contains("glm-4"), "{summary}");
    }

    #[test]
    fn body_summary_truncates_to_limit() {
        let payload = serde_json::json!({
            "data": { "list": [ { "modelCode": "长".repeat(2000) } ] }
        });
        let summary = body_summary(&payload, 200);
        assert!(
            summary.chars().count() <= 200,
            "len={}",
            summary.chars().count()
        );
        assert!(summary.ends_with('…'), "{summary}");
    }

    #[test]
    fn sensitive_key_positive_and_negative_controls() {
        for key in [
            "authorization",
            "cookie",
            "set-cookie",
            "access_token",
            "refreshToken",
            "clientSecret",
            "secretKey",
            "accessKey",
            "privateKey",
            "x-api-key",
            "apiKey",
            "password",
            "passwd",
            "pwd",
        ] {
            assert!(is_sensitive_key(key), "应判敏感: {key}");
        }
        for key in [
            "name",
            "displayName",
            "modelCode",
            "modelId",
            "contextWindow",
            "maxOutputTokens",
            "max_tokens",
            "tags",
            "description",
            "id",
            "type",
        ] {
            assert!(!is_sensitive_key(key), "不应判敏感: {key}");
        }
    }

    #[test]
    fn body_summary_masks_key_variants_and_short_sk() {
        let payload = serde_json::json!({
            "secretKey": "short-secret-1",
            "accessKey": "AKIA-short",
            "privateKey": "pk-short",
            "x-api-key": "xkey-short",
            "passwd": "pw-short-1",
            "pwd": "pw-short-2",
            "token": "sk-short",
            "note": "prefix sk-abc123 tail",
            "modelCode": "glm-4",
            "modelName": "GLM-4"
        });
        let summary = body_summary(&payload, 800);
        for secret in [
            "short-secret-1",
            "AKIA-short",
            "pk-short",
            "xkey-short",
            "pw-short-1",
            "pw-short-2",
            "sk-short",
            "sk-abc123",
        ] {
            assert!(!summary.contains(secret), "{secret} 泄漏: {summary}");
        }
        // 普通字段保留，便于看结构
        assert!(summary.contains("modelCode"), "{summary}");
        assert!(summary.contains("glm-4"), "{summary}");
    }

    #[test]
    fn empty_catalog_error_reports_empty_body_marker() {
        let message = empty_catalog_error("https://xiaohuanxiong.com/model_catalog", &Value::Null);
        assert!(message.contains("<空响应体>"), "{message}");
        let blank = empty_catalog_error(
            "https://xiaohuanxiong.com/model_catalog",
            &Value::String("   ".into()),
        );
        assert!(blank.contains("<空响应体>"), "{blank}");
    }

    #[test]
    fn raccoon_catalog_empty_array_yields_empty() {
        let payload = serde_json::json!({ "data": { "models": [] } });
        assert!(parse_raccoon_catalog(&payload).is_empty());
    }

    /// 真实形态：`data.categories[].models[]`，id 在 `name`，params 嵌套上下文/输出上限。
    #[test]
    fn raccoon_catalog_reads_categories_with_nested_params() {
        let payload = serde_json::json!({
            "data": { "categories": [
                { "type": "default", "models": [
                    { "name": "raccoon-8c4485", "displayName": "Raccoon-Work",
                      "tags": ["general", "vision"],
                      "params": { "context_window": 1_000_000, "max_tokens": 100_000 } },
                    { "name": "raccoon-19b265", "tags": ["code"],
                      "context_window": 500_000 },
                    { "name": "raccoon-8c4485", "tags": ["duplicate"] }
                ]},
                { "type": "other", "models": [
                    { "name": "sn-sensenova-6-8-flash-lite", "supportThinking": false,
                      "params": { "max_tokens": 63_999 } }
                ]}
            ]}
        });
        let entries = parse_raccoon_catalog(&payload);
        assert_eq!(entries.len(), 3, "{entries:?}");

        let flagship = entries
            .iter()
            .find(|entry| entry.id == "raccoon-8c4485")
            .expect("应解析出 raccoon-8c4485");
        assert_eq!(flagship.context_length, Some(1_000_000));
        assert!(flagship.capabilities.iter().any(|c| c == "vision"));
        // 缺省 supportThinking=true → reasoning
        assert!(flagship.capabilities.iter().any(|c| c == "reasoning"));
        assert!(flagship.capabilities.iter().any(|c| c == "tool"));

        let second = entries
            .iter()
            .find(|entry| entry.id == "raccoon-19b265")
            .expect("应解析出 raccoon-19b265");
        // 顶层 context_window 也认
        assert_eq!(second.context_length, Some(500_000));

        let sensenova = entries
            .iter()
            .find(|entry| entry.id == "sn-sensenova-6-8-flash-lite")
            .expect("应解析出 sn-sensenova-6-8-flash-lite");
        // context_window 缺失 → 退化到 params.max_tokens；supportThinking=false → 无 reasoning
        assert_eq!(sensenova.context_length, Some(63_999));
        assert!(!sensenova.capabilities.iter().any(|c| c == "reasoning"));
    }

    #[test]
    fn raccoon_catalog_fallback_covers_core_models() {
        let spec = spec_for("xiaohuanxiong").expect("xiaohuanxiong 应有目录 spec");
        assert_eq!(spec.fallback.len(), 5);
        let snapshot = catalog_from_static(spec.fallback);
        assert_eq!(snapshot.source, "fallback");
        assert_eq!(snapshot.models.len(), 5);
        let ids: Vec<&str> = snapshot
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(ids.contains(&"raccoon-8c4485"), "{ids:?}");
        assert!(ids.contains(&"sn-sensenova-6-8-flash-lite"), "{ids:?}");
    }

    /// `static_models_for` 复用各家 `fallback` 表：有表的家非空，无证据的家空。
    #[test]
    fn static_models_for_reuses_fallback_tables() {
        assert!(
            !static_models_for("xiaohuanxiong").is_empty(),
            "raccoon 应有兜底"
        );
        assert!(
            !static_models_for("deepseek-web").is_empty(),
            "deepseek-web 应有兜底"
        );
        // 目录端点在闭源插件 / 无原生后端：无可自证清单（不编造）。
        for provider in ["chatglm", "codar", "qwenwork", "tabbit"] {
            assert!(
                static_models_for(provider).is_empty(),
                "{provider} 不应编造静态模型"
            );
        }
    }

    /// 无目录且无静态清单的家 → 明确「请手动声明模型」，不假装有。
    #[test]
    fn manual_model_hint_covers_no_catalog_providers() {
        for provider in [
            "chatglm",
            "codar",
            "qwenwork",
            "qwenwork-intl",
            "tabbit",
            "tabbit-intl",
            "custom-abc",
        ] {
            assert!(
                provider_needs_manual_models(provider),
                "{provider} 应提示手动声明"
            );
        }
        for provider in ["xiaohuanxiong", "deepseek", "openai_compatible"] {
            assert!(
                !provider_needs_manual_models(provider),
                "{provider} 不应命中手动声明提示"
            );
        }
        assert!(manual_declaration_hint("chatglm").contains("手动声明"));
    }

    #[test]
    fn raccoon_empty_catalog_error_has_url_and_masked_summary() {
        let payload = serde_json::json!({ "data": { "categories": [] } });
        let message = empty_catalog_error(
            "https://xiaohuanxiong.com/api/web/llm/v2/model_catalog?token=secret-value",
            &payload,
        );
        assert!(message.contains("model_catalog"), "{message}");
        // 查询串（可能含凭证）不得进入错误文案
        assert!(!message.contains("token=secret-value"), "{message}");
        assert!(message.contains("响应摘要"), "{message}");
        // 摘要里保留结构，便于定位真实形态
        assert!(message.contains("categories"), "{message}");
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
        // 上游没有可拉的目录接口的家：path 为空 → 走静态兜底
        for kind in [
            "autoclaw",
            "orcaterm",
            "zcode",
            "workbuddy",
            "workbuddy-intl",
        ] {
            let spec = spec_for(kind).expect("应有目录描述");
            assert!(spec.path.is_empty(), "{kind} 不该有远程端点");
            assert!(!spec.fallback.is_empty(), "{kind} 应有静态兜底表");
        }
    }

    #[test]
    fn workbuddy_static_catalog_registered_for_both_regions() {
        for kind in ["workbuddy", "workbuddy-intl"] {
            assert!(supports(kind), "{kind} 应支持目录发现（种子兜底）");
        }
        // 两个地区**共用同一份**内置种子（远程 /v3/config 才按地区分）
        let cn = spec_for("workbuddy").expect("workbuddy 应有目录描述");
        let intl = spec_for("workbuddy-intl").expect("workbuddy-intl 应有目录描述");
        assert_eq!(cn.fallback.len(), intl.fallback.len());
    }

    #[test]
    fn workbuddy_fallback_carries_capabilities_and_context() {
        let spec = spec_for("workbuddy").expect("workbuddy 应有目录描述");
        let pro = spec
            .fallback
            .iter()
            .find(|m| m.id == "deepseek-v4-pro")
            .expect("种子清单应含 deepseek-v4-pro");
        assert!(pro.reasoning && pro.vision && pro.tool);
        assert_eq!(pro.context_window, 1_000_000);
        // 兜底快照把能力位与上下文（取 maxInputTokens）翻出来
        let snapshot = catalog_from_static(spec.fallback);
        assert!(snapshot.models.iter().any(|e| e.id == "hy3-preview"
            && e.context_length == Some(192_000)
            && e.capabilities.iter().any(|c| c == "reasoning")));
    }

    #[test]
    fn workbuddy_fallback_excludes_non_chat_seed_entries() {
        // 种子清单 27 条里 4 条被 A2A 的 `is_chat_model` 过滤（见 const 文档与
        // SPEC §9），移植时按数据裁掉 —— 此处把「别补全」钉成断言：
        //   kimi-k2-instruct-taiji（命中前缀 kimi-k2-instruct）
        //   hunyuan-chat（maxOutputTokens 8192 < 16000）
        //   default-1.1 / default-1.2（命中前缀 default-1.）
        let spec = spec_for("workbuddy").expect("workbuddy 应有目录描述");
        assert_eq!(spec.fallback.len(), 23);
        for excluded in [
            "kimi-k2-instruct-taiji",
            "hunyuan-chat",
            "default-1.1",
            "default-1.2",
        ] {
            assert!(
                !spec.fallback.iter().any(|m| m.id == excluded),
                "{excluded} 属 A2A is_chat_model 过滤项，不该出现在兜底表"
            );
        }
    }

    #[test]
    fn supported_providers_are_consistent() {
        for kind in supported_providers() {
            assert!(supports(kind), "{kind} 在清单里但 spec_for 不认");
        }
    }

    /// 造一份目录条目（只为索引测试用）。
    fn entry(id: &str, upstream: Option<&str>, caps: &[&str]) -> CatalogEntry {
        CatalogEntry {
            id: id.to_string(),
            upstream: upstream.map(|s| s.to_string()),
            context_length: None,
            capabilities: caps.iter().map(|c| c.to_string()).collect(),
        }
    }

    fn snapshot(models: Vec<CatalogEntry>) -> CatalogSnapshot {
        CatalogSnapshot {
            models,
            fetched_at: 1,
            source: "remote".to_string(),
        }
    }

    #[test]
    fn capability_index_matches_by_both_names() {
        use crate::model::capability::CapabilityMask;
        let snap = snapshot(vec![
            entry("GLM-5.3", Some("gmodel"), &["tool", "vision"]),
            entry("glm-5.3-flash", None, &["tools"]),
        ]);
        let index = AccountCapabilityIndex::from_snapshot(&snap);
        assert!(index.has_evidence());
        // 用户可见名与上游真名都进表
        assert_eq!(index.mask_for("GLM-5.3"), index.mask_for("gmodel"));
        // 复数 tags 与单数 tool 同义（两套词表归一到一位）
        assert!(index
            .mask_for("glm-5.3-flash")
            .is_some_and(|m| m.covers(CapabilityMask::TOOL)));

        // 任一名字命中且满足即放行
        assert_eq!(
            index.verdict("gmodel", Some("GLM-5.3"), CapabilityMask::VISION),
            Some(true)
        );
        // 两个名字都已知但都不满足 → 拒
        assert_eq!(
            index.verdict("glm-5.3-flash", None, CapabilityMask::VISION),
            Some(false)
        );
        // 名字不在目录里 → 无证据（调用方放行）
        assert_eq!(index.verdict("unknown", None, CapabilityMask::VISION), None);
        // 无需求 → 不判定
        assert_eq!(index.verdict("GLM-5.3", None, CapabilityMask::EMPTY), None);
    }

    #[test]
    fn capability_index_treats_untagged_entries_as_unknown() {
        use crate::model::capability::CapabilityMask;
        // 老快照（pick_capabilities 上线前）：条目在，但没有能力位
        let legacy = snapshot(vec![entry("glm-5.3", None, &[])]);
        let index = AccountCapabilityIndex::from_snapshot(&legacy);
        assert!(!index.has_evidence(), "无标签 = 无证据");
        assert_eq!(index.verdict("glm-5.3", None, CapabilityMask::VIDEO), None);

        // 空目录同样无证据
        let empty = AccountCapabilityIndex::from_snapshot(&snapshot(Vec::new()));
        assert!(!empty.has_evidence());
        assert_eq!(empty.verdict("glm-5.3", None, CapabilityMask::TOOL), None);
    }

    #[test]
    fn capability_index_any_is_the_union() {
        use crate::model::capability::CapabilityMask;
        let snap = snapshot(vec![
            entry("a", None, &["chat"]),
            entry("b", None, &["video"]),
            entry("c", None, &["tool"]),
        ]);
        let index = AccountCapabilityIndex::from_snapshot(&snap);
        assert!(index.any().covers(CapabilityMask::VIDEO));
        assert!(index.any().covers(CapabilityMask::TOOL));
        assert!(!index.any().covers(CapabilityMask::VISION));
        // 「带标签但三位全无」的条目（如只有 chat）仍是**有证据**：它明确说了
        // 自己不支持视觉/工具/视频，粗筛才敢据此裁人。
        assert!(index.has_evidence());
    }
}
