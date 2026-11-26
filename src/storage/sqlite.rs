//! SQLite 存储层 — 借鉴 aisix-admin/src/file_store.rs 的持久化模式
//!
//! 替代 FileStore 提供：
//! - 原子事务（无部分写入风险）
//! - 并发安全（SQLite WAL 模式处理锁）
//! - 查询能力（无需遍历文件系统）
//! - 数据完整性（无文件损坏风险）
//!
//! 表结构：单表 `kv` (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER)

use parking_lot::Mutex;
use rusqlite::Connection;

/// 把一段同步落盘放进 tokio 的 blocking 池执行。
///
/// 存储层是**同步** API（`rusqlite` + 一把 `Mutex<Connection>`），但绝大多数
/// 调用点都在 async handler 里（账号 persist / 渠道 persist / 设置保存）。直接
/// 在工作线程上拿锁写 WAL 会阻塞该 worker：并发请求下 tokio 的可用 worker
/// 被 IO 吃掉，吞吐随并发量线性掉档，且「一个慢写」会连带同一 worker 上的
/// 其它请求一起排队。
///
/// - 多线程运行时：`block_in_place` 让 tokio 立刻补一个 worker 顶上，当前任务
///   转入 blocking 池（语义仍是同步返回，写失败照常冒泡）；
/// - 单线程运行时 / 无运行时的启动早期与同步测试：原样执行（`block_in_place`
///   在 current_thread 运行时会 panic，必须绕开）。
///
/// 与 Agent2API `core::blocking::in_place` 同一口径。
fn run_blocking<T>(f: impl FnOnce() -> T) -> T {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(f)
        }
        _ => f(),
    }
}

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// SQLite 持久化 KV 存储
pub struct SqliteStore {
    pub(crate) conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// 轻量克隆（Arc 共享连接），供 FileStore::put_async 在 spawn_blocking 里落盘。
    pub(crate) fn cheap_clone(&self) -> SqliteStore {
        SqliteStore {
            conn: self.conn.clone(),
        }
    }
}

impl SqliteStore {
    /// 打开或创建 SQLite 数据库
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path)?;

        // 启用 WAL 模式以提升并发性能。
        // 说明：key 是 TEXT PRIMARY KEY，SQLite 会自动生成主键覆盖索引
        //（sqlite_autoindex），LIKE 'prefix%' 与 ORDER BY key 本就命中该索引
        //（EXPLAIN QUERY PLAN 验证为 COVERING INDEX 扫描）。此前额外建的
        // idx_kv_key 与主键自动索引完全重复，只带来写放大，故删除。
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             -- 写锁被别人持着时最多等 5 秒，而不是立刻 SQLITE_BUSY 失败。
             -- 与 Agent2API `db::apply_pragmas` 同值：网关的写路径是同步调用
             -- （账号/渠道/设置各自 put），进程内虽有 Mutex 串行化，但
             -- 「备份导出」「外部 sqlite3 巡检」「第二个实例」都会短暂持写锁，
             -- 默认 0ms 超时会把这类正常竞争直接变成一次失败的落盘。
             PRAGMA busy_timeout=5000;
             -- 临时表/排序走内存：`list(prefix)` 与分页查询会用到临时 B-tree，
             -- 默认落盘会让每次列表查询多一次文件 I/O。
             PRAGMA temp_store=MEMORY;
             -- 页缓存 20MB（负值 = KiB）：KV 表以十万行量级计，默认 2MB 缓存
             -- 在账号池 + 日志桶并发读时会反复回读同一批页。
             PRAGMA cache_size=-20000;
             CREATE TABLE IF NOT EXISTS kv (
                 key   TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             DROP INDEX IF EXISTS idx_kv_key;
             DROP INDEX IF EXISTS idx_kv_updated_at;",
        )?;

        tracing::info!("SQLite database opened: {}", path.display());

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT value FROM kv WHERE key = ?1")?;
        let mut rows = stmt.query_map([key], |row| row.get::<_, String>(0))?;

        match rows.next() {
            Some(Ok(value)) => {
                let parsed = serde_json::from_str(&value)?;
                Ok(Some(parsed))
            }
            Some(Err(e)) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// 写入 JSON 值（INSERT OR REPLACE）
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let content = serde_json::to_string(value)?;
        // 落盘走 blocking 语义：调用方（账号/渠道/设置的 persist）是同步函数，
        // 直接在工作线程上拿锁 + 写 WAL，会把 tokio worker 卡住（并发请求下
        // 直接表现为整体吞吐掉档）。`run_blocking` 在多线程运行时切到
        // blocking 池（tokio 会补一个 worker），单线程运行时/无运行时原样执行。
        // 与 Agent2API `core::blocking::in_place` 同一口径。
        run_blocking(|| -> anyhow::Result<()> {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
                rusqlite::params![key, content],
            )?;
            Ok(())
        })
    }

    /// 在给定连接上写入（供事务内多键写；`transaction` 回调持有连接锁）。
    pub(crate) fn put_conn(
        conn: &rusqlite::Connection,
        key: &str,
        value: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let content = serde_json::to_string(value)?;
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
            rusqlite::params![key, content],
        )?;
        Ok(())
    }

    /// 写入已序列化的原始 JSON 字符串（迁移旧 FileStore 数据时使用，
    /// 避免对已是 JSON 的内容二次序列化产生转义）
    pub fn put_raw(&self, key: &str, raw_json: &str) -> anyhow::Result<()> {
        run_blocking(|| -> anyhow::Result<()> {
            let conn = self.conn.lock();
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
                rusqlite::params![key, raw_json],
            )?;
            Ok(())
        })
    }

    /// 原子插入：key 已存在时不写入，返回 false。
    ///
    /// 供"check-then-put"竞态场景（如签到幂等）做真正的 CAS：
    /// INSERT ... ON CONFLICT DO NOTHING 在 SQLite 内判定存在性，
    /// 并发双写只有一个成功。
    /// 原子插入**原始 JSON 文本**（迁移用）：`ON CONFLICT DO NOTHING`，返回是否写入。
    ///
    /// 与 [`Self::put_if_absent`] 的差别只有「不做序列化往返」—— 迁移必须搬原始
    /// 字节，形状与存量值不完全一致的历史记录经 `serde_json::Value` 往返会被改写
    /// 成另一份语义（或直接跳过，静默丢数据）。
    #[allow(dead_code)] // 仅经 FileStore::put_if_absent_raw 的 sqlite→postgres 迁移调用（feature-gated）
    pub fn put_if_absent_raw(&self, key: &str, raw_json: &str) -> anyhow::Result<bool> {
        run_blocking(|| -> anyhow::Result<bool> {
            let conn = self.conn.lock();
            let n = conn.execute(
                "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())
                 ON CONFLICT(key) DO NOTHING",
                rusqlite::params![key, raw_json],
            )?;
            Ok(n > 0)
        })
    }

    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        let content = serde_json::to_string(value)?;
        let conn = self.conn.lock();
        let n = conn.execute(
            "INSERT INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())
             ON CONFLICT(key) DO NOTHING",
            rusqlite::params![key, content],
        )?;
        Ok(n > 0)
    }

    /// 判断键是否存在
    pub fn contains(&self, key: &str) -> anyhow::Result<bool> {
        let conn = self.conn.lock();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })?;
        Ok(count > 0)
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        run_blocking(|| -> anyhow::Result<()> {
            let conn = self.conn.lock();
            conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
            Ok(())
        })
    }

    /// 前缀匹配的上界：`prefix + U+10FFFF`（单点实现见 `storage::prefix_upper`）。
    ///
    /// 拿它代替 `LIKE 'prefix%'`：SQLite 的 LIKE 是否被改写索引范围取决于
    /// 大小写折叠配置，而 `key >= prefix AND key < prefix+\u{10FFFF}` 是**纯主键
    /// 范围扫**，行为确定（任何以 prefix 开头的键都落在区间内，且不含其它键）。
    fn prefix_upper(prefix: &str) -> String {
        super::prefix_upper(prefix)
    }

    /// 列出所有键（支持前缀匹配；空前缀 = 全部键）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        if prefix.is_empty() {
            let mut stmt = conn.prepare_cached("SELECT key FROM kv ORDER BY key")?;
            let keys: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            return Ok(keys);
        }
        let upper = Self::prefix_upper(prefix);
        let mut stmt =
            conn.prepare_cached("SELECT key FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key")?;
        let keys: Vec<String> = stmt
            .query_map(rusqlite::params![prefix, &upper], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// P0 性能：按 key 字典序倒序取前 `limit` 个键（前缀过滤）。
    ///
    /// `reqlog:{ts}:{id}` 的 ts 是固宽时间戳字符串（10 位 unix 秒），
    /// 字典序即时间序，倒序 = 最新优先。日志分页只需最新 page*size 条，
    /// 不必全表列键。LIMIT 已在调用方钳制 ≤ page*100，不会负数化。
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        let upper = Self::prefix_upper(prefix);
        // limit 钳到 i64 正区间：usize::MAX as i64 = -1，SQLite 负 LIMIT
        // 语义为"无限制"，会把快路径优化反转成全表扫描。
        let limit = super::clamp_limit(limit);
        let mut stmt = conn.prepare_cached(
            "SELECT key FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key DESC LIMIT ?3",
        )?;
        let keys: Vec<String> = stmt
            .query_map(rusqlite::params![prefix, &upper, limit], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// 列出前缀内、字典序在 `[from, to)` 区间的键（升序）。
    ///
    /// key 的时间戳/日期中段定长，字典序等价数值序；`key >= ? AND key < ?`
    /// 直接吃主键 autoindex 的范围扫描，把「窗口类读」从全表降为近窗。
    pub fn list_range(&self, _prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare_cached("SELECT key FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key")?;
        let keys: Vec<String> = stmt
            .query_map(rusqlite::params![from, to], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// 键区间 `[from, to)` 内**倒序**取前 `limit` 个键。
    ///
    /// 日志类读的「最新 N 条」用它：`ORDER BY key DESC LIMIT ?` 让 SQLite
    /// 在索引上倒着走、凑够 limit 就停，不再把整个前缀的键物化成 Vec。
    /// 区间直接作用于主键（无需 LIKE），确保吃的是主键索引范围扫。
    pub fn list_range_desc(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        // 与 list_latest_keys 同口径：负 LIMIT 在 SQLite 里是「无限制」。
        let limit = super::clamp_limit(limit);
        let mut stmt = conn.prepare_cached(
            "SELECT key FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key DESC LIMIT ?3",
        )?;
        let keys: Vec<String> = stmt
            .query_map(rusqlite::params![from, to, limit], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// 键区间 `[from, to)` 内**升序**取前 `limit` 个键。
    ///
    /// 容量裁剪专用：`reqlog:{ts}:{id}` 定宽时间戳使字典序等于时间序，
    /// 升序 + LIMIT 让 SQLite 在索引上正走、凑够 `limit` 即停，不再把
    /// 整个前缀的键物化后排序（原实现 10 万键 = 10 万 String + 排序）。
    pub fn list_range_asc(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        // 与 list_range_desc 同口径：负 LIMIT 在 SQLite 里是「无限制」。
        let limit = super::clamp_limit(limit);
        let mut stmt = conn.prepare_cached(
            "SELECT key FROM kv WHERE key >= ?1 AND key < ?2 ORDER BY key ASC LIMIT ?3",
        )?;
        let keys: Vec<String> = stmt
            .query_map(rusqlite::params![from, to, limit], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// 键区间 `[from, to)` 内的键数量（`COUNT(*)` 走索引，不物化键）。
    pub fn count_range(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare_cached("SELECT COUNT(*) FROM kv WHERE key >= ?1 AND key < ?2")?;
        let count: i64 = stmt.query_row(rusqlite::params![from, to], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    /// 前缀内的键数量（`COUNT(*)`，不物化键；同样用主键区间而非 LIKE）。
    pub fn count_prefix(&self, prefix: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let upper = Self::prefix_upper(prefix);
        let mut stmt =
            conn.prepare_cached("SELECT COUNT(*) FROM kv WHERE key >= ?1 AND key < ?2")?;
        let count: i64 = stmt.query_row(rusqlite::params![prefix, &upper], |row| row.get(0))?;
        Ok(count.max(0) as usize)
    }

    /// 列出所有键值对（支持前缀匹配）
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn list_pairs<T: DeserializeOwned>(
        &self,
        prefix: &str,
    ) -> anyhow::Result<Vec<(String, T)>> {
        let conn = self.conn.lock();
        let pattern = format!("{prefix}%");
        let mut stmt =
            conn.prepare_cached("SELECT key, value FROM kv WHERE key LIKE ?1 ORDER BY key")?;
        let pairs: Vec<(String, T)> = stmt
            .query_map([&pattern], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(k, v)| serde_json::from_str(&v).ok().map(|val| (k, val)))
            .collect();
        Ok(pairs)
    }

    /// 原子更新（读取-修改-写入）
    /// 返回旧值（如果存在）
    ///
    /// **读与写必须持同一把连接锁**：旧实现先 `get`（取锁→放锁）再 `put`
    /// （再取锁），两段之间别的线程可以插入自己的读-改-写，于是两个并发的
    /// `update` 读到同一个旧值、各自 +1 后写回，最终只加了一次（丢更新）。
    /// 本方法的签名就是「原子更新」，调用方（如 `channel_store.update`、
    /// `user_store.update` 的底层形态）按原子语义依赖它。
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default,
        F: FnOnce(Option<T>) -> T,
    {
        // 与 `mutate` 共用同一条路径：取得连接锁后完成读、闭包、写三步。
        //
        // 返回**旧值**（本方法契约），而 `mutate` 返回写入的新值 —— 所以在闭包
        // 里把旧值 clone 出来带出去。不能改成「先 get 再 put」：那会把原子性
        // 拆成两段，两段之间别的线程可插入自己的读-改-写（正是上面注释说的丢更新）。
        let previous: std::sync::Arc<parking_lot::Mutex<Option<T>>> =
            std::sync::Arc::new(parking_lot::Mutex::new(None));
        let prev_slot = previous.clone();
        self.mutate(key, move |existing| {
            *prev_slot.lock() = existing.clone();
            Ok(f(existing))
        })?;
        let out = previous.lock().clone();
        Ok(out)
    }

    /// 真正的原子读-改-写：**全程持连接锁**（读与写在同一个临界区内）。
    ///
    /// `update` 只能表达「拿旧值算新值」；需要中途报错（校验失败不落盘）或
    /// 需要按前缀读多键的路径用这个。返回写入的新值。
    ///
    /// 跨进程仍由 SQLite 自身的事务/锁保证（单条 `INSERT OR REPLACE` 是原子
    /// 语句），但「读-判定-写」这整段只在本进程内互斥——多实例部署时两个
    /// 进程各自持自己的 `Mutex<Connection>`，仍会互相覆盖。真正跨进程的
    /// 读-改-写要走 `transaction()`。
    pub fn mutate<T, F>(&self, key: &str, f: F) -> anyhow::Result<T>
    where
        T: DeserializeOwned + Serialize + Clone,
        F: FnOnce(Option<T>) -> anyhow::Result<T>,
    {
        let conn = self.conn.lock();
        let existing: Option<T> = {
            let mut stmt = conn.prepare_cached("SELECT value FROM kv WHERE key = ?1")?;
            let mut rows = stmt.query_map([key], |row| row.get::<_, String>(0))?;
            match rows.next() {
                Some(Ok(value)) => Some(serde_json::from_str(&value)?),
                Some(Err(e)) => return Err(e.into()),
                None => None,
            }
        };
        let new_value = f(existing)?;
        let content = serde_json::to_string(&new_value)?;
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
            rusqlite::params![key, content],
        )?;
        Ok(new_value)
    }

    /// 遍历全部键值（迁移用）：不反序列化为具体类型，直接给原始 JSON 文本。
    ///
    /// 迁移必须走原始文本：`get::<serde_json::Value>()` 对**形状漂移**的存量
    /// 值（如某条历史记录少字段/多字段）会静默反序列化成功但语义已变，对
    /// 读不出来的则整条跳过 —— 迁移静默丢数据。这里只搬运字节。
    #[allow(dead_code)] // 仅 sqlite→postgres 迁移（feature-gated）与测试调用；默认构建无调用方
    pub fn raw_pairs(&self) -> anyhow::Result<Vec<(String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT key, value FROM kv ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// 在事务中执行多个操作。
    ///
    /// 事务是**连接级状态**：`BEGIN IMMEDIATE` 之后若闭包 panic（内部 `unwrap`
    /// / 断言失败），连接上会留下一个没人结束的写事务。parking_lot 的 Mutex
    /// 不做 poisoning，下一次拿锁的调用会在**这个还开着的事务里**继续执行：它的
    /// 写会跟着那个事务一起悬着（别人读不到、进程退出才回滚），而它自己若再
    /// `BEGIN` 就直接报 "cannot start a transaction within a transaction"，此后
    /// 所有写路径（含 `write_batch` 的日志聚合落盘）永久失败。用 Drop 守卫保证
    /// ——不是正常 COMMIT 结束，就一定 ROLLBACK。
    pub fn transaction<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Connection) -> anyhow::Result<R>,
    {
        struct RollbackOnDrop<'a> {
            conn: &'a Connection,
            finished: bool,
        }
        impl Drop for RollbackOnDrop<'_> {
            fn drop(&mut self) {
                if !self.finished {
                    // 收尾失败也不能再抛：本守卫在 panic 展开期间运行，
                    // 抛第二个 panic 会直接 abort 进程。
                    let _ = self.conn.execute("ROLLBACK", []);
                }
            }
        }

        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;
        let mut guard = RollbackOnDrop {
            conn: &conn,
            finished: false,
        };
        match f(&conn) {
            Ok(result) => {
                // COMMIT 失败（例如写锁被外部进程抢走）时事务仍然是开着的：
                // finished 不置位，交给 Drop 的 ROLLBACK 收拾。
                conn.execute("COMMIT", [])?;
                guard.finished = true;
                Ok(result)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                guard.finished = true;
                Err(e)
            }
        }
    }

    /// 返回数据库中总条目数
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn count(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock();
        // rusqlite 0.41 的 FromSql 未实现 u64（仅 u16 等），COUNT(*) 在 SQLite
        // 返回 i64，读成 i64 再转 u64（COUNT 恒非负，转换安全）。
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM kv", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    /// 压缩数据库（回收空间）
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn vacuum(&self) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute("VACUUM", [])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn new_test_store() -> (TempDir, SqliteStore) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let store = SqliteStore::open(path).unwrap();
        (dir, store)
    }

    #[test]
    fn test_put_and_get() {
        let (_dir, store) = new_test_store();
        store.put("test_key", &"hello").unwrap();
        let val: Option<String> = store.get("test_key").unwrap();
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn test_get_nonexistent() {
        let (_dir, store) = new_test_store();
        let val: Option<String> = store.get("nonexistent").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_list_range_half_open_interval() {
        let (_dir, store) = new_test_store();
        for k in [
            "dailyagg:d:2026-10-30",
            "dailyagg:d:2026-10-31",
            "dailyagg:d:2026-11-01",
            "dailyagg:d:2026-11-02",
            "dailyagg:u:alice:2026-11-01",
        ] {
            store.put(k, &1).unwrap();
        }
        // from 含键、to 开区间排除
        let keys = store
            .list_range(
                "dailyagg:d:",
                "dailyagg:d:2026-10-31",
                "dailyagg:d:2026-11-02",
            )
            .unwrap();
        assert_eq!(keys, vec!["dailyagg:d:2026-10-31", "dailyagg:d:2026-11-01"]);
        // 空区间
        let empty = store
            .list_range(
                "dailyagg:d:",
                "dailyagg:d:2026-12-01",
                "dailyagg:d:2026-12-31",
            )
            .unwrap();
        assert!(empty.is_empty());
        // 前缀过滤生效：跨整个前缀族但排除其他前缀
        let all = store
            .list_range("dailyagg:", "dailyagg:", "dailyagg:~")
            .unwrap();
        assert_eq!(all.len(), 5);
        let other = store.list("reqlog:").unwrap();
        assert!(other.is_empty());
    }

    #[test]
    fn test_delete() {
        let (_dir, store) = new_test_store();
        store.put("to_delete", &"value").unwrap();
        store.delete("to_delete").unwrap();
        let val: Option<String> = store.get("to_delete").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_list_with_prefix() {
        let (_dir, store) = new_test_store();
        store.put("user:alice", &"data1").unwrap();
        store.put("user:bob", &"data2").unwrap();
        store.put("admin:root", &"data3").unwrap();

        let user_keys = store.list("user:").unwrap();
        assert_eq!(user_keys.len(), 2);
        assert!(user_keys.contains(&"user:alice".to_string()));
        assert!(user_keys.contains(&"user:bob".to_string()));
    }

    #[test]
    fn test_update() {
        let (_dir, store) = new_test_store();
        store.put("counter", &0u64).unwrap();

        let old: Option<u64> = store
            .update("counter", |val| match val {
                Some(n) => n + 1,
                None => 1,
            })
            .unwrap();
        assert_eq!(old, Some(0));

        let val: Option<u64> = store.get("counter").unwrap();
        assert_eq!(val, Some(1));
    }

    #[test]
    fn test_transaction_rollback() {
        let (_dir, store) = new_test_store();
        store.put("key1", &"val1").unwrap();

        let result: anyhow::Result<()> = store.transaction(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
                rusqlite::params!["key2", "val2"],
            )?;
            anyhow::bail!("intentional error");
        });
        assert!(result.is_err());

        // key2 不应该存在
        let val: Option<String> = store.get("key2").unwrap();
        assert_eq!(val, None);
    }

    /// 闭包 panic 后事务必须已回滚，连接仍可继续写。
    ///
    /// 旧实现在 `f(&conn)` panic 时**什么都不做**：`BEGIN IMMEDIATE` 留下一个
    /// 没人结束的写事务。parking_lot 的 Mutex 不 poison，下一个调用方会在这个
    /// 还开着的事务里干活（它的写跟着悬着），而它若再 `BEGIN` 就报
    /// "cannot start a transaction within a transaction" —— 此后**所有**批量写
    /// （含日志聚合 `write_batch`）永久失败，只能重启进程。
    #[test]
    fn transaction_panic_rolls_back_and_leaves_connection_usable() {
        let (_dir, store) = new_test_store();
        store.put("key1", &"val1").unwrap();

        let store = std::sync::Arc::new(store);
        let s2 = store.clone();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s2.transaction(|conn| -> anyhow::Result<()> {
                conn.execute(
                    "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
                    rusqlite::params!["panic_key", "v"],
                )?;
                panic!("boom");
            })
            .ok();
        }));
        assert!(panicked.is_err());

        // 未提交的那条必须回滚
        let leaked: Option<String> = store.get("panic_key").unwrap();
        assert_eq!(leaked, None, "panic 后未提交的写必须回滚");
        // 连接必须仍可用：能再开事务（旧实现这里报 "cannot start a transaction"）
        store
            .transaction(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
                    rusqlite::params!["after_panic", "\"ok\""],
                )?;
                Ok(())
            })
            .expect("panic 之后事务必须仍能开启（连接不能留在未结束事务里）");
        let v: Option<String> = store.get("after_panic").unwrap();
        assert_eq!(v.as_deref(), Some("ok"));
    }

    /// 并发 `update` 不丢更新（读与写必须在同一段连接锁内）。
    #[test]
    fn concurrent_update_does_not_lose_increments() {
        let (_dir, store) = new_test_store();
        store.put("counter", &0u64).unwrap();
        let store = std::sync::Arc::new(store);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .update("counter", |v| {
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            v.unwrap_or(0) + 1
                        })
                        .unwrap();
                });
            }
        });
        let final_v: Option<u64> = store.get("counter").unwrap();
        assert_eq!(final_v, Some(16), "并发自增丢了更新：{final_v:?}");
    }

    /// `limit = 0` 返回空集（与 JSON 后端一致；旧实现 clamp 到 1，会返回 1 条）。
    #[test]
    fn limit_zero_returns_empty() {
        let (_dir, store) = new_test_store();
        for i in 0..3 {
            store.put(&format!("k:{i}"), &i).unwrap();
        }
        assert!(store.list_latest_keys("k:", 0).unwrap().is_empty());
        assert!(store
            .list_range_asc("k:", "k:\u{10FFFF}", 0)
            .unwrap()
            .is_empty());
        assert!(store
            .list_range_desc("k:", "k:\u{10FFFF}", 0)
            .unwrap()
            .is_empty());
        // 对照：非零 limit 仍正常返回（证明上面不是「恒空」）
        assert_eq!(store.list_latest_keys("k:", 1).unwrap().len(), 1);
    }
}
