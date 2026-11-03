use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use serde_json::Value;
use super::*;
use crate::api::common::extract_client_ip;

/// POST /v1/audio/transcriptions - 语音转文字
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub(crate) async fn handle_audio_transcriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            error_response(
                "invalid_request",
                "Missing boundary in content-type",
                StatusCode::BAD_REQUEST,
            )
        })?;

    let bytes = match axum::body::to_bytes(body, current_body_limit_bytes(&state)).await {
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

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, &model)?;

    // 网络层闸门：管理员关闭网络层时拒绝转发
    let _nl_cfg = crate::api::admin::network::load_network_config(&state);
    if !_nl_cfg.enabled {
        return Err(error_response(
            "network_layer_disabled",
            "网络层已停用：推理转发被管理员关闭。请稍后重试。",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(
            &api_key.id,
            &model,
            api_key.user_id.as_deref(),
            client_ip.as_deref(),
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, &model)?;

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    ensure_model_priced(&state, &model)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(
            &model,
            audio_data.to_vec(),
            &mime_type,
            "/v1/audio/transcriptions",
        )
        .await
    {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, &model, &model, &billing_group, 0, 0, 0);

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 音频转写按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(&model, "unknown", "ok", latency_ms);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Transcription error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(&model, "unknown", "error", latency_ms);
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model.clone(),
                    error: format!("Transcription error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Transcription error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}
/// POST /v1/audio/translations - 语音翻译
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub(crate) async fn handle_audio_translations(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let boundary = content_type
        .split("boundary=")
        .nth(1)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            error_response(
                "invalid_request",
                "Missing boundary in content-type",
                StatusCode::BAD_REQUEST,
            )
        })?;

    let bytes = match axum::body::to_bytes(body, current_body_limit_bytes(&state)).await {
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

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, &model)?;

    // 网络层闸门：管理员关闭网络层时拒绝转发
    let _nl_cfg = crate::api::admin::network::load_network_config(&state);
    if !_nl_cfg.enabled {
        return Err(error_response(
            "network_layer_disabled",
            "网络层已停用：推理转发被管理员关闭。请稍后重试。",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(
            &api_key.id,
            &model,
            api_key.user_id.as_deref(),
            client_ip.as_deref(),
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, &model)?;

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    ensure_model_priced(&state, &model)?;

    let mime_type = mime_guess::from_path(&filename)
        .first_or_octet_stream()
        .to_string();

    match state
        .api_client
        .run_audio(
            &model,
            audio_data.to_vec(),
            &mime_type,
            "/v1/audio/translations",
        )
        .await
    {
        Ok(result) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let text = result.get("text").and_then(|t| t.as_str()).unwrap_or("");

            state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(&state, &api_key, &model, &model, &billing_group, 0, 0, 0);

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 音频翻译按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(&model, "unknown", "ok", latency_ms);

            Ok(Json(serde_json::json!({ "text": text })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model.clone();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Translation error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(&model, "unknown", "error", latency_ms);
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model.clone(),
                    error: format!("Translation error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Translation error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}
/// POST /v1/audio/speech - 文本转语音
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub(crate) async fn handle_audio_speech(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("tts");

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();

    // 网络层闸门：管理员关闭网络层时拒绝转发
    let _nl_cfg = crate::api::admin::network::load_network_config(&state);
    if !_nl_cfg.enabled {
        return Err(error_response(
            "network_layer_disabled",
            "网络层已停用：推理转发被管理员关闭。请稍后重试。",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    // 限流检查（功能 3）：鉴权后、推理前
    let rate_bundle = match state
        .rate_limiter
        .check(
            &api_key.id,
            &model_owned,
            api_key.user_id.as_deref(),
            client_ip.as_deref(),
        )
        .await
    {
        Ok(b) => b,
        Err(e) => {
            let retry_after = e.retry_after_secs().unwrap_or(60);
            return Err(error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = check_group_model_permission(&state, &api_key, &model_owned)?;

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    ensure_model_priced(&state, &model_owned)?;

    let input = body.get("input").and_then(|i| i.as_str()).ok_or_else(|| {
        error_response(
            "invalid_input",
            "Missing input field",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let voice = body
        .get("voice")
        .and_then(|v| v.as_str())
        .unwrap_or("default");

    let cf_body = serde_json::json!({
        "model": model,
        "input": input,
        "voice": voice,
    });

    match state
        .api_client
        .run_audio_speech(&model_owned, cf_body)
        .await
    {
        Ok(audio) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;

            state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

            // 计费扣减（按次计价，问题 2/5/6）
            let cost = charge_usage(
                &state,
                &api_key,
                &model_owned,
                &model_owned,
                &billing_group,
                0,
                0,
                0,
            );

            // 事后限流记账（按次计价，记 0 token）
            rate_bundle.commit_tokens(0).await;

            // 记录请求日志（功能 1）— 文本转语音按次计价，tokens 设为 0
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model_owned.clone();
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(&model_owned, "unknown", "ok", latency_ms);

            Ok(Json(serde_json::json!({
                "audio_base64": BASE64.encode(&audio.bytes),
                "content_type": audio.content_type
            })))
        }
        Err(e) => {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.model = model_owned.clone();
            log.latency_ms = latency_ms;
            log.status_code = 502;
            log.error_msg = Some(format!("Text-to-speech error: {e}"));
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(&model_owned, "unknown", "error", latency_ms);
            // 渠道故障通知
            state
                .notify_service
                .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                    channel_name: model_owned.clone(),
                    error: format!("Text-to-speech error: {e}"),
                });
            Err(error_response(
                "api_error",
                &format!("Text-to-speech error: {e}"),
                StatusCode::BAD_GATEWAY,
            ))
        }
    }
}
/// 在字节切片中查找子序列的起始位置（从 `from` 开始）。
///
/// 用于 multipart 字节级解析（B04）：`str::find` 无法处理二进制 body，
/// 需要等价的字节序列搜索。
pub(crate) fn find_sub_slice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    let mut i = from;
    while i <= last_start {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}
/// 解析 multipart 表单中的音频文件。
///
/// B04 修复：multipart 必须按字节级解析——音频是任意二进制数据，
/// 旧实现用 `String::from_utf8_lossy` 解码整个 body，无效字节会被替换为
/// U+FFFD（6 EF BF BD）从而破坏音频内容；且音频流中恰好出现与 boundary
/// 相同的字节序列时还会截断数据。本实现按 boundary 字节序列分割 part，
/// 头部（Content-Disposition 等）按 UTF-8 解析，文件 body 保持原始字节。
pub(crate) fn parse_multipart_audio(
    bytes: &bytes::Bytes,
    boundary: &str,
) -> Result<(bytes::Bytes, String, String), (StatusCode, Json<Value>)> {
    let mut file_data: Option<(bytes::Bytes, String)> = None;
    let mut model = String::from("whisper");

    let boundary_tag = format!("--{boundary}");
    let delim = boundary_tag.as_bytes();

    // 收集所有 boundary 出现位置，相邻两个位置之间即一个 part 的字节区间
    let mut positions: Vec<usize> = Vec::new();
    let mut cursor = 0usize;
    while let Some(pos) = find_sub_slice(bytes, delim, cursor) {
        positions.push(pos);
        cursor = pos + delim.len();
    }

    for window in positions.windows(2) {
        let start = window[0] + delim.len();
        let end = window[1];
        if start >= end {
            continue;
        }
        let mut part = &bytes[start..end];
        // 去掉 boundary 之后紧跟的 CRLF（part 的开始）
        if let Some(p) = part.strip_prefix(b"\r\n") {
            part = p;
        }
        // part 结尾与下一 boundary 之间的 CRLF 属于分隔符语法，不属于 body
        if let Some(p) = part.strip_suffix(b"\r\n") {
            part = p;
        }
        // 终止 boundary（--boundary--）后剩余 "--"，跳过
        if part.is_empty() || part == b"--" {
            continue;
        }

        // 头部与 body 之间以空行（\r\n\r\n）分隔
        let Some(header_end) = find_sub_slice(part, b"\r\n\r\n", 0) else {
            continue;
        };
        let header_bytes = &part[..header_end];
        let body_bytes = &part[header_end + 4..];

        // 头部为文本字段（Content-Disposition 等），按 UTF-8 解析是安全的
        let headers = String::from_utf8_lossy(header_bytes);
        if headers.contains("name=\"model\"") {
            model = String::from_utf8_lossy(body_bytes).trim().to_string();
        } else if headers.contains("name=\"file\"") || headers.contains("name=\"audio\"") {
            let filename = headers
                .split(';')
                .find_map(|s| {
                    let s = s.trim();
                    s.strip_prefix("filename=\"")
                        .or_else(|| s.strip_prefix("filename="))
                        .map(|f| f.trim_matches('"').to_string())
                })
                .unwrap_or_else(|| "audio.wav".to_string());

            // body 保持原始字节，不经过任何文本解码
            let data = bytes::Bytes::copy_from_slice(body_bytes);
            file_data = Some((data, filename));
        }
    }

    file_data
        .ok_or_else(|| {
            error_response(
                "invalid_request",
                "Missing audio file in request",
                StatusCode::BAD_REQUEST,
            )
        })
        .map(|(data, filename)| (data, model, filename))
}
