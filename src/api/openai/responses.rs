use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use super::responses_convert;
use super::*;
use crate::api::common::extract_client_ip;
use crate::bridge::{BridgeContext, BridgeError, ResponsesPassthrough};
use crate::channel::ChannelStore;
use futures::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// `/v1/responses` failover 单次尝试的结果。
///
/// 透传（上游原生 Responses）与降级转换（Responses → Chat，session 型渠道）
/// 共用同一套 failover / 熔断 / 计费骨架，只在最终回包时分支。
//
// `ConvertedJson` 比 `Passthrough` 大约 224 字节：本枚举只在单次上游尝试内
// 短暂存活（failover 循环的局部量），不在热路径上堆叠，故不 box。
#[allow(clippy::large_enum_variant)]
enum ResponsesAttempt {
    /// 上游原生 Responses 透传（既有路径，零行为变化）
    Passthrough(ResponsesPassthrough),
    /// 降级：Chat 非流式响应（待转换为 Responses 形状）
    ConvertedJson(crate::bridge::ChatResponse),
    /// 降级：Chat 流式 chunk 流（待转换为 Responses SSE 事件）
    ConvertedStream(crate::bridge::ChatChunkStream),
}

/// POST /v1/responses - OpenAI Responses API 透传（并支持降级转换）
///
/// 与 handle_chat_completions 不同，本端点默认不做协议转换（不构建
/// ChatFormat / 不走 Bridge::chat），而是把请求 body 原样转发给上游
/// 的 /v1/responses 端点（参考 aisix 的 responses_to_target 透传方案）：
/// 1. 认证 → 2. 限流 → 3. 分组权限 → 4. 定价校验 → 5. resolve_bridges
/// 6. failover 循环调用 Bridge::responses_passthrough
/// 7. 计费从 Responses 格式 usage（input_tokens/output_tokens）提取，
///    流式缺 usage 时回退 token 估算
///
/// **降级**：session 型渠道（qoder/trae/cmdcode…）的 responses_passthrough
/// 默认实现返回 `BridgeError::Config`；此时改为「Responses → Chat 转换 +
/// Bridge::chat/chat_stream」（见 `responses_convert`）。仅 `Config` 降级，
/// 其它 `BridgeError`（Transport/Upstream*）维持既有 failover 语义。
pub(crate) async fn handle_responses(
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
            return openai_error_response(
                "invalid_model",
                "Missing model field",
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // log_body 开关：客户端请求体快照（一次序列化，失败/成功路径共用）
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(body.to_string())
    } else {
        None
    };

    // 网络层闸门：管理员关闭网络层时拒绝转发（管理面不受影响）
    if let Err(resp) = crate::api::admin::network::network_layer_gate(&state) {
        return *resp;
    }

    // 限流检查：鉴权后、推理前（Responses 端点同样纳入限流）
    let rate_bundle = match state
        .rate_limiter
        .check(
            &api_key.id,
            &model,
            api_key.user_id.as_deref(),
            client_ip.as_deref(),
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return openai_error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组
    let billing_group = match check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = ensure_model_priced(&state, &model) {
        return e.into_response();
    }

    // 阶段2：亲和性 session 标识（Responses 透传：user_id 兜底）
    let session_id: Option<String> = api_key.user_id.clone();

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover。
    // 阶段2：带亲和性选取——粘性窗口内命中渠道置顶
    // #40 §1：请求体 → 能力需求（tools / images / video）在选路前推出；
    // Responses 形态的消息在 `input` 里（推导层两个列表都扫）
    let candidates = resolve_bridges_with_affinity_needs(
        &state,
        &model,
        session_id.as_deref(),
        api_key.provider_filter().as_deref(),
        None,
        from_chat_body(&body),
    );
    if candidates.is_empty() {
        return openai_error_response(
            "no_bridge",
            "No bridge available for the requested model",
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .into_response();
    }

    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Responses 协议以 input 字段承载输入（字符串或 item 数组均可），
    // 不使用 chat 的 messages 字段；缺失即 400
    if !matches!(
        body.get("input"),
        Some(Value::String(_)) | Some(Value::Array(_))
    ) {
        return openai_error_response(
            "invalid_input",
            "input field is required",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    // B06：failover 循环——依次尝试候选渠道透传，仅对上游可重试错误切换
    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut used_upstream: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid, ch_ref) in candidates {
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
        let upstream = resolve_upstream_model(&model, ch_ref.as_ref(), &state.model_mapper);
        if upstream != model && ensure_model_priced(&state, &upstream).is_err() {
            tracing::warn!(
                "responses mapping: {model} -> {upstream} unpriced, skip channel {cid:?}"
            );
            last_error = Some(crate::bridge::BridgeError::Config(format!(
                "mapped upstream '{upstream}' has no price configured"
            )));
            continue;
        }
        // 按上游模型名改写请求 body（bridges 从 body["model"] 构造上游请求）
        let mut attempt_body = body.clone();
        // P2-10：指纹脱敏挂载（Responses 透传路径）——在这一家即将发送之前判定
        // provider 作用域（A2A `send_body` 同口径）
        if crate::config::sanitize_fingerprints_enabled() {
            let provider = super::sanitize_provider_id(&state, ch_ref.as_ref());
            crate::sanitize::strip_for_provider(&provider, &mut attempt_body);
        }
        attempt_body["model"] = Value::String(upstream.clone());
        let attempt_start = std::time::Instant::now();
        // T2：SSE 首事件守卫——流式分支等第一个事件（30s 上限），上游挂起/
        // 立刻断流按失败处理（熔断 + failover）；非流式 Json 直接放行
        let attempt: Result<ResponsesAttempt, BridgeError> = match bridge
            .responses_passthrough(&attempt_body, is_stream, &ctx)
            .await
        {
            Ok(ResponsesPassthrough::Stream(s)) => crate::bridge::first_event_or_timeout(s)
                .await
                .map(ResponsesPassthrough::Stream)
                .map(ResponsesAttempt::Passthrough),
            Ok(r) => Ok(ResponsesAttempt::Passthrough(r)),
            // 渠道不支持 Responses 透传（session 型桥的默认实现）：降级为
            // 「Responses → Chat 转换 + chat/chat_stream」。只对 Config 降级；
            // Transport / Upstream* 维持既有 failover 语义。
            Err(BridgeError::Config(reason)) => {
                tracing::debug!(
                    "responses passthrough unsupported on channel {cid:?} ({reason}), \
                     falling back to responses->chat conversion"
                );
                match responses_convert::request_to_chat(&attempt_body) {
                    Ok(converted) => {
                        let mut chat_req = converted.chat;
                        chat_req.model = upstream.clone();
                        if is_stream {
                            match bridge.chat_stream(&chat_req, &ctx).await {
                                Ok(s) => crate::bridge::first_event_or_timeout(s)
                                    .await
                                    .map(ResponsesAttempt::ConvertedStream),
                                Err(e) => Err(e),
                            }
                        } else {
                            bridge
                                .chat(&chat_req, &ctx)
                                .await
                                .map(ResponsesAttempt::ConvertedJson)
                        }
                    }
                    Err(msg) => Err(BridgeError::Config(format!(
                        "responses request cannot be converted to chat: {msg}"
                    ))),
                }
            }
            Err(e) => Err(e),
        };
        match attempt {
            Ok(r) => {
                // 阶段2：成功——记入断路器/健康追踪/亲和性
                if let Some(c) = &cid {
                    state.channel_store.record_channel_success_with_sticky(
                        c,
                        Some(&upstream),
                        attempt_start.elapsed().as_millis() as u64,
                        session_id.as_deref(),
                        crate::channel::routing_policy::StickyMode::sticky_enabled_from(
                            &state.routing_policy.snapshot(),
                        ),
                    );
                }
                result_opt = Some(r);
                used_channel_id = cid;
                used_upstream = Some(upstream);
                break;
            }
            Err(e) => {
                // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                if let Some(c) = &cid {
                    state.channel_store.record_channel_failure(
                        c,
                        Some(&upstream),
                        ChannelStore::classify_bridge_error(&e),
                        &e.to_string(),
                        session_id.as_deref(),
                    );
                }
                if !is_retryable_bridge_error(&e) {
                    // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                    last_error = Some(e);
                    used_channel_id = cid;
                    break;
                }
                if let Some(cid) = &cid {
                    state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                }
                tracing::warn!(
                    "responses failover: channel {cid:?} failed: {e}, trying next channel"
                );
                last_error = Some(e);
            }
        }
    }
    let attempt = match result_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for responses request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.channel_name = used_channel_id
                .as_ref()
                .and_then(|cid| state.channel_store.get(cid))
                .map(|c| c.name.clone());
            log.model = used_upstream.clone().unwrap_or_else(|| model.clone());
            log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                None
            } else {
                Some(model.clone())
            };
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            // log_body：失败路径快照——上游原始错误信息进 response_body
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(e.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(
                &model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "error",
                latency_ms,
            );
            // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.clone(),
                        error: e.to_string(),
                    });
            }
            return bridge_error_response(e).into_response();
        }
    };

    match attempt {
        ResponsesAttempt::Passthrough(ResponsesPassthrough::Json(json)) => {
            // 非流式：上游 JSON body 原样转回客户端，
            // usage 为 Responses 格式（input_tokens/output_tokens）
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let usage = json.get("usage");
            let mut prompt_tokens = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mut completion_tokens = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            // 上游未携带 usage 时用 token 估算兜底（能估就估，拿不到就 0）
            if prompt_tokens == 0 && completion_tokens == 0 {
                let input_text = responses_input_text(&body);
                let output_text = responses_output_text(&json);
                prompt_tokens = crate::token_estimate::count_text(&model, &input_text) as u64;
                completion_tokens = crate::token_estimate::count_text(&model, &output_text) as u64;
            }

            // 记录用量
            state
                .usage_tracker
                .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

            // 计费扣减（用户 quota + key used_quota）
            let billing_model = used_upstream.as_deref().unwrap_or(&model);
            // Responses 形状缓存命中量（input_tokens_details.cached_tokens）
            let cached_tokens = usage
                .and_then(|u| {
                    u.pointer("/input_tokens_details/cached_tokens")
                        .or_else(|| u.pointer("/prompt_tokens_details/cached_tokens"))
                        .and_then(|v| v.as_u64())
                })
                .unwrap_or(0);
            let cost = charge_usage(
                &state,
                &api_key,
                &model,
                billing_model,
                &billing_group,
                prompt_tokens,
                completion_tokens,
                cached_tokens,
            );

            // 事后限流记账（TPM）
            let total_tokens = prompt_tokens + completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.channel_name = used_channel_id
                .as_ref()
                .and_then(|cid| state.channel_store.get(cid))
                .map(|c| c.name.clone());
            log.model = billing_model.to_string();
            log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                None
            } else {
                Some(model.clone())
            };
            log.channel_cost = compute_channel_cost(
                &state,
                used_channel_id.as_deref(),
                &model,
                billing_model,
                &billing_group,
                prompt_tokens,
                completion_tokens,
                cost,
            );
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            // log_body：快照 = 请求体 + 上游响应 JSON
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(json.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(
                billing_model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "ok",
                latency_ms,
            );
            crate::metrics::global().record_tokens(billing_model, "prompt", prompt_tokens);
            crate::metrics::global().record_tokens(billing_model, "completion", completion_tokens);

            Json(json).into_response()
        }
        ResponsesAttempt::Passthrough(ResponsesPassthrough::Stream(byte_stream)) => {
            // 流式：上游 SSE 字节流原样透传给客户端，旁路解析 SSE
            // 提取 usage / 累积输出文本，流 drop 时由守卫计费
            let billing = Arc::new(ResponsesBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: used_upstream.clone().unwrap_or_else(|| model.clone()),
                origin_model: if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                },
                group: billing_group.clone(),
                input_text: responses_input_text(&body),
                usage: Arc::new(parking_lot::Mutex::new(None)),
                acc: Arc::new(parking_lot::Mutex::new(String::new())),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
                channel_name: used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone()),
                debug_request_body,
            });

            // 旁路解析：每个 chunk 原样转发（yield 不变），同时喂给
            // SseDecoder 解析终止事件 usage 与输出文本增量
            let billing_for_map = billing.clone();
            let mut sse_decoder = crate::sse::SseDecoder::new();
            let passthrough_stream = byte_stream.map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    for ev in sse_decoder.feed(bytes.as_ref()) {
                        if let crate::sse::SseEvent::Data(json_str) = ev {
                            if let Ok(json) = serde_json::from_str::<Value>(&json_str) {
                                observe_responses_sse_event(&json, &billing_for_map);
                            }
                        }
                    }
                }
                chunk
            });

            let keepalive_stream = crate::sse::KeepaliveStream::new(
                passthrough_stream,
                crate::bridge::SSE_KEEPALIVE_INTERVAL,
            );
            let guarded = GuardedStream {
                inner: Box::pin(keepalive_stream),
                _guard: ResponsesStreamGuard { billing },
                // Responses 透传路径未登记在途请求，不参与取消
                cancel: None,
                _inflight: None,
            };

            let mut response = Response::new(axum::body::Body::from_stream(guarded));
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            response
        }
        // 降级路径（Responses → Chat）：非流式。计费口径与 chat 非流式一致：
        // usage 归一口径复用 UsageStats::billing_tokens（缓存分量进 prompt）。
        ResponsesAttempt::ConvertedJson(response) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let json = responses_convert::chat_to_response(&response, &model);

            state.usage_tracker.accumulate(
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                0,
                0,
                0,
                0.0,
            );

            let billing_model = used_upstream.as_deref().unwrap_or(&model);
            let (billing_prompt, billing_cached) = response.usage.billing_tokens();
            let cost = charge_usage(
                &state,
                &api_key,
                &model,
                billing_model,
                &billing_group,
                billing_prompt,
                response.usage.completion_tokens,
                billing_cached,
            );

            // 事后限流记账（TPM）
            let total_tokens = billing_prompt + response.usage.completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.channel_name = used_channel_id
                .as_ref()
                .and_then(|cid| state.channel_store.get(cid))
                .map(|c| c.name.clone());
            log.model = billing_model.to_string();
            log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                None
            } else {
                Some(model.clone())
            };
            log.channel_cost = compute_channel_cost(
                &state,
                used_channel_id.as_deref(),
                &model,
                billing_model,
                &billing_group,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                cost,
            );
            log.input_tokens = response.usage.prompt_tokens;
            log.output_tokens = response.usage.completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(json.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);

            crate::metrics::global().record_request(
                billing_model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "ok",
                latency_ms,
            );
            crate::metrics::global().record_tokens(
                billing_model,
                "prompt",
                response.usage.prompt_tokens,
            );
            crate::metrics::global().record_tokens(
                billing_model,
                "completion",
                response.usage.completion_tokens,
            );

            Json(json).into_response()
        }
        // 降级路径（Responses → Chat）：流式。把 Chat chunk 流翻成 Responses
        // SSE 事件，计费仍走 ResponsesStreamGuard（usage 从 chunk 的权威
        // UsageStats 取，缺失时回退 token 估算）。
        ResponsesAttempt::ConvertedStream(chat_stream) => {
            let billing = Arc::new(ResponsesBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: used_upstream.clone().unwrap_or_else(|| model.clone()),
                origin_model: if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                },
                group: billing_group.clone(),
                input_text: responses_input_text(&body),
                usage: Arc::new(parking_lot::Mutex::new(None)),
                acc: Arc::new(parking_lot::Mutex::new(String::new())),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
                channel_name: used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone()),
                debug_request_body,
            });

            let billing_for_map = billing.clone();
            // 流中途出错标志：置位后终帧不再补 [DONE]（与 chat 流式约定一致）
            let errored = Arc::new(AtomicBool::new(false));
            let errored_for_map = errored.clone();
            let errored_for_terminal = errored.clone();
            let client_model = model.clone();
            let mut seq: u64 = 0;
            let sse_stream = chat_stream.flat_map(move |item| {
                let mut out: Vec<Result<bytes::Bytes, BridgeError>> = Vec::new();
                match item {
                    Ok(chunk) => {
                        // 累积输出文本（content + reasoning + 工具名/参数）用于
                        // 上游未上报 usage 时的 completion token 估算
                        {
                            let mut buf = billing_for_map.acc.lock();
                            if let Some(text) = &chunk.delta.content {
                                crate::token_estimate::push_capped(&mut buf, text);
                            }
                            if let Some(reasoning) = &chunk.delta.reasoning {
                                crate::token_estimate::push_capped(&mut buf, reasoning);
                            }
                            if let Some(calls) = &chunk.delta.tool_calls {
                                for tc in calls {
                                    if let Some(name) = &tc.function_name {
                                        crate::token_estimate::push_capped(&mut buf, name);
                                    }
                                    if let Some(args) = &tc.arguments {
                                        crate::token_estimate::push_capped(&mut buf, args);
                                    }
                                }
                            }
                        }
                        // 上游权威 usage（流式终帧携带）
                        if let Some(usage) = chunk.usage {
                            let (prompt, _cached) = usage.billing_tokens();
                            *billing_for_map.usage.lock() = Some((prompt, usage.completion_tokens));
                        }
                        for mut event in
                            responses_convert::chat_chunk_to_response_events(&chunk, &mut seq)
                        {
                            // 终态 response 由 chunk.model（上游名）构造；对客户端
                            // 回显原始请求模型名，与透传路径的可见行为一致
                            if event.get("type").and_then(Value::as_str)
                                == Some("response.completed")
                            {
                                if let Some(obj) =
                                    event.get_mut("response").and_then(|r| r.as_object_mut())
                                {
                                    obj.insert(
                                        "model".to_string(),
                                        Value::String(client_model.clone()),
                                    );
                                }
                            }
                            out.push(Ok(bytes::Bytes::from(responses_convert::sse_frame(&event))));
                        }
                    }
                    Err(e) => {
                        errored_for_map.store(true, Ordering::SeqCst);
                        tracing::warn!("responses converted stream aborted: {e}");
                        let event = serde_json::json!({
                            "type": "error",
                            "error": {
                                "message": crate::error_translate::sanitize_error_message(
                                    &e.to_string(),
                                ),
                                "type": e.error_type(),
                            },
                        });
                        out.push(Ok(bytes::Bytes::from(responses_convert::sse_frame(&event))));
                    }
                }
                futures::stream::iter(out)
            });
            let terminal = futures::stream::once(async move {
                if errored_for_terminal.load(Ordering::SeqCst) {
                    // 已发 error 事件：不再补 [DONE]
                    Ok::<bytes::Bytes, BridgeError>(bytes::Bytes::new())
                } else {
                    Ok::<bytes::Bytes, BridgeError>(bytes::Bytes::from_static(b"data: [DONE]\n\n"))
                }
            });
            let sse_stream = sse_stream.chain(terminal);

            let keepalive_stream =
                crate::sse::KeepaliveStream::new(sse_stream, crate::bridge::SSE_KEEPALIVE_INTERVAL);
            let guarded = GuardedStream {
                inner: Box::pin(keepalive_stream),
                _guard: ResponsesStreamGuard { billing },
                // 转换路径未登记在途请求，不参与取消
                cancel: None,
                _inflight: None,
            };

            let mut response = Response::new(axum::body::Body::from_stream(guarded));
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            response
        }
    }
}
/// 提取 Responses 请求体的 input 文本（token 估算兜底用）。
///
/// input 为字符串，或 item 数组（item 为裸字符串或对象；对象的
/// content / output / reason 槽位可为字符串或 typed parts 数组）；
/// 顶层 instructions 一并计入。参考 aisix 的 responses_input_to_chat
/// / responses_item_text。
pub(crate) fn responses_input_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            parts.push(instructions.to_string());
        }
    }
    match body.get("input") {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                parts.push(s.clone());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                // 裸字符串元素视为用户文本
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        parts.push(s.to_string());
                    }
                    continue;
                }
                // 对象元素：收集 content / output / reason 槽位的文本
                for key in ["content", "output", "reason"] {
                    if let Some(v) = item.get(key) {
                        let text = responses_value_text(v);
                        if !text.is_empty() {
                            parts.push(text);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    parts.join("\n")
}
/// 提取 Responses 内容槽位的纯文本：裸字符串或 typed parts
/// 数组（取各 part 的 text 字段，忽略图片等非文本 part）。
pub(crate) fn responses_value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
/// 提取 Responses 非流式响应的输出文本（token 估算兜底用）。
///
/// 遍历 output 数组：message item 的 content parts 的 text，以及
/// 工具调用 item 顶层的 name / arguments / input。参考 aisix 的
/// responses_output_text。
pub(crate) fn responses_output_text(resp: &Value) -> String {
    let Some(items) = resp.get("output").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for it in items {
        if let Some(content) = it.get("content").and_then(|c| c.as_array()) {
            for p in content {
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
        }
        // 工具调用 item 的输出在顶层 name / arguments / input 字段
        for key in ["name", "arguments", "input"] {
            if let Some(s) = it.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
        }
    }
    parts.join("\n")
}
/// 从一个 Responses SSE 事件中旁路提取计费信息（不影响原样转发）。
///
/// - `response.completed` / `response.incomplete` / `response.failed`
///   终止事件：从 `.response.usage` 提取权威 usage（参考 aisix 的
///   parse_responses_terminal_usage，三者在 `max_output_tokens` 截断
///   / 失败场景下同样携带完整 usage）
/// - `response.output_text.delta`：累积输出文本增量（估算兜底用）
pub(crate) fn observe_responses_sse_event(json: &Value, billing: &ResponsesBillingState) {
    match json.get("type").and_then(|t| t.as_str()) {
        Some("response.completed" | "response.incomplete" | "response.failed") => {
            if let Some(usage) = json.get("response").and_then(|r| r.get("usage")) {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                *billing.usage.lock() = Some((input_tokens, output_tokens));
            }
        }
        Some("response.output_text.delta") => {
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                let mut buf = billing.acc.lock();
                crate::token_estimate::push_capped(&mut buf, delta);
            }
        }
        _ => {}
    }
}
