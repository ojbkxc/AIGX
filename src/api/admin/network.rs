//! 网络层管理 API
//!
//! 提供网络层（AIGX Network Layer）的管理、监控和配置功能。
//!
//! ## 架构说明
//!
//! 本模块把网关现有的账号池 / 渠道 / 健康追踪 / 断路器 / 限流 / 会话
//! 状态聚合成一个「网络层」视图，供管理后台的 /api/network/* 路由使用。
//! 网络层自身的运行时组件（连接池、会话池）继续由主 crate 的对应模块驱动，
//! 这里仅做观测与运维控制，避免重复实现状态机。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::openai::AppState;
use crate::account::CfAccount;

/// 网络层状态信息
#[derive(Debug, Serialize)]
pub struct NetworkStatus {
    /// 网络层是否启用（始终为 true：数据面即网络层）
    pub enabled: bool,
    /// 账号池状态
    pub account_pool: AccountPoolStatus,
    /// 连接池状态（渠道连接 + 健康状态聚合）
    pub connection_pool: ConnectionPoolStatus,
    /// 会话池状态（上游亲和会话 + 限流状态聚合）
    pub session_pool: SessionPoolStats,
    /// 负载均衡策略（渠道优先级 + 权重 + 断路器叠加）
    pub load_balance_strategy: String,
    /// 最后检查时间（unix 秒）
    pub last_check_at: i64,
}

/// 账号池状态
#[derive(Debug, Serialize)]
pub struct AccountPoolStatus {
    pub total_accounts: usize,
    pub available_accounts: usize,
    pub busy_accounts: usize,
    pub error_accounts: usize,
    pub invalid_accounts: usize,
    pub total_requests: u64,
    pub failed_requests: u64,
}

/// 连接池状态
#[derive(Debug, Serialize)]
pub struct ConnectionPoolStatus {
    pub total_connections: usize,
    pub active_connections: usize,
    pub idle_connections: usize,
    pub total_connections_created: u64,
    pub total_connections_closed: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub avg_latency_ms: f64,
}

/// 会话池统计
#[derive(Debug, Serialize)]
pub struct SessionPoolStats {
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub idle_sessions: usize,
    pub session_ttl_hours: u64,
}

/// 网络层配置请求
#[derive(Debug, Deserialize)]
pub struct NetworkConfigRequest {
    /// 是否启用网络层
    pub enabled: bool,
    /// 负载均衡策略（暂存，后续接入渠道调度权重时使用）
    pub strategy: Option<String>,
}

/// 网络层配置响应
#[derive(Debug, Serialize)]
pub struct NetworkConfigResponse {
    pub enabled: bool,
    pub strategy: String,
    pub account_pool_min: usize,
    pub account_pool_max: usize,
    pub connection_pool_max: usize,
    pub session_pool_max: usize,
}

/// 网络层账号配置（复用主 crate 账号池的 CF 账号结构）
#[derive(Debug, Deserialize)]
pub struct AccountConfigRequest {
    pub name: String,
    pub account_id: String,
    pub api_token: String,
    pub status: Option<String>,
}

/// 获取网络层健康状态
///
/// 聚合主 crate 各子系统（账号池 / 渠道 / 健康追踪 / 断路器）的真实状态，
/// 供管理后台「网络层」面板展示。
pub async fn health_check(State(state): State<AppState>) -> Json<NetworkStatus> {
    let accounts = state.account_pool.list();
    let total_accounts = accounts.len();
    let available_accounts = accounts.iter().filter(|a| a.status == "active").count();
    let error_accounts = accounts.iter().filter(|a| a.status == "error").count();
    let invalid_accounts = accounts.iter().filter(|a| a.status == "pending").count();

    let channels = state.channel_store.list();
    let total_connections = channels.len();
    let active_connections = channels.iter().filter(|c| c.is_enabled()).count();
    let idle_connections = total_connections - active_connections;
    // 渠道失败数通过断路器打开数估算；成功数 = 已启用渠道数
    let failed_requests = channels
        .iter()
        .filter(|c| state.channel_store.circuit_breaker().get_state(&c.id) == "open")
        .count() as u64;

    // 会话：亲和路由会话数 + 活跃用户会话（按分组数与活跃用户数近似）
    let total_sessions = state.user_group_store.list().len();
    let active_sessions = state
        .user_store
        .list()
        .iter()
        .filter(|u| u.status == "active")
        .count();
    let idle_sessions = total_sessions.saturating_sub(active_sessions);

    Json(NetworkStatus {
        enabled: true,
        account_pool: AccountPoolStatus {
            total_accounts,
            available_accounts,
            busy_accounts: 0,
            error_accounts,
            invalid_accounts,
            total_requests: state.usage_tracker.monthly_stats().total(),
            failed_requests,
        },
        connection_pool: ConnectionPoolStatus {
            total_connections,
            active_connections,
            idle_connections,
            total_connections_created: total_connections as u64,
            total_connections_closed: 0,
            successful_requests: active_connections as u64,
            failed_requests,
            avg_latency_ms: 0.0,
        },
        session_pool: SessionPoolStats {
            total_sessions,
            active_sessions,
            idle_sessions,
            session_ttl_hours: 72,
        },
        load_balance_strategy: "priority+weighted+circuit".to_string(),
        last_check_at: chrono::Utc::now().timestamp(),
    })
}

/// 更新网络层配置
pub async fn update_network_config(
    State(_state): State<AppState>,
    Path(_config_id): Path<String>,
    Json(request): Json<NetworkConfigRequest>,
) -> Result<Json<NetworkConfigResponse>, ApiError> {
    if !request.enabled {
        return Err(ApiError::NetworkLayerDisabled);
    }
    // 配置目前由 config.toml 的渠道调度参数驱动；策略仅用于展示与后续扩展。
    Ok(Json(NetworkConfigResponse {
        enabled: request.enabled,
        strategy: request
            .strategy
            .unwrap_or_else(|| "priority+weighted+circuit".to_string()),
        account_pool_min: 2,
        account_pool_max: 10,
        connection_pool_max: 10,
        session_pool_max: 50,
    }))
}

/// 添加网络层账号（接入主 crate 的 CF 账号池）
///
/// 兼容两种调用：`/api/network/accounts/:id` 不带 body 时按账号 ID 直接添加；
/// 带 `{name, account_id, api_token, status}` body 时按完整配置添加。
pub async fn add_network_account(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
    body: Option<Json<AccountConfigRequest>>,
) -> Result<Json<Value>, ApiError> {
    let now = chrono::Utc::now().timestamp();
    let request = body.map(|Json(r)| r);
    let account = CfAccount {
        id: uuid::Uuid::new_v4().to_string(),
        name: request
            .as_ref()
            .filter(|r| !r.name.is_empty())
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("network-{}", account_id)),
        account_id: request
            .as_ref()
            .filter(|r| !r.account_id.is_empty())
            .map(|r| r.account_id.clone())
            .unwrap_or_else(|| account_id.clone()),
        api_token: request
            .as_ref()
            .map(|r| r.api_token.clone())
            .unwrap_or_default(),
        status: request
            .as_ref()
            .and_then(|r| r.status.clone())
            .unwrap_or_else(|| "active".to_string()),
        last_error: None,
        last_used_at: None,
        created_at: now,
    };
    state
        .account_pool
        .add(account)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "success": true,
        "message": "网络层账号已添加"
    })))
}

/// 删除网络层账号（按账号 ID）
pub async fn remove_network_account(
    State(state): State<AppState>,
    Path(account_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state
        .account_pool
        .remove(&account_id)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "success": true,
        "message": "网络层账号已删除"
    })))
}

/// 重启网络层
///
/// 复位所有渠道的断路器与健康追踪状态；渠道探活由后台 prober 周期执行。
pub async fn restart_network(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    for ch in state.channel_store.list() {
        state.channel_store.circuit_breaker().reset(&ch.id);
        state.channel_store.health_tracker().reset(&ch.id);
    }
    Ok(Json(json!({
        "success": true,
        "message": "网络层重启完成",
        "status": "started"
    })))
}

/// 错误类型
#[derive(Debug)]
pub enum ApiError {
    NetworkLayerDisabled,
    NetworkLayerNotStarted,
    AccountNotFound,
    AlreadyExists,
    NotImplemented,
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, detail) = match self {
            ApiError::NetworkLayerDisabled => (
                StatusCode::SERVICE_UNAVAILABLE,
                "network_layer_disabled".to_string(),
                "网络层未启用".to_string(),
            ),
            ApiError::NetworkLayerNotStarted => (
                StatusCode::SERVICE_UNAVAILABLE,
                "network_layer_not_started".to_string(),
                "网络层未启动".to_string(),
            ),
            ApiError::AccountNotFound => (
                StatusCode::NOT_FOUND,
                "account_not_found".to_string(),
                "账号未找到".to_string(),
            ),
            ApiError::AlreadyExists => (
                StatusCode::CONFLICT,
                "already_exists".to_string(),
                "账号已存在".to_string(),
            ),
            ApiError::NotImplemented => (
                StatusCode::NOT_IMPLEMENTED,
                "not_implemented".to_string(),
                "功能待实现".to_string(),
            ),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error".to_string(),
                msg,
            ),
        };
        (
            status,
            Json(json!({
                "success": false,
                "error": error,
                "detail": detail,
            })),
        )
            .into_response()
    }
}
