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

use std::path::Path;
use std::sync::Arc;

use super::bun;
use super::catalog::provider;
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
                    ..Default::default()
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

    let (repository, summary) = match &catalog_meta {
        Some(meta) => (meta.repository.clone(), meta.summary.clone()),
        None => (None, None),
    };
    let mut package = install::to_package(installed, declared, repository, summary);
    if let Some(meta) = &catalog_meta {
        package.label = meta.label.clone();
        package.publisher = meta.publisher.clone();
    }

    // GitHub/Local 通路的包名要等包体到手才知道：补一次同样的校验，
    // 不过则把**刚解包的这个版本目录**清掉（不留半成品）。
    //
    // 用 package.version_dir 而不是 `packages/{dir_name}`：后者会连带删掉
    // 同 id 的其它已装版本（升级失败把旧版本一起毁掉）。Local 来源由
    // cleanup_on_install_failure 保护，不删本机目录。
    if known_name.is_none() {
        if let Err(e) = check_install_guards(&existing, &package.id) {
            cleanup_on_install_failure(manager.data_dir(), &package);
            return Err(e);
        }
    }
    // npm / github / local 通路安装时 `declared` 为空（install 阶段还不知道
    // 包名，拿不到目录条目）。包名确定后补 provider 声明：
    //  1) **静态扫描入口源码**（不执行代码）：这是 local 直装与目录外
    //     npm/github 包的唯一来源；
    //  2) 再用目录条目校正 publisher / repository（github 条目带扫描结果）。
    // 不回填会让这类包的 `providers` / init `claimed` / 管理面 provider
    // 徽标永久为空。只是展示/兜底用途，路由仍以 JS 侧 identities 为准
    //（见 runtime::compute_routing），故扫描无结果不影响安装。
    if package.providers.is_empty() {
        if let Some(entry) = package.entry.clone() {
            let scan_root = package.version_dir(dir_root);
            package.providers = scan_declared_providers(&scan_root, &entry);
        }
    }
    if let Some(entry) = manager.catalog().package_metadata(&package.id).await {
        if package.providers.is_empty() && !entry.providers.is_empty() {
            package.providers = entry
                .providers
                .iter()
                .map(|id| ScriptProviderInfo {
                    id: id.clone(),
                    label: String::new(),
                    ..Default::default()
                })
                .collect();
        }
        // 能走到这里说明 catalog_meta 为 None（magpie 必有 providers，
        // 不会进本分支），此时 publisher 仍是 to_package 的默认
        // Official——从目录条目校正成真实分类（github→ThirdParty 等）。
        package.publisher = entry.publisher.clone();
        if package.repository.is_none() {
            package.repository = entry.repository.clone();
        }
    }
    package.trusted = true;
    package.enabled = false;
    // 包图标兜底：宿主握手要等「启用 + 加载成功」才回填 `icon`，但商店
    // 「已安装」卡片与后续列表在启用前就该有图标。先读包清单的
    // `magpie.icon` 落库（data URI / https），刷新 worker 随后落缓存。
    // 宿主回填值优先（`apply_package_list` 会覆盖）。
    if package.icon.is_none() {
        let manifest_dir = package.version_dir(dir_root);
        package.icon = super::icons::manifest(&manifest_dir);
    }

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
                    cleanup_on_install_failure(manager.data_dir(), &package);
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
        cleanup_on_install_failure(manager.data_dir(), &package);
        return Err(format!("登记脚本包失败：{e:#}"));
    }
    // 图标缓存立即刷新一轮（不等整点）：新装包的商店/已装卡片马上有图。
    super::icons::wake(false);
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
/// 多版本共存时按 `version` 选中；`version=None` 时：启用取最后登记的
/// （最新），**停用优先选当前已启用那条**——否则多版本下会去停一个本就
/// 停用的版本，真正在跑的旧版本继续留在宿主里。
pub async fn set_enabled(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    version: Option<&str>,
    enabled: bool,
) -> Result<(), String> {
    let mut packages = storage::load_packages(manager.store());
    let index = select_package_index(&packages, package_id, version, enabled)
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    let mut package = packages[index].clone();
    if package.enabled == enabled {
        return Ok(());
    }
    if enabled {
        if !package.trusted {
            return Err(format!(
                "包 {package_id} 未信任，不能启用（安装时需显式 trusted=true）"
            ));
        }
        // 同一 id 的另一个版本已启用 → 拒绝：两个版本同时进 init 载荷会
        // 造成 provider id 冲突加缀、路由漂移。安装闸门只挡「装新版本」，
        // 挡不住「再启用另一个已装版本」，这里补上。
        if let Some(other) = other_enabled_version(&packages, package_id, &package.version) {
            return Err(format!(
                "包 {package_id} 的版本 {other} 已启用：请先停用再启用其它版本\
                 （避免同 id 双版本同时进 init 载荷）"
            ));
        }
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

/// 选中要操作的那条包记录（见 [`set_enabled`] 的选择口径）。
fn select_package_index(
    packages: &[ScriptPackage],
    id: &str,
    version: Option<&str>,
    enable: bool,
) -> Option<usize> {
    let matches = |p: &ScriptPackage| p.id == id && version.is_none_or(|v| p.version == v);
    if enable {
        return packages.iter().rposition(matches);
    }
    // 停用：优先真正启用中的那条，其次退回最后登记的
    packages
        .iter()
        .rposition(|p| matches(p) && p.enabled)
        .or_else(|| packages.iter().rposition(matches))
}

/// 同 id 的**其它版本**是否已启用；返回其版本串（用于错误文案）。
fn other_enabled_version<'a>(
    packages: &'a [ScriptPackage],
    id: &str,
    version: &str,
) -> Option<&'a str> {
    packages
        .iter()
        .find(|p| p.id == id && p.version != version && p.enabled)
        .map(|p| p.version.as_str())
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
    // 逐个停用**确实启用中**的版本（version=None 时 matched 可能含多个
    // 版本；只对有 enabled 的按精确版本停用，保证重建发生在删目录之前）。
    for p in matched.iter().filter(|p| p.enabled) {
        set_enabled(manager, package_id, Some(&p.version), false).await?;
    }
    storage::remove(manager.store(), manager.data_dir(), package_id, version)
        .map_err(|e| format!("删除元数据失败：{e:#}"))?;
    for package in &matched {
        // Local 来源是原地登记：绝不能删运维的本机目录
        if package.source != ScriptSource::Local {
            storage::remove_package_dir(manager.data_dir(), package);
        }
    }
    // 停用路径已经 rebuild 过（且只有 rebuild 成功才会走到这里），数据面
    // 已不含该包；这次 rebuild 只是清「曾启用但已删」的投影残留，幂等。
    // 失败可安全吞掉：宿主状态仍等价于「包已停用」，不存在指向已删目录的
    // 活路由；下一次启停/重启会重建。故不把这条失败上报成卸载失败。
    if let Err(e) = manager.rebuild("uninstall").await {
        tracing::warn!("[Script] 卸载后宿主重建失败（包已删除且已停用，重启后自愈）：{e}");
    }
    // 清掉已删包的图标绑定（refresh 会 retain_cards）。
    super::icons::wake(false);
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

/// 更新包级 + provider 级设置（A2A `/api/plugins/scripts/settings`）。
///
/// - `options`（包级参数，≤256KiB）与 `provider_options`（`{provider:
///   {option: bool}}` 开关）都是**可选补丁**：缺省字段保持原值；
/// - 校验：options 字节上限；provider_options 的键必须是该包已声明的
///   provider，值必须是布尔（或对象形态时逐项布尔）；
/// - 落盘后若包在启用中，重建宿主让脚本读到新值（重建失败不算保存失败
///   ——配置已落盘，错误文案如实说明）。
pub async fn update_settings(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    version: Option<&str>,
    options: Option<serde_json::Map<String, serde_json::Value>>,
    provider_options: Option<serde_json::Map<String, serde_json::Value>>,
) -> Result<ScriptPackage, String> {
    let mut packages = storage::load_packages(manager.store());
    let index = packages
        .iter()
        .rposition(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    if let Some(values) = options {
        settings::validate_values(&values)?;
        packages[index].options.values = values;
    }
    if let Some(map) = provider_options {
        let declared: Vec<String> = packages[index]
            .providers
            .iter()
            .map(|p| p.id.clone())
            .collect();
        for (provider, values) in &map {
            if !declared.iter().any(|id| id == provider) {
                return Err(format!("开关不属于当前插件：{provider}"));
            }
            let values = values
                .as_object()
                .ok_or_else(|| "提供商开关必须是对象".to_string())?;
            if values.values().any(|v| !v.is_boolean()) {
                return Err("提供商开关必须是布尔值".into());
            }
        }
        packages[index].provider_options = map;
    }
    let package = packages[index].clone();
    persist_packages(manager, &packages)?;
    if package.enabled {
        if let Err(e) = manager.rebuild("settings").await {
            return Err(format!("配置已保存，但宿主重建失败（下次重启生效）：{e}"));
        }
    }
    Ok(package)
}

/// 单独启停包内某一家 provider（A2A `change action=provider`）。
///
/// 不改包启用状态，只把 provider 加/减到 `disabled_providers`；启用中的
/// 包会重建宿主（脚本侧 `script.packages` 不再报这家 → 路由表撤掉它）。
pub async fn set_provider_enabled(
    manager: &Arc<ScriptHostManager>,
    package_id: &str,
    provider: &str,
    enabled: bool,
) -> Result<ScriptPackage, String> {
    let mut packages = storage::load_packages(manager.store());
    let index = packages
        .iter()
        .position(|p| p.id == package_id)
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    if !packages[index].providers.iter().any(|p| p.id == provider) {
        return Err(format!("提供商不属于该脚本插件：{provider}"));
    }
    if enabled {
        packages[index]
            .disabled_providers
            .retain(|id| id != provider);
    } else if !packages[index]
        .disabled_providers
        .iter()
        .any(|id| id == provider)
    {
        packages[index]
            .disabled_providers
            .push(provider.to_string());
    }
    let package = packages[index].clone();
    persist_packages(manager, &packages)?;
    if package.enabled {
        if let Err(e) = manager.rebuild("provider-toggle").await {
            return Err(format!("已保存，但宿主重建失败（下次重启生效）：{e}"));
        }
    }
    Ok(package)
}

/// 单个已装脚本插件的更新检查结果（维护任务 / 管理面共用）。
#[derive(Clone, Debug)]
pub struct CheckOutcome {
    pub id: String,
    pub name: String,
    pub current: String,
    /// 上游最新版本（GitHub 源是提交短号）；拿不到时为空串。
    pub latest: String,
    pub available: bool,
    pub error: Option<String>,
    /// 来源展示名（Magpie / GitHub / npm / 本地）。
    pub source_label: String,
}

/// 检查更新（A2A `GET /api/plugins/scripts/check`）：把逐包 [`CheckOutcome`]
/// 里「可更新」的项投影成前端契约，并附目录状态。
///
/// 目录未刷新时返回空表 + `catalog` 状态（前端负责提示）。
pub async fn check_updates(manager: &ScriptHostManager) -> serde_json::Value {
    let outcomes = check_update_outcomes(manager).await.unwrap_or_default();
    let updates: Vec<serde_json::Value> = outcomes
        .iter()
        .filter(|outcome| outcome.available)
        .map(|outcome| {
            serde_json::json!({
                "id": outcome.id,
                "name": outcome.name,
                "current": outcome.current,
                "latest": outcome.latest,
            })
        })
        .collect();
    serde_json::json!({
        "updates": updates,
        "catalog": manager.catalog().status().await,
    })
}

/// 逐包更新检查（维护任务「社区插件目录检查」用）：每个已装包一条结果，
/// 不论有没有新版本都返回，便于维护记录逐行展示当前 / 最新版本与变化。
pub async fn check_update_outcomes(
    manager: &ScriptHostManager,
) -> Result<Vec<CheckOutcome>, String> {
    let installed = storage::load_packages(manager.store());
    let mut outcomes = Vec::with_capacity(installed.len());
    for package in &installed {
        let (latest, available, error) = match package.source {
            ScriptSource::Magpie => match manager.catalog().package_metadata(&package.id).await {
                Some(entry) => match entry.version.clone().filter(|v| !v.is_empty()) {
                    Some(latest) => {
                        let available = latest != package.version;
                        (latest, available, None)
                    }
                    None => (
                        String::new(),
                        false,
                        Some("目录未提供该插件版本".to_string()),
                    ),
                },
                None => (String::new(), false, Some("目录中未找到该插件".to_string())),
            },
            ScriptSource::Github => github_outcome(manager, package).await,
            // npm / 本地来源没有可问的远端版本：如实报「无最新信息」，不误报有更新。
            _ => (package.version.clone(), false, None),
        };
        outcomes.push(CheckOutcome {
            id: package.id.clone(),
            name: package.label.clone(),
            current: package.version.clone(),
            latest,
            available,
            error,
            source_label: source_label(package),
        });
    }
    Ok(outcomes)
}

/// GitHub 来源：目录里的 `commit_sha` 对比已装钉住的提交。
///
/// 只有 `source_ref` 的引用段是 40 位 SHA 时才可靠判定；未钉 SHA / 目录无该
/// 仓库时返回「无最新信息」（不误报）。
async fn github_outcome(
    manager: &ScriptHostManager,
    package: &ScriptPackage,
) -> (String, bool, Option<String>) {
    let (repository, reference) = package
        .source_ref
        .split_once('@')
        .unwrap_or((package.source_ref.as_str(), ""));
    let Some(latest_sha) = manager.catalog().github_commit(repository).await else {
        return (String::new(), false, None);
    };
    let latest = latest_sha[..12].to_string();
    let available = commit_update(reference, &latest_sha).is_some();
    (latest, available, None)
}

/// 已装包来源的展示名。
fn source_label(package: &ScriptPackage) -> String {
    match package.source {
        ScriptSource::Magpie => "Magpie 分发源".to_string(),
        ScriptSource::Npm => "npm".to_string(),
        ScriptSource::Github => "GitHub".to_string(),
        ScriptSource::Local => "本地安装".to_string(),
    }
}

/// 纯比较：已装引用是 40 位 SHA 且与目录最新 commit 不同 → 返回
/// `(current12, latest12)` 短号；否则 None（未钉 SHA / 已是最新）。
fn commit_update(installed_reference: &str, latest_sha: &str) -> Option<(String, String)> {
    let current = installed_reference.trim();
    if current.len() != 40 || !current.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if current.eq_ignore_ascii_case(latest_sha) {
        return None;
    }
    Some((current[..12].to_string(), latest_sha[..12].to_string()))
}

fn persist_packages(manager: &ScriptHostManager, packages: &[ScriptPackage]) -> Result<(), String> {
    // 走 storage 的 KV + 投影语义：禁止直接 store.put，否则 registry.json
    // 投影不会刷新，脚本侧读到的 options/enabled 会滞后。
    storage::replace_all(manager.store(), manager.data_dir(), packages)
        .map_err(|e| format!("保存脚本包状态失败：{e:#}"))
}

/// 安装失败清理：删刚落盘的包版本目录。
///
/// **Local 来源是原地登记**：`version_dir` 对 Local 返回 `source_ref`，
/// 照删会毁掉运维自己的源码树。登记失败/依赖失败一律保留本机目录。
fn cleanup_on_install_failure(data_dir: &Path, package: &ScriptPackage) {
    if package.source == ScriptSource::Local {
        tracing::info!(
            "[Script] Local 来源安装失败，保留本机目录：{}",
            package.source_ref
        );
        return;
    }
    storage::remove_package_dir(data_dir, package);
}

/// 静态扫描包入口源码，回填 provider 声明（不执行代码）。
///
/// 目录条目之外安装的包（npm 直装 / 任意 GitHub 仓 / 本机目录）在目录里
/// 没有 providers；入口扫描是它们唯一的声明来源。
fn scan_declared_providers(package_dir: &Path, entry: &str) -> Vec<ScriptProviderInfo> {
    let Some(oc) = provider::scan_entry(package_dir, Path::new(entry)) else {
        return Vec::new();
    };
    match oc.provider_id {
        Some(id) => vec![ScriptProviderInfo {
            id,
            label: oc.label.unwrap_or_default(),
            ..Default::default()
        }],
        None => Vec::new(),
    }
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

    #[test]
    fn install_failure_cleanup_never_deletes_local_source_dir() {
        // Local 是原地登记：登记/依赖失败绝不能删运维的本机目录
        //（version_dir 对 Local 返回 source_ref，照删会毁源码树）。
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let src = dir.path().join("my-plugin");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("package.json"),
            br#"{"name":"x","version":"1.0.0"}"#,
        )
        .unwrap();
        let package = ScriptPackage {
            id: "x".into(),
            version: "1.0.0".into(),
            source: ScriptSource::Local,
            source_ref: src.to_string_lossy().into_owned(),
            ..Default::default()
        };
        cleanup_on_install_failure(data_dir, &package);
        assert!(src.is_dir(), "Local 来源目录必须保留");
    }

    #[test]
    fn install_failure_cleanup_removes_only_that_version() {
        // 非 Local：只删本版本目录，同 id 的其它已装版本必须保留。
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path();
        let root = super::storage::root(data_dir);
        let package = ScriptPackage {
            id: "pkg-a".into(),
            version: "2.0.0".into(),
            source: ScriptSource::Github,
            ..Default::default()
        };
        let v2 = package.version_dir(&root);
        let v1 = root
            .join("packages")
            .join(package.package_dir_name())
            .join("1.0.0");
        std::fs::create_dir_all(&v2).unwrap();
        std::fs::create_dir_all(&v1).unwrap();
        cleanup_on_install_failure(data_dir, &package);
        assert!(!v2.exists(), "失败版本目录应删除");
        assert!(v1.is_dir(), "其它版本目录必须保留");
    }

    #[test]
    fn scan_declared_providers_reads_entry() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("index.mjs"),
            "const P = \"scanned-provider\";\nexport default { auth: { provider: P } };\n",
        )
        .unwrap();
        let got = scan_declared_providers(dir.path(), "index.mjs");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "scanned-provider");
    }

    #[test]
    fn scan_declared_providers_empty_when_no_auth() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.mjs"), "export const x = 1;\n").unwrap();
        assert!(scan_declared_providers(dir.path(), "index.mjs").is_empty());
        // 入口不存在也不 panic
        assert!(scan_declared_providers(dir.path(), "missing.mjs").is_empty());
    }

    fn pkgv(id: &str, version: &str, enabled: bool) -> ScriptPackage {
        ScriptPackage {
            id: id.into(),
            version: version.into(),
            enabled,
            ..Default::default()
        }
    }

    #[test]
    fn select_index_for_disable_prefers_enabled_version() {
        // v1 启用、v2（最后登记）停用：version=None 停用必须选中 v1，
        // 否则数据面还在跑已卸载的版本。
        let packages = vec![pkgv("p", "1.0.0", true), pkgv("p", "2.0.0", false)];
        let idx = select_package_index(&packages, "p", None, false).unwrap();
        assert_eq!(packages[idx].version, "1.0.0");
        // 都没有启用 → 退回最后登记
        let packages = vec![pkgv("p", "1.0.0", false), pkgv("p", "2.0.0", false)];
        let idx = select_package_index(&packages, "p", None, false).unwrap();
        assert_eq!(packages[idx].version, "2.0.0");
        // 显式版本优先
        let packages = vec![pkgv("p", "1.0.0", true), pkgv("p", "2.0.0", false)];
        let idx = select_package_index(&packages, "p", Some("2.0.0"), false).unwrap();
        assert_eq!(packages[idx].version, "2.0.0");
        // 未安装
        assert!(select_package_index(&packages, "nope", None, true).is_none());
    }

    #[test]
    fn other_enabled_version_detects_dual_enable() {
        let packages = vec![pkgv("p", "1.0.0", true), pkgv("p", "2.0.0", false)];
        assert_eq!(
            other_enabled_version(&packages, "p", "2.0.0"),
            Some("1.0.0")
        );
        assert_eq!(other_enabled_version(&packages, "p", "1.0.0"), None);
        let none = vec![pkgv("p", "1.0.0", false)];
        assert_eq!(other_enabled_version(&none, "p", "2.0.0"), None);
    }

    #[test]
    fn commit_update_compares_only_pinned_sha() {
        let installed = "0123456789abcdef0123456789abcdef01234567";
        let latest = "fedcba9876543210fedcba9876543210fedcba98";
        // 不同提交 → 可更新（各 12 位短号）
        let (cur, lat) = commit_update(installed, latest).unwrap();
        assert_eq!(cur, &installed[..12]);
        assert_eq!(lat, &latest[..12]);
        // 同一提交（大小写不敏感）→ 无更新
        assert!(commit_update(latest, &latest.to_ascii_uppercase()).is_none());
        // 分支 / 标签 / 短号 → 无法可靠比对，宁可漏报
        assert!(commit_update("main", latest).is_none());
        assert!(commit_update("v1.0.0", latest).is_none());
        assert!(commit_update(&installed[..12], latest).is_none());
        assert!(commit_update("", latest).is_none());
    }
}
