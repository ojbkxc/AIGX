//! 侧边栏布局（服务端权威副本）。
//!
//! `GET/PUT /api/sidebar-layout`，请求/响应均为 `{ layout: <对象> | null }`。
//!
//! 契约（后端只做最小校验，形状由前端 `lib/sidebar-layout.ts` 拥有，随版本
//! 增删菜单漂移，后端**不做键白名单**）：
//! - PUT `layout` 必须是对象或 `null`；`null` = 恢复默认，落 `{}`；
//! - GET 未存过 / 存的是空对象时回 `null`（前端回落 `defaultLayout()`）；
//! - 整份当**不透明 JSON** 存 KV，键 `sidebarLayout`。
//!
//! 写完触发一次云同步 notify（`schedule_now(cloudSync)`）：布局是跨设备
//! 体验的一部分，用户改完不应干等下一个调度周期。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

/// KV 保留键（不透明 JSON）。
const LAYOUT_KEY: &str = "sidebarLayout";

/// 通用 KV 句柄：与本进程其它存储共用同一个 `FileStore`。
fn store(state: &AppState) -> &crate::storage::FileStore {
    &state.scheduled_task_store
}

/// 空对象 `{}` 与「没存过」同义：前端都按默认布局处理。
fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|map| map.is_empty())
}

/// 读服务端权威布局；未存过 / 空对象 → `null`。
fn load_layout(state: &AppState) -> Value {
    match store(state).get::<Value>(LAYOUT_KEY) {
        Ok(Some(value)) if !is_empty_object(&value) => value,
        _ => Value::Null,
    }
}

/// 写完通知云同步调度「下一拍就执行」；调度未安装/失败只告警，不影响本次写入。
fn notify_cloud_sync() {
    if let Err(error) =
        crate::scheduled_tasks::task_state::schedule_now(crate::sync::scheduler::TASK_KEY)
    {
        tracing::warn!("[sidebar-layout] 触发云同步排期失败：{error}");
    }
}

/// GET /api/sidebar-layout
pub async fn handle_get_sidebar_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "layout": load_layout(&state),
    })))
}

/// PUT /api/sidebar-layout — 整份替换；`layout: null` = 恢复默认（落 `{}`）。
pub async fn handle_put_sidebar_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;
    // 缺字段按 null 处理（等同恢复默认），其余非法顶层类型拒绝。
    let layout = body.get("layout").cloned().unwrap_or(Value::Null);
    match &layout {
        Value::Null => {
            store(&state).put(LAYOUT_KEY, &json!({})).map_err(|error| {
                error_response(
                    &format!("保存侧边栏布局失败：{error}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
        }
        Value::Object(_) => {
            store(&state).put(LAYOUT_KEY, &layout).map_err(|error| {
                error_response(
                    &format!("保存侧边栏布局失败：{error}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
        }
        _ => {
            return Err(error_response(
                "layout 必须是对象或 null",
                StatusCode::BAD_REQUEST,
            ))
        }
    }
    notify_cloud_sync();
    // 回读权威值：null 落盘为 `{}`，读侧再统一折回 null。
    Ok(Json(json!({
        "success": true,
        "layout": load_layout(&state),
    })))
}

/// 管理面路由（由主控统一注册）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/sidebar-layout",
        get(handle_get_sidebar_layout).put(handle_put_sidebar_layout),
    )
}
