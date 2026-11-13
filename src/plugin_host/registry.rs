//! 插件包元数据与 provider 归属路由。
//!
//! ## 归属规则（硬约束）
//!
//! 插件**只接管**「插件声明了、且 AIGX 无原生实现」的 provider：
//!
//! ```text
//! is_plugin_owned(p) = 已装插件声明了 p  &&  !is_native_provider(p)
//! ```
//!
//! 原生已有的家（workbuddy / qoder / atomcode …）一律走原生 bridge，
//! 插件不覆盖——避免「装了个插件反而把能用的家换掉」。
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
    /// 是否通过验签（未签名 = false，允许装但界面标黄）。
    pub signed: bool,
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
}

/// 包声明的一家 provider。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct PackProvider {
    pub id: String,
    pub label: String,
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
    pub fn load(&self) -> Result<(), String> {
        let packs = self
            .inner
            .store
            .get::<Vec<PackMeta>>(KV_PACKS)
            .map_err(|e| format!("读取插件包注册表失败: {e}"))?
            .unwrap_or_default();
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
                    // 保留既有 enabled（重装不应重置运维的选择）
                    meta.enabled = existing.enabled;
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
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool, String> {
        let changed = {
            let mut packs = self.inner.packs.write();
            match packs.iter_mut().find(|p| p.id == id) {
                Some(p) if p.enabled != enabled => {
                    p.enabled = enabled;
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

    /// 找到声明了某 provider 且**已启用**的包 id。
    pub fn find_pack_for_provider(&self, provider: &str) -> Option<String> {
        self.inner
            .packs
            .read()
            .iter()
            .find(|p| p.enabled && p.providers.iter().any(|pp| pp.id == provider))
            .map(|p| p.id.clone())
    }

    /// 是否由插件接管该 provider（插件声明 && 非原生）。
    pub fn is_plugin_owned(&self, provider: &str) -> bool {
        !is_native_provider(provider) && self.find_pack_for_provider(provider).is_some()
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
/// 原生实现存在时，插件不接管——避免「装插件反而换掉能用的家」。
pub fn is_native_provider(provider: &str) -> bool {
    crate::account_session::registry::PROVIDERS
        .iter()
        .any(|s| s.kind == provider)
        || crate::account_session::registry::SESSION_PROVIDERS
            .iter()
            .any(|(kind, _)| *kind == provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_registry() -> PackRegistry {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-packs-{}-{seq}", std::process::id()));
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
    fn native_providers_are_recognized() {
        // 这几家 AIGX 有原生实现
        assert!(is_native_provider("workbuddy"));
        assert!(is_native_provider("atomcode"));
        assert!(is_native_provider("cline"));
        // 插件专属家（AIGX 无原生实现）
        assert!(!is_native_provider("kiro"));
        assert!(!is_native_provider("astudio"));
        assert!(!is_native_provider("tabbit"));
    }

    #[test]
    fn plugin_owned_requires_non_native_and_enabled() {
        let reg = temp_registry();
        reg.load().unwrap();
        // 插件声明了原生家 workbuddy 与插件专属家 kiro
        reg.upsert(meta("p", &["workbuddy", "kiro"], true)).unwrap();
        // 原生家不被接管
        assert!(!reg.is_plugin_owned("workbuddy"), "原生家应走原生 bridge");
        // 插件专属家被接管
        assert!(reg.is_plugin_owned("kiro"));
        // 停用后不再接管
        reg.set_enabled("p", false).unwrap();
        assert!(!reg.is_plugin_owned("kiro"));
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
}
