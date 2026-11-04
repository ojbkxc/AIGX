//! **日志写入旁路**：把「请求明细 + 按天聚合」的落盘从请求线程的同步
//! `put` 里摘出来，丢进一个有界队列，由后台任务批量合并写。
//!
//! ── 为什么 ────────────────────────────────────────────────
//! 改造前 `LogStore::record_request` 在请求返回路径上同步做 3 次
//! 独立写（reqlog 一条 + 聚合主档一条 + 用户行一条），每次都是一次
//! SQLite 事务/锁。请求线程因此被磁盘 IO 拖住。旁路把这些写攒成一批
//! （每 100ms 或 512 条），折叠后**一次 `write_batch`**（单事务）落库。
//!
//! ── 语义取舍 ──────────────────────────────────────────────
//! - 生产者 `try_send`：**队列满即丢弃并 warn，绝不阻塞请求线程**。
//!   丢一条日志只是报表少一个数，远好过拖慢在途请求（对齐
//!   `daily_agg` 模块头「丢统计不丢计费」的取舍）。
//! - 无 tokio runtime（单测/启动早期）时**降级为同步写**，语义与改造前
//!   逐字一致——测试里 `record_request` 后立刻读到聚合行。
//! - 优雅关闭靠 [`LogWriter::flush`] 阻塞到队列排空。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::{mpsc, Notify};

use super::daily_agg::{
    date_key, fold_request, fold_user_row, master_key, user_key, DailyAgg, UserDailyRow,
};
use super::RequestLog;
use crate::storage::FileStore;

/// 队列容量：有界，满即丢（见模块头）。
pub const DEFAULT_CAPACITY: usize = 8192;
/// 单批条数上限：攒够即提前 flush，不必等满周期。
const BATCH_MAX: usize = 512;
/// 后台 flush 周期（毫秒）。
const FLUSH_INTERVAL_MS: u64 = 100;
/// reqlog 容量兜底检查频率（与 `RequestLogStore::PURGE_CHECK_EVERY` 对齐）。
const PURGE_CHECK_EVERY: u64 = 1000;

/// 一条待落库的日志操作。
pub enum LogOp {
    /// 请求明细：落 `reqlog` 行，并折进按天聚合（记账主路径）。
    Request(RequestLog),
    /// 仅聚合、不回写明细：供只想要统计的调用方（热路径暂未使用）。
    Daily(RequestLog),
}

/// 异步批量写器。挂在 `LogStore` 内，`record_request` 的生产端。
pub struct LogWriter {
    store: Arc<FileStore>,
    /// 与 `DailyAggStore` 共享的聚合写锁（锁序：聚合锁 → 连接锁）。
    lock: Arc<Mutex<()>>,
    /// 异步模式的发送端；`None` = 无 runtime 的同步降级。
    tx: Option<mpsc::Sender<LogOp>>,
    /// 仅「无消费者」测试模式使用（手工持有接收端以保持通道存活）。
    #[allow(dead_code)]
    rx: Option<mpsc::Receiver<LogOp>>,
    /// 已入队未落库条数（`flush` 排空判定；increment 先于 send，避免下溢）。
    pending: Arc<AtomicUsize>,
    /// 唤醒后台任务立即 flush（`try_flush` / `flush` 用）。
    flush_now: Arc<Notify>,
    /// 已写入的 reqlog 条数（摊薄容量兜底检查）。
    writes: Arc<AtomicU64>,
    /// 队列容量（仅用于满队列告警文案）。
    capacity: usize,
}

impl LogWriter {
    /// 生产构造：有 runtime 时起后台批量任务，否则同步降级。
    pub fn new(store: Arc<FileStore>, daily_lock: Arc<Mutex<()>>) -> Self {
        let pending = Arc::new(AtomicUsize::new(0));
        let flush_now = Arc::new(Notify::new());
        let writes = Arc::new(AtomicU64::new(0));
        if tokio::runtime::Handle::try_current().is_err() {
            // 同步降级：send 直接落库，无队列、无后台任务。
            return Self {
                store,
                lock: daily_lock,
                tx: None,
                rx: None,
                pending,
                flush_now,
                writes,
                capacity: DEFAULT_CAPACITY,
            };
        }
        let (tx, rx) = mpsc::channel::<LogOp>(DEFAULT_CAPACITY);
        tokio::spawn(run_consumer(
            store.clone(),
            daily_lock.clone(),
            rx,
            pending.clone(),
            flush_now.clone(),
            writes.clone(),
        ));
        Self {
            store,
            lock: daily_lock,
            tx: Some(tx),
            rx: None,
            pending,
            flush_now,
            writes,
            capacity: DEFAULT_CAPACITY,
        }
    }

    /// 测试用：起通道但**不起消费者**，让队列可被填满以验证满队列丢弃路径。
    #[cfg(test)]
    pub(crate) fn new_without_consumer(
        store: Arc<FileStore>,
        daily_lock: Arc<Mutex<()>>,
        capacity: usize,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<LogOp>(capacity);
        Self {
            store,
            lock: daily_lock,
            tx: Some(tx),
            rx: Some(rx),
            pending: Arc::new(AtomicUsize::new(0)),
            flush_now: Arc::new(Notify::new()),
            writes: Arc::new(AtomicU64::new(0)),
            capacity,
        }
    }

    /// 入队一条日志操作。队列满/通道关闭时丢弃并告警，**绝不阻塞调用线程**。
    pub fn send(&self, mut op: LogOp) {
        normalize(&mut op);
        match &self.tx {
            None => {
                // 同步降级：立即落库，语义与改造前一致。
                let n = apply_ops(&self.store, &self.lock, vec![op]);
                bump_writes(&self.store, &self.writes, n);
            }
            Some(tx) => {
                // 先计数再投递：消费者先收到并 decrement 也不会让计数下溢。
                self.pending.fetch_add(1, Ordering::AcqRel);
                match tx.try_send(op) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        self.pending.fetch_sub(1, Ordering::AcqRel);
                        tracing::warn!(
                            "日志写入队列已满（容量 {}），丢弃本条日志，不阻塞请求线程",
                            self.capacity
                        );
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        self.pending.fetch_sub(1, Ordering::AcqRel);
                        tracing::warn!("日志写入通道已关闭，丢弃本条日志");
                    }
                }
            }
        }
    }

    /// 已入队但尚未落库的条数（测试与健康检查用）。
    pub fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// 非阻塞尽力 flush：只唤醒后台任务立即处理，不等待。
    pub fn try_flush(&self) {
        if self.tx.is_some() {
            self.flush_now.notify_one();
        }
    }

    /// 同步阻塞直到队列排空（供优雅关闭调用）。
    ///
    /// 无 runtime（同步降级）时立即返回——`send` 本就同步落库，
    /// 不存在未落库数据。有超时兜底（10s），避免消费者卡死时永久阻塞关闭。
    pub fn flush(&self) {
        if self.tx.is_none() {
            return;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let left = self.pending.load(Ordering::Acquire);
            if left == 0 {
                return;
            }
            self.flush_now.notify_one();
            if std::time::Instant::now() >= deadline {
                tracing::warn!("LogWriter::flush 超时，仍有 {left} 条日志未落库");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
}

/// 补 id / created_at 默认值（与原 `RequestLogStore::add` 的规整逐字一致）。
fn normalize(op: &mut LogOp) {
    let l = match op {
        LogOp::Request(l) | LogOp::Daily(l) => l,
    };
    if l.id.is_empty() {
        l.id = uuid::Uuid::new_v4().to_string();
    }
    if l.created_at == 0 {
        l.created_at = chrono::Utc::now().timestamp();
    }
}

/// 后台消费者：每 100ms 或攒够 512 条 flush 一批；通道关闭时冲刷剩余并退出。
async fn run_consumer(
    store: Arc<FileStore>,
    lock: Arc<Mutex<()>>,
    mut rx: mpsc::Receiver<LogOp>,
    pending: Arc<AtomicUsize>,
    flush_now: Arc<Notify>,
    writes: Arc<AtomicU64>,
) {
    let mut buf: Vec<LogOp> = Vec::with_capacity(BATCH_MAX);
    loop {
        let Some(first) = rx.recv().await else {
            break;
        };
        buf.push(first);
        let timer = tokio::time::sleep(std::time::Duration::from_millis(FLUSH_INTERVAL_MS));
        tokio::pin!(timer);
        let mut closed = false;
        while buf.len() < BATCH_MAX {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(op) => buf.push(op),
                    None => { closed = true; break; }
                },
                _ = &mut timer => break,
                _ = flush_now.notified() => break,
            }
        }
        let n = buf.len();
        let ops = std::mem::take(&mut buf);
        let written = apply_ops(&store, &lock, ops);
        pending.fetch_sub(n, Ordering::AcqRel);
        bump_writes(&store, &writes, written);
        if closed {
            break;
        }
        buf = Vec::with_capacity(BATCH_MAX);
    }
}

/// 把一批操作落库：
/// - `Request` 写 `reqlog` 明细，并（与 `Daily` 一样）折进按天聚合；
/// - 聚合主档按 `date` 分组、用户行按 `(uid, date)` 分组——每组只 `get`
///   一次、fold 完只 `put` 一次；
/// - 明细 + 全部聚合行合并成**一次 `write_batch`**（SQLite 单事务）。
///
/// 返回写入的 reqlog 明细条数（供容量兜底计数）。
fn apply_ops(store: &FileStore, lock: &Mutex<()>, ops: Vec<LogOp>) -> u64 {
    if ops.is_empty() {
        return 0;
    }
    // 锁序：聚合锁 → 连接锁（与 DailyAggStore/回填一致，无反向持锁）。
    let _guard = lock.lock();

    let mut entries: Vec<(String, Value)> = Vec::with_capacity(ops.len());
    let mut by_day: BTreeMap<String, Vec<&RequestLog>> = BTreeMap::new();
    let mut by_uid_day: BTreeMap<(String, String), Vec<&RequestLog>> = BTreeMap::new();
    let mut reqlog_count = 0u64;

    for op in &ops {
        match op {
            LogOp::Request(l) => {
                match serde_json::to_value(l) {
                    Ok(v) => entries.push((format!("reqlog:{}:{}", l.created_at, l.id), v)),
                    Err(e) => tracing::warn!("请求日志序列化失败，丢弃：{e}"),
                }
                reqlog_count += 1;
                group_log(&mut by_day, &mut by_uid_day, l);
            }
            LogOp::Daily(l) => group_log(&mut by_day, &mut by_uid_day, l),
        }
    }

    for (day, logs) in by_day {
        let key = master_key(&day);
        let mut agg: DailyAgg = store.get(&key).ok().flatten().unwrap_or_else(|| DailyAgg {
            date: day.clone(),
            ..Default::default()
        });
        for l in logs {
            fold_request(&mut agg, l);
        }
        match serde_json::to_value(&agg) {
            Ok(v) => entries.push((key, v)),
            Err(e) => tracing::warn!("聚合主档序列化失败（{day}）：{e}"),
        }
    }

    for ((uid, day), logs) in by_uid_day {
        let key = user_key(&uid, &day);
        let mut row: UserDailyRow =
            store
                .get(&key)
                .ok()
                .flatten()
                .unwrap_or_else(|| UserDailyRow {
                    date: day.clone(),
                    ..Default::default()
                });
        for l in logs {
            fold_user_row(&mut row, l);
        }
        match serde_json::to_value(&row) {
            Ok(v) => entries.push((key, v)),
            Err(e) => tracing::warn!("用户日行序列化失败（{uid} {day}）：{e}"),
        }
    }

    if !entries.is_empty() {
        let refs: Vec<(&str, Value)> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        if let Err(e) = store.write_batch(&refs) {
            tracing::warn!("批量写日志失败（{} 条，含聚合）：{e}", entries.len());
        }
    }
    reqlog_count
}

/// 把一条日志归入「按天」与「按用户天」分组（聚合折叠的上游）。
fn group_log<'a>(
    by_day: &mut BTreeMap<String, Vec<&'a RequestLog>>,
    by_uid_day: &mut BTreeMap<(String, String), Vec<&'a RequestLog>>,
    log: &'a RequestLog,
) {
    let day = date_key(log.created_at);
    if day.is_empty() {
        return;
    }
    by_day.entry(day.clone()).or_default().push(log);
    if let Some(uid) = &log.user_id {
        by_uid_day.entry((uid.clone(), day)).or_default().push(log);
    }
}

/// 累加 reqlog 写入计数；跨过 1000 的整数倍时跑一次容量兜底清理。
fn bump_writes(store: &FileStore, counter: &AtomicU64, n: u64) {
    if n == 0 {
        return;
    }
    let before = counter.fetch_add(n, Ordering::Relaxed);
    let after = before + n;
    if before / PURGE_CHECK_EVERY != after / PURGE_CHECK_EVERY {
        if let Err(e) = super::purge_request_overflow(store) {
            tracing::warn!("request log purge check failed: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::daily_agg::{aggregate_logs, aggregate_user_rows};
    use crate::log::RequestLog;
    use tempfile::TempDir;

    fn make_store() -> Arc<FileStore> {
        Arc::new(FileStore::new(TempDir::new().unwrap().path().to_path_buf()))
    }

    fn log(uid: Option<&str>, model: &str, cost: i64, ts: i64) -> RequestLog {
        let mut l = RequestLog::new();
        l.user_id = uid.map(str::to_string);
        l.model = model.to_string();
        l.input_tokens = 10;
        l.output_tokens = 5;
        l.cost = cost;
        l.channel_cost = cost / 2;
        l.status_code = 200;
        l.latency_ms = 100;
        l.created_at = ts;
        l
    }

    const DAY0: i64 = 1_735_689_600; // 2025-01-01T00:00:00Z
    const DAY0_STR: &str = "2025-01-01";
    const DAY1: i64 = DAY0 + 86_400; // 2025-01-02
    const DAY1_STR: &str = "2025-01-02";

    /// 1) 同一天 N 条 → 主档 requests == N，用户行 count == N（一次批量 flush）。
    #[test]
    fn batch_flush_aggregates_same_day() {
        let store = make_store();
        let lock = Mutex::new(());
        let logs: Vec<RequestLog> = (0..7)
            .map(|i| log(Some("u1"), "gpt", 10, DAY0 + i))
            .collect();
        let ops: Vec<LogOp> = logs.into_iter().map(LogOp::Request).collect();
        let written = apply_ops(&store, &lock, ops);
        assert_eq!(written, 7);

        let master = store
            .get::<DailyAgg>(&master_key(DAY0_STR))
            .unwrap()
            .expect("主档已写");
        assert_eq!(master.requests, 7);
        let row = store
            .get::<UserDailyRow>(&user_key("u1", DAY0_STR))
            .unwrap()
            .expect("用户行已写");
        assert_eq!(row.count, 7);
        // 明细也逐条落库
        assert_eq!(store.list("reqlog:").unwrap().len(), 7);
    }

    /// 2) 队列满时 try_send 失败被吞、不 panic（无消费者模式填满）。
    #[test]
    fn full_queue_drops_without_panic() {
        let store = make_store();
        let lock = Arc::new(Mutex::new(()));
        let writer = LogWriter::new_without_consumer(store, lock, 4);
        for i in 0..4 {
            writer.send(LogOp::Request(log(Some("u"), "m", 1, DAY0 + i)));
        }
        // 第 5 条起队列已满：send 内部 try_send 返回 Full，仅丢弃 + 告警，不 panic。
        for i in 4..10 {
            writer.send(LogOp::Request(log(Some("u"), "m", 1, DAY0 + i)));
        }
        // 满队列丢弃的条目不进入 pending（应在 4 条容量内）。
        assert!(writer.pending() <= 4, "pending 不应超过容量");
    }

    /// 3) flush() 调用后队列确实排空且数据落库。
    #[test]
    fn flush_drains_queue() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let store = make_store();
        let lock = Arc::new(Mutex::new(()));
        // 在 runtime 上下文内构造 → 走异步通道 + 起后台消费者。
        let writer = rt.block_on(async { LogWriter::new(store.clone(), lock.clone()) });
        for i in 0..300 {
            writer.send(LogOp::Request(log(Some("u1"), "m", 1, DAY0 + (i % 50))));
        }
        assert!(writer.pending() > 0, "入队后应有未落库计数");
        writer.flush();
        assert_eq!(writer.pending(), 0, "flush 后队列必须排空");
        assert_eq!(store.list("reqlog:").unwrap().len(), 300);
        let master = store
            .get::<DailyAgg>(&master_key(DAY0_STR))
            .unwrap()
            .expect("主档已写");
        assert_eq!(master.requests, 300);
    }

    /// 4) 跨天分组：两天各 2 条 → 两行各 == 2。
    #[test]
    fn cross_day_grouping() {
        let store = make_store();
        let lock = Mutex::new(());
        let ops = vec![
            LogOp::Request(log(Some("u1"), "m", 1, DAY0)),
            LogOp::Request(log(Some("u1"), "m", 1, DAY0 + 10)),
            LogOp::Request(log(Some("u1"), "m", 1, DAY1)),
            LogOp::Request(log(Some("u1"), "m", 1, DAY1 + 10)),
        ];
        apply_ops(&store, &lock, ops);
        let d0 = store
            .get::<DailyAgg>(&master_key(DAY0_STR))
            .unwrap()
            .unwrap();
        let d1 = store
            .get::<DailyAgg>(&master_key(DAY1_STR))
            .unwrap()
            .unwrap();
        assert_eq!(d0.requests, 2);
        assert_eq!(d1.requests, 2);
        let r0 = store
            .get::<UserDailyRow>(&user_key("u1", DAY0_STR))
            .unwrap()
            .unwrap();
        let r1 = store
            .get::<UserDailyRow>(&user_key("u1", DAY1_STR))
            .unwrap()
            .unwrap();
        assert_eq!((r0.count, r1.count), (2, 2));
    }

    /// 5) 回归：批量旁路的聚合结果与 `aggregate_logs`/`aggregate_user_rows`
    ///    现算逐字段一致（同一份 fixture，两条代码路径必须同口径）。
    #[test]
    fn matches_reference_aggregation() {
        let store = make_store();
        let lock = Mutex::new(());
        let mut hit = log(Some("u1"), "gpt-4", 40, DAY0);
        hit.cache_hit = true;
        hit.input_tokens = 1000;
        let fixture = vec![
            hit,
            log(Some("u2"), "claude", 60, DAY0 + 5),
            log(None, "gpt-4", 30, DAY1),
            log(Some("u1"), "claude", 20, DAY1 + 5),
        ];
        let ops: Vec<LogOp> = fixture.clone().into_iter().map(LogOp::Request).collect();
        apply_ops(&store, &lock, ops);

        let expect_master = aggregate_logs(&fixture);
        let expect_users = aggregate_user_rows(&fixture);

        for (day, want) in &expect_master {
            let got = store
                .get::<DailyAgg>(&master_key(day))
                .unwrap()
                .unwrap_or_else(|| panic!("主档缺失 {day}"));
            assert_eq!(
                serde_json::to_value(&got).unwrap(),
                serde_json::to_value(want).unwrap(),
                "主档 {day} 与现算不一致"
            );
        }
        for (uid, days) in &expect_users {
            for (day, want) in days {
                let got = store
                    .get::<UserDailyRow>(&user_key(uid, day))
                    .unwrap()
                    .unwrap_or_else(|| panic!("用户行缺失 {uid} {day}"));
                assert_eq!(
                    serde_json::to_value(&got).unwrap(),
                    serde_json::to_value(want).unwrap(),
                    "用户行 {uid} {day} 与现算不一致"
                );
            }
        }
    }
}
