// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成管理面（`/api/tools/image-generation/*`）。
//!
//! 端点（与上游 `api/tool_image_admin.rs` 对齐；管理面全部管理员鉴权）：
//!   · `GET    /api/tools/image-generation/config`  配置 + 可选模型/账号目录
//!   · `PUT    /api/tools/image-generation/config`  保存（形状 + 目录语义校验）
//!   · `DELETE /api/tools/image-generation/config`  清除
//!   · `GET    /api/tools/image-generation/jobs`    最近 50 个任务 + 存储用量
//!   · `DELETE /api/tools/image-generation/jobs/{id}` 清理任务（生成中拒绝）
//!   · `GET    /api/tools/image-generation/files/{id}` 下载生成图片（管理员）
//!   · `GET    /api/tools/mcp-bridge/image-generation.cjs` 本地桥接脚本
//!
//! 数据面（MCP）在 `api::tool_runtime`；任务存储见 `tool_services::image_jobs`。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::tool_services::{self, ImageConfig};

fn services(state: &AppState) -> &tool_services::ToolServices {
    state.tool_services.as_ref()
}

/// 网关基址（管理面结果里的下载 URL 前缀）。
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

fn tool_error(error: tool_services::image_jobs::ImageToolError) -> (StatusCode, Json<Value>) {
    tool_services::image_runtime::admin_error(error)
}

/// `GET /api/tools/image-generation/config`
pub async fn config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let catalog = tool_services::image_config::catalog(&state);
    Ok(Json(tool_services::image_config::management_view(
        services(&state),
        &catalog,
    ))
    .into_response())
}

/// `PUT /api/tools/image-generation/config`
pub async fn save(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let config = serde_json::from_slice::<ImageConfig>(&body).map_err(|error| {
        error_response(&format!("图片配置无效：{error}"), StatusCode::BAD_REQUEST)
    })?;
    let catalog = tool_services::image_config::catalog(&state);
    match tool_services::image_config::save(services(&state), &catalog, config) {
        Ok(value) => Ok(Json(value).into_response()),
        Err(error) => Err(error_response(&error, StatusCode::BAD_REQUEST)),
    }
}

/// `DELETE /api/tools/image-generation/config`
pub async fn clear(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match tool_services::image_config::clear(services(&state)) {
        Ok(()) => Ok(Json(json!({ "success": true })).into_response()),
        Err(error) => Err(error_response(&error, StatusCode::INTERNAL_SERVER_ERROR)),
    }
}

/// `GET /api/tools/image-generation/jobs`
pub async fn jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let base = base_url(&headers);
    let store = tool_services::image_runtime::jobs_for(&state);
    let rows = store
        .list()
        .map_err(tool_error)?
        .iter()
        .map(|job| tool_services::image_runtime::job_result(job, &base, true))
        .collect::<Vec<_>>();
    let storage = store.storage_usage().map_err(tool_error)?;
    Ok(Json(json!({ "jobs": rows, "storage": storage })).into_response())
}

/// `DELETE /api/tools/image-generation/jobs/{id}`
pub async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let store = tool_services::image_runtime::jobs_for(&state);
    store.delete(&id).map_err(tool_error)?;
    Ok(Json(json!({ "success": true })).into_response())
}

/// `GET /api/tools/image-generation/files/{id}`
pub async fn download(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let store = tool_services::image_runtime::jobs_for(&state);
    let (_, file) = store.read_file(&id).map_err(tool_error)?;
    Ok(tool_services::image_runtime::file_response(file))
}

/// `GET /api/tools/mcp-bridge/image-generation.cjs` —— 本地桥接脚本（文本）。
///
/// 脚本本身不含密钥（密钥由安装时经环境变量注入），但统一挂管理员鉴权口径，
/// 避免匿名枚举出本部署安装了哪些工具服务。
pub async fn bridge_script(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok((
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        tool_services::IMAGE_BRIDGE_SCRIPT,
    )
        .into_response())
}

/// 图片生成管理面路由（main.rs 已合并本 router，新增路由无需再接线）。
pub fn router() -> axum::Router<AppState> {
    use axum::routing::{delete as delete_route, get};
    axum::Router::new()
        .route(
            "/api/tools/image-generation/config",
            get(config).put(save).delete(clear),
        )
        .route("/api/tools/image-generation/jobs", get(jobs))
        .route("/api/tools/image-generation/jobs/:id", delete_route(delete))
        .route("/api/tools/image-generation/files/:id", get(download))
        .route(
            "/api/tools/mcp-bridge/image-generation.cjs",
            get(bridge_script),
        )
}
