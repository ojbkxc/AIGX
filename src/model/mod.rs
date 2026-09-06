use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 模型映射管理器（new-api 式通用语义）
///
/// 映射是**可选的别名转换**：`客户端模型名 → 上游真实模型名`。
/// - 网关不做模型白名单：请求的模型只要某个渠道支持（channel.models /
///   discovered_models）即可路由，未配置映射时模型名原样透传。
/// - 映射仅用于改名场景（例如对外暴露 `gpt-4o`，上游实际叫
///   `gpt-4o-2024-08-06`），以及 Cloudflare 渠道的 `@cf/...` 全名别名。
/// - 出厂不内置任何映射（旧版本的 54 条 `@cf/*` 内置映射已移除——
///   那是 cf-ai-gw 专属模型表，对通用网关是噪音）。
pub struct ModelMapper {
    custom_map: RwLock<HashMap<String, String>>,
    store: Arc<FileStore>,
}

impl ModelMapper {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            custom_map: RwLock::new(HashMap::new()),
            store,
        }
    }

    /// 从存储加载自定义映射
    pub fn load(&self) -> Result<()> {
        let raw = self
            .store
            .get::<HashMap<String, String>>("model_custom_map")?;
        if let Some(map) = raw {
            let mut custom = self.custom_map.write();
            custom.clear();
            custom.extend(map);
        }
        Ok(())
    }

    /// 保存自定义映射到存储
    pub fn save(&self) -> Result<()> {
        let custom = self.custom_map.read().clone();
        self.store.put("model_custom_map", &custom)
    }

    /// 解析模型名：查自定义映射，命中则改名，否则**原样透传**。
    /// 空模型名返回空（由调用方决定错误处理），不再强制兜底到某个模型。
    pub fn resolve(&self, model: &str) -> String {
        if model.is_empty() {
            return String::new();
        }
        let custom = self.custom_map.read();
        if let Some(mapped) = custom.get(model) {
            return mapped.clone();
        }
        // 无映射：透传原始模型名（通用网关语义）
        model.to_string()
    }

    /// 获取所有映射（用户自定义的别名表）
    pub fn all_mappings(&self) -> HashMap<String, String> {
        self.custom_map.read().clone()
    }

    /// 获取自定义映射
    pub fn custom_mappings(&self) -> HashMap<String, String> {
        self.custom_map.read().clone()
    }

    /// 设置自定义映射
    pub fn set_custom(&self, source: String, target: String) -> Result<()> {
        self.custom_map.write().insert(source, target);
        self.save()
    }

    /// 删除自定义映射
    pub fn remove_custom(&self, source: &str) -> Result<()> {
        self.custom_map.write().remove(source);
        self.save()
    }

    /// 清空所有自定义映射
    pub fn reset(&self) -> Result<()> {
        self.custom_map.write().clear();
        self.save()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapper() -> ModelMapper {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        // 每个测试独立目录（pid + 原子序号），避免并行测试共用 SQLite 文件导致 database is locked
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-map-test-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));
        ModelMapper::new(store)
    }

    #[test]
    fn test_passthrough_without_mapping() {
        let m = mapper();
        // 无映射时原样透传（通用网关语义）
        assert_eq!(m.resolve("gpt-4o"), "gpt-4o");
        assert_eq!(m.resolve("glm-4.7-flash"), "glm-4.7-flash");
    }

    #[test]
    fn test_alias_mapping() {
        let m = mapper();
        m.set_custom("gpt-4o".into(), "gpt-4o-2024-08-06".into())
            .unwrap();
        assert_eq!(m.resolve("gpt-4o"), "gpt-4o-2024-08-06");
        // 其它模型不受影响
        assert_eq!(m.resolve("claude-3"), "claude-3");
    }

    #[test]
    fn test_empty_model() {
        let m = mapper();
        assert_eq!(m.resolve(""), "");
    }
}
