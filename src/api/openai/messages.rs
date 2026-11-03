use axum::{
    http::StatusCode,
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use crate::bridge::{
    ChatMessage, FinishReason, Role,
};
use super::*;

/// 将 JSON Value 的消息数组解析为 ChatMessage 列表
///
/// `pub(crate)`：B2 成本预估器（`api/admin/pricing.rs`）复用同一解析路径，
/// 保证预估的 token 口径与数据面请求完全一致。
pub(crate) fn parse_messages(value: Option<&Value>) -> Option<Vec<ChatMessage>> {
    parse_messages_with_dev(value, true)
}
/// [`parse_messages`] 的带 developer 兼容开关版本。
///
/// OpenAI 新 SDK 用 `role:"developer"` 取代 system（o1/o3 系）。
/// `developer_to_system = true`（A2A developerRoleCompat 开）时归一为
/// system；false 时按未知角色丢弃（引入该兼容前的行为）。
/// 兼容开关来自 `GatewayPrompt.developer_role_compat`（/api/gateway/prompt）。
pub(crate) fn parse_messages_with_dev(
    value: Option<&Value>,
    developer_to_system: bool,
) -> Option<Vec<ChatMessage>> {
    let arr = value?.as_array()?;
    let mut messages = Vec::new();
    for msg in arr {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some("system") => Role::System,
            Some("developer") if developer_to_system => Role::System,
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        // content 支持三种 wire 形状：string / null / typed-block 数组。
        // 数组形式时：content_blocks 原样保留供支持 blocks 的 bridge verbatim 转发
        // （vision/多模态），content 拼接所有 text 块供不支持 blocks 的 bridge 使用。
        let raw_content = msg.get("content");
        let (content, content_blocks) = match raw_content {
            Some(Value::String(s)) => (Some(s.clone()), None),
            Some(Value::Array(blocks)) => {
                let text = blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let text = if text.is_empty() { None } else { Some(text) };
                (text, Some(blocks.clone()))
            }
            _ => (None, None),
        };
        let name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let tool_calls = msg
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|t| crate::bridge::ToolCall {
                        id: t
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        function_name: t
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: t
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            });
        messages.push(ChatMessage {
            role,
            content,
            content_blocks,
            name,
            tool_call_id,
            tool_calls,
            reasoning: None,
        });
    }
    Some(messages)
}
/// 将 FinishReason 转换为 OpenAI 兼容的字符串
pub(crate) fn finish_reason_str(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::ToolCalls => "tool_calls",
    }
}
/// 将 BridgeError 转换为 HTTP 响应
pub(crate) fn bridge_error_response(e: crate::bridge::BridgeError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    // T3：发给客户端的错误消息先脱敏（截断 + 凭据模式遮蔽），
    // 日志/熔断侧保留原始消息（见 error_translate::sanitize_error_message）
    error_response(
        e.error_type(),
        &crate::error_translate::sanitize_error_message(&e.to_string()),
        status,
    )
}
