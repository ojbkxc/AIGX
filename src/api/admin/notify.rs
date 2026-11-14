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
use super::common::{error_response, extract_client_ip, verify_admin};

/// 脱敏占位符判定（GET 脱敏值不得被当作真实值写回）。
fn is_masked_value(v: &str) -> bool {
    v.contains("***") || v.trim() == "..." || v.trim() == "****"
}

/// webhook URL 出站校验（SSRF 防护）：空/脱敏值放行（交由调用方按原语义
/// 处理），其余经 net_guard 校验——与渠道 `base_url` 同口径。
fn webhook_url_allowed(
    raw: &str,
    allow_internal: bool,
) -> Result<(), crate::net_guard::UrlRejection> {
    let t = raw.trim();
    if t.is_empty() || is_masked_value(t) {
        return Ok(());
    }
    crate::net_guard::validate_outbound_url(t, allow_internal)
}

/// SSRF 拒绝留痕：谁（管理员）、从哪个 IP、试图把哪个 webhook 字段指向
/// 被拒地址。
///
/// 用带 IP 的审计入口（`record_audit_with_ip`）——审计日志 IP 注入的首个
/// 接线点（历史 `record_audit` 无 IP 入参，ip 恒空）。
async fn audit_ssrf_reject(
    state: &AppState,
    headers: &HeaderMap,
    field: &str,
    url: &str,
    rej: &crate::net_guard::UrlRejection,
) {
    let admin_id = super::common::admin_id_from_session(state, headers).await;
    let ip = extract_client_ip(headers);
    state.log_store.record_audit_with_ip(
        admin_id,
        format!("notify.{field}_ssrf_rejected"),
        format!("notify:{field}"),
        ip,
        None,
        Some(json!({ "url": url, "reason": rej.message() })),
    );
}

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
    pub smtp_ssl: Option<bool>,
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
            "smtp_ssl": cfg.smtp_ssl,
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
    // SSRF 防护：webhook 目标由网关主动外发 ⇒ 与渠道 base_url 同口径校验。
    // 先于任何字段写入：拒绝即整体不生效，不留下半改的配置。
    let allow_internal = state
        .config_manager
        .get()
        .await
        .usage
        .allow_internal_upstream;
    if let Some(v) = &body.slack_webhook_url {
        if let Err(rej) = webhook_url_allowed(v, allow_internal) {
            audit_ssrf_reject(&state, &headers, "slack_webhook_url", v, &rej).await;
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    if let Some(v) = &body.webhook_url {
        if let Err(rej) = webhook_url_allowed(v, allow_internal) {
            audit_ssrf_reject(&state, &headers, "webhook_url", v, &rej).await;
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    // 敏感字段守卫：GET 返回的脱敏占位符 "..." 不得被当作真实值写回，
    // 否则前端打开页面点一次保存就把密钥永久覆写成 "..."（P0 数据破坏）。
    // 统一过滤 *** / 裸 ... / **** 三种脱敏形态。
    if let Some(v) = body.enabled {
        cfg.enabled = v;
    }
    if let Some(v) = body.telegram_bot_token {
        let t = v.trim();
        if !t.is_empty() && !is_masked(t) {
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
        if !t.is_empty() && !is_masked(t) {
            cfg.smtp_password = t.to_string();
        }
    }
    if let Some(v) = body.smtp_from {
        cfg.smtp_from = v;
    }
    if let Some(v) = body.smtp_starttls {
        cfg.smtp_starttls = v;
    }
    if let Some(v) = body.smtp_ssl {
        cfg.smtp_ssl = v;
    }
    if let Some(v) = body.slack_webhook_url {
        let t = v.trim().to_string();
        if !t.is_empty() && !is_masked_value(&t) {
            cfg.slack_webhook_url = t;
        }
    }
    if let Some(v) = body.webhook_url {
        cfg.webhook_url = v;
    }
    if let Some(v) = body.webhook_secret {
        let t = v.trim().to_string();
        if !t.is_empty() && !is_masked_value(&t) {
            cfg.webhook_secret = t;
        }
    }
    state.notify_service.update_config(cfg).await;
    // 持久化：重启保留（原先只改内存，重启即丢）
    if let Err(e) = state
        .notify_service
        .persist_config(&state.alert_store)
        .await
    {
        tracing::warn!("通知配置持久化失败: {e}");
    }
    Ok(Json(json!({
        "success": true,
        "data": null
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认（逃生阀关闭）：内网/环回/元数据 webhook 地址被拒。
    #[test]
    fn webhook_url_rejects_internal_by_default() {
        for raw in [
            "http://127.0.0.1:8080/hook",
            "http://localhost/hook",
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.5/hook",
        ] {
            assert!(
                webhook_url_allowed(raw, false).is_err(),
                "{raw} 应被 SSRF 校验拒绝"
            );
        }
    }

    /// 逃生阀打开：内网地址放行（自托管 webhook 接收端场景）。
    #[test]
    fn webhook_url_escape_hatch() {
        assert!(webhook_url_allowed("http://127.0.0.1:8080/hook", true).is_ok());
        assert!(webhook_url_allowed("http://192.168.1.10/hook", true).is_ok());
    }

    /// 空值与脱敏占位符放行（交由调用方按原语义处理，不触发校验报错）。
    #[test]
    fn webhook_url_empty_or_masked_passthrough() {
        assert!(webhook_url_allowed("", false).is_ok());
        assert!(webhook_url_allowed("   ", false).is_ok());
        assert!(webhook_url_allowed("...", false).is_ok());
        assert!(webhook_url_allowed("****", false).is_ok());
        assert!(webhook_url_allowed("sk-***masked", false).is_ok());
    }

    /// 公网字面量放行（字面量 IP 不做 DNS 查询）。
    #[test]
    fn webhook_url_public_literal_ok() {
        assert!(webhook_url_allowed("https://8.8.8.8/hook", false).is_ok());
    }

    /// 脱敏判定与 handler 内联闭包同口径（*** / 裸 ... / ****）。
    #[test]
    fn masked_value_detection() {
        assert!(is_masked_value("..."));
        assert!(is_masked_value("****"));
        assert!(is_masked_value("token***"));
        assert!(!is_masked_value("https://example.com/hook"));
    }
}
