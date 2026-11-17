// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 工具服务管理 API（`/api/tools/*`，全部管理员鉴权）。
//!
//! - 「服务 × 协议」清单 / 开关 / 执行超时；
//! - 四家搜索提供商的密钥管理（只回 configured / key_hint，不回明文）、
//!   搜索 / 抓取提供商选择、连通性测试；
//! - 视觉分析的提供商 + 模型 + 思考等级 + 内部 Key 配置与测试；
//! - 本机接入：安装 / 卸载 Skill 与 MCP 配置，安装目标探测；
//! - 视觉本地桥接脚本的下载（面板安装时直接取用）。
//!
//! 运行时端点（`/skill/web-search*`、`/mcp/*`、`/api/web-search*`）在
//! `api::tool_runtime`，它们自己校验网关 Key，不走这里的面板鉴权。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::tool_services::{self, ToolServices, VisionConfig};

fn text_field<'a>(object: &'a serde_json::Map<String, Value>, key: &str) -> &'a str {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
}

fn parse_object(body: &Bytes) -> Result<serde_json::Map<String, Value>, (StatusCode, Json<Value>)> {
    let payload: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(body).map_err(|error| {
            error_response(&format!("请求体解析失败: {error}"), StatusCode::BAD_REQUEST)
        })?
    };
    payload
        .as_object()
        .cloned()
        .ok_or_else(|| error_response("请求体必须是 JSON 对象", StatusCode::BAD_REQUEST))
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

fn services(state: &AppState) -> &ToolServices {
    state.tool_services.as_ref()
}

fn plain_key(state: &AppState, key_id: &str) -> Result<String, (StatusCode, Json<Value>)> {
    state
        .api_key_store
        .list()
        .into_iter()
        .find(|entry| entry.id == key_id && tool_services::is_usable_key(entry))
        .map(|entry| entry.key)
        .ok_or_else(|| {
            error_response(
                "所选 API Key 不存在、已停用或不可用",
                StatusCode::BAD_REQUEST,
            )
        })
}

fn client_ids(object: &serde_json::Map<String, Value>) -> Vec<String> {
    object
        .get("clients")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn outcomes_json(outcomes: Vec<tool_services::InstallOutcome>) -> Value {
    json!({ "results": outcomes.iter().map(tool_services::InstallOutcome::to_json).collect::<Vec<_>>() })
}

/* ─── 服务 × 协议 ─────────────────────────────────────────── */

/// `GET /api/tools/servers?protocol=skill|mcp`
pub async fn get_servers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let protocol = query.get("protocol").map(String::as_str);
    if let Some(protocol) = protocol {
        if !tool_services::is_protocol(protocol) {
            return Err(error_response(
                "未知的工具服务协议",
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    Ok(Json(Value::Array(tool_services::list_services(
        services(&state),
        protocol,
    )))
    .into_response())
}

/// `PATCH /api/tools/servers/{id}` `{protocol, enabled?, timeout_seconds?}`
pub async fn patch_server(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !tool_services::is_service(&id) {
        return Err(error_response("未知的工具服务", StatusCode::BAD_REQUEST));
    }
    let object = parse_object(&body)?;
    let protocol = text_field(&object, "protocol");
    if !tool_services::is_protocol(protocol) {
        return Err(error_response(
            "protocol 必须是 skill / mcp 之一",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !tool_services::service_protocols(&id).contains(&protocol) {
        return Err(error_response(
            &format!("服务 {id} 不支持 {protocol} 协议"),
            StatusCode::BAD_REQUEST,
        ));
    }
    let enabled = object.get("enabled").and_then(Value::as_bool);
    let timeout = object.get("timeout_seconds").and_then(Value::as_u64);
    if enabled.is_none() && timeout.is_none() {
        return Err(error_response(
            "需要 enabled 或 timeout_seconds",
            StatusCode::BAD_REQUEST,
        ));
    }
    if let Some(seconds) = timeout {
        if !(tool_services::TIMEOUT_MIN_SECONDS..=tool_services::TIMEOUT_MAX_SECONDS)
            .contains(&seconds)
        {
            return Err(error_response(
                &format!(
                    "timeout_seconds 必须是 {}~{} 之间的整数",
                    tool_services::TIMEOUT_MIN_SECONDS,
                    tool_services::TIMEOUT_MAX_SECONDS
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    if let Some(enabled) = enabled {
        tool_services::set_enabled(services(&state), &id, protocol, enabled)
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    }
    if let Some(seconds) = timeout {
        tool_services::set_timeout(services(&state), &id, seconds)
            .map_err(|error| error_response(&error, StatusCode::BAD_REQUEST))?;
    }
    Ok(Json(tool_services::service_info(services(&state), &id, protocol)).into_response())
}

/* ─── 搜索提供商 ───────────────────────────────────────────── */

/// `GET /api/tools/providers`
pub async fn get_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(
        Json(tool_services::list_providers(services(&state), &state.account_sessions).await)
            .into_response(),
    )
}

/// `GET /api/tools/provider-selection`
pub async fn get_selection(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(tool_services::provider_selection(services(&state))).into_response())
}

/// `PUT /api/tools/provider-selection` `{search_providers?, fetch_provider?}`
pub async fn put_selection(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let search = object.get("search_providers").map(|value| {
        value
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let fetch = object
        .get("fetch_provider")
        .and_then(Value::as_str)
        .map(str::to_string);
    match tool_services::update_selection(services(&state), search, fetch) {
        Ok(value) => Ok(Json(value).into_response()),
        Err(error) => Err(error_response(&error, StatusCode::BAD_REQUEST)),
    }
}

/// `PUT /api/tools/providers/{provider}` `{api_key?, account_id?, enabled?}`
pub async fn put_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !tool_services::is_search_provider(&provider) {
        return Err(error_response("未知的搜索提供商", StatusCode::BAD_REQUEST));
    }
    let object = parse_object(&body)?;
    let api_key = match object.get("api_key") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => {
            return Err(error_response(
                "api_key 必须是字符串",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let enabled = match object.get("enabled") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(value)) => Some(*value),
        Some(_) => {
            return Err(error_response(
                "enabled 必须是布尔值",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let account_id = match object.get("account_id") {
        None => None,
        Some(Value::Null) => Some(""),
        Some(Value::String(value)) => Some(value.as_str()),
        Some(_) => {
            return Err(error_response(
                "account_id 必须是字符串或 null",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    match tool_services::update_provider(
        services(&state),
        &state.account_sessions,
        &provider,
        api_key,
        enabled,
        account_id,
    ) {
        Ok(value) => Ok(Json(value).into_response()),
        Err(error) => Err(error_response(&error, StatusCode::BAD_REQUEST)),
    }
}

/// `DELETE /api/tools/providers/{provider}/key`
pub async fn delete_provider_key(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !tool_services::is_search_provider(&provider) {
        return Err(error_response("未知的搜索提供商", StatusCode::BAD_REQUEST));
    }
    if !tool_services::clear_provider_key(services(&state), &provider) {
        return Err(error_response("清除密钥失败", StatusCode::NOT_FOUND));
    }
    Ok(Json(json!({ "success": true })).into_response())
}

/// `POST /api/tools/providers/{provider}/test`
pub async fn test_provider(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !tool_services::is_search_provider(&provider) {
        return Err(error_response("未知的搜索提供商", StatusCode::BAD_REQUEST));
    }
    let value = tool_services::test_provider(
        super::super::tool_runtime::tool_client_ref(),
        services(&state),
        &state.account_sessions,
        &provider,
    )
    .await;
    Ok(Json(value).into_response())
}

/* ─── 视觉分析 ─────────────────────────────────────────────── */

/// `GET /api/tools/vision/config`
pub async fn get_vision_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "config": tool_services::vision_config(services(&state), &state.api_key_store),
        "keys": tool_services::vision_key_options(&state.api_key_store),
    }))
    .into_response())
}

/// `GET /api/tools/api-keys` —— 「本机接入」下拉用的可用网关 Key（只回 id/name/掩码）
pub async fn list_api_keys(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let keys: Vec<Value> = state
        .api_key_store
        .list()
        .into_iter()
        .filter(tool_services::is_usable_key)
        .map(|entry| {
            json!({
                "id": entry.id,
                "name": entry.name,
                "masked": tool_services::mask_key(&entry.key),
            })
        })
        .collect();
    Ok(Json(json!({ "keys": keys })).into_response())
}

/// `PUT /api/tools/vision/config` `{provider, provider_label?, upstream_model, downstream_model?, reasoning_effort?, api_key_id}`
pub async fn put_vision_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let reasoning = {
        let raw = text_field(&object, "reasoning_effort");
        if raw.is_empty() {
            text_field(&object, "reasoning").to_string()
        } else {
            raw.to_string()
        }
    };
    let config = VisionConfig {
        provider: text_field(&object, "provider").to_string(),
        provider_label: text_field(&object, "provider_label").to_string(),
        upstream_model: text_field(&object, "upstream_model").to_string(),
        downstream_model: text_field(&object, "downstream_model").to_string(),
        reasoning,
        api_key_id: text_field(&object, "api_key_id").to_string(),
        api_key_name: String::new(),
    };
    match tool_services::save_vision_config(services(&state), &state.api_key_store, config) {
        Ok(value) => Ok(Json(value).into_response()),
        Err(error) => Err(error_response(&error, StatusCode::BAD_REQUEST)),
    }
}

/// `DELETE /api/tools/vision/config`
pub async fn delete_vision_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    tool_services::clear_vision_config(services(&state));
    Ok(Json(json!({ "success": true })).into_response())
}

/// `POST /api/tools/vision/config/test`
pub async fn test_vision(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let value = tool_services::test_vision(
        services(&state),
        &state.api_key_store,
        super::super::tool_runtime::tool_client_ref(),
        &base_url(&headers),
    )
    .await;
    Ok(Json(value).into_response())
}

/* ─── 本机接入 ─────────────────────────────────────────────── */

/// `GET /api/tools/install-targets?service=web-search&protocol=skill|mcp`
pub async fn install_targets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let service = query.get("service").map(String::as_str).unwrap_or("");
    let protocol = query.get("protocol").map(String::as_str).unwrap_or("");
    if !tool_services::is_service(service) {
        return Err(error_response("未知的工具服务", StatusCode::BAD_REQUEST));
    }
    if !tool_services::service_protocols(service).contains(&protocol) {
        return Err(error_response(
            "该服务不支持这个协议",
            StatusCode::BAD_REQUEST,
        ));
    }
    let targets = match protocol {
        "skill" => tool_services::skill_targets(service),
        "mcp" => tool_services::mcp_targets(service),
        _ => Vec::new(),
    };
    Ok(Json(json!({ "targets": targets })).into_response())
}

/// `GET /api/tools/install-info?service=web-search`
pub async fn install_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let service = query.get("service").map(String::as_str).unwrap_or("");
    if !tool_services::is_service(service) {
        return Err(error_response("未知的工具服务", StatusCode::BAD_REQUEST));
    }
    let base = base_url(&headers);
    let tools: Vec<&str> = match service {
        tool_services::WEB_SEARCH_SERVICE_ID => vec!["web_search", "web_fetch"],
        tool_services::VISION_SERVICE_ID => vec!["image_analyze"],
        _ => Vec::new(),
    };
    let mut value = json!({
        "base_url": base,
        "service": service,
        "name": tool_services::package_name(service),
        "version": tool_services::version(service),
        "timeout_seconds": tool_services::timeout_seconds(services(&state), service),
        "runtime_available": service != tool_services::IMAGE_GENERATION_SERVICE_ID,
        "mcp": {
            "server_name": tool_services::package_name(service),
            "url": format!("{}{}", base.trim_end_matches('/'), tool_services::endpoint(service, "mcp")),
            "tools": tools,
            "bridge_url": match service {
                tool_services::VISION_SERVICE_ID => format!("{}/api/tools/mcp-bridge/vision.js", base.trim_end_matches('/')),
                _ => String::new(),
            },
        },
    });
    if service == tool_services::WEB_SEARCH_SERVICE_ID {
        value["skill"] = json!({
            "name": tool_services::WEB_SEARCH_SKILL_NAME,
            "version": tool_services::WEB_SEARCH_SKILL_VERSION,
            "recommended_variant": tool_services::recommended_skill_variant(),
            "variants": tool_services::skill_variants(),
        });
    }
    Ok(Json(value).into_response())
}

/// `POST /api/tools/install-skill` `{api_key_id, variant, clients}`
pub async fn install_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let variant = text_field(&object, "variant");
    if !tool_services::is_skill_variant(variant) {
        return Err(error_response(
            "Skill 运行时变体无效",
            StatusCode::BAD_REQUEST,
        ));
    }
    let key = plain_key(&state, text_field(&object, "api_key_id"))?;
    let clients = client_ids(&object);
    if clients.is_empty() {
        return Err(error_response(
            "请至少选择一个客户端",
            StatusCode::BAD_REQUEST,
        ));
    }
    let base = base_url(&headers);
    let variant = variant.to_string();
    let outcomes = tokio::task::spawn_blocking(move || {
        tool_services::install_skill(
            tool_services::WEB_SEARCH_SERVICE_ID,
            &variant,
            &base,
            &key,
            &clients,
        )
    })
    .await
    .map_err(|error| {
        error_response(
            &format!("安装任务执行失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(outcomes_json(outcomes)).into_response())
}

/// `POST /api/tools/install-mcp` `{service_id, mode?, api_key_id, clients}`
pub async fn install_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let service = text_field(&object, "service_id").to_string();
    if !tool_services::is_service(&service) {
        return Err(error_response("未知的工具服务", StatusCode::BAD_REQUEST));
    }
    let mode = {
        let raw = text_field(&object, "mode");
        if raw.is_empty() {
            "http".to_string()
        } else {
            raw.to_string()
        }
    };
    if mode != "http" && mode != "bridge" {
        return Err(error_response(
            "接入方式必须是 http 或 bridge",
            StatusCode::BAD_REQUEST,
        ));
    }
    if mode == "bridge" && service != tool_services::VISION_SERVICE_ID {
        return Err(error_response(
            "该服务不支持本地桥接",
            StatusCode::BAD_REQUEST,
        ));
    }
    let key = plain_key(&state, text_field(&object, "api_key_id"))?;
    let clients = client_ids(&object);
    if clients.is_empty() {
        return Err(error_response(
            "请至少选择一个客户端",
            StatusCode::BAD_REQUEST,
        ));
    }
    let base = base_url(&headers);
    let services = state.tool_services.clone();
    let outcomes = tokio::task::spawn_blocking(move || {
        tool_services::install_mcp(&services, &service, &mode, &base, &key, &clients)
    })
    .await
    .map_err(|error| {
        error_response(
            &format!("安装任务执行失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(outcomes_json(outcomes)).into_response())
}

/// `POST /api/tools/uninstall-skill` `{service_id, clients}`
pub async fn uninstall_skill(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let clients = client_ids(&object);
    if clients.is_empty() {
        return Err(error_response(
            "请至少选择一个客户端",
            StatusCode::BAD_REQUEST,
        ));
    }
    let outcomes = tokio::task::spawn_blocking(move || {
        tool_services::uninstall_skill(tool_services::WEB_SEARCH_SERVICE_ID, &clients)
    })
    .await
    .map_err(|error| {
        error_response(
            &format!("卸载任务执行失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(outcomes_json(outcomes)).into_response())
}

/// `POST /api/tools/uninstall-mcp` `{service_id, clients}`
pub async fn uninstall_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let object = parse_object(&body)?;
    let service = text_field(&object, "service_id");
    if !tool_services::is_service(service) {
        return Err(error_response("未知的工具服务", StatusCode::BAD_REQUEST));
    }
    let clients = client_ids(&object);
    if clients.is_empty() {
        return Err(error_response(
            "请至少选择一个客户端",
            StatusCode::BAD_REQUEST,
        ));
    }
    let service = service.to_string();
    let outcomes =
        tokio::task::spawn_blocking(move || tool_services::uninstall_mcp(&service, &clients))
            .await
            .map_err(|error| {
                error_response(
                    &format!("卸载任务执行失败：{error}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
    Ok(Json(outcomes_json(outcomes)).into_response())
}

/// `GET /api/tools/mcp-bridge/vision.js` —— 视觉本地桥接脚本（文本）
pub async fn vision_bridge_script(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    // 脚本本身不含密钥（密钥由安装时经环境变量注入），但统一挂管理员鉴权口径，
    // 避免匿名枚举出本部署安装了哪些工具服务。
    verify_admin(&state, &headers).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        tool_services::VISION_BRIDGE_SCRIPT,
    )
        .into_response())
}

/// 工具服务管理面路由。
pub fn router() -> axum::Router<AppState> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/api/tools/servers", get(get_servers))
        .route("/api/tools/servers/:id", axum::routing::patch(patch_server))
        .route("/api/tools/providers", get(get_providers))
        .route("/api/tools/api-keys", get(list_api_keys))
        .route(
            "/api/tools/provider-selection",
            get(get_selection).put(put_selection),
        )
        .route("/api/tools/providers/:provider", put(put_provider))
        .route(
            "/api/tools/providers/:provider/key",
            delete(delete_provider_key),
        )
        .route("/api/tools/providers/:provider/test", post(test_provider))
        .route(
            "/api/tools/vision/config",
            get(get_vision_config)
                .put(put_vision_config)
                .delete(delete_vision_config),
        )
        .route("/api/tools/vision/config/test", post(test_vision))
        .route("/api/tools/install-targets", get(install_targets))
        .route("/api/tools/install-info", get(install_info))
        .route("/api/tools/install-skill", post(install_skill))
        .route("/api/tools/install-mcp", post(install_mcp))
        .route("/api/tools/uninstall-skill", post(uninstall_skill))
        .route("/api/tools/uninstall-mcp", post(uninstall_mcp))
        .route("/api/tools/mcp-bridge/vision.js", get(vision_bridge_script))
}
