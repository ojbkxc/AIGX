//! 脚本包运维编排 —— 安装 / 启停 / 卸载 / 配置。**不碰进程**。
//!
//! ## 与 runtime 的分工
//!
//! - `operations`：只改**持久状态**（KV + 磁盘）——下载、解包、依赖安装、
//!   登记、enabled/trusted/options 落盘。任何一步失败都不留半成品：
//!   解包失败不登记；登记失败删目录。
//! - `runtime`：读持久状态重建宿主（`rebuild`）。操作成功后由 API 层调
//!   `manager.rebuild()`；rebuild 失败时本模块负责把 enabled 回滚到操作前
//!   的值（见 [`set_enabled`]），保证「落盘状态 = 数据面实际行为」。
//!
//! ## 信任闸门（硬约束）
//!
//! 脚本**不是 OS 沙箱**——装进来的代码以 Bun 进程权限运行。因此安装必须
//! 显式 `trusted=true`（API 层的二次确认就是传这个参数），未信任的包
//! [`set_enabled`] 一律拒绝启用。这是「运维自己选择下载」与「运维为执行
//! 负责」的分界线。
//!
//! ## 来源映射
//!
//! | selector | 通道 | 完整性 |
//! |---|---|---|
//! | `magpie:{name}[@{ver}]` | 目录条目 → dist tgz | 必须（缺 integrity 拒绝） |
//! | 裸包名 / `{name}@{ver}` | npm registry | `dist.integrity` |
//! | `github:{owner/repo}[@{ref}]` / GitHub URL | codeload tarball | 无（commit 固定） |
//! | `file:{绝对路径}` | 本机目录原地登记（不复制） | 无（本机信任） |

use std::sync::Arc;

use super::bun;
use super::dependencies;
use super::install;
use super::runtime::ScriptHostManager;
use super::settings;
use super::source::{self, InstallSource};
use super::storage;
use super::types::{ScriptPackage, ScriptProviderInfo, ScriptSource, MAX_SCRIPT_PACKAGES};

/// 安装一个脚本包（落盘但**不启用**；启用走 [`set_enabled`]）。
///
/// `selector` 见模块头的来源映射表。`trusted` 必须为 true——这不是
/// 布尔开关的花架子，而是运维对「执行第三方代码」的显式确认。
pub async fn install(
    manager: &Arc<ScriptHostManager>,
    selector: &str,
    trusted: bool,
) -> Result<ScriptPackage, String> {
    if !trusted {
        return Err(
            "脚本包安装必须显式确认信任（trusted=true）：脚本不是沙箱，将以 Bun 进程权限运行"
                .into(),
        );
    }
    let source = source::parse(selector).map_err(|e| e.to_string())?;

    // magpie 来源：目录条目是权威（dist tgz + integrity + providers）
    let (source, declared, catalog_meta) = match &source {
        InstallSource::Magpie { name, .. } => {
            let entry = manager
                .catalog()
                .package_metadata(name)
                .await
                .ok_or_else(|| format!("目录中没有包 {name}（目录可能尚未刷新）"))?;
            let contract = entry.to_catalog_entry();
            let resolved = install::entry_to_source(&contract)
                .map_err(|e| format!("目录条目不可安装：{e:#}"))?;
            let declared: Vec<ScriptProviderInfo> = contract
                .providers
                .iter()
                .map(|id| ScriptProviderInfo {
                    id: id.clone(),
                    label: String::new(),
                    upstream: None,
                    models: Vec::new(),
                })
                .collect();
            (resolved, declared, Some(contract))
        }
        other => (other.clone(), Vec::new(), None),
    };

    // 数量上限 + 同 id 启用互斥。
    //
    // 上限：无上限的目录清单会线性放大握手载荷/路由表（types 契约）。
    // 互斥：同一 id 若已有启用版本，再装新版本会让两个版本同时进 init
    // 载荷 → provider id 冲突加缀 → 路由漂移。语义上「升级」应先停用
    // 再装（或在管理面做显式升级流程），这里直接拒绝。
    let existing = storage::load_packages(manager.store());
    let known_name: Option<&str> = match &source {
        InstallSource::Magpie { name, .. } | InstallSource::Npm { name, .. } => Some(name),
        _ => None,
    };
    if let Some(name) = known_name {
        check_install_guards(&existing, name)?;
    }

    // 下载 + 校验 + 解包
    let dir_root = manager.script_root();
    let installed = install::fetch_and_unpack(manager.http(), &source, dir_root, declared.clone())
        .await
        .map_err(|e| format!("下载/解包失败：{e:#}"))?;

    // GitHub/Local 通路的包名要等包体到手才知道：补一次同样的校验，
    // 不过则把刚解包的目录清掉（不留半成品）。
    if known_name.is_none() {
        if let Err(e) = check_install_guards(&existing, &installed.name) {
            let dir = dir_root.join("packages").join(&installed.dir_name);
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    }

    let (repository, summary) = match &catalog_meta {
        Some(meta) => (meta.repository.clone(), meta.summary.clone()),
        None => (None, None),
    };
    let mut package = install::to_package(installed, declared, repository, summary);
    if let Some(meta) = &catalog_meta {
        package.label = meta.label.clone();
        package.publisher = meta.publisher.clone();
    }
    package.trusted = true;
    package.enabled = false;

    // 依赖安装：package.json 有 dependencies 才起 Bun（bun::ensure 幂等）。
    // Bun 暂不可用不阻断安装——宿主启动时会再次 ensure；依赖缺失由宿主
    // 加载失败暴露在 runtime.last_error 里（比安装在半途硬失败更可诊断）。
    // Local 来源跳过：本机目录是运维自己的开发目录，不替它跑 bun install
    //（往别人的源码树里写 node_modules 不是安装器的职责）。
    let package_dir = package.version_dir(dir_root);
    if package.source == ScriptSource::Local {
        tracing::info!("[Script] Local 来源跳过依赖安装：{}", package.id);
    } else {
        match bun::ensure(manager.data_dir()).await {
            Ok(bun_exe) => {
                if let Err(e) = dependencies::install_if_needed(&bun_exe, &package_dir).await {
                    // 依赖失败 = 包不可用：删目录并拒绝登记（不留半成品）
                    storage::remove_package_dir(manager.data_dir(), &package);
                    return Err(format!("安装依赖失败：{e:#}"));
                }
            }
            Err(e) => {
                tracing::warn!(
                    "[Script] Bun 不可用（{e:#}），跳过 {} 的依赖安装（宿主启动时重试）",
                    package.id
                );
            }
        }
    }

    if let Err(e) = storage::register(manager.store(), manager.data_dir(), package.clone()) {
        storage::remove_package_dir(manager.data_dir(), &package);
        return Err(format!("登记脚本包失败：{e:#}"));
    }
    tracing::info!(
        "[Script] 已安装 {} {}（来源 {}，未启用）",
        package.id,
        package.version,
        package.source.as_str()
    );
    Ok(package)
}

/// 启用 / 停用一个已安装的包（落盘 + 重建宿主；失败回滚）。
///
/// 多版本共存时按 `version` 选中；`version=None` 取同 id 的最后一个
/// （安装顺序最后 = 最新登记）。
pub async fn set_enabled(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    version: Option<&str>,
    enabled: bool,
) -> Result<(), String> {
    let mut packages = storage::load_packages(manager.store());
    let index = packages
        .iter()
        .rposition(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    let mut package = packages[index].clone();
    if enabled && !package.trusted {
        return Err(format!(
            "包 {package_id} 未信任，不能启用（安装时需显式 trusted=true）"
        ));
    }
    if package.enabled == enabled {
        return Ok(());
    }
    let previous = package.enabled;
    package.enabled = enabled;
    packages[index] = package.clone();
    persist_packages(manager, &packages)?;

    match manager.rebuild("enable/disable").await {
        Ok(_) => Ok(()),
        Err(e) => {
            // 回滚 enabled：落盘状态必须与数据面实际行为一致
            let mut rollback = storage::load_packages(manager.store());
            if let Some(slot) = rollback
                .iter_mut()
                .rposition(|p| p.id == package_id && p.version == package.version)
                .map(|i| &mut rollback[i])
            {
                slot.enabled = previous;
            }
            let _ = persist_packages(manager, &rollback);
            Err(format!("宿主重建失败，已回滚启停状态：{e}"))
        }
    }
}

/// 卸载一个包：先停用，再删元数据与磁盘目录。
pub async fn uninstall(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    version: Option<&str>,
) -> Result<(), String> {
    let packages = storage::load_packages(manager.store());
    let matched: Vec<ScriptPackage> = packages
        .iter()
        .filter(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
        .cloned()
        .collect();
    if matched.is_empty() {
        return Err(format!("包未安装：{package_id}"));
    }
    if matched.iter().any(|p| p.enabled) {
        set_enabled(manager, package_id, version, false).await?;
    }
    storage::remove(manager.store(), manager.data_dir(), package_id, version)
        .map_err(|e| format!("删除元数据失败：{e:#}"))?;
    for package in &matched {
        // Local 来源是原地登记：绝不能删运维的本机目录
        if package.source != ScriptSource::Local {
            storage::remove_package_dir(manager.data_dir(), package);
        }
    }
    // 停用路径已经 rebuild 过；这里再 rebuild 一次以清掉「删掉但曾启用」的
    // 残留路由（set_enabled(false) 已完成，重复 rebuild 幂等）。
    if let Err(e) = manager.rebuild("uninstall").await {
        tracing::warn!("[Script] 卸载后宿主重建失败（包已删除，重启后自愈）：{e}");
    }
    tracing::info!("[Script] 已卸载 {package_id}");
    Ok(())
}

/// 更新包配置（options）：校验字节上限后落盘；启用中的包会重建宿主
/// 让脚本读到新配置。
pub async fn update_options(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    version: Option<&str>,
    values: serde_json::Map<String, serde_json::Value>,
) -> Result<ScriptPackage, String> {
    settings::validate_values(&values)?;
    let mut packages = storage::load_packages(manager.store());
    let index = packages
        .iter()
        .rposition(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    packages[index].options.values = values;
    let package = packages[index].clone();
    persist_packages(manager, &packages)?;
    if package.enabled {
        if let Err(e) = manager.rebuild("options").await {
            return Err(format!("配置已保存，但宿主重建失败（下次重启生效）：{e}"));
        }
    }
    Ok(package)
}

/// 全部已装包（管理面列表）。
pub fn list(manager: &ScriptHostManager) -> Vec<ScriptPackage> {
    storage::load_packages(manager.store())
}

fn persist_packages(manager: &ScriptHostManager, packages: &[ScriptPackage]) -> Result<(), String> {
    // 走 storage 的 KV + 投影语义：禁止直接 store.put，否则 registry.json
    // 投影不会刷新，脚本侧读到的 options/enabled 会滞后。
    storage::replace_all(manager.store(), manager.data_dir(), packages)
        .map_err(|e| format!("保存脚本包状态失败：{e:#}"))
}

/// 安装闸门：数量上限 + 同 id 已有启用版本拒绝。
fn check_install_guards(existing: &[ScriptPackage], name: &str) -> Result<(), String> {
    let distinct: std::collections::HashSet<&str> =
        existing.iter().map(|p| p.id.as_str()).collect();
    if !distinct.contains(name) && distinct.len() >= MAX_SCRIPT_PACKAGES {
        return Err(format!(
            "脚本包数量已达上限 {MAX_SCRIPT_PACKAGES}，请先卸载不用的包"
        ));
    }
    if existing.iter().any(|p| p.id == name && p.enabled) {
        return Err(format!(
            "包 {name} 已有启用版本：请先停用再安装新版本（避免同 id 双版本同时进 init 载荷）"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(id: &str, enabled: bool) -> ScriptPackage {
        ScriptPackage {
            id: id.to_string(),
            version: "1.0.0".into(),
            enabled,
            ..Default::default()
        }
    }

    #[test]
    fn settings_size_guard_is_wired() {
        // install/enable 需要真实 manager；这里只钉住 settings 校验入口
        let mut values = serde_json::Map::new();
        values.insert("k".into(), serde_json::json!("v"));
        assert!(settings::validate_values(&values).is_ok());
    }

    #[test]
    fn install_guard_rejects_enabled_same_id() {
        let existing = vec![pkg("@magpie-community/kiro", true)];
        let err = check_install_guards(&existing, "@magpie-community/kiro").unwrap_err();
        assert!(err.contains("已有启用版本"), "{err}");
        // 停用后允许升级（先停再装）
        let existing = vec![pkg("@magpie-community/kiro", false)];
        assert!(check_install_guards(&existing, "@magpie-community/kiro").is_ok());
    }

    #[test]
    fn install_guard_enforces_package_cap_for_new_ids() {
        let existing: Vec<ScriptPackage> = (0..MAX_SCRIPT_PACKAGES)
            .map(|i| pkg(&format!("p{i}"), false))
            .collect();
        // 新 id 超上限 → 拒绝
        assert!(check_install_guards(&existing, "brand-new").is_err());
        // 重装已有 id（不在上限计数外新增）→ 放行
        assert!(check_install_guards(&existing, "p0").is_ok());
    }
}
