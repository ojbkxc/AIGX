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
use crate::config::AppConfig;
use crate::graphql;
use crate::payment::{Device, EpayConfig, PurchaseArgs};
use crate::user::{self, Role, User};

use super::auth::SessionStore;
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

/// 密钥请求
#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub name: String,
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
    if body.email != "admin" {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    }

    let password_hash = hash_password(&body.password);
    let is_first_login = config.admin.password.is_empty();

    if is_first_login {
        let mut new_config = config.clone();
        new_config.admin.password = password_hash;
        new_config.admin.session_secret = uuid::Uuid::new_v4().to_string();
        if let Err(e) = state.config_manager.update(new_config.clone()).await {
            tracing::error!("Failed to save config: {e}");
        }
        let _ = state.user_store.create_with_username("admin", "admin", &body.password, Role::Admin, 0);
    } else if !user::verify_password(&body.password, &config.admin.password) {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    }

    let session_store = SessionStore::new(&config.admin.session_secret, 24);
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
pub async fn handle_register(
    State(state): State<AppState>,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
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

    let http_client = reqwest::Client::new();
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

    let http_client = reqwest::Client::new();
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
            let masked_token = if a.api_token.len() > 8 {
                format!("{}...{}", &a.api_token[..4], &a.api_token[a.api_token.len() - 4..])
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
            let masked_key = if k.key.len() > 8 {
                format!("{}...{}", &k.key[..4], &k.key[k.key.len() - 4..])
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
        state.model_mapper.reset().ok();
        for (source, target) in &body.mappings {
            state.model_mapper.set_custom(source.clone(), target.clone()).ok();
        }
    } else {
        // 逐个添加/更新
        for (source, target) in &body.mappings {
            state.model_mapper.set_custom(source.clone(), target.clone()).ok();
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

/// 计算密码哈希（兼容旧 admin 模式，使用 user 模块的 hash_password）
fn hash_password(password: &str) -> String {
    user::hash_password(password)
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
}

fn default_user_role() -> String {
    "user".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    pub email: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub role: Option<String>,
    pub quota: Option<i64>,
    pub status: Option<String>,
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
        Ok(u) => Ok(Json(serde_json::json!({ "success": true, "data": mask_user(&u) }))),
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
pub async fn handle_get_epay_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "pay_address": _config.epay.pay_address,
            "epay_id": _config.epay.epay_id,
            "epay_key": _config.epay.epay_key,
            "pay_methods": _config.epay.pay_methods,
            "price": _config.epay.price,
            "min_topup": _config.epay.min_topup,
            "custom_callback_address": _config.epay.custom_callback_address,
            "server_address": _config.server_address,
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct UpdateEpayConfigRequest {
    pub pay_address: Option<String>,
    pub epay_id: Option<String>,
    pub epay_key: Option<String>,
    pub pay_methods: Option<Vec<String>>,
    pub price: Option<f64>,
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
        device: Device::PC,
    };
    match epay.purchase(&args) {
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
pub async fn handle_epay_notify(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> Response {
    let bytes = match axum::body::to_bytes(body, 64 * 1024).await {
        Ok(b) => b,
        Err(_) => return "fail".into_response(),
    };
    let params = collect_params(None, &bytes);
    let mut params = if params.is_empty() { query.clone() } else { params };
    if params.is_empty() && !query.is_empty() {
        params = query.clone();
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
        return "fail".into_response();
    }
    if let Some(order) = state.order_store.get(&verify.out_trade_no) {
        if order.is_pending() {
            // 入账：amount 即元数，按 epay.price 转为配额
            let quota_to_add = (order.amount as f64 * config.epay.price).round() as i64;
            let _ = state.user_store.add_quota(&order.user_id, quota_to_add);
            let _ = state.order_store.complete(&order.trade_no);
            tracing::info!("Epay order completed: trade_no={} user={} quota=+{}", order.trade_no, order.user_id, quota_to_add);
        }
    }
    "success".into_response()
}

/// POST/GET /api/user/epay/return - 易支付同步跳转
pub async fn handle_epay_return(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    body: axum::body::Body,
) -> Response {
    let bytes = axum::body::to_bytes(body, 64 * 1024).await.unwrap_or_default();
    let params = collect_params(None, &bytes);
    let params = if params.is_empty() { query.clone() } else { params };
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
    if verify.trade_status == "TRADE_SUCCESS" {
        if let Some(order) = state.order_store.get(&verify.out_trade_no) {
            if order.is_pending() {
                let quota_to_add = (order.amount as f64 * config.epay.price).round() as i64;
                let _ = state.user_store.add_quota(&order.user_id, quota_to_add);
                let _ = state.order_store.complete(&order.trade_no);
            }
        }
        return Redirect::to(&make_return_path("success")).into_response();
    }
    Redirect::to(&make_return_path("pending")).into_response()
}
