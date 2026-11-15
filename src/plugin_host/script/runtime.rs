//! 脚本宿主运行期门面 —— 单 Bun 进程的启动 / 热切换 / 崩溃重启 + 路由表。
//!
//! ## 一个宿主承载全部脚本包
//!
//! 与原生插件「每包一个二进制进程」不同，脚本插件共用**一个** Bun 进程
//! （`host_rs::spawn_host`）。因此运维启停一个包 = 重建整个宿主的
//! init 载荷并**热切换**到新进程，而不是单独 spawn/kill。
//!
//! ## 热切换（rebuild）
//!
//! 1. 从 KV 读全部包 → 过滤 `enabled` → 构造 init 载荷（packages/reserved/
//!    claimed/catalog_path）；
//! 2. spawn 新宿主并完成 init 握手（60s）；**成功才换指针**——失败时旧
//!    宿主原样继续服务（回滚 = 什么都不做）；
//! 3. 换指针后，旧宿主交给后台排空：等 `pending_count==0 && active_streams==0`
//!    （最多 60s）再优雅关闭。在途 conversation 不会因热切换被腰斩。
//!
//! 同一时刻只允许一个 rebuild（`rebuild_lock`）；`generation` 代际保证
//! 「崩溃重启」与「人工 rebuild」不会双拉。
//!
//! ## 数据面路由（同步）
//!
//! `resolve.rs` 的候选构造是同步函数——`route_for_provider()` 不能 await。
//! 这里用 parking_lot 锁 + `Option<Arc<ScriptHost>>` 快照：宿主指针存在
//! 即代表 Running（崩溃/关闭时由监督循环清空）。owners/aliases 表在 init
//! 握手成功后原子替换。
//!
//! ## reserved 口径
//!
//! 原生 provider（`PROVIDERS` ∪ `SESSION_PROVIDERS`）+ 原生插件包声明的
//! provider 全部进 reserved；脚本对它们没有接管权（与 `is_plugin_owned`
//! 同立场）。JS 侧 `loader.mjs` 也会按 reserved 让位，网关侧再兜一道。

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock as PlRwLock;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::account_session::AccountSessionStore;
use crate::plugin_host::registry::PackRegistry;
use crate::plugin_host::rpc::RpcChannel;
use crate::storage::FileStore;

use super::callbacks::ScriptCallbacks;
use super::catalog::Catalog;
use super::host_rs::{self, build_init_payload, ScriptHost};
use super::protocol::ProtocolRegistry;
use super::storage;
use super::transport::Transport;
use super::types::{script_plugins_root, ScriptPackage, ScriptProviderInfo};

/// 崩溃重启退避表（与原生插件同节奏）。
const RESTART_BACKOFF: [Duration; 7] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// 连续重启失败上限：达到即停止重试并记 last_error（包保持 enabled，
/// 运维可经 rebuild 手动再试——脚本宿主是共享进程，自动停用全部包
/// 会把无关的正常脚本一起停掉）。
const MAX_RESTART_ATTEMPTS: u32 = 5;

/// 旧宿主排空的轮询间隔 / 最长等待。
const DRAIN_POLL: Duration = Duration::from_millis(500);
const DRAIN_MAX: Duration = Duration::from_secs(60);

/// 运行期状态快照（管理面展示）。
#[derive(Clone, Debug, Default, Serialize)]
pub struct ScriptHostStatus {
    /// 宿主进程是否在跑（握手完成的 Running 态）。
    pub running: bool,
    /// 本次 init 装载的包数。
    pub packages: usize,
    /// 实际接管的 provider id（已按 reserved 让位后的集合）。
    pub providers: Vec<String>,
    /// 最近一次错误（启动失败 / 重启失败 / 握手失败）。
    pub last_error: Option<String>,
    /// 本次进程周期内的意外退出次数。
    pub crashes: u32,
}

/// 数据面路由结果：通道句柄 + 实际 provider id。
///
/// `provider` 与渠道里的 `plugin_provider` 可能不同（id 冲突时 JS 侧
/// 加后缀，`identities` 表记录映射）；桥必须用**实际 id** 调 RPC。
#[derive(Clone)]
pub struct ScriptRoute {
    pub channel: Arc<RpcChannel>,
    pub provider: String,
}

/// 脚本宿主管理器。
pub struct ScriptHostManager {
    data_dir: PathBuf,
    script_root: PathBuf,
    store: Arc<FileStore>,
    account_sessions: Arc<AccountSessionStore>,
    http: reqwest::Client,
    catalog: Arc<Catalog>,
    /// 原生插件包注册表（reserved 集合的补充来源）。
    native_registry: Arc<PackRegistry>,
    /// 出站代理（script.transport.*）。
    transport: Arc<Transport>,
    /// 当前宿主（`Some` = Running；崩溃/关闭立即清空）。
    host: PlRwLock<Option<Arc<ScriptHost>>>,
    /// provider（声明 id 与实际 id 都在表里）→ 包 id。
    owners: PlRwLock<HashMap<String, String>>,
    /// 声明 id → 实际接管 id（无冲突时两者相同）。
    aliases: PlRwLock<HashMap<String, String>>,
    /// 状态快照。
    runtime: PlRwLock<ScriptHostStatus>,
    /// rebuild 串行化。
    rebuild_lock: Mutex<()>,
    /// 代际（监督循环的过期判据）。
    generation: AtomicU64,
}

impl ScriptHostManager {
    /// 构造（不启动进程；由 [`Self::start`] 或 API 层 rebuild 拉起）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_dir: PathBuf,
        store: Arc<FileStore>,
        account_sessions: Arc<AccountSessionStore>,
        http: reqwest::Client,
        catalog: Arc<Catalog>,
        native_registry: Arc<PackRegistry>,
    ) -> Arc<Self> {
        let script_root = script_plugins_root(&data_dir);
        let transport = Transport::new(http.clone());
        Arc::new(Self {
            data_dir,
            script_root,
            store,
            account_sessions,
            http,
            catalog,
            native_registry,
            transport,
            host: PlRwLock::new(None),
            owners: PlRwLock::new(HashMap::new()),
            aliases: PlRwLock::new(HashMap::new()),
            runtime: PlRwLock::new(ScriptHostStatus::default()),
            rebuild_lock: Mutex::new(()),
            generation: AtomicU64::new(0),
        })
    }

    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    pub fn store(&self) -> &Arc<FileStore> {
        &self.store
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn script_root(&self) -> &Path {
        &self.script_root
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// 状态快照。
    pub fn status(&self) -> ScriptHostStatus {
        self.runtime.read().clone()
    }

    /// 已装载的 provider（声明 id → 实际 id 全保留，管理面展示用）。
    pub fn routed_providers(&self) -> Vec<String> {
        let mut v: Vec<String> = self.owners.read().keys().cloned().collect();
        v.sort();
        v
    }

    /// 数据面路由（**同步**）。未接管 / 宿主不在跑 → `None`。
    ///
    /// `provider` 接受声明 id 或实际 id；返回值里的 `provider` 是实际 id。
    pub fn route_for_provider(&self, provider: &str) -> Option<ScriptRoute> {
        if provider.is_empty() {
            return None;
        }
        // 原生实现存在时脚本不接管（与 is_plugin_owned 同口径，含热路径）
        if crate::plugin_host::registry::is_native_provider(provider) {
            return None;
        }
        let host = self.host.read().clone()?;
        if !self.owners.read().contains_key(provider) {
            return None;
        }
        let active = self
            .aliases
            .read()
            .get(provider)
            .cloned()
            .unwrap_or_else(|| provider.to_string());
        Some(ScriptRoute {
            channel: host.rpc(),
            provider: active,
        })
    }

    /// 启动时恢复：有已启用包就拉起宿主，否则什么都不做。
    pub async fn start(self: &Arc<Self>) {
        let packages = storage::load_packages(&self.store);
        if !packages.iter().any(|p| p.enabled) {
            tracing::info!("[Script] 无已启用脚本包，宿主不启动");
            return;
        }
        if let Err(e) = self.rebuild("startup").await {
            tracing::warn!("[Script] 启动恢复失败: {e}");
        }
    }

    /// 重建宿主（启停包 / 改配置 / 崩溃重启共用）。
    ///
    /// 成功返回新状态；失败时旧宿主原样继续（回滚 = 不换指针）。
    ///
    /// **为什么返回装箱 future 而不是 `async fn`**：调用链形成环——
    /// `rebuild` → `rebuild_locked` → `tokio::spawn(supervise_host)` →
    /// `supervise_host` → `rebuild`。若三者都是 `async fn`，编译器无法
    /// 归纳出 `Send`（每个 future 的 Send 都需要环上另一个先成立），
    /// 报 "future cannot be sent between threads safely"。把递归入口装箱
    /// 成 `dyn Future + Send` 后，环上的这条边是**声明**而非推导，环被打断。
    pub fn rebuild<'a>(
        self: &'a Arc<Self>,
        reason: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ScriptHostStatus, String>> + Send + 'a>> {
        Box::pin(async move {
            let _guard = self.rebuild_lock.lock().await;
            self.rebuild_locked(reason).await
        })
    }

    async fn rebuild_locked(self: &Arc<Self>, reason: &str) -> Result<ScriptHostStatus, String> {
        let packages: Vec<ScriptPackage> = storage::load_packages(&self.store)
            .into_iter()
            .filter(|p| p.enabled)
            .collect();

        if packages.is_empty() {
            // 没有启用的包：关掉宿主并清表（旧宿主排空后关闭）。
            // 代际 +1 让旧监督循环醒来自行退出。
            self.generation.fetch_add(1, Ordering::Relaxed);
            let old = self.host.write().take();
            self.owners.write().clear();
            self.aliases.write().clear();
            {
                let mut rt = self.runtime.write();
                rt.running = false;
                rt.packages = 0;
                rt.providers.clear();
                rt.last_error = None;
                // crashes 保留（展示历史），下次成功启动清零
            }
            if let Some(old) = old {
                self.spawn_drain(old);
            }
            tracing::info!("[Script] 无已启用包（{reason}），宿主已停");
            return Ok(self.status());
        }

        // 转换器表每个宿主进程一份：`stream` 键是宿主的 RPC id，跨宿主
        // 重启会复用；共享一张表会让「旧宿主排空中的流」与「新宿主同 id 的
        // 流」互相污染。
        let callbacks = ScriptCallbacks::new(
            self.transport.clone(),
            ProtocolRegistry::new(),
            self.account_sessions.clone(),
            self.store.clone(),
        );
        let reserved = self.reserved_providers();
        let payload = build_init_payload(
            &self.script_root,
            &packages,
            reserved,
            Some(self.catalog.catalog_path().to_string_lossy().into_owned()),
        );

        // 先 spawn 新宿主再动任何状态：失败时旧宿主/旧代际原样保留
        //（否则新宿主没起来、旧监督循环却被代际失效，崩溃后无人接管）。
        let new_host = host_rs::spawn_host(&self.data_dir, payload, callbacks)
            .await
            .map_err(|e| format!("启动脚本宿主失败: {e}"))?;
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;

        // 路由表：声明 id → 实际 id（identities 映射；无冲突同名）
        let (owners, aliases, providers) = self.build_routing(&packages, &new_host);

        // 别名表落盘（KV `plugin:script-identities` + 投影）：宿主重启后
        // JS 侧从 registry.json 读到的 identities 与网关路由同源。
        // 失败只 warn——路由表在内存里已就位，落盘只是给宿主读。
        let persisted = identities_from_host(new_host.identities());
        if let Err(e) = storage::save_identities(&self.store, &self.data_dir, persisted) {
            tracing::warn!("[Script] 落盘 identities 失败（路由不受影响）：{e:#}");
        }

        let old = self.host.write().replace(new_host.clone());
        {
            let mut owners_lock = self.owners.write();
            *owners_lock = owners;
        }
        {
            let mut aliases_lock = self.aliases.write();
            *aliases_lock = aliases;
        }
        {
            let mut rt = self.runtime.write();
            rt.running = true;
            rt.packages = packages.len();
            rt.providers = providers;
            rt.last_error = None;
            rt.crashes = 0;
        }
        if let Some(old) = old {
            self.spawn_drain(old);
        }
        // 监督循环：意外退出 → 退避重启（代际过期的旧循环自行退出）
        let manager = Arc::clone(self);
        let host = new_host;
        tokio::spawn(async move {
            manager.supervise_host(host, generation).await;
        });

        tracing::info!(
            "[Script] 宿主已启动（{reason}）：{} 个包 / {} 家 provider",
            packages.len(),
            self.runtime.read().providers.len()
        );
        Ok(self.status())
    }

    /// 网关关闭：优雅关闭宿主。
    pub async fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
        let old = self.host.write().take();
        self.owners.write().clear();
        self.aliases.write().clear();
        {
            let mut rt = self.runtime.write();
            rt.running = false;
            rt.providers.clear();
        }
        if let Some(old) = old {
            host_rs::shutdown(&old).await;
        }
    }

    /// reserved 集合：内置 provider + 原生插件包声明。
    fn reserved_providers(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = crate::account_session::registry::PROVIDERS
            .iter()
            .map(|s| s.kind.to_string())
            .collect();
        set.extend(
            crate::account_session::registry::SESSION_PROVIDERS
                .iter()
                .map(|(kind, _)| kind.to_string()),
        );
        for pack in self.native_registry.list() {
            for provider in pack.providers {
                set.insert(provider.id);
            }
        }
        set.into_iter().collect()
    }

    /// 由启用包集合 + init 应答构造路由表。
    ///
    /// 投影到纯函数 [`compute_routing`]（宿主句柄只提供只读快照），
    /// 便于单测覆盖「加载失败让位 / 声明与实际 id 不同 / 未声明 provider
    /// 的 npm 包」这些路由形态。
    fn build_routing(
        &self,
        packages: &[ScriptPackage],
        host: &ScriptHost,
    ) -> (
        HashMap<String, String>,
        HashMap<String, String>,
        Vec<String>,
    ) {
        compute_routing(packages, host.providers(), host.identities())
    }

    /// 监督一个宿主进程的生命周期。
    async fn supervise_host(self: Arc<Self>, host: Arc<ScriptHost>, generation: u64) {
        host_rs::supervise(host.clone()).await;
        if host.is_shutdown_requested() {
            return;
        }
        // 代际过期（人工 rebuild/关闭已换新宿主）：旧循环不再动作
        if self.generation.load(Ordering::Relaxed) != generation {
            return;
        }
        // 意外退出：清指针 → 退避重启
        {
            let mut slot = self.host.write();
            if slot.as_ref().is_some_and(|h| Arc::ptr_eq(h, &host)) {
                *slot = None;
            }
        }
        self.owners.write().clear();
        self.aliases.write().clear();
        let crashes = {
            let mut rt = self.runtime.write();
            rt.running = false;
            rt.providers.clear();
            rt.crashes += 1;
            rt.crashes
        };
        tracing::warn!("[Script] 脚本宿主意外退出（本周期第 {crashes} 次），退避重启");

        for attempt in 0..MAX_RESTART_ATTEMPTS {
            // attempt 是 u32、表索引是 usize：先转再钳（超表取最后一档）
            let delay = RESTART_BACKOFF[(attempt as usize).min(RESTART_BACKOFF.len() - 1)];
            tokio::time::sleep(delay).await;
            if self.generation.load(Ordering::Relaxed) != generation {
                return;
            }
            // 没有任何启用包了（运维在退避窗口里全停了）：不再重拉
            let enabled = storage::load_packages(&self.store)
                .iter()
                .any(|p| p.enabled);
            if !enabled {
                self.runtime.write().running = false;
                return;
            }
            match self.rebuild("crash-restart").await {
                Ok(_) => {
                    tracing::info!("[Script] 宿主崩溃后重拉成功（第 {} 次尝试）", attempt + 1);
                    return;
                }
                Err(e) => {
                    {
                        let mut rt = self.runtime.write();
                        rt.last_error = Some(e.clone());
                    }
                    tracing::warn!("[Script] 宿主第 {} 次重拉失败: {e}", attempt + 1);
                }
            }
        }
        let msg = format!("脚本宿主连续 {MAX_RESTART_ATTEMPTS} 次重启失败，已停止重试");
        self.runtime.write().last_error = Some(msg.clone());
        tracing::error!("[Script] {msg}");
    }

    /// 旧宿主排空后关闭：等在途请求 / 流归零（最多 60s）。
    fn spawn_drain(&self, old: Arc<ScriptHost>) {
        tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + DRAIN_MAX;
            loop {
                let busy = old.rpc().pending_count().await + old.rpc().active_streams().await;
                if busy == 0 || tokio::time::Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(DRAIN_POLL).await;
            }
            host_rs::shutdown(&old).await;
        });
    }
}

/// 构造 provider → 包 id 路由表（纯函数，便于单测）。
///
/// **权威来源是 `identities`**（JS 按「实际装载成功的记录」上报，含
/// npm/github 安装时包清单没声明 provider 的情况）；包声明的 provider
/// 表只作兜底（老宿主不报 identities 时）。只登记 `providers` 里确实
/// 在跑的 id——加载失败/让位的家不会留下死路由。
///
/// 返回 `(owners, aliases, actuals)`：
/// - owners：声明 id 与实际 id 都指向包 id（两个方向都能路由）；
/// - aliases：声明 id → 实际 id（无映射 = 同名）；
/// - actuals：实际接管的 id 列表（管理面展示）。
fn compute_routing(
    packages: &[ScriptPackage],
    providers: &[ScriptProviderInfo],
    identities: &serde_json::Map<String, serde_json::Value>,
) -> (
    HashMap<String, String>,
    HashMap<String, String>,
    Vec<String>,
) {
    let loaded: BTreeSet<&str> = providers.iter().map(|p| p.id.as_str()).collect();
    let mut owners: HashMap<String, String> = HashMap::new();
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut actuals: Vec<String> = Vec::new();

    // 1) identities：{pkgId: {声明 id → 实际 id}}（JS 侧唯一权威）
    for (pkg_id, mapping) in identities {
        let Some(map) = mapping.as_object() else {
            continue;
        };
        for (requested, actual_value) in map {
            let Some(actual) = actual_value.as_str() else {
                continue;
            };
            if !loaded.contains(actual) {
                continue;
            }
            owners.insert(requested.clone(), pkg_id.clone());
            owners.insert(actual.to_string(), pkg_id.clone());
            if requested != actual {
                aliases.insert(requested.clone(), actual.to_string());
            }
            actuals.push(actual.to_string());
        }
    }

    // 2) 兜底：包清单声明的 provider（identities 缺失时按同名登记）
    for pkg in packages {
        for declared in &pkg.providers {
            if owners.contains_key(&declared.id) || !loaded.contains(declared.id.as_str()) {
                continue;
            }
            owners.insert(declared.id.clone(), pkg.id.clone());
            actuals.push(declared.id.clone());
        }
    }

    actuals.sort();
    actuals.dedup();
    (owners, aliases, actuals)
}

/// 宿主上报的 identities JSON → 落盘用的强类型表。
///
/// JS 形状 `{pkgId: {声明 id → 实际 id}}`；形状不符的条目静默丢弃
/// （映射缺失只影响冲突包的别名翻译，不值得中断启动流程）。
fn identities_from_host(raw: &serde_json::Map<String, serde_json::Value>) -> storage::Identities {
    let mut out = storage::Identities::new();
    for (pkg_id, mapping) in raw {
        let Some(map) = mapping.as_object() else {
            continue;
        };
        for (requested, actual) in map {
            if let Some(actual) = actual.as_str() {
                out.entry(pkg_id.clone())
                    .or_default()
                    .insert(requested.clone(), actual.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn backoff_table_is_monotonic() {
        for w in RESTART_BACKOFF.windows(2) {
            assert!(w[0] <= w[1], "退避表必须单调不减");
        }
        assert_eq!(RESTART_BACKOFF[0], Duration::from_secs(1));
        assert_eq!(*RESTART_BACKOFF.last().unwrap(), Duration::from_secs(60));
    }

    fn provider(id: &str) -> ScriptProviderInfo {
        ScriptProviderInfo {
            id: id.to_string(),
            label: id.to_string(),
            upstream: None,
            models: Vec::new(),
        }
    }

    fn pkg(id: &str, providers: &[&str]) -> ScriptPackage {
        ScriptPackage {
            id: id.to_string(),
            version: "1.0.0".into(),
            providers: providers.iter().map(|p| provider(p)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn routing_identity_rename_maps_both_ids() {
        // 声明 kiro，实际接管 kiro-2（id 冲突加缀）：两个方向都要能路由，
        // 且别名表把声明 id 翻译到实际 id。
        let packages = vec![pkg("@a/kiro", &["kiro"])];
        let providers = vec![provider("kiro-2")];
        let mut identities = serde_json::Map::new();
        identities.insert("@a/kiro".into(), json!({ "kiro": "kiro-2" }));
        let (owners, aliases, actuals) = compute_routing(&packages, &providers, &identities);
        assert_eq!(owners.get("kiro"), Some(&"@a/kiro".to_string()));
        assert_eq!(owners.get("kiro-2"), Some(&"@a/kiro".to_string()));
        assert_eq!(aliases.get("kiro"), Some(&"kiro-2".to_string()));
        assert_eq!(actuals, vec!["kiro-2".to_string()]);
    }

    #[test]
    fn routing_skips_providers_not_reported_by_host() {
        // 包声明了 kiro，但 JS 侧没装载成功 / 让位（providers 列表里没有）
        // → 不能留死路由。
        let packages = vec![pkg("@a/kiro", &["kiro"])];
        let (owners, aliases, actuals) = compute_routing(&packages, &[], &serde_json::Map::new());
        assert!(owners.is_empty());
        assert!(aliases.is_empty());
        assert!(actuals.is_empty());
    }

    #[test]
    fn routing_covers_undeclared_providers_via_identities() {
        // npm/GitHub 安装的包，包清单里没有 providers 声明；
        // 只要 JS identities 报了，路由就必须成立。
        let packages = vec![pkg("some-npm-pkg", &[])];
        let providers = vec![provider("astudio")];
        let mut identities = serde_json::Map::new();
        identities.insert("some-npm-pkg".into(), json!({ "astudio": "astudio" }));
        let (owners, _aliases, actuals) = compute_routing(&packages, &providers, &identities);
        assert_eq!(owners.get("astudio"), Some(&"some-npm-pkg".to_string()));
        assert_eq!(actuals, vec!["astudio".to_string()]);
    }

    #[test]
    fn routing_falls_back_to_package_declaration() {
        // 老宿主不报 identities：包声明与实际 id 同名时仍要能路由。
        let packages = vec![pkg("@a/kiro", &["kiro"])];
        let providers = vec![provider("kiro")];
        let (owners, aliases, actuals) =
            compute_routing(&packages, &providers, &serde_json::Map::new());
        assert_eq!(owners.get("kiro"), Some(&"@a/kiro".to_string()));
        assert!(aliases.is_empty());
        assert_eq!(actuals, vec!["kiro".to_string()]);
    }

    #[test]
    fn identities_from_host_filters_bad_shapes() {
        let mut raw = serde_json::Map::new();
        raw.insert("@a/kiro".into(), json!({ "kiro": "kiro-2", "bad": 7 }));
        raw.insert("@a/junk".into(), json!("not-an-object"));
        let ids = identities_from_host(&raw);
        assert_eq!(ids.len(), 1, "形状不符的包条目应丢弃");
        assert_eq!(
            ids.get("@a/kiro").and_then(|m| m.get("kiro")),
            Some(&"kiro-2".to_string())
        );
        assert!(
            ids.get("@a/kiro").and_then(|m| m.get("bad")).is_none(),
            "非字符串值应丢弃"
        );
    }
}
