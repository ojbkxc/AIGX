use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

use crate::account::AccountPool;
use crate::bridge::{
    tool_repair, Bridge, BridgeContext, ChatFormat, ChatMessage, EmbeddingRequest, FinishReason,
    ResponsesPassthrough, Role,
};
use crate::channel::ChannelStore;
use crate::config::ConfigManager;
use crate::health::{HealthTracker, LivezState};
use crate::hub::Hub;
use crate::ip::IpFilterStore;
use crate::log::LogStore;
use crate::model::ModelMapper;
use crate::notify::NotifyService;
use crate::payment::order_store::OrderStore;
use crate::pricing::exchange_rate::ExchangeRateService;
use crate::pricing::price_sync::PriceSyncService;
use crate::pricing::PricingStore;
use crate::proxy::CfApiClient;
use crate::ratelimit::RateLimiter;
use crate::redemption::RedemptionStore;
use crate::usage::UsageTracker;
use crate::user::UserStore;
use crate::user_group::UserGroupStore;

// SeaORM 数据库连接（仅当启用 sea-orm feature 时可用）
#[cfg(feature = "sea-orm")]
use sea_orm::DatabaseConnection;

use super::auth::ApiKeyStore;

/// 共享应用状态
#[derive(Clone)]
pub struct AppState {
    pub api_client: Arc<CfApiClient>,
    pub model_mapper: Arc<ModelMapper>,
    pub usage_tracker: Arc<UsageTracker>,
    pub account_pool: Arc<AccountPool>,
    pub api_key_store: Arc<ApiKeyStore>,
    pub config_manager: Arc<ConfigManager>,
    pub hub: Arc<Hub>,
    pub user_store: Arc<UserStore>,
    pub order_store: Arc<OrderStore>,
    pub epay_client: Arc<crate::payment::EpayClient>,
    /// Stripe 支付客户端
    pub stripe_client: Arc<crate::payment::stripe::StripeClient>,
    pub health_tracker: Arc<HealthTracker>,
    pub livez_state: Arc<LivezState>,
    /// 通用渠道存储（混用 CF + 第三方 OpenAI 兼容上游）
    pub channel_store: Arc<ChannelStore>,
    /// 模型定价目录
    pub pricing_store: Arc<PricingStore>,
    /// 用户分组存储
    pub user_group_store: Arc<UserGroupStore>,
    /// 日志与审计存储
    pub log_store: Arc<LogStore>,
    /// 兑换码存储
    pub redemption_store: Arc<RedemptionStore>,
    /// 限流器（多维度 RPM/TPM）
    pub rate_limiter: Arc<RateLimiter>,
    /// 通知服务（Telegram + SMTP + Slack + Webhook）
    pub notify_service: Arc<NotifyService>,
    /// 告警规则评估器（alert_patrol 巡检 + 管理 API 共享）
    pub alert_evaluator: std::sync::Arc<std::sync::Mutex<crate::notify::alert::AlertRuleEvaluator>>,
    /// 底层 FileStore（告警规则/历史持久化等轻量 KV 用）
    pub alert_store: Arc<crate::storage::FileStore>,
    /// 全局 IP 白名单/黑名单过滤（批次3 IP 管理）
    pub ip_filter: Arc<IpFilterStore>,
    /// 价格同步服务（批次3 多源定价同步）
    pub price_sync: Arc<std::sync::Mutex<PriceSyncService>>,
    /// 汇率服务（批次4 多币种转换）
    pub exchange_rate: Arc<ExchangeRateService>,
    /// 公开注册速率限制器（per-IP 计数缓存）。
    ///
    /// key=客户端 IP，value=当前 60 秒窗口内已发起的注册请求数。
    /// TTL=60s，超限（>5）返回 429 Too Many Requests。
    pub register_limiter: Arc<crate::cache::AsyncCache<String, u32>>,
    /// 登录限流器（per-IP 计数缓存，同 IP 每分钟最多 10 次尝试）。
    ///
    /// key=客户端 IP，value=当前 60 秒窗口内已发起的登录尝试数。
    /// TTL=60s，超限返回 429 Too Many Requests。
    pub login_limiter: Arc<crate::cache::AsyncCache<String, u32>>,
    /// 登录失败锁定状态（per-IP 连续失败计数）。
    ///
    /// key=客户端 IP，value=连续失败次数。连续失败 ≥5 次时锁定该 IP
    /// 5 分钟（登录失败时重置 TTL；锁定期间直接返回 429）。
    /// 成功登录清除计数。
    pub login_failures: Arc<crate::cache::AsyncCache<String, u32>>,
    /// SeaORM 数据库连接（可选后端）。
    ///
    /// - `None`：使用默认 FileStore（rusqlite bundled SQLite），零配置
    /// - `Some`：启用 SeaORM 后端（SQLite/PostgreSQL/MySQL），新数据写入 SeaORM
    ///
    /// 仅当启用 `sea-orm` feature 且 `config.database.url` 非空时为 `Some`。
    #[cfg(feature = "sea-orm")]
    pub db_conn: Option<DatabaseConnection>,
    /// 共享 HTTP 客户端（性能热点 H5/H6）。
    ///
    /// 全应用复用同一个 `reqwest::Client`，避免每次请求新建客户端带来的
    /// 连接池/TLS 握手开销。供 `bridge::openai::make_bridge` 与 admin 用量
    /// 刷新等 HTTP 调用使用。`reqwest::Client` 内部已基于 Arc，clone 廉价。
    pub http_client: Arc<reqwest::Client>,
    /// 响应缓存（exact-match：model + messages hash 作 key）。
    ///
    /// 参照 aisix aisix-cache 的 prompt 缓存设计：相同请求直接命中缓存，
    /// 避免重复计费与上游调用；命中时旁路计费与渠道调度。
    pub response_cache: Arc<crate::cache::AsyncCache<String, Value>>,
    /// Semantic routing (prompt embedding match)
    pub semantic_router: Arc<crate::semantic::SemanticRouter>,
}

impl AppState {
    /// 返回是否启用了 SeaORM 后端。
    ///
    /// 仅当 `db_conn` 为 `Some` 时返回 true。
    /// 未启用 `sea-orm` feature 时始终返回 false。
    pub fn has_sea_orm_backend(&self) -> bool {
        #[cfg(feature = "sea-orm")]
        {
            self.db_conn.is_some()
        }
        #[cfg(not(feature = "sea-orm"))]
        {
            false
        }
    }
}

/// 创建 OpenAI 兼容的错误响应
fn error_response(code: &str, message: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(serde_json::json!({
            "error": {
                "message": message,
                "type": "invalid_request_error",
                "param": null,
                "code": code
            }
        })),
    )
}

/// 从请求中提取 API Key（H8：实现移至 `api::common`，此处通过 use 别名保持调用不变）
use super::common::extract_api_key_bearer_first as extract_api_key;
/// 从请求头提取客户端 IP（H8：实现移至 `api::common`）
use super::common::extract_client_ip;

/// 验证请求中的 API Key（简化版，仅校验存在性与有效性，返回 key id）
fn verify_api_key(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let key = extract_api_key(headers)
        .ok_or_else(|| error_response("auth_error", "Missing API key", StatusCode::UNAUTHORIZED))?;

    let api_key = state
        .api_key_store
        .validate(&key)
        .ok_or_else(|| error_response("auth_error", "Invalid API key", StatusCode::UNAUTHORIZED))?;

    Ok(api_key.id)
}

/// 验证 API Key 并执行全部鉴权检查（状态/过期/模型白名单/额度/IP）。
///
/// 参照 new-api token.go 的校验逻辑。返回 ApiKey 实例供计费使用。
fn verify_api_key_full(
    state: &AppState,
    headers: &HeaderMap,
    model: &str,
) -> Result<super::auth::ApiKey, (StatusCode, Json<Value>)> {
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
            use super::auth::ApiKeyError;
            let status = match e {
                // 凭证本身无效：401
                ApiKeyError::Invalid | ApiKeyError::Disabled => StatusCode::UNAUTHORIZED,
                // 凭证有效但无权限/过期/超额：403
                ApiKeyError::Expired
                | ApiKeyError::ModelNotAllowed(_)
                | ApiKeyError::QuotaExhausted
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
pub fn resolve_bridges_with_affinity(
    state: &AppState,
    model: &str,
    session_id: Option<&str>,
) -> Vec<(Arc<dyn Bridge>, Option<String>)> {
    let mut result = resolve_bridges(state, model);
    if let Some(sid) = session_id {
        if let Some(affinity_id) = state.channel_store.affinity_cache().lookup(sid, model) {
            if let Some(pos) = result
                .iter()
                .position(|(_, cid)| cid.as_deref() == Some(affinity_id.as_str()))
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
    result
}

/// 无亲和性版本（既有端点沿用：embeddings/images/audio 等无会话语义的请求）。
pub fn resolve_bridges(state: &AppState, model: &str) -> Vec<(Arc<dyn Bridge>, Option<String>)> {
    let mut result: Vec<(Arc<dyn Bridge>, Option<String>)> = Vec::new();

    // 第一级：通用渠道（按 priority/weight 选取支持该 model 的渠道）
    let candidates = state.channel_store.select_for_model(model);
    for ch in &candidates {
        match ch.channel_type {
            crate::channel::ChannelType::OpenaiCompatible => {
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    result.push((
                        crate::bridge::openai::make_bridge(&ch.base_url, &key, &state.http_client),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Anthropic => {
                // Anthropic 原生上游走 AnthropicBridge（/v1/messages + x-api-key +
                // anthropic-version），而非复用 OpenAI bridge。对接真正的 Anthropic
                // 原生 API 必须用本 bridge，否则 401/400（参见 bridge/anthropic.rs）
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    result.push((
                        crate::bridge::anthropic::make_bridge(
                            &ch.base_url,
                            &key,
                            &state.http_client,
                        ),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Gemini => {
                // Google Gemini 原生上游走 GeminiBridge（/v1beta/models/{model}:generateContent
                // + x-goog-api-key 鉴权）。非 OpenAI 兼容形状，必须用本 bridge。
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    result.push((
                        crate::bridge::gemini::make_bridge(&ch.base_url, &key, &state.http_client),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Zai => {
                // 智谱 AI（Z.AI）上游走 ZaiBridge（Anthropic 兼容协议 + Bearer 鉴权）。
                // 与 AnthropicBridge 的区别：用 Bearer 而非 x-api-key，
                // 不需要 anthropic-version 头。
                let key = ch.decode_api_key();
                if !key.is_empty() {
                    result.push((
                        crate::bridge::zai::make_bridge(&ch.base_url, &key, &state.http_client),
                        Some(ch.id.clone()),
                    ));
                }
            }
            crate::channel::ChannelType::Cloudflare => {
                // CF 渠道走 Hub 专用桥接
                if let Some(b) = state.hub.get_specialized("cloudflare") {
                    result.push((b, Some(ch.id.clone())));
                }
            }
        }
    }

    // 第二级：回退到 Hub 专用提供商（cloudflare），无通用渠道 ID。
    // 仅在通用渠道列表未产出任何候选时追加，避免 CF 桥接重复出现。
    if result.is_empty() {
        if let Some(b) = state.hub.get_specialized("cloudflare") {
            result.push((b, None));
        }
    }
    result
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
            *status >= 500 || *status == 401 || *status == 403 || *status == 429
        }
        BridgeError::Config(_)
        | BridgeError::InvalidUpstreamConfig(_)
        | BridgeError::ModelNotFound(_) => false,
    }
}

/// 解析计费分组：优先取绑定用户的 group，否则用 api_key.group。
///
/// 参照 new-api：用户级 group 优先于 token 级 group。
pub fn resolve_billing_group(state: &AppState, api_key: &super::auth::ApiKey) -> String {
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
pub fn check_group_model_permission(
    state: &AppState,
    api_key: &super::auth::ApiKey,
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
pub fn ensure_model_priced(state: &AppState, model: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if state.pricing_store.get_price(model).is_none() {
        return Err(error_response(
            "model_not_priced",
            &format!("Model '{model}' has no price configured, contact admin"),
            StatusCode::SERVICE_UNAVAILABLE,
        ));
    }
    Ok(())
}

/// 执行计费扣减（用户 quota + key used_quota）。
///
/// 参照 new-api：优先扣 key 绑定用户 quota，再扣 key 自身额度；
/// 用户余额不足时跳过 key 扣费以保持计费一致性。
///
/// 当 `try_charge` 失败或扣费后余额低于阈值时，发送 `QuotaLow` 通知
/// （参照非流式分支阈值计算，与 handle_chat_completions 非流式分支一致）。
pub fn charge_usage(
    state: &AppState,
    api_key: &super::auth::ApiKey,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> i64 {
    charge_usage_with_tools(
        state,
        api_key,
        model,
        group,
        prompt_tokens,
        completion_tokens,
        None,
    )
}

/// 计费扣减（带工具按次调用附加费）。
///
/// 与 `charge_usage` 相同，额外将 `tool_calls`（工具名 → 调用次数）按
/// `PricingStore::calculate_tool_surcharge` 折算为附加费并入总 cost。
pub fn charge_usage_with_tools(
    state: &AppState,
    api_key: &super::auth::ApiKey,
    model: &str,
    group: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
    tool_calls: Option<&crate::pricing::ToolCallCounts>,
) -> i64 {
    // B09：无定价时记告警并按 0 计费兜底——请求已在入口被 ensure_model_priced
    // 拦截，此处仅防御“请求进行中价格被删除”的竞态场景
    let mut cost = match state.pricing_store.calculate_cost_quoted(
        model,
        prompt_tokens,
        completion_tokens,
        group,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("charge_usage: {e}, billing as 0 (price removed mid-request?)");
            0
        }
    };
    if let Some(tools) = tool_calls {
        cost = cost.saturating_add(
            state
                .pricing_store
                .calculate_tool_surcharge(tools, model, group),
        );
    }
    // Prometheus 成本指标：pricing 以 USD 计价，cost 即向上取整后的美元配额
    // （单位 1 美元），统一折算为微美元累加，供 /metrics 展示 aigx_cost_usd_total。
    if cost > 0 {
        crate::metrics::global().record_cost("usd", (cost as u64).saturating_mul(1_000_000));
        if let Some(uid) = &api_key.user_id {
            // 用户余额不足时跳过 key 扣费（问题 6）
            let charged = state.user_store.try_charge(uid, cost);
            // 额度不足或剩余过低通知（与非流式分支一致）
            if let Some(u) = state.user_store.get_by_id(uid) {
                let remaining = u.remaining();
                // 阈值：固定 1000 或 quota 的 10%，取较小者；扣费失败必通知
                let threshold = (u.quota / 10).clamp(1000, 10000);
                if !charged || remaining < threshold {
                    state
                        .notify_service
                        .notify_spawn(crate::notify::NotifyEvent::QuotaLow {
                            user_email: u.email.clone(),
                            remaining,
                        });
                }
            }
            // 问题 6：try_charge 失败时跳过 charge_quota，保持计费一致性
            if charged {
                let _ = state.api_key_store.charge_quota(&api_key.id, cost);
            }
        } else {
            let _ = state.api_key_store.charge_quota(&api_key.id, cost);
        }
    }
    cost
}

// ── 流式计费守卫（B05/B20）────────────────────────────────────────────

/// 流式计费共享状态：后缀事件（正常结束）与 Drop 守卫（断连兜底）共用。
///
/// 通过 `charged` 原子标志保证计费恰好执行一次：
/// - 流正常结束：后缀事件先 `swap` 抢到计费权并完成计费，守卫 drop 时直接跳过；
/// - 客户端断连：SSE 流被中途 drop，后缀事件不会执行，守卫在 Drop 中兜底计费。
///
/// 两条路径互斥，同时杜绝"双计费"与"断连零计费"。
///
/// 若客户端在计费完成前断连，守卫的 Drop 实现会负责兜底计费，确保请求最终被正确记账。
pub(crate) struct StreamBillingState {
    pub(crate) state: AppState,
    pub(crate) api_key: super::auth::ApiKey,
    pub(crate) model: String,
    pub(crate) group: String,
    /// 请求快照（用于估算 prompt tokens）
    pub(crate) chat_req: ChatFormat,
    /// 工具按次调用计数表（流式过程中统计 tool_use 起始块）
    pub(crate) tool_calls: Arc<parking_lot::Mutex<crate::pricing::ToolCallCounts>>,
    /// 输出文本累积器（用于估算 completion tokens）
    pub(crate) acc: Arc<parking_lot::Mutex<String>>,
    /// 计费权标志：false=未计费，true=已计费
    pub(crate) charged: Arc<std::sync::atomic::AtomicBool>,
    /// 事后限流记账句柄（断连兜底路径使用 clone）
    pub(crate) rate_bundle: Option<crate::ratelimit::ReservationBundle>,
    pub(crate) request_start: std::time::Instant,
    pub(crate) client_ip: Option<String>,
    pub(crate) request_id: String,
    pub(crate) channel_id: Option<String>,
}

impl StreamBillingState {
    /// 估算 token 并执行计费三件套：usage 累计 + charge_usage + 请求日志。
    ///
    /// 正常结束与断连兜底两路径共用的收尾逻辑；
    /// `latency_ms` 记录真实耗时（B20：修复流式请求硬编码 0 的问题）。
    /// 返回 (prompt_tokens, completion_tokens) 供调用方提交限流记账。
    pub(crate) fn finalize(&self) -> (u64, u64) {
        let output_text = self.acc.lock().clone();
        let completion_tokens = crate::token_estimate::count_text(&self.model, &output_text) as u64;
        let prompt_tokens =
            crate::token_estimate::count_chat_prompt(&self.model, &self.chat_req) as u64;

        // 累计用量
        self.state
            .usage_tracker
            .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

        // 扣费（try_charge + QuotaLow 通知 + key 扣减，含工具附加费）
        let tool_calls = self.tool_calls.lock().clone();
        let tool_calls_opt = if tool_calls.is_empty() {
            None
        } else {
            Some(&tool_calls)
        };
        let cost = charge_usage_with_tools(
            &self.state,
            &self.api_key,
            &self.model,
            &self.group,
            prompt_tokens,
            completion_tokens,
            tool_calls_opt,
        );

        // 记录请求日志（B20：真实耗时）
        let mut log = crate::log::RequestLog::new();
        log.user_id = self.api_key.user_id.clone();
        log.key_id = Some(self.api_key.id.clone());
        log.channel_id = self.channel_id.clone();
        log.model = self.model.clone();
        log.input_tokens = prompt_tokens;
        log.output_tokens = completion_tokens;
        log.cost = cost;
        log.latency_ms = self.request_start.elapsed().as_millis() as u64;
        log.status_code = 200;
        log.ip = self.client_ip.clone();
        log.request_id = Some(self.request_id.clone());
        self.state.log_store.record_request(log);

        (prompt_tokens, completion_tokens)
    }
}

/// 流式计费 Drop 守卫（B05）。
///
/// 客户端断连时 SSE 流被 hyper 中途 drop，原实现的计费逻辑位于
/// `stream::once(final_event)` 后缀事件中，断连时后缀不执行 → 流式请求零计费。
/// 守卫随流存活（见 [`GuardedStream`]）：流被 drop 时守卫一并 drop，
/// 在 Drop 中原子抢占计费权——若后缀事件尚未计费则在此兜底执行。
pub(crate) struct StreamUsageGuard {
    billing: Arc<StreamBillingState>,
}

impl StreamUsageGuard {
    pub(crate) fn new(billing: Arc<StreamBillingState>) -> Self {
        Self { billing }
    }
}

impl Drop for StreamUsageGuard {
    fn drop(&mut self) {
        // 原子抢占计费权：仅当后缀事件未执行计费时（swap 返回 false）才兜底
        if self
            .billing
            .charged
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return; // 正常路径已计费，无需重复
        }
        // Drop 中不能 await：计费本身（token 估算/扣费/日志）全为同步操作，
        // 直接执行；异步的事后限流记账交由后台任务补交。
        let (prompt_tokens, completion_tokens) = self.billing.finalize();
        // Prometheus 指标（断连兜底路径上报；正常路径由后缀事件上报）
        crate::metrics::global().record_request(
            &self.billing.model,
            self.billing.channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            self.billing.request_start.elapsed().as_millis() as u64,
        );
        crate::metrics::global().record_tokens(&self.billing.model, "prompt", prompt_tokens);
        crate::metrics::global().record_tokens(
            &self.billing.model,
            "completion",
            completion_tokens,
        );
        if let Some(bundle) = self.billing.rate_bundle.clone() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    bundle
                        .commit_tokens(prompt_tokens + completion_tokens)
                        .await;
                });
            }
        }
    }
}

/// 包装流并附加计费守卫（B05）。
///
/// 守卫作为字段随流存活：无论流被完整消费（正常结束，后缀事件已计费）
/// 还是被 hyper 中途 drop（客户端断连），守卫都会随之 drop 并触发兜底计费。
/// 守卫与后缀事件通过原子标志互斥，计费恰好执行一次。
///
/// 泛型 `G` 允许不同端点挂接各自的守卫类型（chat 的
/// StreamUsageGuard / responses 的 ResponsesStreamGuard）。
pub(crate) struct GuardedStream<S, G> {
    pub(crate) inner: std::pin::Pin<Box<S>>,
    pub(crate) _guard: G,
}

// G 仅需 Unpin：poll_next 通过 get_mut 访问 inner，要求整个结构体
// Unpin；两个守卫（StreamUsageGuard/ResponsesStreamGuard）只含 Arc，
// 天然满足。
impl<S: futures::Stream, G: std::marker::Unpin> futures::Stream for GuardedStream<S, G> {
    type Item = S::Item;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        // GuardedStream 所有字段均为 Unpin（Pin<Box<S>> 与普通结构体），可安全 get_mut
        self.get_mut().inner.as_mut().poll_next(cx)
    }
}

// ── Responses 流式计费（透传方案）───────────────────────────────────

/// Responses 流式计费共享状态（透传方案中 StreamBillingState 的对应物）。
///
/// 与 chat 的差异：透传流不能往客户端 SSE 里塞自定义后缀事件，计费
/// 全部由 [`ResponsesStreamGuard`] 在流 drop 时执行——正常结束（流被
/// hyper 完整消费后 drop）与客户端断连（流被中途 drop）都经过 Drop，
/// 天然恰好一次，无需原子标志互斥。
pub(crate) struct ResponsesBillingState {
    pub(crate) state: AppState,
    pub(crate) api_key: super::auth::ApiKey,
    pub(crate) model: String,
    pub(crate) group: String,
    /// 请求侧 input 文本快照（上游未上报 usage 时估算 prompt tokens 用）
    pub(crate) input_text: String,
    /// 上游 usage（input_tokens, output_tokens）——旁路解析
    /// `response.completed` 等终止事件的 `.response.usage` 填充
    pub(crate) usage: Arc<parking_lot::Mutex<Option<(u64, u64)>>>,
    /// 输出文本累积器（估算 completion tokens 兜底）
    pub(crate) acc: Arc<parking_lot::Mutex<String>>,
    /// 事后限流记账句柄
    pub(crate) rate_bundle: Option<crate::ratelimit::ReservationBundle>,
    pub(crate) request_start: std::time::Instant,
    pub(crate) client_ip: Option<String>,
    pub(crate) request_id: String,
    pub(crate) channel_id: Option<String>,
}

impl ResponsesBillingState {
    /// 计费三件套：usage 累计 + charge_usage + 请求日志。
    ///
    /// 上游 usage（权威值）优先；缺失的侧逐项回退 token 估算
    /// （能估就估，拿不到就 0）。返回 (prompt_tokens, completion_tokens)
    /// 供限流记账。
    pub(crate) fn finalize(&self) -> (u64, u64) {
        let (mut prompt_tokens, mut completion_tokens) = self.usage.lock().unwrap_or((0, 0));
        let output_text = self.acc.lock().clone();
        // 上游未上报 usage 的侧逐项估算
        if prompt_tokens == 0 {
            prompt_tokens = crate::token_estimate::count_text(&self.model, &self.input_text) as u64;
        }
        if completion_tokens == 0 {
            completion_tokens = crate::token_estimate::count_text(&self.model, &output_text) as u64;
        }

        // 累计用量
        self.state
            .usage_tracker
            .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

        // 扣费（try_charge + QuotaLow 通知 + key 扣减）
        let cost = charge_usage(
            &self.state,
            &self.api_key,
            &self.model,
            &self.group,
            prompt_tokens,
            completion_tokens,
        );

        // 记录请求日志（真实耗时）
        let mut log = crate::log::RequestLog::new();
        log.user_id = self.api_key.user_id.clone();
        log.key_id = Some(self.api_key.id.clone());
        log.channel_id = self.channel_id.clone();
        log.model = self.model.clone();
        log.input_tokens = prompt_tokens;
        log.output_tokens = completion_tokens;
        log.cost = cost;
        log.latency_ms = self.request_start.elapsed().as_millis() as u64;
        log.status_code = 200;
        log.ip = self.client_ip.clone();
        log.request_id = Some(self.request_id.clone());
        self.state.log_store.record_request(log);

        (prompt_tokens, completion_tokens)
    }
}

/// Responses 流式计费 Drop 守卫（透传方案）。
///
/// 随透传字节流存活（见泛型化的 [`GuardedStream`]）：流被 drop 时
/// （正常结束或客户端断连）在 Drop 中执行计费。Drop 中不能 await：
/// 计费本身（usage 提取/token 估算/扣费/日志）全为同步操作，异步的
/// 事后限流记账交由后台任务补交（同 StreamUsageGuard）。
pub(crate) struct ResponsesStreamGuard {
    pub(crate) billing: Arc<ResponsesBillingState>,
}

impl Drop for ResponsesStreamGuard {
    fn drop(&mut self) {
        let (prompt_tokens, completion_tokens) = self.billing.finalize();
        // Prometheus 指标（透传流没有后缀事件，Drop 即唯一上报点）
        crate::metrics::global().record_request(
            &self.billing.model,
            self.billing.channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            self.billing.request_start.elapsed().as_millis() as u64,
        );
        crate::metrics::global().record_tokens(&self.billing.model, "prompt", prompt_tokens);
        crate::metrics::global().record_tokens(
            &self.billing.model,
            "completion",
            completion_tokens,
        );
        if let Some(bundle) = self.billing.rate_bundle.clone() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    bundle
                        .commit_tokens(prompt_tokens + completion_tokens)
                        .await;
                });
            }
        }
    }
}

/// 将 JSON Value 的消息数组解析为 ChatMessage 列表
fn parse_messages(value: Option<&Value>) -> Option<Vec<ChatMessage>> {
    let arr = value?.as_array()?;
    let mut messages = Vec::new();
    for msg in arr {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some("system") => Role::System,
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            Some("tool") => Role::Tool,
            _ => continue,
        };
        // content 支持三种 wire 形状：string / null / typed-block 数组。
        // 数组形式时：content_blocks 原样保留供支持 blocks 的 bridge verbatim 转发
        // （vision/多模态），content 拼接所有 text 块供不支持 blocks 的 bridge 使用。
        let raw_content = msg.get("content");
        let (content, content_blocks) = match raw_content {
            Some(Value::String(s)) => (Some(s.clone()), None),
            Some(Value::Array(blocks)) => {
                let text = blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text")
                                .and_then(|t| t.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let text = if text.is_empty() { None } else { Some(text) };
                (text, Some(blocks.clone()))
            }
            _ => (None, None),
        };
        let name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .map(|s| s.to_string());
        let tool_call_id = msg
            .get("tool_call_id")
            .and_then(|t| t.as_str())
            .map(|s| s.to_string());
        let tool_calls = msg
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|t| crate::bridge::ToolCall {
                        id: t
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        function_name: t
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: t
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            });
        messages.push(ChatMessage {
            role,
            content,
            content_blocks,
            name,
            tool_call_id,
            tool_calls,
            reasoning: None,
        });
    }
    Some(messages)
}

/// 将 FinishReason 转换为 OpenAI 兼容的字符串
fn finish_reason_str(fr: &FinishReason) -> &'static str {
    match fr {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::ToolCalls => "tool_calls",
    }
}

/// 将 BridgeError 转换为 HTTP 响应
fn bridge_error_response(e: crate::bridge::BridgeError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    error_response(e.error_type(), &e.to_string(), status)
}

/// POST /v1/chat/completions - 聊天补全
///
/// 使用 Bridge/Hub 模式分发请求，参考 aisix 的 chat_completions 处理流程：
/// 1. 认证 → 2. 解析 ChatFormat → 3. Hub 分发 Bridge → 4. 调用 Bridge::chat/chat_stream
pub async fn handle_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return error_response(
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
            return error_response(
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

    // 阶段2：亲和性 session 标识——优先 body.user 字段（OpenAI SDK 常见透传），
    // 否则用 api_key 所属 user_id，保证同用户会话粘到同一渠道（保留上游 KV 缓存）
    let session_id: Option<String> = body
        .get("user")
        .and_then(|u| u.as_str())
        .map(String::from)
        .or_else(|| api_key.user_id.clone());

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover。
    // 阶段2：带亲和性选取——粘性窗口内命中渠道置顶
    let candidates = resolve_bridges_with_affinity(&state, &model, session_id.as_deref());
    if candidates.is_empty() {
        return error_response(
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

    // 解析消息列表
    let messages = parse_messages(body.get("messages")).unwrap_or_default();
    if messages.is_empty() {
        return error_response(
            "invalid_messages",
            "No valid messages",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    // 构建 ChatFormat
    let chat_req = ChatFormat {
        model: model.clone(),
        messages,
        tools: body
            .get("tools")
            .and_then(|t| t.as_array())
            .map(|arr| arr.to_vec()),
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
        tool_choice: body.get("tool_choice").cloned(),
        reasoning_effort: body
            .get("reasoning_effort")
            .and_then(|v| v.as_str())
            .map(String::from),
        web_search_options: body.get("web_search_options").cloned(),
        extra: None,
    };

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    if is_stream {
        // B06：failover 循环——依次尝试候选渠道建立流，仅对上游可重试错误切换
        let mut stream_opt = None;
        let mut used_channel_id: Option<String> = None;
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        for (bridge, cid) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            let attempt_start = std::time::Instant::now();
            match bridge.chat_stream(&chat_req, &ctx).await {
                Ok(s) => {
                    // 阶段2：流建立成功——记入健康/亲和（断路器成功、延迟 EMA、
                    // 空响应计数清零、粘性路由建立）。流中途失败由计费守卫兜底，
                    // 此处只记"建流成功"。
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_success(
                            c,
                            Some(&model),
                            attempt_start.elapsed().as_millis() as u64,
                            session_id.as_deref(),
                        );
                    }
                    stream_opt = Some(s);
                    used_channel_id = cid;
                    break;
                }
                Err(e) => {
                    // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&model),
                            ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    if !is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    if let Some(cid) = &cid {
                        state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                    }
                    tracing::warn!(
                        "chat_stream failover: channel {cid:?} failed: {e}, trying next channel"
                    );
                    last_error = Some(e);
                }
            }
        }
        let stream = match stream_opt {
            Some(s) => s,
            None => {
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for streaming request".into(),
                    )
                });
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                crate::metrics::global().record_request(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    "error",
                    latency_ms,
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
                return bridge_error_response(e).into_response();
            }
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

            let sse_stream = stream.map(move |chunk_result| match chunk_result {
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
                                        *tool_calls_for_map
                                            .lock()
                                            .entry(name.clone())
                                            .or_insert(0) += 1;
                                    }
                                }
                            }
                        }
                        drop(buf);
                        let mut delta = serde_json::json!({
                            "content": chunk.delta.content.unwrap_or_default(),
                        });
                        // 流式推理内容增量透传给 OpenAI 客户端（OpenAI 协议字段）
                        if let Some(reasoning) = &chunk.delta.reasoning {
                            delta["reasoning_content"] = Value::String(reasoning.clone());
                        }
                        if let Some(tool_calls) = &chunk.delta.tool_calls {
                            // 按 index 聚合流式参数分片，并对完整 arguments 做三层自修复
                            // （同一次 chunk 内的多个 index 并行聚合，修复彼此独立）
                            let repaired =
                                tool_repair::accumulate_tool_call_arguments(tool_calls);
                            delta["tool_calls"] = Value::Array(
                                repaired
                                    .iter()
                                    .map(|(idx, id, name, args)| {
                                        serde_json::json!({
                                            "index": idx,
                                            "id": id,
                                            "type": "function",
                                            "function": {
                                                "name": name,
                                                "arguments": args,
                                            }
                                        })
                                    })
                                    .collect(),
                            );
                        }
                        let sse_data = serde_json::json!({
                            "id": chunk.id,
                            "object": "chat.completion.chunk",
                            "created": chrono::Utc::now().timestamp(),
                            "model": chunk.model,
                            "choices": [{
                                "index": 0,
                                "delta": delta,
                                "finish_reason": chunk.finish_reason.as_ref().map(|fr| finish_reason_str(fr)),
                            }]
                        })
                        .to_string();
                        Ok::<_, Infallible>(Event::default().data(sse_data))
                    }
                    Err(e) => {
                        tracing::error!("Stream chunk error: {e}");
                        // 标准 SSE error 事件格式（与 OpenAI API 错误结构一致）：
                        //   event: error
                        //   data: {"error":{"message":"...","type":"...","code":"..."}}
                        let err_data = serde_json::json!({
                            "error": {
                                "message": e.to_string(),
                                "type": e.error_type(),
                                "code": e.error_type(),
                            }
                        })
                        .to_string();
                        Ok(Event::default().event("error").data(err_data))
                    }
                });

            // 流结束：估算 token、扣费、commit_tokens、记录日志（问题 3）
            // B05：计费状态由后缀事件与 Drop 守卫共享（原子标志互斥）——
            // 正常结束时后缀事件计费；客户端断连导致流被 drop 时由守卫兜底。
            let billing = Arc::new(StreamBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: model.clone(),
                group: billing_group.clone(),
                chat_req: chat_req.clone(),
                tool_calls: tool_calls.clone(),
                acc: acc.clone(),
                charged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
            });
            let billing_fin = billing.clone();
            let final_event = async move {
                // 原子抢占计费权：断连场景守卫可能已兜底计费，双保险防重复
                let (prompt_tokens, completion_tokens) = if !billing_fin
                    .charged
                    .swap(true, std::sync::atomic::Ordering::SeqCst)
                {
                    let (pt, ct) = billing_fin.finalize();
                    // 事后限流记账（TPM）
                    rate_bundle.commit_tokens(pt + ct).await;
                    (pt, ct)
                } else {
                    // 守卫已兜底计费：这里只取估算值供指标上报，不再重复扣费
                    let output_text = billing_fin.acc.lock().clone();
                    (
                        crate::token_estimate::count_chat_prompt(
                            &billing_fin.model,
                            &billing_fin.chat_req,
                        ) as u64,
                        crate::token_estimate::count_text(&billing_fin.model, &output_text) as u64,
                    )
                };

                // Prometheus 指标
                crate::metrics::global().record_request(
                    &billing_fin.model,
                    billing_fin.channel_id.as_deref().unwrap_or("unknown"),
                    "ok",
                    billing_fin.request_start.elapsed().as_millis() as u64,
                );
                crate::metrics::global().record_tokens(&billing_fin.model, "prompt", prompt_tokens);
                crate::metrics::global().record_tokens(
                    &billing_fin.model,
                    "completion",
                    completion_tokens,
                );

                Ok::<_, Infallible>(Event::default().data("[DONE]"))
            };
            let combined = sse_stream.chain(futures::stream::once(final_event));

            // B05：包装守卫流——流被 drop（含客户端断连）时兜底计费
            let guarded = GuardedStream {
                inner: Box::pin(combined),
                _guard: StreamUsageGuard::new(billing),
            };

            Sse::new(guarded)
                .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15)))
                .into_response()
        }
    } else {
        // 响应缓存：非流式请求尝试缓存命中
        let cache_key = serde_json::to_string(&serde_json::json!({
            "model": &model,
            "messages": body.get("messages").cloned().unwrap_or(Value::Null),
            "temperature": body.get("temperature").cloned().unwrap_or(Value::Null),
            "max_tokens": body.get("max_tokens").cloned().unwrap_or(Value::Null),
        }))
        .unwrap_or_default();
        if let Some(cached) = state.response_cache.get(&cache_key).await {
            tracing::debug!("response cache hit for model {}", model);
            // 计费修复：缓存命中不再免放行——按命中 0 token 记账并写请求日志，
            // 保持用量可观测（usage 累计 + 日志留痕可审计），费用为 0
            // （缓存命中不重复扣费，但请求必须留痕）。
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
            state
                .usage_tracker
                .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = Some("cache".to_string());
            log.model = model.clone();
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = 0;
            log.latency_ms = request_start.elapsed().as_millis() as u64;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            log.error_msg = Some("cache_hit".to_string());
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
        let mut last_error: Option<crate::bridge::BridgeError> = None;
        for (bridge, cid) in candidates {
            if let Some(c) = &cid {
                state.channel_store.mark_used(c);
            }
            let attempt_start = std::time::Instant::now();
            match bridge.chat(&chat_req, &ctx).await {
                Ok(resp) => {
                    // 阶段2：成功——记入断路器/健康追踪/亲和性/空响应计数
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_success(
                            c,
                            Some(&model),
                            attempt_start.elapsed().as_millis() as u64,
                            session_id.as_deref(),
                        );
                    }
                    response_opt = Some(resp);
                    used_channel_id = cid;
                    break;
                }
                Err(e) => {
                    // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                    if let Some(c) = &cid {
                        state.channel_store.record_channel_failure(
                            c,
                            Some(&model),
                            ChannelStore::classify_bridge_error(&e),
                            &e.to_string(),
                            session_id.as_deref(),
                        );
                    }
                    if !is_retryable_bridge_error(&e) {
                        // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                        last_error = Some(e);
                        used_channel_id = cid;
                        break;
                    }
                    if let Some(cid) = &cid {
                        state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                    }
                    tracing::warn!(
                        "chat failover: channel {cid:?} failed: {e}, trying next channel"
                    );
                    last_error = Some(e);
                }
            }
        }
        let response = match response_opt {
            Some(r) => r,
            None => {
                let e = last_error.unwrap_or_else(|| {
                    crate::bridge::BridgeError::AllAccountsFailed(
                        "all channels failed for non-streaming request".into(),
                    )
                });
                let latency_ms = request_start.elapsed().as_millis() as u64;
                let status_code = StatusCode::from_u16(e.http_status())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                let mut log = crate::log::RequestLog::new();
                log.user_id = api_key.user_id.clone();
                log.key_id = Some(api_key.id.clone());
                log.channel_id = used_channel_id.clone();
                log.model = model.clone();
                log.latency_ms = latency_ms;
                log.status_code = status_code.as_u16();
                log.error_msg = Some(e.to_string());
                log.ip = client_ip.clone();
                log.request_id = Some(request_id.clone());
                state.log_store.record_request(log);
                rate_bundle.commit_tokens(0).await;
                crate::metrics::global().record_request(
                    &model,
                    used_channel_id.as_deref().unwrap_or("unknown"),
                    "error",
                    latency_ms,
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
                return bridge_error_response(e).into_response();
            }
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

            // 计费扣减（M10：复用 charge_usage，消除与非流式分支的重复实现）。
            // 逻辑等价：calculate_cost_quoted → try_charge（用户余额不足跳过 key 扣费）→
            // QuotaLow 通知 → charge_quota。返回 cost 供日志记录。
            let cost = charge_usage(
                &state,
                &api_key,
                &model,
                &billing_group,
                response.usage.prompt_tokens,
                response.usage.completion_tokens,
            );

            // 事后限流记账（TPM）
            let total_tokens = response.usage.prompt_tokens + response.usage.completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志（功能 1）
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.model = model.clone();
            log.input_tokens = response.usage.prompt_tokens;
            log.output_tokens = response.usage.completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(
                &model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "ok",
                latency_ms,
            );
            crate::metrics::global().record_tokens(&model, "prompt", response.usage.prompt_tokens);
            crate::metrics::global().record_tokens(
                &model,
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

/// POST /v1/responses - OpenAI Responses API 透传
///
/// 与 handle_chat_completions 不同，本端点不做协议转换（不构建
/// ChatFormat / 不走 Bridge::chat），而是把请求 body 原样转发给上游
/// 的 /v1/responses 端点（参考 aisix 的 responses_to_target 透传方案）：
/// 1. 认证 → 2. 限流 → 3. 分组权限 → 4. 定价校验 → 5. resolve_bridges
/// 6. failover 循环调用 Bridge::responses_passthrough
/// 7. 计费从 Responses 格式 usage（input_tokens/output_tokens）提取，
///    流式缺 usage 时回退 token 估算
pub async fn handle_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let request_start = std::time::Instant::now();
    let request_id = format!("req-{}", uuid::Uuid::new_v4());
    let client_ip = extract_client_ip(&headers);

    let model = match body.get("model").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            return error_response(
                "invalid_model",
                "Missing model field",
                StatusCode::BAD_REQUEST,
            )
            .into_response()
        }
    };

    // 完整鉴权：校验状态/过期/模型白名单/额度/IP
    let api_key = match verify_api_key_full(&state, &headers, &model) {
        Ok(k) => k,
        Err(e) => return e.into_response(),
    };

    // 限流检查：鉴权后、推理前（Responses 端点同样纳入限流）
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
            return error_response(
                "rate_limit_exceeded",
                &format!("Rate limit exceeded. Retry after {} seconds.", retry_after),
                StatusCode::TOO_MANY_REQUESTS,
            )
            .into_response();
        }
    };

    // 校验用户分组模型权限并解析计费分组
    let billing_group = match check_group_model_permission(&state, &api_key, &model) {
        Ok(g) => g,
        Err(e) => return e.into_response(),
    };

    // B09：前置校验模型定价——未配置价格的模型拒绝请求，避免免费用量
    if let Err(e) = ensure_model_priced(&state, &model) {
        return e.into_response();
    }

    // 阶段2：亲和性 session 标识（Responses 透传：user_id 兜底）
    let session_id: Option<String> = api_key.user_id.clone();

    // B06：获取候选渠道列表（priority/weight 排序），失败时逐个 failover。
    // 阶段2：带亲和性选取——粘性窗口内命中渠道置顶
    let candidates = resolve_bridges_with_affinity(&state, &model, session_id.as_deref());
    if candidates.is_empty() {
        return error_response(
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

    // Responses 协议以 input 字段承载输入（字符串或 item 数组均可），
    // 不使用 chat 的 messages 字段；缺失即 400
    if !matches!(
        body.get("input"),
        Some(Value::String(_)) | Some(Value::Array(_))
    ) {
        return error_response(
            "invalid_input",
            "input field is required",
            StatusCode::BAD_REQUEST,
        )
        .into_response();
    }

    let ctx = BridgeContext::new(request_id.clone(), model.clone());

    // B06：failover 循环——依次尝试候选渠道透传，仅对上游可重试错误切换
    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid) in candidates {
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        let attempt_start = std::time::Instant::now();
        match bridge.responses_passthrough(&body, is_stream, &ctx).await {
            Ok(r) => {
                // 阶段2：成功——记入断路器/健康追踪/亲和性
                if let Some(c) = &cid {
                    state.channel_store.record_channel_success(
                        c,
                        Some(&model),
                        attempt_start.elapsed().as_millis() as u64,
                        session_id.as_deref(),
                    );
                }
                result_opt = Some(r);
                used_channel_id = cid;
                break;
            }
            Err(e) => {
                // 阶段2：失败分类记入断路器/健康追踪/亲和清除
                if let Some(c) = &cid {
                    state.channel_store.record_channel_failure(
                        c,
                        Some(&model),
                        ChannelStore::classify_bridge_error(&e),
                        &e.to_string(),
                        session_id.as_deref(),
                    );
                }
                if !is_retryable_bridge_error(&e) {
                    // 4xx 客户端错误与请求本身相关，换渠道大概率同样失败，直接返回
                    last_error = Some(e);
                    used_channel_id = cid;
                    break;
                }
                if let Some(cid) = &cid {
                    state.channel_store.mark_cooldown(cid, e.to_string(), 60);
                }
                tracing::warn!(
                    "responses failover: channel {cid:?} failed: {e}, trying next channel"
                );
                last_error = Some(e);
            }
        }
    }
    let passthrough = match result_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for responses request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.model = model.clone();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            state.log_store.record_request(log);
            rate_bundle.commit_tokens(0).await;
            crate::metrics::global().record_request(
                &model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "error",
                latency_ms,
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
            return bridge_error_response(e).into_response();
        }
    };

    match passthrough {
        ResponsesPassthrough::Json(json) => {
            // 非流式：上游 JSON body 原样转回客户端，
            // usage 为 Responses 格式（input_tokens/output_tokens）
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let usage = json.get("usage");
            let mut prompt_tokens = usage
                .and_then(|u| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let mut completion_tokens = usage
                .and_then(|u| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);

            // 上游未携带 usage 时用 token 估算兜底（能估就估，拿不到就 0）
            if prompt_tokens == 0 && completion_tokens == 0 {
                let input_text = responses_input_text(&body);
                let output_text = responses_output_text(&json);
                prompt_tokens = crate::token_estimate::count_text(&model, &input_text) as u64;
                completion_tokens = crate::token_estimate::count_text(&model, &output_text) as u64;
            }

            // 记录用量
            state
                .usage_tracker
                .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

            // 计费扣减（用户 quota + key used_quota）
            let cost = charge_usage(
                &state,
                &api_key,
                &model,
                &billing_group,
                prompt_tokens,
                completion_tokens,
            );

            // 事后限流记账（TPM）
            let total_tokens = prompt_tokens + completion_tokens;
            rate_bundle.commit_tokens(total_tokens).await;

            // 记录请求日志
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.model = model.clone();
            log.input_tokens = prompt_tokens;
            log.output_tokens = completion_tokens;
            log.cost = cost;
            log.latency_ms = latency_ms;
            log.status_code = 200;
            log.ip = client_ip.clone();
            log.request_id = Some(request_id.clone());
            state.log_store.record_request(log);

            // Prometheus 指标
            crate::metrics::global().record_request(
                &model,
                used_channel_id.as_deref().unwrap_or("unknown"),
                "ok",
                latency_ms,
            );
            crate::metrics::global().record_tokens(&model, "prompt", prompt_tokens);
            crate::metrics::global().record_tokens(&model, "completion", completion_tokens);

            Json(json).into_response()
        }
        ResponsesPassthrough::Stream(byte_stream) => {
            // 流式：上游 SSE 字节流原样透传给客户端，旁路解析 SSE
            // 提取 usage / 累积输出文本，流 drop 时由守卫计费
            let billing = Arc::new(ResponsesBillingState {
                state: state.clone(),
                api_key: api_key.clone(),
                model: model.clone(),
                group: billing_group.clone(),
                input_text: responses_input_text(&body),
                usage: Arc::new(parking_lot::Mutex::new(None)),
                acc: Arc::new(parking_lot::Mutex::new(String::new())),
                rate_bundle: Some(rate_bundle.clone()),
                request_start,
                client_ip: client_ip.clone(),
                request_id: request_id.clone(),
                channel_id: used_channel_id.clone(),
            });

            // 旁路解析：每个 chunk 原样转发（yield 不变），同时喂给
            // SseDecoder 解析终止事件 usage 与输出文本增量
            let billing_for_map = billing.clone();
            let mut sse_decoder = crate::sse::SseDecoder::new();
            let passthrough_stream = byte_stream.map(move |chunk| {
                if let Ok(bytes) = &chunk {
                    for ev in sse_decoder.feed(bytes.as_ref()) {
                        if let crate::sse::SseEvent::Data(json_str) = ev {
                            if let Ok(json) = serde_json::from_str::<Value>(&json_str) {
                                observe_responses_sse_event(&json, &billing_for_map);
                            }
                        }
                    }
                }
                chunk
            });

            let keepalive_stream = crate::sse::KeepaliveStream::new(
                passthrough_stream,
                std::time::Duration::from_secs(15),
            );
            let guarded = GuardedStream {
                inner: Box::pin(keepalive_stream),
                _guard: ResponsesStreamGuard { billing },
            };

            let mut response = Response::new(axum::body::Body::from_stream(guarded));
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("text/event-stream"),
            );
            response
        }
    }
}

/// 提取 Responses 请求体的 input 文本（token 估算兜底用）。
///
/// input 为字符串，或 item 数组（item 为裸字符串或对象；对象的
/// content / output / reason 槽位可为字符串或 typed parts 数组）；
/// 顶层 instructions 一并计入。参考 aisix 的 responses_input_to_chat
/// / responses_item_text。
fn responses_input_text(body: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(instructions) = body.get("instructions").and_then(|v| v.as_str()) {
        if !instructions.is_empty() {
            parts.push(instructions.to_string());
        }
    }
    match body.get("input") {
        Some(Value::String(s)) => {
            if !s.is_empty() {
                parts.push(s.clone());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                // 裸字符串元素视为用户文本
                if let Some(s) = item.as_str() {
                    if !s.is_empty() {
                        parts.push(s.to_string());
                    }
                    continue;
                }
                // 对象元素：收集 content / output / reason 槽位的文本
                for key in ["content", "output", "reason"] {
                    if let Some(v) = item.get(key) {
                        let text = responses_value_text(v);
                        if !text.is_empty() {
                            parts.push(text);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    parts.join("\n")
}

/// 提取 Responses 内容槽位的纯文本：裸字符串或 typed parts
/// 数组（取各 part 的 text 字段，忽略图片等非文本 part）。
fn responses_value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// 提取 Responses 非流式响应的输出文本（token 估算兜底用）。
///
/// 遍历 output 数组：message item 的 content parts 的 text，以及
/// 工具调用 item 顶层的 name / arguments / input。参考 aisix 的
/// responses_output_text。
fn responses_output_text(resp: &Value) -> String {
    let Some(items) = resp.get("output").and_then(|v| v.as_array()) else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for it in items {
        if let Some(content) = it.get("content").and_then(|c| c.as_array()) {
            for p in content {
                if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
        }
        // 工具调用 item 的输出在顶层 name / arguments / input 字段
        for key in ["name", "arguments", "input"] {
            if let Some(s) = it.get(key).and_then(|v| v.as_str()) {
                if !s.is_empty() {
                    parts.push(s.to_string());
                }
            }
        }
    }
    parts.join("\n")
}

/// 从一个 Responses SSE 事件中旁路提取计费信息（不影响原样转发）。
///
/// - `response.completed` / `response.incomplete` / `response.failed`
///   终止事件：从 `.response.usage` 提取权威 usage（参考 aisix 的
///   parse_responses_terminal_usage，三者在 `max_output_tokens` 截断
///   / 失败场景下同样携带完整 usage）
/// - `response.output_text.delta`：累积输出文本增量（估算兜底用）
fn observe_responses_sse_event(json: &Value, billing: &ResponsesBillingState) {
    match json.get("type").and_then(|t| t.as_str()) {
        Some("response.completed" | "response.incomplete" | "response.failed") => {
            if let Some(usage) = json.get("response").and_then(|r| r.get("usage")) {
                let input_tokens = usage
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                *billing.usage.lock() = Some((input_tokens, output_tokens));
            }
        }
        Some("response.output_text.delta") => {
            if let Some(delta) = json.get("delta").and_then(|d| d.as_str()) {
                let mut buf = billing.acc.lock();
                crate::token_estimate::push_capped(&mut buf, delta);
            }
        }
        _ => {}
    }
}

/// POST /v1/completions - 文本补全
///
/// 通过 Bridge::complete 委托给 Bridge 处理
pub async fn handle_completions(
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

    // 完整鉴权（问题 1）
    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();

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

    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());

    // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid) in candidates {
        // 标记渠道已使用（问题 4）
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        match bridge.complete(&body, &ctx).await {
            Ok(result) => {
                result_opt = Some(result);
                used_channel_id = cid;
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
                tracing::warn!(
                    "completions failover: channel {cid:?} failed: {e}, trying next channel"
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
                    "all channels failed for completions request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.model = model_owned.clone();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
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
    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        let prompt_tokens = result
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let completion_tokens = result
            .get("usage")
            .and_then(|u| u.get("completion_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        state
            .usage_tracker
            .accumulate(prompt_tokens, completion_tokens, 0, 0, 0, 0.0);

        // 计费扣减（问题 2/5/6）
        let cost = charge_usage(
            &state,
            &api_key,
            &model_owned,
            &billing_group,
            prompt_tokens,
            completion_tokens,
        );

        // 事后限流记账（TPM）
        let total_tokens = prompt_tokens + completion_tokens;
        rate_bundle.commit_tokens(total_tokens).await;

        // 记录请求日志（功能 1）
        let mut log = crate::log::RequestLog::new();
        log.user_id = api_key.user_id.clone();
        log.key_id = Some(api_key.id.clone());
        log.channel_id = used_channel_id.clone();
        log.model = model_owned.clone();
        log.input_tokens = prompt_tokens;
        log.output_tokens = completion_tokens;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        state.log_store.record_request(log);

        // Prometheus 指标
        crate::metrics::global().record_request(
            &model_owned,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );
        crate::metrics::global().record_tokens(&model_owned, "prompt", prompt_tokens);
        crate::metrics::global().record_tokens(&model_owned, "completion", completion_tokens);

        Ok(Json(result))
    }
}

/// POST /v1/embeddings - 向量嵌入
///
/// 通过 Bridge::embed 委托给 Bridge 处理
pub async fn handle_embeddings(
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

    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();

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

    let input = body.get("input");
    let texts: Vec<String> = match input {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        _ => {
            return Err(error_response(
                "invalid_input",
                "Invalid input field",
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    let embed_req = EmbeddingRequest {
        model: model_owned.clone(),
        input: texts,
    };
    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());

    // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
    let mut response_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid) in candidates {
        // 标记渠道已使用（问题 4）
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        match bridge.embed(&embed_req, &ctx).await {
            Ok(resp) => {
                response_opt = Some(resp);
                used_channel_id = cid;
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
                tracing::warn!(
                    "embeddings failover: channel {cid:?} failed: {e}, trying next channel"
                );
                last_error = Some(e);
            }
        }
    }
    let response = match response_opt {
        Some(r) => r,
        None => {
            let e = last_error.unwrap_or_else(|| {
                crate::bridge::BridgeError::AllAccountsFailed(
                    "all channels failed for embeddings request".into(),
                )
            });
            let latency_ms = request_start.elapsed().as_millis() as u64;
            let status_code =
                StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut log = crate::log::RequestLog::new();
            log.user_id = api_key.user_id.clone();
            log.key_id = Some(api_key.id.clone());
            log.channel_id = used_channel_id.clone();
            log.model = model_owned.clone();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
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
    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        let data: Vec<Value> = response
            .data
            .iter()
            .map(|obj| {
                serde_json::json!({
                    "object": "embedding",
                    "index": obj.index,
                    "embedding": obj.embedding,
                })
            })
            .collect();

        let prompt_tokens = response.usage.prompt_tokens as u64;
        state
            .usage_tracker
            .accumulate(prompt_tokens, 0, 0, 0, 0, 0.0);

        // 计费扣减（问题 2/5/6）
        let cost = charge_usage(
            &state,
            &api_key,
            &model_owned,
            &billing_group,
            prompt_tokens,
            0,
        );

        // 事后限流记账（TPM）
        rate_bundle.commit_tokens(prompt_tokens).await;

        // 记录请求日志（功能 1）
        let mut log = crate::log::RequestLog::new();
        log.user_id = api_key.user_id.clone();
        log.key_id = Some(api_key.id.clone());
        log.channel_id = used_channel_id.clone();
        log.model = model_owned.clone();
        log.input_tokens = prompt_tokens;
        log.output_tokens = 0;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        state.log_store.record_request(log);

        // Prometheus 指标
        crate::metrics::global().record_request(
            &model_owned,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );
        crate::metrics::global().record_tokens(&model_owned, "prompt", prompt_tokens);

        Ok(Json(serde_json::json!({
            "object": "list",
            "data": data,
            "model": &model_owned,
            "usage": {
                "prompt_tokens": response.usage.prompt_tokens,
                "total_tokens": response.usage.total_tokens,
            }
        })))
    }
}

/// POST /v1/images/generations - 图片生成
///
/// 通过 Bridge::generate_image 委托给 Bridge 处理
pub async fn handle_images_generations(
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

    let api_key = verify_api_key_full(&state, &headers, model)?;
    let model_owned = model.to_string();

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

    let ctx = BridgeContext::new(request_id.clone(), model_owned.clone());

    // B06：failover 循环——依次尝试候选渠道，仅对上游可重试错误切换
    let mut result_opt = None;
    let mut used_channel_id: Option<String> = None;
    let mut last_error: Option<crate::bridge::BridgeError> = None;
    for (bridge, cid) in candidates {
        // 标记渠道已使用（问题 4）
        if let Some(c) = &cid {
            state.channel_store.mark_used(c);
        }
        match bridge.generate_image(&body, &ctx).await {
            Ok(result) => {
                result_opt = Some(result);
                used_channel_id = cid;
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
            log.model = model_owned.clone();
            log.latency_ms = latency_ms;
            log.status_code = status_code.as_u16();
            log.error_msg = Some(e.to_string());
            log.ip = client_ip.clone();
            log.request_id = Some(request_id);
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
    {
        let latency_ms = request_start.elapsed().as_millis() as u64;
        state.usage_tracker.accumulate(0, 0, 0, 0, 0, 0.0);

        // 计费扣减（按次计价，问题 2/5/6）
        let cost = charge_usage(&state, &api_key, &model_owned, &billing_group, 0, 0);

        // 事后限流记账（按次计价，记 1 个请求 token 占位以维持 RPM 一致性）
        rate_bundle.commit_tokens(0).await;

        // 记录请求日志（功能 1）— 图片生成按次计价，tokens 设为 0
        let mut log = crate::log::RequestLog::new();
        log.user_id = api_key.user_id.clone();
        log.key_id = Some(api_key.id.clone());
        log.channel_id = used_channel_id.clone();
        log.model = model_owned.clone();
        log.input_tokens = 0;
        log.output_tokens = 0;
        log.cost = cost;
        log.latency_ms = latency_ms;
        log.status_code = 200;
        log.ip = client_ip.clone();
        log.request_id = Some(request_id);
        state.log_store.record_request(log);

        // Prometheus 指标（图片按次计价，tokens 记 0）
        crate::metrics::global().record_request(
            &model_owned,
            used_channel_id.as_deref().unwrap_or("unknown"),
            "ok",
            latency_ms,
        );

        Ok(Json(result))
    }
}

/// POST /v1/audio/transcriptions - 语音转文字
///
/// 注：Bridge 暂未定义音频方法，仍直接使用 CfApiClient
pub async fn handle_audio_transcriptions(
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

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
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
            let cost = charge_usage(&state, &api_key, &model, &billing_group, 0, 0);

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
pub async fn handle_audio_translations(
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

    let bytes = match axum::body::to_bytes(body, 25 * 1024 * 1024).await {
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
            let cost = charge_usage(&state, &api_key, &model, &billing_group, 0, 0);

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
pub async fn handle_audio_speech(
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
            let cost = charge_usage(&state, &api_key, &model_owned, &billing_group, 0, 0);

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

/// GET /v1/models - 模型列表
pub async fn handle_list_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    let now = chrono::Utc::now().timestamp();

    let model_list: Vec<Value> = mappings
        .into_iter()
        .map(|(name, cf_model)| {
            let owned_by = crate::proxy::get_model_owned_by(&cf_model);
            serde_json::json!({
                "id": name,
                "object": "model",
                "created": now,
                "owned_by": owned_by
            })
        })
        .collect();

    Ok(Json(serde_json::json!({
        "object": "list",
        "data": model_list
    })))
}

/// GET /v1/models/{model} - 模型详情
pub async fn handle_get_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _key_id = verify_api_key(&state, &headers)?;

    let mappings = state.model_mapper.all_mappings();
    if mappings.contains_key(&model) {
        let now = chrono::Utc::now().timestamp();
        let cf_model = state.model_mapper.resolve(&model);
        let owned_by = crate::proxy::get_model_owned_by(&cf_model);
        Ok(Json(serde_json::json!({
            "id": model,
            "object": "model",
            "created": now,
            "owned_by": owned_by,
            "permission": []
        })))
    } else {
        Err(error_response(
            "model_not_found",
            &format!("Model '{model}' not found"),
            StatusCode::NOT_FOUND,
        ))
    }
}

/// 在字节切片中查找子序列的起始位置（从 `from` 开始）。
///
/// 用于 multipart 字节级解析（B04）：`str::find` 无法处理二进制 body，
/// 需要等价的字节序列搜索。
fn find_sub_slice(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
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
fn parse_multipart_audio(
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
