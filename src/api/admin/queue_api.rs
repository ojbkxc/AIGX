//! 排队设置 API —— 对照 Agent2API `api/queue_api.rs`（GET/PUT /api/queue）。
//!
//! 基线把 `queueMaxWaits` / `queueWaitSeconds` 收敛成一组专用端点，前端在
//! 网关设置页直接读写。AIGX 此前只把它们放在 `/api/limits` 的通用限额体里，
//! 没有对齐的专用入口。本模块补上这条专用通道，与基线同键名（camelCase）。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use super::super::openai::AppState;
use super::common::verify_admin;

/// PUT /api/queue 请求体。两项均可选，只传一项只更新一项。
#[derive(Debug, Deserialize)]
pub struct QueueSettingsRequest {
    /// 最多等待次数（0~10，对齐基线 queueMaxWaits）
    #[serde(default, rename = "queueMaxWaits")]
    pub max_waits: Option<u8>,
    /// 单次等待秒数（0~120，对齐基线 queueWaitSeconds）
    #[serde(default, rename = "queueWaitSeconds")]
    pub wait_seconds: Option<u8>,
}

/// GET /api/queue —— 读取当前排队设置。
pub async fn handle_get_queue(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "queueMaxWaits": config.usage.queue_max_waits,
        "queueWaitSeconds": config.usage.queue_wait_seconds,
    })))
}

/// PUT /api/queue —— 更新排队设置（部分字段）。
pub async fn handle_update_queue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<QueueSettingsRequest>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    // 字段级读-改-写（P2）：并发保存不同 tab 时不再整包覆盖。
    // 两个字段都没给时闭包不改任何值 —— 落盘内容与现状相同（幂等）。
    let config = super::common::update_admin_config(&state, &headers, |config| {
        if let Some(v) = body.max_waits {
            if v > 10 {
                return Err("queueMaxWaits 必须在 0~10 之间".to_string());
            }
            config.usage.queue_max_waits = v;
        }
        if let Some(v) = body.wait_seconds {
            if v > 120 {
                return Err("queueWaitSeconds 必须在 0~120 之间".to_string());
            }
            config.usage.queue_wait_seconds = v;
        }
        Ok(())
    })
    .await?;
    Ok(Json(json!({
        "queueMaxWaits": config.usage.queue_max_waits,
        "queueWaitSeconds": config.usage.queue_wait_seconds,
    })))
}
