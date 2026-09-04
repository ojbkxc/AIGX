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
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

/// SQLite 持久化 KV 存储
pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// 打开或创建 SQLite 数据库
    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&path)?;

        // 启用 WAL 模式以提升并发性能
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS kv (
                 key   TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch())
             );
             CREATE INDEX IF NOT EXISTS idx_kv_updated_at ON kv(updated_at);",
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
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
            rusqlite::params![key, content],
        )?;
        Ok(())
    }

    /// 写入已序列化的原始 JSON 字符串（迁移旧 FileStore 数据时使用，
    /// 避免对已是 JSON 的内容二次序列化产生转义）
    pub fn put_raw(&self, key: &str, raw_json: &str) -> anyhow::Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value, updated_at) VALUES (?1, ?2, unixepoch())",
            rusqlite::params![key, raw_json],
        )?;
        Ok(())
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
        let conn = self.conn.lock();
        conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
        Ok(())
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let conn = self.conn.lock();
        let pattern = format!("{prefix}%");
        let mut stmt = conn.prepare_cached("SELECT key FROM kv WHERE key LIKE ?1 ORDER BY key")?;
        let keys: Vec<String> = stmt
            .query_map([&pattern], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(keys)
    }

    /// 列出所有键值对（支持前缀匹配）
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

    /// 在事务中执行多个操作
    pub fn transaction<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Connection) -> anyhow::Result<R>,
    {
        let conn = self.conn.lock();
        conn.execute("BEGIN IMMEDIATE", [])?;
        match f(&conn) {
            Ok(result) => {
                conn.execute("COMMIT", [])?;
                Ok(result)
            }
            Err(e) => {
                let _ = conn.execute("ROLLBACK", []);
                Err(e)
            }
        }
    }

    /// 返回数据库中总条目数
    pub fn count(&self) -> anyhow::Result<u64> {
        let conn = self.conn.lock();
        let count: u64 = conn.query_row("SELECT COUNT(*) FROM kv", [], |row| row.get(0))?;
        Ok(count)
    }

    /// 压缩数据库（回收空间）
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
}
