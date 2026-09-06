// lib+bin 双 target 共享 src/*.rs：lib 将模块 pub 导出（视为对外 API 不报 dead），
// bin 以私有 mod 引入同一批文件，内部未直接调用的预留 API 会被 dead_code 误报。
// 此处统一豁免 bin 视角的结构性 dead_code，lib 视角的警告已单独清理干净。
#![allow(dead_code)]

mod account;
mod api;
mod bridge;
mod cache;
mod channel;
mod config;
mod error_translate;
mod graphql;
mod health;
mod hub;
mod ip;
mod log;
mod metrics;
mod model;
mod monitor;
mod notify;
mod oauth;
mod payment;
mod pricing;
mod proxy;
mod quota_monitor;
mod ratelimit;
mod redemption;
mod semantic;
mod sse;
mod storage;
mod token_estimate;
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

    // 初始化存储（默认 SQLite 后端；--no-default-features 构建降级为 JSON 文件）
    let data_dir = crate::config::expand_path(&config.server.data_dir);
    tokio::fs::create_dir_all(&data_dir).await?;
    #[cfg(feature = "sqlite-kv")]
    let store = Arc::new(FileStore::open(data_dir.join("data"))?);
    #[cfg(not(feature = "sqlite-kv"))]
    let store = Arc::new(FileStore::new(data_dir.join("data")));

    // 初始化账号池
    let account_pool = Arc::new(AccountPool::new(store.clone()));

    // 初始化模型映射
    let model_mapper = Arc::new(ModelMapper::new(store.clone()));
    if let Err(e) = model_mapper.load() {
        tracing::error!("Failed to load model mapper: {e}");
    }

    // 初始化用量追踪
    let usage_tracker = Arc::new(UsageTracker::new(store.clone(), account_pool.clone()));

    // 初始化 API 密钥存储
    let api_key_store = Arc::new(ApiKeyStore::new(store.clone()));
    if let Err(e) = api_key_store.load() {
        tracing::error!("Failed to load API key store: {e}");
    }

    // 初始化用户系统
    let user_store = Arc::new(UserStore::new(store.clone()));

    // 确保默认管理员账户存在
    ensure_default_admin(&user_store);

    // 初始化订单存储
    let order_store = Arc::new(OrderStore::new(store.clone()));

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

    // 初始化兑换码存储（功能 2）
    let redemption_store = Arc::new(RedemptionStore::new(store.clone()));

    // 初始化限流器（功能 3，带持久化配置）
    let rate_limiter = Arc::new(RateLimiter::with_store(store.clone()));

    // 初始化通知服务（Telegram + SMTP + Slack + Webhook）
    let notify_service = Arc::new(NotifyService::new(config.notify.clone()));

    // 初始化告警规则评估器（持久化：FileStore 加载规则集与历史）
    let alert_evaluator = notify::alert_patrol::shared_evaluator_persistent(&store);

    // 共享 HTTP 客户端（性能热点 H5/H6）。
    // 全应用复用同一个 reqwest::Client，避免每次请求新建客户端（连接池/TLS 握手开销）。
    // 超时 300s 覆盖大多数上游推理时长；连接池由 reqwest 内部管理。
    let http_client = Arc::new(
        reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .expect("failed to build shared reqwest::Client"),
    );

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

    // 登录失败锁定状态：<ip> → 连续失败次数（TTL=5 分钟，连续失败 ≥5 次锁 5 分钟）
    let login_failures = Arc::new(
        crate::cache::AsyncCache::<String, u32>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

    // 邮箱验证码登录：<email> → 6 位验证码（TTL=5 分钟）
    let login_code_cache = Arc::new(
        crate::cache::AsyncCache::<String, String>::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(300))
            .build(),
    );

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
                        config.database.url,
                        manager.backend().as_str()
                    );
                    Some(manager.connection().clone())
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to connect SeaORM backend ({}), falling back to FileStore: {}",
                        config.database.url,
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

    let state = AppState {
        api_client,
        model_mapper,
        usage_tracker,
        account_pool,
        api_key_store,
        config_manager: config_manager.clone(),
        hub,
        user_store,
        order_store,
        epay_client,
        stripe_client,
        health_tracker: health_tracker.clone(),
        livez_state: livez_state.clone(),
        channel_store,
        pricing_store,
        user_group_store,
        log_store,
        redemption_store,
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
        http_client,
        response_cache,
        semantic_router,
        #[cfg(feature = "sea-orm")]
        db_conn,
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
    channel::prober::spawn_channel_prober(
        state.channel_store.clone(),
        state.http_client.as_ref().clone(),
    );

    let app = build_router(state, &config);

    // 启动服务器
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("Server listening on {}", addr);

    // 优雅关闭：收到 SIGTERM/SIGINT 后标记 draining，等待现有请求完成
    let livez = livez_state.clone();
    axum::serve(listener, app)
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
fn ensure_default_admin(user_store: &UserStore) {
    const DEFAULT_ADMIN_EMAIL: &str = "admin@gmail.com";
    const DEFAULT_ADMIN_USERNAME: &str = "admin";
    const DEFAULT_ADMIN_PASSWORD: &str = "123456";

    // 检查是否已存在任意 Admin 角色用户；若已存在则跳过，绝不删除/重建
    let has_admin = user_store.list().iter().any(|u| u.role == Role::Admin);
    if has_admin {
        tracing::info!("Admin account already exists, skip default admin initialization");
        return;
    }

    // 创建写死凭据的内置管理员账户
    match user_store.create_with_username(
        DEFAULT_ADMIN_EMAIL,
        DEFAULT_ADMIN_USERNAME,
        DEFAULT_ADMIN_PASSWORD,
        Role::Admin,
        0,
    ) {
        Ok(_) => {
            tracing::warn!(
                "First-time setup: built-in admin account created. \
                 email={} | username={} | password={} \
                 — PLEASE LOGIN AND CHANGE THE PASSWORD IMMEDIATELY.",
                DEFAULT_ADMIN_EMAIL,
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
    // 注意：axum 0.7（matchit 0.7）的路由参数语法是 `:id`，不是 `{id}`。
    // `{id}` 是 axum 0.8（matchit 0.8）的语法，在 0.7 下会被当作字面量路径段，
    // 导致所有带参数路由匹配失败、请求掉到 fallback_service 返回 405/404。
    // 管理 API 路由（模块化）
    let admin_routes = Router::new()
        // Auth handlers (来自 api::admin::auth)
        .route("/api/auth/login", post(api::admin::handle_login))
        .route(
            "/api/auth/login/send-code",
            post(api::admin::handle_login_send_code),
        )
        .route(
            "/api/auth/login/code",
            post(api::admin::handle_login_with_code),
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
        // 用户管理
        .route("/api/users", get(api::admin::handle_list_users))
        .route("/api/users", post(api::admin::handle_create_user))
        .route("/api/users/check", get(api::admin::handle_check_username))
        .route("/api/users/me", get(api::admin::handle_me))
        .route("/api/users/:id", put(api::admin::handle_update_user))
        .route("/api/users/:id", delete(api::admin::handle_delete_user))
        // 渠道管理
        .route("/api/channels", get(api::admin::handle_list_channels))
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
        // 易支付配置
        .route("/api/epay/config", get(api::admin::handle_get_epay_config))
        .route(
            "/api/epay/config",
            put(api::admin::handle_update_epay_config),
        )
        // 订单管理
        .route("/api/orders", get(api::admin::handle_list_orders))
        .route("/api/orders/me", get(api::admin::handle_my_orders))
        .route("/api/topup", post(api::admin::handle_topup_request))
        .route("/api/orders/:id", delete(api::admin::handle_delete_order))
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
        // 系统监控
        .route(
            "/api/monitor/system",
            get(api::admin::handle_monitor_system),
        )
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
            "/api/channels/:id/test",
            post(api::admin::handle_test_channel),
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
        .route("/api/prices", post(api::admin::handle_upsert_price))
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
        .route("/livez", get(handle_livez))
        .route("/readyz", get(handle_readyz))
        .route("/health", get(handle_health))
        .route("/metrics", get(handle_metrics))
        .fallback_service(web::serve_static_files())
        .layer(build_cors_layer(config))
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

/// GET /metrics — Prometheus 指标文本输出
async fn handle_metrics() -> axum::response::Response {
    use axum::http::{header::CONTENT_TYPE, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
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
