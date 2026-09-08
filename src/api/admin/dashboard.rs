//! 数据看板 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供消费趋势、模型分布、渠道统计等数据看板功能。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

// Dashboard 查询参数：时间范围（天数）。
#[derive(Debug, Deserialize)]
pub struct DashboardQuery {
    #[serde(default = "default_dashboard_days")]
    pub days: u32,
}

fn default_dashboard_days() -> u32 {
    30
}

/// 将 days 限制在 [1, 90] 区间，并返回对应的 unix timestamp 下界。
pub fn dashboard_start_ts(days: u32) -> i64 {
    let days = days.clamp(1, 90) as i64;
    chrono::Utc::now().timestamp() - days * 24 * 3600
}

/// 消费趋势
pub async fn handle_consumption_trend(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.all_sorted_asc();
    let mut daily: std::collections::BTreeMap<String, (i64, u64)> =
        std::collections::BTreeMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        let day = chrono::DateTime::<chrono::Utc>::from_timestamp(l.created_at, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default();
        let entry = daily.entry(day).or_insert((0, 0));
        entry.0 += l.cost;
        entry.1 += 1;
    }
    let data: Vec<Value> = daily
        .into_iter()
        .map(|(day, (cost, count))| json!({ "date": day, "cost": cost, "count": count }))
        .collect();
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 模型分布
pub async fn handle_model_distribution(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.all_sorted_asc();
    let mut models: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    for l in &logs {
        if l.created_at < start {
            continue;
        }
        *models.entry(l.model.clone()).or_insert(0) += 1;
    }
    let data: Vec<Value> = models
        .into_iter()
        .map(|(model, count)| json!({ "model": model, "count": count }))
        .collect();
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
            "active_channels": channels.len(),
        })
    })))
}

/// 用户统计
pub async fn handle_user_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let users = state.user_store.list();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "total_users": users.len(),
            "active_users": users.len(),
            "total_quota": users.iter().map(|u| u.quota).sum::<i64>(),
            "quota_used": users.iter().map(|u| u.used_quota).sum::<i64>(),
        })
    })))
}

/// API 调用统计
pub async fn handle_api_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let logs = state.log_store.requests.all_sorted_asc();
    let total_requests = logs.len();
    let total_cost = logs.iter().map(|l| l.cost).sum::<i64>();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "total_requests": total_requests,
            "total_cost": total_cost,
            "avg_cost_per_request": if total_requests > 0 { total_cost / total_requests as i64 } else { 0 },
        })
    })))
}

/// 总览
pub async fn handle_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let logs = state.log_store.requests.all_sorted_asc();
    let channels = state.channel_store.list();
    let users = state.user_store.list();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "users": json!({
                "count": users.len(),
                "active": users.len(),
                "quota_usage": if users.is_empty() { 0 } else { users.iter().map(|u| u.quota - u.used_quota).sum::<i64>() / users.len() as i64 },
            }),
            "channels": json!({
                "count": channels.len(),
                "enabled": channels.len(),
            }),
            "requests": json!({
                "total": logs.len(),
                "cost_last_24h": 0,
            }),
            "trends": json!({
                "consumption": 0,
                "growth": 0,
            })
        })
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
pub async fn handle_cache_savings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DashboardQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let start = dashboard_start_ts(q.days);
    let logs = state.log_store.requests.all_sorted_asc();

    let mut hit_count: u64 = 0;
    let mut cache_tokens: u64 = 0;
    let mut billed_cost: i64 = 0;
    let mut saved_estimate: f64 = 0.0;

    for log in logs.iter().filter(|l| l.cache_hit && l.created_at >= start) {
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
