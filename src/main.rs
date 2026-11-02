// lib+bin 双 target 共享 src/*.rs：lib 将模块 pub 导出（视为对外 API 不报 dead），
// bin 以私有 mod 引入同一批文件，内部未直接调用的预留 API 会被 dead_code 误报。
// 此处统一豁免 bin 视角的结构性 dead_code 与 unused_imports（模块里的
// `pub use` 重导出是给 lib 用的对外 API，bin 视角看不到消费者），
// lib 视角的警告已单独清理干净。
#![allow(dead_code)]
#![allow(unused_imports)]
// 与 lib.rs 一致：openapi.rs 的深层 `json!` 需要更大的宏递归上限。
#![recursion_limit = "512"]

mod account;
mod account_login;
mod account_session;
mod agent;
mod altcha;
mod api;
mod auth;
mod bridge;
mod cache;
mod channel;
mod chat_session;
mod checkin;
mod config;
mod continuation;
mod cron;
mod debug_traffic;
mod egress;
mod error_translate;
mod graphql;
mod health;
mod hub;
mod ip;
mod log;
mod metrics;
mod model;
mod monitor;
mod net_guard;
mod notify;
mod oauth;
mod payment;
mod plan;
mod pricing;
mod prompt;
mod proxy;
mod proxy_pool;
mod quota_monitor;
mod ratelimit;
mod redemption;
mod semantic;
mod sse;
mod storage;
mod ticket;
mod token_estimate;
mod update;
mod usage;
mod user;
mod user_group;
mod web;

// SeaORM 多数据库后端模块（仅当启用 sea-orm feature 时编译）
#[cfg(feature = "sea-orm")]
mod db;

use std::sync::Arc;

use axum::extract::State;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use std::time::Duration;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

use account::AccountPool;
use api::auth::ApiKeyStore;
use api::openai::AppState;
use bridge::cf::CloudflareBridge;
use channel::ChannelStore;
use config::ConfigManager;
use health::{HealthTracker, LivezState};
use hub::Hub;
use ip::IpFilterStore;
use log::LogStore;
use model::ModelMapper;
use notify::NotifyService;
use payment::order_store::OrderStore;
use payment::stripe::StripeClient;
use payment::EpayClient;
use pricing::exchange_rate::ExchangeRateService;
use pricing::price_sync::PriceSyncService;
use pricing::PricingStore;
use proxy::CfApiClient;
use ratelimit::RateLimiter;
use redemption::RedemptionStore;
use storage::FileStore;
use ticket::TicketStore;
use usage::UsageTracker;
use user::{Role, UserStore};
use user_group::UserGroupStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 加载配置
    let config_manager = Arc::new(ConfigManager::new(None).await);
    // 启动时确保 session_secret 非空并持久化，避免登录后 token 无法鉴权
    ensure_session_secret(&config_manager).await;
    let config = config_manager.get().await;
    // XFF/X-Real-IP 信任快照：默认不信任（客户端可伪造），
    // 部署在可信反代后时在 config.toml 设 trust_proxy_headers = true
    crate::config::set_trust_proxy_headers(config.trust_proxy_headers);
    // 请求日志快照开关快照（数据面同步路径读取，见 config::log_body_enabled）
    crate::config::set_log_body(config.usage.log_body);
    // 调试模式（上游原始报文）开关快照（桥接层同步读取，见 config::debug_traffic_enabled）
    crate::config::set_debug_traffic(config.usage.debug_traffic);
    // 换行保活帧过滤开关快照（桥接层同步读取，见 config::strip_newline_keepalive）
    crate::config::set_strip_newline_keepalive(config.usage.strip_newline_keepalive);
    // models.dev 元数据补齐开关快照（模型元信息查询与刷新协程同步读取）
    crate::config::set_models_dev_enabled(config.usage.models_dev_enabled);
    // 排队等待设置快照（Qoder 桥每个请求同步读取，见 config::queue_settings）
    crate::config::set_queue_settings(
        config.usage.queue_max_waits as u64,
        config.usage.queue_wait_seconds as u64,
    );
    // 截断自动续写设置快照（数据面每个请求同步读取，见 config::auto_continue_enabled）
    crate::config::set_auto_continue_settings(
        config.usage.auto_continue,
        config.usage.auto_continue_max_rounds as u64,
    );
    // 按次计费固定额度快照（数据面同步路径读取，见 config::billing_flat_quota）
    crate::config::set_billing_flat_quota(config.usage.billing_flat_quota);

    // 初始化存储（默认 SQLite 后端；--no-default-features 构建降级为 JSON 文件）
    let data_dir = crate::config::expand_path(&config.server.data_dir);
    tokio::fs::create_dir_all(&data_dir).await?;
    // 凭证加密的密钥文件落在数据目录下（account.key，与库同目录、0600 权限）。
    // 必须在任何账号读写之前设定 —— 密钥路径错了会导致解不开存量凭证。
    crate::account_session::secret::set_data_dir(&data_dir);
    #[cfg(feature = "sqlite-kv")]
    let store = {
        // PostgreSQL 后端：database.url 非空且启用了 postgres feature 时走 PG，
        // 否则用默认 SQLite 文件后端。二者同为 FileStore 类型，业务层无感知。
        // 打开 PG 时顺带做一次 SQLite→PG 历史数据迁移（幂等）：老版本 SQLite
        // 库在 data_dir 或 data_dir/data 下，两个路径都探测。
        #[cfg(feature = "postgres")]
        let s = if config.database.is_enabled() {
            // 旧布局优先（老版本直接 data_dir/aigx.db），避免被新布局空库干扰
            let sqlite_dirs = vec![data_dir.clone(), data_dir.join("data")];
            match FileStore::open_postgres(
                &config.database.url,
                config.database.max_connections,
                &sqlite_dirs,
            ) {
                Ok(pg) => {
                    tracing::info!(
                        "PostgreSQL KV store enabled: {}",
                        crate::config::redact_url_credentials(&config.database.url)
                    );
                    pg
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to open PostgreSQL KV store ({}), falling back to SQLite: {}",
                        crate::config::redact_url_credentials(&config.database.url),
                        e
                    );
                    FileStore::open(data_dir.join("data"))?
                }
            }
        } else {
            FileStore::open(data_dir.join("data"))?
        };
        #[cfg(not(feature = "postgres"))]
        let s = FileStore::open(data_dir.join("data"))?;
        Arc::new(s)
    };
    #[cfg(not(feature = "sqlite-kv"))]
    let store = Arc::new(FileStore::new(data_dir.join("data")));

    // 初始化账号池
    let account_pool = Arc::new(AccountPool::new(store.clone()));

    // 初始化模型映射
    let model_mapper = Arc::new(ModelMapper::new(store.clone()));
    if let Err(e) = model_mapper.load() {
        tracing::error!("Failed to load model mapper: {e}");
    }

    // 初始化模型元信息注册表（P1：owned_by/上下文长度/能力 + 管理员覆盖）
    let model_metadata = Arc::new(model::metadata::ModelMetadataRegistry::new(store.clone()));
    if let Err(e) = model_metadata.load() {
        tracing::error!("Failed to load model metadata registry: {e}");
    }

    // models.dev 元数据目录：KV 缓存优先、内嵌快照保底（补值层，见 model::models_dev）
    {
        let (source, count) = model::models_dev::init(&store);
        tracing::info!("models.dev metadata catalog loaded: {count} entries from {source}");
    }

    // 初始化统一模型管理的启停规则（provider × model）
    let model_rules = Arc::new(model::manage::ModelRules::new(store.clone()));
    if let Err(e) = model_rules.load() {
        tracing::error!("Failed to load model rules: {e}");
    }

    // 初始化用量追踪
    let usage_tracker = Arc::new(UsageTracker::new(store.clone(), account_pool.clone()));

    // 初始化 API 密钥存储
    let mut api_key_store = ApiKeyStore::new(store.clone());
    if let Err(e) = api_key_store.load() {
        tracing::error!("Failed to load API key store: {e}");
    }

    // 初始化用户系统
    let user_store = Arc::new(UserStore::new(store.clone()));

    // 确保默认管理员账户存在
    ensure_default_admin(&user_store);

    // 注入用户存储到 API Key 存储（余额预检：零余额用户拒绝请求）
    api_key_store.with_user_store(user_store.clone());
    let api_key_store = Arc::new(api_key_store);

    // 初始化订单存储
    let order_store = Arc::new(OrderStore::new(store.clone()));

    // 初始化签到存储（对齐 new-api 签到送配额）
    let checkin_store = Arc::new(crate::user::checkin::CheckinStore::new(store.clone()));

    // 初始化签到中心存储（替上游账号每日领额度，与上面的用户签到是两套）
    let upstream_checkin = Arc::new(crate::checkin::CheckinStore::new(store.clone()));
    if let Err(e) = upstream_checkin.load() {
        tracing::error!("Failed to load upstream checkin store: {e}");
    }

    // 初始化易支付客户端（运行时按配置即时构造，无需常驻）
    let epay_client = Arc::new(EpayClient::new(config.epay.clone()));
    let stripe_client = Arc::new(StripeClient::new(config.stripe.clone()));

    // 初始化 CF API 客户端（AI Binding 桥接方式）
    let mut cf_api_client = CfApiClient::new(account_pool.clone(), model_mapper.clone());
    // 配置兜底 cf-ai-gw Worker 地址（无账号时使用），保证"零账号即可跑通"
    cf_api_client.with_fallback(config.cf_binding_url.clone(), String::new());
    let api_client = Arc::new(cf_api_client);

    // 初始化 Hub 并注册 Cloudflare Bridge
    let hub = Arc::new(Hub::new());
    let cf_bridge = Arc::new(CloudflareBridge::new(
        api_client.clone(),
        model_mapper.clone(),
    ));
    hub.register_specialized("cloudflare", cf_bridge);

    // 初始化健康检查
    let health_tracker = Arc::new(HealthTracker::new());
    let livez_state = Arc::new(LivezState::new());

    // 初始化通用渠道存储（混用 CF + 第三方 OpenAI 兼容上游）
    let channel_store = Arc::new(ChannelStore::new(store.clone()));

    // 路由策略（对照 Agent2API「设置 → 路由」）：策略/粘性/断路器/调度权重
    // 可配可热更；数据面 resolve_bridges 每请求读快照分派
    let routing_policy =
        Arc::new(crate::channel::routing_policy::RoutingPolicyStore::with_store(store.clone()));
    // 启动时把持久化的路由策略热应用到各组件（免重启生效）：
    // 断路器（开关+三参）、亲和 TTL、调度权重
    {
        let p = routing_policy.snapshot();
        channel_store
            .circuit_breaker()
            .set_enabled(p.breaker.enabled);
        if p.breaker.enabled {
            channel_store.circuit_breaker().update_params(
                p.breaker.threshold,
                p.breaker.cooldown_secs,
                p.breaker.success_threshold,
            );
        }
        if let Some((sticky_secs, hard_secs)) = p.sticky.ttls() {
            channel_store.affinity_cache().update_ttls(
                Duration::from_secs(sticky_secs),
                Duration::from_secs(hard_secs),
            );
        }
        channel_store.set_scheduler_config(p.scheduler_weights.clone());
    }

    // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）：
    // passthrough/custom/append 三模式，保存即生效
    let gateway_prompt = Arc::new(crate::prompt::GatewayPromptStore::with_store(store.clone()));

    // 额外脱敏请求头名单（对照 Agent2API「设置 → 网关 → 指纹脱敏」）：
    // 启动时从盘恢复进程级快照；保存设置时热更新
    if let Ok(Some(list)) = store.get::<Vec<String>>("sanitize_header_blacklist") {
        crate::config::set_extra_sensitive_headers(list);
    }

    // 初始化账号型渠道账号池（渠道内第二级调度：账号级优先级/冷却/并发/粘性）
    let account_sessions = crate::account_session::AccountSessionStore::with_egress(
        store.clone(),
        Arc::new(crate::egress::EgressCache::with_timeouts(
            Duration::from_secs(config.usage.api_timeout_secs.max(1)),
            Duration::from_secs(config.usage.connect_timeout_secs.max(1)),
            Duration::from_secs(config.usage.stream_idle_secs.max(1)),
        )),
    );
    // 换号重试间隔（「请求重试」设置；保存设置时也会实时更新）
    account_sessions.set_retry_interval_ms(config.usage.retry_interval_ms);
    account_sessions.set_retry_on_status(config.usage.retry_on_status.clone());
    // 渠道级重试次数（「请求重试」设置；保存设置时也会实时更新）
    crate::config::set_max_retries(config.usage.max_retries);
    // 账号池「同账号重试」档与非流式响应体超时
    crate::config::set_retry_self_count(config.usage.retry_self_count);
    crate::config::set_body_timeout_secs(config.usage.body_timeout_secs);
    // 出站指纹脱敏开关（请求体侧；保存设置时实时更新）
    crate::config::set_sanitize_fingerprints(config.usage.sanitize_fingerprints);
    // Cline 伪装头覆盖表（对齐 Agent2API「设置 → 网关 → Cline 伪装头」）：
    // 启动时从配置恢复进程级快照；保存设置时热更新
    crate::config::set_cline_header_overrides(
        config
            .usage
            .cline_headers
            .overrides
            .clone()
            .into_iter()
            .collect(),
    );
    // 直接换号/换渠道状态码名单（选路时同步判定，保存设置时实时更新）
    crate::config::set_retry_on_status_codes(config.usage.retry_on_status.clone());

    // 初始化模型定价目录
    let pricing_store = Arc::new(PricingStore::new(store.clone()));

    // 初始化全局 IP 过滤（白名单/黑名单，批次3）
    let ip_filter = Arc::new(IpFilterStore::new(store.clone()));
    if let Err(e) = ip_filter.load() {
        tracing::error!("Failed to load IP filter: {e}");
    }

    // 初始化价格同步服务（多源定价同步，批次3；tokio Mutex 跨 await 持锁）
    let price_sync = Arc::new(tokio::sync::Mutex::new(PriceSyncService::new(
        pricing_store.clone(),
    )));

    // 初始化汇率服务（多币种转换，批次4）
    let exchange_rate = Arc::new(ExchangeRateService::with_store(store.clone()));

    // 初始化用户分组存储
    let user_group_store = Arc::new(UserGroupStore::new(store.clone()));

    // 初始化日志与审计存储（功能 1）
    let log_store = Arc::new(LogStore::new(store.clone()));
    // 在途请求登记表（进程内）：配合日志页「查看并终止正在进行的请求」
    let inflight = Arc::new(crate::log::inflight::InflightRegistry::new());

    // 初始化兑换码存储（功能 2）
    let redemption_store = Arc::new(RedemptionStore::new(store.clone()));

    // 初始化套餐模板存储（按量套餐：模板 → 发 key 交付）
    let plan_store = Arc::new(plan::PlanStore::new(store.clone()));

    // 初始化订阅存储（时长订阅：余额购买 → 独立配额池 + 分组升降级，#85）
    let subscription_store = Arc::new(plan::subscription::SubscriptionStore::new(store.clone()));

    // 初始化工单存储（用户提交问题 / 管理员回复）
    let ticket_store = Arc::new(TicketStore::new(store.clone()));
    let chat_sessions = crate::chat_session::ChatSessionStore::new(store.clone());

    // 初始化限流器（功能 3，带持久化配置）
    let rate_limiter = Arc::new(RateLimiter::with_store(store.clone()));

    // 初始化通知服务（Telegram + SMTP + Slack + Webhook）
    // 启动时先从 FileStore 恢复管理界面保存过的配置（无记录则用 config.toml 值）
    let notify_service = Arc::new(NotifyService::new(config.notify.clone()));
    notify_service.restore_config(&store).await;

    // 初始化告警规则评估器（持久化：FileStore 加载规则集与历史）
    let alert_evaluator = notify::alert_patrol::shared_evaluator_persistent(&store);

    // 共享 HTTP 客户端（性能热点 H5/H6）。
    // 全应用复用同一个 reqwest::Client，避免每次请求新建客户端（连接池/TLS 握手开销）。
    // 超时 300s 覆盖大多数上游推理时长；连接池由 reqwest 内部管理。
    // 强制 HTTP/1.1（参照 rust-tunnel upstream 客户端策略）：reqwest 的
    // read_timeout 按每个 HTTP 帧重置，HTTP/2 的 PING 帧会持续重置读超时，
    // 挂连接将永远检测不到空闲超时——后续若把超时策略收紧为 read_timeout
    // 空闲检测，h2 下挂连接无法检出；且部分上游网关对 h2 连接有激进空闲
    // 超时，连接被静默关闭后复用会批量失败。HTTP/1.1 无此两类风险。
    let http_client = Arc::new(
        reqwest::Client::builder()
            .http1_only()
            .timeout(Duration::from_secs(config.usage.api_timeout_secs.max(1)))
            .connect_timeout(Duration::from_secs(
                config.usage.connect_timeout_secs.max(1),
            ))
            .read_timeout(Duration::from_secs(config.usage.stream_idle_secs.max(1)))
            .build()
            .expect("failed to build shared reqwest::Client"),
    );
    // SSE 首事件守卫上限随「响应头等待」配置生效（保存设置时也会实时更新）
    crate::bridge::set_sse_first_event_deadline(Duration::from_secs(
        config.usage.headers_timeout_secs.max(1),
    ));

    // 初始化公开注册速率限制器（per-IP，60s 窗口，最多 10000 个 IP 条目）
    // 使用基于 dashmap 的 AsyncCache 替代 moka（离线环境 moka 不可得，行为等效）
    let register_limiter = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(60))
            .build(),
    );

    // 初始化登录限流器（per-IP，60s 窗口，最多 10000 个 IP 条目）
    let login_limiter = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(60))
            .build(),
    );

    // 登录失败锁定状态：<ip> → 连续失败次数。
    //
    // TTL = 锁定时长（配置 `security.lock_minutes`，默认 5 分钟）；三个门槛里
    // 只有它建缓存时固定，改后需重启服务生效 —— 与网关监听地址/端口同口径，
    // 页面会给出同样的提示。其余两项（阈值 / 每分钟上限）走原子快照，保存即生效。
    let security = config_manager.get().await.security.normalized();
    let login_failures = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(security.lock_minutes * 60))
            .build(),
    );

    // 安全策略的进程级快照（登录 handler 同步读取，保存即生效）
    crate::config::set_security_policy(
        security.lockout_enabled,
        security.fail_threshold,
        security.rate_limit_per_minute,
    );
    // 机器人校验开关快照（登录 / 注册 handler 同步读取）
    config::set_security_captcha(security.captcha_enabled);

    // 邮箱验证码登录：<email> → 6 位验证码（TTL=5 分钟）
    let login_code_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 2FA/TOTP 待验证缓存：<tmp_token> → email（TTL=5 分钟）
    // 密码验证成功且用户开启 TOTP 时，先生成 tmp_token 挂起，
    // 客户端二次提交 TOTP 码后才签发 session。
    let totp_pending_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 2FA/TOTP 启用暂存缓存：<user_id> → base32 secret（TTL=5 分钟）
    // setup 生成的新 secret 先暂存，enable 验证一次后落库；
    // 未在窗口内启用则作废，不产生残留。
    let totp_setup_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 会话注册表（P1：会话撤销）——无状态 HMAC token 的服务端撤销层
    let session_registry = Arc::new(api::auth::SessionRegistry::new());

    // 初始化 SeaORM 数据库连接（可选后端）
    //
    // 渐进式迁移策略：
    // - config.database.url 为空 → 使用默认 FileStore，db_conn = None
    // - config.database.url 有值 → 启用 SeaORM，db_conn = Some(conn)
    //
    // 仅当启用 sea-orm feature 时编译此段；否则 db_conn 字段不存在。
    #[cfg(feature = "sea-orm")]
    let db_conn: Option<sea_orm::DatabaseConnection> = {
        if config.database.is_enabled() {
            match db::DatabaseManager::connect(&config.database.url).await {
                Ok(manager) => {
                    tracing::info!(
                        "SeaORM backend enabled: {} (backend={})",
                        crate::config::redact_url_credentials(&config.database.url),
                        manager.backend().as_str()
                    );
                    Some(manager.connection().clone())
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to connect SeaORM backend ({}), falling back to FileStore: {}",
                        crate::config::redact_url_credentials(&config.database.url),
                        e
                    );
                    None
                }
            }
        } else {
            tracing::info!("Database URL not configured, using default FileStore backend");
            None
        }
    };

    // 注册 OpenAI 适配器族 bridge（预留）
    // hub.register_family(Adapter::Openai, cf_openai_bridge);

    // 构建应用状态
    let semantic_router = std::sync::Arc::new(crate::semantic::SemanticRouter::new());

    let response_cache = crate::cache::AsyncCache::<String, serde_json::Value>::builder()
        .max_capacity(1000)
        .time_to_live(std::time::Duration::from_secs(300))
        .build();
    let response_cache = std::sync::Arc::new(response_cache);

    let oauth_state_cache = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, i64>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(600))
            .build(),
    );

    // OAuth 绑定 state → 目标 email（个人中心发起绑定；TTL 10 分钟）
    let oauth_bind_state = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(600))
            .build(),
    );
    // OAuth 登录一次性票据 ticket → 会话 token（token 不进 URL，SPA 用 ticket 换回；TTL 2 分钟）
    let oauth_ticket = std::sync::Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(120))
            .build(),
    );

    // 服务器自动更新管理器（Release 检测 / 原地替换 / exec 自重启）
    let update = crate::update::UpdateManager::new(store.clone(), data_dir.clone());

    // 网络代理池（网络代理页）：账号 proxy 字段的 pool:/rotate 编码解析依赖它
    crate::proxy_pool::install(store.clone());

    let state = AppState {
        api_client,
        model_mapper,
        model_metadata,
        model_rules,
        usage_tracker,
        account_pool,
        api_key_store,
        config_manager: config_manager.clone(),
        hub,
        user_store,
        order_store,
        checkin_store,
        upstream_checkin,
        epay_client,
        stripe_client,
        health_tracker: health_tracker.clone(),
        livez_state: livez_state.clone(),
        channel_store,
        routing_policy,
        gateway_prompt,
        account_sessions,
        pricing_store,
        user_group_store,
        log_store,
        inflight,
        redemption_store,
        plan_store,
        subscription_store,
        ticket_store,
        chat_sessions,
        rate_limiter,
        notify_service,
        alert_evaluator,
        alert_store: store.clone(),
        ip_filter,
        price_sync,
        exchange_rate,
        register_limiter,
        login_limiter,
        login_failures,
        login_code_cache,
        totp_pending_cache,
        totp_setup_cache,
        session_registry,
        http_client,
        response_cache,
        semantic_router,
        oauth_state_cache,
        oauth_bind_state,
        oauth_ticket,
        #[cfg(feature = "sea-orm")]
        db_conn,
        // Agent 运行时恒存在；enabled 由运行期配置驱动（设置页保存后立即生效）
        agent_state: Some(std::sync::Arc::new(crate::agent::AgentState::new(
            store.clone(),
            config.agent.clone(),
        ))),
        prompt_source_cache: std::sync::Arc::new(
            crate::api::admin::prompts::PromptSourceCache::new(),
        ),
        update,
        scheduled_task_store: store.clone(),
    };

    tracing::info!(
        "Configuration loaded: {}:{} | Accounts: {} | API Keys: {} | Users: {} | EpayReady: {} | SeaORM: {}",
        config.server.host,
        config.server.port,
        state.account_pool.list().len(),
        state.api_key_store.len(),
        state.user_store.list().len(),
        state.epay_client.config().ready(),
        if state.has_sea_orm_backend() { "enabled" } else { "disabled (FileStore)" },
    );

    // Prometheus 初始快照：渠道数量 + 模型健康等级（/metrics 查询用）。
    // 请求处理路径会持续更新请求/token/latency 指标；此处在启动时一次性填充
    // 渠道与健康状态。后续渠道增删/健康变化由各自调用点刷新（set_channels/set_health）。
    {
        let channels = state.channel_store.list();
        let enabled = channels.iter().filter(|c| c.is_enabled()).count() as u64;
        let disabled = channels.len() as u64 - enabled;
        crate::metrics::global().set_channels(enabled, disabled);
        for (model, level) in state.health_tracker.all_health() {
            crate::metrics::global().set_health(&model, level.into());
        }
    }

    // 构建路由
    // Start CF quota monitoring background task
    crate::quota_monitor::spawn_monitor(
        state.account_pool.clone(),
        state.notify_service.clone(),
        state.http_client.as_ref().clone(),
    );

    // 启动告警巡检后台任务（批次5：渠道健康/内存 → 规则评估 → 多渠道分发）
    notify::alert_patrol::spawn_alert_patrol(
        state.channel_store.clone(),
        state.notify_service.clone(),
        state.alert_evaluator.clone(),
        store.clone(),
    );

    // 启动渠道探活后台任务（批次7c：周期 1-token 探测 → 断路器/健康追踪）
    // 默认关闭：[channel] probe_interval_secs 缺省 0，不对上游发探测请求。
    {
        let probe_interval_secs = state.config_manager.get().await.channel.probe_interval_secs;
        channel::prober::spawn_channel_prober(
            state.channel_store.clone(),
            state.http_client.as_ref().clone(),
            probe_interval_secs,
        );
    }

    // P1-17 定时任务框架：注册会话撤销表清扫（第一个接入的任务）。
    // 撤销记录与会话 TTL 对齐：会话自然过期后撤销记录无意义，
    // 周期清扫避免注册表无限膨胀。清扫门槛 = now - session_ttl，
    // 保证在有效期内被撤销的会话仍被拦截。
    {
        let session_registry = state.session_registry.clone();
        let config_manager = state.config_manager.clone();
        let scheduler = cron::Scheduler::new();
        scheduler.spawn(cron::TaskSpec {
            name: "session-registry-sweep",
            interval: Duration::from_secs(3600),
            first_run_delay: Duration::from_secs(600),
            run: Box::new(move || {
                let reg = session_registry.clone();
                let cfg = config_manager.clone();
                Box::pin(async move {
                    let ttl_hours = cfg.get().await.admin.session_ttl_hours.max(1);
                    let cutoff = chrono::Utc::now().timestamp() - ttl_hours * 3600;
                    reg.sweep_expired_revocations(cutoff) as u64
                })
            }),
        });
        // G2 预留 TTL 解冻：进程崩溃/请求挂死留下的冻结额度定期归还。
        // TTL 30 分钟，远超单请求上限（max_tokens 请求通常在数分钟内完成）。
        {
            let user_store = state.user_store.clone();
            let api_key_store = state.api_key_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "reservation-sweep",
                interval: Duration::from_secs(600),
                first_run_delay: Duration::from_secs(300),
                run: Box::new(move || {
                    let us = user_store.clone();
                    let ks = api_key_store.clone();
                    Box::pin(async move {
                        let ttl = 30 * 60;
                        us.release_stale_reservations(ttl) + ks.release_stale_reservations(ttl)
                    })
                }),
            });
        }
        // C1 渠道健康档案落盘 + 30 天滚动清理：内存累加器每 5 分钟 flush，
        // 进程重启/崩溃最多丢一个采样周期的数据（运营趋势可接受）。
        {
            let channel_store = state.channel_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "health-archive-flush",
                interval: Duration::from_secs(300),
                first_run_delay: Duration::from_secs(120),
                run: Box::new(move || {
                    let cs = channel_store.clone();
                    Box::pin(async move {
                        let a = cs.health_archive();
                        a.flush() + a.prune()
                    })
                }),
            });
        }
        // 签到中心调度：30 秒轮询，按计划的 HH:MM（UTC+8）触发，当天去重
        {
            let store = state.upstream_checkin.clone();
            let sessions = state.account_sessions.clone();
            let egress = state.account_sessions.egress().clone();
            crate::checkin::scheduler::spawn(store, sessions, egress);
        }
        // 账号池周期维护：清理到期冷却记录 + 凭证过期状态同步（每 60s 一轮）
        {
            let sessions = state.account_sessions.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "account-session-sweep",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(30),
                run: Box::new(move || {
                    let pool = sessions.clone();
                    Box::pin(async move { crate::account_session::maintenance::sweep(&pool) })
                }),
            });
        }
        // 账号 token 自动刷新：临期（qoder/autoclaw 5 分钟、trae 24 小时）即刷，
        // 凭证自动维护：已迁入 scheduled_tasks 间隔型定时任务循环（`scheduled_tasks::spawn`），
        // 开关与间隔在「定时任务」页可配，排期跨重启保留（见 scheduled_tasks 模块）。
        // 余额自动查询：按账号独立间隔（usage_query_interval_secs）到期即查，
        // 写回快照供界面上次读数展示。未配间隔的账号一律跳过。
        {
            let sessions = state.account_sessions.clone();
            let http = state.http_client.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "account-session-usage-query",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(120),
                run: Box::new(move || {
                    let pool = sessions.clone();
                    let http = http.clone();
                    Box::pin(async move {
                        crate::account_session::usage_query::sweep(&pool, &http).await;
                        0
                    })
                }),
            });
        }
        // models.dev 元数据目录刷新 / 服务器自动更新检查：已迁入 scheduled_tasks
        // 间隔型定时任务循环（见 `scheduled_tasks::spawn`），开关与间隔在「定时任务」页可配。
        // 服务器自动更新检查：已迁入 scheduled_tasks 间隔型定时任务循环
        // （`scheduled_tasks::spawn` 内的「软件版本检查」任务）。
        // 日志保留清理：按运维设置的「保留天数」每日扫一次 reqlog/auditlog
        // 前缀，删除早于截止时间的日志（参照 new-api DeleteOldLogBatch）。
        // 天数未配置时不清理（容量上限 purge_overflow 仍然兜底）。
        {
            let log_store = state.log_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "log-retention-sweep",
                interval: Duration::from_secs(86400),
                first_run_delay: Duration::from_secs(600),
                run: Box::new(move || {
                    let ls = log_store.clone();
                    Box::pin(async move {
                        let (days, _capacity, audit_days, daily_days) =
                            ls.requests.retention_full();
                        // 容量上限由写入路径的 purge_overflow 消费（同样读
                        // retention 配置），此处只负责按天清理。
                        // 两类日志各自独立：审计可单独设天数（未设时跟随请求日志），
                        // 因此请求日志未设天数时仍要清理审计。
                        let mut removed: u64 = 0;
                        let req_cutoff =
                            days.map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400);
                        if let Some(cutoff) = req_cutoff {
                            match ls.requests.delete_older_than(cutoff) {
                                Ok(n) => removed += n as u64,
                                Err(e) => tracing::warn!("请求日志保留清理失败: {e}"),
                            }
                        }
                        let audit_cutoff = audit_days
                            .map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400)
                            .or(req_cutoff);
                        if let Some(cutoff) = audit_cutoff {
                            match ls.audits.delete_older_than(cutoff) {
                                Ok(m) => removed += m as u64,
                                Err(e) => tracing::warn!("审计日志保留清理失败: {e}"),
                            }
                        }
                        // 第三档：日聚合保留（None = 永久）。键区间尾段是
                        // YYYY-MM-DD，含今天 N 个自然日 → cutoff=今天−(N−1)。
                        if let Some(n) = daily_days {
                            let cutoff = crate::log::daily_agg::cutoff_date_key(n);
                            match ls.daily.prune_before(&cutoff) {
                                Ok(k) => removed += k as u64,
                                Err(e) => tracing::warn!("日聚合保留清理失败: {e}"),
                            }
                        }
                        removed
                    })
                }),
            });
        }
        // 日聚合历史回填：聚合表上线前已有明细，启动后一次性折进按天主档
        // （只动 date<今天 的日子，今天归运行期记账）。回填标记存在即跳过
        // ——marker 同时是报表读路径的聚合闸门（阶段 2 的 agg_available）。
        // 24h 周期只为兜住「首次启动失败/期间明细增长」的补跑。
        {
            let log_store = state.log_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "daily-agg-backfill",
                interval: Duration::from_secs(86400),
                first_run_delay: Duration::from_secs(60),
                run: Box::new(move || {
                    let ls = log_store.clone();
                    Box::pin(async move {
                        if ls.daily.agg_available() {
                            return 0;
                        }
                        match ls.backfill_history(5000) {
                            Ok(r) => {
                                tracing::info!(
                                    "日聚合回填完成: 扫描 {} 条明细, 主档 {} 天, 用户行 {}, shrink 守卫跳过 {}",
                                    r.scanned,
                                    r.days_written,
                                    r.user_rows_written,
                                    r.skipped_shrink
                                );
                                r.days_written as u64
                            }
                            Err(e) => {
                                tracing::warn!("日聚合回填失败（下一周期重试）: {e}");
                                0
                            }
                        }
                    })
                }),
            });
        }
        // #85 订阅周期任务：到期置 expired + 分组回退；周期重置点清零用量。
        // 读侧（find_active/try_charge）已做动态过期判断，此处落库保证
        // 用户订阅列表与分组权益最终一致。
        {
            let sub_store = state.subscription_store.clone();
            let user_store = state.user_store.clone();
            let plan_store = state.plan_store.clone();
            scheduler.spawn(cron::TaskSpec {
                name: "subscription-expire-reset",
                interval: Duration::from_secs(60),
                first_run_delay: Duration::from_secs(30),
                run: Box::new(move || {
                    let ss = sub_store.clone();
                    let us = user_store.clone();
                    let ps = plan_store.clone();
                    Box::pin(async move {
                        let now = chrono::Utc::now().timestamp();
                        // 1) 到期扫描：status 落库 + 分组回退
                        let expired = ss.expire_due(now);
                        let mut actions = expired.len() as u64;
                        for sub in &expired {
                            if let Err(e) =
                                crate::plan::subscription::downgrade_user_group(&us, &ss, sub, now)
                            {
                                tracing::error!(
                                    "subscription group downgrade failed for {}: {e}",
                                    sub.id
                                );
                            }
                        }
                        // 2) 周期重置：到点的活跃订阅清零已用配额并推进锚点
                        for sub in ss.list_reset_due(now) {
                            let Some(plan) = ps.get(&sub.plan_id) else {
                                continue;
                            };
                            // 基准点：上次重置时间或购买时间
                            let mut base = if sub.last_reset_time > 0 {
                                sub.last_reset_time
                            } else {
                                sub.start_time
                            };
                            let mut next = crate::plan::subscription::calc_next_reset(
                                base,
                                &plan,
                                sub.end_time,
                            );
                            let mut advanced = false;
                            while next > 0 && next <= now {
                                advanced = true;
                                base = next;
                                next = crate::plan::subscription::calc_next_reset(
                                    base,
                                    &plan,
                                    sub.end_time,
                                );
                            }
                            if !advanced {
                                continue;
                            }
                            if let Err(e) = ss.apply_reset(&sub.id, base, next) {
                                tracing::error!(
                                    "subscription quota reset failed for {}: {e}",
                                    sub.id
                                );
                            } else {
                                actions += 1;
                            }
                        }
                        actions
                    })
                }),
            });
        }
        tracing::info!(
            "cron scheduler started: {} task(s) (session-registry-sweep, reservation-sweep, health-archive-flush, account-session-sweep, account-session-usage-query, log-retention-sweep, subscription-expire-reset)",
            scheduler.task_count()
        );
    }

    // 间隔型定时任务调度循环：凭证维护 / 模型刷新 / 版本检查三条后端任务按各自
    // 配置的开关与间隔执行，排期跨重启保留。前端三条自动刷新任务只存配置，由界面读。
    crate::scheduled_tasks::spawn(state.clone());
    tracing::info!(
        "scheduled task loop started (credential maintenance / model refresh / update check)"
    );

    let app = build_router(state, &config);

    // 启动服务器
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {}", addr);

    // 优雅关闭：收到 SIGTERM/SIGINT 后标记 draining，等待现有请求完成
    // `into_make_service_with_connect_info` 让 handler 能拿到真实对端 IP
    // （ConnectInfo<SocketAddr>）：反向代理未开 trust_proxy_headers 时，
    // 登录/注册限流按真实来源 IP 计数，而不是全站共用一个 "unknown" 键。
    let livez = livez_state.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutdown signal received, marking draining...");
        livez.mark_shutting_down();
        // 给现有请求一些时间完成
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        tracing::info!("Graceful shutdown complete");
    })
    .await?;

    Ok(())
}

/// 确保配置中存在非空的 session_secret，否则生成随机 UUID 并持久化。
///
/// 修复场景：当 config.admin.session_secret 为空（默认初始状态）时，
/// handle_login 临时生成随机 UUID 作为 secret 签名 session token 返回给客户端，
/// 但该 secret 未持久化，后续 verify_admin/verify_user 仍用空 secret 创建 SessionStore，
/// HMAC 签名不匹配，所有需鉴权的接口返回 401。
///
/// 本函数在启动时一次性补齐并持久化 secret，确保签名与验证使用同一密钥。
async fn ensure_session_secret(config_manager: &ConfigManager) {
    let config = config_manager.get().await;
    if !config.admin.session_secret.is_empty() {
        return;
    }
    let mut new_config = config.clone();
    new_config.admin.session_secret = uuid::Uuid::new_v4().to_string();
    match config_manager.update(new_config).await {
        Ok(_) => tracing::info!("Generated and persisted empty session_secret on first startup"),
        Err(e) => tracing::error!("Failed to persist generated session_secret: {e}"),
    }
}

/// 确保默认管理员账户存在（仅首次启动时创建）
///
/// 首次启动（系统中尚不存在任何 Admin 角色用户）时自动创建写死的内置管理员账户：
/// - 邮箱：`admin@gmail.com`
/// - 用户名：`admin`
/// - 密码：`123456`（写死，用户明确要求内置固定密码）
///
/// 已存在 Admin 用户时直接跳过，绝不删除或重建已存在的管理员。
///
/// 邮箱常量在 `crate::user::DEFAULT_ADMIN_EMAIL`（users.rs 锁定逻辑共用）。
const DEFAULT_ADMIN_USERNAME: &str = "admin";
const DEFAULT_ADMIN_PASSWORD: &str = "123456";

fn ensure_default_admin(user_store: &UserStore) {
    // 检查是否已存在任意 Admin 角色用户；若已存在则跳过，绝不删除/重建
    let has_admin = user_store.list().iter().any(|u| u.role == Role::Admin);
    if has_admin {
        tracing::info!("Admin account already exists, skip default admin initialization");
        return;
    }

    // 创建写死凭据的内置管理员账户
    match user_store.create_with_username(
        crate::user::DEFAULT_ADMIN_EMAIL,
        DEFAULT_ADMIN_USERNAME,
        DEFAULT_ADMIN_PASSWORD,
        Role::Admin,
        0,
    ) {
        Ok(user) => {
            // 标记「首次登录必须改密」：内置密码是公开的弱口令，
            // 前端登录后据此强制跳转改密页。
            if let Err(e) = user_store.update(&user.id, |u| {
                u.must_change_password = true;
            }) {
                tracing::warn!("Failed to set must_change_password for default admin: {e}");
            }
            tracing::warn!(
                "First-time setup: built-in admin account created. \
                 email={} | username={} | password={} \
                 — PLEASE LOGIN AND CHANGE THE PASSWORD IMMEDIATELY.",
                crate::user::DEFAULT_ADMIN_EMAIL,
                DEFAULT_ADMIN_USERNAME,
                DEFAULT_ADMIN_PASSWORD,
            );
        }
        Err(e) => {
            tracing::error!("Failed to create default admin account: {}", e);
        }
    }
}

fn build_router(state: AppState, config: &config::AppConfig) -> Router {
    // 请求体大小上限在运行期可经管理后台修改，故此处用中间件每次请求
    // 实时读配置快照（config_manager 为 tokio RwLock），不依赖启动时的
    // config 副本，保证后台改完后无需重启即生效。
    let body_limit_state = state.clone();
    let body_limit_mw = tower::ServiceBuilder::new().map_request(
        move |mut req: axum::http::Request<axum::body::Body>| {
            let limit = body_limit_state.config_manager.max_request_body_mb().max(1) * 1024 * 1024;
            req.extensions_mut()
                .insert(axum::extract::DefaultBodyLimit::max(limit));
            req
        },
    );

    // 注意：axum 0.7（matchit 0.7）的路由参数语法是 `:id`，不是 `{id}`。
    // `{id}` 是 axum 0.8（matchit 0.8）的语法，在 0.7 下会被当作字面量路径段，
    // 导致所有带参数路由匹配失败、请求掉到 fallback_service 返回 405/404。
    // 管理 API 路由（模块化）
    let admin_routes = Router::new()
        // Auth handlers (来自 api::admin::auth)
        .route("/api/auth/login", post(api::admin::handle_login))
        // 公开端点：机器人校验领题（登录页提交前先解一道 ALTCHA proof-of-work）
        .route("/api/auth/captcha", get(api::admin::handle_captcha))
        // 公开端点：按客户端 IP 推荐界面语言（登录页默认语言；无代理头时返回 null）
        .route("/api/public/geo-lang", get(api::admin::handle_geo_lang))
        .route(
            "/api/auth/login/send-code",
            post(api::admin::handle_login_send_code),
        )
        .route(
            "/api/auth/login/code",
            post(api::admin::handle_login_with_code),
        )
        .route("/api/auth/login/totp", post(api::admin::handle_login_totp))
        // 2FA/TOTP 用户侧自助管理（P1 收尾）
        .route("/api/auth/totp/setup", post(api::admin::handle_totp_setup))
        .route(
            "/api/auth/totp/enable",
            post(api::admin::handle_totp_enable),
        )
        .route(
            "/api/auth/totp/disable",
            post(api::admin::handle_totp_disable),
        )
        .route("/api/auth/register", post(api::admin::handle_register))
        .route(
            "/api/auth/github",
            get(api::admin::handle_github_oauth_authorize),
        )
        .route(
            "/api/auth/github/callback",
            get(api::admin::handle_github_oauth_callback),
        )
        // 阶段1：忘记密码 / 重置密码（参照 burncloud auth.rs）
        .route(
            "/api/auth/forgot-password",
            post(api::admin::handle_forgot_password),
        )
        .route(
            "/api/auth/reset-password",
            post(api::admin::handle_reset_password),
        )
        .route("/api/auth/logout", post(api::admin::handle_logout))
        .route(
            "/api/auth/change-password",
            post(api::admin::handle_change_password),
        )
        // 阶段1：Google OAuth
        .route(
            "/api/auth/google",
            get(api::admin::handle_google_oauth_authorize),
        )
        .route(
            "/api/auth/google/callback",
            get(api::admin::handle_google_oauth_callback),
        )
        // LinuxDO OAuth
        .route(
            "/api/auth/linuxdo",
            get(api::admin::handle_linuxdo_oauth_authorize),
        )
        .route(
            "/api/auth/linuxdo/callback",
            get(api::admin::handle_linuxdo_oauth_callback),
        )
        // OAuth 个人中心绑定：start 需登录态（verify_user），返回授权 URL 供前端整页跳；
        // 三家 callback 复用上面同一路由，由 state 命中 oauth_bind_state 判定走绑定流。
        .route(
            "/api/auth/github/bind/start",
            post(api::admin::handle_github_bind_start),
        )
        .route(
            "/api/auth/google/bind/start",
            post(api::admin::handle_google_bind_start),
        )
        .route(
            "/api/auth/linuxdo/bind/start",
            post(api::admin::handle_linuxdo_bind_start),
        )
        .route(
            "/api/auth/oauth/unbind",
            post(api::admin::handle_oauth_unbind),
        )
        // OAuth 登录回跳：callback 只发一次性 ticket，SPA 用它换回会话数据（token 不进 URL）。
        .route(
            "/api/auth/oauth/consume",
            post(api::admin::handle_oauth_consume),
        )
        // 用户管理
        .route("/api/users", get(api::admin::handle_list_users))
        .route("/api/users", post(api::admin::handle_create_user))
        .route("/api/users/check", get(api::admin::handle_check_username))
        .route("/api/users/me", get(api::admin::handle_me))
        // 用户自助与管理操作（对齐 new-api PUT /self、POST /manage）
        .route("/api/users/self", put(api::admin::handle_update_self))
        .route("/api/users/manage", post(api::admin::handle_manage_user))
        // 邀请返利（对齐 new-api /api/user/aff 与 /api/user/aff_transfer）
        .route("/api/aff", get(api::admin::handle_get_aff_code))
        .route("/api/aff_transfer", post(api::admin::handle_aff_transfer))
        // 签到（对齐 new-api GET/POST /api/user/checkin）—— 用户侧，发本站配额
        .route("/api/checkin", get(api::admin::handle_checkin_status))
        .route("/api/checkin", post(api::admin::handle_do_checkin))
        // 聊天会话（按用户隔离的对话记录；与 Agent 会话同套机制）
        .route(
            "/api/chat/sessions",
            get(crate::chat_session::handle_list_chat_sessions)
                .post(crate::chat_session::handle_create_chat_session),
        )
        .route(
            "/api/chat/sessions/:id",
            get(crate::chat_session::handle_get_chat_session)
                .delete(crate::chat_session::handle_delete_chat_session),
        )
        .route(
            "/api/chat/sessions/:id/rename",
            post(crate::chat_session::handle_rename_chat_session),
        )
        .route(
            "/api/chat/sessions/:id/messages",
            post(crate::chat_session::handle_append_chat_message)
                .put(crate::chat_session::handle_replace_chat_messages),
        )
        // 签到中心（管理员替上游账号签到，与上面的用户签到是两套）
        .route(
            "/api/checkin-center",
            get(api::admin::handle_checkin_center),
        )
        .route(
            "/api/checkin-center/run",
            post(api::admin::handle_checkin_run),
        )
        .route(
            "/api/checkin-center/records",
            get(api::admin::handle_checkin_records)
                .delete(api::admin::handle_clear_checkin_records),
        )
        .route(
            "/api/checkin-center/settings",
            get(api::admin::handle_get_checkin_settings)
                .put(api::admin::handle_update_checkin_settings),
        )
        .route(
            "/api/checkin-center/schedules",
            get(api::admin::handle_list_checkin_schedules)
                .post(api::admin::handle_upsert_checkin_schedule),
        )
        .route(
            "/api/checkin-center/schedules/remove",
            post(api::admin::handle_remove_checkin_schedule),
        )
        .route(
            "/api/checkin-center/schedules/run",
            post(api::admin::handle_run_checkin_schedule),
        )
        // 一次性 / 限时福利通道（新手礼 / 保活 / 成长任务 / 限时套餐 / UMID）
        .route(
            "/api/checkin-center/benefits",
            get(api::admin::handle_benefits_list),
        )
        .route(
            "/api/checkin-center/benefits/run",
            post(api::admin::handle_benefit_run),
        )
        .route(
            "/api/checkin-center/benefits/keepalive",
            get(api::admin::handle_get_keepalive).put(api::admin::handle_put_keepalive),
        )
        .route("/api/users/:id", put(api::admin::handle_update_user))
        .route("/api/users/:id", delete(api::admin::handle_delete_user))
        // 管理员强制禁用用户 2FA（对齐 new-api DELETE /api/user/:id/2fa）
        .route(
            "/api/users/:id/2fa",
            delete(api::admin::handle_admin_disable_2fa),
        )
        // 工单（用户侧 / 管理侧，对齐 v2board Ticket）
        .route("/api/tickets", get(api::admin::handle_user_list_tickets))
        .route("/api/tickets", post(api::admin::handle_user_save_ticket))
        .route(
            "/api/tickets/reply",
            post(api::admin::handle_user_reply_ticket),
        )
        .route(
            "/api/tickets/close",
            post(api::admin::handle_user_close_ticket),
        )
        .route(
            "/api/admin/tickets",
            get(api::admin::handle_admin_list_tickets),
        )
        .route(
            "/api/admin/tickets/reply",
            post(api::admin::handle_admin_reply_ticket),
        )
        .route(
            "/api/admin/tickets/close",
            post(api::admin::handle_admin_close_ticket),
        )
        // 渠道管理
        .route("/api/channels", get(api::admin::handle_list_channels))
        // 用户侧可用模型聚合（对齐 new-api /api/user/models）
        .route(
            "/api/models/available",
            get(api::admin::handle_available_models),
        )
        // 公开提示词源拉取（Prompts 页一键导入社区提示词）
        .route(
            "/api/prompts/sources",
            get(api::admin::handle_prompt_sources),
        )
        .route("/api/prompts/fetch", post(api::admin::handle_prompt_fetch))
        .route(
            "/api/prompts/translate",
            post(api::admin::handle_prompt_translate),
        )
        .route("/api/channels", post(api::admin::handle_add_channel))
        .route("/api/channels/:id", put(api::admin::handle_update_channel))
        .route("/api/channels/:id", patch(api::admin::handle_patch_channel))
        .route(
            "/api/channels/:id",
            delete(api::admin::handle_delete_channel),
        )
        // 令牌管理
        .route("/api/tokens", get(api::admin::handle_list_tokens))
        .route("/api/tokens", post(api::admin::handle_add_token))
        // 按需取回明文密钥（对齐 new-api POST /api/token/:id/key）
        .route("/api/tokens/:id/key", get(api::admin::handle_get_token_key))
        .route("/api/tokens/:id", put(api::admin::handle_update_token))
        .route("/api/tokens/:id", delete(api::admin::handle_delete_token))
        .route(
            "/api/tokens/:id/reset_used",
            post(api::admin::handle_reset_token_used),
        )
        // 定价管理
        .route("/api/pricing", get(api::admin::handle_list_pricing))
        .route("/api/pricing", post(api::admin::handle_add_pricing))
        .route(
            "/api/pricing/:id",
            delete(api::admin::handle_delete_pricing),
        )
        .route(
            "/api/pricing/estimate",
            post(api::admin::handle_cost_estimate),
        )
        .route(
            "/api/pricing/exchange-rates",
            get(api::admin::handle_get_exchange_rates),
        )
        .route(
            "/api/pricing/exchange-rates",
            put(api::admin::handle_update_exchange_rates),
        )
        .route(
            "/api/pricing/sync",
            post(api::admin::handle_trigger_price_sync),
        )
        // P1 缺失模型检测：启用渠道模型 vs 定价目录差集
        .route(
            "/api/pricing/missing-models",
            get(api::admin::handle_missing_pricing_models),
        )
        // 统一模型管理（渠道模型 + 账号池模型聚合视图）
        .route("/api/models/manage", get(api::admin::handle_manage_view))
        .route("/api/models/state", post(api::admin::handle_set_state))
        // 模型测试（模型管理页「测试」按钮：钉 provider+model 走真实链路）
        .route("/api/models/test", post(api::admin::handle_model_test))
        .route(
            "/api/models/price",
            post(api::admin::handle_upsert_model_price),
        )
        .route(
            "/api/models/price/remove",
            post(api::admin::handle_remove_price),
        )
        .route("/api/models/alias", post(api::admin::handle_set_alias))
        .route(
            "/api/models/alias/state",
            post(api::admin::handle_set_alias_state),
        )
        .route(
            "/api/models/alias/remove",
            post(api::admin::handle_remove_alias),
        )
        .route(
            "/api/models/alias/reasoning",
            post(api::admin::handle_set_alias_reasoning),
        )
        .route(
            "/api/models/custom",
            post(api::admin::handle_add_custom_model),
        )
        .route(
            "/api/models/custom/remove",
            post(api::admin::handle_remove_custom_model),
        )
        // P1 模型元信息覆盖管理
        .route("/api/models/meta", get(api::admin::handle_model_meta_list))
        .route(
            "/api/models/meta/:model",
            put(api::admin::handle_model_meta_set),
        )
        .route(
            "/api/models/meta/:model",
            delete(api::admin::handle_model_meta_delete),
        )
        // 易支付配置
        .route("/api/epay/config", get(api::admin::handle_get_epay_config))
        // 用户侧充值页信息（对齐 new-api /api/user/topup/info）
        .route("/api/epay/info", get(api::admin::handle_get_epay_info))
        .route(
            "/api/epay/config",
            put(api::admin::handle_update_epay_config),
        )
        // 订单管理
        .route("/api/orders", get(api::admin::handle_list_orders))
        .route("/api/orders/me", get(api::admin::handle_my_orders))
        .route("/api/topup", post(api::admin::handle_topup_request))
        .route("/api/orders/:id", delete(api::admin::handle_delete_order))
        // 对齐 new-api：订单详情 / 手动补单 / 充值试算
        .route("/api/orders/:id", get(api::admin::handle_get_order))
        .route(
            "/api/orders/:id/complete",
            post(api::admin::handle_complete_order),
        )
        .route("/api/topup/amount", post(api::admin::handle_topup_amount))
        // 日志审计
        .route(
            "/api/logs/requests",
            get(api::admin::handle_list_request_logs),
        )
        .route("/api/logs/audits", get(api::admin::handle_list_audit_logs))
        .route(
            "/api/logs/requests/export",
            get(api::admin::handle_export_request_logs),
        )
        // 日志删除/清空（仅管理员）
        .route(
            "/api/logs/requests",
            delete(api::admin::handle_delete_request_logs),
        )
        .route(
            "/api/logs/requests/clear",
            delete(api::admin::handle_clear_request_logs),
        )
        .route(
            "/api/logs/audits",
            delete(api::admin::handle_delete_audit_logs),
        )
        .route(
            "/api/logs/audits/clear",
            delete(api::admin::handle_clear_audit_logs),
        )
        // 清空预览：先告诉管理员会删多少条，再让他确认
        .route(
            "/api/logs/requests/clear-preview",
            get(api::admin::handle_clear_preview),
        )
        // 在途请求（查看 + 终止）：RequestLog 只在结束后落库，
        // 这里让管理员能看到「谁在跑、跑了多久」并中断卡住的请求。
        .route("/api/logs/inflight", get(api::admin::handle_list_inflight))
        .route(
            "/api/logs/inflight/:id/terminate",
            post(api::admin::handle_terminate_inflight),
        )
        // 日志保留配置（运维设置：保留天数/容量上限/手动清理）
        .route(
            "/api/logs/retention",
            get(api::admin::handle_get_log_retention),
        )
        .route(
            "/api/logs/retention",
            put(api::admin::handle_update_log_retention),
        )
        .route("/api/logs/cleanup", post(api::admin::handle_cleanup_logs))
        // 日聚合手动重建（逃生门：聚合数字可疑时管理员一键重算历史）
        .route(
            "/api/logs/agg/rebuild",
            post(api::admin::handle_agg_rebuild),
        )
        // 数据看板
        .route(
            "/api/dashboard/consumption_trend",
            get(api::admin::handle_consumption_trend),
        )
        .route(
            "/api/dashboard/model_distribution",
            get(api::admin::handle_model_distribution),
        )
        .route(
            "/api/dashboard/user_ranking",
            get(api::admin::handle_user_ranking),
        )
        .route(
            "/api/dashboard/channel_health",
            get(api::admin::handle_channel_health),
        )
        .route("/api/dashboard/realtime", get(api::admin::handle_realtime))
        .route(
            "/api/dashboard/cache_savings",
            get(api::admin::handle_cache_savings),
        )
        .route(
            "/api/dashboard/scheduler_status",
            get(api::admin::handle_scheduler_status),
        )
        // 报表扩展（Agent2API report 视角：热力图/Top来源/Token趋势/总览卡）
        .route(
            "/api/dashboard/report/heatmap",
            get(api::admin::handle_report_heatmap),
        )
        .route(
            "/api/dashboard/report/top_sources",
            get(api::admin::handle_report_top_sources),
        )
        .route(
            "/api/dashboard/report/token_trend",
            get(api::admin::handle_report_token_trend),
        )
        .route(
            "/api/dashboard/report/overview",
            get(api::admin::handle_report_overview),
        )
        .route(
            "/api/dashboard/report/models",
            get(api::admin::handle_report_models),
        )
        .route(
            "/api/dashboard/report/cache_rates",
            get(api::admin::handle_report_cache_rates),
        )
        .route(
            "/api/dashboard/report/cache_trend_24h",
            get(api::admin::handle_report_cache_trend_24h),
        )
        .route(
            "/api/dashboard/report/daily_trend",
            get(api::admin::handle_report_daily_trend),
        )
        // 设置管理
        .route("/api/settings", get(api::admin::handle_get_settings))
        .route("/api/settings", put(api::admin::handle_update_settings))
        .route(
            "/api/oauth/config",
            get(api::admin::handle_get_oauth_config),
        )
        .route(
            "/api/oauth/config",
            put(api::admin::handle_update_oauth_config),
        )
        .route("/api/limits", get(api::admin::handle_get_limits))
        .route("/api/limits", put(api::admin::handle_update_limits))
        .route(
            "/api/agent/settings",
            get(api::admin::handle_get_agent_settings)
                .put(api::admin::handle_update_agent_settings),
        )
        .route(
            "/api/gateway/settings",
            get(api::admin::handle_get_gateway_settings)
                .put(api::admin::handle_update_gateway_settings),
        )
        // 安全策略（登录失败锁定 / 登录限流阈值，对照 Agent2API「设置 → 安全」）
        .route(
            "/api/security/policy",
            get(api::admin::handle_get_security_policy)
                .put(api::admin::handle_update_security_policy),
        )
        // 定时任务（对照 Agent2API「定时任务」页：间隔型任务的开关/间隔/立即执行）
        .route(
            "/api/scheduled-tasks",
            get(api::admin::scheduled_tasks::list_handler),
        )
        .route(
            "/api/scheduled-tasks/:id",
            patch(api::admin::scheduled_tasks::configure_handler)
                .post(api::admin::scheduled_tasks::configure_handler),
        )
        .route(
            "/api/scheduled-tasks/:id/run",
            post(api::admin::scheduled_tasks::run_handler),
        )
        // 调试模式：上游原始报文（对照 Agent2API「设置 → 网关 → 调试模式」）
        .route(
            "/api/debug/traffic",
            get(api::admin::handle_get_debug_traffic)
                .delete(api::admin::handle_clear_debug_traffic),
        )
        // models.dev 模型元数据（对照 Agent2API /api/models/metadata：开关 + 手动刷新）
        .route(
            "/api/models/metadata",
            get(api::admin::handle_get_model_metadata).put(api::admin::handle_put_model_metadata),
        )
        .route(
            "/api/models/metadata/refresh",
            post(api::admin::handle_refresh_model_metadata),
        )
        // 路由策略（对照 Agent2API「设置 → 路由」：策略/粘性/断路器/调度权重）
        .route(
            "/api/routing",
            get(api::admin::handle_get_routing).put(api::admin::handle_update_routing),
        )
        .route(
            "/api/routing/defaults",
            get(api::admin::handle_routing_defaults),
        )
        // 路由队列预演（对照 Agent2API「设置 → 路由 → 候选队列」：只读，
        // 不推进 round_robin 游标；round_robin 下实际首挑由游标决定）
        .route("/api/routing/queue", get(api::admin::handle_routing_queue))
        // 系统提示词注入（对照 Agent2API「设置 → 网关 → 系统提示词」）
        .route(
            "/api/gateway/prompt",
            get(api::admin::handle_get_gateway_prompt)
                .put(api::admin::handle_update_gateway_prompt),
        )
        // 额外脱敏请求头名单（对照 Agent2API「设置 → 网关 → 指纹脱敏」）
        .route(
            "/api/sanitize",
            get(api::admin::handle_get_sanitize).put(api::admin::handle_update_sanitize),
        )
        // Cline 伪装头覆盖表（对照 Agent2API「设置 → 网关 → Cline 伪装头」）
        .route(
            "/api/cline-headers",
            get(api::admin::handle_get_cline_headers).put(api::admin::handle_update_cline_headers),
        )
        // 数据管理（存储概览 + 全量备份/恢复；对照 Agent2API「设置 → 数据」）
        .route("/api/storage", get(api::admin::handle_storage_overview))
        .route("/api/backup/export", get(api::admin::handle_backup_export))
        .route("/api/backup/import", post(api::admin::handle_backup_import))
        // 通知配置
        .route(
            "/api/notify/config",
            get(api::admin::handle_get_notify_config),
        )
        .route(
            "/api/notify/config",
            put(api::admin::handle_update_notify_config),
        )
        .route(
            "/api/notify/test-telegram",
            post(api::admin::handle_test_telegram),
        )
        .route(
            "/api/notify/test-email",
            post(api::admin::handle_test_email),
        )
        .route(
            "/api/notify/test-slack",
            post(api::admin::handle_test_slack),
        )
        .route(
            "/api/notify/test-webhook",
            post(api::admin::handle_test_webhook),
        )
        // 告警规则
        .route(
            "/api/alerts/rules",
            get(api::admin::handle_alert_rules_list),
        )
        .route(
            "/api/alerts/rules",
            put(api::admin::handle_alert_rules_update),
        )
        .route("/api/alerts/active", get(api::admin::handle_alerts_active))
        .route(
            "/api/alerts/history",
            get(api::admin::handle_alerts_history),
        )
        .route("/api/alerts/test", post(api::admin::handle_alert_test))
        // 缓存管理
        .route("/api/cache/stats", get(api::admin::handle_cache_stats))
        .route("/api/cache/clear", post(api::admin::handle_cache_clear))
        // 安全监控
        .route(
            "/api/monitor/security",
            get(api::admin::handle_security_summary),
        )
        .route(
            "/api/monitor/security/events",
            get(api::admin::handle_security_events),
        )
        // 游乐场
        .route(
            "/api/playground/chat",
            post(api::admin::handle_playground_chat),
        )
        .route(
            "/api/playground/images",
            post(api::admin::handle_playground_images),
        )
        .route(
            "/api/playground/tts",
            post(api::admin::handle_playground_tts),
        )
        .route(
            "/api/playground/transcriptions",
            post(api::admin::handle_playground_transcriptions),
        )
        // 系统监控
        .route(
            "/api/monitor/system",
            get(api::admin::handle_monitor_system),
        )
        // 只读诊断端点套件（AI 可运维第①步：全部只读，管理面鉴权）
        .route(
            "/api/diagnostics/summary",
            get(api::admin::handle_diagnostics_summary),
        )
        .route(
            "/api/diagnostics/channels",
            get(api::admin::handle_diagnostics_channels),
        )
        .route(
            "/api/diagnostics/breakers",
            get(api::admin::handle_diagnostics_breakers),
        )
        // 内嵌 MCP server（AI 可运维第②步：Streamable HTTP，JSON-RPC 2.0 单响应；
        // initialize/tools/list/tools/call 三方法 + 11 工具白名单，管理面鉴权）
        .route("/api/mcp", post(api::admin::handle_mcp))
        // 管理面 API 自描述（OpenAPI 3.1，零依赖静态构造；管理面鉴权）
        .route("/api/openapi.json", get(api::admin::handle_openapi_v31))
        // 渠道探活
        .route(
            "/api/channels/fetch_models",
            post(api::admin::handle_fetch_channel_models),
        )
        .route(
            "/api/channels/chat_test",
            post(api::admin::handle_channel_chat_test),
        )
        .route(
            "/api/channels/:id/reset-circuit",
            post(api::admin::handle_reset_channel_circuit),
        )
        .route(
            "/api/channels/:id/health-archive",
            get(api::admin::handle_channel_health_archive),
        )
        .route(
            "/api/channels/:id/test",
            post(api::admin::handle_test_channel),
        )
        .route(
            "/api/channels/:id/balance",
            get(api::admin::handle_get_channel_balance),
        )
        // IP 过滤
        .route("/api/ip/filter", get(api::admin::handle_get_ip_filter))
        .route("/api/ip/filter", put(api::admin::handle_update_ip_filter))
        .route(
            "/api/ip/whitelist",
            post(api::admin::handle_add_ip_whitelist),
        )
        .route(
            "/api/ip/blacklist",
            post(api::admin::handle_add_ip_blacklist),
        )
        .route(
            "/api/ip/whitelist/:pattern",
            delete(api::admin::handle_remove_ip_whitelist),
        )
        .route(
            "/api/ip/blacklist/:pattern",
            delete(api::admin::handle_remove_ip_blacklist),
        )
        // 价格同步配置
        .route(
            "/api/pricing/sync-config",
            get(api::admin::handle_get_price_sync_config),
        )
        .route(
            "/api/pricing/sync-config",
            put(api::admin::handle_update_price_sync_config),
        )
        .route(
            "/api/channels/:id/patch",
            patch(api::admin::handle_patch_channel),
        )
        .route(
            "/api/user/epay/notify",
            post(api::admin::handle_epay_notify).get(api::admin::handle_epay_notify),
        )
        .route(
            "/api/user/epay/return",
            post(api::admin::handle_epay_return).get(api::admin::handle_epay_return),
        )
        .route("/api/stripe/topup", post(api::admin::handle_stripe_topup))
        .route(
            "/api/user/stripe/webhook",
            post(api::admin::handle_stripe_webhook),
        )
        // OpenAPI 文档
        .route(
            "/api-docs/openapi.json",
            get(api::admin::handle_openapi_json),
        )
        .route("/swagger-ui", get(api::admin::handle_swagger_ui))
        // 模型定价
        .route("/api/prices", get(api::admin::handle_list_prices))
        .route("/api/prices", post(api::admin::legacy::handle_upsert_price))
        .route(
            "/api/prices/:model",
            put(api::admin::handle_upsert_price_by_model),
        )
        .route(
            "/api/prices/:model",
            delete(api::admin::handle_delete_price),
        )
        // 用户分组
        .route("/api/groups", get(api::admin::handle_list_groups))
        .route("/api/groups", post(api::admin::handle_upsert_group))
        .route(
            "/api/groups/:name",
            put(api::admin::handle_upsert_group_by_name),
        )
        .route("/api/groups/:name", delete(api::admin::handle_delete_group))
        // 倍率配置
        .route("/api/ratios", get(api::admin::handle_get_ratios))
        .route("/api/ratios", put(api::admin::handle_update_ratios))
        // 兑换码
        .route("/api/redemptions", get(api::admin::handle_list_redemptions))
        .route(
            "/api/redemptions/batch",
            post(api::admin::handle_batch_redemptions),
        )
        .route(
            "/api/redemptions/:id",
            delete(api::admin::handle_delete_redemption),
        )
        .route("/api/redemptions/redeem", post(api::admin::handle_redeem))
        // 套餐管理（按量套餐：模板 CRUD + 按套餐发放 API Key）
        .route("/api/plans", get(api::admin::handle_list_plans))
        .route("/api/plans", post(api::admin::handle_upsert_plan))
        .route("/api/plans/:id", delete(api::admin::handle_delete_plan))
        .route(
            "/api/plans/:id/issue",
            post(api::admin::handle_issue_plan_key),
        )
        // 订阅（#85 时长订阅：用户侧购买 + 管理端绑定/取消）
        .route(
            "/api/subscription/plans",
            get(api::admin::handle_subscription_plans),
        )
        .route(
            "/api/subscription/self",
            get(api::admin::handle_subscription_self),
        )
        .route(
            "/api/subscription/balance/pay",
            post(api::admin::handle_subscription_balance_pay),
        )
        .route(
            "/api/subscription/admin/bind",
            post(api::admin::handle_subscription_admin_bind),
        )
        .route(
            "/api/subscription/admin/users/:id/subscriptions",
            get(api::admin::handle_subscription_admin_list),
        )
        .route(
            "/api/subscription/admin/subscriptions/:id/cancel",
            post(api::admin::handle_subscription_admin_cancel),
        )
        // 限流配置
        .route(
            "/api/ratelimit/config",
            get(api::admin::handle_get_ratelimit_config),
        )
        .route(
            "/api/ratelimit/config",
            put(api::admin::handle_update_ratelimit_config),
        )
        // 令牌增强
        .route("/api/tokens/today", get(api::admin::handle_tokens_today))
        .route("/api/usage/trend", get(api::admin::handle_usage_trend))
        .route("/api/usage/models", get(api::admin::handle_usage_models))
        .route("/api/usage/summary", get(api::admin::handle_usage_summary))
        .route("/api/usage/summary", post(api::admin::handle_refresh_usage))
        .route("/api/accounts", get(api::admin::handle_list_accounts))
        .route("/api/accounts", post(api::admin::handle_add_account))
        .route("/api/accounts/test", post(api::admin::handle_test_account))
        .route("/api/accounts/:id", put(api::admin::handle_update_account))
        .route(
            "/api/accounts/:id",
            delete(api::admin::handle_delete_account),
        )
        // 账号型渠道账号池（账号级调度）：列表/新增/批量导入/改/删/测试/重置冷却
        .route(
            "/api/account-sessions",
            get(api::admin::account_sessions::handle_list_account_sessions)
                .post(api::admin::account_sessions::handle_create_account_session),
        )
        .route(
            "/api/account-sessions/providers",
            get(api::admin::account_sessions::handle_list_account_providers),
        )
        .route(
            "/api/account-sessions/rotation",
            get(api::admin::account_sessions::handle_get_rotation_policy)
                .put(api::admin::account_sessions::handle_set_rotation_policy),
        )
        .route(
            "/api/account-sessions/stats",
            get(api::admin::account_sessions::handle_account_session_stats),
        )
        // 登录态供应商的网页登录（服务端无头登录 + 任务表）
        .route(
            "/api/account-sessions/login/start",
            post(api::admin::account_login::handle_login_start),
        )
        .route(
            "/api/account-sessions/login/wait",
            get(api::admin::account_login::handle_login_wait),
        )
        .route(
            "/api/account-sessions/login/cancel",
            post(api::admin::account_login::handle_login_cancel),
        )
        .route(
            "/api/account-sessions/login/callback",
            post(api::admin::account_login::handle_login_callback),
        )
        .route(
            "/api/account-sessions/login/sms/send",
            post(api::admin::account_login::handle_login_sms_send),
        )
        .route(
            "/api/account-sessions/login/sms/verify",
            post(api::admin::account_login::handle_login_sms_verify),
        )
        .route(
            "/api/account-sessions/login/paste",
            post(api::admin::account_login::handle_login_paste),
        )
        .route(
            "/api/account-sessions/bulk",
            post(api::admin::account_sessions::handle_bulk_import_account_sessions),
        )
        // 导出/导入 JSON 迁移闭环（导出具明文凭证，仅管理员）
        .route(
            "/api/account-sessions/export",
            get(api::admin::account_sessions::handle_export_account_sessions),
        )
        .route(
            "/api/account-sessions/import/json",
            post(api::admin::account_sessions::handle_import_account_sessions_json),
        )
        .route(
            "/api/account-sessions/:id",
            put(api::admin::account_sessions::handle_update_account_session)
                .patch(api::admin::account_sessions::handle_patch_account_session)
                .delete(api::admin::account_sessions::handle_delete_account_session),
        )
        .route(
            "/api/account-sessions/:id/test",
            post(api::admin::account_sessions::handle_test_account_session),
        )
        .route(
            "/api/account-sessions/:id/reset-cooldown",
            post(api::admin::account_sessions::handle_reset_account_session_cooldowns),
        )
        // 账号级余额/额度查询（qoder / autoclaw / 小浣熊 / zcode）
        .route(
            "/api/account-sessions/:id/balance",
            get(api::admin::account_sessions::handle_account_balance),
        )
        .route(
            "/api/account-sessions/:id/refresh",
            post(api::admin::account_sessions::handle_account_refresh),
        )
        // 上游模型目录发现（抄 Agent2API 的 provider 模型目录）
        .route(
            "/api/account-sessions/catalog/sync",
            post(api::admin::account_sessions::handle_sync_account_catalog),
        )
        .route(
            "/api/account-sessions/catalog/providers",
            get(api::admin::account_sessions::handle_catalog_providers),
        )
        // 从本机客户端（cc-switch）导入账号
        .route(
            "/api/account-sessions/import/sources",
            get(api::admin::account_sessions::handle_scan_import_sources),
        )
        .route(
            "/api/account-sessions/import",
            post(api::admin::account_sessions::handle_run_import),
        )
        // 从 Agent2API 账号库导入（SQLite 路径或记录 JSON；幂等更新）
        .route(
            "/api/account-sessions/import/agent2api",
            post(api::admin::account_sessions::handle_import_agent2api),
        )
        // 网络代理池（网络代理页）：CRUD / 启停 / 删除（引用检查）/ 连通性测试
        .route(
            "/api/proxy-pool",
            get(api::admin::proxy_pool_api::handle_pool_list)
                .post(api::admin::proxy_pool_api::handle_pool_create),
        )
        .route(
            "/api/proxy-pool/update",
            post(api::admin::proxy_pool_api::handle_pool_update),
        )
        .route(
            "/api/proxy-pool/enable",
            post(api::admin::proxy_pool_api::handle_pool_enable),
        )
        .route(
            "/api/proxy-pool/remove",
            post(api::admin::proxy_pool_api::handle_pool_remove),
        )
        .route(
            "/api/proxy-pool/test",
            post(api::admin::proxy_pool_api::handle_pool_test),
        )
        .route(
            "/api/proxy-pool/test-account",
            post(api::admin::proxy_pool_api::handle_pool_test_account),
        )
        // 服务器自动更新（检查/应用/重启/回滚/设置）
        .route(
            "/api/update/status",
            get(api::admin::update::handle_update_status),
        )
        .route(
            "/api/update/check",
            post(api::admin::update::handle_update_check),
        )
        .route(
            "/api/update/apply",
            post(api::admin::update::handle_update_apply),
        )
        .route(
            "/api/update/restart",
            post(api::admin::update::handle_update_restart),
        )
        .route(
            "/api/update/rollback",
            post(api::admin::update::handle_update_rollback),
        )
        .route(
            "/api/update/settings",
            get(api::admin::update::handle_get_update_settings)
                .put(api::admin::update::handle_put_update_settings),
        )
        .route("/api/keys", get(api::admin::handle_list_keys))
        .route("/api/keys", post(api::admin::handle_add_key))
        .route("/api/keys/:id", delete(api::admin::handle_delete_key))
        .route(
            "/api/tokens/:id/rotate",
            post(api::admin::handle_rotate_token),
        )
        // 网络层管理路由（新增 AIGX Network Layer）
        .route("/api/network/status", get(api::admin::health_check))
        .route(
            "/api/network/config/:config_id",
            put(api::admin::update_network_config),
        )
        .route("/api/network/restart", post(api::admin::restart_network))
        .route(
            "/api/network/accounts/:account_id",
            post(api::admin::add_network_account),
        )
        .route(
            "/api/network/accounts/:account_id",
            delete(api::admin::remove_network_account),
        )
        .route(
            "/api/network/accounts",
            get(api::admin::list_network_accounts),
        );

    // EasyPay 回调（异步通知 + 同步跳转）— 无需鉴权
    let epay_callback_routes = Router::new();

    // Stripe 回调
    let stripe_callback_routes = Router::new();

    // OpenAI 兼容 API 路由
    let openai_routes = Router::new()
        .route(
            "/v1/chat/completions",
            post(api::openai::handle_chat_completions),
        )
        .route("/v1/responses", post(api::openai::handle_responses))
        .route("/v1/completions", post(api::openai::handle_completions))
        .route("/v1/embeddings", post(api::openai::handle_embeddings))
        .route("/v1/rerank", post(api::openai::handle_rerank))
        .route(
            "/v1/images/generations",
            post(api::openai::handle_images_generations),
        )
        .route(
            "/v1/audio/transcriptions",
            post(api::openai::handle_audio_transcriptions),
        )
        .route(
            "/v1/audio/translations",
            post(api::openai::handle_audio_translations),
        )
        .route("/v1/audio/speech", post(api::openai::handle_audio_speech))
        .route("/v1/models", get(api::openai::handle_list_models))
        .route("/v1/models/:model", get(api::openai::handle_get_model))
        // 无前缀兼容：客户端 base_url 填根地址（http://host:9527）时
        // 请求 /chat/completions 等无 /v1 路径 —— new-api 同样双挂，避免 405。
        .route(
            "/chat/completions",
            post(api::openai::handle_chat_completions),
        )
        .route("/responses", post(api::openai::handle_responses))
        .route("/completions", post(api::openai::handle_completions))
        .route("/embeddings", post(api::openai::handle_embeddings))
        .route("/rerank", post(api::openai::handle_rerank))
        .route(
            "/images/generations",
            post(api::openai::handle_images_generations),
        )
        .route("/models", get(api::openai::handle_list_models))
        .route("/models/:model", get(api::openai::handle_get_model));

    // Anthropic 兼容 API 路由（同样双挂无前缀 /messages）
    let anthropic_routes = Router::new()
        .route("/v1/messages", post(api::anthropic::handle_messages))
        .route("/messages", post(api::anthropic::handle_messages));

    Router::new()
        .merge(admin_routes)
        .merge(epay_callback_routes)
        .merge(stripe_callback_routes)
        .merge(openai_routes)
        .merge(anthropic_routes)
        .merge(crate::agent::api::router())
        .route("/livez", get(handle_livez))
        .route("/readyz", get(handle_readyz))
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .fallback_service(web::serve_static_files())
        .layer(build_cors_layer(config))
        .layer(compression_layer())
        .layer(body_limit_mw)
        .with_state(state)
}

/// 根据配置构建受限 CORS 层。
///
/// - 若 `config.cors_origins` 非空：仅允许列表中的来源（生产环境推荐）。
/// - 若 `config.cors_origins` 为空但 `config.server_address` 非空：允许 server_address 来源。
/// - 否则：默认允许 localhost 开发来源（127.0.0.1 与 localhost，常见端口）。
///
/// 不再使用 `CorsLayer::permissive()` 以避免任意来源带来的安全风险。
fn build_cors_layer(config: &config::AppConfig) -> CorsLayer {
    use axum::http::HeaderValue;

    let origins: Vec<HeaderValue> = if !config.cors_origins.is_empty() {
        config
            .cors_origins
            .iter()
            .filter_map(|s| s.trim().parse::<HeaderValue>().ok())
            .collect()
    } else if !config.server_address.trim().is_empty() {
        // 兜底：使用 server_address 作为唯一允许来源
        config
            .server_address
            .trim()
            .parse::<HeaderValue>()
            .into_iter()
            .collect()
    } else {
        // 无配置：不发 CORS 头（AllowOrigin::list 为空数组 → 不匹配任何外部来源），
        // 浏览器同源请求不受影响；跨源请求将被浏览器拦截。
        // 对外暴露时务必在 config 中显式配置 `cors_origins`。
        Vec::new()
    };

    if origins.is_empty() {
        tracing::warn!(
            "No valid CORS origins configured; falling back to localhost-only. \
             Set `cors_origins` in config for production."
        );
    }

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
            axum::http::header::ACCEPT,
        ])
        .allow_credentials(true)
}

/// gzip 压缩中间件：对可压缩响应（JSON/HTML/CSS/JS/文本）做 gzip。
///
/// 公开提示词源单次可达 1.1MB JSON，压缩后约 300KB，显著降低带宽；
/// SSE 流式响应（text/event-stream）不含 `Content-Length` 时不做压缩，
/// 保持流式透传。静态文件已由 `ServeDir` 按预压缩/按需处理，此处只
/// 兜底 API 响应与 SPA index.html。
fn compression_layer() -> CompressionLayer<
    tower_http::compression::predicate::And<
        tower_http::compression::DefaultPredicate,
        tower_http::compression::predicate::SizeAbove,
    >,
> {
    use tower_http::compression::predicate::{DefaultPredicate, SizeAbove};
    use tower_http::compression::Predicate;

    // 必须用 `.and()` 组合 DefaultPredicate 与 SizeAbove：DefaultPredicate
    // 按 Content-Type 排除 text/event-stream（SSE 流式透传不能被 gzip 破坏），
    // SizeAbove 跳过小于 256 字节的响应（错误 JSON、探针等，压缩收益为负）。
    // 若只用 SizeAbove 替换默认谓词，会丢失 event-stream 排除，把 SSE 流也压坏。
    CompressionLayer::new().compress_when(DefaultPredicate::new().and(SizeAbove::new(256)))
}

/// GET /metrics — Prometheus 指标文本输出（管理员鉴权）
///
/// 指标含各渠道模型调用量/成本/延迟等业务敏感数据，不设防会向任意
/// 未认证访客泄露运营概况。校验管理员会话，失败返回 401。
async fn handle_metrics(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::http::{header::CONTENT_TYPE, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    if crate::api::admin::common::verify_admin(&state, &headers)
        .await
        .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            )],
            "unauthorized",
        )
            .into_response();
    }
    let headers = [(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    )];
    (StatusCode::OK, headers, metrics::global().render()).into_response()
}

/// GET /livez — 存活检查
async fn handle_livez(State(state): State<AppState>) -> axum::response::Response {
    health::livez_response(&state.livez_state, false)
}

/// GET /readyz — 就绪检查
async fn handle_readyz(State(state): State<AppState>) -> axum::response::Response {
    health::readyz_response(&state.livez_state, true, false)
}

/// GET /health — 模型健康状态汇总
async fn handle_health(State(state): State<AppState>) -> axum::Json<serde_json::Value> {
    let models = state.health_tracker.all_health();
    let mut result = serde_json::Map::new();
    for (model, level) in models {
        result.insert(model, serde_json::json!(u8::from(level)));
    }
    axum::Json(serde_json::Value::Object(result))
}
