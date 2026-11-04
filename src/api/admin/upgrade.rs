//! 数据升级 API — 对照 Agent2API `/api/upgrade` + `/api/upgrade/run`。
//!
//! AIGX 原生 SQLite 无旧 JSON/JSONL 包袱，因此无实际迁移动作。
//! 端点保留以对齐基线 API 面，供将来导入第三方数据（auth.json 等）。
//!
//! - `GET /api/upgrade`：`{ pending: false, items: [] }`（无待迁移旧数据）
//! - `POST /api/upgrade/run`：`{ outcomes: [], pending: false }`（幂等空跑）

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::json;

use super::super::openai::AppState;
use super::common::verify_admin;

/// GET /api/upgrade — 返回是否有待迁移旧数据。
///
/// AIGX 原生 SQLite，无旧 JSON/JSONL 文件，`pending` 恒为 false。
pub async fn handle_get_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "pending": false,
            "items": []
        }
    })))
}

/// POST /api/upgrade/run — 执行旧数据迁移（幂等空跑）。
///
/// AIGX 无旧数据可迁移，返回空 outcomes。端点保留以对齐基线 API 面。
pub async fn handle_run_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "outcomes": [],
            "pending": false
        }
    })))
}
