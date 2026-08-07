use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures::StreamExt;
use serde_json::Value;
use std::convert::Infallible;
use std::sync::Arc;

use crate::account::AccountPool;
use crate::bridge::{
    Bridge, BridgeContext, ChatFormat, ChatMessage, EmbeddingRequest, FinishReason, Role,
};
use crate::config::ConfigManager;
use crate::hub::Hub;
use crate::model::ModelMapper;
use crate::payment::order_store::OrderStore;
use crate::proxy::CfApiClient;
use crate::usage::UsageTracker;
use crate::user::UserStore;
use crate::health::{HealthTracker, LivezState};

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
    pub health_tracker: Arc<HealthTracker>,
    pub livez_state: Arc<LivezState>,
}
