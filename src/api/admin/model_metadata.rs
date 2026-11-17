//! models.dev 模型元数据管理端点。
//!
//! Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 对应上游 `api/model_meta.rs`（GET/PUT + refresh）与 `api/models.rs::refresh_models`。
//!
//! - `GET/PUT /api/models/metadata`：读取状态 / 切换开关（保存即生效）
//! - `POST /api/models/metadata/refresh`：手动联网刷新目录（5 秒合并连点）
//! - `POST /api/models/refresh`：全量刷新入口（A2A 形状，返回
//!   `{models, mappings, reasoningLevels, modelsDev}`）
//!
//! 数据来源与降级策略见 [`crate::model::models_dev`]。
//!
//! ── 响应形状（T5 对齐 A2A）────────────────────────────────────
//! `GET /api/models/metadata`：
//! ```json
//! { "success": true, "data": {
//!     "source": "builtin|cache|network|user",   // 四档归一
//!     "rawSource": "embedded|cache|https://models.dev",
//!     "updatedAt": 1730000000000, "count": 412,
//!     "enabled": true,
//!     "modelsDevEnabled": true,                 // A2A 键名（与 enabled 同值）
//! }}
//! ```
//! 两个开关键并存：`enabled` 是 AIGX 旧前端读的（`ModelMetadataStatus`），
//! `modelsDevEnabled` 是 A2A 的键名（`PUT` 两种写法都收）。
//!
//! `POST /api/models/refresh`：返回 `{results, refreshed, skipped, failed,
//! models, mappings, reasoningLevels, modelsDev}` —— AIGX 没有「逐家上游目录」，
//! `results` 恒空数组；`models` 是聚合清单（A2A 的 `session_models` 形状），
//! `mappings` 是全量映射条目，`reasoningLevels` 是候选档位表，
//! `modelsDev` 是元数据状态。

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
use super::model_manage::REASONING_LEVELS;

/// PUT 请求体：`{enabled}` 与 `{modelsDevEnabled}` 都收（AIGX 旧键 / A2A 键）。
#[derive(Debug, Deserialize)]
pub struct ModelMetadataRequest {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub models_dev_enabled: Option<bool>,
}

/// 当前元数据状态（开关 + 目录来源 / 时刻 / 条数）。
fn metadata_json(enabled: bool) -> Value {
    let mut payload = crate::model::models_dev::status();
    if let Some(object) = payload.as_object_mut() {
        object.insert("enabled".to_string(), Value::Bool(enabled));
        object.insert("modelsDevEnabled".to_string(), Value::Bool(enabled));
    }
    payload
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

/// PUT /api/models/metadata —— 切换开关（`enabled` 或 `modelsDevEnabled`）。
pub async fn handle_put_model_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ModelMetadataRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(enabled) = body.enabled.or(body.models_dev_enabled) else {
        return Err(error_response(
            "缺少 enabled（或 modelsDevEnabled）字段",
            StatusCode::BAD_REQUEST,
        ));
    };
    // 字段级读-改-写（P2）：落盘成功后才热更进程级快照
    super::common::update_admin_config(&state, &headers, move |config| {
        config.usage.models_dev_enabled = enabled;
        Ok(())
    })
    .await?;
    crate::config::set_models_dev_enabled(enabled);
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

/// POST /api/models/refresh —— 手动全量刷新模型清单（A2A 形状）。
///
/// AIGX 没有「逐家上游目录」的刷新入口（各家目录在账号页各自同步），
/// 这里刷新的是 models.dev 统一目录，然后把**聚合视图的三块**一起返回：
/// - `models`：聚合清单（与 `/v1/models` 同源的 `list_item` 形状，
///   前端「刷新后就地重绘」不必再拉一次）；
/// - `mappings`：全量映射条目（含 provider / reasoning / enabled）；
/// - `reasoningLevels`：候选档位表；
/// - `modelsDev`：元数据状态（来源 / 时刻 / 条数）。
///
/// `results` 恒为空数组（AIGX 无逐家结果）；`refreshed/skipped/failed`
/// 是单次刷新的汇总计数（0/1），与 A2A 的行语义兼容。
pub async fn handle_refresh_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (refreshed, skipped, failed, count, message) = if !crate::config::models_dev_enabled() {
        (0, 0, 0, 0, Some("models.dev 元数据已关闭".to_string()))
    } else {
        let (status, message, count) =
            crate::model::models_dev::manual_refresh(&state.http_client, &state.alert_store).await;
        let (refreshed, skipped, failed) = match status.as_str() {
            "refreshed" => (1, 0, 0),
            "skipped" => (0, 1, 0),
            "disabled" => (0, 0, 0),
            _ => (0, 0, 1),
        };
        (refreshed, skipped, failed, count, message)
    };
    let enabled = crate::config::models_dev_enabled();
    // 刷新之后取清单：此时数据源已是新内容（A2A「为什么响应里带清单」同理由）
    let view = super::model_manage::current_view(&state);
    let models: Vec<Value> = view
        .models
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "name": m.id,
                "provider": m.provider,
                "providerLabel": m.provider_label,
                "owned_by": m.owned_by,
                "context_length": m.context_length,
                "capabilities": m.capabilities,
                "capabilityFlags": m.capability_flags,
                "isDefault": m.default_enabled,
                "enabled": m.enabled,
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "results": [],
            "refreshed": refreshed,
            "skipped": skipped,
            "failed": failed,
            "count": count,
            "message": message,
            "models": models,
            "mappings": view.mapping_entries,
            "dangling": view.dangling,
            "reasoningLevels": REASONING_LEVELS,
            "modelsDev": metadata_json(enabled),
        },
    })))
}
