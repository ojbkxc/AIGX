//! 模型元信息注册表（P1「模型元信息同步」）—— owned_by / 上下文长度 / 能力标签。
//!
//! 数据来源分五层（优先级从低到高）：
//! 1. 内置推断：模型名前缀 → 归属厂商；常见模型 → 上下文长度；名字特征 → 能力标签
//! 2. 渠道类型：list_models 聚合时由渠道的 channel_type 推导 owned_by（覆盖名字推断）
//! 3. models.dev 补齐（对齐 Agent2API，开关 `usage.models_dev_enabled` 默认开）：
//!    内置表未声明的上下文长度 / 能力标签用社区目录**只填空位**，已声明不改写
//! 4. **插件/供应商目录**（`plugin_host::model_catalog`）：插件自己
//!    `models.refresh` 回报的 `maxInputTokens` / `maxOutputTokens`，按 provider
//!    取各账号最大值。这是账号型供应商（小浣熊一类）申报上限的唯一真实来源，
//!    压过内置推断与 models.dev 的补空位
//! 5. 管理员覆盖：FileStore `model_meta:{name}` 持久化（显式覆盖前四层）
//!
//! 为什么插件目录压过 models.dev：前者是**这家上游自己申报**的，后者是社区
//! 对同名模型的通用估计。为什么仍低于管理员覆盖：管理员的显式配置永远最后说话。
//!
//! 百年工程红线：不引 crate；上下文长度表为静态内置 + models.dev 联网补齐
//! （内嵌快照保底，见 `models_dev` 模块）+ 按需覆盖。

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 单模型元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// 归属方（openai / anthropic / google / cloudflare / …）
    pub owned_by: String,
    /// 上下文长度（token 数；未知为 None）
    #[serde(default)]
    pub context_length: Option<u32>,
    /// 最大输出 token 数（未知为 None）。
    ///
    /// 2026-11-24 补：模型管理页的「上下文 / 输出」列一直只有前半个数字——
    /// `models.dev` 的 `limit.output` 早在 `models_dev.rs` 里解析出来了
    /// （`ModelInfo.output`），但元信息结构没有这个字段，于是整条链路
    /// （metadata → manage → admin 响应 → 前端 budget 列）都没吐过输出上限。
    /// 线上实测 136 个模型里 `max_output_tokens` 有值 0 个。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// 能力标签（chat / embedding / rerank / image / audio）
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// 思考等级候选（low / medium / high / …；管理员覆盖与前端下拉用）。
    ///
    /// 2026-11 前后端契约修复：前端 `ModelManage` 早已发送该字段但后端
    /// `ModelMetaRequest` 缺失，serde 默认忽略未知字段 → 保存即丢、重载即空
    ///（07-recon2 A.5「前后端契约断裂」）。存量 KV 无此键 → serde default
    /// 空数组，零迁移。
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
    /// 工具调用模式（对齐 A2A beta.2 `shape.rs::list_item` 的 `tool_calling_mode`）。
    ///
    /// 来源：插件 / provider 目录条目的 `toolCallingMode` 字段（如 web_tools
    /// 设 `"emulated"` / `"disabled"`）。AIGX 暂无 web_tools，此字段为预留
    /// 透传通道——管理员覆盖或未来功能可设置，`/v1/models` 条目原样透出。
    /// `None` = 未声明，`list_item` 不输出该键（与上游 `shape.rs` 同判据）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calling_mode: Option<String>,
    /// 可见性判据（对齐 Agent2API `providers/<kind>/models.rs` 的 `visibility` 键
    /// 与 `providers/adapter.rs::seed_hidden_visibility_defaults` 的取值域）。
    ///
    /// 取值域**逐字**同 A2A：`"hidden"`（上游能列出、官方客户端不展示）/ `"shown"`
    /// （会展示）；`None` = 这家没有判据（读侧与 A2A 一样归到 `undeclared`）。
    /// A2A 的判据由各 provider 适配器自己算（Antigravity 按世代、Qoder 读
    /// `enable`、CodeArts 读 `display_enabled`）—— AIGX 暂未接这几家的目录，
    /// 所以这里先作为**管理员可覆盖**的字段存在：覆盖了就有判据，没覆盖就是
    /// `None`（管理页落「—」/「未声明」，不伪造「可见」）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
}

/// 可见性取值域（与 A2A 逐字一致；`None` 由调用方表达「无判据」）。
pub const VISIBILITY_HIDDEN: &str = "hidden";
/// 可见性取值域：上游说「会展示」。
pub const VISIBILITY_SHOWN: &str = "shown";

/// 归一化外部传入的可见性值：只认 `hidden` / `shown`（大小写与首尾空白宽容），
/// 其余（含空串）一律当「没有判据」。
pub fn normalize_visibility(raw: Option<&str>) -> Option<String> {
    match raw.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if v == VISIBILITY_HIDDEN => Some(VISIBILITY_HIDDEN.to_string()),
        Some(v) if v == VISIBILITY_SHOWN => Some(VISIBILITY_SHOWN.to_string()),
        _ => None,
    }
}

// ── 内置推断表 ──────────────────────────────────────────────────────

/// 模型名前缀 → 归属厂商。
///
/// 网关语义下的「名字归谁」只用于 /v1/models 的 owned_by 展示，
/// 不参与路由决策（路由看渠道 models 声明）。
const KNOWN_PREFIXES: &[(&str, &str)] = &[
    ("gpt-", "openai"),
    ("o1", "openai"),
    ("o3", "openai"),
    ("o4", "openai"),
    ("chatgpt", "openai"),
    ("davinci", "openai"),
    ("text-embedding", "openai"),
    ("whisper", "openai"),
    ("tts-", "openai"),
    ("dall-e", "openai"),
    ("claude-", "anthropic"),
    ("gemini-", "google"),
    ("glm-", "zhipu"),
    ("deepseek", "deepseek"),
    ("qwen", "alibaba"),
    ("llama", "meta"),
    ("mistral", "mistral"),
    ("mixtral", "mistral"),
    ("kimi", "moonshot"),
    ("ernie", "baidu"),
    ("hunyuan", "tencent"),
    ("minimax", "minimax"),
    ("@cf/", "cloudflare"),
    ("@hf/", "huggingface"),
];

/// 常见模型上下文长度（token）。
///
/// 只收录确定性高的主流型号；不在表内的模型 context_length 为 None，
/// 管理员可通过覆盖机制补齐。前缀匹配取最长命中。
const KNOWN_CONTEXT_LENGTHS: &[(&str, u32)] = &[
    // OpenAI
    ("gpt-4o", 128000),
    ("gpt-4-turbo", 128000),
    ("gpt-4.1", 1047576),
    ("gpt-4", 8192),
    ("gpt-3.5-turbo", 16385),
    ("o1", 200000),
    ("o3", 200000),
    ("o4-mini", 200000),
    // Anthropic
    ("claude-3-5-sonnet", 200000),
    ("claude-3-5-haiku", 200000),
    ("claude-3-opus", 200000),
    ("claude-3-sonnet", 200000),
    ("claude-3-haiku", 200000),
    ("claude-sonnet-4", 200000),
    ("claude-opus-4", 200000),
    // Google
    ("gemini-1.5-pro", 2097152),
    ("gemini-1.5-flash", 1048576),
    ("gemini-2.0-flash", 1048576),
    ("gemini-2.5-pro", 1048576),
    ("gemini-2.5-flash", 1048576),
    // DeepSeek
    ("deepseek-chat", 65536),
    ("deepseek-reasoner", 65536),
    ("deepseek-r1", 65536),
    // 智谱
    ("glm-4", 128000),
    ("glm-5", 128000),
];

/// 模型名特征 → 能力标签。
const CAPABILITY_HINTS: &[(&str, &str)] = &[
    ("embedding", "embedding"),
    ("embed", "embedding"),
    ("rerank", "rerank"),
    ("dall-e", "image"),
    ("image", "image"),
    ("sd3", "image"),
    ("stable-diffusion", "image"),
    ("flux", "image"),
    ("whisper", "audio"),
    ("tts", "audio"),
    ("speech", "audio"),
];

/// 渠道类型 → owned_by（list_models 聚合时的渠道维度覆盖）。
pub fn owned_by_for_channel_type(ct: &str) -> Option<&'static str> {
    match ct {
        "cloudflare" => Some("cloudflare"),
        "anthropic" => Some("anthropic"),
        "gemini" => Some("google"),
        "zai" => Some("zhipu"),
        // openai_compatible 渠道上游混杂（DeepSeek/OpenRouter/…），
        // 不能一概归 openai——回落到模型名推断
        _ => None,
    }
}

/// 按模型名推断归属厂商（无匹配返回 None）。
pub fn infer_owned_by(model: &str) -> Option<&'static str> {
    let lower = model.to_lowercase();
    // 精确前缀匹配（KNOWN_PREFIXES 按长度降序保证 gpt-4.1 优先于 gpt-4 不适用——
    // owned_by 推断无此精度需求，首个命中即可；但 o1/o3/o4 系列需防误命中
    // 如 "o1xxx" 之外的名字，故放在较长前缀之后兜底）
    KNOWN_PREFIXES
        .iter()
        .find(|(p, _)| lower.starts_with(p))
        .map(|(_, owner)| *owner)
}

/// 按模型名查内置上下文长度（最长前缀命中优先）。
pub fn builtin_context_length(model: &str) -> Option<u32> {
    let lower = model.to_lowercase();
    KNOWN_CONTEXT_LENGTHS
        .iter()
        .filter(|(p, _)| lower.starts_with(p))
        .max_by_key(|(p, _)| p.len())
        .map(|(_, n)| *n)
}

/// 按模型名推断能力标签（未命中时默认 chat）。
pub fn infer_capabilities(model: &str) -> Vec<String> {
    let lower = model.to_lowercase();
    let mut caps: Vec<String> = CAPABILITY_HINTS
        .iter()
        .filter(|(p, _)| lower.contains(p))
        .map(|(_, c)| c.to_string())
        .collect();
    caps.dedup();
    if caps.is_empty() {
        caps.push("chat".to_string());
    }
    caps
}

// ── 插件目录申报的上限 ──────────────────────────────────────────────

/// 取某模型在插件目录里的上下文/输出申报。
///
/// `provider` 给定时**精确匹配**这一家（`plugin_host::model_catalog::budget_for`）；
/// 拿不到 provider 时退回跨 provider 取大（`budget_any`）—— 那是已知限制：
/// 同一模型被多家申报时取到的是最大值，可能高于当前这条链的真实上限。
///
/// 目录只在进程内表里（重启后由 `bootstrap`/`manager` 重新并集，见
/// `plugin_host::model_catalog` 模块头），所以本函数是纯同步、零 IO 的查表。
fn plugin_budget(
    model: &str,
    provider: Option<&str>,
) -> Option<crate::plugin_host::model_catalog::ModelBudget> {
    match provider.map(str::trim).filter(|p| !p.is_empty()) {
        Some(provider) => crate::plugin_host::model_catalog::budget_for(provider, model),
        None => crate::plugin_host::model_catalog::budget_any(model),
    }
}

// ── ModelMetadataRegistry ───────────────────────────────────────────

/// 元信息注册表：内置推断 + 管理员覆盖（FileStore 持久化）。
pub struct ModelMetadataRegistry {
    overrides: RwLock<HashMap<String, ModelMetadata>>,
    store: Arc<FileStore>,
}

impl ModelMetadataRegistry {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            overrides: RwLock::new(HashMap::new()),
            store,
        }
    }

    /// 从存储加载覆盖项。
    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("model_meta:")?;
        let mut overrides = HashMap::new();
        for key in keys {
            let name = key.trim_start_matches("model_meta:").to_string();
            if name.is_empty() {
                continue;
            }
            if let Some(meta) = self.store.get::<ModelMetadata>(&key)? {
                overrides.insert(name, meta);
            }
        }
        *self.overrides.write() = overrides;
        Ok(())
    }

    fn persist(&self, model: &str) -> Result<()> {
        let meta = self
            .overrides
            .read()
            .get(model)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("metadata for {model} missing"))?;
        self.store.put(&format!("model_meta:{model}"), &meta)?;
        Ok(())
    }

    /// 查询某模型的元信息（覆盖 > 插件目录 > 内置推断 / models.dev 补空位）。
    ///
    /// 上下文/输出两列遵循**物理不变量**规则（见 [`Self::resolve_with_sources_for`]）：
    /// 申报方申报了哪一列就用哪一列，缺的那列可从通用知识（内置表 / models.dev）
    /// 补；两列拼起来若 `输出 > 上下文`，由终检让低权威的那列退场。
    ///
    /// `channel_owned_by`：渠道维度推导的归属（优先于名字推断）；
    /// 覆盖项存在时整体以覆盖为准。models.dev 只填「内置推断没声明」的
    /// 上下文长度与能力标签（开关关闭时完全不参与）。
    ///
    /// 本方法**没有 provider 维度**（兼容旧调用点）：插件目录的申报按
    /// [`crate::plugin_host::model_catalog::budget_any`] 跨 provider 取大 ——
    /// 同一模型被多家申报时看到的是最大值。拿得到 provider 的调用点用
    /// [`Self::resolve_with_sources_for`]。
    pub fn get(&self, model: &str, channel_owned_by: Option<&str>) -> ModelMetadata {
        self.resolve_with_sources(model, channel_owned_by).0
    }

    /// 解析元信息并给出**逐字段来源标记**（Phase1-A4 capSources，对齐上游
    /// model_meta/fill.rs 的三态可信度）：`user`（管理员覆盖）>
    /// `plugin`（插件/供应商目录申报）> `builtin`（内置推断表）/ `modelsdev`
    /// （models.dev 目录补空位）。管理面能力弹窗据此显示「这个能力是谁说的」，
    /// 防误判。
    ///
    /// 来源判定与 [`Self::get`] 的合并逻辑同源（本方法是 get 的超集），
    /// 两处不要各自演化。
    ///
    /// 本函数是 [`Self::resolve_with_sources_for`] 的 `plugin_provider = None`
    /// 形态（只按模型名解析，插件目录走跨 provider 取大）—— 拿得到 provider
    /// 的调用点请直接用那个，否则同一模型在两家申报值不同时会被抬高。
    pub fn resolve_with_sources(
        &self,
        model: &str,
        channel_owned_by: Option<&str>,
    ) -> (ModelMetadata, serde_json::Value) {
        self.resolve_with_sources_for(model, channel_owned_by, None)
    }

    /// 同 [`Self::resolve_with_sources`]，但带 **provider 维度**。
    ///
    /// 上下文/输出两列遵循**物理不变量**规则（不是「同一账本」）：申报方申报了
    /// 哪一列就用哪一列，缺的那列允许从通用知识补 —— 判据是采信后是否
    /// `输出 ≤ 上下文`。跨账本会拼出不可能组合时才让低权威的那列退场
    /// （Trae 只申报 200k 上下文，models.dev 借来官方 Kimi-K3 的 1M 输出
    /// → 1M > 200k，丢输出；Antigravity 只申报 64k 输出、内置上下文 200k
    /// → 容得下，两列都留）。
    ///
    /// 为什么要 provider：插件的模型目录本身就是 provider 维度的
    /// （`plugin_host::model_catalog`，由 `wasm::catalog` 的并集回推），
    /// 同一个模型 id 在不同 provider 下申报的上下文/输出可以不同
    /// （渠道 A 的 `glm-5.3` 128k、账号池 B 的 200k）。只按模型名取会把
    /// 别家的上限套到这条链上。
    ///
    /// `plugin_provider` 语义：
    /// - `Some(p)`：**只**认这一家的申报（精确匹配；这家没申报就回落下一层，
    ///   不去借别家的数字）；
    /// - `None`：跨全部 provider 逐字段取最大（已知限制，见 [`plugin_budget`]）。
    pub fn resolve_with_sources_for(
        &self,
        model: &str,
        channel_owned_by: Option<&str>,
        plugin_provider: Option<&str>,
    ) -> (ModelMetadata, serde_json::Value) {
        if let Some(m) = self.overrides.read().get(model) {
            // 管理员覆盖整体获胜：非空字段全标 user
            let caps: serde_json::Map<String, serde_json::Value> = m
                .capabilities
                .iter()
                .map(|c| (c.clone(), serde_json::Value::from("user")))
                .collect();
            let sources = serde_json::json!({
                "context_length": if m.context_length.is_some() { serde_json::Value::from("user") } else { serde_json::Value::Null },
                "max_output_tokens": if m.max_output_tokens.is_some() { serde_json::Value::from("user") } else { serde_json::Value::Null },
                "capabilities": caps,
                "reasoning_levels": if m.reasoning_levels.is_empty() { serde_json::Value::Null } else { serde_json::Value::from("user") },
                // 可见性同样标来源：覆盖了才说 user，没覆盖（= 没判据）是 Null
                // —— 与 context_length / capabilities 的「可信度」口径一致
                "visibility": if m.visibility.is_some() { serde_json::Value::from("user") } else { serde_json::Value::Null },
            });
            return (m.clone(), sources);
        }
        let owned_by = channel_owned_by
            .or_else(|| infer_owned_by(model))
            .unwrap_or("aigx")
            .to_string();
        let mut context_length: Option<u32>;
        let mut context_source: &str;
        let mut max_output_tokens: Option<u32>;
        let mut output_source: &str;
        // ── 上下文/输出两列的采信规则（判据是**物理不变量**，不是账本）────
        //
        // 修复点（v1.0.103 线上实测 34 行「输出 > 上下文」的根因）：旧逻辑
        // 逐字段独立回落 —— 插件目录申报了上下文、没申报输出时，输出那列
        // 会去 models.dev 借**另一个供应商的同名模型**的数（Trae 渠道的
        // Kimi-K3 只申报 200k 上下文，models.dev 里 moonshotai/kimi-k3 是
        // 1M/1M 的官方 API 账本），拼出 200k 上下文 + 1M 输出的物理不可能
        // 组合。
        //
        // 判据是**不变量**，不是账本：两列可以来自不同账本，只要采信之后
        // `out ≤ ctx`（或其中一列缺失）。只有**会拼出不可能组合**的跨账本
        // 才被拒绝 —— 不能因为「不是同一家说的」就丢值（那会把只申报了
        // 一列的家一起打成空，Antigravity 那批 17 行就是这么被误伤的）。
        // 冲突时谁退场见函数尾的终检。
        let declared = plugin_budget(model, plugin_provider)
            .filter(|b| b.context_length.is_some() || b.max_output_tokens.is_some());
        // 插件目录**申报了哪一列就用哪一列**，缺的那列允许从通用账本
        // （内置表 / models.dev）补 —— 判据是「组合是否违反物理不变量」，
        // 不是「账本是否相同」。
        //
        // 曾经的做法是「申报了任一列就两列都锁死、缺的留空」，那是把
        // Trae 那次的**症状**当成了通则：Trae 的插件目录只申报上下文
        // （200k），models.dev 给的是 moonshotai 官方 API 的 1M 输出，
        // 两者拼起来 200k/1M 物理不可能 —— 那是**必须**锁的。但
        // Antigravity 只申报输出上限（64000），内置表给的上下文 200k
        // 完全容得下，锁死反而让用户白丢一个能显示的上下文。
        //
        // 所以：先各取各的（申报值优先，缺的列回落内置表），最后统一做一次
        // `out > ctx` 终检（见函数尾），违反才让**权威度较低的那一列**退场。
        context_length = declared
            .and_then(|b| b.context_length)
            .or_else(|| builtin_context_length(model));
        context_source = match (declared.and_then(|b| b.context_length), context_length) {
            (Some(_), Some(_)) => "plugin",
            (None, Some(_)) => "builtin",
            (_, None) => "",
        };
        max_output_tokens = declared.and_then(|b| b.max_output_tokens);
        output_source = if max_output_tokens.is_some() {
            "plugin"
        } else {
            ""
        };
        let mut capabilities = infer_capabilities(model);
        let mut cap_sources: serde_json::Map<String, serde_json::Value> = capabilities
            .iter()
            .map(|c| (c.clone(), serde_json::Value::from("builtin")))
            .collect();
        // models.dev 条目在块外先取出来：下面的「内置表过时修正」与终检都要
        // 用它，而 `lookup` 是进程内查表（零 IO），取一次比查两次清楚。
        let dev = if crate::config::models_dev_enabled() {
            super::models_dev::lookup(model)
        } else {
            None
        };
        let dev_context = dev
            .as_ref()
            .and_then(|d| d.context)
            .and_then(|n| u32::try_from(n).ok());
        let dev_output = dev
            .as_ref()
            .and_then(|d| d.output)
            .and_then(|n| u32::try_from(n).ok());
        if let Some(dev) = dev.as_ref() {
            // 补空位**逐列**开放：块内全是 `is_none()` 判断，插件已申报的
            // 列不会被覆盖，所以缺哪列就补哪列。
            //
            // 曾经这里是整段 `if !plugin_ledger`——只要插件申报了任一列，
            // models.dev 一个字段都不许进来。那是把「防止跨账本拼出不可能
            // 组合」做成了「禁止跨账本」，代价是 Antigravity 那种只申报
            // 输出的家，上下文列被锁成 null（用户白丢一个能显示的值）。
            //
            // 现在只补空位，跨账本的矛盾由函数尾部的 `out > ctx` 终检兜住：
            // Trae 的 out 被补成 384000、ctx 是申报的 200000，终检发现
            // 384000 > 200000 → 丢输出，与旧行为等价；Antigravity 的
            // out=64000 < ctx=200000 → 保留。
            // ── 内置表过时修正：整组改用 models.dev ──────────────
            // builtin 与 models.dev 同属「按模型名的通用知识」账本，
            // 但内置表是**静态快照**，会滞后：`glm-5` 前缀钉死
            // 128000，而社区目录里 GLM-5 早已是 1M 上下文 / 131k
            // 输出。按字段各取一半就会拼出 128k/131k 的物理不可能
            // 组合（线上 17 行）。
            //
            // 判据：**models.dev 的输出上限比内置上下文还大** ⇒ 内置
            // 那条上下文明显过时，此时整组采用 models.dev 的自洽对
            // （ctx / out 两列同源同值，来源都标 modelsdev），而不是
            // 丢掉输出（丢值）或留下不可能组合。前提是 models.dev
            // 自己的 ctx 容得下它的 out —— 容不下说明目录自身矛盾，
            // 交给下面那道物理守卫兜底。
            let stale_builtin = match (context_source, context_length, dev_output) {
                ("builtin", Some(builtin), Some(output)) if output > builtin => {
                    matches!(dev_context, Some(context) if context >= output)
                }
                _ => false,
            };
            if stale_builtin {
                context_length = dev_context;
                context_source = "modelsdev";
                max_output_tokens = dev_output;
                output_source = "modelsdev";
            } else {
                if context_length.is_none() {
                    context_length = dev_context;
                    if context_length.is_some() {
                        context_source = "modelsdev";
                    }
                }
                // 输出上限：内置表没这个维度，缺位就由 models.dev 补 —— 补完
                // 若与上下文矛盾，终检会丢它（Trae 的 1M 输出正是这样被挡掉的）。
                // 不因为「申报方在别处说过话」就整列拒补：那会把 Trae 里那些
                // 数值本来就不冲突的行（GLM-5.3 的 131k 装得进 200k）也一起丢空。
                if max_output_tokens.is_none() {
                    max_output_tokens = dev_output;
                    if max_output_tokens.is_some() {
                        output_source = "modelsdev";
                    }
                }
            }
            // 能力标签只有「默认 chat」（名字没命中任何特征）时才补，
            // embedding / rerank 等显式推断结果原样保留
            if capabilities.len() == 1 && capabilities[0] == "chat" {
                if dev.images {
                    capabilities.push("vision".to_string());
                    cap_sources.insert("vision".to_string(), serde_json::Value::from("modelsdev"));
                }
                if dev.video {
                    capabilities.push("video".to_string());
                    cap_sources.insert("video".to_string(), serde_json::Value::from("modelsdev"));
                }
                if dev.tool_call {
                    capabilities.push("tools".to_string());
                    cap_sources.insert("tools".to_string(), serde_json::Value::from("modelsdev"));
                }
                if dev.reasoning {
                    capabilities.push("reasoning".to_string());
                    cap_sources.insert(
                        "reasoning".to_string(),
                        serde_json::Value::from("modelsdev"),
                    );
                }
            }
        }
        // ── 物理不变量终检：采信的输出上限必须落在采信的上下文窗口内 ──
        //
        // 兜底两种残余：① models.dev 目录自身矛盾（输出 > 它自己的上下文）；
        // ② 上游申报自相矛盾（输出 > 上下文）。管理员覆盖路径不受此限 ——
        // 显式填写，全权在人（见 `override_may_express_output_over_context`）。
        //
        // 谁退场：**一律丢输出、保上下文**。
        //
        // 理由两条：
        //   ① 上下文是模型的固有属性，展示它是无害的（前端拿它做「装得下吗」
        //      的提示）；而输出上限直接决定上游会不会按它截断，填错了会让
        //      请求被截。
        //   ② 更关键：**冲突的两列里，上下文几乎总是一手信息**（申报的、
        //      或内置表按名字给的稳定值），而输出那列往往是社区目录里另一个
        //      同名模型的数（Trae 的 1M 输出就是月之暗面官方 API 的账本）。
        //      用「别家的输出」去反推「本家的上下文」（把它抬到 1M）会让整行
        //      都变成别家的模型 —— 那正是 v1.0.103 线上最误导的形态。
        if let (Some(context), Some(output)) = (context_length, max_output_tokens) {
            if output > context {
                max_output_tokens = None;
                output_source = "";
            }
        }
        let sources = serde_json::json!({
            "context_length": if context_source.is_empty() { serde_json::Value::Null } else { serde_json::Value::from(context_source) },
            "max_output_tokens": if output_source.is_empty() { serde_json::Value::Null } else { serde_json::Value::from(output_source) },
            "capabilities": cap_sources,
            "reasoning_levels": serde_json::Value::Null,
            // 内置推断路径没有可见性判据（同 reasoning_levels）
            "visibility": serde_json::Value::Null,
        });
        (
            ModelMetadata {
                owned_by,
                context_length,
                max_output_tokens,
                capabilities,
                // 内置推断路径不产出思考等级：由覆盖项 / 前端声明提供
                reasoning_levels: Vec::new(),
                tool_calling_mode: None,
                // 内置推断路径同样不产出可见性判据：只有管理员覆盖（各 provider
                // 目录适配器接入后才会由上游给出）——「没依据就不藏」，见 A2A
                // `seed_hidden_visibility_defaults` 的同一条纪律
                visibility: None,
            },
            sources,
        )
    }

    /// 设置/更新覆盖项。
    pub fn set_override(&self, model: String, meta: ModelMetadata) -> Result<()> {
        self.overrides.write().insert(model.clone(), meta);
        self.persist(&model)
    }

    /// 删除覆盖项（回落到内置推断）。
    pub fn remove_override(&self, model: &str) -> Result<()> {
        if self.overrides.write().remove(model).is_some() {
            self.store.delete(&format!("model_meta:{model}"))?;
        }
        Ok(())
    }

    /// 当前全部覆盖项。
    pub fn all_overrides(&self) -> HashMap<String, ModelMetadata> {
        self.overrides.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_owned_by_common_families() {
        assert_eq!(infer_owned_by("gpt-4o"), Some("openai"));
        assert_eq!(
            infer_owned_by("GPT-4o".to_uppercase().as_str()),
            Some("openai")
        );
        assert_eq!(infer_owned_by("claude-3-5-sonnet"), Some("anthropic"));
        assert_eq!(infer_owned_by("gemini-2.0-flash"), Some("google"));
        assert_eq!(infer_owned_by("@cf/meta/llama-3"), Some("cloudflare"));
        assert_eq!(infer_owned_by("glm-4-plus"), Some("zhipu"));
        assert_eq!(infer_owned_by("totally-unknown-model"), None);
    }

    #[test]
    fn context_length_prefix_priority() {
        // gpt-4.1 有专属条目（1M），不被 gpt-4 的 8192 覆盖
        assert_eq!(builtin_context_length("gpt-4.1"), Some(1047576));
        assert_eq!(builtin_context_length("gpt-4"), Some(8192));
        assert_eq!(builtin_context_length("gpt-4o-mini"), Some(128000));
        assert_eq!(
            builtin_context_length("claude-3-5-sonnet-20241022"),
            Some(200000)
        );
        assert_eq!(builtin_context_length("unknown-model"), None);
    }

    /// 可见性取值域归一（判据同 A2A：只认 hidden / shown，其余 = 无判据）。
    ///
    /// 这条测的是**契约**：上游适配器或管理员请求体传进来的值，必须只有两种落库
    /// 形态或 None —— 管理页的三态筛选（可见 / 不展示 / 未声明）依赖它，混进
    /// 第三种拼写时前端会静默归到「未声明」，看起来像「这家没判据」。
    #[test]
    fn visibility_value_domain_is_a2a_verbatim() {
        assert_eq!(
            normalize_visibility(Some("hidden")).as_deref(),
            Some("hidden")
        );
        assert_eq!(
            normalize_visibility(Some("  Shown ")).as_deref(),
            Some("shown")
        );
        // 大小写/空白以外的变体一律不算判据（含 A2A 前端归一用的 "visible"）
        assert_eq!(normalize_visibility(Some("visible")), None);
        assert_eq!(normalize_visibility(Some("")), None);
        assert_eq!(normalize_visibility(None), None);
    }

    #[test]
    fn capabilities_default_chat_and_hints() {
        assert!(infer_capabilities("gpt-4o").contains(&"chat".to_string()));
        assert!(infer_capabilities("text-embedding-3-small").contains(&"embedding".to_string()));
        assert!(infer_capabilities("bge-reranker-v2").contains(&"rerank".to_string()));
        assert!(infer_capabilities("dall-e-3").contains(&"image".to_string()));
    }

    #[test]
    fn channel_type_overrides_name_inference() {
        // openai_compatible 渠道不强行归属 → 名字推断兜底
        assert_eq!(owned_by_for_channel_type("openai_compatible"), None);
        assert_eq!(owned_by_for_channel_type("cloudflare"), Some("cloudflare"));
        assert_eq!(owned_by_for_channel_type("zai"), Some("zhipu"));
    }

    #[test]
    fn registry_override_beats_inference() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-meta-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));

        let reg = ModelMetadataRegistry::new(store);
        // 内置推断路径
        let m = reg.get("gpt-4o", Some("cloudflare"));
        assert_eq!(m.owned_by, "cloudflare");
        assert_eq!(m.context_length, Some(128000));

        // 覆盖路径
        reg.set_override(
            "gpt-4o".into(),
            ModelMetadata {
                owned_by: "my-org".into(),
                context_length: Some(2000),
                max_output_tokens: None,
                capabilities: vec!["chat".into()],
                reasoning_levels: vec!["low".into(), "high".into()],
                tool_calling_mode: None,
                visibility: Some(crate::model::metadata::VISIBILITY_HIDDEN.to_string()),
            },
        )
        .unwrap();
        let m = reg.get("gpt-4o", Some("cloudflare"));
        assert_eq!(m.owned_by, "my-org");
        assert_eq!(m.context_length, Some(2000));
        assert_eq!(
            m.reasoning_levels,
            vec!["low".to_string(), "high".to_string()]
        );

        // 删除覆盖后回落
        reg.remove_override("gpt-4o").unwrap();
        let m = reg.get("gpt-4o", None);
        assert_eq!(m.owned_by, "openai");
    }

    #[test]
    fn override_persists_across_registry_reload() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-meta-persist-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));

        let reg = ModelMetadataRegistry::new(store.clone());
        reg.set_override(
            "gemini-2.5-pro".into(),
            ModelMetadata {
                owned_by: "my-gemini".into(),
                context_length: Some(2_000_000),
                max_output_tokens: None,
                capabilities: vec!["chat".into(), "vision".into()],
                reasoning_levels: vec![],
                tool_calling_mode: None,
                visibility: None,
            },
        )
        .unwrap();

        // 模拟重启：新注册表从同一 FileStore load
        let reg2 = ModelMetadataRegistry::new(store);
        reg2.load().unwrap();
        let m = reg2.get("gemini-2.5-pro", Some("google"));
        assert_eq!(m.owned_by, "my-gemini");
        assert_eq!(m.context_length, Some(2_000_000));
        assert_eq!(
            m.capabilities,
            vec!["chat".to_string(), "vision".to_string()]
        );
    }

    // ── 插件目录层与四层优先级（本次修复的回归面）────────────────────
    //
    // 这一组用例能区分新旧：改动前 `resolve_with_sources` 从不查插件目录，
    // 所以凡是「值只能来自插件目录」的断言在旧实现上都拿不到（None / 空串），
    // 而不是「恰好也绿」。

    /// 独立临时目录（各用例互不影响覆盖项落盘）。
    fn temp_registry(tag: &str) -> ModelMetadataRegistry {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-meta-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ModelMetadataRegistry::new(Arc::new(FileStore::new(dir)))
    }

    /// 造一个只在插件目录里申报上限的 provider（名字唯一，防并行用例互撞）。
    fn seed_plugin_budget(tag: &str, models: serde_json::Value) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let provider = format!("unit-meta-{tag}-{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let list = models.as_array().cloned().unwrap_or_default();
        crate::plugin_host::model_catalog::set_remote(&provider, list, 1);
        provider
    }

    /// 第一层：管理员覆盖压过插件目录（覆盖里的非空字段全标 user）。
    #[test]
    fn override_beats_plugin_catalog() {
        let reg = temp_registry("override-plugin");
        let provider = seed_plugin_budget(
            "override",
            serde_json::json!([{ "id": "raccoon-x1", "maxInputTokens": 200_000,
                "maxOutputTokens": 8_192 }]),
        );
        let (meta, sources) = reg.resolve_with_sources_for("raccoon-x1", None, Some(&provider));
        assert_eq!(meta.context_length, Some(200_000), "插件目录应补上上下文");
        assert_eq!(
            meta.max_output_tokens,
            Some(8_192),
            "插件目录应补上输出上限"
        );
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("plugin"));

        reg.set_override(
            "raccoon-x1".into(),
            ModelMetadata {
                owned_by: "acme".into(),
                context_length: Some(1_000_000),
                max_output_tokens: Some(64_000),
                capabilities: vec!["chat".into()],
                reasoning_levels: vec![],
                tool_calling_mode: None,
                visibility: None,
            },
        )
        .unwrap();
        let (meta, sources) = reg.resolve_with_sources_for("raccoon-x1", None, Some(&provider));
        assert_eq!(meta.context_length, Some(1_000_000), "管理员覆盖必须赢");
        assert_eq!(meta.max_output_tokens, Some(64_000));
        assert_eq!(sources["context_length"], serde_json::json!("user"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("user"));
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 第二层：插件目录压过内置推断（内置表按名字猜的通用值让位给上游申报）。
    #[test]
    fn plugin_catalog_beats_builtin_context() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let reg = temp_registry("plugin-builtin");
        // 名字带唯一后缀：`gpt-4o` 前缀仍命中内置表（128000），而后缀让
        // models.dev 的归一化键必然落空 —— 这条用例只比较「内置 vs 插件」。
        let model = format!("gpt-4o-unitmeta{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        assert!(
            crate::model::models_dev::lookup(&model).is_none(),
            "唯一后缀不该命中 models.dev，否则这条用例测不到插件层"
        );
        let provider = seed_plugin_budget(
            "builtin",
            serde_json::json!([{ "id": model, "maxInputTokens": 96_000,
                "maxOutputTokens": 4_096 }]),
        );
        let (meta, sources) = reg.resolve_with_sources_for(&model, Some("openai"), Some(&provider));
        assert_eq!(
            meta.context_length,
            Some(96_000),
            "插件目录申报压过内置推断（这家上游自己说的比通用表准）"
        );
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        assert_eq!(meta.max_output_tokens, Some(4_096));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("plugin"));
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 第三层：插件目录压过 models.dev（社区目录只填空位，不得改写上游申报）。
    #[test]
    fn plugin_catalog_beats_modelsdev() {
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            "unit-meta-plugin-vs-dev".to_string(),
            crate::model::models_dev::DevEntry {
                canonical: Some("acme/unit-meta-plugin-vs-dev".to_string()),
                reasoning: false,
                tool_call: false,
                images: false,
                video: false,
                context: Some(32_000),
                output: Some(2_048),
            },
        )]);
        let reg = temp_registry("plugin-dev");
        // 模型名不命中内置表（前缀全不匹配）→ 只有 models.dev 与插件目录两档
        assert_eq!(builtin_context_length("unit-meta-plugin-vs-dev"), None);
        let provider = seed_plugin_budget(
            "dev",
            serde_json::json!([{ "id": "unit-meta-plugin-vs-dev",
                "maxInputTokens": 400_000, "maxOutputTokens": 16_384 }]),
        );
        let (meta, sources) =
            reg.resolve_with_sources_for("unit-meta-plugin-vs-dev", None, Some(&provider));
        assert_eq!(meta.context_length, Some(400_000), "插件目录赢 models.dev");
        assert_eq!(meta.max_output_tokens, Some(16_384));
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("plugin"));
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 第四层：插件目录**没有**这个模型时，models.dev 仍按老规矩补空位
    /// （上下文与输出都补），来源标 `modelsdev` —— 这是「插件层不破坏既有
    /// 回落」的正向对照。
    #[test]
    fn modelsdev_still_fills_when_plugin_catalog_misses() {
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            "unit-meta-dev-only".to_string(),
            crate::model::models_dev::DevEntry {
                canonical: Some("acme/unit-meta-dev-only".to_string()),
                reasoning: true,
                tool_call: true,
                images: false,
                video: false,
                context: Some(64_000),
                output: Some(8_192),
            },
        )]);
        let reg = temp_registry("dev-fallback");
        // 唯一 provider 名，且目录里只有别的模型 → 精确匹配必然落空
        let provider = seed_plugin_budget(
            "dev-miss",
            serde_json::json!([{ "id": "some-other-model", "maxInputTokens": 1 }]),
        );
        let (meta, sources) =
            reg.resolve_with_sources_for("unit-meta-dev-only", None, Some(&provider));
        assert_eq!(meta.context_length, Some(64_000));
        assert_eq!(meta.max_output_tokens, Some(8_192));
        assert_eq!(sources["context_length"], serde_json::json!("modelsdev"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("modelsdev"));
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 正向对照：完全不碰插件目录时，解析结果与改动前同一口径 ——
    /// 上下文按内置表，来源就是 `builtin`；输出上限沿用「内置表没有该维度」
    /// 的老规矩（有 models.dev 命中则由它补，否则未声明）。
    ///
    /// 不把 models.dev 那半写死成 `None`：`models_dev` 目录是进程级全局的
    /// （`init` / `seed_for_test` 都会写入），并行用例先种了 `gpt-4o` 也不该
    /// 让这条断言变红。因此这里用**运行时的 lookup 结果当 oracle**，只钉住
    /// 「插件层没有插手」这件事：上下文来自 builtin、来源永不为 plugin。
    #[test]
    fn without_plugin_catalog_resolution_is_unchanged() {
        let reg = temp_registry("no-plugin");
        let (meta, sources) = reg.resolve_with_sources("gpt-4o", Some("openai"));
        let dev = crate::model::models_dev::lookup("gpt-4o");
        assert_eq!(meta.owned_by, "openai");
        assert_eq!(meta.context_length, Some(128_000), "内置表的上下文不受影响");
        assert_eq!(sources["context_length"], serde_json::json!("builtin"));
        let expected_output = dev
            .as_ref()
            .and_then(|d| d.output)
            .and_then(|n| u32::try_from(n).ok());
        assert_eq!(
            meta.max_output_tokens, expected_output,
            "输出上限仍是 models.dev 单独负责的那一档"
        );
        assert!(
            sources["context_length"] != serde_json::json!("plugin")
                && sources["max_output_tokens"] != serde_json::json!("plugin"),
            "没有插件目录数据时来源绝不可能是 plugin"
        );
        assert_eq!(meta.capabilities, vec!["chat".to_string()]);
        // 「谁知道这个模型的归属」也不受插件层影响
        assert!(sources["visibility"].is_null());
        assert!(sources["reasoning_levels"].is_null());
    }

    /// 无 provider 维度时的已知限制：跨全部 provider 逐字段取最大。
    ///
    /// 这条**固化限制本身**（而不是假装它精确）：两家申报 128k / 200k 时，
    /// 拿不到 provider 的调用点会看到 200k。有 provider 的调用点不受影响
    /// （见 `plugin_catalog_beats_builtin_context`）。
    #[test]
    fn without_provider_dimension_takes_max_across_providers() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let reg = temp_registry("any-provider");
        // 唯一模型名：`budget_any` 扫的是全局表，别家同名条目会污染取值。
        let model = format!("shared-unitmeta{}", SEQ.fetch_add(1, Ordering::Relaxed));
        let small = seed_plugin_budget(
            "any-small",
            serde_json::json!([{ "id": model, "maxInputTokens": 128_000 }]),
        );
        let large = seed_plugin_budget(
            "any-large",
            serde_json::json!([{ "id": model, "maxInputTokens": 200_000 }]),
        );
        let (meta, sources) = reg.resolve_with_sources(&model, None);
        assert_eq!(
            meta.context_length,
            Some(200_000),
            "无 provider 时取最大（已知限制）"
        );
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        // 有 provider 时精确匹配自己那家
        let (small_meta, _) = reg.resolve_with_sources_for(&model, None, Some(&small));
        assert_eq!(small_meta.context_length, Some(128_000));
        let (large_meta, _) = reg.resolve_with_sources_for(&model, None, Some(&large));
        assert_eq!(large_meta.context_length, Some(200_000));
        crate::plugin_host::model_catalog::remove(&small);
        crate::plugin_host::model_catalog::remove(&large);
    }

    // ── 物理不变量判据 + 终检（谁退场）─────────────────────────────────
    //
    // 这一组覆盖修复点的两个面：① 跨账本混拼在旧实现的逐字段回落下会拼出
    // 「输出 > 上下文」，凡「矛盾组合不得出现」的断言旧实现都拿不到；
    // ② 反过来，**不矛盾**的跨账本组合必须被补全（不许因为「不是同一家
    // 说的」就把能显示的列丢成空）—— antigravity 那 17 行就是被旧规则
    // 误伤的形态。

    /// 核心场景（Trae/Kimi-K3 线上实况）：插件目录只申报上下文（200k）、
    /// 没申报输出，而 models.dev 里有**另一家同名模型**的完整账本
    /// （moonshotai 官方 API 的 1M/1M）。
    ///
    /// 新判据下输出**会被补上** 1_048_576，但补完立刻被终检发现
    /// `1_048_576 > 200_000` 物理不可能 → 丢弃。所以断言仍是「输出为空」，
    /// 但原因是**不变量**，不是「不许跨账本」。
    ///
    /// 旧实现（逐字段无终检）会把 1_048_576 原样透出 —— 本用例的 `None`
    /// 断言在旧实现上必红。
    #[test]
    fn plugin_context_with_impossible_modelsdev_output_drops_output() {
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            "unit-meta-ledger-kimi".to_string(),
            crate::model::models_dev::DevEntry {
                canonical: Some("moonshotai/unit-meta-ledger-kimi".to_string()),
                reasoning: true,
                tool_call: true,
                images: false,
                video: false,
                context: Some(1_048_576),
                output: Some(1_048_576),
            },
        )]);
        let reg = temp_registry("ledger-kimi");
        let provider = seed_plugin_budget(
            "ledger-kimi",
            serde_json::json!([{ "id": "unit-meta-ledger-kimi",
                "maxInputTokens": 200_000 }]),
        );
        let (meta, sources) =
            reg.resolve_with_sources_for("unit-meta-ledger-kimi", None, Some(&provider));
        assert_eq!(meta.context_length, Some(200_000), "申报的上下文照用");
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        assert_eq!(
            meta.max_output_tokens, None,
            "插件没申报输出就留空 —— models.dev 的 1M 是官方同名模型的账本，不是这条链的"
        );
        assert!(
            sources["max_output_tokens"].is_null(),
            "输出没有来源（不是 modelsdev）"
        );
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 判据是**不变量**，不是账本：插件申报了上下文（200k）、models.dev 给
    /// 输出 131k，`131k <= 200k` 物理允许 ⇒ 补上这一列，来源如实标
    /// `modelsdev`。
    ///
    /// 更早的实现（v1.0.103）在这里是反过来的：只要上下文来自 plugin，输出
    /// 就不许从 models.dev 拿，「合理的是数值，不是账本」。那个判据把 Trae
    /// 那次的症状当成了通则 —— Trae 的 1M 输出确实不能借（见下面
    /// `plugin_context_with_impossible_modelsdev_output_drops_output`），
    /// 但 131k 借过来没有任何问题，锁死只是让用户白丢一列。
    #[test]
    fn plugin_context_with_plausible_modelsdev_output_is_filled() {
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            "unit-meta-ledger-glm".to_string(),
            crate::model::models_dev::DevEntry {
                canonical: Some("zhipuai/unit-meta-ledger-glm".to_string()),
                reasoning: false,
                tool_call: true,
                images: false,
                video: false,
                context: Some(1_000_000),
                output: Some(131_072),
            },
        )]);
        let reg = temp_registry("ledger-glm");
        let provider = seed_plugin_budget(
            "ledger-glm",
            serde_json::json!([{ "id": "unit-meta-ledger-glm",
                "maxInputTokens": 200_000 }]),
        );
        let (meta, sources) =
            reg.resolve_with_sources_for("unit-meta-ledger-glm", None, Some(&provider));
        assert_eq!(meta.context_length, Some(200_000));
        assert_eq!(
            meta.max_output_tokens,
            Some(131_072),
            "131072 <= 200000 不违反不变量，应该补上"
        );
        assert_eq!(sources["max_output_tokens"], "modelsdev");
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 插件只申报输出、上下文缺失：**补**上下文，但如果补完之后
    /// `out > ctx`，终检必须丢输出。
    ///
    /// 这里 `gpt-4o` 前缀命中内置表 → ctx=128000，而插件申报 out=131072，
    /// `131072 > 128000` 物理不可能 ⇒ 最终 `ctx=128000 / out=None`。
    ///
    /// 更早的实现（v1.0.103）在这条上是 `ctx=None / out=131072`（账本锁定，
    /// 上下文留空）—— 虽然也没拼出不可能组合，但白白丢了一个能显示的上下文，
    /// 而且用户看不出「这家其实有 128k 上下文」。
    #[test]
    fn plugin_output_exceeding_borrowed_context_drops_output() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        // 前缀命中内置表（gpt-4o → 128000），唯一后缀防并行用例互撞
        let model = format!("gpt-4o-unitlock{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        let reg = temp_registry("ledger-out-only");
        let provider = seed_plugin_budget(
            "ledger-out",
            serde_json::json!([{ "id": &model, "maxOutputTokens": 131_072 }]),
        );
        let (meta, sources) = reg.resolve_with_sources_for(&model, None, Some(&provider));
        assert_eq!(
            meta.context_length,
            Some(128_000),
            "上下文从内置表补（补完不矛盾就留着，不该锁成空）"
        );
        assert_eq!(sources["context_length"], serde_json::json!("builtin"));
        assert_eq!(
            meta.max_output_tokens, None,
            "131072 > 128000 物理不可能 ⇒ 终检丢输出"
        );
        assert!(sources["max_output_tokens"].is_null());
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 对称的正向用例：插件只申报输出、且补来的上下文**容得下**它 ⇒ 两列都保留，
    /// 来源分别标注。这是 Antigravity 那 17 行的形状（out=64000，ctx 由内置表给 200k）。
    #[test]
    fn plugin_output_within_borrowed_context_keeps_both() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let model = format!("gpt-4o-unitsmall{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        let reg = temp_registry("ledger-out-small");
        let provider = seed_plugin_budget(
            "ledger-out-small",
            serde_json::json!([{ "id": &model, "maxOutputTokens": 64_000 }]),
        );
        let (meta, sources) = reg.resolve_with_sources_for(&model, None, Some(&provider));
        assert_eq!(meta.context_length, Some(128_000));
        assert_eq!(sources["context_length"], serde_json::json!("builtin"));
        assert_eq!(
            meta.max_output_tokens,
            Some(64_000),
            "64000 <= 128000 不违反不变量，保留"
        );
        assert_eq!(sources["max_output_tokens"], serde_json::json!("plugin"));
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 内置表过时修正（线上 GLM 家族实况）：内置 `glm-5` 前缀钉死 128000，
    /// 而 models.dev 是 1M 上下文 / 131072 输出。models.dev 的输出比内置
    /// 上下文还大 ⇒ 内置那条上下文明显过时，**整组改用 models.dev 的自洽
    /// 对**（不丢值、不留不可能组合、两列同源）。
    ///
    /// 旧实现给出 `(128_000 builtin, 131_072 modelsdev)` —— 本用例的两列
    /// 断言在旧实现上都必红。
    #[test]
    fn builtin_context_yields_to_modelsdev_when_output_exceeds_it() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        // glm-5 前缀命中内置表（128000）；同用例内直接断言这个前提
        let model = format!("glm-5-unitstale{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            model.clone(),
            crate::model::models_dev::DevEntry {
                canonical: Some("zhipuai/glm-5-unitstale".to_string()),
                reasoning: false,
                tool_call: true,
                images: false,
                video: false,
                context: Some(1_000_000),
                output: Some(131_072),
            },
        )]);
        let reg = temp_registry("stale-builtin");
        let (meta, sources) = reg.resolve_with_sources(&model, Some("zhipu"));
        assert_eq!(
            meta.context_length,
            Some(1_000_000),
            "内置 128k 明显过时（连 models.dev 的输出都装不下），整组改用社区目录"
        );
        assert_eq!(
            meta.max_output_tokens,
            Some(131_072),
            "输出不丢值 —— 与上下文同源同组"
        );
        assert_eq!(sources["context_length"], serde_json::json!("modelsdev"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("modelsdev"));
    }

    /// 正向对照：models.dev 的输出**装得下**内置上下文时，两列各守本分
    /// —— 上下文仍是内置表的（来源 builtin），输出由 models.dev 补
    /// （来源 modelsdev）。这是 openai_compatible 等无插件目录渠道的既有
    /// 行为，本次修复不得改动它。
    #[test]
    fn builtin_context_kept_when_modelsdev_output_fits_inside_it() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let model = format!("gpt-4o-unitfits{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            model.clone(),
            crate::model::models_dev::DevEntry {
                canonical: Some("openai/gpt-4o-unitfits".to_string()),
                reasoning: false,
                tool_call: true,
                images: false,
                video: false,
                context: Some(128_000),
                output: Some(16_384),
            },
        )]);
        let reg = temp_registry("fits-builtin");
        let (meta, sources) = reg.resolve_with_sources(&model, Some("openai"));
        assert_eq!(meta.context_length, Some(128_000), "内置上下文不变");
        assert_eq!(sources["context_length"], serde_json::json!("builtin"));
        assert_eq!(meta.max_output_tokens, Some(16_384));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("modelsdev"));
    }

    /// 物理守卫的兜底面：models.dev 目录**自身矛盾**（输出 > 它自己的
    /// 上下文）时，既不能整组采用它、也不能留下不可能组合 —— 保上下文、
    /// 丢输出。
    ///
    /// 旧实现原样透出 `(128_000 builtin, 131_072 modelsdev)`。
    #[test]
    fn self_contradictory_modelsdev_entry_loses_output() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let model = format!("glm-5-unitbad{}", SEQ.fetch_add(1, Ordering::Relaxed));
        assert_eq!(builtin_context_length(&model), Some(128_000));
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            model.clone(),
            crate::model::models_dev::DevEntry {
                canonical: Some("zhipuai/glm-5-unitbad".to_string()),
                reasoning: false,
                tool_call: true,
                images: false,
                video: false,
                // 目录自身矛盾：上下文 100k，输出 131k
                context: Some(100_000),
                output: Some(131_072),
            },
        )]);
        let reg = temp_registry("bad-dev");
        let (meta, sources) = reg.resolve_with_sources(&model, Some("zhipu"));
        assert_eq!(
            meta.context_length,
            Some(128_000),
            "目录自身矛盾时不能整组采用，保留内置上下文"
        );
        assert_eq!(sources["context_length"], serde_json::json!("builtin"));
        assert_eq!(
            meta.max_output_tokens, None,
            "131072 > 100000 且 > 128000：物理不可能，丢输出"
        );
        assert!(sources["max_output_tokens"].is_null());
    }

    /// 物理守卫对自相矛盾的同源申报同样生效：插件两列都申报但输出 >
    /// 上下文（申报自身 bug），保上下文、丢输出。
    ///
    /// 旧实现原样透出 `(32_000, 131_072)`。
    #[test]
    fn impossible_plugin_pair_loses_output_not_context() {
        let reg = temp_registry("clamp-plugin");
        let provider = seed_plugin_budget(
            "clamp-plugin",
            serde_json::json!([{ "id": "unit-meta-clamp",
                "maxInputTokens": 32_000, "maxOutputTokens": 131_072 }]),
        );
        let (meta, sources) =
            reg.resolve_with_sources_for("unit-meta-clamp", None, Some(&provider));
        assert_eq!(meta.context_length, Some(32_000));
        assert_eq!(sources["context_length"], serde_json::json!("plugin"));
        assert_eq!(
            meta.max_output_tokens, None,
            "输出 > 上下文的申报组合只展示物理可能的那列"
        );
        assert!(sources["max_output_tokens"].is_null());
        crate::plugin_host::model_catalog::remove(&provider);
    }

    /// 正向对照（不破坏既有回落）：非插件账本内、且输出落在上下文窗口内
    /// 的组合原样保留 —— 上下文/输出都是 models.dev（无插件目录的
    /// openai_compatible 渠道依赖这条通路）。
    #[test]
    fn modelsdev_pair_survives_when_output_within_context() {
        let _seed_guard = crate::model::models_dev::seed_for_test([(
            "unit-meta-ledger-consistent".to_string(),
            crate::model::models_dev::DevEntry {
                canonical: Some("acme/unit-meta-ledger-consistent".to_string()),
                reasoning: false,
                tool_call: true,
                images: false,
                video: false,
                context: Some(64_000),
                output: Some(8_192),
            },
        )]);
        let reg = temp_registry("ledger-consistent");
        let (meta, sources) = reg.resolve_with_sources("unit-meta-ledger-consistent", None);
        assert_eq!(meta.context_length, Some(64_000));
        assert_eq!(
            meta.max_output_tokens,
            Some(8_192),
            "8192 ≤ 64000，同账本原样保留"
        );
        assert_eq!(sources["context_length"], serde_json::json!("modelsdev"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("modelsdev"));
    }

    /// 边界（守卫不越界）：管理员覆盖是显式人的决定，即使填写
    /// 输出 > 上下文也原样保留 —— 物理守卫只作用于推断层的机器拼接
    /// （builtin / modelsdev / plugin），不替管理员改数。
    #[test]
    fn override_may_express_output_over_context() {
        let reg = temp_registry("user-ledger");
        reg.set_override(
            "unit-meta-user-clamp".into(),
            ModelMetadata {
                owned_by: "acme".into(),
                context_length: Some(2_000),
                max_output_tokens: Some(4_000),
                capabilities: vec!["chat".into()],
                reasoning_levels: vec![],
                tool_calling_mode: None,
                visibility: None,
            },
        )
        .unwrap();
        let (meta, sources) = reg.resolve_with_sources_for("unit-meta-user-clamp", None, None);
        assert_eq!(
            meta.context_length,
            Some(2_000),
            "覆盖原样，守卫不碰 user 层"
        );
        assert_eq!(meta.max_output_tokens, Some(4_000));
        assert_eq!(sources["context_length"], serde_json::json!("user"));
        assert_eq!(sources["max_output_tokens"], serde_json::json!("user"));
    }
}
