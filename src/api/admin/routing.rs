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
        // #40：只**新增**字段（既有字段语义不变，旧前端不受影响）
        "enabled": policy.enabled,
        "profiles": policy.profiles,
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
    /// #40 多规则档案总开关（缺省 = 保持现值）
    #[serde(default)]
    pub enabled: Option<bool>,
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
    // #40 档案总开关（档案本体的读写端点在后续步骤落地；上限校验见下）
    if let Some(v) = req.enabled {
        policy.enabled = v;
    }

    // 持久化 + 快照热更（策略/粘性下一请求生效）
    //
    // 档案上限校验（#40 契约 §1/§3）：超限**拒绝写入**（400，客户端问题），
    // 不静默截断；`store.update` 内还有同一道校验兜底（防别的写入路径绕过）。
    if let Err(e) = policy.profiles.validate() {
        return Err(super::common::error_response(
            &format!("路由档案超限：{e}"),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
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
        // #40 新增字段（既有字段不变）
        "enabled": policy.enabled,
        "profiles": policy.profiles,
    })))
}

/// 默认值（前端「恢复默认」按钮用；不落盘，仅回显）。
pub async fn handle_routing_defaults(
    state: State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    // #17-②：管理面端点统一鉴权（FE 唯一调用点=登录后的 RoutingPanel
    // 「恢复默认」回显）。虽只返回静态默认值（零敏感信息），但与同组
    // /api/routing/* 端点对齐，避免「为什么唯独这个不用登录」的困惑。
    crate::api::admin::common::verify_admin(&state, &headers).await?;
    let d = RoutingPolicy::default();
    Ok(Json(json!({
        "strategy": d.strategy,
        "sticky": d.sticky,
        "breaker": BreakerPolicy::default(),
        "scheduler_weights": d.scheduler_weights,
        // #40 新增字段（默认关；档案空）
        "enabled": d.enabled,
        "profiles": d.profiles,
    })))
}
/// GET /api/routing/queue?model=xxx — 候选队列预演（对照 Agent2API 路由队列可见化）。
///
/// 只读预演：复用数据面同一套过滤排序（`select_for_model`：启用/冷却/
/// 断路器/AIMD 过滤 + 策略排序，**不推进 round_robin 游标**），把
/// 「谁排第一、谁被挡在门外」回显给管理页，保存策略前就能看到效果。
/// 注意：round_robin 策略下实际首挑由游标决定，预演以静态排序为准；
/// weighted / ranked 策略下预演与实际首挑一致（页面文案已注明）。
pub async fn handle_routing_queue(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = q
        .get("model")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            super::common::error_response(
                "缺少 model 查询参数",
                axum::http::StatusCode::BAD_REQUEST,
            )
        })?;

    let policy = state.routing_policy.snapshot();
    // #40 判定统一（契约 §3）：预演与数据面 `resolve_bridges` 走**同一序列**
    // ——resolve_profile → 按生效 strategy 选候选 → plan_route 裁剪。此前预演
    // 自成一套口径，正是 #31「回放投影与真实形状不匹配」那类事故的来源。
    let profile =
        crate::channel::routing_policy::resolve_profile(&policy, model, &state.model_mapper, None);
    // 通过筛选的有序候选（与数据面 resolve_bridges 同一口径：按当前路由
    // 策略过滤排序——预演的队首才有「首挑预测」价值）
    let ordered = state.channel_store.select_for_model_by_strategy(
        model,
        profile.strategy,
        &state.pricing_store,
    );
    let plan = crate::channel::routing_policy::plan_route(&policy, &profile, &ordered);
    let breaker_map = state.channel_store.circuit_breaker().get_status_map();

    let ordered_json: Vec<Value> = plan
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "provider": c.channel_type.as_str(),
                "state": breaker_map
                    .get(&c.id)
                    .cloned()
                    .unwrap_or_else(|| "closed".to_string()),
            })
        })
        .collect();

    // 被挡在门外的启用渠道：进不了 ordered 的都算（冷却/熔断/流控/未声明该模型）
    let ordered_ids: Vec<&str> = plan.iter().map(|c| c.id.as_str()).collect();
    let blocked_json: Vec<Value> = state
        .channel_store
        .list()
        .iter()
        .filter(|c| c.is_enabled() && !ordered_ids.contains(&c.id.as_str()))
        .map(|c| {
            let breaker_status = breaker_map
                .get(&c.id)
                .cloned()
                .unwrap_or_else(|| "closed".to_string());
            let reason = if breaker_status == "open" {
                "断路器打开"
            } else if breaker_status == "halfopen" {
                "断路器半开（试探中）"
            } else {
                "冷却中 / 流控受限 / 未声明该模型"
            };
            json!({
                "id": c.id,
                "name": c.name,
                "provider": c.channel_type.as_str(),
                "state": breaker_status,
                "reason": reason,
            })
        })
        .collect();

    Ok(Json(json!({
        "strategy": profile.strategy,
        "sticky": profile.sticky,
        "model": model,
        "pickedId": plan.first().map(|c| c.id.clone()),
        "ordered": ordered_json,
        "blocked": blocked_json,
        // #40 附加字段（旧前端不读即忽略，保持既有字段语义不变）：
        // 档案总开关、实际命中的层、被档案裁掉的原因计数
        "profilesEnabled": policy.enabled,
        "appliedLayers": profile.provenance.applied_layers(),
        "skipped": plan.skipped,
    })))
}

// ── 粘滞会话管理面（#38；对照 Agent2API routing_workspace 的 get_sticky/clear_sticky）──

/// 管理面粘滞快照单条上限：超出截断并以 `truncated` 回执
/// （参照 #26 `ROUTE_REPLAY_MAX_*` 的有界口径，防大缓存撑爆响应）。
const ROUTE_STICKY_MAX_SNAPSHOT: usize = 500;
/// 快照 id 的内部编码分隔符（单元分隔符 U+001F）：model 名理论上可含任意
/// 可打印字符，用控制字符杜绝歧义。前端不解析 id，清除时原样回传。
const ROUTE_STICKY_ID_SEP: char = '\u{1f}';

/// GET /api/routing/sticky?query= — 当前粘滞条目快照（admin）。
///
/// - `query`（可选）：对 session_id / model / channel_id 的小写包含过滤
/// - `data.sticky`：当前粘滞档位（auto / session / turn / off，snake_case）
/// - `data.total / truncated`：过滤后总数 + 是否被快照上限截断
///
/// 性能：`AffinityCache::snapshot` 逐分片短读锁、单条 clone 即放——
/// 管理面低频路径，不改数据面选路热路径（affinity.rs 模块注释有锁分析）。
pub async fn handle_get_route_sticky(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let policy = state.routing_policy.snapshot();
    let (entries, total) = state
        .channel_store
        .affinity_cache()
        .snapshot(ROUTE_STICKY_MAX_SNAPSHOT);
    // id → 渠道名映射：Arc 快照读（load_full 原子指针拷贝，读侧无锁），
    // 让管理面显示渠道名而不是裸 id（前端 channel_name 优先、缺省回落 #id）。
    let channel_names: std::collections::HashMap<String, String> = state
        .channel_store
        .snapshot()
        .iter()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let q = params
        .get("query")
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    let items: Vec<Value> = entries
        .into_iter()
        .filter(|e| {
            q.is_empty()
                || e.session_id.to_lowercase().contains(&q)
                || e.model.to_lowercase().contains(&q)
                || e.channel_id.to_lowercase().contains(&q)
                || channel_names
                    .get(&e.channel_id)
                    .map(|n| n.to_lowercase().contains(&q))
                    .unwrap_or(false)
        })
        .map(|e| {
            json!({
                // 内部编码 session<U+001F>model；前端清除时原样回传
                "id": format!("{}{ROUTE_STICKY_ID_SEP}{}", e.session_id, e.model),
                "channel_id": e.channel_id,
                "channel_name": channel_names.get(&e.channel_id).cloned(),
                // 硬过期时刻（unix 秒；前端 expiryMillis 的 <1e11 视为秒口径）
                "expires_at": now_unix + e.hard_remaining_secs as i64,
                "updated_at": now_unix - e.age_secs as i64,
                "sticky_remaining_secs": e.sticky_remaining_secs,
                "hard_remaining_secs": e.hard_remaining_secs,
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "entries": items,
            "total": total,
            "truncated": total > ROUTE_STICKY_MAX_SNAPSHOT,
            "now": now_unix,
            "sticky": policy.sticky,
        }
    })))
}

/// POST /api/routing/sticky/clear 请求体：单条（`id`）与全清（`all`）二选一；
/// 都传以 `id` 优先；都不给 400。
#[derive(Debug, Deserialize)]
pub struct RouteStickyClearRequest {
    pub id: Option<String>,
    pub all: Option<bool>,
}

/// POST /api/routing/sticky/clear — 解除粘滞（admin）。
///
/// 语义对齐 [`crate::channel::affinity::AffinityCache::evict`]：只删内存亲和
/// 记录，不写策略快照、不产生新状态——下次该会话选路自然重走 HRW。
pub async fn handle_clear_route_sticky(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RouteStickyClearRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let cache = state.channel_store.affinity_cache();
    let cleared = if let Some(id) = body.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        let mut parts = id.split(ROUTE_STICKY_ID_SEP);
        match (parts.next(), parts.next()) {
            (Some(sid), Some(model)) if !sid.is_empty() && !model.is_empty() => {
                usize::from(cache.evict_session(sid, model))
            }
            // 非法 / 过期 id：不 500，回 0 条（幂等清除）
            _ => 0usize,
        }
    } else if body.all == Some(true) {
        cache.clear_all()
    } else {
        return Err(super::common::error_response(
            "id 与 all 至少提供一个（单条清除传 GET 返回的 id；全清传 all:true）",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    };
    Ok(Json(json!({
        "success": true,
        "data": { "cleared": cleared }
    })))
}
