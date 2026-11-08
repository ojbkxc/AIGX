//! 数据看板 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供消费趋势、模型分布、渠道统计等数据看板功能。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use chrono::{TimeZone, Timelike};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{is_admin_user, verify_admin, verify_user};
use crate::log::daily_agg;

// Dashboard 查询参数：时间范围（天数）+ 可选自然日起止覆盖（阶段 4 自定义区间；
// start/end 合法时优先生效，缺省回退 days——向后兼容，旧前端请求行为不变）。
#[derive(Debug, Deserialize)]
pub struct DashboardQuery {
    #[serde(default = "default_dashboard_days")]
    pub days: u32,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

fn default_dashboard_days() -> u32 {
    30
}

/// 校验 `YYYY-MM-DD`（定长 10 且可解析）；非法 → None（当缺省处理）。
fn valid_date(s: Option<&str>) -> Option<String> {
    let s = s?;
    if s.len() != 10 || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_err() {
        return None;
    }
    Some(s.to_string())
}

/// 日期回退 n 天（n ≥ 0）；解析失败原样返回（调用方已校验格式）。
fn shift_date(date: &str, days_back: i64) -> String {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.checked_sub_signed(chrono::Duration::days(days_back)))
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| date.to_string())
}

/// 报表日 0 点（含）的 unix 起点；解析失败 → i64::MIN。
///
/// 口径 = `[report] timezone_offset`（与 [`daily_agg::date_key`] 同源）。**这里必须
/// 跟日键同口径**：它是「聚合读」与「明细现算回退」两条路共用的窗口下界，一边 UTC
/// 一边偏移就会把同一面板劈成两套日界（P3-14 原始症状的复发形态，比现状更糟）。
/// 7 个生产调用点 + `legacy.rs` 两处签名零改动，随快照自动切换。
pub fn date_start_ts(date: &str) -> i64 {
    date_start_ts_at(date, crate::config::report_tz_offset())
}

/// [`date_start_ts`] 的**纯函数变体**（单测直接喂 offset，不碰全局快照）。
///
/// 算法：naive 日 0 点当作报表时区的 0 点 → 减掉偏移秒数得到 unix 秒。固定偏移下
/// 日长恒 86400s，故 `date_start_ts(d) + 86_400 - 1` 是当日最后一秒（`offset = 0`
/// 且本地时区正在夏令时切换的那一日不成立，见 `config::ReportConfig` 文档）。
pub fn date_start_ts_at(date: &str, offset: i32) -> i64 {
    let tz = daily_agg::report_tz_of(offset);
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|nd| nd.and_utc().timestamp() - tz.local_minus_utc() as i64)
        .unwrap_or(i64::MIN)
}

/// 从查询参数解析自然日窗口 `[from, to]`（含端点）与起点时间戳。
///
/// - 语义统一为**自然日**（A2A 口径）：days=N → 含今天的 N 个自然日。
///   旧路径的滚动 N×86400s 窗口与此在当天部分有微小差异，切聚合后一并收敛。
/// - 跨度护栏 ≤365 天（起点自动拉近）；起止写反自动交换。
pub fn resolve_window(days: u32, start: Option<&str>, end: Option<&str>) -> (String, String, i64) {
    let today = daily_agg::today_key();
    let mut to = valid_date(end).unwrap_or_else(|| today.clone());
    let mut from =
        valid_date(start).unwrap_or_else(|| shift_date(&to, days.clamp(1, 365) as i64 - 1));
    if from > to {
        std::mem::swap(&mut from, &mut to);
    }
    let max_from = shift_date(&to, 364);
    if from < max_from {
        from = max_from;
    }
    let from_ts = date_start_ts(&from);
    (from, to, from_ts)
}

/// 消费趋势
pub async fn handle_consumption_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data: Vec<Value> = if state.log_store.daily.agg_available() {
        state
            .log_store
            .daily
            .load_range(&from, &to)
            .into_iter()
            .map(|r| json!({ "date": r.date, "cost": r.cost, "count": r.requests }))
            .collect()
    } else {
        // 回填前（或手动重建窗口）：回退明细现算，数字永不为假。
        let logs = state.log_store.requests.all_sorted_asc();
        let mut daily: std::collections::BTreeMap<String, (i64, u64)> =
            std::collections::BTreeMap::new();
        for l in &logs {
            if l.created_at < from_ts {
                continue;
            }
            let day = daily_agg::date_key(l.created_at);
            let entry = daily.entry(day).or_insert((0, 0));
            entry.0 += l.cost;
            entry.1 += 1;
        }
        daily
            .into_iter()
            .map(|(day, (cost, count))| json!({ "date": day, "cost": cost, "count": count }))
            .collect()
    };
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 模型分布
pub async fn handle_model_distribution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data: Vec<Value> = if state.log_store.daily.agg_available() {
        let rows = state.log_store.daily.load_range(&from, &to);
        daily_agg::merge_models(&rows)
            .into_iter()
            .map(|m| json!({ "model": m.name, "count": m.count }))
            .collect()
    } else {
        let logs = state.log_store.requests.all_sorted_asc();
        let mut models: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        for l in &logs {
            if l.created_at < from_ts {
                continue;
            }
            *models.entry(l.model.clone()).or_insert(0) += 1;
        }
        models
            .into_iter()
            .map(|(model, count)| json!({ "model": model, "count": count }))
            .collect()
    };
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 渠道统计
pub async fn handle_channel_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let channels: Vec<Value> = state
        .channel_store
        .list()
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "name": c.name,
                "status": c.status,
                "last_error": c.last_error,
                "last_used_at": c.last_used_at,
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": channels,
        "stats": json!({
            "total_channels": channels.len(),
            "active_channels": channels.iter().filter(|c| c["status"] == "enabled").count(),
        })
    })))
}

/// 调度实时状态（A2 毫秒级 failover 看板）。
///
/// 逐渠道聚合四路实时信号，供 NetworkLayer「调度实时地图」直接渲染：
/// - `breaker`：断路器三态 + 失败计数 + 剩余冷却/限流（`CircuitBreaker::snapshot_all`）
/// - `health`：健康追踪器汇总（错误率/延迟 EMA/认证/余额，`ChannelStateTracker::get_health`）
/// - `aimd`：AIMD 限额与状态机（`ChannelStore::aimd_snapshot`）
/// - `archive`：当日健康档案（成功率/熔断次数/P95，`HealthArchive::query(1)`）
///
/// 只读：不写断路器/健康/AIMD，不落日志，不触发限流。与
/// `dashboard/channel_health`（日志回放口径）互补——那个看"历史吞吐"，
/// 这个看"此刻调度器眼中的渠道健康"。
pub async fn handle_scheduler_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let channels = state.channel_store.list();
    let breakers = state.channel_store.circuit_breaker().snapshot_all();
    let breaker_by_id: std::collections::HashMap<&str, _> = breakers
        .iter()
        .map(|s| (s.channel_id.as_str(), s))
        .collect();
    let cfg = state.channel_store.scheduler_config();

    let items: Vec<Value> = channels
        .iter()
        .map(|ch| {
            let breaker = breaker_by_id.get(ch.id.as_str()).map(|s| {
                json!({
                    "state": s.state,
                    "failure_count": s.failure_count,
                    "failure_type": s.failure_type,
                    "cooldown_remaining_secs": s.cooldown_remaining_secs,
                    "rate_limit_remaining_secs": s.rate_limit_remaining_secs,
                    "probe_in_flight": s.probe_in_flight,
                })
            });
            let health = state
                .channel_store
                .health_tracker()
                .get_health(&ch.id)
                .map(|h| {
                    json!({
                        "auth_ok": h.auth_ok,
                        "balance_status": h.balance_status,
                        "overall_error_rate": h.overall_error_rate,
                        "overall_avg_latency_ms": h.overall_avg_latency_ms,
                        "last_error": h.last_error,
                    })
                });
            let aimd = state.channel_store.aimd_snapshot(&ch.id).map(|a| {
                json!({
                    "current_limit": a.current_limit,
                    "state": a.state,
                })
            });
            let archive = state
                .channel_store
                .health_archive()
                .query(&ch.id, 1)
                .pop()
                .map(|s| {
                    json!({
                        "success": s.success,
                        "failure": s.failure,
                        "trips": s.trips,
                        "success_rate": (s.success_rate() * 100.0).round() / 100.0,
                        "p95_ms": s.p95_ms(),
                        "last_error": s.last_error,
                    })
                });

            json!({
                "id": ch.id,
                "name": ch.name,
                "enabled": ch.is_enabled(),
                "channel_type": ch.channel_type,
                "priority": ch.priority,
                "weight": ch.weight,
                "models": ch.models,
                "status": ch.status,
                "breaker": breaker,
                "health": health,
                "aimd": aimd,
                "archive": archive,
            })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "scheduler": cfg,
            "channels": items,
        }
    })))
}

/// 缓存命中节省看板（B1）。
///
/// 聚合 `cache_hit = true` 的请求日志：
/// - hit_count：缓存命中次数
/// - cache_tokens：缓存回放的 prompt tokens 合计
/// - billed_cost：实际向用户收取的缓存读费用
/// - saved_estimate：若按普通 input_price 计费的差额（节省）
///
/// 节省估算用「当前定价表 input_price × tokens / 1000」减去实际已收费用。
/// 日志不存 group ratio（历史记录不可追溯），此处按当前 default 分组倍率近似；
/// 看板定位是运营感知「缓存值多少钱」，不是审计级精确账本。
///
/// 权限（阶段 4 矩阵）：report_scope——管理员全站主档；普通用户仅自己
/// （用户日行 models 分项，长尾并入 OTHER 时响应带 `approx:true`）。
pub async fn handle_cache_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    if state.log_store.daily.agg_available() {
        // 管理员：主档 Σcache_billed + Σby_model 查价；普通用户：用户日行
        // models 分项（cache_billed 由模型级求和，行平铺无此字段）。
        let mut hit_count: u64 = 0;
        let mut cache_tokens: u64 = 0;
        let mut billed_cost: i64 = 0;
        let mut saved_estimate: f64 = 0.0;
        let mut approx = false;
        if let Some(uid) = &user_filter {
            let rows = state.log_store.daily.load_user_range(uid, &from, &to);
            for r in &rows {
                hit_count = hit_count.saturating_add(r.cache_hits);
                cache_tokens = cache_tokens.saturating_add(r.cache_input);
            }
            for m in daily_agg::merge_user_models(&rows) {
                if m.name == daily_agg::OTHER {
                    approx = true;
                    continue;
                }
                billed_cost = billed_cost.saturating_add(m.cache_billed);
                if let Some(price) = state.pricing_store.get_price(&m.name) {
                    let full = price.input_price * m.cache_input as f64 / 1000.0;
                    saved_estimate += (full - m.cache_billed as f64).max(0.0);
                }
            }
        } else {
            let rows = state.log_store.daily.load_range(&from, &to);
            for r in &rows {
                hit_count = hit_count.saturating_add(r.cache_hits);
                cache_tokens = cache_tokens.saturating_add(r.cache_input);
                billed_cost = billed_cost.saturating_add(r.cache_billed);
            }
            // 节省估算按当前定价表逐模型算（top-128 次查询，比逐请求便宜）；
            // 模型级口径与逐请求 max(0) 求和近似一致。
            for m in daily_agg::merge_models(&rows) {
                if m.name == daily_agg::OTHER {
                    // 长尾并组无单一模型价可查：节省记 0，响应标注近似。
                    approx = true;
                    continue;
                }
                if let Some(price) = state.pricing_store.get_price(&m.name) {
                    let full = price.input_price * m.cache_input as f64 / 1000.0;
                    saved_estimate += (full - m.cache_billed as f64).max(0.0);
                }
            }
        }
        let mut data = json!({
            "hit_count": hit_count,
            "cache_tokens": cache_tokens,
            "billed_cost": billed_cost,
            "saved_estimate": (saved_estimate * 100.0).round() / 100.0,
        });
        if approx {
            data["approx"] = json!(true);
        }
        return Ok(Json(json!({ "success": true, "data": data })));
    }
    let logs = state.log_store.requests.all_sorted_asc();

    let mut hit_count: u64 = 0;
    let mut cache_tokens: u64 = 0;
    let mut billed_cost: i64 = 0;
    let mut saved_estimate: f64 = 0.0;

    for log in logs.iter().filter(|l| {
        l.cache_hit
            && l.created_at >= from_ts
            && user_filter
                .as_deref()
                .is_none_or(|uid| l.user_id.as_deref() == Some(uid))
    }) {
        hit_count += 1;
        cache_tokens = cache_tokens.saturating_add(log.input_tokens);
        billed_cost = billed_cost.saturating_add(log.cost);
        if let Some(price) = state.pricing_store.get_price(&log.model) {
            let full_price_cost = price.input_price * log.input_tokens as f64 / 1000.0;
            saved_estimate += (full_price_cost - log.cost as f64).max(0.0);
        }
    }

    Ok(Json(json!({
        "success": true,
        "data": {
            "hit_count": hit_count,
            "cache_tokens": cache_tokens,
            "billed_cost": billed_cost,
            "saved_estimate": (saved_estimate * 100.0).round() / 100.0,
        }
    })))
}

// ── 报表扩展（移植 Agent2API report 页视角）──────────────────────────
//
// Agent2API 的报表（report-page.tsx）有四个 AIGX 仪表盘没有的维度：
// 365 天热力图、Top 账号、Top 提供商、prompt/completion 分列 Token 趋势。
// 以下四个端点把它们补齐，数据源仍是 `log_store.requests`（与既有看板同源，
// 不引入新存储）。权限模型与 Dashboard.tsx 页内分支一致：管理员看全量，
// 普通用户只看自己的（按 `user_id` 过滤）——所以这四个端点走 `verify_user`
// 而非 `verify_admin`，在 handler 内按 `is_admin_user` 收敛数据集。

/// 报表查询参数：时间范围 + 可选自然日起止（阶段 4 自定义区间）+ 用户过滤。
#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    #[serde(default = "default_dashboard_days")]
    pub days: u32,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub end: Option<String>,
}

/// 鉴权并返回 (数据集过滤谓词所需的用户 ID Option, 是否管理员)。
///
/// 管理员 → `(None, true)`（全量）；普通用户 → `(Some(user_id), false)`。
/// 未登录请求在 `verify_user` 处即被 401 拦下。
async fn report_scope(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(Option<String>, bool), (StatusCode, Json<Value>)> {
    let user = verify_user(state, headers).await?;
    if is_admin_user(&user) {
        return Ok((None, true));
    }
    Ok((Some(user.id.clone()), false))
}

/// 365 天日粒度消费热力图（Agent2API report 365d heatmap）。
///
/// 默认窗口由前端传 days=365；resolve_window 统一 clamp（修原先
/// 「handler 夹 365 / start_ts 夹 90」双夹 bug——365 天报表实际只有 90 天）。
pub async fn handle_report_heatmap(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "heatmap",
        &format!("{from}:{to}"),
        user_filter.clone(),
        120,
        {
            let state = state.clone();
            move || {
                let user_filter = user_filter.clone();
                if state.log_store.daily.agg_available() {
                    let pts: Vec<Value> = match &user_filter {
                        None => state
                            .log_store
                            .daily
                            .load_range(&from, &to)
                            .into_iter()
                            .map(|r| {
                                // tooltip 的按模型分解（A2A 口径 ≤8）：按当日 token 降序
                                let mut ms = r.by_model.clone();
                                ms.sort_by_key(|m| std::cmp::Reverse(m.tokens_in + m.tokens_out));
                                json!({
                                    "date": r.date,
                                    "cost": r.cost,
                                    "count": r.requests,
                                    "tokens": r.tokens_in + r.tokens_out,
                                    "models": ms
                                        .into_iter()
                                        .take(8)
                                        .map(|m| {
                                            json!({
                                                "model": m.name,
                                                "tokens": m.tokens_in + m.tokens_out
                                            })
                                        })
                                        .collect::<Vec<Value>>(),
                                })
                            })
                            .collect(),
                        Some(uid) => state
                            .log_store
                            .daily
                            .load_user_range(uid, &from, &to)
                            .into_iter()
                            .map(|r| {
                                let mut ms = r.models.clone();
                                ms.sort_by_key(|m| std::cmp::Reverse(m.tokens_in + m.tokens_out));
                                json!({
                                    "date": r.date,
                                    "cost": r.cost,
                                    "count": r.count,
                                    "tokens": r.tokens_in + r.tokens_out,
                                    "models": ms
                                        .into_iter()
                                        .take(8)
                                        .map(|m| {
                                            json!({
                                                "model": m.name,
                                                "tokens": m.tokens_in + m.tokens_out
                                            })
                                        })
                                        .collect::<Vec<Value>>(),
                                })
                            })
                            .collect(),
                    };
                    return json!(pts);
                }
                let to_ts_end = date_start_ts(&to) + 86_400 - 1;
                let logs = state.log_store.requests.all_sorted_asc();
                // 回退现算与聚合同形状（tokens/models 一并补齐，前端不分支）
                let mut daily: std::collections::BTreeMap<String, (i64, u64, u64)> =
                    std::collections::BTreeMap::new();
                let mut per_day_models: std::collections::BTreeMap<
                    String,
                    std::collections::BTreeMap<String, u64>,
                > = std::collections::BTreeMap::new();
                for l in &logs {
                    if l.created_at < from_ts || l.created_at > to_ts_end {
                        continue;
                    }
                    if let Some(uid) = &user_filter {
                        if l.user_id.as_deref() != Some(uid.as_str()) {
                            continue;
                        }
                    }
                    let day = daily_agg::date_key(l.created_at);
                    let entry = daily.entry(day.clone()).or_insert((0, 0, 0));
                    entry.0 += l.cost;
                    entry.1 += 1;
                    entry.2 += l.input_tokens + l.output_tokens;
                    *per_day_models
                        .entry(day)
                        .or_default()
                        .entry(l.model.clone())
                        .or_insert(0) += l.input_tokens + l.output_tokens;
                }
                json!(daily
                    .into_iter()
                    .map(|(date, (cost, count, tokens))| {
                        let mut ms: Vec<(String, u64)> = per_day_models
                            .remove(&date)
                            .map(|m| m.into_iter().collect())
                            .unwrap_or_default();
                        ms.sort_by_key(|(_, tk)| std::cmp::Reverse(*tk));
                        json!({
                            "date": date,
                            "cost": cost,
                            "count": count,
                            "tokens": tokens,
                            "models": ms
                                .into_iter()
                                .take(8)
                                .map(|(model, tk)| json!({ "model": model, "tokens": tk }))
                                .collect::<Vec<Value>>(),
                        })
                    })
                    .collect::<Vec<Value>>())
            }
        },
    )
    .await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// Top 渠道/来源排行（Agent2API Top accounts/providers 的 AIGX 对应物）。
///
/// AIGX 的请求日志没有 provider 与账号维度，只有 `channel_name`——
/// Top 提供商与 Top 账号在这里统一为「Top 渠道」（渠道名即来源标签）。
/// 管理员五指标（请求/消费/Token/成功率/平均延迟，A2A 全量口径）；
/// 个人视图的渠道名后端本不暴露、聚合行也只有名字（ChannelLite），
/// tokens/success_rate/avg_latency 对个人支给 null（前端个人面板改用
/// Top 模型榜，不消费这些列）。
struct ChannelAggRow {
    name: String,
    count: u64,
    cost: i64,
    tokens: u64,
    successful: u64,
    latency_sum: u64,
}

pub async fn handle_report_top_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let personal = user_filter.is_some();
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "top_sources",
        &format!("{from}:{to}"),
        user_filter.clone(),
        120,
        {
            let state = state.clone();
            move || {
                let mut items: Vec<ChannelAggRow>;
                if state.log_store.daily.agg_available() {
                    match &user_filter {
                        None => {
                            let rows = state.log_store.daily.load_range(&from, &to);
                            // 按展示名再聚合（id 维改名的两个渠道现同名 → 合并，与现口径一致）。
                            let mut by_name: std::collections::HashMap<String, ChannelAggRow> =
                                std::collections::HashMap::new();
                            for c in daily_agg::merge_channels(&rows) {
                                let name = c.name.unwrap_or_else(|| "unknown".to_string());
                                let e = by_name.entry(name.clone()).or_insert(ChannelAggRow {
                                    name,
                                    count: 0,
                                    cost: 0,
                                    tokens: 0,
                                    successful: 0,
                                    latency_sum: 0,
                                });
                                e.count += c.count;
                                e.cost += c.cost;
                                e.tokens += c.tokens_in + c.tokens_out;
                                e.successful += c.successful;
                                e.latency_sum += c.latency_sum;
                            }
                            items = by_name.into_values().collect();
                        }
                        Some(uid) => {
                            let rows = state.log_store.daily.load_user_range(uid, &from, &to);
                            items = daily_agg::merge_dim(&rows, |r| &r.channels)
                                .into_iter()
                                .map(|c| ChannelAggRow {
                                    name: c.name,
                                    count: c.count,
                                    cost: c.cost,
                                    tokens: 0,
                                    successful: 0,
                                    latency_sum: 0,
                                })
                                .collect();
                        }
                    }
                } else {
                    let to_ts_end = date_start_ts(&to) + 86_400 - 1;
                    let logs = state.log_store.requests.all_sorted_asc();
                    let mut sources: std::collections::HashMap<String, ChannelAggRow> =
                        std::collections::HashMap::new();
                    for l in &logs {
                        if l.created_at < from_ts || l.created_at > to_ts_end {
                            continue;
                        }
                        if let Some(uid) = &user_filter {
                            if l.user_id.as_deref() != Some(uid.as_str()) {
                                continue;
                            }
                        }
                        let name = l
                            .channel_name
                            .clone()
                            .unwrap_or_else(|| "unknown".to_string());
                        let e = sources.entry(name.clone()).or_insert(ChannelAggRow {
                            name,
                            count: 0,
                            cost: 0,
                            tokens: 0,
                            successful: 0,
                            latency_sum: 0,
                        });
                        e.count += 1;
                        if l.status_code < 400 {
                            e.successful += 1;
                        }
                        e.cost += l.cost;
                        e.tokens += l.input_tokens + l.output_tokens;
                        e.latency_sum += l.latency_ms;
                    }
                    items = sources.into_values().collect();
                }
                items.sort_by(|a, b| b.count.cmp(&a.count).then(b.cost.cmp(&a.cost)));
                items.truncate(10);
                json!(items
                    .into_iter()
                    .map(|c| {
                        // 个人支无渠道级 token/成功/延迟数据 → null（不虚构 0%）
                        let (tokens, success_rate, avg_latency) = if personal {
                            (Value::Null, Value::Null, Value::Null)
                        } else {
                            (
                                json!(c.tokens),
                                json!(if c.count > 0 {
                                    (c.successful as f64 / c.count as f64 * 10000.0).round() / 100.0
                                } else {
                                    0.0
                                }),
                                json!(c.latency_sum.checked_div(c.count).unwrap_or(0)),
                            )
                        };
                        json!({
                            "name": c.name,
                            "count": c.count,
                            "cost": c.cost,
                            "tokens": tokens,
                            "success_rate": success_rate,
                            "avg_latency_ms": avg_latency,
                        })
                    })
                    .collect::<Vec<Value>>())
            }
        },
    )
    .await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// prompt/completion 分列 Token 趋势（Agent2API daily token trend）。
pub async fn handle_report_token_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "token_trend",
        &format!("{from}:{to}"),
        user_filter.clone(),
        120,
        {
            let state = state.clone();
            move || {
            if state.log_store.daily.agg_available() {
                let pts: Vec<Value> = match &user_filter {
                None => state
                    .log_store
                    .daily
                    .load_range(&from, &to)
                    .into_iter()
                    .map(|r| {
                        json!({ "date": r.date, "input_tokens": r.tokens_in, "output_tokens": r.tokens_out })
                    })
                    .collect(),
                Some(uid) => state
                    .log_store
                    .daily
                    .load_user_range(uid, &from, &to)
                    .into_iter()
                    .map(|r| {
                        json!({ "date": r.date, "input_tokens": r.tokens_in, "output_tokens": r.tokens_out })
                    })
                    .collect(),
            };
                return json!(pts);
            }
            let to_ts_end = date_start_ts(&to) + 86_400 - 1;
            let logs = state.log_store.requests.all_sorted_asc();
            let mut daily: std::collections::BTreeMap<String, (u64, u64)> =
                std::collections::BTreeMap::new();
            for l in &logs {
                if l.created_at < from_ts || l.created_at > to_ts_end {
                    continue;
                }
                if let Some(uid) = &user_filter {
                    if l.user_id.as_deref() != Some(uid.as_str()) {
                        continue;
                    }
                }
                let day = daily_agg::date_key(l.created_at);
                let entry = daily.entry(day).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(l.input_tokens);
                entry.1 = entry.1.saturating_add(l.output_tokens);
            }
            json!(daily
                .into_iter()
                .map(|(date, (input, output))| {
                    json!({ "date": date, "input_tokens": input, "output_tokens": output })
                })
                .collect::<Vec<Value>>())
            }
        },
    ).await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 报表总览卡（Agent2API report overview：请求/成功/Token/活跃天数）。
///
/// 成功率按 `status_code < 400` 计（与 Agent2API requests 表口径一致）。
/// 新增 `streak`（当前连续活跃天数，A2A「含今天往前数」口径；旧前端
/// 忽略新字段即可，向后兼容）。
pub async fn handle_report_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "overview",
        &format!("{from}:{to}"),
        user_filter.clone(),
        120,
        {
            let state = state.clone();
            move || {
            if state.log_store.daily.agg_available() {
                let mut total = 0u64;
                let mut success = 0u64;
                let mut input_tokens = 0u64;
                let mut output_tokens = 0u64;
                let mut cost = 0i64;
                let mut present: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                let mut top_models: Vec<(String, u64)>;
                match &user_filter {
                    None => {
                        let rows = state.log_store.daily.load_range(&from, &to);
                        for r in &rows {
                            total += r.requests;
                            success += r.successful;
                            input_tokens = input_tokens.saturating_add(r.tokens_in);
                            output_tokens = output_tokens.saturating_add(r.tokens_out);
                            cost = cost.saturating_add(r.cost);
                            if r.requests > 0 {
                                present.insert(r.date.clone());
                            }
                        }
                        top_models = daily_agg::merge_models(&rows)
                            .into_iter()
                            .filter(|m| m.name != daily_agg::OTHER)
                            .take(5)
                            .map(|m| (m.name, m.count))
                            .collect();
                    }
                    Some(uid) => {
                        let rows = state.log_store.daily.load_user_range(uid, &from, &to);
                        for r in &rows {
                            total += r.count;
                            success += r.successful;
                            input_tokens = input_tokens.saturating_add(r.tokens_in);
                            output_tokens = output_tokens.saturating_add(r.tokens_out);
                            cost = cost.saturating_add(r.cost);
                            if r.count > 0 {
                                present.insert(r.date.clone());
                            }
                        }
                        top_models = daily_agg::merge_dim(&rows, |r| &r.models)
                        .into_iter()
                        .filter(|m| m.name != daily_agg::OTHER)
                        .map(|m| (m.name, m.count))
                        .collect::<Vec<_>>()
                        // count 主排（merge_dim 不排序，此处补上）
                        ;
                        top_models.sort_by_key(|a| std::cmp::Reverse(a.1));
                        top_models.truncate(5);
                    }
                }
                // streak 锚定「今天」而非窗口尾（窗口尾若是过去的自定义 end，
                // 连续天数按 A2A 语义仍从今日往前数）。
                let streak = daily_agg::streak_days(&present, &daily_agg::today_key());
                return json!({
                    "total_requests": total,
                    "success_requests": success,
                    "success_rate": if total > 0 { (success as f64 / total as f64 * 10000.0).round() / 100.0 } else { 0.0 },
                    "input_tokens": input_tokens,
                    "output_tokens": output_tokens,
                    "total_cost": cost,
                    "active_days": present.len(),
                    "streak": streak,
                    "top_models": top_models.into_iter()
                        .map(|(model, count)| json!({ "model": model, "count": count }))
                        .collect::<Vec<Value>>(),
                });
            }
            let to_ts_end = date_start_ts(&to) + 86_400 - 1;
            let logs = state.log_store.requests.all_sorted_asc();

            let mut total = 0u64;
            let mut success = 0u64;
            let mut input_tokens = 0u64;
            let mut output_tokens = 0u64;
            let mut cost = 0i64;
            let mut days_seen: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            let mut models: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();

            for l in &logs {
                if l.created_at < from_ts || l.created_at > to_ts_end {
                    continue;
                }
                if let Some(uid) = &user_filter {
                    if l.user_id.as_deref() != Some(uid.as_str()) {
                        continue;
                    }
                }
                total += 1;
                if l.status_code < 400 {
                    success += 1;
                }
                input_tokens = input_tokens.saturating_add(l.input_tokens);
                output_tokens = output_tokens.saturating_add(l.output_tokens);
                cost = cost.saturating_add(l.cost);
                let d = daily_agg::date_key(l.created_at);
                if !d.is_empty() {
                    days_seen.insert(d);
                }
                *models.entry(l.model.clone()).or_insert(0) += 1;
            }
            let mut top_models: Vec<(String, u64)> = models.into_iter().collect();
            top_models.sort_by_key(|a| std::cmp::Reverse(a.1));
            top_models.truncate(5);
            let streak = daily_agg::streak_days(&days_seen, &daily_agg::today_key());

            json!({
                "total_requests": total,
                "success_requests": success,
                "success_rate": if total > 0 { (success as f64 / total as f64 * 10000.0).round() / 100.0 } else { 0.0 },
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_cost": cost,
                "active_days": days_seen.len(),
                "streak": streak,
                "top_models": top_models.into_iter()
                    .map(|(model, count)| json!({ "model": model, "count": count }))
                    .collect::<Vec<Value>>(),
            })
            }
        },
    ).await;
    Ok(Json(json!({ "success": true, "data": data })))
}

// ── 报表内存缓存 ────────────────────────────────────────────────────────────
//
// report 系列在聚合闸门未开（回填前）仍可能全量扫明细，进程内 TTL 缓存
// 兜住管理端体验：同一 (端点, 自然日窗口, 用户范围) 在 TTL 窗口内直接
// 回放上次结果。日志写入不主动失效（报表容忍 ≤TTL 的滞后），TTL 取短：
// 明细类 60s、聚合类 120s。按天落盘聚合上线后（本模块各 handler 的
// agg_available 分支），全量扫只剩回退路径。

type ReportCacheKey = (String, String, String); // (端点, 窗口 "from:to", 用户范围: ""=全量 / user_id)

fn report_cache() -> &'static parking_lot::Mutex<
    std::collections::HashMap<ReportCacheKey, (std::time::Instant, Value)>,
> {
    static CACHE: std::sync::OnceLock<
        parking_lot::Mutex<std::collections::HashMap<ReportCacheKey, (std::time::Instant, Value)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

/// 命中缓存直接回放；未命中把 `compute` 挪到 blocking 池执行并落缓存。
///
/// 同步读库挪出 async worker（移植 Agent2API 68d0a4e 第 21 项：报表接口的
/// 同步读库 spawn_blocking）——KV 读是 rusqlite 串行连接 + 逐键反序列化，
/// 窗口大或走回退全量现算时能吃满几十毫秒，占着 tokio worker 会拖慢同
/// worker 上的转发请求。锁只护 map 本身，`compute` 在锁外执行（宁可同键
/// 并发算两次，也不把重计算塞进临界区）。
///
/// 无 tokio runtime（同步测试上下文）时直接同步执行，语义不变。
async fn cached_report(
    endpoint: &str,
    window: &str,
    user_filter: Option<String>,
    ttl_secs: u64,
    compute: impl FnOnce() -> Value + Send + 'static,
) -> Value {
    let key: ReportCacheKey = (
        endpoint.to_string(),
        window.to_string(),
        user_filter.unwrap_or_default(),
    );
    {
        let map = report_cache().lock();
        if let Some((at, v)) = map.get(&key) {
            if at.elapsed() < std::time::Duration::from_secs(ttl_secs) {
                return v.clone();
            }
        }
    }
    let fresh = match tokio::runtime::Handle::try_current() {
        Ok(_) => tokio::task::spawn_blocking(compute)
            .await
            .unwrap_or_else(|e| {
                // compute panic（blocking 线程已捕获原 panic，进程未 abort）：
                // 请求失败即可，不缓存空值
                panic!("报表计算任务 join 失败：{e}")
            }),
        Err(_) => compute(),
    };
    {
        let mut map = report_cache().lock();
        evict_report_cache(&mut map, REPORT_CACHE_MAX);
        map.insert(key, (std::time::Instant::now(), fresh.clone()));
    }
    fresh
}

/// 报表缓存条目上限。
const REPORT_CACHE_MAX: usize = 256;

/// 缓存超限时按插入时间淘汰最旧的一批，保留较新的条目。
///
/// 取代原先的「超 256 条整表 `clear()`」：高基数场景（多用户 × 多窗口 ×
/// 多端点）下整表清空会让所有报表一起击穿、周期性重算。这里每次只淘汰
/// **最旧的四分之一**（至少 1 条），把「全清」退化为「滚动淘汰」，热条目
/// 得以保留。`map` 的值是 `(Instant, Value)`，直接用其插入时间排序。
fn evict_report_cache(
    map: &mut std::collections::HashMap<ReportCacheKey, (std::time::Instant, Value)>,
    max: usize,
) {
    if map.len() <= max {
        return;
    }
    let evict = (max / 4).max(1);
    let mut by_age: Vec<(ReportCacheKey, std::time::Instant)> =
        map.iter().map(|(k, (at, _))| (k.clone(), *at)).collect();
    by_age.sort_by_key(|(_, at)| *at);
    for (k, _) in by_age.into_iter().take(evict) {
        map.remove(&k);
    }
}

/// 模型分布（Agent2API report models 维度）：按请求数/Token/消费额排布。
pub async fn handle_report_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report("models", &format!("{from}:{to}"), user_filter.clone(), 120, {
        let state = state.clone();
        move || {
        if state.log_store.daily.agg_available() {
            let items: Vec<Value> = match &user_filter {
                None => daily_agg::merge_models(&state.log_store.daily.load_range(&from, &to))
                    .into_iter()
                    .map(|m| {
                        json!({
                            "model": m.name, "count": m.count,
                            "input_tokens": m.tokens_in, "output_tokens": m.tokens_out, "cost": m.cost,
                        })
                    })
                    .collect(),
                Some(uid) => {
                    let mut merged = daily_agg::merge_dim(
                        &state.log_store.daily.load_user_range(uid, &from, &to),
                        |r| &r.models,
                    );
                    merged.sort_by_key(|m| std::cmp::Reverse(m.count));
                    merged
                        .into_iter()
                        .map(|m| {
                            json!({
                                "model": m.name, "count": m.count,
                                "input_tokens": m.tokens_in, "output_tokens": m.tokens_out, "cost": m.cost,
                            })
                        })
                        .collect()
                }
            };
            return json!(items);
        }
        let to_ts_end = date_start_ts(&to) + 86_400 - 1;
        let logs = state.log_store.requests.all_sorted_asc();
        let mut agg: std::collections::HashMap<String, (u64, u64, u64, i64)> =
            std::collections::HashMap::new();
        for l in &logs {
            if l.created_at < from_ts || l.created_at > to_ts_end {
                continue;
            }
            if let Some(uid) = &user_filter {
                if l.user_id.as_deref() != Some(uid.as_str()) {
                    continue;
                }
            }
            let e = agg.entry(l.model.clone()).or_insert((0, 0, 0, 0));
            e.0 += 1;
            e.1 = e.1.saturating_add(l.input_tokens);
            e.2 = e.2.saturating_add(l.output_tokens);
            e.3 = e.3.saturating_add(l.cost);
        }
        let mut items: Vec<(String, (u64, u64, u64, i64))> = agg.into_iter().collect();
        items.sort_by_key(|a| std::cmp::Reverse(a.1 .0));
        json!(items
            .into_iter()
            .map(|(model, (count, it, ot, cost))| json!({
                "model": model,
                "count": count,
                "input_tokens": it,
                "output_tokens": ot,
                "cost": cost,
            }))
            .collect::<Vec<Value>>())
        }
    }).await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 缓存命中率四档窗口（Agent2API cacheRates：10m/1h/24h/7d）。
///
/// AIGX 日志的 `cache_hit` 是请求级布尔（无命中 token 数），口径取
/// 「命中请求数 / 总请求数」，并附命中请求承担的输入 Token 作参考。
/// 窗口粒度到分钟级，日聚合覆盖不了——走 `reqlog:` 键区间读（时间戳
/// 定宽 → 字典序=数值序），最大窗口 7d 也只扫近窗键段；超过护栏条数
/// 时截断并在响应标 `sampled:true`（宁可标注抽样，不扫穿全表）。
pub async fn handle_report_cache_rates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "cache_rates",
        &format!("{from}:{to}"),
        user_filter.clone(),
        60,
        {
            let state = state.clone();
            move || {
            let now = chrono::Utc::now().timestamp();
            let windows: [(&str, i64); 4] = [
                ("10m", 600),
                ("1h", 3600),
                ("24h", 24 * 3600),
                ("7d", 7 * 24 * 3600),
            ];
            // 一次近窗读覆盖最大窗口（7d），四档在内存里各自过滤时间下界。
            let wide_from = (now - 7 * 24 * 3600).max(from_ts);
            let (logs, sampled) = state
                .log_store
                .requests
                .list_window(wide_from, now, 20_000)
                .unwrap_or_default();
            let mut out = Vec::new();
            for (label, span) in windows {
                let win_from = now - span;
                let mut total = 0u64;
                let mut hit = 0u64;
                let mut input_tokens = 0u64;
                let mut hit_input_tokens = 0u64;
                for l in &logs {
                    if l.created_at < win_from {
                        continue;
                    }
                    if let Some(uid) = &user_filter {
                        if l.user_id.as_deref() != Some(uid.as_str()) {
                            continue;
                        }
                    }
                    total += 1;
                    input_tokens = input_tokens.saturating_add(l.input_tokens);
                    if l.cache_hit {
                        hit += 1;
                        hit_input_tokens = hit_input_tokens.saturating_add(l.input_tokens);
                    }
                }
                out.push(json!({
                "window": label,
                "requests": total,
                "hit_requests": hit,
                "rate": if total > 0 { (hit as f64 / total as f64 * 10000.0).round() / 100.0 } else { 0.0 },
                "input_tokens": input_tokens,
                "hit_input_tokens": hit_input_tokens,
                // 键区间读护栏触发标记（新字段向后兼容，不新增数组元素）。
                "sampled": sampled,
            }));
            }
            json!(out)
            }
        },
    ).await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// GET /api/dashboard/report/cache_trend_24h — 近 24 个本地整点缓存命中双轴序列。
///
/// A2A「近 24 小时命中率折线」的 AIGX 对应物：左轴命中率、右轴总 Token。
/// 分钟级窗口日聚合覆盖不了——与 cache_rates 同款走 `reqlog:` 键区间读
/// （护栏 2 万条，超限每元素标 `sampled:true`）。窗口固定 24h，不受
/// days/start/end 影响。
pub async fn handle_report_cache_trend_24h(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    // 缓存键只到小时粒度：同一小时内反复打开页面命中 60s TTL 不误重算，
    // 跨小时自动重算（补全上一个完整小时的数据）。
    let slot = local_hour_key(now);
    let data = cached_report("cache_trend_24h", &slot, user_filter.clone(), 60, {
        let state = state.clone();
        move || {
            // 多取 1 小时余量：首桶可能只覆盖 [now−24h, 桶起点] 的部分区间，
            // 起点按整点桶表精确过滤即可。
            let from = now - 25 * 3600;
            let (logs, sampled) = state
                .log_store
                .requests
                .list_window(from, now, 20_000)
                .unwrap_or_default();
            let filtered: Vec<crate::log::RequestLog> = logs
                .into_iter()
                .filter(|l| match (user_filter.clone(), l.user_id.as_deref()) {
                    (None, _) => true,
                    (Some(uid), Some(lu)) => lu == uid.as_str(),
                    (Some(_), None) => false,
                })
                .collect();
            json!(bucket_hours(
                &filtered,
                now,
                sampled,
                crate::config::report_tz_offset()
            ))
        }
    })
    .await;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// unix 秒 → 报表时区整点键 `YYYY-MM-DDTHH`。
///
/// P3-14：口径从「服务器本地（`chrono::Local`）」改为 `[report] timezone_offset`
/// 快照，与日粒度同一条边界——以前同一面板上「日粒度=UTC、小时桶=Local」两套边界
/// 并存，是 08-prod 记录的原始症状。缓存 slot 键随之变（形态不变，跨口径自然失效
/// 重算，无害）。
fn local_hour_key(ts: i64) -> String {
    hour_key_at(ts, crate::config::report_tz_offset())
}

/// [`local_hour_key`] 的纯函数变体（单测喂 offset，不吃全局快照）。
fn hour_key_at(ts: i64, offset: i32) -> String {
    daily_agg::report_tz_of(offset)
        .timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H").to_string())
        .unwrap_or_default()
}

/// 24h 分桶累加器：(总请求, 命中请求, 输入 Token, 命中输入 Token, 总 Token, 模型→Token)。
type HourBucketAcc = (
    u64,
    u64,
    u64,
    u64,
    u64,
    std::collections::BTreeMap<String, u64>,
);

/// 把明细按报表时区整点分进「含当前小时的近 24 桶」（升序，缺数据的桶补零行）。
///
/// 纯函数便于单测（`offset` 由调用方给，测试直接喂值、不碰全局快照）；logs 需调用方
/// 先按 user 过滤。口径与 cache_rates 同一条：命中率 = 命中请求数/总请求数（cache_hit
/// 是请求级布尔），附命中请求承担的输入 Token；models 分解按 token 降序取前 8
/// （hover 多行气泡用）。
fn bucket_hours(
    logs: &[crate::log::RequestLog],
    now: i64,
    sampled: bool,
    offset: i32,
) -> Vec<Value> {
    // 当前报表时区整点起点，往回 23 个桶
    let tz = daily_agg::report_tz_of(offset);
    let Some(now_dt) = tz.timestamp_opt(now, 0).single() else {
        return Vec::new();
    };
    let Some(cur) = now_dt
        .with_minute(0)
        .and_then(|d| d.with_second(0))
        .and_then(|d| d.with_nanosecond(0))
    else {
        return Vec::new();
    };
    let mut keys: Vec<(String, i64, i64)> = Vec::with_capacity(24);
    for i in (0..24i64).rev() {
        let dt = cur - chrono::Duration::hours(i);
        keys.push((
            dt.format("%Y-%m-%dT%H").to_string(),
            dt.timestamp(),
            dt.timestamp() + 3600,
        ));
    }
    // 每桶累加器见 HourBucketAcc（total, hit, input, hit_input, total_tokens, model→tokens）
    let mut acc: Vec<HourBucketAcc> = (0..keys.len()).map(|_| Default::default()).collect();
    for l in logs {
        let Some(idx) = keys
            .iter()
            .position(|(_, s, e)| l.created_at >= *s && l.created_at < *e)
        else {
            continue;
        };
        let a = &mut acc[idx];
        a.0 += 1;
        a.2 += l.input_tokens;
        a.4 = a.4.saturating_add(l.input_tokens + l.output_tokens);
        if l.cache_hit {
            a.1 += 1;
            a.3 += l.input_tokens;
        }
        *a.5.entry(l.model.clone()).or_insert(0) += l.input_tokens + l.output_tokens;
    }
    acc
        .into_iter()
        .enumerate()
        .map(|(i, a)| {
            let mut ms: Vec<(String, u64)> = a.5.into_iter().collect();
            ms.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            json!({
                "hour": keys[i].0,
                "requests": a.0,
                "hit_requests": a.1,
                "rate": if a.0 > 0 { (a.1 as f64 / a.0 as f64 * 10000.0).round() / 100.0 } else { 0.0 },
                "input_tokens": a.2,
                "hit_input_tokens": a.3,
                "total_tokens": a.4,
                "models": ms
                    .into_iter()
                    .take(8)
                    .map(|(model, tk)| json!({ "model": model, "tokens": tk }))
                    .collect::<Vec<Value>>(),
                // 键区间读护栏标记（与 cache_rates 同手法：挂元素不增数组）
                "sampled": sampled,
            })
        })
        .collect()
}

/// 按天消费/请求趋势（Agent2API dailyTrend）：供仪表盘折线复用。
///
/// 每行附 `models`（当日 Top6 模型 {model,count}，新字段向后兼容）——
/// 供热力图/趋势 hover 的按模型分解 tooltip（A2A 口径）。
pub async fn handle_report_daily_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (user_filter, _is_admin) = report_scope(&state, &headers).await?;
    let (from, to, from_ts) = resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let data = cached_report(
        "daily_trend",
        &format!("{from}:{to}"),
        user_filter.clone(),
        120,
        {
            let state = state.clone();
            move || {
                if state.log_store.daily.agg_available() {
                    let rows: Vec<Value> = match &user_filter {
                        None => state
                            .log_store
                            .daily
                            .load_range(&from, &to)
                            .into_iter()
                            .map(|r| {
                                let top: Vec<Value> = {
                                    let mut ms = r.by_model.clone();
                                    ms.sort_by_key(|m| std::cmp::Reverse(m.count));
                                    ms.into_iter()
                                        .take(6)
                                        .map(|m| {
                                            json!({
                                                "model": m.name,
                                                "count": m.count,
                                                "tokens": m.tokens_in + m.tokens_out,
                                            })
                                        })
                                        .collect()
                                };
                                json!({
                                    "date": r.date, "count": r.requests,
                                    "input_tokens": r.tokens_in, "output_tokens": r.tokens_out,
                                    "cost": r.cost, "models": top,
                                })
                            })
                            .collect(),
                        Some(uid) => state
                            .log_store
                            .daily
                            .load_user_range(uid, &from, &to)
                            .into_iter()
                            .map(|r| {
                                let mut ms = r.models.clone();
                                ms.sort_by_key(|m| std::cmp::Reverse(m.count));
                                let top: Vec<Value> = ms
                                    .into_iter()
                                    .take(6)
                                    .map(|m| {
                                        json!({
                                            "model": m.name,
                                            "count": m.count,
                                            "tokens": m.tokens_in + m.tokens_out,
                                        })
                                    })
                                    .collect();
                                json!({
                                    "date": r.date, "count": r.count,
                                    "input_tokens": r.tokens_in, "output_tokens": r.tokens_out,
                                    "cost": r.cost, "models": top,
                                })
                            })
                            .collect(),
                    };
                    return json!(rows);
                }
                let to_ts_end = date_start_ts(&to) + 86_400 - 1;
                let logs = state.log_store.requests.all_sorted_asc();
                let mut daily: std::collections::BTreeMap<String, (u64, u64, u64, i64)> =
                    std::collections::BTreeMap::new();
                let mut per_day_models: std::collections::BTreeMap<
                    String,
                    std::collections::HashMap<String, (u64, u64)>,
                > = std::collections::BTreeMap::new();
                for l in &logs {
                    if l.created_at < from_ts || l.created_at > to_ts_end {
                        continue;
                    }
                    if let Some(uid) = &user_filter {
                        if l.user_id.as_deref() != Some(uid.as_str()) {
                            continue;
                        }
                    }
                    let day = daily_agg::date_key(l.created_at);
                    let e = daily.entry(day.clone()).or_insert((0, 0, 0, 0));
                    e.0 += 1;
                    e.1 = e.1.saturating_add(l.input_tokens);
                    e.2 = e.2.saturating_add(l.output_tokens);
                    e.3 = e.3.saturating_add(l.cost);
                    let tk = l.input_tokens + l.output_tokens;
                    let m = per_day_models
                        .entry(day)
                        .or_default()
                        .entry(l.model.clone())
                        .or_insert((0, 0));
                    m.0 += 1;
                    m.1 = m.1.saturating_add(tk);
                }
                json!(daily
                    .into_iter()
                    .map(|(date, (count, it, ot, cost))| {
                        let mut ms: Vec<(String, (u64, u64))> = per_day_models
                            .remove(&date)
                            .unwrap_or_default()
                            .into_iter()
                            .collect();
                        ms.sort_by_key(|a| std::cmp::Reverse(a.1 .0));
                        let top: Vec<Value> = ms
                        .into_iter()
                        .take(6)
                        .map(|(model, (c, tk))| json!({ "model": model, "count": c, "tokens": tk }))
                        .collect();
                        json!({
                            "date": date, "count": count,
                            "input_tokens": it, "output_tokens": ot, "cost": cost,
                            "models": top,
                        })
                    })
                    .collect::<Vec<Value>>())
            }
        },
    )
    .await;
    Ok(Json(json!({ "success": true, "data": data })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// days 语义 = 含今天的 N 个自然日（A2A 口径），from_ts 对齐日起点。
    #[test]
    fn resolve_window_days_is_calendar_inclusive() {
        let (from, to, from_ts) = resolve_window(30, None, None);
        assert_eq!(to, daily_agg::today_key());
        assert_eq!(from, shift_date(&to, 29));
        assert_eq!(from_ts, date_start_ts(&from));
    }

    /// custom 起止：写反交换、跨度 >365 拉近起点、非法格式忽略回 days。
    #[test]
    fn resolve_window_custom_swaps_and_clamps() {
        let (from, to, _) = resolve_window(30, Some("2026-11-10"), Some("2026-11-01"));
        assert_eq!((from.as_str(), to.as_str()), ("2026-11-01", "2026-11-10"));

        let (from, to, _) = resolve_window(30, Some("1999-01-01"), Some("2026-11-01"));
        assert_eq!(to, "2026-11-01");
        assert_eq!(from, shift_date("2026-11-01", 364));

        let (from, to, _) = resolve_window(7, Some("2026-11"), Some("garbage"));
        assert_eq!(to, daily_agg::today_key());
        assert_eq!(from, shift_date(&to, 6));
    }

    /// 单独指定 end（历史锚点）时 days 相对该尾日回溯。
    #[test]
    fn resolve_window_end_only_anchors_days_to_it() {
        let (from, to, _) = resolve_window(10, None, Some("2026-06-15"));
        assert_eq!(to, "2026-06-15");
        assert_eq!(from, "2026-06-06");
    }

    /// **P3-14 的核心防复发钉**：报表窗口下界（`date_start_ts_at`，明细现算回退路径
    /// 用它）与日键（`daily_agg::date_key_at`，聚合路径用它）必须是**同一条日界**。
    /// 两条路径一旦分裂，同一面板上「日粒度」与「自定义区间」的数字会对不上，比现状
    /// （UTC vs Local 两套并存）更难查。三种 offset 各钉一遍往返恒等 + 谓词选出同一批。
    #[test]
    fn day_window_and_day_key_are_the_same_boundary() {
        for offset in [0i32, 8, -3] {
            let day = "2026-11-08";
            let start = date_start_ts_at(day, offset);
            let end = start + 86_400 - 1;
            assert_ne!(start, i64::MIN, "offset={offset} 合法日期不该解析失败");
            assert_eq!(
                daily_agg::date_key_at(start, offset),
                day,
                "offset={offset}：窗口下界必须落在当日"
            );
            assert_eq!(
                daily_agg::date_key_at(end, offset),
                day,
                "offset={offset}：+86400−1 仍是当日最后一秒（固定偏移下日长恒 86400）"
            );
            assert_eq!(daily_agg::date_key_at(end + 1, offset), "2026-11-09");
            assert_eq!(daily_agg::date_key_at(start - 1, offset), "2026-11-07");
            // 谓词等价：时间窗选出的样本 = 日键选出的样本（读侧两条路必须给同一日同一数）
            let samples = [start - 1, start, start + 3600, end - 1, end, end + 1];
            let by_window = samples
                .iter()
                .filter(|ts| **ts >= start && **ts <= end)
                .count();
            let by_key = samples
                .iter()
                .filter(|ts| daily_agg::date_key_at(**ts, offset) == day)
                .count();
            assert_eq!(
                by_window, by_key,
                "offset={offset}：窗口谓词与日键选出的不是同一批（{samples:?}）"
            );
            assert_eq!(
                by_window, 4,
                "offset={offset}：当日样本数应为 4（钉住用例本身）"
            );
        }
    }

    /// 小时键随 offset 平移（原 `Local` 口径换成报表时区口径后，跨口径必须真的动）。
    ///
    /// 具体钟点只钉**固定偏移**；`offset = 0` 分支与系统本地时区比对，不写死字面量
    /// ——CI runner 是 UTC 属于巧合，拿它当期望值就是下一个「换机器就红」的假红雷。
    #[test]
    fn hour_keys_follow_report_offset() {
        // 2026-11-08T16:30:00Z
        let ts = chrono::NaiveDate::from_ymd_opt(2026, 11, 8)
            .unwrap()
            .and_hms_opt(16, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        assert_eq!(hour_key_at(ts, 8), "2026-11-09T00", "UTC+8 已跨日");
        assert_eq!(hour_key_at(ts, 5), "2026-11-08T21");
        assert_eq!(hour_key_at(ts, -3), "2026-11-08T13");
        // offset = 0 → 跟随系统本地时区（与 `chrono::Local` 的墙钟一致即可）
        let local_key = chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
            .map(|d| {
                d.with_timezone(&chrono::Local)
                    .format("%Y-%m-%dT%H")
                    .to_string()
            })
            .unwrap();
        assert_eq!(hour_key_at(ts, 0), local_key, "offset=0 = 系统本地");
        // 同一 now 喂不同 offset：末桶随动，桶数与升序不变
        let keys = |off: i32| -> Vec<String> {
            bucket_hours(&[], ts, false, off)
                .iter()
                .map(|v| v["hour"].as_str().unwrap().to_string())
                .collect()
        };
        let (a, b) = (keys(5), keys(8));
        assert_eq!(a.len(), 24);
        assert_eq!(b.len(), 24);
        assert_eq!(a[23], hour_key_at(ts, 5));
        assert_eq!(b[23], hour_key_at(ts, 8));
        assert_ne!(a[23], b[23], "offset 变了末桶必须变");
        assert!(b.windows(2).all(|w| w[0] < w[1]), "跨日也要严格升序");
    }

    /// 快照变体与纯函数变体同源（防止有人只改其中一个 → 生产与测试口径分家）。
    #[test]
    fn snapshot_variants_match_pure_variants() {
        let off = crate::config::report_tz_offset();
        let ts = 1_730_000_000_i64;
        assert_eq!(
            date_start_ts("2026-11-08"),
            date_start_ts_at("2026-11-08", off)
        );
        assert_eq!(local_hour_key(ts), hour_key_at(ts, off));
    }

    fn bkt_log(ts: i64, model: &str, it: u64, ot: u64, hit: bool) -> crate::log::RequestLog {
        let mut l = crate::log::RequestLog::new();
        l.created_at = ts;
        l.model = model.to_string();
        l.input_tokens = it;
        l.output_tokens = ot;
        l.cache_hit = hit;
        l
    }

    /// 桶/窗口口径类测试统一喂显式 offset（**不碰全局快照** → 与并行用例互不污染，
    /// 也不受 CI runner 的 `Local` 影响；P3-14 的验收点恰恰是「口径可换、内部自洽」）。
    const TZ_OFF: i32 = 8;

    /// 空数据也给满 24 桶（补零行）；hour 键严格递增且都是 13 字符。
    #[test]
    fn bucket_hours_always_yields_24_slots() {
        let now = chrono::Utc::now().timestamp();
        let out = bucket_hours(&[], now, false, TZ_OFF);
        assert_eq!(out.len(), 24);
        assert_eq!(out[23]["requests"], 0);
        assert_eq!(out[23]["rate"], 0.0);
        assert_eq!(out[23]["sampled"], false);
        let hours: Vec<&str> = out.iter().map(|v| v["hour"].as_str().unwrap()).collect();
        for w in hours.windows(2) {
            assert!(w[0] < w[1], "桶键必须严格升序: {:?} > {:?}", w[1], w[0]);
        }
        assert_eq!(
            hours[23],
            hour_key_at(now, TZ_OFF),
            "末桶 = 当前报表时区整点"
        );
    }

    /// 命中/总量/token 归并进正确桶；models 分解按 token 降序。
    #[test]
    fn bucket_hours_groups_by_hour_and_decomposes_models() {
        let now = chrono::Utc::now().timestamp();
        let two_ago = now - 2 * 3600 - 1; // 2 个整点桶之前（桶 [cur−2h, cur−1h)）
        let logs = vec![
            bkt_log(now, "gpt-4", 100, 20, true),
            bkt_log(now, "gpt-4", 50, 10, false),
            bkt_log(now, "claude", 500, 1, false),
            bkt_log(two_ago, "gpt-4", 1, 1, false),
        ];
        let out = bucket_hours(&logs, now, true, TZ_OFF);
        let cur = &out[23];
        assert_eq!(cur["requests"], 3);
        assert_eq!(cur["hit_requests"], 1);
        assert_eq!(cur["rate"], 33.33);
        assert_eq!(cur["input_tokens"], 650);
        assert_eq!(cur["hit_input_tokens"], 100);
        // (100+20) + (50+10) + (500+1) = 681
        assert_eq!(cur["total_tokens"], 681);
        assert_eq!(cur["sampled"], true);
        // models 按 token 降序：claude 501 > gpt-4 180
        let ms = cur["models"].as_array().unwrap();
        assert_eq!(ms[0]["model"], "claude");
        assert_eq!(ms[0]["tokens"], 501);
        assert_eq!(ms[1]["model"], "gpt-4");
        assert_eq!(ms[1]["tokens"], 180);
        // 2 桶前那条按它自己的整点键定位（now 处于钟点的位置不定，索引会变）
        let key2 = hour_key_at(two_ago, TZ_OFF);
        let slot = out.iter().find(|v| v["hour"] == key2).expect("早桶存在");
        assert_eq!(slot["requests"], 1);
    }

    /// 报表缓存超限时只滚动淘汰最旧的 25%，不整表清空；最新 key 始终保留。
    #[test]
    fn report_cache_evicts_oldest_quarter_not_all() {
        use std::collections::HashMap;
        use std::time::{Duration, Instant};

        let base = Instant::now();
        let key = |i: u64| (format!("e{i}"), String::new(), String::new());

        // 精确验证淘汰规模：257 条触发一次淘汰 → 删最旧 64 条（256/4），剩 193。
        let mut m: HashMap<ReportCacheKey, (Instant, Value)> = HashMap::new();
        for i in 0..257u64 {
            m.insert(key(i), (base + Duration::from_secs(i), json!(i)));
        }
        evict_report_cache(&mut m, 256);
        assert_eq!(m.len(), 257 - 64, "只淘汰最旧 25%（64 条），不清空整表");
        assert!(m.contains_key(&key(256)), "最新 key 必须保留");
        assert!(!m.contains_key(&key(0)), "最旧 key 应被淘汰");
        assert!(m.contains_key(&key(64)), "只淘汰最旧 64 条，第 65 老仍在");

        // 模拟逐条插入 300 条：全程不超上限、不整表清空、最新在。
        let mut acc: HashMap<ReportCacheKey, (Instant, Value)> = HashMap::new();
        for i in 0..300u64 {
            acc.insert(key(i), (base + Duration::from_secs(i), json!(i)));
            evict_report_cache(&mut acc, 256);
            assert!(acc.len() <= 256, "缓存不应突破上限");
        }
        assert!(!acc.is_empty() && acc.len() < 300, "应为部分淘汰而非全清");
        assert!(acc.contains_key(&key(299)), "最新插入的 key 仍在");
        assert!(!acc.contains_key(&key(0)), "最旧插入的 key 已淘汰");
    }
}
