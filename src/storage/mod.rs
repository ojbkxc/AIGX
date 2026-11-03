use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
#[cfg(feature = "sqlite-kv")]
use std::path::Path;
use std::path::PathBuf;

// rusqlite KV 存储模块 — 仅当启用 sqlite-kv feature 时编译。
//
// 注意：rusqlite（libsqlite3-sys 0.28）与 sea-orm 的 sqlx-sqlite（libsqlite3-sys 0.26）
// 存在原生库 links 冲突，不能同时启用。当启用 sea-orm + sqlite 时需用
// --no-default-features 禁用 sqlite-kv，此时 FileStore 降级为 JSON 文件后端。
#[cfg(feature = "sqlite-kv")]
pub mod sqlite;

// PostgreSQL KV 存储模块 — 仅当启用 postgres feature 时编译。
// 通过 sea-orm 的 sqlx 依赖（sea-orm 已 re-export sqlx）直连 PostgreSQL。
#[cfg(feature = "postgres")]
pub mod postgres;

/// 将任意 key 编码为文件名安全的形式：ASCII 字母数字及 `-_.` 保持原样，
/// 其余字符以 `%XX` 形式转义。可逆，避免 Windows 下 `:`/`/` 等非法字符问题。
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn decode_key(enc: &str) -> String {
    let bytes = enc.as_bytes();
    let mut out = Vec::with_capacity(enc.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 基于 JSON 文件的 KV 存储（无 rusqlite 时的回退后端）。
/// 每个键存储为 `{dir}/{encoded_key}.json`。
///
/// 默认构建（sqlite-kv）下仅作为旧数据迁移的读取源；`--no-default-features`
/// 构建（配合 SeaORM 多数据库）时 `FileStore` 直接使用本实现。
pub struct JsonFileStore {
    dir: PathBuf,
    cache: RwLock<HashMap<String, String>>,
}

impl JsonFileStore {
    /// 创建一个新的 JSON 文件存储，数据存储在 `dir` 目录下。
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            cache: RwLock::new(HashMap::new()),
        }
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", encode_key(key)))
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        // B19：优先从缓存读取——原先 get 只写缓存从不读，缓存形同虚设；
        // 命中时避免一次磁盘 IO 与文件存在性检查（clone 出内容后立即释放读锁）
        if let Some(content) = self.cache.read().get(key).cloned() {
            let value = serde_json::from_str(&content)?;
            return Ok(Some(value));
        }
        let path = self.path_of(key);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let value = serde_json::from_str(&content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(Some(value))
    }

    /// 写入 JSON 值
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let path = self.path_of(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(value)?;
        std::fs::write(&path, &content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(())
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        let path = self.path_of(key);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.cache.write().remove(key);
        Ok(())
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut keys = Vec::new();
        if !self.dir.exists() {
            return Ok(keys);
        }
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let key = decode_key(stem);
                    if key.starts_with(prefix) {
                        keys.push(key);
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// 倒序取前 `limit` 个键（与 SqliteStore::list_latest_keys 同语义降级实现）
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut keys = self.list(prefix)?;
        keys.reverse();
        keys.truncate(limit);
        Ok(keys)
    }

    /// 前缀内字典序 `[from, to)` 区间的键（与 SqliteStore::list_range 同语义，
    /// JSON 后端 list + filter 降级）。
    pub fn list_range(&self, prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        // list 已按字典序升序，filter 保序，无需再排。
        self.list(prefix).map(|keys| {
            keys.into_iter()
                .filter(|k| k.as_str() >= from && k.as_str() < to)
                .collect()
        })
    }

    /// 原子插入（key 已存在返回 false）。JSON 后端用写锁近似 CAS：
    /// 非默认 feature，仅保证单进程内的互斥（跨进程靠文件系统，尽力而为）。
    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        if self.get::<serde_json::Value>(key)?.is_some() {
            return Ok(false);
        }
        let mut cache = self.cache.write();
        if cache.contains_key(key) {
            return Ok(false);
        }
        let path = self.path_of(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() {
            return Ok(false);
        }
        let content = serde_json::to_string_pretty(value)?;
        std::fs::write(&path, &content)?;
        cache.insert(key.to_string(), content);
        Ok(true)
    }

    /// 原子更新（读取-修改-写入）。
    /// 返回旧值（如果存在）。
    #[allow(dead_code)]
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

    /// 异步写入（JSON 后端降级实现）。
    ///
    /// 与 `FileStore::put_async` 同签名同语义：JSON 后端本就没有连接池/事务，
    /// 直接同步写即可；失败仅记日志，不向调用方传播（调用方原本就是 error!）。
    /// 保留这个降级实现，让热路径调用点不必按后端 feature 分叉。
    pub fn put_async<T: Serialize + Send + 'static>(
        &self,
        key: impl Into<String> + Send + 'static,
        value: T,
    ) {
        let key = key.into();
        if let Err(e) = self.put(&key, &value) {
            tracing::error!("put_async {key} failed: {e:#}");
        }
    }

    /// 多键写入（JSON 后端逐条写，无事务）。
    pub fn write_batch(&self, entries: &[(&str, serde_json::Value)]) -> anyhow::Result<()> {
        for (key, val) in entries {
            self.put(key, val)?;
        }
        Ok(())
    }
}

/// 统一 KV 存储入口。
///
/// - 默认构建（`sqlite-kv`）：基于 SQLite（WAL 模式）的持久化 KV，
///   首次打开时自动将旧版 `*.json` 文件数据迁移进 SQLite。
/// - `--no-default-features`（配合 SeaORM）：降级为 JSON 文件后端。
/// - 同时启用 `postgres` + `database.url`：FileStore 内部改用 PgStore。
#[cfg(feature = "sqlite-kv")]
pub struct FileStore {
    inner: FileStoreInner,
}

/// 统一后端枚举：SQLite（默认）或 PostgreSQL（database.url 配置后启用）。
#[cfg(feature = "sqlite-kv")]
enum FileStoreInner {
    Sqlite(sqlite::SqliteStore),
    #[cfg(feature = "postgres")]
    Postgres(postgres::PgStore),
}

#[cfg(feature = "sqlite-kv")]
impl FileStore {
    /// 打开（或创建）`dir` 下的 SQLite 数据库，并迁移旧版 JSON 数据。
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("aigx.db");
        let inner = sqlite::SqliteStore::open(db_path)?;
        migrate_legacy_json(&dir, &inner)?;
        Ok(Self {
            inner: FileStoreInner::Sqlite(inner),
        })
    }

    /// 兼容旧调用：无法失败时 panic。
    pub fn new(dir: PathBuf) -> Self {
        Self::open(dir).expect("failed to open SQLite storage")
    }

    /// 以 PostgreSQL 为后端打开 KV 存储（保持同样的同步接口）。
    ///
    /// 仅在启用 `postgres` feature 时可用；`database.url` 非空时由 main.rs 调用。
    /// 打开后执行一次性 SQLite→PG 历史数据迁移（幂等，已存在 key 跳过）。
    #[cfg(feature = "postgres")]
    pub fn open_postgres(
        url: &str,
        max_connections: u32,
        sqlite_dirs: &[std::path::PathBuf],
    ) -> anyhow::Result<Self> {
        let inner = postgres::PgStore::new(url, max_connections)?;
        let store = Self {
            inner: FileStoreInner::Postgres(inner),
        };
        migrate_sqlite_to_pg(&store, sqlite_dirs);
        Ok(store)
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.get(key),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.get(key),
        }
    }

    /// 写入 JSON 值
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put(key, value),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put(key, value),
        }
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.delete(key),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.delete(key),
        }
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list(prefix),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list(prefix),
        }
    }

    /// P0 性能：按字典序倒序取前 `limit` 个键（前缀过滤，索引范围扫描）。
    /// 供日志分页用——key 的时间戳前缀使字典序等价时间序，只取最新一页即可。
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_latest_keys(prefix, limit),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list_latest_keys(prefix, limit),
        }
    }

    /// 列出前缀内、字典序在 `[from, to)` 区间的键（升序）。
    ///
    /// 供按天聚合的窗口类读用：定长时间戳/日期中段使字典序等价数值序，
    /// 键范围扫把「最近 N 分钟」类查询从全表降为近窗（吃主键索引）。
    pub fn list_range(&self, prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_range(prefix, from, to),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list_range(prefix, from, to),
        }
    }

    /// 原子插入（key 已存在返回 false）——check-then-put 竞态的 CAS 原语。
    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put_if_absent(key, value),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put_if_absent(key, value),
        }
    }

    /// 原子更新（读取-修改-写入）。
    #[allow(dead_code)]
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default,
        F: FnOnce(Option<T>) -> T,
    {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.update(key, f),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.update(key, f),
        }
    }

    /// 异步写入：序列化在调用线程完成，落盘丢进 blocking 线程池，
    /// 避免请求线程被磁盘 IO 阻塞。
    ///
    /// 供热路径（如 channel 落盘）在**锁外**调用；`put` 本体保持同步语义不变。
    /// 失败仅记日志（与调用方原本的 error! 行为一致），不向调用方传播错误。
    pub fn put_async<T: Serialize + Send + 'static>(
        &self,
        key: impl Into<String> + Send + 'static,
        value: T,
    ) {
        let key = key.into();
        let raw = match serde_json::to_string(&value) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("put_async serialize {key} failed: {e}");
                return;
            }
        };
        // PG 后端本身是 actor 队列（send 只入队，落盘在专用工作线程），
        // 直接同步 send 即已非阻塞，无需 spawn_blocking。
        #[cfg(feature = "postgres")]
        if let FileStoreInner::Postgres(p) = &self.inner {
            if let Err(e) = p.put_raw(&key, &raw) {
                tracing::error!("put_async {key} failed: {e:#}");
            }
            return;
        }
        let sqlite = match &self.inner {
            FileStoreInner::Sqlite(s) => s.cheap_clone(),
        };
        // 无 tokio runtime（同步测试/启动早期）时直接同步写，语义不变
        if tokio::runtime::Handle::try_current().is_err() {
            if let Err(e) = sqlite.put_raw(&key, &raw) {
                tracing::error!("put_async {key} failed: {e:#}");
            }
            return;
        }
        let key_for_log = key.clone();
        tokio::task::spawn(async move {
            let result = tokio::task::spawn_blocking(move || sqlite.put_raw(&key, &raw)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("put_async {key_for_log} failed: {e:#}"),
                Err(e) => tracing::error!("put_async join failed: {e}"),
            }
        });
    }

    /// 多键原子写入（单事务）：中途失败全部回滚，消除「写到一半进程挂了」的中间态。
    /// 仅 SQLite 后端支持真事务；PG 后端逐条写（actor 串行，等效原子）；JSON 后端逐条写。
    pub fn write_batch(&self, entries: &[(&str, serde_json::Value)]) -> anyhow::Result<()> {
        // 无 postgres feature 时 FileStoreInner 只有 Sqlite 一个变体，
        // 模式必然匹配 —— 此时 `if let` 会被 clippy 判为 irrefutable。
        #[cfg(all(feature = "sqlite-kv", feature = "postgres"))]
        if let FileStoreInner::Sqlite(sqlite) = &self.inner {
            return sqlite.transaction(|conn| {
                for (key, val) in entries {
                    sqlite::SqliteStore::put_conn(conn, key, val)?;
                }
                Ok(())
            });
        }
        #[cfg(all(feature = "sqlite-kv", not(feature = "postgres")))]
        {
            let FileStoreInner::Sqlite(sqlite) = &self.inner;
            return sqlite.transaction(|conn| {
                for (key, val) in entries {
                    sqlite::SqliteStore::put_conn(conn, key, val)?;
                }
                Ok(())
            });
        }
        #[allow(unreachable_code)]
        {
            for (key, val) in entries {
                self.put(key, val)?;
            }
            Ok(())
        }
    }

    /// 写入原始 JSON 字符串（`put_async` 内部用；SQLite/PG 走 `put_raw`，JSON 后端直接写文件）。
    ///
    /// 调用点在 `put_async` 里按后端各取所需，这里保留一个统一的便捷实现；
    /// 当前 `put_async` 已原地分派，本方法暂无调用方，保留以免后端增减时重复。
    #[cfg(feature = "sqlite-kv")]
    #[allow(dead_code)]
    fn put_raw(&self, key: &str, raw: &str) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put_raw(key, raw),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put_raw(key, raw),
        }
    }
}

/// 无 rusqlite 时，`FileStore` 即 JSON 文件后端。
#[cfg(not(feature = "sqlite-kv"))]
pub type FileStore = JsonFileStore;

/// 将旧版 JSON 文件数据迁移进 SQLite（幂等）：
/// - 遍历 `dir` 下所有 `*.json`；
/// - 对 SQLite 中尚不存在的 key 写入原始 JSON 内容；
/// - 已存在的 key 跳过，避免覆盖 SQLite 中的新数据。
#[cfg(feature = "sqlite-kv")]
fn migrate_legacy_json(dir: &Path, store: &sqlite::SqliteStore) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut migrated = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let key = decode_key(stem);
        if store.contains(&key)? {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        store.put_raw(&key, &content)?;
        migrated += 1;
    }
    if migrated > 0 {
        tracing::info!("Migrated {migrated} legacy JSON entries into SQLite storage");
    }
    Ok(())
}

/// SQLite→PG 一次性历史数据迁移（幂等）：
/// - 按顺序探测 `sqlite_dirs` 中的 `aigx.db`（新旧两种布局都覆盖）；
/// - 对 PG 中尚不存在的 key 用 put_if_absent 原子写入（并发/重复启动安全）；
/// - 已存在的 key 跳过，避免覆盖 PG 中的新数据。
///
/// 背景：`open_postgres` 此前直接返回空 PG 库，历史数据（请求日志/审计/
/// 会话等）全部留在 SQLite 文件里，切库后页面查不到历史——补上迁移。
#[cfg(all(feature = "sqlite-kv", feature = "postgres"))]
fn migrate_sqlite_to_pg(pg: &FileStore, sqlite_dirs: &[std::path::PathBuf]) {
    for dir in sqlite_dirs {
        let db_path = dir.join("aigx.db");
        if !db_path.exists() {
            continue;
        }
        let sqlite = match sqlite::SqliteStore::open(db_path.clone()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("SQLite→PG migration: skip {}: {e}", db_path.display());
                continue;
            }
        };
        let keys = match sqlite.list("") {
            Ok(k) => k,
            Err(e) => {
                tracing::warn!("SQLite→PG migration: list keys failed: {e}");
                continue;
            }
        };
        let mut migrated = 0usize;
        for key in keys {
            // put_if_absent 保证：PG 已有（更新）的 key 不被旧 SQLite 值覆盖
            match sqlite.get::<serde_json::Value>(&key) {
                Ok(Some(v)) => {
                    if matches!(pg.put_if_absent(&key, &v), Ok(true)) {
                        migrated += 1;
                    }
                }
                _ => continue,
            }
        }
        if migrated > 0 {
            tracing::info!(
                "SQLite→PG migration: {migrated} entries imported from {}",
                db_path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_put_and_get() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        store.put("test_key", &"hello").unwrap();
        let val: Option<String> = store.get("test_key").unwrap();
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn test_get_nonexistent() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        let val: Option<String> = store.get("nonexistent").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_delete() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        store.put("to_delete", &"value").unwrap();
        store.delete("to_delete").unwrap();
        let val: Option<String> = store.get("to_delete").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_list_with_prefix() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

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
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        // 初始写入
        store.put("counter", &0u64).unwrap();

        // 原子更新：递增
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

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn test_legacy_json_migration() {
        let dir = TempDir::new().unwrap();
        // 预置旧版 JSON 文件
        let legacy = JsonFileStore::new(dir.path().to_path_buf());
        legacy.put("user:old1", &"legacy-data").unwrap();
        legacy.put("user:old2", &42u64).unwrap();

        // 打开 SQLite 存储（触发迁移）
        let store = FileStore::new(dir.path().to_path_buf());

        let v1: Option<String> = store.get("user:old1").unwrap();
        assert_eq!(v1, Some("legacy-data".to_string()));
        let v2: Option<u64> = store.get("user:old2").unwrap();
        assert_eq!(v2, Some(42));
    }
}
