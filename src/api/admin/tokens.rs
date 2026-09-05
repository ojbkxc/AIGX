//! API Key 管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供 API Key 的创建、更新、删除、重置使用量等功能。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

// 这里需要引用主 crate 的 ApiKey 相关类型
use crate::api::auth::{ApiKey, CreateApiKeyOptions};

/// API Key 创建请求
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub name: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default)]
    pub quota_limit: Option<i64>,
    #[serde(default)]
    pub ip_limit: Option<Vec<String>>,
}

/// API Key 更新请求
#[derive(Debug, Deserialize)]
pub struct UpdateTokenRequest {
    pub name: Option<String>,
    pub group: Option<String>,
    pub allowed_models: Option<Vec<String>>,
    pub expires_at: Option<i64>,
    pub quota_limit: Option<i64>,
    pub ip_limit: Option<Vec<String>>,
    pub status: Option<String>,
}

/// 构造 API Key JSON 响应（脱敏）
pub fn mask_token(k: &ApiKey) -> Value {
    let masked_key = if k.key.chars().count() > 8 {
        format!("{}{}...", &k.key[..4], &k.key[k.key.len() - 4..])
    } else {
        "****".to_string()
    };
    json!({
        "id": k.id,
        "key": masked_key,
        "name": k.name,
        "status": k.status,
        "is_active": k.is_active,
        "user_id": k.user_id,
        "group": k.group,
        "allowed_models": k.allowed_models,
        "expires_at": k.expires_at,
        "quota_limit": k.quota_limit,
        "used_quota": k.used_quota,
        "ip_limit": k.ip_limit,
        "created_at": k.created_at,
        "updated_at": k.updated_at,
        "last_used_at": k.last_used_at,
    })
}

/// 列出所有 API Key
pub async fn handle_list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let tokens: Vec<Value> = state.api_key_store.list().iter().map(mask_token).collect();
    Ok(Json(json!({ "success": true, "data": tokens })))
}

/// 创建新 API Key
pub async fn handle_add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let opts = CreateApiKeyOptions {
        name: body.name,
        user_id: body.user_id,
        group: body.group.unwrap_or_else(|| "default".to_string()),
        allowed_models: body.allowed_models,
        expires_at: body.expires_at,
        quota_limit: body.quota_limit,
        ip_limit: body.ip_limit,
    };
    match state.api_key_store.generate_with_options(opts) {
        Ok(k) => {
            // F01（契约1）：创建响应一次性返回完整明文密钥
            let mut data = mask_token(&k);
            data["plain_key"] = json!(k.key.clone());
            Ok(Json(json!({ "success": true, "data": data })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to create token: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 更新 API Key
pub async fn handle_update_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateTokenRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.api_key_store.update(&id, |k| {
        if let Some(n) = &body.name {
            k.name = n.clone();
        }
        if let Some(g) = &body.group {
            k.group = g.clone();
        }
        if let Some(m) = &body.allowed_models {
            k.allowed_models = Some(m.clone());
        }
        if let Some(e) = body.expires_at {
            k.expires_at = Some(e);
        }
        if let Some(q) = body.quota_limit {
            k.quota_limit = Some(q);
        }
        if let Some(ip) = &body.ip_limit {
            k.ip_limit = Some(ip.clone());
        }
        if let Some(s) = &body.status {
            k.status = s.clone();
        }
    }) {
        Ok(k) => Ok(Json(json!({ "success": true, "data": mask_token(&k) }))),
        Err(e) => Err(error_response(
            &format!("Failed to update token: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 删除 API Key
pub async fn handle_delete_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.api_key_store.delete(&id) {
        Ok(_) => Ok(Json(json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete token: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 重置 API Key 使用量
pub async fn handle_reset_token_used(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if state.api_key_store.reset_used_quota(&id) {
        Ok(Json(json!({ "success": true, "data": null })))
    } else {
        Err(error_response("Token not found", StatusCode::NOT_FOUND))
    }
}
