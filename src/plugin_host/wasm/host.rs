// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 宿主门面 —— 加载 / 握手 / 调用 / 流式 / 关停。
//!
//! ## 与上游 `Host` 的关系
//!
//! 上游的 `Host` 是四类插件共用的大结构，wasm 只是其中一个分支
//! （`start_wasm` / `call_wasm` / `open_wasm_stream` / `shutdown_wasm`）。
//! AIGX 的 `plugin_host` 没有这个聚合体，所以这里把 wasm 分支的实现原样
//! 收进 [`WasmHost`]，对外只暴露四个入口：
//!
//! - [`start`] —— 建宿主、编译、`init` 握手、登记清单
//! - [`WasmHost::call`] —— 控制面调用（含登录流程/交互/回调）
//! - [`WasmHost::open_stream`] —— 流式对话
//! - [`WasmHost::shutdown`] —— 取消 + 排空在途调用 + 摘除清单
//!
//! ## 接线（integrator）
//!
//! 宿主注册表是进程级 `Mutex<HashMap<provider, Arc<WasmHost>>>`；`start()`
//! 写入、`shutdown()` 摘除。数据面路由点用 [`host_for_provider`] 取宿主
//! （同步），取到即代表「这个 provider 由 Wasm 包接管」。

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use super::context::{Shared, Sink};
use super::errors::decode as unwrap_reply;
use super::manifest::WasmManifest;
use super::runtime::Runtime;
use super::wire::WireError;
use super::{Embedded, WasmAccountAccess, WasmEgress, WasmStreamSink};

/// 流式 credit 窗口（与 `plugin_host::rpc::DEFAULT_WINDOW_BYTES` 同口径）。
const STREAM_WINDOW: usize = 512 * 1024;

/// 启动参数（= 上游 `HostOptions` 里 wasm 相关字段的合集）。
pub struct WasmOptions {
    /// 包目录（`pack.json` + `wasm.json` + `.wasm` 所在）。
    ///
    /// 上游在加载/诊断路径读它；AIGX 侧入口路径已单独存 `entry`，
    /// 本字段目前只作构造期的来源记录，无读取方。
    #[allow(dead_code)]
    pub dir: std::path::PathBuf,
    /// 已校验的权限清单。
    pub manifest: WasmManifest,
    /// Wasm 入口绝对路径。
    pub entry: std::path::PathBuf,
    /// `init` 握手超时。
    pub init_timeout: Duration,
    /// 账号访问面（必需）。
    pub accounts: Arc<dyn WasmAccountAccess>,
    /// 出站访问面（必需）。
    pub egress: Arc<dyn WasmEgress>,
    /// 模型目录出口（可选）。
    pub catalog: Option<Arc<dyn super::catalog::CatalogSink>>,
}

impl WasmOptions {
    /// 从包目录 + 包清单 JSON 构造（读完即校验）。
    pub fn from_pack(
        dir: &std::path::Path,
        pack: &Value,
        accounts: Arc<dyn WasmAccountAccess>,
        egress: Arc<dyn WasmEgress>,
    ) -> Result<Option<Self>, String> {
        let Some(manifest) = super::read_manifest(dir, pack)? else {
            return Ok(None);
        };
        let entry_rel = pack
            .get("entry")
            .and_then(Value::as_object)
            .and_then(|entry| entry.get(super::TARGET))
            .and_then(Value::as_str)
            .ok_or("Wasm 入口不存在")?;
        let entry = super::manifest::checked_entry(dir, entry_rel)?;
        Ok(Some(Self {
            dir: dir.to_path_buf(),
            manifest,
            entry,
            init_timeout: Duration::from_secs(30),
            accounts,
            egress,
            catalog: None,
        }))
    }
}

/// 插件自报的 provider 能力位（`init` 应答的一部分，与上游同形）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub chat: bool,
    pub model_refresh: bool,
    pub web_login: bool,
    pub anonymous_default_session: bool,
    pub env_credentials: bool,
}

/// `init` 应答里的 provider 条目。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WasmProviderInfo {
    pub id: String,
    pub label: String,
    pub capabilities: ProviderCapabilities,
}

/// 一个已启动的 Wasm 宿主。
pub struct WasmHost {
    pub manifest: WasmManifest,
    pub(crate) embedded: Embedded,
    pub shared: Arc<Shared>,
    /// 插件自报的 provider 信息（`init` 后填充）。
    providers: Mutex<Vec<WasmProviderInfo>>,
    /// 插件自报的设置 schema（`init` 后填充）。
    settings: Mutex<Value>,
    /// 关停中标志（拒绝新调用）。
    closing: AtomicBool,
    /// 就绪标志（`init` 成功）。
    ready: AtomicBool,
    /// 流 id 分配。
    next_id: AtomicI64,
    /// 代次（重载时自增，供上层观察）。
    generation: AtomicU64,
}

impl WasmHost {
    /// 插件自报的 provider 列表。
    pub fn providers(&self) -> Vec<WasmProviderInfo> {
        self.providers
            .lock()
            .map(|providers| providers.clone())
            .unwrap_or_default()
    }

    /// 插件自报的设置 schema（原样，未解析）。
    pub fn settings(&self) -> Value {
        self.settings
            .lock()
            .map(|settings| settings.clone())
            .unwrap_or(Value::Null)
    }

    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    pub fn closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// 控制面调用（`call` 语义，见上游 `call_wasm`）。
    pub async fn call(
        &self,
        method: &str,
        mut params: Value,
        timeout: Duration,
    ) -> Result<Value, WireError> {
        if self.closing() {
            return Err(super::wire::error("Wasm 插件已关闭"));
        }
        let runtime = self
            .embedded
            .runtime
            .read()
            .await
            .clone()
            .ok_or_else(|| super::wire::error("Wasm 插件尚未加载"))?;
        self.embedded.active.fetch_add(1, Ordering::AcqRel);
        let _active = Active(&self.embedded);
        let secret = params.get("secret").cloned();
        let cleanup = (method == "login.cancel").then(|| LoginCleanup {
            shared: runtime.shared.clone(),
            flow: params["flow"].as_str().unwrap_or("").to_string(),
        });
        let _cleanup = cleanup;
        if method == "login.start"
            && (runtime.shared.manifest.allows("login.interaction.v1")
                || runtime.shared.manifest.allows("login.callback.v1"))
        {
            let mut nonce = [0u8; 32];
            getrandom::getrandom(&mut nonce)
                .map_err(|_| super::wire::error("无法生成安全的登录流程标识"))?;
            params["flow"] = json!(format!(
                "login_{}",
                nonce
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            ));
            // 本机回调端口由 integrator 在注入时决定；未配置则不声明回调
        }
        let flow = params["flow"].as_str().unwrap_or("").to_string();
        let mut start_cleanup =
            (method == "login.start" && !flow.is_empty()).then(|| LoginCleanup {
                shared: runtime.shared.clone(),
                flow: flow.clone(),
            });
        if method == "login.interaction" {
            return Ok(runtime
                .shared
                .interactions
                .snapshot(&flow)
                .unwrap_or(Value::Null));
        }
        if matches!(method, "login.poll" | "login.callback" | "login.submit") {
            let proxies = runtime
                .shared
                .login_proxies
                .lock()
                .map_err(|_| super::wire::error("登录会话不可用"))?;
            let (_, proxy) = proxies
                .get(&flow)
                .filter(|(until, _)| *until > super::now_ms())
                .ok_or_else(|| super::wire::error("登录流程已取消或过期"))?;
            params["proxy"] = proxy.clone();
        }
        if method == "login.callback" && runtime.shared.manifest.allows("login.callback.v1") {
            let raw = params["url"].as_str().unwrap_or("");
            let url = url::Url::parse(raw)
                .or_else(|_| url::Url::parse(&format!("http://127.0.0.1{raw}")))
                .map_err(|_| super::wire::error("登录回调地址无效"))?;
            let states: Vec<_> = url
                .query_pairs()
                .filter(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned())
                .collect();
            if states.len() != 1 || states[0] != flow {
                return Err(super::wire::error("登录回调不属于当前流程"));
            }
        }
        if method == "login.submit" {
            runtime.shared.interactions.check(&flow, &params)?;
        }
        let proxy = params.get("proxy").cloned().unwrap_or(Value::Null);
        let submitted = (method == "login.submit").then(|| params.clone());
        let reply = runtime.invoke(method, params, timeout, None).await?;
        let result = unwrap_reply(reply)?;
        if let Some(submitted) = submitted {
            runtime.shared.interactions.accepted(&flow, &submitted);
        }
        if method == "login.start" {
            if (runtime.shared.manifest.allows("login.interaction.v1")
                || runtime.shared.manifest.allows("login.callback.v1"))
                && result["flow"].as_str() != Some(flow.as_str())
            {
                return Err(super::wire::error("交互式登录必须使用主程序分配的流程标识"));
            }
            let flow = result["flow"]
                .as_str()
                .filter(|flow| !flow.is_empty() && flow.len() <= 256)
                .ok_or_else(|| super::wire::error("插件登录标识无效"))?;
            if start_cleanup.is_none() {
                start_cleanup = Some(LoginCleanup {
                    shared: runtime.shared.clone(),
                    flow: flow.to_string(),
                });
            }
            let now = super::now_ms();
            let mut proxies = runtime
                .shared
                .login_proxies
                .lock()
                .map_err(|_| super::wire::error("登录会话不可用"))?;
            for expired in proxies
                .iter()
                .filter(|(_, (until, _))| *until <= now)
                .map(|(flow, _)| flow.clone())
                .collect::<Vec<_>>()
            {
                runtime.shared.interactions.remove(&expired);
                if let Ok(mut state) = runtime.shared.login_state.lock() {
                    state.remove_namespace(&format!(
                        "{}:{}",
                        runtime.shared.manifest.provider, expired
                    ));
                }
            }
            proxies.retain(|_, (until, _)| *until > now);
            if proxies.len() >= 64 {
                return Err(super::wire::error("登录会话数量超过上限"));
            }
            proxies.insert(flow.to_string(), (now + 900_000, proxy));
        }
        if matches!(
            method,
            "login.start" | "login.poll" | "login.callback" | "login.submit"
        ) {
            let current_flow = if method == "login.start" {
                result["flow"].as_str().unwrap_or("")
            } else {
                &flow
            };
            runtime.shared.interactions.remember(
                current_flow,
                &result,
                &runtime.shared.manifest,
            )?;
        }
        match method {
            super::methods::gateway::ACCOUNT_VIEW => {
                super::public_view::validate(&result, &secret.unwrap_or(Value::Null))?
            }
            super::methods::gateway::ACCOUNT_NORMALIZE => {
                validate_identity(&runtime.shared.manifest, &result)?;
                super::public_view::validate(&result["public"], &result["secret"])?;
            }
            super::methods::gateway::LOGIN_POLL
            | super::methods::gateway::LOGIN_CALLBACK
            | "login.submit"
                if result["account"].is_object() =>
            {
                validate_identity(&runtime.shared.manifest, &result["account"])?;
                super::public_view::validate(
                    &result["account"]["public"],
                    &result["account"]["secret"],
                )?;
            }
            super::methods::gateway::LOGIN_POLL
            | super::methods::gateway::LOGIN_CALLBACK
            | "login.submit" => {}
            _ => {}
        }
        if let Some(cleanup) = &mut start_cleanup {
            cleanup.flow.clear();
        }
        Ok(result)
    }

    /// 打开流式对话（见上游 `open_wasm_stream`）。
    pub async fn open_stream(
        self: &Arc<Self>,
        params: Value,
        out: Arc<dyn WasmStreamSink>,
    ) -> Result<StreamHandle, String> {
        if self.closing() {
            return Err("Wasm 插件已关闭".into());
        }
        let runtime = self
            .embedded
            .runtime
            .read()
            .await
            .clone()
            .ok_or("Wasm 插件尚未加载")?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let sink = Sink {
            out,
            credit: Arc::new(Semaphore::new(STREAM_WINDOW)),
            cancel: runtime.shared.cancel.child(),
        };
        {
            self.embedded
                .streams
                .lock()
                .map_err(|_| "Wasm 流式登记不可用")?
                .insert(id, sink.clone());
            self.embedded.active.fetch_add(1, Ordering::AcqRel);
        }
        let host = self.clone();
        let task_sink = sink.clone();
        tokio::spawn(async move {
            let _active = Active(&host.embedded);
            let reply = runtime
                .invoke(
                    super::methods::gateway::CONVERSATION,
                    params,
                    Duration::from_secs(3600),
                    Some(task_sink.clone()),
                )
                .await
                .and_then(unwrap_reply);
            match reply {
                Ok(_) => {
                    // 正常收尾：正文已全部经 sink 交付，显式写终点。
                    task_sink.out.end();
                }
                Err(error) => {
                    // 错误必须显式交给 sink —— 消费方（bridge）据 `take_error()`
                    // 做 failover。静默关闭会被读成「插件返回空回复」。
                    task_sink.out.fail(error);
                }
            }
            task_sink.credit.close();
            if let Ok(mut streams) = host.embedded.streams.lock() {
                streams.remove(&id);
            }
        });
        Ok(StreamHandle { id })
    }

    /// 取消一条流（`stream.abort`）。
    pub fn abort_stream(&self, id: i64) -> bool {
        let streams = self
            .embedded
            .streams
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(sink) = streams.get(&id) {
            sink.cancel.cancel();
            sink.credit.close();
            true
        } else {
            false
        }
    }

    /// 归还流式 credit（消费方读完一段后调用）。
    pub fn credit_stream(&self, id: i64, count: usize) {
        let streams = self
            .embedded
            .streams
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(sink) = streams.get(&id) {
            let available = sink.credit.available_permits();
            sink.credit
                .add_permits(count.min(STREAM_WINDOW.saturating_sub(available)));
        }
    }

    /// 关停：取消全部调用，等在途排空，摘除运行时与清单。
    pub async fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.ready.store(false, Ordering::Release);
        if let Some(runtime) = self.embedded.runtime.read().await.as_ref() {
            runtime.shared.cancel.cancel();
            runtime.shared.slots.close();
            runtime.shared.control_slots.close();
        }
        loop {
            let idle = self.embedded.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.embedded.active.load(Ordering::Acquire) == 0 {
                break;
            }
            idle.await;
        }
        self.embedded.runtime.write().await.take();
        super::forget(&self.manifest.provider);
    }
}

/// 流句柄（消费方按 id 取消 / 归还 credit）。
#[derive(Clone, Copy, Debug)]
pub struct StreamHandle {
    pub id: i64,
}

fn validate_identity(manifest: &WasmManifest, account: &Value) -> Result<(), WireError> {
    if manifest.allows("accounts.identity.v1")
        && account["identity"].as_str().is_none_or(|identity| {
            identity.trim().is_empty() || identity.len() > 512 || identity.contains('\0')
        })
    {
        return Err(super::wire::error("插件必须返回稳定、非空的账号身份"));
    }
    Ok(())
}

struct LoginCleanup {
    shared: Arc<Shared>,
    flow: String,
}

impl Drop for LoginCleanup {
    fn drop(&mut self) {
        if self.flow.is_empty() {
            return;
        }
        self.shared.interactions.remove(&self.flow);
        if let Ok(mut state) = self.shared.login_state.lock() {
            state.remove_namespace(&format!("{}:{}", self.shared.manifest.provider, self.flow));
        }
        if let Ok(mut proxies) = self.shared.login_proxies.lock() {
            proxies.remove(&self.flow);
        }
    }
}

struct Active<'a>(&'a Embedded);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
        self.0.idle.notify_waiters();
    }
}

/// 宿主注册表（provider → 宿主）。
fn hosts_table() -> &'static Mutex<std::collections::HashMap<String, Arc<WasmHost>>> {
    static TABLE: OnceLock<Mutex<std::collections::HashMap<String, Arc<WasmHost>>>> =
        OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// 按 provider 取已启动的宿主（数据面同步入口）。
pub fn host_for_provider(provider: &str) -> Option<Arc<WasmHost>> {
    hosts_table().lock().ok()?.get(provider).cloned()
}

/// 全部已启动宿主（管理面快照）。
///
/// 当前无调用方：管理面读的是 `manager::runtime_of` 的运行期槽位，
/// 本函数留给将来的「宿主全景」诊断页。
#[allow(dead_code)]
pub fn hosts() -> Vec<Arc<WasmHost>> {
    hosts_table()
        .lock()
        .map(|table| table.values().cloned().collect())
        .unwrap_or_default()
}

/// 启动一个 Wasm 宿主：编译 → `init` 握手 → 登记清单与注册表。
pub async fn start(options: WasmOptions) -> Result<Arc<WasmHost>, String> {
    let provider = options.manifest.provider.clone();
    if host_for_provider(&provider).is_some() {
        return Err(format!("Wasm 提供商 {provider} 已启动"));
    }
    if let Some(sink) = options.catalog.clone() {
        super::catalog::install_sink(Some(sink));
    }
    let shared = Shared::new(
        options.manifest.clone(),
        options.accounts.clone(),
        options.egress.clone(),
    );
    let runtime = Runtime::load(options.entry.clone(), shared.clone()).await?;
    let host = Arc::new(WasmHost {
        manifest: options.manifest.clone(),
        embedded: Embedded::default(),
        shared: shared.clone(),
        providers: Mutex::new(Vec::new()),
        settings: Mutex::new(Value::Null),
        closing: AtomicBool::new(false),
        ready: AtomicBool::new(false),
        next_id: AtomicI64::new(1),
        generation: AtomicU64::new(1),
    });
    let reply = runtime
        .invoke(
            super::methods::gateway::INIT,
            super::contract(),
            options.init_timeout,
            None,
        )
        .await
        .map_err(|error| error.message)?;
    let value = unwrap_reply(reply).map_err(|error| error.message)?;
    let providers: Vec<WasmProviderInfo> = serde_json::from_value(value["providers"].clone())
        .or_else(|_| serde_json::from_value(value.clone()))
        .map_err(|_| "Wasm 握手返回无效提供商信息")?;
    if providers.len() != 1 || providers[0].id != provider {
        return Err("Wasm 提供商身份与安装清单不一致".to_string());
    }
    let provider_info = &providers[0];
    for (required, capability) in [
        (provider_info.capabilities.chat, "chat.stream.v1"),
        (provider_info.capabilities.model_refresh, "catalog.v1"),
        (provider_info.capabilities.web_login, "login.forms.v1"),
    ] {
        if required && !options.manifest.allows(capability) {
            return Err(format!("Wasm 提供商能力缺少授权声明：{capability}"));
        }
    }
    if provider_info.capabilities.anonymous_default_session
        || provider_info.capabilities.env_credentials
    {
        return Err("Wasm 不允许隐式本机凭证，请使用显式账号或已授权的原生辅助".into());
    }
    if !options.manifest.allows("accounts.cas.v1") && provider_info.capabilities.chat {
        return Err("Wasm 对话必须声明受控账号访问能力".into());
    }
    *host.embedded.runtime.write().await = Some(runtime);
    *host
        .providers
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = providers;
    *host
        .settings
        .lock()
        .unwrap_or_else(|error| error.into_inner()) =
        value.get("settings").cloned().unwrap_or(Value::Null);
    host.ready.store(true, Ordering::Release);
    super::register_manifest(options.manifest.clone());
    super::install_accounts(options.accounts.clone());
    hosts_table()
        .lock()
        .map_err(|_| "Wasm 宿主注册表不可用".to_string())?
        .insert(provider, host.clone());
    Ok(host)
}

/// 关停并摘除一个 provider 的宿主（幂等）。
pub async fn stop(provider: &str) -> bool {
    let host = hosts_table()
        .lock()
        .map(|mut table| table.remove(provider))
        .ok()
        .flatten();
    match host {
        Some(host) => {
            host.shutdown().await;
            true
        }
        None => {
            super::forget(provider);
            false
        }
    }
}

/// 全部关停（进程退出 / 热重载）。
pub async fn stop_all() {
    let list: Vec<String> = hosts_table()
        .lock()
        .map(|table| table.keys().cloned().collect())
        .unwrap_or_default();
    for provider in list {
        stop(&provider).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_capabilities_deserialize_camel_case() {
        let value = json!({ "chat": true, "modelRefresh": true,
            "anonymousDefaultSession": false, "envCredentials": false });
        let caps: ProviderCapabilities = serde_json::from_value(value).unwrap();
        assert!(caps.chat);
        assert!(caps.model_refresh);
        assert!(!caps.anonymous_default_session);
    }

    #[test]
    fn provider_info_tolerates_unknown_fields() {
        let value = json!({ "id": "kiro", "label": "Kiro", "extra": 1,
            "capabilities": { "chat": true } });
        let info: WasmProviderInfo = serde_json::from_value(value).unwrap();
        assert_eq!(info.id, "kiro");
        assert!(info.capabilities.chat);
        assert!(!info.capabilities.model_refresh);
    }

    #[test]
    fn identity_validation_only_when_capability_declared() {
        let manifest = WasmManifest {
            provider: "p".into(),
            abi: 1,
            requires: vec!["accounts.identity.v1".into()],
            origins: vec![],
            helpers: vec![],
            memory_mib: 64,
            concurrency: 8,
            verified: false,
        };
        assert!(validate_identity(&manifest, &json!({})).is_err());
        assert!(validate_identity(&manifest, &json!({ "identity": "  " })).is_err());
        assert!(validate_identity(&manifest, &json!({ "identity": "u-1" })).is_ok());
        let no_cap = WasmManifest {
            requires: vec![],
            ..manifest
        };
        assert!(validate_identity(&no_cap, &json!({})).is_ok());
    }
}
