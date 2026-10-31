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

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::checkin::claim::{self, supports_checkin};
use crate::checkin::scheduler;
use crate::checkin::store::{CheckinOutcome, CheckinSchedule, CheckinSettings};

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
            .checkin_store
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

    Ok(Json(json!({
        "success": true,
        "data": {
            "settings": settings,
            "supported_providers": claim::supported_providers(),
            "providers": providers,
            "schedules": state.upstream_checkin.schedules(),
            "today_summary": summary.iter().map(|(k, (done, total))| json!({
                "provider": k, "done": done, "total": total
            })).collect::<Vec<_>>(),
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
        let client = match state
            .account_sessions
            .egress()
            .client(session.proxy.as_deref())
        {
            Ok(client) => client,
            Err(e) => {
                let message = format!("出口配置错误：{e}");
                scheduler::record(
                    &state.upstream_checkin,
                    session,
                    "manual",
                    CheckinOutcome::Failed,
                    &message,
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
    #[serde(default = "default_days")]
    pub days: u32,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
}

fn default_days() -> u32 {
    7
}

/// GET /api/checkin-center/records —— 台账。
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
        _ => None,
    };
    let records = state
        .checkin_store
        .recent_records(
            q.days,
            q.provider.as_deref().filter(|s| !s.is_empty()),
            q.account_id.as_deref().filter(|s| !s.is_empty()),
            outcome,
        )
        .map_err(|e| {
            error_response(
                &format!("读取签到记录失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    let total = records.len();
    Ok(Json(json!({
        "success": true,
        "data": { "records": records, "total": total }
    })))
}

/// DELETE /api/checkin-center/records —— 清空台账。
pub async fn handle_clear_checkin_records(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let removed = state.upstream_checkin.clear_records(q.days).map_err(|e| {
        error_response(
            &format!("清空签到记录失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "success": true,
        "data": { "removed": removed }
    })))
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
}

fn default_true() -> bool {
    true
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
    let list = state
        .checkin_store
        .upsert_schedule(CheckinSchedule {
            id: body.id,
            label,
            enabled: body.enabled,
            time: body.time,
            providers: body.providers,
            last_fired_date: String::new(),
            last_result: String::new(),
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
        .checkin_store
        .schedules()
        .into_iter()
        .find(|s| s.id == body.id)
        .ok_or_else(|| error_response("签到计划不存在", StatusCode::NOT_FOUND))?;
    let settings = state.upstream_checkin.settings();
    let summary = scheduler::run_schedule(
        &state.upstream_checkin,
        &state.account_sessions,
        state.account_sessions.egress(),
        &schedule.providers,
        settings.gap_ms,
    )
    .await;
    let _ = state
        .checkin_store
        .mark_fired(&schedule.id, &scheduler::today(), &summary);
    Ok(Json(
        json!({ "success": true, "data": { "summary": summary } }),
    ))
}
