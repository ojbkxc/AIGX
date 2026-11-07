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
    /// 原因码：`disabled` / `provider_not_allowed` / `expired` / `model_mismatch` /
    /// `cooling` / `balance_low` / `concurrency` / `limiter_skip` / `limiter_disable`
    ///（与 SkipReasons 档位一一对应）
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
            request_id: None,
            created_at: chrono::Utc::now().timestamp(),
            candidate_channels: Vec::new(),
            filtered_channels: Vec::new(),
            selected_channel: None,
            cache_hit: false,
            debug: None,
            conversation: None,
            first_response_ms: None,
            attempts: 1,
            sensitive_hits: Vec::new(),
            phase: String::new(),
            route_config: None,
            account_skips: Vec::new(),
            route_truncated: 0,
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

impl RequestLogStore {
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
    /// 单用户日志量可控，全量载入内存再过滤可接受。
    pub fn list_by_user(
        &self,
        user_id: &str,
        start: Option<i64>,
        end: Option<i64>,
    ) -> Vec<RequestLog> {
        self.list_all()
            .into_iter()
            .filter(|l| {
                l.user_id.as_deref() == Some(user_id)
                    && start.is_none_or(|s| l.created_at >= s)
                    && end.is_none_or(|e| l.created_at <= e)
            })
            .collect()
    }

    /// Phase1-B 路由回放：按 request_id（或日志 id）取单条请求日志。
    ///
    /// 管理面冷路径：线性扫保留窗口内的日志（有 retention 上限），不做
    /// 额外索引；命中最新一条即返回。
    pub fn get_by_request_id(&self, request_id: &str) -> Option<RequestLog> {
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

    /// 清空全部请求日志，返回删除条数
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("reqlog:")?;
        let n = keys.len();
        for k in &keys {
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
            let total = match self.store.list("reqlog:") {
                Ok(keys) => keys.len(),
                Err(_) => 0,
            };
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
    pub fn export_json(&self) -> String {
        self.export_json_filtered(None, None, None, None, None, true)
    }

    /// 导出按 user_id 过滤后的 JSON（普通用户导出自己的记录，剥离敏感字段）
    pub fn export_json_for_user(&self, user_id: &str) -> String {
        self.export_json_filtered(Some(user_id), None, None, None, None, false)
    }

    /// 导出按完整筛选条件过滤后的 JSON（与列表页筛选语义一致）。
    ///
    /// `admin` = false 时按非管理员视角剥离 `debug` / `conversation` /
    /// `candidate_channels` / `filtered_channels` / `selected_channel`，
    /// 与 `logs.rs::logs_with_user_email` 的剥离口径一致 —— 导出与列表页
    /// 不应给出不同权限边界。
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
                    }
                }
                v
            })
            .collect();
        serde_json::to_string_pretty(&filtered).unwrap_or_else(|_| "[]".to_string())
    }

    /// 单条日志是否匹配筛选条件（列表与导出共用，保证语义一致）
    #[allow(clippy::too_many_arguments)]
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

    /// 导出为 CSV 字符串
    pub fn export_csv(&self) -> String {
        self.export_csv_filtered(None, None, None, None, None)
    }

    /// 导出按 user_id 过滤后的 CSV（普通用户导出自己的记录）
    pub fn export_csv_for_user(&self, user_id: &str) -> String {
        self.export_csv_filtered(Some(user_id), None, None, None, None)
    }

    /// 导出按完整筛选条件过滤后的 CSV（与列表页筛选语义一致）
    #[allow(clippy::too_many_arguments)]
    pub fn export_csv_filtered(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
    ) -> String {
        let all = self.list_all();
        let mut buf = String::from(
            "id,created_at,user_id,key_id,channel_id,channel_name,model,origin_model,input_tokens,output_tokens,cost,channel_cost,latency_ms,status_code,error_msg,ip\n",
        );
        for l in all
            .iter()
            .filter(|l| Self::matches(l, user_id, model, channel_id, start, end))
        {
            buf.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&l.id),
                csv_escape(&l.created_at.to_string()),
                csv_escape(l.user_id.as_deref().unwrap_or("")),
                csv_escape(l.key_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_name.as_deref().unwrap_or("")),
                csv_escape(&l.model),
                csv_escape(l.origin_model.as_deref().unwrap_or("")),
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
/// key 中 created_at 为数字字符串，字典序与数值序不一致
///（如 "999" > "1000"），需 parse 后按数值排序再取最旧。
///
/// 容量上限优先读运维保留配置（LogRetention 页设置），未配置时
/// 回退默认 10 万——原先硬编码 MAX_LOGS，管理员配置的容量被静默忽略。
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
    let keys = store.list("reqlog:")?;
    if keys.len() < max_logs {
        return Ok(());
    }
    let mut timed: Vec<(i64, String)> = keys
        .into_iter()
        .map(|k| {
            let ts = k
                .strip_prefix("reqlog:")
                .and_then(|rest| rest.split(':').next())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(i64::MAX);
            (ts, k)
        })
        .collect();
    timed.sort_by_key(|(ts, _)| *ts);
    let excess =
        (timed.len().saturating_sub(max_logs) + RequestLogStore::PURGE_BATCH).min(timed.len());
    for (_, k) in timed.into_iter().take(excess) {
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

    /// 按 ID 删除单条审计日志
    pub fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let keys = self.store.list("auditlog:")?;
        let target = keys.into_iter().find(|k| k.ends_with(&format!(":{id}")));
        if let Some(k) = target {
            self.store.delete(&k)?;
        }
        Ok(())
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

    /// 批量删除审计日志
    pub fn delete_many(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.delete_by_id(id).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 清空全部审计日志
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("auditlog:")?;
        let n = keys.len();
        for k in &keys {
            if let Err(e) = self.store.delete(k) {
                tracing::warn!("clear audit log failed for {k}: {e}");
            }
        }
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
        let mut entry = AuditLog::new(admin_id, action, target);
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

    #[test]
    fn daily_prune_boundary_keeps_today_and_cutoff_day() {
        let s = store();
        let day = |back: i64| {
            (chrono::Utc::now().naive_utc().date() - chrono::Duration::days(back))
                .format("%Y-%m-%d")
                .to_string()
        };
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
}
