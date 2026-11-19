use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::storage::FileStore;

pub mod capability;
pub mod capability_overrides;
pub mod manage;
pub mod metadata;
pub mod models_dev;

/// 模型映射管理器（new-api 式通用语义 + Agent2API v3.5.0-beta.1 的
/// provider 维度映射）。
///
/// 映射是**可选的别名转换**：`客户端模型名 → 上游真实模型名`。
/// - 网关不做模型白名单：请求的模型只要某个渠道支持（channel.models /
///   discovered_models）即可路由，未配置映射时模型名原样透传。
/// - 映射仅用于改名场景（例如对外暴露 `gpt-4o`，上游实际叫
///   `gpt-4o-2024-08-06`），以及 Cloudflare 渠道的 `@cf/...` 全名别名。
/// - 出厂不内置任何映射（旧版本的 54 条 `@cf/*` 内置映射已移除——
///   那是 cf-ai-gw 专属模型表，对通用网关是噪音）。
///
/// ## provider 维度（移植自 Agent2API `core/model_rules::Mapping`）
///
/// 每条映射是 `{alias, target, provider(可空), reasoning(可空), enabled}`：
/// - `provider = None`（= `*`）：对所有承载 `target` 的提供商生效 —— 这正是
///   存量「全局别名」的语义，**升级时自动迁移成这个形态**，行为逐字不变；
/// - `provider = Some(p)`：这条改名只在 `p` 这一家生效。同一个模型 id 常被
///   多家同时提供，按家绑定才能表达「这一家用别名 A、那一家用原名」。
///
/// 解析时按 provider 过滤：带 provider 的条目只在该家命中；无 provider 的
/// 条目在任何家都命中（旧口径）。无 provider 的调用点（`resolve` /
/// `resolve_with`，如 CF 桥、计费档案键）只认全局条目，避免把某家的私货
/// 串到别家。
pub struct ModelMapper {
    /// 映射条目（新存储形态，权威表）。
    ///
    /// 顺序即写入顺序（管理页渲染与「同名取首条」的稳定来源）。
    mappings: RwLock<Vec<Mapping>>,
    /// 手动登记的自定义模型（provider, model_id）。
    ///
    /// 管理员在模型管理页手动添加、上游渠道未发现（或暂未接入渠道）的模型。
    /// 与渠道/账号发现的模型一起出现在清单里，可定价/测试/启停。
    /// 不编造能力位元数据 —— 能力位由 models.dev 元数据或管理员覆盖决定。
    custom_models: RwLock<Vec<CustomModelEntry>>,
    store: Arc<FileStore>,
}

/// 映射条目在存储里的**新**键（权威表；存量键见下）。
const MAPPINGS_KEY: &str = "model_mappings";

/// 存量键：全局别名表 `alias → target`（只读导入 + 每次保存时镜像写出，
/// 让旧备份 / 降级路径仍能读到一份与当前状态一致的数据）。
const LEGACY_MAP_KEY: &str = "model_custom_map";

/// 存量键：禁用集在存储里的键（与 `model_custom_map` 并列）。
const DISABLED_KEY: &str = "model_custom_map_disabled";

/// 存量键：映射级思考等级。
const REASONING_KEY: &str = "model_custom_map_reasoning";

/// 自定义模型登记在存储里的键。
const CUSTOM_MODELS_KEY: &str = "model_custom_models";

/// 一条自定义模型登记：`(provider, model_id)`。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct CustomModelEntry {
    pub provider: String,
    pub id: String,
}

/// 一条模型映射：把上游模型（`provider` × `target`）以对外名 `alias` 暴露。
///
/// 字段语义逐条对齐 Agent2API `core/model_rules::Mapping`：
/// - `provider: None` = 旧版全局条目（对所有承载 target 的家生效）；
/// - `reasoning: None` = 不覆盖思考等级（挂在映射条目上，与映射同生共死）；
/// - `enabled`: false = 这条别名暂时不存在（不广告、不改名、不参与解析），
///   管理页仍看得到、可再打开。缺省 `true`（缺失键 = 启用，升级不改行为）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Mapping {
    pub alias: String,
    pub target: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default = "default_mapping_enabled")]
    pub enabled: bool,
}

fn default_mapping_enabled() -> bool {
    true
}

/// 一次 `upsert_mapping` 的结果。
///
/// `existed` 区分「新建」与「改已有条目」：API 层据它决定 `create=true/false`
/// 的三态校验与日志文案（A2A 用同一信息做 409/404 的回执）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingUpsert {
    pub existed: bool,
    pub changed: bool,
}

#[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
impl ModelMapper {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            mappings: RwLock::new(Vec::new()),
            custom_models: RwLock::new(Vec::new()),
            store,
        }
    }

    /// 从存储加载映射。
    ///
    /// ── 迁移（一次性、幂等）──────────────────────────────────────
    /// 旧版把映射拆成三份平行表：`model_custom_map`（alias→target）、
    /// `model_custom_map_disabled`（禁用集）、`model_custom_map_reasoning`
    /// （思考等级）。新版只认 `model_mappings`（带 provider / reasoning /
    /// enabled 的条目表）。装载顺序：
    ///   1. 有新键 → 直接用（存量三份键只在**首次**升级时被读一次）；
    ///   2. 没有新键 → 把三份旧表合成条目表（provider = None = `*`，
    ///      reasoning / enabled 从平行表带过来），并立刻落盘新键 ——
    ///      迁移只发生一次，之后旧键不再参与读取。
    ///
    /// 旧键**不删除**：`save()` 每次都会把当前状态镜像写回它们，让旧备份 /
    /// 降级回旧版本仍能工作（不镜像的话，降级后映射全丢）。
    pub fn load(&self) -> Result<()> {
        let stored = self.store.get::<Vec<Mapping>>(MAPPINGS_KEY)?;
        let migrated = stored.is_none();
        let mut mappings = match stored {
            Some(list) => list,
            None => {
                let legacy: Option<HashMap<String, String>> =
                    self.store.get::<HashMap<String, String>>(LEGACY_MAP_KEY)?;
                let disabled: HashSet<String> = self
                    .store
                    .get::<HashSet<String>>(DISABLED_KEY)?
                    .unwrap_or_default();
                let reasoning: HashMap<String, String> = self
                    .store
                    .get::<HashMap<String, String>>(REASONING_KEY)?
                    .unwrap_or_default();
                let mut list: Vec<Mapping> = legacy
                    .unwrap_or_default()
                    .into_iter()
                    // 存量 alias→target 自动迁移为 provider=*（全局条目）；
                    // reasoning 与 enabled 从各自的平行表带过来，语义不变。
                    .map(|(alias, target)| Mapping {
                        provider: None,
                        reasoning: reasoning.get(&alias).cloned(),
                        enabled: !disabled.contains(&alias),
                        alias,
                        target,
                    })
                    .collect();
                // HashMap 遍历序随机：按 alias 排序，让首次迁移的落盘
                // 形状稳定（重复迁移也不会改写顺序）。
                list.sort_by(|a, b| a.alias.cmp(&b.alias));
                list
            }
        };
        sort_mappings(&mut mappings);
        {
            let mut guard = self.mappings.write();
            guard.clear();
            guard.extend(mappings);
        }
        if migrated {
            self.save()?;
        }
        // 自定义模型登记同样是后加的：老存储没有这个键，读不到就当空表
        if let Some(cm) = self.store.get::<Vec<CustomModelEntry>>(CUSTOM_MODELS_KEY)? {
            let mut custom_models = self.custom_models.write();
            custom_models.clear();
            custom_models.extend(cm);
        }
        Ok(())
    }

    /// 保存映射到存储。
    ///
    /// 写三份：
    ///   1. `model_mappings` —— **权威表**（带 provider / reasoning / enabled）；
    ///   2. 旧三键（`model_custom_map` / `_disabled` / `_reasoning`）—— **镜像**，
    ///      只由全局条目（provider = None）生成。镜像存在的唯一目的是降级与
    ///      旧备份工具仍可读；provider 维度的条目在旧表里无法表达，降级后
    ///      这些条目会退化成「不存在」（与升级前它们本就不存在相比，无损）。
    ///
    /// 单事务批量写：此前逐条 put，中途失败会留下「映射改了、禁用没改」的
    /// 中间态。旧镜像键保留写出（与上面同一条理由）。
    pub fn save(&self) -> Result<()> {
        let mappings = self.mappings.read().clone();
        let custom_models = self.custom_models.read().clone();
        let legacy_map: HashMap<String, String> = mappings
            .iter()
            .filter(|m| m.provider.is_none())
            .map(|m| (m.alias.clone(), m.target.clone()))
            .collect();
        let legacy_disabled: HashSet<String> = mappings
            .iter()
            .filter(|m| m.provider.is_none() && !m.enabled)
            .map(|m| m.alias.clone())
            .collect();
        let legacy_reasoning: HashMap<String, String> = mappings
            .iter()
            .filter(|m| m.provider.is_none())
            .filter_map(|m| m.reasoning.clone().map(|level| (m.alias.clone(), level)))
            .collect();
        self.store.write_batch(&[
            (MAPPINGS_KEY, serde_json::to_value(&mappings)?),
            (LEGACY_MAP_KEY, serde_json::to_value(&legacy_map)?),
            (DISABLED_KEY, serde_json::to_value(&legacy_disabled)?),
            (REASONING_KEY, serde_json::to_value(&legacy_reasoning)?),
            (CUSTOM_MODELS_KEY, serde_json::to_value(&custom_models)?),
        ])?;
        Ok(())
    }

    /// 解析模型名：查生效映射，命中则改名，否则**原样透传**。
    /// 空模型名返回空（由调用方决定错误处理），不再强制兜底到某个模型。
    ///
    /// **无 provider 上下文的调用点**（CF 桥、计费档案键）：只认全局条目
    /// （`provider = None`）。带 provider 的条目表达的是「这一家这么改」，
    /// 在没有家上下文的路径上应用它等于把某家的私货串给别家。
    /// 有家上下文的路径走 [`resolve_for`](Self::resolve_for)。
    ///
    /// 被关掉的别名不参与改名 —— 它仍留在清单里（前端能看到、能再打开），
    /// 但行为上等同于没配过这条映射。
    pub fn resolve(&self, model: &str) -> String {
        self.resolve_for(None, model)
    }

    /// 按 provider 解析模型名（A2A `builtin_target` 的别名分支同语义）。
    ///
    /// 命中顺序：**先精确到家的条目，再全局条目**。同一 provider 下同名多条
    /// 只取第一条启用的（顺序 = 写入顺序，管理页的顺序即优先级）。
    /// `provider = None` 的调用方等价于 [`resolve`](Self::resolve)。
    pub fn resolve_for(&self, provider: Option<&str>, model: &str) -> String {
        if model.is_empty() {
            return String::new();
        }
        let mappings = self.mappings.read();
        match find_mapping(&mappings, provider, model) {
            Some(mapping) => mapping.target.clone(),
            None => model.to_string(),
        }
    }

    /// 借用版 [`resolve_for`](Self::resolve_for)：命中映射时把**映射名**交给
    /// 闭包，未命中交给**原名**，语义逐条对齐（含「被关掉的别名不改名」）。
    ///
    /// 存在的理由：调用方只用这个名字**查一次表**（见
    /// `channel::routing_policy::resolve_profile` 的档案键兜底），
    /// 为它分配一个用完即弃的 `String` 是纯浪费。
    ///
    /// 闭包在**持读锁**期间执行：闭包内不得再取本结构体的锁。
    pub fn resolve_with<R>(&self, model: &str, f: impl FnOnce(&str) -> R) -> R {
        self.resolve_with_for(None, model, f)
    }

    /// [`resolve_with`](Self::resolve_with) 的 provider 维度版本。
    pub fn resolve_with_for<R>(
        &self,
        provider: Option<&str>,
        model: &str,
        f: impl FnOnce(&str) -> R,
    ) -> R {
        if model.is_empty() {
            return f(model);
        }
        let mappings = self.mappings.read();
        match find_mapping(&mappings, provider, model) {
            Some(mapping) => f(mapping.target.as_str()),
            None => f(model),
        }
    }

    /// 获取所有映射（用户自定义的别名表；管理面回读 / 兼容旧调用点）。
    ///
    /// 全局条目优先：provider 维度条目无法在 `alias → target` 的单值表里
    /// 表达，这里只导出全局条目（与旧存储的镜像口径一致）。需要全量
    /// （含 provider 维度）走 [`all_mapping_entries`](Self::all_mapping_entries)。
    pub fn all_mappings(&self) -> HashMap<String, String> {
        self.mappings
            .read()
            .iter()
            .filter(|m| m.provider.is_none())
            .map(|m| (m.alias.clone(), m.target.clone()))
            .collect()
    }

    /// 全量映射条目（管理面 / 新 API 用；含 provider / reasoning / enabled）。
    pub fn all_mapping_entries(&self) -> Vec<Mapping> {
        self.mappings.read().clone()
    }

    /// 获取**当前生效**的映射（排除已关闭的别名；只含全局条目）。
    ///
    /// 对外出口（`/v1/models` 列出别名、CF fallback 判「管理员显式暴露」、
    /// `/v1/models/{id}` 判存在）必须用这份：关掉的别名不参与改名
    /// （见 [`resolve`](Self::resolve)），若仍把它列给客户端，就会出现
    /// 「列表里有、调用时当没配」的欺骗性状态。
    ///
    /// 管理面（模型管理页渲染开关状态）用 [`all_mappings`](Self::all_mappings)
    /// + [`disabled_aliases`](Self::disabled_aliases)，需要看到全部条目。
    pub fn active_mappings(&self) -> HashMap<String, String> {
        self.mappings
            .read()
            .iter()
            .filter(|m| m.provider.is_none() && m.enabled)
            .map(|m| (m.alias.clone(), m.target.clone()))
            .collect()
    }

    /// 某条已配置的别名当前是否生效（配了且没关）。
    ///
    /// 无 provider 上下文：只认全局条目（与 [`resolve`](Self::resolve) 同口径）。
    pub fn is_active(&self, alias: &str) -> bool {
        self.mappings
            .read()
            .iter()
            .any(|m| m.provider.is_none() && m.alias == alias && m.enabled)
    }

    /// 获取自定义映射（`all_mappings` 的别名，老调用点兼容）。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn custom_mappings(&self) -> HashMap<String, String> {
        self.all_mappings()
    }

    /// 某条别名当前是否启用（未配过映射也返回 true —— 「没有」不是「关掉」）。
    ///
    /// 无 provider 上下文：只认全局条目。配过但被关掉返回 false。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn is_enabled(&self, source: &str) -> bool {
        self.mappings
            .read()
            .iter()
            .find(|m| m.provider.is_none() && m.alias == source)
            .map(|m| m.enabled)
            .unwrap_or(true)
    }

    /// 被关掉的别名清单（前端渲染开关状态用；只含全局条目）。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn disabled_aliases(&self) -> HashSet<String> {
        self.mappings
            .read()
            .iter()
            .filter(|m| m.provider.is_none() && !m.enabled)
            .map(|m| m.alias.clone())
            .collect()
    }

    /// 全部映射级思考等级（alias → effort；只含全局条目），管理面回读用。
    ///
    /// provider 维度的等级随 [`all_mapping_entries`](Self::all_mapping_entries)
    /// 一起给（它挂在条目上，不再另立一张 alias → level 的表）。
    pub fn all_reasoning(&self) -> HashMap<String, String> {
        self.mappings
            .read()
            .iter()
            .filter(|m| m.provider.is_none())
            .filter_map(|m| m.reasoning.clone().map(|level| (m.alias.clone(), level)))
            .collect()
    }

    /// 查某模型名的映射级思考等级（无 provider 上下文：只认全局条目）。
    ///
    /// 仅当 `model` 是一条**已启用**的别名且配了思考等级时返回 `Some`。
    /// 禁用别名不注入（与 `resolve` 同口径：关掉的映射不参与改名）。
    pub fn reasoning_for(&self, model: &str) -> Option<String> {
        self.reasoning_for_provider(None, model)
    }

    /// [`reasoning_for`](Self::reasoning_for) 的 provider 维度版本。
    ///
    /// 命中顺序与 [`resolve_for`](Self::resolve_for) 同源（先精确到家的条目，
    /// 再全局条目）—— 等级与改名是**同一条映射**上的两个属性，绝不能各解析
    /// 一次（那正是「A 家的等级串到 B 家」的唯一可能来源）。
    pub fn reasoning_for_provider(&self, provider: Option<&str>, model: &str) -> Option<String> {
        let mappings = self.mappings.read();
        find_mapping(&mappings, provider, model)?.reasoning.clone()
    }

    /// 设置 / 清除某别名的映射级思考等级（三态协议）。
    ///
    /// - `Some(非空)` → 设置
    /// - `Some(空串)` 或 `None` → 清除
    ///
    /// 调用方（admin handler）负责在「键缺失」时不调用本方法。
    ///
    /// 作用域是**全局条目**（无 provider 上下文）。provider 维度条目的等级
    /// 走 [`set_mapping_reasoning`](Self::set_mapping_reasoning)。
    pub fn set_reasoning(&self, alias: &str, effort: Option<&str>) -> Result<()> {
        // 写锁必须在 save() 之前释放：save() 要读同一把锁，parking_lot::RwLock
        // 不可重入 —— 守卫跨 save 持有会原地自死锁（管理端点永不返回）。
        {
            let mut mappings = self.mappings.write();
            let level = effort
                .map(str::trim)
                .filter(|level| !level.is_empty())
                .map(str::to_string);
            for mapping in mappings.iter_mut() {
                if mapping.provider.is_none() && mapping.alias == alias {
                    mapping.reasoning = level.clone();
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

    /// 设置自定义映射（全局条目；兼容老调用点）。
    ///
    /// 已存在同 alias 的全局条目时改 target；provider 维度条目不动。
    pub fn set_custom(&self, source: String, target: String) -> Result<()> {
        {
            let mut mappings = self.mappings.write();
            if let Some(existing) = mappings
                .iter_mut()
                .find(|m| m.provider.is_none() && m.alias == source)
            {
                existing.target = target;
            } else {
                mappings.push(Mapping {
                    alias: source,
                    target,
                    provider: None,
                    reasoning: None,
                    enabled: true,
                });
            }
        }
        self.save()
    }

    /// 新增 / 更新一条映射（provider 维度的权威写入口，对齐 A2A
    /// `model_rules::add_mapping` 的三元组定位）。
    ///
    /// 定位键是 `(alias, target, provider)` 三元组：
    /// - 命中已有条目 → 就地更新 reasoning / enabled（**键缺失 = 不动**，
    ///   由调用方用 `Option<Option<_>>` 表达三态）；
    /// - 未命中 → 追加新条目（默认 enabled = true，reasoning = None）。
    ///
    /// `create` 三态：
    /// - `Some(true)`：只允许新建；已存在同名（alias 与 target 都相同）的
    ///   条目 → 返回 `Err`（与 A2A 409 同语义，消息由调用方构造）；
    /// - `Some(false)`：只允许编辑；不存在 → `Err`（A2A 404）；
    /// - `None`：兼容旧调用方的保存语义（有则改、无则建）。
    ///
    /// 不做别名的字符集校验：这条接口同时承担「新建」与「切开关 / 改等级」，
    /// 后者的 alias 可能本就是上游的非 ASCII id（千问办公的 `高级` / `标准`）。
    /// 新建时的合法性由 API 层用 [`alias_name_valid`] 判。
    pub fn upsert_mapping(
        &self,
        alias: &str,
        target: &str,
        provider: Option<&str>,
        reasoning: Option<Option<&str>>,
        enabled: Option<bool>,
        create: Option<bool>,
    ) -> Result<MappingUpsert> {
        let alias = alias.trim();
        let target = target.trim();
        let provider_key = provider
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let mut existed = false;
        let mut changed = false;
        {
            let mut mappings = self.mappings.write();
            let position = mappings.iter().position(|m| {
                m.alias == alias
                    && m.target == target
                    && match (&m.provider, &provider_key) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                        _ => false,
                    }
            });
            match (create, position) {
                // `create=true` 只新增：三元组已存在 = 绑定已在这里（A2A 409）
                (Some(true), Some(_)) => {
                    anyhow::bail!("模型 ID「{alias}」已绑定到此上游模型，请直接使用已有绑定");
                }
                // `create=false` 只编辑：三元组不存在 = 无从编辑（A2A 404）
                (Some(false), None) => {
                    anyhow::bail!("映射不存在: {alias} → {target}");
                }
                _ => {}
            }
            match position {
                Some(index) => {
                    existed = true;
                    let entry = &mut mappings[index];
                    if let Some(level) = reasoning {
                        let next = level
                            .map(str::trim)
                            .filter(|level| !level.is_empty())
                            .map(str::to_string);
                        if entry.reasoning != next {
                            entry.reasoning = next;
                            changed = true;
                        }
                    }
                    if let Some(flag) = enabled {
                        if entry.enabled != flag {
                            entry.enabled = flag;
                            changed = true;
                        }
                    }
                }
                None => {
                    let level = match reasoning {
                        Some(Some(level)) => {
                            let trimmed = level.trim();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed.to_string())
                            }
                        }
                        _ => None,
                    };
                    mappings.push(Mapping {
                        alias: alias.to_string(),
                        target: target.to_string(),
                        provider: provider_key.clone(),
                        reasoning: level,
                        enabled: enabled.unwrap_or(true),
                    });
                    changed = true;
                }
            }
        }
        if changed {
            self.save()?;
        }
        Ok(MappingUpsert { existed, changed })
    }

    /// 删除一条映射（`(alias, target, provider)` 三元组精确定位）。
    ///
    /// `provider = None` 只删全局条目 —— 指名一家删除旧版全局映射的
    /// 「其余承载家开关与等级保持不变」语义，在迁移后的形态里自然成立
    /// （其余家本来就各有自己的条目）。返回是否真的删掉了。
    pub fn remove_mapping(
        &self,
        alias: &str,
        target: &str,
        provider: Option<&str>,
    ) -> Result<bool> {
        let alias = alias.trim();
        let target = target.trim();
        let provider_key = provider
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let removed = {
            let mut mappings = self.mappings.write();
            let before = mappings.len();
            mappings.retain(|m| {
                !(m.alias == alias
                    && m.target == target
                    && match (&m.provider, &provider_key) {
                        (None, None) => true,
                        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
                        _ => false,
                    })
            });
            before != mappings.len()
        };
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// 切换某条别名的启停。
    ///
    /// 别名不存在时返回 `false`（调用方据此报 404，而不是静默造一条
    /// 只存在于禁用集里的孤儿）。无 provider 上下文：只切全局条目。
    pub fn set_enabled(&self, source: &str, enabled: bool) -> Result<bool> {
        let mut found = false;
        {
            let mut mappings = self.mappings.write();
            for mapping in mappings.iter_mut() {
                if mapping.provider.is_none() && mapping.alias == source {
                    mapping.enabled = enabled;
                    found = true;
                }
            }
        }
        if !found {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    /// 删除自定义映射（全部 provider 维度的同名条目一并删除；老语义）。
    ///
    /// 「连带清掉禁用标记与思考等级」在条目化存储里自然成立（它们就是条目
    /// 自身的字段，删条目即一并消失）。
    pub fn remove_custom(&self, source: &str) -> Result<()> {
        {
            let mut mappings = self.mappings.write();
            mappings.retain(|m| m.alias != source);
        }
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
        // 先只读圈出待删条目，再一次性加写锁应用。
        //
        // 链式保护（不动点迭代）：target 是另一条映射的 alias（a→b→c 里的 b）
        // 时，即使不在存活集也不能删 —— 除非那条映射本身也在本轮死亡。
        // 迭代到不动点：链条末端死了，整条链一起清；末端还活着（哪怕中间
        // 节点无渠道声明），整条链全保。
        //
        // 只在**全局条目**（provider = None）上做：provider 维度条目的存活
        // 判据是「那家的清单还承载 target」，不属于本函数的知识范围（调用方
        // 的 alive 集合是全局视图），保守方向是不动它们。
        let doomed: Vec<(String, String)> = {
            let mappings = self.mappings.read();
            let globals: HashMap<&str, &str> = mappings
                .iter()
                .filter(|m| m.provider.is_none())
                .map(|m| (m.alias.as_str(), m.target.as_str()))
                .collect();
            let mut doomed_aliases: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            loop {
                let mut changed = false;
                for (alias, target) in globals.iter() {
                    if doomed_aliases.contains(*alias) {
                        continue;
                    }
                    let target_doomed = !alive.contains(*target)
                        && (!globals.contains_key(*target) || doomed_aliases.contains(*target));
                    if target_doomed {
                        doomed_aliases.insert((*alias).to_string());
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
            }
            globals
                .iter()
                .filter(|(alias, _)| doomed_aliases.contains(**alias))
                .map(|(alias, target)| ((*alias).to_string(), (*target).to_string()))
                .collect()
        };
        if doomed.is_empty() {
            return Ok(Vec::new());
        }
        {
            let mut mappings = self.mappings.write();
            mappings.retain(|m| {
                !(m.provider.is_none()
                    && doomed
                        .iter()
                        .any(|(alias, target)| alias == &m.alias && target == &m.target))
            });
        }
        self.save()?;
        Ok(doomed)
    }

    /// 清空所有自定义映射
    pub fn reset(&self) -> Result<()> {
        self.mappings.write().clear();
        self.custom_models.write().clear();
        self.save()
    }
}

/// 条目排序：全局条目在前（旧口径的渲染顺序），provider 维度条目按
/// (alias, provider) 跟进；同键保持写入顺序（`sort_by` 是稳定排序）。
fn sort_mappings(mappings: &mut [Mapping]) {
    mappings.sort_by(|a, b| {
        a.provider
            .is_some()
            .cmp(&b.provider.is_some())
            .then_with(|| a.alias.cmp(&b.alias))
            .then_with(|| a.provider.cmp(&b.provider))
    });
}

/// 在条目表里找一条**生效**的映射（`resolve_for` / `reasoning_for_provider`
/// 共用的唯一命中口径）。
///
/// 命中顺序：先精确到家的条目，再全局条目；两步都只认 `enabled` 的条目。
/// 大小写：alias 与 provider 均忽略大小写（下游请求大小写不敏感，与
/// A2A `ModelRules::binding` 同口径）。
fn find_mapping<'a>(
    mappings: &'a [Mapping],
    provider: Option<&str>,
    alias: &str,
) -> Option<&'a Mapping> {
    let matches = |m: &&Mapping| m.alias.eq_ignore_ascii_case(alias) && m.enabled;
    if let Some(provider) = provider.map(str::trim).filter(|p| !p.is_empty()) {
        if let Some(mapping) = mappings.iter().filter(matches).find(|m| {
            m.provider
                .as_deref()
                .is_some_and(|owner| owner.eq_ignore_ascii_case(provider))
        }) {
            return Some(mapping);
        }
    }
    mappings
        .iter()
        .filter(matches)
        .find(|m| m.provider.is_none())
}

/// **新建对外映射名**能不能落盘（与 A2A `model_rules::alias_name_valid` 同口径）。
///
/// 对外名是给下游点名用的名字，上游根本不看它；各家官方模型名并不保证是
/// ASCII（千问办公的 `高级` / `标准` 就是中文）。保留的是「没法当模型名用」
/// 那一档：空、超长（按**字符**数 ≤ 128）、带空白或控制字符 ——
/// 空白 / 控制字符会做出「看上去一模一样、请求却对不上」的名字。
///
/// 上游模型 id 的字符集口径另见 [`alias_valid`]。
pub fn alias_name_valid(alias: &str) -> bool {
    !alias.is_empty()
        && alias.chars().count() <= 128
        && !alias.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// **上游模型 id / 自生成别名**的字符集口径：字母 / 数字 / `- _ . / :`。
///
/// 用在手动登记的上游模型 id（`/api/models/custom` 与
/// `POST /api/models/mappings` 的 target）。对外映射名不走它（见
/// [`alias_name_valid`]）。
pub fn alias_valid(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 128
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':'))
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
    for alias in mapping.keys() {
        if alive.iter().any(|m| m.eq_ignore_ascii_case(alias)) {
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

    // ── provider 维度映射（T5 / A2A `model_rules::Mapping` 对齐）─────────────

    #[test]
    fn provider_scoped_mapping_only_hits_that_provider() {
        let m = mapper();
        m.upsert_mapping("alias-p", "upstream-x", Some("workbuddy"), None, None, None)
            .unwrap();
        // 指定的那一家命中
        assert_eq!(m.resolve_for(Some("workbuddy"), "alias-p"), "upstream-x");
        assert_eq!(m.resolve_for(Some("WorkBuddy"), "alias-p"), "upstream-x");
        // 别家不命中（同名模型在别家仍按原名走）
        assert_eq!(m.resolve_for(Some("catpaw"), "alias-p"), "alias-p");
        // 无 provider 上下文的调用点也不命中（provider 条目不是全局口径）
        assert_eq!(m.resolve("alias-p"), "alias-p");
    }

    #[test]
    fn global_mapping_hits_every_provider() {
        let m = mapper();
        // provider = None = 旧版全局语义（存量迁移出来的形态）
        m.set_custom("g".into(), "t".into()).unwrap();
        assert_eq!(m.resolve_for(Some("workbuddy"), "g"), "t");
        assert_eq!(m.resolve_for(Some("catpaw"), "g"), "t");
        assert_eq!(m.resolve("g"), "t");
    }

    #[test]
    fn provider_mapping_beats_global_mapping() {
        let m = mapper();
        m.set_custom("dup".into(), "global-target".into()).unwrap();
        m.upsert_mapping("dup", "scoped-target", Some("catpaw"), None, None, None)
            .unwrap();
        assert_eq!(m.resolve_for(Some("catpaw"), "dup"), "scoped-target");
        // 别家仍走全局条目
        assert_eq!(m.resolve_for(Some("workbuddy"), "dup"), "global-target");
    }

    #[test]
    fn disabled_provider_mapping_falls_back_to_global() {
        let m = mapper();
        m.set_custom("dup2".into(), "global-target".into()).unwrap();
        m.upsert_mapping(
            "dup2",
            "scoped-target",
            Some("catpaw"),
            None,
            Some(false),
            None,
        )
        .unwrap();
        // 家内条目被关 → 回落到全局条目（与 A2A `binding()` 的两级查找同口径）
        assert_eq!(m.resolve_for(Some("catpaw"), "dup2"), "global-target");
        // 再打开 → 家内条目重新生效
        m.upsert_mapping(
            "dup2",
            "scoped-target",
            Some("catpaw"),
            None,
            Some(true),
            None,
        )
        .unwrap();
        assert_eq!(m.resolve_for(Some("catpaw"), "dup2"), "scoped-target");
    }

    #[test]
    fn reasoning_binding_is_provider_scoped() {
        let m = mapper();
        m.upsert_mapping("r", "t", Some("catpaw"), Some(Some("high")), None, None)
            .unwrap();
        assert_eq!(
            m.reasoning_for_provider(Some("catpaw"), "r").as_deref(),
            Some("high")
        );
        // 别家不串味（这是「A 家等级用到 B 家」唯一可能的来源）
        assert_eq!(m.reasoning_for_provider(Some("workbuddy"), "r"), None);
        assert_eq!(m.reasoning_for_provider(None, "r"), None);
    }

    #[test]
    fn create_flags_guard_new_vs_edit() {
        let m = mapper();
        // create=true 在已存在的三元组上 → 拒绝（A2A 409）
        m.upsert_mapping("a", "t", Some("p"), None, None, None)
            .unwrap();
        assert!(m
            .upsert_mapping("a", "t", Some("p"), None, None, Some(true))
            .is_err());
        // create=false 在不存在的三元组上 → 拒绝（A2A 404）
        assert!(m
            .upsert_mapping("zzz", "t", Some("p"), None, None, Some(false))
            .is_err());
        // 同 alias 不同 provider 是不同条目：create=true 正常新建
        assert!(m
            .upsert_mapping("a", "t", Some("q"), None, None, Some(true))
            .is_ok());
    }

    #[test]
    fn legacy_parallel_tables_migrate_to_provider_star() {
        // 旧三键（map / disabled / reasoning）一次性迁成 provider=None 的条目
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-map-migrate-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(FileStore::new(dir));
        let mut legacy = HashMap::new();
        legacy.insert("kept".to_string(), "t1".to_string());
        legacy.insert("off".to_string(), "t2".to_string());
        store.put("model_custom_map", &legacy).unwrap();
        let mut disabled = HashSet::new();
        disabled.insert("off".to_string());
        store.put("model_custom_map_disabled", &disabled).unwrap();
        let mut reasoning = HashMap::new();
        reasoning.insert("kept".to_string(), "high".to_string());
        store.put("model_custom_map_reasoning", &reasoning).unwrap();

        let m = ModelMapper::new(store.clone());
        m.load().unwrap();
        assert_eq!(m.resolve("kept"), "t1");
        assert_eq!(m.reasoning_for("kept").as_deref(), Some("high"));
        assert!(!m.is_enabled("off"));
        // 迁移同时落盘新键（只发生一次）
        let stored: Option<Vec<Mapping>> = store.get("model_mappings").unwrap();
        let stored = stored.expect("迁移后必须写出 model_mappings");
        assert_eq!(stored.len(), 2);
        assert!(stored.iter().all(|m| m.provider.is_none()));

        // 二次 load：走新键，旧键被忽略（在旧键里塞一条脏数据也不会读进来）
        store
            .put(
                "model_custom_map",
                &HashMap::from([("dirty".to_string(), "x".to_string())]),
            )
            .unwrap();
        let again = ModelMapper::new(store);
        again.load().unwrap();
        assert_eq!(again.resolve("dirty"), "dirty", "新键存在后旧键不再参与");
    }

    /// **旧格式 JSON → 新结构反序列化**（向后兼容钉子）。
    ///
    /// 存量 `model_mappings`（如果曾经被别的版本写过）只会有 `{alias, target}`：
    /// 新字段 `provider` / `reasoning` / `enabled` 必须靠 serde 默认值补，
    /// 缺字段**不得**让整条解析失败 —— 一次解析失败会让全部映射静默失效。
    #[test]
    fn mapping_deserializes_from_minimal_legacy_json() {
        let legacy = serde_json::json!({ "alias": "a", "target": "t" });
        let mapping: Mapping = serde_json::from_value(legacy).expect("旧格式必须可解析");
        assert_eq!(mapping.alias, "a");
        assert_eq!(mapping.target, "t");
        assert_eq!(mapping.provider, None, "缺失 provider = 全局条目");
        assert_eq!(mapping.reasoning, None);
        assert!(mapping.enabled, "缺失 enabled = 启用（升级不改行为）");

        // 带 null 的旧写法同样兼容
        let with_nulls =
            serde_json::json!({ "alias": "a", "target": "t", "provider": null, "reasoning": null });
        let mapping: Mapping = serde_json::from_value(with_nulls).expect("null 字段必须可解析");
        assert_eq!(mapping.provider, None);
        assert_eq!(mapping.reasoning, None);
        assert!(mapping.enabled);
    }

    #[test]
    fn save_mirrors_global_entries_to_legacy_keys() {
        let m = mapper();
        m.set_custom("g".into(), "t".into()).unwrap();
        m.set_enabled("g", false).unwrap();
        m.upsert_mapping("g2", "t2", Some("p"), None, None, None)
            .unwrap();
        let legacy: HashMap<String, String> =
            m.store.get("model_custom_map").unwrap().unwrap_or_default();
        assert_eq!(legacy.get("g").map(String::as_str), Some("t"));
        assert!(!legacy.contains_key("g2"), "provider 条目写不进旧单值表");
        let disabled: HashSet<String> = m
            .store
            .get("model_custom_map_disabled")
            .unwrap()
            .unwrap_or_default();
        assert!(disabled.contains("g"));
    }

    #[test]
    fn pruning_never_touches_provider_scoped_entries() {
        let m = mapper();
        m.upsert_mapping("scoped", "dead-upstream", Some("catpaw"), None, None, None)
            .unwrap();
        let alive = HashSet::new();
        // 全局表为空 → 零删除，provider 条目也原样保留（它的存活判据是那家的清单）
        assert!(m.prune_stale_mappings(&alive).unwrap().is_empty());
        assert_eq!(m.all_mapping_entries().len(), 1);
        assert_eq!(m.resolve_for(Some("catpaw"), "scoped"), "dead-upstream");
    }

    #[test]
    fn alias_name_valid_is_lenient_but_rejects_unusable() {
        assert!(alias_name_valid("gpt-4o"));
        assert!(alias_name_valid("高级"));
        assert!(!alias_name_valid(""));
        assert!(!alias_name_valid("has space"));
        assert!(!alias_name_valid(&"x".repeat(129)));
        // 128 个中文按字符数计，合法
        assert!(alias_name_valid(&"高".repeat(128)));
        assert!(alias_valid("pool/model"));
        assert!(!alias_valid("高级"));
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
    fn channel_prune_case_insensitive() {
        // 大小写不敏感口径：存活集比较忽略大小写，与 resolve_channel_mapping 一致
        let mapping = channel_mapping(&[("GPT-4", "t1")]);
        let removed = prune_stale_channel_mappings(&mapping, &channel_alive(&["gpt-4"]));
        assert!(removed.is_empty());
    }

    #[test]
    fn mapping_alias_match_is_case_insensitive() {
        // 下游请求大小写不敏感：find_mapping 的 alias 忽略大小写匹配
        let mapping = Mapping {
            alias: "DeepSeek-v4.1-flash".to_string(),
            target: "deepseek-v4.1-flash".to_string(),
            provider: None,
            reasoning: None,
            enabled: true,
        };
        let mappings = [mapping];
        let hit = find_mapping(&mappings, None, "DeepSeek-v4.1-FLASH");
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().alias, "DeepSeek-v4.1-flash");
    }
}
