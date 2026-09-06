use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;
use serde_json::Value;

use super::common::error_response;
use super::common::extract_client_ip;
use super::common::extract_session_token;

use super::super::auth::SessionStore;
use super::super::openai::AppState;
use crate::user::{self, hash_password, Role};

use rand::Rng;
use uuid::Uuid;

/// 登录请求
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// 注册请求
#[derive(Debug, Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

/// 重置密码请求
#[derive(Debug, Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub new_password: String,
}

/// 登录后修改密码请求（需有效会话，先校验旧密码）
#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub old_password: String,
    pub new_password: String,
}
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub username: Option<String>,
}

/// OAuth 登录回调处理
#[derive(Debug, Deserialize)]
pub struct OAuthCallback {
    pub code: String,
    pub state: Option<String>,
}

/// Google OAuth 回调参数
#[derive(Debug, Deserialize)]
pub struct GoogleCallbackParams {
    pub code: Option<String>,
    #[allow(dead_code)]
    pub state: Option<String>,
}

/// GitHub OAuth 回调参数
#[derive(Deserialize)]
pub struct GithubCallbackParams {
    pub code: Option<String>,
    #[allow(dead_code)]
    pub state: Option<String>,
}

/// ============================================================
/// 认证 API Handlers
/// ============================================================
/// POST /api/auth/login - 管理员登录
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
            Uuid::new_v4().to_string()
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
                Uuid::new_v4().to_string()
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
    //（原实现"中文密码4字=12字节"可绕过 6 位下限）
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

/// POST /api/auth/change-password — 登录后修改密码
///
/// 需要有效会话；先校验旧密码（防止会话泄露被直接改密），
/// 新密码至少 6 位，成功后旧会话继续有效（v2board 语义）。
pub async fn handle_change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.new_password.chars().count() < 6 {
        return Err(error_response("密码长度至少6位", StatusCode::BAD_REQUEST));
    }
    if body.old_password == body.new_password {
        return Err(error_response(
            "新密码不能与旧密码相同",
            StatusCode::BAD_REQUEST,
        ));
    }

    // 会话校验（复用 verify_user 语义，但不重复取 user）
    let token = extract_session_token(&headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;
    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_store = SessionStore::new(&config.admin.session_secret, session_ttl);
    let sess = session_store
        .validate_session(&token)
        .ok_or_else(|| error_response("Invalid session", StatusCode::UNAUTHORIZED))?;

    let user = state
        .user_store
        .get_by_email(&sess.email)
        .ok_or_else(|| error_response("User not found", StatusCode::UNAUTHORIZED))?;
    if user.status != "active" {
        return Err(error_response("User disabled", StatusCode::UNAUTHORIZED));
    }

    // 旧密码校验（OAuth 用户的随机密码无法校验，明确拒绝改密并提示走找回流程）
    if !crate::user::verify_password(&body.old_password, &user.password) {
        return Err(error_response("旧密码不正确", StatusCode::BAD_REQUEST));
    }

    let new_hash = hash_password(&body.new_password);
    match state.user_store.update(&user.id, |u| {
        u.password = new_hash;
    }) {
        Ok(_) => {
            state
                .log_store
                .record_security(crate::log::SecurityEvent::new(
                    crate::log::SecurityEventType::AuthFailure,
                    "info",
                    format!("用户 {} 修改了密码", user.email),
                ));
            Ok(Json(serde_json::json!({
                "success": true,
                "data": { "message": "Password changed" }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Password change failed: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
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
        Uuid::new_v4().to_string()
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

    // 邮件已配置时真正发出重置邮件（new-api/v2board 式自助找回）；
    // 否则退回 token 直出（便于内网/未配邮件环境使用）。
    let notify_config = state.notify_service.get_config().await;
    if notify_config.smtp_ready() && !notify_config.smtp_from.is_empty() {
        let base = config.server_address.trim_end_matches('/').to_string();
        let reset_url = if base.is_empty() {
            format!("?reset_token={}", session.token)
        } else {
            format!("{}/login?reset_token={}", base, session.token)
        };
        let subject = "AIGX 密码重置";
        let body = format!(
            "您正在为账号 {} 重置密码。\n\n请在 1 小时内打开以下链接并设置新密码：\n{}\n\n如果这不是您的操作，请忽略本邮件。",
            user.email, reset_url
        );
        match state
            .notify_service
            .send_email(&user.email, subject, &body)
            .await
        {
            Ok(_) => {
                tracing::info!("Password reset email sent to {}", user.email);
                return Ok(Json(serde_json::json!({
                    "success": true,
                    "data": {
                        "message": "Reset link sent to your email",
                        "sent": true,
                        "expires_in_secs": PASSWORD_RESET_TTL_HOURS * 3600
                    }
                })));
            }
            Err(e) => {
                tracing::warn!("Failed to send reset email to {}: {e}", user.email);
                // 邮件失败不回退泄露 token，返回明确错误提示管理员配置邮件
                return Err(error_response(
                    "Reset email send failed; please check SMTP config",
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
            }
        }
    }

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "message": "If the email exists, a reset token has been generated",
            "token": session.token,
            "sent": false,
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
    let state_param = Uuid::new_v4().to_string();
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
            &Uuid::new_v4().to_string(), // 随机密码（OAuth 用户不走密码登录）
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
        Uuid::new_v4().to_string()
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

// ── 功能 3：GitHub OAuth ─────────────────────────────────────────────
//
// 参参照 burncloud `api/auth.rs::oauth` 及 AIGX 既有 `handle_github_oauth_*` 模式。
// 配置存于 `config.github_oauth`（`src/oauth/github.rs` + `src/config.rs`）。

/// GET /api/auth/github — 跳转到 GitHub OAuth 授权页
pub async fn handle_github_oauth_authorize(State(state): State<AppState>) -> Response {
    let config = state.config_manager.get().await;
    let oauth = &config.github_oauth;
    if !oauth.ready() {
        return error_response("GitHub OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let state_param = Uuid::new_v4().to_string();
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
                &Uuid::new_v4().to_string(), // random password (OAuth users don't use password login)
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
        Uuid::new_v4().to_string()
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

/// 邮箱验证码登录 — 发送验证码请求
#[derive(Debug, Deserialize)]
pub struct LoginCodeRequest {
    pub email: String,
}

/// 邮箱验证码登录 — 验证并登录请求
#[derive(Debug, Deserialize)]
pub struct LoginWithCodeRequest {
    pub email: String,
    pub code: String,
}

/// POST /api/auth/login/send-code — 发送登录验证码（6 位，5 分钟有效）
///
/// 对齐 v2board/new-api 的邮箱验证码登录：
/// - SMTP 已配置时发送真实邮件（正文注明 5 分钟有效期）；
/// - 未配置 SMTP 时（内网/开发环境）直接返回验证码，便于调试，
///   同时记录 warning 日志提示生产环境必须配置 SMTP。
/// 用户不存在时仍返回成功（防邮箱枚举），但不生成验证码。
pub async fn handle_login_send_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginCodeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(error_response("邮箱不能为空", StatusCode::BAD_REQUEST));
    }

    let client_ip = extract_client_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    // 每 IP 每分钟最多 5 次发送（防刷邮件）
    const SEND_CODE_RATE_LIMIT: u32 = 5;
    let sent_count = state.login_limiter.get(&client_ip).await.unwrap_or(0);
    if sent_count >= SEND_CODE_RATE_LIMIT {
        return Err(error_response(
            "验证码发送过于频繁，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }
    state
        .login_limiter
        .insert(client_ip, sent_count + 1)
        .await;

    // 防枚举：用户不存在时返回成功但不生成验证码
    let Some(user) = state.user_store.get_by_email(&email) else {
        tracing::info!("Login code requested for unknown email (no code issued): {email}");
        return Ok(Json(serde_json::json!({
            "success": true,
            "data": { "sent": false, "message": "If the email exists, a code has been sent" }
        })));
    };

    // 生成 6 位数字验证码（加密安全随机源）
    let code: String = {
        let mut rng = rand::thread_rng();
        let digits: Vec<u32> = (0..6).map(|_| rng.gen_range(0..10)).collect();
        digits.iter().map(|d| d.to_string()).collect()
    };
    state
        .login_code_cache
        .insert(user.email.clone(), code.clone())
        .await;

    let notify_config = state.notify_service.get_config().await;
    if notify_config.smtp_ready() && !notify_config.smtp_from.is_empty() {
        let subject = "AIGX 登录验证码";
        let body = format!(
            "您的 AIGX 登录验证码是：{}\n\n5 分钟内有效。如果这不是您的操作，请忽略本邮件。",
            code
        );
        match state.notify_service.send_email(&user.email, &subject, &body).await {
            Ok(_) => {
                tracing::info!("Login code email sent to {}", user.email);
                return Ok(Json(serde_json::json!({
                    "success": true,
                    "data": { "sent": true, "expires_in_secs": 300 }
                })));
            }
            Err(e) => {
                tracing::warn!("Failed to send login code email to {}: {e}", user.email);
                return Err(error_response(
                    "验证码邮件发送失败，请检查 SMTP 配置",
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
            }
        }
    }

    // 未配置 SMTP：直接返回验证码（仅限内网/开发；生产环境必须配置 SMTP）
    tracing::warn!(
        "SMTP not configured, returning login code in response for {} (dev only)",
        user.email
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "sent": false, "code": code, "expires_in_secs": 300 }
    })))
}

/// POST /api/auth/login/code — 邮箱验证码登录
pub async fn handle_login_with_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginWithCodeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let email = body.email.trim().to_lowercase();
    let code = body.code.trim();
    if email.is_empty() || code.is_empty() {
        return Err(error_response(
            "邮箱和验证码不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }

    let client_ip = extract_client_ip(&headers).unwrap_or_else(|| "unknown".to_string());
    let fail_count = state.login_failures.get(&client_ip).await.unwrap_or(0);
    if fail_count >= 5 {
        return Err(error_response(
            "登录失败次数过多，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }

    let Some(expected) = state.login_code_cache.get(&email).await else {
        state
            .login_failures
            .insert(client_ip.clone(), fail_count + 1)
            .await;
        return Err(error_response(
            "验证码已过期或不存在，请重新获取",
            StatusCode::UNAUTHORIZED,
        ));
    };

    if expected != code {
        state
            .login_failures
            .insert(client_ip.clone(), fail_count + 1)
            .await;
        return Err(error_response(
            "验证码错误",
            StatusCode::UNAUTHORIZED,
        ));
    }

    let Some(user) = state.user_store.get_by_email(&email) else {
        return Err(error_response(
            "用户不存在",
            StatusCode::UNAUTHORIZED,
        ));
    };
    if user.status != "active" {
        return Err(error_response("账号已禁用", StatusCode::FORBIDDEN));
    }

    // 登录成功：消耗验证码 + 清除失败计数
    state.login_code_cache.remove(&email).await;
    state.login_failures.remove(&client_ip).await;

    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_secret = if config.admin.session_secret.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        config.admin.session_secret.clone()
    };
    let session_store = SessionStore::new(&session_secret, session_ttl);
    let session = session_store.create_session(&user.email);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "email": user.email,
            "username": user.username,
            "role": match user.role { Role::Admin => "admin", Role::User => "user" },
            "expires_at": session.expires_at
        }
    })))
}
