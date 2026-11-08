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

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;
use crate::channel::routing_policy::{
    caps, BreakerPolicy, RequestNeeds, RouteStrategy, RoutingGroup, RoutingPolicy, RoutingProfiles,
    StickyMode,
};
use crate::model::capability::CapabilityMask;

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
    let (ordered_json, blocked_json) = queue_projection(&state, &plan, &breaker_map);

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

/// 预演投影（`/api/routing/queue` 与 `/api/routing/preview` 共用，**不造
/// 第二套**）：plan 的参与候选 + 被挡门外的启用渠道两段 JSON。
fn queue_projection(
    state: &AppState,
    plan: &crate::channel::routing_policy::RoutePlan<'_>,
    breaker_map: &std::collections::HashMap<String, String>,
) -> (Vec<Value>, Vec<Value>) {
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
    (ordered_json, blocked_json)
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

// ── 多规则档案管理面（#40 第 2 步；契约冻结稿 §5：在既有 /api/routing/* 上扩）──

/// GET /api/routing/profiles — 整份档案读回（含总开关）。
pub async fn handle_get_routing_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let policy = state.routing_policy.snapshot();
    Ok(Json(json!({
        "enabled": policy.enabled,
        "profiles": policy.profiles,
    })))
}

/// PUT /api/routing/profiles 请求体：**只承载 profiles 本体**（整份替换 ——
/// 模型级档案/组的增量编辑走单组端点或先 GET 改完再 PUT）。
///
/// **没有 `enabled` 字段（有意为之）**：总开关的唯一写入口是
/// `PUT /api/routing`（[`handle_update_routing`]，前端 fe-routing 已联调）。
/// 同一字段留两个写入口就是两份真相 —— `enabled` 在本请求体里根本不存在，
/// 读-改-写路径天然保留其原值，不会被整份替换冲成默认。
#[derive(Debug, Deserialize)]
pub struct RoutingProfilesUpdate {
    #[serde(default)]
    pub profiles: Option<RoutingProfiles>,
}

/// PUT /api/routing/profiles — 保存 profiles 本体（上限校验，超限 400 拒绝写入）。
///
/// 写路径：读当前快照 → 只替换 `profiles` 字段 → `validate()` → `update()`；
/// `enabled` 等其余字段保持原值。总开关走 `PUT /api/routing`。
pub async fn handle_put_routing_profiles(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<RoutingProfilesUpdate>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let saved = replace_routing_profiles(&state.routing_policy, req.profiles)
        .map_err(|(code, msg)| super::common::error_response(&msg, code))?;
    Ok(Json(json!({
        "enabled": saved.enabled,
        "profiles": saved.profiles,
    })))
}

/// [`handle_put_routing_profiles`] 的写入逻辑（纯函数，handler 与端点级单测
/// 共用同一份判定）：读当前快照 → 只替换 `profiles` 字段（`None` = 不动，
/// 端点契约「提供哪份改哪份」）→ `validate()` 超限拒绝 → `update()` 落盘热更。
///
/// `enabled` 等其余字段**原样保留**（update 是整份替换的 copy-on-write，
/// 读-改-写保证它们不被冲掉）——这是「enabled 唯一写入口是 `PUT /api/routing`」
/// 的实现侧闸门，单测 `replace_routing_profiles_swaps_body_and_keeps_enabled`
/// 钉住。
fn replace_routing_profiles(
    store: &crate::channel::routing_policy::RoutingPolicyStore,
    profiles: Option<RoutingProfiles>,
) -> Result<RoutingPolicy, (axum::http::StatusCode, String)> {
    let mut policy = store.snapshot().as_ref().clone();
    if let Some(p) = profiles {
        policy.profiles = p;
    }
    if let Err(e) = policy.profiles.validate() {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("路由档案超限：{e}"),
        ));
    }
    // store.update 内再跑一次 validate（防其他写入路径绕过的兜底）
    store.update(policy).map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("路由策略持久化失败: {e}"),
        )
    })
}

/// GET /api/routing/profiles/group/:id — 单组读（404 = 不存在）。
pub async fn handle_get_routing_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let policy = state.routing_policy.snapshot();
    policy
        .profiles
        .group(&id)
        .map(|g| Json(json!(g)))
        .ok_or_else(|| {
            super::common::error_response(
                &format!("路由组 `{id}` 不存在"),
                axum::http::StatusCode::NOT_FOUND,
            )
        })
}

/// PUT /api/routing/profiles/group/:id — 单组 upsert（同 id 替换，否则追加）。
///
/// body.id 为空时以路径 id 为准；两者都有且不一致则 400（防「改 A 写成 B」）。
pub async fn handle_put_routing_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut group): Json<RoutingGroup>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if group.id.is_empty() {
        group.id = id.clone();
    }
    if group.id != id {
        return Err(super::common::error_response(
            &format!("body.id `{}` 与路径 id `{id}` 不一致", group.id),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
    let mut policy = state.routing_policy.snapshot().as_ref().clone();
    if policy.profiles.group(&id).is_none() && policy.profiles.groups.len() >= caps::MAX_GROUPS {
        return Err(super::common::error_response(
            &format!("路由组数已达上限 {}", caps::MAX_GROUPS),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
    policy.profiles.upsert_group(group);
    if let Err(e) = policy.profiles.validate() {
        return Err(super::common::error_response(
            &format!("路由档案超限：{e}"),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
    let saved = state.routing_policy.update(policy).map_err(|e| {
        super::common::error_response(
            &format!("路由策略持久化失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let group = saved.profiles.group(&id).cloned();
    Ok(Json(json!({ "group": group })))
}

/// DELETE /api/routing/profiles/group/:id — 删除组并连带清各档案层
/// `default_group` 悬空引用（`clearedDefaultGroupRefs` 回执清了几处）。
pub async fn handle_delete_routing_group(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut policy = state.routing_policy.snapshot().as_ref().clone();
    let refs_before = policy.profiles.default_group_refs(&id);
    if !policy.profiles.remove_group(&id) {
        return Err(super::common::error_response(
            &format!("路由组 `{id}` 不存在"),
            axum::http::StatusCode::NOT_FOUND,
        ));
    }
    state.routing_policy.update(policy).map_err(|e| {
        super::common::error_response(
            &format!("路由策略持久化失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "removed": id,
        "clearedDefaultGroupRefs": refs_before,
    })))
}

/// POST /api/routing/preview 请求体（契约 §5：{model, needs, group_id}）。
#[derive(Debug, Deserialize)]
pub struct RoutingPreviewRequest {
    pub model: String,
    #[serde(default)]
    pub needs: Option<RequestNeeds>,
    /// 显式指定路由组（契约 §6.1 第 1 环的预演形态；数据面的 Key 授权链
    /// 在后续步骤接入 —— 预演端点是管理面，不模拟 Key 白名单）。
    #[serde(default)]
    pub group_id: Option<String>,
}

/// POST /api/routing/preview — 判定预演（复用 queue 的投影 helper 与数据面
/// 同一判定序列），比 GET /api/routing/queue 多支持 `group_id` 与
/// `needs` 入参，并回显 resolved profile 的逐字段 provenance。
///
/// `needs` 与数据面同口径（#40 §1）：body 里的 needs（管理员模拟的本次请求
/// 需求）走与 `resolve_bridges_needs` 同一条渠道级粗筛
/// （`channel_may_serve`，掩码来自**唯一映射链** `required_tags` →
/// `CapabilityMask::from_tags`）；空 = 不过滤。账号级精筛在预演不展开
/// （候选是渠道级投影，账号身份在选号时才确定 —— 与数据面的两级分离一致）。
pub async fn handle_routing_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RoutingPreviewRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = body.model.trim();
    if model.is_empty() {
        return Err(super::common::error_response(
            "model 不能为空",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
    let policy = state.routing_policy.snapshot();
    let group_id = body
        .group_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    // 与数据面 / queue 完全同一判定序列（#31 教训：预演不造第二套）
    let profile = crate::channel::routing_policy::resolve_profile(
        &policy,
        model,
        &state.model_mapper,
        group_id,
    );
    let ordered = {
        // #40 §1 预演同口径：needs → 掩码 → 渠道级粗筛（与数据面
        // resolve_bridges_needs 里的同一判定，非账号渠道由 channel_may_serve
        // 放行、无目录证据放行 —— 「宁放勿误伤」）
        let needs_mask = CapabilityMask::from_tags(body.needs.unwrap_or_default().required_tags());
        let capability_gate = |ch: &crate::channel::Channel| {
            state.account_sessions.channel_may_serve(&ch.id, needs_mask)
        };
        let coarse: Option<crate::channel::ChannelGate<'_>> = if needs_mask.is_empty() {
            None
        } else {
            Some(&capability_gate)
        };
        state.channel_store.select_for_model_by_strategy_filtered(
            model,
            profile.strategy,
            &state.pricing_store,
            coarse,
        )
    };
    let plan = crate::channel::routing_policy::plan_route(&policy, &profile, &ordered);
    let breaker_map = state.channel_store.circuit_breaker().get_status_map();
    let (ordered_json, blocked_json) = queue_projection(&state, &plan, &breaker_map);

    Ok(Json(json!({
        "model": model,
        "requestedGroupId": group_id,
        "profilesEnabled": policy.enabled,
        "resolved": {
            "strategy": profile.strategy,
            "sticky": profile.sticky,
            "needs": profile.needs,
            "defaultGroup": profile.default_group,
            "scope": profile.scope,
            "sortRules": profile.sort_rules,
            "provenance": profile.provenance,
            "appliedLayers": profile.provenance.applied_layers(),
        },
        "pickedId": plan.first().map(|c| c.id.clone()),
        "ordered": ordered_json,
        "blocked": blocked_json,
        "skipped": plan.skipped,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::routing_policy::{
        AccountSortRule, RoutingMember, RoutingProfile, RoutingScope, SortDir,
    };

    fn store() -> crate::channel::routing_policy::RoutingPolicyStore {
        crate::channel::routing_policy::RoutingPolicyStore::in_memory(RoutingPolicy::default())
    }

    /// 造一个路由组（strategy/sticky 缺省 = 继承上层，三态语义）。
    fn group(id: &str, members: Vec<RoutingMember>) -> RoutingGroup {
        RoutingGroup {
            id: id.to_string(),
            name: String::new(),
            model: String::new(),
            strategy: None,
            sticky: None,
            members,
        }
    }

    /// 造一份超限档案（每种形态只超一个维度，其余合规；纯构造，无字段重赋）。
    fn over_limit(kind: &str) -> RoutingProfiles {
        match kind {
            "models" => RoutingProfiles {
                models: (0..=caps::MAX_MODELS)
                    .map(|i| (format!("m{i}"), RoutingProfile::default()))
                    .collect(),
                ..RoutingProfiles::default()
            },
            "groups" => RoutingProfiles {
                groups: (0..=caps::MAX_GROUPS)
                    .map(|i| group(&format!("g{i}"), Vec::new()))
                    .collect(),
                ..RoutingProfiles::default()
            },
            "providers" => RoutingProfiles {
                global: RoutingProfile {
                    scope: Some(RoutingScope {
                        providers: (0..=caps::MAX_PROVIDERS).map(|i| format!("p{i}")).collect(),
                        ..Default::default()
                    }),
                    ..RoutingProfile::default()
                },
                ..RoutingProfiles::default()
            },
            "accounts" => RoutingProfiles {
                groups: vec![group(
                    "g",
                    vec![RoutingMember {
                        provider: "openai".to_string(),
                        model: "m".to_string(),
                        included: true,
                        accounts: (0..=caps::MAX_ACCOUNTS).map(|i| format!("a{i}")).collect(),
                    }],
                )],
                ..RoutingProfiles::default()
            },
            "sort_rules" => RoutingProfiles {
                global: RoutingProfile {
                    sort_rules: Some(vec![
                        AccountSortRule::Added { dir: SortDir::Desc },
                        AccountSortRule::Expires { dir: SortDir::Asc },
                        AccountSortRule::Balance { dir: SortDir::Desc },
                    ]),
                    ..RoutingProfile::default()
                },
                ..RoutingProfiles::default()
            },
            other => unreachable!("未知超限形态 {other}"),
        }
    }

    /// #40 §3 硬要求：五个维度超限一律拒绝（400），且**现值不被改坏**。
    #[test]
    fn replace_routing_profiles_rejects_over_limit_and_keeps_current() {
        let store = store();
        // 先放一份合法现值，验证失败时原样保留（copy-on-write 不得半途污染）
        store
            .update(RoutingPolicy {
                enabled: true,
                profiles: RoutingProfiles {
                    global: RoutingProfile {
                        strategy: Some(RouteStrategy::Ranked),
                        ..RoutingProfile::default()
                    },
                    ..RoutingProfiles::default()
                },
                ..RoutingPolicy::default()
            })
            .unwrap();

        for kind in ["models", "groups", "providers", "accounts", "sort_rules"] {
            let err = replace_routing_profiles(&store, Some(over_limit(kind)))
                .expect_err("超限必须拒绝写入");
            assert_eq!(
                err.0,
                axum::http::StatusCode::BAD_REQUEST,
                "{kind} 映射 400"
            );
            assert!(
                err.1.contains("超限"),
                "{kind} 的报错应含「超限」：{}",
                err.1
            );
            // 现值原样：enabled / strategy 保留、档案数不变
            let snap = store.snapshot();
            assert!(snap.enabled, "拒绝写入不得波及 enabled");
            assert_eq!(snap.profiles.global.strategy, Some(RouteStrategy::Ranked));
            assert!(snap.profiles.models.is_empty());
            assert!(snap.profiles.groups.is_empty());
        }
    }

    /// #40 §3 写入分工闸门：本端点只换 profiles 本体，**enabled 保持原值**
    /// （update 是整份替换，漏掉读-改-写就会把总开关冲掉）。
    #[test]
    fn replace_routing_profiles_swaps_body_and_keeps_enabled() {
        let store = store();
        store
            .update(RoutingPolicy {
                enabled: true,
                profiles: RoutingProfiles {
                    groups: vec![group("old", Vec::new())],
                    ..RoutingProfiles::default()
                },
                ..RoutingPolicy::default()
            })
            .unwrap();

        let mut next = RoutingProfiles::default();
        next.models.insert(
            "gpt-4o".to_string(),
            RoutingProfile {
                strategy: Some(RouteStrategy::RoundRobin),
                ..RoutingProfile::default()
            },
        );
        let out = replace_routing_profiles(&store, Some(next)).unwrap();
        assert!(out.enabled, "enabled 不是本端点的写入口，必须保留现值");
        assert!(out.profiles.groups.is_empty(), "整份替换：旧组不残留");
        assert_eq!(
            out.profiles.models.get("gpt-4o").map(|p| p.strategy),
            Some(Some(RouteStrategy::RoundRobin))
        );
        // 快照热更：下一次 snapshot() 读到的就是新档案
        assert!(store.snapshot().profiles.models.contains_key("gpt-4o"));
    }

    /// #40 §3 缺省语义（契约口径：提供哪份改哪份）：请求不带 profiles
    /// （`None`）= 什么都不改，既不清空也不动 enabled。
    #[test]
    fn replace_routing_profiles_absent_profiles_keeps_current() {
        let store = store();
        store
            .update(RoutingPolicy {
                enabled: true,
                profiles: RoutingProfiles {
                    groups: vec![group("old", Vec::new())],
                    ..RoutingProfiles::default()
                },
                ..RoutingPolicy::default()
            })
            .unwrap();
        let out = replace_routing_profiles(&store, None).unwrap();
        assert!(out.enabled);
        assert_eq!(out.profiles.groups.len(), 1);
        assert_eq!(out.profiles.groups[0].id, "old");
    }
}
