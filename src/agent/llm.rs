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
    /// 所有候选模型均失败（跨模型 failover 已穷尽）。
    ///
    /// 与 [`AllChannelsFailed`] 的区别：后者是「一个模型的所有渠道都挂了」，
    /// 本变体是「连换几个模型也没成功」。文本里逐行带上每个模型的失败原因，
    /// 这样运营能直接看出是凭证 401、模型被关 404、还是渠道校验拦截。
    #[error("all candidate models failed: {0}")]
    AllModelsFailed(String),
    /// 暂时性失败且已用尽重试轮次。与 [`AllChannelsFailed`] 区分，是为了让
    /// 前端能给出「稍后重试即可」而不是「配置有问题」的建议。
    #[error("all channels failed after {rounds} rounds: {last}")]
    TransientExhausted { rounds: u32, last: String },
}

/// 跨模型切换的上限：一次调用最多尝试多少个模型。
///
/// 为什么封顶：模型清单可能几十个，全试一遍会把一次对话拖成几分钟，用户
/// 早就断了。5 个已经能覆盖「首选挂了换备选」的绝大多数情况。
const MAX_MODEL_CANDIDATES: usize = 5;

/// 跨模型切换的总耗时上限（秒）。**硬上限**。
///
/// 与 `MAX_MODEL_CANDIDATES` 是两道独立的闸：模型少但每个都很慢时，只靠数量
/// 封顶拦不住；反过来只靠时间封顶，模型多时又可能把时间浪费在注定失败的
/// 候选上。两道都要。
///
/// 注意：这是**真的硬**——每个模型的整次尝试都包在
/// `tokio::time::timeout(剩余预算, …)` 里。只在「两次尝试之间看表」是不够的：
/// 单次非流式请求默认超时 300s（`body_timeout_secs`）、流式更长，一次就能把
/// 预算撑爆，那样 60s 只是句空话。
const MODEL_FAILOVER_BUDGET_SECS: u64 = 60;

/// 本次调用要依次尝试的模型列表（首选在前）。
///
/// - 首选 = [`effective_model`]（显式 `[agent].model` 优先，否则自动发现）。
/// - 备选 = `pick_default_models` 的同一套排序（启用+已定价 → 非预览 → 字典序），
///   最多 [`MAX_MODEL_CANDIDATES`] 个。
/// - **`[agent].channel` 显式锁定时不做跨模型切换**：锁渠道的语义就是「只走这
///   一条」的排障口径，换模型就不再是「只走这条」，会把排障结论带偏。
fn candidate_models(state: &AppState, config: &AgentConfig) -> Result<Vec<String>, AgentLlmError> {
    let primary = effective_model(state, config)?;
    if !config.channel.trim().is_empty() {
        return Ok(vec![primary]);
    }
    let view = crate::api::admin::model_manage::current_view(state);
    let listed = crate::model::manage::pick_default_models(&view, MAX_MODEL_CANDIDATES);
    Ok(compose_model_candidates(
        &primary,
        listed,
        MAX_MODEL_CANDIDATES,
    ))
}

/// 纯函数：把「首选」与「候选清单」拼成实际尝试顺序。
///
/// 规则：首选永远第一；清单里与首选同名或已出现的跳过（不重复打同一个模型）；
/// 总数封顶 `limit`（至少保留首选）。抽成纯函数，是为了排序/去重/封顶这三条
/// 能被单测钉住，而不必构造 `AppState`。
fn compose_model_candidates(primary: &str, listed: Vec<String>, limit: usize) -> Vec<String> {
    let cap = limit.max(1);
    let mut out: Vec<String> = Vec::with_capacity(cap.min(listed.len() + 1));
    out.push(primary.to_string());
    for id in listed {
        if out.len() >= cap {
            break;
        }
        if id != primary && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// 纯函数：把「尝试过的模型 + 各自原因」收口成最终错误。
///
/// 只试过一个模型（渠道锁定，或候选清单只有它）时**保留原错误类型** ——
/// 前端靠 `NoChannelAvailable` / `TransientExhausted` 区分「配置问题」与
/// 「稍后重试」，包一层 AllModelsFailed 会把这两个语义磨平。试过多个才汇总，
/// 并把每个模型的原因带出去（否则运营只看到一句「暂无可用渠道」）。
fn final_error(tried: &[String], last: Option<AgentLlmError>) -> AgentLlmError {
    match last {
        // 一次都没真正跑过（预算一开始就耗尽）→ 汇总，而不是谎称「模型未配置」
        None if !tried.is_empty() => AgentLlmError::AllModelsFailed(tried.join(" | ")),
        None => AgentLlmError::ModelNotConfigured,
        // 只试过一个模型且真的失败了：保留原错误类型（前端据此区分
        // 「配置问题」与「稍后重试」，包平会把这个语义磨掉）
        Some(e) if tried.len() <= 1 => e,
        Some(_) => AgentLlmError::AllModelsFailed(tried.join(" | ")),
    }
}

/// 距离总预算还剩多少；已耗尽返回 `None`。
fn remaining_budget(deadline: std::time::Instant) -> Option<std::time::Duration> {
    let now = std::time::Instant::now();
    (now < deadline).then(|| deadline - now)
}

/// 把「没来得及试」的模型如实记进 `tried`。
///
/// 不记的话，预算耗尽时最终错误只提试过的那几个，读的人会以为候选清单里
/// 本来就没有别的模型 —— 与「把每个模型的失败原因如实报出」相反。
fn mark_untried(tried: &mut Vec<String>, rest: &[String]) {
    for model in rest {
        tried.push(format!(
            "{model}: 未尝试（总耗时预算 {MODEL_FAILOVER_BUDGET_SECS}s 已用尽）"
        ));
    }
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
        // 插件渠道暂不在 agent 内部 LLM 选路中（M3 仅接线数据面）
        ChannelType::Plugin => None,
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
    let models = candidate_models(state, config)?;
    let mut attempt = RealAttempt {
        state,
        config,
        messages: &messages,
        tools: &tools,
        key_id,
    };
    run_model_failover(
        &models,
        std::time::Duration::from_secs(MODEL_FAILOVER_BUDGET_SECS),
        &mut attempt,
    )
    .await
}

/// 「尝试一个模型」的注入点：生产传 [`RealAttempt`]（真打上游），测试传假实现。
///
/// 抽这一层的目的是让**切换、预算硬闸、未尝试上报**三条契约都由生产同一段
/// 循环代码保证 —— 谁把 `tokio::time::timeout` 拆掉、或把循环改回「只试一个
/// 模型」，对应用例立刻变红（而不是只测到 tokio 原语）。
trait ModelAttempt {
    type Output;
    async fn attempt(
        &mut self,
        model: &str,
        deadline: std::time::Instant,
    ) -> Result<Self::Output, AgentLlmError>;
}

/// 生产实现：真打上游。渠道级 failover 与时间维度重试都在 `chat_once_model` 里。
struct RealAttempt<'a> {
    state: &'a AppState,
    config: &'a AgentConfig,
    messages: &'a [ChatMessage],
    tools: &'a Option<Vec<serde_json::Value>>,
    key_id: &'a str,
}

impl ModelAttempt for RealAttempt<'_> {
    type Output = (ChatResponse, String, Option<String>);

    async fn attempt(
        &mut self,
        model: &str,
        deadline: std::time::Instant,
    ) -> Result<Self::Output, AgentLlmError> {
        chat_once_model(
            self.state,
            self.config,
            model,
            self.messages,
            self.tools,
            self.key_id,
            deadline,
        )
        .await
    }
}

/// 跨模型 failover 的主循环（生产与测试共用同一段代码）。
async fn run_model_failover<A: ModelAttempt>(
    models: &[String],
    budget: std::time::Duration,
    attempt: &mut A,
) -> Result<A::Output, AgentLlmError> {
    let deadline = std::time::Instant::now() + budget;
    // 逐模型试。每个模型内部仍保留原有的「渠道级 failover + 时间维度重试」，
    // 本层只负责「这个模型没指望了，换下一个」。
    let mut tried: Vec<String> = Vec::new();
    let mut last_err: Option<AgentLlmError> = None;
    for (index, model) in models.iter().enumerate() {
        let Some(left) = remaining_budget(deadline) else {
            // 预算耗尽：把还没轮到的模型如实列出来，而不是让错误看起来像
            // 「候选清单里本来就没有别的模型」。
            mark_untried(&mut tried, &models[index..]);
            break;
        };
        // **硬闸**：这个模型的整次尝试（含 `try_candidates` 扫全部渠道、以及
        // 退避睡眠）都必须在剩余预算内完成。只在「两次尝试之间看表」是不够的：
        // 单次非流式请求默认超时 300s（`body_timeout_secs`）、流式更长。
        match tokio::time::timeout(left, attempt.attempt(model, deadline)).await {
            Ok(Ok(v)) => {
                if !tried.is_empty() {
                    tracing::info!(
                        "agent llm switched model after {:?}, succeeded on {model}",
                        tried
                    );
                }
                return Ok(v);
            }
            Ok(Err(e)) => {
                tracing::warn!("agent llm model {model} failed: {e}");
                tried.push(format!("{model}: {e}"));
                last_err = Some(e);
            }
            Err(_elapsed) => {
                let e = AgentLlmError::AllChannelsFailed(format!(
                    "尝试超过总耗时预算（{}s）被中止",
                    budget.as_secs()
                ));
                tracing::warn!("agent llm model {model} aborted: {e}");
                tried.push(format!("{model}: {e}"));
                last_err = Some(e);
                mark_untried(&mut tried, &models[index + 1..]);
                break;
            }
        }
    }
    // 收口规则见 `final_error`：只试过一个模型时保留原错误类型（前端靠
    // `NoChannelAvailable` / `TransientExhausted` 区分「配置问题」与「稍后重试」），
    // 试过多个（或一个都没跑起来）才汇总成 `AllModelsFailed`，逐条带原因。
    Err(final_error(&tried, last_err))
}

/// 单个模型的完整尝试：渠道级 failover + 时间维度重试（轮次来自配置）。
///
/// 与 [`chat_once_as`] 的分工：本函数只对一个模型负责，失败（无论暂时还是
/// 确定性）都只把错误还给调用方，**不再把整次调用打死** —— 换模型是外层的决定。
async fn chat_once_model(
    state: &AppState,
    config: &AgentConfig,
    model: &str,
    messages: &[ChatMessage],
    tools: &Option<Vec<serde_json::Value>>,
    key_id: &str,
    deadline: std::time::Instant,
) -> Result<(ChatResponse, String, Option<String>), AgentLlmError> {
    let rounds = config
        .transient_retry_rounds
        .clamp(1, MAX_TRANSIENT_RETRY_ROUNDS);
    let mut last_msg = String::new();
    let mut last_recoverable = false;

    for round in 1..=rounds {
        if remaining_budget(deadline).is_none() {
            break;
        }
        // 每轮重新解析候选：冷却中的渠道会随冷却到期重新可用，
        // 固定用第一轮的候选列表会白白错过刚恢复的渠道。
        match resolve_candidates(state, config, model) {
            Ok(candidates) => {
                match try_candidates(state, config, model, &candidates, messages, tools, key_id)
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
                            // 确定性失败（模型已关闭/参数错误/凭证硬性无效）：
                            // 本模型重试无意义，交回外层换下一个模型。
                            return Err(AgentLlmError::AllChannelsFailed(last_msg));
                        }
                    }
                }
            }
            Err(e @ AgentLlmError::NoChannelAvailable { .. }) => {
                // 全渠道冷却也属暂时性：等冷却到期再试。
                //
                // **不把 `last_msg` 覆写成这条占位文案**：上一轮若已拿到具体的
                // 上游失败原因（如 `Auth failed: 401 Unauthorized`），那才是排障
                // 要看的；被「暂无可用渠道或账号」顶掉之后，最终错误只剩一句
                // 无从下手的话。线上 AI 运维就是这样：渠道都在、只是账号 401，
                // 报出来的却是「该模型暂无可用渠道」。
                if last_msg.is_empty() {
                    last_msg = e.to_string();
                }
                last_recoverable = true;
            }
            Err(e) => return Err(e),
        }

        if round < rounds {
            let wait = retry_backoff(config, round);
            // 等完就超过总预算了就别等了 —— 外层也没时间再试下一个模型。
            if remaining_budget(deadline)
                .is_none_or(|left| left <= std::time::Duration::from_millis(wait))
            {
                break;
            }
            tracing::warn!(
                "agent llm transient failure on model {model} (round {round}/{rounds}): {last_msg}; retrying in {wait}ms"
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

/// 抖动幅度：±20%。
const RETRY_JITTER_PERCENT: u64 = 20;

/// 抖动源：进程内单调原子计数器（每次调用自增）。
///
/// 用计数器而非 rand 伪随机：不引新依赖、无锁原子、天然线程安全；且不同
/// 调用（尤其并发重试）计数不同，产生的抖动值错开，正是抖动要解决的
/// 「多请求同步重试打爆上游」问题。计数器周期性取模，回绕不影响均匀性。
static RETRY_JITTER_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 第 `round` 轮失败后的等待时长（毫秒）。
///
/// 按轮次线性退避：`retry_interval_ms`（默认 0 → 兜底 1000ms）× 轮次，
/// 上限 15s。运维场景上游抖动通常秒级恢复，线性退避够用且比指数退避更
/// 早重试、体感更好。
///
/// 退避上叠加 **±20% 抖动**：基础值 × (0.8..=1.2)。抖动打破多请求在相
/// 同时刻齐步重试的同步节拍（惊群），降低对刚恢复的上游造成二次冲击。
/// 抖动后仍截断到 15s 硬上限，保证「一次对话等待不超 15s」的既有契约。
fn retry_backoff(config: &AgentConfig, round: u32) -> u64 {
    let base = if config.retry_interval_ms > 0 {
        config.retry_interval_ms
    } else {
        1000
    };
    let base = (base * u64::from(round)).min(15_000);
    // 抖动系数（千分比）等距覆盖 [1000-P*10, 1000+P*10]：P=20 → [800,1200]，
    // 即 ±20%。101 档、步长 = 跨度/100。全程整数运算避免浮点，saturating_mul
    // 防大基数溢出。计数器 0..=100 映射到该区间，实现 ±20% 抖动。
    let span = RETRY_JITTER_PERCENT * 10;
    let lower = 1_000 - span;
    let upper = 1_000 + span;
    let seq = RETRY_JITTER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 101;
    let coeff = lower + seq * (upper - lower) / 100;
    let jittered = base.saturating_mul(coeff).saturating_div(1000);
    jittered.min(15_000)
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
            messages: messages.to_vec(),
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
                    model,
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
                    model,
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
        purge_applied: false,
        special_route: None,
        id: request_id.to_string(),
        user_id: None,
        key_id: Some(key_id.to_string()),
        channel_id: channel_id.map(|s| s.to_string()),
        channel_name: channel_name.map(|s| s.to_string()),
        // 供应商（与数据面同口径）：按 channel_id 反查；渠道已删/无渠道 → None。
        provider: channel_id
            .and_then(|cid| crate::api::openai::resolve::provider_of_channel_id(state, cid)),
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
        client_name: crate::api::client_id::SELF_CLIENT.to_string(),
        client_version: String::new(),
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
    use crate::config::AgentConfig;

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

    /// 退避时长：线性递增、有上限、带 ±20% 抖动，且 `retry_interval_ms=0`
    /// 时用 1s 兜底。
    ///
    /// 钉住四条边界：①配置 0 不能退化成「无等待连续猛打上游」；②轮次越高等
    /// 待越长（给上游恢复时间）；③抖动幅度 ±20%（基础值 × [0.8, 1.2]）；
    /// ④有 15s 硬上限（避免一次对话卡太久）。
    ///
    /// 断言用**范围**而非精确值：抖动是随机的（进程内原子计数器每次自增），
    /// 同一输入在不同调用下产生不同等待，精确值契约被打破——这是设计意图，
    /// 抖动本就要打破多请求同步重试的节拍。故以 [基础×0.8, 基础×1.2] 为界。
    #[test]
    fn retry_backoff_is_linear_capped_jittered_and_never_zero() {
        let cfg = AgentConfig {
            retry_interval_ms: 0,
            ..Default::default()
        };
        // 兜底基数 1000ms × 轮次 1 → 基础 1000，抖动范围 800..=1200
        assert!(
            (800..=1200).contains(&super::retry_backoff(&cfg, 1)),
            "0 配置须兜底 1s 基础，抖动 ±20%"
        );
        assert!(
            (1600..=2400).contains(&super::retry_backoff(&cfg, 2)),
            "2 轮基础 2000，抖动 ±20%"
        );
        let mut cfg = AgentConfig {
            retry_interval_ms: 500,
            ..Default::default()
        };
        assert!(
            (1200..=1800).contains(&super::retry_backoff(&cfg, 3)),
            "3 轮基础 1500，抖动 ±20%"
        );
        assert!(
            (1600..=2400).contains(&super::retry_backoff(&cfg, 4)),
            "4 轮基础 2000，抖动 ±20%"
        );
        // 上限：基数再大也不超过 15s；且因抖动可能下探，但不得超上限
        cfg.retry_interval_ms = 10_000;
        assert!(
            super::retry_backoff(&cfg, 5) <= 15_000,
            "须有上限，抖动后仍不超 15s"
        );
        assert!(super::retry_backoff(&cfg, 99) <= 15_000);
    }

    /// 抖动须产生非确定值：同一输入多次调用不应恒定（否则等于没抖动）。
    #[test]
    fn retry_backoff_jitter_varies_across_calls() {
        let cfg = AgentConfig {
            retry_interval_ms: 1000,
            ..Default::default()
        };
        // 基础 = 1000 × 1 = 1000；抖动范围 800..=1200。跑多次应至少出现
        // 两个不同的值（计数器自增保证逐步变化）。
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..50 {
            seen.insert(super::retry_backoff(&cfg, 1));
        }
        assert!(seen.len() > 1, "抖动须使等待非恒定，实际集合 {seen:?}");
        assert!(seen.iter().all(|&v| (800..=1200).contains(&v)));
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
    // ─── 跨模型切换（failover）────────────────────────────────────

    /// 首选永远排第一，且清单里与首选同名的跳过 —— 否则会把同一个模型打两遍。
    #[test]
    fn compose_candidates_puts_primary_first_and_skips_duplicate() {
        let got = super::compose_model_candidates(
            "deepseek-v4.1-flash",
            vec![
                "deepseek-v4.1-flash".to_string(),
                "glm-5.3-flash".to_string(),
            ],
            5,
        );
        assert_eq!(
            got,
            vec!["deepseek-v4.1-flash", "glm-5.3-flash"],
            "首选第一且不重复"
        );
    }

    /// 封顶：清单再长也只取 limit 个（线上清单几十个，全试会把对话拖成几分钟）。
    #[test]
    fn compose_candidates_caps_at_limit() {
        let listed: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
        let got = super::compose_model_candidates("primary", listed, super::MAX_MODEL_CANDIDATES);
        assert_eq!(got.len(), super::MAX_MODEL_CANDIDATES, "应封顶到 5");
        assert_eq!(got[0], "primary", "首选仍在第一");
        assert_eq!(
            &got[1..],
            &["m0", "m1", "m2", "m3"],
            "按清单原序补齐到 5 个"
        );
    }

    /// 清单为空（= 渠道被显式锁定，`candidate_models` 直接返回单元素）时只试首选。
    #[test]
    fn compose_candidates_single_when_no_alternatives() {
        let got = super::compose_model_candidates("only-model", vec![], 5);
        assert_eq!(got, vec!["only-model"]);
    }

    /// 只试过一个模型时**保留原错误类型**：前端靠它区分「配置问题」与「稍后重试」。
    #[test]
    fn final_error_keeps_original_type_for_single_model() {
        let original = AgentLlmError::TransientExhausted {
            rounds: 3,
            last: "upstream timeout".to_string(),
        };
        let got = super::final_error(&["a: upstream timeout".to_string()], Some(original));
        assert!(
            matches!(got, AgentLlmError::TransientExhausted { .. }),
            "单模型时不得包成 AllModelsFailed: {got}"
        );
    }

    /// 试过多个模型时汇总，且**逐条带出模型名与原因** —— 这是运营能看出
    /// 「凭证 401 / 模型被关 404 / 渠道校验拦截」的唯一入口。
    #[test]
    fn final_error_lists_every_model_and_reason() {
        let tried = vec![
            "deepseek-v4.1-flash: Auth failed: 401 Unauthorized".to_string(),
            "glm-5.3-flash: upstream returned HTTP 404: model disabled".to_string(),
        ];
        let got = super::final_error(
            &tried,
            Some(AgentLlmError::AllChannelsFailed("x".to_string())),
        );
        let msg = got.to_string();
        assert!(
            matches!(got, AgentLlmError::AllModelsFailed(_)),
            "多模型应汇总: {msg}"
        );
        assert!(msg.contains("deepseek-v4.1-flash"), "应点名模型 A: {msg}");
        assert!(msg.contains("401"), "应带出 A 的原因: {msg}");
        assert!(msg.contains("glm-5.3-flash"), "应点名模型 B: {msg}");
        assert!(msg.contains("404"), "应带出 B 的原因: {msg}");
    }
    /// 预算已过 → `remaining_budget` 返回 `None`，外层据此不再发起新尝试。
    #[test]
    fn remaining_budget_none_once_deadline_passed() {
        let past = std::time::Instant::now() - std::time::Duration::from_millis(1);
        assert!(super::remaining_budget(past).is_none(), "过期应返回 None");
        let future = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let left = super::remaining_budget(future).expect("未来应有剩余");
        assert!(!left.is_zero() && left <= std::time::Duration::from_secs(60));
    }

    /// 预算耗尽时，未尝试的模型要如实列出并标明「未尝试」，
    /// 不得让错误看起来像「候选清单里本来没有别的模型」，也不得编造失败原因。
    #[test]
    fn untried_models_are_reported_when_budget_exhausted() {
        let mut tried = vec!["a: upstream timeout".to_string()];
        super::mark_untried(&mut tried, &["b".to_string(), "c".to_string()]);
        let msg = super::final_error(
            &tried,
            Some(AgentLlmError::AllChannelsFailed("x".to_string())),
        )
        .to_string();
        assert!(msg.contains("a:"), "应保留已试模型: {msg}");
        assert!(msg.contains("b: 未尝试"), "应标明 b 未尝试: {msg}");
        assert!(msg.contains("c: 未尝试"), "应标明 c 未尝试: {msg}");
        assert!(
            !msg.contains("b: upstream"),
            "不得给未试模型编造原因: {msg}"
        );
    }
    /// 假 attempt：记录调用顺序，按脚本决定成功/失败/挂住。
    /// 走的是生产同一段 `run_model_failover`，因此能真正区分新旧实现。
    struct FakeAttempt {
        order: Vec<String>,
        fail: Vec<&'static str>,
        hang_ms: u64,
    }

    impl super::ModelAttempt for FakeAttempt {
        type Output = &'static str;

        async fn attempt(
            &mut self,
            model: &str,
            _deadline: std::time::Instant,
        ) -> Result<Self::Output, AgentLlmError> {
            self.order.push(model.to_string());
            if self.hang_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.hang_ms)).await;
            }
            if self.fail.iter().any(|m| *m == model) {
                Err(AgentLlmError::AllChannelsFailed(format!("{model} boom")))
            } else {
                Ok("ok")
            }
        }
    }

    /// 主需求：某个模型失败后**真的换下一个**并成功。
    /// 把 `run_model_failover` 改回「只试一个模型」，这条立刻红。
    #[tokio::test]
    async fn failover_switches_to_next_model_and_records_order() {
        let models = vec!["a".to_string(), "b".to_string()];
        let mut fake = FakeAttempt {
            order: Vec::new(),
            fail: vec!["a"],
            hang_ms: 0,
        };
        let got =
            super::run_model_failover(&models, std::time::Duration::from_secs(60), &mut fake).await;
        assert_eq!(got.expect("b 应当成功"), "ok");
        assert_eq!(
            fake.order,
            vec!["a".to_string(), "b".to_string()],
            "应先试 a 再试 b"
        );
    }

    /// **硬闸**：挂住的尝试必须被预算切断，且未轮到的模型如实标「未尝试」。
    ///
    /// 钉的是生产循环里的 `tokio::time::timeout(剩余预算, attempt)`。
    /// 若把它拆成「尝试前检查一下时间」，这条会跑满 500ms×3 且拿不到「被中止」
    /// 文案 —— 断言全部失败。
    #[tokio::test]
    async fn failover_hard_stops_on_budget_and_marks_untried() {
        let models = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut fake = FakeAttempt {
            order: Vec::new(),
            fail: Vec::new(),
            hang_ms: 500,
        };
        let started = std::time::Instant::now();
        let got =
            super::run_model_failover(&models, std::time::Duration::from_millis(50), &mut fake)
                .await;
        let elapsed = started.elapsed();
        assert!(got.is_err(), "应报错");
        assert!(
            elapsed < std::time::Duration::from_millis(400),
            "必须被硬闸切断，实际 {elapsed:?}"
        );
        let msg = got.unwrap_err().to_string();
        assert!(msg.contains("被中止"), "应说明超预算被中止: {msg}");
        assert!(
            msg.contains("b: 未尝试") && msg.contains("c: 未尝试"),
            "应标注未尝试的模型: {msg}"
        );
        assert_eq!(fake.order, vec!["a".to_string()], "只该真正试过 a");
    }
}
