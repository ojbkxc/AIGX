// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件原生辅助程序授权（`/api/plugins/helpers`）。
//!
//! ── 这是什么 ────────────────────────────────────────────────
//! Wasm / 脚本插件有时需要调用**本机已有的原生辅助程序**（打包好的小工具，
//! 例如生成设备指纹、解密本地凭据）。这类调用跨出沙箱、具有本机权限，
//! 因此必须**逐项显式授权**：插件申报要哪些 helper，用户在管理面看到
//! 候选清单（哪个包提供的、调用方摘要）后点确认，网关记下授权。
//!
//! ── 分工 ────────────────────────────────────────────────────
//! 本文件只是 HTTP 壳：参数形状校验 + 「必须显式确认」这条人工闸门。
//! **候选发现、授权落盘与调用全部在 `plugin_host::wasm::native`**（T3 交付）：
//! - `snapshot(pack)` 出 `{grants, requested, candidates}`；
//! - `grant(provider, helper, target, pack)` 落授权（`target=null` = 撤销）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::plugin_host::wasm::native;

/// 授权输入上限（A2A 256 KiB 同量级）。
const MAX_HELPER_BYTES: usize = 256 * 1024;

/// 管理面路由（全部挂鉴权）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/plugins/helpers",
        axum::routing::get(list).patch(configure),
    )
}

/// GET /api/plugins/helpers?pack=<可选> —— 候选清单 + 已授权记录。
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let snapshot = native::snapshot(query.get("pack").map(String::as_str)).await;
    Ok(Json(json!({ "success": true, "data": snapshot })))
}

/// PATCH /api/plugins/helpers —— 授权 / 撤销某个 helper。
///
/// 撤销（`target = null`）不需要确认；授权（`target` 非空）必须
/// `confirmed == true` —— 原生辅助程序有本机权限，静默授权等于把沙箱
/// 边界开一个洞。
pub async fn configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_HELPER_BYTES {
        return Err(error_response(
            "原生辅助授权输入过大",
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
    if !value.is_object()
        || value.get("target").is_none()
        || value.get("pack").is_some_and(|pack| {
            pack.as_str()
                .is_none_or(|id| id.is_empty() || id.len() > 128)
        })
    {
        return Err(error_response(
            "原生辅助授权请求格式无效",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !value["target"].is_null() && value["confirmed"] != true {
        return Err(error_response(
            "原生辅助程序具有本机权限，请明确确认后再授权",
            StatusCode::BAD_REQUEST,
        ));
    }
    let pack = value["pack"].as_str();
    match native::grant(
        value["provider"].as_str().unwrap_or(""),
        value["helper"].as_str().unwrap_or(""),
        &value["target"],
        pack,
    )
    .await
    {
        Ok(saved) => {
            let mut result = native::snapshot(pack).await;
            result["saved"] = saved;
            Ok(Json(json!({ "success": true, "data": result })))
        }
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 请求形状判定（与 handler 内逐字同口径，抽出来单测）。
    fn shape_ok(value: &Value) -> bool {
        value.is_object()
            && value.get("target").is_some()
            && value.get("pack").is_none_or(|pack| {
                pack.as_str()
                    .is_some_and(|id| !id.is_empty() && id.len() <= 128)
            })
    }

    #[test]
    fn grant_requires_target_and_confirmation() {
        // 合法授权请求（带 target）
        assert!(shape_ok(
            &json!({ "provider": "kiro", "helper": "fingerprint", "target": {"pack":"p"}, "confirmed": true })
        ));
        // 撤销请求（target = null）
        assert!(shape_ok(
            &json!({ "provider": "kiro", "helper": "fingerprint", "target": null })
        ));
        // 缺 target
        assert!(!shape_ok(
            &json!({ "provider": "kiro", "helper": "fingerprint" })
        ));
        // pack 为空串
        assert!(!shape_ok(&json!({ "target": {"pack":"p"}, "pack": "" })));
        // 非对象
        assert!(!shape_ok(&json!("x")));
        assert_eq!(MAX_HELPER_BYTES, 262_144);
    }
}
