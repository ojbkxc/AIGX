//! 护栏管理面（`/api/guardrail`）。
//!
//! 与 `purge` / `prompt` 同级：GET 读配置 + 降级状态，PUT 全量写配置并热生效；
//! `POST /api/guardrail/degrade/clear` 手动解除降级。
//!
//! 状态机（`degrade`）是进程内运行期状态、重启清零——与参考项目一致，
//! 不落盘（见 `guardrail::degrade` 模块头）。这里只暴露「读 + 手动清」。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

use crate::guardrail::GuardrailSettings;

/// GET /api/guardrail —— 读取护栏配置与降级状态。
pub async fn handle_get_guardrail(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // 旧 bin 未接启动钩子时懒挂持久化（有存储则不改动）
    crate::guardrail::ensure_installed(state.alert_store.clone());
    Ok(Json(payload()))
}

/// PUT /api/guardrail —— 全量保存配置（热生效）。
///
/// 请求体既可以是配置对象本身，也可以是 `{ "settings": {...} }`；
/// 正则在保存时编译校验，非法直接 400（避免「存进去了但不生效」）。
pub async fn handle_update_guardrail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    crate::guardrail::ensure_installed(state.alert_store.clone());
    let raw = body
        .get("settings")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(body);
    let parsed: GuardrailSettings = serde_json::from_value(raw).map_err(|e| {
        error_response(
            &format!("Invalid guardrail settings: {e}"),
            StatusCode::BAD_REQUEST,
        )
    })?;

    crate::guardrail::update(parsed).map_err(|e| {
        error_response(
            &format!("Failed to save guardrail settings: {e}"),
            StatusCode::BAD_REQUEST,
        )
    })?;

    Ok(Json(payload()))
}

/// POST /api/guardrail/degrade/clear —— 手动解除降级（立即恢复客户端提示词）。
pub async fn handle_clear_guardrail_degrade(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    crate::guardrail::degrade::clear();
    Ok(Json(payload()))
}

fn payload() -> Value {
    let settings = crate::guardrail::current_settings();
    json!({
        "success": true,
        "data": {
            "settings": settings,
            "degrade": {
                "active": crate::guardrail::degrade::active(),
                "until_ms": crate::guardrail::degrade::until_ms(),
                "until_text": crate::guardrail::degrade::until_text(),
            }
        }
    })
}
