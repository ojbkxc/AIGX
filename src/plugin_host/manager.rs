//! 插件宿主管理器 —— 安装、启停与进程生命周期。
//!
//! ## 启停语义
//!
//! 「启用」= 落盘 `enabled=true` **且** spawn 子进程并完成握手；「停用」=
//! 落盘 `enabled=false` **且** 关掉子进程。运维的「选择下载或不下载」在
//! 这里落地：不装 = 注册表里没有；装了不启用 = 有元数据、无进程。
//!
//! ## 失败处置
//!
//! 任何一步失败都不留半成品：spawn 失败 → 不落 enabled；握手失败 →
//! 关掉刚起的进程并报错。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::RwLock as PlRwLock;

use super::install::{self, InstallError};
use super::process::{self, HostEnv, PluginProcess, ProcessState, ProviderInfo};
use super::registry::{PackMeta, PackRegistry};

/// 一个包在运行期的状态（管理面展示 + 数据面路由用）。
#[derive(Clone, Debug, Default)]
pub struct PackRuntime {
    /// 子进程是否在跑。
    pub running: bool,
    /// 握手申报的 provider。
    pub providers: Vec<ProviderInfo>,
    /// 最近一次错误。
    pub last_error: Option<String>,
}

/// 插件宿主管理器。
pub struct PluginHostManager {
    registry: Arc<PackRegistry>,
    /// 插件根目录（`{data_dir}/plugins` 或配置指定）。
    plugins_root: PathBuf,
    /// 运行中的子进程。
    processes: PlRwLock<HashMap<String, Arc<PluginProcess>>>,
    /// 运行期状态（最近错误/申报 provider）。用 `Arc` 包住以便观察任务持有。
    runtime: Arc<PlRwLock<HashMap<String, PackRuntime>>>,
    /// 出站 HTTP（下载插件包用）。
    http: reqwest::Client,
    /// 传给插件的宿主环境。
    host_env: HostEnv,
}

impl PluginHostManager {
    pub fn new(
        registry: Arc<PackRegistry>,
        plugins_root: PathBuf,
        http: reqwest::Client,
        host_env: HostEnv,
    ) -> Self {
        Self {
            registry,
            plugins_root,
            processes: PlRwLock::new(HashMap::new()),
            runtime: Arc::new(PlRwLock::new(HashMap::new())),
            http,
            host_env,
        }
    }

    pub fn registry(&self) -> &Arc<PackRegistry> {
        &self.registry
    }

    pub fn plugins_root(&self) -> &PathBuf {
        &self.plugins_root
    }

    /// 从 zip 字节安装。
    pub fn install_bytes(
        &self,
        bytes: &[u8],
        expected_sha256: Option<&str>,
    ) -> Result<PackMeta, InstallError> {
        let meta = install::install_from_zip(bytes, &self.plugins_root, expected_sha256)?;
        self.registry
            .upsert(meta.clone())
            .map_err(InstallError::Io)?;
        Ok(meta)
    }

    /// 从 URL 下载并安装。
    pub async fn install_from_url(
        &self,
        url: &str,
        expected_sha256: Option<&str>,
    ) -> Result<PackMeta, InstallError> {
        let bytes = install::download_bytes(&self.http, url).await?;
        self.install_bytes(&bytes, expected_sha256)
    }

    /// 启用一个包：spawn + 握手。
    pub async fn enable(&self, pack_id: &str) -> Result<Vec<ProviderInfo>, String> {
        let meta = self
            .registry
            .get(pack_id)
            .ok_or_else(|| format!("插件包「{pack_id}」不存在"))?;
        // 已在跑：直接返回（先取快照再 await，避免持锁跨 await）
        let running = {
            let procs = self.processes.read();
            procs.get(pack_id).cloned()
        };
        if let Some(p) = running {
            if matches!(*p.state.read().await, ProcessState::Running) {
                let providers = self
                    .runtime
                    .read()
                    .get(pack_id)
                    .map(|r| r.providers.clone())
                    .unwrap_or_default();
                return Ok(providers);
            }
        }
        let entry = meta.entry_path(&self.plugins_root);
        if !entry.is_file() {
            let msg = format!("入口不存在: {}", entry.display());
            self.set_error(pack_id, Some(msg.clone()));
            return Err(msg);
        }
        // spawn
        let proc = process::spawn(&meta.id, &entry, &self.host_env)
            .map_err(|e| format!("启动插件失败: {e}"))?;
        // 握手
        let providers = match process::handshake(&proc.rpc, &self.host_env).await {
            Ok(p) => p,
            Err(e) => {
                // 握手失败：关掉刚起的进程，不留半成品
                process::shutdown(&proc).await;
                let msg = format!("插件握手失败: {}", e.message);
                self.set_error(pack_id, Some(msg.clone()));
                return Err(msg);
            }
        };
        *proc.state.write().await = ProcessState::Running;
        // 落 enabled + 记录运行期状态
        self.registry.set_enabled(pack_id, true)?;
        {
            let mut rt = self.runtime.write();
            let slot = rt.entry(pack_id.to_string()).or_default();
            slot.running = true;
            slot.providers = providers.clone();
            slot.last_error = None;
        }
        // 观察退出
        let watched = proc.clone();
        let manager_id = pack_id.to_string();
        let rt_handle = self.runtime.clone();
        tokio::spawn(async move {
            process::supervise(watched).await;
            if let Some(slot) = rt_handle.write().get_mut(&manager_id) {
                slot.running = false;
            }
        });
        self.processes.write().insert(pack_id.to_string(), proc);
        tracing::info!(
            "[PluginHost] 已启用插件包 {pack_id}（{} 家 provider）",
            providers.len()
        );
        Ok(providers)
    }

    /// 停用一个包：关进程 + 落 enabled=false。
    pub async fn disable(&self, pack_id: &str) -> Result<(), String> {
        let proc = self.processes.write().remove(pack_id);
        if let Some(p) = proc {
            process::shutdown(&p).await;
        }
        self.registry.set_enabled(pack_id, false)?;
        if let Some(slot) = self.runtime.write().get_mut(pack_id) {
            slot.running = false;
        }
        tracing::info!("[PluginHost] 已停用插件包 {pack_id}");
        Ok(())
    }

    /// 卸载一个包：先停，再删元数据与磁盘目录。
    pub async fn uninstall(&self, pack_id: &str) -> Result<(), String> {
        self.disable(pack_id).await?;
        self.registry.remove(pack_id)?;
        self.runtime.write().remove(pack_id);
        let dir = self.plugins_root.join(pack_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("删除插件目录失败: {e}"))?;
        }
        tracing::info!("[PluginHost] 已卸载插件包 {pack_id}");
        Ok(())
    }

    /// 运行期状态快照（管理面展示）。
    pub fn runtime_of(&self, pack_id: &str) -> PackRuntime {
        self.runtime
            .read()
            .get(pack_id)
            .cloned()
            .unwrap_or_default()
    }

    /// 启动时恢复：把「已启用」的包重新拉起来（best effort）。
    ///
    /// 单个包起不来不影响网关启动，只记错误。
    pub async fn restore_enabled(&self) {
        let enabled: Vec<String> = self
            .registry
            .list()
            .into_iter()
            .filter(|p| p.enabled)
            .map(|p| p.id)
            .collect();
        for id in enabled {
            if let Err(e) = self.enable(&id).await {
                tracing::warn!("[PluginHost] 启动恢复插件包 {id} 失败: {e}");
                let _ = self.registry.set_enabled(&id, false);
            }
        }
    }

    /// 网关关闭：全部优雅关闭。
    pub async fn shutdown_all(&self) {
        let procs: Vec<Arc<PluginProcess>> =
            self.processes.write().drain().map(|(_, p)| p).collect();
        for p in procs {
            process::shutdown(&p).await;
        }
        for slot in self.runtime.write().values_mut() {
            slot.running = false;
        }
    }

    fn set_error(&self, pack_id: &str, err: Option<String>) {
        let mut rt = self.runtime.write();
        let slot = rt.entry(pack_id.to_string()).or_default();
        slot.running = false;
        slot.last_error = err;
    }
}
