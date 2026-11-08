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
    let key = extract_api_key(headers).ok_or_else(|| {
        anthropic_error(
            "authentication_error",
            "Missing API key",
            StatusCode::UNAUTHORIZED,
        )
    })?;
    let ip = extract_client_ip(headers);
    // 全局 IP 过滤（与 openai.rs verify_api_key_full 对齐）：白名单/黑名单
    // 优先于 token 级校验。原先 /v1/messages 缺失此检查，可绕过 IP 封禁。
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(anthropic_error(
                "invalid_request_error",
                &ip_err.to_string(),
                StatusCode::FORBIDDEN,
            ));
        }
    }
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
                | ApiKeyError::UserQuotaExhausted
                | ApiKeyError::IpNotAllowed(_) => StatusCode::FORBIDDEN,
            };
            anthropic_error("authentication_error", &e.to_string(), status)
        })
}

/// Anthropic 错误格式
fn anthropic_error(
    error_type: &str,
    message: &str,
    status: StatusCode,
) -> (StatusCode, Json<Value>) {
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

/// 将 Anthropic role 转为内部 Role。
///
/// `developer_to_system` = developer 角色兼容开关（A2A developerRoleCompat）：
/// Anthropic 协议本身无 developer 角色，但客户端代理层（如把 OpenAI SDK
/// 请求转成 Anthropic 形状的中间件）可能透传——开关开时归一为 system，
/// 与 OpenAI 入口同口径。
fn anthropic_role_to_internal(role: &str, developer_to_system: bool) -> Option<Role> {
    match role {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "system" => Some(Role::System),
        "developer" if developer_to_system => Some(Role::System),
        _ => None,
    }
}

/// 将内部 FinishReason 转为 Anthropic stop_reason
///
/// 参照 new-api `reasonmap.OpenAIFinishReasonToClaudeStopReason`：
/// - Stop → end_turn
/// - Length → max_tokens
/// - ContentFilter → refusal（不是 stop_sequence）
/// - ToolCalls → tool_use
fn to_anthropic_stop_reason(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "end_turn",
        FinishReason::Length => "max_tokens",
        FinishReason::ContentFilter => "refusal",
        FinishReason::ToolCalls => "tool_use",
    }
}

/// 解析 Anthropic 消息列表为内部 ChatMessage 列表。
///
/// `developer_to_system` = developer 角色兼容开关（网关提示词设置）。
fn parse_anthropic_messages(messages: &[Value], developer_to_system: bool) -> Vec<ChatMessage> {
    let mut result = Vec::new();
    for msg in messages {
        let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
        let role = match anthropic_role_to_internal(role_str, developer_to_system) {
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
                            let id = p
                                .get("id")
                                .and_then(|i| i.as_str())
                                .unwrap_or("")
                                .to_string();
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
                    content_blocks: None,
                    name: None,
                    tool_call_id: Some(tid),
                    tool_calls: None,
                    reasoning: None,
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
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            reasoning: None,
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
            return anthropic_error(
                "invalid_request_error",
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

    // 网络层闸门：管理员关闭网络层时拒绝转发（管理面不受影响）
    if let Err(resp) = crate::api::admin::network::network_layer_gate(&state) {
        return *resp;
    }

    // 限流检查（功能 3）
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
            return anthropic_error(
                "rate_limit_error",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = match super::openai::check_group_model_permission(&state, &api_key, &model)
    {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = super::openai::ensure_model_priced(&state, &model) {
        return e.into_response();
    }

    // 阶段2：亲和性 session 标识——优先 body.metadata.user_id（Anthropic SDK 标准透传），
    // 否则用 api_key 所属 user_id，保证同用户会话粘到同一渠道
    let session_id: Option<String> = body
        .get("metadata")
        .and_then(|m| m.get("user_id"))
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
    let candidates = super::openai::resolve_bridges_with_affinity(
        &state,
        &model,
        session_id.as_deref(),
        api_key.provider_filter().as_deref(),
        route_capture.as_mut(),
    );
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
        .map(|arr| {
            parse_anthropic_messages(arr, state.gateway_prompt.snapshot().developer_role_compat)
        })
        .unwrap_or_default();

    if messages.is_empty() {
        return anthropic_error(
            "invalid_request_error",
            "No valid messages",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    // 处理 system prompt（Anthropic 顶级 system 字段）
    let mut all_messages = messages;
    if let Some(system) = body.get("system") {
        let system_text = match system {
            Value::String(s) => s.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !system_text.is_empty() {
            all_messages.insert(
                0,
                ChatMessage {
                    role: Role::System,
                    content: Some(system_text),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                },
            );
        }
    }

    // 出站指纹脱敏（对照 Agent2API sanitizeBlacklistFingerprints）：发送上游前
    // 剥离客户端注入的审核黑名单指纹。放在网关提示词注入之前——
    // apply_prompt 注入的是网关自身文案，不该被触碰。
    if crate::config::sanitize_fingerprints_enabled() {
        crate::sanitize::strip_chat_messages(&mut all_messages);
    }

    // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）：
    // 客户端 system 已折进消息列表头，再统一走网关策略（custom 会替换它）
    let all_messages = match crate::prompt::apply_prompt_for(
        &model,
        &all_messages,
        &state.gateway_prompt.snapshot(),
    ) {
        Some(injected) => injected,
        None => all_messages,
    };

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
        max_tokens: body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
        top_k: body
            .get("top_k")
            .and_then(|v| v.as_u64())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        stop: body
            .get("stop_sequences")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            }),
        // Anthropic `tool_choice` 原样透传（上游 Anthropic bridge 在 build_body
        // 里已识别 Anthropic 形状的 tool_choice）。同时兼容 OpenAI 形状。
        tool_choice: body.get("tool_choice").cloned(),
        reasoning_effort: state.model_mapper.reasoning_for(&model),
        web_search_options: None,
        extra: body.get("metadata").cloned(),
    };

    // P1 两段式计费：请求前预留配额，流式/非流式结算时多退少补。
    // 此前 Anthropic 分支未接两段式（reservation: None），余额耗尽用户可在
    // 单请求内超额消费。与 chat.rs 同口径。
    let estimated_prompt_tokens =
        crate::token_estimate::count_chat_prompt(&model, &chat_req) as u64;
    let estimated_completion_tokens = chat_req.max_tokens.unwrap_or(256).clamp(256, 4096) as u64;
    let reservation = match super::openai::reserve_usage(
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

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    if is_stream {
        // B06：failover 循环——依次尝试候选渠道建立流，仅对上游可重试错误切换
        let mut stream_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        // P1-7：调度决策回放——记录候选渠道与被过滤渠道（与 openai 流式路径对齐）
        let candidate_channel_ids: Vec<String> = candidates
            .iter()
            .filter_map(|(_, cid, _)| cid.clone())
            .collect();
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // #32 路由回放：真实上游调用计数 + 亲和命中判定（同 resolve 口径）
        let mut attempt_counter: u32 = 0;
        let affinity_hit_id = session_id
            .as_deref()
            .and_then(|sid| state.channel_store.affinity_cache().lookup(sid, &model));
        for (bridge, cid, ch_ref) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
            let upstream =
                super::openai::resolve_upstream_model(&model, ch_ref.as_ref(), &state.model_mapper);
            if upstream != model && super::openai::ensure_model_priced(&state, &upstream).is_err() {
                tracing::warn!(
                    "messages stream mapping: {model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            attempt_counter += 1;
            let attempt_index = attempt_counter;
            let attempt_started_at = chrono::Utc::now().timestamp();
            let sticky_hit = affinity_hit_id.as_deref() == cid.as_deref();
            let attempt_start = std::time::Instant::now();
            // T2：SSE 首事件守卫——建流成功后等第一个事件（30s 上限），
            // 上游挂起/建流后立刻断流 → 按失败处理（熔断记账 + failover）
            let attempt = match bridge.chat_stream(&attempt_req, &ctx).await {
                Ok(s) => crate::bridge::first_event_or_timeout(s).await,
                Err(e) => Err(e),
            };
            match attempt {
                Ok(s) => {
                    // 阶段2：流建立成功——记入断路器/健康追踪/亲和性
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
                    // #32 路由回放：建流成功轮明细（流式终态在计费守卫）
                    if route_capture.is_some() {
                        route_capture
                            .as_mut()
                            .unwrap()
                            .push_attempt(crate::log::AttemptDetail {
                                index: attempt_index,
                                channel_id: cid.clone().unwrap_or_default(),
                                channel_name: ch_ref
                                    .as_ref()
                                    .map(|c| c.name.clone())
                                    .unwrap_or_default(),
                                provider: ch_ref
                                    .as_ref()
                                    .map(|c| c.channel_type.as_str().to_string())
                                    .unwrap_or_default(),
                                account: String::new(),
                                model: upstream.clone(),
                                sticky_hit,
                                started_at: attempt_started_at,
                                finished_at: Some(chrono::Utc::now().timestamp()),
                                status: Some(200),
                                error: None,
                                notice: None,
                            });
                    }
                    stream_opt = Some(s);
                    used_channel_id = cid;
                    used_upstream = Some(upstream);
                    break;
                }
                Err(e) => {
                    // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                    if let Some(c) = &cid {
                        // P1-7：记录被过滤的渠道及原因
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&upstream),
                            crate::channel::ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    // #32 路由回放：失败轮明细
                    if route_capture.is_some() {
                        route_capture
                            .as_mut()
                            .unwrap()
                            .push_attempt(crate::log::AttemptDetail {
                                index: attempt_index,
                                channel_id: cid.clone().unwrap_or_default(),
                                channel_name: ch_ref
                                    .as_ref()
                                    .map(|c| c.name.clone())
                                    .unwrap_or_default(),
                                provider: ch_ref
                                    .as_ref()
                                    .map(|c| c.channel_type.as_str().to_string())
                                    .unwrap_or_default(),
                                account: String::new(),
                                model: upstream.clone(),
                                sticky_hit,
                                started_at: attempt_started_at,
                                finished_at: Some(chrono::Utc::now().timestamp()),
                                status: e.http_status().try_into().ok(),
                                error: Some(e.to_string()),
                                notice: None,
                            });
                    }
                    if !super::openai::is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    if let Some(cid) = &cid {
                        state.channel_store.mark_cooldown(cid, e.to_string(), 60);
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
                // P1-7：调度决策回放字段（与 openai 失败路径对齐）
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
                // （as_mut：attempt_details 按值取出，as_ref 借用不可）
                if let Some(capture) = route_capture.as_mut() {
                    log.filtered_channels
                        .extend(capture.filtered.iter().cloned());
                    log.route_config = capture.config.clone();
                    log.route_truncated = capture.filtered_truncated;
                    log.attempt_details = std::mem::take(&mut capture.attempts);
                    log.attempt_details_truncated = capture.attempts_truncated;
                    log.account_skips = capture
                        .account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                }
                log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                // G5：失败路径释放预留——渠道全败时解冻，防止预留永久冻结
                super::openai::release_reservation(&state, &api_key, &reservation);
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
                // T3：发给客户端的错误消息先脱敏（截断 + 凭据模式遮蔽），
                // 日志/熔断侧保留原始消息（见 error_translate::sanitize_error_message）
                return anthropic_error(
                    e.error_type(),
                    &crate::error_translate::sanitize_error_message(&e.to_string()),
                    status_code,
                )
                .into_response();
            }
        };
        {
            // 累积输出文本用于流结束时估算 token（问题 3）
            let acc = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
            let acc_for_map = acc.clone();
            // 工具按次调用计数表（流式过程统计 tool_use 起始块）
            let tool_calls = std::sync::Arc::new(parking_lot::Mutex::new(
                crate::pricing::ToolCallCounts::new(),
            ));
            let tool_calls_for_map = tool_calls.clone();

            // 流中途出错标志：Err 分支置位，suffix 据此给 finalize 传 errored
            // （日志 status_code 记 500 而非恒 200）
            let stream_errored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stream_errored_for_map = stream_errored.clone();

            // 前缀事件：仅 message_start（内容块统一由 middle 动态开启，
            // 参照 aisix AnthropicSseEncoder：文本块在首个文本增量到达时才开块，
            // 避免 thinking/文本/tool 块 index 冲突）
            let msg_id = format!("msg_{}", uuid::Uuid::new_v4());
            let input_tokens_est =
                crate::token_estimate::count_chat_prompt(&model, &chat_req) as u64;
            // message_start 回显映射后的上游模型名（与计费/日志口径一致）
            let model_for_prefix = used_upstream.clone().unwrap_or_else(|| model.clone());
            let prefix = futures::stream::once(async move {
                Ok::<_, Infallible>(
                    Event::default().event("message_start").data(
                        serde_json::json!({
                            "type": "message_start",
                            "message": {
                                "id": msg_id,
                                "type": "message",
                                "role": "assistant",
                                "content": [],
                                "model": model_for_prefix,
                                "stop_reason": null,
                                "stop_sequence": null,
                                "usage": {"input_tokens": input_tokens_est, "output_tokens": 0}
                            }
                        })
                        .to_string(),
                    ),
                )
            });

            // 中间流：动态开块（thinking/文本/tool 共用递增 index）+ 增量
            let has_tool = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let has_tool_middle = has_tool.clone();
            let has_reasoning = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let has_reasoning_middle = has_reasoning.clone();
            // 是否开启过任意内容块（thinking/文本/tool）——空流兜底用
            let has_any_block = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let has_any_block_middle = has_any_block.clone();
            // 块 index 状态：thinking_block_index / text_block_index 首次开启时
            // 取 next_block_index 并自增，此后固定。tool_use 沿用 OpenAI 的
            // tc.index（参照 aisix：tool_use 块 index = 下一个可用块 index）。
            let mut next_block_index = 0usize;
            let mut thinking_block_index: Option<usize> = None;
            let mut text_block_index: Option<usize> = None;
            let mut tool_block_indexes_mid: std::collections::BTreeMap<usize, usize> =
                std::collections::BTreeMap::new();
            // 已发过 content_block_stop（finish_reason 分支置位）——force_finish 兜底用
            let stop_emitted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_emitted_middle = stop_emitted.clone();
            let middle = stream.flat_map(move |chunk_result| {
                    use std::sync::atomic::Ordering;
                    let mut events: Vec<std::result::Result<Event, Infallible>> = Vec::new();
                    match chunk_result {
                        Ok(chunk) => {
                            // 推理内容增量 → Anthropic thinking 块（首个增量开块）
                            if let Some(reasoning) = &chunk.delta.reasoning {
                                has_reasoning_middle.store(true, Ordering::Relaxed);
                                has_any_block_middle.store(true, Ordering::Relaxed);
                                // 推理内容计入 completion token 估算（与 cf-ai-gw reasoningTokens 对齐）
                                let mut buf = acc_for_map.lock();
                                crate::token_estimate::push_capped(&mut buf, reasoning);
                                drop(buf);
                                let idx = match thinking_block_index {
                                    Some(i) => i,
                                    None => {
                                        let i = next_block_index;
                                        next_block_index += 1;
                                        thinking_block_index = Some(i);
                                        events.push(Ok(
                                            Event::default().event("content_block_start").data(
                                                serde_json::json!({
                                                    "type": "content_block_start",
                                                    "index": i,
                                                    "content_block": {"type": "thinking", "thinking": ""}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                        i
                                    }
                                };
                                events.push(Ok(
                                    Event::default().event("content_block_delta").data(
                                        serde_json::json!({
                                            "type": "content_block_delta",
                                            "index": idx,
                                            "delta": {"type": "thinking_delta", "thinking": reasoning}
                                        })
                                        .to_string(),
                                    ),
                                ));
                            }
                            if let Some(text) = &chunk.delta.content {
                                has_any_block_middle.store(true, Ordering::Relaxed);
                                let mut buf = acc_for_map.lock();
                                crate::token_estimate::push_capped(&mut buf, text);
                                // 工具调用参数计入 completion token 估算
                                if let Some(tcs) = &chunk.delta.tool_calls {
                                    for tc in tcs {
                                        if let Some(name) = &tc.function_name {
                                            crate::token_estimate::push_capped(&mut buf, name);
                                        }
                                        if let Some(args) = &tc.arguments {
                                            crate::token_estimate::push_capped(&mut buf, args);
                                        }
                                    }
                                }
                                drop(buf);
                                let idx = match text_block_index {
                                    Some(i) => i,
                                    None => {
                                        let i = next_block_index;
                                        next_block_index += 1;
                                        text_block_index = Some(i);
                                        events.push(Ok(
                                            Event::default().event("content_block_start").data(
                                                serde_json::json!({
                                                    "type": "content_block_start",
                                                    "index": i,
                                                    "content_block": {"type": "text", "text": ""}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                        i
                                    }
                                };
                                events.push(Ok(Event::default().event("content_block_delta").data(
                                    serde_json::json!({
                                        "type": "content_block_delta",
                                        "index": idx,
                                        "delta": {"type": "text_delta", "text": text}
                                    })
                                    .to_string(),
                                )));
                            }
                            if let Some(tool_calls) = &chunk.delta.tool_calls {
                                has_tool_middle.store(true, Ordering::Relaxed);
                                has_any_block_middle.store(true, Ordering::Relaxed);
                                for tc in tool_calls {
                                    let idx = *tool_block_indexes_mid
                                        .entry(tc.index)
                                        .or_insert_with(|| {
                                            let i = next_block_index;
                                            next_block_index += 1;
                                            i
                                        });
                                    // 首帧带 id/name，用于启动 tool_use 块
                                    if tc.id.is_some() || tc.function_name.is_some() {
                                        let id = tc.id.clone().unwrap_or_else(|| {
                                            format!("toolu_{}", uuid::Uuid::new_v4())
                                        });
                                        let name = tc.function_name.clone().unwrap_or_default();
                                        // 统计工具调用（仅首帧），计费时折算附加费
                                        if tc.id.is_some() && tc.function_name.is_some() {
                                            *tool_calls_for_map
                                                .lock()
                                                .entry(name.clone())
                                                .or_insert(0) += 1;
                                        }
                                        events.push(Ok(
                                            Event::default().event("content_block_start").data(
                                                serde_json::json!({
                                                    "type": "content_block_start",
                                                    "index": idx,
                                                    "content_block": {"type": "tool_use", "id": id, "name": name}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                    }
                                    if let Some(pj) = &tc.arguments {
                                        events.push(Ok(
                                            Event::default().event("content_block_delta").data(
                                                serde_json::json!({
                                                    "type": "content_block_delta",
                                                    "index": idx,
                                                    "delta": {"type": "input_json_delta", "partial_json": pj}
                                                })
                                                .to_string(),
                                            ),
                                        ));
                                    }
                                }
                            }
                            // 收尾块（方案 A，参照 aisix AnthropicSseEncoder）：
                            // finish_reason chunk 到达时，用中间流自己的块 index 状态
                            // 发 content_block_stop——此时这些状态才是实际开启过的块。
                            // 后缀只负责 message_delta + message_stop，不再重复发 stop。
                            if chunk.finish_reason.is_some() {
                                if let Some(i) = thinking_block_index {
                                    events.push(Ok(Event::default().event("content_block_stop").data(
                                        serde_json::json!({"type": "content_block_stop", "index": i})
                                            .to_string(),
                                    )));
                                }
                                if let Some(i) = text_block_index {
                                    events.push(Ok(Event::default().event("content_block_stop").data(
                                        serde_json::json!({"type": "content_block_stop", "index": i})
                                            .to_string(),
                                    )));
                                }
                                for i in tool_block_indexes_mid.values() {
                                    events.push(Ok(Event::default().event("content_block_stop").data(
                                        serde_json::json!({"type": "content_block_stop", "index": i})
                                            .to_string(),
                                    )));
                                }
                                // 已收尾：后缀无需再补
                                stop_emitted_middle.store(true, Ordering::Relaxed);
                            }
                            if events.is_empty() {
                                events.push(Ok(Event::default().event("ping").data("{}")));
                            }
                        }
                        Err(e) => {
                            // 标记流中途出错：suffix 据此给 finalize 传 errored
                            stream_errored_for_map.store(
                                true,
                                std::sync::atomic::Ordering::SeqCst,
                            );
                            // T3：发给客户端的错误消息先脱敏（日志侧保留原始消息）
                            let safe_msg =
                                crate::error_translate::sanitize_error_message(&e.to_string());
                            let err = serde_json::json!({
                                "type": "error",
                                "error": {"type": "api_error", "message": safe_msg}
                            })
                            .to_string();
                            events.push(Ok(Event::default().event("error").data(err)));
                        }
                    }
                    futures::stream::iter(events)
                });

            // 后缀：计费 + 按块序收尾 content_block_stop + message_delta + message_stop（问题 3/7）
            // B05：计费状态由后缀事件与 Drop 守卫共享（原子标志互斥）——
            // 正常结束时后缀事件计费；客户端断连导致流被 drop 时由守卫兜底。
            // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
            if let Some(capture) = route_capture.as_ref() {
                filtered_channels.extend(capture.filtered.iter().cloned());
            }
            let route_config = route_capture.as_ref().and_then(|c| c.config.clone());
            let route_truncated = route_capture
                .as_ref()
                .map(|c| c.filtered_truncated)
                .unwrap_or(0);
            // #32：流式路径把 failover 循环里捕获的逐次尝试明细移交给计费守卫
            // （成功轮的终态在守卫里写，故此处按值取出而非借用）。
            let (attempt_details, attempt_details_truncated) = route_capture
                .as_mut()
                .map(|c| {
                    (
                        std::mem::take(&mut c.attempts),
                        std::mem::take(&mut c.attempts_truncated),
                    )
                })
                .unwrap_or_default();
            let account_skips = route_capture
                .as_ref()
                .map(|c| {
                    c.account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                })
                .unwrap_or_default();
            let billing = std::sync::Arc::new(super::openai::StreamBillingState {
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
                charged: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
                route_truncated,
                attempt_details,
                attempt_details_truncated,
                account_skips,
                // 两段式计费：reserve_usage 已在请求前完成，finalize 走 settle_usage
                reservation: Some(reservation.clone()),
                debug_request_body: if crate::config::log_body_enabled() {
                    Some(crate::bridge::chat_format_debug_json(&chat_req))
                } else {
                    None
                },
            });
            let billing_fin = billing.clone();
            let has_tool_fin = has_tool.clone();
            let has_any_block_fin = has_any_block.clone();
            let stop_emitted_fin = stop_emitted.clone();
            let suffix = futures::stream::once(async move {
                // completion tokens 需在 message_delta 事件中回显，先估算
                let completion_tokens = {
                    let output_text = billing_fin.acc.lock().clone();
                    crate::token_estimate::count_text(&billing_fin.model, &output_text) as u64
                };

                // 原子抢占计费权：断连场景守卫可能已兜底计费，双保险防重复
                let (prompt_tokens, completion_tokens) = if !billing_fin
                    .charged
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let errored = stream_errored.load(std::sync::atomic::Ordering::SeqCst);
                    let (pt, ct) = billing_fin.finalize(errored);
                    // 事后限流记账
                    rate_bundle.commit_tokens(pt + ct).await;
                    (pt, ct)
                } else {
                    // 守卫已兜底计费：此处仅取估算值供 message_delta 回显
                    // 与 Prometheus 指标上报，不再重复扣费
                    let prompt_tokens = crate::token_estimate::count_chat_prompt(
                        &billing_fin.model,
                        &billing_fin.chat_req,
                    ) as u64;
                    (prompt_tokens, completion_tokens)
                };

                // Prometheus 指标
                crate::metrics::global().record_request(
                    &billing_fin.model,
                    billing_fin.channel_id.as_deref().unwrap_or("unknown"),
                    "ok",
                    billing_fin.request_start.elapsed().as_millis() as u64,
                );
                crate::metrics::global().record_tokens(&billing_fin.model, "prompt", prompt_tokens);
                crate::metrics::global().record_tokens(
                    &billing_fin.model,
                    "completion",
                    completion_tokens,
                );

                // 结束事件序列（方案 A）：content_block_stop 已由中间流在
                // finish_reason chunk 处发出（用中间流自己的块 index 状态），
                // 后缀只负责：空流兜底 + force_finish 兜底 + message_delta + message_stop。
                let mut events: Vec<std::result::Result<Event, Infallible>> = Vec::new();
                let has_tool = has_tool_fin.load(std::sync::atomic::Ordering::Relaxed);
                let has_any_block = has_any_block_fin.load(std::sync::atomic::Ordering::Relaxed);
                let stop_sent = stop_emitted_fin.load(std::sync::atomic::Ordering::Relaxed);
                // force_finish 兜底（参照 aisix AnthropicSseEncoder::force_finish）：
                // 上游没发过 finish_reason，直接流结束。此时：
                // - 若开过块但 stop 未发 → 补发 stop（非空流漏 stop）
                // - 若一个块都没开过 → 补空文本块 + stop（空流兜底）
                // 两者都用 index 0（此场景下中间流必然只有一个活动块，
                // 多个块混排时上游必然发了 finish_reason 走中间流收尾）。
                if !stop_sent {
                    let i = 0;
                    if has_any_block {
                        events.push(Ok(Event::default().event("content_block_stop").data(
                            serde_json::json!({"type": "content_block_stop", "index": i})
                                .to_string(),
                        )));
                    } else {
                        events.push(Ok(Event::default().event("content_block_start").data(
                            serde_json::json!({
                                "type": "content_block_start",
                                "index": i,
                                "content_block": {"type": "text", "text": ""}
                            })
                            .to_string(),
                        )));
                        events.push(Ok(Event::default().event("content_block_stop").data(
                            serde_json::json!({"type": "content_block_stop", "index": i})
                                .to_string(),
                        )));
                    }
                }
                // 结束事件（工具调用时 stop_reason=tool_use）
                let stop_reason = if has_tool { "tool_use" } else { "end_turn" };
                events.push(Ok(Event::default().event("message_delta").data(
                    serde_json::json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                        "usage": {"output_tokens": completion_tokens}
                    })
                    .to_string(),
                )));
                events.push(Ok(Event::default()
                    .event("message_stop")
                    .data(serde_json::json!({"type": "message_stop"}).to_string())));
                events
            })
            .map(futures::stream::iter)
            .flatten();

            let combined = prefix.chain(middle).chain(suffix);

            // B05：包装守卫流——流被 drop（含客户端断连）时兜底计费
            let guarded = super::openai::GuardedStream {
                inner: Box::pin(combined),
                _guard: super::openai::StreamUsageGuard::new(billing),
                // Anthropic 数据面暂未登记在途请求，不参与「终止」
                cancel: None,
                _inflight: None,
            };
            Sse::new(guarded)
                // Anthropic 上游自带 `event: ping`，但那是**上游**的心跳，
                // 不会自动转发（bridge 里 ping 被丢弃，见 parse_stream_event）。
                // 这里给下游补 SSE 注释行保活，语义与 OpenAI 侧一致：
                // 注释行被客户端忽略，但能刷新代理/CDN 的空闲计时器。
                .keep_alive(
                    axum::response::sse::KeepAlive::new()
                        .interval(crate::bridge::SSE_KEEPALIVE_INTERVAL)
                        .text(crate::bridge::SSE_KEEPALIVE_TEXT),
                )
                .into_response()
        }
    } else {
        // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
        let mut response_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        // P1-7：调度决策回放——记录候选渠道与被过滤渠道（与 openai 非流式路径对齐）
        let candidate_channel_ids: Vec<String> = candidates
            .iter()
            .filter_map(|(_, cid, _)| cid.clone())
            .collect();
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // #32 路由回放：真实上游调用计数 + 亲和命中判定（同 resolve 口径）
        let mut attempt_counter: u32 = 0;
        let affinity_hit_id = session_id
            .as_deref()
            .and_then(|sid| state.channel_store.affinity_cache().lookup(sid, &model));
        for (bridge, cid, ch_ref) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
            let upstream =
                super::openai::resolve_upstream_model(&model, ch_ref.as_ref(), &state.model_mapper);
            if upstream != model && super::openai::ensure_model_priced(&state, &upstream).is_err() {
                tracing::warn!(
                    "messages mapping: {model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            attempt_counter += 1;
            let attempt_index = attempt_counter;
            let attempt_started_at = chrono::Utc::now().timestamp();
            let sticky_hit = affinity_hit_id.as_deref() == cid.as_deref();
            let attempt_start = std::time::Instant::now();
            match bridge.chat(&attempt_req, &ctx).await {
                Ok(resp) => {
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
                    // #32 路由回放：非流式成功轮明细
                    if route_capture.is_some() {
                        route_capture
                            .as_mut()
                            .unwrap()
                            .push_attempt(crate::log::AttemptDetail {
                                index: attempt_index,
                                channel_id: cid.clone().unwrap_or_default(),
                                channel_name: ch_ref
                                    .as_ref()
                                    .map(|c| c.name.clone())
                                    .unwrap_or_default(),
                                provider: ch_ref
                                    .as_ref()
                                    .map(|c| c.channel_type.as_str().to_string())
                                    .unwrap_or_default(),
                                account: String::new(),
                                model: upstream.clone(),
                                sticky_hit,
                                started_at: attempt_started_at,
                                finished_at: Some(chrono::Utc::now().timestamp()),
                                status: Some(200),
                                error: None,
                                notice: None,
                            });
                    }
                    response_opt = Some(resp);
                    used_channel_id = cid;
                    used_upstream = Some(upstream);
                    break;
                }
                Err(e) => {
                    // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                    if let Some(c) = &cid {
                        // P1-7：记录被过滤的渠道及原因
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&upstream),
                            crate::channel::ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    // #32 路由回放：失败轮明细
                    if route_capture.is_some() {
                        route_capture
                            .as_mut()
                            .unwrap()
                            .push_attempt(crate::log::AttemptDetail {
                                index: attempt_index,
                                channel_id: cid.clone().unwrap_or_default(),
                                channel_name: ch_ref
                                    .as_ref()
                                    .map(|c| c.name.clone())
                                    .unwrap_or_default(),
                                provider: ch_ref
                                    .as_ref()
                                    .map(|c| c.channel_type.as_str().to_string())
                                    .unwrap_or_default(),
                                account: String::new(),
                                model: upstream.clone(),
                                sticky_hit,
                                started_at: attempt_started_at,
                                finished_at: Some(chrono::Utc::now().timestamp()),
                                status: e.http_status().try_into().ok(),
                                error: Some(e.to_string()),
                                notice: None,
                            });
                    }
                    if !super::openai::is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    if let Some(cid) = &cid {
                        state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                    }
                    tracing::warn!(
                        "messages failover: channel {cid:?} failed: {e}, trying next channel"
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
                // P1-7：调度决策回放字段（与 openai 失败路径对齐）
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // 失败路径也记对话快照：管理员能看到「用户问了什么导致失败」，
                // 比只有一句 error_msg 有用得多。
                {
                    let conv = crate::log::LogConversation::from_chat_messages(&chat_req.messages);
                    if !conv.is_empty() {
                        log.conversation = Some(conv);
                    }
                }
                log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                // G5：失败路径释放预留——渠道全败时解冻，防止预留永久冻结
                super::openai::release_reservation(&state, &api_key, &reservation);
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
                // T3：发给客户端的错误消息先脱敏（截断 + 凭据模式遮蔽），
                // 日志/熔断侧保留原始消息（见 error_translate::sanitize_error_message）
                return anthropic_error(
                    e.error_type(),
                    &crate::error_translate::sanitize_error_message(&e.to_string()),
                    status_code,
                )
                .into_response();
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
            // 缓存透传：Anthropic 的 input_tokens 不含 cache 分量，按计费口径
            // 归一（prompt 补上 creation+read，read 单独走缓存价）
            let billing_model = used_upstream.as_deref().unwrap_or(&model);
            let (billing_prompt, billing_cached) = response.usage.billing_tokens();
            // 两段式计费：settle_usage 结算之前预留的配额，多退少补
            let cost = super::openai::settle_usage(
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

            // 事后限流记账
            let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志（含 channel_id，问题 4）
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
            log.channel_cost = super::openai::compute_channel_cost(
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
            // P1-7：调度决策回放字段（与 openai 非流式成功路径对齐）
            // + Phase1-B 路由回放策略快照 / #32 逐次尝试明细
            log.candidate_channels = candidate_channel_ids.clone();
            log.filtered_channels = filtered_channels.clone();
            log.selected_channel = used_channel_id.clone();
            if let Some(capture) = route_capture.as_mut() {
                log.filtered_channels
                    .extend(capture.filtered.iter().cloned());
                log.route_config = capture.config.clone();
                log.route_truncated = capture.filtered_truncated;
                log.attempt_details = std::mem::take(&mut capture.attempts);
                log.attempt_details_truncated = capture.attempts_truncated;
                log.account_skips = capture
                    .account_buf
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
            }
            // 对话快照（默认记录，不依赖 log_body 开关）
            {
                let mut conv = crate::log::LogConversation::from_chat_messages(&chat_req.messages);
                conv.push_assistant(response.message.content_str());
                if !conv.is_empty() {
                    log.conversation = Some(conv);
                }
            }
            // log_body：快照 = 请求体 + 响应正文（content 优先，tool_calls 计数）
            let debug_request_body = if crate::config::log_body_enabled() {
                Some(crate::bridge::chat_format_debug_json(&chat_req))
            } else {
                None
            };
            if debug_request_body.is_some() {
                let resp_snapshot = serde_json::json!({
                    "content": response.message.content_str(),
                    "reasoning": response.message.reasoning,
                    "tool_calls_count": response.message.tool_calls.as_ref().map_or(0, |t| t.len()),
                    "usage": { "prompt_tokens": response.usage.prompt_tokens, "completion_tokens": response.usage.completion_tokens },
                });
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body,
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
            Json(anthropic_resp).into_response()
        }
    }
}

/// POST /v1/messages/count_tokens — Anthropic 计数估算端点（Claude Code 启动探测）
///
/// 不做真实 token 计数，按 chars/3.5 估算并标记 estimated:true。
/// 不转发上游，故跳过模型白名单校验；但过期/禁用/额度/IP/用户余额仍校验
/// （用 validate_listing，此前用 validate 只查 is_active，被禁用或额度耗尽
/// 的 Key 仍可调，属鉴权口径不一致、可枚举 Key 有效性）。
pub async fn handle_count_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let key = match extract_api_key(&headers) {
        Some(k) => k,
        None => {
            return anthropic_error(
                "authentication_error",
                "Missing API key",
                StatusCode::UNAUTHORIZED,
            )
            .into_response()
        }
    };
    let client_ip = extract_client_ip(&headers);
    match state
        .api_key_store
        .validate_listing(&key, client_ip.as_deref())
    {
        Ok(_) => {}
        Err(e) => {
            let (msg, code) = match &e {
                crate::api::auth::ApiKeyError::Disabled => {
                    ("API key is disabled".to_string(), StatusCode::FORBIDDEN)
                }
                crate::api::auth::ApiKeyError::QuotaExhausted
                | crate::api::auth::ApiKeyError::UserQuotaExhausted => (
                    "API key quota exhausted".to_string(),
                    StatusCode::PAYMENT_REQUIRED,
                ),
                crate::api::auth::ApiKeyError::IpNotAllowed(ip) => (
                    format!("IP '{ip}' is not allowed for this API key"),
                    StatusCode::FORBIDDEN,
                ),
                _ => ("Invalid API key".to_string(), StatusCode::UNAUTHORIZED),
            };
            return anthropic_error("authentication_error", &msg, code).into_response();
        }
    }

    let mut total_chars: usize = 0;
    if let Some(msgs) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in msgs {
            if let Some(content) = msg.get("content") {
                if let Some(text) = content.as_str() {
                    total_chars += text.chars().count();
                } else if let Some(blocks) = content.as_array() {
                    for block in blocks {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            total_chars += text.chars().count();
                        }
                    }
                }
            }
        }
    }

    let estimated = ((total_chars as f64) / 3.5).round() as u64;
    Json(serde_json::json!({
        "count": estimated,
        "estimated": true
    }))
    .into_response()
}
