use axum::{
    http::{HeaderMap, StatusCode},
    Json,
};

use super::*;
use crate::api::common::extract_api_key_bearer_first as extract_api_key;
use crate::api::common::extract_client_ip;
use crate::bridge::Bridge;
use crate::channel::routing_policy::RequestNeeds;
use crate::model::capability::CapabilityMask;
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
    /// 逐次上游尝试明细（#32「执行时间线」）：failover 循环里每次真实
    /// 上游调用一条，行数有界（超出转 [`Self::attempts_truncated`]）。
    pub attempts: Vec<crate::log::AttemptDetail>,
    /// attempts 被截断的条数（不静默丢）
    pub attempts_truncated: usize,
    /// 逐账号跳过明细缓冲：账号型渠道的 AccountBridge 选号时按渠道分组填充，
    /// 调用方记录日志时 drain。请求本地（同请求多账号渠道共享）。
    pub account_buf: Arc<std::sync::Mutex<Vec<crate::log::AccountSkipGroup>>>,
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

    /// 追加一条尝试明细（#32；行数有界，超出转计数——不静默丢）。
    /// 错误摘要截断 300 字符（对齐 agent/llm 的错误截断口径）。
    pub(crate) fn push_attempt(&mut self, detail: crate::log::AttemptDetail) {
        if self.attempts.len() >= crate::log::ROUTE_REPLAY_MAX_ATTEMPTS {
            self.attempts_truncated += 1;
            return;
        }
        let mut detail = detail;
        if let Some(err) = detail.error.take() {
            detail.error = Some(err.chars().take(300).collect());
        }
        self.attempts.push(detail);
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
    let key = extract_api_key(headers).ok_or_else(|| {
        openai_error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED)
    })?;
    let ip = extract_client_ip(headers);
    // 全局 IP 过滤（批次3）：白名单/黑名单优先于 token 级校验
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(openai_error_response(
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
            openai_error_response("auth_error", &e.to_string(), status)
        })
}
/// 列举类端点（`/v1/models`）的鉴权：完整校验但跳过模型白名单。
pub(crate) fn verify_api_key_listing(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<crate::api::auth::ApiKey, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers).ok_or_else(|| {
        openai_error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED)
    })?;
    let ip = extract_client_ip(headers);
    if let Some(ip_str) = ip.as_deref() {
        if let Err(ip_err) = crate::ip::check_ip(&state.ip_filter.get(), ip_str) {
            return Err(openai_error_response(
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
            openai_error_response("auth_error", &e.to_string(), status)
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
///
/// #40 §1：本函数 = [`resolve_bridges_with_affinity_needs`] 传
/// `RequestNeeds::default()`（无需求 = 不做能力过滤，与历史逐字节等价）。
/// 能从请求体推出能力需求的调用方用 `_needs` 版本（chat 侧见 #47 收口）。
pub(crate) fn resolve_bridges_with_affinity(
    state: &AppState,
    model: &str,
    session_id: Option<&str>,
    allowed_providers: Option<&[String]>,
    capture: Option<&mut RouteCapture>,
) -> Vec<BridgeCandidate> {
    resolve_bridges_with_affinity_needs(
        state,
        model,
        session_id,
        allowed_providers,
        capture,
        RequestNeeds::default(),
    )
}

/// [`resolve_bridges_with_affinity`] 的带能力需求版本（#40 §1 的**会话端点接缝**）。
///
/// 与默认版本的差别只有一处：`needs` 非空时按能力位过滤候选（语义见
/// [`resolve_bridges_needs`]）；亲和置顶逻辑逐字节相同（过滤发生在候选构造
/// 阶段，置顶只在过滤后的候选池里找，命中不了就自然不做置顶）。
///
/// 会话型端点（chat / anthropic / responses）应在调 `resolve_bridges` **之前**
/// 从请求体推出需求（`api::openai::needs::from_chat_body`）传进来。
pub(crate) fn resolve_bridges_with_affinity_needs(
    state: &AppState,
    model: &str,
    session_id: Option<&str>,
    allowed_providers: Option<&[String]>,
    capture: Option<&mut RouteCapture>,
    needs: RequestNeeds,
) -> Vec<BridgeCandidate> {
    let mut result = resolve_bridges_needs(state, model, allowed_providers, capture, needs);
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
///
/// #40 §1：本函数 = [`resolve_bridges_needs`] 传
/// `RequestNeeds::default()`（无需求 = 不做能力过滤，与历史逐字节等价）。
pub(crate) fn resolve_bridges(
    state: &AppState,
    model: &str,
    allowed_providers: Option<&[String]>,
    capture: Option<&mut RouteCapture>,
) -> Vec<BridgeCandidate> {
    resolve_bridges_needs(
        state,
        model,
        allowed_providers,
        capture,
        RequestNeeds::default(),
    )
}

/// 带能力需求的候选构造（#40 §1 数据面接线）。
///
/// `needs` 由调用方从请求体推出（唯一推导层 `api::openai::needs`），本函数
/// 只把它走**唯一映射链**换成能力位——
/// `RequestNeeds::required_tags()` → `CapabilityMask::from_tags` ——
/// 绝不在这里再写一遍「这个需求叫什么标签」。
///
/// 两级接线（`needs` 为空时两级都短路，零额外开销/零行为变化）：
/// - **渠道级粗筛**：`AccountSessionStore::channel_may_serve`（账号目录能力位
///   的并集）在候选构造阶段裁渠道 —— 粗筛必须在 `select_for_model_by_strategy`
///   的分组之前，否则被裁的渠道还会参与加权随机/轮询游标；
/// - **账号级精筛**：`AccountBridge::with_capabilities`（此刻上游真名已定，
///   按该账号目录里这个模型的能力位精确判定）。
///
/// 两者都是**有证据才裁**：目录无标签 / 模型名未收录 / 非账号型渠道一律放行
/// （#40-B 降级口径「宁放勿误伤」）。
pub(crate) fn resolve_bridges_needs(
    state: &AppState,
    model: &str,
    allowed_providers: Option<&[String]>,
    mut capture: Option<&mut RouteCapture>,
    needs: RequestNeeds,
) -> Vec<BridgeCandidate> {
    let mut result: Vec<BridgeCandidate> = Vec::new();

    // 第一级：通用渠道——按路由策略选取（对照 Agent2API「设置 → 路由」）：
    // weighted=priority 分组+组内加权随机（默认）；round_robin=组内确定性
    // 轮询；ranked=多因子 health×cost×rpm（策略来自 /api/routing，可热更）
    //
    // #40 多规则档案：与预演（`api/admin/routing.rs::handle_routing_queue`）
    // 走**同一判定序列**（契约 §3）：resolve_profile → 按生效 strategy 选候选
    // → plan_route 裁剪。`enabled=false`（默认）时 resolve_profile 直接返回
    // 策略基座、plan_route 原切片直通 —— 与历史逐字节等价且零额外分配。
    // 组 id 的第 1/2 环（请求头 / Key 绑定，契约 §6.1）随 `ApiKey` 字段在后续
    // 步骤接入，本步两处调用点均传 None（只走 global / 模型档案两层）。
    let policy = state.routing_policy.snapshot();
    let profile =
        crate::channel::routing_policy::resolve_profile(&policy, model, &state.model_mapper, None);
    if let Some(c) = capture.as_mut() {
        c.config = Some(crate::log::RouteConfigSnapshot {
            strategy: profile.strategy.as_str().to_string(),
            sticky_enabled: policy.sticky.sticky_enabled(),
        });
    }
    // #40 §1：本次请求的能力需求 → 能力位掩码。走**唯一映射链**
    // （`required_tags` → `CapabilityMask::from_tags`），本函数不认识标签名；
    // `EMPTY` = 无需求 = 两级过滤全短路（零额外开销、与历史逐字节等价）。
    let needs_mask = CapabilityMask::from_tags(needs.required_tags());
    let candidates = {
        // 渠道级粗筛：`channel_may_serve` 只读两把锁 + 掩码比较，无分配。
        let capability_gate = |ch: &crate::channel::Channel| {
            state.account_sessions.channel_may_serve(&ch.id, needs_mask)
        };
        let coarse: Option<crate::channel::ChannelGate<'_>> = if needs_mask.is_empty() {
            None
        } else {
            Some(&capability_gate)
        };
        state.channel_store.select_for_model_by_strategy_filtered(
            model,
            profile.strategy,
            &state.pricing_store,
            coarse,
        )
    };
    let plan = crate::channel::routing_policy::plan_route(&policy, &profile, &candidates);
    // #40 §2 账号级规则（参与名单 / 排序规则）：档案里真配了内容才构造
    // （默认关 = 零额外分配；空 Vec 的 clone 不分配）。同一次请求的所有
    // 账号型候选共享同一份，逐候选只做 Arc/小 Vec clone。
    let account_scope = (!profile.scope.account_order.is_empty()
        || !profile.scope.excluded_accounts.is_empty())
    .then(|| std::sync::Arc::new(profile.scope.clone()));
    let account_sort_rules = profile.sort_rules.clone();
    for ch in plan.iter() {
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
                // #40 §1 账号级精筛：把本次请求的能力位注入选号层（EMPTY =
                // 不接，行为与改造前逐字节一致）
                let account_capture = capture.as_ref().map(|c| c.account_buf.clone());
                let bridge: Arc<dyn crate::bridge::Bridge> = Arc::new(
                    crate::bridge::account::AccountBridge::new(
                        ch.id.clone(),
                        state.account_sessions.clone(),
                    )
                    .with_rules(state.model_rules.clone())
                    .with_plugin_disabled(state.provider_gates.clone())
                    .with_allowed_providers(allowed_providers.map(<[String]>::to_vec))
                    .with_account_capture(account_capture)
                    .with_account_rules(account_scope.clone(), account_sort_rules.clone())
                    .with_capabilities(needs_mask),
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
        return Err(openai_error_response(
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
            return Err(openai_error_response(
                "model_not_priced",
                &format!("Model '{model}' has no price configured, contact admin"),
                StatusCode::SERVICE_UNAVAILABLE,
            ));
        }
    }
    Ok(())
}

/// 脱敏规则的 provider 作用域标识（对齐 Agent2API `sanitize.applies_to(provider_id)`）。
///
/// A2A 的 `provider_id` 是「家」的 id（workbuddy / qoder …）。AIGX 的渠道有两种形态：
/// - **账号型渠道**（`channel_type = account`）：真正干活的是绑定账号，取该渠道下
///   首个非空 `provider` —— 用户在脱敏作用域里写的正是这个家名；
/// - 其余渠道：没有「家」的概念，退化为渠道类型名（`openai_compatible` / `anthropic` …）。
///
/// 取不到渠道（例如候选渠道已失效）时返回空串：空串只有在作用域列表为空（= 全部）
/// 时才命中，与「拿不准就按全部处理」的保守方向一致。
pub(crate) fn sanitize_provider_id(
    state: &AppState,
    channel: Option<&crate::channel::Channel>,
) -> String {
    let Some(channel) = channel else {
        return String::new();
    };
    if channel.channel_type == crate::channel::ChannelType::Account {
        // 账号型渠道的「家」在账号上；取不到（无账号 / provider 为空）→ 空串。
        return state
            .account_sessions
            .provider_for_channel(&channel.id)
            .unwrap_or_default();
    }
    channel.channel_type.as_str().to_string()
}

/// 转发点脱敏（归一化消息列表）：主开关 + provider 作用域，就地改写 `messages`。
///
/// 调用点必须是「这一家即将发送之前」（A2A `send_body` 的时机）。每次尝试的
/// 请求都是从同一份原始 `chat_req` 克隆出来的，所以在循环里逐次调用不会叠加改写。
pub(crate) fn sanitize_attempt_messages(
    state: &AppState,
    channel: Option<&crate::channel::Channel>,
    messages: &mut [crate::bridge::ChatMessage],
) {
    if !crate::config::sanitize_fingerprints_enabled() {
        return;
    }
    let provider = sanitize_provider_id(state, channel);
    crate::sanitize::sanitize_messages_for_provider(&provider, messages);
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
