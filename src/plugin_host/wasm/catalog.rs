// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件账号的模型目录 —— 每账号快照 + provider 维度并集。
//!
//! ## 与上游的差异（仅接线方式）
//!
//! 上游直接读 `account_store::plugin_accounts` 并把并集写进 `rpc_cache`。
//! AIGX 的账号存储走注入 trait（[`WasmAccountAccess`]），并集通过
//! [`CatalogSink`] 回推给网关的模型目录（由 integrator 实现，通常转发到
//! `crate::model` 的远程目录缓存）。未注入 sink 时并集只在本模块内可用，
//! 不影响插件行为。
//!
//! 快照读取是 async（账号存储的 `plugin_state` 是 async）；**同步**查询
//! （[`allowed`] / [`route_allowed`]）只读进程内缓存，缓存未命中返回 `None`
//! —— 上层据此触发一次刷新，而不是拿别的账号的目录顶替。

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::access::WasmAccountAccess;
use super::wire::WireError;

/// 账号状态里存模型目录的键（与上游同值）。
const KEY: &str = "host.catalog.v1";

/// 串行化目录写入的闸门。
///
/// **必须是 `tokio::sync::Mutex`**：`save()` 持闸门跨多个 `.await`
/// （读旧快照 → CAS 写 → 重建并集）。用 `std::sync::Mutex` 会在 await 点
/// 阻塞 tokio worker 线程（`clippy::await_holding_lock`），重了会死锁。
///
/// 选 `const_new` 而非 `OnceLock`：`tokio::sync::Mutex` 自带
/// `const fn new()`，静态初始化即可，省一层 `OnceLock` 探测。
static GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// 目录并集的出口。实现方把并集写进网关的模型目录。
pub trait CatalogSink: Send + Sync {
    /// `models` 是模型对象数组（上游 `ModelList.models` 同形）。
    fn publish(&self, provider: &str, models: Vec<Value>, refreshed_at: i64);
}

fn sink_slot() -> &'static Mutex<Option<Arc<dyn CatalogSink>>> {
    static SINK: OnceLock<Mutex<Option<Arc<dyn CatalogSink>>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

fn sink() -> Option<Arc<dyn CatalogSink>> {
    sink_slot().lock().ok()?.clone()
}

/// 注入目录出口（`start()` 时由 [`super::WasmOptions`] 带入；幂等，保留首个）。
pub fn install_sink(value: Option<Arc<dyn CatalogSink>>) {
    if let Ok(mut slot) = sink_slot().lock() {
        if slot.is_none() {
            *slot = value;
        }
    }
}

struct Snapshot {
    revision: u64,
    at: Instant,
    accounts: BTreeMap<String, Value>,
}

fn snapshots() -> &'static Mutex<HashMap<String, Arc<Snapshot>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Arc<Snapshot>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 缓存快照（未命中返回 `None`，不触发读取）。
fn cached_snapshot(provider: &str) -> Option<Arc<Snapshot>> {
    snapshots().lock().ok()?.get(provider).cloned()
}

/// 从账号存储重建快照（1 秒内复用缓存）。
async fn snapshot(accounts: &Arc<dyn WasmAccountAccess>, provider: &str) -> Option<Arc<Snapshot>> {
    if let Some(cached) =
        cached_snapshot(provider).filter(|cached| cached.at.elapsed() < Duration::from_secs(1))
    {
        return Some(cached);
    }
    let mut table = BTreeMap::new();
    for account in accounts.accounts_for_provider(provider) {
        let Some(id) = account["id"].as_str() else {
            continue;
        };
        let state = accounts.plugin_state(provider, id, KEY).await.ok()?;
        if let Ok(list) = list(state["value"].clone()) {
            table.insert(id.to_string(), list);
        }
    }
    let revision = cached_snapshot(provider)
        .as_ref()
        .map(|cached| cached.revision)
        .unwrap_or(0)
        + 1;
    let current = Arc::new(Snapshot {
        revision,
        at: Instant::now(),
        accounts: table,
    });
    snapshots()
        .lock()
        .ok()?
        .insert(provider.into(), current.clone());
    Some(current)
}

pub(crate) fn enabled(provider: &str) -> bool {
    super::has_capability(provider, "catalog.accounts.v1")
}

/// 读某账号的目录快照（`catalog.get` 在 `catalog.accounts.v1` 下的语义）。
pub(crate) async fn cached(provider: &str, account: &str) -> Result<Value, WireError> {
    let accounts =
        super::active_accounts().ok_or_else(|| super::wire::error("账号存储尚未就绪"))?;
    let state = accounts.plugin_state(provider, account, KEY).await?;
    Ok(state["value"].clone())
}

fn list(value: Value) -> Result<Value, WireError> {
    if value.to_string().len() > 256 * 1024 {
        return Err(super::wire::error("账号目录超过大小限制"));
    }
    if !value.is_object() {
        return Err(super::errors::protocol());
    }
    let meta = &value["meta"];
    if meta["remote"] != true
        || meta["refreshed_at"].as_i64().is_none_or(|at| at < 0)
        || value["models"].as_array().map_or(0, Vec::len) > 4096
        || value["models"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|model| {
                model["id"]
                    .as_str()
                    .is_none_or(|id| id.trim().is_empty() || id.len() > 512)
            })
    {
        return Err(super::errors::protocol());
    }
    Ok(value)
}

/// 保存一个账号的目录快照（CAS，旧快照不覆盖新快照）。
///
/// `accounts` 由调用方显式传入：管理面同步路径用 AppState 现搭的访问面，
/// 宿主内 `models.refresh` 由注入的宿主访问面转发 —— 不再依赖「本进程曾有
/// 一个 wasm 宿主启动过」这一进程级单例（脚本宿主部署也能落目录）。
pub(crate) async fn save(
    accounts: &Arc<dyn WasmAccountAccess>,
    provider: &str,
    account: &str,
    value: Value,
) -> Result<usize, WireError> {
    let list = list(value)?;
    // 持闸门跨后续全部 await（见 `GATE` 的注释：这正是它必须是 tokio 锁的原因）。
    let _gate = GATE.lock().await;
    let count = list["models"].as_array().map_or(0, Vec::len);
    let old = accounts.plugin_state(provider, account, KEY).await?;
    if old["value"]["meta"]["refreshed_at"]
        .as_i64()
        .is_some_and(|at| at > list["meta"]["refreshed_at"].as_i64().unwrap_or(0))
    {
        return Ok(old["value"]["models"].as_array().map_or(0, Vec::len));
    }
    let result = accounts
        .plugin_state_compare_exchange(
            provider,
            account,
            KEY,
            old["revision"].as_u64().unwrap_or(0),
            1,
            list,
        )
        .await?;
    if result["updated"] != true {
        return Err(super::wire::error("账号目录已被另一轮刷新，请重试"));
    }
    rebuild(accounts, provider).await;
    Ok(count)
}

/// 插件 `models.refresh` 应答落盘后的真实快照。
#[derive(Debug, Clone)]
pub(crate) struct SavedCatalog {
    /// 本轮写入（或保留的旧快照之上插件给出的）模型对象数组。
    pub models: Vec<Value>,
    /// 目录刷新时刻（unix 毫秒；插件未给则为 0）。
    pub refreshed_at: i64,
}

/// 解析插件 `models.refresh` 应答并落一个账号快照（= 上游 `refresh_models`
/// 的 wasm/脚本分支）。
///
/// 应答形状 = 上游 `RefreshOutcome` 与 `ModelList` 的合体：
/// `{refreshed: true, count: n, models: [...], meta: {remote: true, refreshed_at}}`。
/// `refreshed != true`（插件自己说没拿到在线目录）与 `count != models.len()`
/// （刷新结果与快照来自两次不同拉取）一律拒写 —— **不把旧清单当新目录**。
pub(crate) async fn save_refresh(
    accounts: &Arc<dyn WasmAccountAccess>,
    provider: &str,
    account: &str,
    reply: &Value,
) -> Result<SavedCatalog, WireError> {
    if reply["refreshed"] != true {
        let message = reply["message"]
            .as_str()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("插件未返回刷新后的模型目录");
        return Err(super::wire::error(message));
    }
    let models = reply["models"]
        .as_array()
        .cloned()
        .ok_or_else(super::errors::protocol)?;
    if reply["count"]
        .as_u64()
        .is_some_and(|count| count as usize != models.len())
    {
        return Err(super::wire::error("插件刷新结果与模型快照条数不一致"));
    }
    save(
        accounts,
        provider,
        account,
        json!({ "models": models, "meta": reply["meta"].clone() }),
    )
    .await?;
    Ok(SavedCatalog {
        models,
        refreshed_at: reply["meta"]["refreshed_at"].as_i64().unwrap_or(0),
    })
}

fn merge(target: &mut Value, source: &Value) {
    for key in [
        "supportsToolCall",
        "supportsImages",
        "supportsVideo",
        "supportsReasoning",
    ] {
        if source[key] == true {
            target[key] = Value::Bool(true);
        }
    }
    for key in ["maxInputTokens", "maxOutputTokens"] {
        if let Some(value) = source[key].as_u64() {
            if value > target[key].as_u64().unwrap_or(0) {
                target[key] = Value::from(value);
            }
        }
    }
    if let Some(levels) = source["reasoningEffortLevels"].as_array() {
        let mut combined = target["reasoningEffortLevels"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for level in levels {
            if !combined.contains(level) {
                combined.push(level.clone());
            }
        }
        target["reasoningEffortLevels"] = Value::Array(combined);
    }
}

/// provider 维度并集（同 id 合并能力位与上限）。
fn aggregate(snapshot: &Snapshot) -> (Vec<Value>, i64) {
    let mut models = BTreeMap::new();
    let mut at = 0;
    for list in snapshot.accounts.values() {
        at = at.max(list["meta"]["refreshed_at"].as_i64().unwrap_or(0));
        for model in list["models"].as_array().into_iter().flatten() {
            if let Some(id) = model["id"].as_str() {
                let entry = models
                    .entry(id.to_ascii_lowercase())
                    .or_insert_with(|| model.clone());
                merge(entry, model);
            }
        }
    }
    (models.into_values().collect(), at)
}

/// 重算并回推并集。
pub(crate) async fn rebuild(accounts: &Arc<dyn WasmAccountAccess>, provider: &str) {
    if let Ok(mut table) = snapshots().lock() {
        table.remove(provider);
    }
    if let Some(snapshot) = snapshot(accounts, provider).await {
        let (models, at) = aggregate(&snapshot);
        if let Some(sink) = sink() {
            sink.publish(provider, models, at);
        }
    }
}

/// 卸载时清缓存。
pub(crate) fn forget(provider: &str) {
    if let Ok(mut table) = snapshots().lock() {
        table.remove(provider);
    }
}

/// 选号权限门：账号目录快照是否**明确**包含该模型（`None` = 无证据，放行）。
///
/// 入参是账号记录上的 `plugin_state`（`{key: {revision, schema, value}}`，
/// `value` 形状与 [`save`] 写入的一致）。与上游 `route_allowed` 的差异仅在
/// 证据来源：上游读账号列表上的 `pluginModelPermissions{known, models}`，
/// AIGX 的账号记录没有那个字段 —— 改用插件自己刷新出来的目录快照。
///
/// 三态（与「未知一律放行」同口径，见 [`crate::account_session::catalog`]
/// 的能力索引文档）：
/// - 有 `meta.remote = true` 的目录且含该模型 → `Some(true)`；
/// - 有目录且明确不含 → `Some(false)`（**有证据的拒否**才裁号）；
/// - 无状态槽 / 无目录 / 非 remote / 形状不合法 / 模型名为空 → `None`。
pub(crate) fn route_allowed(plugin_state: Option<&Value>, model: &str) -> Option<bool> {
    if model.is_empty() {
        return None;
    }
    let value = plugin_state?.get(KEY)?.get("value")?;
    if value["meta"]["remote"] != true {
        return None;
    }
    Some(value["models"].as_array()?.iter().any(|entry| {
        entry["id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(model))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_list_shape() {
        assert!(list(json!({})).is_err());
        assert!(
            list(json!({ "meta": { "remote": false, "refreshed_at": 1 }, "models": [] })).is_err()
        );
        assert!(
            list(json!({ "meta": { "remote": true, "refreshed_at": -1 }, "models": [] })).is_err()
        );
        assert!(list(json!({ "meta": { "remote": true, "refreshed_at": 1 },
            "models": [{ "id": " " }] }))
        .is_err());
        assert!(list(json!({ "meta": { "remote": true, "refreshed_at": 1 },
            "models": [{ "id": "gpt-x" }] }))
        .is_ok());
    }

    #[test]
    fn merge_takes_maxima_and_unions() {
        let mut target = json!({ "id": "m", "maxInputTokens": 100,
            "supportsImages": false, "reasoningEffortLevels": ["low"] });
        let source = json!({ "id": "m", "maxInputTokens": 200, "maxOutputTokens": 50,
            "supportsImages": true, "reasoningEffortLevels": ["low", "high"] });
        merge(&mut target, &source);
        assert_eq!(target["maxInputTokens"], 200);
        assert_eq!(target["maxOutputTokens"], 50);
        assert_eq!(target["supportsImages"], true);
        assert_eq!(target["reasoningEffortLevels"].as_array().unwrap().len(), 2);
    }

    /// 选号权限门的证据来自账号目录状态槽（上游同名函数读的是
    /// `pluginModelPermissions`；AIGX 账号记录没有那个字段，改用插件自己
    /// 刷新出来的 `host.catalog.v1` 快照 —— 只有「有目录且明确不含」才裁号）。
    #[test]
    fn route_allowed_reads_catalog_slot() {
        assert_eq!(route_allowed(None, "m"), None);
        assert_eq!(route_allowed(Some(&json!({})), "m"), None);
        let mut slot = serde_json::Map::new();
        slot.insert(
            KEY.to_string(),
            json!({
                "revision": 1,
                "schema": 1,
                "value": {
                    "models": [{ "id": "M-1" }],
                    "meta": { "remote": true, "refreshed_at": 10 }
                }
            }),
        );
        assert_eq!(route_allowed(Some(&json!(slot)), "m-1"), Some(true));
        assert_eq!(route_allowed(Some(&json!(slot)), "m-2"), Some(false));
        // 空模型名 / 空目录：无证据或明确不含
        assert_eq!(route_allowed(Some(&json!(slot)), ""), None);
        let mut empty = serde_json::Map::new();
        empty.insert(
            KEY.to_string(),
            json!({ "value": { "models": [], "meta": { "remote": true, "refreshed_at": 1 } } }),
        );
        assert_eq!(route_allowed(Some(&json!(empty)), "m"), Some(false));
        // 非 remote（静态兜底 / 本地目录）→ 无证据，放行
        let mut stale = serde_json::Map::new();
        stale.insert(
            KEY.to_string(),
            json!({ "value": { "models": [{ "id": "m" }], "meta": { "remote": false } } }),
        );
        assert_eq!(route_allowed(Some(&json!(stale)), "m"), None);
    }

    #[test]
    fn aggregate_unions_accounts() {
        let snapshot = Snapshot {
            revision: 1,
            at: Instant::now(),
            accounts: BTreeMap::from([
                (
                    "a".to_string(),
                    json!({ "meta": { "refreshed_at": 10 },
                        "models": [{ "id": "M", "maxInputTokens": 100 }] }),
                ),
                (
                    "b".to_string(),
                    json!({ "meta": { "refreshed_at": 20 },
                        "models": [{ "id": "m", "maxInputTokens": 300, "supportsImages": true }] }),
                ),
            ]),
        };
        let (models, at) = aggregate(&snapshot);
        assert_eq!(at, 20);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["maxInputTokens"], 300);
        assert_eq!(models[0]["supportsImages"], true);
    }

    /// 与 `wasm::account_access` 测试同手法：编码路径走全局 data_dir，
    /// 拿跨模块共享锁设独立目录，防并行测试互相换密钥。
    #[must_use = "守卫必须活到测试结束；丢弃会立刻释放全局锁，隔离失效"]
    fn with_secret_dir(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-wasm-catalog-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        guard
    }

    fn harness() -> (
        Arc<crate::account_session::AccountSessionStore>,
        Arc<dyn WasmAccountAccess>,
    ) {
        let store = Arc::new(crate::storage::FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let sessions = Arc::new(crate::account_session::AccountSessionStore::new(
            store.clone(),
        ));
        let access = crate::plugin_host::wasm::AccountSessionAccess::new(
            "catalog-test",
            sessions.clone(),
            store,
        );
        (sessions, access)
    }

    fn account(id: &str, provider: &str) -> crate::account_session::model::AccountSession {
        let mut session = crate::account_session::model::AccountSession::new("ch-cat", id);
        session.id = id.to_string();
        session.provider = provider.to_string();
        session.credential.secret = format!("sk-{id}");
        session
    }

    fn reply(id: &str, refreshed_at: i64) -> Value {
        json!({
            "refreshed": true,
            "count": 1,
            "models": [{ "id": id }],
            "meta": { "remote": true, "refreshed_at": refreshed_at },
        })
    }

    /// `models.refresh` 应答 → 账号快照 → 并集：第二个账号加入后 union 增长，
    /// 时刻取各账号最大值；旧时刻应答不覆盖新快照（CAS 语义）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn refresh_writes_snapshot_and_grows_union() {
        let _guard = with_secret_dir("refresh");
        let (sessions, access) = harness();
        let provider = "unit-catalog-refresh";
        let _ = sessions.upsert(account("a1", provider));
        let _ = sessions.upsert(account("a2", provider));
        let saved = save_refresh(&access, provider, "a1", &reply("m-one", 1_000))
            .await
            .unwrap();
        assert_eq!(saved.models.len(), 1);
        assert_eq!(saved.refreshed_at, 1_000);
        let (models, at) = aggregate(&snapshot(&access, provider).await.unwrap());
        assert_eq!(models.len(), 1);
        assert_eq!(at, 1_000);
        save_refresh(&access, provider, "a2", &reply("m-two", 2_000))
            .await
            .unwrap();
        let (models, at) = aggregate(&snapshot(&access, provider).await.unwrap());
        assert_eq!(models.len(), 2);
        assert_eq!(at, 2_000);
        // 旧时刻应答不覆盖新快照（save 返回旧条数，账号状态保持 m-one）
        save_refresh(&access, provider, "a1", &reply("m-zero", 500))
            .await
            .unwrap();
        let state = access.plugin_state(provider, "a1", KEY).await.unwrap();
        assert_eq!(state["value"]["models"][0]["id"], "m-one");
        assert_eq!(state["value"]["meta"]["refreshed_at"], 1_000);
    }

    /// 插件明确「未刷新」或形状不符的应答一律拒写（不伪造目录）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn refresh_rejects_unconfirmed_and_malformed_replies() {
        let _guard = with_secret_dir("refresh-bad");
        let (sessions, access) = harness();
        let provider = "unit-catalog-bad";
        let _ = sessions.upsert(account("a1", provider));
        // refreshed=false → 稳定错误（带插件自述原因）
        let fell_back = json!({
            "refreshed": false,
            "count": 1,
            "models": [{ "id": "m" }],
            "meta": { "remote": true, "refreshed_at": 1 },
            "message": "在线目录未能确认",
        });
        let error = save_refresh(&access, provider, "a1", &fell_back)
            .await
            .unwrap_err();
        assert!(error.message.contains("在线目录未能确认"));
        // count 与 models 条数不一致 → 拒
        let mismatch = json!({
            "refreshed": true,
            "count": 2,
            "models": [{ "id": "m" }],
            "meta": { "remote": true, "refreshed_at": 1 },
        });
        assert!(save_refresh(&access, provider, "a1", &mismatch)
            .await
            .is_err());
        // meta.remote 非 true（本地/兜底目录）→ 目录形状不合法
        let local = json!({
            "refreshed": true,
            "count": 1,
            "models": [{ "id": "m" }],
            "meta": { "remote": false, "refreshed_at": 1 },
        });
        assert!(save_refresh(&access, provider, "a1", &local).await.is_err());
        // 任一失败都不落快照
        let state = access.plugin_state(provider, "a1", KEY).await.unwrap();
        assert!(state["value"].is_null());
    }
}
