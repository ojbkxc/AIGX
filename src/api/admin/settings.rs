//! 系统设置 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供模型映射、限额配置等系统设置功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

#[derive(Debug, Deserialize)]
pub struct SettingsRequest {
    pub mappings: std::collections::HashMap<String, String>,
    pub replace_all: bool,
}

#[derive(Debug, Deserialize)]
pub struct LimitsRequest {
    pub daily_limit: Option<u64>,
    pub monthly_limit: Option<u64>,
    pub threshold: Option<f64>,
    pub api_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
}

/// 获取系统设置
pub async fn handle_get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mappings = state.model_mapper.all_mappings();
    Ok(Json(json!({
        "success": true,
        "data": {
            "mappings": mappings
        }
    })))
}

/// 保存模型映射
pub async fn handle_update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if body.replace_all {
        // 替换所有映射
        if let Err(e) = state.model_mapper.reset() {
            tracing::error!("Failed to reset model mappings: {}", e);
        }
        for (source, target) in &body.mappings {
            if let Err(e) = state
                .model_mapper
                .set_custom(source.clone(), target.clone())
            {
                tracing::error!("Failed to set mapping {} -> {}: {}", source, target, e);
            }
        }
    } else {
        // 逐个添加/更新
        for (source, target) in &body.mappings {
            if let Err(e) = state
                .model_mapper
                .set_custom(source.clone(), target.clone())
            {
                tracing::error!("Failed to set mapping {} -> {}: {}", source, target, e);
            }
        }
    }
    Ok(Json(json!({
        "success": true,
        "data": null
    })))
}

/// 获取限额配置
pub async fn handle_get_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let daily = state.usage_tracker.today_stats();
    let monthly = state.usage_tracker.monthly_stats();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "daily_limit": config.usage.daily_limit,
            "daily_used": daily.total(),
            "monthly_limit": config.usage.monthly_limit,
            "monthly_used": monthly.total(),
            "threshold": config.usage.threshold,
            "api_timeout_secs": config.usage.api_timeout_secs,
            "max_retries": config.usage.max_retries,
        })
    })))
}

/// 更新限额配置
pub async fn handle_update_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LimitsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.daily_limit {
        config.usage.daily_limit = v;
    }
    if let Some(v) = body.monthly_limit {
        config.usage.monthly_limit = v;
    }
    if let Some(v) = body.threshold {
        config.usage.threshold = v;
    }
    if let Some(v) = body.api_timeout_secs {
        config.usage.api_timeout_secs = v;
    }
    if let Some(v) = body.max_retries {
        config.usage.max_retries = v;
    }
    match state.config_manager.update(config).await {
        Ok(_) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": updated.usage
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save limits: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}
#[derive(Debug, Deserialize)]
pub struct OauthConfigRequest {
    pub github: Option<OauthProviderFields>,
    pub google: Option<OauthProviderFields>,
}

#[derive(Debug, Deserialize)]
pub struct OauthProviderFields {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
}

/// 获取 GitHub / Google OAuth 配置与就绪状态
pub async fn handle_get_oauth_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "github": {
                "client_id": config.github_oauth.client_id,
                "client_secret": config.github_oauth.client_secret,
                "redirect_uri": config.github_oauth.redirect_uri,
                "ready": config.github_oauth.ready(),
            },
            "google": {
                "client_id": config.google_oauth.client_id,
                "client_secret": config.google_oauth.client_secret,
                "redirect_uri": config.google_oauth.redirect_uri,
                "ready": config.google_oauth.ready(),
            },
        }
    })))
}

/// 更新 GitHub / Google OAuth 配置（未提供的字段保持原值）
pub async fn handle_update_oauth_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OauthConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(g) = body.github {
        if let Some(v) = g.client_id {
            config.github_oauth.client_id = v;
        }
        if let Some(v) = g.client_secret {
            config.github_oauth.client_secret = v;
        }
        if let Some(v) = g.redirect_uri {
            config.github_oauth.redirect_uri = v;
        }
    }
    if let Some(g) = body.google {
        if let Some(v) = g.client_id {
            config.google_oauth.client_id = v;
        }
        if let Some(v) = g.client_secret {
            config.google_oauth.client_secret = v;
        }
        if let Some(v) = g.redirect_uri {
            config.google_oauth.redirect_uri = v;
        }
    }
    match state.config_manager.update(config).await {
        Ok(_) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": {
                    "github": {
                        "client_id": updated.github_oauth.client_id,
                        "client_secret": updated.github_oauth.client_secret,
                        "redirect_uri": updated.github_oauth.redirect_uri,
                        "ready": updated.github_oauth.ready(),
                    },
                    "google": {
                        "client_id": updated.google_oauth.client_id,
                        "client_secret": updated.google_oauth.client_secret,
                        "redirect_uri": updated.google_oauth.redirect_uri,
                        "ready": updated.google_oauth.ready(),
                    },
                }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save oauth config: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}
