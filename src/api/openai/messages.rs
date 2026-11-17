use axum::{http::StatusCode, Json};

use crate::bridge::{ChatMessage, FinishReason, Role};
use serde_json::Value;

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
///
/// 走 [`crate::error_translate::render_openai_error`] 做跨协议错误翻译：
/// Anthropic / Bedrock / Vertex / Azure 的上游错误码会推导成统一的 OpenAI
/// `error.code`（如 `rate_limit_exceeded` / `invalid_api_key`），客户端可按
/// code 统一重试与提示；`type`/`param` 同步透出。原实现把 `type` 恒写成
/// `invalid_request_error`、`code` 恒写成变体名，上游语义全部丢失。
///
/// 安全边界不变：发给客户端的 message 先脱敏（截断 + 凭据遮蔽）；日志侧仍是
/// 未脱敏原文供排障。
pub(crate) fn bridge_error_response(e: crate::bridge::BridgeError) -> (StatusCode, Json<Value>) {
    tracing::warn!("bridge error (raw, ops): {e}");
    let (status_u16, body) = crate::error_translate::render_openai_error(&e);
    let status = StatusCode::from_u16(status_u16).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = crate::error_translate::sanitize_error_message(&body.message);
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": body.kind,
                "param": body.param,
                "code": body.code,
            }
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{BridgeError, UpstreamErrorView, UpstreamWire};

    /// Anthropic 上游鉴权失败：kind 推导成 `invalid_api_key`，客户端消息脱敏。
    #[test]
    fn bridge_error_response_translates_upstream_codes_and_sanitizes() {
        let e = BridgeError::UpstreamStatus {
            status: 400,
            message: "anthropic 400".into(),
            parsed: Some(Box::new(UpstreamErrorView {
                kind: Some("authentication_error".into()),
                message: Some("bad key sk-ant-api03-SECRETSECRET".into()),
                code: None,
                param: None,
            })),
            wire: UpstreamWire::Anthropic,
            retry_after: None,
        };
        let (status, Json(body)) = bridge_error_response(e);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["type"], "upstream_error");
        assert_eq!(body["error"]["code"], "invalid_api_key");
        assert!(
            body["error"]["message"].as_str().unwrap().contains("***"),
            "客户端消息必须脱敏：{body}"
        );
    }

    /// 非上游变体：type/code/status 与变体语义一致（原实现 type 恒为
    /// invalid_request_error）。
    #[test]
    fn bridge_error_response_uses_variant_type_and_status() {
        let (status, Json(body)) = bridge_error_response(BridgeError::RateLimited);
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(body["error"]["code"], "rate_limit_exceeded");
    }
}
