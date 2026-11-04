//! 调试模式 · 上游原始报文查看（对照 Agent2API「设置 → 网关 → 调试模式」）。
//!
//! 数据面在桥接层把**实际发往上游**的 URL / 脱敏头 / 请求体写进
//! [`crate::debug_traffic`] 的进程内环形缓冲 + FileStore 持久化；
//! 这里做开关读取/更新与报文查看/清空。
//! 报文只在开关开启时抓取（见 `config.usage.debug_traffic`）。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

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
    let mut config = verify_admin(&state, &headers).await?;
    let Some(enabled) = payload.get("enabled").and_then(|v| v.as_bool()) else {
        return Err(super::common::error_response(
            "缺少 enabled 字段",
            StatusCode::BAD_REQUEST,
        ));
    };
    config.usage.debug_traffic = enabled;
    crate::config::set_debug_traffic(enabled);
    if !enabled {
        crate::debug_traffic::clear();
    }
    state.config_manager.update(config).await.map_err(|e| {
        super::common::error_response(
            &format!("调试模式更新失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "success": true,
        "data": { "enabled": enabled }
    })))
}

/// GET /api/debug/traffic —— 读取最近抓到的上游原始报文。
pub async fn handle_get_debug_traffic(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": crate::debug_traffic::snapshot(),
    })))
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
