use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
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
async fn verify_admin(state: &AppState, headers: &HeaderMap) -> Result<AppConfig, (StatusCode, Json<Value>)> {
    let token = extract_session_token(headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;

    let config = state.config_manager.get().await;

    // 备用验证：直接比较 token 与 session_secret（简化模式）
    if !config.admin.session_secret.is_empty() && token == config.admin.session_secret {
        return Ok(config);
    }

    // 使用 session store 验证
    let session_store = SessionStore::new(&config.admin.session_secret, 24);
    if let Some(sess) = session_store.validate_session(&token) {
        // 若存在用户系统，校验该用户仍为管理员且启用
        if let Some(u) = state.user_store.get_by_email(&sess.email) {
            if u.status == "active" && u.is_admin() {
                return Ok(config);
            }
            return Err(error_response("User disabled or not admin", StatusCode::UNAUTHORIZED));
        }
        // 兼容模式：旧 admin 账户
        if sess.email == "admin" {
            return Ok(config);
        }
    }

    Err(error_response("Invalid session", StatusCode::UNAUTHORIZED))
}

/// 验证任意已登录用户，返回用户记录
async fn verify_user(state: &AppState, headers: &HeaderMap) -> Result<User, (StatusCode, Json<Value>)> {
    let token = extract_session_token(headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;
    let config = state.config_manager.get().await;
    let session_store = SessionStore::new(&config.admin.session_secret, 24);
    let sess = session_store
        .validate_session(&token)
        .ok_or_else(|| error_response("Invalid session", StatusCode::UNAUTHORIZED))?;
    let user = state
        .user_store
        .get_by_email(&sess.email)
        .or_else(|| {
            // 回退：用户系统为空时合成 admin 用户
            if sess.email == "admin" {
                Some(User {
                    id: "admin".into(),
                    email: "admin".into(),
                    username: "admin".into(),
                    password: String::new(),
                    role: Role::Admin,
                    quota: 0,
                    used_quota: 0,
                    status: "active".into(),
                    group: "default".into(),
                    created_at: 0,
                })
            } else {
                None
            }
        })
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
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config_manager.get().await;

    // 优先走用户系统
    if let Some(u) = state.user_store.authenticate(&body.email, &body.password) {
        // H3：旧 SHA256 密码登录成功后自动升级为 argon2（best-effort rehash）
        // 旧版本用无盐 SHA256（64 位十六进制）存储密码，新版本统一用 argon2。
        // rehash 失败不阻止登录，仅记录告警。
        if u.password.len() == 64 && u.password.chars().all(|c| c.is_ascii_hexdigit()) {
            let new_hash = hash_password(&body.password);
            if let Err(e) = state.user_store.update(&u.id, |user| {
                user.password = new_hash;
            }) {
                tracing::warn!("Failed to rehash legacy SHA256 password for {}: {e}", u.email);
            }
        }
        let session_secret = if config.admin.session_secret.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            config.admin.session_secret.clone()
        };
        let session_store = SessionStore::new(&session_secret, 24);
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

    // 回退：旧 admin 模式（首次使用）
    //
    // [L7 遗留] 旧 admin 密码模式：使用 config.admin.password（明文哈希）而非 user_store。
    // 当前启动时 `ensure_default_admin` 已用 user_store 创建随机密码管理员，正常流程
    // 不会走到此分支。保留作为 user_store 创建失败时的兜底回退，避免首次登录完全不可用。
    // 后续可在确认 user_store 初始化可靠后移除，并删除 config.admin.password 字段。
    if body.email != "admin" {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    }

    let password_hash = hash_password(&body.password);
    let is_first_login = config.admin.password.is_empty();

    let session_secret = if is_first_login {
        let mut new_config = config.clone();
        new_config.admin.password = password_hash;
        new_config.admin.session_secret = if config.admin.session_secret.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            config.admin.session_secret.clone()
        };
        if let Err(e) = state.config_manager.update(new_config.clone()).await {
            tracing::error!("Failed to save config: {e}");
        }
        if let Err(e) = state.user_store.create_with_username("admin", "admin", &body.password, Role::Admin, 0) {
            tracing::error!("Failed to create admin user on first login: {e}");
        }
        new_config.admin.session_secret
    } else if !user::verify_password(&body.password, &config.admin.password) {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    } else {
        // H3：旧 SHA256 密码登录成功后自动升级为 argon2（best-effort rehash）
        if config.admin.password.len() == 64
            && config.admin.password.chars().all(|c| c.is_ascii_hexdigit())
        {
            let new_hash = hash_password(&body.password);
            let mut new_config = config.clone();
            new_config.admin.password = new_hash;
            if let Err(e) = state.config_manager.update(new_config).await {
                tracing::warn!("Failed to rehash legacy admin password: {e}");
            }
        }
        config.admin.session_secret.clone()
    };

    let session_store = SessionStore::new(&session_secret, 24);
    let session = session_store.create_session(&body.email);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "email": body.email,
            "username": "admin",
            "role": "admin",
            "expires_at": session.expires_at
        }
    })))
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
    if body.password.len() < 6 {
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
            state.user_store.create_with_username(body.email.trim(), username.trim(), &body.password, Role::User, default_quota)
                .map_err(|e| error_response(&format!("注册失败: {e}"), StatusCode::BAD_REQUEST))?
        } else {
            state.user_store.create(body.email.trim(), &body.password, Role::User, default_quota)
                .map_err(|e| error_response(&format!("注册失败: {e}"), StatusCode::BAD_REQUEST))?
        }
    } else {
        state.user_store.create(body.email.trim(), &body.password, Role::User, default_quota)
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
        Ok(result) => {
            Ok(Json(serde_json::json!({
                "success": true,
                "data": {
                    "message": result.message,
                    "models": result.models,
                    "inference": result.inference,
                    "analytics": result.analytics,
                    "overall": result.success,
                }
            })))
        }
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
            if let Err(e) = state.model_mapper.set_custom(source.clone(), target.clone()) {
                tracing::error!("Failed to set mapping {} -> {}: {}", source, target, e);
            }
        }
    } else {
        // 逐个添加/更新
        for (source, target) in &body.mappings {
            if let Err(e) = state.model_mapper.set_custom(source.clone(), target.clone()) {
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
            state.user_store.create_with_username(&body.email, username.trim(), &body.password, role, body.quota)
        } else {
            state.user_store.create(&body.email, &body.password, role, body.quota)
        }
    } else {
        state.user_store.create(&body.email, &body.password, role, body.quota)
    };
    match result {
        Ok(u) => {
            // 应用请求指定的 group（非空且非默认时更新）
            let final_user = if !body.group.is_empty() && body.group != "default" {
                match state.user_store.update(&u.id, |x| x.group = body.group.clone()) {
                    Ok(updated) => updated,
                    Err(_) => u,
                }
            } else {
                u
            };
            Ok(Json(serde_json::json!({ "success": true, "data": mask_user(&final_user) })))
        }
        Err(e) => Err(error_response(&format!("Failed to create user: {e}"), StatusCode::BAD_REQUEST)),
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
        Ok(u) => Ok(Json(serde_json::json!({ "success": true, "data": mask_user(&u) }))),
        Err(e) => Err(error_response(&format!("Failed to update user: {e}"), StatusCode::BAD_REQUEST)),
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
        Err(e) => Err(error_response(&format!("Failed to delete user: {e}"), StatusCode::BAD_REQUEST)),
    }
}

/// GET /api/users/me - 当前登录用户信息
pub async fn handle_me(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let u = verify_user(&state, &headers).await?;
    Ok(Json(serde_json::json!({ "success": true, "data": mask_user(&u) })))
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
    if let Some(v) = body.pay_address { config.epay.pay_address = v; }
    if let Some(v) = body.epay_id { config.epay.epay_id = v; }
    if let Some(v) = body.epay_key { config.epay.epay_key = v; }
    if let Some(v) = body.pay_methods { config.epay.pay_methods = v; }
    if let Some(v) = body.price { config.epay.price = v; }
    if let Some(v) = body.amount_discount { config.epay.amount_discount = v; }
    if let Some(v) = body.min_topup { config.epay.min_topup = v; }
    if let Some(v) = body.custom_callback_address { config.epay.custom_callback_address = v; }
    if let Some(v) = body.server_address { config.server_address = v; }
    state.config_manager.update(config.clone()).await.map_err(|e| {
        error_response(&format!("Failed to save config: {e}"), StatusCode::INTERNAL_SERVER_ERROR)
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

fn pay_money(epay: &EpayConfig, amount: i64) -> f64 {
    let discount = *epay.amount_discount.get(&amount).filter(|d| **d > 0.0).unwrap_or(&1.0);
    let money = (amount as f64) * epay.price * discount;
    (money * 100.0).round() / 100.0
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
        return error_response("Payment method not supported", StatusCode::BAD_REQUEST).into_response();
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
        payment_method: body.payment_method.clone(),
        status: "pending".into(),
        create_time: chrono::Utc::now().timestamp(),
        paid_time: None,
    };
    if let Err(e) = state.order_store.insert(&order) {
        tracing::error!("Failed to create order: {e}");
        return error_response("Failed to create order", StatusCode::INTERNAL_SERVER_ERROR).into_response();
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
            })).into_response()
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

fn urlencoding_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'+' {
            out.push(' ');
            i += 1;
        } else if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(b as char);
                i += 3;
                continue;
            }
            out.push('%');
            i += 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
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
        tracing::warn!("Epay notify: verify failed or trade not success, trade_status={}", verify.trade_status);
        return "fail".into_response();
    }

    // 提取回调金额用于校验
    let callback_money: f64 = params
        .get("money")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);

    if let Some(order) = state.order_store.get(&verify.out_trade_no) {
        if !order.is_pending() {
            return "success".into_response(); // 幂等：已处理
        }

        // 金额校验：容忍度 max(订单金额 * 2%, 0.01元)，参照 VFaka
        let tolerance = (order.money * 0.02).max(0.01);
        let amount_diff = (callback_money - order.money).abs();
        if amount_diff > tolerance {
            tracing::error!(
                "Epay notify amount mismatch: trade_no={} expected={} received={} diff={}",
                order.trade_no, order.money, callback_money, amount_diff
            );
            return "fail".into_response();
        }

        // 入账：使用 order.money / price 计算配额（正确处理折扣场景）
        // 下单时 pay_money 已应用折扣，回调时用实际支付金额反算配额
        let quota_to_add = if config.epay.price > 0.0 {
            (order.money / config.epay.price).round() as i64
        } else {
            order.amount
        };
        if let Err(e) = state.user_store.add_quota(&order.user_id, quota_to_add) {
            tracing::error!("Epay notify: failed to add quota for order {}: {}", order.trade_no, e);
        }
        if let Err(e) = state.order_store.complete(&order.trade_no) {
            tracing::error!("Epay notify: failed to complete order {}: {}", order.trade_no, e);
        }
        tracing::info!(
            "Epay order completed: trade_no={} user={} amount={} money={} quota=+{}",
            order.trade_no, order.user_id, order.amount, order.money, quota_to_add
        );

        // 通知：充值成功（异步，不阻塞回调）
        let user_email = state
            .user_store
            .get_by_id(&order.user_id)
            .map(|u| u.email)
            .unwrap_or_default();
        state.notify_service.notify_spawn(crate::notify::NotifyEvent::PaymentSuccess {
            user_email,
            amount: order.money,
            quota: quota_to_add,
        });
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
    let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap_or_default();
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
            if order.is_pending() {
                // 金额校验：容忍度 max(订单金额 * 2%, 0.01元)
                let tolerance = (order.money * 0.02).max(0.01);
                if (callback_money - order.money).abs() > tolerance {
                    tracing::error!(
                        "Epay return amount mismatch: trade_no={} expected={} received={}",
                        order.trade_no, order.money, callback_money
                    );
                    return Redirect::to(&make_return_path("fail")).into_response();
                }

                // 使用 order.money / price 计算配额（正确处理折扣）
                let quota_to_add = if config.epay.price > 0.0 {
                    (order.money / config.epay.price).round() as i64
                } else {
                    order.amount
                };
                if let Err(e) = state.user_store.add_quota(&order.user_id, quota_to_add) {
                    tracing::error!("Epay return: failed to add quota for order {}: {}", order.trade_no, e);
                }
                if let Err(e) = state.order_store.complete(&order.trade_no) {
                    tracing::error!("Epay return: failed to complete order {}: {}", order.trade_no, e);
                }
                tracing::info!(
                    "Epay return completed: trade_no={} user={} quota=+{}",
                    order.trade_no, order.user_id, quota_to_add
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
    let channels: Vec<Value> = state.channel_store.list().iter().map(mask_channel).collect();
    Ok(Json(serde_json::json!({ "success": true, "data": channels })))
}

pub async fn handle_add_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = body.to_channel(String::new());
    match state.channel_store.add(ch) {
        Ok(c) => Ok(Json(serde_json::json!({ "success": true, "data": mask_channel(&c) }))),
        Err(e) => Err(error_response(&format!("Failed to add channel: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Ok(c) => Ok(Json(serde_json::json!({ "success": true, "data": mask_channel(&c) }))),
        Err(e) => Err(error_response(&format!("Failed to update channel: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        ch.status = if enabled { "enabled".to_string() } else { "disabled".to_string() };
    }

    match state.channel_store.update(&id, ch) {
        Ok(c) => Ok(Json(serde_json::json!({ "success": true, "data": mask_channel(&c) }))),
        Err(e) => Err(error_response(&format!("Failed to patch channel: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to delete channel: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
    }
}

pub async fn handle_test_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = state.channel_store.get(&id).ok_or_else(|| error_response("Channel not found", StatusCode::NOT_FOUND))?;
    let result = state.channel_store.test(&ch).await;
    if result.success {
        state.channel_store.mark_healthy(&id);
    } else {
        state.channel_store.mark_unhealthy(&id, result.message.clone());
    }
    Ok(Json(serde_json::json!({ "success": true, "data": result })))
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
        Ok(k) => Ok(Json(serde_json::json!({ "success": true, "data": mask_token(&k) }))),
        Err(e) => Err(error_response(&format!("Failed to create token: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        if let Some(n) = &body.name { k.name = n.clone(); }
        if let Some(g) = &body.group { k.group = g.clone(); }
        if let Some(m) = &body.allowed_models { k.allowed_models = Some(m.clone()); }
        if let Some(e) = body.expires_at { k.expires_at = Some(e); }
        if let Some(q) = body.quota_limit { k.quota_limit = Some(q); }
        if let Some(ip) = &body.ip_limit { k.ip_limit = Some(ip.clone()); }
        if let Some(s) = &body.status { k.status = s.clone(); }
    }) {
        Ok(k) => Ok(Json(serde_json::json!({ "success": true, "data": mask_token(&k) }))),
        Err(e) => Err(error_response(&format!("Failed to update token: {e}"), StatusCode::BAD_REQUEST)),
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
        Err(e) => Err(error_response(&format!("Failed to delete token: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to save price: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to save price: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to delete price: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to update ratios: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
        Err(e) => Err(error_response(&format!("Failed to save group: {e}"), StatusCode::BAD_REQUEST)),
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
        Err(e) => Err(error_response(&format!("Failed to save group: {e}"), StatusCode::BAD_REQUEST)),
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
        Err(e) => Err(error_response(&format!("Failed to delete group: {e}"), StatusCode::BAD_REQUEST)),
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
    state.log_store.record_audit(admin_id, action, target, before, after);
}

async fn admin_id_from_session(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(token) = extract_session_token(headers) {
        let config = state.config_manager.get().await;
        let session_store = SessionStore::new(&config.admin.session_secret, 24);
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
                (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
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
                (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
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
    match state.redemption_store.batch_generate(body.count, body.quota, &body.name, body.expires_at) {
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
        Err(e) => Err(error_response(&format!("Failed to generate redemptions: {e}"), StatusCode::BAD_REQUEST)),
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
            record_audit(&state, &admin_id, "delete", &format!("redemption:{id}"), None, None);
            Ok(Json(serde_json::json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(&format!("Failed to delete redemption: {e}"), StatusCode::BAD_REQUEST)),
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
                return error_response("Failed to add quota", StatusCode::INTERNAL_SERVER_ERROR).into_response();
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
            record_audit(&state, &admin_id, "update", "ratelimit:config", None, Some(serde_json::json!(cfg.clone())));
            Ok(Json(serde_json::json!({ "success": true, "data": cfg })))
        }
        Err(e) => Err(error_response(&format!("Failed to update ratelimit config: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
    let mut daily: std::collections::BTreeMap<String, (i64, u64)> = std::collections::BTreeMap::new();
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
        .map(|(day, (cost, count))| {
            serde_json::json!({ "date": day, "cost": cost, "count": count })
        })
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
    ranking.sort_by(|a, b| b.2.cmp(&a.2));
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
    let data: Vec<Value> = channels
        .iter()
        .map(|ch| {
            let (total, success, total_latency) = by_channel.get(&ch.id).copied().unwrap_or((0, 0, 0));
            let success_rate = if total > 0 {
                (success as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            let avg_latency = if total > 0 {
                total_latency / total
            } else {
                0
            };
            serde_json::json!({
                "id": ch.id,
                "name": ch.name,
                "status": ch.status,
                "last_error": ch.last_error,
                "total_requests": total,
                "success_rate": success_rate,
                "avg_latency_ms": avg_latency,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "success": true, "data": data })))
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
    let avg_latency = if total > 0 {
        recent.iter().map(|l| l.latency_ms).sum::<u64>() / total
    } else {
        0
    };
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
            "telegram_ready": cfg.telegram_ready(),
            "smtp_ready": cfg.smtp_ready(),
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

    if let Some(v) = body.enabled { cfg.enabled = v; }
    if let Some(v) = body.telegram_bot_token {
        if !v.contains("***") { cfg.telegram_bot_token = v; }
    }
    if let Some(v) = body.telegram_chat_id { cfg.telegram_chat_id = v; }
    if let Some(v) = body.smtp_host { cfg.smtp_host = v; }
    if let Some(v) = body.smtp_port { cfg.smtp_port = v; }
    if let Some(v) = body.smtp_username { cfg.smtp_username = v; }
    if let Some(v) = body.smtp_password {
        if !v.contains("***") { cfg.smtp_password = v; }
    }
    if let Some(v) = body.smtp_from { cfg.smtp_from = v; }

    // 同步到 ConfigManager（持久化）
    let mut app_config = state.config_manager.get().await;
    app_config.notify = cfg.clone();
    state.config_manager.update(app_config).await.map_err(|e| {
        error_response(&format!("Failed to save notify config: {e}"), StatusCode::INTERNAL_SERVER_ERROR)
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
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": "Telegram 测试消息已发送" }))),
        Err(e) => Err(error_response(&format!("发送失败: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
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
    match state.notify_service.send_email(&body.to, subject, body_text).await {
        Ok(_) => Ok(Json(serde_json::json!({ "success": true, "data": format!("测试邮件已发送至 {}", body.to) }))),
        Err(e) => Err(error_response(&format!("发送失败: {e}"), StatusCode::INTERNAL_SERVER_ERROR)),
    }
}
