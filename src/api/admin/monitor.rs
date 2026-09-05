//! 监控系统 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供系统监控和健康检查功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde_json::{json, Value};

use super::common::{
    error_response,
    verify_admin,
};
use super::super::openai::AppState;

/// 监控系统收集器（未来实现）
static SYSTEM_COLLECTOR: std::sync::OnceLock<std::sync::Arc<crate::monitor::SystemCollector>> =
    std::sync::OnceLock::new();

/// GET /api/monitor/system - 获取系统监控快照
pub async fn handle_monitor_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let collector = SYSTEM_COLLECTOR.get_or_init(|| std::sync::Arc::new(crate::monitor::SystemCollector::new()));
    let snap = collector.snapshot();
    Ok(Json(json!({ "success": true, "data": snap })))
}

/// GET /api/monitor/health - 健康检查
pub async fn handle_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "status": "healthy",
            "timestamp": chrono::Utc::now().timestamp(),
            "uptime": 0, // 未来实现
            "memory": "0 MB", // 未来实现
        })
    })))
}

/// GET /api/monitor/healthz
pub async fn handle_healthz(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(json!({ "ok": true })))
}
