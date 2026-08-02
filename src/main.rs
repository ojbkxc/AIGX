mod account;
mod api;
mod bridge;
mod config;
mod graphql;
mod hub;
mod model;
mod proxy;
mod storage;
mod usage;
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
use proxy::CfApiClient;
use usage::UsageTracker;

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
    };

    tracing::info!(
        "Configuration loaded: {}:{} | Accounts: {} | API Keys: {}",
        config.server.host,
        config.server.port,
        state.account_pool.list().len(),
        state.api_key_store.len(),
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
        .route("/api/usage/models", get(api::admin::handle_usage_models));

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
        .merge(openai_routes)
        .fallback_service(web::serve_static_files())
        .layer(CorsLayer::permissive())
        .with_state(state)
}