// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件 provider 设置（`/api/provider-settings` 与 `/api/plugins/provider-settings`）。
//!
//! ── 分工 ────────────────────────────────────────────────────
//! 本文件只是 HTTP 壳：参数形状校验（白名单键）、体积上限、错误形状整形。
//! **schema 校验与合并逻辑全部在 `plugin_host::provider_settings`**（T1 交付）：
//! - `snapshot()` 出全部 provider 的设置项 + 当前值 + 修订号；
//! - `configure(provider, revision, values)` 按 schema 校验并合并（CAS：
//!   revision 不符 → 409）。
//!
//! A2A 把这两个路径都指向同一个 handler（`/api/provider-settings` 是旧名，
//! `/api/plugins/provider-settings` 是插件页入口），AIGX 保持同口径。
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
use crate::plugin_host::provider_settings;

/// 设置输入上限（A2A 128 KiB）：schema 驱动的表单不会接近这个量级。
const MAX_SETTINGS_BYTES: usize = 128 * 1024;

/// 管理面路由：旧名 + 插件页入口，两个路径同一批 handler。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/provider-settings",
            axum::routing::get(list).patch(configure),
        )
        .route(
            "/api/plugins/provider-settings",
            axum::routing::get(list).patch(configure),
        )
}

/// GET —— 全部 provider 的设置快照。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let snapshot = provider_settings::snapshot();
    Ok(Json(json!({ "success": true, "data": snapshot })))
}

/// PATCH —— 保存某家 provider 的设置（CAS：revision 不符 → 409）。
pub async fn configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_SETTINGS_BYTES {
        return Err(error_response(
            "插件设置输入过大",
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
    // 只收 provider / revision / values 三个键（A2A 同口径白名单）
    if value.as_object().is_none_or(|object| {
        object
            .keys()
            .any(|key| !matches!(key.as_str(), "provider" | "revision" | "values"))
    }) {
        return Err(error_response(
            "插件设置请求格式无效",
            StatusCode::BAD_REQUEST,
        ));
    }
    match provider_settings::configure(
        value["provider"].as_str().unwrap_or(""),
        value["revision"].as_str().unwrap_or(""),
        value.get("values").unwrap_or(&Value::Null),
    )
    .await
    {
        Ok(saved) => Ok(Json(json!({ "success": true, "data": saved }))),
        Err(error) => Err(error_response(
            &error.message,
            StatusCode::from_u16(error.status_code).unwrap_or(StatusCode::BAD_REQUEST),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_limit_is_128k() {
        assert_eq!(MAX_SETTINGS_BYTES, 131_072);
    }

    /// 白名单键：多余的键一律 400（与 handler 内判定同口径）。
    fn keys_ok(value: &Value) -> bool {
        value.as_object().is_some_and(|object| {
            object
                .keys()
                .all(|key| matches!(key.as_str(), "provider" | "revision" | "values"))
        })
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(keys_ok(
            &json!({ "provider": "kiro", "revision": "1", "values": {} })
        ));
        assert!(!keys_ok(&json!({ "provider": "kiro", "secret": "x" })));
        assert!(!keys_ok(&json!([1, 2, 3])));
    }
}
