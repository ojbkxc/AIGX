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

use super::super::openai::AppState;
use super::common::{default_page, default_size, error_response, verify_admin, verify_user};

// 这里我们实际上需要引用主 crate 的 log_store
// 由于子模块内 super 跳到了 api::admin，需要主级引用

/// 把请求日志条目序列化为 JSON，并附加 user_email（user_id → 邮箱解析）。
///
/// 前端用户列展示邮箱而非 UUID；解析失败（用户已删）回退原 user_id。
fn logs_with_user_email(state: &AppState, logs: Vec<serde_json::Value>) -> Vec<Value> {
    logs.into_iter()
        .map(|mut l| {
            if let Some(obj) = l.as_object_mut() {
                if let Some(uid) = obj.get("user_id").and_then(|v| v.as_str()) {
                    if let Some(u) = state.user_store.get_by_id(uid) {
                        obj.insert("user_email".into(), Value::String(u.email));
                    }
                }
            }
            l
        })
        .collect()
}

/// 审计日志附加 admin_email（admin_id → 邮箱解析，"admin" 等旧值原样保留）。
fn audits_with_admin_email(state: &AppState, logs: Vec<serde_json::Value>) -> Vec<Value> {
    logs.into_iter()
        .map(|mut l| {
            if let Some(obj) = l.as_object_mut() {
                if let Some(aid) = obj.get("admin_id").and_then(|v| v.as_str()) {
                    if let Some(u) = state.user_store.get_by_id(aid) {
                        obj.insert("admin_email".into(), Value::String(u.email));
                    }
                }
            }
            l
        })
        .collect()
}

/// 请求日志查询参数
#[derive(Debug, Deserialize)]
pub struct RequestLogQuery {
    /// 用户过滤：支持 UUID 或邮箱（邮箱自动解析为对应 user_id）
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

/// 列出请求日志（全员可见：管理员看全部，普通用户只看自己的）
///
/// 普通用户自动按 `user_id` 过滤，忽略 URL 中的 `user` 参数（安全）。
/// 管理员保持原行为，可按任意 user/model/channel 筛选。
pub async fn handle_list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RequestLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 尝试管理员验证；失败则回退到普通用户
    let admin = verify_admin(&state, &headers).await.is_ok();
    // 管理员按 user 参数过滤（UUID 或邮箱，邮箱解析为 user_id）
    let resolved_filter: Option<String>;
    let user_email: Option<String>;
    let (filter_user, filter_channel) = if admin {
        resolved_filter = q.user.as_deref().and_then(|u| {
            // 含 '@' 视为邮箱 → 解析 user_id；否则原样按 UUID 过滤
            if u.contains('@') {
                state.user_store.get_by_email(u).map(|usr| usr.id)
            } else {
                Some(u.to_string())
            }
        });
        (resolved_filter.as_deref(), q.channel.as_deref())
    } else {
        // 普通用户：必须登录，且只能看自己的
        let user = verify_user(&state, &headers).await?;
        user_email = Some(user.id);
        (user_email.as_deref(), None)
    };
    let (logs, total) = state.log_store.requests.list_with_filter(
        filter_user,
        q.model.as_deref(),
        filter_channel,
        q.start,
        q.end,
        q.page,
        q.size,
    );
    // user_id → user_email 解析（前端用户列展示邮箱）
    let data = logs_with_user_email(
        &state,
        logs.into_iter()
            .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
            .filter(|v| !v.is_null())
            .collect(),
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

/// 列出审计日志（仅管理员，审计日志保持 admin only）
pub async fn handle_list_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state.log_store.audits.list_paged(q.page, q.size);
    // admin_id → admin_email 解析（前端管理员列展示邮箱）
    let data = audits_with_admin_email(
        &state,
        logs.into_iter()
            .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
            .filter(|v| !v.is_null())
            .collect(),
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

/// 导出请求日志（全员可见：管理员导出全部，普通用户导出自己的）
///
/// 权限与 `handle_list_request_logs` 对齐：管理员看全部，普通用户按 user_id 过滤。
pub async fn handle_export_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> Response {
    // 尝试管理员验证；失败则要求普通用户登录并按 user_id 过滤
    let admin = verify_admin(&state, &headers).await.is_ok();
    let user_email: Option<String> = if admin {
        None
    } else {
        match verify_user(&state, &headers).await {
            Ok(u) => Some(u.email),
            Err(e) => return e.into_response(),
        }
    };
    let fmt = q.format.as_deref().unwrap_or("json").to_lowercase();

    let result = match fmt.as_str() {
        "csv" => {
            let csv = match &user_email {
                Some(email) => state.log_store.requests.export_csv_for_user(email),
                None => state.log_store.requests.export_csv(),
            };
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        "attachment; filename=\\\"request_logs.csv\\\"".to_string(),
                    ),
                ],
                csv,
            )
        }
        "json" => {
            let json = match &user_email {
                Some(email) => state.log_store.requests.export_json_for_user(email),
                None => state.log_store.requests.export_json(),
            };
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/json; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        "attachment; filename=\\\"request_logs.json\\\"".to_string(),
                    ),
                ],
                json,
            )
        }
        _ => {
            return error_response(
                "Unsupported format, use 'csv' or 'json'",
                StatusCode::BAD_REQUEST,
            )
            .into_response();
        }
    };

    axum::response::IntoResponse::into_response(result)
}

/// 批量删除请求日志（仅管理员）
///
/// Body: `{ "ids": ["id1", "id2", ...] }`；返回 `{ success, removed }`。
pub async fn handle_delete_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeleteLogsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.requests.delete_many(&body.ids);
    Ok(Json(serde_json::json!({ "success": true, "removed": removed })))
}

/// 清空全部请求日志（仅管理员）
pub async fn handle_clear_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state
        .log_store
        .requests
        .clear_all()
        .map_err(|e| error_response(&format!("Failed to clear: {e}"), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::json!({ "success": true, "removed": removed })))
}

/// 批量删除审计日志（仅管理员）
pub async fn handle_delete_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeleteLogsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.audits.delete_many(&body.ids);
    Ok(Json(serde_json::json!({ "success": true, "removed": removed })))
}

/// 清空全部审计日志（仅管理员）
pub async fn handle_clear_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state
        .log_store
        .audits
        .clear_all()
        .map_err(|e| error_response(&format!("Failed to clear: {e}"), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(serde_json::json!({ "success": true, "removed": removed })))
}

/// 批量删除日志的请求体
#[derive(Debug, Deserialize)]
pub struct DeleteLogsBody {
    #[serde(default)]
    pub ids: Vec<String>,
}
