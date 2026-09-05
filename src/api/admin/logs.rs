//! 日志 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供请求日志、审计日志的查询和导出功能。
//!
//! ## 路径说明
//!
//! - 使用 `super::super::common` 访问共享认证逻辑

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::Value;

use super::common::{error_response, verify_admin, default_page, default_size};
use super::super::openai::AppState;

// 这里我们实际上需要引用主 crate 的 log_store
// 由于子模块内 super 跳到了 api::admin，需要主级引用


/// 请求日志查询参数
#[derive(Debug, Deserialize)]
pub struct RequestLogQuery {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

/// 审计日志查询参数
#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

/// 导出格式参数
#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
}

/// 列出请求日志
pub async fn handle_list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RequestLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state.log_store.requests.list_with_filter(
        q.user.as_deref(),
        q.model.as_deref(),
        q.channel.as_deref(),
        q.start,
        q.end,
        q.page,
        q.size,
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": logs,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

/// 列出审计日志
pub async fn handle_list_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state.log_store.audits.list_paged(q.page, q.size);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": logs,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

/// 导出请求日志
pub async fn handle_export_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> Response {
    if verify_admin(&state, &headers).await.is_err() {
        return error_response("Not authenticated", StatusCode::UNAUTHORIZED)
            .into_response();
    }
    let fmt = q.format.as_deref().unwrap_or("json").to_lowercase();

    let result = match fmt.as_str() {
        "csv" => {
            let csv = state.log_store.requests.export_csv();
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
                    (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\\\"request_logs.csv\\\"".to_string()),
                ],
                csv,
            )
        }
        "json" => {
            let json = state.log_store.requests.export_json();
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "application/json; charset=utf-8".to_string()),
                    (axum::http::header::CONTENT_DISPOSITION, "attachment; filename=\\\"request_logs.json\\\"".to_string()),
                ],
                json,
            )
        }
        _ => {
            return error_response("Unsupported format, use 'csv' or 'json'", StatusCode::BAD_REQUEST)
                .into_response();
        }
    };

    axum::response::IntoResponse::into_response(result)
}
