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
use rand::RngCore;
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
    pub state: Option<String>,
}

/// GitHub OAuth 回调参数
#[derive(Deserialize)]
pub struct GithubCallbackParams {
    pub code: Option<String>,
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
        // 2FA/TOTP（P1）：用户已开启 TOTP 时不下发 session，
        // 返回 require_2fa + 一次性 tmp_token，客户端二次提交验证码。
        // totp_enabled=false 的用户走原路径，行为不变（向后兼容）。
        if u.totp_enabled && !u.totp_secret.is_empty() {
            let tmp_token = Uuid::new_v4().to_string();
            state
                .totp_pending_cache
                .insert(tmp_token.clone(), u.email.clone())
                .await;
            state.log_store.record_security(
                crate::log::SecurityEvent::new(
                    crate::log::SecurityEventType::AuthFailure,
                    "info",
                    format!("登录密码通过，等待 TOTP 二次验证（邮箱: {}）", u.email),
                )
                .with_ip(Some(client_ip)),
            );
            return Ok(Json(serde_json::json!({
                "success": true,
                "data": {
                    "require_2fa": true,
                    "tmp_token": tmp_token
                }
            })));
        }
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
        state
            .session_registry
            .register(&u.email, &session.session_id);
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
            state
                .session_registry
                .register(&u.email, &session.session_id);
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
    // 注册赠送配额与月度限额分离（原实现把 monthly_limit 当赠送额度，
    // 与充值配额口径混淆）
    let default_quota = config.usage.register_quota;
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
            // 改密成功 → 撤销该用户全部会话（旧密码签发的 token 全部失效，
            // 用户需用新密码重新登录；new-api 的全端踢出语义）
            let revoked = state.session_registry.revoke_all(&user.email);
            tracing::info!(
                "Password changed for {}: {} session(s) revoked",
                user.email,
                revoked
            );
            state
                .log_store
                .record_security(crate::log::SecurityEvent::new(
                    crate::log::SecurityEventType::AuthFailure,
                    "info",
                    format!("用户 {} 修改了密码（撤销 {} 个会话）", user.email, revoked),
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

/// POST /api/auth/logout - 登出（撤销当前会话）
///
/// P1 会话撤销：登出时把当前 jti 加入撤销表，旧 token 立即失效
/// （此前只是前端删 localStorage，token 本身到过期前一直有效）。
pub async fn handle_logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let token = extract_session_token(&headers)
        .ok_or_else(|| error_response("Not authenticated", StatusCode::UNAUTHORIZED))?;
    let config = state.config_manager.get().await;
    let session_store = SessionStore::new(
        &config.admin.session_secret,
        config.admin.session_ttl_hours.max(1),
    );
    if let Some(sess) = session_store.validate_session(&token) {
        state.session_registry.revoke(&sess.email, &sess.session_id);
        state
            .log_store
            .record_security(crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::AuthFailure,
                "info",
                format!("用户 {} 登出（会话已撤销）", sess.email),
            ));
    }

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
            // 重置密码成功 → 撤销该用户全部会话（改密的踢出语义）
            let revoked = state.session_registry.revoke_all(&user.email);
            tracing::info!(
                "Password reset successful for {}: {} session(s) revoked",
                user.email,
                revoked
            );
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
    // CSRF 防护：state 存入缓存，callback 校验并一次性消费
    state
        .oauth_state_cache
        .insert(state_param.clone(), chrono::Utc::now().timestamp())
        .await;
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
    // CSRF 防护：校验 state（一次性消费，防重放/伪造回调）
    let state_param = match params.state {
        Some(ref s) if !s.is_empty() => s.clone(),
        _ => return error_response("Missing OAuth state", StatusCode::BAD_REQUEST).into_response(),
    };
    if state.oauth_state_cache.get(&state_param).await.is_none() {
        tracing::warn!("Google OAuth callback: invalid or expired state (CSRF check failed)");
        return error_response("Invalid OAuth state", StatusCode::BAD_REQUEST).into_response();
    }
    state.oauth_state_cache.remove(&state_param).await;
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
            config.usage.register_quota,
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
    state
        .session_registry
        .register(&user.email, &session.session_id);
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
    // CSRF 防护：state 存入缓存，callback 校验并一次性消费
    state
        .oauth_state_cache
        .insert(state_param.clone(), chrono::Utc::now().timestamp())
        .await;
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
    // CSRF 防护：校验 state（一次性消费，防重放/伪造回调）
    let state_param = match params.state {
        Some(ref s) if !s.is_empty() => s.clone(),
        _ => return error_response("Missing OAuth state", StatusCode::BAD_REQUEST).into_response(),
    };
    if state.oauth_state_cache.get(&state_param).await.is_none() {
        tracing::warn!("GitHub OAuth callback: invalid or expired state (CSRF check failed)");
        return error_response("Invalid OAuth state", StatusCode::BAD_REQUEST).into_response();
    }
    state.oauth_state_cache.remove(&state_param).await;
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
                config.usage.register_quota,
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
    state
        .session_registry
        .register(&user.email, &session.session_id);
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

/// TOTP 二次验证登录 — 请求（P1：2FA）
#[derive(Debug, Deserialize)]
pub struct LoginTotpRequest {
    /// 密码/验证码阶段下发的一次性令牌
    pub tmp_token: String,
    /// 6 位 TOTP 验证码
    pub code: String,
}

// ── TOTP 管理端（P1：2FA 收尾）─────────────────────────────────────────
//
// 用户侧自助启用/停用 TOTP 的三接口：
//   1. POST /api/auth/totp/setup   — 生成新 secret（不落库），返回 otpauth URI
//   2. POST /api/auth/totp/enable  — 提交一次有效验证码，把 secret 落库启用
//   3. POST /api/auth/totp/disable — 校验当前密码后关闭 TOTP 并清空 secret
//
// 安全语义：
// - setup 生成的 secret 暂存于 totp_setup_cache（key=user.id，TTL 5 分钟），
//   enable 时一次性消费；未在窗口内启用则作废，防止半途丢弃的 secret 残留
// - 已启用用户重复 setup 会覆盖暂存（旧已启用 secret 不受影响，直到 enable 成功）
// - disable 需当前密码（防止会话被劫持后直接关掉 2FA）

/// POST /api/auth/totp/setup — 生成 TOTP secret（不落库）
///
/// 随机 20 字节 secret（base32 编码后 32 字符），返回 otpauth URI 供
/// 认证器扫码/手动录入。真正的落库发生在 enable（用户验证一次后）。
pub async fn handle_totp_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = super::common::verify_user(&state, &headers).await?;

    // 生成 20 字节随机 secret（加密安全随机源；base32 后 32 字符，
    // 与 Google/Microsoft Authenticator 的推荐长度一致）
    let mut raw = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut raw);
    let secret_b32 = crate::auth::totp::base32_encode(&raw);

    // 暂存（key=user.id）：enable 时一次性消费
    state
        .totp_setup_cache
        .insert(user.id.clone(), secret_b32.clone())
        .await;

    // otpauth URI（RFC 6238 惯例格式，认证器扫码录入）。
    // email 仅需 percent-encode 到 query/path 安全字符，用 url crate
    // （既有依赖）的 form 字节转义——email 字符集（字母数字+@._-）转义后
    // 与主流认证器的解析行为兼容。
    let issuer = "AIGX";
    let email_escaped: String = user
        .email
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect();
    let otpauth = format!(
        "otpauth://totp/{issuer}:{email_escaped}?secret={secret_b32}&issuer={issuer}&algorithm=SHA1&digits=6&period=30"
    );

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "secret": secret_b32,
            "otpauth_uri": otpauth,
            // 前端可据此渲染二维码（本地生成，不经第三方服务）
            "qr_content": otpauth
        }
    })))
}

/// POST /api/auth/totp/enable — 启用 TOTP（验证一次后落库）
#[derive(Debug, Deserialize)]
pub struct TotpEnableRequest {
    /// 6 位 TOTP 验证码（来自认证器）
    pub code: String,
}

pub async fn handle_totp_enable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TotpEnableRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = super::common::verify_user(&state, &headers).await?;
    let code = body.code.trim();
    if code.is_empty() {
        return Err(error_response("验证码不能为空", StatusCode::BAD_REQUEST));
    }

    // 取出暂存的 secret（一次性消费：取出即移除，防止重放）
    let Some(secret_b32) = state.totp_setup_cache.get(&user.id).await else {
        return Err(error_response(
            "请先调用 setup 生成密钥（或密钥已过期，请重新生成）",
            StatusCode::BAD_REQUEST,
        ));
    };
    state.totp_setup_cache.remove(&user.id).await;

    // 验证一次：通过才落库启用（确保用户扫码成功、时钟大致同步）
    let valid = crate::auth::totp::base32_decode(&secret_b32)
        .map(|raw| crate::auth::totp::verify(&raw, code, 1))
        .unwrap_or(false);
    if !valid {
        return Err(error_response(
            "验证码错误，请确认认证器时间同步后重试（需重新 setup）",
            StatusCode::UNAUTHORIZED,
        ));
    }

    // 生成 10 个一次性恢复码（G3）：明文只在此次响应中展示一次，
    // 落库仅存 SHA-256 哈希。换设备/丢失认证器时可用恢复码登录。
    let recovery_codes: Vec<String> = (0..10)
        .map(|_| crate::auth::totp::generate_recovery_code())
        .collect();
    let recovery_hashes: Vec<String> = recovery_codes
        .iter()
        .map(|code| {
            crate::auth::totp::sha256_hex(
                crate::auth::totp::normalize_recovery_code(code).as_bytes(),
            )
        })
        .collect();

    // 落库：secret + enabled + 恢复码哈希（原子更新，含 FileStore 持久化）
    state
        .user_store
        .update(&user.id, |u| {
            u.totp_secret = secret_b32;
            u.totp_enabled = true;
            u.totp_recovery_codes = recovery_hashes.clone();
        })
        .map_err(|e| {
            error_response(&format!("启用失败: {e}"), StatusCode::INTERNAL_SERVER_ERROR)
        })?;

    state
        .log_store
        .record_security(crate::log::SecurityEvent::new(
            crate::log::SecurityEventType::AuthFailure,
            "info",
            format!("用户 {} 启用了 TOTP 两步验证（含恢复码）", user.email),
        ));

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "enabled": true,
            // 明文恢复码仅此一次返回；前端展示后即丢弃，
            // 服务端只保留哈希，无法再查询明文。
            "recovery_codes": recovery_codes,
            "recovery_codes_remaining": recovery_codes.len()
        }
    })))
}

/// POST /api/auth/totp/disable — 停用 TOTP（需当前密码）
#[derive(Debug, Deserialize)]
pub struct TotpDisableRequest {
    pub password: String,
}

pub async fn handle_totp_disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TotpDisableRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = super::common::verify_user(&state, &headers).await?;

    // 已启用才能停用
    if !user.totp_enabled {
        return Err(error_response("未启用 TOTP", StatusCode::BAD_REQUEST));
    }

    // 密码确认（防止会话被劫持后直接关掉 2FA）
    if !crate::user::verify_password(body.password.trim(), &user.password) {
        state
            .log_store
            .record_security(crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::AuthFailure,
                "warning",
                format!("用户 {} 尝试停用 TOTP 但密码校验失败", user.email),
            ));
        return Err(error_response("密码不正确", StatusCode::UNAUTHORIZED));
    }

    state
        .user_store
        .update(&user.id, |u| {
            u.totp_secret = String::new();
            u.totp_enabled = false;
            u.totp_recovery_codes = Vec::new();
        })
        .map_err(|e| {
            error_response(&format!("停用失败: {e}"), StatusCode::INTERNAL_SERVER_ERROR)
        })?;

    state
        .log_store
        .record_security(crate::log::SecurityEvent::new(
            crate::log::SecurityEventType::AuthFailure,
            "info",
            format!("用户 {} 停用了 TOTP 两步验证", user.email),
        ));

    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "enabled": false }
    })))
}

/// POST /api/auth/login/totp — TOTP 二次验证登录（P1：2FA）
///
/// 流程：密码（或邮箱验证码）验证通过且用户开启 TOTP 时，登录接口
/// 返回 require_2fa + tmp_token；客户端携 TOTP 码调本接口完成登录。
/// - tmp_token 5 分钟有效（totp_pending_cache），一次性（验证后即移除）
/// - 验证失败计入 login_failures（复用 5 次锁定机制，防暴力破解 6 位码）
pub async fn handle_login_totp(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginTotpRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client_ip = extract_client_ip(&headers).unwrap_or_else(|| "unknown".to_string());

    // 5 次锁定检查（与密码登录共用计数器）
    let fail_count = state.login_failures.get(&client_ip).await.unwrap_or(0);
    if fail_count >= 5 {
        return Err(error_response(
            "登录失败次数过多，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }

    let tmp_token = body.tmp_token.trim().to_string();
    if tmp_token.is_empty() || body.code.trim().is_empty() {
        return Err(error_response("参数不能为空", StatusCode::BAD_REQUEST));
    }

    // 取出挂起的 email（一次性：取出即移除，防 tmp_token 重放）
    let Some(email) = state.totp_pending_cache.get(&tmp_token).await else {
        return Err(error_response(
            "二次验证会话已过期，请重新登录",
            StatusCode::UNAUTHORIZED,
        ));
    };
    let Some(user) = state.user_store.get_by_email(&email) else {
        state.totp_pending_cache.remove(&tmp_token).await;
        return Err(error_response("用户不存在", StatusCode::UNAUTHORIZED));
    };
    if user.status != "active" {
        state.totp_pending_cache.remove(&tmp_token).await;
        return Err(error_response("账号已禁用", StatusCode::FORBIDDEN));
    }

    // 验证 TOTP（window=1 容忍 ±30s 时钟偏移）；TOTP 失败时
    // 回退验证一次性恢复码（G3：换设备/丢失认证器场景）。
    let code = body.code.trim();
    let secret_ok = crate::auth::totp::base32_decode(&user.totp_secret)
        .map(|raw| crate::auth::totp::verify(&raw, code, 1));
    let recovery_code = crate::auth::totp::normalize_recovery_code(code);
    let recovery_hash = (!recovery_code.is_empty())
        .then(|| crate::auth::totp::sha256_hex(recovery_code.as_bytes()));
    let recovery_matched = recovery_hash
        .as_deref()
        .is_some_and(|h| user.totp_recovery_codes.iter().any(|stored| stored == h));
    let auth_ok = matches!(secret_ok, Some(true)) || recovery_matched;
    if !auth_ok {
        // 失败计入锁定计数（不移除 tmp_token：窗口内允许重试，
        // 但每次失败都逼近 5 次锁定阈值）
        state
            .login_failures
            .insert(client_ip.clone(), fail_count + 1)
            .await;
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::AuthFailure,
                "warning",
                format!("TOTP 二次验证失败（邮箱: {email}）"),
            )
            .with_ip(Some(client_ip.clone())),
        );
        return Err(error_response("验证码错误", StatusCode::UNAUTHORIZED));
    }

    // 恢复码命中：立即消费该码（用完即焚），避免重放。
    let remaining_codes = if recovery_matched {
        let hash = recovery_hash.unwrap_or_default();
        let mut remaining = 0usize;
        let _ = state.user_store.update(&user.id, |u| {
            u.totp_recovery_codes.retain(|stored| stored != &hash);
            remaining = u.totp_recovery_codes.len();
        });
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::AuthFailure,
                "info",
                format!(
                    "用户 {} 使用一次性恢复码完成二次验证（剩余 {remaining} 个）",
                    user.email
                ),
            )
            .with_ip(Some(client_ip.clone())),
        );
        Some(remaining)
    } else {
        None
    };

    // 全部通过：消耗 tmp_token + 签发 session
    state.totp_pending_cache.remove(&tmp_token).await;
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
    state
        .session_registry
        .register(&user.email, &session.session_id);

    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "token": session.token,
            "email": user.email,
            "username": user.username,
            "role": match user.role { Role::Admin => "admin", Role::User => "user" },
            "recovery_codes_remaining": remaining_codes,
            "expires_at": session.expires_at
        }
    })))
}

/// POST /api/auth/login/send-code — 发送登录验证码（6 位，5 分钟有效）
///
/// 对齐 v2board/new-api 的邮箱验证码登录：
/// - SMTP 已配置时发送真实邮件（正文注明 5 分钟有效期）；
/// - 未配置 SMTP 时（内网/开发环境）直接返回验证码，便于调试，
///   同时记录 warning 日志提示生产环境必须配置 SMTP。
///   用户不存在时仍返回成功（防邮箱枚举），但不生成验证码。
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
    state.login_limiter.insert(client_ip, sent_count + 1).await;

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
        match state
            .notify_service
            .send_email(&user.email, subject, &body)
            .await
        {
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
        return Err(error_response("验证码错误", StatusCode::UNAUTHORIZED));
    }

    let Some(user) = state.user_store.get_by_email(&email) else {
        return Err(error_response("用户不存在", StatusCode::UNAUTHORIZED));
    };
    if user.status != "active" {
        return Err(error_response("账号已禁用", StatusCode::FORBIDDEN));
    }

    // 2FA/TOTP（P1）：验证码登录同样要过 TOTP 关（若已启用）
    if user.totp_enabled && !user.totp_secret.is_empty() {
        let tmp_token = Uuid::new_v4().to_string();
        state
            .totp_pending_cache
            .insert(tmp_token.clone(), user.email.clone())
            .await;
        return Ok(Json(serde_json::json!({
            "success": true,
            "data": {
                "require_2fa": true,
                "tmp_token": tmp_token
            }
        })));
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
    state
        .session_registry
        .register(&user.email, &session.session_id);

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
