//! models.dev 模型元数据管理端点（对照 Agent2API `api/model_meta.rs`）。
//!
//! - `GET/PUT /api/models/metadata`：读取状态 / 切换开关（保存即生效）
//! - `POST /api/models/metadata/refresh`：手动联网刷新目录（5 秒合并连点）
//!
//! 数据来源与降级策略见 [`crate::model::models_dev`]。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::error_response;
use super::common::verify_admin;

/// PUT 请求体：只带 enabled 一项（缺字段回 400，对齐上游严格口径）。
#[derive(Debug, Deserialize)]
pub struct ModelMetadataRequest {
    pub enabled: Option<bool>,
}

fn metadata_json(enabled: bool) -> Value {
    let (updated_at, count, source) = crate::model::models_dev::stats();
    json!({
        "enabled": enabled,
        "updatedAt": updated_at,
        "count": count,
        "source": source,
    })
}

/// GET /api/models/metadata —— 开关状态 + 已装载目录信息。
pub async fn handle_get_model_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": metadata_json(config.usage.models_dev_enabled),
    })))
}

/// PUT /api/models/metadata —— `{enabled: bool}` 切换开关。
pub async fn handle_put_model_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ModelMetadataRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let Some(enabled) = body.enabled else {
        return Err(error_response("缺少 enabled 字段", StatusCode::BAD_REQUEST));
    };
    let mut config = state.config_manager.get().await;
    config.usage.models_dev_enabled = enabled;
    // 快照先行：即便落盘失败，本次运行内立即生效（下一轮刷新/补值都读它）
    crate::config::set_models_dev_enabled(enabled);
    if let Err(e) = state.config_manager.update(config).await {
        return Err(error_response(
            &format!("保存失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(Json(json!({
        "success": true,
        "data": metadata_json(enabled),
    })))
}

/// POST /api/models/metadata/refresh —— 手动联网刷新目录。
///
/// 恒 2xx：结果放在 `data.status`（refreshed / disabled / skipped / failed），
/// 对齐上游「刷新是运维动作，失败不是管理端错误」的口径。
pub async fn handle_refresh_model_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if !crate::config::models_dev_enabled() {
        return Ok(Json(json!({
            "success": true,
            "data": { "status": "disabled", "message": "models.dev 元数据已关闭" },
        })));
    }
    let (status, message, count) =
        crate::model::models_dev::manual_refresh(&state.http_client, &state.alert_store).await;
    let enabled = crate::config::models_dev_enabled();
    Ok(Json(json!({
        "success": true,
        "data": {
            "status": status,
            "message": message,
            "count": count,
            "metadata": metadata_json(enabled),
        },
    })))
}

/// POST /api/models/refresh —— 手动全量刷新模型清单（对照 Agent2API）。
///
/// 基线这里是「逐家刷新各自目录 + 返回聚合清单」的入口。AIGX 走 models.dev
/// 统一目录，刷新动作与 `/api/models/metadata/refresh` 同源（同一份
/// `manual_refresh`），但返回基线的汇总形状 `{results, refreshed, skipped, failed}`，
/// 供前端按基线契约对接。`results` 为空数组（AIGX 无逐家结果，目录刷新是
/// 单次 models.dev 拉取）。
pub async fn handle_refresh_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if !crate::config::models_dev_enabled() {
        return Ok(Json(json!({
            "success": true,
            "data": {
                "results": [],
                "refreshed": 0,
                "skipped": 0,
                "failed": 0,
                "message": "models.dev 元数据已关闭",
            },
        })));
    }
    let (status, message, count) =
        crate::model::models_dev::manual_refresh(&state.http_client, &state.alert_store).await;
    let (refreshed, skipped, failed) = match status.as_str() {
        "refreshed" => (1, 0, 0),
        "skipped" => (0, 1, 0),
        "disabled" => (0, 0, 0),
        _ => (0, 0, 1),
    };
    let enabled = crate::config::models_dev_enabled();
    Ok(Json(json!({
        "success": true,
        "data": {
            "results": [],
            "refreshed": refreshed,
            "skipped": skipped,
            "failed": failed,
            "count": count,
            "message": message,
            "metadata": metadata_json(enabled),
        },
    })))
}
