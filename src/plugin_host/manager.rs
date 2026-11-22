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

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use parking_lot::RwLock as PlRwLock;
use serde_json::Value;

use crate::account_session::AccountSessionStore;
use crate::storage::FileStore;

use super::callbacks::PluginCallbacks;
use super::install::{self, InstallError};
use super::process::{self, HostEnv, PluginProcess, ProcessState, ProviderInfo};
use super::registry::{PackMeta, PackRegistry};

/// 读包目录的 `pack.json`（原始 JSON —— wasm 判据与校验都按 JSON 形状走）。
fn read_pack_json(dir: &Path) -> Result<serde_json::Value, InstallError> {
    let path = dir.join("pack.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        InstallError::Manifest(format!("读不到 pack.json（{}）: {e}", path.display()))
    })?;
    serde_json::from_str::<serde_json::Value>(&text)
        .map_err(|e| InstallError::Manifest(format!("pack.json 解析失败: {e}")))
}

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

/// 有界 FIFO 缓存（按插入顺序淘汰；插件包数量极少，无需严格 LRU）。
///
/// key 由调用方拼好（见 `plugins.rs` 的 `pack_view_key`：包 id+版本+
/// `pack.json` 的 mtime/size），缓存自身只负责「有界 + 命中计数」。
struct BoundedCache<V> {
    map: HashMap<String, V>,
    order: VecDeque<String>,
    cap: usize,
}

impl<V> BoundedCache<V> {
    fn new(cap: usize) -> Self {
        Self {
            map: HashMap::new(),
            order: VecDeque::new(),
            cap,
        }
    }

    fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    fn insert(&mut self, key: String, value: V) {
        // 覆盖已有 key：不重复占用淘汰序（`get_mut` 判定，避开 clippy::map_entry）。
        if self.map.get_mut(&key).is_some() {
            self.map.insert(key, value);
            return;
        }
        while self.map.len() >= self.cap {
            match self.order.pop_front() {
                Some(old) => {
                    self.map.remove(&old);
                }
                None => break,
            }
        }
        self.order.push_back(key.clone());
        self.map.insert(key, value);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map.len()
    }
}

/// 插件列表视图缓存（图标 data URI + `pack.json` 解析结果）。
///
/// `GET /api/plugins` 在前端轮询时每 2s 拉一次，而每个已装包都要读
/// `pack.json` 并把图标读成 base64——这两步都是重复 IO。缓存键含
/// `pack.json` 的 mtime/size，重装或覆盖写会自动失效。上限
/// [`Self::CAPACITY`] 条，超出按插入序淘汰（包数量远小于上限）。
pub struct PluginViewCache {
    icons: Mutex<BoundedCache<Option<String>>>,
    manifests: Mutex<BoundedCache<Value>>,
    icon_hits: AtomicU64,
    icon_misses: AtomicU64,
    manifest_hits: AtomicU64,
    manifest_misses: AtomicU64,
}

impl PluginViewCache {
    /// 缓存条目上限（每个包最多 2 条：图标 + 清单）。
    pub const CAPACITY: usize = 256;

    pub fn new() -> Self {
        Self {
            icons: Mutex::new(BoundedCache::new(Self::CAPACITY)),
            manifests: Mutex::new(BoundedCache::new(Self::CAPACITY)),
            icon_hits: AtomicU64::new(0),
            icon_misses: AtomicU64::new(0),
            manifest_hits: AtomicU64::new(0),
            manifest_misses: AtomicU64::new(0),
        }
    }

    /// 取图标缓存；`Some(None)` = 命中「无图标」，`None` = 未命中。
    pub fn get_icon(&self, key: &str) -> Option<Option<String>> {
        let hit = self.icons.lock().get(key).cloned();
        match &hit {
            Some(_) => self.icon_hits.fetch_add(1, Ordering::Relaxed),
            None => self.icon_misses.fetch_add(1, Ordering::Relaxed),
        };
        hit
    }

    pub fn put_icon(&self, key: String, value: Option<String>) {
        self.icons.lock().insert(key, value);
    }

    /// 取清单缓存（`Value::Null` 表示读不到/解析失败，同样缓存以免反复读盘）。
    pub fn get_manifest(&self, key: &str) -> Option<Value> {
        let hit = self.manifests.lock().get(key).cloned();
        match &hit {
            Some(_) => self.manifest_hits.fetch_add(1, Ordering::Relaxed),
            None => self.manifest_misses.fetch_add(1, Ordering::Relaxed),
        };
        hit
    }

    pub fn put_manifest(&self, key: String, value: Value) {
        self.manifests.lock().insert(key, value);
    }

    /// `(hits, misses)`（图标缓存）——计数型单测证明「N 次列表只读 1 次盘」。
    #[cfg(test)]
    pub fn icon_stats(&self) -> (u64, u64) {
        (
            self.icon_hits.load(Ordering::Relaxed),
            self.icon_misses.load(Ordering::Relaxed),
        )
    }

    /// `(hits, misses)`（清单缓存）。
    #[cfg(test)]
    pub fn manifest_stats(&self) -> (u64, u64) {
        (
            self.manifest_hits.load(Ordering::Relaxed),
            self.manifest_misses.load(Ordering::Relaxed),
        )
    }

    #[cfg(test)]
    pub fn icon_len(&self) -> usize {
        self.icons.lock().len()
    }

    #[cfg(test)]
    pub fn manifest_len(&self) -> usize {
        self.manifests.lock().len()
    }
}

impl Default for PluginViewCache {
    fn default() -> Self {
        Self::new()
    }
}

/// 发布目录 TTL 缓存 + 单飞。
///
/// 列表接口轮询时不应每次都打上游：成功结果缓存 [`CatalogCache::ok_ttl`]
/// （默认 60s），不可用/错误态缓存更短（[`CatalogCache::err_ttl`]，默认
/// 10s）避免把上游打爆。`get_or_fetch` 在 tokio 互斥锁内完成「查缓存 →
/// 未命中则拉取 → 写回」，并发请求只会有一个真正发起拉取（其余等待后
/// 命中新值），即单飞。`force = true`（`GET /api/plugins/release` 刷新
/// 语义）绕过 TTL，但仍会把新结果写回缓存。
pub struct CatalogCache {
    state: tokio::sync::Mutex<CatalogState>,
    ok_ttl: Duration,
    err_ttl: Duration,
}

struct CatalogState {
    source: String,
    value: Option<Value>,
    at: Option<Instant>,
    ok: bool,
}

impl CatalogCache {
    pub fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(CatalogState {
                source: String::new(),
                value: None,
                at: None,
                ok: false,
            }),
            ok_ttl: Duration::from_secs(60),
            err_ttl: Duration::from_secs(10),
        }
    }

    /// 测试用：自定义 TTL（避免单测真的等 60s）。
    #[cfg(test)]
    pub fn with_ttl(ok_ttl: Duration, err_ttl: Duration) -> Self {
        let mut cache = Self::new();
        cache.ok_ttl = ok_ttl;
        cache.err_ttl = err_ttl;
        cache
    }

    /// 命中判定：源一致、有过成功/失败结果、且未超过对应 TTL。
    fn fresh(state: &CatalogState, source: &str, ok_ttl: Duration, err_ttl: Duration) -> bool {
        if state.value.is_none() || state.source != source {
            return false;
        }
        let ttl = if state.ok { ok_ttl } else { err_ttl };
        state.at.map(|at| at.elapsed() < ttl).unwrap_or(false)
    }

    /// 取目录；未命中/过期/`force` 时调用 `fetch` 拉取并写回。
    ///
    /// `fetch` 在持锁期间执行，故同一时刻只会有一个上游请求（单飞）。
    pub async fn get_or_fetch<F, Fut>(&self, source: &str, force: bool, fetch: F) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Value>,
    {
        let mut state = self.state.lock().await;
        if !force && Self::fresh(&state, source, self.ok_ttl, self.err_ttl) {
            return state.value.clone().unwrap_or(Value::Null);
        }
        let value = fetch().await;
        state.ok = value
            .get("available")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        state.source = source.to_string();
        state.at = Some(Instant::now());
        state.value = Some(value.clone());
        value
    }

    /// 当前是否持有未过期结果（诊断/单测用）。
    #[cfg(test)]
    pub async fn is_fresh(&self, source: &str) -> bool {
        let state = self.state.lock().await;
        Self::fresh(&state, source, self.ok_ttl, self.err_ttl)
    }
}

impl Default for CatalogCache {
    fn default() -> Self {
        Self::new()
    }
}

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
    /// 列表视图缓存（图标 / `pack.json`），见 [`PluginViewCache`]。
    view_cache: PluginViewCache,
    /// 发布目录 TTL + 单飞缓存，见 [`CatalogCache`]。
    catalog_cache: CatalogCache,
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
            view_cache: PluginViewCache::new(),
            catalog_cache: CatalogCache::new(),
        }
    }

    pub fn registry(&self) -> &Arc<PackRegistry> {
        &self.registry
    }

    /// 列表视图缓存（图标 / 清单；`plugins.rs` 热路径用）。
    pub fn view_cache(&self) -> &PluginViewCache {
        &self.view_cache
    }

    /// 发布目录缓存（TTL + 单飞；`plugins.rs` 热路径用）。
    pub fn catalog_cache(&self) -> &CatalogCache {
        &self.catalog_cache
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

    /// 下载插件包字节（带上限与进度回调；安装流程的「下载」阶段）。
    pub async fn download_with_progress<F>(
        &self,
        url: &str,
        on_progress: F,
    ) -> Result<Vec<u8>, InstallError>
    where
        F: FnMut(u64, u64),
    {
        install::download_bytes_with_progress(&self.http, url, on_progress).await
    }

    /// 底层存储句柄（Operation 进度记录等运行期状态持久化用）。
    pub fn store(&self) -> Arc<FileStore> {
        self.store.clone()
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
        // Wasm 包与原生包走**完全不同**的启停路径：没有子进程、没有
        // spawn/握手/崩溃退避，而是进程内加载 wasmtime 模块。判据是
        // `pack.json` 的 `entry` 是否声明 `wasm32-unknown-unknown` 目标
        // （与 `wasm::manifest::read_manifest` 同一判据，二者互斥）。
        if let Some(providers) = self.enable_wasm(&meta).await? {
            return Ok(providers);
        }
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
        // Wasm 包：关宿主 + 摘清单 + 清任务/目录状态，然后落 enabled=false。
        if let Some(provider) = self.provider_of_wasm_pack(pack_id) {
            crate::plugin_host::wasm::stop(&provider).await;
            crate::plugin_host::wasm::forget(&provider);
            self.registry.set_enabled(pack_id, false)?;
            if let Some(slot) = self.runtime.write().get_mut(pack_id) {
                slot.running = false;
            }
            tracing::info!("[PluginHost] 已停用 Wasm 插件包 {pack_id}（provider {provider}）");
            return Ok(());
        }
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

    /// 按包 id 直接取插件进程（不经过 provider 归属判定）。
    ///
    /// Wasm 沙箱的「原生辅助调用」需要向**指定包**发 RPC（`native.call`），
    /// 此时手里只有包 id、没有 provider，且不应受 provider 级闸门影响——
    /// 授权判定由调用方（`wasm::native`）自己做。`processes` 是私有字段，
    /// 除本方法外没有别的读取入口，故单独暴露。
    pub fn process_for_pack(&self, pack_id: &str) -> Option<Arc<PluginProcess>> {
        self.processes.read().get(pack_id).cloned()
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
    ///
    /// Wasm 宿主也要一并关停 —— 它们不占子进程表，`processes` 里没有它们。
    pub async fn shutdown_all(&self) {
        let procs: Vec<Arc<PluginProcess>> =
            self.processes.write().drain().map(|(_, p)| p).collect();
        let total = procs.len();
        // 每个包是否按流程关闭（RPC 确认 vs 超时强杀）；汇总到全局事件。
        let mut routine = true;
        for p in procs {
            routine &= process::shutdown(&p).await;
        }
        // Wasm 宿主：取消在途调用 + 排空 + 摘清单。
        crate::plugin_host::wasm::stop_all().await;
        // 网关关闭：受控任务审批与在途任务全部作废（fail-closed，重启需重新批准）。
        crate::plugin_host::wasm::managed_task::invalidate();
        for slot in self.runtime.write().values_mut() {
            slot.running = false;
        }
        // 全局关闭事件（对齐 beta.7 `plugin.shutdown` 事件：无单包 pluginId，
        // `routine` 表示所有原生包都在宽限期内确认退出）。
        tracing::info!(
            target: "plugin",
            action = "plugin.shutdown",
            routine,
            plugins = total,
            "插件宿主已全部关停"
        );
    }

    /// Wasm 包的启用路径（返回 `None` 表示这不是 wasm 包，交给原生路径）。
    ///
    /// 判据是包目录的 `pack.json` 是否把入口声明在
    /// `wasm32-unknown-unknown` 目标下（`wasm::manifest::is_wasm`）。
    async fn enable_wasm(&self, meta: &PackMeta) -> Result<Option<Vec<ProviderInfo>>, String> {
        let dir = meta.version_dir(&self.plugins_root);
        let pack = match read_pack_json(&dir) {
            Ok(pack) => pack,
            // 没有 pack.json 说明不是（或损坏的）插件包：交给原生路径，
            // 由它报「入口不存在」——不在 here 抢报错口径。
            Err(_) => return Ok(None),
        };
        if !crate::plugin_host::wasm::manifest::is_wasm(&pack) {
            return Ok(None);
        }
        // 已装载：直接返回当前 provider（重复 enable 幂等）。
        let provider = crate::plugin_host::wasm::read_manifest(&dir, &pack)
            .map_err(|error| self.note_wasm_error(meta, error))?
            .ok_or_else(|| {
                self.note_wasm_error(meta, "Wasm 清单缺失".to_string())
                    .to_string()
            })?
            .provider;
        if let Some(host) = crate::plugin_host::wasm::host_for_provider(&provider) {
            let providers: Vec<ProviderInfo> = host
                .providers()
                .into_iter()
                .map(|info| ProviderInfo {
                    id: info.id,
                    label: info.label,
                    ..Default::default()
                })
                .collect();
            let mut rt = self.runtime.write();
            let slot = rt.entry(meta.id.clone()).or_default();
            slot.running = true;
            slot.providers = providers.clone();
            return Ok(Some(providers));
        }
        // 构造注入面：账号访问 + 出站（复用脚本宿主的 Transport 实例）。
        // `Transport` 由每实例自建——wasm 包的出站与脚本宿主不共享会话表
        // 反而更安全（一个 wasm 包的最大连接数不会挤占脚本包）。
        let access = crate::plugin_host::wasm::AccountSessionAccess::new(
            meta.id.clone(),
            self.account_sessions.clone(),
            self.store.clone(),
        );
        // 目录重建要同一个账号访问面（`catalog::rebuild` 显式收注入面，
        // 不再读进程级单例）；`from_pack` 会拿走 `access`，先留一份。
        let rebuild_access: std::sync::Arc<
            dyn crate::plugin_host::wasm::access::WasmAccountAccess,
        > = access.clone();
        let egress = crate::plugin_host::wasm::TransportEgress::new(
            crate::plugin_host::script::transport::Transport::new(self.http.clone()),
        );
        let options = crate::plugin_host::wasm::WasmOptions::from_pack(&dir, &pack, access, egress)
            .map_err(|error| self.note_wasm_error(meta, error))?
            .ok_or_else(|| {
                self.note_wasm_error(meta, "Wasm 清单缺失".to_string())
                    .to_string()
            })?;
        // 模型目录出口：`wasm::catalog` 的 provider 并集经 sink 落到
        // `plugin_host::model_catalog`（= 上游 `rpc_cache` 的写入面）。
        // 存储与本管理器其余 KV 消费者共用同一实例；`install` 幂等。
        crate::plugin_host::model_catalog::install(self.store.clone());
        let options = crate::plugin_host::wasm::WasmOptions {
            catalog: Some(crate::plugin_host::model_catalog::FileCatalogSink::new()),
            ..options
        };
        let provider = options.manifest.provider.clone();
        let host = match crate::plugin_host::wasm::start(options).await {
            Ok(host) => host,
            Err(error) => {
                // 失败不留半成品：start 内部已自行清理，这里只落 last_error。
                self.registry.set_enabled(&meta.id, false).ok();
                return Err(self.note_wasm_error(meta, error));
            }
        };
        let providers: Vec<ProviderInfo> = host
            .providers()
            .into_iter()
            .map(|info| ProviderInfo {
                id: info.id,
                label: info.label,
                ..Default::default()
            })
            .collect();
        self.registry.set_enabled(&meta.id, true)?;
        {
            let mut rt = self.runtime.write();
            let slot = rt.entry(meta.id.clone()).or_default();
            slot.running = true;
            slot.providers = providers.clone();
            slot.last_error = None;
            slot.crashes = 0;
        }
        // 任务声明：失败只 warn，不阻断启用（插件可能没声明 tasks.v1）。
        if let Err(error) = crate::plugin_host::wasm::tasks::load(&provider).await {
            tracing::warn!("[PluginHost] Wasm 包 {} 任务声明加载失败: {error}", meta.id);
        }
        // 目录并集回推：账号状态里已落盘的 `host.catalog.v1`（上一次成功
        // 刷新）在进程重启 / 重新启用后不会自动出现在目录出口 —— 与上游
        // `bootstrap.rs` 的 `if wasm::catalog::enabled(&id) { rebuild }` 同
        // 位置做一次重建，sink 随即发布到 `model_catalog`。
        if crate::plugin_host::wasm::catalog::enabled(&provider) {
            crate::plugin_host::wasm::catalog::rebuild(&rebuild_access, &provider).await;
        }
        tracing::info!(
            "[PluginHost] 已启用 Wasm 插件包 {}（provider {provider}）",
            meta.id
        );
        Ok(Some(providers))
    }

    /// 记录 wasm 启用失败的 last_error 并返回原错误文案。
    fn note_wasm_error(&self, meta: &PackMeta, error: String) -> String {
        {
            let mut rt = self.runtime.write();
            let slot = rt.entry(meta.id.clone()).or_default();
            slot.running = false;
            slot.last_error = Some(error.clone());
        }
        error
    }

    /// 该包若是 wasm 包，返回它声明的 provider id（`disable` / 诊断用）。
    fn provider_of_wasm_pack(&self, pack_id: &str) -> Option<String> {
        let meta = self.registry.get(pack_id)?;
        let dir = meta.version_dir(&self.plugins_root);
        let pack = read_pack_json(&dir).ok()?;
        crate::plugin_host::wasm::read_manifest(&dir, &pack)
            .ok()
            .flatten()
            .map(|manifest| manifest.provider)
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

    /// 计数型证明：TTL 内连续调用只触发一次拉取；`force` 绕过 TTL
    /// （刷新目录语义）；源变化视为未命中。
    #[tokio::test]
    async fn catalog_cache_ttl_hits_and_force_bypass() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let cache = Arc::new(CatalogCache::with_ttl(
            Duration::from_secs(60),
            Duration::from_millis(20),
        ));
        let calls = Arc::new(AtomicU32::new(0));

        let fetch = |tag: &'static str| {
            let calls = calls.clone();
            move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({ "available": true, "tag": tag })
                }
            }
        };

        // TTL 内两次：只拉一次
        let _ = cache.get_or_fetch("src", false, fetch("a")).await;
        let _ = cache.get_or_fetch("src", false, fetch("a")).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1, "TTL 内不应重复拉取");

        // force（刷新目录）绕过 TTL：再拉一次
        let forced = cache.get_or_fetch("src", true, fetch("b")).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "force 必须绕过缓存");
        assert_eq!(forced["tag"], "b");

        // 源变化视为未命中
        let _ = cache.get_or_fetch("other", false, fetch("c")).await;
        assert_eq!(calls.load(Ordering::SeqCst), 3, "源变化应重新拉取");
    }

    /// 错误态 TTL 更短：失败结果过期后重试，成功结果在更长的 TTL 内不重拉。
    #[tokio::test]
    async fn catalog_cache_error_ttl_is_shorter() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let cache = CatalogCache::with_ttl(Duration::from_secs(60), Duration::from_millis(15));
        let calls = Arc::new(AtomicU32::new(0));

        let make_fail = || {
            let calls = calls.clone();
            move || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({ "available": false, "error": true })
                }
            }
        };
        cache.get_or_fetch("s", false, make_fail()).await;
        // 未过错误 TTL：命中
        let ok = cache.get_or_fetch("s", false, make_fail()).await;
        assert_eq!(ok["available"], false);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        tokio::time::sleep(Duration::from_millis(40)).await;
        // 过了错误 TTL：重拉
        cache.get_or_fetch("s", false, make_fail()).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2, "错误态应短 TTL 后重试");

        let ok_calls = Arc::new(AtomicU32::new(0));
        let make_succeed = || {
            let ok_calls = ok_calls.clone();
            move || {
                let ok_calls = ok_calls.clone();
                async move {
                    ok_calls.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({ "available": true })
                }
            }
        };
        cache.get_or_fetch("ok", false, make_succeed()).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        cache.get_or_fetch("ok", false, make_succeed()).await;
        assert_eq!(
            ok_calls.load(Ordering::SeqCst),
            1,
            "成功态在 60s TTL 内不应重拉"
        );
    }

    /// 并发单飞：8 个并发请求只触发一次上游拉取。
    #[tokio::test]
    async fn catalog_cache_single_flight_under_concurrency() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let cache = Arc::new(CatalogCache::new());
        let calls = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let calls = calls.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_fetch("src", false, move || {
                        let calls = calls.clone();
                        async move {
                            calls.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_millis(20)).await;
                            serde_json::json!({ "available": true })
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "并发请求只应发起一次上游拉取"
        );
        assert!(cache.is_fresh("src").await);
    }
}
