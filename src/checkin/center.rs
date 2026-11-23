// Ported from Agent2API v3.5.0-beta.3 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 签到中心的**状态视图 + 立即执行入口**（对齐 Agent2API `core/auto_checkin`
//! 的 `state()` / `run_now` / `run_schedule_now`，以及 `api/checkin_center.rs`
//! 的聚合形状）。
//!
//! ## 为什么单独一层
//!
//! `store` 管持久化、`scheduler` 管到点触发、`schedules` 管条目 CRUD；
//! 这里只负责**读侧组装**（界面渲染要的形状）与**手动执行**（按钮入口）。
//! 三者共用同一份 `CheckinStore` 句柄，没有第二个落点。
//!
//! ## 契约（对齐 A2A，字段一律 camelCase）
//!
//! - `schedules[]`：`displayLabel / timeEnd / providerTimes / providerRows /
//!   retryTime / providerRetryTimes / retryGapMinutes / retryLimit /
//!   lastFiredDate / lastFiredToday / pendingProviders / retryCount /
//!   retryExhausted / retryAttempts / nextRunAt / lastResult`
//! - 顶层：`enabled / running / providerOptions[{id,label,builtinTime}] /
//!   defaultRetryTime / schedules`，另带 AIGX 扩展 `settings`（设置卡直读）。
//!
//! ## 手动执行与定时执行的唯一差别
//!
//! 手动执行**不看**总开关与条目开关（用户点了就是明确要跑），也不做窗口
//! 判定与去重（按范围全量跑一遍）；但共用同一条 `run_targets` 执行链与
//! 同一份台账 —— 判据、落账、重试口径都不会分叉。
//!
//! ## 并发闸
//!
//! 手动执行与定时执行共用一个进程级「正在执行」标志：签到是串行打上游的
//! 长动作，两轮叠跑既浪费额度又招风控。抢不到闸返回 `None`，路由层翻成
//! 「签到正在执行中，请稍候」。

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{json, Map, Value};

use super::claim;
use super::scheduler::{self, ScheduleOutcome};
use super::schedules::DEFAULT_RETRY_TIME;
use super::store::{CheckinSchedule, CheckinStore};
use crate::account_session::AccountSessionStore;

/// 进程级「正在执行」闸。
static RUNNING: AtomicBool = AtomicBool::new(false);

/// 抢闸守卫：Drop 时复位（正常返回 / 中途 panic 都不会把闸永久卡死）。
struct Guard;

impl Guard {
    fn acquire() -> Option<Self> {
        if RUNNING.swap(true, Ordering::SeqCst) {
            None
        } else {
            Some(Guard)
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// 是否正在执行签到。
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// 界面可勾选的签到提供商（顺序 = 注册顺序）。
///
/// 只列**实际可用**的家（见 [`claim::available_checkin_providers`]）：没实现
/// 的三家（插件仓）与没账号的家不出现，避免一屏点了必然空转的复选框。
pub fn provider_options(sessions: &AccountSessionStore) -> Vec<String> {
    claim::available_checkin_providers(sessions)
}

/// 条目最近一轮的结构化结果（对齐 A2A `lastResult`）。
///
/// 优先 `last_summary`（新写法）；存量条目没有它时，用上一句摘要 + 计数字段
/// 拼一份同形对象 —— 界面不必判「字段缺失」。`retryAttempts` 按今天的重试
/// 记账合并进来（对齐 A2A `retry::merge` 的输出）。
fn last_result_view(schedule: &CheckinSchedule, retry_attempts: &Map<String, Value>) -> Value {
    let mut value = if !schedule.last_summary.is_null() {
        schedule.last_summary.clone()
    } else if !schedule.last_result.is_empty() || !schedule.last_fired_date.is_empty() {
        json!({
            "date": schedule.last_fired_date,
            "reason": schedule.last_result,
            "succeeded": schedule.last_ok,
            "active": schedule.last_keepalive,
            "failedCount": schedule.last_failed,
        })
    } else {
        Value::Null
    };
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "retryAttempts".to_string(),
            Value::Object(retry_attempts.clone()),
        );
    }
    value
}

/// 一条条目的对外视图（对齐 A2A `AutoCheckin::state()` 的 `schedules[]` 元素）。
fn schedule_view(
    store: &CheckinStore,
    schedule: &CheckinSchedule,
    master: bool,
    today: &str,
) -> Value {
    let pending = super::scheduler::pending_providers(schedule, today);
    let fired_today = pending.is_empty();
    let rows = super::schedules::provider_rows(store, schedule, today);
    let (retry_count, retry_exhausted, retry_attempts) = scheduler::retry_snapshot(store, schedule);
    let retry_attempts_json: Map<String, Value> = retry_attempts
        .into_iter()
        .map(|(id, count)| (id, Value::from(count)))
        .collect();
    let provider_retry_times = super::schedules::provider_retry_times(schedule);
    json!({
        "id": schedule.id,
        "label": schedule.label,
        "displayLabel": super::schedules::display_label(schedule),
        "enabled": schedule.enabled,
        "time": schedule.time,
        "timeEnd": schedule.time_end,
        "randomize": schedule.randomize,
        "providers": schedule.providers,
        "providerTimes": schedule.provider_times,
        "providerRows": rows,
        // A2A 的「补签时刻」叫 retryTime；AIGX 落盘字段名是 makeup_time
        "retryTime": schedule.makeup_time,
        // 逐家的补签时刻（窗口结束 + 等待间隔，不早于 retryTime）
        "providerRetryTimes": provider_retry_times,
        "retryGapMinutes": schedule.retry_gap_minutes,
        "retryLimit": schedule.retry_limit,
        "lastFiredDate": if schedule.last_fired_date.is_empty() {
            Value::Null
        } else {
            Value::String(schedule.last_fired_date.clone())
        },
        "lastFiredToday": fired_today,
        "pendingProviders": pending,
        // 中性未领取、等窗口重试的账号数（界面上要说清「还会再试」）
        "retryCount": retry_count,
        // 今天重试次数已用完的账号（已不在重试名单里，但必须单独说一句）
        "retryExhausted": retry_exhausted,
        "retryAttempts": retry_attempts_json,
        "nextRunAt": if master && schedule.enabled {
            super::scheduler::next_run_at(schedule, today)
        } else {
            None
        },
        "lastResult": last_result_view(schedule, &retry_attempts_json),
    })
}

/// 自动签到状态（对齐 A2A `GET /api/auto-checkin` / `GET /api/checkin-schedules`）。
///
/// 顶层是**兼容视图**（默认条目的投影：`time` / `providers` / `lastResult` …），
/// `schedules` 是完整条目数组。前端两套页面（旧的单条目视图 + 新的多条目视图）
/// 都能直接用同一份响应；`settings` 是 AIGX 扩展（设置卡直读）。
pub fn auto_state(store: &CheckinStore, sessions: &AccountSessionStore) -> Value {
    let settings = store.settings();
    let today = scheduler::today();
    let master = settings.enabled;
    let schedules = store.schedules();
    let entries: Vec<Value> = schedules
        .iter()
        .map(|schedule| schedule_view(store, schedule, master, &today))
        .collect();
    let next_run_at = if master {
        entries
            .iter()
            .filter_map(|entry| entry.get("nextRunAt").and_then(Value::as_i64))
            .min()
    } else {
        None
    };
    let default_entry = entries.first();
    let default_last_result = default_entry
        .and_then(|entry| entry.get("lastResult").cloned())
        .unwrap_or(Value::Null);
    let options: Vec<Value> = provider_options(sessions)
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "label": crate::model::manage::provider_label(&id),
                "builtinTime": super::scheduler::PROVIDER_TIMES
                    .iter()
                    .find(|(pid, _)| *pid == id)
                    .map(|(_, time)| Value::from(*time))
                    .unwrap_or(Value::Null),
            })
        })
        .collect();
    let empty_schedule = CheckinSchedule {
        id: String::new(),
        label: String::new(),
        enabled: false,
        time: "00:01".to_string(),
        time_end: String::new(),
        randomize: false,
        retry_limit: 3,
        retry_gap_minutes: 5,
        makeup_time: DEFAULT_RETRY_TIME.to_string(),
        providers: Vec::new(),
        provider_times: Default::default(),
        last_fired_date: String::new(),
        last_fired_providers: Default::default(),
        last_retried_providers: Default::default(),
        account_plan: Default::default(),
        last_result: String::new(),
        last_summary: Value::Null,
        last_ok: 0,
        last_keepalive: 0,
        last_failed: 0,
    };
    let default_schedule = schedules.first().unwrap_or(&empty_schedule);
    json!({
        "enabled": master,
        "running": is_running(),
        // 兼容字段（默认条目的投影；语义与改造前的顶层字段一致）
        "time": default_schedule.time,
        "timeEnd": default_schedule.time_end,
        "providers": default_schedule.providers,
        "lastFiredDate": if default_schedule.last_fired_date.is_empty() {
            Value::Null
        } else {
            Value::String(default_schedule.last_fired_date.clone())
        },
        "lastFiredToday": default_entry
            .and_then(|entry| entry.get("lastFiredToday").and_then(Value::as_bool))
            .unwrap_or(false),
        "lastResult": default_last_result,
        "nextRunAt": next_run_at,
        // 界面的复选框清单（签到范围），顺序即注册顺序
        "providerOptions": options,
        // 缺省补签时刻：界面「补签时刻」输入框的 placeholder 用
        "defaultRetryTime": DEFAULT_RETRY_TIME,
        "schedules": entries,
        // AIGX 扩展：签到设置（间隔 / 时区 / 保留期）—— 设置卡直接读这一块
        "settings": settings,
    })
}

/// 立即执行**全部**可签账号（手动全量，对齐 A2A `run_now`）。
///
/// 返回 `None` = 已有一次在执行（路由层翻成 400「签到正在执行中」）。
pub async fn run_now(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
) -> Option<ScheduleOutcome> {
    let guard = Guard::acquire()?;
    let settings = store.settings();
    let providers: Vec<String> = claim::CHECKIN_PROVIDERS
        .iter()
        .map(|id| id.to_string())
        .collect();
    let targets = scheduler::collect_targets(sessions, &providers);
    let outcome = scheduler::run_targets_with_reason(
        store,
        sessions,
        targets,
        "manual",
        settings.gap_ms,
        "手动触发（全部）",
    )
    .await;
    drop(guard);
    Some(outcome)
}

/// 立即执行**某条条目**的范围（对齐 A2A `run_schedule_now`）。
///
/// 不看总开关与条目开关（用户点了就是明确要跑）。条目不存在返回 `None`，
/// 路由层先查存在性以区分「不存在」与「正在执行」。
pub async fn run_schedule_now(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    id: &str,
) -> Option<ScheduleOutcome> {
    let schedule = store.schedules().into_iter().find(|s| s.id == id)?;
    let guard = Guard::acquire()?;
    let settings = store.settings();
    let targets = scheduler::collect_targets(sessions, &schedule.providers);
    let reason = format!("手动触发（{}）", super::schedules::display_label(&schedule));
    let outcome = scheduler::run_targets_with_reason(
        store,
        sessions,
        targets,
        "manual",
        settings.gap_ms,
        &reason,
    )
    .await;
    // 手动执行也更新条目「上次执行」摘要，界面按钮点完立刻能看到
    let counts = super::store::ScheduleCounts {
        ok: outcome.ok,
        keepalive: outcome.keepalive,
        failed: outcome.failed,
    };
    let _ = store.mark_fired(&schedule.id, &scheduler::today(), &outcome.summary, &counts);
    let _ = store.mark_last_summary(&schedule.id, &outcome.last_summary);
    drop(guard);
    Some(outcome)
}

// ─── 快照的账号行 / 分组（对齐 A2A `api/checkin_center.rs`）────────────

/// 账号的可用性（签到中心「不可用」徽标口径）：**只看凭证是否过期**——
/// 停用（disabled）账号仍可签到（用户要求），故不再以启用状态判不可用。
pub fn account_available(session: &crate::account_session::model::AccountSession) -> bool {
    !session.is_expired(chrono::Utc::now().timestamp())
}

/// 账号行（对齐 A2A `CheckinAccount`：`{id,name,available,checkinAt,
/// checkedInToday,dailyCheckin,edition,streakDays}`，另带 AIGX 展示用扩展）。
pub fn account_row(
    store: &CheckinStore,
    session: &crate::account_session::model::AccountSession,
    today: &str,
) -> Value {
    let last = store
        .last_today(&session.id, &session.provider)
        .ok()
        .flatten();
    json!({
        "id": session.id,
        "name": if session.name.is_empty() { session.id.clone() } else { session.name.clone() },
        "available": account_available(session),
        // 上次签到时间（毫秒；A2A 同名字段）
        "checkinAt": last.as_ref().map(|r| r.ts.saturating_mul(1000)),
        "checkedInToday": last.as_ref().map(|r| r.ok).unwrap_or(false),
        // 当日签到台账（只有 ok/already 才带 claimAt）
        "dailyCheckin": store
            .daily_checkin(&session.id, today)
            .unwrap_or(Value::Null),
        // 版本（cn / intl，缺失 null）：国际版按钮文案是「领日活」而非「签到」
        "edition": session.extra_headers.get("edition").cloned().unwrap_or_else(|| {
            if session.provider.ends_with("-intl") { "intl".to_string() } else { "cn".to_string() }
        }),
        // 成长中心连登天数（0 = 还没读到过，恒为数字）
        "streakDays": store.streak_days(&session.id),
        // AIGX 展示扩展（表格「最近结果」列直读）
        "lastOutcome": last.as_ref().map(|r| r.outcome.as_str()),
        "lastMessage": last.as_ref().map(|r| r.message.clone()),
        "lastTs": last.as_ref().map(|r| r.ts),
        "channel_id": session.channel_id,
    })
}

/// 账号为什么不在签到范围内（对齐 A2A `out_of_scope_reason`）。
///
/// AIGX 的排除分支：实现未进主仓（插件仓）/ 没有签到链路 / 凭证过期。
/// 注：**账号停用不再排除**（用户要求 disabled 账号也签到——停用但凭证有效
/// 仍参与）；判据与 `scheduler::collect_targets` 同源。
pub fn out_of_scope_reason(
    session: &crate::account_session::model::AccountSession,
) -> Option<&'static str> {
    if claim::UNSUPPORTED_PROVIDERS.contains(&session.provider.as_str()) {
        return Some("实现随上游进入插件仓，主仓未接入");
    }
    if !claim::supports_checkin(&session.provider) {
        return Some("没有签到链路");
    }
    if session.is_expired(chrono::Utc::now().timestamp()) {
        return Some("凭证已过期");
    }
    None
}

/// 新手福利的账号入口（对齐 A2A `extras.onboarding`）。
///
/// 判据是 `benefits::actions_for` 里有 `onboarding` 的家（loomy /
/// xiaohuanxiong）；OfficeAce 的新人礼与每日签到是同一个上游动作（A2A 同样
/// 不把它列进新手福利档），因此排除。`settled` 是**一次性福利的结算记忆**
/// （本地事实、零上游请求），界面据此直接渲染「已领取」并跳过自动查询。
pub fn onboarding_rows(store: &CheckinStore, sessions: &AccountSessionStore) -> Vec<Value> {
    sessions
        .all()
        .into_iter()
        .filter(|session| {
            session.provider != "officeace"
                && crate::checkin::benefits::actions_for(&session.provider)
                    .contains(&"onboarding")
        })
        .map(|session| {
            let memory = crate::checkin::benefits::load_memory(
                store.file_store(),
                &crate::checkin::benefits::memory_key(&session.id, "onboarding"),
            );
            let settled = memory
                .as_ref()
                .and_then(|value| value.get("settled"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let settled_at = memory
                .as_ref()
                .and_then(|value| value.get("updated_at"))
                .cloned()
                .unwrap_or(Value::Null);
            json!({
                "id": session.id,
                "name": if session.name.is_empty() { session.id.clone() } else { session.name.clone() },
                "provider": session.provider,
                "settled": if settled {
                    // A2A `OnboardingSettlement` 的形状（at/tasks/earned/total）；
                    // AIGX 的结算记忆只记「结清」这一事实，明细字段给空值
                    json!({
                        "at": settled_at,
                        "tasks": [],
                        "earned": Value::Null,
                        "total": Value::Null,
                    })
                } else {
                    Value::Null
                },
                // 待领项数（只读记忆，零上游请求；None = 还没盘点过）
                "pending": crate::checkin::benefits::onboarding::pending_count(
                    store.file_store(),
                    &session.id,
                    &session.provider,
                ),
            })
        })
        .collect()
}

/// 活动套餐的账号入口（对齐 A2A `extras.plans`）：带**本地领取台账**
/// （`account.claimPlans` 形状，零上游请求），界面「今天领过哪几份」按它
/// 逐份比对。
pub fn plan_rows(store: &CheckinStore, sessions: &AccountSessionStore) -> Vec<Value> {
    sessions
        .all()
        .into_iter()
        .filter(|session| {
            crate::checkin::benefits::actions_for(&session.provider).contains(&"plan")
        })
        .map(|session| {
            let memory = crate::checkin::benefits::load_memory(
                store.file_store(),
                &crate::checkin::benefits::memory_key(&session.id, "zcode_plan"),
            )
            .unwrap_or(Value::Null);
            let claimed_at = memory
                .get("claimed_at")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            // A2A 的 claimAt / claimPlans 值是**毫秒**；AIGX 记忆里存的是秒
            let claimed_at_ms = claimed_at.saturating_mul(1000);
            let claim_plans: Map<String, Value> = memory
                .get("claimed")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let id = item
                                .as_str()
                                .or_else(|| item.get("id").and_then(Value::as_str))?;
                            Some((id.to_string(), Value::from(claimed_at_ms)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "id": session.id,
                "name": if session.name.is_empty() { session.id.clone() } else { session.name.clone() },
                "provider": session.provider,
                "claimAt": if claimed_at_ms > 0 { Value::from(claimed_at_ms) } else { Value::Null },
                "claimPlans": Value::Object(claim_plans),
            })
        })
        .collect()
}

/// 成长任务的账号入口（对齐 A2A `extras.growth`）。
pub fn growth_rows(sessions: &AccountSessionStore) -> Vec<Value> {
    sessions
        .all()
        .into_iter()
        .filter(|session| {
            crate::checkin::benefits::actions_for(&session.provider).contains(&"growth")
        })
        .map(|session| {
            json!({
                "id": session.id,
                "name": if session.name.is_empty() { session.id.clone() } else { session.name.clone() },
                "provider": session.provider,
            })
        })
        .collect()
}

/// 活动福利的账号入口（对齐 A2A `extras.campaigns`）：判据是「有活动链路但
/// 没归入新手福利 / 套餐」的家。AIGX 当前的活动链路只有 ZCode 限时套餐，
/// 已归入 `plans`（`actions_for` 里没有独立的活动能力位），因此这里为空集 ——
/// 形状保留，将来接入独立活动链路时往这里填即可。
pub fn campaign_rows() -> Vec<Value> {
    Vec::new()
}

/// 各家申报的签到链路信息（A2A `checkinFeatures` 的 AIGX 等价物）。
///
/// A2A 由插件申报（`CheckinInfo`）；AIGX 的执行体在主仓，能力从
/// `claim::supports_checkin` 与 `benefits::actions_for` 推导。字段名按 A2A
/// 的序列化形状（camelCase），没申报的家不出现（旧读端走自己的兜底）。
pub fn checkin_features() -> Map<String, Value> {
    let mut map = Map::new();
    for provider in claim::CHECKIN_PROVIDERS {
        let provider = *provider;
        let actions = crate::checkin::benefits::actions_for(provider);
        let daily =
            claim::supports_checkin(provider) && !claim::UNSUPPORTED_PROVIDERS.contains(&provider);
        let onboarding = actions.contains(&"onboarding");
        let plans = actions.contains(&"plan");
        let growth = actions.contains(&"growth");
        let newbie = actions.contains(&"newbie");
        // 完全没有签到链路且没有福利通道的家不进表（界面显示空态）
        if !daily && !onboarding && !plans && !growth && !newbie {
            continue;
        }
        let mut modes: Vec<&str> = Vec::new();
        if daily {
            modes.push("checkin");
        }
        if provider == "workbuddy-intl" {
            modes.extend(["keepalive", "claim", "full"]);
        }
        let ui_hooks: Vec<&str> = if provider == "qoder-intl" {
            vec!["umid_component"]
        } else {
            Vec::new()
        };
        map.insert(
            provider.to_string(),
            json!({
                "daily": daily,
                "onboarding": onboarding || newbie,
                "plans": plans,
                "growth": growth,
                "manualPlans": daily && plans,
                "excludeGuest": provider == "chatglm",
                "description": Value::Null,
                "modes": modes,
                "uiHooks": ui_hooks,
                // AIGX 扩展：该家的全部福利动作（福利通道页用）
                "benefitActions": actions,
            }),
        );
    }
    map
}

/// 界面要显示的管理面板（对齐 A2A `adminPanels`：`daily_keepalive` /
/// `growth_tasks`）。
///
/// A2A 由插件申报；AIGX 按「库里有没有该能力的目标账号」推导 —— 没有
/// workbuddy-intl 账号就不显示「保活模型链」，没有 workbuddy 账号就不显示
/// 「猫猫旅行」。与「没装插件 → 面板不出现」同一取向。
pub fn admin_panels(sessions: &AccountSessionStore) -> Vec<&'static str> {
    let providers: std::collections::HashSet<String> =
        sessions.all().into_iter().map(|s| s.provider).collect();
    let mut panels: Vec<&'static str> = Vec::new();
    if providers.contains("workbuddy-intl") {
        panels.push("daily_keepalive");
    }
    if providers.contains("workbuddy") {
        panels.push("growth_tasks");
    }
    panels
}

/// 保活免费模型链的对外视图（A2A `KeepaliveState`：camelCase `defaultModels`，
/// 另带 AIGX 既有 snake_case 别名）。
pub fn keepalive_view(store: &CheckinStore) -> Value {
    let view = crate::checkin::benefits::keepalive::config_view(store.file_store());
    let models = view.get("models").cloned().unwrap_or(json!([]));
    let default_models = view.get("default_models").cloned().unwrap_or(json!([]));
    json!({
        "models": models,
        "defaultModels": default_models,
        "default_models": default_models,
    })
}

/// 活动预览的空形状（对齐 A2A `CampaignPreview`：`CampaignReward[]` /
/// `MonthlyGift[]` / `riskNote` / `redemption`）。
///
/// AIGX 目前没有接入任何 provider 的「活动 / 月度赠送」下发链路，因此这里返回
/// **空集但形状对齐**：前端按 A2A 的 `CampaignPreview` 类型解析不会拿到
/// `undefined`，将来接入某家的活动时只需往 `campaigns` 里填数据，形状不用改。
pub fn campaign_preview_shape() -> Value {
    json!({
        "campaigns": [],
        // 风控提示（Qoder 国际版这类需要设备指纹的家会用到；暂空）
        "riskNote": Value::Null,
        "notice": Value::Null,
        // 月度赠送（MonthlyGift[]）：{id, orderId, period, amount, scheduledAt,
        // grantedAt, status}
        "monthlyGifts": [],
        // 兑换码通道是否可用（BenefitRedemptionResult）
        "redemption": { "supported": false },
    })
}

// 已删除（2026-11-18）：此处原有一份 `account_row()`，是
// `checkin_center::handle_checkin_center` 内联 `json!` 的**副本**。核账时
// 发现它已与 handler 漂移：缺 `edition` 字段、`available` 少了
// `!is_expired(now)` 判据。现由本文件的 [`account_row`] 唯一化，handler
// 直接调用 —— 行形状的权威定义只此一处。

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_store() -> Arc<crate::storage::FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering as O};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, O::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-center-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(crate::storage::FileStore::new(dir))
    }

    /// 全局 `RUNNING` 闸是进程级单例：凡是断言它具体值的测试必须串行，
    /// 否则并行跑时 `guard_prevents_concurrent_runs` 持闸会让别的测试读到 true。
    static GLOBAL_STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn auto_state_shape_has_a2a_contract_and_settings() {
        // 只断言「字段存在且类型正确」，**不断言 running 的具体值** ——
        // running 是进程级全局闸，并行测试下由别的用例持有，断言值会 flaky。
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        let sessions = AccountSessionStore::new(temp_store());
        let state = auto_state(&store, &sessions);
        // A2A 契约字段
        assert!(state.get("enabled").is_some());
        assert!(state.get("time").is_some());
        assert!(state.get("providerOptions").is_some());
        assert!(state.get("defaultRetryTime").is_some());
        assert!(state.get("schedules").is_some());
        // AIGX 扩展：设置卡
        assert!(state.get("settings").is_some());
        // running 只断言是布尔（不断言值）
        assert!(state.get("running").and_then(Value::as_bool).is_some());
        // 没有条目时 schedules 为空数组（不是 null）
        assert_eq!(
            state
                .get("schedules")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn schedule_view_emits_camel_case_contract() {
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        let schedule = CheckinSchedule {
            id: "s1".into(),
            label: "每日".into(),
            enabled: true,
            time: "09:00".into(),
            time_end: "09:30".into(),
            randomize: true,
            retry_limit: 3,
            retry_gap_minutes: 5,
            makeup_time: "10:00".into(),
            providers: vec!["loomy".into()],
            provider_times: Default::default(),
            last_fired_date: String::new(),
            last_fired_providers: Default::default(),
            last_retried_providers: Default::default(),
            account_plan: Default::default(),
            last_result: "成功 1/1".into(),
            last_summary: serde_json::json!({ "succeeded": 1, "total": 1 }),
            last_ok: 1,
            last_keepalive: 0,
            last_failed: 0,
        };
        let view = schedule_view(&store, &schedule, true, &scheduler::today());
        for key in [
            "displayLabel",
            "providerRows",
            "providerTimes",
            "retryTime",
            "providerRetryTimes",
            "retryGapMinutes",
            "retryLimit",
            "lastFiredDate",
            "lastFiredToday",
            "pendingProviders",
            "retryCount",
            "retryExhausted",
            "retryAttempts",
            "nextRunAt",
            "lastResult",
        ] {
            assert!(view.get(key).is_some(), "缺少字段 {key}");
        }
        // lastResult 带上 retryAttempts（A2A retry::merge 同形）
        assert!(view["lastResult"].get("retryAttempts").is_some());
    }

    #[test]
    fn guard_prevents_concurrent_runs() {
        // 持锁串行化：这条会改写全局 RUNNING，避免与其它读它的用例竞争。
        let _serial = GLOBAL_STATE.lock().unwrap_or_else(|e| e.into_inner());
        let first = Guard::acquire();
        assert!(first.is_some());
        // 第二次抢不到
        assert!(Guard::acquire().is_none());
        assert!(is_running());
        drop(first);
        assert!(!is_running());
        // 释放后又能抢到
        assert!(Guard::acquire().is_some());
    }
}
