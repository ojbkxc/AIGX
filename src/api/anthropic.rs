//! Anthropic Messages API 兼容端点
//!
//! 实现 POST /v1/messages 接口，兼容 Anthropic Claude API 格式。
//! 通过 Bridge/Hub 架构将请求转换为内部格式，调用 Cloudflare Workers AI，
//! 然后将响应转换回 Anthropic 格式返回。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;

use super::openai::AppState;
use crate::bridge::{BridgeContext, ChatFormat, ChatMessage, FinishReason, Role};

/// 从请求中提取 API Key
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    // x-api-key header (Anthropic standard)
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(key_str) = key.to_str() {
            return Some(key_str.to_string());
        }
    }
    // Authorization: Bearer sk-xxx (OpenAI compatible)
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// 验证 API Key
fn verify_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| anthropic_error("authentication_error", "Missing API key", StatusCode::UNAUTHORIZED))?;

    state
        .api_key_store
        .validate(&key)
        .map(|k| k.id)
        .ok_or_else(|| anthropic_error("authentication_error", "Invalid API key", StatusCode::UNAUTHORIZED))
}

/// Anthropic 错误格式
fn anthropic_error(error_type: &str, message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "type": "error",
            "error": {
                "type": error_type,
                "message": message
            }
        })),
    )
}

/// 将 Anthropic role 转为内部 Role
fn anthropic_role_to_internal(role: &str) -> Option<Role> {
    match role {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "system" => Some(Role::System),
        _ => None,
    }
}

/// 将内部 FinishReason 转为 Anthropic stop_reason
fn to_anthropic_stop_reason(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "end_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::ContentFilter => "stop_sequence",
        FinishReason::ToolCalls => "tool_use",
    }
}

/// 解析 Anthropic 消息列表为内部 ChatMessage 列表
fn parse_anthropic_messages(messages: &[Value]) -> Vec<ChatMessage> {
    let mut result = Vec::new();
    for msg in messages {
        let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let role = match anthropic_role_to_internal(role_str) {
            Some(r) => r,
            None => continue,
        };

        let content = msg.get("content");
        let text = match content {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => {
                parts
                    .iter()
                    .filter_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text").and_then(|t| t.as_str()).map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => String::new(),
        };

        result.push(ChatMessage {
            role,
            content: if text.is_empty() { None } else { Some(text) },
            name: None,
            tool_call_id: None,
        });
    }
    result
}

/// POST /v1/messages - Anthropic Messages API
///
/// 兼容 Anthropic Claude API 格式，将请求转换为内部 Bridge 调用，
/// 然后将响应转换回 Anthropic 格式。
pub async fn handle_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let _key_id = match verify_api_key(&state, &headers) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return anthropic_error("invalid_request_error", "Missing model field", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    let bridge = match super::openai::resolve_bridge(&state.hub, &model) {
        Some(b) => b,
        None => {
            return anthropic_error(
                "service_unavailable",
                "No bridge available for the requested model",
                StatusCode::SERVICE_UNAVAILABLE,
            )
            .into_response()
        }
    };

    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    let messages = body
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| parse_anthropic_messages(arr))
        .unwrap_or_default();

    if messages.is_empty() {
        return anthropic_error("invalid_request_error", "No valid messages", StatusCode::BAD_REQUEST)
            .into_response();
    }

    // 处理 system prompt（Anthropic 顶级 system 字段）
    let mut all_messages = messages;
    if let Some(system) = body.get("system") {
        let system_text = match system {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !system_text.is_empty() {
            all_messages.insert(0, ChatMessage {
                role: Role::System,
                content: Some(system_text),
                name: None,
                tool_call_id: None,
            });
        }
    }

    let chat_req = ChatFormat {
        model: model.clone(),
        messages: all_messages,
        max_tokens: body.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as u32),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
    };

    let ctx = BridgeContext::new(format!("req-{}", uuid::Uuid::new_v4()), model.clone());

    if is_stream {
        match bridge.chat_stream(&chat_req, &ctx).await {
            Ok(stream) => {
                let _msg_id = format!("msg_{}", uuid::Uuid::new_v4());
                let _model_clone = model.clone();

                let sse_stream = stream.map(move |chunk_result| {
                    match chunk_result {
                        Ok(chunk) => {
                            let finish = chunk.finish_reason.as_ref();
                            let sse_data = if let Some(fr) = finish {
                                // 结束事件
                                serde_json::json!({
                                    "type": "message_delta",
                                    "delta": {
                                        "stop_reason": to_anthropic_stop_reason(fr),
                                        "stop_sequence": null
                                    },
                                    "usage": {
                                        "output_tokens": 0
                                    }
                                })
                            } else {
                                // 内容增量
                                let text = chunk.delta.content.unwrap_or_default();
                                if text.is_empty() {
                                    serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {
                                            "type": "text_delta",
                                            "text": ""
                                        }
                                    })
                                } else {
                                    serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {
                                            "type": "text_delta",
                                            "text": text
                                        }
                                    })
                                }
                            };
                            Ok::<_, Infallible>(Event::default().data(sse_data.to_string()))
                        }
                        Err(e) => {
                            let err = serde_json::json!({
                                "type": "error",
                                "error": {
                                    "type": "api_error",
                                    "message": e.to_string()
                                }
                            });
                            Ok(Event::default().data(err.to_string()))
                        }
                    }
                });

                Sse::new(sse_stream).into_response()
            }
            Err(e) => {
                let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                anthropic_error(e.error_type(), &e.to_string(), status).into_response()
            }
        }
    } else {
        match bridge.chat(&chat_req, &ctx).await {
            Ok(response) => {
                state.usage_tracker.accumulate(
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    0,
                    0,
                    0,
                    0.0,
                );

                let msg_id = format!("msg_{}", uuid::Uuid::new_v4());
                let anthropic_resp = serde_json::json!({
                    "id": msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": response.message.content_str()
                    }],
                    "model": response.model,
                    "stop_reason": to_anthropic_stop_reason(&response.finish_reason),
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": response.usage.prompt_tokens,
                        "output_tokens": response.usage.completion_tokens,
                    }
                });
                Json(anthropic_resp).into_response()
            }
            Err(e) => {
                let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                anthropic_error(e.error_type(), &e.to_string(), status).into_response()
            }
        }
    }
}