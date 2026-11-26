//! PostgreSQL 存储层 — 通过 sqlx 异步驱动 + 独立线程桥接，提供同步 KV 接口。
//!
//! ## 三后端语义对齐（与 `sqlite.rs` / `JsonFileStore` 同一把尺）
//!
//! | 操作 | 本后端 | 对齐口径 |
//! |---|---|---|
//! | `list(prefix)` | `key COLLATE "C" >= p AND < p+U+10FFFF` | 字面前缀（不是 `LIKE`，前缀含 `_`/`%` 不过匹配） |
//! | `list_range(_, from, to)` | **纯键区间**，忽略 prefix | 与 SQLite 一致（旧实现拼了 `LIKE`，三后端三套结果） |
//! | `limit` | `clamp_limit`，`0` → 空集 | SQLite 原先 clamp 到 1；PG 原样绑 `usize::MAX` |
//! | `write_batch` | 单事务 `BEGIN..COMMIT` | 旧实现逐条提交（失败时半批已落库） |
//! | `mutate` | 单条命令内读-改-写 | 跨命令的 get+put 之间会被别的命令插入 |
//!
//! `COLLATE "C"` 是必要的：库默认 collation（`en_US.UTF-8` / `zh_CN.UTF-8`）
//! 的排序规则与「字面前缀的区间上界」不兼容——`p <= key < p+U+10FFFF` 在上界
//! 处可能被判为真而把不相干的键扫进来；`"C"` 是 bytewise，与 SQLite 的
//! BINARY 排序、JSON 后端的 `str::starts_with` 一致。
//!
//! 为什么独立线程桥接：
//! - store 的 get/put/list 被大量同步函数调用，且部分调用发生在 tokio
//!   runtime 的 async 上下文里（如 health_check 内调用 channel_store.list()）。
//! - sqlx 是纯异步 API，若用 Handle::block_on 在 async 上下文里会 panic。
//! - 独立 OS 线程内跑 current_thread runtime，通过 channel 收发命令，
//!   无论调用方处于同步还是异步上下文都安全（真正的 async 执行发生在
//!   独立的 OS 线程上，不占用调用方的 runtime 线程）。
//!
//! 表结构与 SQLite 后端对齐：单表 `kv (key TEXT PRIMARY KEY, value TEXT,
//! updated_at BIGINT)`，保证迁移与语义一致。

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

use sea_orm::sqlx;
use sea_orm::sqlx::Row;

/// 存储层命令（后台线程内执行 sqlx 异步查询）
enum Cmd {
    Get {
        key: String,
        reply: SyncSender<anyhow::Result<Option<String>>>,
    },
    Put {
        key: String,
        value: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    PutIfAbsent {
        key: String,
        value: String,
        reply: SyncSender<anyhow::Result<bool>>,
    },
    Delete {
        key: String,
        reply: SyncSender<anyhow::Result<()>>,
    },
    List {
        prefix: String,
        reply: SyncSender<anyhow::Result<Vec<String>>>,
    },
    ListLatest {
        prefix: String,
        limit: usize,
        reply: SyncSender<anyhow::Result<Vec<String>>>,
    },
    ListRange {
        prefix: String,
        from: String,
        to: String,
        reply: SyncSender<anyhow::Result<Vec<String>>>,
    },
    ListRangeAsc {
        prefix: String,
        from: String,
        to: String,
        limit: usize,
        reply: SyncSender<anyhow::Result<Vec<String>>>,
    },
    /// 多键写（单事务：中途失败整批回滚）。
    Batch {
        entries: Vec<(String, String)>,
        reply: SyncSender<anyhow::Result<()>>,
    },
    /// 原子读-改-写（actor 内串行执行，等价 `SELECT ... FOR UPDATE` 的
    /// 「同一把行锁贯穿读与写」）。闭包收到旧值（原始 JSON 文本），
    /// 返回要写入的新文本；回包带上**改写前的原始文本**（`update` 要返回旧值，
    /// 跨线程闭包不能借用调用方的栈变量）。
    Mutate {
        key: String,
        f: Box<dyn FnOnce(Option<String>) -> anyhow::Result<String> + Send>,
        reply: SyncSender<anyhow::Result<(Option<String>, String)>>,
    },
}

/// PostgreSQL 持久化 KV 存储（同步接口，内部桥接 sqlx）
pub struct PgStore {
    tx: SyncSender<Cmd>,
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl PgStore {
    /// 连接 PostgreSQL 并启动后台工作线程。
    pub fn new(url: &str, max_connections: u32) -> anyhow::Result<Self> {
        let (tx, rx) = sync_channel::<Cmd>(1024);
        let url = url.to_string();
        let max_conn = max_connections.max(1);

        std::thread::Builder::new()
            .name("aigx-pg-store".to_string())
            .spawn(move || {
                if let Err(e) = pg_worker(rx, &url, max_conn) {
                    tracing::error!("PgStore worker exited: {e}");
                }
            })
            .map_err(|e| anyhow::anyhow!("spawn pg store thread: {e}"))?;

        Ok(Self { tx })
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Get {
            key: key.to_string(),
            reply,
        })?;
        let raw = rx.recv()??;
        match raw {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    /// 写入原始 JSON 字符串（FileStore::put_async 落盘用，免二次序列化）。
    pub fn put_raw(&self, key: &str, raw: &str) -> anyhow::Result<()> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Put {
            key: key.to_string(),
            value: raw.to_string(),
            reply,
        })?;
        rx.recv()?
    }

    /// 原子插入**原始 JSON 文本**（迁移用）：key 已存在时不写入，返回 false。
    ///
    /// 与 [`Self::put_if_absent`] 的区别只在于「不经过反序列化再序列化」——
    /// 迁移必须搬运原始字节，否则形状与存量值不完全一致的历史记录会被
    /// `serde_json::Value` 往返改写成另一份语义（或直接跳过，静默丢数据）。
    pub fn put_if_absent_raw(&self, key: &str, raw: &str) -> anyhow::Result<bool> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::PutIfAbsent {
            key: key.to_string(),
            value: raw.to_string(),
            reply,
        })?;
        rx.recv()?
    }

    /// 写入 JSON 值（INSERT ... ON CONFLICT 更新）
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let content = serde_json::to_string(value)?;
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Put {
            key: key.to_string(),
            value: content,
            reply,
        })?;
        rx.recv()?
    }

    /// 原子插入：key 已存在时不写入，返回 false（签到幂等等 CAS 场景）。
    /// INSERT ... ON CONFLICT DO NOTHING 在 PostgreSQL 内判定存在性。
    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        let content = serde_json::to_string(value)?;
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::PutIfAbsent {
            key: key.to_string(),
            value: content,
            reply,
        })?;
        rx.recv()?
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Delete {
            key: key.to_string(),
            reply,
        })?;
        rx.recv()?
    }

    /// 列出所有键（支持前缀匹配，字典序升序）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::List {
            prefix: prefix.to_string(),
            reply,
        })?;
        rx.recv()?
    }

    /// 按字典序倒序取前 `limit` 个键（前缀过滤，索引范围扫描）
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::ListLatest {
            prefix: prefix.to_string(),
            limit,
            reply,
        })?;
        rx.recv()?
    }

    /// 原子更新（读取-修改-写入），返回旧值（若存在）。
    ///
    /// 旧实现是两个**独立命令**（`Cmd::Get` 然后 `Cmd::Put`）：actor 只保证
    /// 单条命令原子，两条之间会插进别的命令，两个并发的 `update` 读到同一个
    /// 旧值、各自写回，最终只算了一次（丢更新）。改为一条 `Mutate` 命令，
    /// 读与写在 actor 内不间断。
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone,
        F: FnOnce(Option<T>) -> T + Send + 'static,
    {
        let (old_raw, _new_raw) = self.mutate_raw(key, move |existing| {
            let parsed: Option<T> = match existing {
                Some(raw) => Some(serde_json::from_str(&raw)?),
                None => None,
            };
            let value = f(parsed);
            Ok(serde_json::to_string(&value)?)
        })?;
        match old_raw {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    /// 前缀内字典序 `[from, to)` 区间的键（升序，索引范围扫描）
    pub fn list_range(&self, prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::ListRange {
            prefix: prefix.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            reply,
        })?;
        rx.recv()?
    }

    /// 前缀内字典序 `[from, to)` 区间**升序**取前 `limit` 个键（索引范围扫描）。
    ///
    /// 容量裁剪用：`ORDER BY key ASC LIMIT` 在索引上正走、凑够即停，
    /// 不再把整个前缀的键物化后排序。
    pub fn list_range_asc(
        &self,
        prefix: &str,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::ListRangeAsc {
            prefix: prefix.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            limit,
            reply,
        })?;
        rx.recv()?
    }

    fn send(&self, cmd: Cmd) -> anyhow::Result<()> {
        self.tx
            .send(cmd)
            .map_err(|_| anyhow::anyhow!("pg store worker has terminated"))
    }

    /// 多键写：**单事务**（`BEGIN` → 逐条 upsert → `COMMIT`，任一条失败整批回滚）。
    ///
    /// 旧实现（`FileStore::write_batch` 在 PG 分支直接 `for ... self.put()`）
    /// 是 N 次独立提交：第 k 条失败时前 k-1 条已经落库，而调用方拿到 `Err`
    /// 后按「单事务全回滚」的语义整批重试（见 `log::writer::apply_ops` 的
    /// 注释与回队逻辑）→ 聚合行被折叠两次，日/用户报表数字虚高且无法事后
    /// 区分。SQLite 后端是真事务，两个后端语义因此不同。
    pub fn put_many_raw(&self, entries: &[(String, String)]) -> anyhow::Result<()> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Batch {
            entries: entries.to_vec(),
            reply,
        })?;
        rx.recv()?
    }

    /// 原子读-改-写（单命令，actor 内既读又写）。返回 `(改写前的原始文本, 新文本)`。
    ///
    /// 跨命令的 `get` + `put` 会被其它命令插入（actor 只保证单条命令原子），
    /// 因此「读 → 改 → 写」必须合成一条命令。
    pub fn mutate_raw(
        &self,
        key: &str,
        f: impl FnOnce(Option<String>) -> anyhow::Result<String> + Send + 'static,
    ) -> anyhow::Result<(Option<String>, String)> {
        let (reply, rx) = sync_channel(1);
        self.send(Cmd::Mutate {
            key: key.to_string(),
            f: Box::new(f),
            reply,
        })?;
        rx.recv()?
    }
}

/// 后台工作线程：独立 runtime 上执行 sqlx 异步查询。
fn pg_worker(rx: Receiver<Cmd>, url: &str, max_conn: u32) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(max_conn)
            .connect(url)
            .await
            .map_err(|e| anyhow::anyhow!("connect postgres: {e}"))?;

        // 自建表与索引（与 SQLite 后端对齐，不依赖 sea-orm migration）
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS kv (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at BIGINT NOT NULL DEFAULT 0
            )",
        )
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("create kv table: {e}"))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_kv_key ON kv(key)")
            .execute(&pool)
            .await
            .map_err(|e| anyhow::anyhow!("create kv index: {e}"))?;

        tracing::info!("PostgreSQL KV store ready (max_connections={max_conn})");

        while let Ok(cmd) = rx.recv() {
            match cmd {
                Cmd::Get { key, reply } => {
                    let r = async {
                        let row = sqlx::query("SELECT value FROM kv WHERE key = $1")
                            .bind(&key)
                            .fetch_optional(&pool)
                            .await
                            .map_err(|e| anyhow::anyhow!("pg get: {e}"))?;
                        Ok::<Option<String>, anyhow::Error>(
                            row.map(|r| r.get::<String, _>(0)),
                        )
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::Put { key, value, reply } => {
                    let r = sqlx::query(
                        "INSERT INTO kv (key, value, updated_at) VALUES ($1, $2, $3)
                         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
                    )
                    .bind(&key)
                    .bind(&value)
                    .bind(now_ts())
                    .execute(&pool)
                    .await
                    .map(|_| ())
                    .map_err(|e| anyhow::anyhow!("pg put: {e}"));
                    let _ = reply.send(r);
                }
                Cmd::PutIfAbsent { key, value, reply } => {
                    let r = sqlx::query(
                        "INSERT INTO kv (key, value, updated_at) VALUES ($1, $2, $3)
                         ON CONFLICT (key) DO NOTHING",
                    )
                    .bind(&key)
                    .bind(&value)
                    .bind(now_ts())
                    .execute(&pool)
                    .await
                    .map(|res| res.rows_affected() > 0)
                    .map_err(|e| anyhow::anyhow!("pg put_if_absent: {e}"));
                    let _ = reply.send(r);
                }
                Cmd::Delete { key, reply } => {
                    let r = sqlx::query("DELETE FROM kv WHERE key = $1")
                        .bind(&key)
                        .execute(&pool)
                        .await
                        .map(|_| ())
                        .map_err(|e| anyhow::anyhow!("pg delete: {e}"));
                    let _ = reply.send(r);
                }
                Cmd::List { prefix, reply } => {
                    // 前缀判定用**主键区间**（`key >= p AND key < p+U+10FFFF`），
                    // 与 SQLite/JSON 后端的「字面前缀」一致。旧实现是
                    // `LIKE '{prefix}%'`：LIKE 模式里 `_` 匹配任意单字符、`%`
                    // 匹配任意串，而前缀是用户侧拼出来的键（如 `checkin:rec:`、
                    // `plugin:`，甚至带账号 id 的 `user:`），一旦前缀含 `_`
                    // 或 `%` 就会**过匹配**——同一份数据在 PG 上多返回行、
                    // 在 SQLite 上不会，`count_prefix` 随之虚高。
                    let upper = super::prefix_upper(&prefix);
                    let r = async {
                        let rows = sqlx::query(
                            "SELECT key FROM kv WHERE key COLLATE \"C\" >= $1 AND key COLLATE \"C\" < $2 ORDER BY key COLLATE \"C\"",
                        )
                        .bind(&prefix)
                        .bind(&upper)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("pg list: {e}"))?;
                        Ok::<Vec<String>, anyhow::Error>(
                            rows.into_iter().map(|r| r.get::<String, _>(0)).collect(),
                        )
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::ListLatest { prefix, limit, reply } => {
                    let upper = super::prefix_upper(&prefix);
                    // 与 SQLite 同口径：`limit = 0` 返回空集（负 LIMIT 在 PG
                    // 里是错误/无限制，绝不能把 usize::MAX 原样绑下去）。
                    let limit = super::clamp_limit(limit);
                    let r = async {
                        let rows = sqlx::query(
                            "SELECT key FROM kv WHERE key COLLATE \"C\" >= $1 AND key COLLATE \"C\" < $2 ORDER BY key COLLATE \"C\" DESC LIMIT $3",
                        )
                        .bind(&prefix)
                        .bind(&upper)
                        .bind(limit)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("pg list_latest: {e}"))?;
                        Ok::<Vec<String>, anyhow::Error>(
                            rows.into_iter().map(|r| r.get::<String, _>(0)).collect(),
                        )
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::ListRange {
                    prefix: _,
                    from,
                    to,
                    reply,
                } => {
                    // 与 `SqliteStore::list_range` 同语义：**纯键区间**，prefix
                    // 只作兼容签名保留（当前三后端的生产调用方都把前缀算进
                    // from/to）。旧实现把 prefix 拼进 LIKE，PG 上是「区间 ∧ 模式」、
                    // SQLite/JSON 上是「区间」（它们忽略 prefix），三后端不同。
                    let r = async {
                        let rows = sqlx::query(
                            "SELECT key FROM kv WHERE key COLLATE \"C\" >= $1 AND key COLLATE \"C\" < $2 ORDER BY key COLLATE \"C\"",
                        )
                        .bind(&from)
                        .bind(&to)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("pg list_range: {e}"))?;
                        Ok::<Vec<String>, anyhow::Error>(
                            rows.into_iter().map(|r| r.get::<String, _>(0)).collect(),
                        )
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::ListRangeAsc {
                    prefix: _,
                    from,
                    to,
                    limit,
                    reply,
                } => {
                    let limit = super::clamp_limit(limit);
                    let r = async {
                        let rows = sqlx::query(
                            "SELECT key FROM kv WHERE key COLLATE \"C\" >= $1 AND key COLLATE \"C\" < $2 ORDER BY key COLLATE \"C\" ASC LIMIT $3",
                        )
                        .bind(&from)
                        .bind(&to)
                        .bind(limit)
                        .fetch_all(&pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("pg list_range_asc: {e}"))?;
                        Ok::<Vec<String>, anyhow::Error>(
                            rows.into_iter().map(|r| r.get::<String, _>(0)).collect(),
                        )
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::Batch { entries, reply } => {
                    // 单事务：任一条失败整批回滚（与 SQLite 的 transaction 一致）。
                    let r = async {
                        let mut tx = pool
                            .begin()
                            .await
                            .map_err(|e| anyhow::anyhow!("pg batch begin: {e}"))?;
                        for (key, value) in &entries {
                            sqlx::query(
                                "INSERT INTO kv (key, value, updated_at) VALUES ($1, $2, $3)
                                 ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
                            )
                            .bind(key)
                            .bind(value)
                            .bind(now_ts())
                            .execute(&mut *tx)
                            .await
                            .map_err(|e| anyhow::anyhow!("pg batch put {key}: {e}"))?;
                        }
                        tx.commit()
                            .await
                            .map_err(|e| anyhow::anyhow!("pg batch commit: {e}"))?;
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    let _ = reply.send(r);
                }
                Cmd::Mutate { key, f, reply } => {
                    let r = async {
                        let row = sqlx::query("SELECT value FROM kv WHERE key = $1")
                            .bind(&key)
                            .fetch_optional(&pool)
                            .await
                            .map_err(|e| anyhow::anyhow!("pg mutate read: {e}"))?;
                        let existing = row.map(|r| r.get::<String, _>(0));
                        let new_value = f(existing.clone())?;
                        sqlx::query(
                            "INSERT INTO kv (key, value, updated_at) VALUES ($1, $2, $3)
                             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at",
                        )
                        .bind(&key)
                        .bind(&new_value)
                        .bind(now_ts())
                        .execute(&pool)
                        .await
                        .map_err(|e| anyhow::anyhow!("pg mutate write: {e}"))?;
                        Ok::<(Option<String>, String), anyhow::Error>((existing, new_value))
                    }
                    .await;
                    let _ = reply.send(r);
                }
            }
        }
        Ok(())
    })
}
