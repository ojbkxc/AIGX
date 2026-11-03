use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};

use super::*;
use crate::api::common::extract_client_ip;
use crate::bridge::BridgeContext;
use serde_json::Value;

/// POST /v1/rerank - 重排序
///
/// 通过 Bridge::rerank 委托给 Bridge 处理（P1：RAG 客户刚需端点）。
/// 请求体（Cohere/Jina 兼容形状）：
///   { "model": "...", "query": "...", "documents": [{"content": "..."}...], "top_n": N? }
/// 响应体：{ "results": [{"index": i, "relevance_score": s}...], "usage": {...} }
pub(crate) async fn handle_rerank(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = body.get("model").and_then(|m| m.as_str()).ok_or_else(|| {
        error_response(
            "invalid_model",
            "Missing model field",
            StatusCode::BAD_REQUEST,
        )
    })?;
    let query = body.get("query").and_then(|q| q.as_str()).ok_or_else(|| {
        error_response(
            "invalid_query",
            "Missing query field",
            StatusCode::BAD_REQUEST,
        )
    })?;

    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();
    let query_owned = query.to_string();

    // log_body 开关：客户端请求体快照（一次序列化，失败/成功路径共用）
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(body.to_string())
    } else {
        None
    };

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

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover
    let candidates = resolve_bridges(&state, &model_owned);
    if candidates.is_empty() {
        return Err(error_response(
            "no_bridge",
            "No bridge available",
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }

    // documents 宽容解析：[{content}] | [纯字符串]
    let documents: Vec<crate::bridge::RerankDocument> = body
        .get("documents")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        Some(crate::bridge::RerankDocument {
                            content: s.to_string(),
                        })
                    } else {
                        v.get("content").and_then(|c| c.as_str()).map(|s| {
                            crate::bridge::RerankDocument {
                                content: s.to_string(),
                            }
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    if documents.is_empty() {
        return Err(error_response(
            "invalid_documents",
            "documents must be a non-empty array",
            StatusCode::BAD_REQUEST,
        ));
    }
    let top_n: Option<usize> = body
        .get("top_n")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let rerank_req = crate::bridge::RerankRequest {
        model: model_owned.clone(),
        query: query_owned,
        documents,
        top_n,
    };
    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());
    // G1：rerank 计费对齐两段式——请求发起前按 query+documents 精确估算预留，
    // 完成后按上游实际 prompt token 结算；失败路径统一 release。
    let estimated_text = {
        let mut s = String::with_capacity(
            rerank_req
                .documents
                .iter()
                .map(|d| d.content.len() + 1)
                .sum::<usize>()
                + rerank_req.query.len(),
        );
        s.push_str(&rerank_req.query);
        for doc in &rerank_req.documents {
            s.push(char::from(10));
            s.push_str(&doc.content);
        }
        s
    };
    let estimated_prompt_tokens =
        crate::token_estimate::count_text(&model_owned, &estimated_text) as u64;
    let reservation = match reserve_usage(
        &state,
        &api_key,
        &model_owned,
        &model_owned,
        &billing_group,
        estimated_prompt_tokens,
        0,
    ) {
        Ok(r) => r,
        Err(e) => return Err(e),
    };

    // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
    let mut response_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut used_upstream: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid, ch_ref) in candidates {
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
        let upstream = resolve_upstream_model(&model_owned, ch_ref.as_ref(), &state.model_mapper);
        if upstream != model_owned && ensure_model_priced(&state, &upstream).is_err() {
            tracing::warn!(
                "rerank mapping: {model_owned} -> {upstream} unpriced, skip channel {cid:?}"
            );
            last_error = Some(crate::bridge::BridgeError::Config(format!(
                "mapped upstream '{upstream}' has no price configured"
            )));
            continue;
        }
        // 按上游模型名构建本次尝试的请求（bridges 从 req.model 构造上游 body）
        let mut attempt_req = rerank_req.clone();
        attempt_req.model = upstream.clone();
        match bridge.rerank(&attempt_req, &ctx).await {
            Ok(resp) => {
                response_opt = Some(resp);
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
                tracing::warn!("rerank failover: channel {cid:?} failed: {e}, trying next channel");
                last_error = Some(e);
            }
        }
    }
    let mut response = match response_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for rerank request".into(),
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
            if status_code.as_u16() >= 500 {
                state
                    .notify_service
                    .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: model_owned.clone(),
                        error: e.to_string(),
                    });
            }
            release_reservation(&state, &api_key, &reservation);
            return Err(bridge_error_response(e));
        }
    };

    // top_n 截断（上游未截断时由网关兜底）
    if let Some(n) = top_n {
        response.results.truncate(n);
    }

    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        let prompt_tokens = response.usage.prompt_tokens as u64;
        state
            .usage_tracker
            .accumulate(prompt_tokens, 0, 0, 0, 0, 0.0);

        // 计费扣减：G1 对齐两段式——用 settle_usage 结算之前预留的配额。
        let billing_model = used_upstream.as_deref().unwrap_or(&model_owned);
        let cost = settle_usage(
            &state,
            &api_key,
            &model_owned,
            billing_model,
            &billing_group,
            &reservation,
            prompt_tokens,
            0,
            // embedding 无缓存语义
            0,
            None,
        );

        // 事后限流记账（TPM）
        rate_bundle.commit_tokens(prompt_tokens).await;

        // 记录请求日志（功能 1）
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
            prompt_tokens,
            0,
            cost,
        );
        log.input_tokens = prompt_tokens;
        log.output_tokens = 0;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        // log_body：快照 = 请求体 + 响应 JSON
        if let Some(rb) = debug_request_body.as_deref() {
            let resp_snapshot = serde_json::json!({
                "results": response.results.iter().map(|r| serde_json::json!({
                    "index": r.index,
                    "relevance_score": r.relevance_score,
                })).collect::<Vec<_>>(),
                "usage": { "prompt_tokens": response.usage.prompt_tokens, "total_tokens": response.usage.total_tokens },
            });
            log.debug = crate::log::LogDebugSnapshot::new(
                Some(rb.to_string()),
                Some(resp_snapshot.to_string()),
                None,
            );
        }
        state.log_store.record_request(log);

        // Prometheus 指标
        crate::metrics::global().record_request(
            billing_model,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );
        crate::metrics::global().record_tokens(billing_model, "prompt", prompt_tokens);

        Ok(Json(serde_json::json!({
            "model": &model_owned,
            "results": response.results.iter().map(|r| serde_json::json!({
                "index": r.index,
                "relevance_score": r.relevance_score,
            })).collect::<Vec<_>>(),
            "usage": {
                "prompt_tokens": response.usage.prompt_tokens,
                "total_tokens": response.usage.total_tokens,
            }
        })))
    }
}
