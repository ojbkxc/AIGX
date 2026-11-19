//! 系统提示词注入 — 对照 Agent2API「设置 → 网关 → 系统提示词」（core/prompt.rs）。
//!
//! 三种模式（`passthrough` / `custom` / `append`）：
//! - `passthrough`：透传客户端 system（默认，行为与引入本模块前逐字相同）
//! - `custom`：删除 messages 里所有 system 消息，头部插入网关提示词
//! - `append`：在开头连续的 system 块之后追加一条网关提示词，既有消息不动
//!
//! AIGX 数据面在 `ChatFormat`（归一化 ChatMessage 列表）构造后调用
//! [`apply_prompt`]；管理面经 `/api/gateway/prompt` 读写（FileStore 键
//! `gateway_prompt`），保存即生效。
//!
//! 覆盖层级（优先级从高到低，对照 A2A 2.9.0 的逐 provider 覆盖）：
//! 1. **模型覆盖**（`models`，键 = 客户端请求里的模型名）——入口处即可判定；
//! 2. **提供商覆盖**（`providers`，键 = 渠道/账号 provider：账号型渠道取
//!    账号的 provider（qoder / trae …），其余渠道取渠道类型
//!    （openai_compatible / anthropic / gemini / zai））——选路后才知道是哪一家；
//! 3. **全局**（`mode` / `text`）。
//!
//! 命中任一覆盖层即**整条替代**下一层的 mode/text（不是逐字段合并）；
//! 文本为空表示「该层不注入」，等价 passthrough。
//!
//! 简化口径（相对 A2A）：不做提示词文件来源三档、不做降级中性提示词——
//! AIGX 无该降级机制。

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::bridge::{ChatMessage, Role};
use crate::storage::FileStore;

/// 持久化键。
pub const GATEWAY_PROMPT_KEY: &str = "gateway_prompt";

// ── 提示词规则引擎（对齐 Agent2API v3.5.0-beta.2 `config/prompt_rules.rs` +
//    `core/prompt/rules.rs`）────────────────────────────────────────────
//
// AIGX 既有的 `models` / `providers` 覆盖表是「精确键 → 整条覆盖」的简化形态；
// 这里补的是参考树的**条件规则**：每条规则由若干条件组成（字段 = provider /
// client / model，操作 = 是 / 不是 / 包含 / 不包含 / glob / 正则），`logic`
// 决定 All/Any 组合，命中后整条替代下一层。规则优先级最高（rules > models >
// providers > 全局），默认空表 = 走既有三层，零行为变化。

/// 规则数上限。
pub const MAX_PROMPT_RULES: usize = 200;
/// 单规则条件数上限。
pub const MAX_PROMPT_CONDITIONS: usize = 32;
/// 单条件值数上限（非 model 字段）。
pub const MAX_PROMPT_VALUES: usize = 100;
/// 单条件值字符上限。
pub const MAX_PROMPT_PATTERN_CHARS: usize = 256;
/// 单条提示词正文字节上限。
pub const MAX_PROMPT_TEXT_BYTES: usize = 1024 * 1024;

/// 规则条件字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptField {
    Provider,
    Client,
    Model,
}

/// 规则条件操作符。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptOperator {
    Is,
    IsNot,
    Contains,
    NotContains,
    Glob,
    Regex,
}

/// 条件组合逻辑。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptLogic {
    #[default]
    All,
    Any,
}

/// 单条规则条件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCondition {
    pub id: String,
    pub field: PromptField,
    pub op: PromptOperator,
    pub values: Vec<String>,
}

/// 单条提示词规则。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub logic: PromptLogic,
    pub conditions: Vec<PromptCondition>,
    pub mode: PromptMode,
    /// 提示词文件路径（对齐参考树；AIGX 当前只消费 `text`，保留字段以便线表一致）。
    #[serde(default)]
    pub file: String,
    #[serde(default)]
    pub text: String,
}

impl PromptRule {
    /// 规则自身的 (mode, text)。
    pub fn content(&self) -> (PromptMode, &str) {
        (self.mode, self.text.as_str())
    }
}

/// 编译后的条件匹配器（模型字段的 glob/regex 预编译）。
#[derive(Clone, Debug)]
pub struct PromptMatcher {
    condition: PromptCondition,
    pattern: Option<Regex>,
}

impl PromptMatcher {
    pub fn compile(condition: &PromptCondition) -> Result<Self, String> {
        let value = condition.values.first().map(String::as_str).unwrap_or("");
        let pattern = if condition.field == PromptField::Model && !value.is_empty() {
            match condition.op {
                PromptOperator::Glob => {
                    let mut body = String::from("\\A");
                    for ch in value.chars() {
                        match ch {
                            '*' => body.push_str(".*"),
                            '?' => body.push('.'),
                            other => body.push_str(&regex::escape(&other.to_string())),
                        }
                    }
                    body.push_str("\\z");
                    Some(compile_regex(&body)?)
                }
                PromptOperator::Regex => Some(compile_regex(value)?),
                _ => None,
            }
        } else {
            None
        };
        Ok(Self {
            condition: condition.clone(),
            pattern,
        })
    }

    /// 条件是否命中（provider / client / model 三选一取值）。
    pub fn matches(&self, provider: &str, client: &str, model: &str) -> bool {
        let actual = match self.condition.field {
            PromptField::Provider => provider,
            PromptField::Client => client,
            PromptField::Model => model,
        };
        // 空条件不扩大范围；未指定模型也不参与「不是 / 不包含」。
        if actual.is_empty() || self.condition.values.is_empty() {
            return false;
        }
        if self.condition.field != PromptField::Model {
            let hit = self
                .condition
                .values
                .iter()
                .any(|value| actual.eq_ignore_ascii_case(value));
            return if self.condition.op == PromptOperator::IsNot {
                !hit
            } else {
                hit
            };
        }
        let value = &self.condition.values[0];
        if value.is_empty() {
            return false;
        }
        match self.condition.op {
            PromptOperator::Is => actual.eq_ignore_ascii_case(value),
            PromptOperator::IsNot => !actual.eq_ignore_ascii_case(value),
            PromptOperator::Contains => actual
                .to_ascii_lowercase()
                .contains(&value.to_ascii_lowercase()),
            PromptOperator::NotContains => !actual
                .to_ascii_lowercase()
                .contains(&value.to_ascii_lowercase()),
            PromptOperator::Glob | PromptOperator::Regex => {
                self.pattern.as_ref().is_some_and(|p| p.is_match(actual))
            }
        }
    }
}

fn compile_regex(value: &str) -> Result<Regex, String> {
    RegexBuilder::new(value)
        .case_insensitive(true)
        .size_limit(1024 * 1024)
        .dfa_size_limit(1024 * 1024)
        .build()
        .map_err(|error| format!("模型匹配表达式无效：{error}"))
}

fn valid_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

/// 校验一组提示词规则（对齐参考树 `validate_rules`）。
pub fn validate_prompt_rules(rules: &[PromptRule]) -> Result<(), String> {
    use std::collections::HashSet;
    if rules.len() > MAX_PROMPT_RULES {
        return Err(format!("最多允许 {MAX_PROMPT_RULES} 条提示词规则"));
    }
    let mut ids = HashSet::new();
    for rule in rules {
        if !valid_rule_id(&rule.id) || !ids.insert(&rule.id) {
            return Err("提示词规则标识无效或重复".into());
        }
        if rule.name.chars().count() > 80 || rule.name.chars().any(char::is_control) {
            return Err("规则名称最长 80 个字符，不能包含换行等控制字符".into());
        }
        if rule.text.len() > MAX_PROMPT_TEXT_BYTES {
            return Err("提示词正文不能超过 1 MiB".into());
        }
        if rule.file.len() > 4096 || rule.file.chars().any(char::is_control) {
            return Err("提示词文件路径无效或过长".into());
        }
        if rule.conditions.len() > MAX_PROMPT_CONDITIONS {
            return Err(format!("每条规则最多允许 {MAX_PROMPT_CONDITIONS} 个条件"));
        }
        let mut conditions = HashSet::new();
        for condition in &rule.conditions {
            if !valid_rule_id(&condition.id) || !conditions.insert(&condition.id) {
                return Err("条件标识无效或重复".into());
            }
            if condition.field != PromptField::Model
                && !matches!(condition.op, PromptOperator::Is | PromptOperator::IsNot)
            {
                return Err("提供商和客户端条件只能使用「是 / 不是」".into());
            }
            let limit = if condition.field == PromptField::Model {
                1
            } else {
                MAX_PROMPT_VALUES
            };
            if condition.values.len() > limit {
                return Err("条件值数量超过上限".into());
            }
            let mut values = HashSet::new();
            for value in &condition.values {
                if value.trim() != value
                    || value.is_empty()
                    || value.chars().count() > MAX_PROMPT_PATTERN_CHARS
                    || value.chars().any(char::is_control)
                    || !values.insert(value)
                {
                    return Err("条件值不能为空、重复或含控制字符，最长 256 个字符".into());
                }
                if condition.field != PromptField::Model && !valid_rule_id(value) {
                    return Err("提供商或客户端标识无效".into());
                }
            }
            PromptMatcher::compile(condition)?;
        }
    }
    Ok(())
}

/// 编译一条规则的全部条件匹配器。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn compile_prompt_rule(rule: &PromptRule) -> Result<Vec<PromptMatcher>, String> {
    rule.conditions.iter().map(PromptMatcher::compile).collect()
}

/// 一组已编译规则。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct CompiledPromptRules {
    entries: Vec<(PromptRule, Vec<PromptMatcher>)>,
}

#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
impl CompiledPromptRules {
    /// 编译 + 校验；任一失败即整体失败（与参考树 `compile` 同口径）。
    pub fn new(rules: Vec<PromptRule>) -> Result<Self, String> {
        validate_prompt_rules(&rules)?;
        let mut entries = Vec::with_capacity(rules.len());
        for rule in rules {
            let matchers = compile_prompt_rule(&rule)?;
            entries.push((rule, matchers));
        }
        Ok(Self { entries })
    }

    /// 返回第一条命中且启用的规则。
    pub fn first_match(&self, provider: &str, client: &str, model: &str) -> Option<&PromptRule> {
        self.entries
            .iter()
            .find(|(rule, matchers)| {
                rule.enabled
                    && !matchers.is_empty()
                    && match rule.logic {
                        PromptLogic::All => {
                            matchers.iter().all(|m| m.matches(provider, client, model))
                        }
                        PromptLogic::Any => {
                            matchers.iter().any(|m| m.matches(provider, client, model))
                        }
                    }
            })
            .map(|(rule, _)| rule)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 单条条件是否命中（临时编译；规则为 opt-in，默认空表不进入）。
fn condition_matches(
    condition: &PromptCondition,
    provider: &str,
    client: &str,
    model: &str,
) -> bool {
    match PromptMatcher::compile(condition) {
        Ok(matcher) => matcher.matches(provider, client, model),
        Err(_) => false,
    }
}

/// 返回第一条命中且启用的规则（引用绑定 `rules`，不借用临时值）。
pub fn first_matching_rule<'a>(
    rules: &'a [PromptRule],
    provider: &str,
    client: &str,
    model: &str,
) -> Option<&'a PromptRule> {
    rules.iter().find(|rule| {
        if !rule.enabled || rule.conditions.is_empty() {
            return false;
        }
        let matches =
            |condition: &PromptCondition| condition_matches(condition, provider, client, model);
        match rule.logic {
            PromptLogic::All => rule.conditions.iter().all(matches),
            PromptLogic::Any => rule.conditions.iter().any(matches),
        }
    })
}

/// 提示词模式（字符串形态是 API 契约，见 `as_str`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// 透传客户端 system（默认）
    #[default]
    Passthrough,
    /// 用网关提示词替换客户端的 system 消息
    Custom,
    /// 在开头连续 system 块之后追加网关提示词
    Append,
}

impl PromptMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passthrough => "passthrough",
            Self::Custom => "custom",
            Self::Append => "append",
        }
    }

    /// 解析：大小写不敏感 + 去首尾空白；空串按默认，非法值 None。
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "" | "passthrough" => Some(Self::Passthrough),
            "custom" => Some(Self::Custom),
            "append" => Some(Self::Append),
            _ => None,
        }
    }
}

/// 单个模型 / 单个提供商的提示词覆盖。
///
/// AIGX 适配形态：A2A 按 provider 覆盖（它一家一个上游），而 AIGX 的数据面
/// 入口在解析阶段只拿到客户端请求的**模型名**，provider 要等选路后才确定。
/// 因此这里按「模型名」覆盖 —— 语义等价（都是「这一路请求用另一套提示词」），
/// 且能在入口处直接判定；**提供商层**（[`GatewayPrompt::providers`]）在
/// 选路后的逐次尝试里生效。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPromptOverride {
    #[serde(default)]
    pub mode: PromptMode,
    /// 该模型的提示词文本（空文本 = 该模型不做注入）
    #[serde(default)]
    pub text: String,
}

/// 网关提示词配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayPrompt {
    /// 注入模式
    #[serde(default)]
    pub mode: PromptMode,
    /// 提示词文本（custom/append 模式生效；空文本等价 passthrough）
    #[serde(default)]
    pub text: String,
    /// 按模型覆盖（键 = 客户端请求里的模型名原文）：命中则整条替代全局
    /// mode/text，未命中回落提供商 / 全局。空表 = 全部走下一层（默认路径）。
    #[serde(default)]
    pub models: std::collections::BTreeMap<String, ModelPromptOverride>,
    /// 按提供商覆盖（键 = 渠道/账号 provider）：
    /// - 账号型渠道取账号的 provider（`account_sessions.provider_for_channel`，
    ///   与出站脱敏的 provider 作用域同一口径）；
    /// - 其余渠道取渠道类型名（openai_compatible / anthropic / gemini / zai …）。
    ///
    /// 优先级：模型覆盖 > 提供商覆盖 > 全局。命中提供商层时整条替代全局
    /// mode/text；空文本 = 该提供商不注入。空表 = 全部走全局（默认路径，
    /// 数据面零额外分配）。
    #[serde(default)]
    pub providers: std::collections::BTreeMap<String, ModelPromptOverride>,
    /// developer 角色兼容（对照 A2A developerRoleCompat，默认开）：
    /// OpenAI 新 SDK 用 `role:"developer"` 取代 system，多数上游只认
    /// system——开启时入口解析把 developer 归一为 system；关闭时按原
    /// 样丢弃未知角色（引入该开关前的行为）。
    #[serde(default = "default_developer_role_compat")]
    pub developer_role_compat: bool,
    /// 网关自带提示词逐家开关（A2A `promptGateway`）：`{"<providerId>": false}`
    /// = 这一家不装网关内置段。键缺失 = 默认（装）。网关只存开关与正文，
    /// 真正的拼装/注入在 provider 实现侧（A2A 2026-10 已插件化；AIGX 原生
    /// zcode 目前只注入用户覆盖文本，官方原文属于闭源插件知识）。
    #[serde(default)]
    pub gateway: std::collections::BTreeMap<String, bool>,
    /// 网关自带提示词正文覆盖（A2A `promptGatewayText`）：三段各自可覆盖，
    /// 缺段 = 用官方原文（由 provider 实现提供）。空表 = 全部官方原文。
    #[serde(default)]
    pub gateway_text: std::collections::BTreeMap<String, GatewayTextBlocks>,
    /// 条件规则（对齐 Agent2API v3.5.0-beta.2 `promptRules`）。
    ///
    /// 优先级最高：命中第一条启用规则时整条替代 models/providers/全局。
    /// 空表 = 不参与判定（默认，零行为变化）。
    #[serde(default)]
    pub rules: Vec<PromptRule>,
}

/// 网关自带提示词的三段正文（A2A `{identity, stable, dynamic}`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GatewayTextBlocks {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dynamic: Option<String>,
}

fn default_developer_role_compat() -> bool {
    true
}

impl Default for GatewayPrompt {
    fn default() -> Self {
        Self {
            mode: PromptMode::Passthrough,
            text: String::new(),
            models: std::collections::BTreeMap::new(),
            providers: std::collections::BTreeMap::new(),
            developer_role_compat: default_developer_role_compat(),
            gateway: std::collections::BTreeMap::new(),
            gateway_text: std::collections::BTreeMap::new(),
            rules: Vec::new(),
        }
    }
}

/// 进程级持有者：每请求快照（Arc clone），保存即生效。
pub struct GatewayPromptStore {
    current: parking_lot::RwLock<Arc<GatewayPrompt>>,
    store: Option<Arc<FileStore>>,
}

impl GatewayPromptStore {
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn in_memory(cfg: GatewayPrompt) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(cfg)),
            store: None,
        }
    }

    pub fn with_store(store: Arc<FileStore>) -> Self {
        let cfg = store
            .get::<GatewayPrompt>(GATEWAY_PROMPT_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(cfg)),
            store: Some(store),
        }
    }

    pub fn snapshot(&self) -> Arc<GatewayPrompt> {
        self.current.read().clone()
    }

    pub fn update(&self, cfg: GatewayPrompt) -> anyhow::Result<GatewayPrompt> {
        if let Some(store) = &self.store {
            store.put(GATEWAY_PROMPT_KEY, &cfg)?;
        }
        *self.current.write() = Arc::new(cfg.clone());
        Ok(cfg)
    }
}

/// 对归一化消息列表应用提示词策略（数据面注入点）。
///
/// 返回 `Some(新列表)` = 改动过（调用方用返回值替换）；`None` = 不动
/// （passthrough 或文本为空——默认路径零开销，连 Vec 都不分配）。
/// 进程级网关提示词快照（插件回调 `settings.get` 读 gatewayPrompt* 用）。
///
/// 与安全策略快照同构：启动/保存时 install 一次，读侧无锁读 Arc。
static GATEWAY_PROMPT_SNAPSHOT: parking_lot::RwLock<Option<Arc<GatewayPrompt>>> =
    parking_lot::RwLock::new(None);

/// 安装/刷新进程级快照（main 启动与 `/api/gateway/prompt` 保存后调用）。
pub fn install_snapshot(cfg: Arc<GatewayPrompt>) {
    *GATEWAY_PROMPT_SNAPSHOT.write() = Some(cfg);
}

/// 读进程级快照（未安装时返回 None）。
pub fn snapshot() -> Option<Arc<GatewayPrompt>> {
    GATEWAY_PROMPT_SNAPSHOT.read().clone()
}

/// 提示词配置的修订号（SHA256），供 `/api/prompt/rules` 保存时做乐观并发校验。
pub fn revision(cfg: &GatewayPrompt) -> String {
    use sha2::{Digest, Sha256};
    let text = serde_json::to_string(cfg).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn apply_prompt(messages: &[ChatMessage], cfg: &GatewayPrompt) -> Option<Vec<ChatMessage>> {
    apply_prompt_for("", messages, cfg)
}

/// 按模型名应用提示词策略：模型覆盖 > 全局。
///
/// 入口解析阶段（选路前）用它 —— 此时还不知道本次请求会落到哪一家，
/// 提供商层由 [`apply_prompt_selected`] 在**逐次尝试**里补上。
pub fn apply_prompt_for(
    model: &str,
    messages: &[ChatMessage],
    cfg: &GatewayPrompt,
) -> Option<Vec<ChatMessage>> {
    let (mode, text) = resolve_layers(model, None, cfg);
    apply_with(mode, text, messages)
}

/// 按模型名 + 提供商应用提示词策略：模型覆盖 > 提供商覆盖 > 全局。
///
/// 数据面的**唯一推荐入口**：选路后逐次尝试时调用，`provider` 取
/// `api::openai::sanitize_provider_id`（账号型渠道 = 账号 provider，
/// 其余 = 渠道类型名），与出站脱敏同一口径。`provider` 为空串时只剩
/// 模型覆盖 / 全局两层（等价 [`apply_prompt_for`]）。
///
/// **为什么在逐次尝试里而不是入口**：同一请求可能 failover 到不同 provider
/// 的渠道，每换一家都要按那一家的覆盖重新套一遍；入口那次（无 provider）
/// 的结果不能复用。见 chat/anthropic 两个 failover 循环的调用点。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn apply_prompt_selected(
    model: &str,
    provider: &str,
    messages: &[ChatMessage],
    cfg: &GatewayPrompt,
) -> Option<Vec<ChatMessage>> {
    let (mode, text) = resolve_layers(model, Some(provider), cfg);
    apply_with(mode, text, messages)
}

/// 带客户端维度的应用（规则层的 `client` 条件仅在此入口生效）。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn apply_prompt_full(
    model: &str,
    provider: &str,
    client: &str,
    messages: &[ChatMessage],
    cfg: &GatewayPrompt,
) -> Option<Vec<ChatMessage>> {
    let (mode, text) = resolve_layers_full(model, Some(provider), client, cfg);
    apply_with(mode, text, messages)
}

/// 提供商层重套判据（failover 循环专用）：提供商层的解析结果与入口层
/// （模型覆盖 > 全局）**不同**时，返回从 `pre_purge` 快照重建的消息列表；
/// 相同（或无提供商覆盖 / provider 为空）时返回 `None`——调用方什么都不
/// 用做，零开销。
///
/// **为什么重建而不是就地改写**：入口层注入之后紧跟破甲治理（purge，
/// 非幂等——structured/skill 会向 user 消息追加文本），failover 的每次
/// 尝试都从已治理的 `chat_req` 克隆而来。提供商层命中时若只对克隆体改写，
/// 治理产物会残留/叠加；所以必须回退到 purge 前的入口层消息、按三层
/// 解析重套、再由调用方重跑一次 purge。
///
/// `pre_purge` = 入口层 [`apply_prompt_for`] 之后、purge 之前的消息快照
/// （仅 `cfg.providers` 非空时才需要调用方 clone，否则入口直接跳过）。
/// `model` 与入口层同口径 = 客户端请求里的模型名原文。
pub fn provider_prompt_rebuild(
    model: &str,
    provider: &str,
    pre_purge: &[ChatMessage],
    cfg: &GatewayPrompt,
) -> Option<Vec<ChatMessage>> {
    let provider = provider.trim();
    if cfg.providers.is_empty() || provider.is_empty() {
        return None;
    }
    let entry = resolve_layers(model, None, cfg);
    let selected = resolve_layers(model, Some(provider), cfg);
    if entry == selected {
        return None;
    }
    // 文本为空 = 该提供商不注入（撤销入口层的注入），快照原样返回
    Some(apply_with(selected.0, selected.1, pre_purge).unwrap_or_else(|| pre_purge.to_vec()))
}

/// 三层解析：模型覆盖 > 提供商覆盖 > 全局（返回借用视图，不克隆文本）。
///
/// 命中某层后**整条替代**（mode + text 一起），不再向下回落 —— 与模型层
/// 既有语义一致：覆盖层是「这一路请求用另一套提示词」，不是逐字段合并。
fn resolve_layers<'a>(
    model: &str,
    provider: Option<&str>,
    cfg: &'a GatewayPrompt,
) -> (PromptMode, &'a str) {
    resolve_layers_full(model, provider, "", cfg)
}

/// 带客户端维度的解析（规则层可用 `client` 条件）。
fn resolve_layers_full<'a>(
    model: &str,
    provider: Option<&str>,
    client: &str,
    cfg: &'a GatewayPrompt,
) -> (PromptMode, &'a str) {
    // 规则层最高优先级；空表直接跳过（默认零开销）。逐条判定，命中返回
    // `cfg.rules` 里的规则引用（生命周期绑定 cfg，不借用临时编译产物）。
    if !cfg.rules.is_empty() {
        if let Some(rule) = first_matching_rule(&cfg.rules, provider.unwrap_or(""), client, model) {
            return rule.content();
        }
    }
    if let Some(over) = cfg.models.get(model) {
        return (over.mode, over.text.as_str());
    }
    if let Some(provider) = provider.filter(|p| !p.is_empty()) {
        if let Some(over) = cfg.providers.get(provider) {
            return (over.mode, over.text.as_str());
        }
    }
    (cfg.mode, cfg.text.as_str())
}

/// 提示词策略本体：给定了模式与文本时如何改写消息列表。
fn apply_with(mode: PromptMode, text: &str, messages: &[ChatMessage]) -> Option<Vec<ChatMessage>> {
    if text.trim().is_empty() {
        return None;
    }
    let gateway = || ChatMessage {
        role: Role::System,
        content: Some(text.to_string()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    };
    match mode {
        PromptMode::Passthrough => None,
        PromptMode::Custom => {
            // 替换：删掉全部 system 消息，头部插网关提示词
            let mut next = Vec::with_capacity(messages.len() + 1);
            next.push(gateway());
            next.extend(messages.iter().filter(|m| m.role != Role::System).cloned());
            Some(next)
        }
        PromptMode::Append => {
            // 追加：开头连续 system 块之后插入一条，既有消息不动
            let insert_at = messages
                .iter()
                .take_while(|m| m.role == Role::System)
                .count();
            let mut next = Vec::with_capacity(messages.len() + 1);
            next.extend_from_slice(&messages[..insert_at]);
            next.push(gateway());
            next.extend_from_slice(&messages[insert_at..]);
            Some(next)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sys(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::System,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    #[test]
    fn passthrough_and_empty_text_are_noop() {
        let msgs = vec![sys("client"), user("hi")];
        let cfg = GatewayPrompt::default();
        assert!(apply_prompt(&msgs, &cfg).is_none());
        // append 模式但文本为空 → 同样不动
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "   ".to_string(),
            ..GatewayPrompt::default()
        };
        assert!(apply_prompt(&msgs, &cfg).is_none());
    }

    #[test]
    fn custom_replaces_all_system() {
        let msgs = vec![sys("a"), user("q"), sys("b"), user("r")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GATEWAY".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out.len(), 3, "两条 system 都被删，插入一条网关的");
        assert_eq!(out[0].content.as_deref(), Some("GATEWAY"));
        assert_eq!(out[1].content.as_deref(), Some("q"));
        assert_eq!(out[2].content.as_deref(), Some("r"));
        assert!(out.iter().filter(|m| m.role == Role::System).count() == 1);
    }

    #[test]
    fn append_keeps_existing_and_inserts_after_leading_system() {
        let msgs = vec![sys("a"), sys("b"), user("q"), sys("late")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "GATEWAY".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out.len(), 5);
        // 位置：a, b, GATEWAY, q, late（中段/尾部的 system 原样保留）
        assert_eq!(out[0].content.as_deref(), Some("a"));
        assert_eq!(out[1].content.as_deref(), Some("b"));
        assert_eq!(out[2].content.as_deref(), Some("GATEWAY"));
        assert_eq!(out[3].content.as_deref(), Some("q"));
        assert_eq!(out[4].content.as_deref(), Some("late"));
    }

    #[test]
    fn append_without_leading_system_inserts_at_head() {
        let msgs = vec![user("q")];
        let cfg = GatewayPrompt {
            mode: PromptMode::Append,
            text: "G".to_string(),
            ..GatewayPrompt::default()
        };
        let out = apply_prompt(&msgs, &cfg).unwrap();
        assert_eq!(out[0].content.as_deref(), Some("G"));
        assert_eq!(out[1].content.as_deref(), Some("q"));
    }

    #[test]
    fn model_override_beats_global_and_falls_back_when_absent() {
        let msgs = vec![user("q")];
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "gpt-4o".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "PER-MODEL".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            models,
            ..GatewayPrompt::default()
        };
        // 命中覆盖：用模型自己的文本
        let hit = apply_prompt_for("gpt-4o", &msgs, &cfg).unwrap();
        assert_eq!(hit[0].content.as_deref(), Some("PER-MODEL"));
        // 未命中：回落全局
        let miss = apply_prompt_for("claude-3-7", &msgs, &cfg).unwrap();
        assert_eq!(miss[0].content.as_deref(), Some("GLOBAL"));
    }

    #[test]
    fn model_override_with_empty_text_disables_injection_for_that_model() {
        let msgs = vec![user("q")];
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "silent-model".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "  ".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            models,
            ..GatewayPrompt::default()
        };
        // 该模型文本为空 → 不做任何注入（即使全局是 custom）
        assert!(apply_prompt_for("silent-model", &msgs, &cfg).is_none());
        // 其它模型仍走全局
        assert!(apply_prompt_for("other", &msgs, &cfg).is_some());
    }

    #[test]
    fn provider_override_beats_global_and_model_beats_provider() {
        let msgs = vec![user("q")];
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "anthropic".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "PER-PROVIDER".to_string(),
            },
        );
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "claude-3-7".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Custom,
                text: "PER-MODEL".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            models,
            providers,
            ..GatewayPrompt::default()
        };
        // 三层全在：模型 > 提供商 > 全局
        let model_hit = apply_prompt_selected("claude-3-7", "anthropic", &msgs, &cfg).unwrap();
        assert_eq!(model_hit[0].content.as_deref(), Some("PER-MODEL"));
        // 模型未命中、提供商命中：用提供商层（整条替代，不是 mode 混合）
        let provider_hit = apply_prompt_selected("gpt-4o", "anthropic", &msgs, &cfg).unwrap();
        assert_eq!(provider_hit[0].content.as_deref(), Some("PER-PROVIDER"));
        // 两层都未命中：回落全局
        let fallback = apply_prompt_selected("gpt-4o", "gemini", &msgs, &cfg).unwrap();
        assert_eq!(fallback[0].content.as_deref(), Some("GLOBAL"));
    }

    #[test]
    fn provider_override_empty_text_disables_injection() {
        let msgs = vec![user("q")];
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "qoder".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: " ".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            providers,
            ..GatewayPrompt::default()
        };
        // 该提供商文本为空 → 不做注入（即使全局是 custom）
        assert!(apply_prompt_selected("m", "qoder", &msgs, &cfg).is_none());
        // 其它提供商仍走全局
        assert!(apply_prompt_selected("m", "trae", &msgs, &cfg).is_some());
    }

    #[test]
    fn empty_provider_falls_through_to_global() {
        // 取不到 provider（空串）时不允许命中 providers 表里的空键
        let msgs = vec![user("q")];
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            String::new(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "EMPTY-KEY".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            providers,
            ..GatewayPrompt::default()
        };
        let out = apply_prompt_selected("m", "", &msgs, &cfg).unwrap();
        assert_eq!(out[0].content.as_deref(), Some("GLOBAL"));
    }

    /// 提供商层重套判据：providers 空 / provider 空 / 解析结果与入口层相同
    /// → None（零开销）；不同 → 从 pre_purge 快照重建（含撤销入口注入的
    /// 空文本分支）。
    #[test]
    fn provider_prompt_rebuild_triage() {
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "anthropic".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Append,
                text: "PER-PROVIDER".to_string(),
            },
        );
        // providers 表为空 → 永远 None
        let cfg_empty = GatewayPrompt::default();
        let pre = vec![user("q")];
        assert!(provider_prompt_rebuild("m", "anthropic", &pre, &cfg_empty).is_none());

        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            providers,
            ..GatewayPrompt::default()
        };
        // provider 为空 → None
        assert!(provider_prompt_rebuild("m", "  ", &pre, &cfg).is_none());
        // 提供商未命中（回落全局，与入口层相同）→ None
        assert!(provider_prompt_rebuild("m", "gemini", &pre, &cfg).is_none());
        // 命中且与入口层不同 → 重建：网关文本 = 提供商层的
        let rebuilt = provider_prompt_rebuild("m", "anthropic", &pre, &cfg).unwrap();
        assert_eq!(rebuilt.len(), 2);
        assert_eq!(rebuilt[0].content.as_deref(), Some("PER-PROVIDER"));
        assert_eq!(rebuilt[1].content.as_deref(), Some("q"));
        // pre_purge 快照未被就地改写（重建是纯拷贝语义）
        assert_eq!(pre[0].content.as_deref(), Some("q"));
    }

    #[test]
    fn provider_prompt_rebuild_empty_text_revokes_entry_injection() {
        // 入口层是 custom 注入、提供商层文本为空（= 该提供商不注入）：
        // 重建结果应撤销注入（回到 purge 前快照的形态）
        let mut providers = std::collections::BTreeMap::new();
        providers.insert(
            "qoder".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Custom,
                text: "  ".to_string(),
            },
        );
        let cfg = GatewayPrompt {
            mode: PromptMode::Custom,
            text: "GLOBAL".to_string(),
            providers,
            ..GatewayPrompt::default()
        };
        let pre = vec![user("q")];
        let rebuilt = provider_prompt_rebuild("m", "qoder", &pre, &cfg).unwrap();
        assert_eq!(rebuilt.len(), 1);
        assert_eq!(rebuilt[0].content.as_deref(), Some("q"));
        // 模型覆盖命中同一提供商层时也参与比较：模型层命中则入口与所选
        // 解析可能相同（都命中同一覆盖）→ None
        let mut models = std::collections::BTreeMap::new();
        models.insert(
            "m".to_string(),
            ModelPromptOverride {
                mode: PromptMode::Custom,
                text: "  ".to_string(),
            },
        );
        let cfg2 = GatewayPrompt {
            models,
            ..cfg.clone()
        };
        // 模型覆盖（空文本）命中 → 入口层=所选层=同一覆盖 → None
        assert!(provider_prompt_rebuild("m", "qoder", &pre, &cfg2).is_none());
    }

    #[test]
    fn mode_parse() {
        assert_eq!(PromptMode::parse(""), Some(PromptMode::Passthrough));
        assert_eq!(PromptMode::parse(" Custom "), Some(PromptMode::Custom));
        assert_eq!(PromptMode::parse("APPEND"), Some(PromptMode::Append));
        assert_eq!(PromptMode::parse("bogus"), None);
    }

    #[test]
    fn config_roundtrip_defaults() {
        let v: GatewayPrompt = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(v.mode, PromptMode::Passthrough);
        assert_eq!(v.text, "hi");
        // 旧数据（无 developer_role_compat 字段）回落默认开
        assert!(v.developer_role_compat);
    }

    #[test]
    fn developer_compat_can_be_disabled() {
        let v: GatewayPrompt = serde_json::from_str(r#"{"developer_role_compat":false}"#).unwrap();
        assert!(!v.developer_role_compat);
    }

    /// developer 角色归一（数据面入口解析层；A2A developerRoleCompat）。
    /// 兼容开关开：developer → system；关：按未知角色丢弃（老行为）。
    #[test]
    fn parse_messages_developer_role_compat() {
        use serde_json::json;
        let body = json!([
            { "role": "developer", "content": "You are a helpful assistant." },
            { "role": "user", "content": "hi" },
        ]);
        let raw = serde_json::to_value(&body).unwrap();

        // 开：归一为 system，内容逐字保留
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw), true).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::System);
        assert_eq!(
            msgs[0].content.as_deref(),
            Some("You are a helpful assistant.")
        );
        assert_eq!(msgs[1].role, Role::User);

        // 关：developer 按未知角色丢弃（引入兼容前的行为）
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw), false).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);

        // system/tool 不受开关影响
        let body2 = json!([
            { "role": "system", "content": "s" },
            { "role": "tool", "content": "t" },
        ]);
        let raw2 = serde_json::to_value(&body2).unwrap();
        let msgs = crate::api::openai::parse_messages_with_dev(Some(&raw2), false).unwrap();
        assert_eq!(msgs.len(), 2);
    }

    /// promptGateway 快照：缺省（键缺失）= 装；显式 false = 不装；正文覆盖只
    /// 透出用户写过的段（缺段 = 官方原文，序列化时省略字段）。
    #[test]
    fn gateway_snapshot_defaults_and_text_blocks() {
        let mut cfg = GatewayPrompt::default();
        cfg.gateway.insert("zcode".to_string(), false);
        cfg.gateway_text.insert(
            "zcode".to_string(),
            GatewayTextBlocks {
                identity: Some("你是助手".to_string()),
                stable: None,
                dynamic: Some("cwd={cwd}".to_string()),
            },
        );
        install_snapshot(std::sync::Arc::new(cfg));

        let snap = snapshot().expect("snapshot installed");
        // 键缺失 = 默认装；显式 false = 不装
        assert_eq!(snap.gateway.get("zcode"), Some(&false));
        assert_eq!(snap.gateway.get("other"), None);

        let blocks = serde_json::to_value(snap.gateway_text.get("zcode").unwrap()).unwrap();
        assert_eq!(blocks["identity"], "你是助手");
        assert_eq!(blocks["dynamic"], "cwd={cwd}");
        assert!(
            blocks.get("stable").is_none(),
            "未覆盖的段不出现（= 官方原文）：{blocks}"
        );

        // 默认配置：空表、无覆盖（数据面零额外分配）
        let default = GatewayPrompt::default();
        assert!(default.gateway.is_empty());
        assert!(default.gateway_text.is_empty());
    }
}
