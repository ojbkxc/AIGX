//! 通知管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供通知服务配置的获取和更新功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

#[derive(Debug, Deserialize)]
pub struct UpdateNotifyConfigRequest {
    pub enabled: Option<bool>,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_from: Option<String>,
    pub smtp_starttls: Option<bool>,
    pub slack_webhook_url: Option<String>,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
}

/// 获取通知配置
pub async fn handle_get_notify_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;
    Ok(Json(json!({
        "success": true,
        "data": {
            "enabled": cfg.enabled,
            "telegram_bot_token": "...", // 脱敏
            "telegram_chat_id": cfg.telegram_chat_id,
            "smtp_host": cfg.smtp_host,
            "smtp_port": cfg.smtp_port,
            "smtp_username": cfg.smtp_username,
            "smtp_password": "...", // 脱敏
            "smtp_from": cfg.smtp_from,
            "smtp_starttls": cfg.smtp_starttls,
            "slack_webhook_url": "...", // 脱敏
            "webhook_url": cfg.webhook_url,
            "webhook_secret": "...", // 脱敏
            "telegram_ready": cfg.telegram_ready(),
            "smtp_ready": cfg.smtp_ready(),
            "slack_ready": cfg.slack_ready(),
            "webhook_ready": cfg.webhook_ready(),
        }
    })))
}

/// 更新通知配置
pub async fn handle_update_notify_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateNotifyConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let mut cfg = state.notify_service.get_config().await;
    if let Some(v) = body.enabled {
        cfg.enabled = v;
    }
    if let Some(v) = body.telegram_bot_token {
        let t = v.trim();
        if !t.is_empty() && !t.contains("***") {
            cfg.telegram_bot_token = t.to_string();
        }
    }
    if let Some(v) = body.telegram_chat_id {
        cfg.telegram_chat_id = v;
    }
    if let Some(v) = body.smtp_host {
        cfg.smtp_host = v;
    }
    if let Some(v) = body.smtp_port {
        cfg.smtp_port = v;
    }
    if let Some(v) = body.smtp_username {
        cfg.smtp_username = v;
    }
    if let Some(v) = body.smtp_password {
        let t = v.trim();
        if !t.is_empty() && !t.contains("***") {
            cfg.smtp_password = t.to_string();
        }
    }
    if let Some(v) = body.smtp_from {
        cfg.smtp_from = v;
    }
    if let Some(v) = body.smtp_starttls {
        cfg.smtp_starttls = v;
    }
    if let Some(v) = body.slack_webhook_url {
        let t = v.trim().to_string();
        if !t.is_empty() && !t.contains("***") {
            cfg.slack_webhook_url = t;
        }
    }
    if let Some(v) = body.webhook_url {
        cfg.webhook_url = v;
    }
    if let Some(v) = body.webhook_secret {
        let t = v.trim().to_string();
        if !t.is_empty() && !t.contains("***") {
            cfg.webhook_secret = t;
        }
    }
    state.notify_service.update_config(cfg).await;
    Ok(Json(json!({
        "success": true,
        "data": null
    })))
}

/// 发送测试通知
pub async fn handle_send_test_notify(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    // 未来实现：发送测试通知
    Ok(Json(json!({
        "success": true,
        "data": null
    })))
}
