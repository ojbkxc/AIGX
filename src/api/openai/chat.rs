use super::*;
use crate::api::common::extract_client_ip;
use crate::bridge::{tool_repair, Bridge, BridgeContext, ChatFormat};
use crate::channel::ChannelStore;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

/// POST /v1/chat/completions - 聊天补全
///
/// 使用 Bridge/Hub 模式分发请求，参考 aisix 的 chat_completions 处理流程：
/// 1. 认证 → 2. 解析 ChatFormat → 3. Hub 分发 Bridge → 4. 调用 Bridge::chat/chat_stream
pub(crate) async fn handle_chat_completions(
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
            return error_response(
                "invalid_model",
                "Missing model field",
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP（参照 new-api token.go）
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // 网络层闸门：管理员关闭网络层时拒绝转发（管理面不受影响）
    if let Err(resp) = crate::api::admin::network::network_layer_gate(&state) {
        return *resp;
    }

    // 限流检查（功能 3）：鉴权后、推理前
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
            return error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = match check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = ensure_model_priced(&state, &model) {
        return e.into_response();
    }

    // 阶段2：亲和性 session 标识——优先 body.user 字段（OpenAI SDK 常见透传），
    // 否则用 api_key 所属 user_id，保证同用户会话粘到同一渠道（保留上游 KV 缓存）
    let session_id: Option<String> = body
        .get("user")
        .and_then(|u| u.as_str())
        .map(String::from)
        .or_else(|| api_key.user_id.clone());

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover。
    // 阶段2：带亲和性选取——粘性窗口内命中渠道置顶
    // Phase1-B 路由回放：开关开启时随选路捕获过滤原因/策略快照（关闭=零分配）
    let mut route_capture = if crate::config::route_replay_enabled() {
        Some(crate::api::openai::RouteCapture::new())
    } else {
        None
    };
    let candidates = resolve_bridges_with_affinity(
        &state,
        &model,
        session_id.as_deref(),
        api_key.provider_filter().as_deref(),
        route_capture.as_mut(),
    );
    if candidates.is_empty() {
        return error_response(
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

    // 解析消息列表
    let messages = parse_messages_with_dev(
        body.get("messages"),
        state.gateway_prompt.snapshot().developer_role_compat,
    )
    .unwrap_or_default();
    if messages.is_empty() {
        return error_response(
            "invalid_messages",
            "No valid messages",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    // 出站指纹脱敏（对照 Agent2API sanitizeBlacklistFingerprints）：发送上游前
    // 剥离客户端注入的审核黑名单指纹（模板句最小改写 + 表头键值整段删除）。
    // 放在网关提示词注入之前——apply_prompt 注入的是网关自身文案，不该被触碰。
    let messages = {
        let mut msgs = messages;
        if crate::config::sanitize_fingerprints_enabled() {
            crate::sanitize::strip_chat_messages(&mut msgs);
        }
        msgs
    };

    // 构建 ChatFormat
    // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）：
    // passthrough 不动（零开销）；custom 替换 / append 追加网关提示词；
    // 按模型覆盖优先于全局设置（AIGX 入口只有模型名，故按模型挂）
    let messages = match crate::prompt::apply_prompt_for(
        &model,
        &messages,
        &state.gateway_prompt.snapshot(),
    ) {
        Some(injected) => injected,
        None => messages,
    };

    let chat_req = ChatFormat {
        model: model.clone(),
        messages,
        tools: body
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|arr| arr.to_vec()),
        max_tokens: body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
        top_k: None,
        stop: body.get("stop").and_then(|s| s.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        }),
        tool_choice: body.get("tool_choice").cloned(),
        reasoning_effort: body
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| state.model_mapper.reasoning_for(&model)),
        web_search_options: body.get("web_search_options").cloned(),
        // 透传白名单字段：这些是 OpenAI 协议标准参数，缓存键已认识它们
        // （presence_penalty/frequency_penalty/seed/response_format），但此前
        // extra: None 恒丢弃，导致不同 seed/response_format 的请求命中同一缓存
        // 且效果相同。通过 extra 承载，build_body 会合并进请求体。
        // stream_options 仅流式有意义（include_usage 终帧），非流式不提取。
        extra: {
            let mut obj = serde_json::Map::new();
            for k in [
                "response_format",
                "seed",
                "presence_penalty",
                "frequency_penalty",
                "n",
                "parallel_tool_calls",
                "logit_bias",
                "max_completion_tokens",
                "service_tier",
            ] {
                if let Some(v) = body.get(k) {
                    if !v.is_null() {
                        obj.insert(k.to_string(), v.clone());
                    }
                }
            }
            if is_stream {
                if let Some(v) = body.get("stream_options") {
                    if !v.is_null() {
                        obj.insert("stream_options".to_string(), v.clone());
                    }
                }
            }
            if obj.is_empty() {
                None
            } else {
                Some(Value::Object(obj))
            }
        },
    };

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    // 在途请求登记：管理员可在日志页看到这条请求并终止它。
    // guard 在函数返回时 drop → 自动注销（成功 / 失败 / 客户端断连三条路径都覆盖）。
    //
    // `cancel_signal` 会被挂到响应流上：管理员点「终止」时置位，
    // GuardedStream::poll_next 立刻返回 None 收尾，上游连接随流 drop 断开。
    let (_inflight_id, inflight_guard) = state.inflight.begin(
        model.clone(),
        is_stream,
        api_key.user_id.clone(),
        Some(api_key.id.clone()),
    );
    let cancel_signal = inflight_guard.cancel_signal();
    // 非流式请求没有可中断的流：终止只在流式路径生效（登记仍然保留，
    // 管理员至少能看到「这条非流式请求还在跑」）。

    // P1：配额预留/结算两段式——在请求发起前预留预估费用，
    // 请求完成后按实际 token 结算，多预留的部分归还。
    // 估算：prompt 用 token 计数器精确估算；completion 跟随请求 max_tokens
    // （未指定时保守默认 256，上限 4096 防止恶意超大 max_tokens 占满配额）。
    let estimated_prompt_tokens =
        crate::token_estimate::count_chat_prompt(&model, &chat_req) as u64;
    let estimated_completion_tokens = chat_req.max_tokens.unwrap_or(256).clamp(256, 4096) as u64;
    let reservation = match reserve_usage(
        &state,
        &api_key,
        &model,
        &model,
        &billing_group,
        estimated_prompt_tokens,
        estimated_completion_tokens,
    ) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // P1-7：调度决策回放——记录候选渠道列表
    let candidate_channel_ids: Vec<String> = candidates
        .iter()
        .filter_map(|(_, cid, _)| cid.clone())
        .collect();

    // log_body 开关：客户端请求体快照（一次序列化，失败/成功路径共用）
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(crate::bridge::chat_format_debug_json(&chat_req))
    } else {
        None
    };

    if is_stream {
        // B06：failover 循环——依次尝试候选渠道建立流，仅对上游可重试错误切换
        let mut stream_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // auto-continue：留住成功轮的桥与（映射后）请求，续写轮复用同一渠道
        let mut cont_bridge: Option<Arc<dyn Bridge>> = None;
        let mut cont_req: Option<ChatFormat> = None;
        for (bridge, cid, ch_ref) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）。
            // 每个渠道都需单独解析——同原始名在不同渠道可能映射到不同上游。
            let upstream = resolve_upstream_model(&model, ch_ref.as_ref(), &state.model_mapper);
            // 防"映射指向未定价模型 → 免费用量"漏洞：仅当映射后名字变化时补查一次
            if upstream != model && ensure_model_priced(&state, &upstream).is_err() {
                // 该渠道映射的上游无定价，跳过此渠道（failover 下一候选）
                tracing::warn!(
                    "chat_stream mapping: {model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            let attempt_start = std::time::Instant::now();
            // T2：SSE 首事件守卫——建流成功后等第一个事件（30s 上限），
            // 上游挂起/建流后立刻断流 → 按失败处理（熔断记账 + failover）
            let attempt = match bridge.chat_stream(&attempt_req, &ctx).await {
                Ok(s) => crate::bridge::first_event_or_timeout(s).await,
                Err(e) => Err(e),
            };
            match attempt {
                Ok(s) => {
                    // 阶段2：流建立成功——记入健康/亲和（断路器成功、延迟 EMA、
                    // 空响应计数清零、粘性路由建立）。流中途失败由计费守卫兜底，
                    // 此处只记"建流成功"。
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
                    stream_opt = Some(s);
                    used_channel_id = cid;
                    // 记录映射结果：流计费守卫用上游名计价/记账
                    used_upstream = Some(upstream);
                    cont_bridge = Some(bridge);
                    cont_req = Some(attempt_req);
                    break;
                }
                Err(e) => {
                    // P1-7：记录被过滤的渠道及原因
                    if let Some(c) = &cid {
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
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
                        "chat_stream failover: channel {cid:?} failed: {e}, trying next channel"
                    );
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
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
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
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
                if let Some(capture) = route_capture.as_ref() {
                    log.filtered_channels
                        .extend(capture.filtered.iter().cloned());
                    log.route_config = capture.config.clone();
                    log.account_skips = capture
                        .account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                }
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
                // G5：失败路径释放预留——渠道全败/客户端错误时解冻，
                // 防止预留永久冻结（2026-09-08 二轮审查）
                release_reservation(&state, &api_key, &reservation);
                return bridge_error_response(e).into_response();
            }
        };
        // auto-continue（流式）：开关打开才包装，在 ChatChunk 层透传全部轮次；
        // 关着时下发字节与接线前逐字一致（零开销）。计费/文本累积在包装的
        // 下游（map 层），天然看到跨轮拼接的整体。
        let stream = match (crate::continuation::plan(), cont_bridge, cont_req) {
            (Some(max_rounds), Some(bridge), Some(req)) => {
                crate::continuation::wrap_stream(stream, bridge, req, ctx.clone(), max_rounds)
            }
            _ => stream,
        };
        {
            // 累积输出文本用于流结束时估算 token（问题 3）
            let acc = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
            let acc_for_map = acc.clone();
            // 工具按次调用计数表（流式过程统计 tool_use 起始块，计费时折算附加费）
            let tool_calls = std::sync::Arc::new(parking_lot::Mutex::new(
                crate::pricing::ToolCallCounts::new(),
            ));
            let tool_calls_for_map = tool_calls.clone();

            // 流中途出错标志：Err 分支置位，final_event 据此不发 [DONE]、
            // metrics 记 error、日志 status_code 记 500（此前出错仍按成功处理）
            let stream_errored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stream_errored_for_map = stream_errored.clone();

            let sse_stream = stream.flat_map(move |chunk_result| match chunk_result {
                    Ok(chunk) => {
                        // 累积内容文本用于事后估算 completion tokens
                        // （含 thinking 推理内容与工具调用参数，与 cf-ai-gw 的
                        //   reasoningTokens/output 计量口径对齐）
                        let mut buf = acc_for_map.lock();
                        if let Some(text) = &chunk.delta.content {
                            crate::token_estimate::push_capped(&mut buf, text);
                        }
                        if let Some(reasoning) = &chunk.delta.reasoning {
                            crate::token_estimate::push_capped(&mut buf, reasoning);
                        }
                        if let Some(tool_calls) = &chunk.delta.tool_calls {
                            for tc in tool_calls {
                                if let Some(name) = &tc.function_name {
                                    crate::token_estimate::push_capped(&mut buf, name);
                                }
                                if let Some(args) = &tc.arguments {
                                    crate::token_estimate::push_capped(&mut buf, args);
                                }
                                // 仅统计 tool_use 起始帧（带 id/name 的首帧），
                                // 避免对参数分片帧重复计数（与 new-api
                                // `content_block_start tool_use` 的计数口径一致）。
                                if tc.function_name.is_some() && tc.id.is_some() {
                                    if let Some(name) = &tc.function_name {
                                        *tool_calls_for_map
                                            .lock()
                                            .entry(name.clone())
                                            .or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                        drop(buf);
                        let mut delta = serde_json::json!({
                            "content": chunk.delta.content.unwrap_or_default(),
                        });
                        // 流式推理内容增量透传给 OpenAI 客户端（OpenAI 协议字段）
                        if let Some(reasoning) = &chunk.delta.reasoning {
                            delta["reasoning_content"] = Value::String(reasoning.clone());
                        }
                        if let Some(tool_calls) = &chunk.delta.tool_calls {
                            // 按 index 聚合流式参数分片，并对完整 arguments 做三层自修复
                            // （同一次 chunk 内的多个 index 并行聚合，修复彼此独立）
                            let repaired =
                                tool_repair::accumulate_tool_call_arguments(tool_calls);
                            // Antigravity 思考签名随 tool_call 带出：位置取 Google
                            // 自家 OpenAI 兼容端点的官方非标字段
                            // extra_content.google.thought_signature。多数客户端会
                            // 丢弃该字段 —— 那时请求侧按服务端缓存回填
                            // （antigravity_signature_cache）。
                            let signature = tool_calls
                                .iter()
                                .find_map(|tc| tc.signature.clone());
                            delta["tool_calls"] = Value::Array(
                                repaired
                                    .iter()
                                    .map(|(idx, id, name, args)| {
                                        let mut item = serde_json::json!({
                                            "index": idx,
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": args,
                                            }
                                        });
                                        if let Some(sig) = &signature {
                                            item["extra_content"] = serde_json::json!({
                                                "google": { "thought_signature": sig }
                                            });
                                        }
                                        item
                                    })
                                    .collect(),
                            );
                        }
                        let sse_data = serde_json::json!({
                            "id": chunk.id,
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": chunk.model,
                            "choices": [{
                                "index": 0,
                                "delta": delta,
                                "finish_reason": chunk.finish_reason.as_ref().map(|fr| finish_reason_str(fr)),
                            }]
                        })
                        .to_string();
                        let mut evs = vec![Ok::<_, Infallible>(Event::default().data(sse_data))];
                        // 流式 usage 终帧：客户端请求 stream_options.include_usage=true 时
                        // 回传 usage 帧（choices 空数组，OpenAI 官方契约）。此前
                        // chat.rs 忽略 chunk.usage，客户端收不到 usage 帧。
                        if let Some(usage) = &chunk.usage {
                            let usage_data = serde_json::json!({
                                "id": chunk.id,
                                "object": "chat.completion.chunk",
                                "created": chrono::Utc::now().timestamp(),
                                "model": chunk.model,
                                "choices": [],
                                "usage": {
                                    "prompt_tokens": usage.prompt_tokens,
                                    "completion_tokens": usage.completion_tokens,
                                    "total_tokens": usage.total_tokens,
                                }
                            })
                            .to_string();
                            evs.push(Ok::<_, Infallible>(Event::default().data(usage_data)));
                        }
                        futures::stream::iter(evs)
                    }
                    Err(e) => {
                        tracing::error!("Stream chunk error: {e}");
                        // 标记流中途出错：final_event 据此不发 [DONE]、
                        // metrics 记 error、日志记 500
                        stream_errored_for_map.store(
                            true,
                            std::sync::atomic::Ordering::SeqCst,
                        );
                        // 标准 SSE error 事件格式（与 OpenAI API 错误结构一致）：
                        //   event: error
                        //   data: {"error":{"message":"...","type":"...","code":"..."}}
                        // T3：发给客户端的错误消息先脱敏（日志侧保留原始消息）
                        let safe_msg =
                            crate::error_translate::sanitize_error_message(&e.to_string());
                        let err_data = serde_json::json!({
                            "error": {
                                "message": safe_msg,
                                "type": e.error_type(),
                                "code": e.error_type(),
                            }
                        })
                        .to_string();
                        futures::stream::iter(vec![Ok::<_, Infallible>(
                            Event::default().event("error").data(err_data),
                        )])
                    }
                });

            // 流结束：估算 token、扣费、commit_tokens、记录日志（问题 3）
            // B05：计费状态由后缀事件与 Drop 守卫共享（原子标志互斥）——
            // 正常结束时后缀事件计费；客户端断连导致流被 drop 时由守卫兜底。
            // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照（开关关闭
            // 时 route_capture 为 None，两行均零开销）
            if let Some(capture) = route_capture.as_ref() {
                filtered_channels.extend(capture.filtered.iter().cloned());
            }
            let route_config = route_capture.as_ref().and_then(|c| c.config.clone());
            let account_skips = route_capture
                .as_ref()
                .map(|c| {
                    c.account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                })
                .unwrap_or_default();
            let billing = Arc::new(StreamBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: used_upstream.clone().unwrap_or_else(|| model.clone()),
                origin_model: if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                },
                group: billing_group.clone(),
                chat_req: chat_req.clone(),
                tool_calls: tool_calls.clone(),
                acc: acc.clone(),
                charged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
                channel_name: used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone()),
                candidate_channels: candidate_channel_ids.clone(),
                filtered_channels: filtered_channels.clone(),
                route_config,
                account_skips,
                reservation: Some(reservation),
                debug_request_body,
            });
            let billing_fin = billing.clone();
            let final_event = async move {
                // 原子抢占计费权：断连场景守卫可能已兜底计费，双保险防重复
                let (prompt_tokens, completion_tokens) = if !billing_fin
                    .charged
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let errored = stream_errored.load(std::sync::atomic::Ordering::SeqCst);
                    let (pt, ct) = billing_fin.finalize(errored);
                    // C1 健康档案：流正常完成，按真实总时延补记延迟样本。
                    // 建流时已 note_success（清零断路器）；此处只补延迟，
                    // 不重复计成功次数（见 health_archive::note_latency）。
                    if let Some(channel_id) = &billing_fin.channel_id {
                        billing_fin
                            .state
                            .channel_store
                            .health_archive()
                            .note_latency(
                                channel_id,
                                billing_fin.request_start.elapsed().as_millis() as u64,
                            );
                    }
                    // 事后限流记账（TPM）
                    rate_bundle.commit_tokens(pt + ct).await;
                    (pt, ct)
                } else {
                    // 守卫已兜底计费：这里只取估算值供指标上报，不再重复扣费
                    let output_text = billing_fin.acc.lock().clone();
                    (
                        crate::token_estimate::count_chat_prompt(
                            &billing_fin.model,
                            &billing_fin.chat_req,
                        ) as u64,
                        crate::token_estimate::count_text(&billing_fin.model, &output_text) as u64,
                    )
                };

                // Prometheus 指标（流中途出错记 error，不再恒 ok）
                let errored = stream_errored.load(std::sync::atomic::Ordering::SeqCst);
                crate::metrics::global().record_request(
                    &billing_fin.model,
                    billing_fin.channel_id.as_deref().unwrap_or("unknown"),
                    if errored { "error" } else { "ok" },
                    billing_fin.request_start.elapsed().as_millis() as u64,
                );
                crate::metrics::global().record_tokens(&billing_fin.model, "prompt", prompt_tokens);
                crate::metrics::global().record_tokens(
                    &billing_fin.model,
                    "completion",
                    completion_tokens,
                );

                // 流中途出错不发 [DONE]：已发 error 事件，客户端应停止读取；
                // 发 [DONE] 会让严格 SDK 把半截流当正常完成
                if errored {
                    Ok::<_, Infallible>(Event::default().data(""))
                } else {
                    Ok::<_, Infallible>(Event::default().data("[DONE]"))
                }
            };
            let combined = sse_stream.chain(futures::stream::once(final_event));

            // B05：包装守卫流——流被 drop（含客户端断连）时兜底计费
            let guarded = GuardedStream {
                inner: Box::pin(combined),
                _guard: StreamUsageGuard::new(billing),
                // 管理员「终止在途请求」时立刻收尾下游流
                cancel: Some(cancel_signal),
                // guard 移进流：流存活期间才算是「在途」。函数返回响应后
                // guard 不能立刻 drop，否则列表里会立刻消失。
                _inflight: Some(inflight_guard),
            };

            Sse::new(guarded)
                // 保活用 SSE 注释行（` : keepalive`），不是 `event: ping`：
                // OpenAI 标准流里没有 ping 事件，硬塞会让严格解析的 SDK 收到
                // 未知事件；注释行按规范被忽略，但能刷新 TCP/CDN 空闲计时器。
                .keep_alive(
                    KeepAlive::new()
                        .interval(crate::bridge::SSE_KEEPALIVE_INTERVAL)
                        .text(crate::bridge::SSE_KEEPALIVE_TEXT),
                )
                .into_response()
        }
    } else {
        // 响应缓存：非流式请求尝试缓存命中。
        // P1 修复：key 必须包含用户隔离维度与全部生成参数——
        // 原先仅 model/messages/temperature/max_tokens，导致
        // (a) 跨用户响应泄漏（他人相同 prompt 可命中你的缓存）
        // (b) tools/tool_choice/stop/top_p 不同的请求命中同一条目，返回错误结果
        let cache_key = serde_json::to_string(&serde_json::json!({
            "key_id": &api_key.id,
            "model": &model,
            "messages": body.get("messages").cloned().unwrap_or(Value::Null),
            "temperature": body.get("temperature").cloned().unwrap_or(Value::Null),
            "max_tokens": body.get("max_tokens").cloned().unwrap_or(Value::Null),
            "tools": body.get("tools").cloned().unwrap_or(Value::Null),
            "tool_choice": body.get("tool_choice").cloned().unwrap_or(Value::Null),
            "stop": body.get("stop").cloned().unwrap_or(Value::Null),
            "top_p": body.get("top_p").cloned().unwrap_or(Value::Null),
            "presence_penalty": body.get("presence_penalty").cloned().unwrap_or(Value::Null),
            "frequency_penalty": body.get("frequency_penalty").cloned().unwrap_or(Value::Null),
            "seed": body.get("seed").cloned().unwrap_or(Value::Null),
            "response_format": body.get("response_format").cloned().unwrap_or(Value::Null),
        }))
        .unwrap_or_default();
        if let Some(cached) = state.response_cache.get(&cache_key).await {
            tracing::debug!("response cache hit for model {}", model);
            let prompt_tokens = cached
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion_tokens = cached
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // 计费只走一条路：按次订阅固定扣额度；否则按 token 量计费。
            // 命中意味着整个 prompt 由网关内部响应缓存直接回放（未消耗上游资源），
            // prompt_tokens 全部按「缓存读」价格计费；cache_price 未配置的模型
            // 该部分为 0，维持旧的命中免费行为（升级零破坏）。
            let cache_cost = if is_count_billing(&state, &api_key) {
                crate::config::billing_flat_quota()
            } else {
                match state.pricing_store.calculate_cost_quoted_with_cache(
                    &model,
                    0,
                    0,
                    prompt_tokens,
                    &billing_group,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("cache hit billing: {e}, billing as 0");
                        0
                    }
                }
            };
            // P0-1：缓存命中必须结算此前预留的配额，否则预留冻结到 TTL 才释放。
            // 走与正常路径一致的分配逻辑（订阅池 → 用户钱包 → key），按
            // cache_cost 结算并逐笔摘除预留：预留归零，实扣 == cache_cost；
            // cache_cost==0（未配 cache_price）时即全额退还预留。
            let cache_cost = settle_cache_hit(&state, &api_key, &reservation, cache_cost);
            state
                .usage_tracker
                .accumulate(0, 0, 0, prompt_tokens, 0, 0.0);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = Some("cache".to_string());
            log.model = model.clone();
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = cache_cost;
            log.latency_ms = request_start.elapsed().as_millis() as u64;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            log.error_msg = Some("cache_hit".to_string());
            log.cache_hit = true;
            // log_body：缓存命中快照 = 请求体 + 缓存的响应 JSON
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(cached.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);
            crate::metrics::global().record_request(
                &model,
                "cache",
                "ok",
                request_start.elapsed().as_millis() as u64,
            );
            let mut resp = Json(cached).into_response();
            resp.headers_mut()
                .insert("x-aigx-cache", "hit".parse().unwrap());
            return resp;
        }
        // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
        let mut response_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // auto-continue：留住成功轮的桥与（映射后）请求，续写轮复用同一渠道
        let mut cont_bridge: Option<Arc<dyn Bridge>> = None;
        let mut cont_req: Option<ChatFormat> = None;
        for (bridge, cid, ch_ref) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
            let upstream = resolve_upstream_model(&model, ch_ref.as_ref(), &state.model_mapper);
            if upstream != model && ensure_model_priced(&state, &upstream).is_err() {
                tracing::warn!(
                    "chat mapping: {model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            let attempt_start = std::time::Instant::now();
            match bridge.chat(&attempt_req, &ctx).await {
                Ok(resp) => {
                    // 阶段2：成功——记入断路器/健康追踪/亲和性/空响应计数
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
                    response_opt = Some(resp);
                    used_channel_id = cid;
                    used_upstream = Some(upstream);
                    cont_bridge = Some(bridge);
                    cont_req = Some(attempt_req);
                    break;
                }
                Err(e) => {
                    // P1-7：记录被过滤的渠道及原因
                    if let Some(c) = &cid {
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
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
                        "chat failover: channel {cid:?} failed: {e}, trying next channel"
                    );
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
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
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
                // P1-7：调度决策回放
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
                if let Some(capture) = route_capture.as_ref() {
                    log.filtered_channels
                        .extend(capture.filtered.iter().cloned());
                    log.route_config = capture.config.clone();
                    log.account_skips = capture
                        .account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                }
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
                // G5：失败路径释放预留——渠道全败/客户端错误时解冻，
                // 防止预留永久冻结（2026-09-08 二轮审查）
                release_reservation(&state, &api_key, &reservation);
                return bridge_error_response(e).into_response();
            }
        };
        // auto-continue（非流式）：逐轮续写拼接正文、usage 求和；没续轮或
        // 开关关着时响应原样返回。续写轮失败保留已得内容（见 continue_non_streaming）。
        let response = match (crate::continuation::plan(), cont_bridge, cont_req) {
            (Some(max_rounds), Some(bridge), Some(req)) => {
                crate::continuation::continue_non_streaming(bridge, req, &ctx, response, max_rounds)
                    .await
            }
            _ => response,
        };
        {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            // 记录用量
            state.usage_tracker.accumulate(
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                0,
                0,
                0,
                0.0,
            );

            // 计费扣减：P1 两段式——用 settle_usage 结算之前预留的配额。
            // 若无预留记录（旧路径），回退到 charge_usage。
            // 计费与日志统一用映射后的上游模型名
            let billing_model = used_upstream.as_deref().unwrap_or(&model);
            // 上游缓存量透传：prompt 按真实口径（含 Anthropic 缓存分量），
            // 缓存命中部分走缓存价而不是免费
            let (billing_prompt, billing_cached) = response.usage.billing_tokens();
            let cost = settle_usage(
                &state,
                &api_key,
                &model,
                billing_model,
                &billing_group,
                &reservation,
                billing_prompt,
                response.usage.completion_tokens,
                billing_cached,
                None,
            );

            // 事后限流记账（TPM）
            let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志（功能 1）
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
            // P1-7 + Phase1-B 路由回放：候选/过滤/选中/策略快照（此前成功
            // 路径漏记路由字段，一并补齐）
            log.candidate_channels = candidate_channel_ids.clone();
            log.filtered_channels = filtered_channels.clone();
            log.selected_channel = used_channel_id.clone();
            if let Some(capture) = route_capture.as_ref() {
                log.filtered_channels
                    .extend(capture.filtered.iter().cloned());
                log.route_config = capture.config.clone();
            }
            // log_body：快照 = 请求体 + 响应正文（content 优先，tool_calls 计数）
            if debug_request_body.is_some() {
                let resp_snapshot = serde_json::json!({
                    "content": response.message.content_str(),
                    "reasoning": response.message.reasoning,
                    "tool_calls_count": response.message.tool_calls.as_ref().map_or(0, |t| t.len()),
                    "usage": { "prompt_tokens": response.usage.prompt_tokens, "completion_tokens": response.usage.completion_tokens },
                });
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(resp_snapshot.to_string()),
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

            let mut message = serde_json::json!({
                "role": "assistant",
                "content": response.message.content_str(),
            });
            if let Some(reasoning) = &response.message.reasoning {
                message["reasoning_content"] = Value::String(reasoning.clone());
            }
            if let Some(tool_calls) = &response.message.tool_calls {
                message["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.function_name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect(),
                );
            }
            let json = serde_json::json!({
                "id": response.id,
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": response.model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason_str(&response.finish_reason),
                }],
                "usage": {
                    "prompt_tokens": response.usage.prompt_tokens,
                    "completion_tokens": response.usage.completion_tokens,
                    "total_tokens": response.usage.total_tokens,
                }
            });
            state.response_cache.insert(cache_key, json.clone()).await;
            let mut resp = Json(json).into_response();
            resp.headers_mut()
                .insert("x-aigx-cache", "miss".parse().unwrap());
            resp
        }
    }
}
