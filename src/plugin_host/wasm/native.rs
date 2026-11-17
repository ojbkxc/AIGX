// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 原生辅助程序 —— Wasm 插件经「显式授权」调用已安装的原生包。
//!
//! ## 为什么需要它
//!
//! Wasm 沙箱没有文件/进程/环境变量，某些上游能力只能由**原生插件包**提供
//! （例如本机密钥链、浏览器自动化）。上游的解法是「显式授权 + 包指纹钉死」：
//!
//! 1. Wasm 插件在 `wasm.json` 里申报 `helpers`（必须同时声明
//!    `native.helpers.v1`）
//! 2. 运维在管理面逐个授权「哪个 provider 的哪个 helper 由哪个包的哪个版本
//!    提供」，授权记录里带 `callerPack` / `callerDigest`（授权时 Wasm 包的
//!    指纹）与目标包的 `pack` / `version` / `digest`
//! 3. 运行期两侧指纹都必须与当前安装一致，否则报 `native_helper_not_authorized`
//!
//! ## AIGX 侧的注入面
//!
//! 包注册表、目标包调用、授权记录分别由 [`WasmHelperSource`] 提供 ——
//! integrator 用 `plugin_host::registry::PackRegistry` +
//! `plugin_host::manager` 实现它，wasm 模块不直接依赖这两处。

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::plugin_host::manager::PluginHostManager;
use crate::plugin_host::registry::PackRegistry;
use crate::storage::FileStore;

use super::context::Context;
use super::wire::WireError;

/// 一个已安装的包（管理面 / 授权校验用的最小形状）。
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PackInfo {
    pub id: String,
    pub version: String,
    pub sha256: String,
    pub enabled: bool,
    /// 该包声明的 provider（`loaded` 表示当前是否在跑）。
    pub providers: Vec<PackProviderInfo>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PackProviderInfo {
    pub id: String,
    pub loaded: bool,
}

/// 原生辅助程序的宿主侧能力（integrator 实现）。
// `async-trait` 宏给 async 方法推 `#[must_use]`，与 `Result` / `Future` 自带的
// must_use 重复（`clippy::double_must_use` 误报）。
#[allow(clippy::double_must_use)]
#[async_trait::async_trait]
pub trait WasmHelperSource: Send + Sync {
    /// 全部已安装包。
    fn packs(&self) -> Vec<PackInfo>;
    /// provider → 归属包 id（owners 表）。
    fn provider_pack(&self, provider: &str) -> Option<String>;
    /// 某 provider 是否由当前 Wasm 包接管（等价上游 `serves_provider`）。
    fn serves(&self, provider: &str) -> bool;
    /// 读全部授权记录（`{provider: {helper: grant}}`）。
    fn grants(&self) -> Value;
    /// 覆盖写全部授权记录。
    fn set_grants(&self, grants: Value) -> Result<(), String>;
    /// 向某个**非 Wasm** 包发起调用（原生包或脚本包）。
    async fn call_pack(
        &self,
        pack_id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String>;
    /// 串行化授权变更（避免并发写丢更新）。
    fn mutation_guard(&self) -> Arc<tokio::sync::Mutex<()>> {
        static GUARD: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
        GUARD
            .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

fn source_slot() -> &'static Mutex<Option<Arc<dyn WasmHelperSource>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<dyn WasmHelperSource>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// 注入辅助程序宿主（后注入覆盖先前值）。
pub fn install_source(source: Arc<dyn WasmHelperSource>) {
    *source_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(source);
}

fn source() -> Option<Arc<dyn WasmHelperSource>> {
    source_slot().lock().ok()?.clone()
}

/// **授权记录的 KV 键**。
///
/// 上游把授权直接存进配置文件的 `pluginNativeHelpers` 字段；AIGX 的
/// `AppConfig` 没有该键（扩配置字段要动 lead-only 的 config/main），
/// 因此落 `FileStore` 单键。语义逐字保持：整份 JSON 对象
/// `{provider: {helper: grant}}`，读改写由
/// [`WasmHelperSource::mutation_guard`] 串行化。
pub(crate) const KV_GRANTS: &str = "plugin:native_helpers";

/// [`WasmHelperSource`] 的 AIGX 实现 —— 包注册表 + 原生插件进程 + FileStore。
///
/// ## 数据源（与上游逐条对应）
///
/// | trait 方法 | 上游 | AIGX |
/// |---|---|---|
/// | `packs` | `PackRegistry::load()` | [`PackRegistry::list`]（+ 运行期 loaded） |
/// | `provider_pack` | `setup::owners` / `provider_pack` | [`PackRegistry::find_pack_for_provider`]（只认已启用包） |
/// | `serves` | `registry::serves`（adapter 在册） | wasm 宿主存活 ∨ 原生插件进程在跑 |
/// | `grants` / `set_grants` | config `pluginNativeHelpers` | FileStore [`KV_GRANTS`] |
/// | `call_pack` | `Host::call_coded` | [`PluginHostManager::process_for_pack`] + RPC |
///
/// ## 已知边界（如实）
///
/// `call_pack` 只覆盖**原生插件包**（每包一个子进程）。脚本包由单个 Bun
/// 宿主承载、`PackRegistry` 里也没有它们的元数据，因此不会出现在
/// `packs()` 的候选里 —— 上游把脚本宿主也当 helper 提供方，这是本移植
/// 的差异（报告里记录）。
pub struct RegistryHelperSource {
    registry: Arc<PackRegistry>,
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
}

impl RegistryHelperSource {
    /// 由 integrator 在 `main.rs` 构造并注入：
    ///
    /// ```ignore
    /// crate::plugin_host::wasm::native::install_source(
    ///     crate::plugin_host::wasm::native::RegistryHelperSource::new(
    ///         plugin_host.clone(), store.clone()),
    /// );
    /// ```
    pub fn new(manager: Arc<PluginHostManager>, store: Arc<FileStore>) -> Arc<Self> {
        let registry = manager.registry().clone();
        Arc::new(Self {
            registry,
            manager,
            store,
        })
    }

    /// provider 当前是否有活着的执行体（wasm 宿主或原生插件进程）。
    fn loaded(&self, provider: &str) -> bool {
        super::host_for_provider(provider).is_some_and(|host| host.ready() && !host.closing())
            || self.manager.process_for_provider(provider).is_some()
    }
}

/// 读全部授权记录（缺失 / 非对象返回空对象，与上游 `unwrap_or_else(json!({}))` 同口径）。
fn read_grants(store: &FileStore) -> Value {
    store
        .get::<Value>(KV_GRANTS)
        .ok()
        .flatten()
        .unwrap_or_else(|| json!({}))
}

/// 覆盖写全部授权记录（只接受 JSON 对象）。
fn write_grants(store: &FileStore, grants: &Value) -> Result<(), String> {
    if !grants.is_object() {
        return Err("原生辅助授权记录无效".into());
    }
    store
        .put(KV_GRANTS, grants)
        .map_err(|error| format!("保存原生辅助授权失败: {error}"))
}

/// 向一个插件进程发起带超时的 RPC（`native.helpers` / `native.call` 共用）。
///
/// 超时映射成可读文案而不是 `RpcError`：trait 的错误出口是 `String`，
/// 调用方（`snapshot` / `Context::native`）只需要「不可用」这一个结论。
async fn call_with_timeout(
    rpc: &crate::plugin_host::rpc::RpcChannel,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, String> {
    match tokio::time::timeout(timeout, rpc.call(method, params)).await {
        Err(_) => Err(format!(
            "原生辅助调用超时（{} ms 未应答）",
            timeout.as_millis()
        )),
        Ok(Err(error)) => Err(error.message),
        Ok(Ok(value)) => Ok(value),
    }
}

#[async_trait::async_trait]
impl WasmHelperSource for RegistryHelperSource {
    fn packs(&self) -> Vec<PackInfo> {
        self.registry
            .list()
            .into_iter()
            .map(|meta| PackInfo {
                id: meta.id,
                version: meta.version,
                sha256: meta.sha256.unwrap_or_default(),
                enabled: meta.enabled,
                providers: meta
                    .providers
                    .into_iter()
                    .map(|provider| PackProviderInfo {
                        loaded: self.loaded(&provider.id),
                        id: provider.id,
                    })
                    .collect(),
            })
            .collect()
    }

    fn provider_pack(&self, provider: &str) -> Option<String> {
        self.registry.find_pack_for_provider(provider)
    }

    fn serves(&self, provider: &str) -> bool {
        super::owns(provider) || self.manager.process_for_provider(provider).is_some()
    }

    fn grants(&self) -> Value {
        read_grants(&self.store)
    }

    fn set_grants(&self, grants: Value) -> Result<(), String> {
        write_grants(&self.store, &grants)
    }

    async fn call_pack(
        &self,
        pack_id: &str,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let process = self
            .manager
            .process_for_pack(pack_id)
            .ok_or_else(|| format!("原生插件包「{pack_id}」未运行"))?;
        call_with_timeout(&process.rpc, method, params, timeout).await
    }
}

/// 测试专用：清空注入的宿主（进程级 static，用例之间必须显式复位）。
#[cfg(test)]
pub(crate) fn reset_source_for_test() {
    *source_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
}

/// 测试专用：串行化「安装/复位全局 source」的用例（跨模块共享同一把锁）。
#[cfg(test)]
pub(crate) fn source_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    &LOCK
}

/// 一条 helper 声明（目标包自报）。
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Helper {
    pub id: String,
    pub operations: Vec<String>,
}

fn parse(value: Value) -> Result<Vec<Helper>, WireError> {
    let helpers: Vec<Helper> =
        serde_json::from_value(value).map_err(|_| super::errors::protocol())?;
    if helpers.len() > 32
        || helpers.iter().any(|helper| {
            helper.id.is_empty()
                || helper.id.len() > 80
                || helper.operations.len() > 32
                || helper.operations.is_empty()
                || helper
                    .operations
                    .iter()
                    .any(|operation| operation.is_empty() || operation.len() > 80)
        })
    {
        return Err(super::errors::protocol());
    }
    Ok(helpers)
}

/// 授权与候选的聚合视图（管理面 `/api/plugins/helpers` 的数据源）。
pub async fn snapshot(pack_id: Option<&str>) -> Value {
    let Some(source) = source() else {
        return json!({ "grants": {}, "requested": [], "candidates": [] });
    };
    let packs = source.packs();
    let all_grants = source.grants();
    let mut scoped_grants = serde_json::Map::new();
    let mut requested = Vec::new();
    for pack in packs
        .iter()
        .filter(|pack| pack_id.is_none_or(|id| pack.id == id))
    {
        // 该包接管了哪些 Wasm provider（manifest 已登记即视为 Wasm）
        let wasm_providers: Vec<_> = pack
            .providers
            .iter()
            .filter_map(|provider| {
                let manifest = super::manifest_for(&provider.id)?;
                Some((provider.id.clone(), manifest))
            })
            .collect();
        for (provider, manifest) in &wasm_providers {
            if source.provider_pack(provider).as_deref() != Some(pack.id.as_str()) {
                continue;
            }
            let available = source.serves(provider) && super::host_for_provider(provider).is_some();
            for id in &manifest.helpers {
                requested.push(json!({ "pack": pack.id, "provider": provider, "id": id,
                    "available": available, "callerDigest": pack.sha256 }));
            }
        }
        if let Some(providers) = all_grants.as_object() {
            for (provider, helpers) in providers {
                let Some(helpers) = helpers.as_object() else {
                    continue;
                };
                for (id, grant) in helpers {
                    if grant["callerPack"].as_str() != Some(pack.id.as_str()) {
                        continue;
                    }
                    scoped_grants
                        .entry(provider.clone())
                        .or_insert_with(|| json!({}))[id] = grant.clone();
                    if !requested
                        .iter()
                        .any(|item| item["provider"] == *provider && item["id"] == *id)
                    {
                        requested.push(json!({ "pack": pack.id, "provider": provider, "id": id,
                            "available": false, "callerDigest": pack.sha256 }));
                    }
                }
            }
        }
    }
    let wanted: HashSet<String> = requested
        .iter()
        .filter(|item| item["available"] == true)
        .filter_map(|item| item["id"].as_str().map(str::to_string))
        .collect();
    let mut candidates = Vec::new();
    if !wanted.is_empty() {
        let futures: Vec<_> = packs
            .iter()
            .filter(|pack| pack.enabled)
            .filter(|pack| {
                // 只有非 Wasm 的包能当提供方
                !pack
                    .providers
                    .iter()
                    .any(|provider| super::manifest_for(&provider.id).is_some())
            })
            .flat_map(|pack| {
                pack.providers
                    .iter()
                    .filter(|provider| provider.loaded)
                    .map(move |provider| (pack.clone(), provider.id.clone()))
            })
            .collect();
        // 小并发（≤4）拉每家的 helper 清单
        let mut handles = Vec::new();
        for (pack, provider) in futures {
            let source = source.clone();
            let wanted = wanted.clone();
            handles.push(tokio::spawn(async move {
                let result = source
                    .call_pack(
                        &pack.id,
                        "native.helpers",
                        json!({ "provider": provider }),
                        Duration::from_secs(5),
                    )
                    .await;
                let Ok(value) = result else { return Vec::new() };
                let Ok(helpers) = parse(value) else {
                    return Vec::new();
                };
                helpers
                    .into_iter()
                    .filter(|helper| wanted.contains(&helper.id))
                    .map(|helper| {
                        json!({ "pack": pack.id, "version": pack.version, "provider": provider,
                            "id": helper.id, "operations": helper.operations,
                            "digest": pack.sha256 })
                    })
                    .collect::<Vec<_>>()
            }));
        }
        for handle in handles {
            if let Ok(items) = handle.await {
                candidates.extend(items);
            }
        }
        candidates
            .sort_by_key(|item| format!("{}:{}:{}", item["pack"], item["provider"], item["id"]));
    }
    json!({ "grants": scoped_grants, "requested": requested, "candidates": candidates })
}

/// 授权 / 撤销一个 helper。
///
/// `selected` 为 `null` 时撤销；否则必须是 [`snapshot`] 里 `candidates` 的
/// 某一项原样（含 `pack` / `version` / `digest` / `operations`）。
pub async fn grant(
    provider: &str,
    helper: &str,
    selected: &Value,
    pack_id: Option<&str>,
) -> Result<Value, String> {
    let source = source().ok_or("原生辅助授权存储尚未就绪")?;
    let guard = source.mutation_guard();
    let _mutation = guard.lock().await;
    if provider.is_empty() || helper.is_empty() || provider.len() > 128 || helper.len() > 80 {
        return Err("插件或辅助能力标识无效".into());
    }
    let mut all = source.grants();
    if !all.is_object() {
        all = json!({});
    }
    if selected.is_null() {
        let grant = &all[provider][helper];
        if grant.is_null() {
            return Ok(json!({ "removed": false }));
        }
        if pack_id.is_some_and(|id| grant["callerPack"].as_str() != Some(id)) {
            return Err("不能修改其他插件的原生辅助授权".into());
        }
        all[provider]
            .as_object_mut()
            .ok_or("授权记录无效")?
            .remove(helper);
        source.set_grants(all)?;
        return Ok(json!({ "removed": true }));
    }
    let caller = super::host_for_provider(provider)
        .filter(|host| host.ready())
        .ok_or("Wasm 插件未加载")?;
    if !caller.manifest.helpers.iter().any(|id| id == helper) {
        return Err("插件没有申报该辅助能力".into());
    }
    let caller_pack = source.provider_pack(provider).ok_or("插件归属不可用")?;
    if pack_id.is_some_and(|id| id != caller_pack) {
        return Err("不能修改其他插件的原生辅助授权".into());
    }
    let available = snapshot(Some(&caller_pack)).await;
    let candidate = available["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|item| {
            item["pack"] == selected["pack"]
                && item["provider"] == selected["provider"]
                && item["id"] == helper
                && item["version"] == selected["version"]
                && item["digest"] == selected["digest"]
                && item["operations"] == selected["operations"]
        })
        .cloned()
        .ok_or("原生辅助程序未安装、未启用或不支持此能力")?;
    if !all[provider].is_object() {
        all[provider] = json!({});
    }
    let mut candidate = candidate;
    candidate["callerPack"] = json!(caller_pack);
    candidate["callerDigest"] = json!(source
        .packs()
        .into_iter()
        .find(|pack| pack.id == caller_pack)
        .map(|pack| pack.sha256));
    all[provider][helper] = candidate;
    source.set_grants(all)?;
    Ok(json!({ "granted": true }))
}

impl Context {
    /// `native.call` —— 经授权把调用转给原生包。
    pub(super) async fn native(&mut self, params: &Value) -> Result<Value, WireError> {
        self.permission("native.helpers.v1")?;
        let source = source().ok_or_else(|| super::wire::error("原生辅助宿主尚未就绪"))?;
        let helper = params["helper"].as_str().unwrap_or("");
        let operation = params["operation"].as_str().unwrap_or("");
        if !self.shared.manifest.helpers.iter().any(|id| id == helper)
            || params.to_string().len() > 256 * 1024
        {
            return Err(WireError::with_code(
                "原生辅助调用未申报或输入过大",
                "forbidden",
            ));
        }
        let provider = self.shared.manifest.provider.clone();
        let caller_pack = source.provider_pack(&provider);
        let caller_digest = caller_pack.as_ref().and_then(|id| {
            source
                .packs()
                .into_iter()
                .find(|pack| pack.id == *id)
                .map(|pack| pack.sha256)
        });
        let grant = source.grants()[&provider][helper].clone();
        if grant["callerPack"].as_str() != caller_pack.as_deref()
            || grant["callerDigest"].as_str() != caller_digest.as_deref()
        {
            return Err(WireError::with_code(
                "插件归属已变化，需要重新授权",
                "native_helper_not_authorized",
            ));
        }
        let pack_id = grant["pack"].as_str().ok_or_else(|| {
            WireError::with_code(
                "请先在当前插件的设置中授权原生辅助程序",
                "native_helper_not_authorized",
            )
        })?;
        let target_provider = grant["provider"]
            .as_str()
            .ok_or_else(super::errors::protocol)?;
        let pack = source
            .packs()
            .into_iter()
            .find(|pack| {
                pack.id == pack_id
                    && pack.enabled
                    && grant["version"].as_str() == Some(pack.version.as_str())
            })
            .ok_or_else(|| {
                WireError::with_code(
                    "原生辅助程序已更新或不可用，需要重新授权",
                    "native_helper_not_authorized",
                )
            })?;
        if grant["digest"].as_str() != Some(pack.sha256.as_str()) {
            return Err(WireError::with_code(
                "辅助程序包内容已变化，需要重新授权",
                "native_helper_not_authorized",
            ));
        }
        if source.provider_pack(target_provider).as_deref() != Some(pack.id.as_str()) {
            return Err(WireError::with_code("原生辅助授权目标已变化", "forbidden"));
        }
        let declared = parse(
            source
                .call_pack(
                    &pack.id,
                    "native.helpers",
                    json!({ "provider": target_provider }),
                    Duration::from_secs(5),
                )
                .await
                .map_err(super::wire::error)?,
        )?;
        if !declared
            .iter()
            .any(|item| item.id == helper && item.operations.iter().any(|item| item == operation))
            || !grant["operations"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| item.as_str() == Some(operation))
        {
            return Err(WireError::with_code("原生辅助操作未获授权", "forbidden"));
        }
        let result = source
            .call_pack(
                &pack.id,
                "native.call",
                json!({ "provider": target_provider, "caller": provider,
                    "helper": helper, "operation": operation,
                    "payload": params["payload"], "flow": self.flow }),
                Duration::from_secs(60),
            )
            .await
            .map_err(super::wire::error)?;
        if result.to_string().len() > 256 * 1024 {
            return Err(super::wire::error("原生辅助结果过大"));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn parses_and_validates_helpers() {
        let value = json!([{ "id": "h1", "operations": ["op"] }]);
        assert_eq!(parse(value).unwrap().len(), 1);
        assert!(parse(json!([{ "id": "", "operations": ["op"] }])).is_err());
        assert!(parse(json!([{ "id": "h", "operations": [] }])).is_err());
        assert!(parse(json!([{ "id": "h", "operations": ["x"], "extra": 1 }])).is_err());
    }

    #[test]
    fn pack_info_tolerates_missing_fields() {
        let info: PackInfo = serde_json::from_value(json!({ "id": "p" })).unwrap();
        assert_eq!(info.id, "p");
        assert!(info.providers.is_empty());
        assert!(!info.enabled);
    }

    #[tokio::test]
    async fn snapshot_without_source_is_empty() {
        let _guard = source_test_lock().lock().await;
        reset_source_for_test();
        let value = snapshot(None).await;
        assert!(value["requested"].as_array().unwrap().is_empty());
        assert!(value["candidates"].as_array().unwrap().is_empty());
    }

    /// `grants` / `set_grants` 的 FileStore 往返（跨实例读回，证落盘）。
    #[test]
    fn grants_roundtrip_through_file_store() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-native-grants-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = FileStore::new(dir.clone());
        assert_eq!(read_grants(&store), json!({}), "缺省为空对象");
        let grants = json!({
            "kiro": { "keychain": {
                "pack": "native-pack", "version": "1.0.0", "digest": "abc",
                "provider": "native-prov", "id": "keychain", "operations": ["read"],
                "callerPack": "wasm-pack", "callerDigest": "def"
            }}
        });
        write_grants(&store, &grants).unwrap();
        // 新建同目录实例读回（证明确实落盘）
        let reopened = FileStore::new(dir);
        assert_eq!(read_grants(&reopened), grants);
        // 非对象拒绝（反向对照：对象写成功，非对象必须失败）
        let error = write_grants(&store, &json!([1, 2])).unwrap_err();
        assert!(error.contains("无效"), "{error}");
        assert_eq!(read_grants(&store), grants, "失败写入不得破坏既有记录");
    }

    /// 超时出口：对面不应答时必须在 `timeout` 后返回可读错误（真跑 RPC 通道）。
    #[tokio::test]
    async fn call_with_timeout_times_out() {
        use crate::plugin_host::rpc::{NoopCallbacks, RpcChannel};
        use tokio::io::AsyncRead;

        type BoxReader = Box<dyn AsyncRead + Send + Unpin>;
        type BoxStream = Box<dyn tokio::io::AsyncWrite + Send + Unpin>;
        let (client_out, plugin_in) = tokio::io::duplex(1 << 16);
        let (plugin_out, client_in) = tokio::io::duplex(1 << 16);
        let rpc = RpcChannel::new(
            Box::new(client_in) as BoxReader,
            Box::new(client_out) as BoxStream,
            Arc::new(NoopCallbacks),
        );
        // 插件端持着两个句柄但**不应答**：模拟原生包卡死
        let _hold = (plugin_in, plugin_out);
        let started = std::time::Instant::now();
        let error = call_with_timeout(
            &rpc,
            "native.helpers",
            json!({}),
            Duration::from_millis(120),
        )
        .await
        .unwrap_err();
        assert!(error.contains("超时"), "{error}");
        assert!(
            started.elapsed() >= Duration::from_millis(100),
            "必须真的等满超时"
        );
        rpc.shutdown();
    }
}
