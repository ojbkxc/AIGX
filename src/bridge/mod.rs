//! Bridge 模式 — 提供商适配器 trait。
//!
//! 参考 aisix 项目的 Bridge 架构，将每个 AI 提供商封装为统一的 Bridge trait，
//! 通过 Hub 进行分发调度。当前主要实现 Cloudflare Workers AI 的适配器。
//!
//! 每个 Bridge 实现负责：
//! - 将归一化的 ChatFormat 转换为上游请求体
//! - 执行 HTTP 调用（认证、超时、传输层重试）
//! - 对于流式请求，产生 `Stream<Item = ChatChunk>`
//! - 对于非流式请求，产生完整的 ChatResponse
//! - 将错误映射为类型化的 BridgeError 变体

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::time::Duration;

pub mod cf;

/// 聊天消息角色
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 归一化的聊天消息，参考 aisix ChatMessage 设计
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: Option<String>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            name: None,
            tool_call_id: None,
        }
    }

    /// 获取消息内容字符串
    pub fn content_str(&self) -> &str {
        self.content.as_deref().unwrap_or("")
    }
}

/// 归一化的聊天请求格式
#[derive(Debug, Clone)]
pub struct ChatFormat {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stream: bool,
}

/// 聊天完成原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ContentFilter,
    ToolCalls,
}

/// 聊天响应
#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub message: ChatMessage,
    pub finish_reason: FinishReason,
    pub usage: UsageStats,
}

/// 用量统计
#[derive(Debug, Clone, Copy, Default)]
pub struct UsageStats {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl UsageStats {
    pub fn new(prompt: u64, completion: u64) -> Self {
        Self {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
        }
    }
}

/// 流式聊天块
#[derive(Debug, Clone)]
pub struct ChatChunk {
    pub id: String,
    pub model: String,
    pub delta: ChatDelta,
    pub finish_reason: Option<FinishReason>,
}

/// 聊天增量
#[derive(Debug, Clone)]
pub struct ChatDelta {
    pub content: Option<String>,
}

/// 流式聊天块流
pub type ChatChunkStream = BoxStream<'static, Result<ChatChunk, BridgeError>>;

/// 嵌入请求
#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub model: String,
    pub input: Vec<String>,
}

/// 嵌入响应
#[derive(Debug, Clone)]
pub struct EmbeddingResponse {
    pub data: Vec<EmbeddingObject>,
    pub usage: EmbeddingUsage,
}

/// 嵌入对象
#[derive(Debug, Clone)]
pub struct EmbeddingObject {
    pub index: usize,
    pub embedding: Vec<f64>,
}

/// 嵌入用量
#[derive(Debug, Clone, Default)]
pub struct EmbeddingUsage {
    pub prompt_tokens: u64,
    pub total_tokens: u64,
}

/// 桥接上下文，参考 aisix BridgeContext 设计
#[derive(Debug, Clone)]
pub struct BridgeContext {
    pub request_id: String,
    pub model: String,
    pub account_id: Option<String>,
    pub api_token: Option<String>,
    pub deadline: Option<Duration>,
}

impl BridgeContext {
    pub fn new(request_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            model: model.into(),
            account_id: None,
            api_token: None,
            deadline: None,
        }
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = Some(deadline);
        self
    }
}

/// 上游主体错误信息的结构化视图，参考 aisix UpstreamErrorView 设计
#[derive(Debug, Clone, Default)]
pub struct UpstreamErrorView {
    pub kind: Option<String>,
    pub message: Option<String>,
    pub code: Option<String>,
    pub param: Option<String>,
}

/// 上游响应使用的传输协议格式，参考 aisix UpstreamWire 设计
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamWire {
    /// OpenAI 兼容协议：`{error:{message,type,code,param}}`
    OpenAI,
    /// Anthropic 协议：`{type:"error",error:{type,message}}`
    Anthropic,
    /// Azure OpenAI：OpenAI-like 但 content policy 有 inner_error 特例
    AzureOpenAI,
    /// AWS Bedrock 结构化错误，`kind` 携带 AWS 异常码（如 `ThrottlingException`）
    Bedrock,
    /// Vertex AI：`{error:{code:int,message,status}}`，`status` 为 gRPC 规范码字符串
    Vertex,
    /// 未知 / 不适用（测试，合成错误）
    Unknown,
}

/// 桥接错误，参考 aisix BridgeError 设计
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("upstream request timed out after {elapsed_ms}ms")]
    Timeout { elapsed_ms: u64, cause: String },

    #[error("upstream returned HTTP {status}: {message}")]
    UpstreamStatus {
        status: u16,
        message: String,
        parsed: Option<Box<UpstreamErrorView>>,
        wire: UpstreamWire,
        retry_after: Option<Duration>,
    },

    #[error("upstream returned an unparseable body: {0}")]
    UpstreamDecode(String),

    #[error("bridge is misconfigured: {0}")]
    Config(String),

    #[error("invalid upstream configuration: {0}")]
    InvalidUpstreamConfig(String),

    #[error("invalid upstream credentials: {0}")]
    InvalidUpstreamCredentials(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("upstream cancelled the response mid-stream")]
    StreamAborted,

    #[error("authentication error: {0}")]
    AuthError(String),

    #[error("rate limited")]
    RateLimited,

    #[error("model not found: {0}")]
    ModelNotFound(String),

    #[error("all accounts failed: {0}")]
    AllAccountsFailed(String),
}

impl BridgeError {
    /// Convenience constructor for synthesised upstream errors (tests,
    /// cooldown fixtures) where no real upstream envelope is involved.
    /// Sets `UpstreamWire::Unknown` and `parsed: None`.
    pub fn upstream_status(status: u16, message: impl Into<String>) -> Self {
        Self::UpstreamStatus {
            status,
            message: message.into(),
            parsed: None,
            wire: UpstreamWire::Unknown,
            retry_after: None,
        }
    }

    /// Convenience constructor for synthesised upstream errors that
    /// carry a parsed `Retry-After` hint.
    pub fn upstream_status_with_retry_after(
        status: u16,
        message: impl Into<String>,
        retry_after: Option<Duration>,
    ) -> Self {
        Self::UpstreamStatus {
            status,
            message: message.into(),
            parsed: None,
            wire: UpstreamWire::Unknown,
            retry_after,
        }
    }

    /// 稳定的 HTTP 状态映射
    pub fn http_status(&self) -> u16 {
        match self {
            BridgeError::Timeout { .. } => 504,
            BridgeError::UpstreamStatus { status, .. } => {
                if (400..500).contains(status) {
                    *status
                } else {
                    502
                }
            }
            BridgeError::UpstreamDecode(_) => 502,
            BridgeError::Config(_) => 500,
            BridgeError::InvalidUpstreamConfig(_) => 400,
            BridgeError::InvalidUpstreamCredentials(_) => 401,
            BridgeError::Transport(_) => 502,
            BridgeError::StreamAborted => 502,
            BridgeError::AuthError(_) => 401,
            BridgeError::RateLimited => 429,
            BridgeError::ModelNotFound(_) => 404,
            BridgeError::AllAccountsFailed(_) => 503,
        }
    }

    /// 稳定错误类型标识
    pub fn error_type(&self) -> &'static str {
        match self {
            BridgeError::Timeout { .. } => "timeout",
            BridgeError::UpstreamStatus { .. } => "upstream_error",
            BridgeError::UpstreamDecode(_) => "upstream_decode_error",
            BridgeError::Config(_) => "config_error",
            BridgeError::InvalidUpstreamConfig(_) => "invalid_request_error",
            BridgeError::InvalidUpstreamCredentials(_) => "authentication_error",
            BridgeError::Transport(_) => "transport_error",
            BridgeError::StreamAborted => "stream_aborted",
            BridgeError::AuthError(_) => "authentication_error",
            BridgeError::RateLimited => "rate_limit_error",
            BridgeError::ModelNotFound(_) => "model_not_found",
            BridgeError::AllAccountsFailed(_) => "all_accounts_failed",
        }
    }
}

/// 解析 `Retry-After` 响应头为 Duration（RFC 9110 §10.2.3 的秒数形式）
pub fn parse_retry_after(headers: &http::HeaderMap) -> Option<Duration> {
    let raw = headers.get(http::header::RETRY_AFTER)?.to_str().ok()?;
    let seconds: u64 = raw.trim().parse().ok()?;
    Some(Duration::from_secs(seconds))
}

/// 限制读取响应体，至多 `limit` 字节
pub async fn read_body_capped(resp: reqwest::Response, limit: usize) -> bytes::Bytes {
    use futures::StreamExt;
    let mut buf = bytes::BytesMut::with_capacity(limit.min(16 * 1024));
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        if buf.len() >= limit {
            continue;
        }
        let remaining = limit - buf.len();
        let take = chunk.len().min(remaining);
        buf.extend_from_slice(&chunk[..take]);
    }
    buf.freeze()
}

/// Content-Type 是否为 application/json
pub fn content_type_is_json(ct: &str) -> bool {
    let ct = ct.trim_start();
    ct.starts_with("application/json")
}

/// 响应是否为 JSON 类型
pub fn response_is_json(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| content_type_is_json(&ct.to_ascii_lowercase()))
        .unwrap_or(false)
}

/// 从上游错误响应中捕获并解析错误体，返回 BridgeError::UpstreamStatus
///
/// 参考 aisix 的 `capture_upstream_error_http` 设计。
pub async fn capture_upstream_error_http(
    status: http::StatusCode,
    resp: reqwest::Response,
    wire: UpstreamWire,
    parse: impl FnOnce(&[u8]) -> Option<UpstreamErrorView>,
) -> BridgeError {
    let retry_after = parse_retry_after(resp.headers());
    let body = read_body_capped(resp, 64 * 1024).await;
    let parsed = parse(&body).map(|mut v| {
        v.message = v.message.map(|m| truncate_lossy(&m, 1024));
        v.kind = v.kind.map(|k| truncate_lossy(&k, 1024));
        v.code = v.code.map(|c| truncate_lossy(&c, 1024));
        v.param = v.param.map(|p| truncate_lossy(&p, 1024));
        v
    });
    let message = parsed
        .as_ref()
        .and_then(|v| v.message.clone())
        .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
    BridgeError::UpstreamStatus {
        status: status.as_u16(),
        message: truncate_lossy(&message, 1024),
        parsed: parsed.map(Box::new),
        wire,
        retry_after,
    }
}

/// 截断字符串至最多 `max` 字节，仅切在 UTF-8 边界上
fn truncate_lossy(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// 提供商适配器 trait，参考 aisix Bridge trait 设计
#[async_trait]
pub trait Bridge: Send + Sync + 'static {
    /// 人类可读的 Bridge 名称，用于日志和指标
    fn name(&self) -> &'static str;

    /// 非流式聊天补全
    async fn chat(&self, req: &ChatFormat, ctx: &BridgeContext) -> Result<ChatResponse, BridgeError>;

    /// 流式聊天补全
    async fn chat_stream(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError>;

    /// 嵌入调用
    async fn embed(
        &self,
        _req: &EmbeddingRequest,
        _ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        Err(BridgeError::Config(
            "this provider does not support embeddings".into(),
        ))
    }

    /// 文本补全
    async fn complete(
        &self,
        _body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        Err(BridgeError::Config(
            "this provider does not support text completions".into(),
        ))
    }

    /// 图片生成
    async fn generate_image(
        &self,
        _body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        Err(BridgeError::Config(
            "this provider does not support image generation".into(),
        ))
    }
}