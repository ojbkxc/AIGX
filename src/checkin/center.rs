// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
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

use serde_json::{json, Value};

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
/// 只列**库里真有账号**的家：没账号的家勾了也没账号可签，列出来是一屏点了
/// 必然空转的复选框（A2A 2026-10 用户反馈同一结论）。一家账号都没有时回落到
/// 完整白名单（首次配置场景，用户还没导入账号）。
pub fn provider_options(sessions: &AccountSessionStore) -> Vec<String> {
    let present: std::collections::HashSet<String> =
        sessions.all().into_iter().map(|s| s.provider).collect();
    let picked: Vec<String> = claim::CHECKIN_PROVIDERS
        .iter()
        .filter(|id| present.contains(**id))
        .map(|id| id.to_string())
        .collect();
    if picked.is_empty() {
        claim::CHECKIN_PROVIDERS
            .iter()
            .map(|id| id.to_string())
            .collect()
    } else {
        picked
    }
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
    // 字段一律 snake_case（对齐 AIGX 既有 `CheckinSchedule` 序列化与前端
    // `types/index.ts` 的读取点；camelCase 会让整批字段静默读成 undefined）。
    json!({
        "id": schedule.id,
        "label": schedule.label,
        "display_label": super::schedules::display_label(schedule),
        "enabled": schedule.enabled,
        "time": schedule.time,
        "time_end": schedule.time_end,
        "randomize": schedule.randomize,
        "providers": schedule.providers,
        "provider_times": schedule.provider_times,
        "provider_rows": rows,
        // A2A 叫 retryTime（补签时刻）；AIGX 既有字段名是 makeup_time
        "retry_time": schedule.makeup_time,
        "makeup_time": schedule.makeup_time,
        "retry_gap_minutes": schedule.retry_gap_minutes,
        "retry_limit": schedule.retry_limit,
        "last_fired_date": schedule.last_fired_date,
        "last_fired_today": fired_today,
        "last_fired_providers": schedule.last_fired_providers,
        "last_retried_providers": schedule.last_retried_providers,
        "pending_providers": pending,
        "next_run_at": if master && schedule.enabled {
            super::scheduler::next_run_at(schedule, today)
        } else {
            None
        },
        "last_result": schedule.last_result,
        "last_ok": schedule.last_ok,
        "last_keepalive": schedule.last_keepalive,
        "last_failed": schedule.last_failed,
    })
}

/// 自动签到状态（对齐 A2A `GET /api/auto-checkin` 与 `/api/checkin-schedules` 的 GET）。
///
/// 顶层是**兼容视图**（默认条目的投影：`time` / `providers` / `lastResult` …），
/// `schedules` 是完整条目数组。前端两套页面（旧的单条目视图 + 新的多条目视图）
/// 都能直接用同一份响应。
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
            .filter_map(|entry| entry.get("next_run_at").and_then(Value::as_i64))
            .min()
    } else {
        None
    };
    let default_entry = schedules.first();
    let options: Vec<Value> = provider_options(sessions)
        .into_iter()
        .map(|id| {
            json!({
                "id": id,
                "label": crate::model::manage::provider_label(&id),
                "builtin_time": super::scheduler::PROVIDER_TIMES
                    .iter()
                    .find(|(pid, _)| *pid == id)
                    .map(|(_, time)| Value::from(*time))
                    .unwrap_or(Value::Null),
            })
        })
        .collect();
    json!({
        "enabled": master,
        // 兼容字段（默认条目的投影；语义与改造前的顶层字段一致）
        "time": default_entry.map(|s| s.time.clone()).unwrap_or_else(|| "00:01".to_string()),
        "time_end": default_entry.map(|s| s.time_end.clone()).unwrap_or_default(),
        "providers": default_entry.map(|s| s.providers.clone()).unwrap_or_default(),
        "last_fired_date": default_entry.map(|s| s.last_fired_date.clone()).unwrap_or_default(),
        "last_fired_today": default_entry
            .map(|s| super::scheduler::pending_providers(s, &today).is_empty())
            .unwrap_or(false),
        "last_result": default_entry.map(|s| s.last_result.clone()).unwrap_or_default(),
        "next_run_at": next_run_at,
        "provider_options": options,
        "default_retry_time": DEFAULT_RETRY_TIME,
        "schedules": entries,
        "running": is_running(),
        // 签到设置（间隔 / 时区 / 保留期）—— 旧界面「设置」卡直接读这一块
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
    drop(guard);
    Some(outcome)
}

/// 聚合快照里的 `campaigns` 行（活动 / 月度赠送 / 套餐）。
///
/// AIGX 目前只有 ZCode 两地的「限时套餐」一条活动链路（`benefits::zcode_claim`），
/// 本地领取台账落在 `benefit:zcode_plan:{account_id}` 的 `claimed` 数组里。
/// 形状对齐 A2A `CampaignEntry`（`{id, name, provider, available, claimPlans}`），
/// 没有活动链路的家**不出现**（空集但形状不变）。
pub fn campaign_rows(store: &CheckinStore, sessions: &AccountSessionStore) -> Vec<Value> {
    let _ = store;
    sessions
        .all()
        .into_iter()
        .filter(|session| matches!(session.provider.as_str(), "zcode" | "zcode-intl"))
        .map(|session| {
            // 本地领取台账：`{planId: 毫秒}`（形状对齐 A2A `claimPlans`）
            let memory = crate::checkin::benefits::load_memory(
                store.file_store(),
                &crate::checkin::benefits::memory_key(&session.id, "zcode_plan"),
            )
            .unwrap_or(Value::Null);
            let claimed_at = memory
                .get("claimed_at")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let claim_plans: serde_json::Map<String, Value> = memory
                .get("claimed")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            let id = item
                                .as_str()
                                .or_else(|| item.get("id").and_then(Value::as_str))?;
                            Some((id.to_string(), Value::from(claimed_at)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "id": session.id,
                "name": session.name,
                "provider": session.provider,
                "available": session.is_enabled(),
                "claim_at": if claimed_at > 0 { Value::from(claimed_at) } else { Value::Null },
                "claim_plans": Value::Object(claim_plans),
            })
        })
        .collect()
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

/// 一条账号的签到状态行（签到中心「每日签到」分组用）。
///
/// 与 `checkin_center::handle_checkin_center` 的行形状保持一致：`checkedInToday`
/// 读台账当日末条记录，`streakDays` 读签到后回读落盘的连登天数。
pub fn account_row(
    store: &CheckinStore,
    session: &crate::account_session::model::AccountSession,
) -> Value {
    let last = store
        .last_today(&session.id, &session.provider)
        .ok()
        .flatten();
    // 与签到中心的账号行同口径：snake_case（前端 `CheckinAccountRow` 逐字段对应）
    json!({
        "id": session.id,
        "name": session.name,
        "channel_id": session.channel_id,
        "available": session.is_enabled(),
        "checked_in_today": last.as_ref().map(|r| r.ok).unwrap_or(false),
        "last_outcome": last.as_ref().map(|r| r.outcome.as_str()),
        "last_message": last.as_ref().map(|r| r.message.clone()),
        "last_ts": last.as_ref().map(|r| r.ts),
        "streak_days": store.streak_days(&session.id),
        // 当日签到台账（对齐 A2A dailyCheckin；只有 ok/already 才带 claim_at）
        "daily_checkin": store
            .daily_checkin(&session.id, &scheduler::today())
            .unwrap_or(Value::Null),
    })
}

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
    fn auto_state_shape_has_compat_and_schedules() {
        // 只断言「字段存在且类型正确」，**不断言 running 的具体值** ——
        // running 是进程级全局闸，并行测试下由别的用例持有，断言值会 flaky。
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        let sessions = AccountSessionStore::new(temp_store());
        let state = auto_state(&store, &sessions);
        // 兼容字段存在
        assert!(state.get("enabled").is_some());
        assert!(state.get("time").is_some());
        assert!(state.get("provider_options").is_some());
        assert!(state.get("schedules").is_some());
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
