// Ported from Agent2API v3.5.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 插件权限授权面（`GET/PATCH /api/plugins/permissions`）。
//!
//! 对齐上游 `api/plugin_permissions.rs`：一次读写两类「宿主代插件执行」的
//! 授权——账号来源（`accountOrigins`）与受控任务（`managedTasks`）。
//!
//! ## 与上游的差异（诚实）
//!
//! 上游把两类授权都落成**持久化的逐条批准表**（运维在管理面逐条确认，
//! 存 `pluginAccountOriginGrants` / 受控任务批准）。AIGX 的 Wasm 宿主刻意
//! 与配置存储解耦，采用的是：
//!
//! - 账号来源：**清单声明即授权**（`wasm.json` 的 `account_origin` 经安装即
//!   代表批准，见 `wasm::account_origin` 模块头）；
//! - 受控任务：进程内审批闸门（`wasm::managed_task` 的 `approve` / `revoke`，
//!   不落盘，停用/卸载即失效）。
//!
//! 因此本文件的 `PATCH` 只对 `kind="managedTask"` 做真实的批准/撤销；
//! 无 `kind`（旧式账号来源授权）如实返回 400 并说明「以清单声明为准」，
//! 不假装保存了一份并不存在的授权。
//!
//! ## 接线
//!
//! `router()` 由主控在 `api/admin/mod.rs` + `main.rs` 合并时挂载。当前尚无
//! 路由引用，故三个入口都标 `#[allow(dead_code)]`（不是遗漏，是待接线）。

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::plugin_host::wasm::{managed_task, manifest_for};

/// 授权输入上限（与上游 16 KiB 一致）。
const MAX_PERMISSION_BYTES: usize = 16 * 1024;

/// 管理面路由（全部挂鉴权）。由主控接线；见模块头。
#[allow(dead_code)] // 待接线：路由注册在 main.rs / api/admin/mod.rs
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/plugins/permissions",
        axum::routing::get(list).patch(configure),
    )
}

/// 当前授权快照（声明式模型，见模块头）。
///
/// `pack` 在 AIGX 里按 **provider id** 解释（Wasm 宿主以 provider 为键）。
#[allow(dead_code)] // 待接线：经 list/configure 使用
fn snapshot(pack: &str) -> Value {
    let account_origins = manifest_for(pack)
        .and_then(|manifest| manifest.account_origin)
        .map(|raw| json!([raw]))
        .unwrap_or_else(|| json!([]));
    json!({
        "pack": pack,
        "accountOrigins": account_origins,
        "managedTasks": {
            "supported": true,
            "approved": managed_task::is_approved(pack),
        },
        "declarationOnly": true,
    })
}

/// GET /api/plugins/permissions?pack=<provider> —— 当前授权快照。
#[allow(dead_code)] // 待接线：路由注册在 main.rs / api/admin/mod.rs
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let pack = query
        .get("pack")
        .or_else(|| query.get("provider"))
        .map(String::as_str)
        .unwrap_or("");
    if pack.is_empty() || pack.len() > 128 {
        return Err(error_response("缺少有效插件标识", StatusCode::BAD_REQUEST));
    }
    Ok(Json(json!({ "success": true, "data": snapshot(pack) })))
}

/// PATCH /api/plugins/permissions —— 批准 / 撤销受控任务执行。
///
/// 请求体：
/// - `{"kind":"managedTask","pack":"<provider>","authorized":true|false}`
///   —— 批准或撤销（幂等）；
/// - 无 `kind` —— 账号来源授权（AIGX 以清单声明为准，不支持运行时变更）。
#[allow(dead_code)] // 待接线：路由注册在 main.rs / api/admin/mod.rs
pub async fn configure(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.len() > MAX_PERMISSION_BYTES {
        return Err(error_response(
            "插件权限授权输入过大",
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return Err(error_response(
                "插件权限授权请求不是有效 JSON",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    if !value.is_object() {
        return Err(error_response(
            "插件权限授权请求格式无效",
            StatusCode::BAD_REQUEST,
        ));
    }
    match value.get("kind") {
        Some(Value::String(kind)) if kind == "managedTask" => {
            let provider = value["pack"]
                .as_str()
                .or_else(|| value["provider"].as_str())
                .unwrap_or("");
            if provider.is_empty() || provider.len() > 128 {
                return Err(error_response("缺少有效插件标识", StatusCode::BAD_REQUEST));
            }
            let authorized = match value["authorized"].as_bool() {
                Some(authorized) => authorized,
                None => {
                    return Err(error_response(
                        "受控任务授权必须明确 authorized 布尔值",
                        StatusCode::BAD_REQUEST,
                    ))
                }
            };
            if authorized {
                managed_task::approve(provider);
            } else {
                managed_task::revoke(provider);
            }
            let mut result = snapshot(provider);
            result["saved"] = json!(true);
            Ok(Json(json!({ "success": true, "data": result })))
        }
        None => Err(error_response(
            "AIGX 的账号来源授权以插件清单声明为准，不支持运行时变更",
            StatusCode::BAD_REQUEST,
        )),
        Some(_) => Err(error_response(
            "插件权限授权类型无效",
            StatusCode::BAD_REQUEST,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_shape_is_stable_for_unknown_pack() {
        let value = snapshot("unit-unknown");
        assert_eq!(value["pack"], "unit-unknown");
        assert_eq!(value["accountOrigins"], json!([]));
        assert_eq!(value["declarationOnly"], json!(true));
        assert_eq!(value["managedTasks"]["supported"], json!(true));
    }

    #[test]
    fn managed_task_approval_round_trip() {
        managed_task::revoke("unit-perm-provider");
        assert!(!managed_task::is_approved("unit-perm-provider"));
        managed_task::approve("unit-perm-provider");
        assert!(managed_task::is_approved("unit-perm-provider"));
        assert_eq!(
            snapshot("unit-perm-provider")["managedTasks"]["approved"],
            json!(true)
        );
        managed_task::revoke("unit-perm-provider");
        assert!(!managed_task::is_approved("unit-perm-provider"));
    }
}
