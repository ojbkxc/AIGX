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
async fn snapshot(provider: &str) -> Option<Arc<Snapshot>> {
    let accounts = super::active_accounts()?;
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
pub(crate) async fn save(provider: &str, account: &str, value: Value) -> Result<usize, WireError> {
    let list = list(value)?;
    // 持闸门跨后续全部 await（见 `GATE` 的注释：这正是它必须是 tokio 锁的原因）。
    let _gate = GATE.lock().await;
    let accounts =
        super::active_accounts().ok_or_else(|| super::wire::error("账号存储尚未就绪"))?;
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
    rebuild(provider).await;
    Ok(count)
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
pub(crate) async fn rebuild(provider: &str) {
    if let Ok(mut table) = snapshots().lock() {
        table.remove(provider);
    }
    if let Some(snapshot) = snapshot(provider).await {
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

/// 账号记录上的模型权限判定（`known=false` 时返回 `None`）。
pub(crate) fn route_allowed(account: &Value, model: &str) -> Option<bool> {
    let provider = account["provider"].as_str()?;
    if !enabled(provider) || model.is_empty() {
        return None;
    }
    let permissions = &account["pluginModelPermissions"];
    if permissions["known"] != true {
        return None;
    }
    Some(
        permissions["models"]
            .as_array()?
            .iter()
            .any(|id| id.as_str().is_some_and(|id| id.eq_ignore_ascii_case(model))),
    )
}

/// 目录缓存判定某账号是否有该模型（缓存未命中返回 `None`）。
pub(crate) fn allowed(provider: &str, account: &str, model: &str) -> Option<bool> {
    if !enabled(provider) || model.is_empty() {
        return None;
    }
    let snapshot = cached_snapshot(provider)?;
    let list = snapshot.accounts.get(account)?;
    Some(list["models"].as_array()?.iter().any(|entry| {
        entry["id"]
            .as_str()
            .is_some_and(|id| id.eq_ignore_ascii_case(model))
    }))
}

/// 目录快照的调试视图（管理面展示用）。
pub(crate) fn snapshot_view(provider: &str) -> Value {
    match cached_snapshot(provider) {
        Some(snapshot) => json!({
            "revision": snapshot.revision,
            "accounts": snapshot.accounts.iter().map(|(id, list)| json!({
                "account": id,
                "refreshedAt": list["meta"]["refreshed_at"],
                "models": list["models"].as_array().map_or(0, Vec::len),
            })).collect::<Vec<_>>()
        }),
        None => Value::Null,
    }
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

    #[test]
    fn route_allowed_needs_known_permissions() {
        assert_eq!(route_allowed(&json!({}), "m"), None);
        let account = json!({ "provider": "p", "pluginModelPermissions": { "known": false } });
        assert_eq!(route_allowed(&account, "m"), None);
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
}
