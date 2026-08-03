mod account;
mod api;
mod bridge;
mod config;
mod graphql;
mod hub;
mod model;
mod payment;
mod proxy;
mod storage;
mod usage;
mod user;
mod web;

use std::sync::Arc;

use axum::routing::{get, post, put, delete};
use axum::Router;
use tower_http::cors::CorsLayer;

use api::auth::ApiKeyStore;
use api::openai::AppState;
use bridge::cf::CloudflareBridge;
use config::ConfigManager;
use storage::FileStore;
use account::AccountPool;
use hub::Hub;
use model::ModelMapper;
use payment::order_store::OrderStore;
use payment::EpayClient;
use proxy::CfApiClient;
use usage::UsageTracker;
use user::UserStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // 加载配置
    let config_manager = Arc::new(ConfigManager::new(None).await);
    let config = config_manager.get().await;

    // 初始化存储
    let data_dir = crate::config::expand_path(&config.server.data_dir);
    tokio::fs::create_dir_all(&data_dir).await?;
    let store = Arc::new(FileStore::new(data_dir.join("data")));

    // 初始化账号池
    let account_pool = Arc::new(AccountPool::new(store.clone()));

    // 初始化模型映射
    let model_mapper = Arc::new(ModelMapper::new(store.clone()));
    let _ = model_mapper.load();

    // 初始化用量追踪
    let usage_tracker = Arc::new(UsageTracker::new(store.clone(), account_pool.clone()));

    // 初始化 API 密钥存储
    let api_key_store = Arc::new(ApiKeyStore::new(store.clone()));
    let _ = api_key_store.load();

    // 初始化用户系统
    let user_store = Arc::new(UserStore::new(store.clone()));
    // 首次启动若不存在任何用户，则用旧 admin 密码哈希迁移，或保持空（仍可单用户模式登录）
    if user_store.list().is_empty() && !config.admin.password.is_empty() {
        let _ = user_store.create_with_username("admin", "admin", &config.admin.password, user::Role::Admin, 0);
    }

    // 初始化订单存储
    let order_store = Arc::new(OrderStore::new(store.clone()));

    // 初始化易支付客户端（运行时按配置即时构造，无需常驻）
    let epay_client = Arc::new(EpayClient::new(config.epay.clone()));

    // 初始化 CF API 客户端
    let api_client = Arc::new(CfApiClient::new(
        account_pool.clone(),
        model_mapper.clone(),
    ));

    // 初始化 Hub 并注册 Cloudflare Bridge
    let hub = Arc::new(Hub::new());
    let cf_bridge = Arc::new(CloudflareBridge::new(
        api_client.clone(),
        model_mapper.clone(),
    ));
    hub.register_specialized("cloudflare", cf_bridge);

    // 注册 OpenAI 适配器族 bridge（预留）
    // hub.register_family(Adapter::Openai, cf_openai_bridge);

    // 构建应用状态
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
    };

    tracing::info!(
        "Configuration loaded: {}:{} | Accounts: {} | API Keys: {} | Users: {} | EpayReady: {}",
        config.server.host,
        config.server.port,
        state.account_pool.list().len(),
        state.api_key_store.len(),
        state.user_store.list().len(),
        state.epay_client.config().ready(),
    );

    // 构建路由
    let app = build_router(state);

    // 启动服务器
    let addr = format!("{}:{}", config.server.host, config.server.port);
    tracing::info!("Starting server at {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

fn build_router(state: AppState) -> Router {
    // 管理 API 路由
    let admin_routes = Router::new()
        .route("/api/auth/login", post(api::admin::handle_login))
        .route("/api/auth/register", post(api::admin::handle_register))
        .route("/api/auth/logout", post(api::admin::handle_logout))
        .route("/api/usage/summary", get(api::admin::handle_usage_summary))
        .route("/api/usage/summary", post(api::admin::handle_refresh_usage))
        .route("/api/accounts", get(api::admin::handle_list_accounts))
        .route("/api/accounts", post(api::admin::handle_add_account))
        .route("/api/accounts/test", post(api::admin::handle_test_account))
        .route("/api/accounts/{id}", put(api::admin::handle_update_account))
        .route("/api/accounts/{id}", delete(api::admin::handle_delete_account))
        .route("/api/keys", get(api::admin::handle_list_keys))
        .route("/api/keys", post(api::admin::handle_add_key))
        .route("/api/keys/{id}", delete(api::admin::handle_delete_key))
        .route("/api/settings", get(api::admin::handle_get_settings))
        .route("/api/settings", put(api::admin::handle_update_settings))
        .route("/api/limits", get(api::admin::handle_get_limits))
        .route("/api/limits", put(api::admin::handle_update_limits))
        .route("/api/tokens/today", get(api::admin::handle_tokens_today))
        .route("/api/usage/trend", get(api::admin::handle_usage_trend))
        .route("/api/usage/models", get(api::admin::handle_usage_models))
        // 用户管理
        .route("/api/users", get(api::admin::handle_list_users))
        .route("/api/users", post(api::admin::handle_create_user))
        .route("/api/users/{id}", put(api::admin::handle_update_user))
        .route("/api/users/{id}", delete(api::admin::handle_delete_user))
        .route("/api/users/me", get(api::admin::handle_me))
        // 易支付配置
        .route("/api/epay/config", get(api::admin::handle_get_epay_config))
        .route("/api/epay/config", put(api::admin::handle_update_epay_config))
        // 订单查询
        .route("/api/orders", get(api::admin::handle_list_orders))
        .route("/api/orders/me", get(api::admin::handle_my_orders))
        // 充值下单
        .route("/api/topup", post(api::admin::handle_topup_request));

    // 易支付回调（异步通知 + 同步跳转）— 无需鉴权
    let epay_callback_routes = Router::new()
        .route("/api/user/epay/notify", post(api::admin::handle_epay_notify).get(api::admin::handle_epay_notify))
        .route("/api/user/epay/return", post(api::admin::handle_epay_return).get(api::admin::handle_epay_return));

    // OpenAI 兼容 API 路由
    let openai_routes = Router::new()
        .route("/v1/chat/completions", post(api::openai::handle_chat_completions))
        .route("/v1/completions", post(api::openai::handle_completions))
        .route("/v1/embeddings", post(api::openai::handle_embeddings))
        .route("/v1/images/generations", post(api::openai::handle_images_generations))
        .route("/v1/audio/transcriptions", post(api::openai::handle_audio_transcriptions))
        .route("/v1/audio/translations", post(api::openai::handle_audio_translations))
        .route("/v1/audio/speech", post(api::openai::handle_audio_speech))
        .route("/v1/models", get(api::openai::handle_list_models))
        .route("/v1/models/{model}", get(api::openai::handle_get_model));

    Router::new()
        .merge(admin_routes)
        .merge(epay_callback_routes)
        .merge(openai_routes)
        .fallback_service(web::serve_static_files())
        .layer(CorsLayer::permissive())
        .with_state(state)
}