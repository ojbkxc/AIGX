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
pub struct SmsSendRequest {
    pub provider: String,
    pub phone: String,
}

#[derive(Debug, Deserialize)]
pub struct SmsVerifyRequest {
    pub provider: String,
    pub phone: String,
    pub code: String,
    #[serde(default)]
    pub msgid: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub name: String,
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
    Ok(Json(
        json!({ "success": true, "data": { "canceled": canceled } }),
    ))
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
    let account_id =
        account_login::submit_callback(&state, body.state.trim(), body.callback_url.trim())
            .await
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(
        json!({ "success": true, "data": { "accountId": account_id } }),
    ))
}

/// POST /api/account-sessions/login/sms/send（发送手机验证码）
pub async fn handle_login_sms_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SmsSendRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    let msgid = match provider.as_str() {
        "loomy" => account_login::loomy::send_code(&reqwest::Client::new(), &body.phone)
            .await
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?,
        other => {
            return Err(error_response(
                &format!("「{other}」的手机验证码登录尚未接入网关（AutoClaw 发码强制要求阿里云验证码，请在客户端登录后粘贴 token）"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    Ok(Json(json!({ "success": true, "data": { "msgid": msgid } })))
}

/// POST /api/account-sessions/login/sms/verify（验证码登录并落账号）
pub async fn handle_login_sms_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SmsVerifyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    if provider != "loomy" {
        return Err(error_response(
            &format!("「{provider}」的手机验证码登录尚未接入网关"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let creds = account_login::loomy::login_with_code(
        &reqwest::Client::new(),
        &body.phone,
        &body.code,
        &body.msgid,
        if body.name.trim().is_empty() {
            None
        } else {
            Some(body.name.trim())
        },
    )
    .await
    .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    let account_id = account_login::save_credentials(&state, &provider, &body.channel_id, creds)
        .map_err(|error| error_response(&error, StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({ "success": true, "data": { "accountId": account_id } }),
    ))
}
