//! 缓存管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供缓存统计信息查询和清空功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

/// GET /api/cache/stats - 获取缓存统计信息
pub async fn handle_cache_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let entry_count = state.response_cache.entry_count();
    Ok(Json(json!({
        "success": true,
        "data": {
            "entry_count": entry_count,
            "max_capacity": 1000,
            "ttl_secs": 300,
            "description": "response_cache (exact-match prompt cache)"
        }
    })))
}

/// POST /api/cache/clear - 清空缓存
pub async fn handle_cache_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    state.response_cache.invalidate_all();
    Ok(Json(json!({
        "success": true,
        "data": { "message": "Cache cleared" }
    })))
}

/// GET /api/cache/info - 获取缓存详细信息（未来实现）
pub async fn handle_cache_info(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 未来实现：返回详细的缓存键和 TTL 信息
    Ok(Json(json!({
        "success": true,
        "data": {
            "info": "Cache information endpoint (to be implemented)"
        }
    })))
}
