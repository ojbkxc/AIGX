// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 维护任务管理面（`/api/maintenance*`）。
//!
//!   GET  /api/maintenance                        总览：两条维护任务 + 最近一轮 + 记录列表
//!   GET  /api/maintenance/records/{id}           单轮完整明细
//!   POST /api/maintenance/run/{task}             立即执行（credentialMaintenance / modelRefresh）
//!   POST /api/maintenance/records/{id}/retry     重试一轮（可选 `{itemId}` 只重试一条失败项）
//!
//! 全部管理员鉴权。执行记录落在 `maintenance::storage`；排期由
//! `scheduled_tasks::task_state` 管，自动循环与手动执行共用同一租约。

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::maintenance::{self, storage, types};

fn safe_error(status: StatusCode, message: &str) -> (StatusCode, Json<Value>) {
    error_response(&types::safe_message(message), status)
}

/// `GET /api/maintenance`
pub async fn overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<types::Query>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let records = storage::list(&state.maintenance.store, &query)
        .map_err(|error| safe_error(StatusCode::INTERNAL_SERVER_ERROR, &error))?;
    let latest = storage::latest(&state.maintenance.store)
        .map_err(|error| safe_error(StatusCode::INTERNAL_SERVER_ERROR, &error))?;
    let mut tasks = Vec::new();
    for task_id in [maintenance::CREDENTIAL, maintenance::MODEL] {
        tasks.push(crate::scheduled_tasks::task_by_id(&state, task_id).await);
    }
    Ok(Json(json!({
        "tasks": tasks,
        "latest": latest,
        "records": records,
        "retentionDays": storage::RETENTION_DAYS,
        "maxRecords": storage::MAX_RECORDS,
    }))
    .into_response())
}

/// `GET /api/maintenance/records/{id}`
pub async fn record_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match storage::detail(&state.maintenance.store, &id) {
        Ok(detail) => Ok(Json(json!(detail)).into_response()),
        Err(error) => {
            let status = if error == "维护记录不存在或已清理" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            Err(safe_error(status, &error))
        }
    }
}

/// `POST /api/maintenance/run/{task}`
pub async fn run_task(
    State(state): State<AppState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !maintenance::is_maintenance_task(&task_id) {
        return Err(error_response("未知的维护任务", StatusCode::BAD_REQUEST));
    }
    let detail =
        maintenance::launch_async(state.clone(), maintenance::Request::full(&task_id, true))
            .await
            .map_err(|error| safe_error(StatusCode::BAD_REQUEST, &error))?;
    Ok(Json(json!(detail)).into_response())
}

/// `POST /api/maintenance/records/{id}/retry` `{itemId?}`
pub async fn retry_record(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let item_id = if body.is_empty() {
        None
    } else {
        let payload: Value = serde_json::from_slice(&body).map_err(|error| {
            error_response(&format!("请求体解析失败: {error}"), StatusCode::BAD_REQUEST)
        })?;
        payload
            .get("itemId")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let detail = maintenance::retry_async(state.clone(), &id, item_id.as_deref())
        .await
        .map_err(|error| safe_error(StatusCode::BAD_REQUEST, &error))?;
    Ok(Json(json!(detail)).into_response())
}

/// 维护管理面路由。
pub fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/api/maintenance", get(overview))
        .route("/api/maintenance/records/:id", get(record_detail))
        .route("/api/maintenance/run/:task", post(run_task))
        .route("/api/maintenance/records/:id/retry", post(retry_record))
}
