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

/// 脚本包注册表的写锁（进程级）。
///
/// 所有「读 KV → 改内存 → 整表写回」的路径都必须在同一把锁内完成，
/// 否则并发写会互相覆盖：`set_enabled` 刚把包打开，同时另一个请求拿着
/// 更早读到的快照写回，启停状态就被旧值顶回去了（数据面按旧状态路由，
/// 界面显示新状态）。典型受害者是 `configure_settings`——它在宿主 RPC
/// 上等最多 10s 才写回，期间任何写都会丢。
///
/// `parking_lot::Mutex` 是同步锁：所有持锁段都不 await（KV 与投影都是
/// 本地文件 IO），不构成跨 await 持锁。
static WRITE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// 在写锁内执行一次「读现有 → 改 → 写回」事务。
///
/// 读失败**必须**中止（`try_load_packages` 的严格语义）：把损坏的 KV
/// 当空表改完写回，等于静默抹掉整个注册表。
///
/// 投影写失败仍然只 warn（KV 才是事实源，下次写会重试），但**不能**因此
/// 让调用方以为整笔事务失败——KV 已经落了。
pub fn mutate_packages<T>(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    mutate: impl FnOnce(&mut Vec<ScriptPackage>) -> Result<T>,
) -> Result<T> {
    let _guard = WRITE_LOCK.lock();
    let mut packages = try_load_packages(store)?;
    let out = mutate(&mut packages)?;
    store
        .put(KV_KEY, &packages)
        .context("写入 KV plugin:script-packs 失败")?;
    // 别名表也必须**严格读**：`load_identities` 是「读坏当空表」的宽松版，
    // 用它写投影会把 registry.json 里的 `identities: {}` 覆写掉 —— 宿主随后
    // 按空别名表加载，所有「包声明的 id ≠ 运行时 id」的脚本包静默路由到默认
    // provider（`save_identities` 早就为此改成严格读，这条路径当时漏了）。
    match try_load_identities(store) {
        Ok(identities) => {
            if let Err(err) = write_projection(data_dir, &packages, &identities) {
                tracing::warn!(
                    "[Script] 写 registry.json 投影失败（KV 已落盘，不影响运行）：{err:#}"
                );
            }
        }
        Err(err) => {
            // KV 的包表已落盘（这是权威状态），投影这半笔失败只能留痕：
            // 回滚包表会让「安装成功」变成「安装失败」而磁盘目录已经写好了。
            tracing::warn!(
                "[Script] 读 KV 别名表失败，本次不写 registry.json 投影（保留旧投影，避免覆写成空表）：{err:#}"
            );
        }
    }
    Ok(out)
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
    // KV 权威状态：读现有 → 替换同 id 同 version → 写回，全程持写锁。
    //
    // 严格读：读失败**必须**中止。旧实现用 `.ok().flatten().unwrap_or_default()`，
    // 于是「KV 读坏」被当成「一个包都没装」，紧接着把只含新包的整表写回去 ——
    // 其余已装包在这一刻从注册表里永久消失（磁盘目录还在，但没有任何入口
    // 再列出它们）。文件不存在（首次安装）仍是正常的空表。
    mutate_packages(store, data_dir, |packages| {
        packages.retain(|p| !(p.id == package.id && p.version == package.version));
        packages.push(package);
        Ok(())
    })
}

/// 批量替换全部包状态（启停/配置整表落盘用）。
///
/// 与 [`register`] 的区别：这里把调用方给的**整表**原样写回（KV + 投影），
/// 不再逐条合并——调用方已持有完整列表（如 `operations::set_enabled`
/// 改完一个包后的全量快照）。
///
/// 注意：调用方若在拿到快照后 await 过（如等宿主 RPC），快照可能已过期，
/// 整表写回会覆盖期间的其它写。这类路径必须改用 [`mutate_packages`]
/// 在锁内重读后再改（见 `runtime::configure_settings_with`）。
/// **只给测试用**：生产路径一律走 [mutate_packages]。
///
/// 整表覆盖是丢更新的载体（读快照 → await → 整表写回，期间别人的写被抹掉），
/// 审计把生产调用点全部换成了 mutate_packages，这个函数只剩测试在造初始状态时用。
#[cfg(test)]
pub fn replace_all(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    packages: &[ScriptPackage],
) -> Result<()> {
    mutate_packages(store, data_dir, |current| {
        current.clear();
        current.extend_from_slice(packages);
        Ok(())
    })
}

/// 读全部已装包（KV 为准），**读失败是错误**。
///
/// 写路径必须用这一份：`register` / `remove` 都是「读现有 → 改 → 整表写回」。
/// 若读失败被当成空集，写回就会把**其它所有已装包**一起抹掉 —— 与
/// `config.rs` 那次「config.toml 读坏 → 整份配置静默丢弃」同族，只是这里
/// 丢的是脚本包注册表（磁盘目录还在，但宿主再也看不到它们）。
fn try_load_packages(store: &crate::storage::FileStore) -> Result<Vec<ScriptPackage>> {
    store
        .get(KV_KEY)
        .context("读取 KV plugin:script-packs 失败（拒绝以空表覆盖既有注册）")
        .map(Option::unwrap_or_default)
}

/// 读全部已装包（KV 为准）。
///
/// 读取时归一化（A2A v3.5.0 `normalize_single_provider` 对齐）：历史数据里
/// 「包启用 + 唯一 provider 被子级停用」是一个矛盾态（宿主不会路由这家，
/// 界面却显示包开着）。折合成包停用：保留用户的停用选择，同时再开包时
/// 不会被残留的子开关卡住。
///
/// 只读路径（列表/路由计算）没有「写回」这一步，读失败按空表处理不会造成
/// 数据损失，但**必须留痕**：否则「脚本包全没了」与「KV 坏了」在日志里
/// 长得一模一样。
pub fn load_packages(store: &crate::storage::FileStore) -> Vec<ScriptPackage> {
    let mut packages = match try_load_packages(store) {
        Ok(packages) => packages,
        Err(err) => {
            tracing::warn!("[Script] 读取脚本包注册表失败，本次按空表处理：{err:#}");
            Vec::new()
        }
    };
    for package in &mut packages {
        if package.enabled && package.providers.len() == 1 {
            let only = &package.providers[0].id;
            if package.disabled_providers.iter().any(|id| id == only) {
                package.enabled = false;
                package.disabled_providers.clear();
            }
        }
    }
    packages
}

/// 删除一个包：KV + 投影 + 磁盘目录。
pub fn remove(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    package_id: &str,
    version: Option<&str>,
) -> Result<()> {
    // 严格读（同 `register`）：读失败时按空表算出「包不存在」会误导运维，
    // 而按空表写回则会连带抹掉其余包。写锁覆盖「读 → 改 → 写」整段。
    mutate_packages(store, data_dir, |packages| {
        let before = packages.len();
        packages.retain(|p| {
            let same_id = p.id == package_id;
            let same_ver = version.is_none_or(|v| p.version == v);
            !(same_id && same_ver)
        });
        if packages.len() == before {
            anyhow::bail!("包不存在：{package_id}");
        }
        Ok(())
    })
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

/// 读别名表（KV 为准），**读失败是错误**。
///
/// 写路径（投影写回）必须用这一份：宽松的 [`load_identities`] 把读失败当空表，
/// 用它写投影会把 registry.json 的 `identities` 覆写成 `{}`。
fn try_load_identities(store: &crate::storage::FileStore) -> Result<Identities> {
    store
        .get(IDENTITIES_KEY)
        .context("读取 KV plugin:script-identities 失败（拒绝以空别名表覆盖投影）")
        .map(Option::unwrap_or_default)
}

/// 读别名表。
///
/// 与 [`load_packages`] 同口径：只读路径按空表兜底（宿主拿不到别名最多是
/// 路由少几家），但读失败要留痕，别把「KV 坏了」伪装成「还没有别名」。
pub fn load_identities(store: &crate::storage::FileStore) -> Identities {
    match store.get(IDENTITIES_KEY) {
        Ok(identities) => identities.unwrap_or_default(),
        Err(err) => {
            tracing::warn!("[Script] 读取别名表失败，本次按空表处理：{err:#}");
            Identities::default()
        }
    }
}

/// 写别名表（js-host 握手上报后调）。
///
/// **投影写回不许用宽松读**：旧实现写别名后调 `load_packages`（读坏当空表）
/// 再写投影，于是「KV 包表读坏」这一瞬间会把 `registry.json` 覆写成
/// `packages: []` —— KV 里包还在，但宿主文件视图空了，所有脚本包静默停止
/// 加载，直到下一次 `register`/`replace_all` 才被顺手修好。别名表与包表一起
/// 在同一把写锁内落盘，避免与其它写并发时两者互相覆盖。
pub fn save_identities(
    store: &crate::storage::FileStore,
    data_dir: &Path,
    identities: Identities,
) -> Result<()> {
    let _guard = WRITE_LOCK.lock();
    // 先严格读包表（读坏立刻中止，**一个字都不写**）：否则「别名表写成功、
    // 投影写失败」会留下半个事务，调用方却看到 Err。
    let packages = try_load_packages(store)
        .context("读取 KV plugin:script-packs 失败（拒绝以空包表覆盖 registry 投影）")?;
    store
        .put(IDENTITIES_KEY, &identities)
        .context("写入 KV 别名表失败")?;
    if let Err(err) = write_projection(data_dir, &packages, &identities) {
        tracing::warn!("[Script] 写 registry.json 投影失败（KV 已落盘，不影响运行）：{err:#}");
    }
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

    /// v3.5.0 对齐：单提供商包「包开 + 子停」的矛盾态在读取时折合成包停
    ///（保留用户的停用选择，且再开包不被隐藏子开关卡住）。
    #[test]
    fn load_normalizes_single_provider_conflict() {
        let (dir, store) = temp_store();
        let mut conflicted = pkg("single", "1.0.0");
        conflicted.enabled = true;
        conflicted.providers = vec![crate::plugin_host::script::types::ScriptProviderInfo {
            id: "kiro".into(),
            ..Default::default()
        }];
        conflicted.disabled_providers = vec!["kiro".into()];
        register(&store, dir.path(), conflicted).unwrap();

        let packages = load_packages(&store);
        assert_eq!(packages.len(), 1);
        assert!(!packages[0].enabled, "矛盾态折合为包停用");
        assert!(packages[0].disabled_providers.is_empty(), "子开关清空");

        // 多提供商包不受影响：只有那家被停，包照开。
        let (dir2, store2) = temp_store();
        let mut multi = pkg("multi", "1.0.0");
        multi.enabled = true;
        multi.providers = vec![
            crate::plugin_host::script::types::ScriptProviderInfo {
                id: "a".into(),
                ..Default::default()
            },
            crate::plugin_host::script::types::ScriptProviderInfo {
                id: "b".into(),
                ..Default::default()
            },
        ];
        multi.disabled_providers = vec!["a".into()];
        register(&store2, dir2.path(), multi).unwrap();
        let packages = load_packages(&store2);
        assert!(packages[0].enabled, "多提供商包不折合");
        assert_eq!(packages[0].disabled_providers, vec!["a".to_string()]);
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

    /// 制造「KV 里这个键读不出来」：形状对不上强类型（幂等、与后端无关）。
    fn corrupt_packages_kv(store: &crate::storage::FileStore) {
        store
            .put(KV_KEY, &serde_json::json!({"not": "a package list"}))
            .unwrap();
        assert!(
            store.get::<Vec<ScriptPackage>>(KV_KEY).is_err(),
            "前置条件：KV 必须是读不出来的状态"
        );
    }

    /// 制造「别名表读不出来」：形状对不上 `Identities`（BTreeMap 套 BTreeMap）。
    fn corrupt_identities_kv(store: &crate::storage::FileStore) {
        store
            .put(IDENTITIES_KEY, &serde_json::json!("not-an-identity-table"))
            .unwrap();
        assert!(
            store.get::<Identities>(IDENTITIES_KEY).is_err(),
            "前置条件：别名表 KV 必须是读不出来的状态"
        );
    }

    /// 别名表读坏时**不许**用空别名表覆写 registry 投影。
    ///
    /// `mutate_packages` 写投影时调的是宽松的 `load_identities`（读失败当空表），
    /// 于是「KV 包表正常、别名表读坏」这一瞬间会把 `registry.json` 的
    /// `identities` 覆写成 `{}` —— 宿主按空别名表加载，所有「包声明的 id ≠
    /// 运行时 id」的脚本包静默路由到默认 provider。`save_identities` 早就为此
    /// 改成严格读，这条路径当时漏了。
    #[test]
    fn mutate_packages_never_overwrites_projection_with_empty_identities() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("pkg-a", "1.0.0")).unwrap();
        let mut ids = Identities::new();
        ids.entry("pkg-a".into())
            .or_default()
            .insert("kiro".into(), "kiro-magpie".into());
        save_identities(&store, data_dir, ids.clone()).unwrap();
        assert_eq!(read_projection(data_dir).unwrap().identities, ids);

        corrupt_identities_kv(&store);
        register(&store, data_dir, pkg("pkg-b", "1.0.0")).unwrap();

        // KV（权威状态）必须已经落盘：pkg-b 在册
        let kv_ids: Vec<String> = try_load_packages(&store)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert!(
            kv_ids.contains(&"pkg-b".to_string()),
            "KV 权威状态必须包含新包：{kv_ids:?}"
        );
        // 投影这一笔**整体跳过**（宁可停在旧版本也不写空别名表）
        let after = read_projection(data_dir).unwrap();
        assert_eq!(
            after.identities, ids,
            "别名表读坏时投影必须保留原别名（旧实现会在这里被覆写成空表）"
        );
    }

    /// 写别名表时 KV 包表读坏：**不许**用空包表覆写 registry 投影。
    ///
    /// 旧实现写别名后调 `load_packages`（读失败当空表）再写投影，于是这一
    /// 瞬间 `registry.json` 变成 `packages: []` —— KV 里包还在，宿主文件
    /// 视图空了，所有脚本包静默停止加载，直到下次 `register` 顺手修好。
    #[test]
    fn save_identities_never_overwrites_projection_with_empty_table() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        register(&store, data_dir, pkg("pkg-a", "1.0.0")).unwrap();
        let before = read_projection(data_dir).unwrap();
        assert_eq!(before.packages.len(), 1);

        corrupt_packages_kv(&store);
        let mut ids = Identities::new();
        ids.entry("pkg-a".into())
            .or_default()
            .insert("kiro".into(), "kiro-magpie".into());
        let err = save_identities(&store, data_dir, ids).expect_err("读坏时不许装作成功");
        assert!(
            format!("{err:#}").contains("plugin:script-packs"),
            "错误要点名是包表读失败：{err:#}"
        );

        let after = read_projection(data_dir).unwrap();
        assert_eq!(
            after.packages.len(),
            1,
            "投影必须保留原有包（旧实现会在这里变成 0 条）"
        );
        assert_eq!(after.packages[0].id, "pkg-a");
    }

    /// 写路径的事务性：闭包执行期间写锁必须已被持有。
    ///
    /// 这是「读 → 改 → 写回」能否串行化的唯一判据。旧实现各写路径在锁外
    /// 读快照再整表写回，并发写互相覆盖（`set_enabled` 刚打开包，另一个
    /// 请求拿更早的快照写回，启停状态被静默顶回旧值）。
    #[test]
    fn mutate_packages_holds_write_lock_for_whole_transaction() {
        let (dir, store) = temp_store();
        let data_dir = dir.path();
        let observed = mutate_packages(&store, data_dir, |packages| {
            let free = super::WRITE_LOCK.try_lock().is_some();
            packages.push(pkg("pkg-a", "1.0.0"));
            Ok(free)
        })
        .unwrap();
        assert!(
            !observed,
            "闭包执行期间写锁必须被持有（读改写不可被并发插入）"
        );
        assert_eq!(load_packages(&store).len(), 1);
    }

    /// 并发写不丢更新：N 个线程各登记一个不同的包，最终必须全部在册。
    ///
    /// 旧实现（读快照 → 改 → 整表写回，无锁）在这条上会丢——是否必现取决于
    /// 调度，所以判据不依赖它：真正的确定性判据是上一条「闭包内写锁必须被
    /// 持有」，本条是它的行为面覆盖。
    #[test]
    fn concurrent_registers_do_not_lose_updates() {
        let (dir, store) = temp_store();
        let data_dir = dir.path().to_path_buf();
        let store = std::sync::Arc::new(store);
        std::thread::scope(|scope| {
            for i in 0..16 {
                let store = store.clone();
                let data_dir = data_dir.clone();
                scope.spawn(move || {
                    register(&store, &data_dir, pkg(&format!("pkg-{i}"), "1.0.0")).unwrap();
                });
            }
        });
        let ids: Vec<String> = load_packages(&store).into_iter().map(|p| p.id).collect();
        for i in 0..16 {
            assert!(
                ids.contains(&format!("pkg-{i}")),
                "并发登记丢了 pkg-{i}：{ids:?}"
            );
        }
    }
}
