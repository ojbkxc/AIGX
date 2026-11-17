// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成管理面（`/api/tools/image-generation/config`）。
//!
//! ── 本波范围 ────────────────────────────────────────────────
//! 上游还有 `jobs` / `jobs/{id}` / `files/{id}` 三个运行时端点（任务表 +
//! 文件下载）。AIGX 当前没有可路由的生图账号/模型目录接口，本波只移植
//! **配置面**；运行时端点未注册（见 T7 报告的「未移植清单」）。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::tool_services::{self, ImageConfig};

fn services(state: &AppState) -> &tool_services::ToolServices {
    state.tool_services.as_ref()
}

/// `GET /api/tools/image-generation/config`
pub async fn config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(tool_services::image_config::management_view(services(
        &state,
    )))
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
    match tool_services::image_config::save(services(&state), config) {
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

/// 图片生成管理面路由（仅配置面；运行时未移植）。
pub fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new().route(
        "/api/tools/image-generation/config",
        get(config).put(save).delete(clear),
    )
}
