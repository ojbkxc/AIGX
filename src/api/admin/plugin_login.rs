// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件登录的两个交互面：**交互式登录**（表单/验证码）与**本机 wasm 回调**。
//!
//! ```text
//! GET  /api/session/login/interaction?state=<网关登录 state>  读当前交互步骤
//! POST /api/session/login/interaction                          提交一步输入
//! GET  /api/session/login/wasm-callback?state=<网关登录 state>  浏览器 302 过来的回调
//! ```
//!
//! ── 与 A2A 的对应关系 ────────────────────────────────────────
//! A2A 的这三个 handler 直接读写自己的 `LoginService` 任务表。AIGX 的插件
//! 登录桥在 `admin::account_sessions`（`plugin:<uuid>` 网关任务 + 插件
//! `login.*` RPC），任务表是那边私有的 —— 所以本文件只做 HTTP 形状与鉴权，
//! 三个动作转发给那边的两个 `pub(crate)` 入口：
//! - [`super::account_sessions::plugin_login_interaction`]（读当前步骤）
//! - [`super::account_sessions::plugin_login_submit`]（提交一步）
//!
//! 回调复用已有的 [`super::account_sessions::plugin_login_callback`]。
//!
//! ── wasm-callback 为什么免鉴权 ──────────────────────────────
//! 调用方是**用户的浏览器**（授权页 302 回来），它没有面板令牌；安全性由
//! 本次登录任务的一次性 `state` 承担（state 是本进程生成的不可预测随机串，
//! 任务表按它索引）。与 A2A 把它放在 public 组同一理由。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use super::account_sessions;
use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;

/// 交互输入上限（A2A 256 KiB 同量级）：表单字段不会接近这个量级。
const MAX_INTERACTION_BYTES: usize = 256 * 1024;

/// 管理面路由。
///
/// `wasm-callback` 与其余几条分开：它是浏览器回调，免鉴权；
/// `interaction` 的两条读写账号库/插件状态，必须过管理面鉴权。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/session/login/interaction",
            axum::routing::get(get).post(submit),
        )
        .route(
            "/api/session/login/wasm-callback",
            axum::routing::get(callback),
        )
}

/// GET /api/session/login/interaction?state=...
///
/// 返回 `{ interaction: null }` 表示当前没有待填步骤（登录已结束 / 该家
/// 不声明交互能力）—— 前端据此继续轮询 `/wait`，不是错误。
pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<axum::response::Json<Value>, (StatusCode, axum::response::Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let flow = query.get("state").map(String::as_str).unwrap_or("");
    if flow.trim().is_empty() {
        return Err(error_response("缺少 state", StatusCode::BAD_REQUEST));
    }
    match account_sessions::plugin_login_interaction(&state, flow).await {
        Ok(Some(interaction)) => Ok(axum::response::Json(
            json!({ "success": true, "data": { "interaction": interaction } }),
        )),
        Ok(None) => Err(error_response(
            "登录任务不存在或已过期",
            StatusCode::NOT_FOUND,
        )),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/session/login/interaction
///
/// body `{ state, interaction, inputs?, captcha? }`。提交一步后插件可能：
/// 返回下一步交互（继续填）、完成登录（落账号）、或失败。
pub async fn submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<axum::response::Json<Value>, (StatusCode, axum::response::Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_INTERACTION_BYTES {
        return Err(error_response(
            "登录交互输入过大",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return Err(error_response(
                "请求体不是合法 JSON",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let flow = value["state"].as_str().unwrap_or("");
    if flow.trim().is_empty() {
        return Err(error_response("缺少 state", StatusCode::BAD_REQUEST));
    }
    match account_sessions::plugin_login_submit(&state, flow, &value).await {
        Ok(result) => Ok(axum::response::Json(
            json!({ "success": true, "data": result }),
        )),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// GET /api/session/login/wasm-callback?state=...&code=...
///
/// 浏览器从授权页 302 回来：把**完整回调 URL**（本机 127.0.0.1 + 原路径与
/// 查询串）交给插件换凭证。返回一句人读文案（浏览器整页显示），不是 JSON。
pub async fn callback(
    State(state): State<AppState>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let flow = query.get("state").map(String::as_str).unwrap_or("");
    if account_sessions::plugin_login_state_known(flow) {
        let port = state.config_manager.get().await.server.port;
        let callback_url = format!("http://127.0.0.1:{port}{uri}");
        let result = account_sessions::plugin_login_callback(&state, flow, &callback_url).await;
        if result.is_ok() {
            return callback_page("授权回调已交给插件处理，请返回面板查看登录结果。");
        }
    }
    callback_page("授权回调未完成，请返回面板查看原因或重新登录。")
}

/// 回调结果的浏览器页面（纯文本，禁缓存、禁 referrer）。
fn callback_page(text: &'static str) -> Response {
    let mut response = text.into_response();
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response.headers_mut().insert(
        "referrer-policy",
        axum::http::HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_body_requires_state() {
        let ok: Value = json!({ "state": "plugin:abc", "interaction": "step-1" });
        assert!(ok["state"].as_str().is_some_and(|s| !s.trim().is_empty()));
        let missing = json!({ "interaction": "step-1" });
        assert!(missing["state"].as_str().is_none());
        assert_eq!(MAX_INTERACTION_BYTES, 262_144);
    }

    /// 回调 URL 拼接：网关端口 + **原样**路径与查询串（含 state/code）。
    fn callback_url(port: u16, uri: &str) -> String {
        format!("http://127.0.0.1:{port}{uri}")
    }

    #[test]
    fn callback_url_keeps_original_query() {
        assert_eq!(
            callback_url(8765, "/api/session/login/wasm-callback?state=x&code=y"),
            "http://127.0.0.1:8765/api/session/login/wasm-callback?state=x&code=y"
        );
    }
}
