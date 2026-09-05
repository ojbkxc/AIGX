//! Playground API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供在线聊天 Playground 功能，直接测试渠道连接。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

#[derive(Debug, Deserialize)]
pub struct PlaygroundChatRequest {
    pub channel_id: Option<String>,
    pub model: String,
    pub messages: Vec<serde_json::Value>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
}

/// POST /api/admin/playground/chat - Playground 聊天
pub async fn handle_playground_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundChatRequest>,
) -> Response {
    if let Err(e) = verify_admin(&state, &headers).await {
        return e.into_response();
    }

    // 选渠道：优先 channel_id，否则第一个启用渠道
    let ch = if let Some(ref cid) = body.channel_id {
        match state.channel_store.get(cid) {
            Some(c) => c,
            None => {
                return error_response("Channel not found", StatusCode::NOT_FOUND).into_response()
            }
        }
    } else {
        match state
            .channel_store
            .list()
            .into_iter()
            .find(|c| c.is_enabled())
        {
            Some(c) => c,
            None => {
                return error_response(
                    "No enabled channel available for playground",
                    StatusCode::BAD_REQUEST,
                )
                .into_response()
            }
        }
    };

    let model = if body.model.trim().is_empty() {
        ch.models
            .first()
            .cloned()
            .unwrap_or_else(|| "gpt-3.5-turbo".to_string())
    } else {
        body.model.trim().to_string()
    };

    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/chat/completions");

    let mut payload = json!({
        "model": model,
        "messages": body.messages,
        "stream": false,
    });
    if let Some(t) = body.temperature {
        payload["temperature"] = json!(t);
    }
    if let Some(m) = body.max_tokens {
        payload["max_tokens"] = json!(m);
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return error_response(
                &format!("HTTP client error: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
            .into_response()
        }
    };

    let mut req = client.post(&url).json(&payload);
    if !api_key.is_empty() {
        req = req.bearer_auth(&api_key);
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                return error_response(
                    &format!("Upstream HTTP {status}: {text}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response();
            }
            match resp.json::<Value>().await {
                Ok(j) => {
                    let content = j
                        .get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .to_string();
                    Json(json!({
                        "success": true,
                        "data": {
                            "content": content,
                            "model": model,
                            "usage": j.get("usage")
                        }
                    }))
                    .into_response()
                }
                Err(e) => error_response(
                    &format!("Upstream returned non-JSON: {e}"),
                    StatusCode::BAD_GATEWAY,
                )
                .into_response(),
            }
        }
        Err(e) => {
            error_response(&format!("Request failed: {e}"), StatusCode::BAD_GATEWAY).into_response()
        }
    }
}

/// GET /api/admin/playground/channels - 列出可用渠道
pub async fn handle_playground_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let channels: Vec<Value> = state
        .channel_store
        .list()
        .iter()
        .map(|c| {
            json!({
                "id": &c.id,
                "name": &c.name,
                "status": &c.status,
                "models": &c.models,
            })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": channels })))
}
