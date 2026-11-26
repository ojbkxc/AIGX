//! AI 助手配置（服务端权威副本）。
//!
//! `GET/PUT /api/assistant/config`：
//! - GET → `{ config: <对象> }`（未存过回 `{}`）；
//! - PUT body `{ config }` → **整块替换**，服务端不做字段白名单/合并
//!   （`AssistantConfigWire` 形状由前端拥有，随版本增删字段漂移）；
//! - 配置整份当不透明 JSON 存 KV，键 `assistantConfig`。
//!
//! 契约来源：`frontend/src/api/endpoints/assistant.ts` 的 `AssistantConfigWire`。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

const CONFIG_KEY: &str = "assistantConfig";

fn store(state: &AppState) -> &crate::storage::FileStore {
    &state.scheduled_task_store
}

/// 读服务端权威配置；未存过回空对象（前端用自带默认值）。
fn load_config(state: &AppState) -> Value {
    match store(state).get::<Value>(CONFIG_KEY) {
        Ok(Some(value @ Value::Object(_))) => value,
        _ => json!({}),
    }
}

/// GET /api/assistant/config
pub async fn handle_get_assistant_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "config": load_config(&state),
    })))
}

/// PUT /api/assistant/config — 整块替换。
pub async fn handle_put_assistant_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;
    let Some(config) = body.get("config") else {
        return Err(error_response("缺少 config 字段", StatusCode::BAD_REQUEST));
    };
    if !config.is_object() {
        return Err(error_response("config 必须是对象", StatusCode::BAD_REQUEST));
    }
    store(&state).put(CONFIG_KEY, config).map_err(|error| {
        error_response(
            &format!("保存助手配置失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "success": true,
        "config": load_config(&state),
    })))
}

/// 管理面路由（由主控统一注册）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/assistant/config",
        get(handle_get_assistant_config).put(handle_put_assistant_config),
    )
}
