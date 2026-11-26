// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 插件沙箱 —— 无 WASI、每次调用独立实例、不暴露文件/进程/环境变量。
//!
//! ## 与上游的定位差异
//!
//! 上游把 Wasm 当成**第四种插件形态**（原生包 / 脚本插件 / Wasm / 内置），
//! 宿主在 `plugin_host::Host` 这个巨型结构里按 `options.wasm` 分派。AIGX 的
//! `plugin_host` 是「每包一个子进程」，没有那样的 Host 聚合体，因此本模块
//! 自带一个**独立、自洽**的宿主：
//!
//! - [`WasmHost`] —— 一个 Wasm 包的运行时（加载 → init 握手 → 调用 → 关停）
//! - [`WasmOptions`] —— 启动参数（清单 + 模块路径 + 注入的宿主能力）
//! - [`access`] —— 宿主能力注入面（账号 / 出站 / 流出口三个 trait）
//! - [`tasks`] —— 插件声明的后台任务（快照 / 配置 / 执行）
//! - [`native`] —— 原生辅助程序的授权与调用
//!
//! ## 沙箱边界（与上游逐条一致）
//!
//! - 只允许导入 `agent2api.call` / `agent2api.read` 两个宿主函数；其余导入
//!   一律拒绝加载
//! - 每次调用新建 `Store` + 实例；内存上限来自 `wasm.json` 的 `memory_mib`
//! - 燃料计费 20 亿单位 + 每 10 万单位让出调度（`fuel_async_yield_interval`）
//! - 消息 ≤16 MiB、Wasm 文件 ≤16 MiB、编译串行（全局 1 个编译槽）
//! - 全局并发：流式 16、控制面 8（跨全部 Wasm 包）
//!
//! ## AIGX 侧的接线（由 integrator 在 T9 做）
//!
//! 本模块不碰 `plugin_host/mod.rs`、`manager.rs`、`main.rs`、`api/**`。接线点见
//! 交付报告；最小改动是 `plugin_host/mod.rs` 加一行 `pub(crate) mod wasm;`。

// 本模块的 dead_code 分三区标注，**不再用模块级 allow**。
//
// 为什么不能删干净：AIGX 是 lib+bin 双 target 共享 `src/*.rs` —— `lib.rs` 有
// `pub mod plugin_host`（公开树），`main.rs` 有 `mod plugin_host`（私有树，
// 独立编译单元）。`wasm::tasks` 的真实消费者只有 bin 的 `main.rs`
// （`install_store_settings` / `spawn` / `shutdown` 三行），而
// `cargo clippy --lib -D warnings`（T9 门禁第 5 步 / CI 必经关）只看公开树，
// **永远看不到 bin 的调用点**。这与模块可见性无关：改成 `pub mod` 也救不了
// （实测 32→28，tasks 侧一条不少）。
//
// 分区 allow 与模块级 allow 的实质区别：前者能被逐区证伪（删掉 `tasks` 的
// allow 只会报出 tasks 的条目），后者会连带掩盖真正的接线遗漏。
pub(crate) mod access;
pub(crate) mod account_access;
/// 账号来源授权（`http.open_account`）。
pub(crate) mod account_origin;
pub(crate) mod catalog;
mod context;
/// 本机客户端登录态读取（`desktop.credentials.read`，账号池「从桌面客户端导入」）。
///
/// 依赖 `rusqlite`（quwork / kiro-cli / kuku / doubao 的客户端库都是 SQLite），
/// 而 `rusqlite` 在 `sqlite-kv` feature 下才是可选依赖 —— 未启用的构建里这个
/// 模块整块不编，`desktop.credentials.read` 由 `services.rs` 如实回 `unsupported`，
/// 与仓库里其它 rusqlite 用法（`storage`、`account_login::chatglm`）同一口径。
#[cfg(feature = "sqlite-kv")]
pub(crate) mod desktop_credentials;
pub(crate) mod egress;
pub(crate) mod errors;
/// Kiro 外部 IdP 续期（`credential.kiro.external.refresh`）：插件注册时写进凭据的
/// `tokenURL` 只有插件知道，原生续期无从推断。
pub(crate) mod external_refresh;
#[cfg(test)]
pub(crate) mod fixture;
mod host;
mod http;
mod limits;
mod login;
/// 托管登录（`login.managed.v1`）：宿主签发会话 + 五个受控动作。
pub(crate) mod managed_login;
/// 受控任务审批闸门 + 任务生命周期（`managed_task.*`）。
pub(crate) mod managed_task;
pub(crate) mod manifest;
pub(crate) mod methods;
#[allow(dead_code)] // deferred：不注入时 native.snapshot 返回空表
pub(crate) mod native;
#[cfg(test)]
mod probe;
/// 协议转换（`protocol.convert.v1`）：只转换、不发网络请求，句柄归当前调用。
pub(crate) mod protocol;
mod public_view;
/// 插件侧公网图片下载（`resource.fetch_public`）：只产本次调用资源，不借账号身份。
pub(crate) mod resource_fetch;
mod resources;
mod runtime;
mod services;
mod state;
#[allow(dead_code)] // lib target 无调用方：唯一消费者是 bin 的 main.rs
pub(crate) mod tasks;
/// 宿主签发的验证租约（`verification.lease.open|check|close`）与状态 CAS 提交。
pub(crate) mod verification;
pub(crate) mod wire;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use tokio::sync::Semaphore;

use context::Sink;
use runtime::Runtime;

// 公开面：T9 由 integrator 在 `main.rs` / 数据面路由点使用。当前 crate 内
// 尚无调用方（wasm 宿主接线在 T9），故对未使用导入显式放行——否则
// `clippy -D warnings` 会因「pub use 但无人引用」失败。
#[allow(unused_imports)]
pub use access::{WasmAccountAccess, WasmEgress, WasmStreamSink};
#[allow(unused_imports)]
pub use account_access::AccountSessionAccess;
#[allow(unused_imports)]
pub use egress::{ChannelSink, TransportEgress};
#[allow(unused_imports)]
pub use host::{
    host_for_provider, host_providers, hosts, start, stop, stop_all, ProviderCapabilities,
    StreamHandle, WasmHost, WasmOptions, WasmProviderInfo,
};
#[allow(unused_imports)]
pub use manifest::{read_manifest, WasmManifest, ABI, CAPABILITIES, TARGET};
#[allow(unused_imports)]
pub use wire::WireError;

/// 单条消息上限（与上游 `MAX_MESSAGE` 一致）。
pub(crate) const MAX_MESSAGE: usize = 16 * 1024 * 1024;

/// 流式 credit 的每帧开销（与 `plugin_host::rpc::FRAME_OVERHEAD_BYTES` 同口径）。
pub(crate) const FRAME_OVERHEAD: usize = 2048;

/// 毫秒时间戳（替代上游 `crate::server::logging::now_ms`）。
pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 连接 / 资源 id（`net` / `blob` 前缀 + 随机后缀 + 进程内序号）。
pub(crate) fn net_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let mut nonce = [0u8; 8];
    let _ = getrandom::getrandom(&mut nonce);
    format!(
        "{}{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// 注入的账号访问面（`start()` 时登记；未注入时能力查询返回「未就绪」）。
fn accounts() -> &'static Mutex<Option<Arc<dyn WasmAccountAccess>>> {
    static ACCOUNTS: OnceLock<Mutex<Option<Arc<dyn WasmAccountAccess>>>> = OnceLock::new();
    ACCOUNTS.get_or_init(|| Mutex::new(None))
}

/// 注入账号访问面（幂等；重复注入保留首个）。
pub fn install_accounts(access: Arc<dyn WasmAccountAccess>) {
    if let Ok(mut slot) = accounts().lock() {
        if slot.is_none() {
            *slot = Some(access);
        }
    }
}

pub(crate) fn active_accounts() -> Option<Arc<dyn WasmAccountAccess>> {
    accounts().lock().ok()?.clone()
}

/// 一个 Wasm 包的运行时容器（= 上游 `wasm::Embedded`）。
pub(crate) struct Embedded {
    pub runtime: tokio::sync::RwLock<Option<Arc<Runtime>>>,
    pub streams: Mutex<HashMap<i64, Sink>>,
    pub idle: tokio::sync::Notify,
    pub active: std::sync::atomic::AtomicUsize,
}

impl Default for Embedded {
    fn default() -> Self {
        Self {
            runtime: tokio::sync::RwLock::new(None),
            streams: Mutex::new(HashMap::new()),
            idle: tokio::sync::Notify::new(),
            active: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

/// 可克隆的取消令牌（tokio-util 不是本仓直接依赖，这里最小实现同语义）。
///
/// ## 父子传播为什么**不**用后台任务
///
/// 旧实现给每个子令牌 `tokio::spawn` 一个「等父取消 → 置位」的常驻任务。
/// 父令牌是 `Shared::cancel`，只在宿主整个生命周期结束（`shutdown`）时才
/// 取消，而每次 `Runtime::invoke`（无 sink 的常规控制面调用）与每条
/// `open_stream` 都会 `child()` 一次 —— 于是每收一次调用就**永久多一个
/// 任务**并各自持着父令牌的 `Arc` 克隆：任务表与父令牌强引用计数随调用量
/// 无上限增长，直到停用/重载该包才一次性清零。这与「wasm store 泄漏」同一
/// 形态，只是漏的是任务与引用而不是 Store。
///
/// 现在子令牌直接持有父令牌，`cancelled()` 两级同时等、`is_cancelled()`
/// 逐级向上查：零任务、零常驻内存，语义（父取消 ⇒ 子取消）不变。
#[derive(Clone)]
pub(crate) struct Cancel(Arc<CancelInner>);

/// 手写 `Default`（不派生）：`CancelInner` 里嵌了 `Option<Cancel>` 的父指针，
/// 让令牌的默认构造保持显式（父 = `None`），不依赖派生链上的隐式取值。
impl Default for Cancel {
    fn default() -> Self {
        Self(Arc::new(CancelInner::default()))
    }
}

#[derive(Default)]
struct CancelInner {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
    /// 父令牌：父取消即子取消（直接持有，不起传播任务）。
    parent: Option<Cancel>,
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.flag.store(true, Ordering::Release);
        self.0.notify.notify_waiters();
    }

    /// 本令牌或任一祖先是否已取消。
    pub fn is_cancelled(&self) -> bool {
        self.0.flag.load(Ordering::Acquire)
            || self
                .0
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
    }

    /// 等待取消（已取消时立即返回）。
    ///
    /// 先建 `notified()` 再复查标志，避免「cancel 先到」丢信号；有父令牌时
    /// 把自己与父的等待一起 `select`——父取消时本令牌的 `flag` 不必被改写，
    /// 判定一律经 [`Self::is_cancelled`] 向上查。
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.0.notify.notified();
        if self.is_cancelled() {
            return;
        }
        match &self.0.parent {
            Some(parent) => {
                tokio::select! {
                    biased;
                    // 递归 async fn 必须 boxing（E0733）：父链判定要走同一套
                    // 「先建 notified 再复查 flag」的逻辑，所以直接复用父的
                    // cancelled()，而不是在这里展开父的 flag 快照（那会丢掉
                    // 「父在两次检查之间被取消」的窗口）。
                    _ = Box::pin(parent.cancelled()) => {}
                    _ = notified => {}
                }
            }
            None => notified.await,
        }
    }

    /// 子令牌：父取消会连带取消子（每次调用一个，避免跨调用误伤）。
    ///
    /// **不 spawn 传播任务**：子令牌持父令牌，判定与等待都走两级（见类型注释
    /// 里记的那次泄漏）。
    pub fn child(&self) -> Self {
        let child = Self(Arc::new(CancelInner {
            flag: AtomicBool::new(false),
            notify: tokio::sync::Notify::new(),
            parent: Some(self.clone()),
        }));
        if self.is_cancelled() {
            child.cancel();
        }
        child
    }
}

fn compile_slot() -> &'static Semaphore {
    static SLOT: OnceLock<Semaphore> = OnceLock::new();
    SLOT.get_or_init(|| Semaphore::new(1))
}

fn global_slots(streaming: bool) -> &'static Semaphore {
    static STREAMS: OnceLock<Semaphore> = OnceLock::new();
    static CONTROL: OnceLock<Semaphore> = OnceLock::new();
    if streaming {
        STREAMS.get_or_init(|| Semaphore::new(16))
    } else {
        CONTROL.get_or_init(|| Semaphore::new(8))
    }
}

/// 能力契约（`init` 的 params，插件据此校验宿主支持面）。
///
/// `capabilities` 只广告**宿主实际服务**的能力（[`manifest::SERVED_CAPABILITIES`]），
/// `declaredCapabilities` 是插件**可声明**的全集（[`manifest::CAPABILITIES`]），
/// 两者之差即「能装上但调用会如实报未支持」的能力。子系统键（compute /
/// scheduling / tasks / desktopLaunch / verification / managedLogin / sms /
/// accountOrigin / protocol）按 beta.2 形状给出，`served` 标识宿主是否真正
/// 提供该子系统的 host call。
pub(crate) fn contract() -> serde_json::Value {
    json!({
        "abi": manifest::ABI,
        "capabilities": manifest::SERVED_CAPABILITIES,
        "declaredCapabilities": manifest::CAPABILITIES,
        "limits": {
            "messageBytes": MAX_MESSAGE,
            "streamChunkBytes": 64 * 1024,
            "resourceBytes": 96 * 1024 * 1024,
            "accountStateBytes": 2 * 1024 * 1024,
            "conversationTtlMs": 86_400_000,
            "verificationMessageBytes": 256 * 1024,
            "verificationTimeoutMs": 2000,
            "verificationFuel": 50_000_000_u64,
            "verificationHostCalls": 128_u32
        },
        "compute": {
            "owner": "host",
            "fuel": 2_000_000_000_u64,
            "yieldInterval": 100_000_u64,
            "clock": "fuel"
        },
        "scheduling": {
            "owner": "host",
            "legacyConcurrency": "advisory",
            "classes": ["conversation", "interactive", "background", "image"],
            "global": { "streaming": 16, "control": 8 }
        },
        "state": {
            "login": "discard_on_reload",
            "conversation": "preserve_on_reload_reset_on_restart",
            "account": "encrypted_persistent_compare_exchange",
            "schema": "plugin_owned"
        },
        "settings": {
            "schema": "init.settings_schema",
            "group": "init.settings_group",
            "types": ["string", "number", "boolean", "select"],
            "placement": "host_owned",
            "values": "host_saved_overrides",
            "secrets": "account_store_only",
            "runtimeOverrides": "settings.overrides"
        },
        "images": "normalized_result_resources",
        "catalog": "account_snapshots_provider_union",
        "tasks": {
            "declarations": "tasks.list",
            "categories": "tasks.categories",
            "placement": "host_owned",
            "categoryValues": ["checkin", "growth", "rewards", "maintenance", "other"],
            "categoryGrantsPermissions": false,
            "accountScope": "current_provider_current_account",
            "served": manifest::served("tasks.v1")
        },
        "desktopLaunch": {
            "capability": "desktop.launch.v1",
            "served": manifest::served("desktop.launch.v1")
        },
        "managedTask": {
            "capability": "http.managed-task.v1",
            "methods": ["managed_task.create", "managed_task.observe", "managed_task.read", "managed_task.status", "managed_task.close"],
            "approval": "host_admin_then_fence",
            "executor": "plugin_performs_own_egress",
            "served": manifest::served("http.managed-task.v1")
        },
        "nativeHelpers": "explicit_grant_pinned_package",
        "network": "https_exact_origins_no_redirects",
        "accountOrigin": {
            "capability": "http.account-origin.v1",
            "method": "http.open_account",
            "authorization": "current_installed_runtime_current_account_routes",
            "source": "current_account_secret",
            "proxySupported": true,
            "served": manifest::served("http.account-origin.v1")
        },
        "protocol": {
            "capability": "protocol.convert.v1",
            "request": ["chat", "anthropic", "responses"],
            "stream": ["chat", "anthropic", "responses", "gemini"],
            "output": "chat_sse",
            "handles": "current_call_only",
            "served": manifest::served("protocol.convert.v1")
        },
        "verification": {
            "capability": "verification.account.v1",
            "claimCapability": "verification.claim.v1",
            "actions": ["captcha.push", "claim.verification.poll", "claim.verification.complete"],
            "lease": ["verification.lease.open", "verification.lease.check", "verification.lease.close"],
            "leaseMaxMs": 50_000_u64,
            "entry": "host_owned_account_call",
            "network": false,
            "concurrency": 4,
            "servedLease": manifest::served("verification.claim.v1"),
            "servedActions": manifest::served("verification.account.v1")
        },
        "managedLogin": {
            "capability": "login.managed.v1",
            "steps": "host_owned_compare_exchange",
            "methods": ["login.context", "login.input.take", "login.step.publish", "login.loopback.open", "login.loopback.close"],
            "browserModes": ["external"],
            "input": "current_trusted_call_once",
            "lifetime": "login_task",
            "served": manifest::served("login.managed.v1")
        },
        "sms": {
            "capability": "login.sms.v1",
            "methods": ["login.sms.send", "login.sms.verify"],
            "transport": "admin.action",
            "state": "host_owned_opaque_ticket",
            "sendStatus": ["sent", "captcha"],
            "served": manifest::served("login.sms.v1")
        }
    })
}

/// 管理面可读的能力契约（= `init` params + 已装载清单扩展声明）。
///
/// 与沙箱内部用的 [`contract`] 同一份数据；单独导出是因为 `contract` 是
/// `pub(crate)`，管理面在 `api::admin` 树里需要公开入口。
pub fn contract_public() -> serde_json::Value {
    contract()
}

/// 已加载的 Wasm 清单表（provider → 清单）。
///
/// `has_capability` / `owns` 是**同步**能力查询（数据面调用点不能 await），
/// 所以清单必须在 `start()` 时登记、关停时摘除，而不是每次现读磁盘。
fn manifests() -> &'static Mutex<HashMap<String, WasmManifest>> {
    static TABLE: OnceLock<Mutex<HashMap<String, WasmManifest>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register_manifest(manifest: WasmManifest) {
    if let Ok(mut table) = manifests().lock() {
        table.insert(manifest.provider.clone(), manifest);
    }
}

pub(crate) fn unregister_provider(provider: &str) {
    if let Ok(mut table) = manifests().lock() {
        table.remove(provider);
    }
}

/// 该 provider 是否由 Wasm 包接管。
///
/// **判据是「清单已登记 + 宿主仍存活且未在关闭中」**，不是「清单表里有条目」——
/// 后者在停用/卸载的瞬间到 `shutdown()` 摘除之间会短暂为真，管理面据此放行
/// 登录回调就会打到一个正在关闭的宿主上。两个条件同时满足才代表「真能用」。
pub fn owns(provider: &str) -> bool {
    let registered = manifests()
        .lock()
        .map(|table| table.contains_key(provider))
        .unwrap_or(false);
    registered && host::host_for_provider(provider).is_some_and(|host| !host.closing())
}

/// 同步能力查询。
///
/// 语义与 [`owns`] 同口径：**只有已加载且未在关闭中的宿主**才回答 `true`。
/// 停用 / 卸载 / 崩溃关停后一律 `false` —— 管理面的能力闸门（如登录回调）
/// 依赖这一点，否则会给一个不存在的插件放行。
pub fn has_capability(provider: &str, capability: &str) -> bool {
    let allowed = manifests()
        .lock()
        .ok()
        .and_then(|table| table.get(provider).map(|m| m.allows(capability)))
        .unwrap_or(false);
    allowed && owns(provider)
}

/// 该 provider 的清单快照（供管理面展示）。
pub fn manifest_for(provider: &str) -> Option<WasmManifest> {
    manifests().lock().ok()?.get(provider).cloned()
}

/// 已装载 Wasm 清单的只读快照（provider → 声明摘要）。
///
/// 管理面据此如实展示「本包是否声明了 beta.2 扩展能力
/// （desktopLaunch / managedLogin / managedTask / accountOrigin）」——
/// 声明了但 `served=false` 的能力即宿主未提供，界面标「未支持」。
pub fn declared_extensions() -> serde_json::Value {
    let table = manifests()
        .lock()
        .map(|table| table.clone())
        .unwrap_or_default();
    let mut rows: Vec<serde_json::Value> = table
        .into_values()
        .map(|manifest| {
            json!({
                "provider": manifest.provider,
                "verified": manifest.verified,
                "capabilities": manifest.requires,
                "accountOrigin": manifest.account_origin,
                "desktopLaunch": manifest.desktop_launch,
                "managedLogin": manifest.managed_login,
                "managedTask": manifest.managed_task,
                "concurrencyHardLimit": manifest.concurrency_hard_limit,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a["provider"]
            .as_str()
            .unwrap_or("")
            .cmp(b["provider"].as_str().unwrap_or(""))
    });
    serde_json::Value::Array(rows)
}

/// 摘除一个 provider 的全部临时状态（卸载 / 停用）。
pub(crate) fn forget(provider: &str) {
    tasks::forget(provider);
    state::forget_provider(provider);
    verification::forget_provider(provider);
    managed_login::forget_provider(provider);
    managed_task::forget_provider(provider);
    // 停用/卸载即撤销受控任务批准（fail-closed：旧批准不能续期）。
    managed_task::revoke(provider);
    catalog::forget(provider);
    // provider 维度的模型目录（`catalog` 并集的出口）同步摘除 ——
    // 宿主都没了，目录不能继续对外广告（= 上游 `rpc_cache::forget`）。
    crate::plugin_host::model_catalog::remove(provider);
    unregister_provider(provider);
}

/// 当前已加载清单的 provider 列表。
///
/// 当前无调用方：管理面的 provider 列表读 `PackRegistry`，本函数留给
/// 「已装载 wasm 宿主」的诊断视图。
#[allow(dead_code)]
pub fn loaded_providers() -> Vec<String> {
    let mut list: Vec<String> = manifests()
        .lock()
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default();
    list.sort();
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_propagates_to_child() {
        let parent = Cancel::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        child.cancelled().await;
        assert!(child.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_after_the_fact_is_immediate() {
        let cancel = Cancel::new();
        cancel.cancel();
        tokio::time::timeout(std::time::Duration::from_millis(100), cancel.cancelled())
            .await
            .expect("already-cancelled token must resolve immediately");
    }

    /// 计数型证明：`child()` 不得留下常驻任务，也不得残留对父令牌的引用。
    ///
    /// 旧实现每次 `child()` 都 `tokio::spawn` 一个「等父取消」的任务并持父
    /// 令牌的 `Arc` 克隆 —— 父令牌（`Shared::cancel`）只在宿主关停时取消，
    /// 于是每收一次 `Runtime::invoke` / 每开一条流就永久多一个任务与一份引用。
    /// 这里循环 100 次「建了就用完 drop」，父令牌强引用计数必须回到基线。
    #[tokio::test]
    async fn child_token_does_not_leak_parent_reference() {
        let parent = Cancel::new();
        let baseline = Arc::strong_count(&parent.0);
        for _ in 0..100 {
            let child = parent.child();
            assert!(!child.is_cancelled());
            drop(child);
        }
        assert_eq!(
            Arc::strong_count(&parent.0),
            baseline,
            "子令牌全部 drop 后父令牌强引用计数应回到基线（残留 = 传播任务泄漏）"
        );
    }

    /// 语义不变：父 → 子 → 孙 逐级取消，且孙令牌在不 spawn 传播任务的前提下
    /// 也能被祖先取消（旧实现靠常驻任务传播，新实现靠逐级查询）。
    #[tokio::test]
    async fn cancel_cascades_to_grandchild() {
        let parent = Cancel::new();
        let child = parent.child();
        let grandchild = child.child();
        assert!(!grandchild.is_cancelled());
        parent.cancel();
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            grandchild.cancelled(),
        )
        .await
        .expect("祖先取消必须传播到孙令牌");
        assert!(grandchild.is_cancelled());
    }

    /// 只登记清单、没有活宿主时：能力查询必须一律为假。
    ///
    /// 这是管理面能力闸门（`plugin_login.rs` 的 403 判断）的正确性来源 ——
    /// 停用 / 卸载 / 启动失败后不能残留 `true`。
    #[test]
    fn capability_lookup_requires_live_host() {
        let manifest = WasmManifest {
            provider: "unit-test-provider".into(),
            abi: ABI,
            requires: vec!["clock.v1".into()],
            origins: vec![],
            helpers: vec![],
            memory_mib: 64,
            concurrency: 8,
            verified: false,
            account_origin: None,
            desktop_launch: None,
            managed_login: None,
            managed_task: None,
            concurrency_hard_limit: None,
        };
        register_manifest(manifest);
        // 清单在，但没有活宿主 -> 不算接管、不放行任何能力
        assert!(manifest_for("unit-test-provider").is_some());
        assert!(!owns("unit-test-provider"));
        assert!(!has_capability("unit-test-provider", "clock.v1"));
        assert!(!has_capability("unit-test-provider", "http.v1"));
        assert!(!has_capability("nobody", "clock.v1"));
        unregister_provider("unit-test-provider");
        assert!(manifest_for("unit-test-provider").is_none());
        assert!(!owns("unit-test-provider"));
    }

    #[test]
    fn contract_advertises_abi_and_capabilities() {
        let value = contract();
        assert_eq!(value["abi"], ABI);
        // capabilities 只广告宿主实际服务的子集；declaredCapabilities 是全集。
        assert_eq!(
            value["capabilities"].as_array().map(Vec::len),
            Some(manifest::SERVED_CAPABILITIES.len())
        );
        assert_eq!(
            value["declaredCapabilities"].as_array().map(Vec::len),
            Some(CAPABILITIES.len())
        );
        assert_eq!(value["limits"]["messageBytes"], MAX_MESSAGE);
        // beta.2 子系统键存在（形状对齐）
        for key in [
            "compute",
            "scheduling",
            "tasks",
            "desktopLaunch",
            "verification",
            "managedLogin",
            "sms",
            "accountOrigin",
            "protocol",
        ] {
            assert!(value.get(key).is_some(), "contract 缺少子系统键: {key}");
        }
        // verification 租约是已服务能力
        assert_eq!(value["verification"]["servedLease"], true);
        // managedTask 广告上游方法集（旧别名 begin/end/abort 仍可分派，但不全列）。
        let methods = value["managedTask"]["methods"]
            .as_array()
            .expect("managedTask.methods 必须是数组");
        for method in [
            "managed_task.create",
            "managed_task.observe",
            "managed_task.read",
            "managed_task.status",
            "managed_task.close",
        ] {
            assert!(
                methods.iter().any(|item| item.as_str() == Some(method)),
                "contract 缺少 managed_task 方法: {method}"
            );
        }
    }
}
