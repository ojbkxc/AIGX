// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 签到记录明细台账的路由（签到中心「签到记录」页的数据源）。
//!
//! 对照 Agent2API `api/checkin_records.rs`（`query_records` / `clear_records`），
//! 数据落点换成 AIGX 既有的 `CheckinStore` 按日分桶台账（见 `checkin::store`）。
//!
//! ```text
//!   GET    /api/checkin-records   查询（?limit=&offset=&outcome=&kind=&provider=&keyword=&start=&end=）
//!   DELETE /api/checkin-records   删除（带筛选 = 只删命中的；全清必须显式 all=1）
//! ```
//!
//! ## 响应形状
//!
//! GET → `{records: [...], total, stats}`：total 是**同条件**的总数（分页器用它算
//! 页数），records 按时间倒序。字段用 A2A 的 camelCase 形状（`accountId` /
//! `accountName` / `keptActive` / `streakDays` / `rowId`…），与签到中心聚合页对齐
//! （`stats` 是 AIGX 扩展，统计卡直读）。DELETE → `{success:true, removed}`
//! （removed = 删除条数）。
//!
//! ## DELETE 的护栏（照抄 logs 的规则）
//!
//! 带任一筛选参数时只删命中的条目（与 GET 同一份过滤链 —— 界面上筛出什么
//! 就删什么）；清空全部必须**显式**带 `all=1`，不带的空请求给 400。
//!
//! ## kind 三档
//!
//! 记录页的「签到类型」筛选把来源归成三档（对齐 A2A `Query::clause`）：
//! `manual` = manual / single / activity；`schedule` = schedule（排除 reason 以
//! 「补签」开头的旧行）；`makeup` = makeup / startup（外加旧口径的补签行）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Json;
use axum::routing::any;
use axum::Router;
use serde_json::{json, Value};

use crate::api::openai::AppState;
use crate::checkin::store::{CheckinOutcome, CheckinRecord};

use super::common::{error_response, verify_admin};

type Params = HashMap<String, String>;

/// 管理面路由。
pub fn router() -> Router<AppState> {
    Router::new().route("/api/checkin-records", any(entry))
}

/// `GET` / `DELETE` 单入口（方法判定收在这里，避免「已注册路径 + 未注册方法」
/// 落到全局 405 兜底）。
async fn entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
    method: Method,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match method {
        Method::GET => query_records(&state, &params),
        Method::DELETE => clear_records(&state, &params),
        _ => Err(error_response(
            "只支持 GET / DELETE",
            StatusCode::METHOD_NOT_ALLOWED,
        )),
    }
}

fn parse_usize(value: Option<&String>) -> Option<usize> {
    value?.trim().parse::<usize>().ok()
}

fn parse_ms(value: Option<&String>) -> Option<i64> {
    value?.trim().parse::<i64>().ok()
}

fn outcome_of(value: Option<&String>) -> Option<CheckinOutcome> {
    match value.map(|v| v.trim()) {
        Some("success") => Some(CheckinOutcome::Success),
        Some("already") => Some(CheckinOutcome::Already),
        Some("pending") => Some(CheckinOutcome::Pending),
        Some("failed") => Some(CheckinOutcome::Failed),
        Some("captcha") => Some(CheckinOutcome::Captcha),
        _ => None,
    }
}

/// 一条记录 → 对外 JSON。
///
/// 字段一律 **camelCase**，与 A2A `core/checkin_records::query` 的行形状逐项
/// 对齐（`rowId / accountId / accountName / providerId / keptActive /
/// streakDays`）。`provider` 是展示名（providerId 才是原始 id）—— 与 A2A
/// 同口径：界面直接展示这一列，id 只有开发者认得出。
fn record_json(record: &CheckinRecord, row_id: usize) -> Value {
    json!({
        "rowId": row_id,
        "ts": record.ts,
        "date": record.date,
        "source": record.source,
        "kind": record.kind,
        "reason": record.reason,
        "accountId": record.account_id,
        "accountName": record.account_name,
        "provider": crate::model::manage::provider_label(&record.provider),
        "providerId": record.provider,
        "outcome": record.outcome.as_str(),
        "ok": record.ok,
        "message": record.message,
        "error": record.error,
        "keptActive": record.kept_active,
        "streakDays": record.streak_days,
    })
}

/// `kind` 三档筛选（在内存里做，`CheckinStore` 的签名不动）。
fn kind_matches(record: &CheckinRecord, kind: &str) -> bool {
    let source = if record.source.is_empty() {
        record.kind.as_str()
    } else {
        record.source.as_str()
    };
    match kind {
        "manual" => matches!(source, "manual" | "single" | "activity"),
        "schedule" => source == "schedule" && !record.reason.starts_with("补签"),
        "makeup" => {
            matches!(source, "makeup" | "startup")
                || (source == "schedule" && record.reason.starts_with("补签"))
        }
        // AIGX 扩展档（A2A 只有前三档）：窗口重试轮单列
        "retry" => source == "retry",
        _ => true,
    }
}

/// 时间区间过滤（闭区间；`ts` 是秒，`start` / `end` 按毫秒传）。
fn within_range(record: &CheckinRecord, start: Option<i64>, end: Option<i64>) -> bool {
    let ts_ms = record.ts.saturating_mul(1000);
    start.is_none_or(|s| ts_ms >= s) && end.is_none_or(|e| ts_ms <= e)
}

/// 是否有任一筛选条件（DELETE 的护栏判据）。
fn has_filters(params: &Params) -> bool {
    ["outcome", "kind", "provider", "keyword", "start", "end"]
        .iter()
        .any(|key| params.get(*key).is_some_and(|v| !v.trim().is_empty()))
}

/// 过滤后的全部记录（查询与删除共用同一份过滤链 —— 「筛出什么就删什么」）。
fn filtered(
    state: &AppState,
    params: &Params,
) -> Result<Vec<CheckinRecord>, (StatusCode, Json<Value>)> {
    // 扫描窗口：start 给了就按它推天数，否则默认最近 90 天（台账本身全量保留）
    let days = parse_ms(params.get("start"))
        .map(|start_ms| {
            let now_ms = chrono::Utc::now().timestamp_millis();
            (((now_ms - start_ms).max(0) / 86_400_000).max(1) as u32).saturating_add(2)
        })
        .unwrap_or(90);
    let records = state
        .upstream_checkin
        .recent_records(
            days,
            params
                .get("provider")
                .map(|v| v.trim())
                .filter(|v| !v.is_empty()),
            None,
            outcome_of(params.get("outcome")),
            params.get("keyword").map(String::as_str),
        )
        .map_err(|e| {
            error_response(
                &format!("读取签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    let kind = params
        .get("kind")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let start = parse_ms(params.get("start"));
    let end = parse_ms(params.get("end"));
    Ok(records
        .into_iter()
        .filter(|r| kind.is_none_or(|k| kind_matches(r, k)))
        .filter(|r| within_range(r, start, end))
        .collect())
}

/// GET /api/checkin-records —— 一页明细 + 同条件总数 + 统计。
fn query_records(
    state: &AppState,
    params: &Params,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let limit = parse_usize(params.get("limit"))
        .unwrap_or(200)
        .clamp(1, 1000);
    let offset = parse_usize(params.get("offset")).unwrap_or(0);
    let records = filtered(state, params)?;
    let total = records.len();
    // 统计卡：按 outcome 计数（同条件）
    let mut stats = serde_json::Map::new();
    for outcome in [
        CheckinOutcome::Success,
        CheckinOutcome::Already,
        CheckinOutcome::Pending,
        CheckinOutcome::Captcha,
        CheckinOutcome::Failed,
    ] {
        let count = records.iter().filter(|r| r.outcome == outcome).count();
        stats.insert(outcome.as_str().to_string(), json!(count));
    }
    stats.insert(
        "keptActive".to_string(),
        json!(records.iter().filter(|r| r.kept_active).count()),
    );
    let page: Vec<Value> = records
        .iter()
        .skip(offset)
        .take(limit)
        .enumerate()
        .map(|(index, record)| record_json(record, offset + index))
        .collect();
    Ok(Json(json!({
        "records": page,
        "total": total,
        "stats": Value::Object(stats),
    })))
}

/// DELETE /api/checkin-records —— 按筛选删除 / 显式全清（护栏见模块头）。
fn clear_records(
    state: &AppState,
    params: &Params,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if !has_filters(params) {
        let explicit = params
            .get("all")
            .map(|v| v.trim() == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !explicit {
            return Err(error_response(
                "未指定筛选条件：要清空全部签到记录请显式带 all=1",
                StatusCode::BAD_REQUEST,
            ));
        }
        let removed = state.upstream_checkin.clear_all().map_err(|e| {
            error_response(
                &format!("清空签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        return Ok(Json(json!({ "success": true, "removed": removed })));
    }
    // 筛选删除：先按同一份过滤链算出命中集合，再按 `(account_id, provider, ts)`
    // 精确定位逐条删除（避免误删同账号同 provider 的其他记录）。
    let matched = filtered(state, params)?;
    let removed = remove_matched(state, &matched)?;
    Ok(Json(json!({ "success": true, "removed": removed })))
}

fn remove_matched(
    state: &AppState,
    matched: &[CheckinRecord],
) -> Result<usize, (StatusCode, Json<Value>)> {
    use std::collections::HashSet;
    let targets: HashSet<(String, String, i64)> = matched
        .iter()
        .map(|r| (r.account_id.clone(), r.provider.clone(), r.ts))
        .collect();
    if targets.is_empty() {
        return Ok(0);
    }
    let store = &state.upstream_checkin;
    let mut removed = 0usize;
    let dates: HashSet<String> = matched.iter().map(|r| r.date.clone()).collect();
    for date in dates {
        let bucket = store.records_of_date(&date).map_err(|e| {
            error_response(
                &format!("读取签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        let kept: Vec<CheckinRecord> = bucket
            .into_iter()
            .filter(|r| {
                let hit = targets.contains(&(r.account_id.clone(), r.provider.clone(), r.ts));
                if hit {
                    removed += 1;
                }
                !hit
            })
            .collect();
        store.replace_records_of_date(&date, kept).map_err(|e| {
            error_response(
                &format!("删除签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: i64, source: &str, reason: &str) -> CheckinRecord {
        CheckinRecord {
            ts,
            date: "2026-01-01".into(),
            account_id: "a".into(),
            account_name: "n".into(),
            provider: "loomy".into(),
            source: source.into(),
            kind: source.into(),
            reason: reason.into(),
            outcome: CheckinOutcome::Success,
            ok: true,
            kept_active: false,
            streak_days: 0,
            message: String::new(),
            error: String::new(),
        }
    }

    #[test]
    fn kind_three_tier_classification() {
        assert!(kind_matches(&rec(1, "manual", ""), "manual"));
        assert!(kind_matches(&rec(1, "single", ""), "manual"));
        assert!(kind_matches(&rec(1, "activity", "手动保活"), "manual"));
        assert!(kind_matches(&rec(1, "schedule", "定时签到"), "schedule"));
        // 旧口径：source=schedule 但 reason 以「补签」开头 → 归 makeup
        assert!(!kind_matches(
            &rec(1, "schedule", "补签（条目）"),
            "schedule"
        ));
        assert!(kind_matches(&rec(1, "schedule", "补签（条目）"), "makeup"));
        assert!(kind_matches(&rec(1, "makeup", "补签"), "makeup"));
        assert!(kind_matches(&rec(1, "startup", "启动补签"), "makeup"));
        // 窗口重试轮单列（AIGX 扩展档）
        assert!(kind_matches(&rec(1, "retry", "重试"), "retry"));
        assert!(!kind_matches(&rec(1, "manual", ""), "retry"));
    }

    #[test]
    fn time_range_is_inclusive_and_ms_aware() {
        let r = rec(1000, "manual", "");
        // ts=1000 秒 → 1_000_000 毫秒
        assert!(within_range(&r, Some(1_000_000), Some(1_000_000)));
        assert!(within_range(&r, Some(999_999), None));
        assert!(!within_range(&r, Some(1_000_001), None));
        assert!(within_range(&r, None, Some(1_000_001)));
        assert!(!within_range(&r, None, Some(999_999)));
        assert!(within_range(&r, None, None));
    }
}
