use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

use crate::account::AccountPool;
use crate::bridge::{
    Bridge, BridgeContext, ChatFormat, ChatMessage, EmbeddingRequest, FinishReason, Role,
};
use crate::config::ConfigManager;
use crate::hub::Hub;
use crate::model::ModelMapper;
use crate::payment::order_store::OrderStore;
use crate::proxy::CfApiClient;
use crate::usage::UsageTracker;
use crate::user::UserStore;
use crate::health::{HealthTracker, LivezState};

use super::auth::ApiKeyStore;

/// 共享应用状态
#[derive(Clone)]
pub struct AppState {
    pub api_client: Arc<CfApiClient>,
    pub model_mapper: Arc<ModelMapper>,
    pub usage_tracker: Arc<UsageTracker>,
    pub account_pool: Arc<AccountPool>,
    pub api_key_store: Arc<ApiKeyStore>,
    pub config_manager: Arc<ConfigManager>,
    pub hub: Arc<Hub>,
    pub user_store: Arc<UserStore>,
    pub order_store: Arc<OrderStore>,
    pub epay_client: Arc<crate::payment::EpayClient>,
    pub health_tracker: Arc<HealthTracker>,
    pub livez_state: Arc<LivezState>,
<<<<<<< HEAD
}

/// 创建 OpenAI 兼容的错误响应
fn error_response(code: &str, message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "param": null,
                "code": code
            }
        })),
    )
}

/// 从请求中提取 API Key
fn extract_api_key(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(key_str) = key.to_str() {
            return Some(key_str.to_string());
        }
    }
    None
}

/// 验证请求中的 API Key
fn verify_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;

    let api_key = state
        .api_key_store
        .validate(&key)
        .ok_or_else(|| error_response("auth_error", "Invalid API key", StatusCode::UNAUTHORIZED))?;

    Ok(api_key.id)
}

/// 根据模型名解析提供商并获取对应的 Bridge
///
/// 参考 aisix 的 dispatch_two_tier 模式：
/// 先查专用提供商（specialized），再查适配器族（family）。
/// 当前仅注册了 Cloudflare 专用桥接。
pub fn resolve_bridge(hub: &Hub, _model: &str) -> Option<Arc<dyn Bridge>> {
    hub.get_specialized("cloudflare")
}

/// 将 JSON Value 的消息数组解析为 ChatMessage 列表
fn parse_messages(value: Option<&Value>) -> Option<Vec<ChatMessage>> {
    let arr = value?.as_array()?;
    let mut messages = Vec::new();
    for msg in arr {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some("system") => Role::System,
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        let content = msg
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());
        let name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        messages.push(ChatMessage {
            role,
            content,
            name,
            tool_call_id,
        });
    }
    Some(messages)
}

/// 将 FinishReason 转换为 OpenAI 兼容的字符串
fn finish_reason_str(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::ToolCalls => "tool_calls",
    }
}

/// 将 BridgeError 转换为 HTTP 响应
fn bridge_error_response(e: crate::bridge::BridgeError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(e.error_type(), &e.to_string(), status)
}

/// POST /v1/chat/completions - 聊天补全
///
/// 使用 Bridge/Hub 模式分发请求，参考 aisix 的 chat_completions 处理流程：
/// 1. 认证 → 2. 解析 ChatFormat → 3. Hub 分发 Bridge → 4. 调用 Bridge::chat/chat_stream
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let _key_id = match verify_api_key(&state, &headers) {
        Ok(id) => id,
        Err(e) => return e.into_response(),
    };

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST)
                .into_response()
        }
    };

    // 通过 Hub 获取 Bridge
    let bridge = match resolve_bridge(&state.hub, &model) {
        Some(b) => b,
        None => {
            return error_response(
                "no_bridge",
                "No bridge available for the requested model",
                StatusCode::SERVICE_UNAVAILABLE,
            )
            .into_response()
        }
    };

    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // 解析消息列表
    let messages = parse_messages(body.get("messages")).unwrap_or_default();
    if messages.is_empty() {
        return error_response("invalid_messages", "No valid messages", StatusCode::BAD_REQUEST)
            .into_response();
    }

    // 构建 ChatFormat
    let chat_req = ChatFormat {
        model: model.clone(),
        messages,
        max_tokens: body.get("max_tokens").and_then(|v| v.as_u64()).map(|v| v as u32),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
    };

    let ctx = BridgeContext::new(format!("req-{}", uuid::Uuid::new_v4()), model.clone());

    if is_stream {
        match bridge.chat_stream(&chat_req, &ctx).await {
            Ok(stream) => {
                let sse_stream = stream.map(move |chunk_result| match chunk_result {
                    Ok(chunk) => {
                        let sse_data = serde_json::json!({
                            "id": chunk.id,
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": chunk.model,
                            "choices": [{
                                "index": 0,
                                "delta": {
                                    "content": chunk.delta.content.unwrap_or_default(),
                                },
                                "finish_reason": chunk.finish_reason.as_ref().map(|fr| finish_reason_str(fr)),
                            }]
                        })
                        .to_string();
                        Ok::<_, Infallible>(Event::default().data(sse_data))
                    }
                    Err(e) => {
                        tracing::error!("Stream chunk error: {e}");
                        Ok(Event::default().data(format!("error: {e}")))
                    }
                });

                // 发送结束标记
                let final_event = Ok::<_, Infallible>(Event::default().data("[DONE]"));
                let combined = sse_stream.chain(futures::stream::once(async { final_event }));

                Sse::new(combined).into_response()
            }
            Err(e) => bridge_error_response(e).into_response(),
        }
    } else {
        match bridge.chat(&chat_req, &ctx).await {
            Ok(response) => {
                // 记录用量
                state.usage_tracker.accumulate(
                    response.usage.prompt_tokens,
                    response.usage.completion_tokens,
                    0,
                    0,
                    0,
                    0.0,
                );

                let json = serde_json::json!({
                    "id": response.id,
                    "object": "chat.completion",
                    "created": chrono::Utc::now().timestamp(),
                    "model": response.model,
                    "choices": [{
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": response.message.content_str(),
                        },
                        "finish_reason": finish_reason_str(&response.finish_reason),
                    }],
                    "usage": {
                        "prompt_tokens": response.usage.prompt_tokens,
                        "completion_tokens": response.usage.completion_tokens,
                        "total_tokens": response.usage.total_tokens,
                    }
                });
                Json(json).into_response()
            }
            Err(e) => bridge_error_response(e).into_response(),
        }
    }
}

/// POST /v1/completions - 文本补全
///
/// 通过 Bridge::complete 委托给 Bridge 处理
pub async fn handle_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST))?;

    let bridge = resolve_bridge(&state.hub, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;

    let ctx = BridgeContext::new(format!("req-{}", uuid::Uuid::new_v4()), model.to_string());

    match bridge.complete(&body, &ctx).await {
        Ok(result) => {
            let prompt_tokens = result
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion_tokens = result
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            state
                .usage_tracker
                .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);
            Ok(Json(result))
        }
        Err(e) => Err(bridge_error_response(e)),
    }
}

/// POST /v1/embeddings - 向量嵌入
///
/// 通过 Bridge::embed 委托给 Bridge 处理
pub async fn handle_embeddings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or_else(|| error_response("invalid_model", "Missing model field", StatusCode::BAD_REQUEST))?;

    let bridge = resolve_bridge(&state.hub, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;

    let input = body.get("input");
    let texts: Vec<String> = match input {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return Err(error_response(
                "invalid_input",
                "Invalid input field",
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let embed_req = EmbeddingRequest {
        model: model.to_string(),
        input: texts,
    };
    let ctx = BridgeContext::new(format!("req-{}", uuid::Uuid::new_v4()), model.to_string());

    match bridge.embed(&embed_req, &ctx).await {
        Ok(response) => {
            let data: Vec<Value> = response
                .data
                .iter()
                .map(|obj| {
                    serde_json::json!({
                        "object": "embedding",
                        "index": obj.index,
                        "embedding": obj.embedding,
                    })
                })
                .collect();

            state
                .usage_tracker
                .accumulate(response.usage.prompt_tokens as u64, 0, 0, 0, 0, 0.0);

            Ok(Json(serde_json::json!({
                "object": "list",
                "data": data,
                "model": model,
                "usage": {
                    "prompt_tokens": response.usage.prompt_tokens,
                    "total_tokens": response.usage.total_tokens,
                }
            })))
        }
        Err(e) => Err(bridge_error_response(e)),
    }
}

/// POST /v1/images/generations - 图片生成
///
/// 通过 Bridge::generate_image 委托给 Bridge 处理
pub async fn handle_images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("sdxl");

    let bridge = resolve_bridge(&state.hub, model)
        .ok_or_else(|| error_response("no_bridge", "No bridge available", StatusCode::SERVICE_UNAVAILABLE))?;

    let ctx = BridgeContext::new(format!("req-{}", uuid::Uuid::new_v4()), model.to_string());

    match bridge.generate_image(&body, &ctx).await {
        Ok(result) => {
            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);
            Ok(Json(result))
        }
        Err(e) => Err(bridge_error_response(e)),
    }
}

/// POST /v1/audio/transcriptions - 语音转文字
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type.split("boundary=").nth(1).map(|s| s.to_string()).ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing boundary in content-type",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return Err(error_response(
                "read_error",
                &format!("Failed to read body: {e}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let (audio_data, model, filename) = parse_multipart_audio(&bytes, &boundary)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(&model, audio_data.to_vec(), &mime_type)
        .await
    {
        Ok(result) => {
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => Err(error_response(
            "api_error",
            &format!("Transcription error: {e}"),
            StatusCode::BAD_GATEWAY,
        )),
    }
}

/// POST /v1/audio/translations - 语音翻译
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_translations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type.split("boundary=").nth(1).map(|s| s.to_string()).ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing boundary in content-type",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => {
            return Err(error_response(
                "read_error",
                &format!("Failed to read body: {e}"),
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let (audio_data, model, filename) = parse_multipart_audio(&bytes, &boundary)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(&model, audio_data.to_vec(), &mime_type)
        .await
    {
        Ok(result) => {
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => Err(error_response(
            "api_error",
            &format!("Translation error: {e}"),
            StatusCode::BAD_GATEWAY,
        )),
    }
}

/// POST /v1/audio/speech - 文本转语音
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_speech(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("tts");

    let input = body
        .get("input")
        .and_then(|i| i.as_str())
        .ok_or_else(|| error_response("invalid_input", "Missing input field", StatusCode::BAD_REQUEST))?;

    let voice = body
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let cf_body = serde_json::json!({
        "input": input,
        "voice": voice,
    });

    match state.api_client.run_text(model, cf_body).await {
        Ok(result) => {
            let audio_data = result.get("audio").and_then(|a| a.as_str()).unwrap_or("");

            state
                .usage_tracker
                .accumulate(0, 0, 0, 0, 0, 0.0);

            Ok(Json(serde_json::json!({
                "audio_base64": audio_data,
                "content_type": "audio/wav"
            })))
        }
        Err(e) => Err(error_response(
            "api_error",
            &format!("Text-to-speech error: {e}"),
            StatusCode::BAD_GATEWAY,
        )),
    }
}

/// GET /v1/models - 模型列表
pub async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    let now = chrono::Utc::now().timestamp();

    let model_list: Vec<Value> = mappings
        .into_iter()
        .map(|(name, cf_model)| {
            let owned_by = crate::proxy::get_model_owned_by(&cf_model);
            serde_json::json!({
                "id": name,
                "object": "model",
                "created": now,
                "owned_by": owned_by
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "object": "list",
        "data": model_list
    })))
}

/// GET /v1/models/{model} - 模型详情
pub async fn handle_get_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    if mappings.contains_key(&model) {
        let now = chrono::Utc::now().timestamp();
        let cf_model = state.model_mapper.resolve(&model);
        let owned_by = crate::proxy::get_model_owned_by(&cf_model);
        Ok(Json(serde_json::json!({
            "id": model,
            "object": "model",
            "created": now,
            "owned_by": owned_by,
            "permission": []
        })))
    } else {
        Err(error_response(
            "model_not_found",
            &format!("Model '{model}' not found"),
            StatusCode::NOT_FOUND,
        ))
    }
}

/// 解析 multipart 表单中的音频文件
fn parse_multipart_audio(
    bytes: &bytes::Bytes,
    boundary: &str,
) -> Result<(bytes::Bytes, String, String), (StatusCode, Json<Value>)> {
    let mut file_data: Option<(bytes::Bytes, String)> = None;
    let mut model = String::from("whisper");

    let content_str = String::from_utf8_lossy(bytes);
    let boundary_tag = format!("--{}", boundary);
    let parts: Vec<&str> = content_str.split(&boundary_tag).collect();

    for part in parts {
        if part.is_empty() || part.trim() == "--" || part.trim() == "-" {
            continue;
        }

        if let Some(body_start) = part.find("\r\n\r\n") {
            let header_section = &part[..body_start];
            let body_content = &part[body_start + 4..];
            let body_content = body_content.trim_end_matches('\r').trim_end_matches('\n');

            if header_section.contains("name=\"model\"") {
                model = body_content.trim().to_string();
            } else if header_section.contains("name=\"file\"") || header_section.contains("name=\"audio\"") {
                let filename = header_section
                    .split(';')
                    .find_map(|s| {
                        let s = s.trim();
                        s.strip_prefix("filename=\"")
                            .or_else(|| s.strip_prefix("filename="))
                            .map(|f| f.trim_matches('"').to_string())
                    })
                    .unwrap_or_else(|| "audio.wav".to_string());

                let data = bytes::Bytes::copy_from_slice(body_content.as_bytes());
                file_data = Some((data, filename));
            }
        }
    }

    file_data.ok_or_else(|| {
        error_response(
            "invalid_request",
            "Missing audio file in request",
            StatusCode::BAD_REQUEST,
        )
    })
    .map(|(data, filename)| (data, model, filename))
=======
>>>>>>> ee15e7fbfeb81da01c35045c9eb257bce0b7a8dd
}
