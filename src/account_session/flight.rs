//! 凭证刷新的**单飞原语**：同一份凭据的并发刷新只发一次真实请求。
//!
//! ── 为什么这对 AIGX 是正确性问题而非优化 ────────────────────
//! CodeArts / OfficeAce 的 `refresh_token` 是**一次性**的：换证成功那一刻旧串
//! 作废（重放会得到 `the refresh token has been used`，严重时连带整个授权作废）。
//! 而 AIGX 有两处会触发刷新：10 分钟一轮的巡检（`refresh_expiring`）与管理面
//! 的强制刷新端点（`POST /api/account-sessions/:id/refresh`，`force=true`）。
//! 两者撞在一起、或用户连点两次，就会对同一串 refresh_token 打两次上游 ——
//! CAS 只能保证「写回只有一次生效」，**拦不住令牌被消耗两次**。所以并发必须
//! 在发请求之前就收口。
//!
//! ── 两个必须一起关掉的坑（语义抄 Agent2API `providers/refresh_flight.rs`）──
//!   1. **结果不留在表里当缓存**。本模块**没有 Done 槽**：结果只经 `Arc<Cell>`
//!      交给**本轮已加入的等待者**，表项在本轮结束时（成功 / 失败 / 取消）移除。
//!      因此 `force=true`（401 后的强制刷新）在上一轮结束后一定发起新请求，
//!      失败也不会被缓存成下一轮的结果。
//!   2. **取消必须释放占位并唤醒等待者**。leader 的 future 随时可能被 drop
//!      （客户端断开、超时取消）。leader 持有 RAII 守卫，无论正常结束、`?`
//!      提前返回还是 future 被 drop，`Drop` 都会移除占位并唤醒等待者 ——
//!      否则占位永久留在表里，后续请求全部走等待循环直到超时，且**再也刷不了**。
//!
//! ── 纪律 ────────────────────────────────────────────────────
//!   - **不在 await 点持有 `MutexGuard`**：所有表操作都在同步函数里完成，
//!     锁在函数返回前释放（否则 future 不是 `Send`，转发跑在多线程运行时上）。
//!   - **先移除表项、再写结果/发信号**：这样「结果可读」蕴含「表项已释放」。
//!   - **key 绝不打印**：key 含凭据指纹，只作为 `HashMap` 的键；日志只打账号 id。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::sync::watch;

/// 等一次并发刷新的上限（兜底：leader 卡住时等待者不至于挂死）。
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// leader 被取消（future drop）时等待者看到的错误文案。
pub const CANCELLED_MESSAGE: &str = "刷新已被取消，请稍后重试";
/// 等待超时的错误文案。
pub const WAIT_TIMEOUT_MESSAGE: &str = "该账号正在刷新中，请稍后重试";

/// 取锁（中毒时取回内层：刷新失败不该被另一处 panic 连坐成「整个池子不可用」）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 凭证指纹：截断的 SHA-256 十六进制（32 字符）。
///
/// 只用于单飞表的 key —— 既让「同一凭据」稳定命中同一格，又不在 key 里长期
/// 持有完整 token。它与 token 同等敏感：**绝不写进日志或错误文案**。
pub fn fingerprint(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(secret.as_bytes());
    let mut out = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 构造单飞 key：provider + 账号 id + 凭据指纹。
///
/// 带上指纹是为了「凭据被换成另一份」（重新登录 / 手工粘贴）时不会与旧的飞行
/// 复用同一格。
pub fn key(provider: &str, account_id: &str, secret: &str) -> String {
    format!("{provider}:{account_id}:{}", fingerprint(secret))
}

/// 一格飞行：结果槽 + 唤醒信号。
struct Cell<T> {
    result: Mutex<Option<Result<T, String>>>,
    signal: watch::Sender<u64>,
}

impl<T> Cell<T> {
    fn new() -> Self {
        let (signal, _) = watch::channel(0u64);
        Self {
            result: Mutex::new(None),
            signal,
        }
    }
}

impl<T: Clone> Cell<T> {
    /// 读一眼结果槽（锁在语句内释放，不跨 await）。
    fn peek(&self) -> Option<Result<T, String>> {
        let slot = lock(&self.result);
        slot.clone()
    }
}

/// 进程级单飞表：key → 正在进行的飞行。
pub struct Table<T> {
    cells: Mutex<HashMap<String, Arc<Cell<T>>>>,
}

impl<T> Default for Table<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Table<T> {
    pub const fn new() -> Self {
        Self {
            cells: Mutex::new(HashMap::new()),
        }
    }

    /// 加入（或发起）一次飞行。
    pub fn join(self: &'static Self, key: &str) -> Join<T> {
        let mut cells = lock(&self.cells);
        if let Some(cell) = cells.get(key) {
            // 订阅先于返回：leader 结束时的信号一定发生在「表项移除」之后，
            // 因此「join 时表项还在」蕴含「订阅早于这次信号」，不会错过唤醒。
            let rx = cell.signal.subscribe();
            return Join::Waiter(Waiter {
                cell: cell.clone(),
                rx,
            });
        }
        let cell = Arc::new(Cell::new());
        cells.insert(key.to_string(), cell.clone());
        Join::Leader(Leader {
            key: key.to_string(),
            cell,
            table: self,
            published: false,
        })
    }

    /// 移除一格（仅当仍是本轮那一格：避免误删后来者的新飞行）。
    fn release(&self, key: &str, cell: &Arc<Cell<T>>) {
        let mut cells = lock(&self.cells);
        if let Some(current) = cells.get(key) {
            if Arc::ptr_eq(current, cell) {
                cells.remove(key);
            }
        }
    }
}

/// 一次飞行的加入结果。
pub enum Join<T> {
    /// 本次是 leader：负责真正执行操作并 `finish`。
    Leader(Leader<T>),
    /// 已有同 key 的飞行：等它的结果。
    Waiter(Waiter<T>),
}

/// leader：结束（含被 drop 取消）时释放占位并唤醒等待者。
pub struct Leader<T> {
    key: String,
    cell: Arc<Cell<T>>,
    table: &'static Table<T>,
    published: bool,
}

impl<T: Clone> Leader<T> {
    /// 发布结果：**先移除表项，再写结果、发信号**。
    fn publish(&mut self, result: Result<T, String>) {
        self.table.release(&self.key, &self.cell);
        {
            let mut slot = lock(&self.cell.result);
            *slot = Some(result);
        }
        // 发信号（无接收者说明没人等，忽略错误）
        let _ = self.cell.signal.send(1);
        self.published = true;
    }

    /// 正常结束：发布结果。
    pub fn finish(mut self, result: Result<T, String>) {
        self.publish(result);
    }
}

impl<T> Drop for Leader<T> {
    fn drop(&mut self) {
        if !self.published {
            // 取消路径：占位必须释放、等待者必须被唤醒（否则这格再也刷不了）
            self.table.release(&self.key, &self.cell);
            {
                let mut slot = lock(&self.cell.result);
                if slot.is_none() {
                    *slot = Some(Err(CANCELLED_MESSAGE.to_string()));
                }
            }
            let _ = self.cell.signal.send(1);
            self.published = true;
        }
    }
}

/// 等待者：等 leader 发布结果。
pub struct Waiter<T> {
    cell: Arc<Cell<T>>,
    rx: watch::Receiver<u64>,
}

impl<T: Clone> Waiter<T> {
    pub async fn wait(mut self) -> Result<T, String> {
        let deadline = tokio::time::Instant::now() + WAIT_TIMEOUT;
        loop {
            // 先查结果槽：leader 可能已经发完信号
            let ready = self.cell.peek();
            if let Some(result) = ready {
                return result;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(WAIT_TIMEOUT_MESSAGE.to_string());
            }
            match tokio::time::timeout(remaining, self.rx.changed()).await {
                // 收到信号：回到循环顶部查结果槽
                Ok(Ok(_)) => continue,
                // sender 被 drop（leader 整个消失了）：再查一次槽，取不到就报取消
                Ok(Err(_)) => {
                    let last = self.cell.peek();
                    return last.unwrap_or_else(|| Err(CANCELLED_MESSAGE.to_string()));
                }
                // 超时
                Err(_) => return Err(WAIT_TIMEOUT_MESSAGE.to_string()),
            }
        }
    }
}

/// 按 key 单飞执行一次操作：leader 真跑，等待者复用同一结果。
///
/// 把「换证 + CAS 落盘」整段作为操作传进来 —— 等待者直接拿 leader 的结果，
/// 不会重复换证（一次性令牌因此只被消耗一次）、也不会重复写。
pub async fn fly<T, F, Fut>(table: &'static Table<T>, key: &str, operation: F) -> Result<T, String>
where
    T: Clone,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    match table.join(key) {
        Join::Waiter(waiter) => waiter.wait().await,
        Join::Leader(leader) => {
            let result = operation().await;
            leader.finish(result.clone());
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn fingerprint_is_stable_and_truncated() {
        let a = fingerprint("secret-token");
        assert_eq!(a, fingerprint("secret-token"), "同一输入要稳定");
        assert_ne!(a, fingerprint("other-token"), "不同输入要不同");
        assert_eq!(32, a.len(), "截断到 16 字节 = 32 个十六进制字符");
    }

    #[test]
    fn key_separates_provider_account_and_credential() {
        let base = key("codearts", "acct-1", "tok");
        assert_ne!(
            base,
            key("officeace", "acct-1", "tok"),
            "换 provider 要换格"
        );
        assert_ne!(base, key("codearts", "acct-2", "tok"), "换账号要换格");
        assert_ne!(base, key("codearts", "acct-1", "tok2"), "换凭据要换格");
        assert_eq!(base, key("codearts", "acct-1", "tok"), "同输入要稳定命中");
    }

    /// 单飞的核心断言：同一 key 上 N 个并发只应触发一次真实操作。
    #[tokio::test]
    async fn concurrent_flights_run_the_operation_once() {
        static TABLE: Table<u32> = Table::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let calls = Arc::clone(&calls);
            handles.push(tokio::spawn(async move {
                fly(&TABLE, "acct:once", move || {
                    let calls = Arc::clone(&calls);
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        // 悬一会儿，把后来者都挤进等待队列
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok(7u32)
                    }
                })
                .await
            }));
        }
        for handle in handles {
            let value = handle.await.expect("任务不该 panic").expect("应当成功");
            assert_eq!(7, value, "等待者要拿到 leader 的结果");
        }
        assert_eq!(1, calls.load(Ordering::SeqCst), "八个并发只该真打一次");
    }

    /// 上一轮失败**不能**被缓存成下一轮的结果。
    #[tokio::test]
    async fn a_finished_flight_is_not_reused() {
        static TABLE: Table<u32> = Table::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let first = Arc::clone(&calls);
        let result = fly(&TABLE, "acct:fail", move || {
            let first = Arc::clone(&first);
            async move {
                first.fetch_add(1, Ordering::SeqCst);
                Err("上游挂了".to_string())
            }
        })
        .await;
        assert!(result.is_err());
        let second = Arc::clone(&calls);
        let result = fly(&TABLE, "acct:fail", move || {
            let second = Arc::clone(&second);
            async move {
                second.fetch_add(1, Ordering::SeqCst);
                Ok(9u32)
            }
        })
        .await;
        assert!(result.is_ok(), "上一轮失败后，下一轮必须是新的一次尝试");
        assert_eq!(2, calls.load(Ordering::SeqCst), "第二次应当真的又跑了一次");
    }

    /// leader 被取消（future drop）时，等待者必须被唤醒而不是挂到超时。
    #[tokio::test]
    async fn cancelled_leader_wakes_the_waiter() {
        static TABLE: Table<u32> = Table::new();
        // leader：拿到后就 drop（模拟客户端断开）
        let leader = TABLE.join("acct:cancel");
        let Join::Leader(leader) = leader else {
            panic!("首个加入者应当是 leader");
        };
        // 等待者加入
        let Join::Waiter(waiter) = TABLE.join("acct:cancel") else {
            panic!("第二个加入者应当是等待者");
        };
        drop(leader);
        let result = tokio::time::timeout(Duration::from_secs(5), waiter.wait())
            .await
            .expect("等待者不该挂到超时");
        assert!(result.is_err(), "leader 被取消时等待者应拿到错误");
        // 取消后这一格必须已释放：新加入者又是 leader
        assert!(matches!(TABLE.join("acct:cancel"), Join::Leader(_)));
    }
}
