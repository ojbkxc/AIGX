//! 签到中心 API（仅管理员）。
//!
//! - `GET  /api/checkin-center`          聚合快照（A2A 形状：daily / extras /
//!   auto / keepalive / travel / history / adminPanels / checkinFeatures）
//! - `GET  /api/checkin-center?view=summary` 轻量汇总（只给 todayDone /
//!   todayEligible，顶栏徽标轮询用）
//! - `POST /api/checkin-center/run`      立即签到（全部 / 指定账号 / 指定 provider）
//! - `GET  /api/checkin-center/records`  台账分页（旧路径；新契约见
//!   `api::admin::checkin_records` 的 `/api/checkin-records`）
//! - `DELETE /api/checkin-center/records` 清空台账
//! - `GET/PUT /api/checkin-center/settings` 总开关 + 间隔
//! - `GET/POST /api/checkin-center/schedules` 计划列表 / 新增更新（旧路径；
//!   新契约见 `api::admin::checkin_schedules` 的 `/api/checkin-schedules`）
//! - `POST /api/checkin-center/schedules/remove` 删除计划
//! - `POST /api/checkin-center/schedules/run` 立即执行某计划
//! - `GET/POST /api/travel-schedule`      猫猫旅行巡检状态 / 改配置
//! - `POST /api/travel-schedule/run`      立即巡检一轮（真打上游）
//! - `PATCH /api/checkin-center/travel`   旧路径：猫猫旅行巡检配置
//! - `POST /api/checkin-center/travel/run` 旧路径：立即巡检一轮
//!
//! ## 与 `/api/checkin` 的区别
//!
//! `/api/checkin` 是**用户**签到（钱包配额奖励，`verify_user`）。
//! 这里的 `/api/checkin-center` 是**管理员**替上游账号签到，两者不要混淆。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::checkin::center;
use crate::checkin::claim::{self, supports_checkin};
use crate::checkin::history;
use crate::checkin::scheduler;
use crate::checkin::store::{CheckinOutcome, CheckinRecord, CheckinSchedule, CheckinSettings};
use crate::checkin::travel::{self, RunOutcome, TravelActionError, TravelExecutor};

/// GET /api/checkin-center —— 聚合快照（零上游请求）。
///
/// 形状对齐 Agent2API `api/checkin_center::get_center`：
///
/// ```json
/// {
///   "daily": { "providers": [...], "outOfScope": [...],
///              "todayDone": 6, "todayEligible": 9 },
///   "extras": { "onboarding": [...], "plans": [...], "campaigns": [...], "growth": [...] },
///   "auto": { ...AutoState... },
///   "keepalive": { "models": [...], "defaultModels": [...] },
///   "travel": { ...TravelState... },
///   "history": [ ...时间线... ],
///   "adminPanels": ["daily_keepalive", "growth_tasks"],
///   "checkinFeatures": { "loomy": {...}, ... }
/// }
/// ```
///
/// `?view=summary` 只回 `{daily:{todayDone,todayEligible}}` —— 顶栏徽标轮询
/// 不必拉全量快照。
pub async fn handle_checkin_center(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let store = &state.upstream_checkin;
    let today = scheduler::today();
    let sessions = state.account_sessions.all();
    // 可用清单：插件/实现接管 + 库里已有账号的家（顺序 = 白名单注册顺序）。
    // 范围外账号统一进 `outOfScope`（按 provider + 原因聚合）。
    let available = claim::available_checkin_providers(&state.account_sessions);
    let summary_only = params
        .get("view")
        .is_some_and(|view| view.trim() == "summary");
    if summary_only {
        let eligible: Vec<_> = sessions
            .iter()
            .filter(|session| {
                available.iter().any(|id| id == &session.provider)
                    && center::out_of_scope_reason(session).is_none()
            })
            .collect();
        let done = eligible
            .iter()
            .filter(|session| {
                store
                    .last_today(&session.id, &session.provider)
                    .ok()
                    .flatten()
                    .map(|record| record.ok)
                    .unwrap_or(false)
            })
            .count();
        return Ok(Json(json!({
            "daily": { "todayDone": done, "todayEligible": eligible.len() }
        })));
    }

    // 按可用清单分桶（界面行序稳定）；范围外账号按「provider + 原因」聚合
    let mut buckets: Vec<Vec<Value>> = vec![Vec::new(); available.len()];
    let mut out_of_scope: BTreeMap<String, (String, String, i64)> = BTreeMap::new();
    for session in &sessions {
        let slot = available
            .iter()
            .position(|id| id == &session.provider)
            .filter(|_| center::out_of_scope_reason(session).is_none());
        if let Some(index) = slot {
            buckets[index].push(center::account_row(store, session, &today));
        } else {
            let label = crate::model::manage::provider_label(&session.provider);
            let reason = center::out_of_scope_reason(session).unwrap_or("没有签到链路");
            let entry = out_of_scope
                .entry(format!("{}|{reason}", session.provider))
                .or_insert_with(|| (label, reason.to_string(), 0));
            entry.2 += 1;
        }
    }

    let features = center::checkin_features();
    let mut provider_rows: Vec<Value> = available
        .iter()
        .zip(buckets)
        .map(|(id, rows)| {
            let done = rows
                .iter()
                .filter(|row| row.get("checkedInToday").and_then(Value::as_bool) == Some(true))
                .count();
            let declared = features.get(id.as_str()).cloned().unwrap_or(Value::Null);
            let manual_plans = declared
                .get("manualPlans")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            json!({
                "id": id,
                "label": crate::model::manage::provider_label(id),
                "accounts": rows,
                "doneCount": done,
                "totalCount": rows.len(),
                "manualPlans": manual_plans,
                // 插件/能力申报的链路信息（A2A 同名字段；无申报的家给 null）
                "description": declared.get("description").cloned().unwrap_or(Value::Null),
                "modes": declared.get("modes").cloned().unwrap_or(Value::Null),
                "uiHooks": declared.get("uiHooks").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    // 无账号的家排在最后（与 A2A 同一取舍：空行不占首屏）
    provider_rows
        .sort_by_key(|row| row.get("totalCount").and_then(Value::as_u64).unwrap_or(0) == 0);
    // 库里一家账号都没有时，可用清单回落到「全部有实现的家」给编辑器用；
    // 但签到中心的**分组行**只列真有账号的家 —— 否则首次使用会看到十几行
    // 点了必然空转的空行（A2A 2026-10 用户反馈同一结论）。范围外账号所在的
    // 家仍然出现（行上 0 个可签账号 + outOfScope 原因）。
    let present: std::collections::HashSet<&str> = sessions
        .iter()
        .map(|session| session.provider.as_str())
        .collect();
    provider_rows.retain(|row| {
        row.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| present.contains(id))
    });
    let today_done: u64 = provider_rows
        .iter()
        .map(|row| row.get("doneCount").and_then(Value::as_u64).unwrap_or(0))
        .sum();
    let today_eligible: u64 = provider_rows
        .iter()
        .map(|row| row.get("totalCount").and_then(Value::as_u64).unwrap_or(0))
        .sum();
    let out_rows: Vec<Value> = out_of_scope
        .into_values()
        .map(|(label, reason, count)| json!({ "label": label, "reason": reason, "count": count }))
        .collect();

    Ok(Json(json!({
        "daily": {
            "providers": provider_rows,
            "outOfScope": out_rows,
            "todayDone": today_done,
            "todayEligible": today_eligible,
        },
        "extras": {
            "onboarding": center::onboarding_rows(store, &state.account_sessions),
            "plans": center::plan_rows(store, &state.account_sessions),
            "campaigns": center::campaign_rows(),
            "growth": center::growth_rows(&state.account_sessions),
        },
        // 自动签到（多条目状态机；与 /api/checkin-schedules 的 GET 同形）
        "auto": center::auto_state(store, &state.account_sessions),
        // WorkBuddy 国际版日活保活的模型链（签到中心可编辑）
        "keepalive": center::keepalive_view(store),
        // 猫猫旅行定时排程（国内版）：开关 / 时刻 / 今日已跑时刻 / 上一轮结果
        "travel": travel_state(&state),
        // 插件申报的签到窗口任务（AIGX 由本仓调度接管，形状占位空数组）
        "pluginTasks": [],
        // 轮次时间线（新条目在前；只记批量轮次）
        "history": history::list(store),
        // 面板可见性（对齐 A2A adminPanels）：没装/没账号时不显示面板
        "adminPanels": center::admin_panels(&state.account_sessions),
        // 各家签到链路信息（说明 / 档位 / UI 钩子），按 provider id 索引
        "checkinFeatures": features,
        // ── AIGX 扩展（A2A 快照没有这些键；前端设置卡与统计卡在用）──
        "settings": state.upstream_checkin.settings(),
        "recordsTotal": state.upstream_checkin.total_records().unwrap_or(0),
        "supportedProviders": claim::supported_providers(),
        "unsupportedProviders": claim::UNSUPPORTED_PROVIDERS,
        // 活动预览的空形状（A2A `CampaignPreview`）：AIGX 暂无 provider 活动
        // 下发链路，返回空集但形状不变（前端按类型解析不会拿到 undefined）
        "campaignPreview": center::campaign_preview_shape(),
    })))
}

#[derive(Debug, Deserialize, Default)]
pub struct RunRequest {
    /// 指定账号 id；空 = 全部
    #[serde(default)]
    pub account_ids: Vec<String>,
    /// 指定 provider；空 = 全部支持的
    #[serde(default)]
    pub providers: Vec<String>,
}

/// POST /api/checkin-center/run —— 立即签到。
pub async fn handle_checkin_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<RunRequest>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let request = body.map(|Json(b)| b).unwrap_or_default();
    let settings = state.upstream_checkin.settings();

    let mut targets = scheduler::collect_targets(&state.account_sessions, &request.providers);
    if !request.account_ids.is_empty() {
        targets.retain(|s| request.account_ids.iter().any(|id| id == &s.id));
    }
    if targets.is_empty() {
        return Ok(Json(json!({
            "success": true,
            "data": { "results": [], "message": "没有匹配的账号" }
        })));
    }

    let mut results: Vec<Value> = Vec::with_capacity(targets.len());
    let mut ok = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for session in &targets {
        // 出口统一走 `client_for`（解析 pool:/rotate 编码），与数据面同链路
        let client = match state.account_sessions.client_for(session) {
            Ok(client) => client,
            Err(e) => {
                let message = format!("出口配置错误：{e}");
                scheduler::record(
                    &state.upstream_checkin,
                    session,
                    "manual",
                    CheckinOutcome::Failed,
                    &message,
                    false,
                );
                failures.push(format!("{}（{message}）", session.name));
                results.push(json!({
                    "account_id": session.id,
                    "account_name": session.name,
                    "provider": session.provider,
                    "success": false,
                    "outcome": "failed",
                    "message": message,
                }));
                continue;
            }
        };
        let result = claim::claim_for(&client, session).await;
        if result.outcome.is_ok() {
            ok += 1;
        }
        if matches!(
            result.outcome,
            CheckinOutcome::Failed | CheckinOutcome::Captcha
        ) {
            failures.push(format!("{}（{}）", session.name, result.msg));
        }
        scheduler::record(
            &state.upstream_checkin,
            session,
            "manual",
            result.outcome,
            &result.msg,
            false,
        );
        results.push(json!({
            "account_id": session.id,
            "account_name": session.name,
            "provider": session.provider,
            "success": result.success,
            "outcome": result.outcome.as_str(),
            "message": result.msg,
        }));
        if settings.gap_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(settings.gap_ms)).await;
        }
    }
    // 批量手动轮次也进签到中心时间线（单账号点签不进 —— 见 history 模块头）。
    // A2A 的 `run_now` 走 `run_checkin`，两种批量手动同样记一条。
    if request.account_ids.is_empty() {
        history::record(
            &state.upstream_checkin,
            "手动触发",
            ok,
            0,
            targets.len(),
            0,
            &failures,
        );
    }

    Ok(Json(json!({
        "success": true,
        "data": {
            "results": results,
            "done": ok,
            "total": targets.len(),
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct RecordsQuery {
    #[serde(default)]
    pub days: Option<u32>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    /// 记录类型筛选：manual | schedule | makeup | retry（空/缺省 = 不过滤）。
    /// 存量记录 `kind` 为空 —— 回退按 `source` 匹配，保证旧台账也能被筛到。
    #[serde(default)]
    pub kind: Option<String>,
    /// 起始 unix 秒（闭区间；缺省 = 不限）
    #[serde(default)]
    pub start: Option<i64>,
    /// 结束 unix 秒（闭区间；缺省 = 不限）
    #[serde(default)]
    pub end: Option<i64>,
    /// 关键词（账号名 / 账号 id / 上游 / 说明，大小写不敏感包含）
    #[serde(default)]
    pub keyword: Option<String>,
    /// 分页偏移（从 0 起）
    #[serde(default)]
    pub offset: usize,
    /// 分页大小（≤0 = 不分页，返回全部）
    #[serde(default = "default_page_size")]
    pub limit: usize,
    /// 清空全部记录的显式护栏（与基线 `all=1` 同义）。
    /// 不传此参数且未指定任何筛选条件时返回 400，避免误触全清。
    #[serde(default)]
    pub all: Option<u32>,
}

fn default_page_size() -> usize {
    50
}

/// 台账在内存里的二次筛选：`kind`（空 = 不过滤，存量记录 `kind` 为空时回退按
/// `source` 匹配）与 `[start, end]` 闭区间时间过滤。抽成纯函数便于单测边界。
pub(crate) fn filter_records(
    records: Vec<CheckinRecord>,
    kind: Option<&str>,
    start: Option<i64>,
    end: Option<i64>,
) -> Vec<CheckinRecord> {
    let kind = kind.filter(|s| !s.is_empty());
    records
        .into_iter()
        .filter(|r| {
            kind.is_none_or(|k| r.kind == k || r.source == k)
                && start.is_none_or(|s| r.ts >= s)
                && end.is_none_or(|e| r.ts <= e)
        })
        .collect()
}

/// GET /api/checkin-center/records —— 台账（分页 + 关键词）。
pub async fn handle_checkin_records(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let outcome = match q.outcome.as_deref() {
        Some("success") => Some(CheckinOutcome::Success),
        Some("already") => Some(CheckinOutcome::Already),
        Some("pending") => Some(CheckinOutcome::Pending),
        Some("failed") => Some(CheckinOutcome::Failed),
        Some("captcha") => Some(CheckinOutcome::Captcha),
        _ => None,
    };
    let records = state
        .upstream_checkin
        .recent_records(
            q.days.unwrap_or(7),
            q.provider.as_deref().filter(|s| !s.is_empty()),
            q.account_id.as_deref().filter(|s| !s.is_empty()),
            outcome,
            q.keyword.as_deref(),
        )
        .map_err(|e| {
            error_response(
                &format!("读取签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    // kind / 时间区间（闭区间）在内存里再筛一遍：`recent_records` 的签名不动，
    // 分页与 `total` 语义保持不变（total = 筛选后的总数）。
    let records = filter_records(records, q.kind.as_deref(), q.start, q.end);
    // 过滤后总数（供分页控件），再截取当前页
    let total = records.len();
    let page = if q.limit > 0 {
        records.into_iter().skip(q.offset).take(q.limit).collect()
    } else {
        records
    };
    Ok(Json(json!({
        "success": true,
        "data": { "records": page, "total": total }
    })))
}

/// DELETE /api/checkin-center/records —— 删除台账记录。
///
/// 三种模式（优先级从高到低）：
/// 1. `?all=1` — 清空全部记录（显式护栏，不带筛选时必须指定）
/// 2. 带筛选条件（provider/account_id/outcome/keyword）— 逐条删除匹配记录
/// 3. `?days=N` — 清除最近 N 天的记录（按日桶删除）
///
/// 三者都不指定时返回 400，避免误触全清。
pub async fn handle_clear_checkin_records(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    // 1. all=1 显式全清
    if q.all.is_some() {
        let removed = state.upstream_checkin.clear_all().map_err(|e| {
            error_response(
                &format!("清空全部签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        return Ok(Json(json!({
            "success": true,
            "data": { "removed": removed, "mode": "all" }
        })));
    }

    // 2. 筛选删除（任一筛选条件存在即走此路径）
    let has_filters = q.provider.is_some()
        || q.account_id.is_some()
        || q.outcome.is_some()
        || q.keyword.as_ref().is_some_and(|k| !k.trim().is_empty());
    if has_filters {
        let outcome = match q.outcome.as_deref() {
            Some("success") => Some(CheckinOutcome::Success),
            Some("already") => Some(CheckinOutcome::Already),
            Some("pending") => Some(CheckinOutcome::Pending),
            Some("failed") => Some(CheckinOutcome::Failed),
            Some("captcha") => Some(CheckinOutcome::Captcha),
            _ => None,
        };
        let removed = state
            .upstream_checkin
            .remove_matching(
                q.provider.as_deref().filter(|s| !s.is_empty()),
                q.account_id.as_deref().filter(|s| !s.is_empty()),
                outcome,
                q.keyword.as_deref(),
            )
            .map_err(|e| {
                error_response(
                    &format!("按筛选删除签到记录失败：{e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
        return Ok(Json(json!({
            "success": true,
            "data": { "removed": removed, "mode": "filter" }
        })));
    }

    // 3. days=N 按日桶清除
    if let Some(days) = q.days {
        let removed = state.upstream_checkin.clear_records(days).map_err(|e| {
            error_response(
                &format!("清除签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        return Ok(Json(json!({
            "success": true,
            "data": { "removed": removed, "mode": "days" }
        })));
    }

    // 三者都不指定 → 400
    Err(error_response(
        "未指定删除范围：清空全部请带 all=1，按条件删除请带筛选参数，按天数清除请带 days=N",
        StatusCode::BAD_REQUEST,
    ))
}

/// GET /api/checkin-center/settings
pub async fn handle_get_checkin_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": state.upstream_checkin.settings()
    })))
}

/// PUT /api/checkin-center/settings
pub async fn handle_update_checkin_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CheckinSettings>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let settings = state.upstream_checkin.set_settings(body).map_err(|e| {
        error_response(
            &format!("保存签到设置失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({ "success": true, "data": settings })))
}

/// GET /api/checkin-center/schedules
pub async fn handle_list_checkin_schedules(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": state.upstream_checkin.schedules()
    })))
}

#[derive(Debug, Deserialize)]
pub struct ScheduleBody {
    #[serde(default)]
    pub id: String,
    pub label: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub time: String,
    #[serde(default)]
    pub providers: Vec<String>,
    /// 逐家时刻覆盖（`{provider: "HH:MM"}`；空串 = 撤销覆盖）。前端未传时
    /// 保持存量值（编辑别家字段不该把覆盖清掉）。
    #[serde(default)]
    pub provider_times: Option<HashMap<String, String>>,
    /// 窗口结束时刻（HH:MM）；空 = 无窗口。未传时继承存量。
    #[serde(default)]
    pub time_end: Option<String>,
    /// true = 每个账号在 `[time, time_end]` 内取确定性随机时刻。未传时继承存量。
    #[serde(default)]
    pub randomize: Option<bool>,
    /// 每账号每天重试次数上限（0 = 关闭重试）。未传时继承存量（缺省 3）。
    #[serde(default)]
    pub retry_limit: Option<u32>,
    /// 重试间隔（分钟，<=0 回落全局）。未传时继承存量（缺省 5）。
    #[serde(default)]
    pub retry_gap_minutes: Option<u32>,
    /// 补签轮时刻（HH:MM）；空 = 不跑补签轮。未传时继承存量。
    #[serde(default)]
    pub makeup_time: Option<String>,
}

/// 条目缺省重试上限 / 间隔（与 `store` 的 `default_retry_limit/_minutes` 一致）。
const DEFAULT_RETRY_LIMIT: u32 = 3;
const DEFAULT_RETRY_GAP_MINUTES: u32 = 5;

fn default_true() -> bool {
    true
}

/// 归一一个「HH:MM」输入：空串 → 空；合法 → 原值；非法 → Err（调用方转 400）。
fn normalize_hhmm(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() || scheduler::is_hhmm(&trimmed) {
        Ok(trimmed)
    } else {
        Err(format!("{field} 格式必须是 HH:MM（如 11:00）"))
    }
}

/// POST /api/checkin-center/schedules —— 新增或更新。
pub async fn handle_upsert_checkin_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScheduleBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if !scheduler::is_hhmm(&body.time) {
        return Err(error_response(
            "时刻格式必须是 HH:MM（如 09:00）",
            StatusCode::BAD_REQUEST,
        ));
    }
    for provider in &body.providers {
        if !supports_checkin(provider) {
            return Err(error_response(
                &format!("{provider} 暂不支持自动签到"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    let label = if body.label.trim().is_empty() {
        "每日签到".to_string()
    } else {
        body.label.trim().to_string()
    };
    // 现状计划（按 id 匹配）：运行状态与「未传字段」都从它继承 —— 编辑别家
    // 字段不该把上次执行结果/覆盖/窗口设置清掉。
    let existing = state
        .upstream_checkin
        .schedules()
        .into_iter()
        .find(|s| s.id == body.id);
    // 区间窗两个时刻：未传 → 继承存量；传了 → 归一（空 = 清空，非法 → 400）
    let time_end = match body.time_end.as_deref() {
        None => existing
            .as_ref()
            .map(|s| s.time_end.clone())
            .unwrap_or_default(),
        Some(raw) => normalize_hhmm(raw, "窗口结束时刻")
            .map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?,
    };
    let makeup_time = match body.makeup_time.as_deref() {
        None => existing
            .as_ref()
            .map(|s| s.makeup_time.clone())
            .unwrap_or_default(),
        Some(raw) => normalize_hhmm(raw, "补签时刻")
            .map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?,
    };
    let randomize = body
        .randomize
        .unwrap_or_else(|| existing.as_ref().map(|s| s.randomize).unwrap_or(false));
    if randomize && !time_end.is_empty() && time_end.as_str() <= body.time.as_str() {
        return Err(error_response(
            "开启区间随机时，窗口结束时刻必须晚于开始时刻",
            StatusCode::BAD_REQUEST,
        ));
    }
    let retry_limit = body.retry_limit.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|s| s.retry_limit)
            .unwrap_or(DEFAULT_RETRY_LIMIT)
    });
    let retry_gap_minutes = body.retry_gap_minutes.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|s| s.retry_gap_minutes)
            .unwrap_or(DEFAULT_RETRY_GAP_MINUTES)
    });
    // 保存计划只改配置：运行状态（上次触发/结果/成功·保活·失败计数）必须从现有
    // 计划继承，否则编辑一次就把界面「上次执行」清空，且空 last_fired_date 会让
    // 调度器当天重复触发签到（浪费上游日配额）。
    let (
        last_fired_date,
        last_result,
        last_summary,
        last_ok,
        last_keepalive,
        last_failed,
        last_fired_providers,
        last_retried_providers,
        account_plan,
    ) = existing
        .as_ref()
        .map(|s| {
            (
                s.last_fired_date.clone(),
                s.last_result.clone(),
                s.last_summary.clone(),
                s.last_ok,
                s.last_keepalive,
                s.last_failed,
                s.last_fired_providers.clone(),
                s.last_retried_providers.clone(),
                s.account_plan.clone(),
            )
        })
        .unwrap_or_default();
    // 逐家时刻覆盖：前端未传时沿用存量；传了则按三档语义归一（只认签到白名单
    // 里的家 + HH:MM 格式，非法值丢弃 —— 宁可回落条目时刻也不带脏数据进调度）。
    let provider_times = match body.provider_times {
        Some(overrides) => {
            let mut cleaned: HashMap<String, String> = HashMap::new();
            for (provider, time) in overrides {
                if !supports_checkin(&provider) {
                    continue;
                }
                let time = time.trim().to_string();
                if time.is_empty() {
                    // 空串 = 撤销覆盖，保留为空串（让「撤掉覆盖」生效）
                    cleaned.insert(provider, String::new());
                } else if scheduler::is_hhmm(&time) {
                    cleaned.insert(provider, time);
                }
            }
            cleaned
        }
        None => existing
            .as_ref()
            .map(|s| s.provider_times.clone())
            .unwrap_or_default(),
    };
    let list = state
        .upstream_checkin
        .upsert_schedule(CheckinSchedule {
            id: body.id,
            label,
            enabled: body.enabled,
            time: body.time,
            time_end,
            randomize,
            retry_limit,
            retry_gap_minutes,
            makeup_time,
            providers: body.providers,
            provider_times,
            last_fired_date,
            last_fired_providers,
            last_retried_providers,
            account_plan,
            last_result,
            last_summary,
            last_ok,
            last_keepalive,
            last_failed,
        })
        .map_err(|e| {
            error_response(
                &format!("保存签到计划失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(json!({ "success": true, "data": list })))
}

#[derive(Debug, Deserialize)]
pub struct ScheduleIdBody {
    pub id: String,
}

/// POST /api/checkin-center/schedules/remove
pub async fn handle_remove_checkin_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScheduleIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let list = state
        .upstream_checkin
        .remove_schedule(&body.id)
        .map_err(|e| {
            error_response(
                &format!("删除签到计划失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(json!({ "success": true, "data": list })))
}

/// POST /api/checkin-center/schedules/run —— 立即执行某计划的范围。
pub async fn handle_run_checkin_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ScheduleIdBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let schedule = state
        .upstream_checkin
        .schedules()
        .into_iter()
        .find(|s| s.id == body.id)
        .ok_or_else(|| error_response("签到计划不存在", StatusCode::NOT_FOUND))?;
    let settings = state.upstream_checkin.settings();
    let outcome = scheduler::run_schedule(
        &state.upstream_checkin,
        &state.account_sessions,
        state.account_sessions.egress(),
        &schedule.providers,
        settings.gap_ms,
    )
    .await;
    let counts = crate::checkin::store::ScheduleCounts {
        ok: outcome.ok,
        keepalive: outcome.keepalive,
        failed: outcome.failed,
    };
    let _ = state.upstream_checkin.mark_fired(
        &schedule.id,
        &scheduler::today(),
        &outcome.summary,
        &counts,
    );
    let _ = state
        .upstream_checkin
        .mark_last_summary(&schedule.id, &outcome.last_summary);
    Ok(Json(json!({
        "success": true,
        "data": {
            "summary": outcome.summary,
            "ok": outcome.ok,
            "keepalive": outcome.keepalive,
            "failed": outcome.failed,
        }
    })))
}

// ── 猫猫旅行巡检（对照上游 core/growth_scheduler + api/growth_schedule）──
//
// 与每日签到分开：旅行是独立排程，配置落 `checkin:travel`（见
// `checkin::travel` 模块头）。执行体在提供商插件里，网关只转发
// `admin.action {action: "growth.travel"}`——与上游 `api/growth.rs` 的
// `call_action("growth.travel")` 同名同形（`account` 空串 = 该家全部账号）。

/// 插件调用超时：这些动作要真打上游（任务树查询 / 领奖），用网络面档位
/// （对齐上游 `network` 档，不用控制面 10s）。
const TRAVEL_ACTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// `AppState` 上的旅行执行器：原生包进程 → 脚本宿主 → Wasm 沙箱（与数据面
/// `resolve.rs` 的三级查找同顺序、同判据）。
pub struct StateTravelExecutor {
    state: AppState,
}

impl StateTravelExecutor {
    pub fn new(state: &AppState) -> Self {
        Self {
            state: state.clone(),
        }
    }

    /// 当前该走哪个宿主（None = 插件未装/未启用/崩溃退避中）。
    fn route(&self) -> Option<TravelRoute> {
        let provider = travel::TRAVEL_PROVIDER;
        if let Some(process) = self.state.plugin_host.process_for_provider(provider) {
            return Some(TravelRoute::Native(process));
        }
        if let Some(route) = self.state.script_host.route_for_provider(provider) {
            return Some(TravelRoute::Script(route));
        }
        crate::plugin_host::wasm::host_for_provider(provider).map(TravelRoute::Wasm)
    }
}

/// 执行链路的三种后端（句柄快照）。
enum TravelRoute {
    Native(std::sync::Arc<crate::plugin_host::process::PluginProcess>),
    Script(crate::plugin_host::script::runtime::ScriptRoute),
    Wasm(std::sync::Arc<crate::plugin_host::wasm::WasmHost>),
}

/// 插件 `admin.action` 的参数形状（与脚本宿主 `provider.mjs` 的
/// `{provider, account, action, payload}`、wasm 的 `params.provider`
/// 校验逐字对齐）。
fn travel_action_params() -> Value {
    json!({
        "provider": travel::TRAVEL_PROVIDER,
        "account": "",
        "action": travel::ACTION,
        "payload": Value::Null,
    })
}

#[async_trait::async_trait]
impl TravelExecutor for StateTravelExecutor {
    fn pause_reason(&self) -> Option<String> {
        if self.route().is_some() {
            return None;
        }
        Some(format!(
            "{} 提供商插件未安装或未启用（旅行巡检需要插件 admin.action 支持）",
            travel::TRAVEL_PROVIDER
        ))
    }

    async fn run(&self) -> Result<Value, TravelActionError> {
        let Some(route) = self.route() else {
            return Err(TravelActionError {
                message: self.pause_reason().unwrap_or_default(),
                code: Some("configuration".to_string()),
            });
        };
        let params = travel_action_params();
        match route {
            TravelRoute::Native(process) => {
                match tokio::time::timeout(
                    TRAVEL_ACTION_TIMEOUT,
                    process.rpc.call("admin.action", params),
                )
                .await
                {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(TravelActionError {
                        message: error.message,
                        code: error.code,
                    }),
                    Err(_) => Err(TravelActionError {
                        message: "旅行巡检超时（插件 120 秒未返回）".to_string(),
                        code: Some("timeout".to_string()),
                    }),
                }
            }
            TravelRoute::Script(route) => {
                // 直连通道（不经 `call_provider`）：后者只保留 message，
                // 丢弃 `unsupported` 等 code —— handler 靠它映射 501 口径。
                match tokio::time::timeout(
                    TRAVEL_ACTION_TIMEOUT,
                    route.channel.call("admin.action", params),
                )
                .await
                {
                    Ok(Ok(value)) => Ok(value),
                    Ok(Err(error)) => Err(TravelActionError {
                        message: error.message,
                        code: error.code,
                    }),
                    Err(_) => Err(TravelActionError {
                        message: "旅行巡检超时（脚本插件 120 秒未返回）".to_string(),
                        code: Some("timeout".to_string()),
                    }),
                }
            }
            TravelRoute::Wasm(host) => {
                match host
                    .call("admin.action", params, TRAVEL_ACTION_TIMEOUT)
                    .await
                {
                    Ok(value) => Ok(value),
                    Err(error) => Err(TravelActionError {
                        message: error.message,
                        code: error.code,
                    }),
                }
            }
        }
    }
}

/// 调度器的执行器句柄（main.rs 接线：`checkin::travel::spawn(store, executor)`）。
pub fn travel_executor(state: &AppState) -> std::sync::Arc<dyn TravelExecutor> {
    std::sync::Arc::new(StateTravelExecutor::new(state))
}

/// `data.travel` 视图（前端 `CheckinTravelState` 字段逐字对齐；零上游请求）。
fn travel_state(state: &AppState) -> Value {
    let executor = StateTravelExecutor::new(state);
    travel::state(&state.upstream_checkin, &executor)
}

// ── 猫猫旅行定时排程（A2A 契约：GET/POST /api/travel-schedule +
//    POST /api/travel-schedule/run；旧 PATCH 路径保留在下面）──────────────

/// GET /api/travel-schedule —— 开关、时刻数组、今日已执行时刻、上一轮结果。
pub async fn handle_get_travel_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(travel_state(&state)))
}

/// POST /api/travel-schedule —— 改配置 `{enabled?, times?}`（只改传进来的字段）。
///
/// 校验口径对齐 A2A `growth_scheduler::configure`：空对象 / 非法时刻 /
/// 空时刻表 / 超过上限 → 400 管理信封（`{success:false,error}`）。
pub async fn handle_configure_travel_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let config = travel::patch(&state.upstream_checkin, &body)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Travel] 排程已{}，时刻 {:?}",
        if config.enabled { "开启" } else { "关闭" },
        config.times
    );
    Ok(Json(travel_state(&state)))
}

/// POST /api/travel-schedule/run —— 立即巡检一轮（真打上游）。
///
/// 响应对齐 A2A `api::growth_schedule::run_now`：
/// - 成功 → 200 `{outcome:"completed", summary, schedule}`
/// - 能力不可用（插件未装/未启用）→ 200 `{outcome:"skipped", message, schedule}`
/// - 已有一轮在跑 → 409 `{success:false,outcome:"busy",error,schedule}`
/// - 上游失败 → `{success:false,outcome:"failed",error,code,schedule}`
pub async fn handle_run_travel_schedule(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let executor = StateTravelExecutor::new(&state);
    match travel::run_now(&state.upstream_checkin, &executor).await {
        RunOutcome::Completed {
            summary,
            ok,
            failed,
        } => Ok(Json(json!({
            "outcome": "completed",
            "summary": { "message": summary, "ok": ok, "failed": failed },
            "schedule": travel_state(&state),
        }))),
        RunOutcome::Skipped(message) => Ok(Json(json!({
            "outcome": "skipped",
            "message": message,
            "summary": Value::Null,
            "schedule": travel_state(&state),
        }))),
        RunOutcome::Busy => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "outcome": "busy",
                "error": "旅行巡检正在执行中，请稍候",
                "schedule": travel_state(&state),
            })),
        )),
        RunOutcome::Failed(error) => {
            let status = match error.code.as_deref() {
                Some("unsupported") => StatusCode::NOT_IMPLEMENTED,
                Some("configuration") => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_GATEWAY,
            };
            Err((
                status,
                Json(json!({
                    "success": false,
                    "outcome": "failed",
                    "error": error.message,
                    "code": error.code,
                    "schedule": travel_state(&state),
                })),
            ))
        }
    }
}

/// PATCH /api/checkin-center/travel —— 部分更新 `{enabled?, times?}`（旧路径）。
///
/// 校验口径见 [`travel::patch`]：非法 HH:MM / 空时刻表 / 超过上限 → 400，
/// 不落半截配置。响应给刷新后的完整状态（前端就地重绘）。
pub async fn handle_patch_checkin_travel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let config = travel::patch(&state.upstream_checkin, &body)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    tracing::info!(
        "[Travel] 排程已{}，时刻 {:?}",
        if config.enabled { "开启" } else { "关闭" },
        config.times
    );
    Ok(Json(
        json!({ "success": true, "data": travel_state(&state) }),
    ))
}

/// POST /api/checkin-center/travel/run —— 立即巡检一轮（真跑，不伪造）。
///
/// - 成功 → `{summary, ok, failed}`
/// - 能力不可用（插件未装/未启用）→ 503 + 可读原因（不落任何标记）
/// - 插件没实现该动作（`unsupported`）→ 501
/// - 上游失败 → 502 + 失败原文
/// - 已有一轮在跑 → 409
pub async fn handle_run_checkin_travel(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let executor = StateTravelExecutor::new(&state);
    match travel::run_now(&state.upstream_checkin, &executor).await {
        RunOutcome::Completed {
            summary,
            ok,
            failed,
        } => Ok(Json(json!({
            "success": true,
            "data": { "summary": summary, "ok": ok, "failed": failed }
        }))),
        RunOutcome::Skipped(message) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "success": false, "error": message })),
        )),
        RunOutcome::Busy => Err((
            StatusCode::CONFLICT,
            Json(json!({
                "success": false,
                "error": "旅行巡检正在执行中，请稍候"
            })),
        )),
        RunOutcome::Failed(error) => {
            let status = match error.code.as_deref() {
                Some("unsupported") => StatusCode::NOT_IMPLEMENTED,
                Some("configuration") => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_GATEWAY,
            };
            Err((
                status,
                Json(json!({ "success": false, "error": error.message })),
            ))
        }
    }
}

// ── 一次性 / 限时福利通道（对照 Agent2API 的 onboarding/keepalive/growth/claim）──
//
// 与每日签到分开：这些动作一次性或限时（领完即结清），不进调度器，
// 只能手动触发。见 `checkin::benefits` 的模块头说明。

/// 福利动作请求体。
#[derive(Debug, Deserialize)]
pub struct BenefitRunRequest {
    pub account_id: String,
    /// 动作 id（newbie / onboarding / onboarding_claim / keepalive / growth / plan / umid）
    pub action: String,
    /// 动作参数（growth 的 `code`、zcode 的 `mode`/`plan_id`/`captcha_verify_param`）
    #[serde(default)]
    pub params: Value,
}

/// GET /api/checkin-center/benefits —— 可用福利通道清单（零上游请求）。
pub async fn handle_benefits_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    let accounts: Vec<Value> = state
        .account_sessions
        .all()
        .into_iter()
        .filter_map(|session| {
            let actions = crate::checkin::benefits::actions_for(&session.provider);
            if actions.is_empty() {
                return None;
            }
            Some(json!({
                "id": session.id,
                "name": session.name,
                "provider": session.provider,
                "channel_id": session.channel_id,
                "status": session.runtime_status(now),
                "actions": actions,
                // 待领新手福利项数（只读记忆，零上游请求；None = 还没盘点过）
                "pending_onboarding": crate::checkin::benefits::onboarding::pending_count(
                    &state.alert_store,
                    &session.id,
                    &session.provider,
                ),
                // 上次保活执行留下的痕迹（仅国际版有）
                "keepalive": crate::checkin::benefits::keepalive::memory_view(
                    &state.alert_store,
                    &session.id,
                ),
            }))
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "accounts": accounts,
            "keepalive": crate::checkin::benefits::keepalive::config_view(&state.alert_store),
        }
    })))
}

/// POST /api/checkin-center/benefits/run —— 执行一次福利动作。
///
/// 不设「一次性动作每日去重」：结清状态由各通道自己的记忆表达（见
/// `benefits::*`），重复调用对已结清的通道是零上游请求的 `already`。
pub async fn handle_benefit_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BenefitRunRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Some(session) = state.account_sessions.get(&body.account_id) else {
        return Err(error_response("账号不存在", StatusCode::NOT_FOUND));
    };
    // 出口走账号自己的代理（与转发/签到同一条链路，解析 pool:/rotate 编码）
    let client = state
        .account_sessions
        .client_for(&session)
        .map_err(|e| error_response(&format!("出口配置错误：{e}"), StatusCode::BAD_REQUEST))?;
    let result = crate::checkin::benefits::run(
        &state.account_sessions,
        &state.alert_store,
        &client,
        &session,
        &body.action,
        &body.params,
    )
    .await;
    Ok(Json(json!({ "success": true, "data": result })))
}

/// POST /api/checkin-center/records/mark-kept-active —— 签到后自动链回写。
///
/// 对签到成功的国际版账号顺带执行保活成功后，把该账号当日最后一条签到
/// 记录标记「附带保活」（见 `CheckinStore::mark_kept_active` 的说明）。
#[derive(Debug, Deserialize)]
pub struct KeptActiveBody {
    pub account_id: String,
    pub provider: String,
}

pub async fn handle_mark_kept_active(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<KeptActiveBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let _ = state.upstream_checkin.mark_kept_active(
        &body.account_id,
        &body.provider,
        &scheduler::today(),
    );
    Ok(Json(
        json!({ "success": true, "data": { "kept_active": true } }),
    ))
}

/// GET /api/checkin-center/benefits/keepalive —— 保活免费模型链配置。
pub async fn handle_get_keepalive(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": crate::checkin::benefits::keepalive::config_view(&state.alert_store),
    })))
}

/// PUT /api/checkin-center/benefits/keepalive —— 保存保活免费模型链（空 = 恢复默认）。
pub async fn handle_put_keepalive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let models: Vec<String> = body
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let view = crate::checkin::benefits::keepalive::save_config(&state.alert_store, models)
        .map_err(|e| {
            error_response(
                &format!("保存保活配置失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(json!({ "success": true, "data": view })))
}

// ─── 活动福利 / 月度赠送（对齐 Agent2API checkin-campaign-* 契约）────────
//
// 前端 `AccountSessions.tsx` 的活动福利卡片按 A2A 口径写：**只有「HTTP 400 +
// 文案含『活动福利』」才判 notSupported → 显示空态**。因此这里的「该家没有活动
// 链路」一律回 400（不是 404）：404 在 AIGX 语义里是「路由不存在」，混用会让
// 用户看到红错而不是空态。404 只保留给「账号 id 不存在」。
//
// 目前只有 ZCode 两地有「限时套餐」活动链路（`checkin::benefits::zcode_claim`）；
// 其余家如实回 400。

/// 该 provider 是否有活动福利链路（目前只有 ZCode 两地的限时套餐）。
fn has_campaign_rewards(provider: &str) -> bool {
    matches!(provider, "zcode" | "zcode-intl")
}

/// 取账号；不存在 → 404（真正的资源缺失）。
fn campaign_account(
    state: &AppState,
    id: &str,
) -> Result<crate::account_session::model::AccountSession, (StatusCode, Json<Value>)> {
    state
        .account_sessions
        .get(id)
        .ok_or_else(|| error_response("账号不存在", StatusCode::NOT_FOUND))
}

/// 把 ZCode 的原始套餐映射成 A2A `CampaignReward` 形状（尽力读取，缺字段给 null）。
fn zcode_plan_to_campaign(plan: &Value, claimed: &[Value]) -> Value {
    let plan_id = plan
        .get("planId")
        .or_else(|| plan.get("plan_id"))
        .or_else(|| plan.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let id_text = plan_id.as_str().unwrap_or("").to_string();
    let is_claimed = claimed.iter().any(|item| {
        item == &plan_id
            || item.as_str() == Some(id_text.as_str())
            || item.get("id").and_then(Value::as_str) == Some(id_text.as_str())
    });
    let title = plan
        .get("name")
        .or_else(|| plan.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("ZCode 限时套餐");
    json!({
        "campaignId": plan_id,
        "title": title,
        "titleEn": title,
        "description": plan.get("desc").or_else(|| plan.get("description")).cloned().unwrap_or(Value::Null),
        "descriptionEn": plan.get("desc").or_else(|| plan.get("description")).cloned().unwrap_or(Value::Null),
        "amount": plan.get("amount").or_else(|| plan.get("credits")).cloned().unwrap_or(Value::Null),
        "unit": "credits",
        // claimed / claimable / inactive / ineligible / confirming
        "status": if is_claimed { "claimed" } else { "claimable" },
        "startsAt": plan.get("startsAt").or_else(|| plan.get("start_at")).cloned().unwrap_or(Value::Null),
        "endsAt": plan.get("endsAt").or_else(|| plan.get("end_at")).cloned().unwrap_or(Value::Null),
        "claimedAt": Value::Null,
        "expiresAt": Value::Null,
        "validDays": plan.get("validDays").or_else(|| plan.get("valid_days")).cloned().unwrap_or(Value::Null),
    })
}

/// GET /api/accounts/:id/campaigns —— 活动福利预览（对齐 A2A `CampaignPreview`）。
///
/// 该家没有活动链路 → 400 + 文案含「活动福利」（前端据此显示空态）。
/// ZCode 两地：上游 preview 查一次套餐，映射成 `campaigns[]`（含本地已领标记）。
pub async fn handle_get_campaigns(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let session = campaign_account(&state, &id)?;
    if !has_campaign_rewards(&session.provider) {
        return Err(error_response(
            "该提供商未提供活动福利",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 出口走 `client_for`（与数据面/签到同链路）
    let client = state
        .account_sessions
        .client_for(&session)
        .map_err(|e| error_response(&format!("出口配置错误：{e}"), StatusCode::BAD_REQUEST))?;
    let preview = crate::checkin::benefits::run(
        &state.account_sessions,
        &state.alert_store,
        &client,
        &session,
        "plan",
        &json!({ "mode": "preview" }),
    )
    .await;
    if !preview.ok
        && matches!(
            preview.outcome,
            crate::checkin::benefits::BenefitOutcome::Failed
        )
    {
        return Err(error_response(&preview.message, StatusCode::BAD_GATEWAY));
    }
    let plans = preview
        .data
        .get("plans")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let claimed = preview
        .data
        .get("claimed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let campaigns: Vec<Value> = plans
        .iter()
        .map(|plan| zcode_plan_to_campaign(plan, &claimed))
        .collect();
    Ok(Json(json!({
        "campaigns": campaigns,
        "riskNote": Value::Null,
        "notice": Value::Null,
        "monthlyGifts": [],
        "redemption": { "supported": false },
    })))
}

/// POST /api/accounts/:id/campaigns/claim —— 领取一份活动福利。
///
/// 该家没有活动链路 → 400 + 「活动福利」；账号不存在 → 404。
/// 响应对齐 A2A `CampaignClaimResult`：`{campaignId, outcome, campaign}`，
/// `outcome` 取 `claimed/already_claimed/inactive/ineligible/confirming/failed`。
pub async fn handle_claim_campaign(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let session = campaign_account(&state, &id)?;
    if !has_campaign_rewards(&session.provider) {
        return Err(error_response(
            "该提供商未提供活动福利",
            StatusCode::BAD_REQUEST,
        ));
    }
    let campaign_id = payload
        .get("campaignId")
        .or_else(|| payload.get("planId"))
        .cloned()
        .unwrap_or(Value::Null);
    if campaign_id.is_null() {
        return Err(error_response("缺少 campaignId", StatusCode::BAD_REQUEST));
    }
    let client = state
        .account_sessions
        .client_for(&session)
        .map_err(|e| error_response(&format!("出口配置错误：{e}"), StatusCode::BAD_REQUEST))?;
    // 前端当场求解的验证码参数（有就带上，没令牌池也能领）
    let mut params = json!({ "mode": "claim", "plan_id": campaign_id.clone() });
    if let Some(object) = params.as_object_mut() {
        if let Some(param) = payload.get("captchaVerifyParam").and_then(Value::as_str) {
            object.insert("captcha_verify_param".to_string(), Value::from(param));
        }
        if let Some(region) = payload.get("captchaVerifyRegion").and_then(Value::as_str) {
            object.insert("captcha_verify_region".to_string(), Value::from(region));
        }
    }
    let result = crate::checkin::benefits::run(
        &state.account_sessions,
        &state.alert_store,
        &client,
        &session,
        "plan",
        &params,
    )
    .await;
    let outcome = match result.outcome {
        crate::checkin::benefits::BenefitOutcome::Done => "claimed",
        crate::checkin::benefits::BenefitOutcome::Already => "already_claimed",
        crate::checkin::benefits::BenefitOutcome::Pending => "confirming",
        crate::checkin::benefits::BenefitOutcome::Skipped => "ineligible",
        crate::checkin::benefits::BenefitOutcome::Failed => "failed",
    };
    // 落当日台账 + 明细（与签到同一份判据）
    let checkin_outcome = match outcome {
        "claimed" => CheckinOutcome::Success,
        "already_claimed" => CheckinOutcome::Already,
        "confirming" => CheckinOutcome::Pending,
        _ => CheckinOutcome::Failed,
    };
    crate::checkin::scheduler::record_with_reason(
        &state.upstream_checkin,
        &session,
        "activity",
        checkin_outcome,
        &result.message,
        false,
        "手动领取活动（活动福利）",
        0,
    );
    Ok(Json(json!({
        "campaignId": campaign_id,
        "outcome": outcome,
        "campaign": Value::Null,
        "message": result.message,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: i64, source: &str, kind: &str) -> CheckinRecord {
        CheckinRecord {
            ts,
            date: "2026-01-01".into(),
            account_id: "a".into(),
            account_name: "n".into(),
            provider: "loomy".into(),
            source: source.into(),
            kind: kind.into(),
            reason: String::new(),
            outcome: CheckinOutcome::Success,
            ok: true,
            kept_active: false,
            streak_days: 0,
            message: String::new(),
            error: String::new(),
        }
    }

    #[test]
    fn filter_records_kind_and_time_range_boundaries() {
        let records = vec![
            rec(100, "manual", "manual"),
            rec(200, "schedule", "schedule"),
            rec(300, "makeup", "makeup"),
            rec(400, "", "schedule"), // 存量记录：kind 空、source 有值
        ];
        // kind 精确筛选；存量 kind 为空时回退 source
        let ids = |v: Vec<CheckinRecord>| v.into_iter().map(|r| r.ts).collect::<Vec<_>>();
        assert_eq!(
            ids(filter_records(
                records.clone(),
                Some("schedule"),
                None,
                None
            )),
            vec![200, 400]
        );
        assert_eq!(
            ids(filter_records(records.clone(), Some("makeup"), None, None)),
            vec![300]
        );
        // 空串 = 不过滤
        assert_eq!(
            filter_records(records.clone(), Some(""), None, None).len(),
            4
        );
        // 时间区间为**闭区间**：端点都包含
        assert_eq!(
            ids(filter_records(records.clone(), None, Some(200), Some(300))),
            vec![200, 300]
        );
        assert_eq!(
            ids(filter_records(records.clone(), None, Some(201), None)),
            vec![300, 400]
        );
        assert_eq!(
            ids(filter_records(records.clone(), None, None, Some(199))),
            vec![100]
        );
        // 组合：kind + 区间
        assert_eq!(
            ids(filter_records(records, Some("schedule"), Some(300), None)),
            vec![400]
        );
    }
}
