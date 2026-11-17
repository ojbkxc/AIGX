// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件声明的后台任务 —— 声明解析、排程判定、执行、配置读写。
//!
//! ## 与上游的差异（仅接线方式）
//!
//! 上游直接读写 `config` 全局的 `pluginTaskSettings` 字段并调
//! `registry::serves` / `Host::call_coded`。AIGX 的配置是 `ConfigManager`
//! （异步、强类型 `AppConfig`），插件宿主是 Wasm 注册表。因此这里把两处
//! 外部依赖收成注入面：
//!
//! - [`WasmTaskSettings`] —— 配置读写（`get` / `put` / `legacy` / `wake`），
//!   未注入时用进程内内存实现（管理面可改，重启丢失）
//! - 执行走 [`super::host_for_provider`] 的 [`WasmHost::call`]；账号列表走
//!   注入的 [`WasmAccountAccess`]
//!
//! 排期状态仍用 AIGX 既有的 `scheduled_tasks::task_state`（KV + 租约 + 失败
//! 退避），键格式与上游一致：`pluginTask:{provider}:{task}:{account}`。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use chrono::Timelike;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::scheduled_tasks::task_state::{self, Claim, ManualBackoff};

use super::WasmAccountAccess;

/// 一条任务声明（与上游 `tasks::Definition` 逐字一致）。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub times: Vec<String>,
    #[serde(default)]
    pub interval_ms: Option<i64>,
    #[serde(default = "zone")]
    pub utc_offset_minutes: i32,
    #[serde(default)]
    pub checkin: bool,
    #[serde(default)]
    pub legacy_config: Option<String>,
}

fn zone() -> i32 {
    480
}

fn definitions() -> &'static Mutex<HashMap<String, Vec<Definition>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Vec<Definition>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 插件任务配置的读写出口（AIGX 侧通常接到 `config.plugin_task_settings`）。
///
/// 同步签名：`configure` 是管理面的同步调用点（与上游 `config::update_raw_field`
/// 同语义），实现方内部可用 `Handle::block_on` 或先写内存再由后台落盘。
pub trait WasmTaskSettings: Send + Sync {
    /// 读某任务保存的覆盖值（`null` 表示没保存过）。
    fn get(&self, provider: &str, task: &str) -> Value;
    /// 合并写一个任务的覆盖值。
    fn put(&self, provider: &str, task: &str, patch: &Value) -> Result<(), String>;
    /// 读旧版全局配置项（仅允许白名单键，见 `Definition::legacy_config`）。
    fn legacy(&self, key: &str) -> Value;
    /// 唤醒调度循环（AIGX 的循环每 10s 自醒，默认实现可空）。
    fn wake(&self) {}
}

#[derive(Default)]
struct MemorySettings {
    tasks: Mutex<HashMap<(String, String), Value>>,
}

impl WasmTaskSettings for MemorySettings {
    fn get(&self, provider: &str, task: &str) -> Value {
        self.tasks
            .lock()
            .ok()
            .and_then(|table| {
                table
                    .get(&(provider.to_string(), task.to_string()))
                    .cloned()
            })
            .unwrap_or(Value::Null)
    }

    fn put(&self, provider: &str, task: &str, patch: &Value) -> Result<(), String> {
        let mut table = self
            .tasks
            .lock()
            .map_err(|_| "任务设置存储不可用".to_string())?;
        let entry = table
            .entry((provider.to_string(), task.to_string()))
            .or_insert_with(|| json!({}));
        if !entry.is_object() {
            *entry = json!({});
        }
        for (key, value) in patch.as_object().ok_or("任务设置无效")? {
            entry[key] = value.clone();
        }
        Ok(())
    }

    fn legacy(&self, _key: &str) -> Value {
        Value::Null
    }
}

/// 落 `FileStore` 的任务配置（重启不丢）。
///
/// 键形态：`wasm:task_settings:{provider}:{task}` → 该任务的覆盖值对象
/// （`enabled` / `times`）。`legacy` 读 AIGX 既有配置的白名单项
/// （当前只有 `growthSchedule`，与 `Definition::legacy_config` 的校验同源）。
pub struct StoreSettings {
    store: Arc<crate::storage::FileStore>,
}

impl StoreSettings {
    pub fn new(store: Arc<crate::storage::FileStore>) -> Arc<Self> {
        Arc::new(Self { store })
    }

    fn key(provider: &str, task: &str) -> String {
        format!("wasm:task_settings:{provider}:{task}")
    }
}

impl WasmTaskSettings for StoreSettings {
    fn get(&self, provider: &str, task: &str) -> Value {
        self.store
            .get::<Value>(&Self::key(provider, task))
            .ok()
            .flatten()
            .unwrap_or(Value::Null)
    }

    fn put(&self, provider: &str, task: &str, patch: &Value) -> Result<(), String> {
        // 读-改-写整条（FileStore 无字段级 CAS；任务配置只有管理面一个写者，
        // 且 `configure` 已按 id 定位，并发写同一任务属运维误操作，后写胜出即可）。
        let patch = patch.as_object().ok_or("任务设置无效")?;
        let mut current = self.get(provider, task);
        if !current.is_object() {
            current = json!({});
        }
        let object = current.as_object_mut().ok_or("任务设置无效")?;
        for (key, value) in patch {
            object.insert(key.clone(), value.clone());
        }
        self.store
            .put(&Self::key(provider, task), &current)
            .map_err(|error| format!("保存插件任务设置失败: {error}"))
    }

    fn legacy(&self, key: &str) -> Value {
        // 只允许白名单键（`Definition::legacy_config` 已在 load() 时校验过，
        // 这里再挡一次防止别的调用点绕过）。
        if key != "growthSchedule" {
            return Value::Null;
        }
        self.store
            .get::<Value>("growthSchedule")
            .ok()
            .flatten()
            .unwrap_or(Value::Null)
    }
}

/// 便捷入口：用 `FileStore` 装配并注入任务配置存储。
///
/// 等价于 `install_settings(StoreSettings::new(store))`，给 integrator 一行调用。
pub fn install_store_settings(store: Arc<crate::storage::FileStore>) {
    install_settings(StoreSettings::new(store));
}

fn settings_slot() -> &'static Mutex<Option<Arc<dyn WasmTaskSettings>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<dyn WasmTaskSettings>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn settings() -> Arc<dyn WasmTaskSettings> {
    let mut slot = settings_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    slot.get_or_insert_with(|| Arc::new(MemorySettings::default()))
        .clone()
}

/// 注入任务配置出口（后注入覆盖先前值；建议在 `tasks::spawn()` 之前调用）。
pub fn install_settings(store: Arc<dyn WasmTaskSettings>) {
    *settings_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(store);
}

/// 规范化 `HH:MM`（接受 `H:MM`，返回零填充；非法返回 `None`）。
pub(crate) fn normalize_time(raw: &str) -> Option<String> {
    let (hour, minute) = raw.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(format!("{hour:02}:{minute:02}"))
}

/// 读取并校验插件声明的任务列表（`tasks.list` 调用）。
pub async fn load(provider: &str) -> Result<(), String> {
    if !super::has_capability(provider, "tasks.v1") {
        return Err("插件未声明任务能力".into());
    }
    let host = super::host_for_provider(provider).ok_or("Wasm 插件尚未加载")?;
    let declared = host
        .call(
            "tasks.list",
            json!({ "provider": provider }),
            Duration::from_secs(10),
        )
        .await
        .map_err(|error| error.message)?;
    let mut tasks: Vec<Definition> =
        serde_json::from_value(declared).map_err(|_| "插件任务声明格式无效")?;
    if tasks.len() > 32 {
        return Err("插件任务数量超过上限".into());
    }
    let mut ids = HashSet::new();
    let mut checkin = false;
    for task in &mut tasks {
        if task.checkin && (checkin || task.interval_ms.is_some() || task.times.len() != 1) {
            return Err("签到窗口只能声明一条每日时刻".into());
        }
        checkin |= task.checkin;
        if task
            .legacy_config
            .as_deref()
            .is_some_and(|key| key != "growthSchedule")
        {
            return Err("任务不允许读取其他主程序配置".into());
        }
        if task.id.is_empty()
            || task.id.len() > 64
            || !task
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
            || !ids.insert(task.id.clone())
            || task.label.is_empty()
            || task.label.len() > 256
            || !(-720..=840).contains(&task.utc_offset_minutes)
            || task.times.len() > 24
            || task
                .interval_ms
                .is_some_and(|ms| !(60_000..=30 * 86_400_000).contains(&ms))
            || task.interval_ms.is_some() == !task.times.is_empty()
        {
            return Err("插件任务名称或排程无效".into());
        }
        for time in &mut task.times {
            *time = normalize_time(time).ok_or("插件任务时刻无效")?;
        }
        task.times.sort();
        task.times.dedup();
    }
    definitions()
        .lock()
        .map_err(|_| "插件任务列表不可用")?
        .insert(provider.into(), tasks);
    Ok(())
}

/// 摘除某 provider 的任务声明。
pub fn forget(provider: &str) {
    if let Ok(mut table) = definitions().lock() {
        table.remove(provider);
    }
}

/// 签到窗口的本地时刻（`HH:MM`，UTC+8 口径）。
///
/// 待签到中心接 wasm 签到窗口后启用（见 docs/a2a-alignment/README.md 未完成项）。
#[allow(dead_code)]
pub fn checkin_time(provider: &str) -> Option<String> {
    let table = definitions().lock().ok()?;
    let task = table.get(provider)?.iter().find(|task| task.checkin)?;
    let time = chrono::NaiveTime::parse_from_str(task.times.first()?, "%H:%M").ok()?;
    let minutes = (time.num_seconds_from_midnight() as i32 / 60 + 480 - task.utc_offset_minutes)
        .rem_euclid(1440);
    Some(format!("{:02}:{:02}", minutes / 60, minutes % 60))
}

fn current(provider: &str, task: &Definition) -> Definition {
    let mut task = task.clone();
    let mut value = settings().get(provider, &task.id);
    if value.is_null() {
        if let Some(key) = &task.legacy_config {
            value = settings().legacy(key);
        }
    }
    if let Some(enabled) = value["enabled"].as_bool() {
        task.enabled = enabled;
    }
    if let Some(items) = value["times"].as_array() {
        let times: Vec<_> = items
            .iter()
            .filter_map(Value::as_str)
            .filter_map(normalize_time)
            .collect();
        if !times.is_empty() {
            task.times = times;
            task.interval_ms = None;
        }
    }
    task
}

/// 任务 + 每账号运行状态快照（管理面 `/api/plugins/tasks` 的数据源）。
pub fn snapshot() -> Value {
    let table = definitions()
        .lock()
        .map(|table| table.clone())
        .unwrap_or_default();
    let states = task_state::read_all().unwrap_or_default();
    json!(table
        .into_iter()
        .filter(|(provider, _)| super::owns(provider))
        .flat_map(|(provider, tasks)| {
            let states = &states;
            tasks
                .into_iter()
                .filter(|task| !task.checkin)
                .map(move |task| {
                    let task = current(&provider, &task);
                    let prefix = format!("pluginTask:{provider}:{}:", task.id);
                    let runs: Vec<_> = states
                        .iter()
                        .filter(|(key, _)| key.starts_with(&prefix))
                        .map(|(key, state)| {
                            json!({
                                "account": key.strip_prefix(&prefix),
                                "running": state.running(),
                                "lastRunAt": state.last_run_at,
                                "nextRunAt": state.next_run_at,
                                "error": state.last_error,
                                "result": state.last_result
                            })
                        })
                        .collect();
                    json!({ "provider": provider, "task": task, "runs": runs })
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>())
}

/// 保存任务配置（`enabled` / `times` 两键，其余拒绝）。
pub fn configure(provider: &str, id: &str, patch: &Value) -> Result<(), String> {
    if !super::has_capability(provider, "tasks.v1") || !super::owns(provider) {
        return Err("插件任务当前不可用".into());
    }
    let task = definitions()
        .lock()
        .map_err(|_| "任务列表不可用".to_string())?
        .get(provider)
        .and_then(|tasks| tasks.iter().find(|task| task.id == id && !task.checkin))
        .cloned()
        .ok_or("任务不存在")?;
    if !patch.is_object()
        || patch.as_object().is_some_and(|map| {
            map.keys()
                .any(|key| !matches!(key.as_str(), "enabled" | "times"))
        })
        || patch
            .get("enabled")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err("任务设置无效".into());
    }
    if let Some(times) = patch.get("times") {
        let times = times.as_array().ok_or("任务时刻必须是数组")?;
        if task.interval_ms.is_some()
            || times.is_empty()
            || times.len() > 24
            || times
                .iter()
                .any(|time| time.as_str().and_then(normalize_time).is_none())
        {
            return Err("任务时刻无效".into());
        }
    }
    settings().put(provider, id, patch)?;
    settings().wake();
    Ok(())
}

/// 排程判定：`(是否到点, 下一轮间隔 ms, 本次计划时刻)`。
fn interval(task: &Definition, now: i64) -> (bool, i64, i64) {
    if let Some(interval) = task.interval_ms {
        return (true, interval, 0);
    }
    let offset = i64::from(task.utc_offset_minutes) * 60_000;
    let local = now + offset;
    let day = local.div_euclid(86_400_000) * 86_400_000 - offset;
    let times: Vec<i64> = task
        .times
        .iter()
        .filter_map(|time| {
            let parsed = chrono::NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            Some(day + i64::from(parsed.num_seconds_from_midnight()) * 1000)
        })
        .collect();
    let due = times
        .iter()
        .copied()
        .filter(|at| *at <= now)
        .max()
        .unwrap_or(0);
    let next = times
        .iter()
        .copied()
        .filter(|at| *at > now)
        .min()
        .or_else(|| times.first().map(|at| at + 86_400_000))
        .unwrap_or(now + 86_400_000);
    (due > 0, (next - now).max(1000), due)
}

/// 执行一个任务（`manual=true` 跳过 enabled 与排期，但仍受在途占位约束）。
///
/// 账号列表来自注入的 [`WasmAccountAccess`]（`start()` 时登记的那个）。
pub async fn run(provider: &str, id: &str, manual: bool) -> Result<Value, String> {
    let accounts =
        super::active_accounts().ok_or("账号访问面尚未就绪（Wasm 宿主未启动）".to_string())?;
    run_with(accounts, provider, id, manual).await
}

/// 同 [`run`]，但显式传账号访问面（测试 / 多实例）。
pub async fn run_with(
    accounts: Arc<dyn WasmAccountAccess>,
    provider: &str,
    id: &str,
    manual: bool,
) -> Result<Value, String> {
    if !super::has_capability(provider, "tasks.v1") || !super::owns(provider) {
        return Err("插件任务当前不可用".into());
    }
    let task = definitions()
        .lock()
        .map_err(|_| "任务列表不可用".to_string())?
        .get(provider)
        .and_then(|tasks| tasks.iter().find(|task| task.id == id && !task.checkin))
        .cloned()
        .ok_or("任务不存在")?;
    let task = current(provider, &task);
    if !manual && !task.enabled {
        return Ok(json!({ "skipped": true }));
    }
    let mut results = Vec::new();
    for account in accounts.accounts_for_provider(provider) {
        let Some(account) = account["id"].as_str() else {
            continue;
        };
        let Some(host) = super::host_for_provider(provider) else {
            break;
        };
        if !host.manifest.allows("tasks.v1") || host.closing() {
            break;
        }
        if !manual && !current(provider, &task).enabled {
            break;
        }
        let now = super::now_ms();
        let (due, interval_ms, planned_at) = interval(&task, now);
        let key = format!("pluginTask:{provider}:{id}:{account}");
        let previous = task_state::read(&key)?;
        if !manual
            && (!due
                || planned_at > 0
                    && previous.last_attempt_at >= planned_at
                    && previous.failures == 0)
        {
            continue;
        }
        let guard =
            match task_state::claim(&key, interval_ms, manual, ManualBackoff::Respect, 1000)? {
                Claim::Acquired(guard) => guard,
                Claim::Deferred(_) => continue,
            };
        let result = host
            .call(
                "tasks.run",
                json!({
                    "provider": provider, "account": account, "task": id,
                    "manual": manual, "scheduled_at": planned_at
                }),
                Duration::from_secs(600),
            )
            .await;
        let (success, message) = match result {
            Ok(value) if value["success"].is_boolean() => (
                value["success"] == true,
                value["message"]
                    .as_str()
                    .unwrap_or("任务已结束")
                    .chars()
                    .take(1000)
                    .collect::<String>(),
            ),
            Ok(_) => (false, "插件未返回有效的任务结果".into()),
            Err(error) => (false, redact(&error.message)),
        };
        guard.finish(success, message.clone(), None, 0, interval_ms)?;
        results.push(json!({ "account": account, "success": success, "message": message }));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok(json!({ "results": results }))
}

/// 错误文案脱敏（去掉可能的凭据片段，与原生插件宿主的 `process::redact` 同口径）。
///
/// 逐行处理：行内出现敏感键时，把**该键之后到行尾**的整段替换成 `[redacted]`
/// —— 只抹键名会让 `Authorization: Bearer <token>` 的值漏出去。
fn redact(message: &str) -> String {
    let keys = [
        "authorization",
        "cookie",
        "token",
        "api-key",
        "apikey",
        "secret",
    ];
    let mut out = String::new();
    for line in message.lines() {
        let lower = line.to_ascii_lowercase();
        match keys.iter().filter_map(|key| lower.find(key)).min() {
            Some(index) => {
                out.push_str(&line[..index]);
                out.push_str("[redacted]");
            }
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out.chars().take(1000).collect()
}

/// 任务调度循环的取消令牌（进程级单例）。
///
/// `spawn()` 只允许启动一次；`shutdown()` 置位并唤醒循环，让关服路径能把
/// 这个后台循环**完整停掉**，不留孤儿任务。与 `plugin_host::wasm::Cancel`
/// 同语义（本模块不引 tokio-util），但独立实例：宿主级 cancel 是「关一个包」
/// ，这里是「关整个进程的插件任务调度」。
fn scheduler_token() -> &'static super::Cancel {
    static TOKEN: OnceLock<super::Cancel> = OnceLock::new();
    TOKEN.get_or_init(super::Cancel::new)
}

/// 调度循环是否已启动（防重复 spawn 起两个循环）。
fn scheduler_started() -> &'static std::sync::atomic::AtomicBool {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    &STARTED
}

/// 等待一轮（可被取消立即打断，不必等满 30s）。
async fn tick_or_cancel(cancel: &super::Cancel) {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {}
        _ = tokio::time::sleep(Duration::from_secs(30)) => {}
    }
}

/// 周期驱动：每 30 秒扫一遍声明表，把到点的任务交给 [`run`]。
///
/// 由 integrator 在 `main.rs` 里 `spawn` 一次（与 `scheduled_tasks::spawn`
/// 并列，且必须在其之后 —— 后者负责 `task_state::install`）。provider 级别的
/// 并发用 `running` 集合挡住。
///
/// **幂等**：重复调用不会再起第二个循环。**可关闭**：见 [`shutdown`]。
///
/// ## 为什么 `scheduler_started` 在 [`shutdown`] 里不复位
///
/// 本循环**每轮都重读** `definitions()` 表，而插件增删会同步这张表：
/// 宿主停用 → `mod::forget(provider)` → `tasks::forget(provider)` 移除条目；
/// 宿主启用 → `tasks::load(provider)` 写回条目。因此插件重载**不需要**重启
/// 本循环，30s 的下一拍自然跟上。
///
/// 结论：进程生命周期内只起一次。关服后令牌已取消，再调本函数直接 return
/// 不会「复活」循环 —— 这是有意设计（进程即将退出）。**若将来改成本地缓存
/// 任务表（不再每轮重读 `definitions()`），才必须在 `shutdown()` 里复位
/// `scheduler_started`**，否则停用再启用插件后任务会永远不跑。
pub fn spawn() {
    if scheduler_started().swap(true, Ordering::AcqRel) {
        return;
    }
    spawn_loop(scheduler_token().clone());
}

/// 用**指定的**取消令牌起循环（测试用 / 多实例）。
///
/// 生产只用 [`spawn`]（进程级单例令牌）；本入口存在是为了让单测能拿到
/// 一个「新鲜的、可独立取消」的令牌，而不污染进程级单例的终态。
#[allow(dead_code)] // 仅测试用（生产走 `spawn` 的单例令牌）
pub fn spawn_with(cancel: super::Cancel) {
    spawn_loop(cancel);
}

fn spawn_loop(cancel: super::Cancel) {
    let running = Arc::new(Mutex::new(HashSet::new()));
    tokio::spawn(async move {
        loop {
            let table = definitions()
                .lock()
                .map(|table| table.clone())
                .unwrap_or_default();
            for (provider, tasks) in table {
                if cancel.is_cancelled() {
                    return;
                }
                if !super::owns(&provider) || !super::has_capability(&provider, "tasks.v1") {
                    continue;
                }
                if !running
                    .lock()
                    .is_ok_and(|mut running| running.insert(provider.clone()))
                {
                    continue;
                }
                let running = running.clone();
                let child = cancel.child();
                tokio::spawn(async move {
                    for task in tasks.into_iter().filter(|task| !task.checkin) {
                        // 关服时不再发起新的账号级调用；在途的那一轮会自然收尾
                        if child.is_cancelled() {
                            break;
                        }
                        if let Err(error) = run(&provider, &task.id, false).await {
                            tracing::debug!("[PluginTask] {provider}/{}: {error}", task.id);
                        }
                    }
                    if let Ok(mut running) = running.lock() {
                        running.remove(&provider);
                    }
                });
            }
            tick_or_cancel(&cancel).await;
            if cancel.is_cancelled() {
                tracing::debug!("[PluginTask] 调度循环已停止");
                return;
            }
        }
    });
}

/// 停止调度循环（幂等；由 `main.rs` 的优雅关闭路径调用）。
///
/// 取消后：本轮 tick 立即返回、已在跑的任务子循环在下一个任务边界退出。
/// 已派发的 `tasks.run` 调用会走宿主本身的 `cancel`/超时收尾，不会悬挂。
pub fn shutdown() {
    scheduler_token().cancel();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_times() {
        assert_eq!(normalize_time("9:05").as_deref(), Some("09:05"));
        assert_eq!(normalize_time(" 23:59 ").as_deref(), Some("23:59"));
        assert_eq!(normalize_time("24:00"), None);
        assert_eq!(normalize_time("12:60"), None);
        assert_eq!(normalize_time("noon"), None);
    }

    #[test]
    fn interval_for_fixed_interval_tasks() {
        let task = Definition {
            id: "t".into(),
            label: "T".into(),
            enabled: true,
            times: vec![],
            interval_ms: Some(120_000),
            utc_offset_minutes: 480,
            checkin: false,
            legacy_config: None,
        };
        let (due, next, planned) = interval(&task, 1_000_000);
        assert!(due);
        assert_eq!(next, 120_000);
        assert_eq!(planned, 0);
    }

    #[test]
    fn interval_for_daily_times() {
        // 取一个远离 epoch 的基准，避免「计划时刻恰为 0」被当成未到点
        const BASE: i64 = 100 * 86_400_000;
        let task = Definition {
            id: "t".into(),
            label: "T".into(),
            enabled: true,
            times: vec!["08:00".into(), "20:00".into()],
            interval_ms: None,
            utc_offset_minutes: 480,
            checkin: false,
            legacy_config: None,
        };
        // 本地 12:00（UTC 04:00）：08:00 已过 -> due，下一轮本地 20:00（8 小时后）
        let now = BASE + 4 * 3_600_000i64;
        let (due, next, planned) = interval(&task, now);
        assert!(due);
        assert_eq!(planned, BASE); // 当日首个时刻（本地 08:00 = UTC 00:00）
        assert_eq!(next, 8 * 3_600_000);
    }

    #[test]
    fn memory_settings_merges_patches() {
        let store = MemorySettings::default();
        store.put("p", "t", &json!({ "enabled": true })).unwrap();
        store.put("p", "t", &json!({ "times": ["09:00"] })).unwrap();
        let value = store.get("p", "t");
        assert_eq!(value["enabled"], true);
        assert_eq!(value["times"][0], "09:00");
        assert_eq!(store.get("p", "missing"), Value::Null);
    }

    #[test]
    fn redact_strips_credential_fragments() {
        let out = redact("upstream 401 Authorization: Bearer abc123\nmore");
        assert!(!out.contains("abc123"), "{out}");
        assert!(out.contains("[redacted]"));
    }

    /// `spawn_with` 起的循环必须可被**该**令牌停止（令牌之间互不影响）。
    ///
    /// **不再断言进程级单例的状态**：cargo 默认并行跑测试，另一个用例
    /// （`shutdown_is_idempotent`）会取消 `scheduler_token()`；此处断言它
    /// 未取消就是顺序依赖的假红。本用例只验自己的令牌语义。
    #[tokio::test]
    async fn spawn_with_is_stoppable() {
        let token = crate::plugin_host::wasm::Cancel::new();
        assert!(!token.is_cancelled(), "初始未取消");
        spawn_with(token.clone());
        // 取消前 tick 会等 30s；取消后必须立即返回
        token.cancel();
        tokio::time::timeout(Duration::from_millis(500), tick_or_cancel(&token))
            .await
            .expect("已取消时 tick_or_cancel 必须立即返回");
        assert!(token.is_cancelled());
        // 令牌互不影响：另一个新鲜令牌仍是未取消态
        assert!(!crate::plugin_host::wasm::Cancel::new().is_cancelled());
    }

    /// `shutdown()` 幂等，且置位后 `tick_or_cancel` 立即返回。
    #[tokio::test]
    async fn shutdown_is_idempotent() {
        shutdown();
        assert!(scheduler_token().is_cancelled());
        tokio::time::timeout(
            Duration::from_millis(500),
            tick_or_cancel(scheduler_token()),
        )
        .await
        .expect("已取消时 tick_or_cancel 必须立即返回");
        shutdown();
        assert!(scheduler_token().is_cancelled());
    }
}
