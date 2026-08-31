use std::collections::HashMap;
#[cfg(feature = "sqlite-kv")]
use std::path::Path;
use std::path::PathBuf;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;

// rusqlite KV 存储模块 — 仅当启用 sqlite-kv feature 时编译。
//
// 注意：rusqlite（libsqlite3-sys 0.28）与 sea-orm 的 sqlx-sqlite（libsqlite3-sys 0.26）
// 存在原生库 links 冲突，不能同时启用。当启用 sea-orm + sqlite 时需用
// --no-default-features 禁用 sqlite-kv，此时 FileStore 降级为 JSON 文件后端。
#[cfg(feature = "sqlite-kv")]
pub mod sqlite;

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
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
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
}

/// 统一 KV 存储入口。
///
/// - 默认构建（`sqlite-kv`）：基于 SQLite（WAL 模式）的持久化 KV，
///   首次打开时自动将旧版 `*.json` 文件数据迁移进 SQLite。
/// - `--no-default-features`（配合 SeaORM）：降级为 JSON 文件后端。
#[cfg(feature = "sqlite-kv")]
pub struct FileStore {
    inner: sqlite::SqliteStore,
}

#[cfg(feature = "sqlite-kv")]
impl FileStore {
    /// 打开（或创建）`dir` 下的 SQLite 数据库，并迁移旧版 JSON 数据。
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("aigx.db");
        let inner = sqlite::SqliteStore::open(db_path)?;
        migrate_legacy_json(&dir, &inner)?;
        Ok(Self { inner })
    }

    /// 兼容旧调用：无法失败时 panic。
    pub fn new(dir: PathBuf) -> Self {
        Self::open(dir).expect("failed to open SQLite storage")
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        self.inner.get(key)
    }

    /// 写入 JSON 值
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        self.inner.put(key, value)
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.inner.delete(key)
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        self.inner.list(prefix)
    }

    /// 原子更新（读取-修改-写入）。
    #[allow(dead_code)]
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default,
        F: FnOnce(Option<T>) -> T,
    {
        self.inner.update(key, f)
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
        let old: Option<u64> = store.update("counter", |val| match val {
            Some(n) => n + 1,
            None => 1,
        }).unwrap();
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
