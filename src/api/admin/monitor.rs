//! 监控系统 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供系统监控和健康检查功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

/// 监控系统收集器（进程级单例，`/api/monitor/system` 与 `/api/monitor/health` 共用）。
static SYSTEM_COLLECTOR: std::sync::OnceLock<std::sync::Arc<crate::monitor::SystemCollector>> =
    std::sync::OnceLock::new();

/// GET /api/monitor/system - 获取系统监控快照
pub async fn handle_monitor_system(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let collector = SYSTEM_COLLECTOR
        .get_or_init(|| std::sync::Arc::new(crate::monitor::SystemCollector::new()));
    let snap = collector.snapshot();
    Ok(Json(json!({ "success": true, "data": snap })))
}

/// 字节数格式化为人类可读（MB，保留一位小数）。
fn format_mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

/// GET /api/monitor/health - 健康检查
///
/// 复用 `/api/monitor/system` 的同一采集器（CPU/内存/负载/进程），
/// 返回真实 uptime 与内存占用，而非硬编码占位值。
pub async fn handle_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let collector = SYSTEM_COLLECTOR
        .get_or_init(|| std::sync::Arc::new(crate::monitor::SystemCollector::new()));
    let snap = collector.snapshot();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "status": "healthy",
            "timestamp": snap.collected_at,
            "uptime": snap.process.uptime_secs,
            "memory": format_mb(snap.memory.used),
            "memory_total": format_mb(snap.memory.total),
            "memory_usage_percent": snap.memory.usage_percent,
            "load_1m": snap.load.load_1m,
            "cpu_usage_percent": snap.cpu.usage_percent,
            "available": snap.memory.available_flag || snap.load.available_flag,
        })
    })))
}

/// GET /api/monitor/healthz
pub async fn handle_healthz(
    State(_state): State<AppState>,
    _headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    Ok(Json(json!({ "ok": true })))
}
