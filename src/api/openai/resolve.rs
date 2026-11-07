use axum::{
    http::{HeaderMap, StatusCode},
    Json,
};

use super::*;
use crate::api::common::extract_api_key_bearer_first as extract_api_key;
use crate::api::common::extract_client_ip;
use crate::bridge::Bridge;
use serde_json::Value;
use std::sync::Arc;

// ── Phase1-B 路由回放捕获 ───────────────────────────────────────────
//
// 性能约束（Phase1 红线，逐条对齐 verifier 审查基线）：
// - 捕获结构是栈上小 Vec，落库**复用 LogOp::Request → LogWriter 既有异步
//   有界队列**（满队列丢弃不阻塞），不新增任何 IO 路径或全局锁；
// - 过滤原因用常量码（&'static str），**无逐候选 String 格式化**；每过滤
//   一个渠道仅一次 channel_id 克隆，规模 O(渠道数)、大小有界；
// - 开关关闭（`route_replay_enabled()==false`）时调用方传 None，零分配。

/// resolve 层过滤原因码（前端按码映射文案；运行期 failover 的原因沿用
/// chat/anthropic 记录点已有的人类可读文案，两者键空间不重叠）
pub const REASON_MODEL_RULES_DISABLED: &str = "model_rules_disabled";
pub const REASON_KEY_PROVIDER_NOT_ALLOWED: &str = "key_provider_not_allowed";

/// 一次转发的路由捕获（`route_replay` 开关开启时才有实例）。
#[derive(Default)]
pub(crate) struct RouteCapture {
    /// resolve 层被过滤的渠道（未进 failover 循环；运行期 failover 的
    /// 过滤由 chat/anthropic 记录点另行并入 log.filtered_channels）。
    /// 上限 [`crate::log::ROUTE_REPLAY_MAX_FILTERED`]（#26），超出转计数。
    pub filtered: Vec<crate::log::FilteredChannel>,
    /// filtered 被截断的条数（不静默丢）
    pub filtered_truncated: usize,
    /// 选路那一刻的策略快照
    pub config: Option<crate::log::RouteConfigSnapshot>,
    /// 逐账号跳过明细缓冲：账号型渠道的 AccountBridge 选号时填充，
    /// 调用方记录日志时 drain。请求本地（同请求多账号渠道共享）。
    pub account_buf: Arc<std::sync::Mutex<Vec<crate::log::AccountSkip>>>,
}

impl RouteCapture {
    pub fn new() -> Self {
        Self::default()
    }

    fn push_filtered(&mut self, channel_id: &str, reason: &'static str) {
        if self.filtered.len() >= crate::log::ROUTE_REPLAY_MAX_FILTERED {
            self.filtered_truncated += 1;
            return;
        }
        self.filtered.push(crate::log::FilteredChannel {
            channel_id: channel_id.to_string(),
            reason: reason.to_string(),
        });
    }
}

/// 验证 API Key 并执行全部鉴权检查（状态/过期/模型白名单/额度/IP）。
///
/// 参照 new-api token.go 的校验逻辑。返回 ApiKey 实例供计费使用。
pub(crate) fn verify_api_key_full(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<crate::api::auth::ApiKey, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;
    let ip = extract_client_ip(headers);
    // 全局 IP 过滤（批次3）：白名单/黑名单优先于 token 级校验
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(error_response(
                "ip_blocked",
                &ip_err.to_string(),
                StatusCode::FORBIDDEN,
            ));
        }
    }
    // B22：按结构化错误变体映射状态码，取代原先的 msg.contains(...) 文本匹配
    state
        .api_key_store
        .validate_request(&key, model, ip.as_deref())
        .map_err(|e| {
            use crate::api::auth::ApiKeyError;
            let status = match e {
                // 凭证本身无效：401
                ApiKeyError::Invalid | ApiKeyError::Disabled => StatusCode::UNAUTHORIZED,
                // 凭证有效但无权限/过期/超额：403
                ApiKeyError::Expired
                | ApiKeyError::ModelNotAllowed(_)
                | ApiKeyError::QuotaExhausted
                | ApiKeyError::UserQuotaExhausted
                | ApiKeyError::IpNotAllowed(_) => StatusCode::FORBIDDEN,
            };
            error_response("auth_error", &e.to_string(), status)
        })
}
/// 列举类端点（`/v1/models`）的鉴权：完整校验但跳过模型白名单。
pub(crate) fn verify_api_key_listing(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::api::auth::ApiKey, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;
    let ip = extract_client_ip(headers);
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(error_response(
                "ip_blocked",
                &ip_err.to_string(),
                StatusCode::FORBIDDEN,
            ));
        }
    }
    state
        .api_key_store
        .validate_listing(&key, ip.as_deref())
        .map_err(|e| {
            use crate::api::auth::ApiKeyError;
            let status = match e {
                ApiKeyError::Invalid | ApiKeyError::Disabled => StatusCode::UNAUTHORIZED,
                ApiKeyError::Expired
                | ApiKeyError::ModelNotAllowed(_)
                | ApiKeyError::QuotaExhausted
                | ApiKeyError::UserQuotaExhausted
                | ApiKeyError::IpNotAllowed(_) => StatusCode::FORBIDDEN,
            };
            error_response("auth_error", &e.to_string(), status)
        })
}
/// 根据模型名解析提供商并获取全部候选 Bridge（B06：渠道 failover）。
///
/// 调度逻辑（参照 aisix dispatch_two_tier + new-api channel 选取）：
/// 1. 先查 ChannelStore 是否有支持该 model 的 openai_compatible/anthropic 渠道 → 动态构造 Bridge
/// 2. 回退到 Hub 专用提供商（cloudflare）
///
/// 返回按 priority/weight 排序的候选列表（bridge, channel_id），
/// 调用方依次尝试：失败（上游可重试错误）时切换下一个渠道。
///
/// 阶段2：`resolve_bridges_with_affinity` 在此之上叠加 session 粘性路由——
/// 亲和缓存命中的渠道被移到候选列表首位（若仍在候选池且未被断路器拦截）。
/// 粘性档位由路由策略控制：`off` 时跳过亲和（每请求独立选路）。
///
/// Phase1-B 路由回放：`capture` 非 None 时（`route_replay` 开关开启），
/// resolve 层的过滤原因与策略快照随选路填充进捕获结构，随请求日志落库。
/// 性能约束：开关关闭传 None，**零增量**；开启时每过滤一个渠道仅一次
/// channel_id 克隆（原因用常量码，无逐候选 String 格式化），规模 O(渠道数)。
pub(crate) fn resolve_bridges_with_affinity(
    state: &AppState,
    model: &str,
    session_id: Option<&str>,
    allowed_providers: Option<&[String]>,
    mut capture: Option<&mut RouteCapture>,
) -> Vec<BridgeCandidate> {
    let mut result = resolve_bridges(state, model, allowed_providers, capture.as_deref_mut());
    let policy = state.routing_policy.snapshot();
    if let Some(sid) = session_id {
        if policy.sticky.sticky_enabled() {
            if let Some(affinity_id) = state.channel_store.affinity_cache().lookup(sid, model) {
                if let Some(pos) = result
                    .iter()
                    .position(|(_, cid, _)| cid.as_deref() == Some(affinity_id.as_str()))
                {
                    if pos > 0 {
                        let item = result.remove(pos);
                        result.insert(0, item);
                        tracing::debug!(
                            "affinity hit: session {sid} model {model} -> channel {affinity_id}"
                        );
                    }
                }
            }
        }
    }
    result
}
/// 无亲和性版本（既有端点沿用：embeddings/images/audio 等无会话语义的请求）。
pub(crate) fn resolve_bridges(
    state: &AppState,
    model: &str,
    allowed_providers: Option<&[String]>,
    mut capture: Option<&mut RouteCapture>,
) -> Vec<BridgeCandidate> {
    let mut result: Vec<BridgeCandidate> = Vec::new();

    // 第一级：通用渠道——按路由策略选取（对照 Agent2API「设置 → 路由」）：
    // weighted=priority 分组+组内加权随机（默认）；round_robin=组内确定性
    // 轮询；ranked=多因子 health×cost×rpm（策略来自 /api/routing，可热更）
    let policy = state.routing_policy.snapshot();
    if let Some(c) = capture.as_mut() {
        c.config = Some(crate::log::RouteConfigSnapshot {
            strategy: policy.strategy.as_str().to_string(),
            sticky_enabled: policy.sticky.sticky_enabled(),
        });
    }
    let candidates = state.channel_store.select_for_model_by_strategy(
        model,
        policy.strategy,
        &state.pricing_store,
    );
    for ch in &candidates {
        // 统一模型管理里按 (provider, model) 关掉的模型不再进候选。
        // 账号型渠道的 provider 由账号决定，渠道级无从判断 —— 那一层由
        // `AccountSessionStore::acquire` 按账号白名单过滤，这里跳过即可。
        if ch.channel_type != crate::channel::ChannelType::Account
            && state
                .model_rules
                .is_disabled(ch.channel_type.as_str(), model)
        {
            if let Some(c) = capture.as_mut() {
                c.push_filtered(&ch.id, REASON_MODEL_RULES_DISABLED);
            }
            continue;
        }
        // Key 级 provider 白名单（07 D 章 allowedProviders）：渠道型按
        // channel_type 过滤；账号型无单一 provider，进 AccountBridge 后按
        // 账号 provider 过滤（与上面 model_rules 的分层同一形状）。
        if let Some(allowed) = allowed_providers {
            if ch.channel_type != crate::channel::ChannelType::Account
                && !allowed.iter().any(|p| p == ch.channel_type.as_str())
            {
                if let Some(c) = capture.as_mut() {
                    c.push_filtered(&ch.id, REASON_KEY_PROVIDER_NOT_ALLOWED);
                }
                continue;
            }
        }
        match ch.channel_type {
            // 空密钥渠道合法（免鉴权上游 / 网关侧 IP 白名单）：
            // bridge 内部对空 key 不发送鉴权头
            crate::channel::ChannelType::OpenaiCompatible => {
                let key = ch.decode_api_key();
                result.push((
                    crate::bridge::openai::make_bridge_with_quirks(
                        &ch.base_url,
                        &key,
                        &state.http_client,
                        ch.quirks.clone(),
                    ),
                    Some(ch.id.clone()),
                    Some(ch.clone()),
                ));
            }
            crate::channel::ChannelType::Anthropic => {
                let key = ch.decode_api_key();
                result.push((
                    crate::bridge::anthropic::make_bridge_with_quirks(
                        &ch.base_url,
                        &key,
                        &state.http_client,
                        ch.quirks.clone(),
                    ),
                    Some(ch.id.clone()),
                    Some(ch.clone()),
                ));
            }
            crate::channel::ChannelType::Gemini => {
                let key = ch.decode_api_key();
                result.push((
                    crate::bridge::gemini::make_bridge(&ch.base_url, &key, &state.http_client),
                    Some(ch.id.clone()),
                    Some(ch.clone()),
                ));
            }
            crate::channel::ChannelType::Zai => {
                let key = ch.decode_api_key();
                result.push((
                    crate::bridge::zai::make_bridge_with_quirks(
                        &ch.base_url,
                        &key,
                        &state.http_client,
                        ch.quirks.clone(),
                    ),
                    Some(ch.id.clone()),
                    Some(ch.clone()),
                ));
            }
            crate::channel::ChannelType::Account => {
                // 账号型渠道：凭证不进 Channel，bridge 在调用时按账号池选号
                // Phase1-B：route_replay 开启时共享逐账号跳过明细缓冲
                let account_capture = capture.as_ref().map(|c| c.account_buf.clone());
                let bridge: Arc<dyn crate::bridge::Bridge> = Arc::new(
                    crate::bridge::account::AccountBridge::new(
                        ch.id.clone(),
                        state.account_sessions.clone(),
                    )
                    .with_rules(state.model_rules.clone())
                    .with_plugin_disabled(state.provider_gates.clone())
                    .with_allowed_providers(allowed_providers.map(<[String]>::to_vec))
                    .with_account_capture(account_capture),
                );
                result.push((bridge, Some(ch.id.clone()), Some(ch.clone())));
            }
            crate::channel::ChannelType::Cloudflare => {
                // CF 渠道走 Hub 专用桥接
                if let Some(b) = state.hub.get_specialized("cloudflare") {
                    result.push((b, Some(ch.id.clone()), Some(ch.clone())));
                }
            }
        }
    }

    // 第二级：回退到 Hub 专用提供商（cloudflare），无通用渠道 ID。
    // 仅在通用渠道列表未产出任何候选时追加，避免 CF 桥接重复出现。
    //
    // 收紧条件：fallback 桥接受任意模型名，若放任"无候选即回退"，
    // 只在禁用渠道上声明的模型（或任何有定价的名字）仍会被客户路由到
    // CF Worker——违反"禁用渠道不暴露、不可用"的语义。因此仅当模型
    // 由任一启用渠道声明/发现，或存在全局映射（管理员显式暴露的别名）
    // 时才允许 fallback；否则返回空，让调用方按 no_bridge 拒绝。
    if result.is_empty()
        && (model_declared_by_channel(state, model) || state.model_mapper.is_active(model))
    {
        if let Some(b) = state.hub.get_specialized("cloudflare") {
            result.push((b, None, None));
        }
    }
    // 渠道级重试上限：最多尝试 `max_retries + 1` 个候选渠道（0 = 只试一个）
    let limit = crate::config::max_retries() as usize + 1;
    if result.len() > limit {
        result.truncate(limit);
    }
    result
}
/// 把原始模型名解析为上游真实模型名（= 价格表里的名）。
///
/// 优先级：渠道级映射（链式）> 全局映射 > 原名透传。
/// 仿 new-api ModelMappedHelper：渠道级支持链式重定向 + 循环检测。
pub(crate) fn resolve_upstream_model(
    origin: &str,
    channel: Option<&crate::channel::Channel>,
    global_mapper: &crate::model::ModelMapper,
) -> String {
    if let Some(ch) = channel {
        if let Some(m) = ch.resolve_channel_mapping(origin) {
            return m;
        }
    }
    global_mapper.resolve(origin)
}
/// 判断桥接错误是否值得切换到下一个渠道重试（B06）。
///
/// 仅对"上游侧可恢复错误"做渠道切换：5xx、超时、网络传输错误、
/// 上游限流（429）、渠道凭证失效（401/403）等——换一个渠道可能成功。
/// 4xx 客户端错误（上下文超限、参数错误、模型不存在）由请求本身决定，
/// 切换渠道大概率同样失败，应直接原样返回给客户端。
pub(crate) fn is_retryable_bridge_error(e: &crate::bridge::BridgeError) -> bool {
    use crate::bridge::BridgeError;
    match e {
        BridgeError::Timeout { .. }
        | BridgeError::Transport(_)
        | BridgeError::StreamAborted
        | BridgeError::RateLimited
        | BridgeError::AllAccountsFailed(_)
        | BridgeError::UpstreamDecode(_)
        | BridgeError::AuthError(_)
        | BridgeError::InvalidUpstreamCredentials(_) => true,
        BridgeError::UpstreamStatus { status, .. } => {
            // 内置可重试：5xx / 鉴权 / 限流；外加管理员配置的「直接换号/换渠道」名单
            *status >= 500
                || *status == 401
                || *status == 403
                || *status == 429
                || crate::config::is_retry_status(*status)
        }
        BridgeError::Config(_)
        | BridgeError::InvalidUpstreamConfig(_)
        | BridgeError::ModelNotFound(_) => false,
    }
}
/// 解析计费分组：优先取绑定用户的 group，否则用 api_key.group。
///
/// 参照 new-api：用户级 group 优先于 token 级 group。
pub(crate) fn resolve_billing_group(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
) -> String {
    if let Some(uid) = &api_key.user_id {
        if let Some(user) = state.user_store.get_by_id(uid) {
            if !user.group.is_empty() {
                return user.group.clone();
            }
        }
    }
    api_key.group.clone()
}
/// 校验用户分组的模型权限，返回计费分组名。
///
/// 返回 Err 时调用方应返回 403。
pub(crate) fn check_group_model_permission(
    state: &AppState,
    api_key: &crate::api::auth::ApiKey,
    model: &str,
) -> Result<String, (StatusCode, Json<Value>)> {
    let group = resolve_billing_group(state, api_key);
    if !state.user_group_store.allows_model(&group, model) {
        return Err(error_response(
            "model_not_allowed",
            &format!("Model '{model}' is not allowed for group '{group}'"),
            StatusCode::FORBIDDEN,
        ));
    }
    Ok(group)
}
/// B09：推理前置校验——模型必须已配置定价，未配置直接拒绝请求。
///
/// 未配置价格的模型若被放行，事后计费将静默为 0（产生免费用量）；
/// 在入口处拦截可避免“管理员忘记配置价格 → 用户免费白嫖”的资金损失。
pub(crate) fn ensure_model_priced(
    state: &AppState,
    model: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    if state.pricing_store.get_price(model).is_none() {
        // 放行：模型由任一启用渠道声明或发现（models ∪ discovered_models），
        // 即管理员显式暴露的模型——即便 pricing_store 无条目也允许调用（按 0 计费），
        // 避免列表显示可用但调用 503 的体验割裂。仅对"无渠道声明又无定价"的
        // 任意模型名才拒绝（防止用户猜模型名蹭免费）。
        if !model_declared_by_channel(state, model) {
            return Err(error_response(
                "model_not_priced",
                &format!("Model '{model}' has no price configured, contact admin"),
                StatusCode::SERVICE_UNAVAILABLE,
            ));
        }
    }
    Ok(())
}
/// 模型是否由任一启用渠道声明或发现（models 非空 = 白名单；空 = 全部，用发现快照）。
///
/// 账号型渠道的模型声明在账号上（`AccountSession.models`），渠道级 models 通常
/// 为空 —— 只查渠道会让「只在账号池里声明的模型」被误判为未声明。
pub(crate) fn model_declared_by_channel(state: &AppState, model: &str) -> bool {
    // 走只读快照（`Arc<Vec<Channel>>`）：每请求都会调用，返回的 Arc 仅做一次
    // 指针拷贝，避免 `list()` 那样每请求 clone 全部渠道（含凭证字段）。
    let channels = state.channel_store.snapshot();
    channels.iter().any(|c| {
        if !c.is_enabled() {
            return false;
        }
        if c.channel_type == crate::channel::ChannelType::Account {
            return state.account_sessions.for_channel(&c.id).iter().any(|s| {
                s.status == "enabled"
                    && !state.model_rules.is_disabled(&s.provider, model)
                    && (s.models.iter().any(|m| m == model)
                        || (s.models.is_empty() && c.effective_models().iter().any(|m| m == model)))
            });
        }
        !state
            .model_rules
            .is_disabled(c.channel_type.as_str(), model)
            && c.effective_models().iter().any(|m| m == model)
    })
}

/// 渠道候选元组：bridge + 渠道 id + 渠道快照（映射/成本价解析用）。
pub(crate) type BridgeCandidate = (
    Arc<dyn Bridge>,
    Option<String>,
    Option<crate::channel::Channel>,
);
