//! 自环 LLM 推理——Agent 的"大脑"走 AIGX 自己的渠道。
//!
//! 与客户请求的本质区别：**不经 HTTP 端口、不经 verify_api_key / 计费**，
//! 而是进程内直调 bridge，复用渠道调度 / 熔断 / 亲和 / 故障转移。
//! 模型与渠道由 `[agent]` 配置决定（可自由选渠道选模型）。

use std::sync::Arc;

use crate::api::openai::{
    model_declared_by_channel, resolve_bridges_with_affinity, resolve_upstream_model, AppState,
};
use crate::bridge::{BridgeContext, ChatFormat, ChatMessage, ChatResponse, Role};
use crate::channel::ChannelStore;
use crate::config::AgentConfig;

/// Agent 推理错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentLlmError {
    /// 未配置 `[agent].model` 且自动发现没有可用模型。
    #[error(
        "agent model not configured and no enabled model available (set [agent].model or add pricing)"
    )]
    ModelNotConfigured,
    /// 该模型没有任何可用渠道（或全部被熔断）。
    #[error("no bridge available for agent model")]
    NoBridge,
    /// 该模型由某启用渠道声明，但当前没有任何可用渠道/账号（禁用或均在冷却中）。
    /// 此时不退回「不会服务该模型」的 Hub 兜底桥，以免把「无可用渠道」发成一条
    /// 注定失败、且会误导用户（看似鉴权问题）的上游请求。
    #[error(
        "no available channel for agent model {model}（该模型暂无可用渠道或账号：可能均被禁用或正在冷却中，请稍后重试或更换模型）"
    )]
    NoChannelAvailable { model: String },
    /// 所有渠道均失败（候选已用尽）。`recoverable` 表示失败原因属**暂时性**
    /// （超时/传输/限流/5xx/账号冷却），等一会儿重试有机会成功；
    /// false 表示确定性失败（模型不存在、参数错误等），重试无意义。
    #[error("all channels failed: {0}")]
    AllChannelsFailed(String),
    /// 暂时性失败且已用尽重试轮次。与 [`AllChannelsFailed`] 区分，是为了让
    /// 前端能给出「稍后重试即可」而不是「配置有问题」的建议。
    #[error("all channels failed after {rounds} rounds: {last}")]
    TransientExhausted { rounds: u32, last: String },
}

/// 把渠道快照 + bridge 绑定成一个候选，供自环 failover 循环使用。
struct Candidate {
    bridge: Arc<dyn crate::bridge::Bridge>,
    channel_id: Option<String>,
    channel: Option<crate::channel::Channel>,
}

/// 解析实际生效的推理模型：显式配置优先，空则从模型管理视图自动发现。
///
/// 自动发现策略见 `model::manage::pick_default_model`（启用+已定价 →
/// 非预览 → 字典序）。平台新上模型/渠道无需任何配置即可被 Agent 用上。
pub fn effective_model(state: &AppState, config: &AgentConfig) -> Result<String, AgentLlmError> {
    let explicit = config.model.trim();
    if !explicit.is_empty() {
        return Ok(explicit.to_string());
    }
    crate::model::manage::pick_default_model(&crate::api::admin::model_manage::current_view(state))
        .ok_or(AgentLlmError::ModelNotConfigured)
}

/// 解析 Agent 推理候选渠道。
///
/// - `config.channel` 非空 → 只取该渠道（排障锁定）。
/// - 否则走 `resolve_bridges_with_affinity` 全局调度（复用优先级/权重/亲和）。
fn resolve_candidates(
    state: &AppState,
    config: &AgentConfig,
    model: &str,
) -> Result<Vec<Candidate>, AgentLlmError> {
    if !config.channel.trim().is_empty() {
        // 锁定单渠道：直接从 ChannelStore 构造（与 resolve_bridges 同源逻辑，
        // 含 model_rules 校验与 Account/Cloudflare 桥支持）
        let Some(ch) = state.channel_store.get(config.channel.trim()) else {
            return Err(AgentLlmError::NoBridge);
        };
        let key = ch.decode_api_key();
        let bridge = make_bridge_for_channel(&ch, &key, &state.http_client, state, model);
        let bridge = match bridge {
            Some(b) => b,
            None => return Err(AgentLlmError::NoBridge),
        };
        return Ok(vec![Candidate {
            bridge,
            channel_id: Some(ch.id.clone()),
            channel: Some(ch),
        }]);
    }
    // 运维 Agent 的 LLM 调用无用户 Key（管理面自用），provider 白名单不适用
    let candidates = resolve_bridges_with_affinity(state, model, None, None, None);
    // 全局调度在「真实渠道全不可用」时会把 Hub 专用桥（channel_id = None）作为
    // 兜底候选塞进来。若该模型确由某启用渠道声明，说明它本该由真实渠道服务、
    // Hub 兜底不会服务它——保留只会发出一个注定失败的请求，并把真实的
    // 「无可用渠道/账号」掩盖成误导性的上游鉴权错误（如 channel None 的 401）。
    // 故此时剔除兜底候选，并在落空时给出可读、可操作的 NoChannelAvailable。
    // 模型仅经全局映射暴露（可能确由 Hub 服务）时 `declared=false`，兜底候选
    // 照常保留，语义与改造前一致。
    let declared = model_declared_by_channel(state, model);
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|(_, cid, _)| keep_global_candidate(cid.is_some(), declared))
        .collect();
    if candidates.is_empty() {
        return Err(if declared {
            AgentLlmError::NoChannelAvailable {
                model: model.to_string(),
            }
        } else {
            AgentLlmError::NoBridge
        });
    }
    Ok(candidates
        .into_iter()
        .map(|(bridge, channel_id, channel)| Candidate {
            bridge,
            channel_id,
            channel,
        })
        .collect())
}

/// 全局调度下的候选保留规则（纯函数，便于单测）。
///
/// 模型由渠道声明时，只保留真实渠道候选（`has_channel=true`），剔除 Hub 兜底
/// 候选（无渠道）；模型未由任何渠道声明时（仅全局映射，可能确由 Hub 服务），
/// 兜底候选照常保留。
fn keep_global_candidate(has_channel: bool, declared_by_channel: bool) -> bool {
    !declared_by_channel || has_channel
}

/// 按渠道类型构造 bridge（与 `resolve_bridges` 中的分支一致）。
///
/// 锁渠道路径同样过 `model_rules` 启停校验、支持 Account（账号池选号）与
/// Cloudflare（Hub 专用桥）——与全局调度口径一致。
fn make_bridge_for_channel(
    ch: &crate::channel::Channel,
    key: &str,
    client: &reqwest::Client,
    state: &AppState,
    model: &str,
) -> Option<Arc<dyn crate::bridge::Bridge>> {
    use crate::channel::ChannelType;
    if ch.channel_type != ChannelType::Account
        && state
            .model_rules
            .is_disabled(ch.channel_type.as_str(), model)
    {
        return None;
    }
    match ch.channel_type {
        ChannelType::OpenaiCompatible => Some(crate::bridge::openai::make_bridge_with_quirks(
            &ch.base_url,
            key,
            client,
            ch.quirks.clone(),
        )),
        ChannelType::Anthropic => Some(crate::bridge::anthropic::make_bridge_with_quirks(
            &ch.base_url,
            key,
            client,
            ch.quirks.clone(),
        )),
        ChannelType::Gemini => Some(crate::bridge::gemini::make_bridge(
            &ch.base_url,
            key,
            client,
        )),
        ChannelType::Zai => Some(crate::bridge::zai::make_bridge_with_quirks(
            &ch.base_url,
            key,
            client,
            ch.quirks.clone(),
        )),
        // 账号型渠道无渠道级凭证：AccountBridge 在调用时按账号池选号
        ChannelType::Account => Some(Arc::new(
            crate::bridge::account::AccountBridge::new(
                ch.id.clone(),
                state.account_sessions.clone(),
            )
            .with_rules(state.model_rules.clone())
            .with_plugin_disabled(state.provider_gates.clone()),
        )),
        // Cloudflare 渠道走 Hub 专用桥
        ChannelType::Cloudflare => state.hub.get_specialized("cloudflare"),
    }
}

/// 思考强度归一化：只认 low/medium/high（off/空/未知 = 不发送）。
fn effort_level(raw: Option<&str>) -> Option<String> {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("low") => Some("low".to_string()),
        Some("medium") => Some("medium".to_string()),
        Some("high") => Some("high".to_string()),
        _ => None,
    }
}

/// 单次非流式推理（工具调用用）。带 failover。请求日志来源固定为 "agent"。
pub async fn chat_once(
    state: &AppState,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<serde_json::Value>>,
) -> Result<(ChatResponse, String, Option<String>), AgentLlmError> {
    chat_once_as(state, config, messages, tools, "agent").await
}

/// [`chat_once`] 的多来源变体：`key_id` 决定请求日志的来源标签
/// （agent 主循环="agent"，聊天自动标题="chat_title"），其余行为一致。
///
/// 外层带**时间维度重试**：单轮候选遍历（换渠道 failover）失败后，若失败原因
/// 属暂时性（上游超时/传输中断/限流/5xx/账号冷却），等待后重跑整轮，而不是立刻
/// 把错误抛给用户。运维上游的抖动往往几秒到几十秒自愈，直接报错会让用户以为
/// 配置坏了。确定性失败（模型不存在/参数错误）不重试，立即返回。
pub async fn chat_once_as(
    state: &AppState,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<serde_json::Value>>,
    key_id: &str,
) -> Result<(ChatResponse, String, Option<String>), AgentLlmError> {
    let model = effective_model(state, config)?;
    let rounds = config
        .transient_retry_rounds
        .max(1)
        .min(MAX_TRANSIENT_RETRY_ROUNDS);
    let mut last_msg = String::new();
    let mut last_recoverable = false;

    for round in 1..=rounds {
        // 每轮重新解析候选：冷却中的渠道会随冷却到期重新可用，
        // 固定用第一轮的候选列表会白白错过刚恢复的渠道。
        match resolve_candidates(state, config, &model) {
            Ok(candidates) => {
                match try_candidates(
                    state,
                    config,
                    &model,
                    &candidates,
                    &messages,
                    &tools,
                    key_id,
                )
                .await
                {
                    Ok(v) => {
                        if round > 1 {
                            tracing::info!("agent llm recovered on retry round {round}/{rounds}");
                        }
                        return Ok(v);
                    }
                    Err(TryFailure {
                        message,
                        recoverable,
                    }) => {
                        last_msg = message;
                        last_recoverable = recoverable;
                        if !recoverable {
                            return Err(AgentLlmError::AllChannelsFailed(last_msg));
                        }
                    }
                }
            }
            Err(e @ AgentLlmError::NoChannelAvailable { .. }) => {
                // 全渠道冷却也属暂时性：等冷却到期再试
                last_msg = e.to_string();
                last_recoverable = true;
            }
            Err(e) => return Err(e),
        }

        if round < rounds {
            let wait = retry_backoff(config, round);
            tracing::warn!(
                "agent llm transient failure (round {round}/{rounds}): {last_msg}; retrying in {wait}ms"
            );
            tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
        }
    }

    if last_recoverable {
        Err(AgentLlmError::TransientExhausted {
            rounds,
            last: last_msg,
        })
    } else {
        Err(AgentLlmError::AllChannelsFailed(last_msg))
    }
}

/// 暂时性重试的最大轮次上限（防止配置写大导致一次对话卡很久）。
const MAX_TRANSIENT_RETRY_ROUNDS: u32 = 5;

/// 第 `round` 轮失败后的等待时长（毫秒）。
///
/// 按轮次线性退避：`retry_interval_ms`（默认 0 → 兜底 1000ms）× 轮次，
/// 上限 15s。运维场景上游抖动通常秒级恢复，线性退避够用且比指数退避更
/// 早重试、体感更好。
fn retry_backoff(config: &AgentConfig, round: u32) -> u64 {
    let base = if config.retry_interval_ms > 0 {
        config.retry_interval_ms
    } else {
        1000
    };
    (base * u64::from(round)).min(15_000)
}

/// 单轮候选遍历的结果：失败时附带「是否值得再试一轮」的判定。
struct TryFailure {
    message: String,
    recoverable: bool,
}

/// 遍历一轮候选渠道（换渠道 failover），成功即返回。
async fn try_candidates(
    state: &AppState,
    config: &AgentConfig,
    model: &str,
    candidates: &[Candidate],
    messages: &[ChatMessage],
    tools: &Option<Vec<serde_json::Value>>,
    key_id: &str,
) -> Result<(ChatResponse, String, Option<String>), TryFailure> {
    let request_id = format!("{key_id}-{}", uuid::Uuid::new_v4());
    let ctx = BridgeContext::new(request_id.clone(), model.to_string());

    let mut last_error: Option<String> = None;
    // 只要有一个候选的失败是「暂时性」的，整轮就值得重试；
    // 全是确定性失败（模型不存在等）才不重试。
    let mut any_recoverable = false;
    for cand in candidates {
        if let Some(cid) = &cand.channel_id {
            state.channel_store.mark_used(cid);
        }
        let upstream = resolve_upstream_model(model, cand.channel.as_ref(), &state.model_mapper);
        // 无工具时不得声明 tool_choice="auto"：严格的 OpenAI 兼容上游会在
        // 仅带 tool_choice 而缺 tools 时返回 400。纯文本自环（如提示词翻译）
        // 复用 chat_once 时 tools=None，必须同步置空，否则翻译会整体失败。
        let tool_choice = tools
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|_| serde_json::json!("auto"));
        let mut req = ChatFormat {
            model: upstream.clone(),
            messages: messages.clone(),
            tools: tools.clone(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice,
            reasoning_effort: effort_level(config.reasoning_effort.as_deref()),
            web_search_options: None,
            extra: None,
        };
        req.model = upstream.clone();

        let start = std::time::Instant::now();
        match cand.bridge.chat(&req, &ctx).await {
            Ok(resp) => {
                if let Some(cid) = &cand.channel_id {
                    state.channel_store.record_channel_success(
                        cid,
                        Some(&upstream),
                        start.elapsed().as_millis() as u64,
                        None,
                    );
                }
                record_agent_request_log(
                    state,
                    key_id,
                    &request_id,
                    &model,
                    &upstream,
                    cand.channel_id.as_deref(),
                    cand.channel.as_ref().map(|c| c.name.as_str()),
                    &req,
                    Some(&resp),
                    start.elapsed().as_millis() as u64,
                    None,
                );
                return Ok((resp, upstream, cand.channel_id.clone()));
            }
            Err(e) => {
                let msg = e.to_string();
                if let Some(cid) = &cand.channel_id {
                    state.channel_store.record_channel_failure(
                        cid,
                        Some(&upstream),
                        ChannelStore::classify_bridge_error(&e),
                        &msg,
                        None,
                    );
                }
                record_agent_request_log(
                    state,
                    key_id,
                    &request_id,
                    &model,
                    &upstream,
                    cand.channel_id.as_deref(),
                    cand.channel.as_ref().map(|c| c.name.as_str()),
                    &req,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(&msg),
                );
                if !crate::api::openai::is_retryable_bridge_error(&e) {
                    // 4xx 客户端错误（上下文超限/参数错/模型不存在）——换渠道大概率同样失败，
                    // 且等一会儿也不会变好：立即返回，不做时间维度重试。
                    return Err(TryFailure {
                        message: msg,
                        recoverable: false,
                    });
                }
                if let Some(cid) = &cand.channel_id {
                    state.channel_store.mark_cooldown(cid, msg.clone(), 60);
                }
                tracing::warn!(
                    "agent llm failover: channel {:?} failed: {e}, trying next",
                    cand.channel_id
                );
                any_recoverable = true;
                last_error = Some(msg);
            }
        }
    }
    Err(TryFailure {
        message: last_error.unwrap_or_else(|| "all channels failed".to_string()),
        recoverable: any_recoverable,
    })
}

/// Agent 推理落请求日志（成功/失败都记，不扣费——运维自环不产生销售成本）。
///
/// - key_id：来源标签（"agent"=运维主循环；"chat_title"=聊天自动标题）
/// - user_id = None（系统身份）
/// - input token 按消息文本估算（自环响应 usage 缺失时输出按 0）
#[allow(clippy::too_many_arguments)]
fn record_agent_request_log(
    state: &AppState,
    key_id: &str,
    request_id: &str,
    model: &str,
    upstream_model: &str,
    channel_id: Option<&str>,
    channel_name: Option<&str>,
    req: &ChatFormat,
    resp: Option<&crate::bridge::ChatResponse>,
    latency_ms: u64,
    error: Option<&str>,
) {
    let input_text: String = req
        .messages
        .iter()
        .filter_map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let input_tokens = crate::token_estimate::count_text(model, &input_text) as u64;
    let output_tokens = resp
        .map(|r| {
            let text = r.message.content.clone().unwrap_or_default();
            crate::token_estimate::count_text(model, &text) as u64
        })
        .unwrap_or(0);
    let log = crate::log::RequestLog {
        id: request_id.to_string(),
        user_id: None,
        key_id: Some(key_id.to_string()),
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: channel_name.map(|s| s.to_string()),
        model: model.to_string(),
        origin_model: Some(upstream_model.to_string()),
        input_tokens,
        output_tokens,
        cost: 0,
        channel_cost: 0,
        latency_ms,
        status_code: if error.is_some() { 502 } else { 200 },
        error_msg: error.map(|e| e.chars().take(200).collect()),
        ip: None,
        request_id: None,
        created_at: chrono::Utc::now().timestamp(),
        candidate_channels: Vec::new(),
        cache_hit: false,
        conversation: None,
        filtered_channels: Vec::new(),
        selected_channel: None,
        debug: None,
        first_response_ms: None,
        attempts: 1,
        sensitive_hits: Vec::new(),
        phase: String::new(),
        route_config: None,
        route_truncated: 0,
        attempt_details: Vec::new(),
        attempt_details_truncated: 0,
        account_skips: Vec::new(),
    };
    state.log_store.record_request(log);
}

/// 从 [`ChatResponse`] 提取助手消息（含 tool_calls）。
pub fn response_message(resp: ChatResponse) -> ChatMessage {
    resp.message
}

/// 构造 tool 角色消息（工具结果回填）。
pub fn tool_message(tool_call_id: String, content: String) -> ChatMessage {
    ChatMessage {
        role: Role::Tool,
        content: Some(content),
        content_blocks: None,
        name: None,
        tool_call_id: Some(tool_call_id),
        tool_calls: None,
        reasoning: None,
    }
}

/// 构造用户消息。
pub fn user_message(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: Some(content),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    }
}

/// 构造系统消息。
pub fn system_message(content: String) -> ChatMessage {
    ChatMessage {
        role: Role::System,
        content: Some(content),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{effort_level, keep_global_candidate, AgentLlmError};

    #[test]
    fn effort_level_only_accepts_known_levels() {
        assert_eq!(effort_level(Some(" HIGH ")), Some("high".to_string()));
        assert_eq!(effort_level(Some("low")), Some("low".to_string()));
        assert_eq!(effort_level(Some("off")), None);
        assert_eq!(effort_level(Some("")), None);
        assert_eq!(effort_level(None), None);
    }

    /// 渠道声明的模型：Hub 兜底候选（无 channel_id）必须被剔除，真实渠道保留。
    #[test]
    fn declared_model_drops_hub_fallback_candidate() {
        assert!(keep_global_candidate(true, true), "真实渠道候选应保留");
        assert!(!keep_global_candidate(false, true), "Hub 兜底候选应剔除");
    }

    /// 仅全局映射暴露的模型（可能确由 Hub 服务）：兜底候选照常保留，语义不变。
    #[test]
    fn mapped_only_model_keeps_hub_fallback_candidate() {
        assert!(keep_global_candidate(false, false), "兜底候选应保留");
        assert!(keep_global_candidate(true, false));
    }

    /// 无可用渠道时的错误文案必须点名模型且给出可操作提示（供前端映射展示）。
    #[test]
    fn no_channel_available_message_is_actionable() {
        let e = AgentLlmError::NoChannelAvailable {
            model: "deepseek-v4.1-flash".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("deepseek-v4.1-flash"), "应点名模型: {msg}");
        assert!(
            msg.contains("暂无可用渠道") && msg.contains("冷却"),
            "应说明原因与建议动作: {msg}"
        );
    }

    /// 退避时长：线性递增、有上限，且 `retry_interval_ms=0` 时用 1s 兜底。
    ///
    /// 钉住三条边界：①配置 0 不能退化成「无等待连续猛打上游」；②轮次越高等
    /// 待越长（给上游恢复时间）；③有 15s 硬上限（避免一次对话卡太久）。
    #[test]
    fn retry_backoff_is_linear_capped_and_never_zero() {
        let mut cfg = AgentConfig::default();
        cfg.retry_interval_ms = 0;
        assert_eq!(super::retry_backoff(&cfg, 1), 1000, "0 配置须兜底 1s");
        assert_eq!(super::retry_backoff(&cfg, 2), 2000);
        cfg.retry_interval_ms = 500;
        assert_eq!(super::retry_backoff(&cfg, 3), 1500);
        assert_eq!(super::retry_backoff(&cfg, 4), 2000);
        // 上限：基数再大也不超过 15s
        cfg.retry_interval_ms = 10_000;
        assert_eq!(super::retry_backoff(&cfg, 5), 15_000, "须有上限");
        assert!(super::retry_backoff(&cfg, 99) <= 15_000);
    }

    /// 暂时性耗尽与确定性失败的文案必须可区分：前者要让用户「稍后再试」，
    /// 后者不该鼓励重试（重试也不会好）。
    #[test]
    fn transient_exhausted_message_is_distinct_and_actionable() {
        let e = AgentLlmError::TransientExhausted {
            rounds: 3,
            last: "transport error".to_string(),
        };
        let msg = e.to_string();
        assert!(msg.contains("after 3 rounds"), "应报轮次: {msg}");
        assert!(msg.contains("transport error"), "应带最后原因: {msg}");

        let d = AgentLlmError::AllChannelsFailed("model not found".to_string()).to_string();
        assert_ne!(msg, d, "两类失败文案不得相同");
        assert!(!d.contains("rounds"), "确定性失败不该报重试轮次: {d}");
    }
}
