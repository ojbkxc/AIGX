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

/// 从请求头提取客户端 IP
fn extract_client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            let ip = first.trim();
            if !ip.is_empty() {
                return Some(ip.to_string());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 验证 API Key 并执行全部鉴权检查（状态/过期/模型白名单/额度/IP）。
///
/// 参照 new-api token.go 校验逻辑。
fn verify_api_key_full(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<super::auth::ApiKey, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| anthropic_error("authentication_error", "Missing API key", StatusCode::UNAUTHORIZED))?;
    let ip = extract_client_ip(headers);
    state
        .api_key_store
        .validate_request(&key, model, ip.as_deref())
        .map_err(|msg| {
            let status = if msg.contains("not allowed") || msg.contains("quota") || msg.contains("expired") {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            };
            anthropic_error("authentication_error", &msg, status)
        })
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
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return anthropic_error("invalid_request_error", "Missing model field", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // 限流检查（功能 3）
    let rate_bundle = match state.rate_limiter.check(
        &api_key.id,
        &model,
        api_key.user_id.as_deref(),
        client_ip.as_deref(),
    ).await {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return anthropic_error(
                "rate_limit_error",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ).into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = match super::openai::check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    let (bridge, channel_id) = match super::openai::resolve_bridge(&state, &model) {
        Some(pair) => pair,
        None => {
            return anthropic_error(
                "service_unavailable",
                "No bridge available for the requested model",
                StatusCode::SERVICE_UNAVAILABLE,
            )
            .into_response()
        }
    };
    // 标记渠道已使用（问题 4）
    if let Some(cid) = &channel_id {
        state.channel_store.mark_used(cid);
    }

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

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    if is_stream {
        match bridge.chat_stream(&chat_req, &ctx).await {
            Ok(stream) => {
                // 累积输出文本用于流结束时估算 token（问题 3）
                let acc = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
                let acc_for_map = acc.clone();

                // 前缀事件：message_start + content_block_start（问题 7）
                let msg_id = format!("msg_{}", uuid::Uuid::new_v4());
                let input_tokens_est =
                    crate::token_estimate::count_chat_prompt(&model, &chat_req) as u64;
                let prefix = futures::stream::iter([
                    Ok::<_, Infallible>(
                        Event::default()
                            .event("message_start")
                            .data(
                                serde_json::json!({
                                    "type": "message_start",
                                    "message": {
                                        "id": msg_id,
                                        "type": "message",
                                        "role": "assistant",
                                        "content": [],
                                        "model": model,
                                        "stop_reason": null,
                                        "stop_sequence": null,
                                        "usage": {"input_tokens": input_tokens_est, "output_tokens": 0}
                                    }
                                })
                                .to_string(),
                            ),
                    ),
                    Ok::<_, Infallible>(
                        Event::default()
                            .event("content_block_start")
                            .data(
                                serde_json::json!({
                                    "type": "content_block_start",
                                    "index": 0,
                                    "content_block": {"type": "text", "text": ""}
                                })
                                .to_string(),
                            ),
                    ),
                ]);

                // 中间流：content_block_delta（累积内容）
                let middle = stream.map(move |chunk_result| match chunk_result {
                    Ok(chunk) => {
                        if let Some(text) = &chunk.delta.content {
                            let mut buf = acc_for_map.lock();
                            crate::token_estimate::push_capped(&mut buf, text);
                        }
                        let text = chunk.delta.content.unwrap_or_default();
                        let sse_data = serde_json::json!({
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": text}
                        })
                        .to_string();
                        Ok::<_, Infallible>(
                            Event::default().event("content_block_delta").data(sse_data),
                        )
                    }
                    Err(e) => {
                        let err = serde_json::json!({
                            "type": "error",
                            "error": {"type": "api_error", "message": e.to_string()}
                        })
                        .to_string();
                        Ok(Event::default().event("error").data(err))
                    }
                });

                // 后缀：计费 + content_block_stop + message_delta + message_stop（问题 3/7）
                let state_fin = state.clone();
                let api_key_fin = api_key.clone();
                let model_fin = model.clone();
                let client_ip_fin = client_ip.clone();
                let request_id_fin = request_id.clone();
                let group_fin = billing_group.clone();
                let channel_id_fin = channel_id.clone();
                let chat_req_fin = chat_req.clone();
                let suffix = futures::stream::once(async move {
                    let output_text = acc.lock().clone();
                    let completion_tokens =
                        crate::token_estimate::count_text(&model_fin, &output_text) as u64;
                    let prompt_tokens =
                        crate::token_estimate::count_chat_prompt(&model_fin, &chat_req_fin) as u64;

                    // 累计用量
                    state_fin.usage_tracker.accumulate(
                        prompt_tokens,
                        completion_tokens,
                        0,
                        0,
                        0,
                        0.0,
                    );

                    // 扣费（问题 2/5/6）
                    let cost = super::openai::charge_usage(
                        &state_fin,
                        &api_key_fin,
                        &model_fin,
                        &group_fin,
                        prompt_tokens,
                        completion_tokens,
                    );

                    // 事后限流记账
                    let total_tokens = prompt_tokens + completion_tokens;
                    rate_bundle.commit_tokens(total_tokens).await;

                    // 记录请求日志（含 channel_id，问题 4）
                    let mut log = crate::log::RequestLog::new();
                    log.user_id = api_key_fin.user_id.clone();
                    log.key_id = Some(api_key_fin.id.clone());
                    log.channel_id = channel_id_fin;
                    log.model = model_fin.clone();
                    log.input_tokens = prompt_tokens;
                    log.output_tokens = completion_tokens;
                    log.cost = cost;
                    log.latency_ms = 0;
                    log.status_code = 200;
                    log.ip = client_ip_fin;
                    log.request_id = Some(request_id_fin);
                    state_fin.log_store.record_request(log);

                    // 结束事件序列
                    vec![
                        Ok::<_, Infallible>(
                            Event::default().event("content_block_stop").data(
                                serde_json::json!({"type": "content_block_stop", "index": 0})
                                    .to_string(),
                            ),
                        ),
                        Ok::<_, Infallible>(
                            Event::default().event("message_delta").data(
                                serde_json::json!({
                                    "type": "message_delta",
                                    "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                                    "usage": {"output_tokens": completion_tokens}
                                })
                                .to_string(),
                            ),
                        ),
                        Ok::<_, Infallible>(
                            Event::default()
                                .event("message_stop")
                                .data(serde_json::json!({"type": "message_stop"}).to_string()),
                        ),
                    ]
                })
                .map(futures::stream::iter)
                .flatten();

                let combined = prefix.chain(middle).chain(suffix);
                Sse::new(combined).into_response()
            }
            Err(e) => {
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                anthropic_error(e.error_type(), &e.to_string(), status_code).into_response()
            }
        }
    } else {
        match bridge.chat(&chat_req, &ctx).await {
            Ok(response) => {
                let latency_ms = request_start.elapsed().as_millis() as u64;
                state.usage_tracker.accumulate(
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    0,
                    0,
                    0,
                    0.0,
                );

                // 计费扣减（用解析的 billing_group，问题 5；余额不足跳过 key 扣费，问题 6）
                let cost = state.pricing_store.calculate_cost_quoted(
                    &model,
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    &billing_group,
                );
                if cost > 0 {
                    if let Some(uid) = &api_key.user_id {
                        if state.user_store.try_charge(uid, cost) {
                            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
                        }
                    } else {
                        let _ = state.api_key_store.charge_quota(&api_key.id, cost);
                    }
                }

                // 事后限流记账
                let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
                rate_bundle.commit_tokens(total_tokens).await;

                // 记录请求日志（含 channel_id，问题 4）
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = channel_id.clone();
                log.model = model.clone();
                log.input_tokens = response.usage.prompt_tokens;
                log.output_tokens = response.usage.completion_tokens;
                log.cost = cost;
                log.latency_ms = latency_ms;
                log.status_code = 200;
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);

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
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                anthropic_error(e.error_type(), &e.to_string(), status_code).into_response()
            }
        }
    }
}
