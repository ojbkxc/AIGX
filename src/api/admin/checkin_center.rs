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
            "providers": providers,
            "schedules": state.upstream_checkin.schedules(),
            "records_total": records_total,
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
    #[serde(default = "default_days")]
    pub days: u32,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub outcome: Option<String>,
    /// 关键词（账号名 / 账号 id / 上游 / 说明，大小写不敏感包含）
    #[serde(default)]
    pub keyword: Option<String>,
    /// 分页偏移（从 0 起）
    #[serde(default)]
    pub offset: usize,
    /// 分页大小（≤0 = 不分页，返回全部）
    #[serde(default = "default_page_size")]
    pub limit: usize,
}

fn default_days() -> u32 {
    7
}

fn default_page_size() -> usize {
    50
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
        _ => None,
    };
    let records = state
        .upstream_checkin
        .recent_records(
            q.days,
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
    // 保存计划只改配置：运行状态（上次触发/结果/成功·保活·失败计数）必须从现有
    // 计划继承，否则编辑一次就把界面「上次执行」清空，且空 last_fired_date 会让
    // 调度器当天重复触发签到（浪费上游日配额）。
    let (last_fired_date, last_result, last_ok, last_keepalive, last_failed) = state
        .upstream_checkin
        .schedules()
        .into_iter()
        .find(|s| s.id == body.id)
        .map(|s| {
            (
                s.last_fired_date,
                s.last_result,
                s.last_ok,
                s.last_keepalive,
                s.last_failed,
            )
        })
        .unwrap_or_default();
    let list = state
        .upstream_checkin
        .upsert_schedule(CheckinSchedule {
            id: body.id,
            label,
            enabled: body.enabled,
            time: body.time,
            providers: body.providers,
            last_fired_date,
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
    // 出口走账号自己的代理（与转发/签到同一条链路）
    let client = state
        .account_sessions
        .egress()
        .client(session.proxy.as_deref())
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
