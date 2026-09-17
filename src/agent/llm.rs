//! 自环 LLM 推理——Agent 的"大脑"走 AIGX 自己的渠道。
//!
//! 与客户请求的本质区别：**不经 HTTP 端口、不经 verify_api_key / 计费**，
//! 而是进程内直调 bridge，复用渠道调度 / 熔断 / 亲和 / 故障转移。
//! 模型与渠道由 `[agent]` 配置决定（可自由选渠道选模型）。

use std::sync::Arc;

use crate::api::openai::{resolve_bridges_with_affinity, resolve_upstream_model, AppState};
use crate::bridge::{BridgeContext, ChatFormat, ChatMessage, ChatResponse, Role};
use crate::channel::ChannelStore;
use crate::config::AgentConfig;

/// Agent 推理错误。
#[derive(Debug, thiserror::Error)]
pub enum AgentLlmError {
    /// 未配置 `[agent].model`。
    #[error("agent model not configured (set [agent].model)")]
    ModelNotConfigured,
    /// 该模型没有任何可用渠道（或全部被熔断）。
    #[error("no bridge available for agent model")]
    NoBridge,
    /// 所有渠道均失败。
    #[error("all channels failed: {0}")]
    AllChannelsFailed(String),
}

/// 把渠道快照 + bridge 绑定成一个候选，供自环 failover 循环使用。
struct Candidate {
    bridge: Arc<dyn crate::bridge::Bridge>,
    channel_id: Option<String>,
    channel: Option<crate::channel::Channel>,
}

/// 解析 Agent 推理候选渠道。
///
/// - `config.channel` 非空 → 只取该渠道（排障锁定）。
/// - 否则走 `resolve_bridges_with_affinity` 全局调度（复用优先级/权重/亲和）。
fn resolve_candidates(
    state: &AppState,
    config: &AgentConfig,
) -> Result<Vec<Candidate>, AgentLlmError> {
    if config.model.trim().is_empty() {
        return Err(AgentLlmError::ModelNotConfigured);
    }
    if !config.channel.trim().is_empty() {
        // 锁定单渠道：直接从 ChannelStore 构造（与 resolve_bridges 同源逻辑）
        let Some(ch) = state.channel_store.get(config.channel.trim()) else {
            return Err(AgentLlmError::NoBridge);
        };
        let key = ch.decode_api_key();
        if key.is_empty() {
            return Err(AgentLlmError::NoBridge);
        }
        let bridge = make_bridge_for_channel(&ch, &key, &state.http_client);
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
    let candidates = resolve_bridges_with_affinity(state, &config.model, None);
    if candidates.is_empty() {
        return Err(AgentLlmError::NoBridge);
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

/// 按渠道类型构造 bridge（与 `resolve_bridges` 中的分支一致）。
fn make_bridge_for_channel(
    ch: &crate::channel::Channel,
    key: &str,
    client: &reqwest::Client,
) -> Option<Arc<dyn crate::bridge::Bridge>> {
    use crate::channel::ChannelType;
    match ch.channel_type {
        ChannelType::OpenaiCompatible => Some(crate::bridge::openai::make_bridge(
            &ch.base_url,
            key,
            client,
        )),
        ChannelType::Anthropic => Some(crate::bridge::anthropic::make_bridge(
            &ch.base_url,
            key,
            client,
        )),
        ChannelType::Gemini => Some(crate::bridge::gemini::make_bridge(
            &ch.base_url,
            key,
            client,
        )),
        ChannelType::Zai => Some(crate::bridge::zai::make_bridge(&ch.base_url, key, client)),
        // Cloudflare 渠道走 Hub 专用桥，自环暂不支持（阶段三再补）
        ChannelType::Cloudflare => None,
    }
}

/// 单次非流式推理（工具调用用）。带 failover。
///
/// 每次推理落一条 RequestLog（key_id="agent"），成功与失败都记——
/// AI 运维对话在用量日志页可见（与渠道调试/Playground 同口径）。
///
/// 返回 (`ChatResponse`, 实际使用的上游模型名, 渠道 id)。
pub async fn chat_once(
    state: &AppState,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<serde_json::Value>>,
) -> Result<(ChatResponse, String, Option<String>), AgentLlmError> {
    let candidates = resolve_candidates(state, config)?;
    let request_id = format!("agent-{}", uuid::Uuid::new_v4());
    let ctx = BridgeContext::new(request_id.clone(), config.model.clone());

    let mut last_error: Option<String> = None;
    for cand in &candidates {
        if let Some(cid) = &cand.channel_id {
            state.channel_store.mark_used(cid);
        }
        let upstream =
            resolve_upstream_model(&config.model, cand.channel.as_ref(), &state.model_mapper);
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
            reasoning_effort: None,
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
                    &request_id,
                    &config.model,
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
                    &request_id,
                    &config.model,
                    &upstream,
                    cand.channel_id.as_deref(),
                    cand.channel.as_ref().map(|c| c.name.as_str()),
                    &req,
                    None,
                    start.elapsed().as_millis() as u64,
                    Some(&msg),
                );
                if !crate::api::openai::is_retryable_bridge_error(&e) {
                    // 4xx 客户端错误（上下文超限/参数错/模型不存在）——换渠道大概率同样失败
                    return Err(AgentLlmError::AllChannelsFailed(msg));
                }
                if let Some(cid) = &cand.channel_id {
                    state.channel_store.mark_cooldown(cid, msg.clone(), 60);
                }
                tracing::warn!(
                    "agent llm failover: channel {:?} failed: {e}, trying next",
                    cand.channel_id
                );
                last_error = Some(msg);
            }
        }
    }
    Err(AgentLlmError::AllChannelsFailed(
        last_error.unwrap_or_else(|| "all channels failed".to_string()),
    ))
}

/// Agent 推理落请求日志（成功/失败都记，不扣费——运维自环不产生销售成本）。
///
/// - key_id = "agent"（用量日志页展示为「AI 运维」来源标签）
/// - user_id = None（系统身份）
/// - input token 按消息文本估算（自环响应 usage 缺失时输出按 0）
#[allow(clippy::too_many_arguments)]
fn record_agent_request_log(
    state: &AppState,
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
        key_id: Some("agent".to_string()),
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
        filtered_channels: Vec::new(),
        selected_channel: None,
        debug: None,
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
