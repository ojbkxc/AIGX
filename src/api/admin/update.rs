//! 服务器自动更新 API（检查 / 应用 / 重启 / 回滚 / 设置）。
//!
//! 全部管理面鉴权（verify_admin）。高风险动作（应用更新、重启、回滚）
//! 写审计日志；检查更新是只读网络请求，不落审计。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};
use crate::config::UpdateConfig;

/// 更新设置部分更新请求（None = 保持原值）。
#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub auto_check: Option<bool>,
    #[serde(default)]
    pub auto_apply: Option<bool>,
    #[serde(default)]
    pub interval_hours: Option<u64>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub include_prerelease: Option<bool>,
    #[serde(default)]
    pub proxy: Option<String>,
    /// GitHub 令牌：缺省保持；空串清除；非空设置新值。
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub restart_after_apply: Option<bool>,
}

/// GET /api/update/status —— 当前版本 / 最近检查结果 / 下载进度 / 是否可原地更新。
pub async fn handle_update_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": state.update.status(&config.update),
    })))
}

/// POST /api/update/check —— 立即检查 GitHub Release（受最短间隔与冷却约束）。
pub async fn handle_update_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    match state.update.check(&config.update).await {
        Ok(data) => Ok(Json(json!({ "success": true, "data": data }))),
        Err(message) => Err(error_response(&message, StatusCode::BAD_GATEWAY)),
    }
}

/// POST /api/update/apply —— 后台下载并原地替换（按配置自动 exec 重启）。
pub async fn handle_update_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    match state.update.start_apply(&config.update) {
        Ok(data) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "start_update",
                &format!("from={}", crate::update::CURRENT_VERSION),
                Some(json!({ "version": crate::update::CURRENT_VERSION })),
                Some(json!({ "latest": data["latestVersion"] })),
            );
            Ok(Json(json!({ "success": true, "data": data })))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/update/restart —— 延迟 2 秒 exec 自重启。
pub async fn handle_update_restart(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.update.request_restart() {
        Ok(data) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "restart_service",
                "update",
                None,
                Some(json!({ "reason": "manual" })),
            );
            Ok(Json(json!({ "success": true, "data": data })))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/update/rollback —— 用 `aigx.bak` / `static.bak` 回滚上一版本。
pub async fn handle_update_rollback(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    match state.update.rollback(&config.update) {
        Ok(data) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "rollback_update",
                "update",
                Some(json!({ "version": crate::update::CURRENT_VERSION })),
                Some(json!({ "restored": true })),
            );
            Ok(Json(json!({ "success": true, "data": data })))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// GET /api/update/settings —— 自动更新配置（令牌脱敏）。
pub async fn handle_get_update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": settings_json(&config.update),
    })))
}

/// PUT /api/update/settings —— 保存自动更新配置（部分更新，立即生效）。
pub async fn handle_put_update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(enabled) = body.enabled {
        config.update.enabled = enabled;
    }
    if let Some(auto_check) = body.auto_check {
        config.update.auto_check = auto_check;
    }
    if let Some(auto_apply) = body.auto_apply {
        config.update.auto_apply = auto_apply;
    }
    if let Some(interval) = body.interval_hours {
        config.update.interval_hours = interval.clamp(1, 720);
    }
    if let Some(repo) = body.repo.as_deref() {
        let repo = repo.trim();
        if !valid_repo(repo) {
            return Err(error_response(
                "仓库格式应为 owner/repo",
                StatusCode::BAD_REQUEST,
            ));
        }
        config.update.repo = repo.to_string();
    }
    if let Some(api_base) = body.api_base.as_deref() {
        let trimmed = api_base.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            let parsed = url::Url::parse(trimmed)
                .map_err(|_| error_response("更新源地址不是合法 URL", StatusCode::BAD_REQUEST))?;
            if parsed.scheme() != "http" && parsed.scheme() != "https" {
                return Err(error_response(
                    "更新源地址只支持 http/https",
                    StatusCode::BAD_REQUEST,
                ));
            }
        }
        config.update.api_base = trimmed.to_string();
    }
    if let Some(include_prerelease) = body.include_prerelease {
        config.update.include_prerelease = include_prerelease;
    }
    if let Some(proxy) = body.proxy.as_deref() {
        config.update.proxy = proxy.trim().to_string();
    }
    if let Some(restart) = body.restart_after_apply {
        config.update.restart_after_apply = restart;
    }
    if let Some(token) = body.token.as_deref() {
        config.update.token = token.trim().to_string();
    }
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": settings_json(&updated.update),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save update settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 设置视图（令牌只回脱敏值，另给 tokenConfigured 供前端显示占位符）。
fn settings_json(config: &UpdateConfig) -> Value {
    json!({
        "enabled": config.enabled,
        "auto_check": config.auto_check,
        "auto_apply": config.auto_apply,
        "interval_hours": config.interval_hours,
        "repo": config.repo,
        "api_base": config.api_base,
        "include_prerelease": config.include_prerelease,
        "proxy": config.proxy,
        "token": crate::api::admin::channels::mask_secret(&config.token),
        "tokenConfigured": !config.token.trim().is_empty(),
        "restart_after_apply": config.restart_after_apply,
        "supported": crate::update::supported(),
        "currentVersion": crate::update::CURRENT_VERSION,
    })
}

/// 校验 GitHub 仓库名：严格 `owner/repo` 两段，不允许空白、多级路径或首尾斜杠。
fn valid_repo(repo: &str) -> bool {
    if repo.chars().any(char::is_whitespace) {
        return false;
    }
    match repo.split_once('/') {
        Some((owner, name)) => {
            !owner.is_empty() && !name.is_empty() && !owner.contains('/') && !name.contains('/')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::valid_repo;

    #[test]
    fn repo_name_requires_exactly_two_segments() {
        assert!(valid_repo("ojbkxc/AIGX"));
        assert!(!valid_repo("ojbkxc"));
        assert!(!valid_repo("/AIGX"));
        assert!(!valid_repo("ojbkxc/"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("ojbkxc /AIGX"));
    }
}
