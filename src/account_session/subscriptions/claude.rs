//! Claude 订阅的数据面占位：**必须经官方 Claude Code CLI 转发**。
//!
//! 对齐 Agent2API `claude_subscription`：它同样拒绝把订阅令牌直接拼成
//! Anthropic API Key 请求（`build_chat_request` 返回 `claude_cli_required`），
//! 真正的转发由官方 CLI 子进程完成（`-p --output-format stream-json`）。
//!
//! AIGX 当前没有「受控 CLI 子进程池」这一层，因此这里**如实拒绝**而不是
//! 发一个必然 401 的 HTTP 请求（不假装支持）。登录/关联（读本机
//! `.claude.json` 的 oauthAccount）由 `account_login::claude_subscription`
//! 负责，账号仍可落库与展示；要打通对话需主控接线 CLI 进程桥（见报告
//! 待接线清单）。

use async_trait::async_trait;

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunkStream, ChatFormat, ChatResponse,
    ResponsesPassthrough,
};

/// 稳定的错误文案（前端/日志可据此识别「缺 CLI 桥」而非凭证问题）。
const CLI_REQUIRED: &str =
    "claude_cli_required: Claude 订阅必须经官方 Claude Code CLI 转发，AIGX 尚未内置 CLI 进程桥";

/// Claude 订阅 Bridge（仅用于让账号进入调度并在调用时给出明确错误）。
pub struct ClaudeSubscriptionBridge;

impl ClaudeSubscriptionBridge {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeSubscriptionBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Bridge for ClaudeSubscriptionBridge {
    fn name(&self) -> &'static str {
        "claude_subscription"
    }

    async fn chat(
        &self,
        _req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        Err(BridgeError::Config(CLI_REQUIRED.to_string()))
    }

    async fn chat_stream(
        &self,
        _req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        Err(BridgeError::Config(CLI_REQUIRED.to_string()))
    }

    async fn responses_passthrough(
        &self,
        _body: &serde_json::Value,
        _stream: bool,
        _ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        Err(BridgeError::Config(CLI_REQUIRED.to_string()))
    }
}
