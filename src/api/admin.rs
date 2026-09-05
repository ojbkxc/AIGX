use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

use crate::account::CfAccount;
use crate::channel::{Channel, ChannelType};
use crate::config::AppConfig;
use crate::graphql;
use crate::payment::{Device, EpayConfig, PurchaseArgs};
use crate::pricing::{ModelPrice, RatioConfig};
use crate::user::{self, hash_password, Role, User};
use crate::user_group::UserGroup;

use super::auth::SessionStore;
use super::common::extract_client_ip;
use super::openai::AppState;

/// 登录请求
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// 注册请求
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub username: Option<String>,
}

/// 账号请求
#[derive(Debug, Deserialize)]
pub struct AccountRequest {
    pub name: String,
    pub account_id: String,
    pub api_token: String,
    pub status: Option<String>,
}

/// 设置请求
#[derive(Debug, Deserialize)]
pub struct SettingsRequest {
    pub mappings: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub replace_all: bool,
}

/// 限额请求
#[derive(Debug, Deserialize)]
pub struct LimitsRequest {
    pub daily_limit: Option<u64>,
    pub monthly_limit: Option<u64>,
    pub threshold: Option<f64>,
    pub api_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
}

/// 创建错误响应
fn error_response(message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "success": false,
            "error": message
        })),
    )
}

/// 验证管理员认证
async fn verify_admin(
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
async fn verify_user(
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
fn extract_session_token(headers: &HeaderMap) -> Option<String> {
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

// ============================================================
// 认证 API
// ============================================================

/// POST /api/auth/login - 管理员/用户登录（邮箱）
pub async fn handle_login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // ── 登录限流：同 IP 每分钟最多 10 次 ──
    const LOGIN_RATE_LIMIT_PER_MINUTE: u32 = 10;
    const LOGIN_FAIL_LOCK_THRESHOLD: u32 = 5; // 连续失败 ≥5 次锁定
    let client_ip = extract_client_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    let attempts = state.login_limiter.get(&client_ip).await.unwrap_or(0);
    if attempts >= LOGIN_RATE_LIMIT_PER_MINUTE {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::RateLimit,
                "warning",
                format!("登录限流触发：IP {client_ip} 每分钟超过 {LOGIN_RATE_LIMIT_PER_MINUTE} 次"),
            )
            .with_ip(Some(client_ip.clone())),
        );
        return Err(error_response(
            "登录尝试过于频繁，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }
    state
        .login_limiter
        .insert(client_ip.clone(), attempts + 1)
        .await;

    // ── 失败锁定：连续失败 ≥5 次 → 锁定 5 分钟（TTL 由 login_failures 控制）──
    let fail_count = state.login_failures.get(&client_ip).await.unwrap_or(0);
    if fail_count >= LOGIN_FAIL_LOCK_THRESHOLD {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::IpBlocked,
                "critical",
                format!("IP {client_ip} 连续失败 {fail_count} 次，已锁定 5 分钟"),
            )
            .with_ip(Some(client_ip.clone())),
        );
        return Err(error_response(
            "登录失败次数过多，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }

    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);

    // 用户系统认证（唯一路径；旧 admin.password 遗留已移除，admin 由 ensure_default_admin 建号）
    if let Some(u) = state.user_store.authenticate(&body.email, &body.password) {
        // 登录成功：清除失败计数
        state.login_failures.remove(&client_ip).await;
        // H3：旧 SHA256 密码登录成功后自动升级为 argon2（best-effort rehash）
        // 旧版本用无盐 SHA256（64 位十六进制）存储密码，新版本统一用 argon2。
        // rehash 失败不阻止登录，仅记录告警。
        if u.password.len() == 64 && u.password.chars().all(|c| c.is_ascii_hexdigit()) {
            let new_hash = hash_password(&body.password);
            if let Err(e) = state.user_store.update(&u.id, |user| {
                user.password = new_hash;
            }) {
                tracing::warn!(
                    "Failed to rehash legacy SHA256 password for {}: {e}",
                    u.email
                );
            }
        }
        let session_secret = if config.admin.session_secret.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            config.admin.session_secret.clone()
        };
        let session_store = SessionStore::new(&session_secret, session_ttl);
        let session = session_store.create_session(&u.email);
        return Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "token": session.token,
                "email": u.email,
                "username": u.username,
                "role": match u.role { Role::Admin => "admin", Role::User => "user" },
                "expires_at": session.expires_at
            }
        })));
    }

    // 用户名也是合法登录标识（与 user_store.authenticate 的 email 校验互补）
    if let Some(u) = state.user_store.get_by_username(&body.email) {
        if user::verify_password(&body.password, &u.password) {
            // 登录成功：清除失败计数
            state.login_failures.remove(&client_ip).await;
            let session_secret = if config.admin.session_secret.is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                config.admin.session_secret.clone()
            };
            let session_store = SessionStore::new(&session_secret, session_ttl);
            let session = session_store.create_session(&u.email);
            return Ok(Json(serde_json::json!({
                "success": true,
                "data": {
                    "token": session.token,
                    "email": u.email,
                    "username": u.username,
                    "role": match u.role { Role::Admin => "admin", Role::User => "user" },
                    "expires_at": session.expires_at
                }
            })));
        }
    }

    // 认证失败：累计失败计数（TTL=5 分钟，达阈值后锁定）
    let next_fail = fail_count + 1;
    state
        .login_failures
        .insert(client_ip.clone(), next_fail)
        .await;
    // 安全事件：认证失败（每次失败都记录，锁定阈值时升级 critical）
    let severity = if next_fail >= LOGIN_FAIL_LOCK_THRESHOLD {
        "critical"
    } else {
        "warning"
    };
    state.log_store.record_security(
        crate::log::SecurityEvent::new(
            crate::log::SecurityEventType::AuthFailure,
            severity,
            format!("登录认证失败（邮箱/用户名: {}）", body.email),
        )
        .with_ip(Some(client_ip.clone())),
    );
    if next_fail >= LOGIN_FAIL_LOCK_THRESHOLD {
        tracing::warn!("Login failed {next_fail} times from {client_ip}; locked for 5 minutes");
    }

    Err(error_response(
        "Invalid credentials",
        StatusCode::UNAUTHORIZED,
    ))
}

/// POST /api/auth/register - 公开邮箱注册
///
/// 安全：对同一 IP 每 60 秒最多允许 5 次注册请求，超出返回 429 Too Many Requests。
/// 限流基于 moka TTL 缓存（key=IP, value=计数），窗口 60s。
pub async fn handle_register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // ── 速率限制：同 IP 每分钟最多 5 次 ──
    const REGISTER_RATE_LIMIT_PER_MINUTE: u32 = 5;
    let client_ip = extract_client_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    let current_count = state.register_limiter.get(&client_ip).await.unwrap_or(0);
    if current_count >= REGISTER_RATE_LIMIT_PER_MINUTE {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::RateLimit,
                "warning",
                format!(
                    "注册限流触发：IP {client_ip} 每分钟超过 {REGISTER_RATE_LIMIT_PER_MINUTE} 次"
                ),
            )
            .with_ip(Some(client_ip.clone())),
        );
        return Err(error_response(
            "注册请求过于频繁，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }
    state
        .register_limiter
        .insert(client_ip, current_count + 1)
        .await;

    if body.email.trim().is_empty() {
        return Err(error_response("邮箱不能为空", StatusCode::BAD_REQUEST));
    }
    // B25：按字符数而非字节数校验长度，避免中文等非 ASCII 密码被误判
    //（原实现“中文密码4字=12字节”可绕过 6 位下限）
    if body.password.chars().count() < 6 {
        return Err(error_response("密码长度至少6位", StatusCode::BAD_REQUEST));
    }
    let config = state.config_manager.get().await;
    let default_quota = config.usage.monthly_limit as i64;
    let user = if let Some(username) = &body.username {
        if !username.trim().is_empty() {
            // 检查 username 是否已被使用
            if state.user_store.get_by_username(username.trim()).is_some() {
                return Err(error_response("用户名已存在", StatusCode::CONFLICT));
            }
            state
                .user_store
                .create_with_username(
                    body.email.trim(),
                    username.trim(),
                    &body.password,
                    Role::User,
                    default_quota,
                )
                .map_err(|e| error_response(&format!("注册失败: {e}"), StatusCode::BAD_REQUEST))?
        } else {
            state
                .user_store
                .create(body.email.trim(), &body.password, Role::User, default_quota)
                .map_err(|e| error_response(&format!("注册失败: {e}"), StatusCode::BAD_REQUEST))?
        }
    } else {
        state
            .user_store
            .create(body.email.trim(), &body.password, Role::User, default_quota)
            .map_err(|e| error_response(&format!("注册失败: {e}"), StatusCode::BAD_REQUEST))?
    };

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "id": user.id,
            "email": user.email,
            "username": user.username,
            "role": "user",
            "quota": user.quota,
        }
    })))
}

/// POST /api/auth/logout - 管理员登出
pub async fn handle_logout(
    State(_state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _token = extract_session_token(&headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": null
    })))
}

// ============================================================
// 用量 API
// ============================================================

/// GET /api/usage/summary - 用量汇总
pub async fn handle_usage_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    // 性能（H5/H6）：复用 AppState 的共享 reqwest::Client，避免每次请求新建客户端。
    let http_client = state.http_client.clone();
    let accounts = state.account_pool.list();
    let mut graphql_results = Vec::new();

    for account in &accounts {
        match graphql::query_usage_summary(account, &http_client).await {
            Ok(usage) => {
                graphql_results.push(serde_json::json!({
                    "account_id": account.account_id,
                    "account_name": account.name,
                    "neurons": usage.neurons,
                    "requests": usage.requests,
                    "today_neurons": usage.today_neurons,
                    "today_requests": usage.today_requests,
                }));
            }
            Err(e) => {
                tracing::warn!("GraphQL query failed for account {}: {e}", account.name);
            }
        }
    }

    let today = state.usage_tracker.today_stats();
    let monthly = state.usage_tracker.monthly_stats();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "total_tokens": monthly.total(),
            "total_input_tokens": monthly.input,
            "total_output_tokens": monthly.output,
            "active_accounts": graphql_results.len(),
            "graphql": graphql_results,
            "local": {
                "daily_tokens": today,
                "monthly_tokens": monthly,
            }
        }
    })))
}

/// POST /api/usage/summary - 强制刷新用量
pub async fn handle_refresh_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    // 性能（H5/H6）：复用 AppState 的共享 reqwest::Client，避免每次请求新建客户端。
    let http_client = state.http_client.clone();
    let accounts = state.account_pool.list();
    let mut graphql_results = Vec::new();

    for account in &accounts {
        match graphql::query_usage_summary(account, &http_client).await {
            Ok(usage) => {
                graphql_results.push(serde_json::json!({
                    "account_id": account.account_id,
                    "account_name": account.name,
                    "neurons": usage.neurons,
                    "requests": usage.requests,
                    "today_neurons": usage.today_neurons,
                    "today_requests": usage.today_requests,
                }));
            }
            Err(e) => {
                tracing::warn!("GraphQL refresh failed for account {}: {e}", account.name);
            }
        }
    }

    let today = state.usage_tracker.today_stats();
    let monthly = state.usage_tracker.monthly_stats();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "total_tokens": monthly.total(),
            "total_input_tokens": monthly.input,
            "total_output_tokens": monthly.output,
            "active_accounts": graphql_results.len(),
            "graphql": graphql_results,
            "local": {
                "daily_tokens": today,
                "monthly_tokens": monthly,
            }
        }
    })))
}

// ============================================================
// 账号管理 API
// ============================================================

/// GET /api/accounts - 列出账号
pub async fn handle_list_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let accounts = state.account_pool.list();

    let masked: Vec<Value> = accounts
        .into_iter()
        .map(|a| {
            let masked_token = if a.api_token.chars().count() > 8 {
                mask_with(&a.api_token, 4, 4, "...")
            } else {
                "****".to_string()
            };

            serde_json::json!({
                "id": a.id,
                "name": a.name,
                "account_id": a.account_id,
                "api_token": masked_token,
                "status": a.status,
                "last_error": a.last_error,
                "last_used_at": a.last_used_at,
                "created_at": a.created_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": masked
    })))
}

/// POST /api/accounts - 添加账号
pub async fn handle_add_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AccountRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();

    let account = CfAccount {
        id: id.clone(),
        name: body.name,
        account_id: body.account_id,
        api_token: body.api_token,
        status: body.status.unwrap_or_else(|| "active".to_string()),
        last_error: None,
        last_used_at: None,
        created_at: now,
    };

    match state.account_pool.add(account) {
        Ok(_) => Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "id": id
            }
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to add account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// POST /api/accounts/test - 测试账号连接
pub async fn handle_test_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AccountRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let test_account = CfAccount {
        id: String::new(),
        name: body.name,
        account_id: body.account_id,
        api_token: body.api_token,
        status: "active".to_string(),
        last_error: None,
        last_used_at: None,
        created_at: 0,
    };

    match state.account_pool.test(&test_account).await {
        Ok(result) => Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "message": result.message,
                "models": result.models,
                "inference": result.inference,
                "analytics": result.analytics,
                "overall": result.success,
            }
        }))),
        Err(e) => Err(error_response(
            &format!("Account test failed: {e}"),
            StatusCode::BAD_GATEWAY,
        )),
    }
}

/// PUT /api/accounts/:id - 更新账号
pub async fn handle_update_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AccountRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let existing = state.account_pool.list().into_iter().find(|a| a.id == id);
    let existing = match existing {
        Some(a) => a,
        None => return Err(error_response("Account not found", StatusCode::NOT_FOUND)),
    };

    let updated = CfAccount {
        id: id.clone(),
        name: body.name,
        account_id: body.account_id,
        api_token: body.api_token,
        status: body.status.unwrap_or(existing.status),
        last_error: existing.last_error,
        last_used_at: existing.last_used_at,
        created_at: existing.created_at,
    };

    match state.account_pool.update(&id, updated) {
        Ok(_) => Ok(Json(serde_json::json!({
            "success": true,
            "data": null
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to update account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// DELETE /api/accounts/:id - 删除账号
pub async fn handle_delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    match state.account_pool.remove(&id) {
        Ok(_) => Ok(Json(serde_json::json!({
            "success": true,
            "data": null
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete account: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ============================================================
// API 密钥管理
// ============================================================

/// GET /api/keys - 列出 API 密钥
pub async fn handle_list_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let keys = state.api_key_store.list();
    let masked: Vec<Value> = keys
        .into_iter()
        .map(|k| {
            let masked_key = if k.key.chars().count() > 8 {
                mask_with(&k.key, 4, 4, "...")
            } else {
                "****".to_string()
            };

            serde_json::json!({
                "id": k.id,
                "key": masked_key,
                "name": k.name,
                "is_active": k.is_active,
                "created_at": k.created_at,
                "last_used_at": k.last_used_at,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": masked
    })))
}

/// POST /api/keys - 生成 API 密钥
pub async fn handle_add_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    match state.api_key_store.generate(&body.name) {
        Ok(key) => Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "id": key.id,
                "key": key.key,
                "name": key.name,
                "created_at": key.created_at
            }
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to generate key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// DELETE /api/keys/:id - 删除 API 密钥
pub async fn handle_delete_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    match state.api_key_store.delete(&id) {
        Ok(_) => Ok(Json(serde_json::json!({
            "success": true,
            "data": null
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ============================================================
// 设置管理
// ============================================================

/// GET /api/settings - 获取模型映射
pub async fn handle_get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let mappings = state.model_mapper.all_mappings();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "mappings": mappings
        }
    })))
}

/// PUT /api/settings - 保存模型映射
pub async fn handle_update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    if body.replace_all {
        // 替换所有映射（重置自定义映射，再逐个添加）
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

    Ok(Json(serde_json::json!({
        "success": true,
        "data": null
    })))
}

/// GET /api/limits - 获取限额配置
pub async fn handle_get_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;

    let daily = state.usage_tracker.today_stats();
    let monthly = state.usage_tracker.monthly_stats();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "daily_limit": config.usage.daily_limit,
            "daily_used": daily.total(),
            "monthly_limit": config.usage.monthly_limit,
            "monthly_used": monthly.total(),
            "threshold": config.usage.threshold,
            "api_timeout_secs": config.usage.api_timeout_secs,
            "max_retries": config.usage.max_retries,
        }
    })))
}

/// PUT /api/limits - 更新限额配置
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
            Ok(Json(serde_json::json!({
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

/// GET /api/tokens/today - 获取今日 Token 统计
pub async fn handle_tokens_today(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let stats = state.usage_tracker.today_stats();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "input_tokens": stats.input,
            "output_tokens": stats.output,
            "total_tokens": stats.total(),
            "reasoning_tokens": stats.reasoning,
            "cache_read_tokens": stats.cache_read,
            "cache_write_tokens": stats.cache_write,
            "request_count": stats.requests,
            "avg_tok_per_sec": stats.avg_tok_per_sec(),
        }
    })))
}

/// GET /api/usage/trend - 近 7 日消耗趋势
pub async fn handle_usage_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let trend = state.usage_tracker.weekly_trend();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": trend
    })))
}

/// GET /api/usage/models - 模型用量统计
pub async fn handle_usage_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let models = state.usage_tracker.model_usage();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": models
    })))
}

// ============================================================
// 用户管理 API
// ============================================================

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

fn mask_user(u: &User) -> Value {
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

/// GET /api/users - 列出用户
pub async fn handle_list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let users: Vec<Value> = state.user_store.list().iter().map(mask_user).collect();
    Ok(Json(serde_json::json!({ "success": true, "data": users })))
}

/// POST /api/users - 创建用户
pub async fn handle_create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let role = match body.role.as_str() {
        "admin" => Role::Admin,
        _ => Role::User,
    };
    let result = if let Some(username) = &body.username {
        if !username.trim().is_empty() {
            state.user_store.create_with_username(
                &body.email,
                username.trim(),
                &body.password,
                role,
                body.quota,
            )
        } else {
            state
                .user_store
                .create(&body.email, &body.password, role, body.quota)
        }
    } else {
        state
            .user_store
            .create(&body.email, &body.password, role, body.quota)
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

/// PUT /api/users/:id - 更新用户
pub async fn handle_update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateUserRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
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
                u.password = user::hash_password(p);
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
        Ok(u) => Ok(Json(
            serde_json::json!({ "success": true, "data": mask_user(&u) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to update user: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// DELETE /api/users/:id - 删除用户
pub async fn handle_delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.user_store.delete(&id) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete user: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// GET /api/users/me - 当前登录用户信息
pub async fn handle_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let u = verify_user(&state, &headers).await?;
    Ok(Json(
        serde_json::json!({ "success": true, "data": mask_user(&u) }),
    ))
}

// ============================================================
// 易支付配置 API
// ============================================================

/// GET /api/epay/config - 读取易支付配置（仅管理员）
/// 参照 VFaka：敏感字段 epay_key 做脱敏处理（保留前3后3，中间 ***）
pub async fn handle_get_epay_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;

    // 脱敏 epay_key：保留前3后3字符，中间用 *** 替代
    let masked_key = mask_sensitive(&config.epay.epay_key);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "pay_address": config.epay.pay_address,
            "epay_id": config.epay.epay_id,
            "epay_key": masked_key,
            "pay_methods": config.epay.pay_methods,
            "price": config.epay.price,
            "amount_discount": config.epay.amount_discount,
            "min_topup": config.epay.min_topup,
            "custom_callback_address": config.epay.custom_callback_address,
            "server_address": config.server_address,
        }
    })))
}

/// 脱敏字符串：保留前3后3字符，中间用 *** 替代
///
/// 使用 `chars()` 而非字节切片，避免多字节 UTF-8 字符（如中文）在边界处 panic。
fn mask_sensitive(s: &str) -> String {
    mask_with(s, 3, 3, "***")
}

/// 通用脱敏辅助函数：保留前 `prefix` 个字符与后 `suffix` 个字符，中间用 `mask` 替代。
///
/// 使用按字符（非字节）切片，安全处理多字节 UTF-8 字符。
/// 当字符总数 <= prefix + suffix 时，直接返回 `mask` 以避免泄露过多信息。
fn mask_with(s: &str, prefix: usize, suffix: usize, mask: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= prefix + suffix {
        return mask.to_string();
    }
    let head: String = chars[..prefix].iter().collect();
    let tail: String = chars[chars.len() - suffix..].iter().collect();
    format!("{}{}{}", head, mask, tail)
}

#[derive(Debug, Deserialize)]
pub struct UpdateEpayConfigRequest {
    pub pay_address: Option<String>,
    pub epay_id: Option<String>,
    pub epay_key: Option<String>,
    pub pay_methods: Option<Vec<String>>,
    pub price: Option<f64>,
    pub amount_discount: Option<std::collections::HashMap<i64, f64>>,
    pub min_topup: Option<i64>,
    pub custom_callback_address: Option<String>,
    pub server_address: Option<String>,
}

/// PUT /api/epay/config - 更新易支付配置（仅管理员）
pub async fn handle_update_epay_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateEpayConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.pay_address {
        config.epay.pay_address = v;
    }
    if let Some(v) = body.epay_id {
        config.epay.epay_id = v;
    }
    // F03（契约3）：商户密钥防覆盖——前端回显时密钥以脱敏形式展示，
    // 保存时若原值未修改会带 *** 占位，空值/脱敏值均跳过更新，
    // 仅在管理员输入了新完整密钥时才覆盖。
    if let Some(v) = body.epay_key {
        let t = v.trim();
        if !t.is_empty() && !t.contains("***") {
            config.epay.epay_key = t.to_string();
        }
    }
    if let Some(v) = body.pay_methods {
        config.epay.pay_methods = v;
    }
    if let Some(v) = body.price {
        config.epay.price = v;
    }
    if let Some(v) = body.amount_discount {
        config.epay.amount_discount = v;
    }
    if let Some(v) = body.min_topup {
        config.epay.min_topup = v;
    }
    if let Some(v) = body.custom_callback_address {
        config.epay.custom_callback_address = v;
    }
    if let Some(v) = body.server_address {
        config.server_address = v;
    }
    state
        .config_manager
        .update(config.clone())
        .await
        .map_err(|e| {
            error_response(
                &format!("Failed to save config: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

// ============================================================
// 订单与充值 API
// ============================================================

#[derive(Debug, Deserialize)]
pub struct TopupRequest {
    /// 充值数量（元，按 price 转换为配额）
    pub amount: i64,
    pub payment_method: String,
}

/// F02（契约2）：实付金额 = amount × discount。
///
/// 旧实现误将倍率 price 计入实付（money = amount × price × discount），
/// 用户会被多收 price 倍；倍率只应作用于入账配额（见 `topup_quota`）。
fn pay_money(epay: &EpayConfig, amount: i64) -> f64 {
    let discount = *epay
        .amount_discount
        .get(&amount)
        .filter(|d| **d > 0.0)
        .unwrap_or(&1.0);
    let money = (amount as f64) * discount;
    (money * 100.0).round() / 100.0
}

/// F02（契约2）：入账配额 = amount × price × discount（向上取整，
/// 与 pricing 模块 `calculate_cost_quoted` 的取整惯例一致）。
fn topup_quota(epay: &EpayConfig, amount: i64) -> i64 {
    let discount = *epay
        .amount_discount
        .get(&amount)
        .filter(|d| **d > 0.0)
        .unwrap_or(&1.0);
    ((amount as f64) * epay.price * discount + 0.999999) as i64
}

fn callback_address(_state: &AppState, config: &AppConfig) -> String {
    if !config.epay.custom_callback_address.is_empty() {
        return config.epay.custom_callback_address.clone();
    }
    config.server_address.clone()
}

fn make_return_path(suffix: &str) -> String {
    let base = "/wallet";
    if suffix.is_empty() {
        base.to_string()
    } else {
        format!("{base}?pay={suffix}")
    }
}

/// POST /api/topup - 用户发起充值
pub async fn handle_topup_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TopupRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let config = state.config_manager.get().await;
    let epay = crate::payment::EpayClient::new(config.epay.clone());
    if !epay.config().ready() {
        return error_response("Epay not configured", StatusCode::BAD_REQUEST).into_response();
    }
    if body.amount < epay.config().min_topup {
        return error_response("Amount below minimum", StatusCode::BAD_REQUEST).into_response();
    }
    if !epay.config().contains_pay_method(&body.payment_method) {
        return error_response("Payment method not supported", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let money = pay_money(epay.config(), body.amount);
    if money < 0.01 {
        return error_response("Amount too low", StatusCode::BAD_REQUEST).into_response();
    }
    let callback = callback_address(&state, &config);
    let return_url = format!("{}{}", callback, make_return_path(""));
    let notify_url = format!("{}/api/user/epay/notify", callback.trim_end_matches('/'));
    let trade_no = user::new_trade_no("USR", &user.id);

    let order = crate::payment::TopUpOrder {
        trade_no: trade_no.clone(),
        user_id: user.id.clone(),
        amount: body.amount,
        money,
        // F02（契约2）：下单时锁定入账配额，回调直接使用（见 TopUpOrder.quota）
        quota: topup_quota(epay.config(), body.amount),
        payment_method: body.payment_method.clone(),
        status: "pending".into(),
        create_time: chrono::Utc::now().timestamp(),
        paid_time: None,
    };
    if let Err(e) = state.order_store.insert(&order) {
        tracing::error!("Failed to create order: {e}");
        return error_response("Failed to create order", StatusCode::INTERNAL_SERVER_ERROR)
            .into_response();
    }
    let args = PurchaseArgs {
        pay_type: body.payment_method.clone(),
        out_trade_no: trade_no.clone(),
        name: format!("TUC{}", body.amount),
        money: format!("{:.2}", money),
        notify_url,
        return_url,
        clientip: "127.0.0.1".into(), // 参照 VFaka：部分易支付网关要求必填
        device: Device::PC,
    };
    match epay.purchase(&args).await {
        Ok(res) => {
            let params: HashMap<String, String> = res.params.into_iter().collect();
            Json(serde_json::json!({
                "success": true,
                "data": params,
                "url": res.url,
                "trade_no": trade_no,
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!("Epay purchase failed: {e}");
            error_response("Failed to start payment", StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

/// GET /api/orders - 管理员查询所有订单
pub async fn handle_list_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let orders = state.order_store.list_all();
    Ok(Json(serde_json::json!({ "success": true, "data": orders })))
}

/// GET /api/orders/me - 当前用户查询自己的订单
pub async fn handle_my_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let u = verify_user(&state, &headers).await?;
    let orders = state.order_store.list_by_user(&u.id);
    Ok(Json(serde_json::json!({ "success": true, "data": orders })))
}

/// 解析易支付回调参数（支持 GET query 与 POST form）
fn collect_params(query: Option<&str>, body_bytes: &bytes::Bytes) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(q) = query {
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = urlencoding_decode(it.next().unwrap_or(""));
            let v = urlencoding_decode(it.next().unwrap_or(""));
            if !k.is_empty() {
                map.insert(k, v);
            }
        }
    }
    if !body_bytes.is_empty() {
        let s = String::from_utf8_lossy(body_bytes);
        for pair in s.split('&') {
            let mut it = pair.splitn(2, '=');
            let k = urlencoding_decode(it.next().unwrap_or(""));
            let v = urlencoding_decode(it.next().unwrap_or(""));
            if !k.is_empty() {
                map.insert(k, v);
            }
        }
    }
    map
}

/// 解码 application/x-www-form-urlencoded（UTF-8 感知）
///
/// B18：原实现 `b as char` 逐字节解码为 Latin-1 字符，%E4%B8%AD 这类 UTF-8
/// 多字节序列会被拆成三个独立字符导致中文参数乱码。现先把 %XX 还原为原始
/// 字节，再整体按 UTF-8 解码；非法序列回退 lossy，保证不 panic。
fn urlencoding_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            // %XX 十六进制转义还原为原始字节
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
            out.push(b'%');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// POST/GET /api/user/epay/notify - 易支付异步通知
/// 参照 VFaka 回调实现：合并 query + body 参数，优先 body（POST 更可靠），
/// 签名验证后做金额校验（2% 容忍度），使用 order.money / price 计算配额（处理折扣）
pub async fn handle_epay_notify(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> Response {
    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return "fail".into_response(),
    };

    // 合并 query + body 参数，body 优先（POST form 更可靠）
    let mut params = query.clone();
    let body_params = collect_params(None, &bytes);
    for (k, v) in body_params {
        params.insert(k, v);
    }

    if params.is_empty() {
        return "fail".into_response();
    }
    let config = state.config_manager.get().await;
    let epay = crate::payment::EpayClient::new(config.epay.clone());
    let verify = match epay.verify(&params) {
        Ok(v) => v,
        Err(_) => return "fail".into_response(),
    };
    if !verify.verify_status || verify.trade_status != "TRADE_SUCCESS" {
        tracing::warn!(
            "Epay notify: verify failed or trade not success, trade_status={}",
            verify.trade_status
        );
        return "fail".into_response();
    }

    // 提取回调金额用于校验
    let callback_money: f64 = params
        .get("money")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if let Some(order) = state.order_store.get(&verify.out_trade_no) {
        // 金额校验：容忍度 max(订单金额 * 2%, 0.01元)，参照 VFaka
        let tolerance = (order.money * 0.02).max(0.01);
        let amount_diff = (callback_money - order.money).abs();
        if amount_diff > tolerance {
            tracing::error!(
                "Epay notify amount mismatch: trade_no={} expected={} received={} diff={}",
                order.trade_no,
                order.money,
                callback_money,
                amount_diff
            );
            return "fail".into_response();
        }

        // B01 修复：先原子完成订单（CAS pending → paid），仅当本次回调真正
        // 抢到订单时才入账。notify 与 return 并发到达时只有一方能成功，
        // 杜绝双倍入账；重复回调直接幂等跳过。
        let completed = state.order_store.complete_if_pending(&order.trade_no);
        if let Some(order) = completed {
            // F02（契约2）：优先使用下单时锁定的 quota（amount × price × discount）；
            // 旧订单（quota=0，serde default 反序列化）回退 money/price 反推
            let quota_to_add = if order.quota > 0 {
                order.quota
            } else if order.money > 0.0 && config.epay.price > 0.0 {
                (order.money / config.epay.price).round() as i64
            } else {
                order.quota
            };
            // B01/B02：使用原子加款；失败仅告警不回滚订单状态（订单已支付，
            // 绝不能让网关重试造成二次加钱），由管理员依据告警人工补偿。
            if let Err(e) = state.user_store.add_quota(&order.user_id, quota_to_add) {
                tracing::error!(
                    "Epay notify: CRITICAL quota compensation required: order={} user={} quota={} error={}",
                    order.trade_no, order.user_id, quota_to_add, e
                );
                state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: format!("payment/order:{}", order.trade_no),
                    error: format!("add_quota failed, manual compensation required: quota=+{quota_to_add}, err={e}"),
                });
            }
            tracing::info!(
                "Epay order completed: trade_no={} user={} amount={} money={} quota=+{}",
                order.trade_no,
                order.user_id,
                order.amount,
                order.money,
                quota_to_add
            );

            // 通知：充值成功（异步，不阻塞回调）
            let user_email = state
                .user_store
                .get_by_id(&order.user_id)
                .map(|u| u.email)
                .unwrap_or_default();
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::PaymentSuccess {
                    user_email,
                    amount: order.money,
                    quota: quota_to_add,
                });
        }
    }
    "success".into_response()
}

/// POST/GET /api/user/epay/return - 易支付同步跳转
/// 参照 VFaka：签名验证 + 金额校验 + 折扣正确计算配额
pub async fn handle_epay_return(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> Response {
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let mut params = query.clone();
    let body_params = collect_params(None, &bytes);
    for (k, v) in body_params {
        params.insert(k, v);
    }
    if params.is_empty() {
        return Redirect::to(&make_return_path("fail")).into_response();
    }
    let config = state.config_manager.get().await;
    let epay = crate::payment::EpayClient::new(config.epay.clone());
    let verify = match epay.verify(&params) {
        Ok(v) => v,
        Err(_) => return Redirect::to(&make_return_path("fail")).into_response(),
    };
    if !verify.verify_status {
        return Redirect::to(&make_return_path("fail")).into_response();
    }

    // 提取回调金额
    let callback_money: f64 = params
        .get("money")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if verify.trade_status == "TRADE_SUCCESS" {
        if let Some(order) = state.order_store.get(&verify.out_trade_no) {
            // 金额校验：容忍度 max(订单金额 * 2%, 0.01元)
            let tolerance = (order.money * 0.02).max(0.01);
            if (callback_money - order.money).abs() > tolerance {
                tracing::error!(
                    "Epay return amount mismatch: trade_no={} expected={} received={}",
                    order.trade_no,
                    order.money,
                    callback_money
                );
                return Redirect::to(&make_return_path("fail")).into_response();
            }

            // B01 修复：先原子完成订单（CAS pending → paid），仅本次抢到订单
            // 才入账，与 notify 并发时不会双倍入账。
            let completed = state.order_store.complete_if_pending(&order.trade_no);
            if let Some(order) = completed {
                // F02（契约2）：优先使用下单时锁定的 quota（amount × price × discount）；
                // 旧订单（quota=0，serde default 反序列化）回退 money/price 反推
                let quota_to_add = if order.quota > 0 {
                    order.quota
                } else if order.money > 0.0 && config.epay.price > 0.0 {
                    (order.money / config.epay.price).round() as i64
                } else {
                    order.quota
                };
                // B01/B02：原子加款；失败仅告警（订单已支付，不回滚、不重复加钱）
                if let Err(e) = state.user_store.add_quota(&order.user_id, quota_to_add) {
                    tracing::error!(
                        "Epay return: CRITICAL quota compensation required: order={} user={} quota={} error={}",
                        order.trade_no, order.user_id, quota_to_add, e
                    );
                    state.notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: format!("payment/order:{}", order.trade_no),
                        error: format!("add_quota failed, manual compensation required: quota=+{quota_to_add}, err={e}"),
                    });
                }
                tracing::info!(
                    "Epay return completed: trade_no={} user={} quota=+{}",
                    order.trade_no,
                    order.user_id,
                    quota_to_add
                );
            }
        }
        return Redirect::to(&make_return_path("success")).into_response();
    }
    Redirect::to(&make_return_path("pending")).into_response()
}

// ============================================================
// 通用渠道管理（功能 2 - 核心数据层）
// ============================================================

#[derive(Debug, Deserialize)]
pub struct ChannelRequest {
    pub name: String,
    #[serde(default)]
    pub channel_type: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_enabled_status")]
    pub status: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub account_id: String,
}

fn default_weight() -> u32 {
    1
}

fn default_enabled_status() -> String {
    "enabled".to_string()
}

impl ChannelRequest {
    fn to_channel(&self, id: String) -> Channel {
        let now = chrono::Utc::now().timestamp();
        Channel {
            id,
            name: self.name.clone(),
            channel_type: ChannelType::from_str_lossy(&self.channel_type),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            priority: self.priority,
            weight: self.weight,
            status: self.status.clone(),
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

fn mask_channel(ch: &Channel) -> Value {
    let masked_key = if ch.api_key.is_empty() {
        String::new()
    } else if ch.api_key.chars().count() > 12 {
        mask_with(&ch.api_key, 8, 4, "...")
    } else {
        "****".to_string()
    };
    serde_json::json!({
        "id": ch.id,
        "name": ch.name,
        "channel_type": ch.channel_type.as_str(),
        "base_url": ch.base_url,
        "api_key": masked_key,
        "priority": ch.priority,
        "weight": ch.weight,
        "status": ch.status,
        "models": ch.models,
        "account_id": ch.account_id,
        "last_error": ch.last_error,
        "last_used_at": ch.last_used_at,
        "created_at": ch.created_at,
        "updated_at": ch.updated_at,
    })
}

pub async fn handle_list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let channels: Vec<Value> = state
        .channel_store
        .list()
        .iter()
        .map(mask_channel)
        .collect();
    Ok(Json(
        serde_json::json!({ "success": true, "data": channels }),
    ))
}

pub async fn handle_add_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = body.to_channel(String::new());
    match state.channel_store.add(ch) {
        Ok(c) => Ok(Json(
            serde_json::json!({ "success": true, "data": mask_channel(&c) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to add channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let existing = state.channel_store.get(&id);
    let mut ch = body.to_channel(id.clone());
    if let Some(e) = existing {
        ch.created_at = e.created_at;
        ch.last_error = e.last_error;
        ch.last_used_at = e.last_used_at;
        if ch.api_key.is_empty() {
            ch.api_key = e.api_key;
        }
    }
    match state.channel_store.update(&id, ch) {
        Ok(c) => Ok(Json(
            serde_json::json!({ "success": true, "data": mask_channel(&c) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to update channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// PATCH /api/channels/{id} - 渠道部分更新（仅更新传入的字段，未传入字段保留现有值）
///
/// 与 PUT 不同，PATCH 不要求提供完整 ChannelRequest，仅更新 JSON body 中出现的字段。
/// 典型用途：前端 handleToggle 仅传 `{status}` 切换启用/禁用，避免脱敏 api_key 覆盖真实密钥。
///
/// 支持的部分更新字段：name / channel_type / base_url / api_key / priority / weight /
/// status / models / account_id / enabled。其中 api_key 为空字符串时保留现有值。
pub async fn handle_patch_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mut ch = state
        .channel_store
        .get(&id)
        .ok_or_else(|| error_response("Channel not found", StatusCode::NOT_FOUND))?;

    // 逐字段部分更新：仅在 JSON 中出现该 key 时更新
    if let Some(name) = body.get("name").and_then(|v| v.as_str()) {
        ch.name = name.to_string();
    }
    if let Some(channel_type) = body.get("channel_type").and_then(|v| v.as_str()) {
        ch.channel_type = ChannelType::from_str_lossy(channel_type);
    }
    if let Some(base_url) = body.get("base_url").and_then(|v| v.as_str()) {
        ch.base_url = base_url.to_string();
    }
    // api_key：非空才更新，空字符串保留现有（避免脱敏值覆盖真实密钥）
    if let Some(api_key) = body.get("api_key").and_then(|v| v.as_str()) {
        if !api_key.is_empty() {
            ch.api_key = api_key.to_string();
        }
    }
    if let Some(priority) = body.get("priority").and_then(|v| v.as_i64()) {
        ch.priority = priority;
    }
    if let Some(weight) = body.get("weight").and_then(|v| v.as_u64()) {
        ch.weight = weight as u32;
    }
    if let Some(status) = body.get("status").and_then(|v| v.as_str()) {
        ch.status = status.to_string();
    }
    if let Some(models) = body.get("models").and_then(|v| v.as_array()) {
        ch.models = models
            .iter()
            .filter_map(|m| m.as_str().map(|s| s.to_string()))
            .collect();
    }
    if let Some(account_id) = body.get("account_id").and_then(|v| v.as_str()) {
        ch.account_id = account_id.to_string();
    }
    // 兼容布尔 enabled 字段：true → "enabled"，false → "disabled"
    if let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) {
        ch.status = if enabled {
            "enabled".to_string()
        } else {
            "disabled".to_string()
        };
    }

    match state.channel_store.update(&id, ch) {
        Ok(c) => Ok(Json(
            serde_json::json!({ "success": true, "data": mask_channel(&c) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to patch channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.channel_store.remove(&id) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_test_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = state
        .channel_store
        .get(&id)
        .ok_or_else(|| error_response("Channel not found", StatusCode::NOT_FOUND))?;
    let result = state.channel_store.test(&ch).await;
    if result.success {
        state.channel_store.mark_healthy(&id);
    } else {
        state
            .channel_store
            .mark_unhealthy(&id, result.message.clone());
    }
    Ok(Json(serde_json::json!({ "success": true, "data": result })))
}

#[derive(Debug, Deserialize)]
pub struct FetchModelsRequest {
    /// 渠道类型：cloudflare / openai_compatible / anthropic
    #[serde(default)]
    pub channel_type: String,
    /// 上游 Base URL（openai_compatible / anthropic）
    #[serde(default)]
    pub base_url: String,
    /// 明文 API Key（编辑已有渠道时可留空，从存储解码）
    #[serde(default)]
    pub api_key: String,
    /// 已有渠道 ID（编辑时用于回填密钥）
    #[serde(default)]
    pub channel_id: String,
}

/// POST /api/channels/fetch_models - 拉取上游支持的模型列表。
///
/// 前端浏览器直接请求第三方 /v1/models 会被 CORS 拦截，
/// 因此由后端代理转发：
/// - openai_compatible：GET {base_url}/models（Bearer 认证），兼容 /v1/models
/// - anthropic：GET {base_url}/v1/models（x-api-key + anthropic-version）
/// - cloudflare：走 cf-ai-gw Worker 的 /v1/models（用 cf_binding_url 或账号池）
#[axum::debug_handler]
pub async fn handle_fetch_channel_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FetchModelsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    // 密钥优先级：body 明文 > 已存渠道解码 > 空
    let api_key = if !body.api_key.is_empty() {
        body.api_key.clone()
    } else if !body.channel_id.is_empty() {
        state
            .channel_store
            .get(&body.channel_id)
            .map(|ch| ch.decode_api_key())
            .unwrap_or_default()
    } else {
        String::new()
    };

    let channel_type = crate::channel::ChannelType::from_str_lossy(&body.channel_type);
    let channel_id_for_save = body.channel_id.clone();
    let (url, models) = match channel_type {
        crate::channel::ChannelType::OpenaiCompatible => {
            let base = body.base_url.trim().trim_end_matches('/');
            if base.is_empty() {
                return Err(error_response(
                    "base_url is required",
                    StatusCode::BAD_REQUEST,
                ));
            }
            let mut url = format!("{base}/models");
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| {
                    error_response(
                        &format!("HTTP client error: {e}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            let mut req = client.get(&url).bearer_auth(&api_key);
            // 部分上游（如 OpenRouter）要求 /v1 前缀，若 /models 404 再试 /v1/models
            let resp = req.send().await;
            let resp = match resp {
                Ok(r) if r.status().as_u16() == 404 => {
                    url = format!("{base}/v1/models");
                    req = client.get(&url).bearer_auth(&api_key);
                    req.send().await.map_err(|e| {
                        error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY)
                    })?
                }
                Ok(r) => r,
                Err(e) => {
                    return Err(error_response(
                        &format!("Request failed: {e}"),
                        StatusCode::BAD_GATEWAY,
                    ));
                }
            };
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if status == 401 || status == 403 {
                return Err(error_response(
                    &format!("Auth failed: HTTP {status} (invalid api key?)"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !(200..300).contains(&status) {
                return Err(error_response(
                    &format!("Upstream returned HTTP {status}"),
                    StatusCode::BAD_GATEWAY,
                ));
            }
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                error_response(
                    "Upstream returned non-JSON response",
                    StatusCode::BAD_GATEWAY,
                )
            })?;
            let models: Vec<String> = json
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            (url, models)
        }
        crate::channel::ChannelType::Anthropic => {
            let base = body.base_url.trim().trim_end_matches('/');
            if base.is_empty() {
                return Err(error_response(
                    "base_url is required",
                    StatusCode::BAD_REQUEST,
                ));
            }
            let url = format!("{base}/v1/models");
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| {
                    error_response(
                        &format!("HTTP client error: {e}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            let resp = client
                .get(&url)
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
                .send()
                .await
                .map_err(|e| {
                    error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY)
                })?;
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if status == 401 || status == 403 {
                return Err(error_response(
                    &format!("Auth failed: HTTP {status} (invalid api key?)"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !(200..300).contains(&status) {
                return Err(error_response(
                    &format!("Upstream returned HTTP {status}"),
                    StatusCode::BAD_GATEWAY,
                ));
            }
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                error_response(
                    "Upstream returned non-JSON response",
                    StatusCode::BAD_GATEWAY,
                )
            })?;
            let models: Vec<String> = json
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            (url, models)
        }
        crate::channel::ChannelType::Cloudflare => {
            // Cloudflare 渠道：走 cf-ai-gw Worker（Binding 架构）
            let worker_url = if !body.base_url.trim().is_empty() {
                body.base_url.trim().trim_end_matches('/').to_string()
            } else {
                state
                    .config_manager
                    .get()
                    .await
                    .cf_binding_url
                    .trim()
                    .trim_end_matches('/')
                    .to_string()
            };
            let url = format!("{worker_url}/v1/models");
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| {
                    error_response(
                        &format!("HTTP client error: {e}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            let mut req = client.get(&url);
            if !api_key.is_empty() {
                req = req.bearer_auth(&api_key);
            }
            let resp = req.send().await.map_err(|e| {
                error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY)
            })?;
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if status == 401 || status == 403 {
                return Err(error_response(
                    &format!("Auth failed: HTTP {status} (invalid api key?)"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !(200..300).contains(&status) {
                return Err(error_response(
                    &format!("Upstream returned HTTP {status}"),
                    StatusCode::BAD_GATEWAY,
                ));
            }
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                error_response(
                    "Upstream returned non-JSON response",
                    StatusCode::BAD_GATEWAY,
                )
            })?;
            let models: Vec<String> = json
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            (url, models)
        }
        crate::channel::ChannelType::Gemini => {
            // Google Gemini 模型发现：GET {base_url}/models，用 x-goog-api-key 鉴权
            // 响应格式：{models: [{name: "models/gemini-pro", ...}]}
            let base = body.base_url.trim().trim_end_matches('/');
            let base = if base.is_empty() {
                "https://generativelanguage.googleapis.com/v1beta"
            } else {
                base
            };
            let url = format!("{base}/models");
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|e| {
                    error_response(
                        &format!("HTTP client error: {e}"),
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?;
            let resp = client
                .get(&url)
                .header("x-goog-api-key", &api_key)
                .send()
                .await
                .map_err(|e| {
                    error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY)
                })?;
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            if status == 401 || status == 403 {
                return Err(error_response(
                    &format!("Auth failed: HTTP {status} (invalid api key?)"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !(200..300).contains(&status) {
                return Err(error_response(
                    &format!("Upstream returned HTTP {status}"),
                    StatusCode::BAD_GATEWAY,
                ));
            }
            let json: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
                error_response(
                    "Upstream returned non-JSON response",
                    StatusCode::BAD_GATEWAY,
                )
            })?;
            // Gemini 响应：{models: [{name: "models/gemini-pro", ...}]}
            // 提取 name 字段并去掉 "models/" 前缀
            let models: Vec<String> = json
                .get("models")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| {
                            m.get("name")
                                .and_then(|i| i.as_str())
                                .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
                        })
                        .collect()
                })
                .unwrap_or_default();
            (url, models)
        }
        crate::channel::ChannelType::Zai => {
            // 智谱 AI（Z.AI）模型发现：智谱 AI 无公开 models 列表端点，
            // 返回常见 GLM 模型作为候选列表（管理员可手动调整）。
            let base = body.base_url.trim().trim_end_matches('/');
            let base = if base.is_empty() {
                "https://api.z.ai/api/v2"
            } else {
                base
            };
            let url = format!("{base}/models");
            // 智谱 AI 常见模型列表（无上游 API 可拉取，硬编码候选）
            let models: Vec<String> = vec![
                "glm-4-plus".to_string(),
                "glm-4-flash".to_string(),
                "glm-4-long".to_string(),
                "glm-4-air".to_string(),
                "glm-4-airx".to_string(),
                "glm-5".to_string(),
            ];
            (url, models)
        }
    };
    if let Some(cid) = channel_id_for_save.split(',').next() {
        if !cid.trim().is_empty() && state.channel_store.get(cid).is_some() {
            if let Err(e) = state
                .channel_store
                .save_discovered_models(cid, models.clone())
            {
                tracing::error!("Failed to persist discovered models for channel {cid}: {e}");
            }
        }
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "url": url,
            "models": models,
            "count": models.len(),
        }
    })))
}

// ── 渠道对话调试（Chat Tester）────────────────────────────────────────
// 在渠道管理页直接向某个渠道发起对话，验证上游能否正常出字。
// 与 /v1/chat/completions 走完整鉴权/计费链路不同，这里仅校验管理员，
// 并让前端自由选择协议（openai / anthropic）、模型与消息历史。

#[derive(Debug, Deserialize)]
pub struct ChannelChatTestRequest {
    pub channel_id: String,
    /// 协议：openai（/v1/chat/completions）或 anthropic（/v1/messages）
    #[serde(default = "default_chat_protocol")]
    pub protocol: String,
    /// 要发送的用户消息
    pub message: String,
    /// 对话历史（role/content），用于多轮调试
    #[serde(default)]
    pub history: Vec<Value>,
    /// 目标模型（默认取渠道 models[0] 或兜底 glm-4.7-flash）
    #[serde(default)]
    pub model: String,
    /// 是否流式返回（默认 true；流式时以 SSE 原样透传，前端解析增量）
    #[serde(default = "default_true")]
    pub stream: bool,
}

fn default_chat_protocol() -> String {
    "openai".to_string()
}

fn default_true() -> bool {
    true
}

/// 渠道对话调试入口。
///
/// 用渠道自身配置（type/base_url/api_key）直连上游，绕过 AIGX 的
/// 鉴权/定价/计费/调度，专注验证“这个渠道到底能不能对话”。
/// 返回：
/// - stream=true：`text/event-stream` SSE（OpenAI/Anthropic 原生增量）
/// - stream=false：`{success, data:{content, model, usage}}`
///
/// 说明：直接透传上游的 HTTP 状态/错误体，便于前端展示原始报错。
pub async fn handle_channel_chat_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelChatTestRequest>,
) -> Response {
    if let Err(e) = verify_admin(&state, &headers).await {
        return e.into_response();
    }

    let ch = match state.channel_store.get(&body.channel_id) {
        Some(c) => c,
        None => return error_response("Channel not found", StatusCode::NOT_FOUND).into_response(),
    };

    // 确定目标模型
    let model = if body.model.trim().is_empty() {
        ch.models
            .first()
            .cloned()
            .unwrap_or_else(|| "glm-4.7-flash".to_string())
    } else {
        body.model.trim().to_string()
    };

    // 构建消息列表：history + 当前消息
    let mut messages: Vec<Value> = body.history.clone();
    messages.push(serde_json::json!({ "role": "user", "content": body.message }));

    let api_key = ch.decode_api_key();
    let protocol = body.protocol.to_lowercase();
    let is_anthropic = protocol == "anthropic";

    // 构造上游 URL
    let base = {
        let b = if is_anthropic {
            // Anthropic：base_url 通常不含 /v1（上游根就是 api.anthropic.com）
            ch.base_url.trim().trim_end_matches('/').to_string()
        } else {
            // OpenAI 兼容：归一化补 /v1（与 bridge::openai::normalize_base_url 一致）
            crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string())
        };
        b
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                &format!("HTTP client error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response()
        }
    };

    let stream = body.stream;

    // ── OpenAI 协议 ──
    if !is_anthropic {
        let url = format!("{base}/chat/completions");
        let payload = serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": stream,
        });
        let mut req = client.post(&url).json(&payload);
        if !api_key.is_empty() {
            req = req.bearer_auth(&api_key);
        }
        return match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return error_response(
                        &format!("Upstream HTTP {status}: {text}"),
                        StatusCode::BAD_GATEWAY,
                    )
                    .into_response();
                }
                if stream {
                    // 流式：SSE 原样透传（将字节块包装为 sse::Event 事件）
                    let body = resp.bytes_stream();
                    let sse = axum::response::sse::Sse::new(body.map(|chunk| {
                        match chunk {
                            Ok(bytes) => Ok(axum::response::sse::Event::default()
                                .data(String::from_utf8_lossy(&bytes))),
                            Err(e) => Err(axum::Error::new(format!("upstream stream error: {e}"))),
                        }
                    }));
                    sse.into_response()
                } else {
                    match resp.json::<Value>().await {
                        Ok(json) => {
                            let content = json
                                .get("choices")
                                .and_then(|c| c.get(0))
                                .and_then(|c| c.get("message"))
                                .and_then(|m| m.get("content"))
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            Json(serde_json::json!({
                                "success": true,
                                "data": { "content": content, "model": model, "usage": json.get("usage") }
                            }))
                            .into_response()
                        }
                        Err(e) => error_response(
                            &format!("Upstream returned non-JSON: {e}"),
                            StatusCode::BAD_GATEWAY,
                        )
                        .into_response(),
                    }
                }
            }
            Err(e) => error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY)
                .into_response(),
        };
    }

    // ── Anthropic 协议 ──
    let url = format!("{base}/v1/messages");
    let mut payload = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": messages,
    });
    if stream {
        payload["stream"] = serde_json::json!(true);
    }
    let mut req = client.post(&url).json(&payload);
    if !api_key.is_empty() {
        req = req.header("x-api-key", &api_key);
    }
    req = req.header("anthropic-version", "2023-06-01");
    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return error_response(
                    &format!("Upstream HTTP {status}: {text}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response();
            }
            if stream {
                let body = resp.bytes_stream();
                let sse = axum::response::sse::Sse::new(body.map(|chunk| match chunk {
                    Ok(bytes) => Ok(
                        axum::response::sse::Event::default().data(String::from_utf8_lossy(&bytes)),
                    ),
                    Err(e) => Err(axum::Error::new(format!("upstream stream error: {e}"))),
                }));
                sse.into_response()
            } else {
                match resp.json::<Value>().await {
                    Ok(json) => {
                        let text = json
                            .get("content")
                            .and_then(|c| c.as_array())
                            .and_then(|arr| {
                                arr.iter().find(|p| {
                                    p.get("type").and_then(|t| t.as_str()) == Some("text")
                                })
                            })
                            .and_then(|p| p.get("text"))
                            .and_then(|t| t.as_str())
                            .unwrap_or("")
                            .to_string();
                        Json(serde_json::json!({
                            "success": true,
                            "data": {
                                "content": text,
                                "model": json.get("model"),
                                "usage": json.get("usage")
                            }
                        }))
                        .into_response()
                    }
                    Err(e) => error_response(
                        &format!("Upstream returned non-JSON: {e}"),
                        StatusCode::BAD_GATEWAY,
                    )
                    .into_response(),
                }
            }
        }
        Err(e) => {
            error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

// ============================================================
// 令牌管理增强（功能 2 - 核心数据层）
// ============================================================

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

fn mask_token(k: &super::auth::ApiKey) -> Value {
    let masked_key = if k.key.chars().count() > 8 {
        mask_with(&k.key, 4, 4, "...")
    } else {
        "****".to_string()
    };
    serde_json::json!({
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

pub async fn handle_list_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let tokens: Vec<Value> = state.api_key_store.list().iter().map(mask_token).collect();
    Ok(Json(serde_json::json!({ "success": true, "data": tokens })))
}

pub async fn handle_add_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let opts = super::auth::CreateApiKeyOptions {
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
            // F01（契约1）：创建响应一次性返回完整明文密钥——存储侧仅落哈希，
            // 明文离开本响应后无法再次获取；列表/更新接口仍走 mask_token 脱敏。
            // 前端 Keys.jsx 读取 created.plain_key || created.key 展示。
            let mut data = mask_token(&k);
            data["plain_key"] = serde_json::json!(k.key.clone());
            Ok(Json(serde_json::json!({ "success": true, "data": data })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to create token: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

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
        Ok(k) => Ok(Json(
            serde_json::json!({ "success": true, "data": mask_token(&k) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to update token: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

pub async fn handle_delete_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.api_key_store.delete(&id) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete token: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_reset_token_used(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if state.api_key_store.reset_used_quota(&id) {
        Ok(Json(serde_json::json!({ "success": true, "data": null })))
    } else {
        Err(error_response("Token not found", StatusCode::NOT_FOUND))
    }
}

// ============================================================
// 模型定价目录（功能 2 - 核心数据层）
// ============================================================

#[derive(Debug, Deserialize)]
pub struct PriceRequest {
    pub model_name: String,
    #[serde(default)]
    pub input_price: f64,
    #[serde(default)]
    pub output_price: f64,
    #[serde(default)]
    pub cache_price: Option<f64>,
    #[serde(default = "default_price_type")]
    pub price_type: String,
}

fn default_price_type() -> String {
    "token".to_string()
}

impl PriceRequest {
    fn to_model_price(&self) -> ModelPrice {
        let now = chrono::Utc::now().timestamp();
        ModelPrice {
            model_name: self.model_name.clone(),
            input_price: self.input_price,
            output_price: self.output_price,
            cache_price: self.cache_price,
            price_type: self.price_type.clone(),
            created_at: now,
            updated_at: now,
        }
    }
}

pub async fn handle_list_prices(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let prices = state.pricing_store.list_prices();
    Ok(Json(serde_json::json!({ "success": true, "data": prices })))
}

pub async fn handle_upsert_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PriceRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let price = body.to_model_price();
    match state.pricing_store.upsert_price(price) {
        Ok(p) => Ok(Json(serde_json::json!({ "success": true, "data": p }))),
        Err(e) => Err(error_response(
            &format!("Failed to save price: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_upsert_price_by_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
    Json(mut body): Json<PriceRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    body.model_name = model;
    let price = body.to_model_price();
    match state.pricing_store.upsert_price(price) {
        Ok(p) => Ok(Json(serde_json::json!({ "success": true, "data": p }))),
        Err(e) => Err(error_response(
            &format!("Failed to save price: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_delete_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.pricing_store.delete_price(&model) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete price: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

pub async fn handle_get_ratios(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ratios = state.pricing_store.get_ratios();
    Ok(Json(serde_json::json!({ "success": true, "data": ratios })))
}

pub async fn handle_update_ratios(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RatioConfig>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.pricing_store.update_ratios(body) {
        Ok(r) => Ok(Json(serde_json::json!({ "success": true, "data": r }))),
        Err(e) => Err(error_response(
            &format!("Failed to update ratios: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ============================================================
// 用户分组管理（功能 2 - 核心数据层）
// ============================================================

#[derive(Debug, Deserialize)]
pub struct GroupRequest {
    pub name: String,
    #[serde(default = "default_group_ratio")]
    pub ratio: f64,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub description: String,
}

fn default_group_ratio() -> f64 {
    1.0
}

impl GroupRequest {
    fn to_user_group(&self) -> UserGroup {
        let now = chrono::Utc::now().timestamp();
        UserGroup {
            name: self.name.clone(),
            ratio: self.ratio,
            allowed_models: self.allowed_models.clone(),
            description: self.description.clone(),
            created_at: now,
            updated_at: now,
        }
    }
}

pub async fn handle_list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let groups = state.user_group_store.list();
    Ok(Json(serde_json::json!({ "success": true, "data": groups })))
}

pub async fn handle_upsert_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GroupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let group = body.to_user_group();
    match state.user_group_store.upsert(group) {
        Ok(g) => Ok(Json(serde_json::json!({ "success": true, "data": g }))),
        Err(e) => Err(error_response(
            &format!("Failed to save group: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

pub async fn handle_upsert_group_by_name(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(mut body): Json<GroupRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    body.name = name;
    let group = body.to_user_group();
    match state.user_group_store.upsert(group) {
        Ok(g) => Ok(Json(serde_json::json!({ "success": true, "data": g }))),
        Err(e) => Err(error_response(
            &format!("Failed to save group: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

pub async fn handle_delete_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.user_group_store.remove(&name) {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete group: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

// ============================================================
// 日志与审计 API（功能 1）
// ============================================================

pub(crate) fn record_audit(
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

async fn admin_id_from_session(state: &AppState, headers: &HeaderMap) -> String {
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

#[derive(Debug, Deserialize)]
pub struct RequestLogQuery {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

fn default_page() -> usize {
    1
}

fn default_size() -> usize {
    20
}

pub async fn handle_list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RequestLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state.log_store.requests.list_with_filter(
        q.user.as_deref(),
        q.model.as_deref(),
        q.channel.as_deref(),
        q.start,
        q.end,
        q.page,
        q.size,
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": logs,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

pub async fn handle_list_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state.log_store.audits.list_paged(q.page, q.size);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": logs,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
}

pub async fn handle_export_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> Response {
    if verify_admin(&state, &headers).await.is_err() {
        return error_response("Not authenticated", StatusCode::UNAUTHORIZED).into_response();
    }
    let fmt = q.format.as_deref().unwrap_or("json").to_lowercase();
    if fmt == "csv" {
        let csv = state.log_store.requests.export_csv();
        (
            StatusCode::OK,
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "text/csv; charset=utf-8".to_string(),
                ),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"request_logs.csv\"".to_string(),
                ),
            ],
            csv,
        )
            .into_response()
    } else {
        let json = state.log_store.requests.export_json();
        (
            StatusCode::OK,
            [
                (
                    axum::http::header::CONTENT_TYPE,
                    "application/json; charset=utf-8".to_string(),
                ),
                (
                    axum::http::header::CONTENT_DISPOSITION,
                    "attachment; filename=\"request_logs.json\"".to_string(),
                ),
            ],
            json,
        )
            .into_response()
    }
}

// ============================================================
// 兑换码 API（功能 2）
// ============================================================

#[derive(Debug, Deserialize)]
pub struct BatchRedemptionRequest {
    pub count: usize,
    pub quota: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub expires_at: i64,
}

pub async fn handle_batch_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchRedemptionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state
        .redemption_store
        .batch_generate(body.count, body.quota, &body.name, body.expires_at)
    {
        Ok(codes) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "create",
                "redemptions:batch",
                None,
                Some(serde_json::json!({ "count": body.count, "quota": body.quota })),
            );
            Ok(Json(serde_json::json!({ "success": true, "data": codes })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to generate redemptions: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

pub async fn handle_list_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (items, total) = state.redemption_store.list_paged(q.page, q.size);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": items,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

pub async fn handle_delete_redemption(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.redemption_store.delete(&id) {
        Ok(_) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "delete",
                &format!("redemption:{id}"),
                None,
                None,
            );
            Ok(Json(serde_json::json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to delete redemption: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct RedeemRequest {
    pub code: String,
}

pub async fn handle_redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RedeemRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.redemption_store.redeem(&body.code, &user.id) {
        Ok(quota) => {
            if let Err(e) = state.user_store.add_quota(&user.id, quota) {
                tracing::error!("Failed to add quota after redemption: {e}");
                // B03 修复：入账失败时回滚兑换码为 unused，允许用户重试，
                // 避免配额永久丢失后反复报 already used。
                if let Err(re) = state.redemption_store.rollback_redeem(&body.code, &user.id) {
                    tracing::error!(
                        "CRITICAL: failed to rollback redemption {}: {re} (quota={} user={}), manual compensation required",
                        body.code, quota, user.id
                    );
                }
                return error_response("Failed to add quota", StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response();
            }
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "redeem",
                &format!("redemption:{}", body.code),
                None,
                Some(serde_json::json!({ "user_id": user.id, "quota": quota })),
            );
            Json(serde_json::json!({
                "success": true,
                "data": { "quota": quota },
                "message": format!("兑换成功，获得 {} 配额", quota),
            }))
            .into_response()
        }
        Err(e) => error_response(&e.to_string(), StatusCode::BAD_REQUEST).into_response(),
    }
}

// ============================================================
// 限流配置 API（功能 3）
// ============================================================

pub async fn handle_get_ratelimit_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let cfg = state.rate_limiter.config();
    Ok(Json(serde_json::json!({ "success": true, "data": cfg })))
}

pub async fn handle_update_ratelimit_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<crate::ratelimit::RateLimitConfig>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.rate_limiter.update_config(body) {
        Ok(cfg) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "update",
                "ratelimit:config",
                None,
                Some(serde_json::json!(cfg.clone())),
            );
            Ok(Json(serde_json::json!({ "success": true, "data": cfg })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to update ratelimit config: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ============================================================
// 数据看板增强 API（功能 4）
// ============================================================

/// Dashboard 查询参数：时间范围（天数）。
///
/// - 默认 30 天，最大 90 天，最小 1 天。
/// - 用于限制全量日志加载，避免性能退化。
#[derive(Debug, Deserialize)]
pub struct DashboardQuery {
    #[serde(default = "default_dashboard_days")]
    pub days: u32,
}

fn default_dashboard_days() -> u32 {
    30
}

/// 将 days 限制在 [1, 90] 区间，并返回对应的 unix timestamp 下界。
fn dashboard_start_ts(days: u32) -> i64 {
    let days = days.clamp(1, 90) as i64;
    chrono::Utc::now().timestamp() - days * 24 * 3600
}

pub async fn handle_consumption_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.all_sorted_asc();
    let mut daily: std::collections::BTreeMap<String, (i64, u64)> =
        std::collections::BTreeMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        let day = chrono::DateTime::<chrono::Utc>::from_timestamp(l.created_at, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let entry = daily.entry(day).or_insert((0, 0));
        entry.0 += l.cost;
        entry.1 += 1;
    }
    let data: Vec<Value> = daily
        .into_iter()
        .map(
            |(day, (cost, count))| serde_json::json!({ "date": day, "cost": cost, "count": count }),
        )
        .collect();
    Ok(Json(serde_json::json!({ "success": true, "data": data })))
}

pub async fn handle_model_distribution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.list_all();
    let mut by_model: HashMap<String, (u64, i64, u64)> = HashMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        let entry = by_model.entry(l.model.clone()).or_insert((0, 0, 0));
        entry.0 += 1;
        entry.1 += l.cost;
        entry.2 += l.input_tokens + l.output_tokens;
    }
    let data: Vec<Value> = by_model
        .into_iter()
        .map(|(model, (count, cost, tokens))| {
            serde_json::json!({ "model": model, "count": count, "cost": cost, "tokens": tokens })
        })
        .collect();
    Ok(Json(serde_json::json!({ "success": true, "data": data })))
}

pub async fn handle_user_ranking(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.list_all();
    let mut by_user: HashMap<String, (u64, i64, u64)> = HashMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        if let Some(uid) = &l.user_id {
            let entry = by_user.entry(uid.clone()).or_insert((0, 0, 0));
            entry.0 += 1;
            entry.1 += l.cost;
            entry.2 += l.input_tokens + l.output_tokens;
        }
    }
    let mut ranking: Vec<(String, u64, i64, u64)> = by_user
        .into_iter()
        .map(|(uid, (count, cost, tokens))| (uid, count, cost, tokens))
        .collect();
    ranking.sort_by_key(|b| std::cmp::Reverse(b.2));
    let data: Vec<Value> = ranking
        .into_iter()
        .take(20)
        .map(|(uid, count, cost, tokens)| {
            let email = state
                .user_store
                .get_by_id(&uid)
                .map(|u| u.email)
                .unwrap_or_else(|| uid.clone());
            serde_json::json!({ "user_id": uid, "email": email, "count": count, "cost": cost, "tokens": tokens })
        })
        .collect();
    Ok(Json(serde_json::json!({ "success": true, "data": data })))
}

pub async fn handle_channel_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let channels = state.channel_store.list();
    let logs = state.log_store.requests.list_all();
    let mut by_channel: HashMap<String, (u64, u64, u64)> = HashMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        if let Some(cid) = &l.channel_id {
            let entry = by_channel.entry(cid.clone()).or_insert((0, 0, 0));
            entry.0 += 1;
            if l.status_code < 400 {
                entry.1 += 1;
            }
            entry.2 += l.latency_ms;
        }
    }
    // 批次6：补充断路器状态与健康追踪快照（巡检同款信号源）
    let cb = state.channel_store.circuit_breaker();
    let data: Vec<Value> = channels
        .iter()
        .map(|ch| {
            let (total, success, total_latency) =
                by_channel.get(&ch.id).copied().unwrap_or((0, 0, 0));
            let success_rate = if total > 0 {
                (success as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let avg_latency = total_latency.checked_div(total).unwrap_or(0);
            let breaker = cb.get_state(&ch.id);
            let health = state
                .channel_store
                .health_tracker()
                .get_health(&ch.id)
                .map(|h| {
                    serde_json::json!({
                        "error_rate": h.overall_error_rate,
                        "avg_latency_ms": h.overall_avg_latency_ms,
                        "auth_ok": h.auth_ok,
                        "last_error": h.last_error,
                    })
                })
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "id": ch.id,
                "name": ch.name,
                "status": ch.status,
                "last_error": ch.last_error,
                "total_requests": total,
                "success_rate": success_rate,
                "avg_latency_ms": avg_latency,
                "circuit_breaker": breaker,
                "health": health,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "success": true, "data": data })))
}

/// POST /api/channels/:id/reset-circuit - 手动重置渠道断路器（管理面用）
pub async fn handle_reset_channel_circuit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if state.channel_store.get(&id).is_none() {
        return Err(error_response("Channel not found", StatusCode::NOT_FOUND));
    }
    state.channel_store.circuit_breaker().reset(&id);
    // 一并重置健康追踪状态，让渠道恢复后重新统计
    state.channel_store.health_tracker().reset(&id);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "message": "Circuit breaker reset" }
    })))
}

pub async fn handle_realtime(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let logs = state.log_store.requests.list_all();
    let now = chrono::Utc::now().timestamp();
    let window_secs = 5 * 60;
    let start = now - window_secs;
    let recent: Vec<_> = logs.into_iter().filter(|l| l.created_at >= start).collect();
    let total = recent.len() as u64;
    let errors = recent.iter().filter(|l| l.status_code >= 400).count() as u64;
    let total_latency: u64 = recent.iter().map(|l| l.latency_ms).sum();
    let avg_latency = total_latency.checked_div(total).unwrap_or(0);
    let qps = total as f64 / window_secs as f64;
    let error_rate = if total > 0 {
        (errors as f64 / total as f64) * 100.0
    } else {
        0.0
    };
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "window_secs": window_secs,
            "total_requests": total,
            "qps": qps,
            "avg_latency_ms": avg_latency,
            "error_rate": error_rate,
            "errors": errors,
        }
    })))
}

// ============================================================
// 通知系统 API（Telegram + SMTP）
// ============================================================

/// GET /api/notify/config - 获取通知配置（敏感字段脱敏）
pub async fn handle_get_notify_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "enabled": cfg.enabled,
            "telegram_bot_token": mask_sensitive(&cfg.telegram_bot_token),
            "telegram_chat_id": cfg.telegram_chat_id,
            "smtp_host": cfg.smtp_host,
            "smtp_port": cfg.smtp_port,
            "smtp_username": cfg.smtp_username,
            "smtp_password": mask_sensitive(&cfg.smtp_password),
            "smtp_from": cfg.smtp_from,
            "smtp_starttls": cfg.smtp_starttls,
            "slack_webhook_url": mask_sensitive(&cfg.slack_webhook_url),
            "webhook_url": cfg.webhook_url,
            "webhook_secret": mask_sensitive(&cfg.webhook_secret),
            "telegram_ready": cfg.telegram_ready(),
            "smtp_ready": cfg.smtp_ready(),
            "slack_ready": cfg.slack_ready(),
            "webhook_ready": cfg.webhook_ready(),
        }
    })))
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
    pub slack_webhook_url: Option<String>,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
}

/// PUT /api/notify/config - 更新通知配置（仅管理员）
///
/// 密码类字段：若提交的值形如脱敏格式（含 ***）则保留原值，否则覆盖。
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
        // F09: 空串与脱敏占位值不应覆盖已保存的凭据，先 trim 再判空
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
        // F09: 空串与脱敏占位值不应覆盖已保存的凭据，先 trim 再判空
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

    // 同步到 ConfigManager（持久化）
    let mut app_config = state.config_manager.get().await;
    app_config.notify = cfg.clone();
    state.config_manager.update(app_config).await.map_err(|e| {
        error_response(
            &format!("Failed to save notify config: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    // 同步到 NotifyService 运行时
    state.notify_service.update_config(cfg).await;

    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// POST /api/notify/test-telegram - 测试 Telegram（发送一条测试消息）
pub async fn handle_test_telegram(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;
    if !cfg.telegram_ready() {
        return Err(error_response(
            "Telegram bot_token 或 chat_id 未配置",
            StatusCode::BAD_REQUEST,
        ));
    }
    let text = "<b>🔔 AIGX 测试通知</b>\n\nTelegram 通知配置成功！";
    match state.notify_service.send_telegram(text).await {
        Ok(_) => Ok(Json(
            serde_json::json!({ "success": true, "data": "Telegram 测试消息已发送" }),
        )),
        Err(e) => Err(error_response(
            &format!("发送失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct TestEmailRequest {
    pub to: String,
}

/// POST /api/notify/test-email - 测试邮件（body: {to}）
pub async fn handle_test_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TestEmailRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;
    if !cfg.smtp_ready() {
        return Err(error_response(
            "SMTP host/port 未配置",
            StatusCode::BAD_REQUEST,
        ));
    }
    if body.to.is_empty() {
        return Err(error_response("收件邮箱不能为空", StatusCode::BAD_REQUEST));
    }
    let subject = "AIGX 测试邮件";
    let body_text = "这是一封来自 AIGX 的测试邮件。如果您收到此邮件，说明 SMTP 配置正确。";
    match state
        .notify_service
        .send_email(&body.to, subject, body_text)
        .await
    {
        Ok(_) => Ok(Json(
            serde_json::json!({ "success": true, "data": format!("测试邮件已发送至 {}", body.to) }),
        )),
        Err(e) => Err(error_response(
            &format!("发送失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ────────────────────────────────────────────────────────────────

/// POST /api/notify/test-slack - 测试 Slack Webhook
pub async fn handle_test_slack(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;
    if !cfg.slack_ready() {
        return Err(error_response(
            "Slack Webhook URL 未配置",
            StatusCode::BAD_REQUEST,
        ));
    }
    match state
        .notify_service
        .send_slack("info", "🔔 AIGX 测试告警 - Slack 通知配置成功！")
        .await
    {
        Ok(_) => Ok(Json(
            serde_json::json!({ "success": true, "data": "Slack 测试消息已发送" }),
        )),
        Err(e) => Err(error_response(
            &format!("发送失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct TestWebhookRequest {
    /// 可选：自定义测试载荷内容（默认 "AIGX 测试告警"）
    pub message: Option<String>,
}

/// POST /api/notify/test-webhook - 测试通用 Webhook
pub async fn handle_test_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<TestWebhookRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let cfg = state.notify_service.get_config().await;
    if !cfg.webhook_ready() {
        return Err(error_response(
            "Webhook URL 未配置",
            StatusCode::BAD_REQUEST,
        ));
    }
    let message = body
        .and_then(|Json(b)| b.message)
        .unwrap_or_else(|| "AIGX 测试告警".to_string());
    let payload = serde_json::json!({
        "source": "aigx",
        "event": "test",
        "message": message,
        "triggered_at": chrono::Utc::now().to_rfc3339(),
    });
    match state.notify_service.send_webhook(&payload).await {
        Ok(_) => Ok(Json(
            serde_json::json!({ "success": true, "data": "Webhook 测试消息已发送" }),
        )),
        Err(e) => Err(error_response(
            &format!("发送失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ============================================================
// 批次5：告警规则管理 API（参照 burncloud alert rules）
// ============================================================

/// GET /api/alerts/rules - 列出告警规则
pub async fn handle_alert_rules_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let rules = state.alert_evaluator.lock().unwrap().rules().to_vec();
    Ok(Json(serde_json::json!({ "success": true, "data": rules })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateAlertRulesRequest {
    /// 全量替换规则集
    pub rules: Vec<crate::notify::alert::AlertRule>,
}

/// PUT /api/alerts/rules - 更新告警规则集（全量替换 + 持久化）
pub async fn handle_alert_rules_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateAlertRulesRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    if body.rules.is_empty() {
        return Err(error_response(
            "规则集不能为空（如需禁用请设置 enabled=false）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let count = body.rules.len();
    {
        let mut ev = state.alert_evaluator.lock().unwrap();
        ev.set_rules(body.rules);
        ev.persist_rules(&state.alert_store);
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "updated": count }
    })))
}

/// GET /api/alerts/active - 当前活跃告警（静默期跟踪表）
pub async fn handle_alerts_active(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let alerts = state.alert_evaluator.lock().unwrap().active_alerts();
    Ok(Json(serde_json::json!({ "success": true, "data": alerts })))
}

/// GET /api/alerts/history - 告警触发历史（最新在前，环形 500 条）
#[derive(Debug, Deserialize)]
pub struct AlertHistoryQuery {
    /// 限制返回条数（默认 100，上限 500）
    pub limit: Option<usize>,
}

pub async fn handle_alerts_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AlertHistoryQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let limit = q
        .limit
        .unwrap_or(100)
        .min(crate::notify::alert::ALERT_HISTORY_LIMIT);
    let history: Vec<_> = state
        .alert_evaluator
        .lock()
        .unwrap()
        .history()
        .iter()
        .take(limit)
        .cloned()
        .collect();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "total": history.len(), "items": history }
    })))
}

#[derive(Debug, Deserialize)]
pub struct AlertTestRequest {
    /// 模拟的告警类型 kind（如 "memory_high"）
    pub kind: Option<String>,
    /// 模拟的当前值
    pub value: Option<u64>,
}

/// POST /api/alerts/test - 手动触发一条测试告警（走完整分发链路）
pub async fn handle_alert_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<AlertTestRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let (Json(b),) = match body {
        Some(j) => (j,),
        None => (Json(AlertTestRequest {
            kind: None,
            value: None,
        }),),
    };
    let kind = match b.kind.as_deref() {
        Some("channel_failure") => crate::notify::alert::AlertKind::ChannelFailure {
            channel_id: "test-channel".into(),
        },
        Some("channel_high_latency") => crate::notify::alert::AlertKind::ChannelHighLatency {
            channel_id: "test-channel".into(),
        },
        Some("channel_quota_low") => crate::notify::alert::AlertKind::ChannelQuotaLow {
            channel_id: "test-channel".into(),
        },
        Some("memory_high") | None => crate::notify::alert::AlertKind::MemoryHigh,
        Some("queue_backlog") => crate::notify::alert::AlertKind::QueueBacklog,
        Some("user_quota_exhausted") => crate::notify::alert::AlertKind::UserQuotaExhausted {
            user_id: "test-user".into(),
        },
        Some("abnormal_traffic") => crate::notify::alert::AlertKind::AbnormalTraffic,
        Some("cost_anomaly") => crate::notify::alert::AlertKind::CostAnomaly,
        Some(other) => {
            return Err(error_response(
                &format!("未知告警类型: {other}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    };
    let value = b.value.unwrap_or(99);
    let alert = {
        let mut ev = state.alert_evaluator.lock().unwrap();
        let alert = ev.evaluate(&kind, value);
        if alert.is_some() {
            ev.persist_history(&state.alert_store);
        }
        alert
    };
    match alert {
        Some(a) => {
            // 走完整分发（Telegram/Email/Slack/Webhook）
            let level_str = a.level.as_str();
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::AlertTriggered {
                    level: level_str.to_string(),
                    message: a.message.clone(),
                });
            let cfg = state.notify_service.get_config().await;
            if cfg.slack_ready() {
                let _ = state.notify_service.send_slack(level_str, &a.message).await;
            }
            if cfg.webhook_ready() {
                let payload = serde_json::json!({
                    "source": "aigx",
                    "event": "test",
                    "level": level_str,
                    "message": a.message,
                    "triggered_at": chrono::Utc::now().to_rfc3339(),
                });
                let _ = state.notify_service.send_webhook(&payload).await;
            }
            Ok(Json(serde_json::json!({
                "success": true,
                "data": { "triggered": true, "message": a.message }
            })))
        }
        None => Ok(Json(serde_json::json!({
            "success": true,
            "data": { "triggered": false, "message": "低于阈值或处于静默期，未触发" }
        }))),
    }
}

// ────────────────────────────────────────────────────────────────

/// 批次6：系统监控采集器（进程级单例——CPU 差分采样需要跨请求保留上次值）
static SYSTEM_COLLECTOR: std::sync::OnceLock<crate::monitor::SystemCollector> =
    std::sync::OnceLock::new();

/// GET /api/monitor/system - 系统资源快照（CPU/内存/负载/进程）
///
/// 参照 burncloud monitor collectors：CPU 差分采样需要两次请求，
/// 首次返回 usage 0（前端轮询场景天然满足）。
pub async fn handle_monitor_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let collector = SYSTEM_COLLECTOR.get_or_init(crate::monitor::SystemCollector::new);
    let snap = collector.snapshot();
    Ok(Json(serde_json::json!({ "success": true, "data": snap })))
}

// ────────────────────────────────────────────────────────────────
pub async fn handle_stripe_topup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let stripe = &state.stripe_client;
    if !stripe.config().ready() {
        return error_response("Stripe is not configured", StatusCode::BAD_REQUEST).into_response();
    }
    let amount = body.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
    if amount <= 0 {
        return error_response("amount must be positive", StatusCode::BAD_REQUEST).into_response();
    }
    let config = state.config_manager.get().await;
    let callback = callback_address(&state, &config);
    let success_url = if !stripe.config().success_url.is_empty() {
        stripe.config().success_url.clone()
    } else {
        format!("{}{}", callback, "/api/user/stripe/return")
    };
    let cancel_url = if !stripe.config().cancel_url.is_empty() {
        stripe.config().cancel_url.clone()
    } else {
        format!("{}{}", callback, "/api/user/stripe/cancel")
    };
    let trade_no = user::new_trade_no("STP", &user.id);
    let amount_cents = amount * 100;
    let quota = amount * 10000; // 1 cent = 10000 quota units (same ratio as epay)

    let order = crate::payment::TopUpOrder {
        trade_no: trade_no.clone(),
        user_id: user.id.clone(),
        amount,
        money: amount as f64,
        quota,
        payment_method: "stripe".into(),
        status: "pending".into(),
        create_time: chrono::Utc::now().timestamp(),
        paid_time: None,
    };
    if let Err(e) = state.order_store.insert(&order) {
        tracing::error!("Failed to create stripe order: {e}");
        return error_response("Failed to create order", StatusCode::INTERNAL_SERVER_ERROR)
            .into_response();
    }
    let params = crate::payment::stripe::CheckoutParams {
        trade_no: trade_no.clone(),
        user_id: user.id.clone(),
        amount_cents,
        quota,
        success_url,
        cancel_url,
    };
    match stripe.create_checkout_session(params).await {
        Ok(session) => Json(serde_json::json!({
            "success": true,
            "session_id": session.id,
            "url": session.url,
            "trade_no": trade_no,
        }))
        .into_response(),
        Err(e) => {
            tracing::error!("Stripe checkout failed: {e}");
            error_response(&e.to_string(), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

/// POST /api/user/stripe/webhook — Stripe Webhook 回调
pub async fn handle_stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let stripe = &state.stripe_client;
    if !stripe.config().ready() {
        return error_response("Stripe is not configured", StatusCode::BAD_REQUEST).into_response();
    }
    let sig_header = match headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
    {
        Some(s) => s.to_string(),
        None => {
            return error_response("Missing stripe-signature header", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };
    let event = match stripe.verify_webhook(&body, &sig_header) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Stripe webhook verification failed: {e}");
            return error_response("Invalid webhook signature", StatusCode::BAD_REQUEST)
                .into_response();
        }
    };
    // Only process checkout.session.completed
    if event.event_type != "checkout.session.completed" {
        return Json(serde_json::json!({"received": true})).into_response();
    }
    let obj = &event.data.object;
    if !obj.is_paid() {
        return Json(serde_json::json!({"received": true, "status": "unpaid"})).into_response();
    }
    let trade_no = match obj.trade_no() {
        Some(t) => t,
        None => {
            return error_response("No trade_no in event", StatusCode::BAD_REQUEST).into_response()
        }
    };
    // Atomically complete the order and credit quota
    match state.order_store.complete_if_pending(&trade_no) {
        Some(order) => {
            if let Err(e) = state.user_store.add_quota(&order.user_id, order.quota) {
                tracing::error!("Failed to add quota for order {}: {e}", trade_no);
                return error_response("Failed to add quota", StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response();
            }
            tracing::info!(
                "Stripe payment completed: order={}, user={}, quota={}",
                trade_no,
                order.user_id,
                order.quota
            );
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::PaymentSuccess {
                    user_email: order.user_id.clone(),
                    amount: order.amount as f64,
                    quota: order.quota,
                });
            Json(serde_json::json!({"received": true, "completed": true})).into_response()
        }
        None => {
            // Already processed or doesn't exist ? idempotent success
            Json(serde_json::json!({"received": true, "already_processed": true})).into_response()
        }
    }
}

// ────────────────────────────────────────────────────────────────

/// GET /api/auth/github — 跳转到 GitHub OAuth 授权页
pub async fn handle_github_oauth_authorize(State(state): State<AppState>) -> Response {
    let config = state.config_manager.get().await;
    let oauth = &config.github_oauth;
    if !oauth.ready() {
        return error_response("GitHub OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let state_param = uuid::Uuid::new_v4().to_string();
    let url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=user:email&state={}",
        oauth.client_id,
        oauth.redirect_uri,
        state_param,
    );
    Redirect::to(&url).into_response()
}

/// GET /api/auth/github/callback — GitHub OAuth 回调处理
pub async fn handle_github_oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<GithubCallbackParams>,
) -> Response {
    let config = state.config_manager.get().await;
    let oauth = config.github_oauth.clone();
    if !oauth.ready() {
        return error_response("GitHub OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let code = match params.code {
        Some(c) => c,
        None => {
            return error_response("Missing authorization code", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };
    // Exchange code for access token
    let access_token =
        match crate::oauth::github::exchange_code(&oauth, &code, &state.http_client).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("GitHub OAuth token exchange failed: {e}");
                return error_response("OAuth token exchange failed", StatusCode::BAD_GATEWAY)
                    .into_response();
            }
        };
    // Fetch user info
    let gh_user = match crate::oauth::github::get_user_info(&access_token, &state.http_client).await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("GitHub OAuth user info failed: {e}");
            return error_response("Failed to fetch GitHub user info", StatusCode::BAD_GATEWAY)
                .into_response();
        }
    };
    // Determine email: prefer primary email, fallback to github id pseudo-email
    let email = gh_user
        .email
        .clone()
        .unwrap_or_else(|| format!("{}@github.local", gh_user.login));
    // Find or create user
    let user = match state.user_store.get_by_email(&email) {
        Some(u) => u,
        None => {
            // Auto-create user for GitHub OAuth
            match state.user_store.create_with_username(
                &email,
                &gh_user.login,
                &uuid::Uuid::new_v4().to_string(), // random password (OAuth users don't use password login)
                crate::user::Role::User,
                0,
            ) {
                Ok(u) => u,
                Err(e) => {
                    tracing::error!("Failed to create OAuth user: {e}");
                    return error_response(
                        "Failed to create user",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                    .into_response();
                }
            }
        }
    };
    // Create session
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_secret = if config.admin.session_secret.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        config.admin.session_secret.clone()
    };
    let session_store = super::auth::SessionStore::new(&session_secret, session_ttl);
    let session = session_store.create_session(&user.email);
    Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "email": user.email,
            "username": user.username,
            "role": match user.role {
                crate::user::Role::Admin => "admin",
                crate::user::Role::User => "user",
            },
            "expires_at": session.expires_at,
        }
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct GithubCallbackParams {
    pub code: Option<String>,
    #[allow(dead_code)]
    pub state: Option<String>,
}

// ============================================================
// 阶段1：管理 API 缺失功能（与 burncloud 业务对齐）
//
// 参照 burncloud `crates/server/src/api/{auth,cache,security,openapi,token,user}.rs`，
// 在 AIGX 中以单 crate + axum 0.7 + rusqlite/sea-orm 架构实现等价能力。
// 以下 handler 均为新增，不修改任何现有 handler 的签名或逻辑。
// ============================================================

// ── 功能 1：忘记密码 / 重置密码 ──────────────────────────────────────
//
// 参照 burncloud `api/auth.rs::forgot_password` / `reset_password`。
// AIGX 无邮件发送设施，采用无状态 HMAC token 方案：复用 SessionStore 签名
// 机制生成重置 token（token 内含 email + expires_at + HMAC 签名），
// forgot_password 返回 token（由前端/管理员负责安全分发），
// reset_password 验证签名后更新密码。零存储、零新依赖。

/// 重置密码 token 有效期（小时）
const PASSWORD_RESET_TTL_HOURS: i64 = 1;

#[derive(Debug, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

/// POST /api/auth/forgot-password — 忘记密码，生成重置 token
///
/// 参照 burncloud `auth.rs::forgot_password`。
/// 与 burncloud 不同：AIGX 无邮件发送设施，直接返回 token（仅此一次），
/// 由前端/管理员负责安全分发。用户不存在时仍返回成功（防止邮箱枚举），
/// 但不返回有效 token。
pub async fn handle_forgot_password(
    State(state): State<AppState>,
    Json(body): Json<ForgotPasswordRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.email.trim().is_empty() {
        return Err(error_response("邮箱不能为空", StatusCode::BAD_REQUEST));
    }

    let config = state.config_manager.get().await;
    let session_secret = if config.admin.session_secret.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        config.admin.session_secret.clone()
    };

    // 用户不存在时返回成功但不返回 token（防止邮箱枚举）
    let user = match state.user_store.get_by_email(body.email.trim()) {
        Some(u) => u,
        None => {
            tracing::info!(
                "Forgot password requested for unknown email (no token issued): {}",
                body.email
            );
            return Ok(Json(serde_json::json!({
                "success": true,
                "data": {
                    "message": "If the email exists, a reset token has been generated",
                    "token": null
                }
            })));
        }
    };

    let session_store = SessionStore::new(&session_secret, PASSWORD_RESET_TTL_HOURS);
    let session = session_store.create_session(&user.email);

    tracing::info!("Password reset token generated for {}", user.email);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "message": "If the email exists, a reset token has been generated",
            "token": session.token,
            "expires_in_secs": PASSWORD_RESET_TTL_HOURS * 3600
        }
    })))
}

/// POST /api/auth/reset-password — 重置密码
///
/// 参照 burncloud `auth.rs::reset_password`。
/// 验证 token 签名与有效期，通过后更新用户密码。
pub async fn handle_reset_password(
    State(state): State<AppState>,
    Json(body): Json<ResetPasswordRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.new_password.chars().count() < 6 {
        return Err(error_response("密码长度至少6位", StatusCode::BAD_REQUEST));
    }

    let config = state.config_manager.get().await;
    let session_secret = if config.admin.session_secret.is_empty() {
        return Err(error_response(
            "Reset password not available: session secret not configured",
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    } else {
        config.admin.session_secret.clone()
    };

    let session_store = SessionStore::new(&session_secret, PASSWORD_RESET_TTL_HOURS);
    let sess = session_store
        .validate_session(&body.token)
        .ok_or_else(|| error_response("Invalid or expired reset token", StatusCode::BAD_REQUEST))?;

    // 查找用户
    let user = state
        .user_store
        .get_by_email(&sess.email)
        .ok_or_else(|| error_response("User not found", StatusCode::NOT_FOUND))?;

    // 更新密码
    let new_hash = hash_password(&body.new_password);
    match state.user_store.update(&user.id, |u| {
        u.password = new_hash;
    }) {
        Ok(_) => {
            tracing::info!("Password reset successful for {}", user.email);
            Ok(Json(serde_json::json!({
                "success": true,
                "data": { "message": "Password reset successful" }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Password reset failed: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── 功能 2：Google OAuth ─────────────────────────────────────────────
//
// 参照 burncloud `api/auth.rs::oauth_google` 及 AIGX 既有 `handle_github_oauth_*` 模式。
// 配置存于 `config.google_oauth`（`src/oauth/google.rs` + `src/config.rs`）。

/// GET /api/auth/google — 跳转到 Google OAuth 授权页
pub async fn handle_google_oauth_authorize(State(state): State<AppState>) -> Response {
    let config = state.config_manager.get().await;
    let oauth = &config.google_oauth;
    if !oauth.ready() {
        return error_response("Google OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let state_param = uuid::Uuid::new_v4().to_string();
    let url = crate::oauth::google::build_authorize_url(oauth, &state_param);
    Redirect::to(&url).into_response()
}

/// GET /api/auth/google/callback — Google OAuth 回调处理
pub async fn handle_google_oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<GoogleCallbackParams>,
) -> Response {
    let config = state.config_manager.get().await;
    let oauth = config.google_oauth.clone();
    if !oauth.ready() {
        return error_response("Google OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let code = match params.code {
        Some(c) => c,
        None => {
            return error_response("Missing authorization code", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };
    // 用授权码换取 access token
    let access_token =
        match crate::oauth::google::exchange_code(&oauth, &code, &state.http_client).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("Google OAuth token exchange failed: {e}");
                return error_response("OAuth token exchange failed", StatusCode::BAD_GATEWAY)
                    .into_response();
            }
        };
    // 拉取用户信息
    let g_user = match crate::oauth::google::get_user_info(&access_token, &state.http_client).await
    {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("Google OAuth user info failed: {e}");
            return error_response("Failed to fetch Google user info", StatusCode::BAD_GATEWAY)
                .into_response();
        }
    };
    // 确定邮箱：优先 email，否则用 sub 造伪邮箱
    let email = g_user
        .email
        .clone()
        .unwrap_or_else(|| format!("{}@google.local", g_user.sub));
    // 用户名：优先 name，否则用 sub
    let username = g_user.name.clone().unwrap_or_else(|| g_user.sub.clone());
    // 查找或创建用户
    let user = match state.user_store.get_by_email(&email) {
        Some(u) => u,
        None => match state.user_store.create_with_username(
            &email,
            &username,
            &uuid::Uuid::new_v4().to_string(), // 随机密码（OAuth 用户不走密码登录）
            crate::user::Role::User,
            0,
        ) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("Failed to create Google OAuth user: {e}");
                return error_response("Failed to create user", StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response();
            }
        },
    };
    // 创建会话
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_secret = if config.admin.session_secret.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        config.admin.session_secret.clone()
    };
    let session_store = SessionStore::new(&session_secret, session_ttl);
    let session = session_store.create_session(&user.email);
    Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "email": user.email,
            "username": user.username,
            "role": match user.role {
                crate::user::Role::Admin => "admin",
                crate::user::Role::User => "user",
            },
            "expires_at": session.expires_at,
        }
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct GoogleCallbackParams {
    pub code: Option<String>,
    #[allow(dead_code)]
    pub state: Option<String>,
}

// ── 功能 3：缓存管理 API ────────────────────────────────────────────
//
// 参照 burncloud `api/cache.rs::stats` / `clear`。
// AIGX 的 `AppState.response_cache` 是 `AsyncCache<String, Value>`，
// 暴露 `entry_count()` 与 `invalidate_all()`。

/// GET /api/cache/stats — 查看缓存统计
pub async fn handle_cache_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let entry_count = state.response_cache.entry_count();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "entry_count": entry_count,
            "max_capacity": 1000,
            "ttl_secs": 300,
            "description": "response_cache (exact-match prompt cache)"
        }
    })))
}

/// POST /api/cache/clear — 清空缓存
pub async fn handle_cache_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    state.response_cache.invalidate_all();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "message": "Cache cleared" }
    })))
}

// ── 功能 4：安全监控 ────────────────────────────────────────────────
//
// 参照 burncloud `api/security.rs::security_summary` / `security_events`。
// AIGX 复用 `log_store.requests`（RequestLog）中 status_code >= 400 的记录
// 作为风险事件。sparkline 按 7 天聚合。

/// GET /api/monitor/security — 安全汇总
pub async fn handle_security_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 优先基于结构化安全事件计算概览；请求日志仅作兜底
    let events = state.log_store.security.list_all();
    let total = events.len().max(1) as u64;
    let mut critical = 0u64;
    let mut threat_sources = std::collections::HashSet::new();
    let mut recent_24h = 0u64;
    let now = chrono::Utc::now().timestamp();
    for ev in &events {
        if ev.severity == "critical" {
            critical += 1;
        }
        if let Some(ref ip) = ev.ip {
            threat_sources.insert(ip.clone());
        }
        if let Some(ref uid) = ev.user_id {
            threat_sources.insert(uid.clone());
        }
        if now - ev.created_at <= 24 * 3600 {
            recent_24h += 1;
        }
    }
    let score = if events.is_empty() {
        100
    } else {
        (100.0 - (critical as f64 / total as f64) * 100.0).round() as u8
    };
    // 7 天 sparkline
    let mut buckets: [u64; 7] = [0; 7];
    for ev in &events {
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(ev.created_at, 0);
        if let Some(dt) = dt {
            let days_ago = (chrono::Utc::now() - dt).num_days().clamp(0, 6) as usize;
            buckets[6 - days_ago] += 1;
        }
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "score": score,
            "total_events": total,
            "critical_events": critical,
            "recent_24h": recent_24h,
            "blocked_count": total,
            "threat_source_count": threat_sources.len(),
            "sparkline": buckets.to_vec()
        }
    })))
}

/// GET /api/monitor/security/events — 安全事件列表（分页）
#[derive(Debug, Deserialize)]
pub struct SecurityEventsQuery {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
    pub r#type: Option<String>,
    pub range: Option<String>,
}

pub async fn handle_security_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SecurityEventsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(20).clamp(1, 100);

    // 时间范围：1h/24h/7d/30d → start 时间戳
    let start = params.range.as_deref().and_then(|r| {
        let secs = match r {
            "1h" => 3600,
            "24h" => 24 * 3600,
            "7d" => 7 * 24 * 3600,
            "30d" => 30 * 24 * 3600,
            _ => return None,
        };
        Some(chrono::Utc::now().timestamp() - secs)
    });

    let (events, total) = state.log_store.security.list_paged(
        params.r#type.as_deref(),
        start,
        None,
        page as usize,
        page_size as usize,
    );

    let page_items: Vec<Value> = events
        .into_iter()
        .map(|ev| {
            serde_json::json!({
                "id": ev.id,
                "created_at": ev.created_at,
                "time": ev.created_at,
                "event_type": ev.event_type.as_str(),
                "type": ev.event_type.as_str(),
                "severity": ev.severity,
                "ip": ev.ip,
                "client_ip": ev.ip,
                "user_id": ev.user_id,
                "request_id": ev.request_id,
                "detail": ev.detail,
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "events": page_items,
            "total": total,
            "page": page,
            "page_size": page_size
        }
    })))
}

// ── 功能 5：OpenAPI 文档 + Swagger UI ────────────────────────────────
//
// 参照 burncloud `api/openapi.rs::openapi_json` / `swagger_ui`。
// 手动构建 OpenAPI 3.0 JSON spec（列出 AIGX 主要端点），避免引入 utoipa 新依赖。
// Swagger UI 通过 CDN 引入 swagger-ui-dist，与 burncloud 一致。

/// GET /api-docs/openapi.json — OpenAPI 3.0 JSON spec
pub async fn handle_openapi_json() -> Json<Value> {
    let version = env!("CARGO_PKG_VERSION");
    Json(serde_json::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "AIGX API",
            "description": "AIGX — OpenAI-compatible AI gateway with multi-account Cloudflare Workers AI and Epay support",
            "version": version,
            "contact": {
                "name": "AIGX Team",
                "url": "https://github.com/AIGX"
            }
        },
        "servers": [{ "url": "/", "description": "Current server" }],
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "JWT",
                    "description": "Session token via Authorization: Bearer {token}"
                }
            },
            "schemas": {
                "ErrorResponse": {
                    "type": "object",
                    "properties": {
                        "success": { "type": "boolean", "example": false },
                        "error": { "type": "string" }
                    }
                },
                "LoginRequest": {
                    "type": "object",
                    "required": ["email", "password"],
                    "properties": {
                        "email": { "type": "string" },
                        "password": { "type": "string" }
                    }
                },
                "RegisterRequest": {
                    "type": "object",
                    "required": ["email", "password"],
                    "properties": {
                        "email": { "type": "string" },
                        "password": { "type": "string" },
                        "username": { "type": "string" }
                    }
                },
                "ChatCompletionRequest": {
                    "type": "object",
                    "required": ["model", "messages"],
                    "properties": {
                        "model": { "type": "string" },
                        "messages": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "role": { "type": "string", "enum": ["system", "user", "assistant"] },
                                    "content": { "type": "string" }
                                }
                            }
                        },
                        "temperature": { "type": "number" },
                        "max_tokens": { "type": "integer" },
                        "stream": { "type": "boolean", "default": false }
                    }
                }
            }
        },
        "paths": {
            "/api/auth/login": {
                "post": {
                    "tags": ["Authentication"],
                    "summary": "用户登录",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/LoginRequest" } } } },
                    "responses": { "200": { "description": "登录成功" }, "401": { "description": "认证失败" } }
                }
            },
            "/api/auth/register": {
                "post": {
                    "tags": ["Authentication"],
                    "summary": "用户注册",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/RegisterRequest" } } } },
                    "responses": { "200": { "description": "注册成功" }, "409": { "description": "用户名已存在" } }
                }
            },
            "/api/auth/forgot-password": {
                "post": {
                    "tags": ["Authentication"],
                    "summary": "忘记密码，生成重置 token",
                    "responses": { "200": { "description": "重置 token 已生成" } }
                }
            },
            "/api/auth/reset-password": {
                "post": {
                    "tags": ["Authentication"],
                    "summary": "重置密码",
                    "responses": { "200": { "description": "重置成功" }, "400": { "description": "无效 token" } }
                }
            },
            "/api/auth/google": {
                "get": { "tags": ["Authentication"], "summary": "Google OAuth 授权跳转", "responses": { "302": { "description": "重定向到 Google" } } }
            },
            "/api/auth/google/callback": {
                "get": { "tags": ["Authentication"], "summary": "Google OAuth 回调", "responses": { "200": { "description": "登录成功" } } }
            },
            "/api/cache/stats": {
                "get": { "tags": ["Cache"], "summary": "查看缓存统计", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "缓存统计" } } }
            },
            "/api/cache/clear": {
                "post": { "tags": ["Cache"], "summary": "清空缓存", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "缓存已清空" } } }
            },
            "/api/monitor/security": {
                "get": { "tags": ["Security"], "summary": "安全汇总", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "安全评分与汇总" } } }
            },
            "/api/monitor/security/events": {
                "get": { "tags": ["Security"], "summary": "安全事件列表", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "分页风险事件" } } }
            },
            "/api/tokens": {
                "get": { "tags": ["Token"], "summary": "列出令牌", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "令牌列表" } } },
                "post": { "tags": ["Token"], "summary": "创建令牌", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "令牌已创建" } } }
            },
            "/api/tokens/:id/rotate": {
                "post": { "tags": ["Token"], "summary": "轮换令牌", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "新令牌" } } }
            },
            "/api/playground/chat": {
                "post": { "tags": ["Playground"], "summary": "Playground 聊天测试", "security": [{"bearerAuth": []}], "responses": { "200": { "description": "聊天响应" } } }
            },
            "/api/users/check": {
                "get": { "tags": ["User"], "summary": "检查用户名/邮箱是否可用", "responses": { "200": { "description": "可用性结果" } } }
            },
            "/v1/chat/completions": {
                "post": {
                    "tags": ["LLM API"],
                    "summary": "Chat completions (OpenAI 兼容)",
                    "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ChatCompletionRequest" } } } },
                    "responses": { "200": { "description": "Chat completion 响应" }, "401": { "description": "未授权" }, "429": { "description": "限流" } }
                }
            },
            "/health": {
                "get": { "tags": ["System"], "summary": "健康检查", "responses": { "200": { "description": "服务健康" } } }
            }
        }
    }))
}

/// GET /swagger-ui — Swagger UI HTML 页面
pub async fn handle_swagger_ui() -> axum::response::Html<&'static str> {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>AIGX API Documentation</title>
    <link rel="stylesheet" type="text/css" href="https://unpkg.com/swagger-ui-dist@5/swagger-ui.css">
    <style>
        html { box-sizing: border-box; overflow: -moz-scrollbars-vertical; overflow-y: scroll; }
        *, *:before, *:after { box-sizing: inherit; }
        body { margin: 0; padding: 0; background: #fafafa; }
    </style>
</head>
<body>
    <div id="swagger-ui"></div>
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
    <script src="https://unpkg.com/swagger-ui-dist@5/swagger-ui-standalone-preset.js"></script>
    <script>
        window.onload = function() {
            const ui = SwaggerUIBundle({
                url: "/api-docs/openapi.json",
                dom_id: '#swagger-ui',
                presets: [
                    SwaggerUIBundle.presets.apis,
                    SwaggerUIStandalonePreset
                ],
                layout: "StandaloneLayout",
                deepLinking: true,
                displayOperationId: false,
                defaultModelsExpandDepth: 1,
                defaultModelExpandDepth: 1,
                docExpansion: "list",
                syntaxHighlight: {
                    activate: true,
                    theme: "monokai"
                }
            });
            window.ui = ui;
        };
    </script>
</body>
</html>"#;
    axum::response::Html(html)
}

// ── 功能 6：令牌轮换 ────────────────────────────────────────────────
//
// 参照 burncloud `api/token.rs::rotate_token`。
// AIGX 的 `ApiKeyStore::update` 已处理 key 变更后的 hash_map 重算，
// 故在此 handler 内直接生成新 key 并通过 update 写入，保留 name/group/quota 等设置。

/// POST /api/tokens/:id/rotate — 轮换 API token
///
/// 生成新 key，旧 key 立即失效（hash_map 移除旧 hash），保留配额/分组等设置。
/// 新 key 仅在此响应中返回一次（与 burncloud 行为一致）。
pub async fn handle_rotate_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let new_key = format!("sk-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
    match state.api_key_store.update(&id, |k| {
        k.key = new_key.clone();
    }) {
        Ok(k) => {
            tracing::info!(token_id = %id, "API token rotated");
            Ok(Json(serde_json::json!({
                "success": true,
                "data": {
                    "id": k.id,
                    "key": k.key,
                    "rotated_at": k.updated_at,
                    "message": "Token rotated; old key invalidated"
                }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to rotate token: {e}"),
            StatusCode::NOT_FOUND,
        )),
    }
}

// ── 功能 7：Playground ──────────────────────────────────────────────
//
// 参照 burncloud `api/token.rs::playground_chat`。
// burncloud 用 data_plane.oneshot 走内部路由；AIGX 无此设施，
// 改为复用 `channel_store` 选渠道 + `http_client` 直连上游
//（与既有 `handle_channel_chat_test` 同源，但 playground 固定非流式 OpenAI 协议，
// 且 channel_id 可选——缺省时选第一个启用渠道）。

#[derive(Debug, Deserialize)]
pub struct PlaygroundChatRequest {
    pub model: String,
    pub messages: Vec<Value>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<i64>,
    /// 可选渠道 ID；缺省时选第一个启用渠道
    #[serde(default)]
    pub channel_id: Option<String>,
}

/// POST /api/playground/chat — Playground 聊天测试
pub async fn handle_playground_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundChatRequest>,
) -> Response {
    if let Err(e) = verify_admin(&state, &headers).await {
        return e.into_response();
    }

    // 选渠道：优先 channel_id，否则第一个启用渠道
    let ch = if let Some(ref cid) = body.channel_id {
        match state.channel_store.get(cid) {
            Some(c) => c,
            None => {
                return error_response("Channel not found", StatusCode::NOT_FOUND).into_response()
            }
        }
    } else {
        match state
            .channel_store
            .list()
            .into_iter()
            .find(|c| c.is_enabled())
        {
            Some(c) => c,
            None => {
                return error_response(
                    "No enabled channel available for playground",
                    StatusCode::BAD_REQUEST,
                )
                .into_response()
            }
        }
    };

    let model = if body.model.trim().is_empty() {
        ch.models
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-3.5-turbo".to_string())
    } else {
        body.model.trim().to_string()
    };

    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/chat/completions");

    let mut payload = serde_json::json!({
        "model": model,
        "messages": body.messages,
        "stream": false,
    });
    if let Some(t) = body.temperature {
        payload["temperature"] = serde_json::json!(t);
    }
    if let Some(m) = body.max_tokens {
        payload["max_tokens"] = serde_json::json!(m);
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                &format!("HTTP client error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response()
        }
    };

    let mut req = client.post(&url).json(&payload);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return error_response(
                    &format!("Upstream HTTP {status}: {text}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response();
            }
            match resp.json::<Value>().await {
                Ok(json) => {
                    let content = json
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    Json(serde_json::json!({
                        "success": true,
                        "data": {
                            "content": content,
                            "model": model,
                            "usage": json.get("usage")
                        }
                    }))
                    .into_response()
                }
                Err(e) => error_response(
                    &format!("Upstream returned non-JSON: {e}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response(),
            }
        }
        Err(e) => {
            error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

// ── 功能 8：用户名检查 ──────────────────────────────────────────────
//
// 参照 burncloud `api/user.rs::check_username`。
// 检查用户名/邮箱是否已存在（注册前预检），无需鉴权。

#[derive(Debug, Deserialize)]
pub struct CheckUsernameQuery {
    /// 用户名或邮箱
    pub username: String,
}

/// GET /api/users/check?username=xxx — 检查用户名/邮箱是否可用
pub async fn handle_check_username(
    State(state): State<AppState>,
    Query(params): Query<CheckUsernameQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let name = params.username.trim();
    if name.is_empty() {
        return Err(error_response(
            "username cannot be empty",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 同时检查 username 与 email 两个维度
    let exists = state.user_store.get_by_username(name).is_some()
        || state.user_store.get_by_email(name).is_some();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "available": !exists }
    })))
}

// ============================================================
// 批次3/4 补齐：IP 过滤 + 价格同步 + 汇率管理 API
// （此前前端已调用这些端点但后端未实现，属前后端契约断裂）
// ============================================================

/// GET /api/ip/filter - 获取全局 IP 过滤配置
pub async fn handle_get_ip_filter(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let filter = state.ip_filter.get();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "enabled": filter.enabled,
            "whitelist": filter.whitelist,
            "blacklist": filter.blacklist,
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateIpFilterRequest {
    pub enabled: Option<bool>,
}

/// PUT /api/ip/filter - 更新 IP 过滤开关
pub async fn handle_update_ip_filter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateIpFilterRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    if let Some(enabled) = body.enabled {
        state.ip_filter.set_enabled(enabled).map_err(|e| {
            error_response(
                &format!("Failed to update IP filter: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    }
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

#[derive(Debug, Deserialize)]
pub struct AddIpRuleRequest {
    pub pattern: String,
    #[serde(default)]
    pub note: String,
}

/// POST /api/ip/whitelist - 添加白名单规则
pub async fn handle_add_ip_whitelist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddIpRuleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    if body.pattern.trim().is_empty() {
        return Err(error_response(
            "pattern is required",
            StatusCode::BAD_REQUEST,
        ));
    }
    state
        .ip_filter
        .add_whitelist(body.pattern.trim(), body.note.trim())
        .map_err(|e| {
            error_response(
                &format!("Failed to add whitelist: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// POST /api/ip/blacklist - 添加黑名单规则
pub async fn handle_add_ip_blacklist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AddIpRuleRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    if body.pattern.trim().is_empty() {
        return Err(error_response(
            "pattern is required",
            StatusCode::BAD_REQUEST,
        ));
    }
    state
        .ip_filter
        .add_blacklist(body.pattern.trim(), body.note.trim())
        .map_err(|e| {
            error_response(
                &format!("Failed to add blacklist: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// DELETE /api/ip/whitelist/:pattern - 移除白名单规则
pub async fn handle_remove_ip_whitelist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pattern): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    state.ip_filter.remove_whitelist(&pattern).map_err(|e| {
        error_response(
            &format!("Failed to remove whitelist: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// DELETE /api/ip/blacklist/:pattern - 移除黑名单规则
pub async fn handle_remove_ip_blacklist(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(pattern): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    state.ip_filter.remove_blacklist(&pattern).map_err(|e| {
        error_response(
            &format!("Failed to remove blacklist: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// GET /api/pricing/sync-config - 获取价格同步配置
pub async fn handle_get_price_sync_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let svc = state.price_sync.lock().unwrap();
    let cfg = svc.config();
    let last_sync = svc.last_remote_sync().map(|t| t.to_rfc3339());
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "enabled": cfg.remote_sync_enabled,
            "sync_url": cfg.remote_url,
            "sync_url_fallback": cfg.remote_url_fallback,
            "interval_secs": cfg.remote_sync_interval_secs,
            "last_sync": last_sync,
        }
    })))
}

/// PUT /api/pricing/sync-config - 更新价格同步配置
#[derive(Debug, Deserialize)]
pub struct UpdatePriceSyncConfigRequest {
    pub enabled: Option<bool>,
    pub sync_url: Option<String>,
    pub sync_url_fallback: Option<String>,
    pub interval_secs: Option<u64>,
}

pub async fn handle_update_price_sync_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdatePriceSyncConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let mut svc = state.price_sync.lock().unwrap();
    if let Some(v) = body.enabled {
        svc.set_remote_sync_enabled(v);
    }
    if let Some(v) = body.sync_url {
        if !v.trim().is_empty() {
            svc.set_remote_url(v);
        }
    }
    if let Some(v) = body.sync_url_fallback {
        svc.set_remote_url_fallback(Some(v));
    }
    if let Some(v) = body.interval_secs {
        svc.set_remote_sync_interval(v);
    }
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}

/// POST /api/pricing/sync - 手动触发价格同步
pub async fn handle_trigger_price_sync(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let result = state
        .price_sync
        .lock()
        .unwrap()
        .sync_all(true)
        .await
        .map_err(|e| {
            error_response(
                &format!("Price sync failed: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "models_synced": result.models_synced,
            "errors": result.errors,
            "source": result.source,
        }
    })))
}

/// GET /api/pricing/exchange-rates - 获取全部汇率（扁平对象：{CNY: 7.2, ...}）
pub async fn handle_get_exchange_rates(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    // 返回 {币种: 相对 USD 的汇率}，USD 恒为 1.0
    let mut map = serde_json::Map::new();
    map.insert("USD".to_string(), serde_json::json!(1.0));
    for (from, to, rate, _updated) in state.exchange_rate.list_rates() {
        if from.code() == "USD" {
            map.insert(to.code().to_string(), serde_json::json!(rate));
        }
    }
    Ok(Json(serde_json::json!({ "success": true, "data": map })))
}

/// PUT /api/pricing/exchange-rates - 更新汇率（扁平对象：{CNY: 7.2, ...}）
pub async fn handle_update_exchange_rates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<HashMap<String, f64>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    for (currency, rate) in &body {
        if currency == "USD" {
            continue; // USD 是基准，忽略
        }
        if rate <= &0.0 {
            return Err(error_response(
                "rate must be positive",
                StatusCode::BAD_REQUEST,
            ));
        }
        let cur: crate::pricing::exchange_rate::Currency =
            currency
                .parse()
                .map_err(|e: crate::pricing::exchange_rate::CurrencyParseError| {
                    error_response(&format!("invalid currency: {e}"), StatusCode::BAD_REQUEST)
                })?;
        // 存储 USD -> 该币种 的汇率（1 USD = rate 币种）
        state
            .exchange_rate
            .set_rate_persisted(crate::pricing::exchange_rate::Currency::Usd, cur, *rate)
            .map_err(|e| {
                error_response(
                    &format!("Failed to persist rate: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
    }
    Ok(Json(serde_json::json!({ "success": true, "data": null })))
}
