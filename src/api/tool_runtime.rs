// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 工具服务的运行时端点（对外给客户端调用，自己校验网关 Key）。
//!
//! 与上游端点路径逐字对齐（客户端接入地址写死，改动会影响已接入方）：
//!   · `GET  /skill/web-search`           服务元数据（免鉴权）
//!   · `POST /skill/web-search/search`    批量搜索
//!   · `POST /skill/web-search/fetch`     批量抓取
//!   · `POST /mcp/web-search`             网络搜索 MCP（Streamable HTTP）
//!   · `POST /mcp/vision`                 视觉分析 MCP
//!   · `GET  /api/web-search`             REST 元数据（免鉴权）
//!   · `POST /api/web-search/search|fetch`
//!
//! 鉴权复用网关 Key（`Authorization: Bearer <key>` 或 `x-api-key`），与模型
//! 调用同一把钥匙；**跳过模型白名单**（工具调用没有具体模型），保留启用 /
//! 过期 / 额度 / IP / 用户余额检查。账号型搜索遵守 Key 的 provider 白名单，
//! 独立搜索 API 不按模型过滤。

use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::api::auth::{ApiKey, ApiKeyError};
use crate::api::common::{extract_api_key_bearer_first, extract_client_ip};
use crate::api::openai::AppState;
use crate::tool_services::{
    self, McpRequestContext, McpServerKind, SearchError, SearchErrorCode, ToolServices,
};

/// 工具数据面共用 HTTP 客户端。
///
/// 关闭自动重定向：抓取 URL 的「公网白名单」只校验了初始地址，跟随重定向
/// 会让 `https://public.example` → `http://127.0.0.1` 绕过检查（SSRF）。
/// 超时由每个请求自己覆盖（`RequestBuilder::timeout`），这里给一个宽松上限。
fn tool_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(600))
            .user_agent("AIGX-Tools/1.0")
            .build()
            .unwrap_or_else(|error| {
                tracing::warn!("[Tools] 工具数据面 HTTP 客户端构造失败，回退默认：{error}");
                reqwest::Client::new()
            })
    })
}

/// 管理面复用同一客户端（工具连通性测试 / 视觉测试）。
pub(crate) fn tool_client_ref() -> &'static reqwest::Client {
    tool_client()
}

/// 网关基址：优先取请求 Host（客户端看到的就是它要填的地址），没有则回环。
fn base_url(headers: &HeaderMap) -> String {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let scheme = headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .filter(|value| *value == "https")
        .unwrap_or("http");
    format!("{scheme}://{host}")
}

fn json_response(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

fn unauthorized() -> Response {
    json_response(
        StatusCode::UNAUTHORIZED,
        json!({ "error": { "code": "unauthorized", "message": "Missing or invalid API key. Pass it as \"Authorization: Bearer <AIGX API Key>\"." } }),
    )
}

fn auth_error(error: ApiKeyError) -> Response {
    let status = match error {
        ApiKeyError::Invalid | ApiKeyError::Disabled => StatusCode::UNAUTHORIZED,
        ApiKeyError::Expired
        | ApiKeyError::ModelNotAllowed(_)
        | ApiKeyError::QuotaExhausted
        | ApiKeyError::UserQuotaExhausted
        | ApiKeyError::IpNotAllowed(_) => StatusCode::FORBIDDEN,
    };
    json_response(
        status,
        json!({ "error": { "code": "auth_error", "message": error.to_string() } }),
    )
}

/// 校验网关 Key（跳过模型白名单；保留其余全部检查）。
///
/// 错误用 `Box<Response>`：axum 的 `Response` 自身 ~128 字节，直接塞进
/// `Result::Err` 会让每个调用栈帧都变大（clippy `result_large_err`）。
fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<ApiKey, Box<Response>> {
    let key = extract_api_key_bearer_first(headers).ok_or_else(|| Box::new(unauthorized()))?;
    let ip = extract_client_ip(headers);
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(Box::new(json_response(
                StatusCode::FORBIDDEN,
                json!({ "error": { "code": "ip_blocked", "message": ip_err.to_string() } }),
            )));
        }
    }
    state
        .api_key_store
        .validate_listing(&key, ip.as_deref())
        .map_err(|error| Box::new(auth_error(error)))
}

fn search_error_response(error: &SearchError) -> Response {
    json_response(
        StatusCode::from_u16(error.code.http_status()).unwrap_or(StatusCode::BAD_GATEWAY),
        json!({ "error": { "code": error.code.as_str(), "message": error.message } }),
    )
}

fn skill_disabled() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({ "error": "Skill service is disabled", "code": "skill_service_disabled" }),
    )
}

fn api_disabled() -> Response {
    json_response(
        StatusCode::SERVICE_UNAVAILABLE,
        json!({ "error": { "code": "api_service_disabled", "message": "API service is disabled" } }),
    )
}

fn parse_json_body(body: &Bytes) -> Result<Value, Box<Response>> {
    serde_json::from_slice::<Value>(body).map_err(|error| {
        Box::new(json_response(
            StatusCode::BAD_REQUEST,
            json!({ "error": { "code": "invalid_input", "message": format!("请求体不是合法 JSON：{error}") } }),
        ))
    })
}

fn services(state: &AppState) -> &ToolServices {
    state.tool_services.as_ref()
}

/// 网关 Key 的 provider 白名单（空列表归一为 None = 不限）。
fn allowed_providers(api_key: &ApiKey) -> Option<Vec<String>> {
    api_key
        .allowed_providers
        .as_ref()
        .filter(|list| !list.is_empty())
        .cloned()
}

/* ─── Skill 端点 ───────────────────────────────────────────── */

/// `GET /skill/web-search`（免鉴权：只回服务元数据）。
pub async fn skill_metadata(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !tool_services::is_enabled(
        services(&state),
        tool_services::WEB_SEARCH_SERVICE_ID,
        "skill",
    ) {
        return skill_disabled();
    }
    let base = base_url(&headers);
    json_response(
        StatusCode::OK,
        json!({
            "name": tool_services::WEB_SEARCH_SKILL_NAME,
            "version": tool_services::WEB_SEARCH_SKILL_VERSION,
            "recommended_variant": tool_services::recommended_skill_variant(),
            "variants": tool_services::skill_variants(),
            "endpoints": {
                "search": format!("{}/skill/web-search/search", base.trim_end_matches('/')),
                "fetch": format!("{}/skill/web-search/fetch", base.trim_end_matches('/')),
            },
        }),
    )
}

async fn skill_operation(
    state: &AppState,
    api_key: &ApiKey,
    body: Bytes,
    operation: &str,
) -> Response {
    if !tool_services::is_enabled(
        services(state),
        tool_services::WEB_SEARCH_SERVICE_ID,
        "skill",
    ) {
        return skill_disabled();
    }
    let payload = match parse_json_body(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let field = if operation == "search" {
        "queries"
    } else {
        "urls"
    };
    let items = match tool_services::batch_items(payload.get(field), field) {
        Ok(items) => items,
        Err(error) => return search_error_response(&error),
    };
    let allowed = allowed_providers(api_key);
    let outcome = if operation == "search" {
        tool_services::run_search(
            services(state),
            &state.account_sessions,
            tool_client(),
            allowed.as_deref(),
            items,
        )
        .await
        .map(|run| (run.payload, run.successful))
    } else {
        tool_services::run_fetch(
            services(state),
            &state.account_sessions,
            tool_client(),
            items,
        )
        .await
        .map(|run| (run.payload, run.successful))
    };
    match outcome {
        Ok((payload, successful)) => {
            let status = if successful == 0 {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            };
            json_response(status, payload)
        }
        Err(error) => search_error_response(&error),
    }
}

/// `POST /skill/web-search/search`
pub async fn skill_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = match authenticate(&state, &headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    skill_operation(&state, &api_key, body, "search").await
}

/// `POST /skill/web-search/fetch`
pub async fn skill_fetch(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = match authenticate(&state, &headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    skill_operation(&state, &api_key, body, "fetch").await
}

/* ─── REST API 端点 ────────────────────────────────────────── */

/// `GET /api/web-search`（免鉴权：只回服务元数据）。
pub async fn api_metadata(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !tool_services::is_enabled(
        services(&state),
        tool_services::WEB_SEARCH_SERVICE_ID,
        "api",
    ) {
        return api_disabled();
    }
    let base = base_url(&headers);
    json_response(
        StatusCode::OK,
        json!({
            "name": tool_services::WEB_SEARCH_SKILL_NAME,
            "version": tool_services::WEB_SEARCH_SKILL_VERSION,
            "auth": "Authorization: Bearer <AIGX API Key>",
            "endpoints": {
                "search": format!("{}/api/web-search/search", base.trim_end_matches('/')),
                "fetch": format!("{}/api/web-search/fetch", base.trim_end_matches('/')),
            },
        }),
    )
}

async fn api_operation(
    state: &AppState,
    api_key: &ApiKey,
    body: Bytes,
    operation: &str,
) -> Response {
    if !tool_services::is_enabled(services(state), tool_services::WEB_SEARCH_SERVICE_ID, "api") {
        return api_disabled();
    }
    let payload = match parse_json_body(&body) {
        Ok(value) => value,
        Err(response) => return *response,
    };
    let field = if operation == "search" {
        "queries"
    } else {
        "urls"
    };
    let items = match tool_services::batch_items(payload.get(field), field) {
        Ok(items) => items,
        Err(error) => return search_error_response(&error),
    };
    let allowed = allowed_providers(api_key);
    let outcome = if operation == "search" {
        tool_services::run_search(
            services(state),
            &state.account_sessions,
            tool_client(),
            allowed.as_deref(),
            items,
        )
        .await
        .map(|run| (run.payload, run.successful))
    } else {
        tool_services::run_fetch(
            services(state),
            &state.account_sessions,
            tool_client(),
            items,
        )
        .await
        .map(|run| (run.payload, run.successful))
    };
    match outcome {
        Ok((payload, successful)) => {
            let status = if successful == 0 {
                StatusCode::BAD_GATEWAY
            } else {
                StatusCode::OK
            };
            json_response(status, payload)
        }
        Err(error) => search_error_response(&error),
    }
}

/// `POST /api/web-search/search`
pub async fn api_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let api_key = match authenticate(&state, &headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    api_operation(&state, &api_key, body, "search").await
}

/// `POST /api/web-search/fetch`
pub async fn api_fetch(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let api_key = match authenticate(&state, &headers) {
        Ok(key) => key,
        Err(response) => return *response,
    };
    api_operation(&state, &api_key, body, "fetch").await
}

/* ─── MCP 端点 ─────────────────────────────────────────────── */

async fn mcp_post(
    state: &AppState,
    headers: HeaderMap,
    body: Bytes,
    kind: McpServerKind,
) -> Response {
    let api_key = match authenticate(state, &headers) {
        Ok(key) => key,
        Err(_) => {
            return json_response(
                StatusCode::UNAUTHORIZED,
                json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32600, "message": "Unauthorized" } }),
            )
        }
    };
    let service = match kind {
        McpServerKind::WebSearch => tool_services::WEB_SEARCH_SERVICE_ID,
        McpServerKind::Vision => tool_services::VISION_SERVICE_ID,
    };
    if !tool_services::is_enabled(services(state), service, "mcp") {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32000, "message": "MCP service is disabled" } }),
        );
    }
    let payload = match serde_json::from_slice::<Value>(&body) {
        Ok(value) => value,
        Err(_) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "jsonrpc": "2.0", "id": Value::Null, "error": { "code": -32700, "message": "Request body must be a JSON-RPC 2.0 message" } }),
            )
        }
    };
    let context = McpRequestContext {
        services: services(state),
        sessions: &state.account_sessions,
        // 结构体字段初始化不做自动解引用：`&Arc<ApiKeyStore>` 不能当 `&ApiKeyStore`。
        keys: state.api_key_store.as_ref(),
        client: tool_client(),
        base_url: base_url(&headers),
        allowed_providers: allowed_providers(&api_key),
        owner: api_key.id.clone(),
    };
    match tool_services::mcp_dispatch(&payload, kind, &context).await {
        Some(response) => json_response(StatusCode::OK, response),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// `POST /mcp/web-search`
pub async fn mcp_web_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mcp_post(&state, headers, body, McpServerKind::WebSearch).await
}

/// `POST /mcp/vision`
pub async fn mcp_vision(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    mcp_post(&state, headers, body, McpServerKind::Vision).await
}

/// `GET/DELETE /mcp/*`：按规范用 405 明确告知（本类服务不提供服务端主动推流）。
pub async fn mcp_unsupported() -> Response {
    tool_services::mcp_unsupported().await
}

/// 工具数据面路由（无管理鉴权；每个 handler 自己校验网关 Key）。
pub fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/skill/web-search", get(skill_metadata))
        .route("/skill/web-search/search", post(skill_search))
        .route("/skill/web-search/fetch", post(skill_fetch))
        .route("/api/web-search", get(api_metadata))
        .route("/api/web-search/search", post(api_search))
        .route("/api/web-search/fetch", post(api_fetch))
        .route("/mcp/web-search", post(mcp_web_search).get(mcp_unsupported))
        .route("/mcp/vision", post(mcp_vision).get(mcp_unsupported))
}

/// 便于测试与文档引用：错误码 → HTTP 状态。
pub fn status_for(code: SearchErrorCode) -> u16 {
    code.http_status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_prefers_host_and_forwarded_proto() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "gw.example.com:8443".parse().unwrap());
        assert_eq!(base_url(&headers), "http://gw.example.com:8443");
        headers.insert("x-forwarded-proto", "https".parse().unwrap());
        assert_eq!(base_url(&headers), "https://gw.example.com:8443");
        let empty = HeaderMap::new();
        assert_eq!(base_url(&empty), "http://127.0.0.1");
    }

    #[test]
    fn allowed_providers_normalizes_empty_to_none() {
        let mut key: ApiKey = serde_json::from_value(json!({
            "id": "k",
            "key": "sk-test",
            "name": "k",
            "created_at": 0,
        }))
        .unwrap();
        assert!(allowed_providers(&key).is_none());
        key.allowed_providers = Some(Vec::new());
        assert!(allowed_providers(&key).is_none());
        key.allowed_providers = Some(vec!["workbuddy".into()]);
        assert_eq!(allowed_providers(&key).unwrap(), vec!["workbuddy"]);
    }
}
