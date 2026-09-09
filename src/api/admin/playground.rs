//! Playground API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供在线聊天 Playground 功能，直接测试渠道连接。

use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

#[derive(Debug, Deserialize)]
pub struct PlaygroundChatRequest {
    pub channel_id: Option<String>,
    pub model: String,
    #[serde(default)]
    pub messages: Vec<serde_json::Value>,
    /// P1 Playground V2：Completions 模式用顶层 prompt；缺省走 chat messages
    #[serde(default)]
    pub prompt: Option<String>,
    /// 参数透传（chat 与 completions 共用）：None 时不写入上游
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub presence_penalty: Option<f32>,
    #[serde(default)]
    pub frequency_penalty: Option<f32>,
    #[serde(default)]
    pub response_format: Option<serde_json::Value>,
    /// JSON mode 快捷开关：response_format = { "type": "json_object" }
    #[serde(default)]
    pub json_mode: Option<bool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<i32>,
}

/// POST /api/admin/playground/chat - Playground 聊天
///
/// 权限对齐 new-api：Playground 是普通用户与管理员共用的调试沙盒，
/// 登录即可使用；channel_id 为空时自动选择第一个启用渠道。
pub async fn handle_playground_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundChatRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    // 普通用户不得指定 channel_id（与 /api/channels/chat_test 守卫一致）
    if !user.is_admin()
        && body
            .channel_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return error_response(
            "Only administrators can target a specific channel",
            StatusCode::FORBIDDEN,
        )
        .into_response();
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
    // Completions 模式走 /completions，chat 模式走 /chat/completions
    let is_completions = body.prompt.as_deref().is_some();
    let url = if is_completions {
        format!("{base}/completions")
    } else {
        format!("{base}/chat/completions")
    };

    let mut payload = json!({
        "model": model,
        "messages": body.messages,
        "stream": false,
    });
    // P1 Playground V2：Completions 模式 —— 顶层 prompt 替换 messages
    if let Some(prompt) = body.prompt.as_deref() {
        payload = json!({
            "model": model,
            "prompt": prompt,
            "stream": false,
        });
    }
    if let Some(t) = body.temperature {
        payload["temperature"] = json!(t);
    }
    if let Some(m) = body.max_tokens {
        payload["max_tokens"] = json!(m);
    }
    if let Some(p) = body.top_p {
        payload["top_p"] = json!(p);
    }
    if let Some(p) = body.presence_penalty {
        payload["presence_penalty"] = json!(p);
    }
    if let Some(f) = body.frequency_penalty {
        payload["frequency_penalty"] = json!(f);
    }
    if body.json_mode == Some(true) {
        payload["response_format"] = json!({ "type": "json_object" });
    } else if let Some(rf) = body.response_format {
        payload["response_format"] = rf;
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

/// GET /api/admin/playground/channels - 列出可用渠道（登录即可，列表已脱敏）
pub async fn handle_playground_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;
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

/// POST /api/playground/images - Playground Images 模式（图片生成）
///
/// 与 handle_playground_chat 同权限语义：登录即可用，普通用户不得指定
/// channel_id。请求体透传上游 /images/generations（model/prompt/n/size）。
#[derive(Debug, Deserialize)]
pub struct PlaygroundImagesRequest {
    pub channel_id: Option<String>,
    pub model: String,
    pub prompt: String,
    #[serde(default)]
    pub n: Option<u32>,
    #[serde(default)]
    pub size: Option<String>,
}

pub async fn handle_playground_images(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundImagesRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if !user.is_admin()
        && body
            .channel_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    {
        return error_response(
            "Only administrators can target a specific channel",
            StatusCode::FORBIDDEN,
        )
        .into_response();
    }

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
        ch.models.first().cloned().unwrap_or_default()
    } else {
        body.model.trim().to_string()
    };
    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/images/generations");

    let mut payload = json!({ "model": model, "prompt": body.prompt });
    if let Some(n) = body.n {
        payload["n"] = json!(n);
    }
    if let Some(size) = body.size.as_deref() {
        payload["size"] = json!(size);
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
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
                Ok(j) => Json(json!({ "success": true, "data": j })).into_response(),
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

/// POST /api/playground/tts - Playground 文本转语音（登录即可）
///
/// 请求体透传上游 OpenAI 兼容 /audio/speech（model/input/voice），
/// 返回 { audio_base64, content_type }，前端 <audio> 直接播放。
#[derive(Debug, Deserialize)]
pub struct PlaygroundTtsRequest {
    pub model: String,
    pub input: String,
    #[serde(default)]
    pub voice: Option<String>,
}

pub async fn handle_playground_tts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlaygroundTtsRequest>,
) -> Response {
    let _user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    if body.input.trim().is_empty() {
        return error_response("input is required", StatusCode::BAD_REQUEST).into_response();
    }

    let ch = match state
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
    };

    let model = if body.model.trim().is_empty() {
        ch.models.first().cloned().unwrap_or_default()
    } else {
        body.model.trim().to_string()
    };
    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/audio/speech");

    let mut payload = json!({ "model": model, "input": body.input });
    if let Some(v) = body.voice.as_deref() {
        payload["voice"] = json!(v);
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
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
            let content_type = resp
                .headers()
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("audio/mpeg")
                .to_string();
            match resp.bytes().await {
                Ok(b) => Json(json!({
                    "success": true,
                    "data": {
                        "audio_base64": BASE64.encode(&b),
                        "content_type": content_type
                    }
                }))
                .into_response(),
                Err(e) => error_response(
                    &format!("Upstream returned no audio: {e}"),
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

/// POST /api/playground/transcriptions - Playground 语音转文字（登录即可）
///
/// multipart/form-data（file + model + 可选 language）透传上游
/// /audio/transcriptions，返回上游 JSON（含 text）。
pub async fn handle_playground_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let _user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let boundary = match content_type.split("boundary=").nth(1) {
        Some(b) => b.to_string(),
        None => {
            return error_response("Missing boundary in content-type", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                &format!("Failed to read body: {e}"),
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    let (audio_data, model, filename) =
        match super::super::openai::parse_multipart_audio(&bytes, &boundary) {
            Ok(v) => v,
            Err((_status, json_err)) => {
                let msg = json_err
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("invalid multipart")
                    .to_string();
                return error_response(&msg, StatusCode::BAD_REQUEST).into_response();
            }
        };

    let ch = match state
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
    };

    let model = if model.trim().is_empty() {
        ch.models.first().cloned().unwrap_or_default()
    } else {
        model
    };
    let api_key = ch.decode_api_key();
    let base = crate::bridge::openai::normalize_base_url(ch.base_url.trim().to_string());
    let url = format!("{base}/audio/transcriptions");

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(180))
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

    let part = reqwest::multipart::Part::bytes(audio_data.to_vec())
        .file_name(filename)
        .mime_str(&mime_type)
        .unwrap_or_else(|_| {
            reqwest::multipart::Part::bytes(audio_data.to_vec()).file_name("audio.webm")
        });
    let form = reqwest::multipart::Form::new()
        .text("model", model.clone())
        .part("file", part);

    let mut req = client.post(&url).multipart(form);
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
                Ok(j) => Json(json!({ "success": true, "data": j })).into_response(),
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
