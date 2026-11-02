//! 调试模式 · 上游原始报文查看（对照 Agent2API「设置 → 网关 → 调试模式」）。
//!
//! 数据面在桥接层把**实际发往上游**的 URL / 脱敏头 / 请求体写进
//! [`crate::debug_traffic`] 的进程内环形缓冲；这里只做读取与清空。
//! 报文只在开关开启时抓取（见 `config.usage.debug_traffic`）。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

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
