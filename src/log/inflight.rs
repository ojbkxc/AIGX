//! 在途请求登记表。
//!
//! Agent2API 的请求日志能「看到正在跑的请求并终止它」。AIGX 的 `RequestLog`
//! 只在请求**结束后**落一条，过程中完全不可见 —— 卡住的长流只能等超时。
//!
//! 这里维护一份进程内的「正在进行的请求」快照：
//! - 请求开始 → [`InflightRegistry::begin`] 拿到一个 `id` 与取消句柄
//! - 请求结束（成功/失败/客户端断连）→ guard 的 Drop 自动注销
//! - 管理员调 `POST /api/logs/requests/:id/terminate` → 触发取消，上游流被 drop
//!
//! ## 为什么用进程内表而不是持久化
//!
//! 在途请求本来就是「此刻活着」的概念：进程重启后它们全都不存在了。
//! 落库反而会留下永远清不掉的幽灵行。
//!
//! ## 取消语义
//!
//! 取消通过 [`CancelSignal`] 传递，数据面在流式循环里 select 它。
//! 客户端断连（`GuardedStream` 被 drop）也走同一条注销路径。

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// 一条在途请求的公开信息（列表用）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct InflightEntry {
    pub id: String,
    pub user_id: Option<String>,
    pub key_id: Option<String>,
    pub model: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    /// 开始时刻（unix 秒）
    pub started_at: i64,
    /// 已运行毫秒
    pub elapsed_ms: u64,
    /// 是否流式
    pub stream: bool,
}

/// 取消信号：`AtomicBool` 供轮询，`Notify` 供阻塞等待（二选一用法皆可）。
///
/// 不用 `tokio_util::sync::CancellationToken` 是为了不新增依赖 —— 数据面本来
/// 就在 `select!` 里同时等流与信号，`Notify::notified()` 直接可用。
#[derive(Clone, Default)]
pub struct CancelSignal {
    flag: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
}

impl CancelSignal {
    pub fn new() -> Self {
        Self::default()
    }

    /// 触发取消（幂等）。
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// 是否已取消（非阻塞）。
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    /// 等待取消（可放进 `tokio::select!`）。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

struct Entry {
    user_id: Option<String>,
    key_id: Option<String>,
    model: String,
    channel_id: Option<String>,
    channel_name: Option<String>,
    started_at: std::time::Instant,
    started_unix: i64,
    stream: bool,
    cancel: CancelSignal,
}

/// 在途请求登记表（进程内）。
#[derive(Default)]
pub struct InflightRegistry {
    entries: DashMap<String, Entry>,
    seq: AtomicU64,
}

impl InflightRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条在途请求，返回 (id, guard)。
    ///
    /// guard 被 drop 时自动注销 —— 无论请求是正常完成、报错还是客户端断连。
    pub fn begin(
        self: &Arc<Self>,
        model: impl Into<String>,
        stream: bool,
        user_id: Option<String>,
        key_id: Option<String>,
    ) -> (String, InflightGuard) {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let id = format!("req-{}-{}", chrono::Utc::now().timestamp_millis(), n);
        let cancel = CancelSignal::new();
        self.entries.insert(
            id.clone(),
            Entry {
                user_id,
                key_id,
                model: model.into(),
                channel_id: None,
                channel_name: None,
                started_at: std::time::Instant::now(),
                started_unix: chrono::Utc::now().timestamp(),
                stream,
                cancel: cancel.clone(),
            },
        );
        (
            id.clone(),
            InflightGuard {
                registry: self.clone(),
                id,
                cancel,
            },
        )
    }

    /// 当前在途请求快照（按开始时间升序 = 最早的在前）。
    ///
    /// 排序键用 `Instant`（纳秒精度）而不是公开字段 `started_at`（unix 秒）：
    /// 同一秒内创建的多条请求，用秒做键顺序不确定，列表会随机抖动。
    pub fn list(&self) -> Vec<InflightEntry> {
        let mut pairs: Vec<(std::time::Instant, InflightEntry)> = self
            .entries
            .iter()
            .map(|kv| {
                let e = kv.value();
                (
                    e.started_at,
                    InflightEntry {
                        id: kv.key().clone(),
                        user_id: e.user_id.clone(),
                        key_id: e.key_id.clone(),
                        model: e.model.clone(),
                        channel_id: e.channel_id.clone(),
                        channel_name: e.channel_name.clone(),
                        started_at: e.started_unix,
                        elapsed_ms: e.started_at.elapsed().as_millis() as u64,
                        stream: e.stream,
                    },
                )
            })
            .collect();
        pairs.sort_by_key(|(at, _)| *at);
        pairs.into_iter().map(|(_, e)| e).collect()
    }

    /// 终止一条在途请求。返回是否找到。
    pub fn terminate(&self, id: &str) -> bool {
        match self.entries.get(id) {
            Some(e) => {
                e.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// 当前在途数量。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn remove(&self, id: &str) {
        self.entries.remove(id);
    }
}

/// 在途请求的生命周期守卫：drop 即注销。
pub struct InflightGuard {
    registry: Arc<InflightRegistry>,
    id: String,
    cancel: CancelSignal,
}

impl InflightGuard {
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 取消信号（数据面在流式循环里 select 它，收到即中断）。
    pub fn cancel_signal(&self) -> CancelSignal {
        self.cancel.clone()
    }

    /// 选路完成后回填渠道信息（供列表展示）。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn set_channel(
        &self,
        registry: &InflightRegistry,
        channel_id: Option<String>,
        channel_name: Option<String>,
    ) {
        if let Some(mut e) = registry.entries.get_mut(&self.id) {
            e.channel_id = channel_id;
            e.channel_name = channel_name;
        }
    }
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.registry.remove(&self.id);
    }
}

/// 响应产生前客户端断开的终态码。
///
/// AIGX 统一口径 = **499**（client closed before first event），与
/// `AttemptStartGuard`（`src/api/openai/billing.rs`）落的那条一致。**不要**
/// 用 408：在 AIGX 里 408 的语义是「上游超时」，同一个「客户端响应前断开」
/// 事件若落 408 会让统计口径分裂并误导排障。
pub const CLIENT_CLOSED_STATUS: u16 = 499;

/// 客户端在**响应尚未产出**时断开的错误摘要。
///
/// 与 `AttemptStartGuard` 的 499 分支逐字相同（`write_attempt_499_log`
/// 写的就是这句），两条兜底在报表里能一眼归为一类。
pub const CLIENT_CLOSED: &str = "client closed before first event";

/// 响应前断线的**日志兜底守卫**（499）。
///
/// ── 它解决什么 ──────────────────────────────────────────────
/// `InflightGuard` 只管在途列表的注销，**不写日志**；chat / anthropic 的
/// 「响应前断线」由 `AttemptStartGuard` 的 499 兜底覆盖。但原生 Responses
/// 透传端点两样都没有：handler 在写出任何终态日志之前被取消（客户端断连）
/// 时，这条请求在统计里彻底消失。
///
/// 本守卫把「handler 被丢弃」变成第三条收尾路径（与 Agent2API 同义）：
/// Drop 时若未显式 [`disarm`](Self::disarm)，补一条 status=499 的请求日志。
///
/// ── 与 AttemptStartGuard 的关系（不可并存）────────────────────
/// `AttemptStartGuard` 需要一条已建的 `Reservation` 才能在 Drop 里
/// `settle_usage`；本守卫只写日志、不结算，专供**没有两段式预留**的端点
/// （当前仅 responses.rs 的透传/降级链路）。两端点各用各的，**同一条请求
/// 只会武装其中一个**，不会出现 499 双写。
///
/// ── 三条出口 ────────────────────────────────────────────────
///   - 正常记账（非流式 / 转发失败）：写完终态日志后 [`disarm`](Self::disarm)；
///   - 流式：**在返回 Response 之前** `disarm` —— 从这一刻起收尾归流的
///     计费守卫（`ResponsesStreamGuard` / `StreamUsageGuard`），它们随流
///     Drop 落日志；
///   - 什么都不做（Drop）：handler 被取消 → 补 499 终态。
///
/// ── 幂等 ────────────────────────────────────────────────────
/// `pending: Option<_>` + `disarm()` 把「补写」与「正常收尾」互斥：一条
/// 请求至多写一次（`take()` 后 `pending` 为 None，Drop 不再写）。
pub struct DisconnectLogGuard {
    log_store: Arc<super::LogStore>,
    started_at: std::time::Instant,
    /// 待补的日志模板；`None` = 已正常收尾，不再兜底。
    pending: Option<super::RequestLog>,
}

impl DisconnectLogGuard {
    /// 在「转发开始前」创建（此刻已知 request_id / 模型 / 用户 / 客户端）。
    pub fn new(
        log_store: Arc<super::LogStore>,
        log: super::RequestLog,
        started_at: std::time::Instant,
    ) -> Self {
        Self {
            log_store,
            started_at,
            pending: Some(log),
        }
    }

    /// 正常收尾：解除兜底，绝不重复写（幂等）。
    pub fn disarm(&mut self) {
        self.pending = None;
    }
}

impl Drop for DisconnectLogGuard {
    fn drop(&mut self) {
        if let Some(mut log) = self.pending.take() {
            log.status_code = CLIENT_CLOSED_STATUS;
            if log.error_msg.is_none() {
                log.error_msg = Some(CLIENT_CLOSED.to_string());
            }
            log.latency_ms = self.started_at.elapsed().as_millis() as u64;
            // 走日志旁路：有 runtime 时 try_send（Drop 里绝不阻塞请求线程），
            // 无 runtime（单测）时同步降级落库。
            self.log_store.record_request(log);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_registers_and_drop_removes() {
        let reg = Arc::new(InflightRegistry::new());
        assert!(reg.is_empty());
        {
            let (_id, _guard) = reg.begin("gpt-4o", true, Some("u1".into()), None);
            assert_eq!(reg.len(), 1);
            let list = reg.list();
            assert_eq!(list[0].model, "gpt-4o");
            assert!(list[0].stream);
        }
        // guard 离开作用域 → 自动注销
        assert!(reg.is_empty());
    }

    #[test]
    fn terminate_signals_cancel() {
        let reg = Arc::new(InflightRegistry::new());
        let (id, guard) = reg.begin("m", false, None, None);
        assert!(!guard.cancel_signal().is_cancelled());
        assert!(reg.terminate(&id));
        assert!(guard.cancel_signal().is_cancelled());
        // 未知 id 返回 false
        assert!(!reg.terminate("nope"));
    }

    #[test]
    fn list_sorted_by_start_time() {
        let reg = Arc::new(InflightRegistry::new());
        let (_a, _ga) = reg.begin("first", false, None, None);
        std::thread::sleep(std::time::Duration::from_millis(10));
        let (_b, _gb) = reg.begin("second", false, None, None);
        let list = reg.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].model, "first");
        assert_eq!(list[1].model, "second");
    }

    /// 取消后 `cancelled()` 立刻返回（供数据面 select! 使用）。
    #[tokio::test]
    async fn cancel_signal_wakes_waiter() {
        let sig = CancelSignal::new();
        let s2 = sig.clone();
        let waiter = tokio::spawn(async move {
            s2.cancelled().await;
            true
        });
        // 让 waiter 先挂到 Notify 上
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        sig.cancel();
        let woke = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("取消后 waiter 必须在 1s 内被唤醒")
            .expect("waiter 不应 panic");
        assert!(woke);
    }

    /// 已经取消过的信号，再调 `cancelled()` 不挂起（幂等）。
    #[tokio::test]
    async fn cancelled_returns_immediately_when_already_cancelled() {
        let sig = CancelSignal::new();
        sig.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), sig.cancelled())
            .await
            .expect("已取消的信号不应挂起");
    }

    /// terminate 之后，从登记表里仍能找到该条目（等请求自己收尾注销），
    /// 但信号已置位 —— 这是「终止」与「注销」的分工。
    #[test]
    fn terminate_does_not_remove_entry_immediately() {
        let reg = Arc::new(InflightRegistry::new());
        let (id, guard) = reg.begin("m", true, None, None);
        assert!(reg.terminate(&id));
        // 条目仍在（由请求自己的 guard drop 注销，避免与 terminate 竞争）
        assert_eq!(reg.len(), 1);
        assert!(guard.cancel_signal().is_cancelled());
    }

    /// 499 补数：未 disarm 的守卫在 Drop 时补一条 499；已 disarm 的一条不写。
    /// 无 tokio runtime（`#[test]`）时 `LogWriter` 同步降级，drop 后即可读库。
    #[test]
    fn disconnect_guard_writes_499_only_when_armed() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        let log_store = Arc::new(crate::log::LogStore::new(store.clone()));

        // 1) 断线（未 disarm）→ 补一条 499
        {
            let mut log = crate::log::RequestLog::new();
            log.model = "m".to_string();
            log.request_id = Some("req-drop".to_string());
            let _guard = DisconnectLogGuard::new(log_store.clone(), log, std::time::Instant::now());
            // 离开作用域 = handler 被取消
        }

        // 2) 正常收尾（disarm 后 drop）→ 不补
        {
            let mut log = crate::log::RequestLog::new();
            log.model = "m".to_string();
            log.request_id = Some("req-ok".to_string());
            let mut guard =
                DisconnectLogGuard::new(log_store.clone(), log, std::time::Instant::now());
            guard.disarm();
        }

        let keys = store.list("reqlog:").unwrap();
        assert_eq!(keys.len(), 1, "只有未 disarm 的那条会落库（幂等）");
        let log: crate::log::RequestLog = store.get(&keys[0]).unwrap().expect("日志应可反序列化");
        assert_eq!(log.status_code, CLIENT_CLOSED_STATUS);
        assert_eq!(log.status_code, 499, "统一回 AIGX 既有口径 499，不用 408");
        assert_eq!(log.request_id.as_deref(), Some("req-drop"));
        assert_eq!(log.error_msg.as_deref(), Some(CLIENT_CLOSED));
        assert_eq!(
            log.error_msg.as_deref(),
            Some("client closed before first event")
        );
    }
}
