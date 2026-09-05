//! 用户管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供用户列表、创建、更新、删除功能。
//!
//! ## 路径说明
//!
//! - 模块声明外使用 `crate::user::X` 访问 user crate，避免 `super::` 陷阱
//! - `mask_user` 贴合返回格式，仅隐藏敏感字段（不暴露 password/md5）

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::Value;

use super::super::openai::AppState;
use super::common::{error_response, record_audit, verify_admin};

use crate::user::{Role, User};

/// 创建用户请求
#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    pub email: String,
    pub password: String,
    pub username: Option<String>,
    #[serde(default = "default_user_role")]
    pub role: String,
    #[serde(default)]
    pub quota: i64,
    #[serde(default = "default_user_group")]
    pub group: String,
}

fn default_user_role() -> String {
    "user".to_string()
}

fn default_user_group() -> String {
    "default".to_string()
}

/// 更新用户请求
#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub role: Option<String>,
    pub quota: Option<i64>,
    pub status: Option<String>,
    pub group: Option<String>,
}

/// 构造用户 JSON 响应（mask 敏感字段）
pub fn mask_user(u: &User) -> Value {
    serde_json::json!({
        "id": u.id,
        "email": u.email,
        "username": u.username,
        "role": match u.role { Role::Admin => "admin", Role::User => "user" },
        "quota": u.quota,
        "used_quota": u.used_quota,
        "remaining": u.remaining(),
        "status": u.status,
        "group": u.group,
        "created_at": u.created_at,
    })
}

/// 从当前会话获取管理员 ID（用于审计日志）
async fn admin_id_from_session_local(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(token) = super::common::extract_session_token(headers) {
        let config = state.config_manager.get().await;
        let session_ttl = config.admin.session_ttl_hours.max(1);
        let session_store =
            super::super::auth::SessionStore::new(&config.admin.session_secret, session_ttl);
        if let Some(sess) = session_store.validate_session(&token) {
            return sess.email;
        }
    }
    "unknown".to_string()
}

/// 列出所有用户
pub async fn handle_list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let users: Vec<Value> = state.user_store.list().iter().map(mask_user).collect();
    Ok(Json(serde_json::json!({ "success": true, "data": users })))
}

/// 创建新用户
pub async fn handle_create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 获取管理员 ID 用于审计
    let admin_id = admin_id_from_session_local(&state, &headers).await;
    let role = match body.role.as_str() {
        "admin" => Role::Admin,
        _ => Role::User,
    };
    let result = if let Some(username) = &body.username {
        if !username.trim().is_empty() {
            // 检查 username 是否已被使用
            if state.user_store.get_by_username(username.trim()).is_some() {
                return Err(error_response("用户名已存在", StatusCode::CONFLICT));
            }
            state.user_store.create_with_username(
                body.email.trim(),
                username.trim(),
                &body.password,
                role,
                body.quota,
            )
        } else {
            state
                .user_store
                .create(body.email.trim(), &body.password, role, body.quota)
        }
    } else {
        state
            .user_store
            .create(body.email.trim(), &body.password, role, body.quota)
    };
    match result {
        Ok(u) => {
            // 应用请求指定的 group（非空且非默认时更新）
            let final_user = if !body.group.is_empty() && body.group != "default" {
                match state
                    .user_store
                    .update(&u.id, |x| x.group = body.group.clone())
                {
                    Ok(updated) => updated,
                    Err(_) => u,
                }
            } else {
                u
            };
            // 记录审计日志
            record_audit(
                &state,
                &admin_id,
                "create_user",
                &format!("id={}", final_user.id),
                None,
                Some(mask_user(&final_user)),
            );
            Ok(Json(
                serde_json::json!({ "success": true, "data": mask_user(&final_user) }),
            ))
        }
        Err(e) => Err(error_response(
            &format!("Failed to create user: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 更新用户信息
pub async fn handle_update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 获取管理员 ID 用于审计
    let admin_id = admin_id_from_session_local(&state, &headers).await;
    // 获取用户快照用于记录审计 before
    let user_before = match state.user_store.get_by_id(&id) {
        Some(u) => Some(mask_user(&u)),
        None => return Err(error_response("User not found", StatusCode::NOT_FOUND)),
    };
    match state.user_store.update(&id, |u| {
        if let Some(e) = &body.email {
            if !e.is_empty() {
                u.email = e.clone();
            }
        }
        if let Some(n) = &body.username {
            u.username = n.clone();
        }
        if let Some(p) = &body.password {
            if !p.is_empty() {
                u.password = crate::user::hash_password(p);
            }
        }
        if let Some(r) = &body.role {
            u.role = match r.as_str() {
                "admin" => Role::Admin,
                _ => Role::User,
            };
        }
        if let Some(q) = body.quota {
            u.quota = q;
        }
        if let Some(s) = &body.status {
            u.status = s.clone();
        }
        if let Some(g) = &body.group {
            if !g.is_empty() {
                u.group = g.clone();
            }
        }
    }) {
        Ok(u) => {
            // 记录审计日志
            record_audit(
                &state,
                &admin_id,
                "update_user",
                &format!("id={}", u.id),
                user_before,
                Some(mask_user(&u)),
            );
            Ok(Json(
                serde_json::json!({ "success": true, "data": mask_user(&u) }),
            ))
        }
        Err(e) => Err(error_response(
            &format!("Failed to update user: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 删除用户
pub async fn handle_delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 获取管理员 ID 用于审计
    let admin_id = admin_id_from_session_local(&state, &headers).await;
    // 查询用户用于记录审计
    let user_before = state.user_store.get_by_id(&id).map(|u| mask_user(&u));
    match state.user_store.delete(&id) {
        Ok(_) => {
            // 记录审计日志
            record_audit(
                &state,
                &admin_id,
                "delete_user",
                &format!("id={}", id),
                user_before,
                None,
            );
            Ok(Json(serde_json::json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to delete user: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}
