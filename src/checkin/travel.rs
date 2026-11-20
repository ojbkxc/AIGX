// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 猫猫旅行巡检排程（对齐上游 `core/growth_scheduler.rs` + `api/growth_schedule.rs`）。
//!
//! ## 与每日签到的关系
//!
//! 旅行与签到是**两个独立排程**（上游 `travel_hours` / `checkin_hours` 两套
//! 开关与时刻）：它们服务不同玩法，用户可能只想自动签到、不想自动派猫。
//! 合成一组会让「关掉旅行」变成「关掉签到」的副作用 —— 所以配置单独一格
//! （`checkin:travel`），调度循环单独一个，只是随签到调度一起启动。
//!
//! ## 执行体在插件里
//!
//! 任务树 / 旅行是 WorkBuddy 的运营玩法，按项目的风险隔离要求这类实现不进
//! 主仓：网关只保留配置面与调度面，执行走提供商插件的 `admin.action`
//!（`action = "growth.travel"`，与上游 `api/growth.rs` 的
//! `call_action("growth.travel")` 同名）。插件由 [`TravelExecutor`] 抽象，
//! API 层给 `AppState` 的实现（原生包 → 脚本宿主 → Wasm 沙箱）。
//!
//! ## 调度口径（与上游 growth_scheduler 逐条对齐）
//!
//! - 按**时刻数组**跑：每个时刻每天至多一轮，去重标记按**计划时刻**落
//!   （不是实际执行时刻 —— 到点后最多晚一个 tick 执行，两者永不相等）。
//! - 每轮只推进一步：多个时刻同时过期时逐轮补跑，下个 tick 再补下一条。
//! - 失败保留原因：`last_result` / `last_error` 如实落「失败：…」，但该计划
//!   时刻仍记为已跑 —— 每天定点的动作不应因失败变成每 30 秒的高频重试。
//! - 能力不可用（插件未装/未启用）返回中性跳过，**不落任何标记**：插件装好
//!   后的下一轮会补跑当天错过的时刻。
//! - 手动执行不占用当天的定时轮次（不写 `fired_times`）。
//!
//! ## 偏离（如实记录）
//!
//! - 插件在 **RPC 成功** 但 summary 里自报 `outcome=failed` 时，本模块按
//!   失败处理并把原因透出（上游会把它记成 completed）。「不伪造已巡检」优先。
//! - 上游默认时刻来自插件的 `schedules.list` 申报；AIGX 暂无该申报通道，
//!   缺省时刻表为空（开配置不写时刻 ⇔ 无定时轮次，`paused_reason` 如实提示）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::scheduler;
use super::store::CheckinStore;
#[cfg(test)]
use crate::storage::FileStore;

/// 配置键（沿用 `checkin:*` 既有风格）。
pub const TRAVEL_KEY: &str = "checkin:travel";

/// 每天时刻上限（与上游 `schedules::MAX_TIMES` 同值）。
pub const MAX_TIMES: usize = 6;

/// 轮询间隔：与上游 `growth_scheduler::TICK_MS` 同值（到点后最多晚 30 秒）。
pub const TICK: Duration = Duration::from_secs(30);

/// 插件执行动作名（上游 `api/growth.rs` 的 `call_action("growth.travel")`）。
pub const ACTION: &str = "growth.travel";

/// 旅行巡检的目标 provider（上游 `growth_scheduler::PROVIDER`）。
/// 只有国内版 WorkBuddy 有成长中心 / 猫猫旅行。
pub const TRAVEL_PROVIDER: &str = "workbuddy";

/// 旅行配置（`checkin:travel` 一格的落盘形状）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TravelConfig {
    /// 自动巡检开关
    pub enabled: bool,
    /// 每天执行时刻（HH:MM，已归一、去重、升序）
    pub times: Vec<String>,
    /// 上次触发日期（YYYY-MM-DD，跨天自动作废当日去重）
    pub last_fired_date: String,
    /// 今天已跑过的**计划时刻**（界面据此把已执行条目标灰）
    pub fired_times: Vec<String>,
    /// 上次实际执行时刻（HH:MM，展示用；不参与去重）
    pub last_fired_time: String,
    /// 上次结果一句话摘要（成功或失败原因，界面直读）
    pub last_result: String,
    /// 上次结果分类：completed / failed（空 = 从未跑过）
    pub last_outcome: String,
    /// 上次执行的 unix 秒
    pub last_run: Option<i64>,
    /// 上次失败原文（成功时清空）
    pub last_error: String,
}

impl TravelConfig {
    /// 读入归一：时刻只留合法值（过滤脏数据），去重、升序、截断上限。
    fn normalize(&mut self) {
        let mut times: Vec<String> = Vec::new();
        for item in &self.times {
            if let Some(normalized) = normalize_time(item) {
                if !times.contains(&normalized) {
                    times.push(normalized);
                }
            }
        }
        times.sort();
        times.truncate(MAX_TIMES);
        self.times = times;
        let mut fired: Vec<String> = Vec::new();
        for item in &self.fired_times {
            if let Some(normalized) = normalize_time(item) {
                if !fired.contains(&normalized) {
                    fired.push(normalized);
                }
            }
        }
        fired.sort();
        self.fired_times = fired;
    }
}

/// 时刻归一化为 `HH:MM`（与上游 `schedules::normalize_time` 同语义：
/// 允许 `9:5` → `09:05`，越界/带杂字符/非数字一律 `None`）。
pub fn normalize_time(text: &str) -> Option<String> {
    let (hour, minute) = text.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

/// 插件执行体抽象：API 层为 `AppState` 实现（原生包 → 脚本宿主 → Wasm），
/// 单测注入替身。`pause_reason` 必须是**同步**的（调度每 tick 先判可用性，
/// 也供 GET 载荷的 `paused_reason` 直读）。
/// `double_must_use`：`async_trait` 宏给每个方法的返回值包一层 pinned future
///（本身已是 must_use），再叠加一层 `#[must_use]` 会被 clippy 判为重复。
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait TravelExecutor: Send + Sync {
    /// `None` = 能力可用；`Some(原因)` = 不可用（暂停原因，界面直读）。
    fn pause_reason(&self) -> Option<String>;
    /// 执行一轮巡检。返回插件 summary（形状由插件定义，本模块按
    /// `actions` / `accounts` / `credit` / `failed` 尽力解析）。
    async fn run(&self) -> Result<Value, TravelActionError>;
}

/// 插件执行错误（保留 code 供 handler 映射：`unsupported` → 501）。
#[derive(Debug, Clone)]
pub struct TravelActionError {
    pub message: String,
    pub code: Option<String>,
}

/// 一轮执行的结果（与上游 `RunOutcome` 同四分法）。
pub enum RunOutcome {
    Completed {
        summary: String,
        ok: usize,
        failed: usize,
    },
    /// 能力不可用 / 插件未申报 —— 中性跳过，不落标记
    Skipped(String),
    /// 已有一轮在执行中
    Busy,
    Failed(TravelActionError),
}

// ─── 配置读写（单写者锁：PATCH 与执行回写串行）────────────────────

fn config_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    &LOCK
}

/// 读配置（归一后；缺失/损坏给默认值）。
pub fn read(store: &CheckinStore) -> TravelConfig {
    let mut config = store
        .file_store()
        .get::<TravelConfig>(TRAVEL_KEY)
        .ok()
        .flatten()
        .unwrap_or_default();
    config.normalize();
    config
}

fn write(store: &CheckinStore, config: &TravelConfig) -> Result<(), String> {
    store
        .file_store()
        .put(TRAVEL_KEY, config)
        .map_err(|e| format!("保存巡检配置失败：{e}"))
}

// ─── 调度判定 ───────────────────────────────────────────────

/// 当天该跑的时刻里还有没跑过的 → `Some(那一条)`，否则 `None`。
///
/// 取「已到点且今天没跑过」里最早的一条：多个时刻同时过期时逐轮补跑
///（一轮只跑一次，下个 tick 再补下一条 —— 与上游同语义）。
pub fn due_time(config: &TravelConfig, today: &str, now_hhmm: &str) -> Option<String> {
    if !config.enabled || config.times.is_empty() {
        return None;
    }
    let fired: &[String] = if config.last_fired_date == today {
        &config.fired_times
    } else {
        &[]
    };
    config
        .times
        .iter()
        .find(|time| {
            time.as_str() <= now_hhmm && !fired.iter().any(|item| item.as_str() == time.as_str())
        })
        .cloned()
}

/// 当前时刻（按签到配置的时区解释，与签到计划同一坐标系）。
fn now_local() -> (String, String) {
    let now = chrono::Utc::now().with_timezone(&scheduler::beijing());
    (
        now.format("%Y-%m-%d").to_string(),
        now.format("%H:%M").to_string(),
    )
}

// ─── 运行状态（跨进程内全局，与上游 running 同构）──────────────

fn running_flag() -> &'static AtomicBool {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    &RUNNING
}

pub fn is_running() -> bool {
    running_flag().load(Ordering::SeqCst)
}

/// 抢「执行中」标记的守卫：Drop 时复位。
struct RunGuard;

impl Drop for RunGuard {
    fn drop(&mut self) {
        running_flag().store(false, Ordering::SeqCst);
    }
}

fn acquire() -> Option<RunGuard> {
    if running_flag().swap(true, Ordering::SeqCst) {
        return None;
    }
    Some(RunGuard)
}

// ─── 执行与落账 ─────────────────────────────────────────────

/// 落一轮结果。`plan_time` 有值 = 定时轮次（占坑去重）；手动执行传 `None`。
///
/// 「已跑时刻」按**计划时刻**落（不是实际执行时刻），否则 `due_time` 拿计划
/// 时刻永远查不到，每个 tick 都重跑 —— 上游源码注释里钉过的真实 bug。
fn record_run(
    store: &CheckinStore,
    today: &str,
    now_hhmm: &str,
    plan_time: Option<&str>,
    last_result: String,
    last_outcome: &str,
    last_error: &str,
) {
    let _guard = config_lock().lock();
    let mut config = read(store);
    if config.last_fired_date != today {
        config.last_fired_date = today.to_string();
        config.fired_times.clear();
    }
    if let Some(plan_time) = plan_time.filter(|text| !text.is_empty()) {
        if !config
            .fired_times
            .iter()
            .any(|item| item.as_str() == plan_time)
        {
            config.fired_times.push(plan_time.to_string());
            config.fired_times.sort();
        }
    }
    config.last_fired_time = now_hhmm.to_string();
    config.last_run = Some(chrono::Utc::now().timestamp());
    config.last_result = last_result;
    config.last_outcome = last_outcome.to_string();
    config.last_error = last_error.to_string();
    let _ = write(store, &config);
}

/// 插件 summary 的自报失败（RPC 成功但内容失败）→ 失败原因。
fn plugin_failed(summary: &Value) -> Option<String> {
    if summary.get("outcome").and_then(Value::as_str) == Some("failed") {
        return Some(
            summary
                .get("error")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("上游未返回失败原因")
                .to_string(),
        );
    }
    None
}

/// 插件 summary → 一句话摘要 + (ok, failed) 计数（缺字段尽力而为）。
fn summarize(summary: &Value) -> (String, usize, usize) {
    let actions = summary
        .get("actions")
        .and_then(Value::as_array)
        .map(Vec::len);
    let accounts = summary
        .get("accounts")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    let failed = summary.get("failed").and_then(Value::as_u64).unwrap_or(0) as usize;
    let credit = summary.get("credit").and_then(Value::as_i64).unwrap_or(0);
    let ok = actions.or(accounts).unwrap_or(0);
    let mut text = match actions {
        Some(count) => format!("共 {count} 个动作"),
        None => match accounts.filter(|count| *count > 0) {
            Some(count) => format!("巡检 {count} 个账号"),
            None => summary
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("已完成")
                .to_string(),
        },
    };
    if credit > 0 {
        text.push_str(&format!("，+{credit} 积分"));
    }
    if failed > 0 {
        text.push_str(&format!("，失败 {failed}"));
    }
    (text, ok, failed)
}

/// 跑一轮巡检。
///
/// 能力不可用 → [`RunOutcome::Skipped`]（不落标记）；真正失败 → 记失败原文
/// 并把计划时刻记为已跑（不做高频重试）；成功 → 记摘要与计数。
pub async fn run_once(
    store: &CheckinStore,
    executor: &dyn TravelExecutor,
    reason: &str,
    plan_time: Option<&str>,
) -> RunOutcome {
    if let Some(message) = executor.pause_reason() {
        return RunOutcome::Skipped(message);
    }
    let Some(_guard) = acquire() else {
        return RunOutcome::Busy;
    };
    let (today, now_hhmm) = now_local();
    tracing::info!("[Travel] 巡检开始（{reason}）");
    match executor.run().await {
        Ok(summary) => {
            if let Some(message) = plugin_failed(&summary) {
                record_run(
                    store,
                    &today,
                    &now_hhmm,
                    plan_time,
                    format!("失败：{message}"),
                    "failed",
                    &message,
                );
                tracing::warn!("[Travel] 巡检失败：{message}");
                return RunOutcome::Failed(TravelActionError {
                    message,
                    code: None,
                });
            }
            let (text, ok, failed) = summarize(&summary);
            record_run(
                store,
                &today,
                &now_hhmm,
                plan_time,
                text.clone(),
                "completed",
                "",
            );
            tracing::info!("[Travel] 巡检完成：{text}");
            RunOutcome::Completed {
                summary: text,
                ok,
                failed,
            }
        }
        Err(error) => {
            record_run(
                store,
                &today,
                &now_hhmm,
                plan_time,
                format!("失败：{}", error.message),
                "failed",
                &error.message,
            );
            tracing::warn!("[Travel] 巡检失败：{}", error.message);
            RunOutcome::Failed(error)
        }
    }
}

/// 手动执行：不占用当天的定时轮次（不写 `fired_times`）。
pub async fn run_now(store: &CheckinStore, executor: &dyn TravelExecutor) -> RunOutcome {
    run_once(store, executor, "手动执行", None).await
}

/// 对外状态（GET /api/travel-schedule 与 `/api/checkin-center` 的 `travel` 直读；
/// 零上游请求）。
///
/// 契约对齐 A2A `growth_scheduler::state()`（camelCase）；snake_case 别名保留
/// 一整版（旧 `/api/checkin-center/travel` 仍在服役）。
pub fn state(store: &CheckinStore, executor: &dyn TravelExecutor) -> Value {
    let config = read(store);
    let today = scheduler::today();
    let fired_today = if config.last_fired_date == today {
        config.fired_times.clone()
    } else {
        Vec::new()
    };
    let paused_reason = executor.pause_reason();
    let last_result = if config.last_result.is_empty() {
        Value::Null
    } else {
        Value::String(config.last_result.clone())
    };
    let last_outcome = if config.last_outcome.is_empty() {
        Value::Null
    } else {
        Value::String(config.last_outcome.clone())
    };
    let last_error = if config.last_error.is_empty() {
        Value::Null
    } else {
        Value::String(config.last_error.clone())
    };
    json!({
        "enabled": config.enabled,
        "available": paused_reason.is_none(),
        "pausedReason": paused_reason,
        "paused_reason": paused_reason,
        "times": config.times,
        "firedToday": fired_today,
        "fired_today": fired_today,
        "lastFiredDate": if config.last_fired_date.is_empty() {
            Value::Null
        } else {
            Value::String(config.last_fired_date.clone())
        },
        // 上次结果一句话摘要（A2A `lastResult` 是对象；AIGX 的摘要本身是一句话，
        // 如实给字符串，另带 `lastOutcome` 分类）
        "lastResult": last_result,
        "last_result": last_result,
        "lastOutcome": last_outcome,
        "last_outcome": last_outcome,
        "lastRun": config.last_run,
        "last_run": config.last_run,
        "lastError": last_error,
        "last_error": last_error,
        // 上次实际执行时刻（HH:MM，展示用；不参与去重）
        "lastFiredTime": if config.last_fired_time.is_empty() {
            Value::Null
        } else {
            Value::String(config.last_fired_time.clone())
        },
        "running": is_running(),
    })
}

/// 部分更新 `{enabled?, times?}`（只改传进来的字段）。
///
/// 校验口径对齐上游 `growth_scheduler::configure`：
/// - `enabled` 必须是布尔值；
/// - `times` 必须是时刻数组（≤ [`MAX_TIMES`]，逐项归一，去重升序）；
/// - 传了 `times` 就不允许为空（至少保留一个时刻 —— 想全停请关开关）；
/// - 时刻表变了 → 当日已跑标记作废（旧时刻的去重标记对新时刻无意义）。
pub fn patch(store: &CheckinStore, body: &Value) -> Result<TravelConfig, String> {
    let Some(object) = body.as_object() else {
        return Err("请求体必须是 JSON 对象".to_string());
    };
    if !object.contains_key("enabled") && !object.contains_key("times") {
        return Err("没有需要更新的字段（enabled / times）".to_string());
    }
    let _guard = config_lock().lock();
    let mut next = read(store);
    if let Some(value) = object.get("enabled") {
        let Some(enabled) = value.as_bool() else {
            return Err("enabled 必须是布尔值".to_string());
        };
        next.enabled = enabled;
    }
    if let Some(items) = object.get("times") {
        let Some(items) = items.as_array() else {
            return Err("times 必须是时刻数组".to_string());
        };
        if items.len() > MAX_TIMES {
            return Err(format!("时刻最多 {MAX_TIMES} 条"));
        }
        let mut times: Vec<String> = Vec::new();
        for item in items {
            let Some(normalized) = item.as_str().and_then(normalize_time) else {
                return Err(format!(
                    "非法时刻「{}」（应为 HH:MM，00:00 - 23:59）",
                    item.as_str().unwrap_or("?")
                ));
            };
            if !times.contains(&normalized) {
                times.push(normalized);
            }
        }
        if times.is_empty() {
            return Err("至少保留一个时刻".to_string());
        }
        times.sort();
        if times != next.times {
            next.last_fired_date.clear();
            next.fired_times.clear();
        }
        next.times = times;
    }
    write(store, &next)?;
    Ok(next)
}

/// 调度循环（由 `main.rs` 随签到调度一起起一次）。
///
/// 每 [`TICK`] 醒一次重新判定：改完设置下一轮就生效、机器休眠后自愈 ——
/// 与签到调度同一取舍。启动时先判一次（进程在计划时刻之后才启动时，
/// 当天还能补上；上游 growth_scheduler 首轮也要等到下个 tick，这里是
/// 有意为之的小改进）。
pub fn spawn(store: Arc<CheckinStore>, executor: Arc<dyn TravelExecutor>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    tokio::spawn(async move {
        loop {
            // 能力不可用时整轮跳过且不落标记：插件装好后下一轮补跑当天时刻
            if executor.pause_reason().is_none() {
                let config = read(&store);
                let (today, now_hhmm) = now_local();
                if let Some(plan_time) = due_time(&config, &today, &now_hhmm) {
                    let _ = run_once(
                        &store,
                        executor.as_ref(),
                        &format!("到点触发（{plan_time}）"),
                        Some(&plan_time),
                    )
                    .await;
                }
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

#[cfg(test)]
// 执行中标记是进程级静态：涉及执行的用例用 `serialize()` 串行化，guard 会
// 跨 `.await` 持有（测试内无并发执行者，语义安全）。
#[allow(clippy::await_holding_lock)]
mod tests {
    use super::*;

    fn temp_store() -> Arc<CheckinStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-checkin-travel-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(CheckinStore::new(Arc::new(FileStore::new(dir))))
    }

    /// 执行中标记是**进程级静态**：并行测试会互相干扰（一个测试持标记时
    /// 另一个测试的 run 会拿到 Busy），涉及执行的用例一律串行化。
    fn serialize() -> parking_lot::MutexGuard<'static, ()> {
        static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        LOCK.lock()
    }

    struct FakeExecutor {
        pause: Option<String>,
        result: Result<Value, TravelActionError>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeExecutor {
        fn ok(summary: Value) -> Self {
            Self {
                pause: None,
                result: Ok(summary),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                pause: None,
                result: Err(TravelActionError {
                    message: message.to_string(),
                    code: None,
                }),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn without_plugin(message: &str) -> Self {
            Self {
                pause: Some(message.to_string()),
                result: Ok(Value::Null),
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl TravelExecutor for FakeExecutor {
        fn pause_reason(&self) -> Option<String> {
            self.pause.clone()
        }
        async fn run(&self) -> Result<Value, TravelActionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    #[test]
    fn normalize_time_accepts_and_rejects() {
        assert_eq!(normalize_time("09:00"), Some("09:00".to_string()));
        assert_eq!(normalize_time(" 21:30 "), Some("21:30".to_string()));
        assert_eq!(normalize_time("9:5"), Some("09:05".to_string()));
        assert_eq!(normalize_time("00:00"), Some("00:00".to_string()));
        assert_eq!(normalize_time("23:59"), Some("23:59".to_string()));
        // 越界 / 带杂字符 / 空
        assert_eq!(normalize_time("24:00"), None);
        assert_eq!(normalize_time("12:60"), None);
        assert_eq!(normalize_time("09:00:00"), None);
        assert_eq!(normalize_time(""), None);
        assert_eq!(normalize_time("aa:bb"), None);
    }

    #[test]
    fn patch_rejects_invalid_times_and_empty_body() {
        let store = temp_store();
        // 空对象 = 没有字段可更新
        assert_eq!(
            patch(&store, &json!({})).unwrap_err(),
            "没有需要更新的字段（enabled / times）"
        );
        // enabled 非布尔
        assert_eq!(
            patch(&store, &json!({ "enabled": "yes" })).unwrap_err(),
            "enabled 必须是布尔值"
        );
        // times 非法格式：整体拒绝，不落半截配置
        assert!(patch(&store, &json!({ "times": ["09:00", "25:00"] }))
            .unwrap_err()
            .contains("非法时刻"));
        // times 非数组
        assert_eq!(
            patch(&store, &json!({ "times": "09:00" })).unwrap_err(),
            "times 必须是时刻数组"
        );
        // 空数组 = 「至少保留一个时刻」
        assert_eq!(
            patch(&store, &json!({ "times": [] })).unwrap_err(),
            "至少保留一个时刻"
        );
        // 超过上限
        assert!(patch(
            &store,
            &json!({ "times": ["00:01", "00:02", "00:03", "00:04", "00:05", "00:06", "00:07"] })
        )
        .unwrap_err()
        .contains("最多"));
        // 全部被拒后配置仍是默认值（没有半截写入）
        let config = read(&store);
        assert!(!config.enabled);
        assert!(config.times.is_empty());
    }

    #[test]
    fn patch_partial_update_keeps_other_fields_and_normalizes() {
        let store = temp_store();
        let config = patch(&store, &json!({ "times": ["21:00", "9:5", "21:00"] })).unwrap();
        // 归一：9:5 → 09:05；去重；升序
        assert_eq!(config.times, vec!["09:05".to_string(), "21:00".to_string()]);
        assert!(!config.enabled);
        // 只改 enabled：时刻原样保留
        let config = patch(&store, &json!({ "enabled": true })).unwrap();
        assert!(config.enabled);
        assert_eq!(config.times.len(), 2);
        // 落盘可回读
        let reread = read(&store);
        assert!(reread.enabled);
        assert_eq!(reread.times, vec!["09:05".to_string(), "21:00".to_string()]);
    }

    #[test]
    fn changing_times_resets_today_fired_marks() {
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        record_run(
            &store,
            &scheduler::today(),
            "09:00",
            Some("09:00"),
            "共 1 个动作".to_string(),
            "completed",
            "",
        );
        assert_eq!(read(&store).fired_times, vec!["09:00".to_string()]);
        // 换时刻表 = 旧标记作废
        patch(&store, &json!({ "times": ["10:00"] })).unwrap();
        let config = read(&store);
        assert!(config.fired_times.is_empty());
        assert!(config.last_fired_date.is_empty());
    }

    #[test]
    fn due_time_picks_earliest_unfired_past_slot() {
        let mut config = TravelConfig {
            enabled: true,
            times: vec![
                "09:00".to_string(),
                "13:00".to_string(),
                "21:00".to_string(),
            ],
            ..Default::default()
        };
        let today = "2026-11-17";
        // 现在 22:00、今天一个都没跑 → 先补最早的 09:00（一轮一条）
        assert_eq!(due_time(&config, today, "22:00").as_deref(), Some("09:00"));
        config.fired_times = vec!["09:00".to_string()];
        config.last_fired_date = today.to_string();
        assert_eq!(due_time(&config, today, "22:00").as_deref(), Some("13:00"));
        // 未到点的时刻不选（12:59 时 13:00 与 21:00 都还没到）
        assert_eq!(due_time(&config, today, "12:59"), None);
        assert_eq!(due_time(&config, today, "13:00").as_deref(), Some("13:00"));
        assert_eq!(due_time(&config, today, "08:59"), None);
        // 跨天：昨天的已跑标记作废，09:00 重新可跑
        assert_eq!(
            due_time(&config, "2026-11-18", "09:00").as_deref(),
            Some("09:00")
        );
        // 关掉开关 → 什么都不跑
        config.enabled = false;
        assert_eq!(due_time(&config, today, "22:00"), None);
    }

    /// 去重标记必须按**计划时刻**比对（上游源码钉过的真实 bug：按实际执行
    /// 时刻落标记会让每个 tick 都重跑一遍）。
    #[test]
    fn fired_slot_is_plan_time_not_actual_time() {
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        let today = scheduler::today();
        // 实际执行时刻是 09:01，但计划时刻 09:00 必须记成已跑
        record_run(
            &store,
            &today,
            "09:01",
            Some("09:00"),
            "共 1 个动作".to_string(),
            "completed",
            "",
        );
        let config = read(&store);
        assert_eq!(config.last_fired_time, "09:01");
        assert_eq!(config.fired_times, vec!["09:00".to_string()]);
        assert_eq!(due_time(&config, &today, "09:01"), None);
    }

    #[tokio::test]
    async fn run_now_without_plugin_is_stable_error_and_keeps_config() {
        let _serial = serialize();
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        let executor = FakeExecutor::without_plugin("未安装或未启用 WorkBuddy 提供商插件");
        match run_now(&store, &executor).await {
            RunOutcome::Skipped(message) => assert!(message.contains("插件")),
            _ => panic!("无插件时必须中性跳过而不是伪造结果"),
        }
        // 没执行、没落任何标记
        assert_eq!(executor.calls(), 0);
        let config = read(&store);
        assert!(config.last_result.is_empty());
        assert!(config.last_run.is_none());
        assert!(config.fired_times.is_empty());
    }

    #[tokio::test]
    async fn run_once_success_records_summary_and_slot() {
        let _serial = serialize();
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        let executor = FakeExecutor::ok(json!({
            "outcome": "completed",
            "accounts": 1,
            "actions": [{"kind": "adopt"}, {"kind": "dispatch"}],
            "credit": 5,
            "failed": 0,
        }));
        let today = scheduler::today();
        match run_once(&store, &executor, "测试", Some("09:00")).await {
            RunOutcome::Completed {
                summary,
                ok,
                failed,
            } => {
                assert!(summary.contains("2 个动作"), "summary: {summary}");
                assert!(summary.contains("+5 积分"), "summary: {summary}");
                assert_eq!(ok, 2);
                assert_eq!(failed, 0);
            }
            _ => panic!("应成功"),
        }
        let config = read(&store);
        assert_eq!(config.fired_times, vec!["09:00".to_string()]);
        assert_eq!(config.last_outcome, "completed");
        assert!(config.last_error.is_empty());
        assert!(config.last_result.contains("2 个动作"));
        // 占坑后同一时刻不再触发
        assert_eq!(due_time(&config, &today, "09:30"), None);
        // 手动执行不占定时轮次
        let mut config = read(&store);
        config.last_fired_date.clear();
        config.fired_times.clear();
        let _ = write(&store, &config);
        let _ = run_now(&store, &executor).await;
        assert!(read(&store).fired_times.is_empty());
    }

    #[tokio::test]
    async fn failure_keeps_reason_and_marks_slot_to_avoid_tight_retry() {
        let _serial = serialize();
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        let executor = FakeExecutor::failing("上游返回 HTTP 500");
        let today = scheduler::today();
        match run_once(&store, &executor, "测试", Some("09:00")).await {
            RunOutcome::Failed(error) => assert_eq!(error.message, "上游返回 HTTP 500"),
            _ => panic!("应失败"),
        }
        let config = read(&store);
        assert_eq!(config.last_outcome, "failed");
        assert_eq!(config.last_error, "上游返回 HTTP 500");
        assert!(config.last_result.starts_with("失败："));
        // 计划时刻已占坑 —— 下个 tick 不会每 30 秒重试同一失败
        assert_eq!(config.fired_times, vec!["09:00".to_string()]);
        assert_eq!(due_time(&config, &today, "09:01"), None);
    }

    #[tokio::test]
    async fn plugin_reported_failure_is_surfaced_not_disguised_as_completed() {
        let _serial = serialize();
        let store = temp_store();
        patch(&store, &json!({ "enabled": true, "times": ["09:00"] })).unwrap();
        let executor = FakeExecutor::ok(json!({
            "outcome": "failed",
            "error": "账号登录态失效",
            "accounts": 0,
            "actions": [],
            "credit": 0,
            "failed": 1,
        }));
        match run_now(&store, &executor).await {
            RunOutcome::Failed(error) => assert_eq!(error.message, "账号登录态失效"),
            _ => panic!("插件自报失败必须如实透出"),
        }
        let config = read(&store);
        assert_eq!(config.last_outcome, "failed");
        assert!(config.last_result.contains("账号登录态失效"));
    }

    #[tokio::test]
    async fn concurrent_run_reports_busy() {
        let _serial = serialize();
        let store = temp_store();
        // 先抢占 running 标记模拟一轮执行中
        let guard = acquire().expect("首次抢标记");
        let executor = FakeExecutor::ok(json!({}));
        match run_now(&store, &executor).await {
            RunOutcome::Busy => {}
            _ => panic!("执行中应报 Busy"),
        }
        drop(guard);
        assert!(matches!(
            run_now(&store, &executor).await,
            RunOutcome::Completed { .. }
        ));
    }

    #[test]
    fn state_view_matches_frontend_contract() {
        let _serial = serialize();
        let store = temp_store();
        patch(
            &store,
            &json!({ "enabled": true, "times": ["09:00", "21:00"] }),
        )
        .unwrap();
        let executor = FakeExecutor::without_plugin("未安装插件");
        let view = state(&store, &executor);
        // 字段名与前端 types/index.ts 的 CheckinTravelState 逐字对齐
        assert_eq!(view["enabled"], json!(true));
        assert_eq!(view["times"], json!(["09:00", "21:00"]));
        assert_eq!(view["available"], json!(false));
        assert_eq!(view["paused_reason"], json!("未安装插件"));
        assert_eq!(view["fired_today"], json!([]));
        assert_eq!(view["running"], json!(false));
        assert!(view["last_result"].is_null());
    }
}
