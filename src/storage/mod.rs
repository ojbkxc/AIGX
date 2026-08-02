use std::collections::HashMap;
use std::path::PathBuf;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// 基于文件的 KV 存储，类似 Cloudflare Workers KV。
/// 每个键存储为 `{dir}/{key}.json`。
pub struct FileStore {
    dir: PathBuf,
    cache: RwLock<HashMap<String, String>>,
}

impl FileStore {
    /// 创建一个新的 FileStore，数据存储在 `dir` 目录下。
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        let path = self.dir.join(format!("{}.json", key));
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
        let path = self.dir.join(format!("{}.json", key));
        let content = serde_json::to_string_pretty(value)?;
        std::fs::write(&path, &content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(())
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        let path = self.dir.join(format!("{}.json", key));
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
                    if stem.starts_with(prefix) {
                        keys.push(stem.to_string());
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
}