//! 账号池「网页登录」接口（`/api/account-sessions/login/*`）。
//!
//! 前端流程：`start` 拿授权地址并打开浏览器 → 每 2 秒 `wait` → 完成后账号
//! 自动落进账号池。远程部署（浏览器到不了网关 loopback）时，可把浏览器地址栏
//! 里的最终回调地址交给 `callback`。

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{Html, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::account_login;

#[derive(Debug, Deserialize)]
pub struct LoginStartRequest {
    pub provider: String,
    #[serde(default)]
    pub channel_id: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginStateRequest {
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct SmsSendRequest {
    pub provider: String,
    pub phone: String,
}

#[derive(Debug, Deserialize)]
pub struct SmsVerifyRequest {
    pub provider: String,
    pub phone: String,
    pub code: String,
    #[serde(default)]
    pub msgid: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginCallbackRequest {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub callback_url: String,
}

/// AutoClaw OAuth 发起请求：`{ provider, vendor, captchaVerifyParam, channelId }`。
///
/// `vendor` 是 `zai` / `google`；`captchaVerifyParam` 由前端跑完阿里云验证码 SDK
/// 得到（后端不解析它，原样转交上游）。`callbackBase` 由后端从 server.port 拼。
#[derive(Debug, Deserialize)]
pub struct OAuthStartRequest {
    pub provider: String,
    /// OAuth 变体：`zai` / `google`
    pub vendor: String,
    /// 阿里云风控验证码参数（前端 SDK 产出）
    #[serde(default, rename = "captchaVerifyParam")]
    pub captcha_verify_param: String,
    #[serde(default, rename = "channelId")]
    pub channel_id: String,
}

/// GET /api/account-sessions/login/oauth/captcha-config?provider=autoclaw
///
/// 取阿里云风控验证码配置（region/prefix/sceneId/supplier），前端据此初始化 SDK。
/// `enabled: false` 不是错误（国内版不启用 OAuth）。
pub async fn handle_login_oauth_captcha_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(
        params.get("provider").map(String::as_str).unwrap_or("autoclaw"),
    );
    if provider != "autoclaw" {
        return Err(error_response(
            "该端点仅支持 AutoClaw OAuth",
            StatusCode::BAD_REQUEST,
        ));
    }
    let userapi = crate::account_login::autoclaw::INTL_USERAPI;
    let client = reqwest::Client::new();
    match crate::account_login::autoclaw::captcha_config(&userapi, &client).await {
        Ok(config) => Ok(Json(json!({ "success": true, "data": config }))),
        Err(error) => Err(error_response(&error, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/account-sessions/login/oauth/start
///
/// 发起 AutoClaw OAuth 登录（Zai / Google）。前端先调 captcha-config 拿风控配置，
/// 跑完阿里云 SDK 后把 `captchaVerifyParam` 传进来，后端取授权地址返回。
pub async fn handle_login_oauth_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OAuthStartRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    if provider != "autoclaw" {
        return Err(error_response(
            "该端点仅支持 AutoClaw OAuth",
            StatusCode::BAD_REQUEST,
        ));
    }
    if body.vendor.trim().is_empty() {
        return Err(error_response(
            "缺少 vendor（可选 zai / google）",
            StatusCode::BAD_REQUEST,
        ));
    }
    if body.captcha_verify_param.trim().is_empty() {
        return Err(error_response(
            "缺少风控验证参数，请先完成验证码",
            StatusCode::BAD_REQUEST,
        ));
    }
    let config = state.config_manager.get().await;
    let callback_base = format!("http://localhost:{}", config.server.port);
    let started = account_login::start_oauth(
        &state,
        &provider,
        &body.channel_id,
        body.vendor.trim(),
        body.captcha_verify_param.trim(),
        &callback_base,
    )
    .await
    .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({ "success": true, "data": started })))
}

/// 粘贴式登录请求：把凭证交给网关归一化 + 校验一次后落账号。
#[derive(Debug, Deserialize)]
pub struct PasteLoginRequest {
    pub provider: String,
    #[serde(default)]
    pub channel_id: String,
    /// 粘贴的凭证（Cookie / token / API Key，形态由 provider 决定）
    pub secret: String,
    /// 站点覆盖（MonkeyCode 国内 .com / 国际 .net）
    #[serde(default)]
    pub site: String,
    #[serde(default)]
    pub name: String,
    /// KukuAI Cookies 文件导入：`Network\Cookies` 文件内容的 base64
    /// （与粘贴的 `secret` 二选一，给了文件就走文件导入分支）。
    #[serde(default)]
    pub auth_file_b64: String,
}

/// POST /api/account-sessions/login/paste
///
/// 用于「没有可代跑网页登录」的家（MonkeyCode / KukuAI / OrcaTerm /
/// Command Code）：用户在浏览器里自己登录后把凭证粘进来，网关归一化并
/// 校验一次（打一个只读端点）后落账号。校验失败直接返回可读错误，
/// 不必等到转发时才暴露。
pub async fn handle_login_paste(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PasteLoginRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    let client = reqwest::Client::new();
    let secret = body.secret.trim();
    let importing_file = provider == "kuku" && !body.auth_file_b64.trim().is_empty();
    if secret.is_empty() && !importing_file {
        return Err(error_response("请粘贴凭证", StatusCode::BAD_REQUEST));
    }
    // KukuAI 文件导入分支：Cookies SQLite 文件 base64 → 解析出 Cookie 串，
    // 再走与粘贴登录完全相同的 verify_kuku 校验与落库（口径不分叉）。
    // 解析依赖 rusqlite，仅 sqlite-kv feature 下可用；其余后端构建明确拒绝，
    // 而不是让用户看到「请粘贴凭证」这种与操作不符的提示。
    #[cfg(feature = "sqlite-kv")]
    if importing_file {
        let cookie = account_login::paste::kuku_cookie_from_sqlite_b64(body.auth_file_b64.trim())
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
        let creds = account_login::paste::verify_kuku(&client, &cookie)
            .await
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
        let creds = if body.name.trim().is_empty() {
            creds
        } else {
            let mut named = creds;
            named.name = body.name.trim().to_string();
            named
        };
        let account_id =
            account_login::save_credentials(&state, &provider, &body.channel_id, creds)
                .map_err(|error| error_response(&error, StatusCode::INTERNAL_SERVER_ERROR))?;
        return Ok(Json(
            json!({ "success": true, "data": { "accountId": account_id } }),
        ));
    }
    #[cfg(not(feature = "sqlite-kv"))]
    if importing_file {
        return Err(error_response(
            "当前构建不支持 Cookies 文件导入（需要 sqlite-kv 后端），请改用粘贴登录",
            StatusCode::BAD_REQUEST,
        ));
    }
    let mut creds = match provider.as_str() {
        "monkeycode" | "monkeycode-intl" => {
            let site = if body.site.trim().is_empty() {
                if provider == "monkeycode-intl" {
                    "https://monkeycode-ai.net"
                } else {
                    "https://monkeycode-ai.com"
                }
            } else {
                body.site.trim()
            };
            account_login::paste::verify_monkeycode(&client, site, secret).await
        }
        "kuku" => account_login::paste::verify_kuku(&client, secret).await,
        "orcaterm" => account_login::paste::verify_orcaterm(secret).await,
        "commandcode" => account_login::paste::verify_commandcode(&client, secret).await,
        other => Err(format!(
            "「{other}」不走粘贴式登录（请用网页登录或「填写凭证」表单）"
        )),
    }
    .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    if !body.name.trim().is_empty() {
        creds.name = body.name.trim().to_string();
    }
    let account_id = account_login::save_credentials(&state, &provider, &body.channel_id, creds)
        .map_err(|error| error_response(&error, StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({ "success": true, "data": { "accountId": account_id } }),
    ))
}

/// POST /api/account-sessions/login/start
pub async fn handle_login_start(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginStartRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let started = account_login::start(&state, &body.provider, &body.channel_id)
        .await
        .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(json!({ "success": true, "data": started })))
}

/// GET /api/account-sessions/login/wait?state=
pub async fn handle_login_wait(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let task_state = params.get("state").map(String::as_str).unwrap_or("");
    let Some(task) = account_login::snapshot(task_state) else {
        return Err(error_response(
            "登录任务不存在或已过期",
            StatusCode::NOT_FOUND,
        ));
    };
    Ok(Json(json!({
        "success": true,
        "data": account_login::wait_response(&task),
    })))
}

/// POST /api/account-sessions/login/cancel
pub async fn handle_login_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginStateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let canceled = account_login::cancel(body.state.trim());
    Ok(Json(
        json!({ "success": true, "data": { "canceled": canceled } }),
    ))
}

/// POST /api/account-sessions/login/callback（手工粘贴回调地址）
pub async fn handle_login_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LoginCallbackRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.callback_url.trim().is_empty() {
        return Err(error_response(
            "缺少 callback_url（登录回调地址）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let account_id =
        account_login::submit_callback(&state, body.state.trim(), body.callback_url.trim())
            .await
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    Ok(Json(
        json!({ "success": true, "data": { "accountId": account_id } }),
    ))
}

/// POST /api/account-sessions/login/sms/send（发送手机验证码）
pub async fn handle_login_sms_send(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SmsSendRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    let msgid = match provider.as_str() {
        "loomy" => account_login::loomy::send_code(&reqwest::Client::new(), &body.phone)
            .await
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?,
        other => {
            return Err(error_response(
                &format!("「{other}」的手机验证码登录尚未接入网关（AutoClaw 发码强制要求阿里云验证码，请在客户端登录后粘贴 token）"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    Ok(Json(json!({ "success": true, "data": { "msgid": msgid } })))
}

/// POST /api/account-sessions/login/sms/verify（验证码登录并落账号）
pub async fn handle_login_sms_verify(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SmsVerifyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = crate::account_session::registry::normalize_kind(&body.provider);
    if provider != "loomy" {
        return Err(error_response(
            &format!("「{provider}」的手机验证码登录尚未接入网关"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let creds = account_login::loomy::login_with_code(
        &reqwest::Client::new(),
        &body.phone,
        &body.code,
        &body.msgid,
        if body.name.trim().is_empty() {
            None
        } else {
            Some(body.name.trim())
        },
    )
    .await
    .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    let account_id = account_login::save_credentials(&state, &provider, &body.channel_id, creds)
        .map_err(|error| error_response(&error, StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({ "success": true, "data": { "accountId": account_id } }),
    ))
}

/// 浏览器 OAuth 回调（免鉴权）：换凭证 → 落账号 → 返回 HTML。
///
/// 供 `GET /auth/callback-{zai|google}` 路由调用。从 URI 路径提取 vendor，
/// 从 query 提取 code/state，找到进行中的任务后完成换证。
///
/// 回调 URL 不带 task state（与上游 state 参数名冲突），靠 provider + vendor
/// 匹配最近发起的 pending 任务。
pub async fn handle_browser_oauth_callback(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    uri: Uri,
) -> Html<String> {
    let path = uri.path();
    let Some(vendor) = path.strip_prefix("/auth/callback-") else {
        return Html(page_html(
            false,
            "无效的回调地址",
            "请从管理界面发起登录。",
        ));
    };
    let code = params.get("code").map(String::as_str).unwrap_or("");
    let state_param = params.get("state").map(String::as_str).unwrap_or("");
    if code.trim().is_empty() {
        return Html(page_html(
            false,
            "回调没有携带授权码",
            "请返回管理界面重新发起登录。",
        ));
    }
    let config = state.config_manager.get().await;
    let callback_url = format!(
        "http://localhost:{}/auth/callback-{}?code={}&state={}",
        config.server.port, vendor, code, state_param
    );
    match account_login::find_pending_by_vendor("autoclaw", vendor) {
        Some(task) => {
            match account_login::complete_browser_callback(&state, &task, &callback_url).await {
                Ok(account_id) => Html(page_html(
                    true,
                    "登录成功",
                    &format!("账号已添加（ID: {account_id}），可以关闭此页面返回管理界面。"),
                )),
                Err(error) => Html(page_html(
                    false,
                    "登录失败",
                    &format!("{error}。请返回管理界面重新发起登录。"),
                )),
            }
        }
        None => Html(page_html(
            false,
            "没有找到进行中的登录任务",
            "可能已过期或已完成。请返回管理界面重新发起登录。",
        )),
    }
}

/// 简单的 HTML 结果页（成功/失败给用户一个明确反馈）。
fn page_html(success: bool, title: &str, message: &str) -> String {
    let icon = if success { "✅" } else { "❌" };
    let color = if success { "#22c55e" } else { "#ef4444" };
    format!(
        "<!DOCTYPE html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>{title}</title></head><body style=\"font-family:system-ui,sans-serif;text-align:center;padding:4rem 1rem\"><h2 style=\"color:{color}\">{icon} {title}</h2><p>{message}</p></body></html>"
    )
}
