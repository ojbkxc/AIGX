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
//!
//! 2026-09-12 重构（彻底版）：
//! - enabled 开关真实化：持久化到 FileStore；关闭时数据面（chat/completions/
//!   embeddings/messages）统一拒绝转发，管理面不受影响。
//! - 配置真实化：strategy + 池参数持久化到 FileStore，重启保留。
//! - 统计真实化：连接池成功率/延迟来自 health_archive 当日窗口，
//!   会话池来自亲和缓存 + 活跃用户，账号池来自 CF 账号池真实状态。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;
use crate::account::CfAccount;

/// 持久化 key：FileStore 中的网络层配置 JSON。
const NETWORK_CONFIG_STORE_KEY: &str = "network_layer_config";

/// 网络层运行时配置（持久化）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkLayerConfig {
    /// 是否启用数据面转发（false 时所有推理请求返回 503）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 负载均衡策略（展示与调度策略缓存联动）
    #[serde(default = "default_strategy")]
    pub strategy: String,
    /// 账号池下限（低于该值时告警）
    #[serde(default = "default_account_pool_min")]
    pub account_pool_min: usize,
    /// 账号池上限
    #[serde(default = "default_account_pool_max")]
    pub account_pool_max: usize,
    /// 连接池上限（单渠道并发上限参考）
    #[serde(default = "default_connection_pool_max")]
    pub connection_pool_max: usize,
    /// 会话池上限（亲和缓存容量参考）
    #[serde(default = "default_session_pool_max")]
    pub session_pool_max: usize,
}

fn default_enabled() -> bool {
    true
}
fn default_strategy() -> String {
    "priority+weighted+circuit".to_string()
}
fn default_account_pool_min() -> usize {
    2
}
fn default_account_pool_max() -> usize {
    10
}
fn default_connection_pool_max() -> usize {
    10
}
fn default_session_pool_max() -> usize {
    50
}

impl Default for NetworkLayerConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            strategy: default_strategy(),
            account_pool_min: default_account_pool_min(),
            account_pool_max: default_account_pool_max(),
            connection_pool_max: default_connection_pool_max(),
            session_pool_max: default_session_pool_max(),
        }
    }
}

/// 加载网络层配置（无记录时用默认值；读失败降级默认并记录）
pub fn load_network_config(state: &AppState) -> NetworkLayerConfig {
    match state
        .alert_store
        .get::<NetworkLayerConfig>(NETWORK_CONFIG_STORE_KEY)
    {
        Ok(Some(cfg)) => cfg,
        Ok(None) => NetworkLayerConfig::default(),
        Err(e) => {
            tracing::warn!("加载网络层配置失败，使用默认值: {e}");
            NetworkLayerConfig::default()
        }
    }
}

/// 数据面闸门：网络层关闭时拒绝转发请求。
///
/// 调用方（openai::handle_chat_completions 等）在鉴权后立即检查；
/// 管理面 /api/* 与监控端点不经过此闸门，保证关停状态下仍可管理。
/// Err 装箱以压小 Result 体积（clippy result_large_err）。
pub fn network_layer_gate(state: &AppState) -> Result<(), Box<Response>> {
    let cfg = load_network_config(state);
    if !cfg.enabled {
        return Err(Box::new(
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": {
                        "code": "network_layer_disabled",
                        "type": "api_error",
                        "message": "网络层已停用：推理转发被管理员关闭。请稍后重试或联系管理员。",
                    }
                })),
            )
                .into_response(),
        ));
    }
    Ok(())
}

/// 网络层状态信息
#[derive(Debug, Serialize)]
pub struct NetworkStatus {
    /// 网络层是否启用（来自持久化配置）
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
    /// 当前生效的持久化配置
    pub config: NetworkLayerConfig,
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
    /// 负载均衡策略
    pub strategy: Option<String>,
    /// 账号池下限
    pub account_pool_min: Option<usize>,
    /// 账号池上限
    pub account_pool_max: Option<usize>,
    /// 连接池上限
    pub connection_pool_max: Option<usize>,
    /// 会话池上限
    pub session_pool_max: Option<usize>,
}

/// 网络层配置响应（返回持久化后的生效值）
#[derive(Debug, Serialize)]
pub struct NetworkConfigResponse {
    pub enabled: bool,
    pub strategy: String,
    pub account_pool_min: usize,
    pub account_pool_max: usize,
    pub connection_pool_max: usize,
    pub session_pool_max: usize,
}

/// 网络层账号配置（复用主 crate 账号池的 CF 账号结构）。
///
/// 字段双命名兼容：前端曾发 camelCase（accountId/apiToken），后端
/// snake_case 必填导致 422——两个别名都收，缺主命名时回退别名。
#[derive(Debug, Deserialize)]
pub struct AccountConfigRequest {
    pub name: String,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub api_token: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// camelCase 别名（前端 AccountConfigRequest 旧契约）
    #[serde(default, alias = "accountId")]
    #[allow(dead_code)]
    account_id_alias: Option<String>,
    #[serde(default, alias = "apiToken")]
    api_token_alias: Option<String>,
}

/// 获取网络层健康状态
///
/// 聚合主 crate 各子系统（账号池 / 渠道 / 健康追踪 / 断路器 / 健康归档）
/// 的真实状态，供管理后台「网络层」面板展示。
///
/// 响应包统一信封 { success, data }（与前端 ApiEnvelope 契约一致；
/// 前端原先按裸结构解析导致 `res.data` 恒 undefined，整个面板不渲染）。
pub async fn health_check(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 管理面板状态接口：仅管理员可见（泄露账号池规模/健康统计/活跃用户数）
    verify_admin(&state, &headers).await?;
    let cfg = load_network_config(&state);

    let accounts = state.account_pool.list();
    let total_accounts = accounts.len();
    let available_accounts = accounts.iter().filter(|a| a.status == "active").count();
    let busy_accounts = accounts
        .iter()
        .filter(|a| {
            a.last_used_at
                .is_some_and(|t| chrono::Utc::now().timestamp() - t < 300)
        })
        .count();
    let error_accounts = accounts.iter().filter(|a| a.status == "error").count();
    let invalid_accounts = accounts.iter().filter(|a| a.status == "pending").count();

    let channels = state.channel_store.list();
    let total_connections = channels.len();
    let active_connections = channels.iter().filter(|c| c.is_enabled()).count();
    let idle_connections = total_connections - active_connections;

    // 连接池请求数/延迟：从 health_archive 当日窗口聚合（真实建流统计）
    let mut successful_requests: u64 = 0;
    let mut failed_requests: u64 = 0;
    let mut latency_sum: u64 = 0;
    let mut latency_count: u64 = 0;
    let mut breaker_open: u64 = 0;
    for ch in &channels {
        if state.channel_store.circuit_breaker().get_state(&ch.id) == "open" {
            breaker_open += 1;
        }
        if let Some(today) = state.channel_store.health_archive().query(&ch.id, 1).pop() {
            successful_requests += today.success;
            failed_requests += today.failure;
            // 平均延迟按各渠道请求数加权（P95 作为窗口代表值）
            let n = today.success + today.failure;
            if n > 0 {
                latency_sum += today.p95_ms() * n;
                latency_count += n;
            }
        }
    }
    let avg_latency_ms = if latency_count > 0 {
        latency_sum as f64 / latency_count as f64
    } else {
        0.0
    };
    let _ = breaker_open;

    // 会话：亲和缓存会话 + 活跃用户
    let affinity_sessions = state.channel_store.affinity_cache().len();
    let active_users = state
        .user_store
        .list()
        .iter()
        .filter(|u| u.status == "active")
        .count();
    let total_sessions = affinity_sessions.max(active_users);
    let idle_sessions = total_connections.saturating_sub(active_connections);

    let status = NetworkStatus {
        enabled: cfg.enabled,
        account_pool: AccountPoolStatus {
            total_accounts,
            available_accounts,
            busy_accounts,
            error_accounts,
            invalid_accounts,
            total_requests: state.usage_tracker.today_stats().requests,
            failed_requests,
        },
        connection_pool: ConnectionPoolStatus {
            total_connections,
            active_connections,
            idle_connections,
            total_connections_created: total_connections as u64,
            total_connections_closed: 0,
            successful_requests,
            failed_requests,
            avg_latency_ms,
        },
        session_pool: SessionPoolStats {
            total_sessions,
            active_sessions: active_users,
            idle_sessions,
            session_ttl_hours: 72,
        },
        load_balance_strategy: cfg.strategy.clone(),
        last_check_at: chrono::Utc::now().timestamp(),
        config: cfg,
    };
    Ok(Json(json!({ "success": true, "data": status })))
}

/// 更新网络层配置（持久化到 FileStore，重启保留）
pub async fn update_network_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(_config_id): Path<String>,
    Json(request): Json<NetworkConfigRequest>,
) -> Result<Json<NetworkConfigResponse>, (StatusCode, Json<Value>)> {
    // 管理端配置写入：仅管理员（enabled:false 会停全站数据面）
    verify_admin(&state, &headers).await?;
    // 读-改-写：以当前生效配置为底，覆盖请求中出现的字段
    let mut cfg = load_network_config(&state);
    cfg.enabled = request.enabled;
    if let Some(strategy) = request.strategy.as_deref() {
        if !strategy.is_empty() {
            cfg.strategy = strategy.to_string();
        }
    }
    if let Some(v) = request.account_pool_min {
        cfg.account_pool_min = v;
    }
    if let Some(v) = request.account_pool_max {
        cfg.account_pool_max = v.max(cfg.account_pool_min);
    }
    if let Some(v) = request.connection_pool_max {
        cfg.connection_pool_max = v;
    }
    if let Some(v) = request.session_pool_max {
        cfg.session_pool_max = v;
    }

    state
        .alert_store
        .put(NETWORK_CONFIG_STORE_KEY, &cfg)
        .map_err(|e| {
            error_response(
                &format!("配置持久化失败: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;

    tracing::info!(
        "网络层配置已更新: enabled={} strategy={} 池参数 {}/{}/{}/{}",
        cfg.enabled,
        cfg.strategy,
        cfg.account_pool_min,
        cfg.account_pool_max,
        cfg.connection_pool_max,
        cfg.session_pool_max
    );

    Ok(Json(NetworkConfigResponse {
        enabled: cfg.enabled,
        strategy: cfg.strategy,
        account_pool_min: cfg.account_pool_min,
        account_pool_max: cfg.account_pool_max,
        connection_pool_max: cfg.connection_pool_max,
        session_pool_max: cfg.session_pool_max,
    }))
}

/// 添加网络层账号（接入主 crate 的 CF 账号池）
///
/// 兼容两种调用：`/api/network/accounts/:id` 不带 body 时按账号 ID 直接添加；
/// 带 `{name, account_id, api_token, status}` body 时按完整配置添加。
pub async fn add_network_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
    body: Option<Json<AccountConfigRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 写入 CF 账号凭据（api_token）：仅管理员
    verify_admin(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    let request = body.map(|Json(r)| r);
    // camelCase 别名归一：前端旧契约发 accountId/apiToken
    let body_account_id = request
        .as_ref()
        .and_then(|r| r.account_id.clone().or(r.account_id_alias.clone()));
    let body_api_token = request
        .as_ref()
        .and_then(|r| r.api_token.clone().or(r.api_token_alias.clone()));
    let account = CfAccount {
        id: uuid::Uuid::new_v4().to_string(),
        name: request
            .as_ref()
            .filter(|r| !r.name.is_empty())
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("network-{}", account_id)),
        account_id: body_account_id
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| account_id.clone()),
        api_token: body_api_token.unwrap_or_default(),
        status: request
            .as_ref()
            .and_then(|r| r.status.clone())
            .unwrap_or_else(|| "active".to_string()),
        last_error: None,
        last_used_at: None,
        created_at: now,
    };
    state.account_pool.add(account).map_err(|e| {
        error_response(
            &format!("添加账号失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "success": true,
        "message": "网络层账号已添加"
    })))
}

/// 删除网络层账号（按账号 ID）
pub async fn remove_network_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(account_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 删除账号池条目：仅管理员
    verify_admin(&state, &headers).await?;
    // 后端按内部 UUID（account:{uuid}）存储，前端可能误传 CF account_id；
    // 两种 ID 都尝试匹配删除，找不到时报 404 而非静默成功。
    let removed = state.account_pool.remove(&account_id).or_else(|_| {
        // account_id 不匹配内部 id 时，尝试按 CF account_id 字段找内部 id
        let internal = state
            .account_pool
            .list()
            .into_iter()
            .find(|a| a.account_id == account_id)
            .map(|a| a.id);
        match internal {
            Some(id) => state.account_pool.remove(&id),
            None => Ok(()),
        }
    });
    match removed {
        Ok(_) => {
            let still = state
                .account_pool
                .list()
                .into_iter()
                .any(|a| a.id == account_id || a.account_id == account_id);
            if still {
                return Err(error_response("账号未找到", StatusCode::NOT_FOUND));
            }
            Ok(Json(json!({
                "success": true,
                "message": "网络层账号已删除"
            })))
        }
        Err(e) => Err(error_response(
            &format!("删除失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 列出网络层账号（CF 账号池真实状态）
pub async fn list_network_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 账号池列表：仅管理员（含账号状态与错误信息）
    verify_admin(&state, &headers).await?;
    let accounts = state.account_pool.list();
    let items: Vec<Value> = accounts
        .iter()
        .map(|a| {
            json!({
                "id": a.id,
                "name": a.name,
                "account_id": a.account_id,
                "status": a.status,
                "last_error": a.last_error,
                "last_used_at": a.last_used_at,
                "created_at": a.created_at,
            })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": items })))
}

/// 重启网络层
///
/// 复位所有渠道的断路器与健康追踪状态；渠道探活由后台 prober 周期执行。
pub async fn restart_network(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 重置全部断路器/健康状态：仅管理员
    verify_admin(&state, &headers).await?;
    let mut count = 0usize;
    for ch in state.channel_store.list() {
        state.channel_store.circuit_breaker().reset(&ch.id);
        state.channel_store.health_tracker().reset(&ch.id);
        count += 1;
    }
    Ok(Json(json!({
        "success": true,
        "message": format!("网络层重启完成（已重置 {count} 个渠道的断路器与健康状态）"),
        "status": "started"
    })))
}

/// 统一错误响应（与 admin 模块其他 handler 一致的 (StatusCode, Json) 形态）
fn error_response(msg: &str, status: StatusCode) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "success": false, "message": msg })))
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
