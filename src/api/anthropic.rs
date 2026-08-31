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

// H8：extract_api_key / extract_client_ip 实现移至 `api::common`，此处通过 use 别名保持调用不变。
// Anthropic 协议优先 x-api-key，故使用 xapi_first 变体。
use super::common::extract_api_key_xapi_first as extract_api_key;
use super::common::extract_client_ip;

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
    // B22：按结构化错误变体映射状态码，取代原先的 msg.contains(...) 文本匹配
    state
        .api_key_store
        .validate_request(&key, model, ip.as_deref())
        .map_err(|e| {
            use super::auth::ApiKeyError;
            let status = match e {
                // 凭证本身无效：401
                ApiKeyError::Invalid | ApiKeyError::Disabled => StatusCode::UNAUTHORIZED,
                // 凭证有效但无权限/过期/超额：403
                ApiKeyError::Expired
                | ApiKeyError::ModelNotAllowed(_)
                | ApiKeyError::QuotaExhausted
                | ApiKeyError::IpNotAllowed(_) => StatusCode::FORBIDDEN,
            };
            anthropic_error("authentication_error", &e.to_string(), status)
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
        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<crate::bridge::ToolCall> = Vec::new();
        let mut tool_results: Vec<(String, String)> = Vec::new();
        match content {
            Some(Value::String(s)) => text_parts.push(s.clone()),
            Some(Value::Array(parts)) => {
                for p in parts {
                    match p.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                                text_parts.push(t.to_string());
                            }
                        }
                        Some("tool_use") => {
                            let id = p.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                            let name = p
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let arguments = p
                                .get("input")
                                .map(|i| serde_json::to_string(i).unwrap_or_else(|_| "{}".into()))
                                .unwrap_or_else(|| "{}".into());
                            tool_calls.push(crate::bridge::ToolCall {
                                id,
                                function_name: name,
                                arguments,
                            });
                        }
                        Some("tool_result") => {
                            let tid = p
                                .get("tool_use_id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string();
                            let t = p
                                .get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            tool_results.push((tid, t));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }

        // tool_result 转为 OpenAI tool 角色消息，供上游理解多轮工具调用
        if !tool_results.is_empty() {
            for (tid, t) in tool_results {
                result.push(ChatMessage {
                    role: Role::Tool,
                    content: Some(t),
                    name: None,
                    tool_call_id: Some(tid),
                    tool_calls: None,
                });
            }
            continue;
        }

        let text = text_parts.join("\n");
        result.push(ChatMessage {
            role,
            content: if text.is_empty() && tool_calls.is_empty() {
                None
            } else {
                Some(text)
            },
            name: None,
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
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

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = super::openai::ensure_model_priced(&state, &model) {
        return e.into_response();
    }

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover
    let candidates = super::openai::resolve_bridges(&state, &model);
    if candidates.is_empty() {
        return anthropic_error(
            "service_unavailable",
            "No bridge available for the requested model",
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .into_response();
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
                tool_calls: None,
            });
        }
    }

    let chat_req = ChatFormat {
        model: model.clone(),
        messages: all_messages,
        // Anthropic tools 格式 → OpenAI tools 格式（上游统一 OpenAI 协议）
        tools: body
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|tool| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": tool.get("name").and_then(|n| n.as_str()).unwrap_or(""),
                                "description": tool.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                                "parameters": tool.get("input_schema").cloned().unwrap_or(Value::Object(Default::default())),
                            }
                        })
                    })
                    .collect()
            }),
        max_tokens: body.get("max_tokens").and_then(|v| v.as_u64()).map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
    };

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    if is_stream {
        // B06：failover 循环——依次尝试候选渠道建立流，仅对上游可重试错误切换
        let mut stream_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        for (bridge, cid) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            match bridge.chat_stream(&chat_req, &ctx).await {
                Ok(s) => {
                    stream_opt = Some(s);
                    used_channel_id = cid;
                    break;
                }
                Err(e) => {
                    if !super::openai::is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    tracing::warn!("messages stream failover: channel {cid:?} failed: {e}, trying next channel");
                    last_error = Some(e);
                }
            }
        }
        let stream = match stream_opt {
            Some(s) => s,
            None => {
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for streaming request".into(),
                    )
                });
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.clone(),
                        error: e.to_string(),
                    });
                }
                return anthropic_error(e.error_type(), &e.to_string(), status_code).into_response();
            }
        };
        {
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

                // 中间流：文本增量 + tool_use 增量
                let has_tool = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let has_tool_middle = has_tool.clone();
                let mut tool_started = false;
                let middle = stream.flat_map(move |chunk_result| {
                    use std::sync::atomic::Ordering;
                    let mut events: Vec<std::result::Result<Event, Infallible>> = Vec::new();
                    match chunk_result {
                        Ok(chunk) => {
                            if let Some(text) = &chunk.delta.content {
                                let mut buf = acc_for_map.lock();
                                crate::token_estimate::push_capped(&mut buf, text);
                                events.push(Ok(Event::default().event("content_block_delta").data(
                                    serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": 0,
                                        "delta": {"type": "text_delta", "text": text}
                                    })
                                    .to_string(),
                                )));
                            }
                            if let Some(tool_calls) = &chunk.delta.tool_calls {
                                has_tool_middle.store(true, Ordering::Relaxed);
                                for tc in tool_calls {
                                    // 首帧带 id/name，用于启动 tool_use 块
                                    if !tool_started
                                        && (tc.id.is_some() || tc.function_name.is_some())
                                    {
                                        let id = tc.id.clone().unwrap_or_else(|| {
                                            format!("toolu_{}", uuid::Uuid::new_v4())
                                        });
                                        let name = tc.function_name.clone().unwrap_or_default();
                                        events.push(Ok(
                                            Event::default().event("content_block_start").data(
                                                serde_json::json!({
                                                    "type": "content_block_start",
                                                    "index": tc.index,
                                                    "content_block": {"type": "tool_use", "id": id, "name": name}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                        tool_started = true;
                                    }
                                    if let Some(pj) = &tc.arguments {
                                        events.push(Ok(
                                            Event::default().event("content_block_delta").data(
                                                serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": tc.index,
                                                    "delta": {"type": "input_json_delta", "partial_json": pj}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                    }
                                }
                            }
                            if events.is_empty() {
                                events.push(Ok(Event::default().event("ping").data("{}")));
                            }
                        }
                        Err(e) => {
                            let err = serde_json::json!({
                                "type": "error",
                                "error": {"type": "api_error", "message": e.to_string()}
                            })
                            .to_string();
                            events.push(Ok(Event::default().event("error").data(err)));
                        }
                    }
                    futures::stream::iter(events)
                });

                // 后缀：计费 + content_block_stop + message_delta + message_stop（问题 3/7）
                // B05：计费状态由后缀事件与 Drop 守卫共享（原子标志互斥）——
                // 正常结束时后缀事件计费；客户端断连导致流被 drop 时由守卫兜底。
                let billing = std::sync::Arc::new(super::openai::StreamBillingState {
                    state: state.clone(),
                    api_key: api_key.clone(),
                    model: model.clone(),
                    group: billing_group.clone(),
                    chat_req: chat_req.clone(),
                    acc: acc.clone(),
                    charged: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rate_bundle: Some(rate_bundle.clone()),
                    request_start,
                    client_ip: client_ip.clone(),
                    request_id: request_id.clone(),
                    channel_id: used_channel_id.clone(),
                });
                let billing_fin = billing.clone();
                let has_tool_fin = has_tool.clone();
                let suffix = futures::stream::once(async move {
                    // completion tokens 需在 message_delta 事件中回显，先估算
                    let completion_tokens = {
                        let output_text = billing_fin.acc.lock().clone();
                        crate::token_estimate::count_text(&billing_fin.model, &output_text) as u64
                    };

                    // 原子抢占计费权：断连场景守卫可能已兜底计费，双保险防重复
                    if !billing_fin
                        .charged
                        .swap(true, std::sync::atomic::Ordering::SeqCst)
                    {
                        let (prompt_tokens, completion_tokens) = billing_fin.finalize();
                        // 事后限流记账
                        rate_bundle.commit_tokens(prompt_tokens + completion_tokens).await;
                    }

                    // 结束事件序列（工具调用时 stop_reason=tool_use）
                    let stop_reason = if has_tool_fin.load(std::sync::atomic::Ordering::Relaxed) {
                        "tool_use"
                    } else {
                        "end_turn"
                    };
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
                                    "delta": {"stop_reason": stop_reason, "stop_sequence": null},
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

                // B05：包装守卫流——流被 drop（含客户端断连）时兜底计费
                let guarded = super::openai::GuardedStream {
                    inner: Box::pin(combined),
                    _guard: super::openai::StreamUsageGuard::new(billing),
                };
                return Sse::new(guarded).into_response();
        }
    } else {
        // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
        let mut response_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        for (bridge, cid) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            match bridge.chat(&chat_req, &ctx).await {
                Ok(resp) => {
                    response_opt = Some(resp);
                    used_channel_id = cid;
                    break;
                }
                Err(e) => {
                    if !super::openai::is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    tracing::warn!("messages failover: channel {cid:?} failed: {e}, trying next channel");
                    last_error = Some(e);
                }
            }
        }
        let response = match response_opt {
            Some(r) => r,
            None => {
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for non-streaming request".into(),
                    )
                });
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.clone(),
                        error: e.to_string(),
                    });
                }
                return anthropic_error(e.error_type(), &e.to_string(), status_code).into_response();
            }
        };
        {
                let latency_ms = request_start.elapsed().as_millis() as u64;
                state.usage_tracker.accumulate(
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    0,
                    0,
                    0,
                    0.0,
                );

                // 计费扣减：复用 charge_usage 以保证 QuotaLow 通知等行为与流式分支一致
                // （问题 5/6；M10 同类问题。原内联实现缺少 QuotaLow 通知，属行为 bug）
                let cost = super::openai::charge_usage(
                    &state,
                    &api_key,
                    &model,
                    &billing_group,
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                );

                // 事后限流记账
                let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
                rate_bundle.commit_tokens(total_tokens).await;

                // 记录请求日志（含 channel_id，问题 4）
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
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
                // 工具调用时输出 tool_use 内容块（与流式分支保持一致）
                let mut content_blocks: Vec<Value> = Vec::new();
                if let Some(tool_calls) = &response.message.tool_calls {
                    for tc in tool_calls {
                        let input = serde_json::from_str::<Value>(&tc.arguments)
                            .unwrap_or_else(|_| Value::Object(Default::default()));
                        content_blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function_name,
                            "input": input,
                        }));
                    }
                }
                let text = response.message.content_str();
                if !text.is_empty() {
                    content_blocks.push(serde_json::json!({
                        "type": "text",
                        "text": text,
                    }));
                }
                let anthropic_resp = serde_json::json!({
                    "id": msg_id,
                    "type": "message",
                    "role": "assistant",
                    "content": content_blocks,
                    "model": response.model,
                    "stop_reason": to_anthropic_stop_reason(&response.finish_reason),
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": response.usage.prompt_tokens,
                        "output_tokens": response.usage.completion_tokens,
                    }
                });
                return Json(anthropic_resp).into_response();
        }
    }
}
