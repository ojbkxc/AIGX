use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

pub mod manage;
pub mod metadata;
pub mod models_dev;

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
    /// 被关掉的别名（仍保留映射，但 `resolve` 时跳过 → 原样透传）。
    ///
    /// 与 `custom_map` 分开存：存量数据只有 alias→target 的字符串表，
    /// 把它整体升级成结构体会让所有老数据需要迁移，且 `all_mappings()`
    /// 这类只读消费者的返回类型被迫改变。用一张并行的禁用集，
    /// 老存储原样可读、老调用点零改动。
    disabled: RwLock<std::collections::HashSet<String>>,
    /// 映射级思考等级（alias → reasoning_effort）。
    ///
    /// 同样走平行集策略：老存储没有这个键，load 读不到就当空表。
    /// 三态协议（与 Agent2API 同口径）：
    /// - 键缺失 → 不动已有等级
    /// - 空串 / null → 清空
    /// - 非空字符串 → 设置
    reasoning: RwLock<HashMap<String, String>>,
    /// 手动登记的自定义模型（provider, model_id）。
    ///
    /// 管理员在模型管理页手动添加、上游渠道未发现（或暂未接入渠道）的模型。
    /// 与渠道/账号发现的模型一起出现在清单里，可定价/测试/启停。
    /// 不编造能力位元数据 —— 能力位由 models.dev 元数据或管理员覆盖决定。
    custom_models: RwLock<Vec<CustomModelEntry>>,
    store: Arc<FileStore>,
}

/// 禁用集在存储里的键（与 `model_custom_map` 并列）。
const DISABLED_KEY: &str = "model_custom_map_disabled";

/// 映射级思考等级在存储里的键。
const REASONING_KEY: &str = "model_custom_map_reasoning";

/// 自定义模型登记在存储里的键。
const CUSTOM_MODELS_KEY: &str = "model_custom_models";

/// 一条自定义模型登记：`(provider, model_id)`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CustomModelEntry {
    pub provider: String,
    pub id: String,
}

impl ModelMapper {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            custom_map: RwLock::new(HashMap::new()),
            disabled: RwLock::new(std::collections::HashSet::new()),
            reasoning: RwLock::new(HashMap::new()),
            custom_models: RwLock::new(Vec::new()),
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
        // 禁用集是后加的：老存储里没有这个键，读不到就当空集（全部启用）
        if let Some(off) = self
            .store
            .get::<std::collections::HashSet<String>>(DISABLED_KEY)?
        {
            let mut disabled = self.disabled.write();
            disabled.clear();
            disabled.extend(off);
        }
        // 映射级思考等级同样是后加的：老存储没有这个键，读不到就当空表
        if let Some(rs) = self.store.get::<HashMap<String, String>>(REASONING_KEY)? {
            let mut reasoning = self.reasoning.write();
            reasoning.clear();
            reasoning.extend(rs);
        }
        // 自定义模型登记同样是后加的：老存储没有这个键，读不到就当空表
        if let Some(cm) = self.store.get::<Vec<CustomModelEntry>>(CUSTOM_MODELS_KEY)? {
            let mut custom_models = self.custom_models.write();
            custom_models.clear();
            custom_models.extend(cm);
        }
        Ok(())
    }

    /// 保存自定义映射到存储
    pub fn save(&self) -> Result<()> {
        let custom = self.custom_map.read().clone();
        self.store.put("model_custom_map", &custom)?;
        let disabled = self.disabled.read().clone();
        self.store.put(DISABLED_KEY, &disabled)?;
        let reasoning = self.reasoning.read().clone();
        self.store.put(REASONING_KEY, &reasoning)?;
        let custom_models = self.custom_models.read().clone();
        self.store.put(CUSTOM_MODELS_KEY, &custom_models)
    }

    /// 解析模型名：查自定义映射，命中则改名，否则**原样透传**。
    /// 空模型名返回空（由调用方决定错误处理），不再强制兜底到某个模型。
    ///
    /// 被关掉的别名不参与改名 —— 它仍留在清单里（前端能看到、能再打开），
    /// 但行为上等同于没配过这条映射。
    pub fn resolve(&self, model: &str) -> String {
        if model.is_empty() {
            return String::new();
        }
        let custom = self.custom_map.read();
        if let Some(mapped) = custom.get(model) {
            // 读锁顺序：先 custom 后 disabled。写入路径（set_enabled / set_custom）
            // 也是这个顺序，不会死锁。
            if self.disabled.read().contains(model) {
                return model.to_string();
            }
            return mapped.clone();
        }
        // 无映射：透传原始模型名（通用网关语义）
        model.to_string()
    }

    /// 获取所有映射（用户自定义的别名表）
    pub fn all_mappings(&self) -> HashMap<String, String> {
        self.custom_map.read().clone()
    }

    /// 获取**当前生效**的映射（排除已关闭的别名）。
    ///
    /// 对外出口（`/v1/models` 列出别名、CF fallback 判「管理员显式暴露」、
    /// `/v1/models/{id}` 判存在）必须用这份：关掉的别名不参与改名
    /// （见 [`resolve`](Self::resolve)），若仍把它列给客户端，就会出现
    /// 「列表里有、调用时当没配」的欺骗性状态。
    ///
    /// 管理面（模型管理页渲染开关状态）用 [`all_mappings`](Self::all_mappings)
    /// + [`disabled_aliases`](Self::disabled_aliases)，需要看到全部条目。
    pub fn active_mappings(&self) -> HashMap<String, String> {
        let disabled = self.disabled.read();
        self.custom_map
            .read()
            .iter()
            .filter(|(alias, _)| !disabled.contains(*alias))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// 某条已配置的别名当前是否生效（配了且没关）。
    pub fn is_active(&self, alias: &str) -> bool {
        self.custom_map.read().contains_key(alias) && self.is_enabled(alias)
    }

    /// 获取自定义映射
    pub fn custom_mappings(&self) -> HashMap<String, String> {
        self.custom_map.read().clone()
    }

    /// 某条别名当前是否启用（未配过映射也返回 true —— 「没有」不是「关掉」）。
    pub fn is_enabled(&self, source: &str) -> bool {
        !self.disabled.read().contains(source)
    }

    /// 被关掉的别名清单（前端渲染开关状态用）。
    pub fn disabled_aliases(&self) -> std::collections::HashSet<String> {
        self.disabled.read().clone()
    }

    /// 全部映射级思考等级（alias → effort），管理面回读用。
    pub fn all_reasoning(&self) -> HashMap<String, String> {
        self.reasoning.read().clone()
    }

    /// 查某模型名的映射级思考等级。
    ///
    /// 仅当 `model` 是一条**已启用**的别名且配了思考等级时返回 `Some`。
    /// 禁用别名不注入（与 `resolve` 同口径：关掉的映射不参与改名）。
    ///
    /// 读锁顺序：custom → disabled → reasoning，与写入路径一致，不会死锁。
    pub fn reasoning_for(&self, model: &str) -> Option<String> {
        if model.is_empty() {
            return None;
        }
        let custom = self.custom_map.read();
        if !custom.contains_key(model) {
            return None;
        }
        if self.disabled.read().contains(model) {
            return None;
        }
        self.reasoning.read().get(model).cloned()
    }

    /// 设置 / 清除某别名的映射级思考等级（三态协议）。
    ///
    /// - `Some(非空)` → 设置
    /// - `Some(空串)` 或 `None` → 清除（不动已有值）
    ///
    /// 调用方（admin handler）负责在「键缺失」时不调用本方法。
    pub fn set_reasoning(&self, alias: &str, effort: Option<&str>) -> Result<()> {
        // 写锁必须在 save() 之前释放：save() 要读同一把锁，parking_lot::RwLock
        // 不可重入 —— 守卫跨 save 持有会原地自死锁（管理端点永不返回）。
        {
            let mut reasoning = self.reasoning.write();
            match effort {
                Some(e) if !e.is_empty() => {
                    reasoning.insert(alias.to_string(), e.to_string());
                }
                _ => {
                    reasoning.remove(alias);
                }
            }
        }
        self.save()
    }

    /// 全部自定义模型登记（管理面 / 清单拼接用）。
    pub fn custom_models(&self) -> Vec<CustomModelEntry> {
        self.custom_models.read().clone()
    }

    /// 添加一条自定义模型登记。已存在的 (provider, id) 静默跳过。
    pub fn add_custom_model(&self, provider: &str, id: &str) -> Result<()> {
        let provider = provider.trim();
        let id = id.trim();
        if provider.is_empty() || id.is_empty() {
            return Ok(());
        }
        let entry = CustomModelEntry {
            provider: provider.to_string(),
            id: id.to_string(),
        };
        let mut list = self.custom_models.write();
        if !list.contains(&entry) {
            list.push(entry);
            // 先放锁再落盘：save() 读同一把锁，重入即死锁
            drop(list);
            self.save()?;
        }
        Ok(())
    }

    /// 删除一条自定义模型登记。不存在时静默返回。
    pub fn remove_custom_model(&self, provider: &str, id: &str) -> Result<()> {
        let entry = CustomModelEntry {
            provider: provider.to_string(),
            id: id.to_string(),
        };
        let mut list = self.custom_models.write();
        if let Some(pos) = list.iter().position(|e| *e == entry) {
            list.remove(pos);
            // 先放锁再落盘：save() 读同一把锁，重入即死锁
            drop(list);
            self.save()?;
        }
        Ok(())
    }

    /// 设置自定义映射
    pub fn set_custom(&self, source: String, target: String) -> Result<()> {
        self.custom_map.write().insert(source, target);
        self.save()
    }

    /// 切换某条别名的启停。
    ///
    /// 别名不存在时返回 `false`（调用方据此报 404，而不是静默造一条
    /// 只存在于禁用集里的孤儿）。
    pub fn set_enabled(&self, source: &str, enabled: bool) -> Result<bool> {
        {
            let custom = self.custom_map.read();
            if !custom.contains_key(source) {
                return Ok(false);
            }
        }
        {
            let mut disabled = self.disabled.write();
            if enabled {
                disabled.remove(source);
            } else {
                disabled.insert(source.to_string());
            }
        }
        self.save()?;
        Ok(true)
    }

    /// 删除自定义映射
    pub fn remove_custom(&self, source: &str) -> Result<()> {
        self.custom_map.write().remove(source);
        // 连带清掉禁用标记，否则同名别名将来重新建出来会继承旧状态
        self.disabled.write().remove(source);
        // 连带清掉思考等级，否则同名别名重建会继承旧等级
        self.reasoning.write().remove(source);
        self.save()
    }

    /// 清空所有自定义映射
    pub fn reset(&self) -> Result<()> {
        self.custom_map.write().clear();
        self.disabled.write().clear();
        self.reasoning.write().clear();
        self.custom_models.write().clear();
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

    #[test]
    fn disabled_alias_passes_through_but_stays_listed() {
        let m = mapper();
        m.set_custom("gpt-4o".into(), "gpt-4o-2024-08-06".into())
            .unwrap();
        assert_eq!(m.resolve("gpt-4o"), "gpt-4o-2024-08-06");

        // 关掉：行为回到透传，但映射本身还在（前端能看到、能再打开）
        assert!(m.set_enabled("gpt-4o", false).unwrap());
        assert_eq!(m.resolve("gpt-4o"), "gpt-4o");
        assert!(m.all_mappings().contains_key("gpt-4o"));
        assert!(!m.is_enabled("gpt-4o"));
        assert!(m.disabled_aliases().contains("gpt-4o"));

        // 再打开：恢复改名
        assert!(m.set_enabled("gpt-4o", true).unwrap());
        assert_eq!(m.resolve("gpt-4o"), "gpt-4o-2024-08-06");
        assert!(m.is_enabled("gpt-4o"));
    }

    #[test]
    fn set_enabled_rejects_unknown_alias() {
        let m = mapper();
        // 不存在的别名不该被「关掉」——否则禁用集里多出一条没有映射的孤儿
        assert!(!m.set_enabled("never-configured", false).unwrap());
        assert!(m.disabled_aliases().is_empty());
    }

    #[test]
    fn active_mappings_exclude_disabled() {
        let m = mapper();
        m.set_custom("a".into(), "t1".into()).unwrap();
        m.set_custom("b".into(), "t2".into()).unwrap();
        assert_eq!(m.active_mappings().len(), 2);

        m.set_enabled("a", false).unwrap();
        // 对外出口只看生效的那条：关掉的别名不该出现在 /v1/models 里
        let active = m.active_mappings();
        assert_eq!(active.len(), 1);
        assert!(active.contains_key("b"));
        assert!(!active.contains_key("a"));
        // 管理面仍能看到全部（渲染开关状态用）
        assert_eq!(m.all_mappings().len(), 2);

        // is_active：配了且没关才算生效；没配过的名字一律不算
        assert!(!m.is_active("a"));
        assert!(m.is_active("b"));
        assert!(!m.is_active("never-configured"));
    }

    #[test]
    fn disabled_state_survives_reload() {
        let dir = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("aigx-map-reload-{}-{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let store = Arc::new(FileStore::new(dir.clone()));
        {
            let m = ModelMapper::new(store.clone());
            m.set_custom("a".into(), "b".into()).unwrap();
            m.set_enabled("a", false).unwrap();
        }
        let reloaded = ModelMapper::new(store);
        reloaded.load().unwrap();
        assert_eq!(reloaded.resolve("a"), "a", "禁用状态必须落盘");
        assert!(!reloaded.is_enabled("a"));
    }

    #[test]
    fn remove_clears_disabled_mark() {
        let m = mapper();
        m.set_custom("a".into(), "b".into()).unwrap();
        m.set_enabled("a", false).unwrap();
        m.remove_custom("a").unwrap();
        assert!(!m.disabled_aliases().contains("a"));
        // 重新建同名别名应当是「启用」的新条目，不继承旧状态
        m.set_custom("a".into(), "c".into()).unwrap();
        assert_eq!(m.resolve("a"), "c");
    }

    #[test]
    fn legacy_store_without_disabled_key_loads() {
        // 老存储只有 model_custom_map 一个键：load 不该报错，且全部启用
        let dir = {
            use std::sync::atomic::{AtomicU64, Ordering};
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("aigx-map-legacy-{}-{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            dir
        };
        let store = Arc::new(FileStore::new(dir));
        let mut legacy = HashMap::new();
        legacy.insert("old".to_string(), "new".to_string());
        store.put("model_custom_map", &legacy).unwrap();

        let m = ModelMapper::new(store);
        m.load().unwrap();
        assert_eq!(m.resolve("old"), "new");
        assert!(m.is_enabled("old"));
    }

    #[test]
    fn mapping_reasoning_inject_and_clear() {
        let m = mapper();
        m.set_custom("alias-1".into(), "gpt-4o".into()).unwrap();
        assert_eq!(m.reasoning_for("alias-1"), None);
        m.set_reasoning("alias-1", Some("high")).unwrap();
        assert_eq!(m.reasoning_for("alias-1").as_deref(), Some("high"));
        assert_eq!(m.reasoning_for("gpt-4o"), None);
        m.set_reasoning("alias-1", Some("")).unwrap();
        assert_eq!(m.reasoning_for("alias-1"), None);
    }

    #[test]
    fn disabled_alias_does_not_inject_reasoning() {
        let m = mapper();
        m.set_custom("alias-2".into(), "gpt-4o".into()).unwrap();
        m.set_reasoning("alias-2", Some("medium")).unwrap();
        assert_eq!(m.reasoning_for("alias-2").as_deref(), Some("medium"));
        m.set_enabled("alias-2", false).unwrap();
        assert_eq!(m.reasoning_for("alias-2"), None);
        m.set_enabled("alias-2", true).unwrap();
        assert_eq!(m.reasoning_for("alias-2").as_deref(), Some("medium"));
    }

    #[test]
    fn custom_models_add_remove_and_persist() {
        let m = mapper();
        m.add_custom_model("openai", "my-model").unwrap();
        m.add_custom_model("anthropic", "claude-x").unwrap();
        assert_eq!(m.custom_models().len(), 2);
        m.add_custom_model("openai", "my-model").unwrap();
        assert_eq!(m.custom_models().len(), 2);
        m.remove_custom_model("openai", "my-model").unwrap();
        assert_eq!(m.custom_models().len(), 1);
    }
}
