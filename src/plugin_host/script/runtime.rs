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
//! 3. 换指针时「升代际 + 换指针 + 换 owners/aliases」在**同一把 host 写锁**
//!    内提交；数据面读者持 host 读锁期间才读路由表，因此永远看到同一代际
//!    的完整视图（不会「新宿主配旧路由表」）；
//! 4. 旧宿主交给后台排空：等 `pending_count==0 && active_streams==0`
//!    （最多 60s）再优雅关闭。在途 conversation 不会因热切换被腰斩。
//!
//! 同一时刻只允许一个 rebuild（`rebuild_lock`）；`generation` 代际保证
//!「崩溃重启」与「人工 rebuild」不会双拉：崩溃退避循环在**锁内**复核代际，
//! 人工路径完成提交后它直接放弃，绝不拆掉刚建立的宿主。崩溃监督循环的
//! 清理同样在 host 写锁内以「指针 ptr_eq」为权威判据——旧循环不可能把
//! 新宿主的路由表/状态清空。
//!
//! ## 数据面路由（同步）
//!
//! `resolve.rs` 的候选构造是同步函数——`route_for_provider()` 不能 await。
//! 这里用 parking_lot 锁 + `Option<Arc<ScriptHost>>` 快照：宿主指针存在
//! 即代表 Running（崩溃/关闭时由监督循环清空）。原生实现永远优先——
//! `route_for_provider` 对「请求 id」与「别名翻译后的实际 id」都查原生
//! 让位判据，路由表构造期（`compute_routing`）亦按 reserved 过滤。
//!
//! ## reserved 口径
//!
//! 原生 provider（`PROVIDERS` ∪ `SESSION_PROVIDERS`）+ 原生插件包声明的
//! provider 全部进 reserved；脚本对它们没有接管权（与 `is_plugin_owned`
//! 同立场）。JS 侧 `loader.mjs` 也会按 reserved 让位，网关侧再兜一道
//!（见 `compute_routing` 的 reserved 过滤与 `route_for_provider` 的原生检查）。

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock as PlRwLock;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Mutex;

use crate::account_session::AccountSessionStore;
use crate::plugin_host::registry::PackRegistry;
use crate::plugin_host::rpc::RpcChannel;
use crate::storage::FileStore;

use super::callbacks::ScriptCallbacks;
use super::catalog::Catalog;
use super::host_rs::{self, build_init_payload, ScriptHost};
use super::protocol::ProtocolRegistry;
use super::rpc::admin as host_admin;
use super::settings;
use super::storage;
use super::transport::Transport;
use super::types::{script_plugins_root, ScriptPackage, ScriptProviderInfo};

/// 崩溃重启退避表（与原生插件同节奏）。
///
/// 档位索引 = 「连续崩溃次数 - 1」+ 「本次崩溃事件内的重拉失败次数」：
/// 崩溃越频、重拉失败越多，退避越长（钳到最后一档 60s）。
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

/// 「稳定运行」门槛：宿主活了这么久才崩，视为新一次独立崩溃（连续计数
/// 归 1）；短于它才崩说明在崩溃循环里，计数继续累加、退避升档。
///
/// 没有这条，稳定运行数周的宿主偶发崩溃会因历史累计次数被罚 60s 退避；
/// 反之「起来就崩」的宿主会以 1s 节奏无限重拉（每次成功重拉都会清计数）。
const STABLE_RUN: Duration = Duration::from_secs(60);

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
    /// 传给脚本宿主的 `HostEnv`（init 载荷的 `env` 字段）。
    ///
    /// 构造时给一份由 `data_dir` 推导的缺省值；`main.rs` 构造完
    /// [`crate::plugin_host::PluginHostManager`] 后应调 [`Self::set_host_env`]
    /// 注入与原生插件同源的真实值（debug_traffic / loopback_port 等）。
    /// 用 `PlRwLock` 而非构造参数：脚本宿主与原生宿主在 main.rs 里是两个
    /// 独立对象，注入顺序与构造顺序解耦。
    host_env: PlRwLock<crate::plugin_host::process::HostEnv>,
    /// 出站代理（script.transport.*）。
    transport: Arc<Transport>,
    /// 当前宿主（`Some` = Running；崩溃/关闭立即清空）。
    host: PlRwLock<Option<Arc<ScriptHost>>>,
    /// 当前宿主的回调句柄（关闭/热切换时清会话表用；与 `host` 同生命周期）。
    callbacks: PlRwLock<Option<Arc<ScriptCallbacks>>>,
    /// provider（声明 id 与实际 id 都在表里）→ 包 id。
    owners: PlRwLock<HashMap<String, String>>,
    /// 声明 id → 实际接管 id（无冲突时两者相同）。
    aliases: PlRwLock<HashMap<String, String>>,
    /// 状态快照。
    runtime: PlRwLock<ScriptHostStatus>,
    /// rebuild 串行化。
    rebuild_lock: Mutex<()>,
    /// 网关关停标记：置位后一切 rebuild（含崩溃退避重拉）拒绝再拉宿主，
    /// 防止关停序列与热切换竞态留下无人监管的 Bun 进程。
    shutting_down: AtomicBool,
    /// 代际（监督循环的过期判据）。
    generation: AtomicU64,
}

/// 崩溃重拉的装箱 future（见 [`ScriptHostManager::restart_after_crash`]）。
pub type CrashRestartFuture<'a> =
    Pin<Box<dyn Future<Output = Option<Result<ScriptHostStatus, String>>> + Send + 'a>>;

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
        let host_env = crate::plugin_host::process::HostEnv {
            data_dir: data_dir.to_string_lossy().into_owned(),
            host_version: env!("CARGO_PKG_VERSION").to_string(),
            target: crate::plugin_host::host_target_triple(),
            proxy: String::new(),
            debug_traffic: false,
            loopback_port: 0,
        };
        Arc::new(Self {
            data_dir,
            script_root,
            store,
            account_sessions,
            http,
            catalog,
            native_registry,
            host_env: PlRwLock::new(host_env),
            transport,
            host: PlRwLock::new(None),
            callbacks: PlRwLock::new(None),
            owners: PlRwLock::new(HashMap::new()),
            aliases: PlRwLock::new(HashMap::new()),
            runtime: PlRwLock::new(ScriptHostStatus::default()),
            rebuild_lock: Mutex::new(()),
            shutting_down: AtomicBool::new(false),
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

    /// 注入宿主环境（init 载荷的 `env`）。main.rs 在构造后调用一次。
    pub fn set_host_env(&self, env: crate::plugin_host::process::HostEnv) {
        *self.host_env.write() = env;
    }

    /// 当前宿主环境快照。
    pub fn host_env(&self) -> crate::plugin_host::process::HostEnv {
        self.host_env.read().clone()
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

    /// 运维面调用一个 provider 的能力方法（`admin.action` / `login.*` 等）。
    ///
    /// 与数据面 `route_for_provider` 同一路由判据：未接管 / 宿主不在跑 →
    /// `Err`（调用方按 503 处理）。`method` 是 provider 契约方法名（如
    /// `admin.action`），`params` 由调用方构造。
    pub async fn call_provider(
        &self,
        provider: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let route = self
            .route_for_provider(provider)
            .ok_or_else(|| format!("provider {provider} 未由脚本宿主接管"))?;
        match tokio::time::timeout(timeout, route.channel.call(method, params)).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(e.message),
            Err(_) => Err(format!("provider {provider} 的 {method} 超时")),
        }
    }

    /// 当前宿主句柄快照（`None` = 不在跑）。
    fn current_host(&self) -> Option<Arc<ScriptHost>> {
        self.host.read().as_ref().cloned()
    }

    /// 更新包级 / provider 级设置（A2A `POST /api/plugins/scripts/settings`
    /// 的数据面语义）。
    ///
    /// 对齐上游 `script/settings.rs::configure`：
    /// - **options 未变**（只改 `providerOptions`）：不重启宿主，改走
    ///   `script.settings` 让宿主应用开关并回最新设置声明（`schema`）；
    /// - 保存失败时用旧 `providerOptions` 反向调 `script.settings`，把宿主
    ///   内存态回滚到保存前的值；
    /// - `options` 变化仍走「落盘 + 重建宿主」（包级参数只在 init 载荷下发）。
    ///
    /// 宿主不在跑（崩溃窗口）时无法刷新 schema，但仍保存（与上游对
    /// `host()` 为 None 的处理一致：下次重建会从 `script.packages` 回填）。
    pub async fn configure_settings(
        self: &Arc<Self>,
        package_id: &str,
        version: Option<&str>,
        options: Option<serde_json::Map<String, Value>>,
        provider_options: Option<serde_json::Map<String, Value>>,
    ) -> Result<ScriptPackage, String> {
        // 先加载包记录决定是否需要在线宿主，再走可注入宿主的纯逻辑
        // （单测用桩宿主的通道验证 script.settings 调用与回滚）。
        let packages = storage::load_packages(&self.store);
        let enabled = packages
            .iter()
            .rfind(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
            .is_some_and(|p| p.enabled);
        let host = if enabled { self.current_host() } else { None };
        let channel = host.as_ref().map(|host| host.rpc());
        configure_settings_with(
            self,
            channel.as_deref(),
            package_id,
            version,
            options,
            provider_options,
        )
        .await
    }

    /// 数据面路由（**同步**）。未接管 / 宿主不在跑 → `None`。
    ///
    /// `provider` 接受声明 id 或实际 id；返回值里的 `provider` 是实际 id。
    ///
    /// **快照一致性**：host 读锁要持有到 owners/aliases 都读完。rebuild 的
    /// 「升代际 + 换指针 + 换两张表」在同一把 host 写锁内提交，读者因此要么
    /// 看到旧代际的完整视图、要么看到新代际的完整视图——不会出现「新宿主配
    /// 旧路由表」的误路由（同步函数，全程不 await，不构成跨 await 持锁）。
    pub fn route_for_provider(&self, provider: &str) -> Option<ScriptRoute> {
        if provider.is_empty() {
            return None;
        }
        // 原生优先（热路径快速判定，不取路由锁）：AIGX 原生实现或已启用的原生
        // 插件包声明的 provider，脚本一律不接管（与 is_plugin_owned 同立场）。
        if script_takeover_blocked(&self.native_registry, provider) {
            return None;
        }
        let slot = self.host.read();
        let host = slot.as_ref()?.clone();
        if !self.owners.read().contains_key(provider) {
            return None;
        }
        let active = self
            .aliases
            .read()
            .get(provider)
            .cloned()
            .unwrap_or_else(|| provider.to_string());
        // 别名翻译后的**实际** id 撞原生同样让位：请求 id 不是原生，不代表
        // 实际接管的 id 不是（identities 是宿主自报，网关侧必须再兜一道）。
        if script_takeover_blocked(&self.native_registry, &active) {
            return None;
        }
        Some(ScriptRoute {
            channel: host.rpc(),
            provider: active,
        })
    }

    /// 启动时恢复：有已启用包就拉起宿主，否则什么都不做。
    pub async fn start(self: &Arc<Self>) {
        // 图标刷新 worker（进程内单例；无启用包也会启动，随 wake 拉取）
        super::icons::spawn(self.script_root.clone(), self.store.clone());
        let packages = storage::load_packages(&self.store);
        if !packages.iter().any(|p| p.enabled) {
            tracing::info!("[Script] 无已启用脚本包，宿主不启动");
            return;
        }
        if let Err(e) = self.rebuild("startup").await {
            tracing::warn!("[Script] 启动恢复失败: {e}");
        }
        // 首轮图标（后台，不阻塞启动）
        super::icons::wake(false);
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
            self.rebuild_locked(reason, false).await
        })
    }

    /// 崩溃重启专用入口：**拿到 rebuild_lock 之后**再复核代际，过期即放弃。
    ///
    /// 监督循环若只在锁外查代际，人工 rebuild 可能恰好在其「查完 → 等锁」的
    /// 窗口里完成并装好新宿主；此时再跑一次 rebuild 会把刚起来的宿主拆掉
    ///（双拉）。返回 `None` = 本次重拉已无意义（新的代际已接手）。
    ///
    /// 返回装箱 future 的原因同 [`Self::rebuild`]（同一个递归环上的声明边）。
    fn restart_after_crash<'a>(self: &'a Arc<Self>, generation: u64) -> CrashRestartFuture<'a> {
        Box::pin(async move {
            let _guard = self.rebuild_lock.lock().await;
            if self.generation.load(Ordering::Relaxed) != generation {
                return None;
            }
            Some(self.rebuild_locked("crash-restart", true).await)
        })
    }

    async fn rebuild_locked(
        self: &Arc<Self>,
        reason: &str,
        keep_crash_streak: bool,
    ) -> Result<ScriptHostStatus, String> {
        // 关停序列已启动：此刻 spawn 出来的宿主没人关，直接拒绝（含崩溃重拉）。
        if self.shutting_down.load(Ordering::SeqCst) {
            return Err("网关正在关闭，拒绝重建脚本宿主".to_string());
        }

        let mut packages: Vec<ScriptPackage> = storage::load_packages(&self.store)
            .into_iter()
            .filter(|p| p.enabled)
            .collect();

        if packages.is_empty() {
            // 没有启用的包：关掉宿主并清表（旧宿主排空后关闭）。
            // 升级代际 + 摘指针 + 清两张表：同一把 host 写锁内完成——崩溃监督
            // 循环的「代际复核 + 清表」也在这把锁内，两边线性化，不会互清。
            let old = {
                let mut slot = self.host.write();
                let mut owners = self.owners.write();
                let mut aliases = self.aliases.write();
                self.generation.fetch_add(1, Ordering::Relaxed);
                let old = slot.take();
                owners.clear();
                aliases.clear();
                old
            };
            {
                let mut rt = self.runtime.write();
                rt.running = false;
                rt.packages = 0;
                rt.providers.clear();
                rt.last_error = None;
                // crashes 保留（展示历史），下次成功启动清零
                if !keep_crash_streak {
                    rt.crashes = 0;
                }
            }
            if let Some(old) = old {
                self.spawn_drain(old);
            }
            // 无启用包：清掉设置声明（管理面不再显示已停用 provider 的设置）
            crate::plugin_host::provider_settings::clear_host_schemas();
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
        let reserved = self.reserved_providers(&packages);
        // claimed：{providerId: packageId}（A2A allocateProvider 的判据）。
        // 多个包声明同一 id 时先登记者赢（BTreeMap::or_insert 语义），与
        // A2A prepare_inner 一致。
        let mut claimed: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for pkg in &packages {
            for provider in &pkg.providers {
                // 包内 provider 级停用：不参与 claimed（宿主不会加载它）
                if pkg.disabled_providers.contains(&provider.id) {
                    continue;
                }
                claimed
                    .entry(provider.id.clone())
                    .or_insert_with(|| pkg.id.clone());
            }
        }
        let payload = build_init_payload(
            &self.script_root,
            &packages,
            reserved.iter().cloned().collect(),
            claimed,
            self.host_env(),
            Some(self.catalog.catalog_path().to_string_lossy().into_owned()),
        );

        // 先 spawn 新宿主再动任何状态：失败时旧宿主/旧代际原样保留
        //（否则新宿主没起来、旧监督循环却被代际失效，崩溃后无人接管）。
        // 失败同时写 last_error：状态快照的契约是「最近一次启动失败」可见。
        let new_host = match host_rs::spawn_host(&self.data_dir, payload, callbacks.clone()).await {
            Ok(host) => host,
            Err(e) => {
                let msg = format!("启动脚本宿主失败: {e}");
                // 旧宿主仍在服务（running/providers 保持原值），只记错误原因
                self.runtime.write().last_error = Some(msg.clone());
                return Err(msg);
            }
        };

        // 宿主包清单（`script.packages`）：init 应答只给 providers/identities，
        // 每包自报的 `settings`（设置 schema）/ 图标 / 别名 / 加载错误只有这份
        // 清单带回。拉取失败不拆宿主（脚本包少一份设置声明不该拖垮数据面），
        // 只 warn 并保留既有字段。
        //
        // **落盘必须写全表**：`packages` 只是启用子集，直接用它会从 KV 里
        // 抹掉未启用的包——合并回完整清单再写。
        match fetch_package_list(&new_host).await {
            Some(list) => {
                apply_package_list(&mut packages, &list);
                let mut stored = storage::load_packages(&self.store);
                apply_package_list(&mut stored, &list);
                if let Err(e) = storage::replace_all(&self.store, &self.data_dir, &stored) {
                    tracing::warn!("[Script] 回填宿主包清单失败（路由不受影响）：{e:#}");
                }
            }
            None => {
                tracing::warn!("[Script] 宿主未返回 script.packages 清单（设置声明/图标回填跳过）")
            }
        }

        // 路由表：声明 id → 实际 id（identities 映射；无冲突同名）
        let (owners, aliases, providers) = self.build_routing(&packages, &new_host, &reserved);

        // 提供商设置声明（A2A `settings.schema.v1` 的脚本形态）：宿主在
        // provider 元数据的 `settings` 字段里回声明。注册到全局表供管理面
        // 「提供商设置」页读取（plugin-api 的 handler 只调
        // `provider_settings::snapshot/configure`，不直接接触宿主）。
        // generation 用「当前代际 + 1」——与下方提交点升代际同值，重建后
        // 旧 revision 自然失效。
        register_provider_settings(
            &packages,
            new_host.providers(),
            self.generation.load(Ordering::Relaxed) + 1,
        );

        // 别名表落盘（KV `plugin:script-identities` + 投影）：宿主重启后
        // JS 侧从 registry.json 读到的 identities 与网关路由同源。
        // init 应答的 identities 为主，`script.packages` 的 per-package
        // aliases 作补充（上游以包清单的 aliases 为权威）。
        // 失败只 warn——路由表在内存里已就位，落盘只是给宿主读。
        let mut persisted = identities_from_host(new_host.identities());
        for package in &packages {
            for (declared, actual) in &package.provider_aliases {
                if let Some(actual) = actual.as_str() {
                    persisted
                        .entry(package.id.clone())
                        .or_default()
                        .insert(declared.clone(), actual.to_string());
                }
            }
        }
        if let Err(e) = storage::save_identities(&self.store, &self.data_dir, persisted) {
            tracing::warn!("[Script] 落盘 identities 失败（路由不受影响）：{e:#}");
        }

        // 提交点：升代际 + 换指针 + 换两张表在同一把 host 写锁内完成。
        // 读者（route_for_provider）持 host 读锁期间才读这两张表，因此永远
        // 看到同一代际的完整视图；崩溃监督循环的清理也用同一把锁复核代际。
        //
        // **锁内复核关停标记**：shutdown() 不抢 rebuild_lock（它可能被一次
        // 含 Bun 下载的重建持有数分钟，关停不能被它拖住）。若关停已置位，
        // 本函数刚 spawn 的宿主会被装上却再没人关——所以在同一把锁内再查
        // 一次，命中即放弃提交、并在锁外关掉新宿主。shutdown 与提交都在这
        // 把写锁上线性化：要么本函数先提交（随后 shutdown 的 take 会关掉它），
        // 要么 shutdown 先置位（本函数看到标记而放弃），不存在孤儿进程。
        let committed = {
            let mut slot = self.host.write();
            if self.shutting_down.load(Ordering::SeqCst) {
                None
            } else {
                let mut owners_lock = self.owners.write();
                let mut aliases_lock = self.aliases.write();
                let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
                let old = slot.replace(new_host.clone());
                *owners_lock = owners;
                *aliases_lock = aliases;
                Some((old, generation))
            }
        };
        let Some((old, generation)) = committed else {
            // 关停窗口里起好的宿主：直接关闭，不留孤儿。
            host_rs::shutdown(&new_host).await;
            return Err("网关正在关闭，放弃提交新宿主".to_string());
        };
        {
            let mut rt = self.runtime.write();
            rt.running = true;
            rt.packages = packages.len();
            rt.providers = providers;
            rt.last_error = None;
            // 人工/启动路径是一次全新启用周期：崩溃连续计数清零；
            // 崩溃重拉成功则保留计数，供下一次崩溃选更长的退避档位。
            if !keep_crash_streak {
                rt.crashes = 0;
            }
        }
        if let Some(old) = old {
            // 旧宿主的 HTTP/WS 会话表随旧回调一起清空（新宿主用新表）
            self.callbacks.write().take();
            self.spawn_drain(old);
        }
        *self.callbacks.write() = Some(callbacks.clone());
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
    ///
    /// **不抢 `rebuild_lock`**：一次重建可能正卡在 Bun 下载/init 握手（最长
    /// 数分钟），关停不能被它拖住。关停与重建在 `host` 写锁上线性化——
    /// 重建的提交点会在锁内复核 `shutting_down`：命中则放弃提交并关掉刚起
    /// 的宿主；已提交的宿主由这里的 take 关闭。两条路径都不会留下孤儿进程。
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        crate::plugin_host::provider_settings::clear_host_schemas();
        // 清空 HTTP/WS 会话表（关闭前断掉脚本遗留的 egress 会话）
        if let Some(cb) = self.callbacks.write().take() {
            cb.clear_sessions();
        }
        let old = {
            let mut slot = self.host.write();
            let mut owners = self.owners.write();
            let mut aliases = self.aliases.write();
            self.generation.fetch_add(1, Ordering::Relaxed);
            let old = slot.take();
            owners.clear();
            aliases.clear();
            old
        };
        {
            let mut rt = self.runtime.write();
            rt.running = false;
            rt.packages = 0;
            rt.providers.clear();
        }
        if let Some(old) = old {
            host_rs::shutdown(&old).await;
        }
    }

    /// reserved 集合：**插件优先**（2026-11 修订）。
    ///
    /// 起点是「内置原生 provider + 原生插件包声明的 provider」，再**减去**
    /// 已启用脚本包声明的 provider——脚本也是插件，同样优先于原生实现。
    /// 例外（保留不放）：
    /// - 硬保留集合（[`crate::plugin_host::registry::HARD_RESERVED`]）——
    ///   网关内部虚拟 id，谁都不能接管；
    /// - 已启用**原生插件包**声明的 provider——两类插件都声明时原生包优先
    ///   （与 A2A `prepare_inner` 的 `native_owners` 口径一致）。
    fn reserved_providers(&self, packages: &[ScriptPackage]) -> BTreeSet<String> {
        let mut set: BTreeSet<String> = crate::account_session::registry::PROVIDERS
            .iter()
            .map(|s| s.kind.to_string())
            .collect();
        set.extend(
            crate::account_session::registry::SESSION_PROVIDERS
                .iter()
                .map(|(kind, _)| kind.to_string()),
        );
        // 原生插件包声明的 provider（这些不因脚本声明而让位）
        let mut native_pack_owned: BTreeSet<String> = BTreeSet::new();
        for pack in self.native_registry.list() {
            if !pack.enabled {
                continue;
            }
            for provider in pack.providers {
                native_pack_owned.insert(provider.id);
            }
        }
        set.extend(native_pack_owned.iter().cloned());
        // 插件优先：已启用脚本包声明的 provider 从 reserved 移除（原生降为兜底）。
        // 包内 provider 级停用（`disabled_providers`）不解除 reserved——宿主不
        // 加载它，让位给原生实现才是正确语义。
        for pkg in packages.iter().filter(|p| p.enabled) {
            for provider in &pkg.providers {
                if pkg.disabled_providers.contains(&provider.id) {
                    continue;
                }
                if !native_pack_owned.contains(&provider.id)
                    && !crate::plugin_host::registry::is_hard_reserved(&provider.id)
                {
                    set.remove(&provider.id);
                }
            }
        }
        set
    }

    /// 由启用包集合 + init 应答构造路由表。
    ///
    /// 投影到纯函数 [`compute_routing`]（宿主句柄只提供只读快照），
    /// 便于单测覆盖「加载失败让位 / 声明与实际 id 不同 / 未声明 provider
    /// 的 npm 包 / 与原生重叠必须让位」这些路由形态。
    #[allow(clippy::type_complexity)]
    fn build_routing(
        &self,
        packages: &[ScriptPackage],
        host: &ScriptHost,
        reserved: &BTreeSet<String>,
    ) -> (
        HashMap<String, String>,
        HashMap<String, String>,
        Vec<String>,
    ) {
        compute_routing(packages, host.providers(), host.identities(), reserved)
    }

    /// 监督一个宿主进程的生命周期。
    async fn supervise_host(self: Arc<Self>, host: Arc<ScriptHost>, generation: u64) {
        let started = tokio::time::Instant::now();
        host_rs::supervise(host.clone()).await;
        if host.is_shutdown_requested() {
            return;
        }
        let ran_for = started.elapsed();
        // 意外退出：整段「复核代际 → 摘指针 → 清两张表」在同一把 host 写锁内。
        //
        // 代际检查单独放在锁外挡不住窗口：人工 rebuild 可能在检查通过之后才
        // 完成提交（升代际 + 换指针 + 换表都在同一把写锁内），旧循环若仍去
        // 清 owners/aliases，就把**新宿主**的路由表清空了——数据面静默停服
        // 直到下一次 rebuild。指针 ptr_eq 命中才是唯一的权威判据。
        {
            let mut slot = self.host.write();
            if self.generation.load(Ordering::Relaxed) != generation {
                return;
            }
            if !slot.as_ref().is_some_and(|h| Arc::ptr_eq(h, &host)) {
                // 指针已被更新的宿主取代：它的路由/状态归它自己的监督循环维护
                return;
            }
            *slot = None;
            self.owners.write().clear();
            self.aliases.write().clear();
        }
        let crashes = {
            let mut rt = self.runtime.write();
            rt.running = false;
            rt.packages = 0;
            rt.providers.clear();
            // 稳定运行后崩溃 = 新一次独立崩溃：连续计数从 1 重算（否则偶发
            // 崩溃会被历史累计拖进 60s 退避）；快速崩溃循环才累加升档。
            rt.crashes = if ran_for >= STABLE_RUN {
                1
            } else {
                rt.crashes.saturating_add(1)
            };
            rt.crashes
        };
        tracing::warn!(
            "[Script] 脚本宿主意外退出（连续第 {crashes} 次，运行 {}s），退避重启",
            ran_for.as_secs()
        );

        for attempt in 0..MAX_RESTART_ATTEMPTS {
            // 退避按「连续崩溃次数 + 本次事件内的失败次数」升档（钳到表尾），
            // 避免「起来就崩」的宿主以 1s 节奏无限重拉。
            tokio::time::sleep(backoff_delay(crashes, attempt)).await;
            if self.generation.load(Ordering::Relaxed) != generation {
                return;
            }
            if self.shutting_down.load(Ordering::SeqCst) {
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
            // 锁内复核代际：人工 rebuild 若已在等锁窗口完成，这里放弃，
            // 绝不把刚起来的宿主再拆一次（拒绝双拉）。
            match self.restart_after_crash(generation).await {
                None => {
                    tracing::debug!("[Script] 崩溃重拉已由新的代际接管，本次放弃");
                    return;
                }
                Some(Ok(_)) => {
                    tracing::info!("[Script] 宿主崩溃后重拉成功（第 {} 次尝试）", attempt + 1);
                    return;
                }
                Some(Err(e)) => {
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
/// **原生优先**：`reserved`（AIGX 原生实现 ∪ 原生插件包声明的 provider）
/// 里的 id 一律不登记——JS 侧装载器本应按 reserved 让位，这里是网关侧
/// 再兜一道（声明 id 与实际 id 两个方向都查）。
///
/// 返回 `(owners, aliases, actuals)`：
/// - owners：声明 id 与实际 id 都指向包 id（两个方向都能路由）；
/// - aliases：声明 id → 实际 id（无映射 = 同名）；
/// - actuals：实际接管的 id 列表（管理面展示）。
#[allow(clippy::type_complexity)]
fn compute_routing(
    packages: &[ScriptPackage],
    providers: &[ScriptProviderInfo],
    identities: &serde_json::Map<String, serde_json::Value>,
    reserved: &BTreeSet<String>,
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
            // 原生让位（网关侧兜底）：声明 id 或实际 id 撞 reserved 都不登记。
            if reserved.contains(requested) || reserved.contains(actual) {
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
            // 包内 provider 级停用：宿主不加载它，路由表不登记
            if pkg.disabled_providers.contains(&declared.id) {
                continue;
            }
            if reserved.contains(&declared.id) {
                continue;
            }
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

/// 路由权威判据：provider 已有 AIGX 原生实现，或由**已启用的原生插件包**
/// 声明 → 脚本不接管（与 `is_plugin_owned` 同立场）。
///
/// 数据面热路径也会走到这里，`is_plugin_owned` 只做列表扫描 + 一次 id
/// clone，没有分配放大。
/// 数据面让位判据（脚本侧）：provider 由**已启用的原生插件包**声明，
/// 或属于硬保留集合 → 脚本不接管。
///
/// **不再判 `is_native_provider`**：插件优先（2026-11 修订）——脚本包
/// 声明了某个有原生实现的 provider，就由脚本接管，原生降为兜底。这与
/// `registry::is_plugin_owned` 同立场。
///
/// 热路径：`is_plugin_owned` 只做列表扫描 + 一次 id clone，无分配放大。
fn script_takeover_blocked(native_registry: &PackRegistry, provider: &str) -> bool {
    crate::plugin_host::registry::is_hard_reserved(provider)
        || native_registry.is_plugin_owned(provider)
}

/// 崩溃重启的退避时长：连续崩溃次数决定起始档位，单次崩溃事件内的重拉
/// 失败次数继续后移；索引钳到表尾（表长是常量 7，无除零面；`saturating`
/// 防长期崩溃把计数推到 u32 边界时溢出）。
fn backoff_delay(crashes: u32, attempt: u32) -> Duration {
    let index = crashes.saturating_sub(1).saturating_add(attempt);
    RESTART_BACKOFF[(index as usize).min(RESTART_BACKOFF.len() - 1)]
}

/// 宿主上报的 identities JSON → 落盘用的强类型表。
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

/// 对一个宿主句柄调一次管理面方法（超时 + 错误文案归一）。
async fn call_host_on(
    host: &ScriptHost,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    call_channel(&host.rpc(), method, params, timeout).await
}

/// 对一个宿主 RPC 通道调一次管理面方法（超时 + 错误文案归一）。
///
/// 单独抽出来是为了单测能注入桩通道（后台任务应答 `script.settings`）。
async fn call_channel(
    channel: &RpcChannel,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    match tokio::time::timeout(timeout, channel.call(method, params)).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.message),
        Err(_) => Err(format!("脚本宿主 {method} 超时")),
    }
}

/// [`ScriptHostManager::configure_settings`] 的核心逻辑（宿主通道可注入）。
///
/// `host` 为 `None` = 宿主不在跑（或包未启用）：跳过 `script.settings`，
/// 只落盘。对齐上游 `script/settings.rs::configure`：
/// - options 未变 → `script.settings` 应用 providerOptions 并回最新 schema；
///   保存失败时反向调用回滚宿主内存态；
/// - options 变化 → 委托 `operations::update_settings`（落盘 + 重建宿主）。
async fn configure_settings_with(
    manager: &Arc<ScriptHostManager>,
    host: Option<&RpcChannel>,
    package_id: &str,
    version: Option<&str>,
    options: Option<serde_json::Map<String, Value>>,
    provider_options: Option<serde_json::Map<String, Value>>,
) -> Result<ScriptPackage, String> {
    let mut packages = storage::load_packages(manager.store());
    let index = packages
        .iter()
        .rposition(|p| p.id == package_id && version.is_none_or(|v| p.version == v))
        .ok_or_else(|| format!("包未安装：{package_id}"))?;
    let before = packages[index].clone();
    if let Some(values) = &options {
        settings::validate_values(values)?;
    }
    if let Some(map) = &provider_options {
        let declared: Vec<&str> = before.providers.iter().map(|p| p.id.as_str()).collect();
        for (provider, values) in map {
            if !declared.contains(&provider.as_str()) {
                return Err(format!("开关不属于当前插件：{provider}"));
            }
            let values = values
                .as_object()
                .ok_or_else(|| "提供商开关必须是对象".to_string())?;
            if values.values().any(|v| !v.is_boolean()) {
                return Err("提供商开关必须是布尔值".into());
            }
        }
    }

    let options_changed = options
        .as_ref()
        .is_some_and(|values| values != &before.options.values);
    let mut next = before.clone();
    if let Some(values) = options {
        next.options.values = values;
    }
    if let Some(map) = provider_options {
        next.provider_options = map;
    }

    // options 变化：包级参数只在 init 载荷下发 → 走既有「落盘 + 重建」，
    // 复用 `operations::update_settings` 的同一套校验与错误文案。
    if options_changed {
        return super::operations::update_settings(
            manager,
            package_id,
            version,
            Some(next.options.values),
            Some(next.provider_options),
        )
        .await;
    }

    // 只改 providerOptions（或纯读回 schema）：宿主在线则应用并回最新设置
    // 声明（不重启 Bun）。与上游口径一致——未变化也照样拉一次，保证返回的
    // schema 是最新的。
    if let Some(channel) = host {
        let schema = call_channel(
            channel,
            host_admin::SETTINGS,
            serde_json::json!({
                "package": package_id,
                "providerOptions": next.provider_options,
            }),
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| format!("宿主拒绝设置更新：{e}"))?;
        next.settings_schema = schema;
    }
    packages[index] = next.clone();
    if let Err(e) = storage::replace_all(manager.store(), manager.data_dir(), &packages) {
        // 保存失败：把宿主内存态回滚到旧 providerOptions（尽力而为）
        if let Some(channel) = host {
            let _ = call_channel(
                channel,
                host_admin::SETTINGS,
                serde_json::json!({
                    "package": package_id,
                    "providerOptions": before.provider_options,
                }),
                Duration::from_secs(10),
            )
            .await;
        }
        return Err(format!("保存脚本包状态失败：{e:#}"));
    }
    Ok(next)
}

/// 拉宿主包清单（`script.packages`，10s——与上游 `call_coded` 同预算）。
///
/// 失败 / 形状不对返回 `None`（**非致命**：清单只带每包自报的设置声明、
/// 图标与加载错误，缺它们不影响数据面路由；上游把失败当致命，AIGX 选择
/// 「设置页少一条声明」而不是「整台宿主起不来」的保守方向）。
async fn fetch_package_list(host: &ScriptHost) -> Option<Vec<Value>> {
    match call_host_on(
        host,
        host_admin::PACKAGES,
        serde_json::json!({}),
        Duration::from_secs(10),
    )
    .await
    {
        Ok(value) => match value.as_array() {
            Some(list) => Some(list.clone()),
            None => {
                tracing::warn!("[Script] script.packages 应答不是数组，忽略");
                None
            }
        },
        Err(e) => {
            tracing::warn!("[Script] 拉取 script.packages 失败：{e}");
            None
        }
    }
}

/// 把 `script.packages` 清单回填到包记录。
///
/// 只覆盖宿主自报的字段：`error` / `settings`（设置声明）/ `icon` /
/// `providerIcons` / `aliases`。`providers` 保持网关侧声明（label/models
/// 是徽标与模型页的数据源，宿主清单只回 id 列表）。
fn apply_package_list(packages: &mut [ScriptPackage], list: &[Value]) {
    for item in list {
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        let Some(package) = packages.iter_mut().find(|package| package.id == id) else {
            continue;
        };
        package.error = item
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(schema) = item.get("settings").filter(|value| value.is_object()) {
            package.settings_schema = schema.clone();
        }
        // 图标：显式 null 表示宿主撤下图标（清空）；键缺失时保留旧值
        if let Some(icon) = item.get("icon") {
            package.icon = icon.as_str().map(str::to_string);
        }
        if let Some(icons) = item.get("providerIcons").and_then(Value::as_object) {
            package.provider_icons = icons
                .iter()
                .filter_map(|(provider, icon)| {
                    icon.as_str()
                        .map(|icon| (provider.clone(), Value::String(icon.to_string())))
                })
                .collect();
        }
        if let Some(aliases) = item.get("aliases").and_then(Value::as_object) {
            package.provider_aliases = aliases
                .iter()
                .filter(|(_, actual)| actual.is_string())
                .map(|(declared, actual)| (declared.clone(), actual.clone()))
                .collect();
        }
    }
}

/// 把宿主上报的 provider 设置声明注册到全局表（供管理面「提供商设置」页）。
///
/// A2A 的脚本宿主在 `script.packages` 里回 `settings`（`packageSettings`
/// 形状：`{id, optionsExample, providerOptions:{provider:[{key,label,…}]}}`）；
/// AIGX 走原生握手形态时 provider 元数据里也可能带 `settings_schema`。
/// 两处都试：优先 `settings_schema`（完整 Schema），否则从
/// `providerOptions` 的选项清单合成一份「boolean 开关」Schema。
///
/// 解析失败只 warn——设置页少一个 provider 不影响数据面。
fn register_provider_settings(
    packages: &[ScriptPackage],
    providers: &[ScriptProviderInfo],
    generation: u64,
) {
    let package_of = |provider_id: &str| {
        packages
            .iter()
            .find(|p| p.providers.iter().any(|pr| pr.id == provider_id))
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };
    let mut entries = Vec::new();
    for provider in providers {
        let schema_value = provider
            .extra
            .get("settings_schema")
            .filter(|v| !v.is_null())
            .cloned()
            .or_else(|| {
                provider
                    .extra
                    .get("settings")
                    .and_then(|s| s.get("providerOptions"))
                    .and_then(|opts| opts.get(&provider.id))
                    .and_then(|options| synthesize_schema(&provider.label, options))
            })
            // 宿主 `script.packages` 回填的包级设置声明（A2A 权威形态：
            // `settings.providerOptions.{provider}` 是选项清单）
            .or_else(|| {
                packages
                    .iter()
                    .find(|package| package.providers.iter().any(|pr| pr.id == provider.id))
                    .and_then(|package| package.settings_schema.get("providerOptions"))
                    .and_then(|opts| opts.get(&provider.id))
                    .and_then(|options| synthesize_schema(&provider.label, options))
            });
        let Some(schema_value) = schema_value else {
            continue;
        };
        match crate::plugin_host::provider_settings::schema::parse(&provider.id, &schema_value) {
            Ok(Some(schema)) => entries.push((
                provider.id.clone(),
                crate::plugin_host::provider_settings::RegisteredSchema {
                    schema,
                    package: package_of(&provider.id),
                    label: provider.label.clone(),
                    generation,
                },
            )),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    "[Script] provider {} 的设置声明无效，已忽略：{e}",
                    provider.id
                );
            }
        }
    }
    crate::plugin_host::provider_settings::register_host_schemas(entries);
}

/// 从 A2A `providerOptions` 的选项清单合成一份 boolean 开关 Schema。
///
/// 选项形状 `[{key, label, description?, default?}]`（见 `environment.mjs`
/// 的 `optionDefinitions`）；每项 → 一个 `boolean` 字段。
fn synthesize_schema(label: &str, options: &serde_json::Value) -> Option<serde_json::Value> {
    let list = options.as_array()?;
    let fields: Vec<serde_json::Value> = list
        .iter()
        .filter_map(|option| {
            let key = option.get("key").and_then(serde_json::Value::as_str)?;
            Some(serde_json::json!({
                "key": key,
                "label": option
                    .get("label")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::String(key.to_string())),
                "type": "boolean",
                "default": option.get("default").and_then(serde_json::Value::as_bool).unwrap_or(false),
            }))
        })
        .collect();
    if fields.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "version": 1,
        "title": if label.is_empty() { "设置" } else { label },
        "fields": fields,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn backoff_table_is_monotonic() {
        for w in RESTART_BACKOFF.windows(2) {
            assert!(w[0] <= w[1], "退避表必须单调不减");
        }
        assert_eq!(RESTART_BACKOFF[0], Duration::from_secs(1));
        assert_eq!(*RESTART_BACKOFF.last().unwrap(), Duration::from_secs(60));
    }

    fn provider(id: &str) -> ScriptProviderInfo {
        ScriptProviderInfo::new(id, id)
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
        let (owners, aliases, actuals) =
            compute_routing(&packages, &providers, &identities, &BTreeSet::new());
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
        let (owners, aliases, actuals) =
            compute_routing(&packages, &[], &serde_json::Map::new(), &BTreeSet::new());
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
        let (owners, _aliases, actuals) =
            compute_routing(&packages, &providers, &identities, &BTreeSet::new());
        assert_eq!(owners.get("astudio"), Some(&"some-npm-pkg".to_string()));
        assert_eq!(actuals, vec!["astudio".to_string()]);
    }

    #[test]
    fn routing_falls_back_to_package_declaration() {
        // 老宿主不报 identities：包声明与实际 id 同名时仍要能路由。
        let packages = vec![pkg("@a/kiro", &["kiro"])];
        let providers = vec![provider("kiro")];
        let (owners, aliases, actuals) = compute_routing(
            &packages,
            &providers,
            &serde_json::Map::new(),
            &BTreeSet::new(),
        );
        assert_eq!(owners.get("kiro"), Some(&"@a/kiro".to_string()));
        assert!(aliases.is_empty());
        assert_eq!(actuals, vec!["kiro".to_string()]);
    }

    #[test]
    fn routing_yields_reserved_even_with_alias() {
        // 网关侧兜底：宿主（即使行为异常）把原生 provider「kiro」报成实际
        // 接管 id「kiro-2」，reserved 包含 kiro 时整对映射都必须丢弃——
        // 脚本对原生实现没有接管权。
        let packages = vec![pkg("@a/kiro", &["kiro"])];
        let providers = vec![provider("kiro-2")];
        let mut identities = serde_json::Map::new();
        identities.insert("@a/kiro".into(), json!({ "kiro": "kiro-2" }));
        let reserved: BTreeSet<String> = ["kiro".to_string()].into_iter().collect();
        let (owners, aliases, actuals) =
            compute_routing(&packages, &providers, &identities, &reserved);
        assert!(owners.is_empty(), "声明 id 撞 reserved 不得登记");
        assert!(aliases.is_empty());
        assert!(actuals.is_empty());
    }

    #[test]
    fn routing_fallback_skips_reserved_declared_id() {
        // 兜底路径同样让位：包清单声明的原生 provider 不许登记。
        let packages = vec![pkg("@a/kiro", &["kiro", "custom"])];
        let providers = vec![provider("kiro"), provider("custom")];
        let reserved: BTreeSet<String> = ["kiro".to_string()].into_iter().collect();
        let (owners, aliases, actuals) =
            compute_routing(&packages, &providers, &serde_json::Map::new(), &reserved);
        assert!(!owners.contains_key("kiro"), "原生 id 不入 owners");
        assert_eq!(owners.get("custom"), Some(&"@a/kiro".to_string()));
        assert!(aliases.is_empty());
        assert_eq!(actuals, vec!["custom".to_string()]);
    }

    #[test]
    fn routing_skips_provider_disabled_inside_package() {
        // 包内 provider 级停用（`disabled_providers`）：宿主不加载它，
        // 兜底路由也不得登记（否则会留一条指向不存在 provider 的死路由）。
        let mut package = pkg("@a/kiro", &["kiro", "astudio"]);
        package.disabled_providers = vec!["kiro".into()];
        let packages = vec![package];
        let providers = vec![provider("kiro"), provider("astudio")];
        let (owners, _aliases, actuals) = compute_routing(
            &packages,
            &providers,
            &serde_json::Map::new(),
            &BTreeSet::new(),
        );
        assert!(!owners.contains_key("kiro"), "包内停用的 provider 不得登记");
        assert_eq!(owners.get("astudio"), Some(&"@a/kiro".to_string()));
        assert_eq!(actuals, vec!["astudio".to_string()]);
    }

    #[test]
    fn backoff_escalates_with_consecutive_crashes() {
        // 首次崩溃首次重拉 = 1s；崩溃数与失败数都按档位后移；钳到 60s。
        assert_eq!(backoff_delay(1, 0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1, 1), Duration::from_secs(2));
        assert_eq!(backoff_delay(5, 0), Duration::from_secs(16));
        assert_eq!(backoff_delay(6, 0), Duration::from_secs(30));
        assert_eq!(backoff_delay(7, 0), Duration::from_secs(60));
        // 越界输入不 panic、不溢出（钳表尾）
        assert_eq!(backoff_delay(u32::MAX, u32::MAX), Duration::from_secs(60));
    }

    #[test]
    fn takeover_blocked_by_native_and_native_pack() {
        use crate::plugin_host::registry::{PackMeta, PackProvider, PackRegistry};
        use crate::storage::FileStore;

        // 插件优先：内置原生实现（openai）**不再**让位——脚本声明了就能接管
        let dir = tempfile::TempDir::new().unwrap();
        let registry = PackRegistry::new(Arc::new(FileStore::new(dir.path().to_path_buf())));
        assert!(
            !script_takeover_blocked(&registry, "openai"),
            "插件优先：无原生插件包声明时，内置原生实现也允许脚本接管"
        );
        // 硬保留集合（内部虚拟 id）永远让位
        assert!(script_takeover_blocked(&registry, "cloudflare"));
        assert!(script_takeover_blocked(&registry, "hub"));
        assert!(script_takeover_blocked(&registry, "custom-abc"));

        // 已启用的原生插件包声明的 provider 让位（两类插件都声明时原生包优先）。
        const PACK_PROVIDER: &str = "script-demo-provider";
        let meta = PackMeta {
            id: "native-pack".into(),
            name: "native-pack".into(),
            version: "1.0.0".into(),
            enabled: true,
            providers: vec![PackProvider {
                id: PACK_PROVIDER.into(),
                label: "Demo".into(),
                order: 0,
                default_enabled: true,
            }],
            ..Default::default()
        };
        registry.upsert(meta.clone()).unwrap();
        assert!(script_takeover_blocked(&registry, PACK_PROVIDER));
        assert!(
            !script_takeover_blocked(&registry, "free-provider"),
            "未被任何原生实现声明的 provider 可被脚本接管"
        );
        // upsert 会保留既有 enabled（重装不重置运维选择），停用要走 set_enabled
        registry.set_enabled("native-pack", false).unwrap();
        assert!(
            !script_takeover_blocked(&registry, PACK_PROVIDER),
            "已停用的原生包不抢路由"
        );
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

    /// `script.packages` 清单回填：宿主自报的设置声明 / 图标 / 别名 /
    /// 加载错误进包记录；形状不符的字段忽略、未知包忽略。
    #[test]
    fn apply_package_list_backfills_host_reported_fields() {
        let mut packages = vec![pkg("@a/kiro", &["kiro"])];
        let list = vec![
            json!({
                "id": "@a/kiro",
                "providers": ["kiro-2"],
                "aliases": { "kiro": "kiro-2", "bad": 7 },
                "icon": "data:image/png;base64,zz",
                "providerIcons": { "kiro-2": "https://example.invalid/i.png", "bad": 7 },
                "error": "load failed",
                "settings": {
                    "id": "@a/kiro",
                    "providerOptions": { "kiro-2": [{ "key": "fast", "label": "Fast" }] }
                }
            }),
            json!({ "id": "not-installed", "settings": {} }),
            json!("garbage"),
        ];
        apply_package_list(&mut packages, &list);
        let package = &packages[0];
        assert_eq!(package.error.as_deref(), Some("load failed"));
        assert_eq!(package.icon.as_deref(), Some("data:image/png;base64,zz"));
        assert_eq!(package.provider_icons.len(), 1, "非字符串图标应丢弃");
        assert_eq!(
            package
                .provider_aliases
                .get("kiro")
                .and_then(|v| v.as_str()),
            Some("kiro-2")
        );
        assert!(package.provider_aliases.get("bad").is_none());
        assert_eq!(
            package.settings_schema["providerOptions"]["kiro-2"][0]["key"],
            "fast"
        );
        // providers 保持网关侧声明（label/models 是徽标与模型页数据源）
        assert_eq!(package.providers[0].id, "kiro");
    }

    #[test]
    fn apply_package_list_keeps_fields_when_absent() {
        let mut package = pkg("@a/kiro", &["kiro"]);
        package.settings_schema = json!({ "version": 1 });
        package.icon = Some("https://example.invalid/old.png".into());
        let mut packages = vec![package];
        apply_package_list(&mut packages, &[json!({ "id": "@a/kiro" })]);
        assert_eq!(packages[0].settings_schema, json!({ "version": 1 }));
        assert_eq!(
            packages[0].icon.as_deref(),
            Some("https://example.invalid/old.png")
        );
        assert!(packages[0].error.is_none());
        // 显式 null 图标 = 宿主撤下图标（清空）
        apply_package_list(&mut packages, &[json!({ "id": "@a/kiro", "icon": null })]);
        assert!(packages[0].icon.is_none());
    }

    // ── configure_settings（桩宿主 RPC 通道）─────────────────────────

    /// 桩宿主：后台应答 `script.settings`（回显收到的 providerOptions），
    /// 记录每次调用的 params 供断言。
    fn stub_host(
        expected_calls: usize,
    ) -> (
        Arc<RpcChannel>,
        Arc<StdMutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        stub_host_with(expected_calls, false)
    }

    /// 同上；`reply_error=true` 时宿主对每次调用回错误帧（拒绝路径）。
    fn stub_host_with(
        expected_calls: usize,
        reply_error: bool,
    ) -> (
        Arc<RpcChannel>,
        Arc<StdMutex<Vec<Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use crate::plugin_host::rpc::CallbackHandler;
        struct NoCallbacks;
        impl CallbackHandler for NoCallbacks {
            fn handle(
                &self,
                method: &str,
                _params: Value,
            ) -> Result<Value, crate::plugin_host::rpc::RpcError> {
                Err(crate::plugin_host::rpc::RpcError {
                    message: format!("stub unsupported: {method}"),
                    code: Some("unsupported".into()),
                })
            }
        }
        let (client_out, host_in) = tokio::io::duplex(64 * 1024);
        let (host_out, client_in) = tokio::io::duplex(64 * 1024);
        let channel = Arc::new(RpcChannel::new(
            Box::new(client_in),
            Box::new(client_out),
            Arc::new(NoCallbacks),
        ));
        let seen = Arc::new(StdMutex::new(Vec::new()));
        let seen_task = seen.clone();
        let handle = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(host_in);
            let mut writer = host_out;
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            for _ in 0..expected_calls {
                let mut buf = Vec::new();
                let Ok(read) = reader.read_until(b'\n', &mut buf).await else {
                    return;
                };
                if read == 0 {
                    return;
                }
                let Ok(frame) =
                    crate::plugin_host::rpc::decode_frame(String::from_utf8_lossy(&buf).trim())
                else {
                    return;
                };
                if let Ok(mut items) = seen_task.lock() {
                    items.push(frame.params.clone().unwrap_or(Value::Null));
                }
                // 回最新设置声明（A2A 形状：{id, optionsExample, providerOptions}）；
                // `reply_error` 时回错误帧（宿主拒绝路径）。
                let reply = if reply_error {
                    json!({
                        "id": frame.id,
                        "error": { "code": "internal", "message": "stub host rejected settings" },
                    })
                } else {
                    json!({
                        "id": frame.id,
                        "result": {
                            "id": "stub",
                            "optionsExample": {},
                            "providerOptions": frame.params.as_ref()
                                .and_then(|p| p.get("providerOptions"))
                                .cloned()
                                .unwrap_or_else(|| json!({})),
                        },
                    })
                };
                let mut line = reply.to_string();
                line.push('\n');
                if writer.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
        });
        (channel, seen, handle)
    }

    fn manager_with_store(
        store: Arc<crate::storage::FileStore>,
        data_dir: &Path,
    ) -> Arc<ScriptHostManager> {
        // Catalog::spawn 会启动后台网络刷新（首刷立即打网络）。测试不需要
        // 目录——首刷失败只落错误状态，不 panic；但为免并行测试互相拖慢，
        // 用一次性 tempdir 作 script_root 即可（每次独立目录）。
        let catalog = Catalog::spawn(&script_plugins_root(data_dir));
        // 账号池 / 原生包注册表只要求「存在且不抛错」，用 data_dir 下的独立
        // 子目录（不能共享进程级临时目录——并行测试会互相污染 SQLite）。
        let sessions_dir = data_dir.join("account-sessions");
        let registry_dir = data_dir.join("native-registry");
        ScriptHostManager::new(
            data_dir.to_path_buf(),
            store,
            Arc::new(AccountSessionStore::new(Arc::new(
                crate::storage::FileStore::new(sessions_dir),
            ))),
            crate::test_http_client(),
            catalog,
            Arc::new(PackRegistry::new(Arc::new(crate::storage::FileStore::new(
                registry_dir,
            )))),
        )
    }

    /// 只改 providerOptions 且宿主在线：走 `script.settings`（不重建），
    /// 返回的 schema 回填到包记录并落盘。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn configure_settings_applies_provider_options_via_host() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        let manager = manager_with_store(store.clone(), dir.path());
        let mut package = pkg("@a/kiro", &["kiro"]);
        package.enabled = true;
        storage::replace_all(&store, dir.path(), &[package]).unwrap();

        let (channel, seen, task) = stub_host(1);
        let updated = configure_settings_with(
            &manager,
            Some(channel.as_ref()),
            "@a/kiro",
            None,
            None,
            Some(
                [("kiro".to_string(), json!({"fast": true}))]
                    .into_iter()
                    .collect(),
            ),
        )
        .await
        .unwrap();
        task.await.unwrap();
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["package"], "@a/kiro");
        assert_eq!(calls[0]["providerOptions"]["kiro"]["fast"], true);
        assert_eq!(updated.provider_options["kiro"]["fast"], true);
        // schema 由宿主应答回填（stub 回显 providerOptions）
        assert_eq!(
            updated.settings_schema["providerOptions"]["kiro"]["fast"],
            true
        );
        // 落盘（KV）
        let stored = storage::load_packages(&store);
        assert_eq!(stored[0].provider_options["kiro"]["fast"], true);
    }

    /// providerOptions 不属于该包声明的 provider / 非布尔值：拒绝且不打宿主。
    #[tokio::test]
    async fn configure_settings_rejects_unknown_provider_and_non_boolean() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        let manager = manager_with_store(store.clone(), dir.path());
        storage::replace_all(&store, dir.path(), &[pkg("@a/kiro", &["kiro"])]).unwrap();
        let (channel, seen, _task) = stub_host(0);
        for map in [
            [("other".to_string(), json!({"fast": true}))]
                .into_iter()
                .collect(),
            [("kiro".to_string(), json!({"fast": "yes"}))]
                .into_iter()
                .collect(),
        ] {
            let err = configure_settings_with(
                &manager,
                Some(channel.as_ref()),
                "@a/kiro",
                None,
                None,
                Some(map),
            )
            .await
            .unwrap_err();
            assert!(
                err.contains("开关不属于当前插件") || err.contains("布尔值"),
                "got: {err}"
            );
        }
        assert!(seen.lock().unwrap().is_empty(), "校验失败不得打宿主");
    }

    /// 宿主拒绝设置更新：错误如实上报且不落盘。
    #[tokio::test]
    async fn configure_settings_reports_host_rejection_without_saving() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        let manager = manager_with_store(store.clone(), dir.path());
        let mut package = pkg("@a/kiro", &["kiro"]);
        package.enabled = true;
        storage::replace_all(&store, dir.path(), &[package]).unwrap();
        // 桩：每次调用回错误帧（确定性的拒绝路径，不依赖通道关闭竞态）
        let (channel, _seen, task) = stub_host_with(1, true);
        let err = configure_settings_with(
            &manager,
            Some(channel.as_ref()),
            "@a/kiro",
            None,
            None,
            Some(
                [("kiro".to_string(), json!({"fast": true}))]
                    .into_iter()
                    .collect(),
            ),
        )
        .await
        .unwrap_err();
        task.await.unwrap();
        assert!(err.contains("宿主拒绝设置更新"), "got: {err}");
        // 未落盘：providerOptions 仍为空
        let stored = storage::load_packages(&store);
        assert!(stored[0].provider_options.is_empty());
    }
}
