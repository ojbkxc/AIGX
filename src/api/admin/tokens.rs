//! API Key 管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供 API Key 的创建、更新、删除、重置使用量等功能。
//!
//! 权限模型对齐 new-api 的 `/api/token` 路由：
//! 登录用户即可管理自己的令牌；管理员可见全部令牌（列表脱敏）。
//! 明文密钥不随列表下发，通过 `GET /api/tokens/:id/key` 按需取回，
//! 保证「查看 + 复制」随时可用且可审计。

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
/// - 管理员：返回所有令牌，列表脱敏（mask）。
/// - 普通用户：仅返回属于自己（user_id == 本人）的令牌。
/// - 明文密钥不下发：两端都通过 `GET /api/tokens/:id/key` 按需取回，
///   避免一次性记忆负担，且每次取回都会写入审计日志。
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
        .map(mask_token)
        .collect();
    Ok(Json(json!({ "success": true, "data": tokens })))
}

/// 查看令牌明文密钥（对齐 new-api `POST /api/token/:id/key`）
///
/// - 普通用户：仅能取回自己的令牌。
/// - 管理员：可查看任意令牌。
/// - 每次取回记录审计事件（明文密钥属于高敏凭据，可追溯）。
pub async fn handle_get_token_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let is_admin = user.is_admin();
    let token = state
        .api_key_store
        .list()
        .into_iter()
        .find(|k| k.id == id && (is_admin || k.user_id.as_deref() == Some(user.id.as_str())))
        .ok_or_else(|| error_response("Token not found", StatusCode::NOT_FOUND))?;
    super::common::record_audit(
        &state,
        &user.email,
        "token.view_key",
        &token.name,
        None,
        None,
    );
    Ok(Json(
        json!({ "success": true, "data": { "id": token.id, "plain_key": token.key } }),
    ))
}

/// 创建新 API Key
pub async fn handle_add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    // 普通用户创建的令牌强制归属本人（对齐 new-api：用户只能创建自己的令牌）
    let user_id = if user.is_admin() {
        body.user_id
    } else {
        Some(user.id.clone())
    };
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
    let is_admin = user.is_admin();
    // 非管理员只能修改自己的令牌
    if !is_admin {
        let owns = state
            .api_key_store
            .list()
            .iter()
            .any(|k| k.id == id && k.user_id.as_deref() == Some(user.id.as_str()));
        if !owns {
            return Err(error_response("Token not found", StatusCode::NOT_FOUND));
        }
        // 普通用户不可修改令牌分组（对齐 new-api：分组由管理员分配）
        if body.group.is_some() {
            return Err(error_response(
                "普通用户不能修改分组",
                StatusCode::FORBIDDEN,
            ));
        }
    }
    match state.api_key_store.update(&id, |k| {
        if let Some(n) = &body.name {
            k.name = n.clone();
        }
        if is_admin {
            if let Some(g) = &body.group {
                k.group = g.clone();
            }
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
