//! 签到中心 API（仅管理员）。
//!
//! - `GET  /api/checkin-center`          总览快照：支持清单 + 今日汇总 + 账号状态
//! - `POST /api/checkin-center/run`      立即签到（全部 / 指定账号 / 指定 provider）
//! - `GET  /api/checkin-center/records`  台账分页
//! - `DELETE /api/checkin-center/records` 清空台账
//! - `GET/PUT /api/checkin-center/settings` 总开关 + 间隔
//! - `GET/POST /api/checkin-center/schedules` 计划列表 / 新增更新
//! - `POST /api/checkin-center/schedules/remove` 删除计划
//! - `POST /api/checkin-center/schedules/run` 立即执行某计划
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
use std::collections::HashMap;

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::checkin::claim::{self, supports_checkin};
use crate::checkin::scheduler;
use crate::checkin::store::{CheckinOutcome, CheckinRecord, CheckinSchedule, CheckinSettings};

/// GET /api/checkin-center —— 总览快照（零上游请求）。
pub async fn handle_checkin_center(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let targets = scheduler::collect_targets(&state.account_sessions, &[]);
    let summary = state.upstream_checkin.today_summary().unwrap_or_default();
    let settings = state.upstream_checkin.settings();

    // 按 provider 分组：每个 provider 下有哪些账号、今天签到状态
    let mut by_provider: std::collections::BTreeMap<String, Vec<Value>> =
        std::collections::BTreeMap::new();
    for session in &targets {
        let last = state
            .upstream_checkin
            .last_today(&session.id, &session.provider)
            .ok()
            .flatten();
        by_provider
            .entry(session.provider.clone())
            .or_default()
            .push(json!({
                "id": session.id,
                "name": session.name,
                "channel_id": session.channel_id,
                "checked_in_today": last.as_ref().map(|r| r.ok).unwrap_or(false),
                "last_outcome": last.as_ref().map(|r| r.outcome.as_str()),
                "last_message": last.as_ref().map(|r| r.message.clone()),
                "last_ts": last.as_ref().map(|r| r.ts),
            }));
    }

    let providers: Vec<Value> = by_provider
        .into_iter()
        .map(|(provider, accounts)| {
            let (done, total) = summary.get(&provider).copied().unwrap_or((0, 0));
            json!({
                "provider": provider,
                "label": crate::model::manage::provider_label(&provider),
                "accounts": accounts,
                "today_done": done,
                "today_total": total,
            })
        })
        .collect();

    let records_total = state.upstream_checkin.total_records().unwrap_or(0);

    Ok(Json(json!({
        "success": true,
        "data": {
            "settings": settings,
            "supported_providers": claim::supported_providers(),
            // 实现随上游进入闭源插件仓、主仓未接入的三家（前端据此置灰入口）：
            "unsupported_providers": claim::UNSUPPORTED_PROVIDERS,
            "providers": providers,
            "schedules": state.upstream_checkin.schedules(),
            "records_total": records_total,
            "today_summary": summary.iter().map(|(k, (done, total))| json!({
                "provider": k, "done": done, "total": total
            })).collect::<Vec<_>>(),
            // #18-⑤（生产实证：三处开关全关，签到静默不跑，用户以为坏了）：
            // 定时总开关关 + 重试间隔关（retry_gap_ms=0）+ 无启用计划 ⇒
            // all_disabled=true，前端据此显示显著提示 + 一键开启入口。
            // 是否开启由用户决策——后端只如实上报，不擅自改配置。
            "all_disabled": !settings.enabled
                && settings.retry_gap_ms == 0
                && state.upstream_checkin.schedules().iter().all(|s| !s.enabled),
        }
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
    let (last_fired_date, last_result, last_ok, last_keepalive, last_failed, last_fired_providers) =
        existing
            .as_ref()
            .map(|s| {
                (
                    s.last_fired_date.clone(),
                    s.last_result.clone(),
                    s.last_ok,
                    s.last_keepalive,
                    s.last_failed,
                    s.last_fired_providers.clone(),
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
            last_result,
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
            outcome: CheckinOutcome::Success,
            ok: true,
            kept_active: false,
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
