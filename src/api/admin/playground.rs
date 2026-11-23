//! Playground API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供在线聊天 Playground 功能，直接测试渠道连接。

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

/// Playground 统一记账：扣费（普通用户）+ 落请求日志（所有用户都落，
/// 管理员 cost=0）+ usage 统计。
///
/// key_id 用 `playground:<kind>` 标注来源（用量日志页展示为来源标签）：
/// chat / images / tts / transcriptions。
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_playground_usage(
    state: &AppState,
    user_id: &str,
    is_admin: bool,
    kind: &str,
    model: &str,
    channel_id: Option<&str>,
    channel_name: Option<&str>,
    prompt_tokens: u64,
    completion_tokens: u64,
    latency_ms: u64,
) {
    let Some(u) = state.user_store.get_by_id(user_id) else {
        return;
    };
    let group = u.group.clone();
    // 计价（失败按 0，与 charge_usage 的兜底一致）
    let cost = state
        .pricing_store
        .calculate_cost_quoted(model, prompt_tokens, completion_tokens, &group)
        .unwrap_or(0);
    if cost > 0 && !is_admin {
        // 订阅池优先 + 钱包兜底（与数据面 charge_usage 相同顺序）
        let now = chrono::Utc::now().timestamp();
        let mut remaining = cost;
        for sub in state.subscription_store.find_active(user_id, now) {
            if remaining <= 0 {
                break;
            }
            let pool = if sub.amount_total > 0 {
                sub.amount_total - sub.amount_used
            } else {
                remaining
            };
            let take = pool.min(remaining);
            if take > 0 && state.subscription_store.try_charge(&sub.id, take) {
                remaining -= take;
            }
        }
        if remaining > 0 && !state.user_store.try_charge(user_id, remaining) {
            // 入口预检挡住了「余额为 0」，但余额不足以覆盖**本次**成本时仍会走到
            // 这里：上游已经产出、响应已经构造，此时改 402 会丢掉用户已经拿到的
            // 结果。改为记一条安全事件，让管理员能看到「有用户长期欠费跑量」，
            // 而不是只留一行 warn 埋在日志里。
            tracing::warn!("playground charge failed for user {user_id} (insufficient quota)");
            state.log_store.record_security(crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::RateLimit,
                "warning",
                format!(
                    "Playground 欠费：用户 {user_id} 的余额不足以支付本次 {kind} 调用（应扣 {remaining}）"
                ),
            ));
        }
    }
    // 落请求日志（管理员可在日志页看到 Playground 消耗，来源渠道标注；
    // 管理员豁免计费 → cost=0）
    let log = crate::log::RequestLog {
        purge_applied: false,
        special_route: None,
        id: uuid::Uuid::new_v4().to_string(),
        user_id: Some(user_id.to_string()),
        key_id: Some(format!("playground:{kind}")),
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: channel_name.map(|s| s.to_string()),
        model: model.to_string(),
        origin_model: Some(model.to_string()),
        input_tokens: prompt_tokens,
        output_tokens: completion_tokens,
        cost: if is_admin { 0 } else { cost },
        channel_cost: cost,
        latency_ms,
        status_code: 200,
        error_msg: None,
        ip: None,
        client_name: crate::api::client_id::SELF_CLIENT.to_string(),
        client_version: String::new(),
        request_id: None,
        created_at: chrono::Utc::now().timestamp(),
        candidate_channels: Vec::new(),
        cache_hit: false,
        conversation: None,
        filtered_channels: Vec::new(),
        selected_channel: None,
        debug: None,
        first_response_ms: None,
        attempts: 1,
        sensitive_hits: Vec::new(),
        phase: String::new(),
        route_config: None,
        route_truncated: 0,
        attempt_details: Vec::new(),
        attempt_details_truncated: 0,
        account_skips: Vec::new(),
    };
    if let Err(e) = state.log_store.requests.add(log) {
        tracing::warn!("playground request log write failed: {e}");
    }
    // 全局 usage 统计同步累加
    state
        .usage_tracker
        .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);
}

/// 从响应 JSON 中提取 usage 的 prompt/completion tokens（无 usage 时按 0）
pub(crate) fn extract_usage_tokens(j: &Value) -> (u64, u64) {
    let p = j
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let c = j
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    (p, c)
}

/// Playground 失败落日志（不计费、不进 usage 统计）。
///
/// 状态码 + 上游错误体进 error_msg，用量日志页可见失败原因。
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_playground_failure(
    state: &AppState,
    user_id: &str,
    kind: &str,
    model: &str,
    channel_id: Option<&str>,
    channel_name: Option<&str>,
    status_code: u16,
    upstream_error: &str,
    latency_ms: u64,
) {
    let log = crate::log::RequestLog {
        purge_applied: false,
        special_route: None,
        id: uuid::Uuid::new_v4().to_string(),
        user_id: Some(user_id.to_string()),
        key_id: Some(format!("playground:{kind}")),
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: channel_name.map(|s| s.to_string()),
        model: model.to_string(),
        origin_model: Some(model.to_string()),
        input_tokens: 0,
        output_tokens: 0,
        cost: 0,
        channel_cost: 0,
        latency_ms,
        status_code,
        error_msg: Some(upstream_error.chars().take(500).collect()),
        ip: None,
        client_name: crate::api::client_id::SELF_CLIENT.to_string(),
        client_version: String::new(),
        request_id: None,
        created_at: chrono::Utc::now().timestamp(),
        candidate_channels: Vec::new(),
        cache_hit: false,
        conversation: None,
        filtered_channels: Vec::new(),
        selected_channel: None,
        debug: None,
        first_response_ms: None,
        attempts: 1,
        sensitive_hits: Vec::new(),
        phase: String::new(),
        route_config: None,
        route_truncated: 0,
        attempt_details: Vec::new(),
        attempt_details_truncated: 0,
        account_skips: Vec::new(),
    };
    if let Err(e) = state.log_store.requests.add(log) {
        tracing::warn!("playground failure log write failed: {e}");
    }
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct PlaygroundChatRequest {
    pub channel_id: Option<String>,
    pub model: String,
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
    /// P1 Playground V2：Completions 模式用顶层 prompt；缺省走 chat messages
    #[serde(default)]
    pub prompt: Option<String>,
    /// 参数透传（chat 与 completions 共用）：None 时不写入上游
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub response_format: Option<serde_json::Value>,
    /// JSON mode 快捷开关：response_format = { "type": "json_object" }
    #[serde(default)]
    pub json_mode: Option<bool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
}

/// Playground 入口的公共准入校验（非管理员）：
/// 1. 余额必须 > 0（计费发生在上游响应之后，0 余额会白嫖站方成本）
/// 2. 模型必须在用户分组白名单内（与数据面 `check_group_model_permission` 同口径）
///
/// 抽成一处是因为 chat / images / tts / transcriptions 四个入口都要做，
/// 分散实现迟早会漏掉某个（改造前 images/tts/transcriptions 就没有余额预检）。
pub(crate) fn playground_guard(
    state: &AppState,
    user: &crate::user::User,
    model: &str,
) -> Option<Response> {
    if user.is_admin() {
        return None;
    }
    if user.remaining() <= 0 {
        return Some(
            error_response("余额不足，请先充值", StatusCode::PAYMENT_REQUIRED).into_response(),
        );
    }
    let model = model.trim();
    if !model.is_empty() && !state.user_group_store.allows_model(&user.group, model) {
        return Some(
            error_response(
                &format!("模型 '{model}' 不在当前分组允许范围内"),
                StatusCode::FORBIDDEN,
            )
            .into_response(),
        );
    }
    None
}

/// POST /api/playground/tts - Playground 文本转语音（登录即可）
///
/// 请求体透传上游 OpenAI 兼容 /audio/speech（model/input/voice），
/// 返回 { audio_base64, content_type }，前端 <audio> 直接播放。
#[derive(Debug, Deserialize)]
pub struct PlaygroundTtsRequest {
    pub model: String,
    pub input: String,
    #[serde(default)]
    pub voice: Option<String>,
}

pub async fn handle_playground_tts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundTtsRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if body.input.trim().is_empty() {
        return error_response("input is required", StatusCode::BAD_REQUEST).into_response();
    }

    // 余额预检 + 分组模型白名单（改造前 tts 无这两项校验）
    if let Some(resp) = playground_guard(&state, &user, &body.model) {
        return resp;
    }

    // 按 model 路由到声明该模型的启用渠道（与 playground/chat 同口径）
    let requested_model = body.model.trim().to_string();
    // models 留空（=全部）的渠道按发现快照收窄（auto_pick_supports），
    // 防止高优先级“全声明”渠道抢走上游实际不提供的模型
    let enabled: Vec<crate::channel::Channel> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|c| c.is_enabled())
        .collect();
    let picked = if requested_model.is_empty() {
        enabled.first().cloned()
    } else {
        enabled
            .iter()
            .filter(|c| c.auto_pick_supports(&requested_model))
            .max_by_key(|c| c.priority)
            .cloned()
            .or_else(|| {
                enabled
                    .iter()
                    .filter(|c| c.models.is_empty())
                    .max_by_key(|c| c.priority)
                    .cloned()
            })
    };
    let ch = match picked {
        Some(c) => c,
        None => {
            return error_response(
                &format!(
                    "No enabled channel provides model '{requested_model}' \
                     (available channels do not declare it)"
                ),
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    let model = if body.model.trim().is_empty() {
        ch.models.first().cloned().unwrap_or_default()
    } else {
        body.model.trim().to_string()
    };
    // 渠道级模型映射（与数据面一致）
    let upstream = ch
        .resolve_channel_mapping(&model)
        .unwrap_or_else(|| model.clone());
    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/audio/speech");

    let mut payload = json!({ "model": upstream, "input": body.input });
    if let Some(v) = body.voice.as_deref() {
        payload["voice"] = json!(v);
    }

    let client = match reqwest::Client::builder()
        // 强制 HTTP/1.1：防 h2 PING 帧掩蔽读超时检测（同共享 client，见 main.rs）
        .http1_only()
        .timeout(std::time::Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                &format!("HTTP client error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response()
        }
    };

    let request_start = std::time::Instant::now();
    let mut req = client.post(&url).json(&payload);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                record_playground_failure(
                    &state,
                    &user.id,
                    "tts",
                    &model,
                    Some(&ch.id),
                    Some(&ch.name),
                    status.as_u16(),
                    &text,
                    request_start.elapsed().as_millis() as u64,
                );
                return error_response(
                    &format!("Upstream HTTP {status}: {text}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response();
            }
            let content_type = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("audio/mpeg")
                .to_string();
            match resp.bytes().await {
                Ok(b) => {
                    // 计费（管理员豁免）+ 日志；TTS 无 usage，按输入字符数近似估 token
                    let est_tokens = (body.input.chars().count() as u64) / 4;
                    record_playground_usage(
                        &state,
                        &user.id,
                        user.is_admin(),
                        "tts",
                        &model,
                        Some(&ch.id),
                        Some(&ch.name),
                        est_tokens,
                        0,
                        request_start.elapsed().as_millis() as u64,
                    );
                    Json(json!({
                        "success": true,
                        "data": {
                            "audio_base64": BASE64.encode(&b),
                            "content_type": content_type
                        }
                    }))
                    .into_response()
                }
                Err(e) => error_response(
                    &format!("Upstream returned no audio: {e}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response(),
            }
        }
        Err(e) => {
            error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

/// POST /api/playground/transcriptions - Playground 语音转文字（登录即可）
///
/// multipart/form-data（file + model + 可选 language）透传上游
/// /audio/transcriptions，返回上游 JSON（含 text）。
pub async fn handle_playground_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let boundary = match content_type.split("boundary=").nth(1) {
        Some(b) => b.to_string(),
        None => {
            return error_response("Missing boundary in content-type", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    let bytes =
        match axum::body::to_bytes(body, super::super::openai::current_body_limit_bytes(&state))
            .await
        {
            Ok(b) => b,
            Err(e) => {
                return error_response(
                    &format!("Failed to read body: {e}"),
                    StatusCode::BAD_REQUEST,
                )
                .into_response()
            }
        };

    let (audio_data, model, filename) =
        match super::super::openai::parse_multipart_audio(&bytes, &boundary) {
            Ok(v) => v,
            Err((_status, json_err)) => {
                let msg = json_err
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("invalid multipart")
                    .to_string();
                return error_response(&msg, StatusCode::BAD_REQUEST).into_response();
            }
        };

    // 按上传的 model 路由到声明该模型的启用渠道（与 playground/chat 同口径）
    let requested_model = model.trim().to_string();

    // 余额预检 + 分组模型白名单（改造前 transcriptions 无这两项校验）
    if let Some(resp) = playground_guard(&state, &user, &requested_model) {
        return resp;
    }

    // models 留空（=全部）的渠道按发现快照收窄（auto_pick_supports），
    // 防止高优先级“全声明”渠道抢走上游实际不提供的模型
    let enabled: Vec<crate::channel::Channel> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|c| c.is_enabled())
        .collect();
    let picked = if requested_model.is_empty() {
        enabled.first().cloned()
    } else {
        enabled
            .iter()
            .filter(|c| c.auto_pick_supports(&requested_model))
            .max_by_key(|c| c.priority)
            .cloned()
            .or_else(|| {
                enabled
                    .iter()
                    .filter(|c| c.models.is_empty())
                    .max_by_key(|c| c.priority)
                    .cloned()
            })
    };
    let ch = match picked {
        Some(c) => c,
        None => {
            return error_response(
                &format!(
                    "No enabled channel provides model '{requested_model}' \
                     (available channels do not declare it)"
                ),
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    let model = if model.trim().is_empty() {
        ch.models.first().cloned().unwrap_or_default()
    } else {
        model
    };
    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/audio/transcriptions");

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    let client = match reqwest::Client::builder()
        // 强制 HTTP/1.1：防 h2 PING 帧掩蔽读超时检测（同共享 client，见 main.rs）
        .http1_only()
        .timeout(std::time::Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                &format!("HTTP client error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response()
        }
    };

    let part = reqwest::multipart::Part::bytes(audio_data.to_vec())
        .file_name(filename)
        .mime_str(&mime_type)
        .unwrap_or_else(|_| {
            reqwest::multipart::Part::bytes(audio_data.to_vec()).file_name("audio.webm")
        });
    let form = reqwest::multipart::Form::new()
        .text("model", model.clone())
        .part("file", part);

    let request_start = std::time::Instant::now();
    let mut req = client.post(&url).multipart(form);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                record_playground_failure(
                    &state,
                    &user.id,
                    "transcriptions",
                    &model,
                    Some(&ch.id),
                    Some(&ch.name),
                    status.as_u16(),
                    &text,
                    request_start.elapsed().as_millis() as u64,
                );
                return error_response(
                    &format!("Upstream HTTP {status}: {text}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response();
            }
            match resp.json::<Value>().await {
                Ok(j) => {
                    // 计费（管理员豁免）+ 日志；转写按音频字节数近似估 token
                    let est_tokens = (audio_data.len() as u64) / 1024;
                    record_playground_usage(
                        &state,
                        &user.id,
                        user.is_admin(),
                        "transcriptions",
                        &model,
                        Some(&ch.id),
                        Some(&ch.name),
                        est_tokens,
                        0,
                        request_start.elapsed().as_millis() as u64,
                    );
                    Json(json!({ "success": true, "data": j })).into_response()
                }
                Err(e) => error_response(
                    &format!("Upstream returned non-JSON: {e}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response(),
            }
        }
        Err(e) => {
            error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}
