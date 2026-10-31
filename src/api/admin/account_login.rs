//! 账号池「网页登录」接口（`/api/account-sessions/login/*`）。
//!
//! 前端流程：`start` 拿授权地址并打开浏览器 → 每 2 秒 `wait` → 完成后账号
//! 自动落进账号池。远程部署（浏览器到不了网关 loopback）时，可把浏览器地址栏
//! 里的最终回调地址交给 `callback`。

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::account_login;

#[derive(Debug, Deserialize)]
pub struct LoginStartRequest {
    pub provider: String,
    #[serde(default)]
    pub channel_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginStateRequest {
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginCallbackRequest {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub callback_url: String,
}

/// POST /api/account-sessions/login/start
pub async fn handle_login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginStartRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let started = account_login::start(&state, &body.provider, &body.channel_id)
        .await
        .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({ "success": true, "data": started })))
}

/// GET /api/account-sessions/login/wait?state=
pub async fn handle_login_wait(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let task_state = params.get("state").map(String::as_str).unwrap_or("");
    let Some(task) = account_login::snapshot(task_state) else {
        return Err(error_response(
            "登录任务不存在或已过期",
            StatusCode::NOT_FOUND,
        ));
    };
    Ok(Json(json!({
        "success": true,
        "data": account_login::wait_response(&task),
    })))
}

/// POST /api/account-sessions/login/cancel
pub async fn handle_login_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginStateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let canceled = account_login::cancel(body.state.trim());
    Ok(Json(json!({ "success": true, "data": { "canceled": canceled } })))
}

/// POST /api/account-sessions/login/callback（手工粘贴回调地址）
pub async fn handle_login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginCallbackRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.callback_url.trim().is_empty() {
        return Err(error_response(
            "缺少 callback_url（登录回调地址）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let account_id = account_login::submit_callback(&state, body.state.trim(), body.callback_url.trim())
        .await
        .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({ "success": true, "data": { "accountId": account_id } })))
}
