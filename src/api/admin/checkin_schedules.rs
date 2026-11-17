// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 定时签到**条目**的路由（对齐 Agent2API `api/checkin_schedules.rs`）。
//!
//! ```text
//!   GET  /api/checkin-schedules                状态（总开关 + 条目数组 + 提供商清单）
//!   POST /api/checkin-schedules                保存一条（按 id upsert）
//!   POST /api/checkin-schedules/{id}/delete    删除一条（DELETE 方法同受理）
//!   POST /api/checkin-schedules/{id}/run       立即执行这一条
//!   GET  /api/auto-checkin                     自动签到状态（兼容视图）
//!   POST /api/auto-checkin                     修改总开关 / 时刻 / 范围
//!   POST /api/auto-checkin/run                 立即执行一次（全部）
//! ```
//!
//! ## 为什么 `/api/checkin-schedules` 与 `/api/auto-checkin` 是两组接口
//!
//! `autoCheckin.enabled` 是**总开关**（旧契约）；条目数组是纯新增的配置形状。
//! 强并成一套会让老接口的 body 形状分叉 —— 分组是刻意的。
//!
//! ## 错误信封
//!
//! 一律**管理风格** 400（`{success:false,error}`）：AIGX 的这几个端点都是新增
//! 的，没有需要对齐的 Node OpenAI 风格契约（既有 `/api/checkin-center/*`
//! 保持不变，前端在用）。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{any, get, post};
use axum::Router;
use serde_json::{json, Value};

use crate::api::openai::AppState;
use crate::checkin::center;
use crate::checkin::schedules;

use super::common::{error_response, verify_admin};

/// 请求体上限（条目 payload 很小；8 MiB 是防御性上界）。
const MAX_BODY: usize = 8 * 1024 * 1024;

/// 管理面路由。
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/checkin-schedules", any(schedules_root))
        .route("/api/checkin-schedules/:id/delete", any(schedule_delete))
        .route("/api/checkin-schedules/:id/run", any(schedule_run))
        .route("/api/auto-checkin", get(auto_state).post(auto_configure))
        .route("/api/auto-checkin/run", post(auto_run))
        // 活动福利 / 月度赠送（对齐 A2A /api/accounts/{id}/campaigns*）：
        // 主口径挂在 /api/accounts/:id，另在 /api/account-sessions/:id 下加影子
        // （AIGX 两套账号前缀并存，前端两处都会用到）。
        .route("/api/accounts/:id/campaigns", get(campaigns_get))
        .route("/api/accounts/:id/campaigns/claim", post(campaigns_claim))
        .route("/api/account-sessions/:id/campaigns", get(campaigns_get))
        .route(
            "/api/account-sessions/:id/campaigns/claim",
            post(campaigns_claim),
        )
        // 人工验证桥（ZCode 定时领取）：挂在本 Router 里而不是另开一个
        // `zcode_captcha::router()` —— 本 Router 已由 main.rs merge，再开一个
        // 未 merge 的 router 会让路由「写了却不生效」。令牌池那几条仍在 main.rs
        // 既有路由块里（前端铸造器在用，不动）。
        .route(
            "/api/zcode/claim-verification",
            post(super::zcode_captcha::handle_claim_verification),
        )
}

/// 活动福利预览（转调 checkin_center 的实现，路径参数同名）。
async fn campaigns_get(
    state: State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::checkin_center::handle_get_campaigns(state, headers, Path(id)).await
}

/// 活动福利领取（转调 checkin_center 的实现）。
async fn campaigns_claim(
    state: State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    super::checkin_center::handle_claim_campaign(state, headers, Path(id), body).await
}

// ─── /api/checkin-schedules* ────────────────────────────────

/// 根路径：GET 状态 / POST 保存（方法判定收在这里）。
async fn schedules_root(
    State(state): State<AppState>,
    headers: HeaderMap,
    method: Method,
    body: Bytes,
) -> Response {
    if let Err(response) = verify_admin(&state, &headers).await {
        return response.into_response();
    }
    match method {
        Method::GET => ok_json(center::auto_state(
            &state.upstream_checkin,
            &state.account_sessions,
        )),
        Method::POST => save(&state, &body),
        _ => not_found(&method),
    }
}

/// `.../{id}/delete`：DELETE 是规范写法，POST 是别名（CORS 预检考虑，同 tasks）。
async fn schedule_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(params): Path<HashMap<String, String>>,
    method: Method,
) -> Response {
    if let Err(response) = verify_admin(&state, &headers).await {
        return response.into_response();
    }
    if !matches!(method, Method::DELETE | Method::POST) {
        return not_found(&method);
    }
    let id = params.get("id").map(String::as_str).unwrap_or_default();
    match schedules::remove(&state.upstream_checkin, id) {
        Ok(_) => ok_json(json!({
            "success": true,
            "state": center::auto_state(&state.upstream_checkin, &state.account_sessions),
        })),
        Err(message) => bad_request(message),
    }
}

/// `.../{id}/run`：立即执行这一条。
async fn schedule_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(params): Path<HashMap<String, String>>,
    method: Method,
) -> Response {
    if let Err(response) = verify_admin(&state, &headers).await {
        return response.into_response();
    }
    if method != Method::POST {
        return not_found(&method);
    }
    let id = params.get("id").map(String::as_str).unwrap_or_default();
    if !state
        .upstream_checkin
        .schedules()
        .iter()
        .any(|entry| entry.id == id)
    {
        return bad_request(format!("定时条目不存在：{id}"));
    }
    let Some(outcome) =
        center::run_schedule_now(&state.upstream_checkin, &state.account_sessions, id).await
    else {
        return bad_request("签到正在执行中，请稍候");
    };
    ok_json(json!({
        "summary": outcome.summary,
        "ok": outcome.ok,
        "keepalive": outcome.keepalive,
        "failed": outcome.failed,
        "state": center::auto_state(&state.upstream_checkin, &state.account_sessions),
    }))
}

/// 保存一条条目（新增或更新）。成功返回**整份状态**（界面以响应为准重绘）。
fn save(state: &AppState, body: &Bytes) -> Response {
    let payload = match parse_json(body) {
        Ok(value) if value.is_object() => value,
        Ok(_) => return bad_request("请求内容不是有效对象"),
        Err(message) => return bad_request(message),
    };
    match schedules::upsert(&state.upstream_checkin, &payload) {
        Ok(_) => ok_json(center::auto_state(
            &state.upstream_checkin,
            &state.account_sessions,
        )),
        Err(message) => bad_request(message),
    }
}

// ─── /api/auto-checkin* ─────────────────────────────────────

/// GET /api/auto-checkin —— 自动签到状态。
async fn auto_state(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(center::auto_state(
        &state.upstream_checkin,
        &state.account_sessions,
    )))
}

/// POST /api/auto-checkin —— 修改设置（{ enabled?, time?, providers?, … }）。
async fn auto_configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let payload = if body.is_empty() {
        json!({})
    } else {
        parse_json(&body).map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?
    };
    if !payload.is_object() {
        return Err(error_response(
            "没有需要更新的字段",
            StatusCode::BAD_REQUEST,
        ));
    }
    // enabled → 总开关；time/providers/… → 默认条目 upsert（与 A2A configure 同口径）
    let mut touched = false;
    if let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) {
        let mut settings = state.upstream_checkin.settings();
        settings.enabled = enabled;
        state.upstream_checkin.set_settings(settings).map_err(|e| {
            error_response(
                &format!("保存签到设置失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        touched = true;
    }
    let entry_keys = [
        "time",
        "time_end",
        "timeEnd",
        "providers",
        "provider_times",
        "providerTimes",
        "retry_time",
        "retryTime",
        "retry_gap_minutes",
        "retryGapMinutes",
        "retry_limit",
        "retryLimit",
        "randomize",
        "label",
    ];
    if entry_keys.iter().any(|key| payload.get(*key).is_some()) {
        // 落到「默认条目」：没有条目时新建一条 id="default"
        let default_id = state
            .upstream_checkin
            .schedules()
            .first()
            .map(|s| s.id.clone())
            .unwrap_or_else(|| "default".to_string());
        let existing = state
            .upstream_checkin
            .schedules()
            .into_iter()
            .find(|s| s.id == default_id);
        let mut entry = payload.clone();
        if let Some(object) = entry.as_object_mut() {
            object
                .entry("id".to_string())
                .or_insert(Value::String(default_id.clone()));
            // 老入口没传 providers / time 时补全（upsert 要求时刻与至少一家）
            if object.get("providers").is_none() {
                let providers = existing
                    .as_ref()
                    .map(|s| s.providers.clone())
                    .unwrap_or_else(|| center::provider_options(&state.account_sessions));
                object.insert("providers".to_string(), json!(providers));
            }
            if object.get("time").is_none() {
                let time = existing
                    .as_ref()
                    .map(|s| s.time.clone())
                    .unwrap_or_else(|| "00:01".to_string());
                object.insert("time".to_string(), Value::String(time));
            }
        }
        schedules::upsert(&state.upstream_checkin, &entry)
            .map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?;
        touched = true;
    }
    if !touched {
        return Err(error_response(
            "没有需要更新的字段",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(Json(center::auto_state(
        &state.upstream_checkin,
        &state.account_sessions,
    )))
}

/// POST /api/auto-checkin/run —— 立即执行一次（全部）。
async fn auto_run(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Some(outcome) = center::run_now(&state.upstream_checkin, &state.account_sessions).await
    else {
        return Err(error_response(
            "签到正在执行中，请稍候",
            StatusCode::BAD_REQUEST,
        ));
    };
    Ok(Json(json!({
        "summary": outcome.summary,
        "ok": outcome.ok,
        "keepalive": outcome.keepalive,
        "failed": outcome.failed,
        "state": center::auto_state(&state.upstream_checkin, &state.account_sessions),
    })))
}

// ─── 小工具 ─────────────────────────────────────────────────

fn parse_json(body: &Bytes) -> Result<Value, String> {
    if body.len() > MAX_BODY {
        return Err("请求体过大".to_string());
    }
    serde_json::from_slice::<Value>(body).map_err(|e| format!("请求体不是有效 JSON：{e}"))
}

fn ok_json(value: Value) -> Response {
    Json(value).into_response()
}

fn bad_request(message: impl Into<String>) -> Response {
    let message = message.into();
    tracing::warn!("[Checkin] {message}");
    error_response(&message, StatusCode::BAD_REQUEST).into_response()
}

fn not_found(method: &Method) -> Response {
    error_response(
        &format!("不支持的请求方法：{}", method.as_str()),
        StatusCode::NOT_FOUND,
    )
    .into_response()
}
