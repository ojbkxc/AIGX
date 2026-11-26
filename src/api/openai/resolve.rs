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

/// 尝试明细的 `provider` 字段取值（与 `RequestLog.provider` **同口径**）。
///
/// 口径单点在 [`crate::log::provider_label_for_channel`]：渠道型取协议族、
/// 账号池型取该渠道下首个非空账号 `provider`。这里只负责补「没有渠道引用」
/// 的那一档 —— Hub 兜底候选是 `(bridge, None, None)`，`ch_ref` 为 `None`，
/// 直接 `unwrap_or_default()` 会把 provider 写成空串，排障时会被误判成
/// 「供应商返回空」（见 `bridge::hub_fallback` 模块头），故用 bridge 自报
/// 身份（如 `hub:cloudflare`）兜底。
///
/// `state`：只为账号型渠道的「家」反查（`AccountSessionStore::provider_for_channel`，
/// O(1) 索引）；非账号型渠道连哈希查找都不做。
pub(crate) fn attempt_provider(
    state: &AppState,
    ch_ref: Option<&crate::channel::Channel>,
    bridge: &dyn crate::bridge::Bridge,
) -> String {
    match ch_ref {
        Some(channel) => {
            let account_provider = (channel.channel_type == crate::channel::ChannelType::Account)
                .then(|| state.account_sessions.provider_for_channel(&channel.id))
                .flatten();
            crate::log::provider_label_for_channel(
                channel.channel_type,
                account_provider.as_deref(),
            )
        }
        None => bridge.name().to_string(),
    }
}

/// 按渠道 ID 反查供应商（写请求日志时用：那一层手上通常只剩 `channel_id`）。
///
/// `None` = 没有渠道（Hub 兜底 / 选路前就失败）→ 调用方不要填 provider。
/// 渠道存在但已不在库里（日志写的这一刻渠道刚被删）同样返回 `None` ——
/// 这是**如实留白**，不猜一个曾经的值：把一个已删渠道的旧协议族写进日志，
/// 读侧无法与「当时确实是这一家」区分。
///
/// 账号型渠道的「家」走 [`crate::account_session::AccountSessionStore::provider_for_channel`]
/// 的 O(1) 索引，不逐条扫账号表。
pub(crate) fn provider_of_channel_id(state: &AppState, channel_id: &str) -> Option<String> {
    let channel = state.channel_store.get(channel_id)?;
    let account_provider = if channel.channel_type == crate::channel::ChannelType::Account {
        state.account_sessions.provider_for_channel(&channel.id)
    } else {
        None
    };
    Some(crate::log::provider_label_for_channel(
        channel.channel_type,
        account_provider.as_deref(),
    ))
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
    let key_obj = state
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
        })?;
    // 撤销闸（model_key_source）：权威已发布时，要求 Key 仍是当前代有效 Key。
    // 未发布（未初始化）时放行，保持既有行为。
    if !crate::model_key_source::check_current(&key) {
        return Err(openai_error_response(
            "auth_error",
            "API key has been revoked",
            StatusCode::UNAUTHORIZED,
        ));
    }
    Ok(key_obj)
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
/// Key 级 provider 白名单判定（`allowedProviders`，数据面候选过滤用）。
///
/// provider 名按渠道形态取：
/// - **账号型渠道**：无单一 provider（真正干活的是选中的账号，`AccountBridge`
///   内部按账号 provider 再过滤），这里一律放行；
/// - **插件渠道**（`ChannelType::Plugin`）：真实 provider 是 `plugin_provider`
///   字段——`channel_type` 只是字面量 `"plugin"`，不是 provider id。用字面量
///   匹配会让任何配了白名单的 Key 把插件渠道**全部**过滤掉；
/// - 其余渠道：provider 名 = `channel_type`（`openai_compatible` 等）。
pub(crate) fn channel_matches_allowed_providers(
    channel_type: crate::channel::ChannelType,
    plugin_provider: &str,
    allowed: &[String],
) -> bool {
    use crate::channel::ChannelType;
    match channel_type {
        ChannelType::Account => true,
        ChannelType::Plugin => allowed.iter().any(|p| p == plugin_provider),
        other => allowed.iter().any(|p| p == other.as_str()),
    }
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
        // Key 级 provider 白名单（07 D 章 allowedProviders）：渠道型按真实
        // provider 过滤（插件渠道 = plugin_provider，不是字面量 "plugin"）；
        // 账号型无单一 provider，进 AccountBridge 后按账号 provider 过滤
        //（与上面 model_rules 的分层同一形状）。
        if let Some(allowed) = allowed_providers {
            if !channel_matches_allowed_providers(ch.channel_type, &ch.plugin_provider, allowed) {
                if let Some(c) = capture.as_mut() {
                    c.push_filtered(&ch.id, REASON_KEY_PROVIDER_NOT_ALLOWED);
                }
                continue;
            }
        }
        match ch.channel_type {
            // 空密钥渠道合法（免鉴权上游 / 网关侧 IP 白名单）：
            // bridge 内部对空 key 不发送鉴权头。
            // 四种渠道级 base_url 类型统一走 [`build_channel_bridge_at`]（主入口与
            // 备用入口共用同一构造口径，见 `backup_base_urls` 的落点）。
            crate::channel::ChannelType::OpenaiCompatible
            | crate::channel::ChannelType::Anthropic
            | crate::channel::ChannelType::Gemini
            | crate::channel::ChannelType::Zai => {
                if let Some(bridge) = build_channel_bridge_at(state, ch, &ch.base_url) {
                    result.push((bridge, Some(ch.id.clone()), Some(ch.clone())));
                }
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
            crate::channel::ChannelType::Plugin => {
                // 插件渠道：转发由插件构造并发出（网关不实现上游
                // 伪装头/签名）。plugin_provider 指明插件里的哪一家；
                // 三级查找：原生插件进程（每包一进程）→ 脚本宿主（一个 Bun
                // 承载全部脚本包）→ Wasm 沙箱宿主（进程内 wasmtime）。
                // 三者互斥：wasm 包的 provider 会进 `PackRegistry` 的归属表，
                // 因此 `script_takeover_blocked` 会挡住脚本宿主接管；而 wasm
                // 包不占 `processes` 表，`process_for_provider` 对它返回 None。
                // 都未接管（未装/未启用/崩溃退避窗口）→ 不 push，调用方按
                // no_bridge 处理。
                let provider = ch.plugin_provider.clone();
                if provider.is_empty() {
                    tracing::warn!("[Plugin] 渠道 {} 未配置 plugin_provider，跳过", ch.id);
                } else if let Some(process) = state.plugin_host.process_for_provider(&provider) {
                    result.push((
                        Arc::new(crate::bridge::plugin::PluginBridge::new(process, provider)),
                        Some(ch.id.clone()),
                        Some(ch.clone()),
                    ));
                } else if let Some(route) = state.script_host.route_for_provider(&provider) {
                    result.push((
                        Arc::new(crate::bridge::plugin::PluginBridge::new_script(
                            route.channel,
                            route.provider,
                        )),
                        Some(ch.id.clone()),
                        Some(ch.clone()),
                    ));
                } else if let Some(host) = crate::plugin_host::wasm::host_for_provider(&provider)
                    .filter(|_| state.plugin_host.registry().is_plugin_owned(&provider))
                {
                    // 归属表闸门：wasm 宿主表只按「已启动」登记，逐家停用
                    // （`disabled_providers`）后宿主可能仍在跑；不查归属会让
                    // 「已停用的 provider」继续被数据面转发（与上游「停用即不
                    // 登记 adapter」不符）。原生路径已在
                    // `manager::process_for_provider` 内过滤、脚本路径在
                    // `runtime::route_for_provider` 内过滤，这里是第三条兜底。
                    // **不影响脚本托管的 provider**：脚本包不在 `PackRegistry`
                    // 里，但这类 provider 的 `host_for_provider` 本来就是 None
                    //（且脚本分支在上一步已 return），条件对其无副作用。
                    result.push((
                        Arc::new(crate::bridge::plugin::PluginBridge::new_wasm(
                            host, provider,
                        )),
                        Some(ch.id.clone()),
                        Some(ch.clone()),
                    ));
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
            // 兜底候选必须带上可自述的身份（`hub:cloudflare`）：
            // 不包装时它是 `(bridge, None, None)`，上层日志打出
            // `channel None failed: …`、请求日志 provider 列为空——
            // 线上排障会误判成「供应商返回空」。见 `bridge::hub_fallback`。
            result.push((
                crate::bridge::hub_fallback::HubBridge::new(b, "hub:cloudflare"),
                None,
                None,
            ));
        }
    }
    // 渠道级重试上限：最多尝试 `max_retries + 1` 个候选渠道（0 = 只试一个）
    let limit = crate::config::max_retries() as usize + 1;
    if result.len() > limit {
        result.truncate(limit);
    }
    result
}

/// 调试入口的单个候选：渠道 + 调度档位 + 本轮 notice。
#[derive(Debug, Clone)]
pub(crate) struct ChatTestPick {
    pub channel: crate::channel::Channel,
    /// 调度档位：0 = 健康，1 = 启用但冷却/断路器/限流中，2 = 渠道已禁用。
    /// 只影响**顺序**，不影响是否尝试。
    pub tier: u8,
    /// 写进 `AttemptDetail.notice`：解释「为什么它排在这里 / 为什么明知它不健康还试」。
    pub notice: Option<String>,
}

/// 档位常量（前端/日志按数值读；文案在 notice 里）。
pub(crate) const CHAT_TEST_TIER_HEALTHY: u8 = 0;
pub(crate) const CHAT_TEST_TIER_THROTTLED: u8 = 1;
pub(crate) const CHAT_TEST_TIER_DISABLED: u8 = 2;

/// 选路分类（纯函数）：**全部渠道进候选池**，只把「模型白名单明确不含该模型」的
/// 剔出去（那一条试了就是必然的 4xx，白白拖慢整条链）。
///
/// 这是 AI 运维（人工调试入口）与生产数据面的**根本分歧**，必须写清楚：
/// - 数据面 `/v1/chat/completions` 要保护生产流量：`disabled` / 冷却 / 断路器打开
///   的渠道一律不调度。那条口径**一个字都不改**（本函数只被 chat_test 调用）。
/// - 调试入口是人在点「发送」，要的是「这个模型到底能不能用」：某个渠道 disabled
///   可能是管理员临时关的，也可能只是凭证过期 —— **试一次就知道**。排除它只会让
///   用户看到「没有可用渠道」却不知道为什么。
///
/// 因此这里不再按 `is_enabled()` 过滤（旧实现过滤了，39 个渠道只剩 4 个进候选，
/// `glm-4.7-flash` 更是只剩 1 个 —— 用户以为 failover 没生效）。渠道是否健康改由
/// [`chat_test_candidates`] 打**档位**（健康 → 限流/冷却 → 已禁用），只影响顺序：
/// 能用的先试、试通即停，全败才会轮到 disabled 那批。
///
/// 返回 `(候选, 真正没进候选的渠道+原因)`。
#[allow(clippy::type_complexity)] // (候选, 未进候选的渠道+原因)
pub(crate) fn classify_chat_test_channels(
    all: Vec<crate::channel::Channel>,
    requested_model: &str,
) -> (
    Vec<crate::channel::Channel>,
    Vec<crate::log::FilteredChannel>,
) {
    let requested = requested_model.trim();
    let mut excluded: Vec<crate::log::FilteredChannel> = Vec::new();
    if requested.is_empty() {
        return (all, excluded);
    }
    let declared: Vec<crate::channel::Channel> = all
        .iter()
        .filter(|c| c.auto_pick_supports(requested))
        .cloned()
        .collect();
    if declared.is_empty() {
        // 无渠道声明该模型：退「models 为空（=全部）」的渠道兜底（改造前口径）
        let fallback: Vec<crate::channel::Channel> =
            all.into_iter().filter(|c| c.models.is_empty()).collect();
        return (fallback, excluded);
    }
    // 只有「models 白名单明确写了别的模型」才真的剔除 —— 那不是「不健康」，
    // 是「这一家客观上没有这个模型」，试了必然 4xx。
    for ch in all.iter() {
        if ch.auto_pick_supports(requested) {
            continue;
        }
        let reason = if ch.models.is_empty() {
            // 声明全部但快照未收录：快照可能过期，**仍然试**（弱证据），不剔除
            continue;
        } else {
            "渠道 models 白名单不含该模型"
        };
        excluded.push(crate::log::FilteredChannel {
            channel_id: ch.id.clone(),
            reason: reason.to_string(),
        });
    }
    (declared, excluded)
}

/// 调试入口（`/api/channels/chat_test`、Playground、AI 运维）的候选链。
///
/// 返回 `(候选链, 未进候选的渠道+原因)`。候选链的**第一个元素最可能成功**，
/// 之后依次降级：健康 → 启用但冷却/断路器/限流 → 已禁用。
///
/// 冷却/断路器判定一律走**只读**查询（`is_in_cooldown` / `get_state` /
/// `aimd_allows`）：候选构造阶段若调 `is_dispatch_allowed` 会原子占用半开名额，
/// 把真实流量本该拿到的试探租约抢走 —— 那是给数据面用的有副作用接口。
#[allow(clippy::type_complexity)] // (候选链, 未进候选的渠道+原因)
pub(crate) fn chat_test_candidates(
    state: &AppState,
    requested_model: &str,
) -> (Vec<ChatTestPick>, Vec<crate::log::FilteredChannel>) {
    let (picked, excluded) =
        classify_chat_test_channels(state.channel_store.list(), requested_model);
    let picks = order_chat_test_candidates(picked, |ch| {
        if !ch.is_enabled() {
            (
                CHAT_TEST_TIER_DISABLED,
                Some(
                    "渠道已禁用（管理员已关闭）；调试入口仍按最后手段尝试，不改其状态".to_string(),
                ),
            )
        } else if state.channel_store.is_in_cooldown(&ch.id) {
            (
                CHAT_TEST_TIER_THROTTLED,
                Some("冷却中（近期失败），仍会尝试".to_string()),
            )
        } else if state.channel_store.circuit_breaker().get_state(&ch.id) == "open" {
            (
                CHAT_TEST_TIER_THROTTLED,
                Some("断路器打开（连续失败），仍会尝试".to_string()),
            )
        } else if !state.channel_store.aimd_allows(&ch.id) {
            (
                CHAT_TEST_TIER_THROTTLED,
                Some("限速冷却中（自适应降速），仍会尝试".to_string()),
            )
        } else {
            (CHAT_TEST_TIER_HEALTHY, None)
        }
    });
    (picks, excluded)
}

/// [`chat_test_candidates`] 的纯排序部分（无状态，便于单测钉住顺序）。
///
/// 排序键：**档位升序 → priority 降序 → 入参顺序**（`sort_by_key` 是稳定排序，
/// 所以同档同优先级保持渠道表原顺序，不引入随机）。冷却与 disabled 渠道
/// **不会被丢弃**，只是排到后面 —— 这正是「候选池不该因为不健康而缩到 1 个」的落点。
pub(crate) fn order_chat_test_candidates(
    mut picked: Vec<crate::channel::Channel>,
    tier_of: impl Fn(&crate::channel::Channel) -> (u8, Option<String>),
) -> Vec<ChatTestPick> {
    let mut picks: Vec<ChatTestPick> = picked
        .drain(..)
        .map(|ch| {
            let (tier, notice) = tier_of(&ch);
            ChatTestPick {
                channel: ch,
                tier,
                notice,
            }
        })
        .collect();
    picks.sort_by(|a, b| {
        a.tier
            .cmp(&b.tier)
            .then_with(|| b.channel.priority.cmp(&a.channel.priority))
    });
    picks
}

/// 把原始模型名解析为上游真实模型名（= 价格表里的名）。
///
/// 优先级：渠道级映射（链式）> 全局映射 > **渠道清单实际写法** > 原名透传。
/// 仿 new-api ModelMappedHelper：渠道级支持链式重定向 + 循环检测。
///
/// 「渠道清单实际写法」这层对齐 A2A `entry_id_in_manifest`：下游请求大小写
/// 一律接受，但发给上游的名字必须与上游清单一一对应——上游清单是
/// `Deepseek-v4.1-Flash` 就发那个写法，不是请求里的大小写。
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
    let mapped = global_mapper.resolve(origin);
    if mapped.as_str() != origin {
        // 映射命中：target 即上游真名（配置口径），原样使用
        return mapped;
    }
    // 未命中映射：按渠道清单里的实际写法一一对应上游（大小写不敏感命中）
    if let Some(ch) = channel {
        if let Some(actual) = ch
            .effective_models()
            .iter()
            .find(|m| m.eq_ignore_ascii_case(origin))
        {
            return actual.clone();
        }
    }
    mapped
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
        // 全忙：选号阶段就没占到槽位，**任何上游都没收到过这条请求**，
        // 换下一个渠道完全安全（不会重复计费）且很可能成功（别家未必忙）。
        | BridgeError::AccountBusy(_)
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
///
/// ── 口径（v1.0.73 收紧）──────────────────────────────────────
/// 旧实现留了个口子：只要模型被任一启用渠道声明/发现就放行（按 0 计费），
/// 理由是「列表看得到、调用却 503」体验割裂。现在列表侧（`/v1/models` 与
/// `/api/models/available`）已经同步剔除未定价模型，「看得到」不再成立，
/// 这个口子只剩下「渠道声明即可白嫖」的效果，与产品要求（没有售价就不让用）
/// 相反，故移除。
///
/// 免费模型仍可用：它在定价目录里有 `price_type = "free"` 条目（0 计费），
/// `get_price` 命中即放行。管理员要放开一个模型，正确做法是在模型管理里
/// 给它配价（哪怕配成免费），而不是让渠道声明替它兜底。
pub(crate) fn ensure_model_priced(
    state: &AppState,
    model: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    if state.pricing_store.get_price(model).is_none() {
        return Err(openai_error_response(
            "model_not_priced",
            &format!("Model '{model}' has no price configured, contact admin"),
            StatusCode::FORBIDDEN,
        ));
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
                    && (s.models.iter().any(|m| m.eq_ignore_ascii_case(model))
                        || (s.models.is_empty()
                            && c.effective_models()
                                .iter()
                                .any(|m| m.eq_ignore_ascii_case(model))))
            });
        }
        !state
            .model_rules
            .is_disabled(c.channel_type.as_str(), model)
            && c.effective_models().iter().any(|m| m == model)
    })
}

/// 渠道级 `base_url` 的 bridge 构造（**主入口与备用入口共用同一口径**）。
///
/// 只覆盖有渠道级地址的四种类型；账号型渠道的上游地址由账号的 `base_url`
/// 决定（`AccountBridge` 内部按账号构造），插件/CF 没有渠道级地址 —— 一律
/// 返回 `None`，备用入口对它们不生效（管理面写入口也已拒绝配置）。
///
/// 备用入口复用同一个 `state.http_client`（同一代理出口），只替换地址 ——
/// 「不改变账号选择、代理出口与选路结果」在构造层就成立。
pub(crate) fn build_channel_bridge_at(
    state: &AppState,
    ch: &crate::channel::Channel,
    base_url: &str,
) -> Option<Arc<dyn Bridge>> {
    let key = ch.decode_api_key();
    let client = &state.http_client;
    match ch.channel_type {
        crate::channel::ChannelType::OpenaiCompatible => {
            Some(crate::bridge::openai::make_bridge_with_quirks(
                base_url,
                &key,
                client,
                ch.quirks.clone(),
            ))
        }
        crate::channel::ChannelType::Anthropic => {
            Some(crate::bridge::anthropic::make_bridge_with_quirks(
                base_url,
                &key,
                client,
                ch.quirks.clone(),
            ))
        }
        crate::channel::ChannelType::Gemini => {
            Some(crate::bridge::gemini::make_bridge(base_url, &key, client))
        }
        crate::channel::ChannelType::Zai => Some(crate::bridge::zai::make_bridge_with_quirks(
            base_url,
            &key,
            client,
            ch.quirks.clone(),
        )),
        _ => None,
    }
}

/// 第 `tried` 个备用入口（从 0 开始）的地址与 bridge；没有更多返回 `None`。
///
/// 列表来自 [`crate::channel::Channel::trusted_backup_base_urls`]（运行期再过滤
/// 一次：仅可信 HTTPS、去重、排除主入口），顺序即配置顺序。
pub(crate) fn next_backup_bridge(
    state: &AppState,
    ch: &crate::channel::Channel,
    tried: usize,
) -> Option<(String, Arc<dyn Bridge>)> {
    let entry = ch.trusted_backup_base_urls().into_iter().nth(tried)?;
    let bridge = build_channel_bridge_at(state, ch, &entry)?;
    Some((entry, bridge))
}

/// 非流式聊天 + 备用入口：主入口 **DNS 解析失败 / 连接建立失败** 时按序改试
/// 备用入口；其余错误（上游 5xx/429、超时、解析失败、请求已受理后的传输错误）
/// 原样返回，交给外层 failover。
///
/// 返回 `(结果, 最终使用的 bridge)`：自动续写复用成功轮的 bridge（可能是备用
/// 入口那一个）。每次换入口记一条 warn（排障可见「为什么这次走了备用」）。
pub(crate) async fn chat_with_backup_entries(
    state: &AppState,
    ch: Option<&crate::channel::Channel>,
    mut bridge: Arc<dyn Bridge>,
    req: &crate::bridge::ChatFormat,
    ctx: &crate::bridge::BridgeContext,
) -> (
    Result<crate::bridge::ChatResponse, crate::bridge::BridgeError>,
    Arc<dyn Bridge>,
) {
    let mut tried = 0usize;
    loop {
        match bridge.chat(req, ctx).await {
            Err(error) if crate::bridge::reachability::is_connect_failure(&error) => {
                match ch.and_then(|ch| next_backup_bridge(state, ch, tried)) {
                    Some((entry, next)) => {
                        tried += 1;
                        tracing::warn!(
                            "channel {:?} 主/上一个入口连接失败（{error}），改试备用入口 #{tried} {entry}",
                            ch.map(|c| c.id.as_str()).unwrap_or_default()
                        );
                        bridge = next;
                    }
                    None => return (Err(error), bridge),
                }
            }
            other => return (other, bridge),
        }
    }
}

/// 流式聊天 + 备用入口（语义同 [`chat_with_backup_entries`]）。
///
/// 只重试 `chat_stream` **建流阶段**的连接失败；建流成功后的流内错误由既有
/// 首事件守卫/failover 处理，不在这里换入口（避免重复计费）。
pub(crate) async fn chat_stream_with_backup_entries(
    state: &AppState,
    ch: Option<&crate::channel::Channel>,
    mut bridge: Arc<dyn Bridge>,
    req: &crate::bridge::ChatFormat,
    ctx: &crate::bridge::BridgeContext,
) -> (
    Result<crate::bridge::ChatChunkStream, crate::bridge::BridgeError>,
    Arc<dyn Bridge>,
) {
    let mut tried = 0usize;
    loop {
        match bridge.chat_stream(req, ctx).await {
            Err(error) if crate::bridge::reachability::is_connect_failure(&error) => {
                match ch.and_then(|ch| next_backup_bridge(state, ch, tried)) {
                    Some((entry, next)) => {
                        tried += 1;
                        tracing::warn!(
                            "channel {:?} 主/上一个入口连接失败（{error}），改试备用入口 #{tried} {entry}",
                            ch.map(|c| c.id.as_str()).unwrap_or_default()
                        );
                        bridge = next;
                    }
                    None => return (Err(error), bridge),
                }
            }
            other => return (other, bridge),
        }
    }
}

/// 渠道候选元组：bridge + 渠道 id + 渠道快照（映射/成本价解析用）。
pub(crate) type BridgeCandidate = (
    Arc<dyn Bridge>,
    Option<String>,
    Option<crate::channel::Channel>,
);

#[cfg(test)]
mod tests {
    use super::channel_matches_allowed_providers;
    use crate::channel::ChannelType;

    /// 造一个最小 `Channel`（只为候选排序/分组单测）。
    fn sample(id: &str, priority: i64) -> crate::channel::Channel {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "name": format!("ch-{id}"),
            "channel_type": "openai_compatible",
            "base_url": "https://upstream.example.com",
            "api_key": "sk-test",
            "priority": priority,
            "status": "enabled",
            "models": ["deepseek-chat"],
        }))
        .expect("测试渠道构造")
    }

    fn disabled(id: &str, priority: i64) -> crate::channel::Channel {
        let mut ch = sample(id, priority);
        ch.status = "disabled".to_string();
        ch
    }

    /// ★本次口径：候选池 = **全部**渠道，disabled 也在池内（只是排最后）。
    ///
    /// 旧实现按 `is_enabled()` 过滤，39 个渠道只剩 4 个进候选、
    /// `glm-4.7-flash` 更只剩 1 个 —— 用户以为 failover 没生效。断言「disabled 也
    /// 在 kept 里」就是这个口径的钉子。
    #[test]
    fn classify_chat_test_channels_keeps_disabled_channels_as_candidates() {
        let (kept, excluded) = super::classify_chat_test_channels(
            vec![sample("on", 10), disabled("off", 99)],
            "deepseek-chat",
        );
        let ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["on", "off"],
            "disabled 渠道必须留在候选池里（排最后，但仍然会试）"
        );
        assert!(
            excluded.is_empty(),
            "没有「白名单明确不含」的渠道时不该有排除项：{excluded:?}"
        );
    }

    /// 只有「`models` 白名单明确写了别的模型」才真剔除 —— 试了必然 4xx 的请求
    /// 不该浪费整条链的时间；`models` 留空（声明全部）但快照未收录的属于弱证据，
    /// 快照可能过期，仍然进候选。
    #[test]
    fn classify_chat_test_channels_only_excludes_explicit_whitelist_mismatch() {
        let mut whitelist_other = sample("white-other", 10);
        whitelist_other.models = vec!["other-model".to_string()];
        let mut snapshot_lacks = sample("snapshot-lacks", 20);
        snapshot_lacks.models = Vec::new();
        snapshot_lacks.discovered_models = vec!["other-model".to_string()];

        let (kept, excluded) = super::classify_chat_test_channels(
            vec![whitelist_other, snapshot_lacks, sample("kept", 5)],
            "deepseek-chat",
        );
        let kept_ids: Vec<&str> = kept.iter().map(|c| c.id.as_str()).collect();
        assert!(
            kept_ids.contains(&"snapshot-lacks"),
            "快照未收录是弱证据（可能过期），仍要进候选：{kept_ids:?}"
        );
        let reasons: Vec<(&str, &str)> = excluded
            .iter()
            .map(|f| (f.channel_id.as_str(), f.reason.as_str()))
            .collect();
        assert_eq!(excluded.len(), 1, "只有白名单明确不含才剔除：{reasons:?}");
        assert_eq!(reasons[0].0, "white-other");
        assert!(
            reasons[0].1.contains("白名单"),
            "原因要说清楚：{}",
            reasons[0].1
        );
    }

    /// 判据与改造前一致：**没有**渠道声明该模型时，退 `models` 留空的渠道兜底。
    #[test]
    fn classify_chat_test_channels_falls_back_to_all_models_channels() {
        let mut empty_models = sample("no-models", 3);
        empty_models.models = Vec::new();
        let (kept, _excluded) =
            super::classify_chat_test_channels(vec![empty_models], "some-unlisted-model");
        assert_eq!(kept.len(), 1, "无渠道声明该模型时退「models 为空」的渠道");
        assert_eq!(kept[0].id, "no-models");
    }

    /// ★三档排序：健康 → 冷却/断开/限流 → **已禁用**；档内按 priority 降序，
    /// 同档同优先级保持渠道表顺序（稳定，不引入随机）。冷却与禁用**都不丢**。
    #[test]
    fn chat_test_candidates_order_by_tier_then_priority() {
        let picks = super::order_chat_test_candidates(
            vec![
                disabled("off-high", 99),
                sample("cooling", 50),
                sample("healthy-low", 1),
                sample("healthy-high", 30),
            ],
            |ch| {
                if !ch.is_enabled() {
                    (super::CHAT_TEST_TIER_DISABLED, Some("已禁用".to_string()))
                } else if ch.id == "cooling" {
                    (super::CHAT_TEST_TIER_THROTTLED, Some("冷却中".to_string()))
                } else {
                    (super::CHAT_TEST_TIER_HEALTHY, None)
                }
            },
        );
        let ids: Vec<&str> = picks.iter().map(|p| p.channel.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["healthy-high", "healthy-low", "cooling", "off-high"],
            "健康优先、其次限流/冷却、disabled 排最后；档内按 priority 降序"
        );
        assert_eq!(picks.len(), 4, "四个渠道一个都不能丢");
        assert_eq!(
            picks[3].tier,
            super::CHAT_TEST_TIER_DISABLED,
            "disabled 档位要如实标出，供 attempt_details.notice 使用"
        );
        assert!(picks[3]
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("已禁用")));
    }

    /// 同档同优先级**保持渠道表顺序**（稳定排序，不引入随机）：
    /// 数据面在组内做 weight 加权随机，调试入口必须可复现。
    #[test]
    fn chat_test_candidates_keep_store_order_within_same_priority() {
        let picks =
            super::order_chat_test_candidates(vec![sample("a", 5), sample("b", 5)], |_ch| {
                (super::CHAT_TEST_TIER_HEALTHY, None)
            });
        let ids: Vec<&str> = picks.iter().map(|p| p.channel.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"], "同优先级保持入参（渠道表）顺序");
    }

    /// 表驱动：Key 级 provider 白名单在各渠道形态下的判定。
    ///
    /// 核心回归：插件渠道必须按 `plugin_provider` 匹配——用字面量
    /// `"plugin"` 会让配了白名单的 Key 把全部插件渠道筛掉（生产实证）。
    #[test]
    fn allowed_providers_table() {
        let allowed = vec!["kiro".to_string(), "anthropic".to_string()];
        let cases: &[(ChannelType, &str, bool, &str)] = &[
            (
                ChannelType::Plugin,
                "kiro",
                true,
                "插件渠道按 plugin_provider 命中",
            ),
            (
                ChannelType::Plugin,
                "astudio",
                false,
                "未命中白名单必须拦截",
            ),
            (
                ChannelType::Plugin,
                "",
                false,
                "空 plugin_provider 不能蒙混",
            ),
            (
                ChannelType::OpenaiCompatible,
                "",
                false,
                "渠道型仍按 channel_type 判定",
            ),
            (ChannelType::Anthropic, "", true, "渠道型命中"),
            (
                ChannelType::Account,
                "",
                true,
                "账号型在选号层过滤，这里放行",
            ),
        ];
        for (ty, plugin_provider, expected, why) in cases {
            assert_eq!(
                channel_matches_allowed_providers(*ty, plugin_provider, &allowed),
                *expected,
                "{why}（{ty:?}/{plugin_provider:?}）"
            );
        }
        // 白名单写字面量 "plugin" 不再放行所有插件渠道（它不是 provider id）
        assert!(!channel_matches_allowed_providers(
            ChannelType::Plugin,
            "kiro",
            &["plugin".to_string()]
        ));
    }
}
