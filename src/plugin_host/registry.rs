//! 插件包元数据与 provider 归属路由。
//!
//! ## 归属规则（插件优先，2026-11 修订）
//!
//! ```text
//! is_plugin_owned(p) = 已装且启用的插件声明了 p  &&  该家未被逐家停用  &&  !is_hard_reserved(p)
//! ```
//!
//! **插件优先**：已装并启用的插件声明了某个 provider，就由插件接管——
//! 即使 AIGX 有原生实现。原生实现只在「插件未装 / 已停用 / 加载失败」时
//! 兜底（由数据面 `resolve.rs` 的两级查找实现：先查插件进程/脚本宿主，
//! 拿不到再走原生 bridge）。
//!
//! **硬保留集合**（[`HARD_RESERVED`]）是唯一的例外：AIGX 内部虚拟 id
//! （不指向真实上游的本地能力，如 `hub`/`cloudflare` 这类由网关自身
//! 实现的转发目标）不允许插件接管——插件即使声明也无效。集合极小且
//! 显式，改它要同步单测。
//!
//! ## 持久化
//!
//! 单条 KV（`plugin:packs`）持有全部包元数据（`Vec<PackMeta>`）。包数量
//! 有限（上游索引只有 3 个），单键足够且免去前缀扫描。

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::storage::FileStore;

/// 全部包元数据的 KV 键。
const KV_PACKS: &str = "plugin:packs";

/// 一个插件包（`pack.json` 的网关侧投影）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PackMeta {
    /// 包 id（如 `agent2api-providers`）。
    pub id: String,
    /// 展示名。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 协议版本（必须等于网关的 `PROTOCOL_VERSION`）。
    pub protocol: u32,
    /// 包声明覆盖的 provider。
    pub providers: Vec<PackProvider>,
    /// 入口二进制的相对路径（相对包版本目录）。
    pub entry: String,
    /// 包自带的签名（原样保留，是否可信由验签决定）。
    pub signature: Option<String>,
    /// 入口二进制的 sha256（安装时现算；上游 `InstalledPack.sha256` 同义）。
    pub sha256: Option<String>,
    /// 是否通过验签（未签名 = false，允许装但界面标黄）。
    pub signed: bool,
    /// 运维按 provider 停用的名单（空 = 全部按包级 `enabled` 生效）。
    ///
    /// 上游原生包用 `providers: BTreeMap<String,bool>` 表达同一语义；AIGX 的
    /// [`Self::providers`] 已是声明列表，故沿用上游**脚本包**的
    /// `disabled_providers` 形态，语义一一对应（名单内 = 停用）。
    pub disabled_providers: Vec<String>,
    /// 安装时刻（unix 秒）。
    pub installed_at: i64,
    /// 运维是否启用这个包（默认停用：装了不等于要跑）。
    pub enabled: bool,
}

impl PackMeta {
    /// 包版本目录的绝对路径。
    pub fn version_dir(&self, plugins_root: &std::path::Path) -> std::path::PathBuf {
        plugins_root.join(&self.id).join(&self.version)
    }

    /// 入口二进制的绝对路径。
    pub fn entry_path(&self, plugins_root: &std::path::Path) -> std::path::PathBuf {
        self.version_dir(plugins_root).join(&self.entry)
    }

    /// 包声明的某 provider 是否处于启用态（包级启用 ∧ 不在停用名单）。
    pub fn provider_enabled(&self, provider: &str) -> bool {
        self.enabled && !self.disabled_providers.iter().any(|id| id == provider)
    }

    /// 只声明一家 provider 的包 id（A2A v3.5.0 `single_provider_id` 同义）。
    ///
    /// 单家包的特殊性：包级 `enabled` 就是那一家的开关，`disabled_providers`
    /// 里出现它对用户是**矛盾态**（界面显示包开着、路由却把这家当停用）。
    pub fn single_provider_id(&self) -> Option<&str> {
        match self.providers.as_slice() {
            [only] => Some(only.id.as_str()),
            _ => None,
        }
    }

    /// 单家包归一化：把「包启用 + 唯一 provider 在停用名单」折成包停用并清空
    /// 子标记（A2A v3.5.0 `normalize_single_provider` 同义）。
    ///
    /// 为什么要归一而不是各读一处自行判断：`provider_enabled()` 按名单判、
    /// `enabled` 按包判，两处判据在矛盾态下给出互相矛盾的答案（`enabled=true`
    /// 但该家不可用）。折成包停用后两处一致：用户「关掉唯一那家」的意图保留
    /// （包停用），下次开包也不会被隐藏的子开关卡住。
    ///
    /// 返回 `true` = 本次确实改了状态（调用方据此决定是否回写）。
    pub fn normalize_single_provider(&mut self) -> bool {
        let Some(only) = self.single_provider_id().map(str::to_string) else {
            return false;
        };
        if !self.enabled || !self.disabled_providers.iter().any(|id| id == &only) {
            return false;
        }
        self.enabled = false;
        self.disabled_providers.clear();
        true
    }
}

/// 包声明的一家 provider。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PackProvider {
    pub id: String,
    pub label: String,
    /// 上游 `pack.json` 的展示/排序序位，**也参与签名载荷**（`id:order:default`）。
    /// 早期包写 0；wasm 新包会写非 0（如 accio=26），漏掉会验签失败。
    pub order: i64,
    /// 包建议的默认启停（网关只作展示参考，实际以 `PackMeta.enabled` 为准）。
    pub default_enabled: bool,
}

/// 包注册表（内存快照 + KV 持久化）。
pub struct PackRegistry {
    inner: Arc<Inner>,
}

struct Inner {
    packs: RwLock<Vec<PackMeta>>,
    store: Arc<FileStore>,
}

impl PackRegistry {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            inner: Arc::new(Inner {
                packs: RwLock::new(Vec::new()),
                store,
            }),
        }
    }

    /// 从 KV 加载（启动时一次）。
    ///
    /// 读入即按 [`PackMeta::normalize_single_provider`] 归一（A2A v3.5.0
    /// `PackRegistry::load` 同口径）：历史数据里可能存着「包启用 + 唯一
    /// provider 被逐家停用」的矛盾态，它在启动瞬间就该折成包停用，否则
    /// 界面显示包开着、数据面却把这家当停用（`is_plugin_owned` 走
    /// `provider_enabled`，返回 false 而 `enabled` 仍为 true）。
    ///
    /// 归一是幂等的：已是自洽态的包不受影响。持久化留到下一次写操作
    /// （与上游一致——load 只修正内存视图，避免启动即写盘）。
    pub fn load(&self) -> Result<(), String> {
        let mut packs = self
            .inner
            .store
            .get::<Vec<PackMeta>>(KV_PACKS)
            .map_err(|e| format!("读取插件包注册表失败: {e}"))?
            .unwrap_or_default();
        for pack in &mut packs {
            if pack.normalize_single_provider() {
                tracing::info!(
                    "[Plugin] 包 {} 存在「启用 + 唯一 provider 被停用」矛盾态，已折合为包停用",
                    pack.id
                );
            }
        }
        *self.inner.packs.write() = packs;
        Ok(())
    }

    /// 当前全部包（快照）。
    pub fn list(&self) -> Vec<PackMeta> {
        self.inner.packs.read().clone()
    }

    /// 按 id 取包。
    pub fn get(&self, id: &str) -> Option<PackMeta> {
        self.inner.packs.read().iter().find(|p| p.id == id).cloned()
    }

    /// 插入或更新一个包（持久化）。
    pub fn upsert(&self, mut meta: PackMeta) -> Result<(), String> {
        {
            let mut packs = self.inner.packs.write();
            match packs.iter_mut().find(|p| p.id == meta.id) {
                Some(existing) => {
                    // 保留既有 enabled 与 provider 停用名单（重装不应重置运维的选择）
                    meta.enabled = existing.enabled;
                    meta.disabled_providers = existing.disabled_providers.clone();
                    *existing = meta;
                }
                None => packs.push(meta),
            }
        }
        self.persist()
    }

    /// 移除一个包（持久化）。
    pub fn remove(&self, id: &str) -> Result<bool, String> {
        let removed = {
            let mut packs = self.inner.packs.write();
            let before = packs.len();
            packs.retain(|p| p.id != id);
            packs.len() != before
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    /// 设置包的启用状态（持久化）。
    ///
    /// 写侧也走单家包归一（A2A v3.5.0 同口径）：只改读侧会造成「读入归一、
    /// 写回又写脏」——开关行为在重启前后反复。单家包开包时清空残留的
    /// 逐家停用标记，避免「包开着但唯一那家仍停用」的矛盾态被写回。
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let changed = {
            let mut packs = self.inner.packs.write();
            match packs.iter_mut().find(|p| p.id == id) {
                Some(p) if p.enabled != enabled => {
                    p.enabled = enabled;
                    // 单家包：包开关即那家的开关，子标记必须跟着清（否则
                    // provider_enabled() 仍判 false，界面说开着、路由说停用）
                    if p.single_provider_id().is_some() {
                        p.disabled_providers.clear();
                    }
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    /// 单独启停包内某一家 provider（持久化；上游 `set_provider_enabled` 同义）。
    ///
    /// 多提供商包只改 `disabled_providers`，不动包级 `enabled`：包停用时仍可
    /// 预配置逐家开关，等包启用后按这份名单生效。
    ///
    /// **单提供商包例外**（A2A v3.5.0 同口径）：包级 `enabled` 是那家唯一的
    /// 开关，停这家 = 停整包（包关掉、子标记清空），开这家 = 开整包。不这么
    /// 折会出现「包开着 + 唯一那家停用」的矛盾态：界面显示插件启用、数据面
    /// `provider_enabled()` 返回 false 不路由，用户点包开关也解不开（子开关是
    /// 隐藏的）。`clear` 而非 `push` 也保证再开包不被残留标记卡住。
    ///
    /// provider 必须由该包声明，否则报错（静默写入一条不存在的家只会在界面
    /// 造出幽灵开关）。
    ///
    /// 返回 `true` = 状态确有变化（调用方据此决定是否记日志）。
    pub fn set_provider_enabled(
        &self,
        pack_id: &str,
        provider: &str,
        enabled: bool,
    ) -> Result<bool, String> {
        let changed = {
            let mut packs = self.inner.packs.write();
            let pack = packs
                .iter_mut()
                .find(|p| p.id == pack_id)
                .ok_or_else(|| format!("插件包「{pack_id}」不存在"))?;
            if !pack.providers.iter().any(|pp| pp.id == provider) {
                return Err(format!("插件包「{pack_id}」未声明提供商「{provider}」"));
            }
            if pack.single_provider_id() == Some(provider) {
                // 单家包：改包级 enabled，子标记恒空
                let next = (enabled, pack.enabled, pack.disabled_providers.is_empty());
                pack.enabled = enabled;
                pack.disabled_providers.clear();
                matches!(next, (true, false, _) | (false, true, _) | (_, _, false))
            } else {
                let listed = pack.disabled_providers.iter().any(|id| id == provider);
                match (enabled, listed) {
                    (true, true) => {
                        pack.disabled_providers.retain(|id| id != provider);
                        true
                    }
                    (false, false) => {
                        pack.disabled_providers.push(provider.to_string());
                        true
                    }
                    _ => false,
                }
            }
        };
        if changed {
            self.persist()?;
        }
        Ok(changed)
    }

    /// 找到声明了某 provider 且**该家处于启用态**的包 id。
    ///
    /// 「启用态」= 包级 `enabled` ∧ 该家不在 `disabled_providers`（见
    /// [`PackMeta::provider_enabled`]）。逐家停用后数据面立即回落原生/无桥。
    pub fn find_pack_for_provider(&self, provider: &str) -> Option<String> {
        self.inner
            .packs
            .read()
            .iter()
            .find(|p| {
                p.provider_enabled(provider) && p.providers.iter().any(|pp| pp.id == provider)
            })
            .map(|p| p.id.clone())
    }

    /// 除 `exclude_pack` 之外，还有没有**启用中**的包声明了 `provider`。
    ///
    /// 停用/卸载一个包时用：`WasmHost` 按 provider 组织，不是按 pack。两个包声明
    /// 同一 provider 时（合法配置：一个只实现 chat、另一个只实现 catalog），
    /// 停掉其中一个不该把共享宿主一起关掉 —— 否则另一个包的数据面静默失效。
    pub fn other_enabled_pack_for_provider(
        &self,
        provider: &str,
        exclude_pack: &str,
    ) -> Option<String> {
        self.inner
            .packs
            .read()
            .iter()
            .find(|p| {
                p.id != exclude_pack
                    && p.provider_enabled(provider)
                    && p.providers.iter().any(|pp| pp.id == provider)
            })
            .map(|p| p.id.clone())
    }

    /// 是否由插件接管该 provider（**插件优先**）。
    ///
    /// 已装且启用的包声明了 `provider` 即接管；**逐家停用**（`disabled_providers`
    /// 名单内）即视为未接管——与上游 `provider_enabled` 同口径。唯一例外是硬
    /// 保留集合（[`HARD_RESERVED`]，AIGX 内部虚拟 id）。
    ///
    /// **插件加载失败的兜底不在这里**：本方法只看「声明 + 启用」这一静态
    /// 事实。数据面的 `PluginHostManager::process_for_provider` 与
    /// `ScriptHostManager::route_for_provider` 还会再查「进程是否在跑」——
    /// 插件崩了/在退避窗口 → 返回 `None` → `resolve.rs` 回落原生 bridge。
    /// 这样 `is_plugin_owned` 保持为纯静态判据（无运行期状态依赖），
    /// 单测稳定；「加载失败回落原生」由运行期两级查找保证。
    pub fn is_plugin_owned(&self, provider: &str) -> bool {
        if is_hard_reserved(provider) {
            return false;
        }
        self.find_pack_for_provider(provider).is_some()
    }

    fn persist(&self) -> Result<(), String> {
        let packs = self.inner.packs.read().clone();
        self.inner
            .store
            .put(KV_PACKS, &packs)
            .map_err(|e| format!("保存插件包注册表失败: {e}"))
    }
}

/// 该 provider 是否已有 AIGX 原生实现（`PROVIDERS` ∪ `SESSION_PROVIDERS`）。
///
/// **2026-11 起不再是归属判据**：插件优先，原生实现只在插件未装/停用/
/// 加载失败时兜底。保留本函数供数据面「原生兜底」路径与管理面展示用
/// （「这家有原生实现，插件接管后原生降为兜底」）。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn is_native_provider(provider: &str) -> bool {
    crate::account_session::registry::PROVIDERS
        .iter()
        .any(|s| s.kind == provider)
        || crate::account_session::registry::SESSION_PROVIDERS
            .iter()
            .any(|(kind, _)| *kind == provider)
}

/// 硬保留集合：AIGX **内部虚拟 id**，插件不得接管（声明了也无效）。
///
/// 这些不是「有原生实现的家」（那些已改为插件优先），而是**不指向真实
/// 上游**的网关自身转发目标：
/// - `cloudflare`：CF Worker 专用桥（`hub.get_specialized("cloudflare")`），
///   请求由网关按 Worker 协议构造，插件没有对等实现；
/// - `hub`：Hub 虚拟提供商标识（无独立上游）；
/// - `custom-*` 前缀：运维自定义渠道的保留前缀（`identity.mjs` 也禁止插件
///   产出该前缀）。
///
/// 集合刻意极小：每加一项都要问「插件真的没法接管吗」。
pub const HARD_RESERVED: &[&str] = &["cloudflare", "hub"];

/// provider 是否属于硬保留集合（含 `custom-` 前缀）。
pub fn is_hard_reserved(provider: &str) -> bool {
    HARD_RESERVED.contains(&provider) || provider.starts_with("custom-")
}

/// 该 provider 是否使用插件管理的动态登录（对齐 beta.2 `registry::dynamic_login`）。
///
/// **语义**：provider 由 Wasm 插件接管时返回 `true`（插件负责登录交互，
/// 不走 AIGX 原生的静态 API Key 路径）。脚本插件由 `ScriptHostManager`
/// 在运行期检查，不在此处（与上游 `adapter.dynamic_login()` 同口径：
/// 上游也是查运行期 adapter 是否存在）。
///
/// 硬保留集合（[`HARD_RESERVED`]）永远返回 `false`：它们是 AIGX 原生虚拟 id。
///
/// 当前无内部调用方（beta.2 形状 API 别名，待登录闸门接线后移除 allow）。
#[allow(dead_code)]
pub fn dynamic_login(provider: &str) -> bool {
    if is_hard_reserved(provider) {
        return false;
    }
    super::wasm::owns(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_registry() -> PackRegistry {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-packs-{}-{seq}", std::process::id()));
        // PID 会复用：上次异常退出残留的同名目录会让「默认空」断言偶发失败
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        PackRegistry::new(Arc::new(FileStore::new(dir)))
    }

    fn meta(id: &str, providers: &[&str], enabled: bool) -> PackMeta {
        PackMeta {
            id: id.into(),
            name: id.into(),
            version: "3.4.2".into(),
            protocol: 1,
            providers: providers
                .iter()
                .map(|p| PackProvider {
                    id: (*p).into(),
                    label: (*p).into(),
                    order: 0,
                    default_enabled: true,
                })
                .collect(),
            entry: "bin/plugin".into(),
            signed: false,
            installed_at: 0,
            enabled,
            ..Default::default()
        }
    }

    /// 两个包声明同一 provider 时，「还有谁在用它」必须能问出来 ——
    /// `manager.disable` 靠它决定要不要关共享宿主（关了会让另一个包静默失效）。
    #[test]
    fn other_enabled_pack_for_provider_reports_the_sharer() {
        let reg = temp_registry();
        reg.load().unwrap();
        // 一个包声明 chat 侧、另一个声明 catalog 侧，同一个 provider
        reg.upsert(meta("pack-a", &["trae"], true)).unwrap();
        reg.upsert(meta("pack-b", &["trae"], true)).unwrap();

        // 从 pack-a 的视角：pack-b 仍在用
        assert_eq!(
            reg.other_enabled_pack_for_provider("trae", "pack-a")
                .as_deref(),
            Some("pack-b"),
            "另一个启用包仍声明该 provider 时必须报出来（否则 disable 会连带停掉它）"
        );
        // 从 pack-b 的视角：pack-a 仍在用
        assert_eq!(
            reg.other_enabled_pack_for_provider("trae", "pack-b")
                .as_deref(),
            Some("pack-a")
        );

        // pack-b 停用后，pack-a 就是唯一持有者 → 可以安全关宿主
        reg.set_enabled("pack-b", false).unwrap();
        assert_eq!(
            reg.other_enabled_pack_for_provider("trae", "pack-a"),
            None,
            "唯一的另一个包已停用时必须返回 None（此时关宿主是安全的）"
        );

        // 没人用的 provider 自然也是 None（正向对照，防「恒返回 Some」蒙对）
        assert_eq!(reg.other_enabled_pack_for_provider("kimi", "pack-a"), None);
    }

    #[test]
    fn upsert_and_persist_roundtrip() {
        let reg = temp_registry();
        reg.load().unwrap();
        assert!(reg.list().is_empty());
        reg.upsert(meta("agent2api-providers", &["kiro"], true))
            .unwrap();
        assert_eq!(reg.list().len(), 1);

        // 重启：新建实例从 KV 恢复
        let store = reg.inner.store.clone();
        let reg2 = PackRegistry::new(store);
        reg2.load().unwrap();
        assert_eq!(reg2.list().len(), 1);
        assert_eq!(reg2.get("agent2api-providers").unwrap().version, "3.4.2");
    }

    #[test]
    fn upsert_preserves_enabled_choice() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro"], false)).unwrap();
        // 重装（新元数据 enabled=true）不应覆盖运维的「停用」选择
        reg.upsert(meta("p", &["kiro"], true)).unwrap();
        assert!(!reg.get("p").unwrap().enabled, "重装应保留既有 enabled");
    }

    #[test]
    fn remove_works() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro"], true)).unwrap();
        assert!(reg.remove("p").unwrap());
        assert!(!reg.remove("p").unwrap());
        assert!(reg.list().is_empty());
    }

    #[test]
    fn find_pack_requires_enabled() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro", "astudio"], false)).unwrap();
        // 停用的包不算数
        assert_eq!(reg.find_pack_for_provider("kiro"), None);
        reg.set_enabled("p", true).unwrap();
        assert_eq!(reg.find_pack_for_provider("kiro"), Some("p".into()));
        assert_eq!(reg.find_pack_for_provider("astudio"), Some("p".into()));
        assert_eq!(reg.find_pack_for_provider("nope"), None);
    }

    #[test]
    fn provider_enabled_three_states() {
        let m = meta("p", &["kiro"], true);
        assert!(m.provider_enabled("kiro"), "包启用且未停用该家 = 启用");
        let mut off = meta("p", &["kiro"], true);
        off.disabled_providers = vec!["kiro".into()];
        assert!(!off.provider_enabled("kiro"), "停用名单内 = 停用");
        let mut pack_off = meta("p", &["kiro"], false);
        pack_off.disabled_providers.clear();
        assert!(!pack_off.provider_enabled("kiro"), "包级停用 = 全部停用");
        // 「是否声明」是另一条判据（find_pack_for_provider 同时校验）：
        // provider_enabled 只看启停名单，未声明的家不会被逐个列进名单。
    }

    #[test]
    fn find_pack_skips_disabled_provider() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro", "astudio"], true)).unwrap();
        assert_eq!(reg.find_pack_for_provider("kiro"), Some("p".into()));
        reg.set_provider_enabled("p", "kiro", false).unwrap();
        assert_eq!(
            reg.find_pack_for_provider("kiro"),
            None,
            "逐家停用后不再归插件"
        );
        assert!(
            !reg.is_plugin_owned("kiro"),
            "is_plugin_owned 同样过滤停用家"
        );
        assert_eq!(
            reg.find_pack_for_provider("astudio"),
            Some("p".into()),
            "同包其它家不受影响"
        );
    }

    #[test]
    fn set_provider_enabled_persists() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro", "astudio"], true)).unwrap();
        assert!(reg.set_provider_enabled("p", "kiro", false).unwrap());
        // 重复设置同值 = 无变化（调用方据此免记日志）
        assert!(!reg.set_provider_enabled("p", "kiro", false).unwrap());

        // 重启：新实例从 KV 恢复停用名单
        let store = reg.inner.store.clone();
        let reg2 = PackRegistry::new(store);
        reg2.load().unwrap();
        let m = reg2.get("p").unwrap();
        assert_eq!(m.disabled_providers, vec!["kiro".to_string()]);

        // 重新启用 = 从名单移除并落盘
        assert!(reg2.set_provider_enabled("p", "kiro", true).unwrap());
        assert!(reg2.get("p").unwrap().disabled_providers.is_empty());
    }

    #[test]
    fn set_provider_enabled_rejects_unknown() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro"], true)).unwrap();
        assert!(reg.set_provider_enabled("nope", "kiro", false).is_err());
        assert!(
            reg.set_provider_enabled("p", "astudio", false).is_err(),
            "未声明的家不能写入停用名单"
        );
    }

    #[test]
    fn reinstall_inherits_disabled_providers() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro", "astudio"], true)).unwrap();
        reg.set_provider_enabled("p", "kiro", false).unwrap();
        // 重装（新元数据名单为空）应继承运维的逐家选择
        reg.upsert(meta("p", &["kiro", "astudio"], true)).unwrap();
        let m = reg.get("p").unwrap();
        assert_eq!(
            m.disabled_providers,
            vec!["kiro".to_string()],
            "重装应保留既有 provider 停用名单"
        );
        assert!(!m.provider_enabled("kiro"));
        assert!(m.provider_enabled("astudio"));
    }

    #[test]
    fn native_providers_are_recognized() {
        // 这几家 AIGX 有原生实现（原生实现降为「插件未装时的兜底」，
        // 但 is_native_provider 的识别能力保留）
        assert!(is_native_provider("workbuddy"));
        assert!(is_native_provider("atomcode"));
        assert!(is_native_provider("cline"));
        // 插件专属家（AIGX 无原生实现）
        assert!(!is_native_provider("kiro"));
        assert!(!is_native_provider("astudio"));
        assert!(!is_native_provider("tabbit"));
    }

    /// **插件优先**：已启用的插件声明了原生家，就由插件接管（原生降为兜底）。
    #[test]
    fn plugin_owned_is_plugin_first() {
        let reg = temp_registry();
        reg.load().unwrap();
        // 插件声明了原生家 workbuddy 与插件专属家 kiro
        reg.upsert(meta("p", &["workbuddy", "kiro"], true)).unwrap();
        assert!(
            reg.is_plugin_owned("workbuddy"),
            "插件优先：已启用插件声明了原生家也接管"
        );
        assert!(reg.is_plugin_owned("kiro"));
        // 停用后不再接管（数据面回落原生）
        reg.set_enabled("p", false).unwrap();
        assert!(!reg.is_plugin_owned("kiro"));
        assert!(!reg.is_plugin_owned("workbuddy"));
    }

    /// 硬保留集合（内部虚拟 id）即使被插件声明也不接管。
    #[test]
    fn hard_reserved_is_never_plugin_owned() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta(
            "p",
            &["cloudflare", "hub", "custom-abc", "kiro"],
            true,
        ))
        .unwrap();
        assert!(!reg.is_plugin_owned("cloudflare"), "CF 桥是网关内部实现");
        assert!(!reg.is_plugin_owned("hub"), "Hub 是虚拟标识");
        assert!(
            !reg.is_plugin_owned("custom-abc"),
            "custom- 前缀是运维自定义渠道保留前缀"
        );
        assert!(reg.is_plugin_owned("kiro"), "普通 provider 正常接管");
    }

    #[test]
    fn hard_reserved_set_is_small_and_explicit() {
        // 集合刻意极小：改它必须同步本测试（防止悄悄扩大保留面）
        assert_eq!(HARD_RESERVED, &["cloudflare", "hub"]);
        assert!(is_hard_reserved("cloudflare"));
        assert!(is_hard_reserved("hub"));
        assert!(is_hard_reserved("custom-xyz"));
        assert!(!is_hard_reserved("openai"));
        assert!(!is_hard_reserved("workbuddy"));
    }

    /// 插件**未装**时：任何 provider 都不归插件（数据面走原生/无桥）。
    #[test]
    fn no_pack_means_not_plugin_owned() {
        let reg = temp_registry();
        reg.load().unwrap();
        for provider in ["kiro", "workbuddy", "openai"] {
            assert!(
                !reg.is_plugin_owned(provider),
                "未装插件时 {provider} 不归插件"
            );
        }
    }

    #[test]
    fn entry_path_joins_version_dir() {
        let m = meta("p", &[], true);
        let root = std::path::Path::new("/data/plugins");
        assert_eq!(m.version_dir(root), root.join("p").join("3.4.2"));
        assert_eq!(
            m.entry_path(root),
            root.join("p").join("3.4.2").join("bin/plugin")
        );
    }

    /* ─── 单提供商包归一（A2A v3.5.0 #40 #43 #45 同口径）───────── */

    /// `single_provider_id()` 只对「恰好一家」给出 Some。
    #[test]
    fn single_provider_id_only_matches_exactly_one() {
        assert_eq!(
            meta("p", &["kiro"], true).single_provider_id(),
            Some("kiro")
        );
        assert_eq!(
            meta("p", &["kiro", "astudio"], true).single_provider_id(),
            None
        );
        assert_eq!(meta("p", &[], true).single_provider_id(), None);
    }

    /// 矛盾态（包启用 + 唯一那家在停用名单）→ 折成包停用并清子标记。
    /// 这是本次修复的核心：修复前 `enabled=true` 与 `provider_enabled=false`
    /// 同时成立，界面说「插件已启用」、数据面却完全不路由这家。
    #[test]
    fn normalize_folds_single_provider_contradiction() {
        let mut m = meta("p", &["kiro"], true);
        m.disabled_providers = vec!["kiro".into()];
        assert!(m.normalize_single_provider(), "矛盾态应被归一");
        assert!(!m.enabled, "折成包停用");
        assert!(m.disabled_providers.is_empty(), "子标记必须清空");

        // 幂等：再归一一次无变化
        assert!(!m.normalize_single_provider(), "已是自洽态，不应再改");
    }

    /// 自洽态 / 多提供商包 / 空包都不该被归一（避免误伤）。
    #[test]
    fn normalize_leaves_consistent_and_multi_provider_alone() {
        // 包启用 + 子标记空 = 自洽
        let mut consistent = meta("p", &["kiro"], true);
        assert!(!consistent.normalize_single_provider());
        assert!(consistent.enabled);

        // 多提供商包：逐家停用是合法态，不能折成包停用
        let mut multi = meta("p", &["kiro", "astudio"], true);
        multi.disabled_providers = vec!["kiro".into()];
        assert!(!multi.normalize_single_provider());
        assert!(multi.enabled, "多提供商包保持启用");
        assert_eq!(multi.disabled_providers, vec!["kiro".to_string()]);

        // 包本就停用：无需再折
        let mut already_off = meta("p", &["kiro"], false);
        already_off.disabled_providers = vec!["kiro".into()];
        assert!(!already_off.normalize_single_provider());
    }

    /// load() 读入即归一：磁盘上的历史矛盾态在启动时被修正，
    /// 且修正结果落进内存视图（get 可见）。
    #[test]
    fn load_normalizes_historical_contradiction() {
        let reg = temp_registry();
        // 直接写入矛盾态（绕过 upsert 的归一——模拟历史数据）
        {
            let mut m = meta("p", &["kiro"], true);
            m.disabled_providers = vec!["kiro".into()];
            reg.inner.store.put(KV_PACKS, &vec![m]).unwrap();
        }
        reg.load().unwrap();
        let m = reg.get("p").expect("包应还在");
        assert!(!m.enabled, "load 应把矛盾态折成包停用");
        assert!(m.disabled_providers.is_empty());
        assert!(!reg.is_plugin_owned("kiro"));
    }

    /// 单家包 `set_provider_enabled`：改包级 enabled，子标记恒空（不写名单）。
    /// 修复前这里是 push 进 disabled_providers，包仍 enabled=true →
    /// `provider_enabled()` 返回 false，用户点包开关也解不开（子开关隐藏）。
    #[test]
    fn set_provider_enabled_folds_single_provider_pack() {
        let reg = temp_registry();
        reg.load().unwrap();
        reg.upsert(meta("p", &["kiro"], true)).unwrap();
        assert!(reg.is_plugin_owned("kiro"));

        // 停唯一那家 = 停整包
        assert!(reg.set_provider_enabled("p", "kiro", false).unwrap());
        let m = reg.get("p").unwrap();
        assert!(!m.enabled, "单家包应折成包停用");
        assert!(m.disabled_providers.is_empty(), "不得写进子标记名单");
        assert!(!reg.is_plugin_owned("kiro"));

        // 再开 = 开整包（不被残留子开关卡住）
        assert!(reg.set_provider_enabled("p", "kiro", true).unwrap());
        assert!(reg.get("p").unwrap().enabled);
        assert!(reg.is_plugin_owned("kiro"));

        // 重复设置同值 = 无变化
        assert!(!reg.set_provider_enabled("p", "kiro", true).unwrap());
    }

    /// 单家包 `set_enabled(true)` 必须清残留子标记：否则「开包」后
    /// `provider_enabled()` 仍判 false（包开了但插件没跑）。
    #[test]
    fn set_enabled_clears_stale_provider_marker() {
        let reg = temp_registry();
        reg.load().unwrap();
        // 造一个「包停用 + 唯一那家被列进子标记」的历史残留
        let mut m = meta("p", &["kiro"], false);
        m.disabled_providers = vec!["kiro".into()];
        reg.inner.store.put(KV_PACKS, &vec![m]).unwrap();
        reg.load().unwrap();
        assert!(!reg.is_plugin_owned("kiro"));

        assert!(reg.set_enabled("p", true).unwrap());
        let m = reg.get("p").unwrap();
        assert!(m.enabled);
        assert!(
            m.disabled_providers.is_empty(),
            "开单家包时残留子标记必须清掉，否则 provider_enabled 仍 false"
        );
        assert!(reg.is_plugin_owned("kiro"), "包开了就该接管");

        // 多提供商包不受此影响：逐家停用是合法预配置
        reg.upsert(meta("q", &["kiro", "astudio"], false)).unwrap();
        reg.set_provider_enabled("q", "kiro", false).unwrap();
        reg.set_enabled("q", true).unwrap();
        let q = reg.get("q").unwrap();
        assert!(q.enabled);
        assert_eq!(
            q.disabled_providers,
            vec!["kiro".to_string()],
            "多提供商包的逐家停用不该被 set_enabled 清掉"
        );
    }
}
