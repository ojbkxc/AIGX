//! 路由策略管理 API — 对照 Agent2API `api/routing_api.rs`（GET/PUT /api/routing）。
//!
//! AIGX 的选路组件（priority+weight、RoundRobin、多因子调度、亲和、断路器）
//! 原本全部硬编码；本 API 把它们收敛成一份可热更新的 `RoutingPolicy`：
//!
//! - `GET /api/routing`：当前策略 + 运行时状态（亲和条目数、断路器开/半开数）
//! - `PUT /api/routing`：整份保存（校验后落 FileStore + 热应用到各组件）
//!
//! 热应用点：
//! - 策略/粘性：数据面 `resolve_bridges` 每请求读快照分派
//! - 断路器参数：`CircuitBreaker::update_params`（下一请求生效）
//! - 调度权重：`ChannelStore::set_scheduler_config`（原闲置接线，接上）

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;
use crate::channel::routing_policy::{BreakerPolicy, RouteStrategy, RoutingPolicy, StickyMode};

/// GET /api/routing — 当前路由策略 + 运行时状态。
pub async fn handle_get_routing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let policy = state.routing_policy.snapshot();
    let breaker_map = state.channel_store.circuit_breaker().get_status_map();
    let breaker_open = breaker_map.values().filter(|s| *s == "open").count();
    let breaker_halfopen = breaker_map.values().filter(|s| *s == "halfopen").count();
    Ok(Json(json!({
        "strategy": policy.strategy,
        "sticky": policy.sticky,
        "breaker": policy.breaker,
        "scheduler_weights": policy.scheduler_weights,
        "runtime": {
            "affinity_entries": state.channel_store.affinity_cache().len(),
            "breaker_open": breaker_open,
            "breaker_halfopen": breaker_halfopen,
        }
    })))
}

/// PUT /api/routing 请求体。
#[derive(Debug, Deserialize)]
pub struct RoutingUpdateRequest {
    /// 选路策略：weighted / round_robin / ranked
    #[serde(default)]
    pub strategy: Option<String>,
    /// 粘性档位：auto / session / off
    #[serde(default)]
    pub sticky: Option<String>,
    /// 断路器参数
    #[serde(default)]
    pub breaker: Option<BreakerUpdate>,
    /// 调度权重（health/cost/rpm，strategy=ranked 时生效）
    #[serde(default)]
    pub scheduler_weights: Option<SchedulerWeightsUpdate>,
}

#[derive(Debug, Deserialize)]
pub struct BreakerUpdate {
    pub enabled: Option<bool>,
    pub threshold: Option<u32>,
    pub cooldown_secs: Option<u64>,
    /// HalfOpen 连续成功多少次才真正关闭（1-10；A2A routingBreakerSuccessThreshold）
    pub success_threshold: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SchedulerWeightsUpdate {
    pub health_weight: Option<f64>,
    pub cost_weight: Option<f64>,
    pub rpm_weight: Option<f64>,
}

/// PUT /api/routing — 保存并热应用（读-改-写：未出现的字段保留现值）。
pub async fn handle_update_routing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RoutingUpdateRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut policy = state.routing_policy.snapshot().as_ref().clone();

    if let Some(s) = req.strategy.as_deref() {
        match RouteStrategy::from_str_opt(s) {
            Some(v) => policy.strategy = v,
            None => {
                return Err(super::common::error_response(
                    &format!("无效的路由策略：{s}（可选 weighted / round_robin / ranked）"),
                    axum::http::StatusCode::BAD_REQUEST,
                ))
            }
        }
    }
    if let Some(s) = req.sticky.as_deref() {
        match StickyMode::from_str_opt(s) {
            Some(v) => policy.sticky = v,
            None => {
                return Err(super::common::error_response(
                    &format!("无效的粘性档位：{s}（可选 auto / session / turn / off）"),
                    axum::http::StatusCode::BAD_REQUEST,
                ))
            }
        }
    }
    if let Some(b) = req.breaker {
        if let Some(v) = b.enabled {
            policy.breaker.enabled = v;
        }
        if let Some(v) = b.threshold {
            if !(1..=20).contains(&v) {
                return Err(super::common::error_response(
                    "断路器阈值须在 1-20 之间",
                    axum::http::StatusCode::BAD_REQUEST,
                ));
            }
            policy.breaker.threshold = v;
        }
        if let Some(v) = b.cooldown_secs {
            if !(10..=600).contains(&v) {
                return Err(super::common::error_response(
                    "断路器冷却须在 10-600 秒之间",
                    axum::http::StatusCode::BAD_REQUEST,
                ));
            }
            policy.breaker.cooldown_secs = v;
        }
        if let Some(v) = b.success_threshold {
            if !(1..=10).contains(&v) {
                return Err(super::common::error_response(
                    "半开成功阈值须在 1-10 之间",
                    axum::http::StatusCode::BAD_REQUEST,
                ));
            }
            policy.breaker.success_threshold = v;
        }
    }
    if let Some(w) = req.scheduler_weights {
        if let Some(v) = w.health_weight {
            policy.scheduler_weights.health_weight = v;
        }
        if let Some(v) = w.cost_weight {
            policy.scheduler_weights.cost_weight = v;
        }
        if let Some(v) = w.rpm_weight {
            policy.scheduler_weights.rpm_weight = v;
        }
        if !policy.scheduler_weights.validate() {
            return Err(super::common::error_response(
                "调度权重非法：须为非负有限数且至少一个为正",
                axum::http::StatusCode::BAD_REQUEST,
            ));
        }
    }

    // 持久化 + 快照热更（策略/粘性下一请求生效）
    state.routing_policy.update(policy.clone()).map_err(|e| {
        super::common::error_response(
            &format!("路由策略持久化失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    // 断路器热更（下一请求生效）：开关 + 三参一起下发。
    // enabled=false 时 set_enabled(false) 让 allow_request 恒放行
    // （记账保留），此前"关闭仍按默认 5/30s 熔断"是假开关。
    state
        .channel_store
        .circuit_breaker()
        .set_enabled(policy.breaker.enabled);
    if policy.breaker.enabled {
        state.channel_store.circuit_breaker().update_params(
            policy.breaker.threshold,
            policy.breaker.cooldown_secs,
            policy.breaker.success_threshold,
        );
    }
    // 粘性档位热更：切换亲和缓存双 TTL（auto=5min/30min，session=6h/6h）
    if let Some((sticky_secs, hard_secs)) = policy.sticky.ttls() {
        state.channel_store.affinity_cache().update_ttls(
            std::time::Duration::from_secs(sticky_secs),
            std::time::Duration::from_secs(hard_secs),
        );
    }
    // 调度权重热更（ranked 策略生效）
    state
        .channel_store
        .set_scheduler_config(policy.scheduler_weights.clone());

    tracing::info!(
        "路由策略已更新: strategy={} sticky={} breaker({},{},{}) 权重({}/{}/{})",
        policy.strategy.as_str(),
        policy.sticky.as_str(),
        policy.breaker.enabled,
        policy.breaker.threshold,
        policy.breaker.cooldown_secs,
        policy.scheduler_weights.health_weight,
        policy.scheduler_weights.cost_weight,
        policy.scheduler_weights.rpm_weight,
    );

    Ok(Json(json!({
        "strategy": policy.strategy,
        "sticky": policy.sticky,
        "breaker": policy.breaker,
        "scheduler_weights": policy.scheduler_weights,
    })))
}

/// 默认值（前端「恢复默认」按钮用；不落盘，仅回显）。
pub async fn handle_routing_defaults(
    _state: State<AppState>,
    _headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let d = RoutingPolicy::default();
    Ok(Json(json!({
        "strategy": d.strategy,
        "sticky": d.sticky,
        "breaker": BreakerPolicy::default(),
        "scheduler_weights": d.scheduler_weights,
    })))
}
