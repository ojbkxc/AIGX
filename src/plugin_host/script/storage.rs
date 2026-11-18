//! 脚本包的持久化：KV 权威状态 + 磁盘投影。
//!
//! ## 两处存储，各司其职
//!
//! | 位置 | 作用 | 谁写 |
//! |---|---|---|
//! | KV 键 `plugin:script-packs` | **权威状态**（operations 读它决定启停） | operations |
//! | `{script_root}/registry.json` | **磁盘投影**（js-host 启动时读） | 本模块 |
//!
//! 为什么两份都要：KV 是网关的统一状态面（与原生包的 `plugin:packs` 平行，
//! 复用现有备份/同步链路）；`registry.json` 是给 Bun 宿主进程的文件视图——
//! 宿主在 `{script_root}/host/` 下运行，用相对路径读同级的 registry.json，
//! 不必再经 RPC 回查网关。写入顺序固定：**先 KV 成功，再写投影**；投影失败
//! 只记 warn 不回滚（下次写会重试），因为 KV 才是事实源。
//!
//! ## 为什么包目录用 hash 名
//!
//! 见 `ScriptPackage::package_dir_name`：npm scoped 名带 `/`，直接进路径会
//! 造出嵌套目录。目录名计算收敛在 types.rs，本模块只调用。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::types::{script_plugins_root, ScriptPackage};

/// KV 键（与原生包的 `plugin:packs` 平行，读写互不干扰）。
pub const KV_KEY: &str = "plugin:script-packs";

/// 磁盘投影文件名。
const REGISTRY_FILE: &str = "registry.json";

/// provider 别名 → 真实 provider id。
///
/// 包声明的 id 与运行时实际导出的 id 可能不同（冲突时 js-host 会加后缀），
/// 因此单独记一层。由 js-host 握手上报填充，安装时留空。
pub type Identities = BTreeMap<String, BTreeMap<String, String>>;

/// 磁盘投影的形状（js-host 按此读）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct RegistryProjection {
    /// 元数据版本，将来改结构时用于拒绝旧文件。
    #[serde(default = "default_version")]
    pub version: u32,
    /// 全部已装包（含未启用的——宿主自行跳过）。
    #[serde(default)]
    pub packages: Vec<ScriptPackage>,
    /// 别名表。
    #[serde(default)]
    pub identities: Identities,
}

fn default_version() -> u32 {
    1
}

/// 脚本根目录。
pub fn root(data_dir: &Path) -> PathBuf {
    script_plugins_root(data_dir)
}

/// 安装前落盘：写包目录（调用方已解包）+ 更新 KV + 更新投影。
///
/// 实际的下载/解包在 `packages.rs` 完成，本函数只负责「把结果登记下来」。
pub fn register(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    package: ScriptPackage,
) -> Result<()> {
    // 1. KV 权威状态：读现有 → 替换同 id 同 version → 写回。
    let mut packages: Vec<ScriptPackage> = store.get(KV_KEY).ok().flatten().unwrap_or_default();
    packages.retain(|p| !(p.id == package.id && p.version == package.version));
    packages.push(package);
    store
        .put(KV_KEY, &packages)
        .context("写入 KV plugin:script-packs 失败")?;

    // 2. 磁盘投影（失败只记 warn：KV 已是事实源，下次写会重试）。
    if let Err(err) = write_projection(data_dir, &packages, &load_identities(store)) {
        tracing::warn!("[Script] 写 registry.json 投影失败（KV 已落盘，不影响运行）：{err:#}");
    }
    Ok(())
}

/// 批量替换全部包状态（启停/配置整表落盘用）。
///
/// 与 [`register`] 的区别：这里把调用方给的**整表**原样写回（KV + 投影），
/// 不再逐条合并——调用方已持有完整列表（如 `operations::set_enabled`
/// 改完一个包后的全量快照）。
pub fn replace_all(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    packages: &[ScriptPackage],
) -> Result<()> {
    // put 的泛型参数默认 Sized，切片 &[ScriptPackage] 过不去——转 Vec。
    store
        .put(KV_KEY, &packages.to_vec())
        .context("写入 KV plugin:script-packs 失败")?;
    if let Err(err) = write_projection(data_dir, packages, &load_identities(store)) {
        tracing::warn!("[Script] 写 registry.json 投影失败（KV 已落盘，不影响运行）：{err:#}");
    }
    Ok(())
}

/// 读全部已装包（KV 为准）。
pub fn load_packages(store: &crate::storage::FileStore) -> Vec<ScriptPackage> {
    store.get(KV_KEY).ok().flatten().unwrap_or_default()
}

/// 删除一个包：KV + 投影 + 磁盘目录。
pub fn remove(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    package_id: &str,
    version: Option<&str>,
) -> Result<()> {
    let mut packages = load_packages(store);
    let before = packages.len();
    packages.retain(|p| {
        let same_id = p.id == package_id;
        let same_ver = version.is_none_or(|v| p.version == v);
        !(same_id && same_ver)
    });
    if packages.len() == before {
        anyhow::bail!("包不存在：{package_id}");
    }
    store.put(KV_KEY, &packages).context("更新 KV 失败")?;
    let _ = write_projection(data_dir, &packages, &load_identities(store));
    Ok(())
}

/// 删除包的磁盘目录（卸载时用）。
///
/// 独立于 [`remove`]：先在 KV 摘掉，再删磁盘；删磁盘失败只记 warn
/// （元数据已清，残留目录不会被执行，下次安装会覆盖）。
pub fn remove_package_dir(data_dir: &Path, package: &ScriptPackage) {
    let dir = package.version_dir(&root(data_dir));
    if let Err(err) = std::fs::remove_dir_all(&dir) {
        if err.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("[Script] 删除包目录失败 {}：{err}", dir.display());
        }
    }
}

/// 读别名表。
pub fn load_identities(store: &crate::storage::FileStore) -> Identities {
    store.get(IDENTITIES_KEY).ok().flatten().unwrap_or_default()
}

/// 写别名表（js-host 握手上报后调）。
pub fn save_identities(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    identities: Identities,
) -> Result<()> {
    store
        .put(IDENTITIES_KEY, &identities)
        .context("写入 KV 别名表失败")?;
    let _ = write_projection(data_dir, &load_packages(store), &identities);
    Ok(())
}

/// 别名表 KV 键（与包列表分开，避免大结构互相覆盖）。
const IDENTITIES_KEY: &str = "plugin:script-identities";

/// 写磁盘投影（tmp + rename 原子写）。
fn write_projection(
    data_dir: &Path,
    packages: &[ScriptPackage],
    identities: &Identities,
) -> Result<()> {
    let root = root(data_dir);
    std::fs::create_dir_all(&root).with_context(|| format!("创建目录失败：{}", root.display()))?;
    let projection = RegistryProjection {
        version: default_version(),
        packages: packages.to_vec(),
        identities: identities.clone(),
    };
    let json = serde_json::to_vec_pretty(&projection).context("序列化 registry 投影失败")?;
    // 投影给宿主读，不该无限增长：包数量有 64 上限，超过 32MB 说明有异常数据。
    if json.len() > 32 * 1024 * 1024 {
        anyhow::bail!("registry 投影过大：{} bytes", json.len());
    }
    let final_path = root.join(REGISTRY_FILE);
    let tmp = root.join(".registry.json.tmp");
    std::fs::write(&tmp, &json).context("写临时文件失败")?;
    std::fs::rename(&tmp, &final_path).context("原子替换 registry.json 失败")?;
    Ok(())
}

/// 读磁盘投影（js-host 侧或排障用）。
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub fn read_projection(data_dir: &Path) -> Result<RegistryProjection> {
    let path = root(data_dir).join(REGISTRY_FILE);
    let bytes = std::fs::read(&path).with_context(|| format!("读取失败：{}", path.display()))?;
    let projection: RegistryProjection =
        serde_json::from_slice(&bytes).with_context(|| format!("解析失败：{}", path.display()))?;
    if projection.version != default_version() {
        anyhow::bail!(
            "registry 投影版本不支持：{}（期望 {}）",
            projection.version,
            default_version()
        );
    }
    Ok(projection)
}

// 宿主文件目录（`host/{sha256前24}/`）的落盘归 `host_rs` 模块所有——
// 它持有内嵌的宿主文件与内容哈希推导；本模块不再重复实现，避免两套
// hash 口径产出不同目录。

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, crate::storage::FileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::storage::FileStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    fn pkg(id: &str, version: &str) -> ScriptPackage {
        ScriptPackage {
            id: id.to_string(),
            version: version.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn register_writes_kv_and_projection() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("@magpie-community/kiro", "1.0.0")).unwrap();

        let packages = load_packages(&store);
        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].id, "@magpie-community/kiro");

        let projection = read_projection(data_dir).unwrap();
        assert_eq!(projection.version, 1);
        assert_eq!(projection.packages.len(), 1);
    }

    #[test]
    fn register_replaces_same_id_and_version() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("pkg-a", "1.0.0")).unwrap();
        let mut updated = pkg("pkg-a", "1.0.0");
        updated.enabled = true;
        register(&store, data_dir, updated).unwrap();
        let packages = load_packages(&store);
        assert_eq!(packages.len(), 1, "同 id 同版本应替换而不是追加");
        assert!(packages[0].enabled);
    }

    #[test]
    fn register_keeps_other_versions() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("pkg-a", "1.0.0")).unwrap();
        register(&store, data_dir, pkg("pkg-a", "2.0.0")).unwrap();
        assert_eq!(load_packages(&store).len(), 2);
    }

    #[test]
    fn remove_drops_only_target() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("a", "1.0.0")).unwrap();
        register(&store, data_dir, pkg("b", "1.0.0")).unwrap();
        remove(&store, data_dir, "a", None).unwrap();
        let remaining = load_packages(&store);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "b");
    }

    #[test]
    fn remove_unknown_errors() {
        let (dir, store) = temp_store();
        assert!(remove(&store, dir.path(), "nope", None).is_err());
    }

    #[test]
    fn identities_roundtrip() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        let mut ids = Identities::new();
        ids.entry("pkg-a".into())
            .or_default()
            .insert("kiro".into(), "kiro-magpie".into());
        save_identities(&store, data_dir, ids.clone()).unwrap();
        assert_eq!(load_identities(&store), ids);
        // 投影里也应带上
        assert_eq!(read_projection(data_dir).unwrap().identities, ids);
    }

    #[test]
    fn projection_rejects_bad_version() {
        let dir = tempfile::tempdir().unwrap();
        let root = root(dir.path());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join(REGISTRY_FILE),
            b"{\"version\":99,\"packages\":[],\"identities\":{}}",
        )
        .unwrap();
        assert!(read_projection(dir.path()).is_err());
    }
}
