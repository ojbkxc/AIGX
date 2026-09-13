//! PostgreSQL 存储层 — 通过 sqlx 异步驱动 + 独立线程桥接，提供同步 KV 接口。
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

    /// 原子更新（读取-修改-写入），返回旧值（若存在）
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default,
        F: FnOnce(Option<T>) -> T,
    {
        let existing: Option<T> = self.get(key)?;
        let old = existing.clone();
        let new_value = f(existing);
        self.put(key, &new_value)?;
        Ok(old)
    }

    fn send(&self, cmd: Cmd) -> anyhow::Result<()> {
        self.tx
            .send(cmd)
            .map_err(|_| anyhow::anyhow!("pg store worker has terminated"))
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
                    let pattern = format!("{prefix}%");
                    let r = async {
                        let rows = sqlx::query("SELECT key FROM kv WHERE key LIKE $1 ORDER BY key")
                            .bind(&pattern)
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
                    let pattern = format!("{prefix}%");
                    let r = async {
                        let rows = sqlx::query(
                            "SELECT key FROM kv WHERE key LIKE $1 ORDER BY key DESC LIMIT $2",
                        )
                        .bind(&pattern)
                        .bind(limit as i64)
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
            }
        }
        Ok(())
    })
}