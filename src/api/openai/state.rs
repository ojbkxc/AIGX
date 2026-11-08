use crate::account::AccountPool;
use crate::api::auth::ApiKeyStore;
use crate::channel::ChannelStore;
use crate::config::ConfigManager;
use crate::health::{HealthTracker, LivezState};
use crate::hub::Hub;
use crate::ip::IpFilterStore;
use crate::log::LogStore;
use crate::model::ModelMapper;
use crate::notify::NotifyService;
use crate::payment::order_store::OrderStore;
use crate::plan::PlanStore;
use crate::pricing::exchange_rate::ExchangeRateService;
use crate::pricing::price_sync::PriceSyncService;
use crate::pricing::PricingStore;
use crate::proxy::CfApiClient;
use crate::ratelimit::RateLimiter;
use crate::redemption::RedemptionStore;
use crate::usage::UsageTracker;
use crate::user::UserStore;
use crate::user_group::UserGroupStore;
use axum::{http::StatusCode, Json};
use serde_json::Value;
use std::sync::Arc;

// SeaORM 数据库连接（仅当启用 sea-orm feature 时可用）
#[cfg(feature = "sea-orm")]
use sea_orm::DatabaseConnection;

/// 共享应用状态
#[derive(Clone)]
pub struct AppState {
    pub api_client: Arc<CfApiClient>,
    pub model_mapper: Arc<ModelMapper>,
    /// 模型元信息注册表（P1：owned_by/上下文长度/能力）
    pub model_metadata: Arc<crate::model::metadata::ModelMetadataRegistry>,
    /// 统一模型管理的启停规则（粒度 provider × model）
    pub model_rules: Arc<crate::model::manage::ModelRules>,
    pub usage_tracker: Arc<UsageTracker>,
    pub account_pool: Arc<AccountPool>,
    pub api_key_store: Arc<ApiKeyStore>,
    pub config_manager: Arc<ConfigManager>,
    pub hub: Arc<Hub>,
    pub user_store: Arc<UserStore>,
    pub order_store: Arc<OrderStore>,
    /// 签到存储（每日签到送配额，对齐 new-api checkin）
    pub checkin_store: Arc<crate::user::checkin::CheckinStore>,
    /// 签到中心存储（替**上游账号**每日领额度，与上面的用户签到是两套）
    pub upstream_checkin: Arc<crate::checkin::CheckinStore>,
    pub epay_client: Arc<crate::payment::EpayClient>,
    /// Stripe 支付客户端
    pub stripe_client: Arc<crate::payment::stripe::StripeClient>,
    pub health_tracker: Arc<HealthTracker>,
    pub livez_state: Arc<LivezState>,
    /// 通用渠道存储（混用 CF + 第三方 OpenAI 兼容上游）
    pub channel_store: Arc<ChannelStore>,
    /// 路由策略（对照 Agent2API「设置 → 路由」：策略/粘性/断路器/调度权重）
    pub routing_policy: Arc<crate::channel::routing_policy::RoutingPolicyStore>,
    /// 终端用户工具权限策略（#34 P0：组级 allowlist + 全局开关，默认全关）
    pub user_tool_policy: Arc<crate::agent::user_tool_policy::UserToolPolicyStore>,
    /// 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）
    pub gateway_prompt: Arc<crate::prompt::GatewayPromptStore>,
    /// 账号型渠道的账号池（渠道内第二级调度：账号级优先级/冷却/并发/粘性）
    pub account_sessions: crate::account_session::AccountSessionStore,
    /// 模型定价目录
    pub pricing_store: Arc<PricingStore>,
    /// 用户分组存储
    pub user_group_store: Arc<UserGroupStore>,
    /// 日志与审计存储
    pub log_store: Arc<LogStore>,
    /// 在途请求登记表（进程内）：管理员可查看并终止正在进行的请求
    pub inflight: Arc<crate::log::inflight::InflightRegistry>,
    /// 兑换码存储
    pub redemption_store: Arc<RedemptionStore>,
    /// 套餐模板存储（按量套餐：模板 → 发 key 交付）
    pub plan_store: Arc<PlanStore>,
    /// 用户订阅存储（时长订阅：余额购买 → 独立配额池 + 分组升降级）
    pub subscription_store: Arc<crate::plan::subscription::SubscriptionStore>,
    /// 工单存储（用户提交问题 / 管理员回复）
    pub ticket_store: Arc<crate::ticket::TicketStore>,
    /// 聊天会话存储（按用户隔离的对话记录；Agent 会话另在 agent_state 里）
    pub chat_sessions: crate::chat_session::ChatSessionStore,
    /// 限流器（多维度 RPM/TPM）
    pub rate_limiter: Arc<RateLimiter>,
    /// 通知服务（Telegram + SMTP + Slack + Webhook）
    pub notify_service: Arc<NotifyService>,
    /// 告警规则评估器（alert_patrol 巡检 + 管理 API 共享）。
    /// parking_lot::Mutex：无中毒语义，持锁 panic 不会永久破坏告警端点。
    pub alert_evaluator:
        std::sync::Arc<parking_lot::Mutex<crate::notify::alert::AlertRuleEvaluator>>,
    /// 底层 FileStore（告警规则/历史持久化等轻量 KV 用）
    pub alert_store: Arc<crate::storage::FileStore>,
    /// 全局 IP 白名单/黑名单过滤（批次3 IP 管理）
    pub ip_filter: Arc<IpFilterStore>,
    /// 价格同步服务（批次3 多源定价同步；tokio Mutex 以便跨 await 持锁）
    pub price_sync: Arc<tokio::sync::Mutex<PriceSyncService>>,
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
    /// 邮箱验证码登录（登录方式之一，对齐 v2board/new-api）。
    ///
    /// key=邮箱（小写），value=6 位验证码。TTL=5 分钟；验证成功即移除。
    pub login_code_cache: Arc<crate::cache::AsyncCache<String, String>>,
    /// 2FA/TOTP 待验证缓存（P1）。
    ///
    /// 密码验证成功但用户开启 TOTP 时，不下发 session，而是生成
    /// 一次性 tmp_token 存入本缓存（key=tmp_token，value=email），
    /// 客户端携 TOTP 码调 /api/auth/login/totp 完成二次验证。
    /// TTL=5 分钟，超时未验证即失效。
    pub totp_pending_cache: Arc<crate::cache::AsyncCache<String, String>>,
    /// 2FA/TOTP 启用暂存缓存（P1）。
    ///
    /// setup 接口生成的新 secret 暂存于此（key=user.id，value=base32 secret），
    /// enable 接口一次性消费后落库。TTL=5 分钟，防止半途丢弃的 secret 残留。
    pub totp_setup_cache: Arc<crate::cache::AsyncCache<String, String>>,
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
    /// OAuth state 参数缓存（CSRF 防护）。
    ///
    /// authorize 时生成随机 state 存入，callback 时校验并一次性消费。
    /// key=state 值，value=生成时间戳（TTL 10 分钟由 AsyncCache 清扫）。
    pub oauth_state_cache: Arc<crate::cache::AsyncCache<String, i64>>,
    /// OAuth **绑定** state → 目标用户 email（个人中心发起绑定时写入）。
    ///
    /// 与登录态的 `oauth_state_cache` 分开：绑定由已登录用户发起，callback 需
    /// 知道把第三方身份绑到「谁」。state 是不透明 UUID，一次性消费（TTL 10 分钟）。
    pub oauth_bind_state: Arc<crate::cache::AsyncCache<String, String>>,
    /// OAuth 登录成功后的**一次性票据** ticket → 会话 token。
    ///
    /// callback 是浏览器整页跳转，会话 token 若直接放 URL（哪怕 fragment）会留在
    /// 浏览器历史里。改为：callback 生成短命 ticket（TTL 2 分钟）放 fragment，SPA
    /// 再用 ticket 调 `/api/auth/oauth/consume` 换回真正的 token，token 不进 URL。
    pub oauth_ticket: Arc<crate::cache::AsyncCache<String, String>>,
    /// 会话注册表（P1：会话撤销）。
    ///
    /// 无状态 HMAC token 的服务端撤销层：登录注册 jti、验证查
    /// revocation、logout/改密/管理端踢人时撤销。重启后注册表为空
    ///（未撤销 token 仍有效，撤销中的会话恢复——见 SessionRegistry 注释）。
    pub session_registry: Arc<crate::api::auth::SessionRegistry>,
    /// AI 运维 Agent 运行时（`[agent]` 配置启用后 Some；否则 None）。
    ///
    /// 自举式运维工作台：Agent 用 AIGX 自己的渠道推理去运维 AIGX。
    /// 会话持久化复用同一个 FileStore（PG kv），审批 pending 表挂
    /// [`crate::agent::approval::AgentApprovals`]。
    pub agent_state: Option<Arc<crate::agent::AgentState>>,
    /// 公开提示词源内存缓存（Prompts 页「拉取公开源」）。
    pub prompt_source_cache: Arc<crate::api::admin::prompts::PromptSourceCache>,
    /// 服务器自动更新管理器（Release 检测 / 下载 / 原地替换 / 自重启）。
    pub update: crate::update::UpdateManager,
    /// 间隔型定时任务的排期状态存储（凭证维护/模型刷新/版本检查的跨重启排期）。
    pub scheduled_task_store: Arc<crate::storage::FileStore>,
    /// 插件面的 provider 启停闸门（管理面写、数据面选号过滤读；KV 持久化）。
    pub provider_gates: crate::api::admin::plugins::ProviderGates,
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
/// 创建 OpenAI 兼容的错误响应（三参：code, message, status）。
///
/// 注意：与 `api::admin` 域的 `error_response`（两参：message, status）签名不同，
/// 故更名以消除跨域搬运代码时的 arity 陷阱（#36 曾因此把 8 个 Rust job 推红）。
pub(crate) fn openai_error_response(
    code: &str,
    message: &str,
    status: StatusCode,
) -> (StatusCode, Json<Value>) {
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
/// 当前请求体大小上限（字节）。数据面 audio multipart 接口用 `to_bytes`
/// 手动消费原始 Body，需显式钳制；其余 JSON 接口由 main.rs 的
/// `DefaultBodyLimit` 统一兜底。运行期后台修改后每次请求实时生效。
pub(crate) fn current_body_limit_bytes(state: &AppState) -> usize {
    state.config_manager.max_request_body_mb().max(1) * 1024 * 1024
}
