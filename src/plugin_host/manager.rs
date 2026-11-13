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
//!
//! ## 崩溃退避（M4）
//!
//! 子进程**非主动退出**（崩溃/被杀）时按 1s→2s→4s→8s→16s→30s→60s 退避
//! 重拉（spawn + 握手）；连续 5 次失败 → 自动停用该包（落 `enabled=false`
//! 并把 `last_error` 写入运行期状态）。主动 disable/shutdown_all 走
//! `shutdown_requested` 标记，监督循环立刻退出、绝不重拉。运维在退避
//! 窗口里点启用/停用时，监督**代际**递增，旧代循环醒来即退出，不会
//! 新旧双拉。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock as PlRwLock;

use crate::account_session::AccountSessionStore;
use crate::storage::FileStore;

use super::callbacks::PluginCallbacks;
use super::install::{self, InstallError};
use super::process::{self, HostEnv, PluginProcess, ProcessState, ProviderInfo};
use super::registry::{PackMeta, PackRegistry};

/// 崩溃退避表：第 n 次重试（0 基）前的等待时长。
const RESTART_BACKOFF: [Duration; 7] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// 连续重启失败上限：达到即自动停用该包（落 enabled=false + 记 last_error）。
const MAX_RESTART_ATTEMPTS: u32 = 5;

/// 观察子进程退出的轮询间隔（`try_wait` 短持锁，不长期占用 child）。
const SUPERVISE_POLL: Duration = Duration::from_millis(500);

/// 一个包在运行期的状态（管理面展示 + 数据面路由用）。
#[derive(Clone, Debug, Default)]
pub struct PackRuntime {
    /// 子进程是否在跑。
    pub running: bool,
    /// 握手申报的 provider。
    pub providers: Vec<ProviderInfo>,
    /// 最近一次错误。
    pub last_error: Option<String>,
    /// 本次启用周期内的意外退出次数（崩溃退避展示用；重新 enable 清零）。
    pub crashes: u32,
}

/// 插件宿主管理器。
pub struct PluginHostManager {
    registry: Arc<PackRegistry>,
    /// 插件根目录（`{data_dir}/plugins` 或配置指定）。
    plugins_root: PathBuf,
    /// 运行中的子进程。用 `Arc` 包住：崩溃退避监督任务要替换句柄。
    processes: Arc<PlRwLock<HashMap<String, Arc<PluginProcess>>>>,
    /// 运行期状态（最近错误/申报 provider/崩溃计数）。用 `Arc` 包住以便观察任务持有。
    runtime: Arc<PlRwLock<HashMap<String, PackRuntime>>>,
    /// 监督任务代际（pack id → 代际号）。每次 enable/disable 递增；
    /// 退避窗口内的旧代监督循环在重拉前校验代际，过期即退出——
    /// 防止「运维在退避窗口里点了启用」时新旧两个监督循环双拉。
    supervisors: Arc<PlRwLock<HashMap<String, u64>>>,
    /// 出站 HTTP（下载插件包用）。
    http: reqwest::Client,
    /// 传给插件的宿主环境。
    host_env: HostEnv,
    /// 插件回调用的账号池与会话存储（`PluginCallbacks` 用）。
    account_sessions: Arc<AccountSessionStore>,
    /// 插件回调用的底层存储（`PluginCallbacks` 用）。
    store: Arc<FileStore>,
}

impl PluginHostManager {
    pub fn new(
        registry: Arc<PackRegistry>,
        plugins_root: PathBuf,
        http: reqwest::Client,
        host_env: HostEnv,
        account_sessions: Arc<AccountSessionStore>,
        store: Arc<FileStore>,
    ) -> Self {
        Self {
            registry,
            plugins_root,
            processes: Arc::new(PlRwLock::new(HashMap::new())),
            runtime: Arc::new(PlRwLock::new(HashMap::new())),
            supervisors: Arc::new(PlRwLock::new(HashMap::new())),
            http,
            host_env,
            account_sessions,
            store,
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
        // 已在跑：直接返回。判定用 runtime.running（权威标记，崩溃即置
        // false）+ 进程句柄状态——只看 proc.state 会在「崩溃后退避窗口」
        // 里误判在跑（state 停在 Running，没人改它）。
        let running = {
            let procs = self.processes.read();
            procs.get(pack_id).cloned()
        };
        if let Some(p) = running {
            let rt_running = self
                .runtime
                .read()
                .get(pack_id)
                .map(|r| r.running)
                .unwrap_or(false);
            if rt_running && matches!(*p.state.read().await, ProcessState::Running) {
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
        // spawn + 握手（M4 起走统一的重拉路径，崩溃退避复用同一逻辑）
        let (proc, providers) = restart_once(
            pack_id,
            &entry,
            &self.host_env,
            &self.account_sessions,
            &self.store,
        )
        .await
        .inspect_err(|e| {
            // 启用路径失败也要记 last_error（管理面能看到原因）
            self.set_error(pack_id, Some(e.clone()));
        })?;
        // 落 enabled + 记录运行期状态（crashes 重置：这是一次全新启用周期）
        self.registry.set_enabled(pack_id, true)?;
        {
            let mut rt = self.runtime.write();
            let slot = rt.entry(pack_id.to_string()).or_default();
            slot.running = true;
            slot.providers = providers.clone();
            slot.last_error = None;
            slot.crashes = 0;
        }
        self.processes
            .write()
            .insert(pack_id.to_string(), proc.clone());
        // 监督代际 +1：退避窗口里的旧代监督循环见到过期代际即退出
        let generation = {
            let mut gens = self.supervisors.write();
            let g = gens.entry(pack_id.to_string()).or_insert(0);
            *g += 1;
            *g
        };
        // 崩溃退避监督（M4）：非主动退出按退避表重拉，连续 5 次失败自动停用
        tokio::spawn(supervise_pack(
            pack_id.to_string(),
            self.plugins_root.clone(),
            self.host_env.clone(),
            self.account_sessions.clone(),
            self.store.clone(),
            self.registry.clone(),
            self.processes.clone(),
            self.runtime.clone(),
            self.supervisors.clone(),
            generation,
            proc,
        ));
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
        // 代际 +1：退避窗口里的监督循环醒来后见到过期代际即退出
        //（shutdown 与 set_enabled 之间有窗口，仅靠 enabled 复查不够）
        {
            let mut gens = self.supervisors.write();
            let g = gens.entry(pack_id.to_string()).or_insert(0);
            *g += 1;
        }
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
        self.supervisors.write().remove(pack_id);
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

    /// 数据面路由用：取接管某 provider 的插件进程（若在跑）。**同步**——
    /// 调用点是 `resolve.rs` 的同步函数 `resolve_bridges_needs`，不能 await。
    ///
    /// 链路：`is_plugin_owned`（插件声明 && 非原生 && 已启用）→ 找包 →
    /// 查运行期标记 + 进程句柄。`runtime.running` 由 enable/disable/
    /// 监督循环维护（崩溃即置 false、重拉成功再置 true），是同步可读的
    /// 权威标记——退避窗口里数据面拿到 `None` 回落原生渠道，绝不把
    /// 请求发给一个不在跑的插件。
    pub fn process_for_provider(&self, provider: &str) -> Option<Arc<PluginProcess>> {
        if !self.registry.is_plugin_owned(provider) {
            return None;
        }
        let pack_id = self.registry.find_pack_for_provider(provider)?;
        if !self
            .runtime
            .read()
            .get(&pack_id)
            .map(|r| r.running)
            .unwrap_or(false)
        {
            return None;
        }
        self.processes.read().get(&pack_id).cloned()
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

/// 第 `attempt` 次（0 基）重试前的退避时长；超表索引钳到最后一档。
fn backoff_delay(attempt: u32) -> Duration {
    RESTART_BACKOFF[attempt.min(RESTART_BACKOFF.len() as u32 - 1) as usize]
}

/// 重拉一个包：spawn + init 握手。失败时关掉刚起的进程，不留半成品。
///
/// enable 首次拉起与崩溃退避重试共用本函数——两条路径的失败语义一致
/// （spawn 失败 / 握手失败都算一次失败）。
async fn restart_once(
    pack_id: &str,
    entry: &Path,
    host_env: &HostEnv,
    account_sessions: &Arc<AccountSessionStore>,
    store: &Arc<FileStore>,
) -> Result<(Arc<PluginProcess>, Vec<ProviderInfo>), String> {
    let callbacks = Arc::new(PluginCallbacks::new(
        pack_id.to_string(),
        account_sessions.clone(),
        store.clone(),
    ));
    let proc = process::spawn(pack_id, entry, host_env, callbacks)
        .map_err(|e| format!("启动插件失败: {e}"))?;
    let providers = match process::handshake(&proc.rpc, host_env).await {
        Ok(p) => p,
        Err(e) => {
            // 握手失败：关掉刚起的进程，不留半成品
            process::shutdown(&proc).await;
            return Err(format!("插件握手失败: {}", e.message));
        }
    };
    *proc.state.write().await = ProcessState::Running;
    Ok((proc, providers))
}

/// 等当前代进程退出（轮询 `try_wait`，短持 child 锁不跨 await）。
///
/// 返回时进程必已退出；`try_wait` 出错也按已退出处理（句柄已坏，
/// 重拉路径兜底）。退出日志在这里打（崩溃 vs 主动关闭的分流在调用方）。
async fn wait_exit(proc: &PluginProcess) {
    loop {
        let exited = {
            let mut child = proc.child.lock().await;
            match child.try_wait() {
                Ok(Some(status)) => {
                    if proc.is_shutdown_requested() {
                        tracing::debug!(
                            "plugin {} exited after shutdown request: {status}",
                            proc.pack_id
                        );
                    } else {
                        tracing::warn!("plugin {} exited unexpectedly: {status}", proc.pack_id);
                    }
                    true
                }
                Ok(None) => false, // 仍在运行
                Err(e) => {
                    tracing::warn!("plugin {} try_wait error: {e}", proc.pack_id);
                    true
                }
            }
        };
        if exited {
            return;
        }
        tokio::time::sleep(SUPERVISE_POLL).await;
    }
}

/// 崩溃退避监督循环（M4）。
///
/// 非主动退出 → 记一次崩溃 → 退避后重拉（spawn + 握手）；连续
/// [`MAX_RESTART_ATTEMPTS`] 次失败 → 自动停用该包（`enabled=false` +
/// `last_error` + 摘掉进程句柄）。主动关闭（disable/shutdown_all 经
/// `shutdown_requested` 标记）或包被停用/卸载时立刻退出、绝不重拉。
///
/// 锁纪律：parking_lot 锁只在同步段取用，绝不跨 `.await` 持有；
/// `child`/`state` 是 tokio 锁，正常跨 await。
#[allow(clippy::too_many_arguments)]
async fn supervise_pack(
    pack_id: String,
    plugins_root: PathBuf,
    host_env: HostEnv,
    account_sessions: Arc<AccountSessionStore>,
    store: Arc<FileStore>,
    registry: Arc<PackRegistry>,
    processes: Arc<PlRwLock<HashMap<String, Arc<PluginProcess>>>>,
    runtime: Arc<PlRwLock<HashMap<String, PackRuntime>>>,
    supervisors: Arc<PlRwLock<HashMap<String, u64>>>,
    generation: u64,
    initial: Arc<PluginProcess>,
) {
    let mut failed: u32 = 0; // 连续重启失败计数（成功重拉即清零）
    let mut proc = initial;
    loop {
        // 1) 等当前代进程退出
        wait_exit(&proc).await;
        // 2) 主动关闭：disable/shutdown_all 已标记 → 立刻退出，不重试
        if proc.is_shutdown_requested() {
            if let Some(slot) = runtime.write().get_mut(&pack_id) {
                slot.running = false;
            }
            return;
        }
        // 3) 非主动退出：记一次崩溃（进程状态置 Crashed，运行期计数 +1）
        let crashes = {
            let mut rt = runtime.write();
            let slot = rt.entry(pack_id.clone()).or_default();
            slot.running = false;
            slot.crashes += 1;
            slot.crashes
        };
        *proc.state.write().await = ProcessState::Crashed { attempts: 1 };
        tracing::warn!(
            "[PluginHost] 插件包 {pack_id} 意外退出（本周期第 {crashes} 次崩溃），准备退避重拉"
        );

        // 4) 退避重拉：连续 MAX 次失败即自动停用该包
        loop {
            failed += 1;
            tokio::time::sleep(backoff_delay(failed - 1)).await;
            // 代际校验：运维在退避窗口里点了启用/停用 → 新旧代不匹配，退出
            if *supervisors.read().get(&pack_id).unwrap_or(&generation) != generation {
                return;
            }
            // 退避窗口内包可能已被停用/卸载：复查注册表，不再重拉
            let Some(meta) = registry.get(&pack_id).filter(|m| m.enabled) else {
                if let Some(slot) = runtime.write().get_mut(&pack_id) {
                    slot.running = false;
                }
                return;
            };
            let entry = meta.entry_path(&plugins_root);
            let restarted = if entry.is_file() {
                restart_once(&pack_id, &entry, &host_env, &account_sessions, &store).await
            } else {
                Err(format!("入口不存在: {}", entry.display()))
            };
            match restarted {
                Ok((new_proc, providers)) => {
                    failed = 0;
                    {
                        let mut rt = runtime.write();
                        let slot = rt.entry(pack_id.clone()).or_default();
                        slot.running = true;
                        slot.providers = providers;
                        slot.last_error = None;
                    }
                    // 替换进程句柄（旧的已死，drop 即回收）
                    processes.write().insert(pack_id.clone(), new_proc.clone());
                    proc = new_proc;
                    tracing::info!("[PluginHost] 插件包 {pack_id} 崩溃后重拉成功");
                    break; // 回到外层，等待新进程
                }
                Err(e) => {
                    {
                        let mut rt = runtime.write();
                        if let Some(slot) = rt.get_mut(&pack_id) {
                            slot.last_error = Some(e.clone());
                        }
                    }
                    tracing::warn!("[PluginHost] 插件包 {pack_id} 第 {failed} 次重拉失败: {e}");
                    if failed >= MAX_RESTART_ATTEMPTS {
                        let msg =
                            format!("插件包连续 {MAX_RESTART_ATTEMPTS} 次重启失败，已自动停用");
                        let _ = registry.set_enabled(&pack_id, false);
                        if let Some(slot) = runtime.write().get_mut(&pack_id) {
                            slot.running = false;
                            slot.last_error = Some(msg.clone());
                        }
                        processes.write().remove(&pack_id);
                        supervisors.write().remove(&pack_id);
                        tracing::error!("[PluginHost] {msg}（pack={pack_id}）");
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_schedule_grows_and_caps() {
        // 退避表：1s→2s→4s→8s→16s→30s→60s
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
        assert_eq!(backoff_delay(4), Duration::from_secs(16));
        assert_eq!(backoff_delay(5), Duration::from_secs(30));
        assert_eq!(backoff_delay(6), Duration::from_secs(60));
        // 超表索引钳到最后一档，不 panic
        assert_eq!(backoff_delay(99), Duration::from_secs(60));
    }

    #[test]
    fn pack_runtime_defaults_have_no_crashes() {
        let rt = PackRuntime::default();
        assert!(!rt.running);
        assert!(rt.providers.is_empty());
        assert!(rt.last_error.is_none());
        assert_eq!(rt.crashes, 0);
    }
}
