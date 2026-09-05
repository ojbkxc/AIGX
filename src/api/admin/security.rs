//! 安全管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供安全监控和事件查询功能。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use super::super::openai::AppState;

#[derive(Debug, Deserialize)]
pub struct SecurityEventsQuery {
    pub r#type: Option<String>,
    pub range: Option<String>,
    pub page: Option<usize>,
    pub page_size: Option<usize>,
}

/// GET /api/monitor/security - 安全汇总
pub async fn handle_security_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let events = state.log_store.security.list_all();
    let total = events.len().max(1) as u64;
    let critical = events.iter().filter(|e| e.severity == "critical").count() as u64;
    Ok(Json(json!({
        "success": true,
        "data": {
            "score": 100 - (critical as f64 / total as f64 * 100.0).round() as u8,
            "total_events": total,
            "critical_events": critical,
            "recent_24h": 0,
        }
    })))
}

/// GET /api/monitor/security/events - 安全事件列表
pub async fn handle_security_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SecurityEventsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let page = params.page.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(20).clamp(1, 100);
    let start = params.range.as_deref().and_then(|r| {
        let secs = match r {
            "1h" => 3600,
            "24h" => 24 * 3600,
            "7d" => 7 * 24 * 3600,
            "30d" => 30 * 24 * 3600,
            _ => return None,
        };
        Some(chrono::Utc::now().timestamp() - secs)
    });
    let (events, total) = state.log_store.security.list_paged(
        params.r#type.as_deref(),
        start,
        None,
        page as usize,
        page_size as usize,
    );
    let data: Vec<Value> = events
        .into_iter()
        .map(|ev| json!({
            "id": ev.id,
            "created_at": ev.created_at,
            "event_type": ev.event_type.as_str(),
            "severity": ev.severity,
            "ip": ev.ip,
            "user_id": ev.user_id,
            "detail": ev.detail,
        }))
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "events": data,
            "total": total,
            "page": page,
            "page_size": page_size
        }
    })))
}

/// POST /api/security/reset
pub async fn handle_reset_security(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 未来实现：重置安全日志
    Ok(Json(json!({ "success": true, "data": null })))
}