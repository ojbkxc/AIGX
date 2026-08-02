use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::account::CfAccount;
use crate::config::AppConfig;
use crate::graphql;

use super::auth::SessionStore;
use super::openai::AppState;

/// 登录请求
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
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
    if session_store.validate_session(&token).is_some() {
        return Ok(config);
    }

    Err(error_response("Invalid session", StatusCode::UNAUTHORIZED))
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

/// POST /api/auth/login - 管理员登录
pub async fn handle_login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = state.config_manager.get().await;

    // 验证用户名和密码
    if body.username != "admin" {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    }

    // 验证密码
    let password_hash = hash_password(&body.password);
    let is_first_login = config.admin.password.is_empty();

    if is_first_login {
        // 首次登录：设置密码
        let mut new_config = config.clone();
        new_config.admin.password = password_hash;
        new_config.admin.session_secret = uuid::Uuid::new_v4().to_string();
        if let Err(e) = state.config_manager.update(new_config).await {
            tracing::error!("Failed to save config: {e}");
        }
    } else if password_hash != config.admin.password {
        return Err(error_response("Invalid credentials", StatusCode::UNAUTHORIZED));
    }

    // 生成会话 token
    let session_store = SessionStore::new(&config.admin.session_secret, 24);
    let session = session_store.create_session(&body.username);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "username": body.username,
            "expires_at": session.expires_at
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

/// 计算密码哈希
fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}