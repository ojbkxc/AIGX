//! 运行容量管理（对齐 Agent2API v3.5.0-beta.2 `config/runtime_capacity.rs` +
//! `core/runtime_capacity/`）。
//!
//! 关注三件事：
//! 1. **配置** [`CapacitySettings`]（持久化语义 = `runtimeCapacity`）：自动/手动档位、
//!    对话/交互/后台/图片/连接上限、内存预算、排队参数、逐提供商上限；
//! 2. **资源采样** [`sample`]：跨平台读内存/进程占用（Linux 读 `/proc`，其它平台回落
//!    保守常量，不引入 `sysinfo` 依赖）；派生生效上限 [`limits`]（`Auto` 档位按内存与
//!    核数动态收紧，`Manual` 档位照搬配置）；
//! 3. **准入** [`try_acquire`] / [`acquire`]：按 [`Class`] 计数并返回 RAII [`Ticket`]，
//!    Drop 即释放；排队等待受 [`CapacitySettings::queue_wait_seconds`] 与队列容量约束。
//!
//! AIGX 数据面在 `main.rs::track_inflight` 中间件里对 `/v1/*` 与无前缀兼容路由
//! 做 `try_acquire(Class::Request)`——拒绝即 503，不阻塞请求线程。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const KEY_RUNTIME_CAPACITY: &str = "runtimeCapacity";
pub const MAX_EXECUTIONS: usize = 4096;
pub const POOL_EXECUTIONS: usize = MAX_EXECUTIONS + 256 + 256 + 128;
pub const MIB: usize = 1024 * 1024;

// ── 配置 ─────────────────────────────────────────────────────────────

/// 容量档位：`Auto` 按资源动态收紧，`Manual` 严格照配置。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityMode {
    #[default]
    Auto,
    Manual,
}

/// 运行容量设置（对齐参考树 `CapacitySettings`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CapacitySettings {
    pub mode: CapacityMode,
    pub max_conversations: usize,
    pub max_interactive: usize,
    pub max_background: usize,
    pub background_per_provider: usize,
    pub max_images: usize,
    pub max_connections: usize,
    pub memory_mib: usize,
    pub queue_requests: usize,
    pub queue_memory_mib: usize,
    pub queue_wait_seconds: u64,
    pub provider_limits: std::collections::BTreeMap<String, usize>,
}

impl Default for CapacitySettings {
    fn default() -> Self {
        Self {
            mode: CapacityMode::Auto,
            max_conversations: 1024,
            max_interactive: 64,
            max_background: 32,
            background_per_provider: 8,
            max_images: 16,
            max_connections: 4096,
            memory_mib: 0,
            queue_requests: 2048,
            queue_memory_mib: 256,
            queue_wait_seconds: 10,
            provider_limits: std::collections::BTreeMap::new(),
        }
    }
}

impl CapacitySettings {
    /// 区间与语义校验（与参考树同口径）。
    pub fn validate(&self) -> Result<(), String> {
        for (label, value, min, max) in [
            ("对话并发", self.max_conversations, 1, MAX_EXECUTIONS),
            ("交互并发", self.max_interactive, 4, 256),
            ("后台并发", self.max_background, 1, 256),
            ("单提供商后台并发", self.background_per_provider, 1, 64),
            ("图片并发", self.max_images, 1, 128),
            ("网络连接", self.max_connections, 64, 32768),
            ("排队请求", self.queue_requests, 1, 16384),
            ("排队内存", self.queue_memory_mib, 8, 4096),
        ] {
            if !(min..=max).contains(&value) {
                return Err(format!("{label}必须在 {min}～{max} 之间"));
            }
        }
        if self.memory_mib != 0 && !(128..=262144).contains(&self.memory_mib) {
            return Err("运行内存必须为自动，或在 128～262144 MiB 之间".into());
        }
        if !(1..=120).contains(&self.queue_wait_seconds) {
            return Err("排队等待必须在 1～120 秒之间".into());
        }
        if self.provider_limits.len() > 256
            || self.provider_limits.iter().any(|(id, limit)| {
                id.is_empty()
                    || id.len() > 128
                    || id.chars().any(char::is_control)
                    || *limit == 0
                    || *limit > MAX_EXECUTIONS
            })
        {
            return Err("提供商并发设置无效".into());
        }
        Ok(())
    }
}

// ── 进程级设置快照 ───────────────────────────────────────────────────

static SETTINGS: parking_lot::RwLock<Option<CapacitySettings>> = parking_lot::RwLock::new(None);

/// 安装/刷新进程级容量设置（启动与保存后调用）。
pub fn install_settings(settings: CapacitySettings) {
    *SETTINGS.write() = Some(settings);
}

/// 读取当前容量设置（未安装时返回默认）。
pub fn current_settings() -> CapacitySettings {
    SETTINGS.read().clone().unwrap_or_default()
}

// ── 资源采样 ─────────────────────────────────────────────────────────

/// 一次资源采样（对齐参考树 `Resources`）。
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub process_bytes: u64,
    pub container_bytes: Option<u64>,
    pub cpu_percent: f32,
    pub cores: usize,
    pub connection_ceiling: usize,
}

fn fallback_resources() -> Resources {
    Resources {
        total_bytes: 1024 * 1024 * 1024,
        available_bytes: 512 * 1024 * 1024,
        process_bytes: 0,
        container_bytes: None,
        cpu_percent: 0.0,
        cores: std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1),
        connection_ceiling: 32768,
    }
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = 0u64;
    let mut available = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        // /proc/meminfo 单位为 kB
        let bytes = value.saturating_mul(1024);
        match key {
            "MemTotal:" => total = bytes,
            "MemAvailable:" => available = bytes,
            _ => {}
        }
    }
    if total == 0 {
        None
    } else {
        Some((total, available))
    }
}

#[cfg(target_os = "linux")]
fn read_process_bytes() -> u64 {
    // /proc/self/statm 第二列 = resident pages
    let text = match std::fs::read_to_string("/proc/self/statm") {
        Ok(text) => text,
        Err(_) => return 0,
    };
    text.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .map(|pages| pages.saturating_mul(4096))
        .unwrap_or(0)
}

/// 采样当前资源；Linux 读 `/proc`，其它平台回落保守常量。
pub fn sample() -> Resources {
    let cores = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    #[cfg(target_os = "linux")]
    {
        if let Some((total_bytes, available_bytes)) = read_meminfo() {
            return Resources {
                total_bytes,
                available_bytes,
                process_bytes: read_process_bytes(),
                container_bytes: None,
                cpu_percent: 0.0,
                cores,
                connection_ceiling: 32768,
            };
        }
    }
    let mut fallback = fallback_resources();
    fallback.cores = cores;
    fallback
}

// ── 生效上限 ─────────────────────────────────────────────────────────

/// 派生生效上限（对齐参考树 `Limits`）。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub conversations: usize,
    pub interactive: usize,
    pub background: usize,
    pub background_per_provider: usize,
    pub images: usize,
    pub connections: usize,
    pub memory_bytes: usize,
    pub queue_requests: usize,
    pub queue_bytes: usize,
    pub queue_wait_seconds: u64,
    pub cpu_pressure: bool,
    pub memory_pressure: bool,
}

impl Limits {
    fn for_class(&self, class: Class) -> usize {
        match class {
            Class::Conversation | Class::Request => self.conversations,
            Class::Interactive => self.interactive,
            Class::Background => self.background,
            Class::Image | Class::ImageRequest => self.images,
            Class::Network => self.connections,
        }
    }
}

fn effective(settings: &CapacitySettings, resources: Resources) -> Limits {
    let auto_memory = (resources.total_bytes / 2) as usize;
    let memory_bytes = if settings.memory_mib == 0 {
        auto_memory.max(16 * MIB)
    } else {
        settings
            .memory_mib
            .saturating_mul(MIB)
            .min((resources.total_bytes * 3 / 4) as usize)
            .max(16 * MIB)
    };
    let process_ceiling = resources.total_bytes.saturating_mul(4) / 5;
    let memory_pressure = resources.available_bytes < resources.total_bytes / 10
        || resources.container_bytes.unwrap_or(resources.process_bytes) >= process_ceiling;
    let cpu_pressure = resources.cpu_percent >= 90.0;
    let automatic = settings.mode == CapacityMode::Auto;
    let conversations = if automatic {
        settings
            .max_conversations
            .min((memory_bytes / (4 * MIB)).max(8))
    } else {
        settings.max_conversations
    };
    Limits {
        conversations,
        interactive: settings.max_interactive,
        background: if cpu_pressure || memory_pressure {
            0
        } else if automatic {
            settings
                .max_background
                .min(resources.cores.saturating_mul(4).max(2))
        } else {
            settings.max_background
        },
        background_per_provider: settings.background_per_provider,
        images: if cpu_pressure || memory_pressure {
            0
        } else if automatic {
            settings.max_images.min((memory_bytes / (64 * MIB)).max(1))
        } else {
            settings.max_images
        },
        connections: settings.max_connections.min(resources.connection_ceiling),
        memory_bytes,
        queue_requests: settings.queue_requests,
        queue_bytes: settings
            .queue_memory_mib
            .saturating_mul(MIB)
            .min(memory_bytes / 4)
            .max(8 * MIB),
        queue_wait_seconds: settings.queue_wait_seconds,
        cpu_pressure,
        memory_pressure,
    }
}

/// 当前生效上限。
pub fn limits() -> Limits {
    effective(&current_settings(), sample())
}

// ── 准入计数 ─────────────────────────────────────────────────────────

/// 准入类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Conversation,
    Interactive,
    Background,
    Image,
    Network,
    Request,
    ImageRequest,
}

impl Class {
    fn index(self) -> usize {
        match self {
            Self::Conversation => 0,
            Self::Interactive => 1,
            Self::Background => 2,
            Self::Image => 3,
            Self::Network => 4,
            Self::Request => 5,
            Self::ImageRequest => 6,
        }
    }
}

#[derive(Default)]
struct State {
    active: [usize; 7],
    memory: usize,
    waiters: Vec<Waiter>,
    queue_bytes: usize,
    rejected: u64,
    sequence: u64,
    providers: HashMap<(usize, String), usize>,
    callers: HashMap<(usize, String), usize>,
}

struct Waiter {
    id: u64,
    class: Class,
    provider: String,
    caller: String,
    bytes: usize,
    deadline: Instant,
}

struct Manager {
    state: Mutex<State>,
    changed: tokio::sync::Notify,
}

fn manager() -> &'static Arc<Manager> {
    static MANAGER: OnceLock<Arc<Manager>> = OnceLock::new();
    MANAGER.get_or_init(|| {
        Arc::new(Manager {
            state: Mutex::new(State::default()),
            changed: tokio::sync::Notify::new(),
        })
    })
}

/// 唤醒所有排队者（容量变更/释放时调用）。
pub fn changed() {
    manager().changed.notify_waiters();
}

/// 准入票据：Drop 即释放并发计数（对齐参考树 `Ticket`）。
pub struct Ticket {
    manager: Arc<Manager>,
    class: Class,
    provider: String,
    caller: String,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let index = self.class.index();
        state.active[index] = state.active[index].saturating_sub(1);
        decrement(&mut state.providers, (index, self.provider.clone()));
        decrement(&mut state.callers, (index, self.caller.clone()));
        drop(state);
        self.manager.changed.notify_waiters();
    }
}

fn decrement(table: &mut HashMap<(usize, String), usize>, key: (usize, String)) {
    if let Some(count) = table.get_mut(&key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            table.remove(&key);
        }
    }
}

struct Waiting {
    manager: Arc<Manager>,
    id: u64,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        let mut state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(index) = state.waiters.iter().position(|waiter| waiter.id == self.id) {
            let waiter = state.waiters.remove(index);
            state.queue_bytes = state.queue_bytes.saturating_sub(waiter.bytes);
        }
        drop(state);
        self.manager.changed.notify_waiters();
    }
}

fn allowed(state: &State, class: Class, provider: &str, limits: &Limits) -> bool {
    if state.active[class.index()] >= limits.for_class(class) {
        return false;
    }
    if limits.memory_pressure
        && matches!(
            class,
            Class::Conversation
                | Class::Image
                | Class::Background
                | Class::Request
                | Class::ImageRequest
        )
    {
        return false;
    }
    let count = state
        .providers
        .get(&(class.index(), provider.to_string()))
        .copied()
        .unwrap_or(0);
    if class == Class::Background && count >= limits.background_per_provider {
        return false;
    }
    if matches!(class, Class::Conversation | Class::Image | Class::Request) {
        let settings = current_settings();
        if settings
            .provider_limits
            .get(provider)
            .is_some_and(|limit| count >= *limit)
        {
            return false;
        }
    }
    true
}

fn selected(state: &State, class: Class, limits: &Limits, now: Instant) -> Option<u64> {
    state
        .waiters
        .iter()
        .filter(|waiter| {
            waiter.class == class
                && waiter.deadline > now
                && allowed(state, class, &waiter.provider, limits)
        })
        .min_by_key(|waiter| {
            (
                state
                    .callers
                    .get(&(class.index(), waiter.caller.clone()))
                    .copied()
                    .unwrap_or(0),
                waiter.id,
            )
        })
        .map(|waiter| waiter.id)
}

fn enter(
    state: &mut State,
    manager: Arc<Manager>,
    class: Class,
    provider: String,
    caller: String,
) -> Ticket {
    let index = class.index();
    state.active[index] += 1;
    *state
        .providers
        .entry((index, provider.clone()))
        .or_insert(0) += 1;
    *state.callers.entry((index, caller.clone())).or_insert(0) += 1;
    Ticket {
        manager,
        class,
        provider,
        caller,
    }
}

/// 非阻塞准入：容量足够立即返回票据，否则返回 `Err(())`。
pub fn try_acquire(class: Class, provider: &str) -> Result<Ticket, ()> {
    let manager = manager().clone();
    let limits = limits();
    let caller = caller_label();
    let mut state = manager
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !allowed(&state, class, provider, &limits) {
        state.rejected = state.rejected.saturating_add(1);
        return Err(());
    }
    Ok(enter(
        &mut state,
        manager.clone(),
        class,
        provider.into(),
        caller,
    ))
}

/// 阻塞准入：容量足够立即返回，否则排队直到 `deadline` 或队列满。
pub async fn acquire(
    class: Class,
    provider: &str,
    caller: &str,
    bytes: usize,
    deadline: Instant,
) -> Result<Ticket, ()> {
    let manager = manager().clone();
    let now = Instant::now();
    if now >= deadline {
        return Err(());
    }
    let initial_limits = limits();
    let id = {
        let mut state = manager
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if allowed(&state, class, provider, &initial_limits)
            && selected(&state, class, &initial_limits, Instant::now()).is_none()
        {
            return Ok(enter(
                &mut state,
                manager.clone(),
                class,
                provider.into(),
                caller.into(),
            ));
        }
        let reservation = bytes
            .saturating_add(provider.len())
            .saturating_add(caller.len())
            .saturating_add(256);
        if state.waiters.len() >= initial_limits.queue_requests
            || state.queue_bytes.saturating_add(reservation) > initial_limits.queue_bytes
        {
            state.rejected = state.rejected.saturating_add(1);
            return Err(());
        }
        state.sequence = state.sequence.wrapping_add(1);
        let id = state.sequence;
        state.waiters.push(Waiter {
            id,
            class,
            provider: provider.into(),
            caller: caller.into(),
            bytes: reservation,
            deadline,
        });
        state.queue_bytes += reservation;
        id
    };
    let waiting = Waiting {
        manager: manager.clone(),
        id,
    };
    loop {
        let wake = manager.changed.notified();
        tokio::pin!(wake);
        wake.as_mut().enable();
        let limits = limits();
        {
            let mut state = manager
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if Instant::now() >= deadline {
                state.rejected = state.rejected.saturating_add(1);
                return Err(());
            }
            if selected(&state, class, &limits, Instant::now()) == Some(id) {
                let index = state
                    .waiters
                    .iter()
                    .position(|waiter| waiter.id == id)
                    .ok_or(())?;
                let waiter = state.waiters.remove(index);
                state.queue_bytes = state.queue_bytes.saturating_sub(waiter.bytes);
                let ticket = enter(
                    &mut state,
                    manager.clone(),
                    class,
                    waiter.provider,
                    waiter.caller,
                );
                drop(state);
                drop(waiting);
                manager.changed.notify_waiters();
                return Ok(ticket);
            }
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(()),
            _ = &mut wake => {},
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
        }
    }
}

tokio::task_local! {
    static CALLER: String;
}

/// 当前调用者标签（默认 `internal`）。
pub fn caller_label() -> String {
    CALLER
        .try_with(Clone::clone)
        .unwrap_or_else(|_| "internal".into())
}

/// 在指定调用者作用域内执行任务。
pub async fn caller_scope<F: std::future::Future>(caller: String, work: F) -> F::Output {
    CALLER.scope(caller, work).await
}

/// 管理面容量快照（配置 + 生效上限 + 实时占用）。
///
/// 同时返回 `revision`（`CapacitySettings` 的 SHA256，供前端保存时做 CAS）。
pub fn snapshot() -> Value {
    let configured = current_settings();
    let revision = revision_of(&configured);
    let resources = sample();
    let limits = effective(&configured, resources);
    let state = manager()
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    json!({
        "settings": configured,
        "revision": revision,
        "error": Value::Null,
        "effective": limits,
        "resources": resources,
        "usage": {
            "conversations": state.active[0],
            "interactive": state.active[1],
            "background": state.active[2],
            "images": state.active[3],
            "connections": state.active[4],
            "memoryBytes": state.memory,
            "queued": state.waiters.len(),
            "queueBytes": state.queue_bytes,
            "requests": state.active[5],
            "imageRequests": state.active[6],
        },
        "rejected": state.rejected,
    })
}

/// 容量设置的修订号（SHA256 十六进制），用于保存时的乐观并发校验。
pub fn revision_of(settings: &CapacitySettings) -> String {
    let text = serde_json::to_string(settings).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

// 让 `AtomicUsize` 导入不被优化警告（未来内存预算扩展使用）。
#[allow(dead_code)]
fn _reserved_atomic() -> &'static AtomicUsize {
    static CELL: AtomicUsize = AtomicUsize::new(0);
    let _ = CELL.load(Ordering::Relaxed);
    &CELL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_capacity_validates() {
        assert!(CapacitySettings::default().validate().is_ok());
    }

    #[test]
    fn capacity_rejects_out_of_range() {
        let cfg = CapacitySettings {
            max_conversations: 0,
            ..CapacitySettings::default()
        };
        assert!(cfg.validate().is_err());
        let cfg = CapacitySettings {
            queue_wait_seconds: 0,
            ..CapacitySettings::default()
        };
        assert!(cfg.validate().is_err());
        let mut provider_limits = std::collections::BTreeMap::new();
        provider_limits.insert("acme".to_string(), 0);
        let cfg = CapacitySettings {
            provider_limits,
            ..CapacitySettings::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn auto_limits_are_positive_and_manual_respected() {
        let resources = fallback_resources();
        let auto = effective(&CapacitySettings::default(), resources);
        assert!(auto.conversations >= 8);
        assert!(auto.memory_bytes >= 16 * MIB);

        let manual = CapacitySettings {
            mode: CapacityMode::Manual,
            max_conversations: 7,
            ..CapacitySettings::default()
        };
        let limits = effective(&manual, resources);
        assert_eq!(limits.conversations, 7);
    }

    #[test]
    fn try_acquire_counts_and_releases() {
        let settings = CapacitySettings {
            mode: CapacityMode::Manual,
            max_conversations: 1,
            ..CapacitySettings::default()
        };
        install_settings(settings);
        let ticket = try_acquire(Class::Request, "p").expect("first acquire ok");
        assert!(try_acquire(Class::Request, "p").is_err(), "limit is 1");
        drop(ticket);
        assert!(
            try_acquire(Class::Request, "p").is_ok(),
            "released after drop"
        );
        // 恢复默认，避免影响其它用例
        install_settings(CapacitySettings::default());
    }
}
