//! 用户管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供用户列表、创建、更新、删除功能。
//!
//! ## 路径说明
//!
//! - 模块声明外使用 `crate::user::X` 访问 user crate，避免 `super::` 陷阱
//! - `mask_user` 贴合返回格式，仅隐藏敏感字段（不暴露 password/md5）

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{
    default_page, default_size, error_response, record_audit, verify_admin, verify_user,
};

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
        // 2FA 状态（不含 secret 本体）：前端 Profile 页据此显示 TOTP 开关
        "totp_enabled": u.totp_enabled,
        // 邀请返利体系（对齐 new-api Aff* 字段）
        "aff_code": u.aff_code,
        "aff_count": u.aff_count,
        "aff_quota": u.aff_quota,
        "aff_history_quota": u.aff_history_quota,
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
/// 用户列表查询参数（分页）
#[derive(Debug, Deserialize)]
pub struct ListUsersQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

pub async fn handle_list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ListUsersQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let all: Vec<Value> = state.user_store.list().iter().map(mask_user).collect();
    let total = all.len();
    let page = q.page.max(1);
    let size = q.size.max(1);
    let start_idx = (page - 1) * size;
    let data: Vec<Value> = if start_idx >= total {
        Vec::new()
    } else {
        let end_idx = (start_idx + size).min(total);
        all[start_idx..end_idx].to_vec()
    };
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "total": total,
        "page": page,
        "size": size,
    })))
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

/// PUT /api/users/self - 用户自助更新资料（对齐 new-api UpdateSelf）。
///
/// 仅允许改 username；带 original_password + password 时先验旧密码再改
/// （改密成功撤销全部旧会话，语义同 change-password）。
#[derive(Debug, Deserialize)]
pub struct UpdateSelfRequest {
    pub username: Option<String>,
    pub original_password: Option<String>,
    pub password: Option<String>,
}

pub async fn handle_update_self(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateSelfRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    // 改密时必须验旧密码（防会话泄露直接改密）
    if let Some(new_pwd) = &body.password {
        let new_pwd = new_pwd.trim();
        if new_pwd.is_empty() {
            return Err(error_response("新密码不能为空", StatusCode::BAD_REQUEST));
        }
        if new_pwd.chars().count() < 6 {
            return Err(error_response("密码长度至少6位", StatusCode::BAD_REQUEST));
        }
        let old = body.original_password.clone().unwrap_or_default();
        if !crate::user::verify_password(&old, &me.password) {
            return Err(error_response("原密码不正确", StatusCode::BAD_REQUEST));
        }
        let new_hash = crate::user::hash_password(new_pwd);
        state
            .user_store
            .update(&me.id, |u| {
                u.password = new_hash;
            })
            .map_err(|e| error_response(&format!("更新失败: {e}"), StatusCode::BAD_REQUEST))?;
        // 改密踢出全部旧会话（new-api 语义），当前会话同样失效
        let revoked = state.session_registry.revoke_all(&me.email);
        tracing::info!("Password changed via self: {} session(s) revoked", revoked);
    }
    // 用户名更新（冲突时报错）
    if let Some(n) = &body.username {
        let n = n.trim();
        if !n.is_empty() && n != me.username {
            if state.user_store.get_by_username(n).is_some() {
                return Err(error_response("用户名已存在", StatusCode::CONFLICT));
            }
            state
                .user_store
                .update(&me.id, |u| {
                    u.username = n.to_string();
                })
                .map_err(|e| error_response(&format!("更新失败: {e}"), StatusCode::BAD_REQUEST))?;
        }
    }
    let updated = state
        .user_store
        .get_by_id(&me.id)
        .ok_or_else(|| error_response("User not found", StatusCode::NOT_FOUND))?;
    Ok(Json(
        serde_json::json!({ "success": true, "data": mask_user(&updated) }),
    ))
}

/// POST /api/users/manage - 管理端用户管理操作（对齐 new-api ManageUser）。
///
/// action: enable / disable；对目标用户置 status 并撤销其全部会话
/// （禁用即时生效，已签发 token 不再可用）。
#[derive(Debug, Deserialize)]
pub struct ManageUserRequest {
    pub id: String,
    pub action: String,
}

pub async fn handle_manage_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ManageUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let target = state
        .user_store
        .get_by_id(&body.id)
        .ok_or_else(|| error_response("User not found", StatusCode::NOT_FOUND))?;
    // 防自我锁死：不允许操作自己（禁用自己会失去管理权）
    if target.id == {
        // 当前管理员 id（从会话取）
        let admin_id = admin_id_from_session_local(&state, &headers).await;
        state
            .user_store
            .get_by_email(&admin_id)
            .map(|u| u.id)
            .unwrap_or_default()
    } {
        return Err(error_response(
            "不能对自己执行该操作",
            StatusCode::BAD_REQUEST,
        ));
    }
    let new_status = match body.action.as_str() {
        "enable" => "active",
        "disable" => "disabled",
        other => {
            return Err(error_response(
                &format!("不支持的操作: {other}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    state
        .user_store
        .update(&target.id, |u| {
            u.status = new_status.to_string();
        })
        .map_err(|e| error_response(&format!("操作失败: {e}"), StatusCode::BAD_REQUEST))?;
    if new_status == "disabled" {
        // 禁用即时生效：撤销该用户全部会话
        state.session_registry.revoke_all(&target.email);
    }
    let admin_id = admin_id_from_session_local(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        &format!("user.{}", body.action),
        &format!("id={}", target.id),
        Some(mask_user(&target)),
        None,
    );
    let updated = state
        .user_store
        .get_by_id(&target.id)
        .ok_or_else(|| error_response("User not found", StatusCode::NOT_FOUND))?;
    Ok(Json(
        serde_json::json!({ "success": true, "data": mask_user(&updated) }),
    ))
}

/// DELETE /api/users/:id/2fa - 管理员强制禁用用户 2FA（对齐 new-api AdminDisable2FA）。
///
/// 用户丢失 TOTP 设备时由管理员重置；同时撤销该用户全部会话强制重新登录。
pub async fn handle_admin_disable_2fa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let target = state
        .user_store
        .get_by_id(&id)
        .ok_or_else(|| error_response("User not found", StatusCode::NOT_FOUND))?;
    if !target.totp_enabled {
        return Err(error_response("用户未启用2FA", StatusCode::BAD_REQUEST));
    }
    state
        .user_store
        .update(&target.id, |u| {
            u.totp_secret = String::new();
            u.totp_enabled = false;
            u.totp_recovery_codes = Vec::new();
        })
        .map_err(|e| error_response(&format!("操作失败: {e}"), StatusCode::BAD_REQUEST))?;
    // 2FA 状态变更：撤销全部会话强制重登（new-api 语义）
    state.session_registry.revoke_all(&target.email);
    let admin_id = admin_id_from_session_local(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "user.disable_2fa",
        &format!("id={}", target.id),
        Some(json!({ "email": target.email })),
        None,
    );
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}
