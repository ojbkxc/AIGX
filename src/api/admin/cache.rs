//! 缓存管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供缓存统计信息查询和清空功能。
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

/// GET /api/cache/stats - 获取缓存统计信息
pub async fn handle_cache_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let entry_count = state.response_cache.entry_count();
    let hit_rate = state.response_cache.hit_rate();
    Ok(Json(json!({
        "success": true,
        "data": {
            // 前端读 entries / hit_rate / memory_bytes：三个都返回，
            // 同时保留 entry_count 等旧字段兼容。
            "entries": entry_count,
            "entry_count": entry_count,
            "hit_rate": (hit_rate * 100.0 * 10.0).round() / 10.0,
            // 内存占用无法精确统计（dashmap 无此 API）：返回 0 由前端显示「—」，
            // 不编一个假数字。
            "memory_bytes": 0,
            "max_capacity": state.response_cache.max_capacity(),
            "ttl_secs": state.response_cache.ttl_secs(),
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
