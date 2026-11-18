//! 护栏配置与运行时（数据面入站/出站拦截 + HTTP 200 假答复门控）。
//!
//! ── 默认关闭、开启才生效 ─────────────────────────────────────
//! `enabled` 默认 `false`，三张词表默认全空。关闭时数据面只做一次
//! `Arc` 快照读取（与 `purge` / `gateway_prompt` 同款），不改写请求、
//! 不包装响应流、不改变任何出站字节 —— 老配置升级后逐字节不变。
//!
//! ── 三张独立的词表 ──────────────────────────────────────────
//! - `input_rules`：请求体消息文本命中 → 按 `action` 处置（降级/拒绝）；
//! - `output_rules`：响应正文命中 → 流式补错误帧 / 非流式返回错误；
//! - `fake_reply_rules`：上游把内容审核拒绝包装成 HTTP 200 时命中 →
//!   按「可轮换失败」处理（记录一次失败尝试、换下一个候选）。
//!
//! 每条规则是「字面子串（大小写不敏感）」或「正则」二选一，复用
//! [`crate::guardrail::keyword`] 的匹配实现（构造时编译一次）。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::fake_reply::FakeReplyMatcher;
use super::keyword::{KeywordBlocklist, KeywordRule};
use crate::bridge::{ChatChunkStream, ChatMessage, Role};
use crate::storage::FileStore;

/// 持久化键（FileStore，与 `purge_settings` / `gateway_prompt` 同款）。
pub const GUARDRAIL_SETTINGS_KEY: &str = "guardrail_settings";

/// 中性提示词默认文本：最小、无风格约束，只保留「助手」角色。
///
/// 降级路径用它替换客户端 system 提示词——目的是绕开**逐字指纹误报**
/// （上游按模板句精确匹配拦截），不是对抗语义审核。
pub const DEFAULT_NEUTRAL_PROMPT: &str =
    "You are a helpful AI assistant. Answer the user's request directly and concisely.";

/// 输入命中的处置策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailAction {
    /// 降级（默认）：同账号换中性提示词原地重发一次，并进入降级期
    #[default]
    Degrade,
    /// 拒绝：直接返回标准错误
    Reject,
}

impl GuardrailAction {
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Degrade => "degrade",
            Self::Reject => "reject",
        }
    }
}

/// 规则类型：字面子串 / 正则。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// 大小写不敏感子串（默认）
    #[default]
    Keyword,
    /// 正则（Rust `regex` 语法，构造时编译一次）
    Regex,
}

/// 单条规则（设置页/管理面契约）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailRuleSpec {
    /// 字面词或正则源码
    #[serde(default)]
    pub pattern: String,
    /// 匹配类型（默认 keyword）
    #[serde(default)]
    pub kind: RuleKind,
}

impl GuardrailRuleSpec {
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn keyword(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            kind: RuleKind::Keyword,
        }
    }

    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn regex(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            kind: RuleKind::Regex,
        }
    }
}

fn default_neutral_prompt() -> String {
    DEFAULT_NEUTRAL_PROMPT.to_string()
}

/// 护栏配置（全部字段可缺省，热更新）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GuardrailSettings {
    /// 总开关（默认关闭——关闭时数据面零行为变化）
    pub enabled: bool,
    /// 输入命中策略（默认降级）
    pub action: GuardrailAction,
    /// 输入词表（默认空 = 不检查）
    pub input_rules: Vec<GuardrailRuleSpec>,
    /// 输出词表（默认空 = 不检查）
    pub output_rules: Vec<GuardrailRuleSpec>,
    /// HTTP 200 假答复词表（默认空 = 不启用）
    pub fake_reply_rules: Vec<GuardrailRuleSpec>,
    /// 降级时使用的中性提示词（空 = 内置默认文本）
    pub neutral_prompt: String,
}

impl Default for GuardrailSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            action: GuardrailAction::Degrade,
            input_rules: Vec::new(),
            output_rules: Vec::new(),
            fake_reply_rules: Vec::new(),
            neutral_prompt: default_neutral_prompt(),
        }
    }
}

fn compile_rules(label: &str, rules: &[GuardrailRuleSpec]) -> Vec<KeywordRule> {
    rules
        .iter()
        .filter_map(|rule| {
            let pattern = rule.pattern.trim();
            if pattern.is_empty() {
                return None;
            }
            match rule.kind {
                RuleKind::Keyword => Some(KeywordRule::literal(pattern)),
                RuleKind::Regex => match KeywordRule::regex(pattern) {
                    Ok(compiled) => Some(compiled),
                    Err(e) => {
                        // 保存时已校验；这里兜住手改 KV / 旧坏配置：跳过该条而不是崩
                        tracing::warn!("guardrail: ignore invalid {label} regex {pattern:?}: {e}");
                        None
                    }
                },
            }
        })
        .collect()
}

/// 编译后的运行时快照（每请求 `Arc` clone，保存即生效）。
pub struct GuardrailRuntime {
    settings: GuardrailSettings,
    input: Option<KeywordBlocklist>,
    output: Option<KeywordBlocklist>,
    fake_reply: FakeReplyMatcher,
}

impl GuardrailRuntime {
    pub fn compile(settings: GuardrailSettings) -> Self {
        let enabled = settings.enabled;
        let input_rules = compile_rules("input", &settings.input_rules);
        let output_rules = compile_rules("output", &settings.output_rules);
        let fake_rules = compile_rules("fake_reply", &settings.fake_reply_rules);
        Self {
            input: (enabled && !input_rules.is_empty())
                .then(|| KeywordBlocklist::input_only(input_rules)),
            output: (enabled && !output_rules.is_empty())
                .then(|| KeywordBlocklist::output_only(output_rules)),
            fake_reply: if enabled {
                FakeReplyMatcher::new(fake_rules)
            } else {
                FakeReplyMatcher::empty()
            },
            settings,
        }
    }

    pub fn settings(&self) -> &GuardrailSettings {
        &self.settings
    }

    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn action(&self) -> GuardrailAction {
        self.settings.action
    }

    /// 生效的中性提示词（配置为空时回落到内置默认文本）。
    pub fn neutral_prompt(&self) -> &str {
        let configured = self.settings.neutral_prompt.trim();
        if configured.is_empty() {
            DEFAULT_NEUTRAL_PROMPT
        } else {
            configured
        }
    }

    pub fn input_enabled(&self) -> bool {
        self.input.is_some()
    }

    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn output_enabled(&self) -> bool {
        self.output.is_some()
    }

    pub fn fake_reply_enabled(&self) -> bool {
        self.fake_reply.is_enabled()
    }

    /// 输入侧裁决（同步、纯函数，便于单测）。
    ///
    /// `degrade_active` = 本请求开始时降级状态机是否已生效：
    /// - 已生效：system 直接换成中性提示词，命中规则则拒绝（**不再重发**——
    ///   重发同一份中性提示词毫无意义，必须收敛）；
    /// - 未生效：首次命中返回 `Degrade`（调用方触发降级并换中性提示词），
    ///   第二次裁决要么放行、要么拒绝，绝不会再返回 `Degrade`。
    pub fn decide_input(&self, messages: &[ChatMessage], degrade_active: bool) -> InputDecision {
        let Some(input) = &self.input else {
            return InputDecision::Allow { messages: None };
        };
        let neutral = if degrade_active {
            Some(neutral_messages(messages, self.neutral_prompt()))
        } else {
            None
        };
        let effective = neutral.as_deref().unwrap_or(messages);
        let combined = combined_text(effective);
        let Some(reason) = combined
            .as_deref()
            .and_then(|text| input.first_match_text(text))
        else {
            return InputDecision::Allow { messages: neutral };
        };
        if degrade_active || self.settings.action == GuardrailAction::Reject {
            InputDecision::Reject { reason }
        } else {
            InputDecision::Degrade {
                reason,
                messages: neutral_messages(messages, self.neutral_prompt()),
            }
        }
    }

    /// 响应正文命中输出规则时返回规则描述（非流式/流式首个命中点共用）。
    pub fn output_hit(&self, text: &str) -> Option<String> {
        self.output.as_ref()?.first_match_text(text)
    }

    /// 输出流扫描器（未启用输出规则时 `None`——调用方原流直下）。
    pub fn output_scanner(&self) -> Option<OutputScanner> {
        self.output.as_ref().map(|blocklist| OutputScanner {
            blocklist: blocklist.clone(),
            buf: String::new(),
            blocked: false,
        })
    }

    /// HTTP 200 假答复正向判定。
    pub fn fake_reply_hit(&self, text: &str) -> Option<String> {
        self.fake_reply.hit(text)
    }

    pub fn fake_reply(&self) -> &FakeReplyMatcher {
        &self.fake_reply
    }
}

/// 输入侧裁决结果。
#[derive(Debug)]
pub enum InputDecision {
    /// 放行；`messages` 为 `Some` 表示降级期已把 system 换成中性提示词
    Allow { messages: Option<Vec<ChatMessage>> },
    /// 拒绝（策略=reject，或降级期仍命中规则）
    Reject { reason: String },
    /// 首次命中：换中性提示词重发一次（调用方触发降级状态并替换消息）
    Degrade {
        reason: String,
        messages: Vec<ChatMessage>,
    },
}

/// 把消息文本拼成一段（与 [`crate::guardrail::Guardrail::check_input`] 同口径）。
fn combined_text(messages: &[ChatMessage]) -> Option<String> {
    let combined = messages
        .iter()
        .map(|m| m.content_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!combined.is_empty()).then_some(combined)
}

/// 构造降级消息列表：system 全部替换为一条中性提示词，其余消息原样保留。
///
/// 与参考项目同口径：只换**系统提示词**（逐字模板误报的载体），
/// 不改用户内容——改用户内容等于篡改请求。
pub fn neutral_messages(messages: &[ChatMessage], neutral_prompt: &str) -> Vec<ChatMessage> {
    let prompt = if neutral_prompt.trim().is_empty() {
        DEFAULT_NEUTRAL_PROMPT
    } else {
        neutral_prompt.trim()
    };
    let mut out = Vec::with_capacity(messages.len() + 1);
    out.push(ChatMessage {
        role: Role::System,
        content: Some(prompt.to_string()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    });
    out.extend(messages.iter().filter(|m| m.role != Role::System).cloned());
    out
}

/// 流式输出扫描窗口（字符）。只保留最近这么多字符做跨 chunk 规则匹配：
/// 命中判定在**每个 chunk 到达时**做，窗口外的历史内容不再回扫
/// （规则长度远小于窗口；命中会在拼出完整规则的那一拍被发现）。
const SCAN_WINDOW_CHARS: usize = 4096;

/// 流式输出扫描器：逐 chunk 累积 + 匹配，命中后锁定（不再放行后续内容）。
pub struct OutputScanner {
    blocklist: KeywordBlocklist,
    buf: String,
    blocked: bool,
}

impl OutputScanner {
    /// 追加一段文本；命中规则时返回规则描述并锁定扫描器。
    pub fn push(&mut self, text: &str) -> Option<String> {
        if self.blocked || text.is_empty() {
            return None;
        }
        self.buf.push_str(text);
        self.trim_window();
        match self.blocklist.first_match_text(&self.buf) {
            Some(reason) => {
                self.blocked = true;
                Some(reason)
            }
            None => None,
        }
    }

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn blocked(&self) -> bool {
        self.blocked
    }

    fn trim_window(&mut self) {
        let count = self.buf.chars().count();
        if count <= SCAN_WINDOW_CHARS {
            return;
        }
        let drop_chars = count - SCAN_WINDOW_CHARS;
        if let Some((cut, _)) = self.buf.char_indices().nth(drop_chars) {
            self.buf.drain(..cut);
        }
    }
}

/// 用输出规则包装上游 chunk 流。
///
/// 未启用输出规则时**原样返回**（零包装、零分配）。命中时把错误作为
/// 流的最后一项吐出（`BridgeError::Config`），随即终止上游流——下游
/// 既有的 `Err` 分支会按标准错误帧收尾（chat: `event: error`；anthropic:
/// `event: error`），并置 `stream_errored`（不发 `[DONE]`、日志记错误）。
pub fn guard_output_stream(stream: ChatChunkStream, runtime: &GuardrailRuntime) -> ChatChunkStream {
    let Some(scanner) = runtime.output_scanner() else {
        return stream;
    };
    Box::pin(futures::stream::unfold(
        (stream, Some(scanner)),
        |(mut stream, scanner)| async move {
            use futures::StreamExt;
            let mut scanner = scanner?;
            match stream.next().await {
                Some(Ok(chunk)) => {
                    let mut hit = None;
                    if let Some(text) = chunk.delta.content.as_deref() {
                        hit = scanner.push(text);
                    }
                    if hit.is_none() {
                        if let Some(reasoning) = chunk.delta.reasoning.as_deref() {
                            hit = scanner.push(reasoning);
                        }
                    }
                    match hit {
                        Some(reason) => {
                            tracing::warn!("guardrail: output blocked mid-stream by {reason}");
                            Some((
                                Err(crate::bridge::BridgeError::Config(format!(
                                    "guardrail: output blocked by {reason}"
                                ))),
                                (stream, None),
                            ))
                        }
                        None => Some((Ok(chunk), (stream, Some(scanner)))),
                    }
                }
                // 上游错误原样交还；后续不再扫描
                Some(Err(e)) => Some((Err(e), (stream, None))),
                None => None,
            }
        },
    ))
}

/// 规则归一化/校验（管理面保存前调用）。
///
/// - 词条去首尾空白、丢空串、上限 500 条/表；
/// - 正则在保存时就编译验证，非法直接报错（避免「存进去了但不生效」）；
/// - 中性提示词为空时回落默认文本。
pub fn normalize_settings(mut settings: GuardrailSettings) -> Result<GuardrailSettings, String> {
    fn normalize_rules(
        label: &str,
        rules: Vec<GuardrailRuleSpec>,
    ) -> Result<Vec<GuardrailRuleSpec>, String> {
        let mut out = Vec::with_capacity(rules.len());
        for rule in rules {
            let pattern = rule.pattern.trim().to_string();
            if pattern.is_empty() {
                continue;
            }
            if rule.kind == RuleKind::Regex {
                regex::Regex::new(&pattern)
                    .map_err(|e| format!("{label} 正则无效 `{pattern}`: {e}"))?;
            }
            out.push(GuardrailRuleSpec {
                pattern,
                kind: rule.kind,
            });
            if out.len() >= 500 {
                break;
            }
        }
        Ok(out)
    }
    settings.input_rules = normalize_rules("input_rules", settings.input_rules)?;
    settings.output_rules = normalize_rules("output_rules", settings.output_rules)?;
    settings.fake_reply_rules = normalize_rules("fake_reply_rules", settings.fake_reply_rules)?;
    let neutral = settings.neutral_prompt.trim();
    settings.neutral_prompt = if neutral.is_empty() {
        default_neutral_prompt()
    } else {
        neutral.to_string()
    };
    Ok(settings)
}

/// 进程级持有者：每请求快照（Arc clone），保存即生效。
///
/// 用进程全局而不是 `AppState` 字段：数据面（chat/anthropic）与破甲/提示词
/// 同属「请求治理」层，但护栏配置有独立的持久化键与独立的生命周期。
/// 未安装（老配置、未接启动钩子）= 默认关闭 → 数据面只做一次 `OnceLock`
/// 读取，逐字节不变。
pub struct GuardrailStore {
    current: parking_lot::RwLock<Arc<GuardrailRuntime>>,
    store: Option<Arc<FileStore>>,
}

impl GuardrailStore {
    pub fn in_memory(settings: GuardrailSettings) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(GuardrailRuntime::compile(settings))),
            store: None,
        }
    }

    pub fn with_store(store: Arc<FileStore>) -> Self {
        let settings = store
            .get::<GuardrailSettings>(GUARDRAIL_SETTINGS_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(GuardrailRuntime::compile(settings))),
            store: Some(store),
        }
    }

    pub fn snapshot(&self) -> Arc<GuardrailRuntime> {
        self.current.read().clone()
    }

    pub fn settings(&self) -> GuardrailSettings {
        self.snapshot().settings().clone()
    }

    /// 是否已挂载持久化（未挂载 = 只改内存，重启丢失）。
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    pub fn update(&self, settings: GuardrailSettings) -> anyhow::Result<GuardrailSettings> {
        let settings = normalize_settings(settings).map_err(anyhow::Error::msg)?;
        if let Some(store) = &self.store {
            store.put(GUARDRAIL_SETTINGS_KEY, &settings)?;
        }
        *self.current.write() = Arc::new(GuardrailRuntime::compile(settings.clone()));
        Ok(settings)
    }
}

/// 进程级护栏存储。
///
/// 用 `std::sync::RwLock<Option<Arc<..>>>` 而不是 `OnceLock`：启动钩子
/// （`install`）可能晚于首个数据面请求（例如旧 bin 尚未接启动钩子时，
/// 管理面首次访问才补挂持久化），`OnceLock` 一旦被默认实例占位就无法
/// 再换成盘上配置 —— 那会让「已保存的配置重启后静默失效」。
type Global = std::sync::RwLock<Option<Arc<GuardrailStore>>>;
static GLOBAL: Global = std::sync::RwLock::new(None);

/// 当前存储；未安装时用一个默认关闭的临时实例（不落盘，安全的保守默认）。
fn global() -> Arc<GuardrailStore> {
    {
        let guard = GLOBAL.read().unwrap_or_else(|e| e.into_inner());
        if let Some(store) = guard.as_ref() {
            return store.clone();
        }
    }
    let mut guard = GLOBAL.write().unwrap_or_else(|e| e.into_inner());
    if let Some(store) = guard.as_ref() {
        return store.clone();
    }
    let store = Arc::new(GuardrailStore::in_memory(GuardrailSettings::default()));
    *guard = Some(store.clone());
    store
}

/// 启动钩子：从 `FileStore` 恢复已保存的护栏配置（缺省 = 全关）。
///
/// 可重复调用：后一次安装替换前一次（用于「首个请求先到、启动钩子后到」
/// 或管理面补挂持久化的场景）。
pub fn install(store: Arc<FileStore>) {
    let installed = Arc::new(GuardrailStore::with_store(store));
    let mut guard = GLOBAL.write().unwrap_or_else(|e| e.into_inner());
    *guard = Some(installed);
}

/// 确保已挂载持久化：当前实例没有存储时才安装（有则保留，避免把未落盘的
/// 内存改动读丢）。管理面 GET/PUT 入口调用。
pub fn ensure_installed(store: Arc<FileStore>) {
    {
        let guard = GLOBAL.read().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = guard.as_ref() {
            if current.has_store() {
                return;
            }
        }
    }
    install(store);
}

/// 数据面每请求快照（未安装时返回默认关闭的运行时）。
pub fn snapshot() -> Arc<GuardrailRuntime> {
    global().snapshot()
}

/// 当前配置的只读副本（管理面 GET 用）。命名避开模块 `settings`。
pub fn current_settings() -> GuardrailSettings {
    global().settings()
}

/// 保存配置（管理面 PUT 用；热生效）。
pub fn update(settings: GuardrailSettings) -> anyhow::Result<GuardrailSettings> {
    global().update(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{BridgeError, ChatChunk, ChatDelta};
    use futures::StreamExt;

    fn msg(role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Some(content.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn runtime(settings: GuardrailSettings) -> GuardrailRuntime {
        GuardrailRuntime::compile(settings)
    }

    fn degrade_cfg() -> GuardrailSettings {
        GuardrailSettings {
            enabled: true,
            action: GuardrailAction::Degrade,
            input_rules: vec![GuardrailRuleSpec::keyword("forbidden")],
            ..GuardrailSettings::default()
        }
    }

    #[test]
    fn disabled_runtime_is_zero_behavior() {
        let rt = runtime(GuardrailSettings::default());
        let messages = vec![msg(Role::User, "forbidden word")];
        match rt.decide_input(&messages, false) {
            InputDecision::Allow { messages: None } => {}
            other => panic!("关闭时必须原样放行: {other:?}"),
        }
        assert!(!rt.input_enabled() && !rt.output_enabled() && !rt.fake_reply_enabled());
    }

    /// 输入命中降级：只重发一次（第二次裁决不会再次返回 Degrade）。
    #[test]
    fn input_hit_degrades_exactly_once() {
        let rt = runtime(degrade_cfg());
        let messages = vec![
            msg(Role::System, "client template says forbidden things"),
            msg(Role::User, "hello"),
        ];
        // 第一次：未降级 → 返回 Degrade + 中性提示词消息
        let degrade_messages = match rt.decide_input(&messages, false) {
            InputDecision::Degrade { messages, reason } => {
                assert!(reason.contains("forbidden"));
                messages
            }
            other => panic!("首次命中应降级: {other:?}"),
        };
        assert_eq!(degrade_messages[0].role, Role::System);
        assert_eq!(
            degrade_messages[0].content_str(),
            DEFAULT_NEUTRAL_PROMPT,
            "system 必须换成中性提示词"
        );
        // 用户消息保留（不改用户内容）
        assert_eq!(degrade_messages[1].content_str(), "hello");

        // 第二次：降级已生效（调用方在两次之间执行 trigger）→ 只换提示词放行，
        // 绝不返回第二个 Degrade —— 这就是「只重发一次」的收敛保证。
        match rt.decide_input(&messages, true) {
            InputDecision::Allow {
                messages: Some(rewritten),
            } => {
                assert_eq!(rewritten[0].content_str(), DEFAULT_NEUTRAL_PROMPT);
            }
            other => panic!("降级期不得再次 Degrade: {other:?}"),
        }
    }

    /// 降级期内 user 内容仍命中 → 拒绝（不允许无限重发）。
    #[test]
    fn input_hit_during_degrade_is_rejected() {
        let rt = runtime(degrade_cfg());
        let messages = vec![msg(Role::User, "forbidden")];
        match rt.decide_input(&messages, true) {
            InputDecision::Reject { reason } => assert!(reason.contains("forbidden")),
            other => panic!("降级期重复命中必须拒绝: {other:?}"),
        }
    }

    #[test]
    fn reject_policy_returns_reject_on_first_hit() {
        let mut cfg = degrade_cfg();
        cfg.action = GuardrailAction::Reject;
        let rt = runtime(cfg);
        let messages = vec![msg(Role::User, "forbidden")];
        assert!(matches!(
            rt.decide_input(&messages, false),
            InputDecision::Reject { .. }
        ));
    }

    #[test]
    fn regex_rule_matches() {
        let cfg = GuardrailSettings {
            enabled: true,
            input_rules: vec![GuardrailRuleSpec::regex(r"ssn:\s*\d{3}-\d{2}-\d{4}")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        let messages = vec![msg(Role::User, "ssn: 123-45-6789")];
        assert!(matches!(
            rt.decide_input(&messages, true),
            InputDecision::Reject { .. }
        ));
    }

    #[test]
    fn invalid_regex_is_skipped_not_panic() {
        let cfg = GuardrailSettings {
            enabled: true,
            input_rules: vec![GuardrailRuleSpec::regex("[unclosed")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        assert!(!rt.input_enabled(), "编译失败的规则应被跳过");
        assert!(matches!(
            rt.decide_input(&[msg(Role::User, "[unclosed")], false),
            InputDecision::Allow { messages: None }
        ));
    }

    #[test]
    fn output_hit_matches_non_stream_text() {
        let cfg = GuardrailSettings {
            enabled: true,
            output_rules: vec![GuardrailRuleSpec::keyword("dangerous")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        assert!(rt.output_hit("here is a dangerous answer").is_some());
        assert!(rt.output_hit("safe answer").is_none());
    }

    fn chunk(text: &str) -> Result<ChatChunk, BridgeError> {
        Ok(ChatChunk {
            id: "c1".into(),
            model: "m1".into(),
            delta: ChatDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                reasoning: None,
            },
            finish_reason: None,
            usage: None,
        })
    }

    /// 输出侧收尾：命中后流以标准 `Err` 结束（下游按错误帧收尾），
    /// 命中之后的 chunk 不再下发。
    #[tokio::test]
    async fn output_stream_ends_with_error_on_hit() {
        let cfg = GuardrailSettings {
            enabled: true,
            output_rules: vec![GuardrailRuleSpec::keyword("dangerous")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        let items = vec![chunk("safe part"), chunk("dangerous"), chunk("tail")];
        let source: ChatChunkStream = Box::pin(futures::stream::iter(items));
        let guarded = guard_output_stream(source, &rt);
        let got: Vec<Result<ChatChunk, BridgeError>> = guarded.collect().await;
        assert_eq!(got.len(), 2, "安全 chunk + 终止错误，tail 不再下发");
        assert!(got[0].is_ok());
        match &got[1] {
            Err(BridgeError::Config(message)) => {
                assert!(
                    message.contains("guardrail"),
                    "错误信息应标识护栏: {message}"
                )
            }
            other => panic!("命中后必须以 Err 收尾: {other:?}"),
        }
    }

    /// 未命中：原流逐字下发，顺序/数量不变。
    #[tokio::test]
    async fn output_stream_passes_verbatim_when_clean() {
        let cfg = GuardrailSettings {
            enabled: true,
            output_rules: vec![GuardrailRuleSpec::keyword("dangerous")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        let items = vec![chunk("a"), chunk("b")];
        let source: ChatChunkStream = Box::pin(futures::stream::iter(items));
        let got: Vec<Result<ChatChunk, BridgeError>> =
            guard_output_stream(source, &rt).collect().await;
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(Result::is_ok));
    }

    /// 输出规则跨 chunk 拼接（"dan" + "gerous"）也能命中。
    #[tokio::test]
    async fn output_stream_matches_across_chunks() {
        let cfg = GuardrailSettings {
            enabled: true,
            output_rules: vec![GuardrailRuleSpec::keyword("dangerous")],
            ..GuardrailSettings::default()
        };
        let rt = runtime(cfg);
        let items = vec![chunk("dan"), chunk("gerous")];
        let source: ChatChunkStream = Box::pin(futures::stream::iter(items));
        let got: Vec<Result<ChatChunk, BridgeError>> =
            guard_output_stream(source, &rt).collect().await;
        assert_eq!(got.len(), 2);
        assert!(got[0].is_ok());
        assert!(got[1].is_err(), "跨 chunk 规则命中必须收尾");
    }

    #[test]
    fn normalize_rejects_invalid_regex() {
        let cfg = GuardrailSettings {
            enabled: true,
            input_rules: vec![GuardrailRuleSpec::regex("[bad")],
            ..GuardrailSettings::default()
        };
        assert!(normalize_settings(cfg).is_err());
    }

    #[test]
    fn normalize_trims_and_drops_empty_patterns() {
        let cfg = GuardrailSettings {
            input_rules: vec![
                GuardrailRuleSpec::keyword("  "),
                GuardrailRuleSpec::keyword(" keep "),
            ],
            ..GuardrailSettings::default()
        };
        let normalized = normalize_settings(cfg).unwrap();
        assert_eq!(normalized.input_rules.len(), 1);
        assert_eq!(normalized.input_rules[0].pattern, "keep");
    }

    #[test]
    fn settings_roundtrip_defaults_are_off() {
        let parsed: GuardrailSettings = serde_json::from_str("{}").unwrap();
        assert!(!parsed.enabled);
        assert_eq!(parsed.action, GuardrailAction::Degrade);
        assert!(parsed.input_rules.is_empty());
        assert!(parsed.output_rules.is_empty());
        assert!(parsed.fake_reply_rules.is_empty());
    }
}
