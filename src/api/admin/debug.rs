//! 调试模式 · 上游原始报文查看（对照 Agent2API「设置 → 网关 → 调试模式」）。
//!
//! 数据面在桥接层把**实际发往上游**的 URL / 脱敏头 / 请求体写进
//! [`crate::debug_traffic`] 的进程内环形缓冲 + FileStore 持久化；
//! 这里做开关读取/更新与报文查看/清空。
//! 报文只在开关开启时抓取（见 `config.usage.debug_traffic`）。
//!
//! `GET /api/debug/traffic?id=<请求日志 id>` 返回**单条详情**（对齐 Agent2API
//! `api/debug_api.rs`）：列表接口只给摘要，详情按需拉，避免数百 KB 的 SSE
//! 响应体把列表响应撑爆；详情在返回前再过一遍脱敏（防御纵深，见
//! [`crate::debug_traffic::detail_payload`]）。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

/// GET /api/debug —— 读取调试模式开关状态。
///
/// 对齐 Agent2API `/api/debug`（GET）；AIGX 开关同时可通过 `/api/limits`
/// 的 `debug_traffic` 字段读写，此端点提供独立入口。
pub async fn handle_get_debug(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": { "enabled": config.usage.debug_traffic }
    })))
}

/// PUT /api/debug —— 更新调试模式开关。
///
/// body: `{ "enabled": bool }`。保存即生效（桥接层逐请求读快照），
/// 关闭时顺手清空缓冲（避免旧报文在开启后突然出现）。
pub async fn handle_update_debug(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let Some(enabled) = payload.get("enabled").and_then(|v| v.as_bool()) else {
        return Err(super::common::error_response(
            "缺少 enabled 字段",
            StatusCode::BAD_REQUEST,
        ));
    };
    // 字段级读-改-写：并发保存不同 tab 时不再整包覆盖
    super::common::update_admin_config(&state, &headers, |config| {
        config.usage.debug_traffic = enabled;
        Ok(())
    })
    .await?;
    crate::config::set_debug_traffic(enabled);
    if !enabled {
        crate::debug_traffic::clear();
    }
    Ok(Json(json!({
        "success": true,
        "data": { "enabled": enabled }
    })))
}

/// GET /api/debug/traffic —— 读取最近抓到的上游原始报文。
///
/// 带 `?id=<请求日志 id>` 时返回单条详情（`data` 为 detail 对象）；
/// 不带 id 时保持原行为（`data` 为缓冲概览 + 列表）。
pub async fn handle_get_debug_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<TrafficQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let id = params.id.as_deref().map(str::trim).unwrap_or_default();
    if id.is_empty() {
        return Ok(Json(json!({
            "success": true,
            "data": crate::debug_traffic::snapshot(),
        })));
    }
    match crate::debug_traffic::get(id) {
        Some(entry) => Ok(Json(json!({
            "success": true,
            "data": crate::debug_traffic::detail_payload(&entry),
        }))),
        // 404 而不是空对象：前端据此显示「该请求没有保存原始报文」，
        // 与 Agent2API 同一文案口径（调试模式当时未开启 / 已超出保留条数）。
        None => Err(error_response(
            "该请求没有保存原始报文（调试模式当时未开启，或记录已超出保留条数）",
            StatusCode::NOT_FOUND,
        )),
    }
}

/// `GET /api/debug/traffic` 的查询参数。
#[derive(Debug, Default, Deserialize)]
pub struct TrafficQuery {
    /// 请求日志 id；缺省 = 列表概览
    #[serde(default)]
    pub id: Option<String>,
}

/// DELETE /api/debug/traffic —— 清空缓冲。
pub async fn handle_clear_debug_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let cleared = crate::debug_traffic::clear();
    Ok(Json(json!({
        "success": true,
        "data": { "cleared": cleared },
    })))
}
