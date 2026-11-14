use super::*;
use crate::api::common::extract_client_ip;
use crate::bridge::{tool_repair, Bridge, BridgeContext, ChatFormat};
use crate::channel::ChannelStore;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

/// 构建本次请求的尝试计划：主模型候选在前，异源模型候选在后。
///
/// 破甲总开关或异源开关关闭时，计划与既有候选列表逐字相同（零行为变化，
/// 且零额外分配——候选标签借用调用方的 model/cfg，不逐候选克隆 String）。
/// 异源候选按 (模型名, 渠道 id) 去重：fallback 与主模型同名、或 fallback
/// 列表自身重复时，不重复追加同一渠道（防配置笔误导致同渠道重复尝试）。
///
/// `pub(crate)`：anthropic `/v1/messages` 端点复用同一计划构建（两协议同口径，
/// 避免两份 fallback 逻辑漂移）。
pub(crate) fn build_attempt_plan<'a>(
    state: &AppState,
    model: &'a str,
    candidates: Vec<BridgeCandidate>,
    provider_filter: Option<&[String]>,
    needs: crate::channel::routing_policy::RequestNeeds,
    cfg: &'a crate::purge::PurgeSettings,
) -> Vec<(&'a str, BridgeCandidate)> {
    let mut plan: Vec<(&'a str, BridgeCandidate)> = candidates
        .into_iter()
        .map(|candidate| (model, candidate))
        .collect();
    if cfg.enabled && cfg.cross_source_fallback {
        for fallback in &cfg.fallback_models {
            // 与主模型同口径：未定价的异源模型不进入候选（B09 防免费用量）
            if ensure_model_priced(state, fallback).is_err() {
                continue;
            }
            let extra = resolve_bridges_with_affinity_needs(
                state,
                fallback,
                None,
                provider_filter,
                None,
                needs,
            );
            for candidate in extra {
                let cid = candidate.1.as_deref();
                let duplicate = plan
                    .iter()
                    .any(|(label, (_, pc, _))| *label == fallback.as_str() && pc.as_deref() == cid);
                if duplicate {
                    continue;
                }
                plan.push((fallback.as_str(), candidate));
            }
        }
    }
    // 重试上限：单请求最多尝试多少个候选（含首轮）。只在破甲总开关打开时
    // 截断——关闭时保持既有「候选列表全跑」（零行为变化）。
    if cfg.enabled {
        let cap = cfg.retry_limit.max(1) as usize;
        plan.truncate(cap);
    }
    plan
}

/// failover 前 Retry-After 等待上限（秒）：上游告知的等待时间超过此值时
/// 不再等待——failover 总预算只有 90s，且给请求凭空增加几十秒延迟不如
/// 立即切换下一个候选（失败渠道已被冷却 max(60, retry_after) 秒，不会
/// 被本轮或近期请求再次选中）。
pub(crate) const RETRY_AFTER_WAIT_CAP_SECS: u64 = 10;

/// 从桥接错误提取 Retry-After 秒数（P1）：上游 429/503 常带 `Retry-After`
/// 头（`capture_upstream_error_http` 已解析进 BridgeError::UpstreamStatus
/// .retry_after，见 bridge/mod.rs 的 parse_retry_after）。无该字段返回 0。
pub(crate) fn bridge_error_retry_after_secs(e: &crate::bridge::BridgeError) -> u64 {
    match e {
        crate::bridge::BridgeError::UpstreamStatus { retry_after, .. } => {
            retry_after.map(|d| d.as_secs()).unwrap_or(0)
        }
        _ => 0,
    }
}

/// POST /v1/chat/completions - 聊天补全
///
/// 使用 Bridge/Hub 模式分发请求，参考 aisix 的 chat_completions 处理流程：
/// 1. 认证 → 2. 解析 ChatFormat → 3. Hub 分发 Bridge → 4. 调用 Bridge::chat/chat_stream
pub(crate) async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return openai_error_response(
                "invalid_model",
                "Missing model field",
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP（参照 new-api token.go）
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // 网络层闸门：管理员关闭网络层时拒绝转发（管理面不受影响）
    if let Err(resp) = crate::api::admin::network::network_layer_gate(&state) {
        return *resp;
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
            return openai_error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组（问题 5）
    let billing_group = match check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = ensure_model_priced(&state, &model) {
        return e.into_response();
    }
    // 破甲控制台：进入转发链路的请求计入响应总数（进程内计数）
    crate::purge::record_response();

    // 阶段2：亲和性 session 标识——优先 body.user 字段（OpenAI SDK 常见透传），
    // 否则用 api_key 所属 user_id，保证同用户会话粘到同一渠道（保留上游 KV 缓存）
    let session_id: Option<String> = body
        .get("user")
        .and_then(|u| u.as_str())
        .map(String::from)
        .or_else(|| api_key.user_id.clone());

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover。
    // 阶段2：带亲和性选取——粘性窗口内命中渠道置顶
    // Phase1-B 路由回放：开关开启时随选路捕获过滤原因/策略快照（关闭=零分配）
    let mut route_capture = if crate::config::route_replay_enabled() {
        Some(crate::api::openai::RouteCapture::new())
    } else {
        None
    };
    // #40 §1 接缝（会话型端点）：从请求体推出能力需求（tools/images/video）
    // 供选路能力过滤——chat 侧 messages 列表 + 顶层 tools/tool_choice
    let body_needs = super::needs::from_chat_body(&body);
    let provider_filter = api_key.provider_filter();
    let candidates = resolve_bridges_with_affinity_needs(
        &state,
        &model,
        session_id.as_deref(),
        provider_filter.as_deref(),
        route_capture.as_mut(),
        body_needs,
    );
    if candidates.is_empty() {
        return openai_error_response(
            "no_bridge",
            "No bridge available for the requested model",
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .into_response();
    }

    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // 响应缓存键（仅非流式）：必须在 chat_req 构建前算好——chat_req 会把
    // tools/tool_choice/extra 等透传字段 take 出 body，键投影必须看到原始
    // 请求体。P1 性能：64 位哈希替代整棵请求子树字符串做 key
    // （见 response_cache_key 注释）。
    let cache_key = if is_stream {
        None
    } else {
        Some(response_cache_key(&api_key.id, &model, &body))
    };

    // 解析消息列表
    let messages = parse_messages_with_dev(
        body.get("messages"),
        state.gateway_prompt.snapshot().developer_role_compat,
    )
    .unwrap_or_default();
    if messages.is_empty() {
        return openai_error_response(
            "invalid_messages",
            "No valid messages",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    // 出站指纹脱敏（对照 Agent2API sanitizeBlacklistFingerprints）**不在这里做**：
    // provider 作用域要按「即将发送的那一家」判定（A2A `send_body` 同口径），而此处
    // 选渠道尚未发生 —— 见两处 failover 循环里的 `sanitize_messages_for_provider`。

    // 构建 ChatFormat
    // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）：
    // passthrough 不动（零开销）；custom 替换 / append 追加网关提示词；
    // 按模型覆盖优先于全局设置（AIGX 入口只有模型名，故按模型挂）
    let mut messages = match crate::prompt::apply_prompt_for(
        &model,
        &messages,
        &state.gateway_prompt.snapshot(),
    ) {
        Some(injected) => injected,
        None => messages,
    };

    // 破甲控制台：请求治理（system 归一化 / 死区词改写 / 结构化提示 / 技能注入）
    let purge_cfg = state.purge.snapshot();
    let purge_applied = crate::purge::apply_messages(&mut messages, &purge_cfg);
    if purge_applied.skill_hit {
        crate::purge::record_skill_hit();
    }
    if messages.is_empty() {
        return openai_error_response(
            "invalid_messages",
            "No valid messages after purge normalization",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    let chat_req = ChatFormat {
        model: model.clone(),
        messages,
        // 透传字段 take 接管：tools 原本 `arr.to_vec()` 深拷贝整棵 schema
        // 树（工具定义可达数百 KB），tool_choice/web_search_options 原本
        // `.cloned()` 再深拷贝——body 在此之后不再被读取（缓存键已在
        // is_stream 下方按原口径提前算好），take 把所有权零拷贝移交
        // ChatFormat，build_body 侧仅剩最后一跳序列化。
        tools: match body.get_mut("tools").map(std::mem::take) {
            Some(Value::Array(arr)) => Some(arr),
            _ => None,
        },
        max_tokens: body
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| u32::try_from(v).unwrap_or(u32::MAX)),
        temperature: body.get("temperature").and_then(|v| v.as_f64()),
        top_p: body.get("top_p").and_then(|v| v.as_f64()),
        stream: is_stream,
        top_k: None,
        stop: body.get("stop").and_then(|s| s.as_array()).map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        }),
        tool_choice: body.get_mut("tool_choice").map(std::mem::take),
        reasoning_effort: crate::purge::clamp_reasoning(
            body.get("reasoning_effort")
                .and_then(|v| v.as_str())
                .map(String::from)
                .or_else(|| state.model_mapper.reasoning_for(&model)),
            &purge_cfg,
        ),
        web_search_options: body.get_mut("web_search_options").map(std::mem::take),
        // 透传白名单字段：这些是 OpenAI 协议标准参数，缓存键已认识它们
        // （presence_penalty/frequency_penalty/seed/response_format），但此前
        // extra: None 恒丢弃，导致不同 seed/response_format 的请求命中同一缓存
        // 且效果相同。通过 extra 承载，build_body 会合并进请求体。
        // stream_options 仅流式有意义（include_usage 终帧），非流式不提取。
        extra: {
            let mut obj = serde_json::Map::new();
            for k in [
                "response_format",
                "seed",
                "presence_penalty",
                "frequency_penalty",
                "n",
                "parallel_tool_calls",
                "logit_bias",
                "max_completion_tokens",
                "service_tier",
            ] {
                if let Some(v) = body.get_mut(k) {
                    let v = std::mem::take(v);
                    if !v.is_null() {
                        obj.insert(k.to_string(), v);
                    }
                }
            }
            if is_stream {
                if let Some(v) = body.get_mut("stream_options") {
                    let v = std::mem::take(v);
                    if !v.is_null() {
                        obj.insert("stream_options".to_string(), v);
                    }
                }
            }
            if obj.is_empty() {
                None
            } else {
                Some(Value::Object(obj))
            }
        },
    };

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    // 在途请求登记：管理员可在日志页看到这条请求并终止它。
    // guard 在函数返回时 drop → 自动注销（成功 / 失败 / 客户端断连三条路径都覆盖）。
    //
    // `cancel_signal` 会被挂到响应流上：管理员点「终止」时置位，
    // GuardedStream::poll_next 立刻返回 None 收尾，上游连接随流 drop 断开。
    let (_inflight_id, inflight_guard) = state.inflight.begin(
        model.clone(),
        is_stream,
        api_key.user_id.clone(),
        Some(api_key.id.clone()),
    );
    let cancel_signal = inflight_guard.cancel_signal();
    // 非流式请求没有可中断的流：终止只在流式路径生效（登记仍然保留，
    // 管理员至少能看到「这条非流式请求还在跑」）。

    // P1：配额预留/结算两段式——在请求发起前预留预估费用，
    // 请求完成后按实际 token 结算，多预留的部分归还。
    // 估算：prompt 用 token 计数器精确估算；completion 跟随请求 max_tokens
    // （未指定时保守默认 256，上限 4096 防止恶意超大 max_tokens 占满配额）。
    // P1 性能：BPE 估算（纯 CPU，每 64KB 切片一次正则编码）移到阻塞线程
    // 池执行，避免每请求阻塞 tokio worker 数毫秒到数十毫秒；
    // chat_req.clone() 是一次 O(n) memcpy，远廉价于编码本身。
    let estimated_prompt_tokens =
        crate::token_estimate::count_chat_prompt_blocking(model.clone(), chat_req.clone()).await
            as u64;
    let estimated_completion_tokens = chat_req.max_tokens.unwrap_or(256).clamp(256, 4096) as u64;
    let reservation = match reserve_usage(
        &state,
        &api_key,
        &model,
        &model,
        &billing_group,
        estimated_prompt_tokens,
        estimated_completion_tokens,
    ) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    // P1-7：调度决策回放——记录候选渠道列表
    let candidate_channel_ids: Vec<String> = candidates
        .iter()
        .filter_map(|(_, cid, _)| cid.clone())
        .collect();

    // log_body 开关：客户端请求体快照（一次序列化，失败/成功路径共用）
    let debug_request_body = if crate::config::log_body_enabled() {
        Some(crate::bridge::chat_format_debug_json(&chat_req))
    } else {
        None
    };

    if is_stream {
        // B06：failover 循环——依次尝试候选渠道建立流，仅对上游可重试错误切换
        let mut stream_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        // 连接稳定性：failover 预算（候选数 + 总时间上限），与 anthropic 面同口径
        let mut failover_budget = crate::api::failover::FailoverBudget::new();
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // auto-continue：留住成功轮的桥与（映射后）请求，续写轮复用同一渠道
        let mut cont_bridge: Option<Arc<dyn Bridge>> = None;
        let mut cont_req: Option<ChatFormat> = None;
        // #32 路由回放：真实上游调用计数（unpriced 跳过不计——没发请求）
        // 亲和命中判定与 resolve 同源（本渠道被粘滞置顶）
        let mut attempt_counter: u32 = 0;
        let affinity_hit_id = session_id
            .as_deref()
            .and_then(|sid| state.channel_store.affinity_cache().lookup(sid, &model));
        // #47：AttemptStart 级守卫——覆盖「预留已建、上游请求已发出、首事件
        // 未到」的断开窗口（E1/E2 实测漏记漏计费）。每轮真实调用前更新
        // 渠道/模型归因；计费接管点见各 handoff()。
        let mut attempt_guard = super::billing::AttemptStartGuard {
            state: state.clone(),
            api_key: api_key.clone(),
            requested_model: model.clone(),
            group: billing_group.clone(),
            reservation: reservation.clone(),
            channel_id: String::new(),
            channel_name: String::new(),
            model: model.clone(),
            chat_req: chat_req.clone(),
            request_id: request_id.clone(),
            ip: client_ip.clone(),
            user_id: api_key.user_id.clone(),
            handled: false,
        };
        // 破甲异源递进：主模型候选在前，配置了 fallback_models 时追加异源候选
        let attempt_plan = build_attempt_plan(
            &state,
            &model,
            candidates,
            provider_filter.as_deref(),
            body_needs,
            &purge_cfg,
        );
        for (plan_model, (bridge, cid, ch_ref)) in attempt_plan {
            if !failover_budget.can_attempt() {
                // 连接稳定性：预算耗尽（候选数或总时间达上限）停止 failover
                last_error = Some(crate::bridge::BridgeError::Config(
                    failover_budget.exhausted_reason(),
                ));
                break;
            }
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）。
            // 每个渠道都需单独解析——同原始名在不同渠道可能映射到不同上游。
            let upstream = resolve_upstream_model(plan_model, ch_ref.as_ref(), &state.model_mapper);
            // 防"映射指向未定价模型 → 免费用量"漏洞：仅当映射后名字变化时补查一次
            if upstream.as_str() != plan_model && ensure_model_priced(&state, &upstream).is_err() {
                // 该渠道映射的上游无定价，跳过此渠道（failover 下一候选）
                tracing::warn!(
                    "chat_stream mapping: {plan_model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            // 出站脱敏：在这一家即将发送之前判定（provider 作用域，A2A 同口径）
            sanitize_attempt_messages(&state, ch_ref.as_ref(), &mut attempt_req.messages);
            attempt_counter += 1;
            let attempt_index = attempt_counter;
            if attempt_index > 1 {
                crate::purge::record_retry();
            }
            let attempt_start = std::time::Instant::now();
            let attempt_started_at = chrono::Utc::now().timestamp();
            let sticky_hit =
                plan_model == model.as_str() && affinity_hit_id.as_deref() == cid.as_deref();
            // #47：更新最后尝试的渠道/模型归因（Drop 时 499 落在此渠道头上）
            attempt_guard.channel_id = cid.clone().unwrap_or_default();
            attempt_guard.channel_name =
                ch_ref.as_ref().map(|c| c.name.clone()).unwrap_or_default();
            attempt_guard.model = upstream.clone();
            attempt_guard.chat_req = attempt_req.clone();
            // T2：SSE 首事件守卫——建流成功后等第一个事件（30s 上限），
            // 上游挂起/建流后立刻断流 → 按失败处理（熔断记账 + failover）
            let attempt = match bridge.chat_stream(&attempt_req, &ctx).await {
                Ok(s) => crate::bridge::first_event_or_timeout(s).await,
                Err(e) => Err(e),
            };
            match attempt {
                Ok(s) => {
                    // 破甲异源递进（流式侧）：建流成功不等于没有拒答——在把流交给
                    // 客户端之前先读少量前缀做拒答判定。命中则丢弃本轮流、按可重试
                    // 失败处理（客户端尚未收到任何字节），继续下一候选（通常是异源模型）。
                    // 只在「异源递进已开启 + 主模型 + 已达触发轮次 + 配了异源模型」
                    // 时探测：其余情形走零改动路径（原流逐字下发）。
                    let s = if purge_cfg.enabled
                        && purge_cfg.cross_source_fallback
                        && plan_model == model.as_str()
                        && attempt_index >= purge_cfg.fallback_trigger_attempt
                        && !purge_cfg.fallback_models.is_empty()
                    {
                        match peek_for_refusal(s).await {
                            Peek::Refused => {
                                // 拒答不是渠道不健康：与非流式路径同口径，不记
                                // 渠道失败/熔断，只计拒绝样本并换下一候选。
                                crate::purge::record_refusal();
                                if let Some(capture) = route_capture.as_mut() {
                                    capture.push_attempt(crate::log::AttemptDetail {
                                        index: attempt_index,
                                        channel_id: cid.clone().unwrap_or_default(),
                                        channel_name: ch_ref
                                            .as_ref()
                                            .map(|c| c.name.clone())
                                            .unwrap_or_default(),
                                        provider: ch_ref
                                            .as_ref()
                                            .map(|c| c.channel_type.as_str().to_string())
                                            .unwrap_or_default(),
                                        account: String::new(),
                                        model: upstream.clone(),
                                        sticky_hit,
                                        started_at: attempt_started_at,
                                        finished_at: Some(chrono::Utc::now().timestamp()),
                                        status: Some(200),
                                        error: Some("refusal detected".into()),
                                        notice: Some("refusal detected in stream prefix".into()),
                                    });
                                }
                                tracing::warn!(
                                    "chat_stream refusal detected (prefix), switching model: {plan_model} -> next candidate"
                                );
                                last_error = Some(crate::bridge::BridgeError::Config(
                                    "refusal detected; switching to fallback model".into(),
                                ));
                                continue;
                            }
                            Peek::Pass(s) => s,
                        }
                    } else {
                        s
                    };
                    // #47：首事件已到 → 计费接管点（StreamUsageGuard 稍后构造，
                    // 流中断开由其 Drop 兜底，本守卫退出不再记账）
                    attempt_guard.handoff();
                    if attempt_index > 1 {
                        // 提前收效：重试后成功
                        crate::purge::record_early_success();
                    }
                    // #32 路由回放：建流成功轮明细（流式终态在计费守卫，此处
                    // 记「建流成功」结论；phase 由终态字段承载）
                    if let Some(capture) = route_capture.as_mut() {
                        capture.push_attempt(crate::log::AttemptDetail {
                            index: attempt_index,
                            channel_id: cid.clone().unwrap_or_default(),
                            channel_name: ch_ref
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            provider: ch_ref
                                .as_ref()
                                .map(|c| c.channel_type.as_str().to_string())
                                .unwrap_or_default(),
                            account: String::new(),
                            model: upstream.clone(),
                            sticky_hit,
                            started_at: attempt_started_at,
                            finished_at: Some(chrono::Utc::now().timestamp()),
                            status: Some(200),
                            error: None,
                            notice: None,
                        });
                    }
                    // 阶段2：流建立成功——记入健康/亲和（断路器成功、延迟 EMA、
                    // 空响应计数清零、粘性路由建立）。流中途失败由计费守卫兜底，
                    // 此处只记"建流成功"。
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_success_with_sticky(
                            c,
                            Some(&upstream),
                            attempt_start.elapsed().as_millis() as u64,
                            session_id.as_deref(),
                            crate::channel::routing_policy::StickyMode::sticky_enabled_from(
                                &state.routing_policy.snapshot(),
                            ),
                        );
                    }
                    stream_opt = Some(s);
                    used_channel_id = cid;
                    // 记录映射结果：流计费守卫用上游名计价/记账
                    used_upstream = Some(upstream);
                    cont_bridge = Some(bridge);
                    cont_req = Some(attempt_req);
                    break;
                }
                Err(e) => {
                    // P1-7：记录被过滤的渠道及原因
                    if let Some(c) = &cid {
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&upstream),
                            ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    // #32 路由回放：失败轮明细（含不可重试的终态轮）
                    if let Some(capture) = route_capture.as_mut() {
                        capture.push_attempt(crate::log::AttemptDetail {
                            index: attempt_index,
                            channel_id: cid.clone().unwrap_or_default(),
                            channel_name: ch_ref
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            provider: ch_ref
                                .as_ref()
                                .map(|c| c.channel_type.as_str().to_string())
                                .unwrap_or_default(),
                            account: String::new(),
                            model: upstream.clone(),
                            sticky_hit,
                            started_at: attempt_started_at,
                            finished_at: Some(chrono::Utc::now().timestamp()),
                            status: Some(e.http_status()),
                            error: Some(e.to_string()),
                            notice: None,
                        });
                    }
                    if !is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        // #47：终态失败由下方既有失败日志+预留释放路径记账 → 接管
                        attempt_guard.handoff();
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    // P1：冷却时长 = max(60, Retry-After 解析值)——上游显式
                    // 告知的恢复时间优先于固定 60s 基础值（指数退避在此基础上倍增）；
                    // failover 前按解析值等待（计入预算）：仅当上游明确要求
                    // 等待时才等，且上限 RETRY_AFTER_WAIT_CAP_SECS 并受
                    // failover 总预算剩余时间约束。
                    let retry_after = bridge_error_retry_after_secs(&e);
                    if let Some(cid) = &cid {
                        state
                            .channel_store
                            .mark_cooldown(cid, e.to_string(), retry_after.max(60));
                    }
                    let wait = retry_after.min(RETRY_AFTER_WAIT_CAP_SECS);
                    if wait > 0
                        && failover_budget.remaining() >= std::time::Duration::from_secs(wait)
                    {
                        tracing::info!(
                            "chat_stream failover: waiting {wait}s for Retry-After before switching channel"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    }
                    tracing::warn!(
                        "chat_stream failover: channel {cid:?} failed: {e}, trying next channel"
                    );
                    last_error = Some(e);
                }
            }
            // 连接稳定性：记录本轮候选尝试（达上限后下轮入口停止）
            failover_budget.record_attempt();
        }
        let stream = match stream_opt {
            Some(s) => s,
            None => {
                // #47：全败由下方既有失败日志+预留释放路径记账 → 接管
                attempt_guard.handoff();
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for streaming request".into(),
                    )
                });
                crate::purge::record_failure(crate::purge::classify_failure(&e.to_string()));
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.channel_name = used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone());
                log.model = used_upstream.clone().unwrap_or_else(|| model.clone());
                log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                };
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
                // （as_mut：attempt_details 按值取出，as_ref 借用不可）
                if let Some(capture) = route_capture.as_mut() {
                    log.filtered_channels
                        .extend(capture.filtered.iter().cloned());
                    log.route_config = capture.config.clone();
                    log.route_truncated = capture.filtered_truncated;
                    log.attempt_details = std::mem::take(&mut capture.attempts);
                    log.attempt_details_truncated = capture.attempts_truncated;
                    log.account_skips = capture
                        .account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                }
                // log_body：失败路径快照——上游原始错误信息进 response_body
                if debug_request_body.is_some() {
                    log.debug = crate::log::LogDebugSnapshot::new(
                        debug_request_body.clone(),
                        Some(e.to_string()),
                        None,
                    );
                }
                log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                crate::metrics::global().record_request(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    "error",
                    latency_ms,
                );
                // P1 指标：错误类型标签（区分 429/5xx/超时等终态错误）
                crate::metrics::global().record_request_error(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    e.error_type(),
                );
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state
                        .notify_service
                        .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                            channel_name: model.clone(),
                            error: e.to_string(),
                        });
                }
                // G5：失败路径释放预留——渠道全败/客户端错误时解冻，
                // 防止预留永久冻结（2026-09-08 二轮审查）
                release_reservation(&state, &api_key, &reservation);
                return bridge_error_response(e).into_response();
            }
        };
        // auto-continue（流式）：开关打开才包装，在 ChatChunk 层透传全部轮次；
        // 关着时下发字节与接线前逐字一致（零开销）。计费/文本累积在包装的
        // 下游（map 层），天然看到跨轮拼接的整体。
        let stream = match (crate::continuation::plan(), cont_bridge, cont_req) {
            (Some(max_rounds), Some(bridge), Some(req)) => {
                crate::continuation::wrap_stream(stream, bridge, req, ctx.clone(), max_rounds)
            }
            _ => stream,
        };
        {
            // 累积输出文本用于流结束时估算 token（问题 3）
            let acc = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
            let acc_for_map = acc.clone();
            // 工具按次调用计数表（流式过程统计 tool_use 起始块，计费时折算附加费）
            let tool_calls = std::sync::Arc::new(parking_lot::Mutex::new(
                crate::pricing::ToolCallCounts::new(),
            ));
            let tool_calls_for_map = tool_calls.clone();

            // 流中途出错标志：Err 分支置位，final_event 据此不发 [DONE]、
            // metrics 记 error、日志 status_code 记 500（此前出错仍按成功处理）
            let stream_errored = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stream_errored_for_map = stream_errored.clone();

            let sse_stream = stream.flat_map(move |chunk_result| match chunk_result {
                Ok(chunk) => {
                    // 累积内容文本用于事后估算 completion tokens
                    // （含 thinking 推理内容与工具调用参数，与 cf-ai-gw 的
                    //   reasoningTokens/output 计量口径对齐）
                    let mut buf = acc_for_map.lock();
                    if let Some(text) = &chunk.delta.content {
                        crate::token_estimate::push_capped(&mut buf, text);
                    }
                    if let Some(reasoning) = &chunk.delta.reasoning {
                        crate::token_estimate::push_capped(&mut buf, reasoning);
                    }
                    if let Some(tool_calls) = &chunk.delta.tool_calls {
                        for tc in tool_calls {
                            if let Some(name) = &tc.function_name {
                                crate::token_estimate::push_capped(&mut buf, name);
                            }
                            if let Some(args) = &tc.arguments {
                                crate::token_estimate::push_capped(&mut buf, args);
                            }
                            // 仅统计 tool_use 起始帧（带 id/name 的首帧），
                            // 避免对参数分片帧重复计数（与 new-api
                            // `content_block_start tool_use` 的计数口径一致）。
                            if tc.function_name.is_some() && tc.id.is_some() {
                                if let Some(name) = &tc.function_name {
                                    *tool_calls_for_map.lock().entry(name.clone()).or_insert(0) +=
                                        1;
                                }
                            }
                        }
                    }
                    drop(buf);
                    // 帧直写：serde 结构体 + to_writer 一次序列化进帧缓冲，
                    // 零中间 Value 树（旧路径每 chunk 建 delta 树 + 外层树
                    // 两棵再 to_string，此处只剩帧缓冲本身的一次分配）。
                    let delta = SseDeltaFrame {
                        content: chunk.delta.content.as_deref().unwrap_or_default(),
                        // 流式推理内容增量透传给 OpenAI 客户端（OpenAI 协议字段）
                        reasoning_content: chunk.delta.reasoning.as_deref(),
                        tool_calls: chunk.delta.tool_calls.as_ref().map(|tool_calls| {
                            // 按 index 聚合流式参数分片，并对完整 arguments 做三层自修复
                            // （同一次 chunk 内的多个 index 并行聚合，修复彼此独立）
                            let repaired = tool_repair::accumulate_tool_call_arguments(tool_calls);
                            // Antigravity 思考签名随 tool_call 带出：位置取 Google
                            // 自家 OpenAI 兼容端点的官方非标字段
                            // extra_content.google.thought_signature。多数客户端会
                            // 丢弃该字段 —— 那时请求侧按服务端缓存回填
                            // （antigravity_signature_cache）。
                            let signature =
                                tool_calls.iter().find_map(|tc| tc.signature.as_deref());
                            repaired
                                .into_iter()
                                .map(|(idx, id, name, args)| SseToolCallFrame {
                                    index: idx,
                                    id,
                                    kind: "function",
                                    function: SseFunctionFrame {
                                        name,
                                        arguments: args,
                                    },
                                    extra_content: signature.map(|sig| SseExtraContent {
                                        google: SseGoogleSig {
                                            thought_signature: sig,
                                        },
                                    }),
                                })
                                .collect()
                        }),
                    };
                    let frame = SseChunkFrame {
                        id: &chunk.id,
                        object: "chat.completion.chunk",
                        created: chrono::Utc::now().timestamp(),
                        model: &chunk.model,
                        choices: [SseChoiceFrame {
                            index: 0,
                            delta,
                            finish_reason: chunk.finish_reason.as_ref().map(finish_reason_str),
                        }],
                    };
                    // to_writer 写 Vec<u8> 不会失败；序列化输出必为合法 UTF-8
                    let mut buf = Vec::with_capacity(256);
                    serde_json::to_writer(&mut buf, &frame)
                        .expect("serde_json to_writer 写 Vec<u8> 不失败");
                    let sse_data = String::from_utf8(buf).expect("serde_json 序列化输出必为 UTF-8");
                    let mut evs = vec![Ok::<_, Infallible>(Event::default().data(sse_data))];
                    // 流式 usage 终帧：客户端请求 stream_options.include_usage=true 时
                    // 回传 usage 帧（choices 空数组，OpenAI 官方契约）。此前
                    // chat.rs 忽略 chunk.usage，客户端收不到 usage 帧。
                    if let Some(usage) = &chunk.usage {
                        let usage_data = serde_json::json!({
                            "id": chunk.id,
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": chunk.model,
                            "choices": [],
                            "usage": {
                                "prompt_tokens": usage.prompt_tokens,
                                "completion_tokens": usage.completion_tokens,
                                "total_tokens": usage.total_tokens,
                            }
                        })
                        .to_string();
                        evs.push(Ok::<_, Infallible>(Event::default().data(usage_data)));
                    }
                    futures::stream::iter(evs)
                }
                Err(e) => {
                    tracing::error!("Stream chunk error: {e}");
                    // 标记流中途出错：final_event 据此不发 [DONE]、
                    // metrics 记 error、日志记 500
                    stream_errored_for_map.store(true, std::sync::atomic::Ordering::SeqCst);
                    // 标准 SSE error 事件格式（与 OpenAI API 错误结构一致）：
                    //   event: error
                    //   data: {"error":{"message":"...","type":"...","code":"..."}}
                    // T3：发给客户端的错误消息先脱敏（日志侧保留原始消息）
                    let safe_msg = crate::error_translate::sanitize_error_message(&e.to_string());
                    let err_data = serde_json::json!({
                        "error": {
                            "message": safe_msg,
                            "type": e.error_type(),
                            "code": e.error_type(),
                        }
                    })
                    .to_string();
                    futures::stream::iter(vec![Ok::<_, Infallible>(
                        Event::default().event("error").data(err_data),
                    )])
                }
            });

            // 流结束：估算 token、扣费、commit_tokens、记录日志（问题 3）
            // B05：计费状态由后缀事件与 Drop 守卫共享（原子标志互斥）——
            // 正常结束时后缀事件计费；客户端断连导致流被 drop 时由守卫兜底。
            // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照（开关关闭
            // 时 route_capture 为 None，两行均零开销）
            if let Some(capture) = route_capture.as_ref() {
                filtered_channels.extend(capture.filtered.iter().cloned());
            }
            let route_config = route_capture.as_ref().and_then(|c| c.config.clone());
            let route_truncated = route_capture
                .as_ref()
                .map(|c| c.filtered_truncated)
                .unwrap_or(0);
            // #32：流式路径把 failover 循环里捕获的逐次尝试明细移交给计费守卫
            // （成功轮的终态在守卫里写，故此处按值取出而非借用）。
            let (attempt_details, attempt_details_truncated) = route_capture
                .as_mut()
                .map(|c| {
                    (
                        std::mem::take(&mut c.attempts),
                        std::mem::take(&mut c.attempts_truncated),
                    )
                })
                .unwrap_or_default();
            let account_skips = route_capture
                .as_ref()
                .map(|c| {
                    c.account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                })
                .unwrap_or_default();
            let billing = Arc::new(StreamBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: used_upstream.clone().unwrap_or_else(|| model.clone()),
                origin_model: if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                },
                group: billing_group.clone(),
                // Arc 共享请求快照：final_event 的 spawn_blocking 预估算可
                // 廉价 clone（P1 性能——BPE 估算移出 tokio worker）
                chat_req: Arc::new(chat_req.clone()),
                tool_calls: tool_calls.clone(),
                acc: acc.clone(),
                charged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
                channel_name: used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone()),
                candidate_channels: candidate_channel_ids.clone(),
                filtered_channels: filtered_channels.clone(),
                route_config,
                route_truncated,
                attempt_details,
                attempt_details_truncated,
                account_skips,
                reservation: Some(reservation),
                debug_request_body,
            });
            let billing_fin = billing.clone();
            // P1 指标：TTFT 的共享句柄（BridgeContext.first_response_ms 由
            // debug_traffic 在上游响应到达时写入，与日志同源）
            let ttft_arc = ctx.first_response_ms.clone();
            let final_event = async move {
                // P1 性能：完成侧估算（prompt + completion）在阻塞线程池
                // 预估算一次，两分支共用——async 上下文内同步 BPE 编码
                // （每 64KB 切片一次正则编码）会阻塞 tokio worker。
                // 估算输入在此时刻读取（acc 已不再变更），与同步路径
                // 逐字一致（见 token_estimate::count_*_blocking 的一致性测试）。
                let (est_prompt, est_completion) = {
                    let output_text = billing_fin.acc.lock().clone();
                    let model = billing_fin.model.clone();
                    let chat_req = billing_fin.chat_req.clone();
                    let (pt, ct) = tokio::join!(
                        crate::token_estimate::count_chat_prompt_blocking_arc(
                            model.clone(),
                            chat_req,
                        ),
                        crate::token_estimate::count_text_blocking(model, output_text),
                    );
                    (pt as u64, ct as u64)
                };
                // 原子抢占计费权：断连场景守卫可能已兜底计费，双保险防重复
                let (prompt_tokens, completion_tokens) = if !billing_fin
                    .charged
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let errored = stream_errored.load(std::sync::atomic::Ordering::SeqCst);
                    let (pt, ct) = billing_fin.finalize_with_token_estimates(
                        errored,
                        est_prompt,
                        est_completion,
                    );
                    // C1 健康档案：流正常完成，按真实总时延补记延迟样本。
                    // 建流时已 note_success（清零断路器）；此处只补延迟，
                    // 不重复计成功次数（见 health_archive::note_latency）。
                    if let Some(channel_id) = &billing_fin.channel_id {
                        billing_fin
                            .state
                            .channel_store
                            .health_archive()
                            .note_latency(
                                channel_id,
                                billing_fin.request_start.elapsed().as_millis() as u64,
                            );
                    }
                    // 事后限流记账（TPM）
                    rate_bundle.commit_tokens(pt + ct).await;
                    (pt, ct)
                } else {
                    // 守卫已兜底计费：这里只取估算值供指标上报，不再重复扣费
                    (est_prompt, est_completion)
                };

                // Prometheus 指标（流中途出错记 error，不再恒 ok）
                let errored = stream_errored.load(std::sync::atomic::Ordering::SeqCst);
                crate::metrics::global().record_request(
                    &billing_fin.model,
                    billing_fin.channel_id.as_deref().unwrap_or("unknown"),
                    if errored { "error" } else { "ok" },
                    billing_fin.request_start.elapsed().as_millis() as u64,
                );
                if errored {
                    // 流中途出错：错误类型为流级类别（具体错误已在 error
                    // 事件里发给客户端，此处只分桶）
                    crate::metrics::global().record_request_error(
                        &billing_fin.model,
                        billing_fin.channel_id.as_deref().unwrap_or("unknown"),
                        "stream_error",
                    );
                }
                // P1 指标：TTFT（首字节延迟）
                if let Some(ttft) = ttft_arc.lock().ok().and_then(|g| *g) {
                    crate::metrics::global().record_ttft(&billing_fin.model, ttft);
                }
                crate::metrics::global().record_tokens(&billing_fin.model, "prompt", prompt_tokens);
                crate::metrics::global().record_tokens(
                    &billing_fin.model,
                    "completion",
                    completion_tokens,
                );

                // 流中途出错不发 [DONE]：已发 error 事件，客户端应停止读取；
                // 发 [DONE] 会让严格 SDK 把半截流当正常完成
                if errored {
                    Ok::<_, Infallible>(Event::default().data(""))
                } else {
                    Ok::<_, Infallible>(Event::default().data("[DONE]"))
                }
            };
            let combined = sse_stream.chain(futures::stream::once(final_event));

            // B05：包装守卫流——流被 drop（含客户端断连）时兜底计费
            let guarded = GuardedStream {
                inner: Box::pin(combined),
                _guard: StreamUsageGuard::new(billing),
                // 管理员「终止在途请求」时立刻收尾下游流
                cancel: Some(cancel_signal),
                // guard 移进流：流存活期间才算是「在途」。函数返回响应后
                // guard 不能立刻 drop，否则列表里会立刻消失。
                _inflight: Some(inflight_guard),
            };

            Sse::new(guarded)
                // 保活用 SSE 注释行（` : keepalive`），不是 `event: ping`：
                // OpenAI 标准流里没有 ping 事件，硬塞会让严格解析的 SDK 收到
                // 未知事件；注释行按规范被忽略，但能刷新 TCP/CDN 空闲计时器。
                .keep_alive(
                    KeepAlive::new()
                        .interval(crate::bridge::SSE_KEEPALIVE_INTERVAL)
                        .text(crate::bridge::SSE_KEEPALIVE_TEXT),
                )
                .into_response()
        }
    } else {
        // 响应缓存：非流式请求尝试缓存命中。
        // P1 修复：key 必须包含用户隔离维度与全部生成参数——
        // 原先仅 model/messages/temperature/max_tokens，导致
        // (a) 跨用户响应泄漏（他人相同 prompt 可命中你的缓存）
        // (b) tools/tool_choice/stop/top_p 不同的请求命中同一条目，返回错误结果
        // （键在函数前段已按原口径算好，见 is_stream 下方的 cache_key）
        let cache_key = cache_key.expect("非流式路径必有缓存键");
        if let Some(cached) = state.response_cache.get(&cache_key).await {
            tracing::debug!("response cache hit for model {}", model);
            let prompt_tokens = cached
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let completion_tokens = cached
                .get("usage")
                .and_then(|u| u.get("completion_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            // 计费只走一条路：按次订阅固定扣额度；否则按 token 量计费。
            // 命中意味着整个 prompt 由网关内部响应缓存直接回放（未消耗上游资源），
            // prompt_tokens 全部按「缓存读」价格计费；cache_price 未配置的模型
            // 该部分为 0，维持旧的命中免费行为（升级零破坏）。
            let cache_cost = if is_count_billing(&state, &api_key) {
                crate::config::billing_flat_quota()
            } else {
                match state.pricing_store.calculate_cost_quoted_with_cache(
                    &model,
                    0,
                    0,
                    prompt_tokens,
                    &billing_group,
                ) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("cache hit billing: {e}, billing as 0");
                        0
                    }
                }
            };
            // P0-1：缓存命中必须结算此前预留的配额，否则预留冻结到 TTL 才释放。
            // 走与正常路径一致的分配逻辑（订阅池 → 用户钱包 → key），按
            // cache_cost 结算并逐笔摘除预留：预留归零，实扣 == cache_cost；
            // cache_cost==0（未配 cache_price）时即全额退还预留。
            let cache_cost = settle_cache_hit(&state, &api_key, &reservation, cache_cost);
            state
                .usage_tracker
                .accumulate(0, 0, 0, prompt_tokens, 0, 0.0);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = Some("cache".to_string());
            log.model = model.clone();
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = cache_cost;
            log.latency_ms = request_start.elapsed().as_millis() as u64;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            log.error_msg = Some("cache_hit".to_string());
            log.cache_hit = true;
            // log_body：缓存命中快照 = 请求体 + 缓存的响应 JSON
            if debug_request_body.is_some() {
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(cached.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);
            crate::metrics::global().record_request(
                &model,
                "cache",
                "ok",
                request_start.elapsed().as_millis() as u64,
            );
            let mut resp = Json(cached).into_response();
            resp.headers_mut()
                .insert("x-aigx-cache", "hit".parse().unwrap());
            return resp;
        }
        // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
        let mut response_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut used_upstream: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        // 连接稳定性：failover 预算（候选数 + 总时间上限），与 anthropic 面同口径
        let mut failover_budget = crate::api::failover::FailoverBudget::new();
        let mut filtered_channels: Vec<crate::log::FilteredChannel> = Vec::new();
        // auto-continue：留住成功轮的桥与（映射后）请求，续写轮复用同一渠道
        let mut cont_bridge: Option<Arc<dyn Bridge>> = None;
        let mut cont_req: Option<ChatFormat> = None;
        // #32 路由回放：真实上游调用计数（unpriced 跳过不计——没发请求）
        let mut attempt_counter: u32 = 0;
        // 亲和命中判定与 resolve 同源（本渠道被粘滞置顶）
        let affinity_hit_id = session_id
            .as_deref()
            .and_then(|sid| state.channel_store.affinity_cache().lookup(sid, &model));
        // #47：AttemptStart 级守卫（同流式分支；E1 非流式断开实测漏记漏计费）
        let mut attempt_guard = super::billing::AttemptStartGuard {
            state: state.clone(),
            api_key: api_key.clone(),
            requested_model: model.clone(),
            group: billing_group.clone(),
            reservation: reservation.clone(),
            channel_id: String::new(),
            channel_name: String::new(),
            model: model.clone(),
            chat_req: chat_req.clone(),
            request_id: request_id.clone(),
            ip: client_ip.clone(),
            user_id: api_key.user_id.clone(),
            handled: false,
        };
        // 破甲异源递进：主模型候选在前，配置了 fallback_models 时追加异源候选
        let attempt_plan = build_attempt_plan(
            &state,
            &model,
            candidates,
            provider_filter.as_deref(),
            body_needs,
            &purge_cfg,
        );
        for (plan_model, (bridge, cid, ch_ref)) in attempt_plan {
            if !failover_budget.can_attempt() {
                // 连接稳定性：预算耗尽（候选数或总时间达上限）停止 failover
                last_error = Some(crate::bridge::BridgeError::Config(
                    failover_budget.exhausted_reason(),
                ));
                break;
            }
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            // 渠道级模型映射：按选定渠道解析上游真实模型名（= 价格表名）
            let upstream = resolve_upstream_model(plan_model, ch_ref.as_ref(), &state.model_mapper);
            if upstream.as_str() != plan_model && ensure_model_priced(&state, &upstream).is_err() {
                tracing::warn!(
                    "chat mapping: {plan_model} -> {upstream} unpriced, skip channel {cid:?}"
                );
                last_error = Some(crate::bridge::BridgeError::Config(format!(
                    "mapped upstream '{upstream}' has no price configured"
                )));
                continue;
            }
            // 按上游模型名构造本次尝试的请求（bridges 从 chat_req.model 构造上游 body）
            let mut attempt_req = chat_req.clone();
            attempt_req.model = upstream.clone();
            // 出站脱敏：在这一家即将发送之前判定（provider 作用域，A2A 同口径）
            sanitize_attempt_messages(&state, ch_ref.as_ref(), &mut attempt_req.messages);
            attempt_counter += 1;
            let attempt_index = attempt_counter;
            if attempt_index > 1 {
                crate::purge::record_retry();
            }
            let attempt_started_at = chrono::Utc::now().timestamp();
            let sticky_hit =
                plan_model == model.as_str() && affinity_hit_id.as_deref() == cid.as_deref();
            let attempt_start = std::time::Instant::now();
            // #47：更新最后尝试的渠道/模型归因（Drop 时 499 落在此渠道头上）
            attempt_guard.channel_id = cid.clone().unwrap_or_default();
            attempt_guard.channel_name =
                ch_ref.as_ref().map(|c| c.name.clone()).unwrap_or_default();
            attempt_guard.model = upstream.clone();
            attempt_guard.chat_req = attempt_req.clone();
            match bridge.chat(&attempt_req, &ctx).await {
                Ok(resp) => {
                    // #47：响应已收 → 计费接管点（下方 settle_usage 记账）
                    attempt_guard.handoff();
                    // 破甲异源递进：到达触发轮次后，非流式响应若判定为拒绝，
                    // 不结算本轮，直接尝试后续候选（通常是异源模型）。
                    if purge_cfg.enabled
                        && purge_cfg.cross_source_fallback
                        && plan_model == model.as_str()
                        && attempt_index >= purge_cfg.fallback_trigger_attempt
                        && !purge_cfg.fallback_models.is_empty()
                    {
                        // 非流式拒答判定走 ChatResponse 正文（与非流式 anthropic
                        // 路径同口径：`message.content_str()`）。
                        if crate::purge::looks_refusal(resp.message.content_str()) {
                            crate::purge::record_refusal();
                            last_error = Some(crate::bridge::BridgeError::Config(
                                "refusal detected; switching to fallback model".into(),
                            ));
                            continue;
                        }
                    }
                    if attempt_index > 1 {
                        // 提前收效：重试后成功（拒绝跳过轮未成功，故在其后计数）
                        crate::purge::record_early_success();
                    }
                    // 阶段2：成功——记入断路器/健康追踪/亲和性/空响应计数
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_success_with_sticky(
                            c,
                            Some(&upstream),
                            attempt_start.elapsed().as_millis() as u64,
                            session_id.as_deref(),
                            crate::channel::routing_policy::StickyMode::sticky_enabled_from(
                                &state.routing_policy.snapshot(),
                            ),
                        );
                    }
                    // #32 路由回放：非流式成功轮明细
                    if let Some(capture) = route_capture.as_mut() {
                        capture.push_attempt(crate::log::AttemptDetail {
                            index: attempt_index,
                            channel_id: cid.clone().unwrap_or_default(),
                            channel_name: ch_ref
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            provider: ch_ref
                                .as_ref()
                                .map(|c| c.channel_type.as_str().to_string())
                                .unwrap_or_default(),
                            account: String::new(),
                            model: upstream.clone(),
                            sticky_hit,
                            started_at: attempt_started_at,
                            finished_at: Some(chrono::Utc::now().timestamp()),
                            status: Some(200),
                            error: None,
                            notice: None,
                        });
                    }
                    response_opt = Some(resp);
                    used_channel_id = cid;
                    used_upstream = Some(upstream);
                    cont_bridge = Some(bridge);
                    cont_req = Some(attempt_req);
                    break;
                }
                Err(e) => {
                    // P1-7：记录被过滤的渠道及原因
                    if let Some(c) = &cid {
                        filtered_channels.push(crate::log::FilteredChannel {
                            channel_id: c.clone(),
                            reason: e.to_string(),
                        });
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&upstream),
                            ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    // #32 路由回放：失败轮明细（含不可重试的终态轮）
                    if let Some(capture) = route_capture.as_mut() {
                        capture.push_attempt(crate::log::AttemptDetail {
                            index: attempt_index,
                            channel_id: cid.clone().unwrap_or_default(),
                            channel_name: ch_ref
                                .as_ref()
                                .map(|c| c.name.clone())
                                .unwrap_or_default(),
                            provider: ch_ref
                                .as_ref()
                                .map(|c| c.channel_type.as_str().to_string())
                                .unwrap_or_default(),
                            account: String::new(),
                            model: upstream.clone(),
                            sticky_hit,
                            started_at: attempt_started_at,
                            finished_at: Some(chrono::Utc::now().timestamp()),
                            status: Some(e.http_status()),
                            error: Some(e.to_string()),
                            notice: None,
                        });
                    }
                    if !is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        // #47：终态失败由下方既有失败日志+预留释放路径记账 → 接管
                        attempt_guard.handoff();
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    // P1：冷却时长 = max(60, Retry-After 解析值)；failover
                    // 前按解析值等待（计入预算），同流式侧口径。
                    let retry_after = bridge_error_retry_after_secs(&e);
                    if let Some(cid) = &cid {
                        state
                            .channel_store
                            .mark_cooldown(cid, e.to_string(), retry_after.max(60));
                    }
                    let wait = retry_after.min(RETRY_AFTER_WAIT_CAP_SECS);
                    if wait > 0
                        && failover_budget.remaining() >= std::time::Duration::from_secs(wait)
                    {
                        tracing::info!(
                            "chat failover: waiting {wait}s for Retry-After before switching channel"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
                    }
                    tracing::warn!(
                        "chat failover: channel {cid:?} failed: {e}, trying next channel"
                    );
                    last_error = Some(e);
                }
            }
            // 连接稳定性：记录本轮候选尝试（达上限后下轮入口停止）
            failover_budget.record_attempt();
        }
        let response = match response_opt {
            Some(r) => r,
            None => {
                // #47：全败由下方既有失败日志+预留释放路径记账 → 接管
                attempt_guard.handoff();
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for non-streaming request".into(),
                    )
                });
                crate::purge::record_failure(crate::purge::classify_failure(&e.to_string()));
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.channel_name = used_channel_id
                    .as_ref()
                    .and_then(|cid| state.channel_store.get(cid))
                    .map(|c| c.name.clone());
                log.model = used_upstream.clone().unwrap_or_else(|| model.clone());
                log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                    None
                } else {
                    Some(model.clone())
                };
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                // P1-7：调度决策回放
                log.candidate_channels = candidate_channel_ids.clone();
                log.filtered_channels = filtered_channels.clone();
                log.selected_channel = used_channel_id.clone();
                // Phase1-B 路由回放：并入 resolve 层过滤原因与策略快照
                // （as_mut：attempt_details 按值取出，as_ref 借用不可）
                if let Some(capture) = route_capture.as_mut() {
                    log.filtered_channels
                        .extend(capture.filtered.iter().cloned());
                    log.route_config = capture.config.clone();
                    log.route_truncated = capture.filtered_truncated;
                    log.attempt_details = std::mem::take(&mut capture.attempts);
                    log.attempt_details_truncated = capture.attempts_truncated;
                    log.account_skips = capture
                        .account_buf
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone();
                }
                // log_body：失败路径快照——上游原始错误信息进 response_body
                if debug_request_body.is_some() {
                    log.debug = crate::log::LogDebugSnapshot::new(
                        debug_request_body.clone(),
                        Some(e.to_string()),
                        None,
                    );
                }
                log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                crate::metrics::global().record_request(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    "error",
                    latency_ms,
                );
                // P1 指标：错误类型标签（区分 429/5xx/超时等终态错误）
                crate::metrics::global().record_request_error(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    e.error_type(),
                );
                // 渠道故障通知（仅 5xx，避免 4xx 刷屏）
                if status_code.as_u16() >= 500 {
                    state
                        .notify_service
                        .notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                            channel_name: model.clone(),
                            error: e.to_string(),
                        });
                }
                // G5：失败路径释放预留——渠道全败/客户端错误时解冻，
                // 防止预留永久冻结（2026-09-08 二轮审查）
                release_reservation(&state, &api_key, &reservation);
                return bridge_error_response(e).into_response();
            }
        };
        // auto-continue（非流式）：逐轮续写拼接正文、usage 求和；没续轮或
        // 开关关着时响应原样返回。续写轮失败保留已得内容（见 continue_non_streaming）。
        let response = match (crate::continuation::plan(), cont_bridge, cont_req) {
            (Some(max_rounds), Some(bridge), Some(req)) => {
                crate::continuation::continue_non_streaming(bridge, req, &ctx, response, max_rounds)
                    .await
            }
            _ => response,
        };
        {
            let latency_ms = request_start.elapsed().as_millis() as u64;
            // 记录用量
            state.usage_tracker.accumulate(
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                0,
                0,
                0,
                0.0,
            );

            // 计费扣减：P1 两段式——用 settle_usage 结算之前预留的配额。
            // 若无预留记录（旧路径），回退到 charge_usage。
            // 计费与日志统一用映射后的上游模型名
            let billing_model = used_upstream.as_deref().unwrap_or(&model);
            // 上游缓存量透传：prompt 按真实口径（含 Anthropic 缓存分量），
            // 缓存命中部分走缓存价而不是免费
            let (billing_prompt, billing_cached) = response.usage.billing_tokens();
            let cost = settle_usage(
                &state,
                &api_key,
                &model,
                billing_model,
                &billing_group,
                &reservation,
                billing_prompt,
                response.usage.completion_tokens,
                billing_cached,
                None,
            );

            // 事后限流记账（TPM）
            let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

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
            log.origin_model = if used_upstream.as_deref() == Some(model.as_str()) {
                None
            } else {
                Some(model.clone())
            };
            log.channel_cost = compute_channel_cost(
                &state,
                used_channel_id.as_deref(),
                &model,
                billing_model,
                &billing_group,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
                cost,
            );
            log.input_tokens = response.usage.prompt_tokens;
            log.output_tokens = response.usage.completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            // P1-7 + Phase1-B 路由回放：候选/过滤/选中/策略快照（此前成功
            // 路径漏记路由字段，一并补齐）。#32：逐次尝试明细同样并入。
            log.candidate_channels = candidate_channel_ids.clone();
            log.filtered_channels = filtered_channels.clone();
            log.selected_channel = used_channel_id.clone();
            if let Some(capture) = route_capture.as_mut() {
                log.filtered_channels
                    .extend(capture.filtered.iter().cloned());
                log.route_config = capture.config.clone();
                log.route_truncated = capture.filtered_truncated;
                log.attempt_details = std::mem::take(&mut capture.attempts);
                log.attempt_details_truncated = capture.attempts_truncated;
                log.account_skips = capture
                    .account_buf
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
            }
            // log_body：快照 = 请求体 + 响应正文（content 优先，tool_calls 计数）
            if debug_request_body.is_some() {
                let resp_snapshot = serde_json::json!({
                    "content": response.message.content_str(),
                    "reasoning": response.message.reasoning,
                    "tool_calls_count": response.message.tool_calls.as_ref().map_or(0, |t| t.len()),
                    "usage": { "prompt_tokens": response.usage.prompt_tokens, "completion_tokens": response.usage.completion_tokens },
                });
                log.debug = crate::log::LogDebugSnapshot::new(
                    debug_request_body.clone(),
                    Some(resp_snapshot.to_string()),
                    None,
                );
            }
            log.first_response_ms = ctx.first_response_ms.lock().ok().and_then(|g| *g);
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(
                billing_model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "ok",
                latency_ms,
            );
            // P1 指标：TTFT（首字节延迟）
            if let Some(ttft) = ctx.first_response_ms.lock().ok().and_then(|g| *g) {
                crate::metrics::global().record_ttft(billing_model, ttft);
            }
            crate::metrics::global().record_tokens(
                billing_model,
                "prompt",
                response.usage.prompt_tokens,
            );
            crate::metrics::global().record_tokens(
                billing_model,
                "completion",
                response.usage.completion_tokens,
            );

            let mut message = serde_json::json!({
                "role": "assistant",
                "content": response.message.content_str(),
            });
            if let Some(reasoning) = &response.message.reasoning {
                message["reasoning_content"] = Value::String(reasoning.clone());
            }
            if let Some(tool_calls) = &response.message.tool_calls {
                message["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.function_name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect(),
                );
            }
            let json = serde_json::json!({
                "id": response.id,
                "object": "chat.completion",
                "created": chrono::Utc::now().timestamp(),
                "model": response.model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "finish_reason": finish_reason_str(&response.finish_reason),
                }],
                "usage": {
                    "prompt_tokens": response.usage.prompt_tokens,
                    "completion_tokens": response.usage.completion_tokens,
                    "total_tokens": response.usage.total_tokens,
                }
            });
            state.response_cache.insert(cache_key, json.clone()).await;
            let mut resp = Json(json).into_response();
            resp.headers_mut()
                .insert("x-aigx-cache", "miss".parse().unwrap());
            resp
        }
    }
}

// ── SSE 输出帧直写（P1 性能）：
// 每 chunk 用 serde 结构体 + to_writer 一次序列化进帧缓冲，替代旧路径
// 「json! 建 delta 树 → 外层 json! 再建树 → to_string」的两步 Value 树
// 构造。字段形状与旧路径逐字段一致：
// - content 恒存在（None 占位为空串，旧 `unwrap_or_default` 同口径）；
// - reasoning_content / tool_calls 仅在有时出现（skip_serializing_if）；
// - tool_call 的 id / function.name 经 json! 序列化 Option → 缺失即 null
//   （字段恒存在），这里同样恒存在（Option<String> 序列化为 null）；
//   来源是 tool_repair 的 repaired 元组（闭包局部），无法借用，故 owned；
// - extra_content 仅在有签名时出现（签名借用自 chunk，零拷贝）。

#[derive(serde::Serialize)]
struct SseChunkFrame<'a> {
    id: &'a str,
    object: &'static str,
    created: i64,
    model: &'a str,
    choices: [SseChoiceFrame<'a>; 1],
}

#[derive(serde::Serialize)]
struct SseChoiceFrame<'a> {
    index: usize,
    delta: SseDeltaFrame<'a>,
    finish_reason: Option<&'a str>,
}

#[derive(serde::Serialize)]
struct SseDeltaFrame<'a> {
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<SseToolCallFrame<'a>>>,
}

#[derive(serde::Serialize)]
struct SseToolCallFrame<'a> {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: &'static str,
    function: SseFunctionFrame,
    #[serde(skip_serializing_if = "Option::is_none")]
    extra_content: Option<SseExtraContent<'a>>,
}

#[derive(serde::Serialize)]
struct SseFunctionFrame {
    name: Option<String>,
    arguments: String,
}

#[derive(serde::Serialize)]
struct SseExtraContent<'a> {
    google: SseGoogleSig<'a>,
}

#[derive(serde::Serialize)]
struct SseGoogleSig<'a> {
    thought_signature: &'a str,
}

// ── 响应缓存键（P1 性能）：
// 旧实现把整棵请求子树（messages/tools/…）序列化成 JSON 字符串做 key——
// 1MiB 请求体的缓存键就是 1MiB 常驻内存（DashMap cap 1000 条时约 GB 级
// 内存上限）。此处把每个字段直接序列化进帧缓冲（零克隆）后哈希为固定
// 16 字符十六进制串：
// - 值经 serde_json::Value（BTreeMap 键序确定）序列化，字节输出确定；
// - 哈希用进程级固定 RandomState（OnceLock 单例）：RandomState::new
//   每次调用随机播种，必须全进程共享一个实例，相同请求体在进程内才能
//   得到相同键；
// - 64 位哈希 + key_id 入键：无跨用户泄漏面，碰撞概率 ≈ cap²/2^65。
static CACHE_KEY_HASHER: std::sync::OnceLock<std::collections::hash_map::RandomState> =
    std::sync::OnceLock::new();

/// 计算响应缓存键（64 位哈希，十六进制串）。
///
/// 字段清单与旧键投影逐字段一致：逐字段直写缓冲（`to_writer` 序列化
/// Value 本身，无 json! 建树 + cloned 的两份深拷贝），键内容对
/// 「缺失字段」与「显式 null」同形（旧键 `cloned().unwrap_or(Value::Null)`
/// 口径），键长度恒定 16 字符。
fn response_cache_key(key_id: &str, model: &str, body: &Value) -> String {
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    for key in [
        "messages",
        "temperature",
        "max_tokens",
        "tools",
        "tool_choice",
        "stop",
        "top_p",
        "presence_penalty",
        "frequency_penalty",
        "seed",
        "response_format",
    ] {
        let v = body.get(key).unwrap_or(&Value::Null);
        buf.push(b',');
        // to_writer 写 Vec<u8> 不会失败
        serde_json::to_writer(&mut buf, v).expect("serde_json to_writer 写 Vec<u8> 不失败");
    }
    let mut hasher = CACHE_KEY_HASHER
        .get_or_init(std::collections::hash_map::RandomState::new)
        .build_hasher();
    std::hash::Hash::hash(key_id.as_bytes(), &mut hasher);
    std::hash::Hash::hash(&b',', &mut hasher);
    std::hash::Hash::hash(model.as_bytes(), &mut hasher);
    std::hash::Hash::hash(&buf, &mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 连接稳定性：流式/非流式 failover 循环接入的预算口径自检。
    // 循环内逻辑为「入口 can_attempt() 判定 → 失败记 last_error → 末尾
    // record_attempt()」，这里直接驱动 FailoverBudget 复现该时序，验证
    // 达上限后入口判定即停、且 exhausted_reason 能作为 Config 错误串。

    #[test]
    fn failover_budget_stops_after_max_attempts() {
        let mut budget = crate::api::failover::FailoverBudget::with_limits(
            crate::api::failover::FAILOVER_MAX_ATTEMPTS,
            std::time::Duration::from_secs(3600),
        );
        // 模拟循环：每个候选尝试一次，入口先判、末尾记
        for _ in 0..crate::api::failover::FAILOVER_MAX_ATTEMPTS {
            assert!(budget.can_attempt(), "达到上限前必须允许尝试");
            budget.record_attempt();
        }
        assert!(
            !budget.can_attempt(),
            "达候选上限后入口判定必须停止 failover"
        );
        let reason = budget.exhausted_reason();
        // 与循环内一致：耗尽时作为 BridgeError::Config 的串
        let err = crate::bridge::BridgeError::Config(reason.clone());
        assert!(err.to_string().contains(reason.as_str()));
    }

    #[test]
    fn failover_budget_default_allows_first_attempt() {
        // 循环首个候选进入前预算必须放行（否则正常首轮就被拦下）
        let budget = crate::api::failover::FailoverBudget::new();
        assert!(budget.can_attempt());
        assert_eq!(budget.attempts(), 0);
    }

    /// P1：Retry-After 提取与等待上限（retry-after 等待项自检）。
    #[test]
    fn retry_after_extraction_and_cap() {
        use crate::bridge::BridgeError;
        use std::time::Duration;
        // 上游 429 带 Retry-After: 30 → 提取 30
        let e = BridgeError::upstream_status_with_retry_after(
            429,
            "slow down",
            Some(Duration::from_secs(30)),
        );
        assert_eq!(super::bridge_error_retry_after_secs(&e), 30);
        // 无 Retry-After → 0（固定 60s 基础冷却，不等待）
        let e = BridgeError::upstream_status(503, "unavailable");
        assert_eq!(super::bridge_error_retry_after_secs(&e), 0);
        // 非 UpstreamStatus 错误（超时/传输）→ 0
        let e = BridgeError::Timeout {
            elapsed_ms: 1000,
            cause: "x".into(),
        };
        assert_eq!(super::bridge_error_retry_after_secs(&e), 0);
        // 等待上限：failover 前最多等 RETRY_AFTER_WAIT_CAP_SECS 秒，
        // 且计入 90s 总预算（remaining() 判定）
        assert!(super::RETRY_AFTER_WAIT_CAP_SECS <= 30);
    }

    /// 帧直写形状与旧 json! 路径逐字段一致：
    /// - 最小 chunk（仅 content）不携带 reasoning_content / tool_calls 键；
    /// - 推理增量出现 reasoning_content；tool_call 的 id/name 缺失序列化为
    ///   null（键恒存在，与 json! 对 Option 的序列化一致）；
    /// - 无签名时 extra_content 不出现；finish_reason 缺失为 null。
    /// 响应缓存键（P1 性能）：确定性、固定 16 字符、任一投影字段变化换键、
    /// 缺失字段与显式 null 同形、key_id/model 隔离维度入键。
    #[test]
    fn response_cache_key_hash_properties() {
        let body = serde_json::json!({
            "messages": [{"role": "user", "content": "你好"}],
            "temperature": 0.7,
            "tools": [{"type": "function", "function": {"name": "f"}}],
        });
        let k1 = super::response_cache_key("key-1", "gpt-4", &body);
        let k2 = super::response_cache_key("key-1", "gpt-4", &body.clone());
        assert_eq!(k1, k2, "相同请求体必须得到相同键");
        assert_eq!(k1.len(), 16, "键必须是固定 16 字符十六进制串");

        // 键投影维度：任一字段变化都必须换键（与旧键同口径）
        let mut b = body.clone();
        b["temperature"] = serde_json::json!(0.8);
        assert_ne!(k1, super::response_cache_key("key-1", "gpt-4", &b));
        let mut b = body.clone();
        b["messages"] = serde_json::json!([{"role": "user", "content": "你好吧"}]);
        assert_ne!(k1, super::response_cache_key("key-1", "gpt-4", &b));
        assert_ne!(
            k1,
            super::response_cache_key("key-2", "gpt-4", &body),
            "key_id 隔离维度必须入键"
        );
        assert_ne!(k1, super::response_cache_key("key-1", "gpt-4o", &body));

        // 缺失字段 = 显式 null 投影（旧键 unwrap_or(Value::Null) 口径）
        let partial = serde_json::json!({
            "messages": body["messages"].clone()
        });
        let explicit_null = serde_json::json!({
            "messages": body["messages"].clone(),
            "temperature": null, "max_tokens": null, "tools": null,
            "tool_choice": null, "stop": null, "top_p": null,
            "presence_penalty": null, "frequency_penalty": null,
            "seed": null, "response_format": null
        });
        assert_eq!(
            super::response_cache_key("key-1", "gpt-4", &partial),
            super::response_cache_key("key-1", "gpt-4", &explicit_null),
            "缺失字段与显式 null 必须同键"
        );
    }

    #[test]
    fn sse_chunk_frame_wire_shape() {
        let frame = SseChunkFrame {
            id: "c1",
            object: "chat.completion.chunk",
            created: 1,
            model: "m",
            choices: [SseChoiceFrame {
                index: 0,
                delta: SseDeltaFrame {
                    content: "hi",
                    reasoning_content: None,
                    tool_calls: None,
                },
                finish_reason: None,
            }],
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["id"], "c1");
        assert_eq!(v["object"], "chat.completion.chunk");
        assert_eq!(v["choices"][0]["delta"]["content"], "hi");
        assert!(v["choices"][0]["delta"].get("reasoning_content").is_none());
        assert!(v["choices"][0]["delta"].get("tool_calls").is_none());
        assert_eq!(v["choices"][0]["finish_reason"], Value::Null);

        // 推理 + tool_calls + finish_reason + extra_content 完整形状
        let frame = SseChunkFrame {
            id: "c1",
            object: "chat.completion.chunk",
            created: 1,
            model: "m",
            choices: [SseChoiceFrame {
                index: 0,
                delta: SseDeltaFrame {
                    content: "",
                    reasoning_content: Some("think"),
                    tool_calls: Some(vec![SseToolCallFrame {
                        index: 0,
                        id: Some("call_1".into()),
                        kind: "function",
                        function: SseFunctionFrame {
                            name: Some("get_weather".into()),
                            arguments: "{}".into(),
                        },
                        extra_content: Some(SseExtraContent {
                            google: SseGoogleSig {
                                thought_signature: "sig1",
                            },
                        }),
                    }]),
                },
                finish_reason: Some("stop"),
            }],
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        assert_eq!(v["choices"][0]["delta"]["reasoning_content"], "think");
        let tc = &v["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["index"], 0);
        assert_eq!(tc["id"], "call_1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "get_weather");
        assert_eq!(tc["function"]["arguments"], "{}");
        assert_eq!(tc["extra_content"]["google"]["thought_signature"], "sig1");
        assert_eq!(v["choices"][0]["finish_reason"], "stop");

        // id/name 缺失 → null 键恒存在（与旧 json! Option 序列化一致）
        let frame = SseChunkFrame {
            id: "c1",
            object: "chat.completion.chunk",
            created: 1,
            model: "m",
            choices: [SseChoiceFrame {
                index: 0,
                delta: SseDeltaFrame {
                    content: "",
                    reasoning_content: None,
                    tool_calls: Some(vec![SseToolCallFrame {
                        index: 0,
                        id: None,
                        kind: "function",
                        function: SseFunctionFrame {
                            name: None,
                            arguments: String::new(),
                        },
                        extra_content: None,
                    }]),
                },
                finish_reason: None,
            }],
        };
        let v: Value = serde_json::to_value(&frame).unwrap();
        let tc = &v["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["id"], Value::Null, "id 键必须恒存在（null）");
        assert_eq!(
            tc["function"]["name"],
            Value::Null,
            "name 键必须恒存在（null）"
        );
        assert!(
            tc.get("extra_content").is_none(),
            "无签名时 extra_content 不出现"
        );
    }
}
