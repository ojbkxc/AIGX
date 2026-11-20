// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 定时签到的**多条目**配置（对齐 Agent2API `core/checkin_schedules.rs`）。
//!
//! ## 形状
//!
//! 每条条目是一次「区间签到」的完整配置：
//! `{id, label, enabled, time, time_end, providers, provider_times, retry_time,
//!   retry_gap_minutes, retry_limit, last_fired_date, last_fired_providers,
//!   last_retried_providers, account_plan, last_result, last_ok, last_keepalive, last_failed}`
//!
//! - `id`        稳定标识（界面按它 upsert / 删除 / 立即执行）
//! - `time` / `time_end` HH:MM，同一自然日区间；相同表示固定时刻
//! - `provider_times` 逐家区间起点覆盖（`{providerId: "HH:MM"}`；空串 = 撤销覆盖）
//! - `retry_time` 补签不早于的时刻（缺省 00:10；空串 = 关掉补签轮）
//! - `retry_gap_minutes` 中性未领取的账号多久后重试（缺省 10；0 = 不重试）
//! - `retry_limit` 每账号每天最多重试几次（缺省 3；0 = 不重试）
//! - `account_plan` 当天逐账号的随机时刻和尝试标记，重启后沿用
//!
//! ## 为什么与 A2A 的存储落点不同
//!
//! A2A 把条目存在 config.json 的 `autoCheckin.schedules` 下；AIGX 的签到配置
//! 已经在 `CheckinStore`（`checkin:schedules` KV）里有一份 `Vec<CheckinSchedule>`
//! —— 沿用同一份存储、同一个 `CheckinStore` 句柄，不引入第二个落点。
//! 本模块只补**入口层**：把 A2A 的 `upsert` / `remove` 校验规则（时刻必填、
//! 至少一家、条数上限、逐家时刻归一）搬过来，交给 `CheckinStore` 落盘。
//!
//! ## 存量兼容
//!
//! `schedules` 为空时（老配置只有全局开关 + 单个时刻）不影响：调度器本来就是
//! 「按条目列表逐条跑」，空列表 = 不跑定时（与改造前一致 —— 改造前也没有条目）。

use std::collections::HashMap;

use serde_json::Value;

use super::claim;
use super::store::{CheckinSchedule, CheckinStore};

/// 条目上限：再多列表难以扫读，定点签到的语义也不需要更多（对齐 A2A）。
pub const MAX_SCHEDULES: usize = 10;
/// 中性未领取的重试间隔上限（分钟）：再长就跨过下一个窗口了。
pub const MAX_RETRY_GAP_MINUTES: i64 = 120;
/// 每账号每天的重试次数上限（对齐 A2A）。
pub const MAX_RETRY_LIMIT: i64 = 20;
/// 补签不早于的缺省时刻（对齐 A2A `DEFAULT_RETRY_TIME`）。
pub const DEFAULT_RETRY_TIME: &str = "00:10";

/// 时刻格式校验（`HH:MM`，与 `scheduler::is_hhmm` 同口径）。
fn is_hhmm(value: &str) -> bool {
    super::scheduler::is_hhmm(value)
}

/// 归一一个可空 HH:MM：空串保留为空（表达「撤销/关闭」），非空必须合法。
fn normalize_hhmm(value: &str, field: &str) -> Result<String, String> {
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() || is_hhmm(&trimmed) {
        Ok(trimmed)
    } else {
        Err(format!("{field}格式必须是 HH:MM（如 09:00）"))
    }
}

/// 数值字段的归一：`None` = 继承存量（调用方给 `fallback`）；`Some` 夹到 `[0, max]`。
fn number_or(raw: Option<i64>, fallback: i64, max: i64) -> i64 {
    raw.map(|value| value.clamp(0, max)).unwrap_or(fallback)
}

/// 逐家时刻覆盖的归一（只认签到白名单里的家；空串保留 = 撤销覆盖）。
pub fn normalize_provider_times(value: Option<&Value>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(object) = value.and_then(Value::as_object) else {
        return map;
    };
    for (provider, raw) in object {
        if !claim::supports_checkin(provider) {
            continue;
        }
        let text = raw.as_str().map(str::trim).unwrap_or("");
        if text.is_empty() {
            map.insert(provider.clone(), String::new());
        } else if is_hhmm(text) {
            map.insert(provider.clone(), text.to_string());
        }
    }
    map
}

/// 归一 `retry_time`：缺失 → 缺省；空串 → 关闭；非法 → 缺省。
fn normalize_retry_time(value: Option<&Value>) -> String {
    match value {
        None => DEFAULT_RETRY_TIME.to_string(),
        Some(Value::String(text)) if text.trim().is_empty() => String::new(),
        Some(raw) => raw
            .as_str()
            .map(str::trim)
            .filter(|text| is_hhmm(text))
            .map(str::to_string)
            .unwrap_or_else(|| DEFAULT_RETRY_TIME.to_string()),
    }
}

/// 保存一条条目（按 `id` upsert）。校验与归一规则对齐 A2A `checkin_schedules::upsert`：
///
/// - `time` 必填且合法（`HH:MM`）；
/// - `providers` 至少勾一家（只认签到白名单，按白名单顺序归一）；
/// - 条目数上限 [`MAX_SCHEDULES`]；
/// - `time_end` 合法且不早于 `time`（暂不支持跨天）；
/// - **未传的字段继承存量**：编辑别家字段不该把覆盖 / 窗口 / 重试设置清掉，
///   更不该把上次执行状态清空（空 `last_fired_date` 会让调度器当天重复触发）。
///
/// 成功返回落盘后的完整条目列表（后端归一之后的值，界面以后端为准）。
pub fn upsert(store: &CheckinStore, payload: &Value) -> Result<Vec<CheckinSchedule>, String> {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("schedule-{}", chrono::Utc::now().timestamp_millis()));
    let time = payload
        .get("time")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if !is_hhmm(time) {
        return Err("时刻格式必须是 HH:MM（如 09:00）".to_string());
    }
    // 范围：只认注册过的提供商（去重、按注册顺序）
    let picked: Vec<String> = claim::CHECKIN_PROVIDERS
        .iter()
        .filter(|id| {
            payload
                .get("providers")
                .and_then(Value::as_array)
                .map(|items| items.iter().any(|item| item.as_str() == Some(**id)))
                .unwrap_or(false)
        })
        .map(|id| id.to_string())
        .collect();
    if picked.is_empty() {
        return Err("至少勾选一家签到提供商".to_string());
    }
    let label = payload
        .get("label")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();

    let existing = store.schedules().into_iter().find(|s| s.id == id);
    let is_new = existing.is_none();
    if is_new && store.schedules().len() >= MAX_SCHEDULES {
        return Err(format!("定时条目最多 {MAX_SCHEDULES} 条"));
    }
    let old = existing.as_ref();

    // 未传字段继承存量（新建给缺省）
    let enabled = match payload.get("enabled").and_then(Value::as_bool) {
        Some(flag) => flag,
        None => old.map(|s| s.enabled).unwrap_or(true),
    };
    let time_end = match payload.get("time_end").or_else(|| payload.get("timeEnd")) {
        Some(raw) => normalize_hhmm(raw.as_str().unwrap_or(""), "窗口结束时刻")?,
        None => old.map(|s| s.time_end.clone()).unwrap_or_default(),
    };
    if !time_end.is_empty() && time_end.as_str() < time {
        return Err("签到区间结束时间不能早于开始时间；暂不支持跨天区间".to_string());
    }
    let randomize = payload
        .get("randomize")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| old.map(|s| s.randomize).unwrap_or(false));
    if randomize && time_end.is_empty() {
        return Err("开启区间随机时必须给出窗口结束时刻".to_string());
    }
    let provider_times = match payload
        .get("provider_times")
        .or_else(|| payload.get("providerTimes"))
    {
        Some(value) => normalize_provider_times(Some(value)),
        None => old.map(|s| s.provider_times.clone()).unwrap_or_default(),
    };
    // 补签时刻：A2A 叫 `retryTime`，AIGX 既有前端叫 `makeup_time` —— 两种键都认。
    let retry_time = match payload
        .get("retry_time")
        .or_else(|| payload.get("retryTime"))
        .or_else(|| payload.get("makeup_time"))
        .or_else(|| payload.get("makeupTime"))
    {
        Some(value) => normalize_retry_time(Some(value)),
        None => old
            .map(|s| s.makeup_time.clone())
            .unwrap_or_else(|| DEFAULT_RETRY_TIME.to_string()),
    };
    let retry_gap_minutes = number_or(
        payload
            .get("retry_gap_minutes")
            .or_else(|| payload.get("retryGapMinutes"))
            .and_then(Value::as_i64),
        old.map(|s| s.retry_gap_minutes as i64).unwrap_or(10),
        MAX_RETRY_GAP_MINUTES,
    ) as u32;
    let retry_limit = number_or(
        payload
            .get("retry_limit")
            .or_else(|| payload.get("retryLimit"))
            .and_then(Value::as_i64),
        old.map(|s| s.retry_limit as i64).unwrap_or(3),
        MAX_RETRY_LIMIT,
    ) as u32;

    // 运行状态一律从存量继承（编辑配置不该重置去重状态 / 上次结果）
    let schedule = CheckinSchedule {
        id: id.clone(),
        label,
        enabled,
        time: time.to_string(),
        time_end,
        randomize,
        retry_limit,
        retry_gap_minutes,
        // makeup_time 是 AIGX 既有字段名（前端沿用）；值来自 retry_time
        makeup_time: retry_time,
        providers: picked,
        provider_times,
        last_fired_date: old.map(|s| s.last_fired_date.clone()).unwrap_or_default(),
        last_fired_providers: old
            .map(|s| s.last_fired_providers.clone())
            .unwrap_or_default(),
        last_retried_providers: old
            .map(|s| s.last_retried_providers.clone())
            .unwrap_or_default(),
        account_plan: old.map(|s| s.account_plan.clone()).unwrap_or_default(),
        last_result: old.map(|s| s.last_result.clone()).unwrap_or_default(),
        last_summary: old.map(|s| s.last_summary.clone()).unwrap_or(Value::Null),
        last_ok: old.map(|s| s.last_ok).unwrap_or(0),
        last_keepalive: old.map(|s| s.last_keepalive).unwrap_or(0),
        last_failed: old.map(|s| s.last_failed).unwrap_or(0),
    };
    store
        .upsert_schedule(schedule)
        .map_err(|e| format!("保存签到计划失败：{e}"))
}

/// 删除一条条目（返回剩余列表）。不存在的 id 静默成功（幂等删除）。
pub fn remove(store: &CheckinStore, id: &str) -> Result<Vec<CheckinSchedule>, String> {
    store
        .remove_schedule(id)
        .map_err(|e| format!("删除签到计划失败：{e}"))
}

/// 条目的展示名（label 为空退到 time，对齐 A2A `display_label`）。
pub fn display_label(schedule: &CheckinSchedule) -> &str {
    if schedule.label.is_empty() {
        &schedule.time
    } else {
        &schedule.label
    }
}

/// 逐家生效时刻的视图（界面每行标注「这家几点签 / 今天跑过没有」）。
///
/// 返回 A2A `ScheduleProviderRow` 形状：
/// `[{id,label,time,timeEnd,builtin,firedToday,accountCount,attemptedCount,
///   pendingCount,nextAccountAt,overdue}]`。
pub fn provider_rows(_store: &CheckinStore, schedule: &CheckinSchedule, today: &str) -> Vec<Value> {
    // 当日排程（跨天的旧排程不算数 —— 与调度判定同一口径）
    let plan = (schedule.account_plan.date == today).then_some(&schedule.account_plan);
    let now_ms = chrono::Utc::now().timestamp_millis();
    schedule
        .providers
        .iter()
        .map(|id| {
            let time = super::scheduler::effective_time(schedule, id);
            let (_, end) = super::scheduler::effective_window(schedule, id);
            let fired = schedule
                .last_fired_providers
                .get(id)
                .is_some_and(|date| date == today);
            let accounts: Vec<&super::store::PlannedAccount> = plan
                .as_ref()
                .map(|plan| {
                    plan.accounts
                        .values()
                        .filter(|account| &account.provider == id)
                        .collect()
                })
                .unwrap_or_default();
            let pending: Vec<&&super::store::PlannedAccount> = accounts
                .iter()
                .filter(|account| account.attempted_at.is_none())
                .collect();
            let pending_count = pending.len();
            let next_account_at = pending.iter().map(|account| account.at).min();
            let overdue = pending.iter().any(|account| account.at <= now_ms);
            let account_count = accounts.len();
            serde_json::json!({
                "id": id,
                "label": crate::model::manage::provider_label(id),
                "time": time,
                "timeEnd": end,
                "builtin": builtin_time(id),
                // 今天跑过且没有待执行账号 = 完成；有排程未到点 = 等窗口
                "firedToday": fired && pending_count == 0,
                "accountCount": account_count,
                "attemptedCount": account_count - pending_count,
                "pendingCount": pending_count,
                "nextAccountAt": next_account_at,
                // 到点了但还没跑（false 且 firedToday=false = 在等窗口）
                "overdue": overdue,
            })
        })
        .collect()
}

/// 逐家的补签时刻（对齐 A2A `times::provider_retry_time`）：取
/// `max(补签时刻, 主签到窗口结束 + 重试间隔)`，不早于补签时刻；越过当天
/// 24:00 的不出现（那家当天没有补签窗口）。
pub fn provider_retry_times(schedule: &CheckinSchedule) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    if schedule.makeup_time.trim().is_empty() {
        return map;
    }
    let Some(base) = minutes_of(&schedule.makeup_time) else {
        return map;
    };
    // 间隔被配成 0（关掉重试）时仍给 1 分钟 —— 补签轮与「等窗口重试」是两件事
    let gap = schedule.retry_gap_minutes.max(1) as i64;
    for provider in &schedule.providers {
        let (start, end) = super::scheduler::effective_window(schedule, provider);
        // 无窗口（未开随机 / 有专属时刻）时「主轮结束」= 生效时刻
        let main_time = end.unwrap_or(start);
        let Some(main) = minutes_of(&main_time) else {
            continue;
        };
        let target = base.max(main + gap);
        if target < 1440 {
            map.insert(provider.clone(), Value::from(format_minutes(target)));
        }
    }
    map
}

/// `HH:MM` → 当天分钟数。
fn minutes_of(value: &str) -> Option<i64> {
    if !super::scheduler::is_hhmm(value) {
        return None;
    }
    let hour = value[0..2].parse::<i64>().ok()?;
    let minute = value[3..5].parse::<i64>().ok()?;
    Some(hour * 60 + minute)
}

/// 分钟数 → `HH:MM`。
fn format_minutes(minutes: i64) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// 内置专属时刻（qoder 10:01 / zcode 19:01），没有则 null。
fn builtin_time(provider: &str) -> Value {
    super::scheduler::PROVIDER_TIMES
        .iter()
        .find(|(id, _)| *id == provider)
        .map(|(_, time)| Value::from(*time))
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_store() -> Arc<crate::storage::FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-sched-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(crate::storage::FileStore::new(dir))
    }

    fn store() -> CheckinStore {
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        store
    }

    #[test]
    fn upsert_validates_time_and_providers() {
        let s = store();
        // 非法时刻
        assert!(upsert(
            &s,
            &serde_json::json!({"time": "9:00", "providers": ["loomy"]})
        )
        .is_err());
        // 一家都没勾
        assert!(upsert(&s, &serde_json::json!({"time": "09:00", "providers": []})).is_err());
        // 合法
        let list = upsert(
            &s,
            &serde_json::json!({"time": "09:00", "providers": ["loomy", "kuku"]}),
        )
        .unwrap();
        assert_eq!(list.len(), 1);
        // providers 按白名单顺序归一
        assert_eq!(
            list[0].providers,
            vec!["loomy".to_string(), "kuku".to_string()]
        );
        assert_eq!(list[0].makeup_time, DEFAULT_RETRY_TIME);
    }

    #[test]
    fn upsert_inherits_unset_fields_and_run_state() {
        let s = store();
        let list = upsert(
            &s,
            &serde_json::json!({"id": "a", "time": "09:00", "providers": ["loomy"]}),
        )
        .unwrap();
        let id = list[0].id.clone();
        s.mark_fired(&id, "2026-11-16", "成功 1/1", &Default::default())
            .unwrap();
        // 只改时刻：providers / 重试 / 上次结果都必须继承
        let list = upsert(
            &s,
            &serde_json::json!({"id": id, "time": "10:30", "providers": ["loomy"]}),
        )
        .unwrap();
        assert_eq!(list[0].time, "10:30");
        assert_eq!(list[0].last_fired_date, "2026-11-16");
        assert_eq!(list[0].retry_limit, 3);
    }

    #[test]
    fn provider_times_override_and_revoke() {
        let s = store();
        let list = upsert(
            &s,
            &serde_json::json!({
                "time": "09:00",
                "providers": ["qoder", "loomy"],
                "provider_times": {"qoder": "08:30", "loomy": ""}
            }),
        )
        .unwrap();
        assert_eq!(
            list[0].provider_times.get("qoder"),
            Some(&"08:30".to_string())
        );
        assert_eq!(list[0].provider_times.get("loomy"), Some(&String::new()));
        // 非白名单被丢弃
        let list = upsert(
            &s,
            &serde_json::json!({
                "time": "09:00",
                "providers": ["loomy"],
                "provider_times": {"unknown-x": "08:30"}
            }),
        )
        .unwrap();
        // 语义等价于 get(...).is_none()，但 contains_key 表达「不该被写入」更直接
        assert!(!list[0].provider_times.contains_key("unknown-x"));
    }

    #[test]
    fn retry_limit_zero_disables_and_clamps() {
        let s = store();
        let list = upsert(
            &s,
            &serde_json::json!({"time": "09:00", "providers": ["loomy"], "retry_limit": 999}),
        )
        .unwrap();
        assert_eq!(list[0].retry_limit, MAX_RETRY_LIMIT as u32);
        let list = upsert(
            &s,
            &serde_json::json!({"id": list[0].id, "time": "09:00", "providers": ["loomy"], "retry_limit": 0}),
        )
        .unwrap();
        assert_eq!(list[0].retry_limit, 0);
    }

    #[test]
    fn remove_is_idempotent() {
        let s = store();
        upsert(
            &s,
            &serde_json::json!({"time": "09:00", "providers": ["loomy"]}),
        )
        .unwrap();
        let list = remove(&s, "no-such-id").unwrap();
        assert_eq!(list.len(), 1);
    }
}
