//! 管理端共享工具（P0-W1 自 `admin.rs` 抽取）。
//!
//! 提供错误响应构造、会话校验（管理员/普通用户）、分页默认值等
//! 各资源域子模块共用的无状态辅助函数。
//!
//! ## 保持行为不变
//!
//! - `verify_admin` / `verify_user` 保留原实现中的安全修复语义
//!   （B08 移除 session_secret 直通后门、B10 移除 email=="admin" 回退），
//!   拆分不允许改变任何校验路径。
//! - `record_audit` 与 `admin_id_from_session` 为审计入口，一并收敛于此，
//!   供日志/兑换等需要审计的子模块复用。
//!
//! 注意：本模块不得引入 `unimplemented!` / 泛型占位实现——所有函数
//! 均为 `admin.rs` 现有代码的逐字搬运，行为与签名保持不变。

use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::Value;

use crate::config::AppConfig;
use crate::user::User;

use super::super::auth::SessionStore;
use super::super::openai::AppState;

/// 从请求头提取客户端 IP（复用 `api::common::extract_client_ip`）。
pub use super::super::common::extract_client_ip;

/// 从验证结果判断是否管理员
pub fn is_admin_user(user: &User) -> bool {
    user.is_admin()
}

/// 创建错误响应
pub fn error_response(message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "success": false,
            "error": message
        })),
    )
}

/// 验证管理员认证
pub async fn verify_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AppConfig, (StatusCode, Json<Value>)> {
    let token = extract_session_token(headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;

    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);

    // B08：移除“token == session_secret 直通”后门——会话必须经 SessionStore
    // 签名校验，且用户系统未启用时旧 admin 兼容路径同样需要有效签名会话

    // 使用 session store 验证
    let session_store = SessionStore::new(&config.admin.session_secret, session_ttl);
    if let Some(sess) = session_store.validate_session(&token) {
        // 若存在用户系统，校验该用户仍为管理员且启用
        if let Some(u) = state.user_store.get_by_email(&sess.email) {
            if u.status == "active" && u.is_admin() {
                return Ok(config);
            }
            return Err(error_response(
                "User disabled or not admin",
                StatusCode::UNAUTHORIZED,
            ));
        }
        // 兼容模式：旧 admin 账户
        if sess.email == "admin" {
            return Ok(config);
        }
    }

    Err(error_response("Invalid session", StatusCode::UNAUTHORIZED))
}

/// 验证任意已登录用户，返回用户记录
pub async fn verify_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<User, (StatusCode, Json<Value>)> {
    let token = extract_session_token(headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;
    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_store = SessionStore::new(&config.admin.session_secret, session_ttl);
    let sess = session_store
        .validate_session(&token)
        .ok_or_else(|| error_response("Invalid session", StatusCode::UNAUTHORIZED))?;
    // B10：移除 email=="admin" 时合成管理员的回退——会话必须对应真实存在的用户，
    // 防止伪造/残留的旧会话绕过用户系统的状态与权限校验
    let user = state
        .user_store
        .get_by_email(&sess.email)
        .ok_or_else(|| error_response("User not found", StatusCode::UNAUTHORIZED))?;
    if user.status != "active" {
        return Err(error_response("User disabled", StatusCode::UNAUTHORIZED));
    }
    Ok(user)
}

/// 从请求中提取会话 token
pub fn extract_session_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(token) = auth_str.strip_prefix("Bearer ") {
                return Some(token.to_string());
            }
        }
    }
    if let Some(cookie) = headers.get("cookie") {
        if let Ok(cookie_str) = cookie.to_str() {
            for part in cookie_str.split(';') {
                let part = part.trim();
                if let Some(value) = part.strip_prefix("session=") {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

/// 从会话解析管理员 ID（无效会话回退为 "unknown"）
pub async fn admin_id_from_session(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(token) = extract_session_token(headers) {
        let config = state.config_manager.get().await;
        let session_store = SessionStore::new(
            &config.admin.session_secret,
            config.admin.session_ttl_hours.max(1),
        );
        if let Some(sess) = session_store.validate_session(&token) {
            return sess.email;
        }
    }
    "unknown".to_string()
}

/// 记录审计日志（原 `pub(crate) fn record_audit`，保持可见性语义）
pub fn record_audit(
    state: &AppState,
    admin_id: &str,
    action: &str,
    target: &str,
    before: Option<Value>,
    after: Option<Value>,
) {
    state
        .log_store
        .record_audit(admin_id, action, target, before, after);
}

/// 分页默认页码
pub fn default_page() -> usize {
    1
}

/// 分页默认每页条数
pub fn default_size() -> usize {
    20
}
