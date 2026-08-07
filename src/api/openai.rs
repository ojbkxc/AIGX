use axum::{
    extract::{Path, State},
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
use std::sync::Arc;

use crate::account::AccountPool;
use crate::bridge::{
    Bridge, BridgeContext, ChatFormat, ChatMessage, EmbeddingRequest, FinishReason, Role,
};
use crate::channel::ChannelStore;
use crate::config::ConfigManager;
use crate::hub::Hub;
use crate::log::LogStore;
use crate::model::ModelMapper;
use crate::notify::NotifyService;
use crate::payment::order_store::OrderStore;
use crate::pricing::PricingStore;
use crate::proxy::CfApiClient;
use crate::ratelimit::RateLimiter;
use crate::redemption::RedemptionStore;
use crate::usage::UsageTracker;
use crate::user::UserStore;
use crate::user_group::UserGroupStore;
use crate::health::{HealthTracker, LivezState};

// SeaORM 数据库连接（仅当启用 sea-orm feature 时可用）
#[cfg(feature = "sea-orm")]
use sea_orm::DatabaseConnection;

use super::auth::ApiKeyStore;

/// 共享应用状态
#[derive(Clone)]
pub struct AppState {
    pub api_client: Arc<CfApiClient>,
    pub model_mapper: Arc<ModelMapper>,
    pub usage_tracker: Arc<UsageTracker>,
    pub account_pool: Arc<AccountPool>,
    pub api_key_store: Arc<ApiKeyStore>,
    pub config_manager: Arc<ConfigManager>,
    pub hub: Arc<Hub>,
    pub user_store: Arc<UserStore>,
    pub order_store: Arc<OrderStore>,
    pub epay_client: Arc<crate::payment::EpayClient>,
    pub health_tracker: Arc<HealthTracker>,
    pub livez_state: Arc<LivezState>,
    /// 通用渠道存储（混用 CF + 第三方 OpenAI 兼容上游）
    pub channel_store: Arc<ChannelStore>,
    /// 模型定价目录
    pub pricing_store: Arc<PricingStore>,
    /// 用户分组存储
    pub user_group_store: Arc<UserGroupStore>,
    /// 日志与审计存储
    pub log_store: Arc<LogStore>,
    /// 兑换码存储
    pub redemption_store: Arc<RedemptionStore>,
    /// 限流器（多维度 RPM/TPM）
    pub rate_limiter: Arc<RateLimiter>,
    /// 通知服务（Telegram + SMTP）
    pub notify_service: Arc<NotifyService>,
    /// 公开注册速率限制器（per-IP 计数缓存）。
    ///
    /// key=客户端 IP，value=当前 60 秒窗口内已发起的注册请求数。
    /// TTL=60s，超限（>5）返回 429 Too Many Requests。
    pub register_limiter: Arc<moka::future::Cache<String, u32>>,
    /// SeaORM 数据库连接（可选后端）。
    ///
    /// - `None`：使用默认 FileStore（rusqlite bundled SQLite），零配置
    /// - `Some`：启用 SeaORM 后端（SQLite/PostgreSQL/MySQL），新数据写入 SeaORM
    ///
    /// 仅当启用 `sea-orm` feature 且 `config.database.url` 非空时为 `Some`。
    #[cfg(feature = "sea-orm")]
    pub db_conn: Option<DatabaseConnection>,
}

impl AppState {
    /// 返回是否启用了 SeaORM 后端。
    ///
    /// 仅当 `db_conn` 为 `Some` 时返回 true。
    /// 未启用 `sea-orm` feature 时始终返回 false。
    pub fn has_sea_orm_backend(&self) -> bool {
        #[cfg(feature = "sea-orm")]
        {
            self.db_conn.is_some()
        }
        #[cfg(not(feature = "sea-orm"))]
        {
            false
        }
    }
}

/// 创建 OpenAI 兼容的错误响应
fn error_response(code: &str, message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "param": null,
                "code": code
            }
        })),
    )
}

/// 从请求中提取 API Key
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(key_str) = key.to_str() {
            return Some(key_str.to_string());
        }
    }
    None
}

/// 从请求头提取客户端 IP（取 X-Forwarded-For 首段或 X-Real-IP）
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

/// 验证请求中的 API Key（简化版，仅校验存在性与有效性，返回 key id）
fn verify_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;

    let api_key = state
        .api_key_store
        .validate(&key)
        .ok_or_else(|| error_response("auth_error", "Invalid API key", StatusCode::UNAUTHORIZED))?;

    Ok(api_key.id)
}

/// 验证 API Key 并执行全部鉴权检查（状态/过期/模型白名单/额度/IP）。
///
/// 参照 new-api token.go 的校验逻辑。返回 ApiKey 实例供计费使用。
fn verify_api_key_full(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<super::auth::ApiKey, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;
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
            error_response("auth_error", &msg, status)
        })
}

/// 根据模型名解析提供商并获取对应的 Bridge。
///
/// 调度逻辑（参照 aisix dispatch_two_tier + new-api channel 选取）：
/// 1. 先查 ChannelStore 是否有支持该 model 的 openai_compatible/anthropic 渠道 → 动态构造 Bridge
/// 2. 回退到 Hub 专用提供商（cloudflare）
pub fn resolve_bridge(state: &AppState, model: &str) -> Option<(Arc<dyn Bridge>, Option<String>)> {
    // 第一级：通用渠道（按 priority/weight 选取支持该 model 的渠道）
    let candidates = state.channel_store.select_for_model(model);
    for ch in &candidates {
        match ch.channel_type {
            crate::channel::ChannelType::OpenaiCompatible => {
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    return Some((
                        crate::bridge::openai::make_bridge(&ch.base_url, &key),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Anthropic => {
                // Anthropic 兼容暂复用 OpenAI bridge（多数上游兼容 OpenAI 协议）
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    return Some((
                        crate::bridge::openai::make_bridge(&ch.base_url, &key),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Cloudflare => {
                // CF 渠道走 Hub 专用桥接
                if let Some(b) = state.hub.get_specialized("cloudflare") {
                    return Some((b, Some(ch.id.clone())));
                }
            }
        }
    }
    // 第二级：回退到 Hub 专用提供商（cloudflare），无通用渠道 ID
    state.hub.get_specialized("cloudflare").map(|b| (b, None))
}

/// 解析计费分组：优先取绑定用户的 group，否则用 api_key.group。
///
/// 参照 new-api：用户级 group 优先于 token 级 group。
pub fn resolve_billing_group(state: &AppState, api_key: &super::auth::ApiKey) -> String {
    if let Some(uid) = &api_key.user_id {
        if let Some(user) = state.user_store.get_by_id(uid) {
            if !user.group.is_empty() {
                return user.group.clone();
            }
        }
    }
    api_key.group.clone()
}

/// 校验用户分组的模型权限，返回计费分组名。
///
/// 返回 Err 时调用方应返回 403。
pub fn check_group_model_permission(
    state: &AppState,
    api_key: &super::auth::ApiKey,
    model: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    let group = resolve_billing_group(state, api_key);
    if !state.user_group_store.allows_model(&group, model) {
        return Err(error_response(
            "model_not_allowed",
            &format!("Model '{model}' is not allowed for group '{group}'"),
            StatusCode::FORBIDDEN,
        ));
    }
    Ok(group)
}

/// 执行计费扣减（用户 quota + key used_quota）。
///
/// 参照 new-api：优先扣 key 绑定用户 quota，再扣 key 自身额度；
/// 用户余额不足时跳过 key 扣费以保持计费一致性。
///
/// 当 `try_charge` 失败或扣费后余额低于阈值时，发送 `QuotaLow` 通知
/// （参照非流式分支阈值计算，与 handle_chat_completions 非流式分支一致）。
pub fn charge_usage(
    state: &AppState,
    api_key: &super::auth::ApiKey,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> i64 {
    let cost = state
        .pricing_store
        .calculate_cost_quoted(model, prompt_tokens, completion_tokens, group);
    if cost > 0 {
        if let Some(uid) = &api_key.user_id {
            // 用户余额不足时跳过 key 扣费（问题 6）
            let charged = state.user_store.try_charge(uid, cost);
            // 额度不足或剩余过低通知（与非流式分支一致）
            if let Some(u) = state.user_store.get_by_id(uid) {
                let remaining = u.remaining();
                // 阈值：固定 1000 或 quota 的 10%，取较小者；扣费失败必通知
                let threshold = (u.quota / 10).max(1000).min(10000);
                if !charged || remaining < threshold {
                    state.notify_service.notify_spawn(
                        crate::notify::NotifyEvent::QuotaLow {
                            user_email: u.email.clone(),
                            remaining,
                        },
                    );
                }
            }
            // 问题 6：try_charge 失败时跳过 charge_quota，保持计费一致性
            if charged {
                let _ = state.api_key_store.charge_quota(&api_key.id, cost);
            }
        } else {
            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
        }
    }
    cost
}

/// 将 JSON Value 的消息数组解析为 ChatMessage 列表
fn parse_messages(value: Option<&Value>) -> Option<Vec<ChatMessage>> {
    let arr = value?.as_array()?;
    let mut messages = Vec::new();
    for msg in arr {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some("system") => Role::System,
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        let name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        messages.push(ChatMessage {
            role,
            content,
            name,
            tool_call_id,
        });
    }
    Some(messages)
}

/// 将 FinishReason 转换为 OpenAI 兼容的字符串
fn finish_reason_str(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::ToolCalls => "tool_calls",
    }
}

/// 将 BridgeError 转换为 HTTP 响应
fn bridge_error_response(e: crate::bridge::BridgeError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(e.error_type(), &e.to_string(), status)
}

/// POST /v1/chat/completions - 聊天补全
///
/// 使用 Bridge/Hub 模式分发请求，参考 aisix 的 chat_completions 处理流程：
/// 1. 认证 → 2. 解析 ChatFormat → 3. Hub 分发 Bridge → 4. 调用 Bridge::chat/chat_stream
pub async fn handle_chat_completions(
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
            return error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP（参照 new-api token.go）
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state.rate_limiter.check(
        &api_key.id,
        &model,
        api_key.user_id.as_deref(),
        client_ip.as_deref(),
    ).await {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ).into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = match check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // 通过 ChannelStore/Hub 获取 Bridge
    let (bridge, channel_id) = match resolve_bridge(&state, &model) {
        Some(pair) => pair,
        None => {
            return error_response(
                "no_bridge",
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

    // 解析消息列表
    let messages = parse_messages(body.get("messages")).unwrap_or_default();
    if messages.is_empty() {
        return error_response("invalid_messages", "No valid messages", StatusCode::BAD_REQUEST)
            .into_response();
    }

    // 构建 ChatFormat
    let chat_req = ChatFormat {
        model: model.clone(),
        messages,
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

                let sse_stream = stream.map(move |chunk_result| match chunk_result {
                    Ok(chunk) => {
                        // 累积内容文本用于事后估算 completion tokens
                        if let Some(text) = &chunk.delta.content {
                            let mut buf = acc_for_map.lock();
                            crate::token_estimate::push_capped(&mut buf, text);
                        }
                        let sse_data = serde_json::json!({
                            "id": chunk.id,
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": chunk.model,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "content": chunk.delta.content.unwrap_or_default(),
                                },
                                "finish_reason": chunk.finish_reason.as_ref().map(|fr| finish_reason_str(fr)),
                            }]
                        })
                        .to_string();
                        Ok::<_, Infallible>(Event::default().data(sse_data))
                    }
                    Err(e) => {
                        tracing::error!("Stream chunk error: {e}");
                        // 标准 SSE error 事件格式（与 OpenAI API 错误结构一致）：
                        //   event: error
                        //   data: {"error":{"message":"...","type":"...","code":"..."}}
                        let err_data = serde_json::json!({
                            "error": {
                                "message": e.to_string(),
                                "type": e.error_type(),
                                "code": e.error_type(),
                            }
                        })
                        .to_string();
                        Ok(Event::default().event("error").data(err_data))
                    }
                });

                // 流结束：估算 token、扣费、commit_tokens、记录日志（问题 3）
                let state_fin = state.clone();
                let api_key_fin = api_key.clone();
                let model_fin = model.clone();
                let client_ip_fin = client_ip.clone();
                let request_id_fin = request_id.clone();
                let group_fin = billing_group.clone();
                let channel_id_fin = channel_id.clone();
                let chat_req_fin = chat_req.clone();
                let final_event = async move {
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
                    let cost = charge_usage(
                        &state_fin,
                        &api_key_fin,
                        &model_fin,
                        &group_fin,
                        prompt_tokens,
                        completion_tokens,
                    );

                    // 事后限流记账（TPM）
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

                    Ok::<_, Infallible>(Event::default().data("[DONE]"))
                };
                let combined = sse_stream.chain(futures::stream::once(final_event));

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
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.clone(),
                        error: e.to_string(),
                    });
                }
                bridge_error_response(e).into_response()
            }
        }
    } else {
        match bridge.chat(&chat_req, &ctx).await {
            Ok(response) => {
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

                // 计费扣减（用解析的 billing_group，问题 5；余额不足跳过 key 扣费，问题 6）
                let cost = state.pricing_store.calculate_cost_quoted(
                    &model,
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    &billing_group,
                );
                if cost > 0 {
                    if let Some(uid) = &api_key.user_id {
                        let charged = state.user_store.try_charge(uid, cost);
                        // 额度不足或剩余过低通知
                        if let Some(u) = state.user_store.get_by_id(uid) {
                            let remaining = u.remaining();
                            // 阈值：固定 1000 或 quota 的 10%，取较小者；扣费失败必通知
                            let threshold = (u.quota / 10).max(1000).min(10000);
                            if !charged || remaining < threshold {
                                state.notify_service.notify_spawn(
                                    crate::notify::NotifyEvent::QuotaLow {
                                        user_email: u.email.clone(),
                                        remaining,
                                    },
                                );
                            }
                        }
                        // 问题 6：try_charge 失败时跳过 charge_quota，保持计费一致性
                        if charged {
                            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
                        }
                    } else {
                        let _ = state.api_key_store.charge_quota(&api_key.id, cost);
                    }
                }

                // 事后限流记账（TPM）
                let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
                rate_bundle.commit_tokens(total_tokens).await;

                // 记录请求日志（功能 1）
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

                let json = serde_json::json!({
                    "id": response.id,
                    "object": "chat.completion",
                    "created": chrono::Utc::now().timestamp(),
                    "model": response.model,
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": response.message.content_str(),
                        },
                        "finish_reason": finish_reason_str(&response.finish_reason),
                    }],
                    "usage": {
                        "prompt_tokens": response.usage.prompt_tokens,
                        "completion_tokens": response.usage.completion_tokens,
                        "total_tokens": response.usage.total_tokens,
                    }
                });
                Json(json).into_response()
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
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.clone(),
                        error: e.to_string(),
                    });
                }
                bridge_error_response(e).into_response()
            }
        }
    }
}

/// POST /v1/completions - 文本补全
///
/// 通过 Bridge::complete 委托给 Bridge 处理
pub async fn handle_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST))?;

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, model)?;

    let (bridge, channel_id) = resolve_bridge(&state, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;
    // 标记渠道已使用（问题 4）
    if let Some(cid) = &channel_id {
        state.channel_store.mark_used(cid);
    }

    let ctx = BridgeContext::new(request_id.clone(), model.to_string());

    match bridge.complete(&body, &ctx).await {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let prompt_tokens = result
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion_tokens = result
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            state
                .usage_tracker
                .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

            // 计费扣减（问题 2/5/6）
            let cost = charge_usage(
                &state,
                &api_key,
                model,
                &billing_group,
                prompt_tokens,
                completion_tokens,
            );

            // 事后限流记账（TPM）
            let total_tokens = prompt_tokens + completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志（功能 1）
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(result))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.to_string(),
                        error: e.to_string(),
                    });
            }
            Err(bridge_error_response(e))
        }
    }
}

/// POST /v1/embeddings - 向量嵌入
///
/// 通过 Bridge::embed 委托给 Bridge 处理
pub async fn handle_embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST))?;

    let api_key = verify_api_key_full(&state, &headers, model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, model)?;

    let (bridge, channel_id) = resolve_bridge(&state, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;
    // 标记渠道已使用（问题 4）
    if let Some(cid) = &channel_id {
        state.channel_store.mark_used(cid);
    }

    let input = body.get("input");
    let texts: Vec<String> = match input {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return Err(error_response(
                "invalid_input",
                "Invalid input field",
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let embed_req = EmbeddingRequest {
        model: model.to_string(),
        input: texts,
    };
    let ctx = BridgeContext::new(request_id.clone(), model.to_string());

    match bridge.embed(&embed_req, &ctx).await {
        Ok(response) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let data: Vec<Value> = response
                .data
                .iter()
                .map(|obj| {
                    serde_json::json!({
                        "object": "embedding",
                        "index": obj.index,
                        "embedding": obj.embedding,
                    })
                })
                .collect();

            let prompt_tokens = response.usage.prompt_tokens as u64;
            state
                .usage_tracker
                .accumulate(prompt_tokens, 0, 0, 0, 0, 0.0);

            // 计费扣减（问题 2/5/6）
            let cost = charge_usage(&state, &api_key, model, &billing_group, prompt_tokens, 0);

            // 事后限流记账（TPM）
            rate_bundle.commit_tokens(prompt_tokens).await;

            // 记录请求日志（功能 1）
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.input_tokens = prompt_tokens;
            log.output_tokens = 0;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(serde_json::json!({
                "object": "list",
                "data": data,
                "model": model,
                "usage": {
                    "prompt_tokens": response.usage.prompt_tokens,
                    "total_tokens": response.usage.total_tokens,
                }
            })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.to_string(),
                        error: e.to_string(),
                    });
            }
            Err(bridge_error_response(e))
        }
    }
}

/// POST /v1/images/generations - 图片生成
///
/// 通过 Bridge::generate_image 委托给 Bridge 处理
pub async fn handle_images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST))?;

    let api_key = verify_api_key_full(&state, &headers, model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, model)?;

    let (bridge, channel_id) = resolve_bridge(&state, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;
    // 标记渠道已使用（问题 4）
    if let Some(cid) = &channel_id {
        state.channel_store.mark_used(cid);
    }

    let ctx = BridgeContext::new(request_id.clone(), model.to_string());

    match bridge.generate_image(&body, &ctx).await {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, model, &billing_group, 0, 0);

            // 事后限流记账（按次计价，记 1 个请求 token 占位以维持 RPM 一致性）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 图片生成按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.input_tokens = 0;
            log.output_tokens = 0;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(result))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = channel_id.clone();
            log.model = model.to_string();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model.to_string(),
                        error: e.to_string(),
                    });
            }
            Err(bridge_error_response(e))
        }
    }
}

/// POST /v1/audio/transcriptions - 语音转文字
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type.split("boundary=").nth(1).map(|s| s.to_string()).ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing boundary in content-type",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return Err(error_response(
                "read_error",
                &format!("Failed to read body: {e}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let (audio_data, model, filename) = parse_multipart_audio(&bytes, &boundary)?;

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, &model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, &model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, &model)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(&model, audio_data.to_vec(), &mime_type)
        .await
    {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, &model, &billing_group, 0, 0);

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 音频转写按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Transcription error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model.clone(),
                    error: format!("Transcription error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Transcription error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

/// POST /v1/audio/translations - 语音翻译
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_translations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type.split("boundary=").nth(1).map(|s| s.to_string()).ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing boundary in content-type",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return Err(error_response(
                "read_error",
                &format!("Failed to read body: {e}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let (audio_data, model, filename) = parse_multipart_audio(&bytes, &boundary)?;

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, &model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, &model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, &model)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(&model, audio_data.to_vec(), &mime_type)
        .await
    {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, &model, &billing_group, 0, 0);

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 音频翻译按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Translation error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model.clone(),
                    error: format!("Translation error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Translation error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

/// POST /v1/audio/speech - 文本转语音
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_speech(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("tts");

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, model)?;

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(&api_key.id, model, api_key.user_id.as_deref(), client_ip.as_deref())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, model)?;

    let input = body
        .get("input")
        .and_then(|i| i.as_str())
        .ok_or_else(|| error_response("invalid_input", "Missing input field", StatusCode::BAD_REQUEST))?;

    let voice = body
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let cf_body = serde_json::json!({
        "input": input,
        "voice": voice,
    });

    match state.api_client.run_text(model, cf_body).await {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let audio_data = result.get("audio").and_then(|a| a.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, model, &billing_group, 0, 0);

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 文本转语音按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.to_string();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            Ok(Json(serde_json::json!({
                "audio_base64": audio_data,
                "content_type": "audio/wav"
            })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.to_string();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Text-to-speech error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model.to_string(),
                    error: format!("Text-to-speech error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Text-to-speech error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}

/// GET /v1/models - 模型列表
pub async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    let now = chrono::Utc::now().timestamp();

    let model_list: Vec<Value> = mappings
        .into_iter()
        .map(|(name, cf_model)| {
            let owned_by = crate::proxy::get_model_owned_by(&cf_model);
            serde_json::json!({
                "id": name,
                "object": "model",
                "created": now,
                "owned_by": owned_by
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "object": "list",
        "data": model_list
    })))
}

/// GET /v1/models/{model} - 模型详情
pub async fn handle_get_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    if mappings.contains_key(&model) {
        let now = chrono::Utc::now().timestamp();
        let cf_model = state.model_mapper.resolve(&model);
        let owned_by = crate::proxy::get_model_owned_by(&cf_model);
        Ok(Json(serde_json::json!({
            "id": model,
            "object": "model",
            "created": now,
            "owned_by": owned_by,
            "permission": []
        })))
    } else {
        Err(error_response(
            "model_not_found",
            &format!("Model '{model}' not found"),
            StatusCode::NOT_FOUND,
        ))
    }
}

/// 解析 multipart 表单中的音频文件
fn parse_multipart_audio(
    bytes: &bytes::Bytes,
    boundary: &str,
) -> Result<(bytes::Bytes, String, String), (StatusCode, Json<Value>)> {
    let mut file_data: Option<(bytes::Bytes, String)> = None;
    let mut model = String::from("whisper");

    let content_str = String::from_utf8_lossy(bytes);
    let boundary_tag = format!("--{}", boundary);
    let parts: Vec<&str> = content_str.split(&boundary_tag).collect();

    for part in parts {
        if part.is_empty() || part.trim() == "--" || part.trim() == "-" {
            continue;
        }

        if let Some(body_start) = part.find("\r\n\r\n") {
            let header_section = &part[..body_start];
            let body_content = &part[body_start + 4..];
            let body_content = body_content.trim_end_matches('\r').trim_end_matches('\n');

            if header_section.contains("name=\"model\"") {
                model = body_content.trim().to_string();
            } else if header_section.contains("name=\"file\"") || header_section.contains("name=\"audio\"") {
                let filename = header_section
                    .split(';')
                    .find_map(|s| {
                        let s = s.trim();
                        s.strip_prefix("filename=\"")
                            .or_else(|| s.strip_prefix("filename="))
                            .map(|f| f.trim_matches('"').to_string())
                    })
                    .unwrap_or_else(|| "audio.wav".to_string());

                let data = bytes::Bytes::copy_from_slice(body_content.as_bytes());
                file_data = Some((data, filename));
            }
        }
    }

    file_data.ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing audio file in request",
            StatusCode::BAD_REQUEST,
        )
    })
    .map(|(data, filename)| (data, model, filename))

}
