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
use super::common::verify_user;

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
    /// ALTCHA 机器人校验 payload（base64 的 JSON；开关关闭时可缺省）。
    #[serde(default)]
    pub captcha: Option<String>,
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
    /// 邀请人邀请码（对齐 new-api aff_code；可选，无效码静默忽略）
    #[serde(default)]
    pub aff_code: String,
    /// ALTCHA 机器人校验 payload（base64 的 JSON；开关关闭时可缺省）。
    #[serde(default)]
    pub captcha: Option<String>,
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

/// LinuxDO OAuth 回调参数
#[derive(Deserialize)]
pub struct LinuxDoCallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
}

/// ============================================================
/// 认证 API Handlers
/// ============================================================
/// GET /api/auth/captcha - 领取机器人校验题（ALTCHA challenge）
///
/// 公开端点（登录页在提交前先领题）。开关关闭时回 400，前端据此隐藏校验组件。
/// 响应体是**裸 challenge 对象**（不带 success/data 信封）——与 Agent2API 同形，
/// 便于前端直接交给解题器。
pub async fn handle_captcha(
    State(state): State<AppState>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match crate::altcha::challenge(&state) {
        Some(challenge) => Ok(Json(challenge)),
        None => Err(error_response(
            "机器人校验当前未启用",
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// POST /api/auth/login - 管理员登录
pub async fn handle_login(
    State(state): State<AppState>,
    connect: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // ── 安全策略门槛（设置 → 安全，保存即生效；默认 10 次/分 + 连续 5 次锁 5 分钟）──
    let rate_limit = crate::config::security_rate_limit_per_minute();
    let fail_threshold = crate::config::security_fail_threshold();
    let lockout_enabled = crate::config::security_lockout_enabled();

    // ── 机器人校验（ALTCHA proof-of-work）：放在限流/锁定之前，让「无答案的
    //    脚本请求」最先被挡掉，连失败计数都不必消耗 ──
    if crate::config::security_captcha_enabled() {
        let token = body.captcha.as_deref().unwrap_or("");
        if token.trim().is_empty() {
            return Err(error_response(
                "请先完成机器人校验",
                StatusCode::BAD_REQUEST,
            ));
        }
        if let Err(message) = crate::altcha::verify(&state, token) {
            return Err(error_response(&message, StatusCode::BAD_REQUEST));
        }
    }

    // 真实来源 IP：优先信任的代理头（trust_proxy_headers 开启时），
    // 否则用 TCP 对端地址。原来两者都拿不到时全站共用 "unknown" 键 ——
    // 任意人 5 次失败就能把**所有用户**锁 5 分钟（登录 DoS）。
    let client_ip = extract_client_ip(&headers)
        .or_else(|| connect.map(|c| c.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    // 限流键带用途前缀：登录、发验证码、忘记密码共用同一个 60s 窗口缓存，
    // 裸 IP 做键会导致「发 5 次登录码」把「登录 10 次的额度」提前耗尽
    // （反之亦然），表现为莫名 429。
    let login_rate_key = format!("login:{client_ip}");
    let attempts = state.login_limiter.get(&login_rate_key).await.unwrap_or(0);
    if attempts >= rate_limit {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::RateLimit,
                "warning",
                format!("登录限流触发：IP {client_ip} 每分钟超过 {rate_limit} 次"),
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
        .insert(login_rate_key, attempts + 1)
        .await;

    // ── 失败锁定：连续失败达阈值 → 锁定（TTL = 配置的 lock_minutes，默认 5 分钟）──
    let fail_count = state.login_failures.get(&client_ip).await.unwrap_or(0);
    if lockout_enabled && fail_count >= fail_threshold {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::IpBlocked,
                "critical",
                format!("IP {client_ip} 连续失败 {fail_count} 次，已锁定"),
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
        // password_set 自愈：旧数据缺该字段按 false，一旦用密码成功登录过就
        // 翻成 true —— 关闭「他人凭相同邮箱注册密码账号、再用 OAuth 登进该
        // 账号」的接管面（OAuth 邮箱兜底只接纳尚未设置密码的存量 OAuth 账号）。
        if !u.password_set {
            let _ = state
                .user_store
                .update(&u.id, |user| user.password_set = true);
        }
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
                "expires_at": session.expires_at,
                // 首次启动的内置管理员用弱口令初始化：前端据此强制跳改密页
                "must_change_password": u.must_change_password
            }
        })));
    }

    // 用户名也是合法登录标识（与 user_store.authenticate 的 email 校验互补）
    if let Some(u) = state.user_store.get_by_username(&body.email) {
        // 与 email 分支同一口径：禁用/封禁账号不得登录。
        // 原来这里只校验密码，被禁用的用户仍能签发会话。
        if u.status == "active" && user::verify_password(&body.password, &u.password) {
            // 登录成功：清除失败计数
            state.login_failures.remove(&client_ip).await;
            // password_set 自愈（同 email 分支）
            if !u.password_set {
                let _ = state
                    .user_store
                    .update(&u.id, |user| user.password_set = true);
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
                    "expires_at": session.expires_at,
                    "must_change_password": u.must_change_password
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
    // 安全事件：认证失败（每次失败都记录，达锁定阈值时升级 critical）
    let severity = if next_fail >= fail_threshold {
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
    if lockout_enabled && next_fail >= fail_threshold {
        tracing::warn!("Login failed {next_fail} times from {client_ip}; lockout threshold reached");
    }

    Err(error_response("账号或密码错误", StatusCode::UNAUTHORIZED))
}

/// POST /api/auth/register - 公开邮箱注册
///
/// 安全：对同一 IP 每 60 秒最多允许 5 次注册请求，超出返回 429 Too Many Requests。
/// 限流基于 moka TTL 缓存（key=IP, value=计数），窗口 60s。
pub async fn handle_register(
    State(state): State<AppState>,
    connect: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<RegisterRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // ── 机器人校验（ALTCHA proof-of-work）：注册是「抢注管理员」的公开入口，
    //    与登录同口径 —— 先验题，再进限流计数 ──
    if crate::config::security_captcha_enabled() {
        let token = body.captcha.as_deref().unwrap_or("");
        if token.trim().is_empty() {
            return Err(error_response(
                "请先完成机器人校验",
                StatusCode::BAD_REQUEST,
            ));
        }
        if let Err(message) = crate::altcha::verify(&state, token) {
            return Err(error_response(&message, StatusCode::BAD_REQUEST));
        }
    }

    // ── 速率限制：同 IP 每分钟最多 5 次 ──
    const REGISTER_RATE_LIMIT_PER_MINUTE: u32 = 5;
    let client_ip = extract_client_ip(&headers)
        .or_else(|| connect.map(|c| c.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
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
    // 注册开关（对齐 new-api RegisterEnabled）：关闭后拒绝自助注册
    if !config.usage.register_enabled {
        return Err(error_response("注册已关闭", StatusCode::FORBIDDEN));
    }
    // 邮箱白名单（对齐 v2board email_whitelist_enable + emailSuffixVerify）：
    // 开启后仅允许指定后缀的邮箱注册，按 @ 分割取域名后缀精确匹配。
    if config.usage.email_whitelist_enable {
        let suffix = body
            .email
            .trim()
            .rsplit_once('@')
            .map(|(_, domain)| domain.trim().to_string())
            .unwrap_or_default();
        let allowed = config
            .usage
            .email_whitelist_suffix
            .iter()
            .any(|s| s.trim() == suffix);
        if !allowed {
            return Err(error_response(
                "邮箱后缀不在注册白名单内",
                StatusCode::FORBIDDEN,
            ));
        }
    }
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

    // 邀请返利（对齐 new-api finishInsert）：注册携带有效邀请码时，
    // 写入 inviter_id、邀请人 aff_count+1/奖励进 aff_quota、新用户奖励
    // 直接进可用配额。无效/缺失邀请码静默忽略（不阻断注册）。
    let inviter = state.user_store.get_by_aff_code(&body.aff_code);
    if let Some(inv) = inviter {
        if inv.id != user.id {
            let _ = state.user_store.update(&user.id, |u| {
                u.inviter_id = inv.id.clone();
            });
            let (for_inviter, for_invitee) = state
                .user_store
                .record_aff(
                    &inv.id,
                    &user.id,
                    config.usage.quota_for_inviter,
                    config.usage.quota_for_invitee,
                )
                .unwrap_or((0, 0));
            if for_inviter > 0 || for_invitee > 0 {
                tracing::info!(
                    "Affiliate: user {} invited by {} (inviter +{}, invitee +{})",
                    user.id,
                    inv.id,
                    for_inviter,
                    for_invitee
                );
            }
        }
    }

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
/// 新密码至少 6 位；已撤销会话直接 401。成功后其他旧会话被 revoke_all
/// 踢出（与 v2board/new-api 的改密踢出语义一致，含当前会话）。
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
    // 撤销检查（P1 会话撤销收尾）：被登出/踢出的旧 token 即使签名有效，
    // 也不得继续执行改密等敏感操作（与 verify_user 同一拦截语义）。
    if state.session_registry.is_revoked(&sess.session_id) {
        return Err(error_response("Session revoked", StatusCode::UNAUTHORIZED));
    }

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
        // 改过密码 = 弱口令风险已解除
        u.must_change_password = false;
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
    connect: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<ForgotPasswordRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if body.email.trim().is_empty() {
        return Err(error_response("邮箱不能为空", StatusCode::BAD_REQUEST));
    }

    // 限流：同一 (IP, 邮箱) 组合 60 秒内最多 3 次。忘记密码会触发邮件发送，
    // 无限制时可被用来对已知邮箱反复轰炸。键里带邮箱：同一 IP 重置不同邮箱
    // 不算滥用，但同一邮箱被同一来源反复请求就是轰炸。
    const FORGOT_RATE_LIMIT: u32 = 3;
    let client_ip = extract_client_ip(&headers)
        .or_else(|| connect.map(|c| c.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let rate_key = format!("forgot:{}:{}", client_ip, body.email.trim().to_lowercase());
    let attempts = state.login_limiter.get(&rate_key).await.unwrap_or(0);
    if attempts >= FORGOT_RATE_LIMIT {
        state.log_store.record_security(
            crate::log::SecurityEvent::new(
                crate::log::SecurityEventType::RateLimit,
                "warning",
                format!("忘记密码限流触发：IP {client_ip} 对同一邮箱请求过于频繁"),
            )
            .with_ip(Some(client_ip)),
        );
        return Err(error_response(
            "请求过于频繁，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }
    state.login_limiter.insert(rate_key, attempts + 1).await;

    let config = state.config_manager.get().await;
    // 重置令牌用**独立的派生密钥**（session_secret + 用途后缀），与登录会话
    // 隔离：原来两者共用同一个 secret，一个有效的登录 token 就能当重置令牌用
    // （反之亦然），违反「令牌用途最小权限」。
    let session_secret = if config.admin.session_secret.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        format!("{}:password-reset", config.admin.session_secret)
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
    // 未配置时拒绝而非直出 token——原先在响应体里返回重置 token，
    // 等于对任意已知邮箱开放账号接管（见安全审计 P0-2）。
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

    // 未配置 SMTP：不返回 token（防任意邮箱账号接管）。提示走管理员线下重置。
    tracing::warn!(
        "Forgot password requested for {} but SMTP not configured; no token issued",
        user.email
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "message": "If the email exists, a reset token has been generated",
            "token": null,
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
        // 与 handle_forgot_password 同一派生口径：重置令牌与登录会话隔离
        format!("{}:password-reset", config.admin.session_secret)
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
        // 改过密码 = 弱口令风险已解除
        u.must_change_password = false;
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

// ── OAuth 统一回调 / 绑定 / 票据基础设施 ──────────────────────────────
//
// 三个 provider（google / github / linuxdo）共用一套流程，差异只在
// profile 字段提取。旧实现有三处问题，一并修正：
// 1. callback 给浏览器返回**原始 JSON**（token 在里面），但发起方是
//    `window.location.href` 整页跳转 → 用户只会看到一片 JSON，token 永远
//    进不了 localStorage，OAuth 登录实际上登不进来。改为 302 回 SPA。
// 2. 归属纯靠 email 隐式匹配 → 任何人用相同邮箱注册密码账号，再用对应
//    OAuth 登录就能登进别人账号（接管）。改为「显式身份优先」，email 兜底
//    只接纳尚未设密码的存量 OAuth 账号（见 resolve_oauth_login）。
// 3. 没有「在已登录状态下把第三方账号绑到当前用户」的能力 → 新增 bind/start
//    （POST，带 Bearer，SPA 拿 url 再整页跳）+ 复用 callback 的绑定分支。
//
// token 不进 URL：登录成功签发一次性 ticket（TTL 2 分钟，oauth_ticket），
// 只把 ticket 放 fragment；SPA 调 /api/auth/oauth/consume 用 ticket 换回
// 完整会话数据。绑定结果回 /profile#oauth_bind=…（成功/失败码，不回任何 token）。

/// 把 provider 身份写入用户的对应字段。
fn set_oauth_identity(u: &mut user::User, provider: &str, identity: &str) {
    match provider {
        "github" => u.oauth_github = identity.to_string(),
        "google" => u.oauth_google = identity.to_string(),
        "linuxdo" => u.oauth_linuxdo = identity.to_string(),
        _ => {}
    }
}

/// 读取用户某 provider 已绑定的身份（空=未绑定）。
///
/// 返回值全部借用自 `u`（`provider` 只参与匹配、不出现在返回值里），
/// 生命周期只需绑 `u`。
fn get_oauth_identity<'a>(u: &'a user::User, provider: &str) -> &'a str {
    match provider {
        "github" => &u.oauth_github,
        "google" => &u.oauth_google,
        "linuxdo" => &u.oauth_linuxdo,
        _ => "",
    }
}

/// 302 回 SPA 登录页并把一次性 ticket 放进 URL fragment（不进 query / 不进日志）。
fn redirect_login_ticket(ticket: &str) -> Response {
    Redirect::to(&format!("/login#oauth_ticket={ticket}")).into_response()
}

/// 302 回 SPA 登录页并带一个稳定的错误码（前端按码给本地化文案）。
fn redirect_login_error(code: &str) -> Response {
    Redirect::to(&format!("/login#oauth_error={code}")).into_response()
}

/// 302 回个人中心并报告绑定结果（provider + 状态 + 可选错误码，均不含敏感数据）。
fn redirect_bind_result(provider: &str, status: &str, reason: Option<&str>) -> Response {
    let tail = match reason {
        Some(r) => format!("&reason={r}"),
        None => String::new(),
    };
    Redirect::to(&format!(
        "/profile#oauth_bind={provider}&status={status}{tail}"
    ))
    .into_response()
}

/// 建会话（与密码登录同一签发口径），返回 (token, expires_at)。
async fn issue_oauth_session(state: &AppState, email: &str) -> (String, i64) {
    let config = state.config_manager.get().await;
    let session_ttl = config.admin.session_ttl_hours.max(1);
    let session_secret = if config.admin.session_secret.is_empty() {
        Uuid::new_v4().to_string()
    } else {
        config.admin.session_secret.clone()
    };
    let session_store = SessionStore::new(&session_secret, session_ttl);
    let session = session_store.create_session(email);
    state.session_registry.register(email, &session.session_id);
    (session.token, session.expires_at)
}

/// 登录流归属解析：显式身份优先 → email 兜底（仅存量未设密码的 OAuth 账号，
/// 命中即补写身份升级为显式绑定）→ 建号。返回 Err(错误码) 交由上层重定向提示。
async fn resolve_oauth_login(
    state: &AppState,
    provider: &str,
    identity: &str,
    email: Option<&str>,
    username: &str,
    register_quota: i64,
) -> Result<user::User, &'static str> {
    // 1. 已显式绑定该身份
    if let Some(u) = state.user_store.find_by_oauth(provider, identity) {
        if u.status != "active" {
            return Err("disabled");
        }
        return Ok(u);
    }
    // 2. email 兜底
    if let Some(em) = email.map(str::trim).filter(|e| !e.is_empty()) {
        if let Some(u) = state.user_store.get_by_email(em) {
            if u.status != "active" {
                return Err("disabled");
            }
            // 该邮箱是「已设密码」的账号：绝不在登录流里自动绑定 —— 这正是接管
            // 攻击面。要关联请先到个人中心登录后手动绑定。
            if u.password_set {
                return Err("need_bind");
            }
            // 存量 OAuth 账号（随机密码 / 从未设过密码）：升级为显式绑定。
            let uid = u.id.clone();
            let pid = provider.to_string();
            let idt = identity.to_string();
            if let Ok(updated) = state
                .user_store
                .update(&uid, |x| set_oauth_identity(x, &pid, &idt))
            {
                return Ok(updated);
            }
            return Ok(u);
        }
    }
    // 3. 无匹配：建号。缺 email 用伪邮箱（与旧行为一致）。
    let final_email = email
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| e.to_string())
        .unwrap_or_else(|| format!("{username}@{provider}.local"));
    let uname = if username.is_empty() {
        format!("{provider}用户")
    } else {
        username.to_string()
    };
    state
        .user_store
        .create_oauth_user(&final_email, &uname, provider, identity, register_quota)
        .map_err(|e| {
            tracing::warn!("OAuth create user failed ({provider}): {e}");
            "create_failed"
        })
}

/// 绑定流归属校验：把 provider 身份绑到 `target_email`（个人中心发起）。
/// 冲突（该身份已属他人 / 目标已有别的同 provider 身份）时拒绝并记安全事件。
async fn resolve_oauth_bind(
    state: &AppState,
    provider: &str,
    identity: &str,
    target_email: &str,
) -> Result<(), &'static str> {
    let target = state
        .user_store
        .get_by_email(target_email)
        .ok_or("unknown_user")?;
    // 该第三方身份是否已被「别的用户」占用
    if let Some(occ) = state.user_store.find_by_oauth(provider, identity) {
        if occ.email != target.email {
            state.log_store.record_security(
                crate::log::SecurityEvent::new(
                    crate::log::SecurityEventType::Intrusion,
                    "critical",
                    format!(
                        "OAuth 绑定冲突：{provider} 身份 {identity} 已属用户 {}，用户 {} 试图占用",
                        occ.email, target.email
                    ),
                )
                .with_user(Some(target.id.clone())),
            );
            return Err("taken");
        }
        // 已在目标身上 → 幂等成功
        return Ok(());
    }
    // 目标用户在同 provider 上已绑了另一个身份
    let cur = get_oauth_identity(&target, provider);
    if !cur.is_empty() && cur != identity {
        return Err("conflict");
    }
    let uid = target.id.clone();
    let pid = provider.to_string();
    let idt = identity.to_string();
    state
        .user_store
        .update(&uid, |x| set_oauth_identity(x, &pid, &idt))
        .map_err(|e| {
            tracing::warn!("OAuth bind persist failed: {e}");
            "persist_failed"
        })?;
    state.log_store.record_security(
        crate::log::SecurityEvent::new(
            crate::log::SecurityEventType::AuthFailure,
            "info",
            format!("用户 {} 绑定了 {provider} 账号", target.email),
        )
        .with_user(Some(target.id.clone())),
    );
    Ok(())
}

/// 统一 OAuth 回调：按 state 命中哪个缓存区分登录 / 绑定两条流。
async fn oauth_handle_callback(
    state: AppState,
    provider: &'static str,
    code: String,
    state_param: Option<String>,
) -> Response {
    if code.trim().is_empty() {
        return redirect_login_error("no_code");
    }
    let sp = match state_param {
        Some(ref s) if !s.is_empty() => s.clone(),
        _ => return redirect_login_error("missing_state"),
    };
    // 判定流程 + 一次性消费对应缓存
    let bind_email: Option<String> = match state.oauth_bind_state.get(&sp).await {
        Some(email) => {
            state.oauth_bind_state.remove(&sp).await;
            Some(email)
        }
        None => {
            if state.oauth_state_cache.get(&sp).await.is_some() {
                state.oauth_state_cache.remove(&sp).await;
                None // 登录流
            } else {
                return redirect_login_error("invalid_state");
            }
        }
    };

    // 换 access token + 拉 profile（三家实现不同）
    let config = state.config_manager.get().await;
    let register_quota = config.usage.register_quota;
    let (identity, email, username) =
        match fetch_oauth_profile(&state, &config, provider, code.trim()).await {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!("OAuth callback {provider} failed: {err}");
                return redirect_login_error("upstream_failed");
            }
        };

    if let Some(target_email) = bind_email {
        // ── 绑定流 ──
        match resolve_oauth_bind(&state, provider, &identity, &target_email).await {
            Ok(()) => redirect_bind_result(provider, "ok", None),
            Err(reason) => redirect_bind_result(provider, "error", Some(reason)),
        }
    } else {
        // ── 登录流 ──
        let user = match resolve_oauth_login(
            &state,
            provider,
            &identity,
            email.as_deref(),
            &username,
            register_quota,
        )
        .await
        {
            Ok(u) => u,
            Err(code) => {
                tracing::info!("OAuth login {provider} rejected: {code}");
                return redirect_login_error(code);
            }
        };
        let (token, expires_at) = issue_oauth_session(&state, &user.email).await;
        let payload = serde_json::json!({
            "token": token,
            "email": user.email,
            "username": user.username,
            "role": match user.role { Role::Admin => "admin", Role::User => "user" },
            "expires_at": expires_at,
            "must_change_password": user.must_change_password,
        });
        let ticket = Uuid::new_v4().to_string();
        state
            .oauth_ticket
            .insert(ticket.clone(), payload.to_string())
            .await;
        redirect_login_ticket(&ticket)
    }
}

/// 拉取并归一化上游 profile：返回 (稳定身份 id, 邮箱?, 用户名)。
async fn fetch_oauth_profile(
    state: &AppState,
    config: &crate::config::AppConfig,
    provider: &str,
    code: &str,
) -> anyhow::Result<(String, Option<String>, String)> {
    match provider {
        "github" => {
            let oauth = &config.github_oauth;
            if !oauth.ready() {
                anyhow::bail!("GitHub OAuth not configured");
            }
            let token =
                crate::oauth::github::exchange_code(oauth, code, &state.http_client).await?;
            let g = crate::oauth::github::get_user_info(&token, &state.http_client).await?;
            Ok((g.id.to_string(), g.email.clone(), g.login.clone()))
        }
        "google" => {
            let oauth = &config.google_oauth;
            if !oauth.ready() {
                anyhow::bail!("Google OAuth not configured");
            }
            let token =
                crate::oauth::google::exchange_code(oauth, code, &state.http_client).await?;
            let g = crate::oauth::google::get_user_info(&token, &state.http_client).await?;
            Ok((
                g.sub.clone(),
                g.email.clone(),
                g.name.clone().unwrap_or_else(|| g.sub.clone()),
            ))
        }
        "linuxdo" => {
            let oauth = &config.linuxdo_oauth;
            if !oauth.ready() {
                anyhow::bail!("LinuxDO OAuth not configured");
            }
            let token =
                crate::oauth::linuxdo::exchange_code(oauth, code, &state.http_client).await?;
            let g = crate::oauth::linuxdo::get_user_info(&token, &state.http_client).await?;
            // LinuxDO 无 email 字段，统一用伪邮箱（与旧行为一致）
            Ok((g.id.to_string(), None, g.username.clone()))
        }
        other => anyhow::bail!("unknown oauth provider {other}"),
    }
}

/// 生成 provider 登录授权 URL（三家端点不同）。未配置返回 None。
fn build_provider_authorize_url(
    config: &crate::config::AppConfig,
    provider: &str,
    state: &str,
) -> Option<String> {
    match provider {
        "github" => {
            let o = &config.github_oauth;
            if !o.ready() {
                return None;
            }
            Some(format!(
                "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&scope=user:email&state={}",
                o.client_id, o.redirect_uri, state
            ))
        }
        "google" => {
            let o = &config.google_oauth;
            if !o.ready() {
                return None;
            }
            Some(crate::oauth::google::build_authorize_url(o, state))
        }
        "linuxdo" => {
            let o = &config.linuxdo_oauth;
            if !o.ready() {
                return None;
            }
            Some(format!(
                "https://connect.linux.do/oauth2/authorize?response_type=code&client_id={}&redirect_uri={}&state={}",
                o.client_id, o.redirect_uri, state
            ))
        }
        _ => None,
    }
}

/// POST /api/auth/{provider}/bind/start —— 已登录用户发起第三方账号绑定。
///
/// 整页跳转（callback）不带 Authorization 头，所以绑定身份不能靠请求头传递：
/// 这里用 Bearer 校验当前用户，生成一次性 state 把其 email 存进 oauth_bind_state，
/// 返回授权 URL；前端再 `window.location.href = url` 整页跳过去。callback 命中
/// 该 state 即走绑定流（见 oauth_handle_callback）。
async fn oauth_bind_start(state: AppState, provider: &'static str, headers: HeaderMap) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if user.status != "active" {
        return error_response("账号已禁用", StatusCode::UNAUTHORIZED).into_response();
    }
    let config = state.config_manager.get().await;
    let state_param = Uuid::new_v4().to_string();
    let url = match build_provider_authorize_url(&config, provider, &state_param) {
        Some(u) => u,
        None => {
            return error_response(&format!("{provider} OAuth 未配置"), StatusCode::BAD_REQUEST)
                .into_response()
        }
    };
    state
        .oauth_bind_state
        .insert(state_param, user.email.clone())
        .await;
    Json(serde_json::json!({ "success": true, "data": { "url": url } })).into_response()
}

pub async fn handle_github_bind_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    oauth_bind_start(state, "github", headers).await
}
pub async fn handle_google_bind_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    oauth_bind_start(state, "google", headers).await
}
pub async fn handle_linuxdo_bind_start(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    oauth_bind_start(state, "linuxdo", headers).await
}

/// POST /api/auth/oauth/unbind —— 解绑当前用户的某 provider 第三方账号。
///
/// 防锁死：解绑前确认还剩至少一种可用登录方式（密码 / 另一个 provider / TOTP），
/// 否则解绑后用户将永远无法登录。
#[derive(Debug, Deserialize)]
pub struct OAuthUnbindRequest {
    pub provider: String,
}

pub async fn handle_oauth_unbind(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OAuthUnbindRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let provider = body.provider.trim().to_lowercase();
    if !matches!(provider.as_str(), "github" | "google" | "linuxdo") {
        return Err(error_response(
            "未知的第三方账号类型",
            StatusCode::BAD_REQUEST,
        ));
    }
    let bound = !get_oauth_identity(&user, &provider).is_empty();
    if !bound {
        return Err(error_response(
            "该第三方账号未绑定",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 防锁死：解绑该 provider 后，是否还剩其它登录方式
    let remaining_oauth = ["github", "google", "linuxdo"]
        .iter()
        .filter(|p| **p != provider.as_str())
        .any(|p| !get_oauth_identity(&user, p).is_empty());
    let has_password = user.password_set && !user.password.is_empty();
    let has_totp = user.totp_enabled;
    if !remaining_oauth && !has_password && !has_totp {
        return Err(error_response(
            "这是唯一的登录方式，解绑后将无法登录。请先设置密码或绑定其它账号。",
            StatusCode::BAD_REQUEST,
        ));
    }
    let uid = user.id.clone();
    let pv = provider.clone();
    state
        .user_store
        .update(&uid, |u| set_oauth_identity(u, &pv, ""))
        .map_err(|e| {
            error_response(&format!("解绑失败：{e}"), StatusCode::INTERNAL_SERVER_ERROR)
        })?;
    state.log_store.record_security(
        crate::log::SecurityEvent::new(
            crate::log::SecurityEventType::AuthFailure,
            "info",
            format!("用户 {} 解绑了 {provider} 账号", user.email),
        )
        .with_user(Some(user.id.clone())),
    );
    Ok(Json(
        serde_json::json!({ "success": true, "data": { "provider": provider } }),
    ))
}

/// POST /api/auth/oauth/consume —— 一次性 ticket 换回会话数据。
#[derive(Debug, Deserialize)]
pub struct OAuthConsumeRequest {
    pub ticket: String,
}

pub async fn handle_oauth_consume(
    State(state): State<AppState>,
    Json(body): Json<OAuthConsumeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ticket = body.ticket.trim();
    if ticket.is_empty() {
        return Err(error_response("缺少 ticket", StatusCode::BAD_REQUEST));
    }
    let payload = state
        .oauth_ticket
        .get(&ticket.to_string())
        .await
        .ok_or_else(|| error_response("登录票据无效或已过期", StatusCode::UNAUTHORIZED))?;
    // 一次性消费：换过即作废，防重放
    state.oauth_ticket.remove(&ticket.to_string()).await;
    let value: Value = serde_json::from_str(&payload)
        .map_err(|_| error_response("票据数据损坏", StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::json!({ "success": true, "data": value })))
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
    oauth_handle_callback(
        state,
        "google",
        params.code.unwrap_or_default(),
        params.state,
    )
    .await
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

/// GET /api/auth/github/callback — GitHub OAuth 回调处理（登录 / 绑定统一走 oauth_handle_callback）
pub async fn handle_github_oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<GithubCallbackParams>,
) -> Response {
    oauth_handle_callback(
        state,
        "github",
        params.code.unwrap_or_default(),
        params.state,
    )
    .await
}

// ── 功能 4：LinuxDO OAuth ────────────────────────────────────────────
//
// 参照 AIGX 既有 `handle_github_oauth_*` 模式；LinuxDO OAuth2 端点细节
// 对齐 new-api `oauth/linuxdo.go`（connect.linux.do）。
// 配置存于 `config.linuxdo_oauth`（`src/oauth/linuxdo.rs` + `src/config.rs`）。

/// GET /api/auth/linuxdo — 跳转到 LinuxDO OAuth 授权页
pub async fn handle_linuxdo_oauth_authorize(State(state): State<AppState>) -> Response {
    let config = state.config_manager.get().await;
    let oauth = &config.linuxdo_oauth;
    if !oauth.ready() {
        return error_response("LinuxDO OAuth not configured", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let state_param = Uuid::new_v4().to_string();
    // CSRF 防护：state 存入缓存，callback 校验并一次性消费
    state
        .oauth_state_cache
        .insert(state_param.clone(), chrono::Utc::now().timestamp())
        .await;
    let url = format!(
        "https://connect.linux.do/oauth2/authorize?response_type=code&client_id={}&redirect_uri={}&state={}",
        oauth.client_id,
        oauth.redirect_uri,
        state_param,
    );
    Redirect::to(&url).into_response()
}

/// GET /api/auth/linuxdo/callback — LinuxDO OAuth 回调处理（统一走 oauth_handle_callback）
pub async fn handle_linuxdo_oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<LinuxDoCallbackParams>,
) -> Response {
    oauth_handle_callback(
        state,
        "linuxdo",
        params.code.unwrap_or_default(),
        params.state,
    )
    .await
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
    /// ALTCHA 机器人校验 payload（base64 的 JSON；开关关闭时可缺省）。
    #[serde(default)]
    pub captcha: Option<String>,
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
    connect: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<LoginTotpRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let client_ip = extract_client_ip(&headers)
        .or_else(|| connect.map(|c| c.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string());

    // 锁定检查（与密码登录共用计数器；阈值走安全策略快照）
    let fail_count = state.login_failures.get(&client_ip).await.unwrap_or(0);
    if crate::config::security_lockout_enabled()
        && fail_count >= crate::config::security_fail_threshold()
    {
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
    connect: Option<axum::extract::ConnectInfo<std::net::SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<LoginCodeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let email = body.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(error_response("邮箱不能为空", StatusCode::BAD_REQUEST));
    }

    let client_ip = extract_client_ip(&headers)
        .or_else(|| connect.map(|c| c.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    // 每 IP 每分钟最多 5 次发送（防刷邮件）。键带 send-code 前缀，
    // 与登录/忘记密码的额度相互独立（否则互吃额度导致误报 429）。
    const SEND_CODE_RATE_LIMIT: u32 = 5;
    let send_code_key = format!("send-code:{client_ip}");
    let sent_count = state.login_limiter.get(&send_code_key).await.unwrap_or(0);
    if sent_count >= SEND_CODE_RATE_LIMIT {
        return Err(error_response(
            "验证码发送过于频繁，请稍后再试",
            StatusCode::TOO_MANY_REQUESTS,
        ));
    }
    state
        .login_limiter
        .insert(send_code_key, sent_count + 1)
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

    // 未配置 SMTP：拒绝发送而非直出验证码——原先把 6 位验证码放在响应体里，
    // 等于对任意已注册邮箱开放无密码登录（见安全审计 P0-3）。
    tracing::warn!(
        "Login code requested for {} but SMTP not configured; no code issued",
        user.email
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "sent": false, "expires_in_secs": 300, "message": "If the email exists, a code has been sent" }
    })))
}

/// POST /api/auth/login/code — 邮箱验证码登录
pub async fn handle_login_with_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginWithCodeRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // ── 机器人校验：与密码登录同口径，放在失败计数之前 ──
    if crate::config::security_captcha_enabled() {
        let token = body.captcha.as_deref().unwrap_or("");
        if token.trim().is_empty() {
            return Err(error_response(
                "请先完成机器人校验",
                StatusCode::BAD_REQUEST,
            ));
        }
        if let Err(message) = crate::altcha::verify(&state, token) {
            return Err(error_response(&message, StatusCode::BAD_REQUEST));
        }
    }

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
    if crate::config::security_lockout_enabled()
        && fail_count >= crate::config::security_fail_threshold()
    {
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

/// GET /api/public/geo-lang — 按客户端 IP 推荐界面语言（登录页默认语言用）。
///
/// - 无代理头（`trust_proxy_headers = false` 或直连）→ `lang: null`，前端回退 `navigator.language`
/// - 中国大陆 IPv4 → `"zh"`；其他 IPv4 → `"en"`；IPv6 → `null`
///
/// 公开端点，无需鉴权；不泄露 IP 本身，只返回语言建议。
pub async fn handle_geo_lang(headers: HeaderMap) -> Json<Value> {
    let lang = extract_client_ip(&headers)
        .and_then(|ip| crate::api::common::geo_language_for_ip(&ip).map(|s| s.to_string()));
    Json(serde_json::json!({ "success": true, "data": { "lang": lang } }))
}
