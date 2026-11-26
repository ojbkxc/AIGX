//! 侧边栏布局（服务端权威副本，**按用户隔离**）。
//!
//! `GET/PUT /api/sidebar-layout`，请求/响应均为 `{ layout: <对象> | null }`。
//!
//! 契约（后端只做最小校验，形状由前端 `lib/sidebar-layout.ts` 拥有，随版本
//! 增删菜单漂移，后端**不做键白名单**）：
//! - PUT `layout` 必须是对象或 `null`；`null` = 恢复默认，落 `{}`；
//! - GET 未存过 / 存的是空对象时回 `null`（前端回落 `defaultLayout()`）；
//! - 整份当**不透明 JSON** 存 KV，键 `sidebarLayout:{user_id}`。
//!
//! 写完触发一次云同步 notify（`schedule_now(cloudSync)`）：布局是跨设备
//! 体验的一部分，用户改完不应干等下一个调度周期。
//!
//! ## 为什么键里必须带 `user_id`（安全修复）
//!
//! 布局是**每个用户自己的**偏好（前端注释写明「桌面壳/网页端多设备」，
//! 多端同步是为了同一个人的多台设备，不是全站统一）。此前整站共用一个
//! 全局键 `sidebarLayout`，于是任何登录用户 PUT 一次就把**所有人**的布局
//! 覆盖掉（跨用户写入越权 + 数据互相踩踏）。现在键前缀带上会话用户的
//! `id`，读写都过同一把刀 [`layout_key`]。
//!
//! 旧全局键 [`LEGACY_LAYOUT_KEY`] 保留为**只读回落**：升级前存过布局的
//! 用户第一次 GET 仍能看到自己原来那份，下次 PUT 即落到自己的键上。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

/// 每用户键前缀（`sidebarLayout:{user_id}`）。
const LAYOUT_KEY_PREFIX: &str = "sidebarLayout:";

/// 旧版全局键：只读回落用，不再写入。
const LEGACY_LAYOUT_KEY: &str = "sidebarLayout";

/// 通用 KV 句柄：与本进程其它存储共用同一个 `FileStore`。
fn store(state: &AppState) -> &crate::storage::FileStore {
    &state.scheduled_task_store
}

/// 会话用户 → KV 键（单点，读写共用）。
fn layout_key(user_id: &str) -> String {
    format!("{LAYOUT_KEY_PREFIX}{user_id}")
}

/// 空对象 `{}` 与「没存过」同义：前端都按默认布局处理。
fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(|map| map.is_empty())
}

/// 读某个键上的布局；缺失 / 空对象 → `None`。
fn read_key(state: &AppState, key: &str) -> Option<Value> {
    match store(state).get::<Value>(key) {
        Ok(Some(value)) if !is_empty_object(&value) => Some(value),
        _ => None,
    }
}

/// 读该用户的服务端权威布局。
///
/// 顺序：用户键 → 旧全局键（升级兼容）→ `null`（前端回落默认）。
fn load_layout(state: &AppState, user_id: &str) -> Value {
    read_key(state, &layout_key(user_id))
        .or_else(|| read_key(state, LEGACY_LAYOUT_KEY))
        .unwrap_or(Value::Null)
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
    let me = verify_user(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "layout": load_layout(&state, &me.id),
    })))
}

/// PUT /api/sidebar-layout — 整份替换；`layout: null` = 恢复默认（落 `{}`）。
pub async fn handle_put_sidebar_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    let key = layout_key(&me.id);
    // 缺字段按 null 处理（等同恢复默认），其余非法顶层类型拒绝。
    let layout = body.get("layout").cloned().unwrap_or(Value::Null);
    let to_store = match &layout {
        Value::Null => json!({}),
        Value::Object(_) => layout.clone(),
        _ => {
            return Err(error_response(
                "layout 必须是对象或 null",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    store(&state).put(&key, &to_store).map_err(|error| {
        error_response(
            &format!("保存侧边栏布局失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    notify_cloud_sync();
    // 回读权威值：null 落盘为 `{}`，读侧再统一折回 null。
    Ok(Json(json!({
        "success": true,
        "layout": load_layout(&state, &me.id),
    })))
}

/// 管理面路由（由主控统一注册）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/sidebar-layout",
        get(handle_get_sidebar_layout).put(handle_put_sidebar_layout),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 键必须带用户维度：不同用户的键互不相同，且都不是旧全局键。
    ///
    /// 这条直接对应跨用户覆盖缺陷——改回单一全局键时必然红。
    #[test]
    fn layout_key_isolates_users() {
        let a = layout_key("user-a");
        let b = layout_key("user-b");
        assert_ne!(a, b, "两个用户的布局键必须不同");
        assert_ne!(a, LEGACY_LAYOUT_KEY, "用户键不能退化为旧全局键");
        assert_ne!(b, LEGACY_LAYOUT_KEY, "用户键不能退化为旧全局键");
        assert!(a.starts_with(LAYOUT_KEY_PREFIX));
        assert_eq!(a, "sidebarLayout:user-a");
    }

    /// 空对象与「没存过」同义（前端按默认布局渲染）。
    #[test]
    fn empty_object_is_treated_as_absent() {
        assert!(is_empty_object(&json!({})));
        assert!(!is_empty_object(&json!({ "groups": [] })));
        assert!(!is_empty_object(&Value::Null));
        assert!(!is_empty_object(&json!([])));
    }
}
