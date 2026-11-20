//! 日志与审计系统 — 请求日志 + 管理员操作审计。
//!
//! 参照 new-api model/log.go 的字段设计（UserId/ModelName/PromptTokens/CompletionTokens/
//! Quota/UseTime/ChannelId/TokenId/Ip/RequestId）与 model/log.go 的 Type=Consume/Manage 区分。
//!
//! - RequestLog：每次推理请求完成后记录一条（含 model/tokens/cost/latency/status）
//! - AuditLog：管理员写操作（创建/删除/更新用户、渠道、令牌、定价等）记录 before/after
//!
//! 持久化使用 FileStore KV：
//! - 请求日志 key: `reqlog:{created_at}:{id}`
//! - 审计日志 key: `auditlog:{created_at}:{id}`
//!
//! 时间戳前缀便于按时间范围扫描与排序。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

use crate::storage::FileStore;

pub mod daily_agg;
pub mod inflight;
pub mod writer;

// ── RequestLog ─────────────────────────────────────────────────────

/// 请求日志 — 每次推理调用记录一条。
///
/// 参照 new-api model/log.go Log struct。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    /// 唯一 ID（uuid）
    pub id: String,
    /// 用户 ID（可能为空，管理员级令牌）
    #[serde(default)]
    pub user_id: Option<String>,
    /// API Key ID
    #[serde(default)]
    pub key_id: Option<String>,
    /// 渠道 ID（若使用通用渠道）
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 渠道名（admin 排查用，用户不展示）
    #[serde(default)]
    pub channel_name: Option<String>,
    /// 模型名 — 映射后的上游真实名（= 价格表里的名），用户看这个
    #[serde(default)]
    pub model: String,
    /// 用户原始请求名（admin 排查用，用户不展示）。无映射时与 model 相同。
    #[serde(default)]
    pub origin_model: Option<String>,
    /// 输入 token 数
    #[serde(default)]
    pub input_tokens: u64,
    /// 输出 token 数
    #[serde(default)]
    pub output_tokens: u64,
    /// 本次请求费用（配额单位，向用户收的销售价）
    #[serde(default)]
    pub cost: i64,
    /// 渠道成本（配额单位）。未配成本价时 = cost，利润显示 "—"；
    /// 配了成本价时 = 按 cost_pricing 算出的成本，利润 = cost - channel_cost。
    #[serde(default)]
    pub channel_cost: i64,
    /// 延迟（毫秒）
    #[serde(default)]
    pub latency_ms: u64,
    /// HTTP 状态码
    #[serde(default)]
    pub status_code: u16,
    /// 错误信息（失败时填）
    #[serde(default)]
    pub error_msg: Option<String>,
    /// 客户端 IP
    #[serde(default)]
    pub ip: Option<String>,
    /// 识别出的客户端名（规范化，如 "Claude Code"；未识别 = "unknown"）。
    /// 老日志无此字段 → 反序列化为空串（零迁移），展示层显示为「未记录」。
    #[serde(default)]
    pub client_name: String,
    /// 客户端版本（显式声明头或 UA 中能提取则填；否则空串）。
    #[serde(default)]
    pub client_version: String,
    /// 请求 ID（关联客户端请求）
    #[serde(default)]
    pub request_id: Option<String>,
    /// 创建时间（unix timestamp）
    pub created_at: i64,
    /// 调度决策回放（P1-7）：候选渠道列表
    #[serde(default)]
    pub candidate_channels: Vec<String>,
    /// 调度决策回放（P1-7）：被过滤的渠道及原因
    #[serde(default)]
    pub filtered_channels: Vec<FilteredChannel>,
    /// 调度决策回放（P1-7）：最终选中渠道
    #[serde(default)]
    pub selected_channel: Option<String>,
    /// 是否缓存命中（G4：区分 cache 请求与普通请求的记账维度）
    #[serde(default)]
    pub cache_hit: bool,
    /// 是否使用破甲控制台（请求治理实际介入：changed 或 skill_hit）。
    /// 仅管理员可见，非管理员不展示。
    #[serde(default)]
    pub purge_applied: bool,
    /// 请求/响应快照（仅 log_body 开关开启时记录；关闭时 None 且不序列化）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<LogDebugSnapshot>,
    /// 对话快照（**默认记录**，与 `debug` 解耦）
    ///
    /// `debug` 是排障用的原始报文（受 `log_body` 开关控制、截断 4KB）；
    /// 这里存的是**结构化对话**：用户的 messages 与模型回复，前端可直接
    /// 渲染成聊天气泡。管理员点开日志就能看到「这轮对话说了什么」，
    /// 不必先去系统设置打开调试模式。
    ///
    /// 隐私取舍：默认只存最后 N 条消息（见 [`LogConversation::MAX_MESSAGES`]），
    /// 每条截断到 [`LogConversation::MAX_MESSAGE_CHARS`]，把存储放大控制在
    /// 可接受范围；系统提示词只留长度、不存正文（常含站方内部指令）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<LogConversation>,
    /// 首字节延迟（TTFB，毫秒）：从请求发出到上游响应首字节到达。
    /// 流式请求的「用户看到第一个字的等待时间」就是它。未捕获为 None。
    #[serde(default)]
    pub first_response_ms: Option<u64>,
    /// 本次请求的上游尝试次数（含重试；基线 attempts 链）。默认 1。
    #[serde(default = "default_attempts")]
    pub attempts: u32,
    /// 本次请求命中的脱敏规则（对照基线 sensitive_hits）。
    #[serde(default)]
    pub sensitive_hits: Vec<SensitiveHit>,
    /// 本次请求当前所处的转发阶段（对照基线 phase）：
    /// `connecting` / `waiting` / `streaming` / `retrying` / `queued`。
    /// 空串 = 终态（已完成或失败）。
    #[serde(default)]
    pub phase: String,
    /// 路由策略快照（Phase1-B）：仅 `route_replay` 开关开启时记录，关闭时
    /// None 且不序列化（零存储/零序列化开销）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_config: Option<RouteConfigSnapshot>,
    /// 账号级逐账号跳过细因（Phase1-B/#26）：按渠道分组，每组行数有界
    ///（[`ROUTE_REPLAY_MAX_SKIPS`]，超出转 truncated 计数）；仅 `route_replay`
    /// 开关开启时捕获，关闭时为空且不序列化。共享缓冲另有总字节兜底。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_skips: Vec<AccountSkipGroup>,
    /// resolve 层 filtered_channels 的截断计数（#26；超出
    /// [`ROUTE_REPLAY_MAX_FILTERED`] 的部分，不静默丢）。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub route_truncated: usize,
    /// 逐次上游尝试明细（#32「执行时间线」数据源）：仅 `route_replay`
    /// 开关开启时捕获，关闭时为空且不序列化。行数有界
    ///（[`ROUTE_REPLAY_MAX_ATTEMPTS`]，超出转 truncated 计数）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempt_details: Vec<AttemptDetail>,
    /// attempt 明细的截断计数（#32；不静默丢）。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub attempt_details_truncated: usize,
}

/// 默认尝试次数。
fn default_attempts() -> u32 {
    1
}

/// usize 零值序列化跳过判定。
fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// 一条脱敏命中（对照基线 `SensitiveHit`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitiveHit {
    /// 命中的脱敏规则名（如 "phone" / "email" / "id_card"）
    pub word: String,
    /// 命中次数
    pub count: u32,
}

/// 一条对话消息（角色 + 文本）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogMessage {
    /// `system` / `user` / `assistant` / `tool`
    pub role: String,
    /// 文本内容（多模态请求只留 text 段，图片记成 `[image]` 占位）
    pub text: String,
    /// 该消息是否被截断
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// 请求的对话快照（可被前端渲染成聊天气泡）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogConversation {
    /// 按时间顺序的消息列表（含模型回复）
    #[serde(default)]
    pub messages: Vec<LogMessage>,
    /// 系统提示词长度（字符数）；正文不存 —— 常含站方内部指令。
    #[serde(default)]
    pub system_prompt_chars: usize,
    /// 是否因为超出上限丢弃了更早的消息
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated_history: bool,
}

impl LogConversation {
    /// 最多保留的消息条数（约 10 轮）。日志是排障视图，不是完整存档；
    /// 保留太多会让 10 万条日志的存储膨胀数倍。
    pub const MAX_MESSAGES: usize = 20;
    /// 单条消息的最大字符数。
    pub const MAX_MESSAGE_CHARS: usize = 2000;

    /// 从 OpenAI 风格的 `messages` 数组构造快照。
    ///
    /// 兼容三种 content 形态：
    /// - 字符串（最常见）
    /// - 内容块数组（多模态：`[{type:"text",text},{type:"image_url",...}]`）
    /// - 缺少 content 的工具调用消息
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn from_messages(messages: &[serde_json::Value]) -> Self {
        let mut out = Self::default();
        // 先统计 system 提示长度（正文不入库）
        for m in messages {
            if m.get("role").and_then(|r| r.as_str()) == Some("system") {
                out.system_prompt_chars += text_of(m).chars().count();
            }
        }
        // 只保留最后 MAX_MESSAGES 条（排障关心的是最近上下文）
        let start = messages.len().saturating_sub(Self::MAX_MESSAGES);
        out.truncated_history = start > 0;
        for m in &messages[start..] {
            let role = m
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string();
            // 系统提示只记长度，不存正文（见字段注释）
            if role == "system" {
                continue;
            }
            let raw = text_of(m);
            let (text, truncated) = truncate_chars(&raw, Self::MAX_MESSAGE_CHARS);
            out.messages.push(LogMessage {
                role,
                text,
                truncated,
            });
        }
        out
    }

    /// 追加模型回复（流式时传拼接后的完整文本）。
    pub fn push_assistant(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        let (text, truncated) = truncate_chars(text, Self::MAX_MESSAGE_CHARS);
        // 保持上限：超了就丢最早的一条并标记
        if self.messages.len() >= Self::MAX_MESSAGES {
            self.messages.remove(0);
            self.truncated_history = true;
        }
        self.messages.push(LogMessage {
            role: "assistant".to_string(),
            text,
            truncated,
        });
    }

    /// 空快照不落库（省存储）。
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// 从归一化的 [`crate::bridge::ChatMessage`] 构造快照。
    ///
    /// 与 [`Self::from_messages`] 的区别：这里直接吃内部强类型，不必先序列化
    /// 成 JSON 再解析 —— 数据面每条请求都会走一次，省掉一次往返。
    pub fn from_chat_messages(messages: &[crate::bridge::ChatMessage]) -> Self {
        use crate::bridge::Role;
        let mut out = Self::default();
        for m in messages {
            if m.role == Role::System {
                let text = chat_message_text(m);
                out.system_prompt_chars += text.chars().count();
            }
        }
        let start = messages.len().saturating_sub(Self::MAX_MESSAGES);
        out.truncated_history = start > 0;
        for m in &messages[start..] {
            if m.role == Role::System {
                continue; // 系统提示只记长度
            }
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            }
            .to_string();
            let (text, truncated) = truncate_chars(&chat_message_text(m), Self::MAX_MESSAGE_CHARS);
            out.messages.push(LogMessage {
                role,
                text,
                truncated,
            });
        }
        out
    }
}

/// 从强类型消息里抽文本（content 优先，其次 content_blocks 的文本段）。
fn chat_message_text(m: &crate::bridge::ChatMessage) -> String {
    if let Some(c) = m.content.as_deref() {
        if !c.is_empty() {
            return c.to_string();
        }
    }
    if let Some(blocks) = m.content_blocks.as_deref() {
        let mut out = String::new();
        for b in blocks {
            let ty = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match ty {
                "text" | "input_text" => {
                    if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                        out.push_str(t);
                    }
                }
                "image_url" | "image" | "input_image" => out.push_str("[image]"),
                "audio" | "input_audio" => out.push_str("[audio]"),
                _ => {}
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
    // 纯工具调用消息
    if let Some(calls) = m.tool_calls.as_deref() {
        return calls
            .iter()
            .map(|c| {
                format!(
                    "[tool:{}] {}",
                    c.function_name,
                    c.arguments.chars().take(200).collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    String::new()
}

/// 按**字符**（非字节）截断，避免切断 UTF-8 多字节序列。
fn truncate_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        return (s.to_string(), false);
    }
    (s.chars().take(max).collect(), true)
}

/// 从一条 OpenAI 消息里抽取可读文本。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
fn text_of(m: &serde_json::Value) -> String {
    match m.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut out = String::new();
            for p in parts {
                let ty = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match ty {
                    "text" | "input_text" => {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                            out.push_str(t);
                        }
                    }
                    // 多模态图片/音频只留占位，不存 base64（会撑爆存储）
                    "image_url" | "image" | "input_image" => out.push_str("[image]"),
                    "audio" | "input_audio" => out.push_str("[audio]"),
                    _ => {}
                }
            }
            out
        }
        // 工具调用消息：列出工具名，参数只留前 200 字符
        None => match m.get("tool_calls").and_then(|t| t.as_array()) {
            Some(list) => list
                .iter()
                .map(|c| {
                    let name = c
                        .pointer("/function/name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("tool");
                    let args = c
                        .pointer("/function/arguments")
                        .and_then(|a| a.as_str())
                        .unwrap_or("");
                    format!(
                        "[tool:{name}] {}",
                        args.chars().take(200).collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
            None => String::new(),
        },
        _ => String::new(),
    }
}

/// 被过滤的渠道信息（P1-7 调度决策回放）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilteredChannel {
    pub channel_id: String,
    pub reason: String,
}

/// 路由策略快照（Phase1-B 路由回放）：选路那一刻的策略形态。
///
/// 只存回放所需的最小集（策略名 + 粘滞开关），不做全量配置深拷贝——
/// 热路径上这是每请求一次的小结构，字段数有界。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfigSnapshot {
    /// 路由策略（weighted / round_robin / ranked）
    pub strategy: String,
    /// 会话粘滞是否启用
    pub sticky_enabled: bool,
}

/// 账号级跳过明细行（Phase1-B/#26 路由回放）：账号型渠道选号时逐账号的
/// 跳过原因。**不携带 channel_id**——同一次选号里每行都重复同一个值，
/// 由 [`AccountSkipGroup`] 按渠道分组承载。
///
/// 仅在 `route_replay` 开关开启时捕获（`acquire_with_reasons_detail`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSkip {
    pub account_id: String,
    pub account_name: String,
    /// 原因码：`disabled` / `needs_relogin` / `provider_not_allowed` / `expired` / `model_mismatch` /
    /// `capability_mismatch` / `scope_excluded` / `cooling` / `balance_low` /
    /// `concurrency` / `limiter_skip` / `limiter_disable`（与 SkipReasons 档位一一对应）
    pub reason: String,
}

/// 每请求/每渠道保留的明细行上限（#26 内存有界红线）。
///
/// 渠道账号数是设备可控数据（可达数千），无上限时单请求缓冲可到 MB 级。
/// 超出的行**不静默丢**：累进 [`AccountSkipGroup::truncated`]，回放面板显示
/// 「还有 n 条未记录」。同一上限口径也用于 resolve 层 filtered_channels。
pub const ROUTE_REPLAY_MAX_SKIPS: usize = 64;

/// resolve 层 filtered_channels 的同口径上限（#26）。
pub const ROUTE_REPLAY_MAX_FILTERED: usize = 64;

/// 共享缓冲的总字节兜底（所有组合计；#26）。
pub const ROUTE_REPLAY_MAX_BYTES: usize = 128 * 1024;

/// 按渠道分组的账号跳过明细（回放面板的自然分组形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSkipGroup {
    pub channel_id: String,
    pub skipped: Vec<AccountSkip>,
    /// 超出 [`ROUTE_REPLAY_MAX_SKIPS`] 未记录的条数（不静默丢）
    pub truncated: usize,
}

impl AccountSkipGroup {
    /// 把一次选号的捕获并进本组（同渠道多次尝试共用一个组）：
    /// 行数超上限转 truncated 计数。
    pub fn merge(&mut self, group: AccountSkipGroup) {
        let mut rows = group.skipped;
        let mut truncated = group.truncated;
        let room = ROUTE_REPLAY_MAX_SKIPS.saturating_sub(self.skipped.len());
        let take = rows.len().min(room);
        self.skipped.extend(rows.drain(..take));
        truncated += rows.len();
        self.truncated += truncated;
    }

    /// 序列化体积估算（字节兜底用；serde 实际输出略小，估算从宽）。
    fn approx_bytes(&self) -> usize {
        self.skipped
            .iter()
            .map(|r| r.account_id.len() + r.account_name.len() + r.reason.len() + 96)
            .sum::<usize>()
            + 128
    }
}

/// 每请求保留的逐次尝试明细上限（#32，与 #26 的 64 口径一致）。
///
/// 尝试次数由渠道数决定（设备可控），无上限时多渠道 failover 可产生
/// 几十条明细；超出转 `attempt_details_truncated` 计数，不静默丢。
pub const ROUTE_REPLAY_MAX_ATTEMPTS: usize = 64;

/// 逐次上游尝试明细（#32 路由回放「执行时间线」数据源）。
///
/// 字段与上游 `request_stats/record.rs AttemptDetail` 对齐后按 AIGX 现实
/// 裁剪：AIGX 的账号轮换发生在 AccountBridge 内部（一次渠道尝试内多账号），
/// 渠道级明细拿不到单账号维度——`account` 恒空串（上游语义：空=无账号记录）；
/// `retries[]`（渠道内自重试）与 `body_bytes` 同理不可见，不建字段。
/// `notice` 字段位保留（上游有「代理回退直连」提示，AIGX v1 无来源恒 None）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptDetail {
    /// 序号（1 起；与 `attempts` 计数同口径：真实上游调用次数）
    pub index: u32,
    pub channel_id: String,
    pub channel_name: String,
    /// 渠道协议域（channel_type.as_str()；账号型恒 "account"）
    pub provider: String,
    /// 承载账号展示名（AIGX 渠道级明细拿不到，恒空串——见类型注释）
    pub account: String,
    /// 本轮尝试的上游真实模型名（映射后）
    pub model: String,
    /// 亲和粘滞命中（本渠道被置顶）
    pub sticky_hit: bool,
    /// 开始时刻（unix 秒）
    pub started_at: i64,
    /// 结束时刻（unix 秒；None = 未定论——流式成功轮的终态在计费守卫，
    /// 这里只记建流结论，流式成功轮 finished_at=建流成功时刻）
    pub finished_at: Option<i64>,
    /// HTTP 状态码（None = 传输层失败/未定论）
    pub status: Option<u16>,
    /// 失败摘要（成功为 None；截断 300 字符）
    pub error: Option<String>,
    /// 提示（v1 恒 None，契约位）
    pub notice: Option<String>,
}

/// 共享缓冲的总字节兜底（#26：防单条日志记录失控）。
///
/// 超限时**丢弃最旧的组**并把它的 truncated 并进最新的组（计数不丢），
/// 保证总在界内。落库与内存走同一兜底。
pub fn cap_account_skip_groups(groups: &mut Vec<AccountSkipGroup>) {
    let mut total: usize = groups.iter().map(AccountSkipGroup::approx_bytes).sum();
    while total > ROUTE_REPLAY_MAX_BYTES && groups.len() > 1 {
        let oldest = groups.remove(0);
        total = total.saturating_sub(oldest.approx_bytes());
        if let Some(newest) = groups.last_mut() {
            newest.truncated += oldest.truncated + oldest.skipped.len();
        }
    }
}

/// 选号一次（acquire 一调用）的明细捕获槽：store 侧 push，bridge 侧取出
/// 并进共享缓冲。cap 在 push 时就地生效。
#[derive(Debug, Default)]
pub struct AccountSkipCapture {
    pub channel_id: String,
    pub rows: Vec<AccountSkip>,
    pub truncated: usize,
}

impl AccountSkipCapture {
    pub fn new(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            ..Default::default()
        }
    }

    /// 记一行跳过明细；超 [`ROUTE_REPLAY_MAX_SKIPS`] 转 truncated 计数（不静默丢）。
    ///
    /// `pub(crate)`：`account_session::store::acquire_with_reasons_detail` 是
    /// 唯一调用方（选号时逐账号 push）。
    pub(crate) fn push(&mut self, row: AccountSkip) {
        if self.rows.len() < ROUTE_REPLAY_MAX_SKIPS {
            self.rows.push(row);
        } else {
            self.truncated += 1;
        }
    }

    /// 收口成按渠道分组的明细；`bridge::account` 取值后并进请求级共享缓冲。
    pub(crate) fn finish(self) -> AccountSkipGroup {
        AccountSkipGroup {
            channel_id: self.channel_id,
            skipped: self.rows,
            truncated: self.truncated,
        }
    }
}

/// 请求/响应快照（排障用，仅 `log_body` 开关开启时记录）。
///
/// 对齐 new-api 无落库、仅 stdout DEBUG 的缺陷：AIGX 把快照直接挂在
/// 请求日志上，点开日志详情即可回放那次对话。开关关闭时字段为 None
/// 且 `skip_serializing_if` 使其不出现在 JSON 中，零存储/零序列化开销。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogDebugSnapshot {
    /// 客户端请求体（截断至 `MAX_SNAPSHOT_BYTES`）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_body: Option<String>,
    /// 上游响应（非流式 = 响应 JSON；流式 = 拼接后文本；失败 = 上游原始错误体）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
    /// 实际请求的上游端点 URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_url: Option<String>,
}

impl LogDebugSnapshot {
    /// 单字段截断上限（字节）。按字符边界截断，避免切断 UTF-8 多字节序列。
    pub const MAX_SNAPSHOT_BYTES: usize = 4096;

    /// 截断到上限（字符安全）
    fn truncate(s: impl Into<String>) -> String {
        let s: String = s.into();
        if s.len() <= Self::MAX_SNAPSHOT_BYTES {
            return s;
        }
        let mut end = Self::MAX_SNAPSHOT_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = s[..end].to_string();
        out.push_str("…[truncated]");
        out
    }

    /// 构造快照（自动截断；空字符串视为 None 不记录）
    pub fn new(
        request_body: Option<String>,
        response_body: Option<String>,
        upstream_url: Option<String>,
    ) -> Option<Self> {
        let req = request_body.filter(|s| !s.is_empty()).map(Self::truncate);
        let resp = response_body.filter(|s| !s.is_empty()).map(Self::truncate);
        let url = upstream_url.filter(|s| !s.is_empty());
        if req.is_none() && resp.is_none() && url.is_none() {
            return None;
        }
        Some(Self {
            request_body: req,
            response_body: resp,
            upstream_url: url,
        })
    }
}

impl Default for RequestLog {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestLog {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: None,
            key_id: None,
            channel_id: None,
            channel_name: None,
            model: String::new(),
            origin_model: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0,
            channel_cost: 0,
            latency_ms: 0,
            status_code: 200,
            error_msg: None,
            ip: None,
            client_name: String::new(),
            client_version: String::new(),
            request_id: None,
            created_at: chrono::Utc::now().timestamp(),
            candidate_channels: Vec::new(),
            filtered_channels: Vec::new(),
            selected_channel: None,
            cache_hit: false,
            purge_applied: false,
            debug: None,
            conversation: None,
            first_response_ms: None,
            attempts: 1,
            sensitive_hits: Vec::new(),
            phase: String::new(),
            route_config: None,
            account_skips: Vec::new(),
            route_truncated: 0,
            attempt_details: Vec::new(),
            attempt_details_truncated: 0,
        }
    }
}

// ── AuditLog ───────────────────────────────────────────────────────

/// 管理员操作审计日志。
///
/// 参照 new-api LogTypeManage：记录 admin 对系统资源的写操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: String,
    /// 管理员用户 ID（或 "admin" 兼容旧模式）
    #[serde(default)]
    pub admin_id: String,
    /// 操作类型：create / update / delete / login / logout / etc.
    #[serde(default)]
    pub action: String,
    /// 目标资源（如 "user:{id}"、"channel:{id}"）
    #[serde(default)]
    pub target: String,
    /// 操作前状态（JSON 字符串，可选）
    #[serde(default)]
    pub before: Option<String>,
    /// 操作后状态（JSON 字符串，可选）
    #[serde(default)]
    pub after: Option<String>,
    /// 客户端 IP（可选；经反代时需 `trust_proxy_headers=true` 才可信）。
    ///
    /// 历史记录此字段恒空（`record_audit` 无 IP 入参）——P0 安全加固补上
    /// 带 IP 的 [`LogStore::record_audit_with_ip`] 入口；旧数据 serde
    /// default 兼容（None）。
    #[serde(default)]
    pub ip: Option<String>,
    /// 创建时间
    pub created_at: i64,
}

impl AuditLog {
    pub fn new(
        admin_id: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            admin_id: admin_id.into(),
            action: action.into(),
            target: target.into(),
            before: None,
            after: None,
            ip: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}

// ── Store ──────────────────────────────────────────────────────────

/// 请求日志存储。
///
/// key 格式：`reqlog:{created_at}:{id}`，时间戳前缀便于按时间范围扫描。
///
/// B13：设置容量上限防止无界增长撑爆磁盘——写入路径每
/// `PURGE_CHECK_EVERY` 次触发一次容量检查，超限时删除最旧一批。
pub struct RequestLogStore {
    store: Arc<FileStore>,
    /// 写入计数：摊薄容量检查频率（每 PURGE_CHECK_EVERY 次写入触发一次）
    writes: std::sync::atomic::AtomicU64,
}

#[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
impl RequestLogStore {
    /// 用户索引完备性标记：存在 = 历史日志已全部补建 `reqlogu:` 索引。
    ///
    /// 缺它时 `list_by_user` 一律走全扫（与改造前等价）；`backfill_user_index`
    /// 成功后写入。没有这道闸门，混合态（新日志有索引、历史没有）下按
    /// 区间读会静默漏数据。
    const USER_INDEX_MARKER: &'static str = "reqlogu:backfill_done";
    /// B13：请求日志容量上限
    const MAX_LOGS: usize = 100_000;
    /// B13：超限时每批清理的最旧日志条数
    const PURGE_BATCH: usize = 10_000;
    /// B13：每隔多少次写入触发一次容量检查
    const PURGE_CHECK_EVERY: u64 = 1000;
    /// HTTP 层分页单页上限：防止 size=usize::MAX 造成 LIMIT 负数
    ///（SQLite 负 LIMIT = 无限制 → 全表物化 OOM）或 (page-1)*size 溢出。
    pub const PAGE_SIZE_MAX: usize = 100;

    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            writes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn key_of(log: &RequestLog) -> String {
        format!("reqlog:{}:{}", log.created_at, log.id)
    }

    /// 二级索引键：`request_id`（以及日志自带的 `id`）→ 主键。
    ///
    /// `reqlog:{ts}:{id}` 的两个部分都是写入时才定的，光凭 `request_id`
    /// 找不到主键，历史上只能 `list_all()` 全键 + 逐条反序列化去比对 ——
    /// 日志保留量上万后单次详情查询就是秒级。写入时顺手记一条索引，
    /// `get_by_request_id` 就能 O(1) 点查。
    ///
    /// 键里对 id 做百分号转义：`request_id` 可能含 `/`（Anthropic 的
    /// `req_...` 之外还有客户端自带格式），直接拼进键会破坏前缀语义。
    pub(crate) fn index_key(id: &str, value: &str) -> String {
        format!("reqlog_id:{id}={value}")
    }

    pub(crate) fn escape_index_component(value: &str) -> String {
        value.replace('%', "%25").replace('=', "%3D")
    }

    /// 一条日志要写的二级索引条目（`id` 与 `request_id` 各一条，去重），
    /// 外加**用户索引**（有 `user_id` 时一条）。
    ///
    /// **两条写入路径必须共用本函数**：管理面同步 [`Self::add`]（逐条
    /// `put`）与数据面批量 `writer::apply_ops`（并入同一次 `write_batch`
    /// 事务）。历史缺口：批量路径最初只写 `reqlog:` 主键、没写索引，导致
    /// 真实流量的 `get_by_request_id` 永远落到全扫回退 —— 读路径按索引
    /// 写了、写路径没按索引写，功能「半死」。
    pub(crate) fn index_entries(log: &RequestLog, primary: &str) -> Vec<(String, Value)> {
        let mut written: Vec<&str> = Vec::new();
        let mut out: Vec<(String, Value)> = Vec::new();
        for candidate in std::iter::once(log.id.as_str())
            .chain(log.request_id.as_deref())
            .filter(|value| !value.is_empty())
        {
            if written.contains(&candidate) {
                continue;
            }
            written.push(candidate);
            out.push((
                Self::index_key(&Self::escape_index_component(candidate), primary),
                Value::Bool(true),
            ));
        }
        if let Some(uid) = log.user_id.as_deref().filter(|u| !u.is_empty()) {
            out.push((
                Self::user_index_key(uid, log.created_at, log.id.as_str()),
                Value::Bool(true),
            ));
        }
        out
    }

    /// 用户索引键：`reqlogu:{uid}:{created_at:010}:{id}`。
    ///
    /// 时间戳零填充到 10 位：今天所有 `created_at` 本就 10 位（与主键
    /// `reqlog:{ts}:{id}` 的字符串完全一致），这次补零是 no-op；2286 年后
    /// 仍保持「键字典序 = 时间序」，区间读才不会错位。
    ///
    /// **主键可由本键直接推导**（去掉 `reqlogu:{uid}:` 前缀后前补
    /// `reqlog:`），所以索引值只存 `true` —— 用户流水读 N 条只花一次区间
    /// 扫描，不必再为每条索引做一次点查。
    pub(crate) fn user_index_key(uid: &str, created_at: i64, id: &str) -> String {
        format!(
            "reqlogu:{}:{created_at:010}:{id}",
            Self::escape_index_component(uid)
        )
    }

    /// 写入某条日志的索引（`id` 与 `request_id` 各一条，重复的不重复写）。
    fn write_indexes(&self, log: &RequestLog, primary: &str) -> anyhow::Result<()> {
        for (key, value) in Self::index_entries(log, primary) {
            self.store.put(&key, &value)?;
        }
        Ok(())
    }

    /// 删日志时连带删它的索引。
    ///
    /// 删除路径手里通常只有主键（保留期裁剪、容量裁剪、清空），所以这里先
    /// 读出整条日志拿 `request_id` —— 只多一次点查，但能避免留下指向已删
    /// 主键的悬空索引（`get_by_request_id` 命中它只会白跑一次 get）。
    fn delete_indexes_by_primary(&self, primary: &str) {
        let mut candidates: Vec<String> = Vec::new();
        let mut user_key: Option<String> = None;
        if let Ok(Some(log)) = self.store.get::<RequestLog>(primary) {
            if !log.id.is_empty() {
                candidates.push(log.id.clone());
            }
            if let Some(request_id) = log.request_id.clone().filter(|value| !value.is_empty()) {
                candidates.push(request_id);
            }
            if let Some(uid) = log.user_id.as_deref().filter(|u| !u.is_empty()) {
                user_key = Some(Self::user_index_key(uid, log.created_at, log.id.as_str()));
            }
        }
        if candidates.is_empty() {
            // 读不到（已损坏/已删）：至少按主键尾段兜底
            if let Some(id) = primary.rsplit(':').next().filter(|id| !id.is_empty()) {
                candidates.push(id.to_string());
            }
        }
        candidates.dedup();
        for candidate in candidates {
            let key = Self::index_key(&Self::escape_index_component(&candidate), primary);
            if let Err(e) = self.store.delete(&key) {
                tracing::warn!("request log index delete failed for {key}: {e}");
            }
        }
        // 用户索引从主键推导不出来（主键里没有 uid）；只有在读得到整条日志
        // 时才清得掉。读不到时这条索引会残留到下次全量重建（可接受：它只
        // 影响该用户的流水多一条悬空键，`get` 落空后自然被跳过）。
        if let Some(key) = user_key {
            if let Err(e) = self.store.delete(&key) {
                tracing::warn!("request log user index delete failed for {key}: {e}");
            }
        }
    }

    /// 追加一条请求日志
    pub fn add(&self, mut log: RequestLog) -> anyhow::Result<RequestLog> {
        if log.id.is_empty() {
            log.id = uuid::Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = chrono::Utc::now().timestamp();
        }
        // B13：定期检查容量，超限清理最旧一批（检查失败不影响本次写入）
        let writes = self
            .writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if writes.is_multiple_of(Self::PURGE_CHECK_EVERY) {
            if let Err(e) = self.purge_overflow() {
                tracing::warn!("request log purge check failed: {e}");
            }
        }
        let key = Self::key_of(&log);
        self.store.put(&key, &log)?;
        self.write_indexes(&log, &key)?;
        Ok(log)
    }

    /// B13：容量超限时删除最旧一批日志。
    ///
    /// 实现见自由函数 [`purge_request_overflow`]（日志写入旁路的批量
    /// 消费者同样要跑这套容量兜底，抽出来两处共用）。
    fn purge_overflow(&self) -> anyhow::Result<()> {
        purge_request_overflow(&self.store)
    }

    /// 日志保留配置（运维设置持久化；None = 不按天数清理，只受容量上限约束）
    ///
    /// key 格式 `reqlog:{created_at}:{id}` 时间戳前缀 + `auditlog:{ts}:{id}`，
    /// 按天清理直接对前缀范围扫描即可，无需遍历解析值。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn set_retention(
        &self,
        max_age_days: Option<u64>,
        max_capacity: Option<usize>,
    ) -> anyhow::Result<()> {
        self.set_retention_full(max_age_days, max_capacity, None, None)
    }

    /// 写入完整保留配置（请求日志天数/容量 + 审计日志独立天数 + 日聚合天数）。
    pub fn set_retention_full(
        &self,
        max_age_days: Option<u64>,
        max_capacity: Option<usize>,
        audit_max_age_days: Option<u64>,
        daily_max_age_days: Option<u64>,
    ) -> anyhow::Result<()> {
        #[derive(Serialize, Deserialize, Default)]
        struct Retention {
            max_age_days: Option<u64>,
            max_capacity: Option<usize>,
            /// 审计日志独立保留天数；None = 跟随请求日志天数
            #[serde(default)]
            audit_max_age_days: Option<u64>,
            /// 日聚合保留天数（含今天 N 个自然日）；None = 永久——聚合行
            /// 一天几十 KB，量级每年顶天几十 MB，默认不清。聚合寿命独立于
            /// 明细：明细被裁掉后聚合仍是报表的完整历史。
            #[serde(default)]
            daily_max_age_days: Option<u64>,
        }
        self.store.put(
            "log_retention",
            &Retention {
                max_age_days,
                max_capacity,
                audit_max_age_days,
                daily_max_age_days,
            },
        )
    }

    /// 读取日志保留配置（无记录返回 None/None = 默认策略：不限天数、容量 10 万）
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn retention(&self) -> (Option<u64>, Option<usize>) {
        let (days, capacity, _audit, _daily) = self.retention_full();
        (days, capacity)
    }

    /// 读取完整保留配置（请求天数/容量 + 审计独立天数 + 日聚合天数）。
    pub fn retention_full(&self) -> (Option<u64>, Option<usize>, Option<u64>, Option<u64>) {
        #[derive(Serialize, Deserialize, Default)]
        struct Retention {
            max_age_days: Option<u64>,
            max_capacity: Option<usize>,
            #[serde(default)]
            audit_max_age_days: Option<u64>,
            #[serde(default)]
            daily_max_age_days: Option<u64>,
        }
        match self.store.get::<Retention>("log_retention") {
            Ok(Some(r)) => (
                r.max_age_days,
                r.max_capacity,
                r.audit_max_age_days,
                r.daily_max_age_days,
            ),
            _ => (None, None, None, None),
        }
    }

    /// 手动/定时清理：删除早于 `before_ts` 的请求日志，返回删除条数（分批限流防阻塞）。
    ///
    /// 与 new-api DeleteOldLogBatch 对齐的语义，但 AIGX 是 KV 存储无需事务，
    /// 直接按键前缀收集 + 分批删除（每批 5000 条，逐条 delete 是 O(1) 点删）。
    pub fn delete_older_than(&self, before_ts: i64) -> anyhow::Result<usize> {
        let keys = self.store.list("reqlog:")?;
        let mut doomed: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                k.strip_prefix("reqlog:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|ts| ts < before_ts)
                    .unwrap_or(false)
            })
            .collect();
        let n = doomed.len();
        const BATCH: usize = 5000;
        for chunk in std::mem::take(&mut doomed).chunks(BATCH) {
            for k in chunk {
                self.delete_indexes_by_primary(k);
                if let Err(e) = self.store.delete(k) {
                    tracing::warn!("retention delete failed for {k}: {e}");
                }
            }
        }
        Ok(n)
    }

    /// 列出全部请求日志（按时间倒序）
    pub fn list_all(&self) -> Vec<RequestLog> {
        let keys = match self.store.list("reqlog:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut logs: Vec<RequestLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<RequestLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        logs
    }

    /// 某用户的全部请求日志（按时间倒序），可限定时间窗。
    ///
    /// 给用户侧积分流水用：与签到奖励合并成时间线后分页，不能走
    /// `list_with_filter`（它按页截断，会漏掉第 N+1 页之后的内容）。
    ///
    /// **优先走用户索引**（`reqlogu:{uid}:{ts}:{id}`）：时间窗直接转成键
    /// 区间，只扫该用户、只扫窗口内的键；主键由索引键去掉前缀、前补
    /// `reqlog:` 直接推导，无需额外点查。索引缺失（历史数据、索引被手工
    /// 清过）时回退旧的全扫，行为不变、只是慢。
    ///
    /// **只有回填完成之后才能走索引快路径**：索引是本次才补建的，库里
    /// 可能同时存在「有索引的新日志」与「无索引的历史日志」。若按「区间
    /// 命中就返回」实现，混合态下会静默漏掉历史行 —— 这类静默少数据比
    /// 慢更糟。故用 `USER_INDEX_MARKER` 做完备性闸门：未回填 = 一律全扫
    /// （与改造前逐字等价）；回填完成后才允许键区间读。
    pub fn list_by_user(
        &self,
        user_id: &str,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Vec<RequestLog> {
        if self.user_index_ready() {
            let uid = Self::escape_index_component(user_id);
            let from = format!("reqlogu:{uid}:{:010}", start.unwrap_or(i64::MIN));
            let to = match end {
                // `to` 设为 end+1 的 10 位表示（含两端）；10 位溢出到 11 位时
                // 字典序仍大于所有 10 位键，仍是正确上界。
                Some(e) => format!("reqlogu:{uid}:{:010}", e.saturating_add(1)),
                None => format!("reqlogu:{uid};"),
            };
            if let Ok(keys) = self.store.list_range("reqlogu:", &from, &to) {
                let prefix = format!("reqlogu:{uid}:");
                let mut logs: Vec<RequestLog> = keys
                    .iter()
                    .filter_map(|k| k.strip_prefix(&prefix))
                    .filter_map(|rest| {
                        self.store
                            .get::<RequestLog>(&format!("reqlog:{rest}"))
                            .ok()
                            .flatten()
                    })
                    .collect();
                logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
                return logs;
            }
        }
        // 回退：索引未就绪（历史数据未建索引）/ 该用户确实无日志
        self.list_all()
            .into_iter()
            .filter(|l| {
                l.user_id.as_deref() == Some(user_id)
                    && start.is_none_or(|s| l.created_at >= s)
                    && end.is_none_or(|e| l.created_at <= e)
            })
            .collect()
    }

    /// 用户索引是否已覆盖全部历史日志（回填 marker 存在）。
    fn user_index_ready(&self) -> bool {
        matches!(
            self.store.get::<bool>(Self::USER_INDEX_MARKER),
            Ok(Some(true))
        )
    }

    /// 一次性回填用户索引：扫描全部 reqlog 明细，按
    /// [`Self::user_index_key`] 补写缺失的索引键，成功后落 marker。
    ///
    /// 幂等：重复调用只会重复 `put` 同样的 `true`。返回补写条数。
    /// 与日聚合回填（`backfill_history`）分开：那个只管聚合、会吞掉
    /// 用户索引语义；这里只补键，不改任何聚合值。
    pub fn backfill_user_index(&self) -> anyhow::Result<usize> {
        if self.user_index_ready() {
            return Ok(0);
        }
        let keys = self.store.list("reqlog:")?;
        let mut written = 0usize;
        let mut batch: Vec<(String, Value)> = Vec::with_capacity(512);
        for k in keys {
            let Ok(Some(log)) = self.store.get::<RequestLog>(&k) else {
                continue;
            };
            let Some(uid) = log.user_id.as_deref().filter(|u| !u.is_empty()) else {
                continue;
            };
            let idx = Self::user_index_key(uid, log.created_at, log.id.as_str());
            if matches!(self.store.get::<bool>(&idx), Ok(Some(true))) {
                continue;
            }
            batch.push((idx, Value::Bool(true)));
            written += 1;
            if batch.len() >= 512 {
                let refs: Vec<(&str, Value)> =
                    batch.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                self.store.write_batch(&refs)?;
                batch.clear();
            }
        }
        if !batch.is_empty() {
            let refs: Vec<(&str, Value)> =
                batch.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            self.store.write_batch(&refs)?;
        }
        self.store.put(Self::USER_INDEX_MARKER, &true)?;
        Ok(written)
    }

    /// Phase1-B 路由回放：按 request_id（或日志 id）取单条请求日志。
    ///
    /// 按 `request_id`（或日志自带 `id`）查单条请求日志。
    ///
    /// **走二级索引**：写入时 `write_indexes` 记了 `reqlog_id:{id}={主键}`，
    /// 这里先索引点查，命中直接按主键取整条 —— O(1)，不再全键扫描 + 逐条
    /// 反序列化比对。索引缺失（历史数据、索引被手工清过）时回退旧的全扫，
    /// 行为不变、只是慢。
    pub fn get_by_request_id(&self, request_id: &str) -> Option<RequestLog> {
        let index_key = Self::index_key(&Self::escape_index_component(request_id), "");
        // 索引键前缀是 `reqlog_id:{id}=`，主键跟在 `=` 之后；用前缀列键取最新一条
        // （同一 request_id 理论上唯一，重复时取最新与旧全扫 `find` 的语义一致）。
        if let Ok(keys) = self
            .store
            .list_latest_keys(&index_key, 1)
            .or_else(|_| self.store.list(&index_key))
        {
            if let Some(key) = keys.first() {
                let primary = key.split_once('=').map(|(_, rest)| rest).unwrap_or("");
                if !primary.is_empty() {
                    if let Ok(Some(log)) = self.store.get::<RequestLog>(primary) {
                        return Some(log);
                    }
                }
            }
        }
        // 回退：索引没命中（历史数据未建索引 / request_id 只存在于旧记录）
        self.list_all()
            .into_iter()
            .find(|l| l.id == request_id || l.request_id.as_deref() == Some(request_id))
    }

    /// 键区间读：`created_at ∈ [from_ts, to_ts]`（含两端）。
    ///
    /// `reqlog:{ts}:{id}` 时间戳定宽 10 位 → 键字典序等价数值序（sqlite.rs
    /// 同论证），`list_range` 直接吃主键索引范围扫，把「最近 N 分钟/小时」    /// 类窗口读从全表降为近窗。超过 `max_rows` 截断并返回 `sampled=true`
    /// （护栏：窗口内日志暴涨时报表宁可标注抽样也不扫穿）。
    pub fn list_window(
        &self,
        from_ts: i64,
        to_ts: i64,
        max_rows: usize,
    ) -> anyhow::Result<(Vec<RequestLog>, bool)> {
        if from_ts > to_ts {
            return Ok((Vec::new(), false));
        }
        let from = format!("reqlog:{from_ts}");
        let to = format!("reqlog:{}", to_ts.saturating_add(1));
        let keys = self.store.list_range("reqlog:", &from, &to)?;
        let total = keys.len();
        let mut logs = Vec::with_capacity(total.min(max_rows));
        for k in keys.into_iter().take(max_rows) {
            if let Some(l) = self.store.get::<RequestLog>(&k)? {
                logs.push(l);
            }
        }
        Ok((logs, total > max_rows))
    }

    /// 全部 reqlog 键按键升序（时间戳定宽 10 位 → 键序=时间序）。
    /// 回填/重建的流式读入口：只拿键不反序列化，配合 `logs_by_keys`
    /// 分批读，避免历史百万级明细一次性驻留内存。
    pub fn all_keys_asc(&self) -> Vec<String> {
        let mut keys = self.store.list("reqlog:").unwrap_or_default();
        keys.sort();
        keys
    }

    /// 时间窗 `[from_ts, to_ts]` 内**最新**的至多 `limit` 条键（倒序）。
    ///
    /// 与 [`Self::list_window`] 的差别：那个升序取前 N（超限丢最新），这个
    /// 倒序取最新 N（超限丢最旧），且**键区间 + LIMIT 在存储层完成**，
    /// 不再先枚举全前缀键。窗口类聚合（routing flow/trace/workspace）用它。
    pub fn list_range_desc(
        &self,
        from_ts: i64,
        to_ts: i64,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        if from_ts > to_ts {
            return Ok(Vec::new());
        }
        let from = format!("reqlog:{from_ts}");
        let to = format!("reqlog:{}", to_ts.saturating_add(1));
        self.store.list_range_desc(&from, &to, limit)
    }

    /// 时间窗 `[from_ts, to_ts]` 内的日志条数（`COUNT(*)`，不物化键）。
    pub fn count_window(&self, from_ts: i64, to_ts: i64) -> anyhow::Result<usize> {
        if from_ts > to_ts {
            return Ok(0);
        }
        let from = format!("reqlog:{from_ts}");
        let to = format!("reqlog:{}", to_ts.saturating_add(1));
        self.store.count_range(&from, &to)
    }

    /// 全部 reqlog 条数（`COUNT(*)`，不物化键）。
    pub fn count_all(&self) -> usize {
        self.store.count_prefix("reqlog:").unwrap_or(0)
    }

    /// 窗口内条数与首尾时间戳（`created_at ∈ [from_ts, to_ts]`，含两端）。
    ///
    /// 「清空预览」要的是「会删多少条、最早/最新是哪天」——原先为此
    /// `all_sorted_asc()` 全键 + 全量反序列化。这里只碰键：`COUNT(*)` 取
    /// 条数，`list_range_asc/desc(limit 1)` 各取首尾一个键再解析时间戳
    /// （键前缀定宽 10 位 → 键序 = 时间序）。返回
    /// `(total_all, window_count, oldest, newest)`；窗口为空时后两项为 None。
    pub fn window_bounds(
        &self,
        from_ts: Option<i64>,
        to_ts: Option<i64>,
    ) -> anyhow::Result<(usize, usize, Option<i64>, Option<i64>)> {
        let total_all = self.store.count_prefix("reqlog:")?;
        let lower = match from_ts {
            Some(t) => format!("reqlog:{t}"),
            None => "reqlog:".to_string(),
        };
        let upper = match to_ts {
            Some(t) => format!("reqlog:{}", t.saturating_add(1)),
            None => format!("reqlog:{}", '\u{10FFFF}'),
        };
        let window_count = self.store.count_range(&lower, &upper)?;
        if window_count == 0 {
            return Ok((total_all, 0, None, None));
        }
        let ts_of = |k: &str| -> Option<i64> {
            k.strip_prefix("reqlog:")
                .and_then(|rest| rest.split(':').next())
                .and_then(|s| s.parse::<i64>().ok())
        };
        let first = self.store.list_range_asc("reqlog:", &lower, &upper, 1)?;
        let last = self.store.list_range_desc(&lower, &upper, 1)?;
        Ok((
            total_all,
            window_count,
            first.first().and_then(|k| ts_of(k)),
            last.first().and_then(|k| ts_of(k)),
        ))
    }

    /// 按 key 批量反序列化请求日志（缺失/损坏静默跳过）。
    pub fn logs_by_keys(&self, keys: &[String]) -> Vec<RequestLog> {
        keys.iter()
            .filter_map(|k| self.store.get::<RequestLog>(k).ok().flatten())
            .collect()
    }

    /// 按 ID 删除单条请求日志（best-effort，不存在视为成功）
    pub fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let keys = self.store.list("reqlog:")?;
        let target = keys.into_iter().find(|k| k.ends_with(&format!(":{id}")));
        if let Some(k) = target {
            self.delete_indexes_by_primary(&k);
            self.store.delete(&k)?;
        }
        Ok(())
    }

    /// 批量删除请求日志（按 ID 列表），返回成功删除的条数
    pub fn delete_many(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.delete_by_id(id).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 清空全部请求日志（连同它们的二级索引），返回删除条数
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("reqlog:")?;
        let n = keys.len();
        for k in &keys {
            self.delete_indexes_by_primary(k);
            if let Err(e) = self.store.delete(k) {
                tracing::warn!("clear request log failed for {k}: {e}");
            }
        }
        Ok(n)
    }

    /// 按条件过滤并分页。
    ///
    /// 参数：
    /// - user_id：按用户 ID 过滤
    /// - model：按模型过滤
    /// - channel_id：按渠道过滤
    /// - start/end：时间范围（unix timestamp，闭区间）
    /// - page/size：分页（1-based）
    #[allow(clippy::too_many_arguments)]
    pub fn list_with_filter(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        page: usize,
        size: usize,
    ) -> (Vec<RequestLog>, usize) {
        // P0 性能：无筛选时用 list_latest_keys 只取最新一页所需的键（索引范围
        // 扫描 + LIMIT），避免全表列键+全量反序列化。10 万条日志时从 O(全表)
        // 降到 O(page*size)。键的时间戳前缀保证字典序=时间序，倒序即最新优先。
        // total 用 COUNT 同样走索引，不再物化全部记录。
        let page = page.clamp(1, 10_000);
        let size = size.clamp(1, Self::PAGE_SIZE_MAX);
        // start/end 也是筛选：仅传时间范围时不能走快路径（会忽略时间窗）
        let has_filter = user_id.is_some()
            || model.is_some()
            || channel_id.is_some()
            || start.is_some()
            || end.is_some();

        if !has_filter {
            let total = self.count_all();
            // 取「前 page*size 条最新键」后在内存中切出第 page 页的 size 条，
            // 与原全量切片语义一致（页码越界返回空）。
            let limit = page.saturating_mul(size);
            let keys = self
                .store
                .list_latest_keys("reqlog:", limit)
                .unwrap_or_default();
            let mut logs: Vec<RequestLog> = keys
                .into_iter()
                .filter_map(|k| self.store.get::<RequestLog>(&k).ok().flatten())
                .collect();
            // 键倒序（最新优先）；值的 created_at 与键一致，按值稳定排序一次
            logs.sort_by_key(|l| std::cmp::Reverse(l.created_at));
            let start_idx = (page - 1) * size;
            let paged: Vec<RequestLog> = if start_idx >= logs.len() {
                Vec::new()
            } else {
                let end_idx = (start_idx + size).min(logs.len());
                logs[start_idx..end_idx].to_vec()
            };
            return (paged, total);
        }

        // 带筛选：退回全量扫描（筛选条件在值内，无索引可用），但只在
        // 时间窗内扫描（有 start/end 时先按键范围裁剪键集）。
        let all = self.list_all();
        let filtered: Vec<RequestLog> = all
            .into_iter()
            .filter(|l| Self::matches(l, user_id, model, channel_id, start, end))
            .collect();
        let total = filtered.len();
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            filtered[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }

    /// 带状态/关键词的扩展筛选（在基础筛选之上再收窄）。
    ///
    /// - `status`：`ok` = 2xx 成功；`error` = 非 2xx 或带 `error_msg`
    /// - `keyword`：大小写不敏感，匹配 model / channel_name / error_msg / user_id
    ///
    /// 不要扩展 `list_with_filter` 的签名：那是热路径（无筛选时走索引快路径），
    /// 多出的条件会让所有调用点都被迫传 None。扩展筛选只在真正用到时退化全扫。
    #[allow(clippy::too_many_arguments)]
    pub fn list_with_filter_ext(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        status: Option<&str>,
        keyword: Option<&str>,
        page: usize,
        size: usize,
    ) -> (Vec<RequestLog>, usize) {
        let kw = keyword
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty());
        // 无扩展条件时直接走基础路径（保留索引快路径）
        if status.is_none() && kw.is_none() {
            return self.list_with_filter(user_id, model, channel_id, start, end, page, size);
        }
        let page = page.clamp(1, 10_000);
        let size = size.clamp(1, Self::PAGE_SIZE_MAX);
        let filtered: Vec<RequestLog> = self
            .list_all()
            .into_iter()
            .filter(|l| Self::matches(l, user_id, model, channel_id, start, end))
            .filter(|l| match status {
                Some("ok") => (200..300).contains(&l.status_code),
                Some("error") => !(200..300).contains(&l.status_code) || l.error_msg.is_some(),
                _ => true,
            })
            .filter(|l| match &kw {
                None => true,
                Some(k) => {
                    l.model.to_lowercase().contains(k)
                        || l.channel_name
                            .as_deref()
                            .map(|c| c.to_lowercase().contains(k))
                            .unwrap_or(false)
                        || l.error_msg
                            .as_deref()
                            .map(|e| e.to_lowercase().contains(k))
                            .unwrap_or(false)
                        || l.user_id
                            .as_deref()
                            .map(|u| u.to_lowercase().contains(k))
                            .unwrap_or(false)
                }
            })
            .collect();
        let total = filtered.len();
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            filtered[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }

    /// 导出全部（或按过滤条件）为 JSON 字符串（管理员视角，含全部字段）
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn export_json(&self) -> String {
        self.export_json_filtered(None, None, None, None, None, true)
    }

    /// 导出按 user_id 过滤后的 JSON（普通用户导出自己的记录，剥离敏感字段）
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn export_json_for_user(&self, user_id: &str) -> String {
        self.export_json_filtered(Some(user_id), None, None, None, None, false)
    }

    /// 导出按完整筛选条件过滤后的 JSON（与列表页筛选语义一致）。
    ///
    /// `admin` = false 时按非管理员视角剥离 `debug` / `conversation` /
    /// `candidate_channels` / `filtered_channels` / `selected_channel`，
    /// 并把上游实际模型名遮掉（`redact_model_for_non_admin`：`model` 重写为
    /// 请求侧原名、移除 `origin_model`），与 `logs.rs::logs_with_user_email`
    /// 的剥离口径一致 —— 导出与列表页不应给出不同权限边界。
    #[allow(clippy::too_many_arguments)]
    pub fn export_json_filtered(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        admin: bool,
    ) -> String {
        let all = self.list_all();
        let filtered: Vec<Value> = all
            .iter()
            .filter(|l| Self::matches(l, user_id, model, channel_id, start, end))
            .map(|l| {
                let mut v = serde_json::to_value(l).unwrap_or(Value::Null);
                if !admin {
                    if let Some(obj) = v.as_object_mut() {
                        obj.remove("debug");
                        obj.remove("conversation");
                        obj.remove("candidate_channels");
                        obj.remove("filtered_channels");
                        obj.remove("selected_channel");
                        // 破甲控制台使用标记仅管理员可见（普通用户不下发），
                        // 口径与列表页 `logs_with_user_email` 一致。
                        obj.remove("purge_applied");
                        // 上游实际模型名遮罩（`model` → 请求名 + 移除
                        // `origin_model`），口径与列表页共用同一函数。
                        redact_model_for_non_admin(obj);
                    }
                }
                v
            })
            .collect();
        serde_json::to_string_pretty(&filtered).unwrap_or_else(|_| "[]".to_string())
    }

    /// 单条日志是否匹配筛选条件（列表与导出共用，保证语义一致）
    #[allow(clippy::too_many_arguments)]
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    fn matches(
        l: &RequestLog,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
    ) -> bool {
        if let Some(u) = user_id {
            if l.user_id.as_deref() != Some(u) {
                return false;
            }
        }
        if let Some(m) = model {
            if l.model != m {
                return false;
            }
        }
        if let Some(c) = channel_id {
            if l.channel_id.as_deref() != Some(c) {
                return false;
            }
        }
        if let Some(s) = start {
            if l.created_at < s {
                return false;
            }
        }
        if let Some(e) = end {
            if l.created_at > e {
                return false;
            }
        }
        true
    }

    /// 导出为 CSV 字符串（无 user 过滤 ⇒ 全量，管理员视角）
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn export_csv(&self) -> String {
        self.export_csv_filtered(None, None, None, None, None, true)
    }

    /// 导出按 user_id 过滤后的 CSV（普通用户导出自己的记录）
    pub fn export_csv_for_user(&self, user_id: &str) -> String {
        self.export_csv_filtered(Some(user_id), None, None, None, None, false)
    }

    /// 导出按完整筛选条件过滤后的 CSV（与列表页筛选语义一致）。
    ///
    /// `admin` = false 时 `model` 列换成请求侧原名（有映射时）、`origin_model`
    /// 列置空 —— 口径与 JSON 列表/导出（`redact_model_for_non_admin`）一致，
    /// 不让普通用户看穿上游实际模型（CSV 是列式输出，无法复用 Map 版遮罩函数）。
    #[allow(clippy::too_many_arguments)]
    pub fn export_csv_filtered(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        admin: bool,
    ) -> String {
        let all = self.list_all();
        let mut buf = String::from(
            "id,created_at,user_id,key_id,channel_id,channel_name,model,origin_model,input_tokens,output_tokens,cost,channel_cost,latency_ms,status_code,error_msg,ip\n",
        );
        for l in all
            .iter()
            .filter(|l| Self::matches(l, user_id, model, channel_id, start, end))
        {
            // 非 admin：`model` 列 = 请求侧原名（未映射时就是 `model`），
            // `origin_model` 列置空（避免请求名在 CSV 里出现两次）；
            // 渠道归属同属上游拓扑（结构体注释「admin 排查用，用户不展示」），
            // 非 admin 两列一并置空，与列表/JSON 导出同一权限边界。
            let (model_col, origin_col, channel_id_col, channel_name_col): (
                &str,
                &str,
                &str,
                &str,
            ) = if admin {
                (
                    l.model.as_str(),
                    l.origin_model.as_deref().unwrap_or(""),
                    l.channel_id.as_deref().unwrap_or(""),
                    l.channel_name.as_deref().unwrap_or(""),
                )
            } else {
                (l.origin_model.as_deref().unwrap_or(&l.model), "", "", "")
            };
            buf.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&l.id),
                csv_escape(&l.created_at.to_string()),
                csv_escape(l.user_id.as_deref().unwrap_or("")),
                csv_escape(l.key_id.as_deref().unwrap_or("")),
                csv_escape(channel_id_col),
                csv_escape(channel_name_col),
                csv_escape(model_col),
                csv_escape(origin_col),
                l.input_tokens,
                l.output_tokens,
                l.cost,
                l.channel_cost,
                l.latency_ms,
                l.status_code,
                csv_escape(l.error_msg.as_deref().unwrap_or("")),
                csv_escape(l.ip.as_deref().unwrap_or("")),
            ));
        }
        buf
    }

    /// 聚合统计辅助方法（供 dashboard 使用）。
    ///
    /// 返回所有请求日志（按时间正序），便于上层聚合。
    pub fn all_sorted_asc(&self) -> Vec<RequestLog> {
        let mut all = self.list_all();
        all.sort_by_key(|a| a.created_at);
        all
    }
}

/// 非管理员视角：把序列化后的请求日志里的「上游实际模型名」遮掉。
///
/// 字段语义（见 `RequestLog` 注释与 `api/openai/chat.rs`）：`model` 存的是
/// **映射后的上游真实模型名**（`log.model = used_upstream`），`origin_model`
/// 才是用户**请求侧的原始模型名**（仅发生映射改名时才有值，否则为 `null`）。
/// 普通用户不应看穿自己实际被哪家/哪个上游模型服务，故：
///
/// - 把 `model` **重写**成 `origin_model`（有值即映射改名场景）；
/// - **删除** `origin_model`，避免同一个请求名在响应里出现两次。
///
/// 为什么是「重写」而不是「删除」`model`：`model` 是前端列表 / 详情的必展示
/// 字段，直接删会让前端显示 `undefined`；重写后字段形状不变、值换成用户自己
/// 请求的名字。未发生映射（`origin_model` 为 `null`）时 `model` 本就是请求名，
/// 无需重写，但 `origin_model` 仍移除（值为 `null`，移除以免留下痕迹）。
///
/// 列表（`api/admin/logs.rs::logs_with_user_email`）与 JSON 导出
/// （`export_json_filtered`）共用本函数，保证两处权限边界一致；CSV 导出
/// （`export_csv_filtered`）是另一套列式输出，口径对齐但就地处理。
pub(crate) fn redact_model_for_non_admin(obj: &mut serde_json::Map<String, Value>) {
    // 先取 owned 副本，避免 if-let 试探式里对 obj 的不可变借用与下面 insert 的
    // 可变借用冲突（if-let 临时量存活期坑）。
    let origin = obj
        .get("origin_model")
        .and_then(|v| v.as_str())
        .map(String::from);
    if let Some(origin) = origin {
        obj.insert("model".into(), Value::String(origin));
    }
    obj.remove("origin_model");
    // 渠道归属同样属于上游拓扑：`channel_id`/`channel_name` 的结构体注释写明
    // 「admin 排查用，用户不展示」，但此前三个读出口都只删了路由回放字段、
    // 漏了这两个，普通用户仍能看穿请求被路由到哪个上游渠道。
    // 收进本函数，保证 列表 / JSON 导出 / CSV 导出 三处口径一致。
    obj.remove("channel_id");
    obj.remove("channel_name");
}

/// 标准 CSV 字段转义（RFC 4180）。
///
/// 规则：
/// - 字段包含逗号、双引号或换行符（`\n`/`\r`）时，用双引号包裹整个字段；
/// - 字段内的双引号用两个双引号转义（`"` → `""`）；
/// - 其余字段原样输出。
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

/// B13：请求日志容量兜底——超限时删除最旧一批。
///
/// 容量上限优先读运维保留配置（LogRetention 页设置），未配置时
/// 回退默认 10 万——原先硬编码 MAX_LOGS，管理员配置的容量被静默忽略。
///
/// R6：不再 `list("reqlog:")` 全键 + parse + 排序（10 万键 = 10 万 String
/// + 排序 + 逐条 DELETE）。改为：
/// 1. `count_prefix` 走 `COUNT(*)` 判是否超限（不物化键）；
/// 2. `list_range_asc` 键区间升序 + LIMIT 只取最旧 `excess` 个键。
///
/// 定宽前提：`reqlog:{ts}:{id}` 的 ts 是 10 位 unix 秒（2001@1e9 –
/// 2286@9.9e9），字典序等价时间序 ⇒ 升序最前 = 最旧。无法解析的脏键
/// 字典序排在数字之后（原实现 parse 失败记为 `i64::MAX`），同样最后才裁。
///
/// 抽成自由函数：写入路径（`RequestLogStore::add`）与异步批量写旁路
///（[`writer::LogWriter`] 的消费者）共用，避免两处实现漂移。
pub(crate) fn purge_request_overflow(store: &FileStore) -> anyhow::Result<()> {
    #[derive(serde::Deserialize)]
    struct Retention {
        #[serde(default)]
        max_capacity: Option<usize>,
    }
    let capacity = store
        .get::<Retention>("log_retention")
        .ok()
        .flatten()
        .and_then(|r| r.max_capacity);
    let max_logs = capacity.unwrap_or(RequestLogStore::MAX_LOGS);
    let total = store.count_prefix("reqlog:")?;
    if total < max_logs {
        return Ok(());
    }
    // 与旧实现同口径：超出的部分再多裁一批（批量摊薄，避免每次写都裁）。
    let excess = (total.saturating_sub(max_logs) + RequestLogStore::PURGE_BATCH).min(total);
    let upper = format!("reqlog:{}", '\u{10FFFF}');
    let doomed = store.list_range_asc("reqlog:", "reqlog:", &upper, excess)?;
    for k in doomed {
        // 每条索引条目都要删：`id` 与 `request_id` 各一条（见
        // [`RequestLogStore::index_entries`]）。只删 `id` 会留下指向已删
        // 主键的 `request_id` 索引 —— 键空间随裁剪永久膨胀，且
        // `get_by_request_id` 命中悬空索引会白跑一次 get。
        //
        // 主键 → 索引的映射不在主键里（`reqlog_id:{id}={主键}` 的反向），
        // 所以先读出整条日志拿回两个候选值；读不到（已损坏/已删）时退回
        // 主键尾段的 `id` 兜底，与 `delete_indexes_by_primary` 同口径。
        let mut candidates: Vec<String> = Vec::new();
        let mut user_key: Option<String> = None;
        if let Ok(Some(log)) = store.get::<RequestLog>(&k) {
            if !log.id.is_empty() {
                candidates.push(log.id.clone());
            }
            if let Some(request_id) = log.request_id.clone().filter(|v| !v.is_empty()) {
                candidates.push(request_id);
            }
            if let Some(uid) = log.user_id.as_deref().filter(|u| !u.is_empty()) {
                user_key = Some(RequestLogStore::user_index_key(
                    uid,
                    log.created_at,
                    log.id.as_str(),
                ));
            }
        }
        if candidates.is_empty() {
            if let Some(id) = k.rsplit(':').next().filter(|id| !id.is_empty()) {
                candidates.push(id.to_string());
            }
        }
        candidates.dedup();
        for candidate in candidates {
            let index = RequestLogStore::index_key(
                &RequestLogStore::escape_index_component(&candidate),
                &k,
            );
            let _ = store.delete(&index);
        }
        if let Some(index) = user_key {
            let _ = store.delete(&index);
        }
        if let Err(e) = store.delete(&k) {
            tracing::warn!("request log purge failed for {k}: {e}");
        }
    }
    Ok(())
}

/// 审计日志存储。
///
/// key 格式：`auditlog:{created_at}:{id}`
pub struct AuditLogStore {
    store: Arc<FileStore>,
}

impl AuditLogStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }

    fn key_of(log: &AuditLog) -> String {
        format!("auditlog:{}:{}", log.created_at, log.id)
    }

    /// 追加一条审计日志
    pub fn add(&self, mut log: AuditLog) -> anyhow::Result<AuditLog> {
        if log.id.is_empty() {
            log.id = uuid::Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = chrono::Utc::now().timestamp();
        }
        let key = Self::key_of(&log);
        self.store.put(&key, &log)?;
        Ok(log)
    }

    /// 列出全部审计日志（按时间倒序）
    pub fn list(&self) -> Vec<AuditLog> {
        let keys = match self.store.list("auditlog:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut logs: Vec<AuditLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<AuditLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        logs
    }

    /// 按 ID 删除单条审计日志。
    ///
    /// 防篡改最简方案（P0 安全加固）：删除动作本身追加一条**自审计**，
    /// 记「删了哪条、何时删的」。自审计写在删除**之后**——本次留痕不被
    /// 自己抹掉；后续删除/清空仍可再抹它，但每一次定向抹除都会留下新的
    /// 删除留痕（哈希链未实现的已知取舍，先做删除审计）。
    ///
    /// 链式证据：被删的若本身是删除/清空留痕，新留痕**继承其原始目标**——
    /// 否则「删原记录 + 删留痕」两步即可把删除事件的主体洗掉；继承后反复
    /// 抹留痕只会不断产生指向同一原始删除对象的留痕，抹不尽。
    ///
    /// 「仅管理员可操作」由 API 层 `verify_admin` 保证
    /// （`handle_delete_audit_logs` / `handle_clear_audit_logs`），存储层
    /// 无操作者上下文、不重复鉴权。
    pub fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let keys = self.store.list("auditlog:")?;
        let target = keys.into_iter().find(|k| k.ends_with(&format!(":{id}")));
        if let Some(k) = target {
            // 链式证据：被删的若本身是留痕记录，新留痕继承其 target
            let chained = self
                .store
                .get::<AuditLog>(&k)
                .ok()
                .flatten()
                .filter(|p| p.action == "audit_log.delete" || p.action == "audit_log.clear")
                .map(|p| p.target);
            self.store.delete(&k)?;
            self.record_deletion(
                "audit_log.delete",
                &chained.unwrap_or_else(|| format!("id:{id}")),
                serde_json::json!({ "deleted_id": id }),
            );
        }
        Ok(())
    }

    /// 删除动作留痕：追加一条自审计。
    ///
    /// 吞错仅告警——留痕失败不改变删除结果本身（存储层尽力而为；
    /// API 层的 purge 前置审计是 fail-closed 的，与此处语义不同）。
    fn record_deletion(&self, action: &str, target: &str, detail: serde_json::Value) {
        let mut entry = AuditLog::new("system", action, target);
        entry.before = Some(serde_json::to_string(&detail).unwrap_or_default());
        if let Err(e) = self.add(entry) {
            tracing::warn!("Failed to record audit deletion self-audit: {e}");
        }
    }

    /// 清理早于 `before_ts` 的审计日志（与请求日志同保留策略）。
    ///
    /// key 格式 `auditlog:{created_at}:{id}` 时间戳前缀，范围扫描即删。
    pub fn delete_older_than(&self, before_ts: i64) -> anyhow::Result<usize> {
        let keys = self.store.list("auditlog:")?;
        let doomed: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                k.strip_prefix("auditlog:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|ts| ts < before_ts)
                    .unwrap_or(false)
            })
            .collect();
        let n = doomed.len();
        for k in doomed {
            if let Err(e) = self.store.delete(&k) {
                tracing::warn!("audit retention delete failed for {k}: {e}");
            }
        }
        Ok(n)
    }

    /// 批量删除审计日志（每条删除各自留一条自审计，见 [`Self::delete_by_id`]）
    pub fn delete_many(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.delete_by_id(id).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 清空全部审计日志。
    ///
    /// 防篡改：清空动作本身追加一条自审计，写在清空**之后** ⇒ 本次留痕
    /// 存活到下一次清空（每次清空替换上一条清空留痕；窗口内的删除留痕
    /// 随清空一并消失——哈希链未实现的已知取舍）。
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("auditlog:")?;
        let n = keys.len();
        for k in &keys {
            if let Err(e) = self.store.delete(k) {
                tracing::warn!("clear audit log failed for {k}: {e}");
            }
        }
        self.record_deletion(
            "audit_log.clear",
            "count",
            serde_json::json!({ "removed": n }),
        );
        Ok(n)
    }

    /// 分页查询（P0 性能：键倒序 LIMIT 取前 page*size 条再内存切片，索引范围扫描）
    pub fn list_paged(&self, page: usize, size: usize) -> (Vec<AuditLog>, usize) {
        let page = page.clamp(1, 10_000);
        let size = size.clamp(1, RequestLogStore::PAGE_SIZE_MAX);
        let total = self.store.list("auditlog:").map(|k| k.len()).unwrap_or(0);
        let limit = page.saturating_mul(size);
        let keys = self
            .store
            .list_latest_keys("auditlog:", limit)
            .unwrap_or_default();
        let mut logs: Vec<AuditLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<AuditLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        let start_idx = (page - 1) * size;
        let paged: Vec<AuditLog> = if start_idx >= logs.len() {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(logs.len());
            logs[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }
}

// ── 安全事件存储 ──────────────────────────────────────────────────────

/// 安全事件类型（结构化分类，供安全中心展示与告警规则引用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityEventType {
    /// 认证失败（登录/API key 校验失败）
    AuthFailure,
    /// 限流触发（登录/注册/API 限流）
    RateLimit,
    /// IP 拦截（IP 黑名单命中）
    IpBlocked,
    /// 滥用检测（额度耗尽后继续尝试等）
    Abuse,
    /// 入侵尝试（路径遍历/异常 UA 等）
    Intrusion,
}

impl SecurityEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AuthFailure => "auth_failure",
            Self::RateLimit => "rate_limit",
            Self::IpBlocked => "ip_blocked",
            Self::Abuse => "abuse",
            Self::Intrusion => "intrusion",
        }
    }
}

/// 单条结构化安全事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: String,
    /// 事件类型
    pub event_type: SecurityEventType,
    /// 严重程度：info / warning / critical
    pub severity: String,
    /// 事件来源（IP）
    pub ip: Option<String>,
    /// 关联用户（若可归因）
    pub user_id: Option<String>,
    /// 关联请求 ID（若在请求路径内）
    pub request_id: Option<String>,
    /// 人类可读详情（脱敏）
    pub detail: String,
    pub created_at: i64,
}

impl SecurityEvent {
    pub fn new(event_type: SecurityEventType, severity: &str, detail: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            severity: severity.to_string(),
            ip: None,
            user_id: None,
            request_id: None,
            detail,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    pub fn with_ip(mut self, ip: Option<String>) -> Self {
        self.ip = ip;
        self
    }

    pub fn with_user(mut self, user_id: Option<String>) -> Self {
        self.user_id = user_id;
        self
    }
}

/// 安全事件存储——FileStore 持久化，环形上限 1000 条。
///
/// key 格式：`security_event:{created_at}:{id}`
pub struct SecurityEventStore {
    store: Arc<FileStore>,
    /// 环形缓冲容量上限
    max_events: usize,
}

impl SecurityEventStore {
    const MAX_EVENTS: usize = 1000;

    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            max_events: Self::MAX_EVENTS,
        }
    }

    fn key_of(ev: &SecurityEvent) -> String {
        format!("security_event:{}:{}", ev.created_at, ev.id)
    }

    /// 追加一条安全事件（超出容量清理最旧）。
    pub fn add(&self, mut ev: SecurityEvent) -> anyhow::Result<SecurityEvent> {
        if ev.id.is_empty() {
            ev.id = uuid::Uuid::new_v4().to_string();
        }
        if ev.created_at == 0 {
            ev.created_at = chrono::Utc::now().timestamp();
        }
        self.store.put(&Self::key_of(&ev), &ev)?;
        self.purge_overflow();
        Ok(ev)
    }

    /// 容量超限时删除最旧事件（best-effort，失败仅告警）。
    fn purge_overflow(&self) {
        let Ok(keys) = self.store.list("security_event:") else {
            return;
        };
        let total = keys.len();
        if total <= self.max_events {
            return;
        }
        let mut timed: Vec<(i64, String)> = keys
            .into_iter()
            .map(|k| {
                let ts = k
                    .strip_prefix("security_event:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(i64::MAX);
                (ts, k)
            })
            .collect();
        timed.sort_by_key(|(ts, _)| *ts);
        let excess = total - self.max_events;
        for (_, k) in timed.into_iter().take(excess) {
            let _ = self.store.delete(&k);
        }
    }

    /// 列出全部事件（按时间倒序）。
    pub fn list_all(&self) -> Vec<SecurityEvent> {
        let keys = match self.store.list("security_event:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut events: Vec<SecurityEvent> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<SecurityEvent>(&k).ok().flatten())
            .collect();
        events.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        events
    }

    /// 过滤 + 分页。
    ///
    /// - `event_type`：按类型过滤（None=全部）
    /// - `start`：时间下限（unix ts，None=不限）
    /// - `end`：时间上限（unix ts，None=不限）
    /// - `page/size`：分页（1-based）
    pub fn list_paged(
        &self,
        event_type: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        page: usize,
        size: usize,
    ) -> (Vec<SecurityEvent>, usize) {
        let all = self.list_all();
        let filtered: Vec<SecurityEvent> = all
            .into_iter()
            .filter(|e| {
                if let Some(ty) = event_type {
                    if e.event_type.as_str() != ty {
                        return false;
                    }
                }
                if let Some(s) = start {
                    if e.created_at < s {
                        return false;
                    }
                }
                if let Some(en) = end {
                    if e.created_at > en {
                        return false;
                    }
                }
                true
            })
            .collect();
        let total = filtered.len();
        let page = page.clamp(1, 10_000);
        let size = size.clamp(1, RequestLogStore::PAGE_SIZE_MAX);
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            filtered[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }
}

// ── 全局组合 Store ──────────────────────────────────────────────────

/// 组合日志存储（请求日志 + 审计日志 + 安全事件）。
///
/// 加入 AppState 供各处使用。
pub struct LogStore {
    pub requests: RequestLogStore,
    pub audits: AuditLogStore,
    pub security: SecurityEventStore,
    /// 按天聚合（记账热路径同步维护，报表读它不扫明细）。
    pub daily: daily_agg::DailyAggStore,
    /// 异步批量写旁路：`record_request` 只入队，后台合并落盘（见 [`writer`]）。
    writer: writer::LogWriter,
}

impl LogStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let daily = daily_agg::DailyAggStore::new(store.clone());
        // 与 daily 共享聚合写锁：旁路批量折叠聚合行时也要和回填/清理互斥。
        let writer = writer::LogWriter::new(store.clone(), daily.lock_handle());
        Self {
            requests: RequestLogStore::new(store.clone()),
            audits: AuditLogStore::new(store.clone()),
            security: SecurityEventStore::new(store.clone()),
            daily,
            writer,
        }
    }

    /// 便捷方法：记录请求日志（忽略错误，仅 tracing）。
    ///
    /// 入队到异步批量写旁路：只做一次 `try_send`，**不阻塞请求线程**；
    /// 队列满则丢弃并告警（丢一条统计远好过拖慢在途请求）。无 tokio
    /// runtime 时旁路自动降级为同步写，语义与改造前一致。
    pub fn record_request(&self, log: RequestLog) {
        self.writer.send(writer::LogOp::Request(log));
    }

    /// 优雅关闭：阻塞直到异步写入队列排空（同步降级模式下立即返回）。
    pub fn flush(&self) {
        self.writer.flush();
    }

    /// 非阻塞尽力 flush：唤醒后台任务立即处理，不等待。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn try_flush(&self) {
        self.writer.try_flush();
    }

    /// 便捷方法：记录审计日志（忽略错误，仅 tracing）
    pub fn record_audit(
        &self,
        admin_id: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) {
        self.record_audit_with_ip(admin_id, action, target, None, before, after);
    }

    /// 便捷方法：记录审计日志（**带客户端 IP**，忽略错误，仅 tracing）
    ///
    /// P0 安全加固：历史审计记录 IP 恒空（`record_audit` 无 IP 入参，且
    /// `api::admin::common::record_audit` 包装层签名已定型、不宜扩散改动），
    /// 故新增本入口供能拿到请求头的新代码使用；IP 经反代时需
    /// `trust_proxy_headers=true` 才可信（`api::common::extract_client_ip`）。
    pub fn record_audit_with_ip(
        &self,
        admin_id: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        ip: Option<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) {
        let mut entry = AuditLog::new(admin_id, action, target);
        entry.ip = ip;
        entry.before = before.map(|v| serde_json::to_string(&v).unwrap_or_default());
        entry.after = after.map(|v| serde_json::to_string(&v).unwrap_or_default());
        if let Err(e) = self.audits.add(entry) {
            tracing::warn!("Failed to record audit log: {e}");
        }
    }

    /// 便捷方法：记录安全事件（忽略错误，仅 tracing）
    pub fn record_security(&self, event: SecurityEvent) {
        if let Err(e) = self.security.add(event) {
            tracing::warn!("Failed to record security event: {e}");
        }
    }

    /// 回填历史日聚合：流式分批读全部明细，按 UTC 日折出主档 + 用户行。
    ///
    /// 覆盖**仅 date < 今天**的日子（今天归运行期 RMW 独占，回填与在途
    /// 记账互不踩踏）。shrink 守卫（蓝本 backfill「拿不准不动」语义）：
    /// 逐行覆盖前与既有行比对，既有行计数更大说明明细已被 retention
    /// 裁过——聚合寿命独立于明细，重算值不可信，保留既有行不覆盖。
    /// 全部写完后落回填 marker（as_of=昨天），报表读路径据此切聚合。
    pub fn backfill_history(&self, batch: usize) -> anyhow::Result<BackfillReport> {
        let today = daily_agg::today_key();
        let keys = self.requests.all_keys_asc();
        let mut aggs: std::collections::BTreeMap<String, daily_agg::DailyAgg> = Default::default();
        let mut users: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, daily_agg::UserDailyRow>,
        > = Default::default();
        let mut report = BackfillReport::default();
        for chunk in keys.chunks(batch.max(1)) {
            for l in self.requests.logs_by_keys(chunk) {
                let day = daily_agg::date_key(l.created_at);
                if day.is_empty() || day >= today {
                    continue;
                }
                report.scanned += 1;
                let agg = aggs
                    .entry(day.clone())
                    .or_insert_with(|| daily_agg::DailyAgg {
                        date: day.clone(),
                        ..Default::default()
                    });
                daily_agg::fold_request(agg, &l);
                if let Some(uid) = l.user_id.as_deref() {
                    let row = users
                        .entry(uid.to_string())
                        .or_default()
                        .entry(day.clone())
                        .or_insert_with(|| daily_agg::UserDailyRow {
                            date: day.clone(),
                            ..Default::default()
                        });
                    daily_agg::fold_user_row(row, &l);
                }
            }
        }
        for (day, agg) in &aggs {
            if let Some(existing) = self.daily.load_day(day) {
                if existing.requests > agg.requests {
                    report.skipped_shrink += 1;
                    tracing::warn!(
                        "回填 shrink 守卫：既有行 {day} requests={} 大于重算={}，保留不动",
                        existing.requests,
                        agg.requests
                    );
                    continue;
                }
            }
            self.daily.overwrite_day(agg)?;
            report.days_written += 1;
        }
        for (uid, days) in &users {
            for (day, row) in days {
                if let Some(ex) = self.daily.load_user_range(uid, day, day).into_iter().next() {
                    if ex.count > row.count {
                        report.skipped_shrink += 1;
                        tracing::warn!(
                            "回填 shrink 守卫：用户 {uid} {day} 既有 count={} 大于重算={}，保留不动",
                            ex.count,
                            row.count
                        );
                        continue;
                    }
                }
                self.daily.overwrite_user_row(uid, row)?;
                report.user_rows_written += 1;
            }
        }
        // as_of=昨天：marker 语义为「as_of 及之前的日子已由回填覆盖」，
        // 今天的数据由运行期 record 保证。空历史也要落 marker（新装首日
        // 即闭合聚合链条，无历史可回填 = 回填完成）。
        let yesterday = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d")
            .map(|d| {
                (d - chrono::Duration::days(1))
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_else(|_| today.clone());
        self.daily.set_backfill_marker(&yesterday)?;
        Ok(report)
    }

    /// 报表时区迁移：把存量聚合行从 `offset_from` 口径重算到 `offset_to` 口径。
    ///
    /// **只在显式调用时动数据**（生产机不做隐式重写库）——启动与 cron 都不碰这里，
    /// 入口是 `POST /api/logs/agg/tz-migrate`。
    ///
    /// 操作序（顺序不可调；任一步中断都必须停在「读路径退回明细现算」的安全态）：
    /// 1. 校验 `offset_to` 就是当前生效口径（进程快照 = 配置文件那份）。否则进程
    ///    重启后配置回退，重算出来的数据反而与运行期口径不一致 —— 先存设置再迁移。
    /// 2. **幂等短路**：口径标记（[`daily_agg::TZ_MARKER`]）已是目标偏移就直接返回，
    ///    不删不重算（老库无标记时以调用方声明的 `offset_from` 兜底）。
    /// 3. 清回填 marker：迁移期间报表读路径退回明细现算，绝不端出半新半旧的假数。
    /// 4. 删窗口 `[最早聚合日, 昨天]` 的旧口径行 —— **必须先删**：回填的 shrink
    ///    守卫（既有行计数更大就保留不动，见上面 `backfill_history`）会把旧口径的
    ///    大值原样留下，不删就迁不动。不能用 `prune_before("9999-12-31")` 代替，
    ///    那会连今天一起抹掉，而回填不管今天 → 报表开天窗。
    /// 5. `backfill_history` 按新口径全量重算（只覆盖 date < 今天 → **切换当日偏低、
    ///    次日自愈**，端点必须把这条回给运维）。
    /// 6. 落口径标记（下一步的幂等判据）。
    ///
    /// 三坑（端点响应里原样回给运维，别只写在文档）：①第 4 步之前必须先存偏移设置，
    /// 否则第 1 步直接 400；②升级后到本次调用之间报表是**新旧口径分裂**的；③切换
    /// 当日「今天」只累计切换之后到达的日志，次日才自愈。
    pub fn tz_migrate(&self, offset_from: i32, offset_to: i32) -> anyhow::Result<TzMigrateReport> {
        let current = crate::config::report_tz_offset();
        anyhow::ensure!(
            current == offset_to,
            "目标偏移 {offset_to} 与当前生效口径 {current} 不一致：请先在报表设置里保存偏移，再调迁移端点"
        );
        let today = daily_agg::today_key();
        let window_to = daily_agg::shift_day(&today, 1);
        let earliest = self.daily.earliest_day()?;
        let mut report = TzMigrateReport {
            // 「自该日起的聚合行都是新口径」：最早落盘日；空库 → 今天
            effective_from: earliest.clone().unwrap_or_else(|| today.clone()),
            window_to: window_to.clone(),
            ..Default::default()
        };
        // 老库从未记过口径 → 以调用方声明的旧口径为准（生产现状 = UTC+0）
        let stored = self.daily.tz_marker().unwrap_or(offset_from);
        if stored == offset_to {
            return Ok(report);
        }
        self.daily.clear_backfill_marker()?;
        if let Some(from) = earliest.as_deref() {
            if from <= window_to.as_str() {
                report.window_applied = true;
                report.rows_deleted = self.daily.delete_day_range(from, &window_to)?;
            }
        }
        report.backfill = self.backfill_history(5000)?;
        self.daily.set_tz_marker(offset_to)?;
        Ok(report)
    }
}

/// 时区迁移的统计返回（`POST /api/logs/agg/tz-migrate` 直接回给运维）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TzMigrateReport {
    /// 新口径生效起始报表日（含）。空库 = 今天。
    pub effective_from: String,
    /// 删除窗上界 = 昨天（回填/迁移都不覆盖今天）。
    pub window_to: String,
    /// 是否真的执行了删除（false = 窗口空或聚合行只到今天）。
    pub window_applied: bool,
    /// 删掉的旧口径聚合行数（主档 + 用户行）。
    pub rows_deleted: usize,
    /// 重算统计（含 shrink 守卫拒绝数，正常迁移后应为 0——窗口已先删干净）。
    pub backfill: BackfillReport,
}

/// 回填/重建的统计返回（cron 与 `POST /api/logs/agg/rebuild` 共用形状）。
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct BackfillReport {
    /// 读入并参与折叠的明细条数（今天行不计）
    pub scanned: usize,
    /// 写入的历史日主档数
    pub days_written: usize,
    /// 写入的用户日行数
    pub user_rows_written: usize,
    /// shrink 守卫拒绝覆盖的行数（既有行计数更大，保留不动）
    pub skipped_shrink: usize,
}

// 用 RwLock 包装以便纳入 AppState（Clone）
#[allow(dead_code)]
pub type SharedLogStore = Arc<RwLock<LogStore>>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> LogStore {
        LogStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    /// `purge_applied`：new() 默认 false；序列化含该字段、反序列化保留；
    /// 老日志缺字段按 `#[serde(default)]` 落 false（零迁移语义）。
    #[test]
    fn request_log_purge_applied_roundtrip() {
        let mut log = RequestLog::new();
        assert!(!log.purge_applied, "new() 应默认 false");
        log.purge_applied = true;
        let json = serde_json::to_string(&log).expect("序列化");
        assert!(
            json.contains("\"purge_applied\":true"),
            "序列化应含字段: {json}"
        );
        let back: RequestLog = serde_json::from_str(&json).expect("反序列化");
        assert!(back.purge_applied, "反序列化应保留 true");
        // 老日志缺该字段 → 默认 false
        let parsed: RequestLog =
            serde_json::from_str(r#"{"id":"x","created_at":1}"#).expect("老日志应能解析");
        assert!(!parsed.purge_applied, "缺字段应默认 false");
    }

    /// `window_bounds`：条数走 COUNT(*)、首尾各取一个键，不反序列化全量。
    /// 与旧「全量 list_all + 过滤」口径逐项对照。
    #[test]
    fn window_bounds_matches_full_scan_semantics() {
        let s = store();
        const BASE: i64 = 1_700_000_000;
        for i in 0..5i64 {
            let mut log = RequestLog::new();
            log.id = format!("w{i}");
            log.created_at = BASE + i * 100;
            log.model = "m".into();
            s.requests.add(log).unwrap();
        }
        // 无下界：全部 5 条，oldest/newest = 首尾时间戳
        let (total, count, oldest, newest) = s.requests.window_bounds(None, None).unwrap();
        assert_eq!((total, count), (5, 5));
        assert_eq!((oldest, newest), (Some(BASE), Some(BASE + 400)));

        // 含两端的窗口（与旧 `created_at >= from` 一致）
        let (total, count, oldest, newest) = s
            .requests
            .window_bounds(Some(BASE + 100), Some(BASE + 300))
            .unwrap();
        assert_eq!((total, count), (5, 3), "全部 5，窗口内 3");
        assert_eq!((oldest, newest), (Some(BASE + 100), Some(BASE + 300)));

        // 空窗口：计数 0 且不返回首尾
        let (_, count, oldest, newest) =
            s.requests.window_bounds(Some(BASE + 10_000), None).unwrap();
        assert_eq!(count, 0);
        assert_eq!((oldest, newest), (None, None));
    }

    #[test]
    fn add_and_list_request() {
        let s = store();
        let mut log = RequestLog::new();
        log.model = "gpt-4".to_string();
        log.input_tokens = 100;
        log.output_tokens = 50;
        log.cost = 5;
        s.requests.add(log.clone()).unwrap();

        let all = s.requests.list_all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].model, "gpt-4");
        assert_eq!(all[0].input_tokens, 100);
    }

    /// R2：`get_by_request_id` 走二级索引（写入时建的 `reqlog_id:{id}={主键}`）。
    ///
    /// 这里断言的是**语义正确性**（按日志 id 与 request_id 都能查到、
    /// 删除后不再命中、重复写入不会互相顶掉），索引的 O(1) 特性由
    /// 存储层 `list_latest_keys` 的单键 LIMIT 保证，不在单测里计时间。
    #[test]
    fn get_by_request_id_uses_index_and_survives_delete() {
        let s = store();
        let mut log = RequestLog::new();
        log.id = "log-abc".to_string();
        log.request_id = Some("req/with=special".to_string());
        log.model = "m1".to_string();
        s.requests.add(log).unwrap();

        let by_id = s.requests.get_by_request_id("log-abc").expect("按 id 命中");
        assert_eq!(by_id.model, "m1");
        let by_request = s
            .requests
            .get_by_request_id("req/with=special")
            .expect("按 request_id 命中（含 / 与 = 的转义）");
        assert_eq!(by_request.model, "m1");

        // 删除后两条索引都必须清理干净，不能留悬空
        s.requests.delete_by_id("log-abc").unwrap();
        assert!(s.requests.get_by_request_id("log-abc").is_none());
        assert!(s.requests.get_by_request_id("req/with=special").is_none());
    }

    /// R2 第二档：`list_by_user` 走用户索引（`reqlogu:{uid}:{ts}:{id}`）。
    ///
    /// 断言语义：marker 未落前一律全扫（与改造前等价）；回填后走区间
    /// 读且结果与全扫一致；窗口两端含；删除后索引同步清。
    #[test]
    fn list_by_user_uses_user_index_and_falls_back() {
        let s = store();
        const BASE: i64 = 1_700_000_000;
        for i in 0..4i64 {
            let mut log = RequestLog::new();
            log.id = format!("u{i}");
            log.user_id = Some("u1".into());
            log.created_at = BASE + i * 10;
            log.model = format!("m{i}");
            s.requests.add(log).unwrap();
        }
        let mut other = RequestLog::new();
        other.id = "x1".into();
        other.user_id = Some("u2".into());
        other.created_at = BASE + 5;
        other.model = "mx".into();
        s.requests.add(other).unwrap();

        // 无 marker：全扫路径（与改造前逐字等价）
        let all = s.requests.list_by_user("u1", None, None);
        assert_eq!(all.len(), 4, "只命中该用户的日志");
        assert!(all.windows(2).all(|w| w[0].created_at >= w[1].created_at));

        // 回填：新写入的日志在 `add()` 时已建用户索引，故补写 0 条；
        // 但 marker 必须落位（闸门从「全扫」切到「索引路径」）。
        let written = s.requests.backfill_user_index().unwrap();
        assert_eq!(written, 0, "新日志的索引已在写入时建好");
        assert!(
            matches!(
                s.requests.store.get::<bool>("reqlogu:backfill_done"),
                Ok(Some(true))
            ),
            "回填后 marker 必须落位"
        );
        assert_eq!(s.requests.backfill_user_index().unwrap(), 0, "幂等");

        // 索引路径与全扫结果逐条一致
        let indexed = s.requests.list_by_user("u1", None, None);
        assert_eq!(
            indexed.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            vec!["u3", "u2", "u1", "u0"]
        );
        // 窗口过滤与旧语义一致（含两端）
        let win = s
            .requests
            .list_by_user("u1", Some(BASE + 10), Some(BASE + 20));
        assert_eq!(win.len(), 2, "窗口 [BASE+10, BASE+20] 含两端共 2 条");
        assert_eq!(
            win.iter().map(|l| l.model.as_str()).collect::<Vec<_>>(),
            vec!["m2", "m1"]
        );

        // 删除后用户索引被连带清理（再读只剩 3 条）
        s.requests.delete_by_id("u2").unwrap();
        let after = s.requests.list_by_user("u1", None, None);
        assert_eq!(after.len(), 3, "删一条后用户索引同步更新");
        assert!(after.iter().all(|l| l.id != "u2"));
    }

    /// 回填前的历史日志（无索引）也必须能被读到 —— 闸门保证这一点。
    #[test]
    fn list_by_user_reads_legacy_rows_before_backfill() {
        let s = store();
        let mut legacy = RequestLog::new();
        legacy.id = "legacy".into();
        legacy.user_id = Some("u9".into());
        legacy.created_at = 1_700_000_000;
        legacy.model = "old".into();
        s.requests
            .store
            .put(
                &format!("reqlog:{}:{}", legacy.created_at, legacy.id),
                &legacy,
            )
            .unwrap();
        // marker 未落 = 全扫，能读到；若误走索引会漏（区间为空）
        assert_eq!(s.requests.list_by_user("u9", None, None).len(), 1);
        // 回填把历史行补上索引后，索引路径同样能读到
        assert_eq!(s.requests.backfill_user_index().unwrap(), 1);
        assert_eq!(s.requests.list_by_user("u9", None, None).len(), 1);
    }

    #[test]
    fn retention_roundtrip_including_daily_tier() {
        let s = store();
        s.requests
            .set_retention_full(Some(30), Some(1000), Some(7), Some(365))
            .unwrap();
        assert_eq!(
            s.requests.retention_full(),
            (Some(30), Some(1000), Some(7), Some(365))
        );
        // 两元组便捷读不受新档影响
        assert_eq!(s.requests.retention(), (Some(30), Some(1000)));
        // 第三档 None = 永久（缺省读回 None，旧配置无字段也兼容）
        s.requests.set_retention(Some(10), None).unwrap();
        assert_eq!(s.requests.retention_full(), (Some(10), None, None, None));
    }

    /// 造一个「昨天」时间戳（now−86400：同钟点前一天，必落在昨日日历）。
    fn yesterday_ts() -> i64 {
        chrono::Utc::now().timestamp() - 86_400
    }

    /// R6：容量裁剪按「键区间升序 + LIMIT」取最旧一批，不再 list 全键再排序。
    ///
    /// 容量上限必须大于 PURGE_BATCH，否则 `excess` 被 min(total) 钳成「全删」，
    /// 测不出「裁最旧、留最新」。这里容量 = 10001、灌 10006 条 → 裁 10005 留 1。
    #[test]
    fn purge_overflow_trims_oldest_and_keeps_newest() {
        let s = store();
        s.requests.set_retention(None, Some(10_001)).unwrap();
        const N: usize = 10_006;
        let base = 1_700_000_000i64; // 10 位定宽时间戳（字典序 = 时间序）
        let entries: Vec<(String, serde_json::Value)> = (0..N)
            .map(|i| {
                let mut log = RequestLog::new();
                log.id = format!("p{i:05}");
                log.created_at = base + i as i64;
                log.model = "m".to_string();
                let key = format!("reqlog:{}:{}", log.created_at, log.id);
                (key, serde_json::to_value(&log).unwrap())
            })
            .collect();
        let refs: Vec<(&str, serde_json::Value)> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        // 单事务灌入（逐条 put 会慢一个量级）
        s.requests.store.write_batch(&refs).unwrap();
        // 最旧那条的二级索引也必须被连带清理（悬空索引 = 越权读旧日志）；
        // 两条都要造：只清 `id` 会留下 `request_id` 索引永久残留。
        let oldest_key = entries[0].0.as_str().to_string();
        let index_id = RequestLogStore::index_key(
            &RequestLogStore::escape_index_component("p00000"),
            &oldest_key,
        );
        s.requests.store.put(&index_id, &oldest_key).unwrap();
        // 给最旧一条装上 request_id，走真实读取路径（purge 从整条日志里取候选）
        let mut oldest = s
            .requests
            .store
            .get::<RequestLog>(&oldest_key)
            .unwrap()
            .expect("最旧日志存在");
        oldest.request_id = Some("req-oldest".to_string());
        s.requests.store.put(&oldest_key, &oldest).unwrap();
        let index_req = RequestLogStore::index_key(
            &RequestLogStore::escape_index_component("req-oldest"),
            &oldest_key,
        );
        s.requests.store.put(&index_req, &oldest_key).unwrap();

        purge_request_overflow(&s.requests.store).unwrap();

        // 登录后剩余恰好 1 条，且是最新那条
        let left = s.requests.list_all();
        assert_eq!(left.len(), 1, "裁掉 10005 条，只留最新");
        assert_eq!(left[0].id, format!("p{:05}", N - 1));
        // 二级索引同步清掉（`id` 与 `request_id` 两条都不得残留）
        assert!(s.requests.store.get::<String>(&index_id).unwrap().is_none());
        assert!(
            s.requests
                .store
                .get::<String>(&index_req)
                .unwrap()
                .is_none(),
            "request_id 索引也必须随主键一起清理"
        );
        // COUNT(*) 与剩余键一致（无残留）
        assert_eq!(s.requests.store.count_prefix("reqlog:").unwrap(), 1);
    }

    /// 未达容量上限时不裁剪（COUNT(*) 早退，且一条不删）。
    #[test]
    fn purge_overflow_noop_below_capacity() {
        let s = store();
        s.requests.set_retention(None, Some(5)).unwrap();
        for i in 0..3i64 {
            let mut log = RequestLog::new();
            log.id = format!("k{i}");
            log.created_at = 1_700_000_000 + i;
            s.requests.add(log).unwrap();
        }
        purge_request_overflow(&s.requests.store).unwrap();
        assert_eq!(s.requests.list_all().len(), 3);
    }

    /// 键区间升序限量扫描：字典序最前 = 时间最旧，LIMIT 控制物化数量。
    #[test]
    fn list_range_asc_returns_oldest_first() {
        let s = store();
        let store = &s.requests.store;
        for i in [5i64, 2, 9, 1, 7] {
            store
                .put(&format!("reqlog:{}:x{i}", 1_700_000_000 + i), &i)
                .unwrap();
        }
        let upper = format!("reqlog:{}", '\u{10FFFF}');
        let keys = store
            .list_range_asc("reqlog:", "reqlog:", &upper, 3)
            .unwrap();
        assert_eq!(
            keys,
            vec![
                "reqlog:1700000001:x1".to_string(),
                "reqlog:1700000002:x2".to_string(),
                "reqlog:1700000005:x5".to_string(),
            ],
            "升序最前三个 = 时间最旧的三个"
        );
    }

    #[test]
    fn daily_prune_boundary_keeps_today_and_cutoff_day() {
        let s = store();
        // 日键一律走 `day_key_back`（与 `today_key`/`cutoff_date_key` 同一快照口径）。
        // 以前这里用 `Utc::now().naive_utc().date() - days(back)` 造键再比：缺省偏移
        // 从隐式 UTC 变成 8 之后，UTC runner 的 16:00-23:59 会两边不同日 → 间歇假红。
        let day = daily_agg::day_key_back;
        let today = daily_agg::today_key();
        assert_eq!(day(0), today);
        // 造三天行：今天、截止日（今天−2）、截止日前一天（今天−3）
        for back in [0i64, 2, 3] {
            let date = day(back);
            s.daily
                .overwrite_day(&daily_agg::DailyAgg {
                    date: date.clone(),
                    requests: 1,
                    ..Default::default()
                })
                .unwrap();
            s.daily
                .overwrite_user_row(
                    "u1",
                    &daily_agg::UserDailyRow {
                        date: date.clone(),
                        count: 1,
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        // N=3 →「含今天 3 个自然日」→ cutoff=今天−2，删严格早于 cutoff 的
        let cutoff = daily_agg::cutoff_date_key(3);
        assert_eq!(cutoff, day(2));
        let removed = s.daily.prune_before(&cutoff).unwrap();
        assert_eq!(removed, 2, "主档+用户行各删一条（今天−3 的）");
        assert!(s.daily.load_day(&today).is_some(), "今天不删");
        assert!(s.daily.load_day(&day(2)).is_some(), "截止日当天保留");
        assert!(s.daily.load_day(&day(3)).is_none(), "早于截止日删除");
    }

    #[test]
    fn backfill_skips_today_and_sets_marker() {
        let s = store();
        // 昨天两条（其一 u1）+ 今天一条（经记账路径进今天聚合）
        for uid in [Some("u1"), None] {
            let mut l = RequestLog::new();
            l.created_at = yesterday_ts();
            l.user_id = uid.map(|u| u.to_string());
            l.model = "gpt-4".to_string();
            s.requests.add(l).unwrap();
        }
        let mut today_log = RequestLog::new();
        today_log.model = "gpt-4".to_string();
        s.record_request(today_log);

        let report = s.backfill_history(5000).unwrap();
        assert_eq!(report.scanned, 2, "今天行不参与回填");
        assert_eq!(report.days_written, 1);
        assert_eq!(report.user_rows_written, 1);
        assert!(s.daily.agg_available(), "回填后 marker 落位");

        let yday = daily_agg::date_key(yesterday_ts());
        let agg = s.daily.load_day(&yday).expect("昨日主档");
        assert_eq!(agg.requests, 2);
        // 今天的行 = 运行期记账（requests=1）；回填若误覆盖今天会变 0
        let today = s.daily.load_day(&daily_agg::today_key()).expect("今天主档");
        assert_eq!(today.requests, 1, "回填不得触碰今天的行");
        let urows = s.daily.load_user_range("u1", &yday, &yday);
        assert_eq!(urows.len(), 1);
        assert_eq!(urows[0].count, 1);
    }

    #[test]
    fn backfill_shrink_guard_and_idempotent() {
        let s = store();
        let yday = daily_agg::date_key(yesterday_ts());
        // 先塞一条「虚大」的既有行（模拟明细被裁后重算偏小的场景）
        s.daily
            .overwrite_day(&daily_agg::DailyAgg {
                date: yday.clone(),
                requests: 999,
                ..Default::default()
            })
            .unwrap();
        let mut l = RequestLog::new();
        l.created_at = yesterday_ts();
        l.model = "gpt-4".to_string();
        s.requests.add(l).unwrap();

        let report = s.backfill_history(5000).unwrap();
        assert_eq!(report.skipped_shrink, 1, "既有行更大 → 拒覆盖");
        assert_eq!(report.days_written, 0);
        assert_eq!(s.daily.load_day(&yday).unwrap().requests, 999, "旧行保留");

        // 幂等：正常回填（无虚大行）两次，结果逐字节一致、无 shrink 拒绝
        let s2 = store();
        for _ in 0..2 {
            let mut l = RequestLog::new();
            l.created_at = yesterday_ts();
            l.user_id = Some("u1".to_string());
            l.model = "gpt-4".to_string();
            s2.requests.add(l).unwrap();
        }
        let r1 = s2.backfill_history(5000).unwrap();
        assert_eq!(r1.skipped_shrink, 0);
        let day_after_1 = serde_json::to_value(s2.daily.load_day(&yday).unwrap()).unwrap();
        let user_after_1 =
            serde_json::to_value(s2.daily.load_user_range("u1", &yday, &yday)[0].clone()).unwrap();
        // marker 已存在时 cron 会跳过；手动重建路径 clear 后再跑等价
        s2.daily.clear_backfill_marker().unwrap();
        let r2 = s2.backfill_history(5000).unwrap();
        assert_eq!(r2.skipped_shrink, 0, "相等不触发守卫（仅严格大于拒）");
        assert_eq!(
            serde_json::to_value(s2.daily.load_day(&yday).unwrap()).unwrap(),
            day_after_1
        );
        assert_eq!(
            serde_json::to_value(s2.daily.load_user_range("u1", &yday, &yday)[0].clone()).unwrap(),
            user_after_1
        );
    }

    /// 「过去第 `days_back` 天的 20:00 UTC」。
    ///
    /// 挑 20:00Z 是有原因的：它在 UTC+8 口径下归**次日**，所以同一瞬间的新旧口径
    /// 必然差一天 —— 迁移测试要的就是这个错位（换成 02:00Z 两种口径同日，测试就
    /// 变成空转）。
    fn ts_at_2000z(days_back: i64) -> i64 {
        let now = chrono::Utc::now().timestamp();
        let day = chrono::DateTime::<chrono::Utc>::from_timestamp(now - days_back * 86_400, 0)
            .unwrap()
            .date_naive();
        day.and_hms_opt(20, 0, 0).unwrap().and_utc().timestamp()
    }

    /// 显式 UTC 日键 —— 复刻「现网旧数据以 GMT+0 落盘」的前提。
    ///
    /// **不能用 `date_key_at(ts, 0)`**：该函数的 `offset == 0` 代表**进程系统本地**
    /// 口径（见 `report_tz_of`），在 UTC+8 的机器上会把「旧键」算成本地(+8)，
    /// 与迁移目标 +8 重合，于是 0→8 的错位前提消失、测试在非 UTC runner 上假红
    /// （这正是本条测试此前在开发者机上报 `old_b(19) != new_a(18)` 的原因）。
    /// 现网旧机器是 GMT+0，用显式 UTC 才在任何宿主机上都是忠实模拟。
    fn utc_date_key(ts: i64) -> String {
        chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
    }

    /// 只落明细、不产生运行期聚合行（`add` 不经 `record_request` 漏斗）。
    fn add_log_at(s: &LogStore, ts: i64, uid: &str) {
        let mut l = RequestLog::new();
        l.created_at = ts;
        l.user_id = Some(uid.to_string());
        l.model = "gpt-4".to_string();
        s.requests.add(l).unwrap();
    }

    /// 预置「旧口径落盘」的主档 + 用户行，requests/count 故意虚大。
    fn seed_old_row(s: &LogStore, date: &str) {
        s.daily
            .overwrite_day(&daily_agg::DailyAgg {
                date: date.to_string(),
                requests: 999,
                ..Default::default()
            })
            .unwrap();
        s.daily
            .overwrite_user_row(
                "u1",
                &daily_agg::UserDailyRow {
                    date: date.to_string(),
                    count: 999,
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// **生产真路径 0→8**（现网机器是 GMT+0，升到缺省 8 就要走这条）。
    ///
    /// 钉四件事：①旧口径键被删干净；②新口径键 = 重算值（999 那条虚大行没活下来
    /// = shrink 守卫确实被「先删后算」绕开，否则它会把旧值原样留着，迁了个寂寞）；
    /// ③今天不在窗口内（这就是不能用 `prune_before("9999-12-31")` 全删的理由：回填
    /// 不管今天，删了就开天窗）；④两个 marker 都落到新状态。
    #[test]
    fn tz_migrate_zero_to_eight_rewrites_old_keys() {
        let s = store();
        let t_a = ts_at_2000z(3);
        let t_b = ts_at_2000z(2);
        add_log_at(&s, t_a, "u1");
        add_log_at(&s, t_b, "u1");
        let old_a = utc_date_key(t_a);
        let old_b = utc_date_key(t_b);
        let new_a = daily_agg::date_key_at(t_a, 8);
        let new_b = daily_agg::date_key_at(t_b, 8);
        // 错位前提本身也要钉住（否则整条测试退化成空转）。
        // 旧口径取**显式 UTC**（复刻现网 GMT+0 落盘），与迁移目标 +8 在 20:00Z
        // 必然差一天——与宿主机的本地时区无关（早先误用 `date_key_at(t, 0)`
        // 会把 offset 0 解成本地口径，在 UTC+8 机器上与目标重合而假红）。
        assert_eq!(old_b, new_a, "20:00Z 在 +8 归次日 → 键整体前移一天");
        assert_ne!(
            old_a, new_a,
            "0→8 必须真的错位：旧 GMT+0 键须早于 +8 键一天"
        );
        seed_old_row(&s, &old_a);
        seed_old_row(&s, &old_b);
        let today = daily_agg::today_key();
        s.daily
            .overwrite_day(&daily_agg::DailyAgg {
                date: today.clone(),
                requests: 7,
                ..Default::default()
            })
            .unwrap();

        let r = s.tz_migrate(0, 8).unwrap();
        assert!(r.window_applied);
        assert_eq!(r.rows_deleted, 4, "主档 2 + 用户行 2，今天的行不在窗口内");
        assert_eq!(r.effective_from, old_a, "窗口下界 = 最早落盘日");
        assert_eq!(r.window_to, daily_agg::shift_day(&today, 1));
        assert_eq!(r.backfill.scanned, 2);
        assert_eq!(r.backfill.days_written, 2);
        assert_eq!(r.backfill.user_rows_written, 2);
        assert_eq!(
            r.backfill.skipped_shrink, 0,
            "先删后算 → 守卫不该拦（拦了就是没迁动）"
        );
        // 旧键彻底消失
        assert!(s.daily.load_day(&old_a).is_none(), "旧键 {old_a} 不该剩");
        assert!(
            s.daily.load_user_range("u1", &old_a, &old_a).is_empty(),
            "旧键 {old_a} 的用户行不该剩"
        );
        // 新键 = 重算值；new_a 与 old_b 同名，正是「删掉虚大 999 → 写回 1」的现场
        assert_eq!(s.daily.load_day(&new_a).unwrap().requests, 1);
        assert_eq!(s.daily.load_day(&new_b).unwrap().requests, 1);
        assert_eq!(s.daily.load_user_range("u1", &new_b, &new_b)[0].count, 1);
        // 今天没被碰
        assert_eq!(
            s.daily.load_day(&today).unwrap().requests,
            7,
            "窗口上界是昨天，今天归运行期记账"
        );
        assert_eq!(s.daily.tz_marker(), Some(8));
        assert!(s.daily.agg_available(), "重算完要重新闭合读路径闸门");
    }

    /// **幂等**：重复迁移不再删、不再重算（生产上运维多点一次按钮不能把报表抖一遍）。
    #[test]
    fn tz_migrate_is_idempotent_on_second_call() {
        let s = store();
        let t_a = ts_at_2000z(3);
        let t_b = ts_at_2000z(2);
        add_log_at(&s, t_a, "u1");
        add_log_at(&s, t_b, "u1");
        let old_a = daily_agg::date_key_at(t_a, 0);
        let new_a = daily_agg::date_key_at(t_a, 8);
        let new_b = daily_agg::date_key_at(t_b, 8);
        seed_old_row(&s, &old_a);
        seed_old_row(&s, &daily_agg::date_key_at(t_b, 0));
        let today = daily_agg::today_key();
        s.daily
            .overwrite_day(&daily_agg::DailyAgg {
                date: today.clone(),
                requests: 7,
                ..Default::default()
            })
            .unwrap();

        let first = s.tz_migrate(0, 8).unwrap();
        assert!(first.window_applied);
        assert_eq!(first.rows_deleted, 4);
        let after_first = serde_json::to_value(s.daily.load_day(&new_b).unwrap()).unwrap();
        let user_after =
            serde_json::to_value(s.daily.load_user_range("u1", &new_b, &new_b)[0].clone()).unwrap();

        let second = s.tz_migrate(0, 8).unwrap();
        assert!(!second.window_applied, "第二次不该再删窗口");
        assert_eq!(second.rows_deleted, 0);
        assert_eq!(second.backfill.scanned, 0, "幂等短路：不该再跑一遍全量回填");
        assert_eq!(second.backfill.days_written, 0);
        assert_eq!(second.backfill.user_rows_written, 0);
        // old_a 已在首轮删掉 → 最早落盘日前移到 new_a
        assert_eq!(second.effective_from, new_a);
        // 数据逐字节不变（没有二次改写、也没有二次搬运 cap）
        assert_eq!(
            serde_json::to_value(s.daily.load_day(&new_b).unwrap()).unwrap(),
            after_first
        );
        assert_eq!(
            serde_json::to_value(s.daily.load_user_range("u1", &new_b, &new_b)[0].clone()).unwrap(),
            user_after
        );
        assert_eq!(s.daily.load_day(&today).unwrap().requests, 7);
        assert_eq!(s.daily.tz_marker(), Some(8));
    }

    /// 空库（新装/明细已全清）：不删不算，只把两个 marker 落位。
    #[test]
    fn tz_migrate_without_data_only_lands_markers() {
        let s = store();
        let r = s.tz_migrate(0, 8).unwrap();
        assert!(!r.window_applied);
        assert_eq!(r.rows_deleted, 0);
        assert_eq!(
            r.effective_from,
            daily_agg::today_key(),
            "空库 → 生效日即今天"
        );
        assert_eq!(r.backfill.scanned, 0);
        assert_eq!(r.backfill.days_written, 0);
        assert!(
            s.daily.agg_available(),
            "无历史可回填 = 回填完成，链条照样闭合（与 backfill_history 同语义）"
        );
        assert_eq!(s.daily.tz_marker(), Some(8));
    }

    /// 顺序坑：目标偏移必须已是生效口径，否则重启后配置回退、重算数据反而与运行期
    /// 不一致。校验必须**先于任何写入**（失败时不留半个 marker）。
    #[test]
    fn tz_migrate_requires_target_offset_to_be_live() {
        let s = store();
        let cur = crate::config::report_tz_offset();
        let err = s.tz_migrate(0, cur + 1).unwrap_err().to_string();
        assert!(
            err.contains("报表设置"),
            "错序调用要指路「先存设置」: {err}"
        );
        assert_eq!(s.daily.tz_marker(), None, "校验失败不得留下任何写入");
        assert!(!s.daily.agg_available());
    }

    /// `delete_day_range` 含两端、区间外不动、反向区间空转。
    #[test]
    fn delete_day_range_covers_both_ends() {
        let s = store();
        let (d1, d2, d3) = (
            daily_agg::day_key_back(1),
            daily_agg::day_key_back(2),
            daily_agg::day_key_back(3),
        );
        for d in [&d1, &d2, &d3] {
            seed_old_row(&s, d);
        }
        // from > to → 直接 0，不扫库
        assert_eq!(
            s.daily
                .delete_day_range("2026-01-02", "2026-01-01")
                .unwrap(),
            0
        );
        // 删 [d3, d2]：两端都在内，d1（昨天）保留
        let removed = s.daily.delete_day_range(&d3, &d2).unwrap();
        assert_eq!(removed, 4, "主档 2 + 用户行 2");
        assert!(s.daily.load_day(&d3).is_none(), "下界含端点");
        assert!(s.daily.load_day(&d2).is_none(), "上界含端点");
        assert!(s.daily.load_day(&d1).is_some(), "窗口外不动");
    }

    #[test]
    fn filter_by_model() {
        let s = store();
        for m in ["gpt-4", "gpt-4", "claude-3"] {
            let mut log = RequestLog::new();
            log.model = m.to_string();
            s.requests.add(log).unwrap();
        }
        let (gpt4, total) =
            s.requests
                .list_with_filter(None, Some("gpt-4"), None, None, None, 1, 10);
        assert_eq!(total, 2);
        assert_eq!(gpt4.len(), 2);
    }

    #[test]
    fn audit_log() {
        let s = store();
        s.record_audit(
            "admin-id",
            "create",
            "user:abc",
            None,
            Some(serde_json::json!({"email": "a@b.c"})),
        );
        let all = s.audits.list();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].action, "create");
        assert!(all[0].after.is_some());
    }

    /// P0：带 IP 的审计入口必须把 IP 写进记录；不带 IP 的旧入口保持 None。
    #[test]
    fn record_audit_with_ip_sets_ip_field() {
        let s = store();
        s.record_audit_with_ip(
            "admin-id",
            "notify.webhook_ssrf_rejected",
            "notify:webhook_url",
            Some("203.0.113.9".to_string()),
            None,
            None,
        );
        s.record_audit("admin-id", "create", "user:abc", None, None);
        let all = s.audits.list();
        assert_eq!(all.len(), 2);
        let with_ip = all
            .iter()
            .find(|a| a.action == "notify.webhook_ssrf_rejected")
            .expect("SSRF 拒绝审计应存在");
        assert_eq!(with_ip.ip.as_deref(), Some("203.0.113.9"));
        let without = all.iter().find(|a| a.action == "create").unwrap();
        assert_eq!(without.ip, None, "旧入口不带 IP");
    }

    /// P0 防篡改：删除动作本身必须留一条自审计，且被删目标确实消失。
    #[test]
    fn audit_delete_records_self_audit() {
        let s = store();
        // AuditLog::new 第三参是 target，id 由 add() 生成——必须按返回值断言
        let keep = s
            .audits
            .add(AuditLog::new("admin-x", "seed", "keep-1"))
            .unwrap();
        let doom = s
            .audits
            .add(AuditLog::new("admin-x", "seed", "doom-1"))
            .unwrap();
        s.audits.delete_by_id(&doom.id).unwrap();

        let all = s.audits.list();
        let ids: Vec<&str> = all.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&keep.id.as_str()), "未删目标保留：{ids:?}");
        assert!(!ids.contains(&doom.id.as_str()), "被删目标消失：{ids:?}");
        let self_audit = all
            .iter()
            .find(|a| a.action == "audit_log.delete")
            .expect("删除必须留自审计");
        assert_eq!(self_audit.target, format!("id:{}", doom.id));
        assert!(
            self_audit
                .before
                .as_deref()
                .unwrap_or_default()
                .contains(doom.id.as_str()),
            "自审计需记被删 ID"
        );
    }

    /// 删除不存在的 ID 不产生自审计（没有删除动作就没有留痕）。
    #[test]
    fn audit_delete_missing_id_no_self_audit() {
        let s = store();
        s.audits
            .add(AuditLog::new("admin-x", "seed", "keep-1"))
            .unwrap();
        s.audits.delete_by_id("nope").unwrap();
        let all = s.audits.list();
        assert_eq!(all.len(), 1, "空删除不留痕");
        assert!(all.iter().all(|a| a.action != "audit_log.delete"));
    }

    /// P0 防篡改：清空后必须存活恰好一条「清空留痕」，且记录被清条数。
    #[test]
    fn audit_clear_records_self_audit_surviving_wipe() {
        let s = store();
        for id in ["a-1", "a-2", "a-3"] {
            s.audits.add(AuditLog::new("admin-x", "seed", id)).unwrap();
        }
        let removed = s.audits.clear_all().unwrap();
        assert_eq!(removed, 3);

        let all = s.audits.list();
        assert_eq!(
            all.len(),
            1,
            "清空后应只剩清空留痕（写在清空之后 ⇒ 不被自己抹掉）：{all:?}"
        );
        assert_eq!(all[0].action, "audit_log.clear");
        assert!(
            all[0]
                .before
                .as_deref()
                .unwrap_or_default()
                .contains("\"removed\":3"),
            "清空留痕需记条数"
        );
    }

    /// 自审计记录本身也可被删除，但删除它又会留下新的删除留痕（链式证据）。
    #[test]
    fn audit_self_audit_deletion_spawns_new_evidence() {
        let s = store();
        let victim = s
            .audits
            .add(AuditLog::new("admin-x", "seed", "victim"))
            .unwrap();
        s.audits.delete_by_id(&victim.id).unwrap();
        let first_self = s
            .audits
            .list()
            .into_iter()
            .find(|a| a.action == "audit_log.delete")
            .unwrap();
        // 抹掉这条自审计 ⇒ 又产生一条新自审计，且仍指向原始删除目标
        s.audits.delete_by_id(&first_self.id).unwrap();
        let selfs: Vec<_> = s
            .audits
            .list()
            .into_iter()
            .filter(|a| a.action == "audit_log.delete")
            .collect();
        assert_eq!(selfs.len(), 1, "抹掉留痕必留新留痕");
        assert_ne!(selfs[0].id, first_self.id);
        assert_eq!(
            selfs[0].target,
            format!("id:{}", victim.id),
            "新留痕仍记原始删除目标"
        );
    }

    #[test]
    fn pagination() {
        let s = store();
        for i in 0..15 {
            let mut log = RequestLog::new();
            log.model = format!("m{i}");
            s.requests.add(log).unwrap();
        }
        let (page1, total) = s
            .requests
            .list_with_filter(None, None, None, None, None, 1, 10);
        assert_eq!(total, 15);
        assert_eq!(page1.len(), 10);
        // P0 快路径：键倒序 LIMIT 取前 page*size 条后内存切片，语义与原全量切片一致
        let (page2, _) = s
            .requests
            .list_with_filter(None, None, None, None, None, 2, 10);
        assert_eq!(page2.len(), 5);
        let (page3, _) = s
            .requests
            .list_with_filter(None, None, None, None, None, 3, 10);
        assert_eq!(page3.len(), 0);
    }

    // ── 对话快照 ────────────────────────────────────────────────────

    /// system 提示只记长度，正文不入库（常含站方内部指令）。
    #[test]
    fn conversation_hides_system_prompt_body() {
        let msgs = vec![
            serde_json::json!({"role":"system","content":"内部指令：不要说你是AI"}),
            serde_json::json!({"role":"user","content":"你好"}),
        ];
        let conv = LogConversation::from_messages(&msgs);
        assert_eq!(
            conv.system_prompt_chars,
            "内部指令：不要说你是AI".chars().count()
        );
        assert_eq!(conv.messages.len(), 1, "系统提示不应作为消息出现");
        assert_eq!(conv.messages[0].role, "user");
        assert_eq!(conv.messages[0].text, "你好");
    }

    /// 多模态内容块：文本拼接，图片/音频只留占位（不存 base64）。
    #[test]
    fn conversation_flattens_multimodal_blocks() {
        let msgs = vec![serde_json::json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "描述这张图"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                {"type": "audio", "input_audio": {"data": "BBBB"}}
            ]
        })];
        let conv = LogConversation::from_messages(&msgs);
        assert_eq!(conv.messages[0].text, "描述这张图[image][audio]");
        assert!(!conv.messages[0].text.contains("base64"));
    }

    /// 超出条数上限时丢最早的并标记 truncated_history。
    #[test]
    fn conversation_caps_history() {
        let msgs: Vec<_> = (0..40)
            .map(|i| serde_json::json!({"role":"user","content":format!("m{i}")}))
            .collect();
        let conv = LogConversation::from_messages(&msgs);
        assert_eq!(conv.messages.len(), LogConversation::MAX_MESSAGES);
        assert!(conv.truncated_history);
        // 保留的是最后 N 条
        assert_eq!(conv.messages.last().unwrap().text, format!("m{}", 39));
    }

    /// 单条超长消息按字符截断并标记（不切断 UTF-8 多字节字符）。
    #[test]
    fn conversation_truncates_long_message() {
        let long = "中".repeat(LogConversation::MAX_MESSAGE_CHARS + 100);
        let msgs = vec![serde_json::json!({"role":"user","content": long})];
        let conv = LogConversation::from_messages(&msgs);
        assert!(conv.messages[0].truncated);
        assert_eq!(
            conv.messages[0].text.chars().count(),
            LogConversation::MAX_MESSAGE_CHARS
        );
    }

    /// 助手回复追加后同样受条数上限约束。
    #[test]
    fn push_assistant_respects_cap() {
        let msgs: Vec<_> = (0..LogConversation::MAX_MESSAGES)
            .map(|i| serde_json::json!({"role":"user","content":format!("m{i}")}))
            .collect();
        let mut conv = LogConversation::from_messages(&msgs);
        conv.push_assistant("回复");
        assert_eq!(conv.messages.len(), LogConversation::MAX_MESSAGES);
        assert!(conv.truncated_history);
        assert_eq!(conv.messages.last().unwrap().role, "assistant");
    }

    /// 空回复不追加（避免产生一条空气泡）。
    #[test]
    fn push_assistant_skips_blank() {
        let mut conv = LogConversation::default();
        conv.push_assistant("   ");
        assert!(conv.is_empty());
    }

    // ── 导出权限剥离 ────────────────────────────────────────────────

    /// 非 admin 导出 JSON 必须剥离 conversation/debug/路由回放字段，
    /// 与列表页 `logs_with_user_email` 的剥离口径一致 —— 导出与列表页
    /// 不应给出不同权限边界（曾存在漏洞：导出走 store 全量序列化，
    /// 普通用户调 `?format=json` 即可拿到对话正文与站方内部拓扑）。
    #[test]
    fn export_json_strips_sensitive_for_non_admin() {
        let s = store();
        let mut log = RequestLog::new();
        log.user_id = Some("u1".to_string());
        log.model = "gpt-4".to_string();
        log.candidate_channels = vec!["ch-a".to_string(), "ch-b".to_string()];
        log.filtered_channels = vec![FilteredChannel {
            channel_id: "ch-b".to_string(),
            reason: "circuit_open".to_string(),
        }];
        log.selected_channel = Some("ch-a".to_string());
        log.conversation = Some(LogConversation {
            messages: vec![LogMessage {
                role: "user".to_string(),
                text: "秘密对话".to_string(),
                truncated: false,
            }],
            system_prompt_chars: 0,
            truncated_history: false,
        });
        s.requests.add(log).unwrap();

        let non_admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, false);
        // 非管理员导出不含任何敏感字段
        assert!(
            !non_admin.contains("conversation"),
            "对话正文不应泄露给非管理员"
        );
        assert!(
            !non_admin.contains("candidate_channels"),
            "候选渠道拓扑不应泄露给非管理员"
        );
        assert!(
            !non_admin.contains("filtered_channels"),
            "过滤原因不应泄露给非管理员"
        );
        assert!(
            !non_admin.contains("selected_channel"),
            "选中渠道不应泄露给非管理员"
        );
        assert!(!non_admin.contains("秘密对话"));

        let admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, true);
        // 管理员导出保留全部字段
        assert!(admin.contains("conversation"));
        assert!(admin.contains("candidate_channels"));
        assert!(admin.contains("秘密对话"));
    }

    /// 破甲控制台使用标记（`purge_applied`）仅管理员可见：非管理员 JSON 导出
    /// 必须剥离，管理员保留 —— 与列表页 `logs_with_user_email` 同一权限边界。
    #[test]
    fn export_json_hides_purge_applied_for_non_admin() {
        let s = store();
        let mut log = RequestLog::new();
        log.user_id = Some("u1".to_string());
        log.model = "gpt-4".to_string();
        log.purge_applied = true;
        s.requests.add(log).unwrap();

        let non_admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, false);
        assert!(
            !non_admin.contains("purge_applied"),
            "破甲使用标记不应泄露给非管理员: {non_admin}"
        );

        let admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, true);
        assert!(
            admin.contains("\"purge_applied\": true") || admin.contains("\"purge_applied\":true"),
            "管理员导出应保留破甲使用标记: {admin}"
        );
    }

    /// 需求6：非 admin 导出必须把 `model`（上游实际模型名）重写为请求侧原名，
    /// 且不残留 `origin_model`；admin 两个字段都保留（排障用）。
    #[test]
    fn export_json_hides_upstream_model_for_non_admin() {
        let s = store();
        let mut log = RequestLog::new();
        log.user_id = Some("u1".to_string());
        log.model = "glm-4".to_string(); // 映射后的上游实际模型
        log.origin_model = Some("gpt-4o".to_string()); // 用户请求名
        s.requests.add(log).unwrap();

        let non_admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, false);
        assert!(
            non_admin.contains("\"model\": \"gpt-4o\""),
            "非 admin 应看到请求侧模型名：{non_admin}"
        );
        assert!(
            !non_admin.contains("glm-4"),
            "非 admin 不得看到上游实际模型名：{non_admin}"
        );
        assert!(
            !non_admin.contains("origin_model"),
            "非 admin 响应不应残留 origin_model：{non_admin}"
        );

        let admin = s
            .requests
            .export_json_filtered(Some("u1"), None, None, None, None, true);
        assert!(
            admin.contains("\"model\": \"glm-4\""),
            "管理员应看到上游实际模型：{admin}"
        );
        assert!(
            admin.contains("\"origin_model\": \"gpt-4o\""),
            "管理员应看到请求原名：{admin}"
        );
    }

    /// 需求6：`redact_model_for_non_admin` 是列表（`logs.rs::logs_with_user_email`）
    /// 与导出共用的遮罩核心。断言：映射改名时重写 `model` + 移除 `origin_model`；
    /// 未映射（`origin_model` 为 null）时 `model` 原样保留，`origin_model` 一并移除。
    ///
    /// 注：列表用的 `logs_with_user_email` 依赖 `AppState`（40+ Arc 字段，无测试
    /// 构造器），故其遮罩逻辑由本纯函数单测覆盖，导出路径另在下面端到端断言。
    #[test]
    fn redact_model_rewrites_only_when_mapped() {
        // 映射改名：model=上游、origin_model=请求名 ⇒ 重写 + 移除
        // 渠道归属（结构体注释「admin 排查用，用户不展示」）一并移除。
        let mut mapped = serde_json::json!({
            "model": "glm-4",
            "origin_model": "gpt-4o",
            "channel_id": "ch-abc",
            "channel_name": "天翼云"
        });
        redact_model_for_non_admin(mapped.as_object_mut().unwrap());
        let got = mapped.as_object().unwrap();
        assert_eq!(got.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
        assert!(!got.contains_key("origin_model"));
        assert!(!got.contains_key("channel_id"), "非 admin 不得看渠道 ID");
        assert!(!got.contains_key("channel_name"), "非 admin 不得看渠道名");

        // 未映射：origin_model=null ⇒ model 原样（本就是请求名），origin_model 移除
        let mut unmapped = serde_json::json!({
            "model": "gpt-4o",
            "origin_model": null,
            "channel_id": "ch-abc",
            "channel_name": "天翼云"
        });
        redact_model_for_non_admin(unmapped.as_object_mut().unwrap());
        let got2 = unmapped.as_object().unwrap();
        assert_eq!(got2.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
        assert!(!got2.contains_key("origin_model"));
        assert!(!got2.contains_key("channel_id"));
        assert!(!got2.contains_key("channel_name"));
    }

    /// 需求6：CSV 导出同口径 —— 非 admin 的 `model` 列 = 请求名、`origin_model`
    /// 行置空；admin 两列原样。
    #[test]
    fn export_csv_hides_upstream_model_for_non_admin() {
        let s = store();
        let mut log = RequestLog::new();
        log.user_id = Some("u1".to_string());
        log.model = "glm-4".to_string();
        log.origin_model = Some("gpt-4o".to_string());
        log.channel_id = Some("ch-abc".to_string());
        log.channel_name = Some("天翼云".to_string());
        s.requests.add(log).unwrap();

        let csv = s
            .requests
            .export_csv_filtered(Some("u1"), None, None, None, None, false);
        // 列头形状不变（脚本仍可解析），只是 model 列值换成请求名
        assert!(csv.contains("model,origin_model"), "列头应保持：{csv}");
        assert!(csv.contains("gpt-4o"), "非 admin CSV 应含请求名：{csv}");
        assert!(
            !csv.contains("glm-4"),
            "非 admin CSV 不得含上游实际模型：{csv}"
        );
        // 渠道归属同为上游拓扑：非 admin 两列置空
        assert!(
            !csv.contains("ch-abc") && !csv.contains("天翼云"),
            "非 admin CSV 不得含渠道归属：{csv}"
        );

        let admin_csv = s
            .requests
            .export_csv_filtered(Some("u1"), None, None, None, None, true);
        assert!(
            admin_csv.contains("glm-4"),
            "管理员 CSV 应含上游实际模型：{admin_csv}"
        );
        assert!(
            admin_csv.contains("gpt-4o"),
            "管理员 CSV 应含请求原名：{admin_csv}"
        );
        assert!(
            admin_csv.contains("ch-abc") && admin_csv.contains("天翼云"),
            "管理员 CSV 应含渠道归属：{admin_csv}"
        );
    }
}

#[cfg(test)]
mod attempt_detail_tests {
    use super::*;

    fn detail(index: u32) -> AttemptDetail {
        AttemptDetail {
            index,
            channel_id: format!("ch-{}", index % 3),
            channel_name: format!("渠道 {}", index % 3),
            provider: "openai_compatible".to_string(),
            account: String::new(),
            model: "gpt-test".to_string(),
            sticky_hit: index == 1,
            started_at: 1_700_000_000,
            finished_at: Some(1_700_000_001),
            status: if index.is_multiple_of(2) {
                Some(502)
            } else {
                Some(200)
            },
            error: index.is_multiple_of(2).then(|| "upstream boom".to_string()),
            notice: None,
        }
    }

    /// #32：明细行数有界（ROUTE_REPLAY_MAX_ATTEMPTS），超出转计数不静默丢。
    #[test]
    fn attempt_capture_is_bounded() {
        // 走再导出路径：`mod resolve` 是私有的，`resolve::` 路径在 test target 不可见
        let mut capture = crate::api::openai::RouteCapture::default();
        for i in 1..=ROUTE_REPLAY_MAX_ATTEMPTS as u32 + 10 {
            capture.push_attempt(detail(i));
        }
        assert_eq!(capture.attempts.len(), ROUTE_REPLAY_MAX_ATTEMPTS);
        assert_eq!(capture.attempts_truncated, 10);
        // 错误摘要截断（300 字符）
        assert!(capture.attempts.iter().all(|d| {
            d.error
                .as_ref()
                .map(|e| e.chars().count() <= 300)
                .unwrap_or(true)
        }));
        // 序号口径：前 64 条是 1..=64（与 attempts 计数同口径、不跳号）
        assert_eq!(capture.attempts.first().map(|d| d.index), Some(1));
        assert_eq!(
            capture.attempts.last().map(|d| d.index),
            Some(ROUTE_REPLAY_MAX_ATTEMPTS as u32)
        );
    }

    /// #32：开关关闭（字段空/零）时不序列化——serde skip 判定。
    #[test]
    fn attempt_fields_skipped_when_off() {
        let log = RequestLog::new();
        let json = serde_json::to_string(&log).unwrap();
        assert!(!json.contains("attempt_details"));
        assert!(!json.contains("attempt_details_truncated"));
        // 开启（有数据）时序列化
        let mut log = RequestLog::new();
        log.attempt_details = vec![detail(1)];
        let json = serde_json::to_string(&log).unwrap();
        assert!(json.contains("attempt_details"));
        assert!(json.contains("stickyHit"));
    }
}
