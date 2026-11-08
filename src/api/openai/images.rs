use axum::{
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};

use super::*;
use crate::api::common::extract_client_ip;
use crate::bridge::{BridgeContext, ImageEditFormPart};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;

/// POST /v1/images/generations - 图片生成
///
/// 通过 Bridge::generate_image 委托给 Bridge 处理
pub(crate) async fn handle_images_generations(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body.get("model").and_then(|m| m.as_str()).ok_or_else(|| {
        openai_error_response(
            "invalid_model",
            "Missing model field",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();

    // log_body 开关：客户端请求体快照（一次序列化，失败/成功路径共用）
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(body.to_string())
    } else {
        None
    };

    // 网络层闸门：管理员关闭网络层时拒绝转发
    let _nl_cfg = crate::api::admin::network::load_network_config(&state);
    if !_nl_cfg.enabled {
        return Err(openai_error_response(
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
            return Err(openai_error_response(
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

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover
    let candidates = resolve_bridges(
        &state,
        &model_owned,
        api_key.provider_filter().as_deref(),
        None,
    );
    if candidates.is_empty() {
        return Err(openai_error_response(
            "no_bridge",
            "No bridge available",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());

    // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut used_upstream: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid, ch_ref) in candidates {
        // 标记渠道已使用（问题 4）
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
        let upstream = resolve_upstream_model(&model_owned, ch_ref.as_ref(), &state.model_mapper);
        if upstream != model_owned && ensure_model_priced(&state, &upstream).is_err() {
            tracing::warn!(
                "images mapping: {model_owned} -> {upstream} unpriced, skip channel {cid:?}"
            );
            last_error = Some(crate::bridge::BridgeError::Config(format!(
                "mapped upstream '{upstream}' has no price configured"
            )));
            continue;
        }
        // 按上游模型名改写请求 body（bridges 从 body["model"] 构造上游请求）
        let mut attempt_body = body.clone();
        attempt_body["model"] = Value::String(upstream.clone());
        match bridge.generate_image(&attempt_body, &ctx).await {
            Ok(result) => {
                result_opt = Some(result);
                used_channel_id = cid;
                used_upstream = Some(upstream);
                break;
            }
            Err(e) => {
                if !is_retryable_bridge_error(&e) {
                    // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                    last_error = Some(e);
                    used_channel_id = cid;
                    break;
                }
                if let Some(cid) = &cid {
                    state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                }
                tracing::warn!("images failover: channel {cid:?} failed: {e}, trying next channel");
                last_error = Some(e);
            }
        }
    }
    let result = match result_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for images request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.channel_name = used_channel_id
                .as_ref()
                .and_then(|cid| state.channel_store.get(cid))
                .map(|c| c.name.clone());
            log.model = used_upstream.clone().unwrap_or_else(|| model_owned.clone());
            log.origin_model = if used_upstream.as_deref() == Some(model_owned.as_str()) {
                None
            } else {
                Some(model_owned.clone())
            };
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            // log_body：失败路径快照——上游原始错误信息进 response_body
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(e.to_string()),
                    None,
                );
            }
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(
                &model_owned,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "error",
                latency_ms,
            );
            // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model_owned.clone(),
                        error: e.to_string(),
                    });
            }
            return Err(bridge_error_response(e));
        }
    };
    // #36：b64_json 资产落盘（开关默认关，关闭时 no-op、响应不变）
    let mut result = result;
    persist_generated_assets(&state, &mut result).await;
    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

        // 计费扣减（按次计价，问题 2/5/6）——日志用映射后的上游模型名；
        // 计价键双名感知：对外请求名优先、上游名兜底
        let billing_model = used_upstream.as_deref().unwrap_or(&model_owned);
        let cost = charge_usage(
            &state,
            &api_key,
            &model_owned,
            billing_model,
            &billing_group,
            0,
            0,
            0,
        );

        // 事后限流记账（按次计价，记 1 个请求 token 占位以维持 RPM 一致性）
        rate_bundle.commit_tokens(0).await;

        // 记录请求日志（功能 1）— 图片生成按次计价，tokens 设为 0
        let mut log = crate::log::RequestLog::new();
        log.user_id = api_key.user_id.clone();
        log.key_id = Some(api_key.id.clone());
        log.channel_id = used_channel_id.clone();
        log.channel_name = used_channel_id
            .as_ref()
            .and_then(|cid| state.channel_store.get(cid))
            .map(|c| c.name.clone());
        log.model = billing_model.to_string();
        log.origin_model = if used_upstream.as_deref() == Some(model_owned.as_str()) {
            None
        } else {
            Some(model_owned.clone())
        };
        log.channel_cost = compute_channel_cost(
            &state,
            used_channel_id.as_deref(),
            &model_owned,
            billing_model,
            &billing_group,
            0,
            0,
            cost,
        );
        log.input_tokens = 0;
        log.output_tokens = 0;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        // log_body：快照 = 请求体 + 响应 JSON（b64 图片体积大，靠 4KB 截断保护）
        if let Some(rb) = debug_request_body.as_deref() {
            log.debug = crate::log::LogDebugSnapshot::new(
                Some(rb.to_string()),
                Some(result.to_string()),
                None,
            );
        }
        state.log_store.record_request(log);

        // Prometheus 指标（图片按次计价，tokens 记 0）
        crate::metrics::global().record_request(
            billing_model,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );

        Ok(Json(result))
    }
}

/// 单张生成资产的大小上限（落盘与下载共用）。生成图片通常 ≤10MiB，
/// 32MiB 留出余量；超出即拒绝（落盘侧跳过该张、下载侧返回 413）。
const MAX_ASSET_BYTES: u64 = 32 * 1024 * 1024;

/// multipart 单请求字段数上限（防海量 0 字节字段刷内存/循环）。
const MAX_MULTIPART_FIELDS: usize = 128;

/// POST /v1/images/edits - 图片编辑（参考图 + prompt + 可选 mask）
///
/// OpenAI 兼容 multipart 接口。鉴权/网络层闸门/限流/分组/计费/日志与
/// generations 完全同一套；上游转发走 Bridge::edit_image（协议转换在
/// Bridge，与 generate_image 的 JSON 透传形态对齐，渠道级模型映射同样生效）。
///
/// 有界（#36）：multipart 按 chunk 边读边限（照 backup_crypto 的循环
/// 写法），不先全量读进内存再判大小；总量上限 = `max_request_body_mb`
/// 与全局 DefaultBodyLimit 双兜底。
pub(crate) async fn handle_images_edits(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);
    let body_limit = current_body_limit_bytes(&state) as u64;

    // 边读边限收集表单项：文本字段与文件字段统一走 chunk 循环
    let mut parts: Vec<ImageEditFormPart> = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(|e| {
        openai_error_response(
            "invalid_request",
            &format!("Failed to read multipart form: {e}"),
            StatusCode::BAD_REQUEST,
        )
    })? {
        if parts.len() >= MAX_MULTIPART_FIELDS {
            return Err(openai_error_response(
                "invalid_request",
                "Too many multipart fields",
                StatusCode::BAD_REQUEST,
            ));
        }
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(|f| f.to_string());
        let content_type = field
            .content_type()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|e| {
            openai_error_response(
                "invalid_request",
                &format!("Failed to read field: {e}"),
                StatusCode::BAD_REQUEST,
            )
        })? {
            if append_bounded(&mut buf, &chunk, body_limit).is_err() {
                return Err(openai_error_response(
                    "request_too_large",
                    &format!("multipart body exceeds limit of {body_limit} bytes"),
                    StatusCode::PAYLOAD_TOO_LARGE,
                ));
            }
        }
        match filename {
            Some(filename) => parts.push(ImageEditFormPart::File {
                name,
                filename,
                content_type,
                data: bytes::Bytes::from(buf),
            }),
            None => {
                let value = String::from_utf8(buf).map_err(|_| {
                    openai_error_response(
                        "invalid_request",
                        "multipart text field is not UTF-8",
                        StatusCode::BAD_REQUEST,
                    )
                })?;
                parts.push(ImageEditFormPart::Text { name, value });
            }
        }
    }

    let model = parts
        .iter()
        .find_map(|p| match p {
            ImageEditFormPart::Text { name, value } if name == "model" => {
                Some(value.trim().to_string())
            }
            _ => None,
        })
        .filter(|m| !m.is_empty())
        .ok_or_else(|| {
            openai_error_response(
                "invalid_model",
                "Missing model field",
                StatusCode::BAD_REQUEST,
            )
        })?;
    // OpenAI edits 必填：image 文件 + prompt
    if !parts.iter().any(|p| {
        matches!(p, ImageEditFormPart::File { name, .. } if name == "image" || name == "image[]")
    }) {
        return Err(openai_error_response(
            "invalid_request",
            "Missing image file in request",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !parts.iter().any(|p| {
        matches!(p, ImageEditFormPart::Text { name, value } if name == "prompt" && !value.trim().is_empty())
    }) {
        return Err(openai_error_response(
            "invalid_request",
            "Missing prompt field",
            StatusCode::BAD_REQUEST,
        ));
    }

    let api_key = verify_api_key_full(&state, &headers, &model)?;
    let model_owned = model;

    // log_body 开关：multipart 不快照原始字节（大文件），记字段摘要
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(debug_parts_summary(&parts))
    } else {
        None
    };

    // 网络层闸门：管理员关闭网络层时拒绝转发
    let _nl_cfg = crate::api::admin::network::load_network_config(&state);
    if !_nl_cfg.enabled {
        return Err(openai_error_response(
            "network_layer_disabled",
            "网络层已停用：推理转发被管理员关闭。请稍后重试。",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    // 限流检查：鉴权后、推理前
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
            return Err(openai_error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            ));
        }
    };

    // 校验用户分组模型权限并解析计费分组
    let billing_group = check_group_model_permission(&state, &api_key, &model_owned)?;

    // B09：前置校验模型定价
    ensure_model_priced(&state, &model_owned)?;

    // B06：获取候选渠道列表，失败时逐个 failover（与 generations 同语义）
    let candidates = resolve_bridges(
        &state,
        &model_owned,
        api_key.provider_filter().as_deref(),
        None,
    );
    if candidates.is_empty() {
        return Err(openai_error_response(
            "no_bridge",
            "No bridge available",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());

    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut used_upstream: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid, ch_ref) in candidates {
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        let upstream = resolve_upstream_model(&model_owned, ch_ref.as_ref(), &state.model_mapper);
        if upstream != model_owned && ensure_model_priced(&state, &upstream).is_err() {
            tracing::warn!(
                "images-edits mapping: {model_owned} -> {upstream} unpriced, skip channel {cid:?}"
            );
            last_error = Some(crate::bridge::BridgeError::Config(format!(
                "mapped upstream '{upstream}' has no price configured"
            )));
            continue;
        }
        // 按上游模型名改写 multipart 中的 model 文本字段（Bytes 为引用计数，克隆廉价）
        let mut attempt_parts = parts.clone();
        for p in attempt_parts.iter_mut() {
            if let ImageEditFormPart::Text { name, value } = p {
                if name == "model" {
                    *value = upstream.clone();
                }
            }
        }
        match bridge.edit_image(&attempt_parts, &ctx).await {
            Ok(result) => {
                result_opt = Some(result);
                used_channel_id = cid;
                used_upstream = Some(upstream);
                break;
            }
            Err(e) => {
                if !is_retryable_bridge_error(&e) {
                    last_error = Some(e);
                    used_channel_id = cid;
                    break;
                }
                if let Some(cid) = &cid {
                    state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                }
                tracing::warn!(
                    "images-edits failover: channel {cid:?} failed: {e}, trying next channel"
                );
                last_error = Some(e);
            }
        }
    }
    let result = match result_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for images edits request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.channel_name = used_channel_id
                .as_ref()
                .and_then(|cid| state.channel_store.get(cid))
                .map(|c| c.name.clone());
            log.model = used_upstream.clone().unwrap_or_else(|| model_owned.clone());
            log.origin_model = if used_upstream.as_deref() == Some(model_owned.as_str()) {
                None
            } else {
                Some(model_owned.clone())
            };
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(e.to_string()),
                    None,
                );
            }
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(
                &model_owned,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "error",
                latency_ms,
            );
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model_owned.clone(),
                        error: e.to_string(),
                    });
            }
            return Err(bridge_error_response(e));
        }
    };
    // #36：b64_json 资产落盘（开关默认关，关闭时 no-op、响应不变）
    let mut result = result;
    persist_generated_assets(&state, &mut result).await;
    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

        // 计费扣减（按次计价，与 generations 一致）
        let billing_model = used_upstream.as_deref().unwrap_or(&model_owned);
        let cost = charge_usage(
            &state,
            &api_key,
            &model_owned,
            billing_model,
            &billing_group,
            0,
            0,
            0,
        );

        rate_bundle.commit_tokens(0).await;

        // 请求日志：图片编辑按次计价，tokens 设为 0
        let mut log = crate::log::RequestLog::new();
        log.user_id = api_key.user_id.clone();
        log.key_id = Some(api_key.id.clone());
        log.channel_id = used_channel_id.clone();
        log.channel_name = used_channel_id
            .as_ref()
            .and_then(|cid| state.channel_store.get(cid))
            .map(|c| c.name.clone());
        log.model = billing_model.to_string();
        log.origin_model = if used_upstream.as_deref() == Some(model_owned.as_str()) {
            None
        } else {
            Some(model_owned.clone())
        };
        log.channel_cost = compute_channel_cost(
            &state,
            used_channel_id.as_deref(),
            &model_owned,
            billing_model,
            &billing_group,
            0,
            0,
            cost,
        );
        log.input_tokens = 0;
        log.output_tokens = 0;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        if let Some(rb) = debug_request_body.as_deref() {
            log.debug = crate::log::LogDebugSnapshot::new(
                Some(rb.to_string()),
                Some(result.to_string()),
                None,
            );
        }
        state.log_store.record_request(log);

        crate::metrics::global().record_request(
            billing_model,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );

        Ok(Json(result))
    }
}

/// log_body 快照的 multipart 摘要：文本字段原样、文件字段只记
/// 文件名/类型/字节数（图片体积大，避免撑爆日志快照）。
fn debug_parts_summary(parts: &[ImageEditFormPart]) -> String {
    let mut obj = serde_json::Map::new();
    for p in parts {
        match p {
            ImageEditFormPart::Text { name, value } => {
                obj.insert(name.clone(), Value::String(value.clone()));
            }
            ImageEditFormPart::File {
                name,
                filename,
                content_type,
                data,
            } => {
                obj.insert(
                    name.clone(),
                    serde_json::json!({
                        "filename": filename,
                        "content_type": content_type,
                        "bytes": data.len(),
                    }),
                );
            }
        }
    }
    Value::Object(obj).to_string()
}

/// 边读边限的单步追加：累计超过 `max` 返回 Err 且不写入（#36，
/// 照 backup_crypto「边读边限」口径抽出的可测纯函数）。
fn append_bounded(buf: &mut Vec<u8>, chunk: &[u8], max: u64) -> Result<(), ()> {
    if buf.len() as u64 + chunk.len() as u64 > max {
        return Err(());
    }
    buf.extend_from_slice(chunk);
    Ok(())
}

/// 资产文件名白名单：只认本服务产出的 `img-<uuid>.png|jpg|webp` 形态。
///
/// 字符集不含路径分隔符/冒号（防绝对路径与 Windows 盘符），显式拒绝
/// `..`（防穿越）；读取侧另行拒绝符号链接。
pub(crate) fn is_safe_asset_name(name: &str) -> bool {
    name.starts_with("img-")
        && (name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".webp"))
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
        && !name.contains("..")
}

/// 下载目标是否为可用的常规文件（#36）。
///
/// 传 `symlink_metadata`（不跟随）的结果：符号链接可能是被放置到
/// 资产目录、指向目录之外文件的越权读取入口，一律拒绝；目录也拒绝
/// （读取必然失败，提前给出 404）。抽成纯函数以便单测钉住。
fn is_downloadable_asset(meta: &std::fs::Metadata) -> bool {
    !meta.is_symlink() && meta.is_file()
}

/// 把响应中的 b64_json 图片落盘到 `{data_dir}/generated_images/`（#36）。
///
/// 开关 `server.save_generated_images` 默认关——关闭时函数是 no-op，
/// 响应原样返回（与既往行为一致）。落盘成功后在对应 data[i] 上补
/// `local_path`（`/v1/images/assets/<file>`，需携带有效 API Key 下载）。
/// 单张超限/解码失败/写盘失败只跳过该张，不影响响应其余部分。
pub(crate) async fn persist_generated_assets(state: &AppState, result: &mut Value) {
    if !state
        .config_manager
        .get()
        .await
        .server
        .save_generated_images
    {
        return;
    }
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let assets_dir = data_dir.join("generated_images");
    if tokio::fs::create_dir_all(&assets_dir).await.is_err() {
        tracing::warn!("generated_images: failed to create dir, skip persisting");
        return;
    }
    let Some(data_arr) = result.get_mut("data").and_then(|d| d.as_array_mut()) else {
        return;
    };
    for item in data_arr.iter_mut() {
        let Some(b64) = item.get("b64_json").and_then(|b| b.as_str()) else {
            continue;
        };
        let Ok(bytes_img) = BASE64.decode(b64) else {
            continue;
        };
        if bytes_img.len() as u64 > MAX_ASSET_BYTES {
            continue;
        }
        let filename = format!("img-{}.png", uuid::Uuid::new_v4());
        if tokio::fs::write(assets_dir.join(&filename), &bytes_img)
            .await
            .is_err()
        {
            continue;
        }
        if let Some(obj) = item.as_object_mut() {
            obj.insert(
                "local_path".to_string(),
                Value::String(format!("/v1/images/assets/{filename}")),
            );
        }
    }
}

/// GET /v1/images/assets/:name - 下载落盘的生成资产（#36）。
///
/// 鉴权：有效 API Key（不校验模型白名单，对齐 /v1/models 的 listing 鉴权）。
/// 防穿越：文件名只认 `is_safe_asset_name` 白名单形态；符号链接拒绝；
/// 单文件有界（`MAX_ASSET_BYTES`，先查 metadata 长度再读）。
pub(crate) async fn handle_generated_asset(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_api_key_listing(&state, &headers)?;
    if !is_safe_asset_name(&name) {
        return Err(openai_error_response(
            "invalid_request",
            "invalid asset name",
            StatusCode::BAD_REQUEST,
        ));
    }
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let path = data_dir.join("generated_images").join(&name);
    // symlink_metadata 读链接本身（不跟随）：符号链接不是本服务产出的形态
    let meta = tokio::fs::symlink_metadata(&path).await.map_err(|_| {
        openai_error_response("asset_not_found", "asset not found", StatusCode::NOT_FOUND)
    })?;
    if !is_downloadable_asset(&meta) {
        return Err(openai_error_response(
            "asset_not_found",
            "asset not found",
            StatusCode::NOT_FOUND,
        ));
    }
    if meta.len() > MAX_ASSET_BYTES {
        return Err(openai_error_response(
            "request_too_large",
            &format!("asset exceeds limit of {MAX_ASSET_BYTES} bytes"),
            StatusCode::PAYLOAD_TOO_LARGE,
        ));
    }
    let bytes = tokio::fs::read(&path).await.map_err(|_| {
        openai_error_response("asset_not_found", "asset not found", StatusCode::NOT_FOUND)
    })?;
    let mime = mime_guess::from_path(&name)
        .first_or_octet_stream()
        .to_string();
    let mut resp = Response::new(axum::body::Body::from(bytes));
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_str(&mime).map_err(|_| {
            openai_error_response(
                "invalid_request",
                "invalid asset name",
                StatusCode::BAD_REQUEST,
            )
        })?,
    );
    Ok(resp)
}

#[cfg(test)]
mod asset_guard_tests {
    use super::{append_bounded, is_downloadable_asset, is_safe_asset_name};

    #[test]
    fn path_traversal_names_are_rejected() {
        // 相对路径穿越
        assert!(!is_safe_asset_name("../etc/passwd"));
        assert!(!is_safe_asset_name("img-a/../secret.png"));
        assert!(!is_safe_asset_name("img-..png"));
        // 绝对路径 / Windows 盘符 / 反斜杠：不在白名单字符集内
        assert!(!is_safe_asset_name("/etc/passwd"));
        assert!(!is_safe_asset_name("C:\\Windows\\system32\\config\\sam"));
        assert!(!is_safe_asset_name("img-x\\..\\..\\evil.png"));
        // 非本服务产出形态
        assert!(!is_safe_asset_name("export-20260101.aigxbak"));
        assert!(!is_safe_asset_name("generated_images/img-x.png"));
        // 合法名（img-<uuid>.png）
        assert!(is_safe_asset_name(
            "img-6f9619ff-8b86-d011-b42d-00cf4fc964ff.png"
        ));
    }

    #[test]
    fn bounded_append_rejects_oversize() {
        let mut buf = Vec::new();
        let max = 16u64;
        assert!(append_bounded(&mut buf, &[0u8; 8], max).is_ok());
        assert!(append_bounded(&mut buf, &[0u8; 8], max).is_ok());
        assert_eq!(buf.len(), 16);
        // 再来 1 字节即超限，被拒且不写入
        assert!(append_bounded(&mut buf, &[0u8], max).is_err());
        assert_eq!(buf.len(), 16);
    }

    /// 下载目标形态：常规文件放行，目录与符号链接拒绝。
    ///
    /// 符号链接用真实文件系统构造（`#[cfg(unix)]`；Windows 建链需
    /// 特权，跳过该段）。防穿越补的是这一层——`is_safe_asset_name`
    /// 只管文件名字符集，管不到「名字合法但指向目录之外」的链接。
    #[test]
    fn symlink_and_directory_assets_are_rejected() {
        let dir = std::env::temp_dir().join(format!("aigx-asset-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let real = dir.join("img-real.png");
        std::fs::write(&real, b"png").expect("write asset");

        // 常规文件：放行
        assert!(is_downloadable_asset(
            &std::fs::symlink_metadata(&real).expect("stat asset")
        ));
        // 目录：拒绝
        assert!(!is_downloadable_asset(
            &std::fs::symlink_metadata(&dir).expect("stat dir")
        ));

        // 符号链接指向资产目录之外的敏感文件：拒绝（symlink_metadata 不跟随）
        #[cfg(unix)]
        {
            let outside = std::env::temp_dir().join(format!("aigx-out-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&outside).expect("create outside dir");
            let secret = outside.join("secret.png");
            std::fs::write(&secret, b"secret").expect("write secret");
            let link = dir.join("img-link.png");
            std::os::unix::fs::symlink(&secret, &link).expect("create symlink");
            let meta = std::fs::symlink_metadata(&link).expect("stat link");
            assert!(meta.is_symlink());
            assert!(!is_downloadable_asset(&meta));
            let _ = std::fs::remove_dir_all(&outside);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
