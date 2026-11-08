use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::storage::FileStore;

pub mod capability;
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
        // 四键单事务写入：此前逐条 put，中途失败会留下「映射改了、禁用没改」的中间态
        let custom = self.custom_map.read().clone();
        let disabled = self.disabled.read().clone();
        let reasoning = self.reasoning.read().clone();
        let custom_models = self.custom_models.read().clone();
        self.store.write_batch(&[
            ("model_custom_map", serde_json::to_value(&custom)?),
            (DISABLED_KEY, serde_json::to_value(&disabled)?),
            (REASONING_KEY, serde_json::to_value(&reasoning)?),
            (CUSTOM_MODELS_KEY, serde_json::to_value(&custom_models)?),
        ])?;
        Ok(())
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

    /// 借用版 [`resolve`](Self::resolve)：命中映射时把**映射名**交给闭包，未命中
    /// 交给**原名**，语义与 `resolve` 逐条对齐（含「被关掉的别名不改名」）。
    ///
    /// 存在的理由：调用方只用这个名字**查一次表**（见
    /// `channel::routing_policy::resolve_profile` 的档案键兜底），
    /// 为它分配一个用完即弃的 `String` 是纯浪费。
    ///
    /// 读锁顺序与 [`resolve`](Self::resolve) 一致（先 `custom_map` 后 `disabled`），
    /// 且闭包在**持读锁**期间执行：闭包内不得再取本结构体的锁。
    pub fn resolve_with<R>(&self, model: &str, f: impl FnOnce(&str) -> R) -> R {
        if model.is_empty() {
            return f(model);
        }
        let custom = self.custom_map.read();
        if let Some(mapped) = custom.get(model) {
            if self.disabled.read().contains(model) {
                return f(model);
            }
            return f(mapped.as_str());
        }
        f(model)
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

    /// 拉取模型目录后自动清理**失效映射**：删除 target 不在存活集里的条目。
    ///
    /// `alive` 由调用方预先合并（启用渠道/账号的模型声明 ∪ 定价表 ∪
    /// 自定义模型登记 ∪ 本轮拉回的目录 ∪ 全部现有映射的 target 本身）。
    /// 把「映射 target」整体并入存活集是刻意的保守取舍：链式映射
    /// （a→b→c）中 b 被 a 引用，即使 b 自身无渠道声明也不该被误删。
    ///
    /// 与 [`Self::remove_custom`](Self::remove_custom) 同语义：禁用标记与
    /// 思考等级连带清除，避免孤儿状态残留。返回删除的 (alias, target) 列表，
    /// 供调用方写日志 / 回传前端展示。
    ///
    /// 调用方硬约束（不在此处校验，见 handle_sync_account_catalog）：
    /// **仅当本轮至少一个账号成功回源到非空目录时才允许调用** ——
    /// 拉取失败 / 缓存命中 / 静态兜底一律不清理，防止一次网络抖动删光映射。
    pub fn prune_stale_mappings(
        &self,
        alive: &std::collections::HashSet<String>,
    ) -> Result<Vec<(String, String)>> {
        // 先只读圈出待删条目，再一次性加写锁应用（锁顺序 custom → disabled
        // → reasoning，与 set_enabled / remove_custom 一致，不会死锁）
        //
        // 链式保护（不动点迭代）：target 是另一条映射的 alias（a→b→c 里的 b）
        // 时，即使不在存活集也不能删 —— 除非那条映射本身也在本轮死亡。
        // 迭代到不动点：链条末端死了，整条链一起清；末端还活着（哪怕中间
        // 节点无渠道声明），整条链全保。
        let doomed: Vec<(String, String)> = {
            let custom = self.custom_map.read();
            let mut doomed_aliases: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            loop {
                let mut changed = false;
                for (alias, target) in custom.iter() {
                    if doomed_aliases.contains(alias) {
                        continue;
                    }
                    let target_doomed = !alive.contains(target)
                        && (!custom.contains_key(target) || doomed_aliases.contains(target));
                    if target_doomed {
                        doomed_aliases.insert(alias.clone());
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            custom
                .iter()
                .filter(|(alias, _)| doomed_aliases.contains(*alias))
                .map(|(alias, target)| (alias.clone(), target.clone()))
                .collect()
        };
        if doomed.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut custom = self.custom_map.write();
            let mut disabled = self.disabled.write();
            let mut reasoning = self.reasoning.write();
            for (alias, _) in &doomed {
                custom.remove(alias);
                disabled.remove(alias);
                reasoning.remove(alias);
            }
        }
        self.save()?;
        Ok(doomed)
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

/// 渠道级悬空映射判定（P2-8，方案见 `_a2a-port-recon/42-catalog-drift-plan.md`）。
///
/// 纯函数：圈出 `Channel.model_mapping` 里**永不生效**的条目，由调用方落盘。
/// 判定口径（与数据面 `Channel::resolve_channel_mapping` 同口径，精确字符串匹配）：
///
/// - `alive` = 该渠道的有效模型清单（调用方组装：`models` 非空用白名单；
///   空则用 `discovered_models` 快照；**都为空时传空切片 = 无判定依据，不清理**）；
/// - alias 不在 `alive` 中 → 悬空候选：alias 不被请求 ⇒ 渠道不会被选中
///   （数据面 `supports_model` 口径）⇒ 映射永不命中；
/// - 链式保护：alias 是存活映射链的中间节点（被清单内 alias 引用）时保留。
///   不动点迭代：存活集沿 target 边扩展，收敛后清单外的孤立节点才删——
///   a→b→c 中 b 不在清单但 a 在 ⇒ 整链保；链末端死 ⇒ 整链一起清；
/// - 空 target 条目：`resolve_channel_mapping` 恒跳过（`!next.is_empty()`），
///   视为坏条目无条件删——不影响引用它的映射链（链式解析从 alias 出发，
///   target 无需在映射表里）。
///
/// 返回待删条目的 (alias, target) 列表（按 alias 排序，便于测试稳定）。
pub fn prune_stale_channel_mappings(
    mapping: &HashMap<String, String>,
    alive: &[String],
) -> Vec<(String, String)> {
    if mapping.is_empty() || alive.is_empty() {
        return Vec::new();
    }
    // 存活集初始 = 清单内且出现在映射表里的 alias
    let mut protected: HashSet<&String> = HashSet::new();
    for (alias, _) in mapping.iter() {
        if alive.iter().any(|m| m == alias) {
            protected.insert(alias);
        }
    }
    // 不动点迭代：存活 alias 的 target 也是链中间节点，必须保
    loop {
        let mut changed = false;
        for (alias, target) in mapping.iter() {
            if protected.contains(&alias) {
                if let Some((key, _)) = mapping.get_key_value(target) {
                    if protected.insert(key) {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    // 空 target 坏条目 + 清单外且未受保护的 alias
    let mut removed: Vec<(String, String)> = mapping
        .iter()
        .filter(|(alias, target)| target.is_empty() || !protected.contains(*alias))
        .map(|(a, t)| (a.clone(), t.clone()))
        .collect();
    removed.sort();
    removed
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

    #[test]
    fn prune_stale_removes_only_dead_targets() {
        let m = mapper();
        m.set_custom("alive".into(), "gpt-4o".into()).unwrap();
        m.set_custom("dead".into(), "retired-model".into()).unwrap();
        m.set_enabled("dead", false).unwrap();
        m.set_reasoning("dead", Some("high")).unwrap();

        let mut alive = std::collections::HashSet::new();
        alive.insert("gpt-4o".to_string());
        let removed = m.prune_stale_mappings(&alive).unwrap();

        assert_eq!(
            removed,
            vec![("dead".to_string(), "retired-model".to_string())]
        );
        assert!(m.all_mappings().contains_key("alive"));
        assert!(!m.all_mappings().contains_key("dead"));
        // 禁用标记与思考等级连带清除，不留孤儿状态
        assert!(!m.disabled_aliases().contains("dead"));
        assert!(!m.all_reasoning().contains_key("dead"));
    }

    #[test]
    fn prune_stale_keeps_chain_targets_and_noop_when_empty() {
        let m = mapper();
        // 链式映射：a→b→c，b 无渠道声明但被 a 引用，不能删
        m.set_custom("a".into(), "b".into()).unwrap();
        m.set_custom("b".into(), "c".into()).unwrap();
        let mut alive = std::collections::HashSet::new();
        alive.insert("c".to_string());
        let removed = m.prune_stale_mappings(&alive).unwrap();
        assert!(removed.is_empty(), "链式中间 target 不该被误删");
        assert_eq!(m.all_mappings().len(), 2);

        // 存活集包含全部 target → 零删除且不落盘
        let mut full = alive;
        full.insert("b".to_string());
        assert!(m.prune_stale_mappings(&full).unwrap().is_empty());
    }

    /// 链条末端死了 → 整条链（不动点）一起清，不留悬空中间层。
    #[test]
    fn prune_stale_removes_dead_chains_transitively() {
        let m = mapper();
        m.set_custom("a".into(), "b".into()).unwrap();
        m.set_custom("b".into(), "c".into()).unwrap();
        let mut alive = std::collections::HashSet::new();
        alive.insert("other-model".to_string());
        let removed = m.prune_stale_mappings(&alive).unwrap();
        assert_eq!(removed.len(), 2, "末端 c 死亡 → b→c 与 a→b 一起清");
        assert!(m.all_mappings().is_empty());
    }

    // ── 渠道级悬空映射判定（P2-8）：prune_stale_channel_mappings ─────────────

    fn channel_mapping(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn channel_alive(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn channel_prune_basic() {
        let mapping = channel_mapping(&[("dead-alias", "t1"), ("good-alias", "t2")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["good-alias"]));
        assert_eq!(removed, vec![("dead-alias".to_string(), "t1".to_string())]);
    }

    #[test]
    fn channel_prune_empty_alive_skips() {
        // 无判定依据（白名单与快照都没有）→ 一律不清理
        let mapping = channel_mapping(&[("any", "t1")]);
        assert!(prune_stale_channel_mappings(&mapping, &[]).is_empty());
        assert!(prune_stale_channel_mappings(&mapping, &channel_alive(&[])).is_empty());
    }

    #[test]
    fn channel_prune_chain_protection() {
        // a→b→c，a 在清单，b 不在：b 是中间节点被 a 引用 → 整链保
        let mapping = channel_mapping(&[("a", "b"), ("b", "c")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["a"]));
        assert!(removed.is_empty());
    }

    #[test]
    fn channel_prune_full_chain_death() {
        // 全链不在清单 → 整链清（不动点迭代）
        let mapping = channel_mapping(&[("a", "b"), ("b", "c")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["other"]));
        assert_eq!(
            removed,
            vec![
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "c".to_string())
            ]
        );
    }

    #[test]
    fn channel_prune_self_loop() {
        // 自环在清单 → 保（resolve 视为无映射，无害，不主动清）
        let mapping = channel_mapping(&[("a", "a")]);
        assert!(prune_stale_channel_mappings(&mapping, &channel_alive(&["a"])).is_empty());
        // 自环不在清单 → 孤立死环，删
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["other"]));
        assert_eq!(removed, vec![("a".to_string(), "a".to_string())]);
    }

    #[test]
    fn channel_prune_empty_target() {
        // 空 target 坏条目无条件删；引用它的 a→b（b 在清单）保留
        let mapping = channel_mapping(&[("b", ""), ("a", "b")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["a", "b"]));
        assert_eq!(removed, vec![("b".to_string(), String::new())]);
    }

    #[test]
    fn channel_prune_fixpoint_isolated_head() {
        // a→b、b→c：b 在清单 → b、c 保（c 被 b 引用）；a 不在清单且 target b
        // 受保护 → a 是孤立死链头，删
        let mapping = channel_mapping(&[("a", "b"), ("b", "c")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["b"]));
        assert_eq!(removed, vec![("a".to_string(), "b".to_string())]);
    }

    #[test]
    fn channel_prune_idempotent() {
        let mapping = channel_mapping(&[("dead", "t1"), ("good", "t2")]);
        let first = prune_stale_channel_mappings(&mapping, &channel_alive(&["good"]));
        assert_eq!(first.len(), 1);
        // 模拟清理后再跑：mapping 已移除死条目 → 返回空
        let cleaned: HashMap<String, String> = mapping
            .into_iter()
            .filter(|(a, _)| !first.iter().any(|(x, _)| x == a))
            .collect();
        assert!(prune_stale_channel_mappings(&cleaned, &channel_alive(&["good"])).is_empty());
    }

    #[test]
    fn channel_prune_exact_match() {
        // 精确匹配口径：大小写变体不归一，与 resolve_channel_mapping 一致
        let mapping = channel_mapping(&[("GPT-4", "t1")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["gpt-4"]));
        assert_eq!(removed, vec![("GPT-4".to_string(), "t1".to_string())]);
    }
}
