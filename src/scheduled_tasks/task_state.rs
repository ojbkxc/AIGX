//! 后台任务的**持久化排期**与执行占位（移植自 Agent2API `core/task_state.rs`）。
//!
//! ── 这个模块解决什么问题 ───────────────────────────────────
//! 后台任务的排期只活在内存里的话，进程一重启就清零，首轮又被排到「现在」，于是
//! **每次打开服务都会把上游打一遍** —— 反复重启就把 GitHub 的匿名限额、上游配额
//! 重复消耗。本模块把「上次尝试 / 上次成功 / 下次执行 / 失败冷却」落进 KV
//! （键 `backgroundTaskState`，存于 `FileStore`），于是：
//!   - **重启不重置节奏**：10:00 查过更新（间隔 20 分钟），10:05、10:10 反复
//!     重启都不再查，仍等到 10:20；
//!   - **停机期间只补一次**：休眠 / 关机错过的那一轮在启动后跑一次，不补跑期间
//!     的每一轮；
//!   - **失败有冷却**：连续失败按间隔指数退避（上限一天），并且**重启不清零**。
//!
//! ── 硬约束：请求发出**之前**就记一笔尝试 ─────────────────────
//! `claim` 在返回执行权之前就把 `lastAttemptAt` / `nextRunAt` 写进 KV。理由：
//! 只在成功后记录的话，「请求途中被杀 / 失败 / 上游超时」这几条路径都会留下一个
//! 「没跑过」的状态，下次启动立刻重试 —— 那正是本模块要消灭的行为。
//!
//! ── 执行占位（lease）：进程被杀也不会卡死 ────────────────────
//! 同一个任务同时只允许一轮真实请求（占位 + 心跳续租 90 秒）。占位**带过期时间**
//! 而不是一个布尔：进程崩溃 / 强杀后没有人来清标志，只有租约到期才会释放 ——
//! 布尔形态会让任务永远卡在「执行中」。心跳任务随 `RunGuard` 一起结束，正常收尾
//! （`finish`）与中途取消（`Drop`）都会释放占位。
//!
//! ── 为什么整份存一个对象 ───────────────────────────────────
//! 六条任务的状态整份存一个 `HashMap<String, TaskState>`，键为 `backgroundTaskState`。
//! 写入是「读改写整份」，由一把进程内 `parking_lot::Mutex` 串行化（单实例语义，
//! 与 Agent2API 的 `TransactionBehavior::Immediate` 同口径），不会丢别的任务的更新。
//! 写入本身是低频动作（每轮一次，加上 30 秒一次的心跳）。
//!
//! ── 硬约束：持锁期间不打日志 ──────────────────────────────────
//! 与 AIGX 的 `config::sql` 同一条：`parking_lot::Mutex` 不可重入，在闭包里打
//! `tracing` 日志会当场死锁（日志写入路径也可能走到本模块的锁）。本模块的日志一律
//! 在 `change` 闭包之外打（`now_ms` 只读时钟，不算 IO）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::storage::FileStore;

/// `FileStore` KV 里的键名（存整份任务状态 map）
const KV_KEY: &str = "backgroundTaskState";

/// 执行占位的租约时长：心跳每 30 秒续一次，留三倍余量。
///
/// 一轮凭证维护可能跨十几个账号的网络请求，几分钟是正常的。租约只需满足「活着的
/// 那一轮不会被人抢走」，过期时间由心跳不断推后；真正的兜底场景是**进程被杀**，
/// 那时 90 秒后自动放行。
const LEASE_MS: i64 = 90_000;

/// 失败退避的上限（一天）：上游长期不可用时也要保证每天试一次。
const MAX_BACKOFF_MS: i64 = 24 * 60 * 60_000;

/// 进程内 `change` 的串行锁：保证「读改写整份」原子（单实例语义）。
static CHANGE_LOCK: Mutex<()> = Mutex::new(());

static STORE: OnceLock<Option<Arc<FileStore>>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 毫秒时间戳（与 Agent2API `logging::now_ms` 同口径）。
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 注入 `FileStore`（由 `scheduled_tasks::spawn` 在启动时调用一次）。
pub fn install(store: Arc<FileStore>) {
    let _ = STORE.set(Some(store));
}

fn store() -> Result<&'static Arc<FileStore>, String> {
    STORE
        .get()
        .and_then(Option::as_ref)
        .ok_or_else(|| "任务状态存储不可用，未发送后台请求".to_string())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TaskState {
    pub last_attempt_at: i64,
    pub last_run_at: i64,
    pub last_success_at: i64,
    pub last_result: Option<String>,
    pub last_error: Option<String>,
    pub next_run_at: i64,
    pub retry_at: i64,
    pub failures: u32,
    pub value: Option<Value>,
    interval_ms: i64,
    lease_until: i64,
    owner: String,
}

impl TaskState {
    pub fn running(&self) -> bool {
        !self.owner.is_empty() && self.lease_until > now_ms()
    }

    pub fn due_at(&self) -> i64 {
        self.next_run_at.max(self.retry_at).max(self.lease_until)
    }

    pub fn waiting_message(&self) -> String {
        if self.running() {
            return "任务正在执行中，请稍候".to_string();
        }
        let seconds = ((self.retry_at - now_ms()).max(0) + 999) / 1000;
        if seconds > 0 {
            format!("请求处于冷却期，请在 {seconds} 秒后重试")
        } else {
            "刚刚已执行，请稍后重试".to_string()
        }
    }

    fn adjust_clock(&mut self, now: i64) {
        if self.last_attempt_at <= now + 60_000 {
            return;
        }
        // 系统时钟回拨时保留剩余等待量，不能把未来的旧时间当作无限期冷却。
        let shift = self.last_attempt_at - now;
        self.last_attempt_at = now;
        self.last_run_at = (self.last_run_at - shift).max(0);
        self.last_success_at = (self.last_success_at - shift).max(0);
        self.next_run_at = (self.next_run_at - shift).max(now);
        self.retry_at = (self.retry_at - shift).max(0);
        self.lease_until = (self.lease_until - shift).max(0);
    }
}

fn read_states(store: &FileStore) -> Result<HashMap<String, TaskState>, String> {
    let states: Option<HashMap<String, TaskState>> = store
        .get(KV_KEY)
        .map_err(|error| format!("读取任务状态失败: {error}"))?;
    Ok(states.unwrap_or_default())
}

pub fn read(key: &str) -> Result<TaskState, String> {
    Ok(read_all()?.get(key).cloned().unwrap_or_default())
}

pub fn read_all() -> Result<HashMap<String, TaskState>, String> {
    read_states(store()?)
}

/// 读改写整份任务状态 map（进程内串行化）。
///
/// 由 `CHANGE_LOCK` 保证「读 → mutate → 写」原子，避免并发 `claim`（调度循环与
/// 手动触发同时到达）各读一份都以为可执行。跨进程不保证原子 —— AIGX 服务端单实例，
/// 且 lease 机制兜底崩溃恢复。
fn change(key: &str, mutate: impl FnOnce(&mut TaskState)) -> Result<TaskState, String> {
    let _guard = CHANGE_LOCK.lock();
    let store = store()?;
    let mut states = read_states(store)?;
    let state = states.entry(key.to_string()).or_default();
    mutate(state);
    let result = state.clone();
    store
        .put(KV_KEY, &states)
        .map_err(|error| format!("保存任务状态失败: {error}"))?;
    Ok(result)
}

/// 立即排到「下一拍就执行」（绕过冷却）。
///
/// 用于「刚开启任务」语义：用户打开开关，希望下一个 tick 就跑一次，而不是按
/// 上次尝试时间算出的旧排期干等。
pub fn schedule_now(key: &str) -> Result<(), String> {
    change(key, |state| {
        let now = now_ms();
        state.retry_at = 0;
        state.next_run_at = now;
    })?;
    Ok(())
}

/// 按给定间隔重新排期：从未跑过的排到「现在」（启动即跑一次），跑过的按「上次尝试 + 间隔」。
///
/// 停机 / 休眠错过的那一轮在醒来时跑一次（`next_run_at` 落到过去 → 下一拍即到点），
/// 不补跑期间的每一轮。
pub fn reschedule(key: &str, interval_ms: i64) -> Result<(), String> {
    change(key, |state| {
        let now = now_ms();
        state.interval_ms = interval_ms;
        // last_attempt=0（从未跑）→ 0+interval 是过去时刻 → max(now)=now，于是启动即跑。
        // 跑过的 → last_attempt+interval；若已过期则 max(now)=now，补跑一次。
        state.next_run_at = state.last_attempt_at.saturating_add(interval_ms).max(now);
    })?;
    Ok(())
}

pub enum Claim {
    Acquired(RunGuard),
    Deferred(TaskState),
}

/// 手动触发**越不越过失败冷却**（[`claim`] 的显式入参）。
///
/// 两种语义同时存在，各有理由 —— 所以它不藏在 `manual` 里顺带决定：
///   - [`ManualBackoff::Respect`]：冷却记的是**上游配额桶什么时候恢复**
///     （检查更新的 GitHub 限额）。提前打一次只会再吃一次 403，不如如实告诉用户
///     还要等多久；
///   - [`ManualBackoff::Bypass`]：冷却记的是**上一轮为什么没成功**（模型目录按
///     上游逐个 401 / 5xx 退避；凭证维护按账号失败退避）。用户按下按钮的预期就是
///     「现在真打一次」，按按钮往往正是因为刚把那个原因修好 —— 继续拿旧结论挡着，
///     界面上只会留着上一次的错误文案，看起来就是按钮坏了。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualBackoff {
    Respect,
    Bypass,
}

/// 判定到期并占位（两种结果都是 `Ok`：Deferred 不是错误，只是这轮不该跑）。
///
/// `manual`（用户主动触发）跳过普通排期；**是否连失败冷却一起跳过**由 `backoff`
/// 决定。在途占位与最短请求间隔**手动也不越过**。
pub fn claim(
    key: &str,
    interval_ms: i64,
    manual: bool,
    backoff: ManualBackoff,
    min_gap_ms: i64,
) -> Result<Claim, String> {
    let now = now_ms();
    let owner = format!(
        "{}-{now}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let mut acquired = false;
    let state = change(key, |state| {
        state.adjust_clock(now);
        let earliest = state.last_attempt_at.saturating_add(min_gap_ms);
        let cooling = state.retry_at > now && !(manual && backoff == ManualBackoff::Bypass);
        if state.running() || now < earliest || cooling || (!manual && now < state.next_run_at) {
            return;
        }
        acquired = true;
        state.last_attempt_at = now;
        state.interval_ms = interval_ms;
        state.next_run_at = now.saturating_add(interval_ms);
        state.lease_until = now.saturating_add(LEASE_MS);
        state.owner = owner.clone();
    })?;
    if !acquired {
        return Ok(Claim::Deferred(state));
    }
    let heartbeat_key = key.to_string();
    let heartbeat_owner = owner.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let result = change(&heartbeat_key, |state| {
                if state.owner == heartbeat_owner {
                    state.lease_until = now_ms().saturating_add(LEASE_MS);
                }
            });
            if !matches!(result, Ok(state) if state.owner == heartbeat_owner) {
                break;
            }
        }
    });
    Ok(Claim::Acquired(RunGuard {
        key: key.to_string(),
        owner,
        heartbeat,
        finished: false,
    }))
}

pub struct RunGuard {
    key: String,
    owner: String,
    heartbeat: tokio::task::JoinHandle<()>,
    finished: bool,
}

impl RunGuard {
    pub fn finish(
        mut self,
        success: bool,
        summary: String,
        value: Option<Value>,
        retry_at: i64,
        interval_ms: i64,
    ) -> Result<TaskState, String> {
        self.heartbeat.abort();
        let result = change(&self.key, |state| {
            if state.owner != self.owner {
                return;
            }
            let now = now_ms();
            state.last_run_at = now;
            state.last_result = Some(summary.clone());
            state.interval_ms = interval_ms;
            if success {
                state.last_success_at = now;
                state.last_error = None;
                state.failures = 0;
                state.retry_at = retry_at;
            } else {
                state.last_error = Some(summary);
                state.failures = state.failures.saturating_add(1);
                let multiplier = 1_i64 << state.failures.saturating_sub(1).min(10);
                let delay = interval_ms
                    .max(60_000)
                    .saturating_mul(multiplier)
                    .min(MAX_BACKOFF_MS);
                state.retry_at = retry_at.max(now.saturating_add(delay));
            }
            if let Some(value) = value {
                state.value = Some(value);
            }
            state.next_run_at = now.saturating_add(interval_ms).max(state.retry_at);
            state.owner.clear();
            state.lease_until = 0;
        });
        self.finished = result.is_ok();
        result
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        // 正常收尾由 finish 完成（owner 已清空、finished=true）；走到这里说明中途
        // 取消（panic / 提前 return）。释放占位，否则任务会卡在「执行中」直到租约过期。
        if !self.finished {
            let _ = change(&self.key, |state| {
                if state.owner == self.owner {
                    state.owner.clear();
                    state.lease_until = 0;
                }
            });
        }
        self.heartbeat.abort();
    }
}
