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
use super::common::{error_response, verify_user};

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

/// 列出 API Key（双角色，对齐 new-api 权限模型）
///
/// - 管理员：返回所有令牌，列表脱敏（mask），明文只在创建/轮换时一次性展示。
/// - 普通用户：仅返回属于自己（user_id == 本人）的令牌，且显示明文 key——
///   令牌是用户本人创建，new-api 同样允许用户查看/复制自己的 key。
pub async fn handle_list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let is_admin = user.is_admin();
    let tokens: Vec<Value> = state
        .api_key_store
        .list()
        .iter()
        .filter(|k| is_admin || k.user_id.as_deref() == Some(user.id.as_str()))
        .map(|k| {
            let mut data = mask_token(k);
            if !is_admin {
                // 普通用户查看自己的令牌：显示明文，便于复制
                data["plain_key"] = json!(k.key.clone());
            }
            data
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": tokens })))
}

/// 创建新 API Key
pub async fn handle_add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    // 普通用户创建的令牌强制归属本人（对齐 new-api：用户只能创建自己的令牌）
    let user_id = if user.is_admin() { body.user_id } else { Some(user.id.clone()) };
    let opts = CreateApiKeyOptions {
        name: body.name,
        user_id,
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
    let user = verify_user(&state, &headers).await?;
    // 非管理员只能修改自己的令牌
    if !user.is_admin() {
        let owns = state
            .api_key_store
            .list()
            .iter()
            .any(|k| k.id == id && k.user_id.as_deref() == Some(user.id.as_str()));
        if !owns {
            return Err(error_response("Token not found", StatusCode::NOT_FOUND));
        }
        // 普通用户不修改归属/分组（防止提权改到其他分组）
        if body.group.is_some() && body.group.as_deref() != Some("default") {
            return Err(error_response("普通用户不能修改分组", StatusCode::FORBIDDEN));
        }
    }
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
    let user = verify_user(&state, &headers).await?;
    if !user.is_admin() {
        let owns = state
            .api_key_store
            .list()
            .iter()
            .any(|k| k.id == id && k.user_id.as_deref() == Some(user.id.as_str()));
        if !owns {
            return Err(error_response("Token not found", StatusCode::NOT_FOUND));
        }
    }
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
    let user = verify_user(&state, &headers).await?;
    if !user.is_admin() {
        let owns = state
            .api_key_store
            .list()
            .iter()
            .any(|k| k.id == id && k.user_id.as_deref() == Some(user.id.as_str()));
        if !owns {
            return Err(error_response("Token not found", StatusCode::NOT_FOUND));
        }
    }
    if state.api_key_store.reset_used_quota(&id) {
        Ok(Json(json!({ "success": true, "data": null })))
    } else {
        Err(error_response("Token not found", StatusCode::NOT_FOUND))
    }
}
