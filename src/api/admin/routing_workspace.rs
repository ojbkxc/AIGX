//! 路由工作台（workspace / flow / trace）— 对照 Agent2API v3.5.0-beta.1
//! `api/routing_workspace.rs`、`api/routing_flow.rs`、`api/routing_trace.rs`
//! 与 `request_stats/{routing,flow,trace}.rs`。
//!
//! AIGX 没有 A2A 的 SQL 统计库：本模块全部聚合在既有数据源上做，冷路径、
//! 有界扫描：
//!
//! - `RequestLogStore`（FileStore KV，`reqlog:{ts}:{id}`）：终态请求日志
//! - `InflightRegistry`（进程内在途表）：running 维度
//! - `RoutingPolicyStore` / `ChannelStore` / `AccountSessionStore`：覆盖对比与 provider 行
//!
//! ## 端点
//!
//! - `GET /api/routing/workspace?model=`：模型覆盖（effective vs inherited）+
//!   实时指标 + provider 行
//! - `GET /api/routing/flow?minutes=&model=&scope=&client=&provider=&account=&detail=&offset=&limit=`
//! - `GET /api/routing/trace?model=&search=&status=&start=&end=&offset=&limit=`
//! - `GET /api/routing/trace/request?id=&ts=`
//! - `GET /api/routing/replays?model=&status=&offset=&limit=`（回放列表；单条详情
//!   仍走既有 `GET /api/routing/replay/:request_id`）
//!
//! 响应统一 `{success:true,data:{...}}`（与 A2A `ok_json` 及 AIGX 既有
//! `/api/routing/sticky*` 同族）。
//!
//! ## 与 A2A 的诚实差异（数据源决定，不造假数据）
//!
//! | A2A 维度 | AIGX 现状 |
//! |---|---|
//! | `client_model` | 无此字段；用 `origin_model`（用户原始请求名）兜底 `model` |
//! | 账号维度（`accountRunning` / flow `account`） | `RequestLog` 无 account_id；`AttemptDetail.account` 恒空串 → 恒空/过滤为空 |
//! | 会话维度（flow `sessions`） | `RequestLog` 无 session_id（`route_config` 只有 strategy/sticky_enabled）→ `supported:false` |
//! | `stickyHit` | 终态日志无该布尔位；仅 `route_replay` 开启时 `attempt_details[].sticky_hit` 可见 |
//! | 在途归因（client/provider/channel） | `InflightEntry` 只有 model/user/key；`set_channel` 数据面未接线 → provider/channelId 恒空 |
//! | `revision` CAS | AIGX 无配置版本号；写入口是既有 `PUT /api/routing/*`（整份保存），**本模块不造重复写端点** |
//! | 时间戳单位 | `RequestLog.created_at` / `InflightEntry.started_at` 为 unix **秒**（A2A 毫秒）。本模块：`createdAt` 给秒，`ts` / `lastSeen` / `now` / `since` / `updatedAt` 统一给**毫秒**（前端 `new Date()` 直接渲染），秒值另给 `nowSeconds` / `sinceSeconds` |

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::{json, Map as JsonMap, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::account_session::AccountSession;
use crate::channel::routing_policy::{
    plan_route, resolve_profile, ResolvedProfile, RoutingProfile, RoutingProfiles,
};
use crate::channel::ChannelType;
use crate::log::inflight::InflightEntry;
use crate::log::{RequestLog, RequestLogStore};

// ── 有界口径（冷路径护栏：宁可标注 sampled，也不扫穿）──────────────────

/// 工作台「今日」指标的最大扫描行数
const WORKSPACE_TODAY_MAX_ROWS: usize = 20_000;
/// flow 时间窗聚合的最大扫描行数
const FLOW_WINDOW_MAX_ROWS: usize = 10_000;
/// trace / replays 的最大扫描行数（只取最新 N 条）
const TRACE_SCAN_MAX: usize = 5_000;
/// 回放列表的最大扫描行数
const REPLAY_SCAN_MAX: usize = 5_000;
/// flow paths 单次返回的分组上限（A2A 同为 300）
const FLOW_PATH_MAX: usize = 300;
/// flow models 下拉单次返回的模型上限（A2A 同为 300）
const FLOW_MODEL_MAX: usize = 300;
/// 分页单页上限（与 RequestLogStore::PAGE_SIZE_MAX 一致）
const PAGE_LIMIT_MAX: usize = 100;
/// 分页默认页大小（A2A 同为 30）
const PAGE_LIMIT_DEFAULT: usize = 30;
/// 工作台模型清单上限（防大目录撑爆响应）
const MAX_WORKSPACE_MODELS: usize = 500;
/// 粘滞计数快照上限
const STICKY_SNAPSHOT_LIMIT: usize = 5_000;

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<Value>) {
    error_response(&message.into(), StatusCode::BAD_REQUEST)
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 报表时区下的「今天 00:00:00」unix 秒。
///
/// 复用报表页的 [`super::dashboard::date_start_ts`]（同一 `[report]`
/// timezone_offset 口径 + 同一日键函数），保证工作台指标与报表页的「今日」
/// 是同一个自然日，不另造第二套日界算法。
pub(crate) fn today_start_ts(now: i64) -> i64 {
    let offset = crate::config::report_tz_offset();
    let date = crate::log::daily_agg::date_key_at(now, offset);
    super::dashboard::date_start_ts(&date)
}

// ── 日志扫描（键范围裁剪 + 有界反序列化）──────────────────────────────

/// 时间窗 `[start, end]` 内**最新**的至多 `cap` 条日志（倒序）+ 窗口内
/// 键总数 + 是否截断。
///
/// 为什么不用 `list_with_filter`：它按页截断且 `model` 只精确匹配映射后的
/// `model`（不认 `origin_model`），做聚合会漏请求；`list_window` 又是**升序**
/// 取前 N 条（超限时丢最新）。这里先按键升序列表（只列键、不反序列化），
/// 按键内时间戳裁剪窗口后**倒序**取最新 N 条，再批量反序列化 —— 与日志存储
/// 同源、有界、且「丢的是最旧而不是最新」。
pub(crate) fn window_logs_desc(
    store: &RequestLogStore,
    start: i64,
    end: i64,
    cap: usize,
) -> WindowScan {
    // 键区间 + DESC + LIMIT 全部在存储层完成：不再先枚举全部 reqlog 键
    // （日志保留量 10 万时那是 10 万个 String + 一次全表扫）。
    let total = store.count_window(start, end).unwrap_or(0);
    let truncated = total > cap;
    let picked = store.list_range_desc(start, end, cap).unwrap_or_default();
    let mut logs = store.logs_by_keys(&picked);
    logs.sort_by_key(|log| std::cmp::Reverse(log.created_at));
    WindowScan {
        logs,
        total,
        truncated,
    }
}

pub(crate) struct WindowScan {
    pub logs: Vec<RequestLog>,
    /// 窗口内键总数（可能 > logs.len()）
    pub total: usize,
    pub truncated: bool,
}

// ── 请求日志口径小工具 ───────────────────────────────────────────────

/// 用户请求名：`origin_model` 非空优先，否则 `model`（A2A `client_model` 的
/// 等价物；A2A 是 `COALESCE(NULLIF(client_model,''), model)`）。
pub(crate) fn request_model(log: &RequestLog) -> &str {
    match log.origin_model.as_deref() {
        Some(model) if !model.is_empty() => model,
        _ => &log.model,
    }
}

/// 模型匹配：映射后名与用户原始名任一命中（大小写不敏感）。
pub(crate) fn matches_model(log: &RequestLog, model: &str) -> bool {
    log.model.eq_ignore_ascii_case(model)
        || log
            .origin_model
            .as_deref()
            .is_some_and(|origin| origin.eq_ignore_ascii_case(model))
}

fn log_is_success(log: &RequestLog) -> bool {
    (200..300).contains(&log.status_code) && log.error_msg.is_none()
}

fn has_route_replay(log: &RequestLog) -> bool {
    log.route_config.is_some()
        || !log.candidate_channels.is_empty()
        || !log.attempt_details.is_empty()
        || log
            .selected_channel
            .as_deref()
            .is_some_and(|selected| !selected.is_empty())
}

// ── 渠道元信息（provider 归因）──────────────────────────────────────

#[derive(Debug, Clone)]
pub(crate) struct ChannelMeta {
    pub name: String,
    pub provider: String,
}

/// channel_id → (渠道名, provider)。provider 口径单点：
/// [`crate::log::provider_label_for_channel`]（渠道型取 `channel_type`；账号型
/// 取账号池声明的 provider，拿不到回落 `"account"`）。
///
/// 与请求日志的 `provider` 列**必须同源**：工作台与日志页对同一渠道显示两个
/// 供应商是不可接受的，故这里不再自建换算。
pub(crate) fn channel_meta_map(state: &AppState) -> HashMap<String, ChannelMeta> {
    state
        .channel_store
        .snapshot()
        .iter()
        .map(|channel| {
            let account_provider = if channel.channel_type == ChannelType::Account {
                state.account_sessions.provider_for_channel(&channel.id)
            } else {
                None
            };
            let provider = crate::log::provider_label_for_channel(
                channel.channel_type,
                account_provider.as_deref(),
            );
            (
                channel.id.clone(),
                ChannelMeta {
                    name: channel.name.clone(),
                    provider,
                },
            )
        })
        .collect()
}

pub(crate) fn provider_of<'a>(
    meta: &'a HashMap<String, ChannelMeta>,
    log: &'a RequestLog,
) -> &'a str {
    log.channel_id
        .as_deref()
        .and_then(|id| meta.get(id))
        .map(|entry| entry.provider.as_str())
        .unwrap_or("")
}

fn resolved_channel_name(meta: &HashMap<String, ChannelMeta>, log: &RequestLog) -> String {
    log.channel_id
        .as_deref()
        .and_then(|id| meta.get(id))
        .map(|entry| entry.name.clone())
        .or_else(|| log.channel_name.clone())
        .unwrap_or_default()
}

fn provider_of_id(meta: &HashMap<String, ChannelMeta>, channel_id: Option<&str>) -> String {
    channel_id
        .and_then(|id| meta.get(id))
        .map(|entry| entry.provider.clone())
        .unwrap_or_default()
}

// ── GET /api/routing/workspace ──────────────────────────────────────

/// GET /api/routing/workspace?model= — 模型覆盖对比 + 实时指标 + provider 行。
///
/// - `profile`：当前生效档案（逐字段三层解析，`policy.enabled=false` 时为基座）
/// - `inheritedProfile`：去掉**模型级**档案后的解析结果（回答「这个模型单独
///   改了什么」；无模型级档案时与 `profile` 相同）
/// - `providers`：provider 行（渠道/账号计数、是否目录内置、对当前模型是否可用）
/// - `metrics`：今日请求/成功率/平均延迟（`RequestLogStore`）+ 在途数
///   （`InflightRegistry`）；`accountRunning` 恒空（见模块头差异表）
pub async fn handle_get_routing_workspace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = params
        .get("model")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let model_name = model.as_deref().unwrap_or("");
    if model_name.len() > 256 {
        return Err(bad_request("model 不能超过 256 字符"));
    }

    let now = now_secs();
    let policy = state.routing_policy.snapshot();

    let effective = resolve_profile(&policy, model_name, &state.model_mapper, None);
    let has_model_layer = !model_name.is_empty()
        && model_layer_present(&policy.profiles, &state.model_mapper, model_name);
    let inherited = if has_model_layer {
        // 去掉模型级层后重放同一解析序列（global + 组层保留）
        let mut stripped = (*policy).clone();
        stripped.profiles.models.clear();
        resolve_profile(&stripped, model_name, &state.model_mapper, None)
    } else {
        effective.clone()
    };

    // 今日指标：与报表页同一「今天」口径
    let day_start = today_start_ts(now);
    let scan = window_logs_desc(
        &state.log_store.requests,
        day_start,
        now,
        WORKSPACE_TODAY_MAX_ROWS,
    );
    let mut today_requests = 0u64;
    let mut successful = 0u64;
    let mut duration_sum = 0u64;
    for log in &scan.logs {
        if model
            .as_deref()
            .is_some_and(|want| !matches_model(log, want))
        {
            continue;
        }
        today_requests += 1;
        if log_is_success(log) {
            successful += 1;
            duration_sum += log.latency_ms;
        }
    }
    let today_success_rate = if today_requests > 0 {
        Some((successful as f64 * 100.0 / today_requests as f64 * 100.0).round() / 100.0)
    } else {
        None
    };
    let average_duration_ms = (successful > 0).then(|| duration_sum / successful);

    // 在途（进程内实时表）：只有 model/user/key 维度
    let running_entries: Vec<InflightEntry> = state
        .inflight
        .list()
        .into_iter()
        .filter(|entry| {
            model
                .as_deref()
                .is_none_or(|want| entry.model.eq_ignore_ascii_case(want))
        })
        .collect();
    let mut channel_running: JsonMap<String, Value> = JsonMap::new();
    for entry in &running_entries {
        if let Some(channel_id) = entry.channel_id.as_deref() {
            let next = channel_running
                .get(channel_id)
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            channel_running.insert(channel_id.to_string(), Value::from(next));
        }
    }

    // 粘滞条目计数（快照有界；超限以 stickyTruncated 标注）
    let (sticky_entries, sticky_total) = state
        .channel_store
        .affinity_cache()
        .snapshot(STICKY_SNAPSHOT_LIMIT);
    let sticky_count = match model.as_deref() {
        Some(want) => sticky_entries
            .iter()
            .filter(|entry| entry.model.eq_ignore_ascii_case(want))
            .count(),
        None => sticky_entries.len(),
    };

    let settings = json!({
        "strategy": policy.strategy,
        "sticky": policy.sticky,
        "breaker": policy.breaker,
        // 前端 `RoutingPolicyItem.scheduler_weights`（snake_case，与 GET /api/routing 同键）
        "scheduler_weights": policy.scheduler_weights,
    });

    let metrics = json!({
        "available": true,
        "running": running_entries.len(),
        "todayRequests": today_requests,
        "todaySuccessRate": today_success_rate,
        "averageDurationMs": average_duration_ms,
        // AIGX RequestLog / Inflight 均无账号维度 → 恒空（不造数）
        "accountRunning": {},
        // InflightEntry 有 channel_id 字段但数据面未接线 set_channel → 现阶段恒空
        "channelRunning": channel_running,
        "sampled": scan.truncated,
        // 指标窗口（unix **毫秒**，前端 `new Date()` 直接渲染；A2A 同为毫秒）
        "windowStartMs": day_start.saturating_mul(1000),
        "windowEndMs": now.saturating_mul(1000),
    });

    Ok(Json(json!({
        "success": true,
        "data": {
            "model": model,
            "customized": has_model_layer,
            "profilesEnabled": policy.enabled,
            "globalCustomized": policy.profiles.global != RoutingProfile::default(),
            "configuredModels": policy.profiles.models.keys().cloned().collect::<Vec<_>>(),
            "models": available_models(&state, &policy.profiles),
            "profile": profile_json(&effective),
            "inheritedProfile": profile_json(&inherited),
            // 前端 RoutingWorkspace 读顶层 provenance（逐字段来源层）
            "provenance": effective.provenance,
            "appliedLayers": effective.provenance.applied_layers(),
            "providers": provider_rows(&state, model.as_deref(), &policy.profiles, now),
            // 候选队列：与 GET /api/routing/queue 共用同一投影（不造第二套）；
            // 无模型时不给 queue（与 A2A 一致：queue 需要 model 才有意义）
            "queue": model.as_deref().map(|model| queue_snapshot(&state, model)),
            "settings": settings,
            // AIGX 只有一份策略级 settings；模型级差异在 profile 里逐字段给出
            "globalSettings": settings,
            // AIGX 无 revision CAS（写入口为既有 PUT /api/routing*）
            "revision": Value::Null,
            "revisionSupported": false,
            "metrics": metrics,
            "stickyCount": sticky_count,
            "stickyTruncated": sticky_total > sticky_entries.len(),
            "updatedAt": now.saturating_mul(1000),
        }
    })))
}

/// 工作台候选队列：复用 `/api/routing/queue` 的同一判定序列与投影 helper
/// （`resolve_profile` → `select_for_model_by_strategy` → `plan_route` →
/// `queue_projection`），保证工作台看到的队首与真实首挑同口径。
fn queue_snapshot(state: &AppState, model: &str) -> Value {
    let policy = state.routing_policy.snapshot();
    let profile = resolve_profile(&policy, model, &state.model_mapper, None);
    let ordered = state.channel_store.select_for_model_by_strategy(
        model,
        profile.strategy,
        &state.pricing_store,
    );
    let plan = plan_route(&policy, &profile, &ordered);
    let breaker_map = state.channel_store.circuit_breaker().get_status_map();
    let (ordered_json, blocked_json) = super::routing::queue_projection(state, &plan, &breaker_map);
    json!({
        "strategy": profile.strategy,
        "sticky": profile.sticky,
        "model": model,
        "pickedId": plan.first().map(|channel| channel.id.clone()),
        "ordered": ordered_json,
        "blocked": blocked_json,
        "profilesEnabled": policy.enabled,
        "appliedLayers": profile.provenance.applied_layers(),
        "skipped": plan.skipped,
    })
}

fn model_layer_present(
    profiles: &RoutingProfiles,
    mapper: &crate::model::ModelMapper,
    model: &str,
) -> bool {
    if profiles.models.contains_key(model) {
        return true;
    }
    mapper.resolve_with(model, |mapped| {
        mapped != model && profiles.models.contains_key(mapped)
    })
}

fn profile_json(profile: &ResolvedProfile) -> Value {
    // 字段名用 snake_case：前端 `RoutingProfile`（types/routing-profile.ts）
    // 的六字段就是这套键，`profileFieldText` 逐字段读 `sort_rules` /
    // `default_group`（camelCase 会让这两行恒显「—」）。
    json!({
        "scope": profile.scope,
        "strategy": profile.strategy,
        "sticky": profile.sticky,
        "needs": profile.needs,
        "sort_rules": profile.sort_rules,
        "default_group": profile.default_group,
    })
}

/// provider 展示名：账号目录 `ProviderSpec::label` → 渠道协议域兜底 → 裸 id。
///
/// 空 id **先短路**：`registry::find("")` 会回落默认 kind（openai），把
/// 「无归因」错标成 OpenAI（前端会显示出一个不存在的供应商）。
fn provider_label(id: &str) -> String {
    if id.trim().is_empty() {
        return String::new();
    }
    crate::account_session::registry::find(id)
        .map(|spec| spec.label.to_string())
        .or_else(|| fallback_provider_label(id).map(str::to_string))
        .unwrap_or_else(|| id.to_string())
}

fn fallback_provider_label(id: &str) -> Option<&'static str> {
    match id {
        "openai_compatible" => Some("OpenAI 兼容"),
        "cloudflare" => Some("Cloudflare"),
        "plugin" => Some("插件"),
        _ => None,
    }
}

#[derive(Default)]
struct ProviderAgg {
    channel_count: usize,
    account_count: usize,
    available: bool,
}

/// provider 行：AIGX 的 provider 身份 = 渠道协议域（非账号型渠道）或账号池
/// 声明的 provider（账号型渠道）。
fn provider_rows(
    state: &AppState,
    model: Option<&str>,
    profiles: &RoutingProfiles,
    now: i64,
) -> Vec<Value> {
    let mut map: BTreeMap<String, ProviderAgg> = BTreeMap::new();
    let channels = state.channel_store.snapshot();
    let accounts: Vec<AccountSession> = state.account_sessions.all();

    for account in &accounts {
        if account.provider.is_empty() {
            continue;
        }
        let agg = map.entry(account.provider.clone()).or_default();
        agg.account_count += 1;
        let enabled = account.status == "enabled" && !account.needs_relogin();
        let not_expired = account
            .expires_at
            .is_none_or(|expires_at| expires_at == 0 || expires_at > now);
        let model_ok = model.is_none_or(|want| {
            account.models.is_empty()
                || account
                    .models
                    .iter()
                    .any(|declared| declared.eq_ignore_ascii_case(want))
        });
        if enabled && not_expired && model_ok {
            agg.available = true;
        }
    }

    for channel in channels.iter() {
        if channel.channel_type == ChannelType::Account {
            // 账号型渠道只是容器：provider 行由账号池决定，可用性也由账号判定
            let mut seen: HashSet<String> = HashSet::new();
            for account in accounts
                .iter()
                .filter(|account| account.channel_id == channel.id && !account.provider.is_empty())
            {
                if seen.insert(account.provider.clone()) {
                    let agg = map.entry(account.provider.clone()).or_default();
                    agg.channel_count += 1;
                }
            }
            continue;
        }
        let provider = channel.channel_type.as_str().to_string();
        let agg = map.entry(provider).or_default();
        agg.channel_count += 1;
        if channel.is_enabled() && model.is_none_or(|want| channel.supports_model(want)) {
            agg.available = true;
        }
    }

    // 档案里配置过但当前没有渠道/账号的 provider 也列出（available=false），
    // 否则管理员写了无效 provider 名在页面上完全不可见。
    for provider in profile_scope_providers(profiles) {
        map.entry(provider).or_default();
    }

    map.into_iter()
        .map(|(id, agg)| {
            let spec = crate::account_session::registry::find(&id);
            let custom = spec.is_none() && fallback_provider_label(&id).is_none();
            json!({
                "id": id,
                "label": provider_label(&id),
                "channelCount": agg.channel_count,
                "accountCount": agg.account_count,
                "custom": custom,
                "available": agg.available,
            })
        })
        .collect()
}

fn profile_scope_providers(profiles: &RoutingProfiles) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    {
        let mut push = |provider: &str| {
            if !provider.is_empty() && !out.iter().any(|seen| seen == provider) {
                out.push(provider.to_string());
            }
        };
        for layer in std::iter::once(&profiles.global).chain(profiles.models.values()) {
            if let Some(scope) = &layer.scope {
                for provider in scope
                    .providers
                    .iter()
                    .chain(scope.disabled_providers.iter())
                {
                    push(provider);
                }
            }
        }
    }
    out
}

/// 工作台模型清单：渠道声明/发现 + 映射别名与目标 + 自定义模型 + 价格表 +
/// 模型级档案键；排除「由路由组服务」的模型（A2A 同口径：组模型在组面板编辑）。
fn available_models(state: &AppState, profiles: &RoutingProfiles) -> Vec<String> {
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut push = |model: &str| {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                map.entry(trimmed.to_ascii_lowercase())
                    .or_insert_with(|| trimmed.to_string());
            }
        };
        for channel in state.channel_store.snapshot().iter() {
            for model in channel.effective_models() {
                push(model);
            }
        }
        for entry in state.model_mapper.all_mapping_entries() {
            push(&entry.alias);
            push(&entry.target);
        }
        for entry in state.model_mapper.custom_models() {
            push(&entry.id);
        }
        for price in state.pricing_store.list_prices() {
            push(&price.model_name);
        }
        for key in profiles.models.keys() {
            push(key);
        }
    }
    let group_models: HashSet<String> = profiles
        .groups
        .iter()
        .filter(|group| !group.model.is_empty())
        .map(|group| group.model.to_ascii_lowercase())
        .collect();
    let mut models: Vec<String> = map
        .into_iter()
        .filter(|(key, _)| !group_models.contains(key))
        .map(|(_, display)| display)
        .collect();
    models.sort_by_key(|model| model.to_ascii_lowercase());
    models.truncate(MAX_WORKSPACE_MODELS);
    models
}

// ── GET /api/routing/flow ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowScope {
    Window,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowDetail {
    Requests,
    Sessions,
    Models,
}

/// 时间窗校验（A2A 口径）：仅 5/10/30/60；缺省/非数字回落 10。
pub(crate) fn parse_flow_minutes(raw: Option<&str>) -> Result<i64, String> {
    let minutes = raw
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(10);
    if [5, 10, 30, 60].contains(&minutes) {
        Ok(minutes)
    } else {
        Err("时间范围必须为 5、10、30 或 60 分钟".to_string())
    }
}

pub(crate) fn parse_flow_scope(raw: &str) -> Result<FlowScope, String> {
    match raw {
        "" | "window" => Ok(FlowScope::Window),
        "running" => Ok(FlowScope::Running),
        other => Err(format!(
            "未知的请求活动范围：{other}（可选 window / running）"
        )),
    }
}

/// flow `detail` 三档下钻。前端发的是 `paths`（A2A `paths` 无 detail 时的默认名），
/// 这里把 `""` / `paths` 都归到「路径聚合」，其余三档同名映射。
pub(crate) fn parse_flow_detail(raw: &str) -> Result<Option<FlowDetail>, String> {
    match raw {
        "" | "paths" => Ok(None),
        "requests" => Ok(Some(FlowDetail::Requests)),
        "sessions" => Ok(Some(FlowDetail::Sessions)),
        "models" => Ok(Some(FlowDetail::Models)),
        other => Err(format!(
            "未知的流向详情类型：{other}（可选 paths / requests / sessions / models）"
        )),
    }
}

/// 分页解析：offset 非法回落 0；limit 缺省 30、夹在 1..=100（A2A 同口径）。
pub(crate) fn parse_paging(offset: Option<&str>, limit: Option<&str>) -> (usize, usize) {
    let offset = offset
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let limit = limit
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(PAGE_LIMIT_DEFAULT)
        .clamp(1, PAGE_LIMIT_MAX);
    (offset, limit)
}

fn slice_page<'a>(rows: &[&'a RequestLog], offset: usize, limit: usize) -> Vec<&'a RequestLog> {
    rows.iter().skip(offset).take(limit).copied().collect()
}

/// GET /api/routing/flow — 请求流向（window / running 两 scope；detail 三档下钻）。
pub async fn handle_get_routing_flow(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let minutes =
        parse_flow_minutes(params.get("minutes").map(String::as_str)).map_err(bad_request)?;
    let scope_raw = params.get("scope").map(|value| value.trim()).unwrap_or("");
    let scope = parse_flow_scope(scope_raw).map_err(bad_request)?;
    let activity = params
        .get("activity")
        .map(|value| value.trim())
        .unwrap_or("");
    if !["", "all", "running", "idle"].contains(&activity) {
        return Err(bad_request("未知的请求活动范围"));
    }
    let detail_raw = params.get("detail").map(|value| value.trim()).unwrap_or("");
    let detail = parse_flow_detail(detail_raw).map_err(bad_request)?;

    let model = params
        .get("model")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let client = params
        .get("client")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let provider = params
        .get("provider")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let account = params
        .get("account")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if model.is_some_and(|value| value.len() > 1024)
        || client.is_some_and(|value| value.len() > 256)
        || provider.is_some_and(|value| value.len() > 256)
        || account.is_some_and(|value| value.len() > 256)
    {
        return Err(bad_request("流向查询条件过长"));
    }

    let (offset, limit) = parse_paging(
        params.get("offset").map(String::as_str),
        params.get("limit").map(String::as_str),
    );
    let filters = FlowFilters {
        model: model.map(str::to_string),
        client: client.map(str::to_string),
        provider: provider.map(str::to_string),
        account: account.map(str::to_string),
    };
    let now = now_secs();
    let data = match scope {
        FlowScope::Window => flow_window(&state, &filters, detail, offset, limit, now, minutes),
        FlowScope::Running => flow_running(&state, &filters, detail, offset, limit, now),
    };
    Ok(Json(json!({ "success": true, "data": data })))
}

#[derive(Debug, Default, Clone)]
pub(crate) struct FlowFilters {
    model: Option<String>,
    client: Option<String>,
    provider: Option<String>,
    account: Option<String>,
}

pub(crate) fn filter_flow_logs<'a>(
    logs: &'a [RequestLog],
    meta: &HashMap<String, ChannelMeta>,
    filters: &FlowFilters,
) -> Vec<&'a RequestLog> {
    logs.iter()
        .filter(|log| {
            if let Some(model) = filters.model.as_deref() {
                if !matches_model(log, model) {
                    return false;
                }
            }
            if let Some(client) = filters.client.as_deref() {
                // `__unknown` = 「没读到客户端」这一档（空名）。判据走
                // `log::normalize_client_name`：存量里旧版本写过的 `unknown`
                // 与空名是同一档，不能在这里分裂成两个筛选项。
                let hit = if client == "__unknown" {
                    crate::log::normalize_client_name(&log.client_name).is_empty()
                } else {
                    log.client_name.eq_ignore_ascii_case(client)
                };
                if !hit {
                    return false;
                }
            }
            if let Some(provider) = filters.provider.as_deref() {
                if !provider_of(meta, log).eq_ignore_ascii_case(provider) {
                    return false;
                }
            }
            // AIGX RequestLog 无 account_id：account 维度恒不命中（不造假数据）
            !filters
                .account
                .as_deref()
                .is_some_and(|value| !value.is_empty())
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FlowPathKey {
    client: String,
    model: String,
    provider: String,
    channel_id: String,
    channel_name: String,
}

#[derive(Debug, Clone, Copy, Default)]
struct FlowPathAgg {
    requests: u64,
    successful: u64,
    failed: u64,
    switched: u64,
    /// 粘滞命中请求数（仅 `route_replay` 捕获的 attempt_details 可见；
    /// 开关关闭时恒 0 —— 是「无观测」不是「没命中」，路径行带 `stickyObserved` 标注）
    sticky_hits: u64,
    sticky_observed: u64,
    last_seen: i64,
}

/// 路径聚合：按 (client, 请求模型, provider, 渠道) 分组。返回（行, 分组总数）。
///
/// `running_counts`：channel_id → 在途数（`InflightRegistry` 里 channel_id
/// 只有在数据面接线 `set_channel` 后才非空；未接线时全 0）。
pub(crate) fn flow_paths(
    logs: &[&RequestLog],
    meta: &HashMap<String, ChannelMeta>,
    running_counts: &HashMap<String, u64>,
) -> (Vec<Value>, usize) {
    let mut groups: BTreeMap<FlowPathKey, (FlowPathAgg, String)> = BTreeMap::new();
    for log in logs {
        let key = FlowPathKey {
            // 分组键与筛选/展示同一判据（没读到 = 空名），见 `log::normalize_client_name`
            client: crate::log::normalize_client_name(&log.client_name).to_string(),
            model: request_model(log).to_string(),
            provider: provider_of(meta, log).to_string(),
            channel_id: log.channel_id.clone().unwrap_or_default(),
            channel_name: resolved_channel_name(meta, log),
        };
        let entry = groups.entry(key).or_default();
        entry.0.requests += 1;
        if log_is_success(log) {
            entry.0.successful += 1;
        } else {
            entry.0.failed += 1;
        }
        if log.attempts > 1 {
            entry.0.switched += 1;
        }
        if has_route_replay(log) {
            entry.0.sticky_observed += 1;
            if log.attempt_details.iter().any(|attempt| attempt.sticky_hit) {
                entry.0.sticky_hits += 1;
            }
        }
        if log.created_at >= entry.0.last_seen {
            entry.0.last_seen = log.created_at;
            if !log.client_version.is_empty() {
                entry.1 = log.client_version.clone();
            }
        }
    }
    let path_count = groups.len();
    let mut rows: Vec<(FlowPathKey, FlowPathAgg, String)> = groups
        .into_iter()
        .map(|(key, (agg, version))| (key, agg, version))
        .collect();
    rows.sort_by(|left, right| {
        let (left_agg, right_agg) = (left.1, right.1);
        right_agg
            .requests
            .cmp(&left_agg.requests)
            .then(right_agg.last_seen.cmp(&left_agg.last_seen))
            .then(left.0.channel_id.cmp(&right.0.channel_id))
    });
    let paths = rows
        .into_iter()
        .take(FLOW_PATH_MAX)
        .map(|(key, agg, version)| {
            json!({
                // A2A 的 id 是分组身份的 JSON 数组串；前端只做原样回传/标识
                "id": json!([key.client, key.model, key.provider, key.channel_id]).to_string(),
                "client": key.client,
                // AIGX 的 client_name 已是规范化展示名（如 "Claude Code"）
                "clientLabel": key.client,
                "clientVersion": version,
                "model": key.model,
                "provider": key.provider,
                "providerLabel": provider_label(&key.provider),
                "channelId": key.channel_id,
                "channelName": key.channel_name,
                "requests": agg.requests,
                // 在途数按渠道回填（InflightRegistry.channel_id 未接线时恒 0）
                "running": running_counts.get(&key.channel_id).copied().unwrap_or(0),
                "successful": agg.successful,
                "failed": agg.failed,
                // 无会话标识（见模块头差异表）
                "sessions": 0,
                // 粘滞命中：仅 route_replay 捕获的请求可观测（stickyObserved 是分母）
                "stickyHits": agg.sticky_hits,
                "stickyObserved": agg.sticky_observed,
                "switched": agg.switched,
                "lastSeen": agg.last_seen.saturating_mul(1000),
            })
        })
        .collect();
    (paths, path_count)
}

/// detail=models：窗口内真实出现过请求的模型（按请求数降序、同数按名升序）。
pub(crate) fn flow_models(logs: &[&RequestLog]) -> (Vec<String>, usize) {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for log in logs {
        let model = request_model(log);
        if !model.is_empty() {
            *counts.entry(model.to_string()).or_insert(0) += 1;
        }
    }
    let model_count = counts.len();
    let mut rows: Vec<(String, u64)> = counts.into_iter().collect();
    rows.sort_by(|left, right| {
        right.1.cmp(&left.1).then(
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase()),
        )
    });
    let models = rows
        .into_iter()
        .take(FLOW_MODEL_MAX)
        .map(|(model, _)| model)
        .collect();
    (models, model_count)
}

/// 请求行（flow `detail=requests` 与 trace 列表共用）。
///
/// 字段名按前端 `RoutingTraceRow`（types/routing-workspace.ts）对齐：
/// `status` 是**数字**（A2A `entry_json` 同口径；0 = 在途/未定论），
/// `ts` / `durationMs` 是它的时间与耗时别名，`clientModel` / `upstreamModel`
/// 分别对应用户请求名与映射后的上游真名（AIGX 无独立的 `upstream_model` 字段，
/// 用 `model` 承担该语义 —— 见 `RequestLog::model` 文档）。
fn request_row_json(log: &RequestLog, meta: &HashMap<String, ChannelMeta>) -> Value {
    let channel_name = resolved_channel_name(meta, log);
    let provider = provider_of(meta, log).to_string();
    json!({
        "id": log.id,
        "traceKey": log.id,
        "requestId": log.request_id,
        "model": log.model,
        "originModel": log.origin_model,
        "clientModel": request_model(log),
        "upstreamModel": log.model,
        "channelId": log.channel_id,
        "channelName": if channel_name.is_empty() { Value::Null } else { Value::String(channel_name) },
        "provider": provider,
        "providerLabel": provider_label(&provider),
        "client": crate::log::normalize_client_name(&log.client_name),
        "clientLabel": crate::log::normalize_client_name(&log.client_name),
        "clientVersion": log.client_version,
        // 数字状态码（0 = 未定论/在途，A2A 同口径）；成功/失败判定另见 `ok`
        "status": log.status_code,
        "statusCode": log.status_code,
        "ok": log_is_success(log),
        "error": log.error_msg,
        "latencyMs": log.latency_ms,
        "durationMs": log.latency_ms,
        "firstResponseMs": log.first_response_ms,
        "attempts": log.attempts,
        "phase": log.phase,
        "inputTokens": log.input_tokens,
        "outputTokens": log.output_tokens,
        "cost": log.cost,
        "cacheHit": log.cache_hit,
        // createdAt 保留 AIGX 原生 unix 秒；ts 为毫秒（前端 new Date 直接渲染）
        "createdAt": log.created_at,
        "ts": log.created_at.saturating_mul(1000),
    })
}

fn flow_window(
    state: &AppState,
    filters: &FlowFilters,
    detail: Option<FlowDetail>,
    offset: usize,
    limit: usize,
    now: i64,
    minutes: i64,
) -> Value {
    let since = now - minutes * 60;
    let scan = window_logs_desc(&state.log_store.requests, since, now, FLOW_WINDOW_MAX_ROWS);
    let meta = channel_meta_map(state);
    let filtered = filter_flow_logs(&scan.logs, &meta, filters);
    let matched = filtered.len();
    let mut unsupported: Vec<&str> = Vec::new();
    if filters.account.is_some() {
        // AIGX RequestLog 无账号字段 → 该过滤维度不可兑现
        unsupported.push("account");
    }
    // 在途请求只有 model 维度可归因（InflightRegistry 无 client/provider/account）
    let running_entries = state
        .inflight
        .list()
        .into_iter()
        .filter(|entry| {
            filters
                .model
                .as_deref()
                .is_none_or(|want| entry.model.eq_ignore_ascii_case(want))
        })
        .collect::<Vec<InflightEntry>>();
    let running_count = running_entries.len();
    // channel_id → 在途数（数据面未接线 set_channel 时恒空 map）
    let mut running_counts: HashMap<String, u64> = HashMap::new();
    for entry in &running_entries {
        if let Some(channel_id) = entry.channel_id.as_deref() {
            *running_counts.entry(channel_id.to_string()).or_insert(0) += 1;
        }
    }

    match detail {
        Some(FlowDetail::Requests) => {
            let entries: Vec<Value> = slice_page(&filtered, offset, limit)
                .into_iter()
                .map(|log| request_row_json(log, &meta))
                .collect();
            json!({
                "available": true,
                "scope": "window",
                "detail": "requests",
                "entries": entries,
                "matched": matched,
                "offset": offset,
                "limit": limit,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "since": since.saturating_mul(1000),
                "sinceSeconds": since,
                "windowMinutes": minutes,
                "sampled": scan.truncated,
                "unsupportedFilters": unsupported,
            })
        }
        Some(FlowDetail::Models) => {
            let (models, model_count) = flow_models(&filtered);
            json!({
                "available": true,
                "scope": "window",
                "detail": "models",
                "models": models,
                "modelCount": model_count,
                "truncated": model_count > FLOW_MODEL_MAX,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "since": since.saturating_mul(1000),
                "sinceSeconds": since,
                "windowMinutes": minutes,
                "sampled": scan.truncated,
                "unsupportedFilters": unsupported,
            })
        }
        Some(FlowDetail::Sessions) => json!({
            "available": true,
            "scope": "window",
            "detail": "sessions",
            // AIGX RequestLog 无 session_id（route_config 只有 strategy/sticky_enabled）
            "supported": false,
            "entries": [],
            "matched": 0,
            "unknownRequests": matched,
            "now": now.saturating_mul(1000),
            "nowSeconds": now,
            "since": since.saturating_mul(1000),
            "sinceSeconds": since,
            "windowMinutes": minutes,
            "sampled": scan.truncated,
            "unsupportedFilters": unsupported,
            "note": "AIGX RequestLog 无会话标识，无法按会话下钻；单请求维度请用 detail=requests",
        }),
        None => {
            let (paths, path_count) = flow_paths(&filtered, &meta, &running_counts);
            json!({
                "available": true,
                "scope": "window",
                "detail": "paths",
                "paths": paths,
                "pathCount": path_count,
                "truncated": path_count > FLOW_PATH_MAX,
                "requests": matched,
                "running": running_count,
                "sessions": 0,
                // 会话概念缺失：窗口内全部请求都算「未知会话」（A2A 同字段语义）
                "unknownRequests": matched,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "since": since.saturating_mul(1000),
                "sinceSeconds": since,
                "windowMinutes": minutes,
                "sampled": scan.truncated,
                // 在途请求无法并入 paths（归因缺失），只计入顶层 running
                "runningUnattributed": running_count > 0,
                "unsupportedFilters": unsupported,
            })
        }
    }
}

fn flow_running(
    state: &AppState,
    filters: &FlowFilters,
    detail: Option<FlowDetail>,
    offset: usize,
    limit: usize,
    now: i64,
) -> Value {
    let meta = channel_meta_map(state);
    let mut entries: Vec<InflightEntry> = state
        .inflight
        .list()
        .into_iter()
        .filter(|entry| {
            filters
                .model
                .as_deref()
                .is_none_or(|want| entry.model.eq_ignore_ascii_case(want))
        })
        .collect();
    entries.sort_by(|left, right| {
        right
            .started_at
            .cmp(&left.started_at)
            .then(right.elapsed_ms.cmp(&left.elapsed_ms))
    });
    let matched = entries.len();
    let mut unsupported: Vec<&str> = Vec::new();
    if filters.client.is_some() {
        unsupported.push("client");
    }
    if filters.provider.is_some() {
        unsupported.push("provider");
    }
    if filters.account.is_some() {
        unsupported.push("account");
    }

    match detail {
        Some(FlowDetail::Requests) => {
            let rows: Vec<Value> = entries
                .iter()
                .skip(offset)
                .take(limit)
                .map(|entry| running_row_json(entry, &meta))
                .collect();
            json!({
                "available": true,
                "scope": "running",
                "detail": "requests",
                "entries": rows,
                "matched": matched,
                "offset": offset,
                "limit": limit,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "unsupportedFilters": unsupported,
            })
        }
        Some(FlowDetail::Models) => {
            let (models, model_count) = running_models(&entries);
            json!({
                "available": true,
                "scope": "running",
                "detail": "models",
                "models": models,
                "modelCount": model_count,
                "truncated": model_count > FLOW_MODEL_MAX,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "unsupportedFilters": unsupported,
            })
        }
        Some(FlowDetail::Sessions) => json!({
            "available": true,
            "scope": "running",
            "detail": "sessions",
            "supported": false,
            "entries": [],
            "matched": 0,
            "unknownRequests": matched,
            "now": now.saturating_mul(1000),
            "nowSeconds": now,
            "unsupportedFilters": unsupported,
            "note": "在途请求没有会话标识（InflightRegistry 只登记 model/user/key）",
        }),
        None => {
            let (paths, path_count) = running_paths(&entries, &meta);
            json!({
                "available": true,
                "scope": "running",
                "paths": paths,
                "pathCount": path_count,
                "truncated": path_count > FLOW_PATH_MAX,
                "requests": matched,
                "running": matched,
                "sessions": 0,
                "unknownRequests": matched,
                "now": now.saturating_mul(1000),
                "nowSeconds": now,
                "unsupportedFilters": unsupported,
                "note": "在途请求只有 model/user/key 维度；provider/channelId 依赖 set_channel 回填（数据面未接线），现阶段恒空",
            })
        }
    }
}

fn running_row_json(entry: &InflightEntry, meta: &HashMap<String, ChannelMeta>) -> Value {
    let provider = provider_of_id(meta, entry.channel_id.as_deref());
    json!({
        "id": entry.id,
        "traceKey": entry.id,
        "requestId": Value::Null,
        "model": entry.model,
        "originModel": Value::Null,
        "clientModel": entry.model,
        "upstreamModel": entry.model,
        "channelId": entry.channel_id,
        "channelName": entry.channel_name,
        "provider": provider,
        "providerLabel": provider_label(&provider),
        "client": "",
        "clientLabel": "",
        "clientVersion": "",
        "status": 0,
        "statusCode": Value::Null,
        "ok": false,
        "error": Value::Null,
        "latencyMs": entry.elapsed_ms,
        "durationMs": entry.elapsed_ms,
        "firstResponseMs": Value::Null,
        "attempts": 1,
        "phase": "running",
        // createdAt 保留 unix 秒；ts 为毫秒
        "createdAt": entry.started_at,
        "ts": entry.started_at.saturating_mul(1000),
        "startedAt": entry.started_at,
        "elapsedMs": entry.elapsed_ms,
        "stream": entry.stream,
        "userId": entry.user_id,
        "keyId": entry.key_id,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RunningPathKey {
    model: String,
    provider: String,
    channel_id: String,
    channel_name: String,
}

fn running_paths(
    entries: &[InflightEntry],
    meta: &HashMap<String, ChannelMeta>,
) -> (Vec<Value>, usize) {
    let mut groups: BTreeMap<RunningPathKey, (u64, i64)> = BTreeMap::new();
    for entry in entries {
        let key = RunningPathKey {
            model: entry.model.clone(),
            provider: provider_of_id(meta, entry.channel_id.as_deref()),
            channel_id: entry.channel_id.clone().unwrap_or_default(),
            channel_name: entry.channel_name.clone().unwrap_or_default(),
        };
        let slot = groups.entry(key).or_insert((0, 0));
        slot.0 += 1;
        slot.1 = slot.1.max(entry.started_at);
    }
    let path_count = groups.len();
    let mut rows: Vec<(RunningPathKey, (u64, i64))> = groups.into_iter().collect();
    rows.sort_by(|left, right| {
        let (left_count, left_seen) = left.1;
        let (right_count, right_seen) = right.1;
        right_count
            .cmp(&left_count)
            .then(right_seen.cmp(&left_seen))
    });
    let paths = rows
        .into_iter()
        .take(FLOW_PATH_MAX)
        .map(|(key, (requests, last_seen))| {
            json!({
                "id": json!([key.model, key.provider, key.channel_id]).to_string(),
                "client": "",
                "clientLabel": "",
                "clientVersion": "",
                "model": key.model,
                "provider": key.provider,
                "providerLabel": provider_label(&key.provider),
                "channelId": key.channel_id,
                "channelName": key.channel_name,
                "requests": requests,
                "running": requests,
                "successful": 0,
                "failed": 0,
                "sessions": 0,
                "stickyHits": 0,
                "stickyObserved": 0,
                "switched": 0,
                "lastSeen": last_seen.saturating_mul(1000),
            })
        })
        .collect();
    (paths, path_count)
}

fn running_models(entries: &[InflightEntry]) -> (Vec<String>, usize) {
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for entry in entries {
        if !entry.model.is_empty() {
            *counts.entry(entry.model.clone()).or_insert(0) += 1;
        }
    }
    let model_count = counts.len();
    let mut rows: Vec<(String, u64)> = counts.into_iter().collect();
    rows.sort_by(|left, right| {
        right.1.cmp(&left.1).then(
            left.0
                .to_ascii_lowercase()
                .cmp(&right.0.to_ascii_lowercase()),
        )
    });
    let models = rows
        .into_iter()
        .take(FLOW_MODEL_MAX)
        .map(|(model, _)| model)
        .collect();
    (models, model_count)
}

// ── GET /api/routing/trace ──────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceStatus {
    All,
    Ok,
    Error,
    Switched,
    Sticky,
}

pub(crate) fn parse_trace_status(raw: &str) -> Result<TraceStatus, String> {
    match raw {
        "" | "all" => Ok(TraceStatus::All),
        "ok" => Ok(TraceStatus::Ok),
        "error" => Ok(TraceStatus::Error),
        "switched" => Ok(TraceStatus::Switched),
        "sticky" => Ok(TraceStatus::Sticky),
        "running" => Err(
            "status=running 不支持：请求日志是终态记录，在途请用 GET /api/routing/flow?scope=running"
                .to_string(),
        ),
        other => Err(format!(
            "未知的跟踪状态：{other}（可选 ok / error / switched / sticky）"
        )),
    }
}

pub(crate) fn filter_trace_logs<'a>(
    logs: &'a [RequestLog],
    model: Option<&str>,
    search: Option<&str>,
    status: TraceStatus,
) -> Vec<&'a RequestLog> {
    let search = search
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty());
    logs.iter()
        .filter(|log| {
            if let Some(model) = model {
                if !matches_model(log, model) {
                    return false;
                }
            }
            if let Some(query) = search.as_deref() {
                let hit = log.model.to_lowercase().contains(query)
                    || log
                        .origin_model
                        .as_deref()
                        .is_some_and(|origin| origin.to_lowercase().contains(query))
                    || log.id.to_lowercase().contains(query)
                    || log
                        .request_id
                        .as_deref()
                        .is_some_and(|request_id| request_id.to_lowercase().contains(query))
                    || log
                        .channel_name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(query));
                if !hit {
                    return false;
                }
            }
            match status {
                TraceStatus::All => true,
                TraceStatus::Ok => log_is_success(log),
                TraceStatus::Error => !log_is_success(log),
                TraceStatus::Switched => log.attempts > 1,
                // 终态日志无 stickyHit 位：仅 route_replay 捕获的逐次尝试可见
                TraceStatus::Sticky => log.attempt_details.iter().any(|attempt| attempt.sticky_hit),
            }
        })
        .collect()
}

fn trace_entry_json(log: &RequestLog, meta: &HashMap<String, ChannelMeta>) -> Value {
    let mut value = request_row_json(log, meta);
    if let Some(object) = value.as_object_mut() {
        object.insert("userId".to_string(), json!(log.user_id));
        object.insert("keyId".to_string(), json!(log.key_id));
        object.insert("hasRouteReplay".to_string(), json!(has_route_replay(log)));
        // 前端 `RoutingTraceRow.route`（回放快照的扁平投影）：只在有捕获时给，
        // 与既有 GET /api/routing/replay/:request_id 的详情端点互补。
        // 特殊路由留痕（compact/title/vision）即使没有回放捕获也要投影，否则
        // 「最近命中」右栏恒为空（frontend SpecialHits 读这三键）。
        let mut route = if has_route_replay(log) {
            json!({
                "strategy": log.route_config.as_ref().map(|config| config.strategy.clone()),
                "sticky": log.route_config.as_ref().map(|config| config.sticky_enabled),
                "picked": log.selected_channel,
                "candidates": log.candidate_channels,
                "attempts": log.attempt_details,
            })
        } else {
            json!({})
        };
        if let Some(trace) = &log.special_route {
            if let Some(object) = route.as_object_mut() {
                match trace.kind.as_str() {
                    "compact" => {
                        object.insert(
                            "compactRoute".to_string(),
                            json!({
                                "source": trace.from.as_deref(),
                                "target": trace.to.as_deref(),
                            }),
                        );
                    }
                    "title" => {
                        object.insert(
                            "titleRoute".to_string(),
                            json!({
                                "source": trace.from.as_deref(),
                                "target": trace.to.as_deref(),
                            }),
                        );
                    }
                    "vision" => {
                        object.insert(
                            "visionCompatibility".to_string(),
                            json!({
                                "mode": trace.mode.as_deref(),
                                "target": trace
                                    .to
                                    .as_deref()
                                    .map(|model| json!({ "model": model })),
                            }),
                        );
                    }
                    _ => {}
                }
            }
        }
        let route = if route.as_object().is_some_and(|object| object.is_empty()) {
            Value::Null
        } else {
            route
        };
        object.insert("route".to_string(), route);
    }
    value
}

fn trace_detail_json(log: &RequestLog, meta: &HashMap<String, ChannelMeta>) -> Value {
    let mut value = trace_entry_json(log, meta);
    if let Some(object) = value.as_object_mut() {
        object.insert("selectedChannel".to_string(), json!(log.selected_channel));
        object.insert(
            "candidateChannels".to_string(),
            json!(log.candidate_channels),
        );
        object.insert("routeConfig".to_string(), json!(log.route_config));
        object.insert("filteredChannels".to_string(), json!(log.filtered_channels));
        object.insert("routeTruncated".to_string(), json!(log.route_truncated));
        object.insert("accountSkips".to_string(), json!(log.account_skips));
        object.insert("attemptDetails".to_string(), json!(log.attempt_details));
        object.insert(
            "attemptDetailsTruncated".to_string(),
            json!(log.attempt_details_truncated),
        );
        object.insert(
            "stickyHits".to_string(),
            json!(log
                .attempt_details
                .iter()
                .filter(|attempt| attempt.sticky_hit)
                .count()),
        );
        object.insert("sensitiveHits".to_string(), json!(log.sensitive_hits));
        object.insert(
            "hasConversation".to_string(),
            json!(log.conversation.is_some()),
        );
        object.insert("hasDebug".to_string(), json!(log.debug.is_some()));
    }
    value
}

/// GET /api/routing/trace — 路由相关请求的分页列表（终态日志，倒序）。
///
/// - `status`：`ok` / `error` / `switched`（attempts>1）/ `sticky`（需
///   `route_replay` 开启才可能有数据）；`running` 明确拒绝并指向 flow
/// - `start` / `end`：unix 秒（可选）；扫描有界（最新 5000 条），
///   `scanTruncated=true` 时 `matched` 只覆盖扫描到的子集
pub async fn handle_get_routing_trace(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let model = params
        .get("model")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let search = params
        .get("search")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let status_raw = params.get("status").map(|value| value.trim()).unwrap_or("");
    let status = parse_trace_status(status_raw).map_err(bad_request)?;
    let start = parse_ts_param(params.get("start").map(String::as_str))?;
    let end = parse_ts_param(params.get("end").map(String::as_str))?;
    // 前端 `RoutingTraceQuery.minutes`（可选）：给时间窗而不是绝对起止；
    // 显式 start/end 优先，minutes 只在不给 start 时生效（1..=1440 分钟夹取）。
    let minutes = params
        .get("minutes")
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|minutes| *minutes > 0)
        .map(|minutes| minutes.min(1440));
    if let (Some(start), Some(end)) = (start, end) {
        if start > end {
            return Err(bad_request("时间范围无效：start 不能大于 end"));
        }
    }
    if model.is_some_and(|value| value.len() > 256)
        || search.is_some_and(|value| value.len() > 1024)
    {
        return Err(bad_request("跟踪查询条件过长"));
    }
    let (offset, limit) = parse_paging(
        params.get("offset").map(String::as_str),
        params.get("limit").map(String::as_str),
    );

    let now = now_secs();
    let window_start = start.or_else(|| minutes.map(|minutes| now - minutes * 60));
    let scan = window_logs_desc(
        &state.log_store.requests,
        window_start.unwrap_or(0),
        end.unwrap_or(now),
        TRACE_SCAN_MAX,
    );
    let meta = channel_meta_map(&state);
    let filtered = filter_trace_logs(&scan.logs, model, search, status);
    let matched = filtered.len();
    let entries: Vec<Value> = slice_page(&filtered, offset, limit)
        .into_iter()
        .map(|log| trace_entry_json(log, &meta))
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "available": true,
            "entries": entries,
            "matched": matched,
            "offset": offset,
            "limit": limit,
            "scanned": scan.logs.len(),
            "scanTruncated": scan.truncated,
            "windowKeys": scan.total,
            "now": now.saturating_mul(1000),
            "nowSeconds": now,
            // sticky 过滤依赖 route_replay 捕获（关闭时恒无命中）
            "stickyFilterRequiresRouteReplay": true,
        }
    })))
}

fn parse_ts_param(raw: Option<&str>) -> Result<Option<i64>, (StatusCode, Json<Value>)> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|_| bad_request("时间戳必须为 unix 秒整数")),
    }
}

/// GET /api/routing/trace/request?id=&ts= — 单条请求详情（含路由回放字段）。
///
/// `ts`（可选，unix 秒）用于同 id 多条时定位；给了 `ts` 走键区间读，未给则
/// 复用 `get_by_request_id`（冷路径线性扫，与既有回放端点同口径）。
pub async fn handle_get_routing_trace_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let id = params.get("id").map(|value| value.trim()).unwrap_or("");
    if id.is_empty() {
        return Err(bad_request("请求 ID 不能为空"));
    }
    if id.len() > 256 {
        return Err(bad_request("请求 ID 不能超过 256 字符"));
    }
    let ts = parse_ts_param(params.get("ts").map(String::as_str))?;

    let log = match ts {
        Some(ts) => state
            .log_store
            .requests
            .list_window(ts, ts, 128)
            .map(|(logs, _)| {
                logs.into_iter()
                    .find(|log| log.id == id || log.request_id.as_deref() == Some(id))
            })
            .unwrap_or(None),
        None => state.log_store.requests.get_by_request_id(id),
    };
    let Some(log) = log else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "success": false, "error": "请求记录已删除或超过留存范围" })),
        ));
    };
    let meta = channel_meta_map(&state);
    Ok(Json(json!({
        "success": true,
        "data": trace_detail_json(&log, &meta),
    })))
}

// ── GET /api/routing/replays ────────────────────────────────────────

/// GET /api/routing/replays — 可回放请求列表（route_replay 有捕获的终态日志）。
///
/// 与既有 `GET /api/routing/replay/:request_id`（单条详情）互补；A2A 同名端点。
/// `status`：`error` / `sticky`（后者依赖 `route_replay` 捕获）。
pub async fn handle_get_routing_replays(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let model = params
        .get("model")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if model.is_some_and(|value| value.len() > 256) {
        return Err(bad_request("model 不能超过 256 字符"));
    }
    let status = params.get("status").map(|value| value.trim()).unwrap_or("");
    if !["", "error", "sticky"].contains(&status) {
        return Err(bad_request("未知的回放状态（可选 error / sticky）"));
    }
    let (offset, limit) = parse_paging(
        params.get("offset").map(String::as_str),
        params.get("limit").map(String::as_str),
    );

    let now = now_secs();
    let scan = window_logs_desc(&state.log_store.requests, 0, now, REPLAY_SCAN_MAX);
    let meta = channel_meta_map(&state);
    let filtered: Vec<&RequestLog> = scan
        .logs
        .iter()
        .filter(|log| {
            if !has_route_replay(log) {
                return false;
            }
            if let Some(model) = model {
                if !matches_model(log, model) {
                    return false;
                }
            }
            match status {
                "" => true,
                "error" => !log_is_success(log),
                "sticky" => log.attempt_details.iter().any(|attempt| attempt.sticky_hit),
                _ => true,
            }
        })
        .collect();
    let matched = filtered.len();
    let entries: Vec<Value> = slice_page(&filtered, offset, limit)
        .into_iter()
        .map(|log| trace_entry_json(log, &meta))
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "available": true,
            "entries": entries,
            "matched": matched,
            "offset": offset,
            "limit": limit,
            "scanned": scan.logs.len(),
            "scanTruncated": scan.truncated,
            "now": now.saturating_mul(1000),
            "nowSeconds": now,
        }
    })))
}

// ── 单测 ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(
        id: &str,
        model: &str,
        origin: Option<&str>,
        channel: &str,
        status: u16,
        latency: u64,
        created_at: i64,
    ) -> RequestLog {
        let mut log = RequestLog::new();
        log.id = id.to_string();
        log.model = model.to_string();
        log.origin_model = origin.map(str::to_string);
        log.channel_id = Some(channel.to_string());
        log.channel_name = Some("渠道".to_string());
        log.status_code = status;
        log.latency_ms = latency;
        log.created_at = created_at;
        log.attempts = 1;
        log
    }

    fn meta() -> HashMap<String, ChannelMeta> {
        let mut map = HashMap::new();
        map.insert(
            "ch1".to_string(),
            ChannelMeta {
                name: "主力".to_string(),
                provider: "openai_compatible".to_string(),
            },
        );
        map
    }

    fn attempt(sticky: bool) -> crate::log::AttemptDetail {
        crate::log::AttemptDetail {
            index: 1,
            channel_id: "ch1".to_string(),
            channel_name: "主力".to_string(),
            provider: "openai_compatible".to_string(),
            account: String::new(),
            model: "gpt-4o".to_string(),
            sticky_hit: sticky,
            started_at: 1,
            finished_at: Some(2),
            status: Some(200),
            error: None,
            notice: None,
        }
    }

    // ── 时间窗 / 分页 / detail 校验 ──

    #[test]
    fn flow_minutes_accepts_only_documented_windows() {
        for ok in ["5", "10", "30", "60"] {
            assert_eq!(
                parse_flow_minutes(Some(ok)).unwrap(),
                ok.parse::<i64>().unwrap()
            );
        }
        assert_eq!(parse_flow_minutes(None).unwrap(), 10);
        // A2A 口径：非数字回落默认 10，而非报错
        assert_eq!(parse_flow_minutes(Some("abc")).unwrap(), 10);
        for bad in ["0", "15", "120", "-5"] {
            assert!(parse_flow_minutes(Some(bad)).is_err(), "{bad} 必须拒绝");
        }
    }

    #[test]
    fn paging_is_clamped_to_api_bounds() {
        assert_eq!(parse_paging(None, None), (0, 30));
        assert_eq!(parse_paging(Some("40"), Some("10")), (40, 10));
        // offset 非法回落 0；limit=0 夹到 1
        assert_eq!(parse_paging(Some("x"), Some("0")), (0, 1));
        // limit 超上限夹到 100
        assert_eq!(parse_paging(Some("0"), Some("1000")), (0, 100));
        // usize 解析失败（负数）回落默认（offset=0 / limit=30）
        assert_eq!(parse_paging(Some("-3"), Some("-1")), (0, 30));
    }

    #[test]
    fn flow_detail_has_three_down_drills() {
        assert_eq!(parse_flow_detail("").unwrap(), None);
        // 前端发 detail=paths（A2A 默认档名），等价于聚合路径
        assert_eq!(parse_flow_detail("paths").unwrap(), None);
        assert_eq!(
            parse_flow_detail("requests").unwrap(),
            Some(FlowDetail::Requests)
        );
        assert_eq!(
            parse_flow_detail("sessions").unwrap(),
            Some(FlowDetail::Sessions)
        );
        assert_eq!(
            parse_flow_detail("models").unwrap(),
            Some(FlowDetail::Models)
        );
        assert!(parse_flow_detail("nope").is_err());
    }

    #[test]
    fn flow_scope_and_trace_status_are_validated() {
        assert_eq!(parse_flow_scope("").unwrap(), FlowScope::Window);
        assert_eq!(parse_flow_scope("window").unwrap(), FlowScope::Window);
        assert_eq!(parse_flow_scope("running").unwrap(), FlowScope::Running);
        assert!(parse_flow_scope("idle").is_err());

        assert_eq!(parse_trace_status("").unwrap(), TraceStatus::All);
        assert_eq!(parse_trace_status("ok").unwrap(), TraceStatus::Ok);
        assert_eq!(parse_trace_status("error").unwrap(), TraceStatus::Error);
        assert_eq!(
            parse_trace_status("switched").unwrap(),
            TraceStatus::Switched
        );
        assert_eq!(parse_trace_status("sticky").unwrap(), TraceStatus::Sticky);
        let err = parse_trace_status("running").unwrap_err();
        assert!(
            err.contains("/api/routing/flow"),
            "running 必须指路 flow：{err}"
        );
        assert!(parse_trace_status("bogus").is_err());
    }

    // ── 分页边界 ──

    #[test]
    fn slice_page_boundaries() {
        let logs: Vec<RequestLog> = (0..5)
            .map(|index| {
                log_with(
                    &format!("r{index}"),
                    "gpt-4o",
                    None,
                    "ch1",
                    200,
                    10,
                    100 + index,
                )
            })
            .collect();
        let refs: Vec<&RequestLog> = logs.iter().collect();
        assert_eq!(slice_page(&refs, 0, 2).len(), 2);
        assert_eq!(slice_page(&refs, 4, 2).len(), 1);
        assert_eq!(slice_page(&refs, 5, 2).len(), 0);
        assert_eq!(slice_page(&refs, 10, 2).len(), 0);
    }

    // ── 请求模型口径 ──

    #[test]
    fn request_model_and_matching_cover_origin_and_mapped() {
        let log = log_with("r", "upstream-name", Some("alias"), "ch1", 200, 1, 1);
        assert_eq!(request_model(&log), "alias");
        assert!(matches_model(&log, "alias"));
        assert!(matches_model(&log, "UPSTREAM-NAME"));
        assert!(!matches_model(&log, "other"));

        // origin_model 为空串（老数据）时回落 model
        let mut legacy = log.clone();
        legacy.origin_model = Some(String::new());
        assert_eq!(request_model(&legacy), "upstream-name");
    }

    // ── detail 三档 / 空数据形状 ──

    #[test]
    fn empty_flow_data_has_stable_shapes() {
        let empty: Vec<&RequestLog> = Vec::new();
        let no_running = HashMap::new();
        let (paths, path_count) = flow_paths(&empty, &meta(), &no_running);
        assert!(paths.is_empty());
        assert_eq!(path_count, 0);

        let (models, model_count) = flow_models(&empty);
        assert!(models.is_empty());
        assert_eq!(model_count, 0);

        let (running_paths_out, running_count) = running_paths(&[], &meta());
        assert!(running_paths_out.is_empty());
        assert_eq!(running_count, 0);

        let (running_models_out, running_models_count) = running_models(&[]);
        assert!(running_models_out.is_empty());
        assert_eq!(running_models_count, 0);
    }

    #[test]
    fn flow_paths_group_by_client_model_and_channel() {
        let mut first = log_with("r1", "gpt-4o", Some("gpt-4o"), "ch1", 200, 10, 100);
        first.client_name = "Claude Code".to_string();
        first.client_version = "2.0".to_string();
        let mut second = first.clone();
        second.id = "r2".to_string();
        second.created_at = 101;
        second.attempts = 2;
        second.client_version = "2.1".to_string();
        // stickyHits 只能从 route_replay 捕获的 attempt_details 观测（见模块头差异表）
        second.attempt_details = vec![attempt(true)];
        let failed = {
            let mut log = log_with("r3", "gpt-4o", Some("gpt-4o"), "ch1", 500, 10, 102);
            log.client_name = "Claude Code".to_string();
            log.error_msg = Some("boom".to_string());
            log
        };
        let logs = [&first, &second, &failed];
        let mut running = HashMap::new();
        running.insert("ch1".to_string(), 2u64);
        let (paths, path_count) = flow_paths(&logs, &meta(), &running);
        assert_eq!(path_count, 1, "同 client+model+channel 应收敛为一条路径");
        assert_eq!(paths[0]["requests"], 3);
        assert_eq!(paths[0]["successful"], 2);
        assert_eq!(paths[0]["failed"], 1);
        assert_eq!(paths[0]["switched"], 1);
        assert_eq!(paths[0]["lastSeen"], 102_000, "lastSeen 为毫秒");
        assert_eq!(paths[0]["clientVersion"], "2.1");
        assert_eq!(paths[0]["provider"], "openai_compatible");
        // stickyHits 只统计 route_replay 有捕获的请求（stickyObserved 是分母）
        assert_eq!(paths[0]["stickyHits"], 1);
        assert_eq!(paths[0]["stickyObserved"], 1);
        // running 按渠道回填（InflightRegistry 的 channel_id 未接线时就是 0）
        assert_eq!(paths[0]["running"], 2);
    }

    #[test]
    fn flow_models_counts_requested_names() {
        let first = log_with("r1", "deepseek-chat", Some("gpt-4o"), "ch1", 200, 1, 1);
        let second = log_with("r2", "upstream-x", None, "ch1", 200, 1, 2);
        let third = log_with("r3", "deepseek-chat", Some("gpt-4o"), "ch1", 200, 1, 3);
        let logs = [&first, &second, &third];
        let (models, model_count) = flow_models(&logs);
        assert_eq!(model_count, 2);
        assert_eq!(models[0], "gpt-4o", "按请求数降序");
        assert_eq!(models[1], "upstream-x");
    }

    #[test]
    fn flow_filters_client_provider_and_unsupported_account() {
        let mut log = log_with("r1", "gpt-4o", None, "ch1", 200, 1, 1);
        log.client_name = "Claude Code".to_string();
        let logs = [log];
        let meta = meta();

        let hit = FlowFilters {
            model: Some("gpt-4o".to_string()),
            client: Some("claude code".to_string()),
            provider: Some("OPENAI_COMPATIBLE".to_string()),
            account: None,
        };
        assert_eq!(filter_flow_logs(&logs, &meta, &hit).len(), 1);

        let unknown = FlowFilters {
            client: Some("__unknown".to_string()),
            ..FlowFilters::default()
        };
        assert_eq!(filter_flow_logs(&logs, &meta, &unknown).len(), 0);

        // 没读到客户端（空名）与存量占位词都归 `__unknown` 这一档。
        let mut blank = log_with("r2", "gpt-4o", None, "ch1", 200, 1, 2);
        blank.client_name = String::new();
        let mut legacy = log_with("r3", "gpt-4o", None, "ch1", 200, 1, 3);
        legacy.client_name = "unknown".to_string();
        let mixed = [blank, legacy];
        assert_eq!(
            filter_flow_logs(&mixed, &meta, &unknown).len(),
            2,
            "空名与存量 unknown 是同一档（没读到）"
        );

        // account 维度 AIGX 无数据源 → 恒不命中（诚实退化，不造假）
        let by_account = FlowFilters {
            account: Some("acc-1".to_string()),
            ..FlowFilters::default()
        };
        assert_eq!(filter_flow_logs(&logs, &meta, &by_account).len(), 0);
    }

    // ── trace 过滤 ──

    /// 特殊路由留痕投影：compact/title/vision 各自映射成前端约定的三键，
    /// 即使没有路由回放捕获也要出现；无留痕时 route 保持 Null（旧契约）。
    #[test]
    fn trace_route_projects_special_hits() {
        use crate::log::SpecialRouteTrace;
        let meta = HashMap::new();

        let mut compact = log_with("id-c", "glm-5.3-flash", Some("glm-5.3"), "ch1", 200, 5, 20);
        compact.special_route = Some(SpecialRouteTrace::rewrite(
            "compact",
            "glm-5.3",
            "glm-5.3-flash",
        ));
        let value = trace_entry_json(&compact, &meta);
        let route = &value["route"];
        assert_eq!(route["compactRoute"]["source"], "glm-5.3");
        assert_eq!(route["compactRoute"]["target"], "glm-5.3-flash");
        assert!(route.get("titleRoute").is_none());
        assert!(route.get("visionCompatibility").is_none());

        let mut title = log_with("id-t", "cheap", Some("gpt-4o"), "ch1", 200, 5, 21);
        title.special_route = Some(SpecialRouteTrace::rewrite("title", "gpt-4o", "cheap"));
        let value = trace_entry_json(&title, &meta);
        assert_eq!(value["route"]["titleRoute"]["source"], "gpt-4o");
        assert_eq!(value["route"]["titleRoute"]["target"], "cheap");

        let mut vision = log_with("id-v", "gpt-4o", None, "ch1", 200, 5, 22);
        vision.special_route = Some(SpecialRouteTrace::vision("switch", Some("qwen-vl".into())));
        let value = trace_entry_json(&vision, &meta);
        assert_eq!(value["route"]["visionCompatibility"]["mode"], "switch");
        assert_eq!(
            value["route"]["visionCompatibility"]["target"]["model"],
            "qwen-vl"
        );

        // 没有任何留痕 → route 仍是 Null（保持旧契约）。
        let plain = log_with("id-p", "gpt-4o", None, "ch1", 200, 5, 23);
        assert!(trace_entry_json(&plain, &meta)["route"].is_null());
    }

    #[test]
    fn trace_filters_status_model_and_search() {
        let ok = log_with("id-ok", "gpt-4o", None, "ch1", 200, 5, 10);
        let failed = {
            let mut log = log_with("id-err", "gpt-4o-mini", None, "ch1", 500, 5, 11);
            log.error_msg = Some("boom".to_string());
            log
        };
        let switched = {
            let mut log = log_with("id-sw", "gpt-4o", Some("gpt-4"), "ch1", 200, 5, 12);
            log.attempts = 2;
            log
        };
        let sticky = {
            let mut log = log_with("id-sticky", "gpt-4o", None, "ch1", 200, 5, 13);
            log.attempt_details = vec![attempt(true)];
            log
        };
        let plain = {
            let mut log = log_with("id-plain", "gpt-4o", None, "ch1", 200, 5, 14);
            log.attempt_details = vec![attempt(false)];
            log
        };
        let logs = [ok, failed, switched, sticky, plain];

        assert_eq!(
            filter_trace_logs(&logs, None, None, TraceStatus::All).len(),
            5
        );
        // gpt-4o 精确名：ok/switched/sticky/plain 四条（gpt-4o-mini 不沾边）
        assert_eq!(
            filter_trace_logs(&logs, Some("gpt-4o"), None, TraceStatus::All).len(),
            4
        );
        assert_eq!(
            filter_trace_logs(&logs, None, None, TraceStatus::Error).len(),
            1
        );
        assert_eq!(
            filter_trace_logs(&logs, None, None, TraceStatus::Switched).len(),
            1
        );
        assert_eq!(
            filter_trace_logs(&logs, None, None, TraceStatus::Sticky).len(),
            1
        );
        assert_eq!(
            filter_trace_logs(&logs, None, Some("id-err"), TraceStatus::All).len(),
            1
        );
    }

    // ── 时间窗扫描 ──

    #[test]
    fn window_scan_keeps_latest_and_reports_truncation() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = RequestLogStore::new(std::sync::Arc::new(crate::storage::FileStore::new(
            dir.path().to_path_buf(),
        )));
        for (index, ts) in [100i64, 101, 102, 103, 104].into_iter().enumerate() {
            let mut log = RequestLog::new();
            log.id = format!("w{index}");
            log.model = "m".to_string();
            log.created_at = ts;
            store.add(log).unwrap();
        }
        let scan = window_logs_desc(&store, 101, 103, 2);
        assert_eq!(scan.total, 3, "窗口内共 3 条键");
        assert!(scan.truncated);
        assert_eq!(scan.logs.len(), 2);
        // 取窗口内最新两条（103、102），丢的是最旧而不是最新
        assert_eq!(scan.logs[0].created_at, 103);
        assert_eq!(scan.logs[1].created_at, 102);

        let all = window_logs_desc(&store, 100, 104, 10);
        assert!(!all.truncated);
        assert_eq!(all.logs.len(), 5);
        assert_eq!(all.logs[0].created_at, 104, "倒序：最新在前");
    }

    // ── 今日边界 ──

    #[test]
    fn today_start_is_same_report_day_and_not_after_now() {
        let now = 1_760_000_000; // 固定输入，不吃系统时钟
        let start = today_start_ts(now);
        assert!(start <= now, "日首不得晚于当前时刻");
        assert_eq!(
            crate::log::daily_agg::date_key(start),
            crate::log::daily_agg::date_key(now),
            "日首与当前必须是同一报表日"
        );
    }

    // ── profile 投影 ──

    #[test]
    fn provider_label_never_mislabels_empty_as_openai() {
        // registry::find("") 回落默认 kind=openai —— 空归因不能被标成 OpenAI
        assert_eq!(provider_label(""), "");
        assert_eq!(provider_label("   "), "");
        assert_eq!(
            provider_label("openai_compatible"),
            "OpenAI 兼容（自填地址）"
        );
        assert_eq!(provider_label("cloudflare"), "Cloudflare");
        // 目录内的 provider 用目录 label
        assert_eq!(provider_label("deepseek"), "DeepSeek");
        // 未知 id 原样回落
        assert_eq!(provider_label("acme-custom"), "acme-custom");
    }

    #[test]
    fn profile_json_uses_frontend_snake_case_fields() {
        let profile = ResolvedProfile {
            scope: Default::default(),
            strategy: crate::channel::routing_policy::RouteStrategy::Ranked,
            sticky: crate::channel::routing_policy::StickyMode::Auto,
            needs: Default::default(),
            sort_rules: Vec::new(),
            default_group: Some("g1".to_string()),
            provenance: Default::default(),
        };
        let value = profile_json(&profile);
        assert_eq!(value["strategy"], "ranked");
        assert_eq!(value["sticky"], "auto");
        assert_eq!(value["default_group"], "g1");
        // 前端 `RoutingProfile` 的键：snake_case（camelCase 会让「账号排序规则 /
        // 默认路由组」两行恒显「—」，profileFieldText 读不到值）
        assert!(value["sort_rules"].is_array());
        assert!(value["scope"].is_object());
        // provenance 由工作台顶层提供（见 handle_get_routing_workspace）
        assert!(value.get("provenance").is_none());
    }
}
