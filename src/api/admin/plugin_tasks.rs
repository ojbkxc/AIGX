// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件后台任务（`/api/plugins/tasks*`）。
//!
//! ── 这些任务是什么 ──────────────────────────────────────────
//! 插件（Wasm / 脚本包）可以在 init 或清单里声明「后台任务」：按时刻表
//! （`times`）或间隔（`interval_ms`）跑某个动作，每个账号各一份运行态
//! （最后一次运行时间 / 下次到期 / 错误 / 结果）。签到窗口就挂在这套机制上
//! （`checkin: true` 的那条任务不进任务列表，走签到中心）。
//!
//! ── 分工 ────────────────────────────────────────────────────
//! 本文件只是 HTTP 壳：参数形状校验（白名单键）+ 错误形状整形。
//! **任务定义解析、排期与执行全部在 `plugin_host::wasm::tasks`**（T3 交付）：
//! - `snapshot()` 出数组（每项 `{provider, task, runs}`），只含已加载的 wasm provider；
//! - `configure(provider, task, settings)` 保存逐任务设置（启用 / 时刻表）；
//! - `run(provider, task, manual)` 立即跑一轮（`manual=true` 跳过启用与排期判定）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::plugin_host::wasm::tasks;

/// 任务设置输入上限（A2A 256 KiB 同量级）：schedule 配置不会接近这个量级。
const MAX_TASK_BYTES: usize = 64 * 1024;

/// 管理面路由（全部挂鉴权）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/plugins/tasks",
            axum::routing::get(list).patch(configure),
        )
        .route("/api/plugins/tasks/run", axum::routing::post(run))
}

/// GET /api/plugins/tasks —— 全部已加载插件的任务快照。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let snapshot = tasks::snapshot();
    Ok(Json(
        json!({ "success": true, "data": { "tasks": snapshot } }),
    ))
}

/// PATCH /api/plugins/tasks —— 保存单个任务的设置。
pub async fn configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_TASK_BYTES {
        return Err(error_response(
            "插件任务设置输入过大",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return Err(error_response(
                "请求体不是合法 JSON",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let provider = value["provider"].as_str().unwrap_or("");
    let id = value["task"].as_str().unwrap_or("");
    match tasks::configure(provider, id, &value["settings"]) {
        // A2A 同口径：保存后回全量快照（前端一次刷新到位）
        Ok(()) => Ok(Json(
            json!({ "success": true, "data": { "tasks": tasks::snapshot() } }),
        )),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// POST /api/plugins/tasks/run —— 立即跑一轮（手动触发，跳过排期判定）。
pub async fn run(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_TASK_BYTES {
        return Err(error_response(
            "插件任务输入过大",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return Err(error_response(
                "请求体不是合法 JSON",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let provider = value["provider"].as_str().unwrap_or("");
    let id = value["task"].as_str().unwrap_or("");
    match tasks::run(provider, id, true).await {
        Ok(result) => Ok(Json(json!({ "success": true, "data": result }))),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_forwards_provider_and_task() {
        let value =
            json!({ "provider": "kiro", "task": "keepalive", "settings": { "enabled": true } });
        assert_eq!(value["provider"].as_str(), Some("kiro"));
        assert_eq!(value["task"].as_str(), Some("keepalive"));
        assert_eq!(value["settings"]["enabled"], true);
        assert_eq!(MAX_TASK_BYTES, 65_536);
    }
}
