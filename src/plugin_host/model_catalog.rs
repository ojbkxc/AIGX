// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件 provider 维度的模型目录（= 上游 `core/plugin_host/rpc_cache.rs`）。
//!
//! ## 数据来路
//!
//! `wasm::catalog` 把**每账号快照**并成 provider 维度的并集，经
//! [`wasm::catalog::CatalogSink::publish`] 回推；本模块是那个 sink 的实现：
//! 原样落 [`FileStore`]（键 `plugin_catalog:{provider}`），同时放进进程内
//! 缓存供**同步**读取（数据面的模型列表与管理视图都是同步函数）。
//!
//! ## 值形状（与上游 `proto::ModelList` 同形）
//!
//! ```json
//! { "models": [ { "id": "…", … } ], "meta": { "remote": true, "refreshed_at": 0 } }
//! ```
//!
//! - `remote` 恒为 `true`：只有「从账号存储算出来的并集」才走这里（静态
//!   兜底目录不经过本模块）。空目录**也照样存 `remote:true`** —— 它表达的是
//!   「刷新成功且这家真的没有模型」，不造默认数据。
//! - `refreshed_at` 取各账号 `meta.refreshed_at` 的最大值（unix 毫秒），
//!   非正数时回落 `wasm::now_ms()`（上游 `set_remote` 同口径）。
//!
//! ## 与上游的差异（仅接线方式）
//!
//! 上游把并集写进 `rpc_cache` 的 `Entry.remote`，`models_for` / `meta` /
//! `snapshot` 是同一张表的读取面；AIGX 没有 `rpc_cache`，本模块自带进程内表 +
//! FileStore 落盘，读取面形状不变。`settings.get("modelCatalog")` 由
//! `wasm::account_access::settings_get` 拦截到 [`snapshot`]（上游
//! `bootstrap.rs::settings_get` 同语义）。
//!
//! 写路径的存储由 [`install`] 装入（`manager::enable_wasm` 调用）——**不**在
//! 本模块里读全局 `data_dir`，也不另开目录：与 `main.rs` 其余 KV 消费者
//! 共用同一个 `FileStore` 实例。
//!
//! **读取面只认进程内表**：KV 里的上一份并集不会被自动复活 —— 否则「目录
//! 从未在本进程刷新过」会伪装成「目录是旧的但看起来正常」（上游
//! `restore` 的语义，本期未接；见 docs/a2a-alignment/README.md §六.1）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::storage::FileStore;

use super::wasm::catalog::CatalogSink;

/// KV 键前缀（provider 粒度）。
const KEY_PREFIX: &str = "plugin_catalog:";

/// `settings.get` 的保留键（= 上游 `rpc_cache::SETTING_MODEL_CATALOG`）。
///
/// 插件问这个键要的是「本 provider 的模型目录快照」，不是自己的设置；
/// `wasm::account_access` 在这里拦截，`provider_settings::schema` 也据此
/// 拒绝把它声明成可写字段（上游 `schema.rs:81-89` 同一组保留键）。
pub const SETTING_MODEL_CATALOG: &str = "modelCatalog";

/// 单条 provider 目录（内部表示）。
#[derive(Clone, Debug)]
struct Entry {
    /// 模型对象数组（`aggregate` 原样产物，不裁剪字段）。
    models: Vec<Value>,
    /// 并集最近刷新时刻（unix 毫秒）。
    refreshed_at: i64,
}

impl Entry {
    fn snapshot(&self) -> Value {
        json!({
            "models": self.models,
            "meta": { "remote": true, "refreshed_at": self.refreshed_at },
        })
    }
}

fn table() -> &'static Mutex<HashMap<String, Arc<Entry>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Arc<Entry>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程内缓存（未命中返回 `None`）。
fn cached(provider: &str) -> Option<Arc<Entry>> {
    table().lock().ok()?.get(provider).cloned()
}

fn key(provider: &str) -> String {
    format!("{KEY_PREFIX}{provider}")
}

/// 目录出口的落盘实现（`start()` 时经 `WasmOptions.catalog` 注入）。
///
/// 无状态：写路径统一走 [`set_remote`]（存储由 [`install`] 装入），这样
/// 「sink 收到的并集」与「`settings.get`/模型聚合读到的目录」必然是同一份。
pub struct FileCatalogSink;

impl FileCatalogSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl CatalogSink for FileCatalogSink {
    fn publish(&self, provider: &str, models: Vec<Value>, refreshed_at: i64) {
        set_remote(provider, models, refreshed_at);
    }
}

/// 进程级存储单例（`main.rs` 启动时装入；重复调用忽略）。
fn store_slot() -> &'static OnceLock<Arc<FileStore>> {
    static STORE: OnceLock<Arc<FileStore>> = OnceLock::new();
    &STORE
}

/// 装入落盘底座（`main.rs` 启动段调用；`enable_wasm` 里的调用是幂等兜底）。
///
/// **为什么必须在启动阶段装**：此前唯一调用点是 `manager::enable_wasm`，
/// 即「第一次启用某个 Wasm 插件」时才装 —— 从不启用插件的实例上
/// [`set_remote`] 只写内存，重启后目录全丢（静默，不报错）。
pub fn install(store: Arc<FileStore>) {
    let _ = store_slot().set(store);
}

/// 是否已装入存储（启动装配自检用）。
pub fn installed() -> bool {
    store_slot().get().is_some()
}

fn entry(models: Vec<Value>, refreshed_at: i64) -> Option<Arc<Entry>> {
    if models.len() > 4096 {
        return None;
    }
    Some(Arc::new(Entry {
        models,
        refreshed_at: if refreshed_at > 0 {
            refreshed_at
        } else {
            super::wasm::now_ms()
        },
    }))
}

/// 写入一个 provider 的目录（上游 `rpc_cache::set_remote` 语义）。
///
/// 空目录照存（见模块头）：`remote:true` 表达「刷新成功且确实没有模型」，
/// 与「从未刷新」（缓存未命中 / KV 无值）是两回事 —— 上层据此决定是否回落
/// 渠道声明。未装入存储时只留进程内缓存，读数语义不变。
pub fn set_remote(provider: &str, models: Vec<Value>, refreshed_at: i64) {
    if provider.is_empty() {
        return;
    }
    let Some(entry) = entry(models, refreshed_at) else {
        return;
    };
    if let Some(store) = store_slot().get() {
        // 落盘失败不影响本次读数（目录是可重建的展示数据，与
        // `account_session::catalog::save_cache` 同取向）。
        let _ = store.put(&key(provider), &entry.snapshot());
    }
    if let Ok(mut table) = table().lock() {
        table.insert(provider.to_string(), entry);
    }
}

/// 目录的模型对象数组（未命中返回 `None`；空目录返回 `Some(vec![])`）。
pub fn models_for(provider: &str) -> Option<Vec<Value>> {
    cached(provider).map(|entry| entry.models.clone())
}

/// 目录的模型 id 列表（模型聚合/管理视图用；未命中返回空）。
///
/// 「空目录」与「未命中」都返回空 —— 调用方据此回落渠道声明，两者回落方向
/// 一致；需要区分时用 [`models_for`] 的 `Option`。
pub fn model_ids(provider: &str) -> Vec<String> {
    models_for(provider)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model["id"].as_str())
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// 目录元信息 `(remote, refreshed_at)`；未命中回落 `(false, 0)`。
///
/// `remote=true` 只在「本进程算过并集」时为真 —— 与 [`models_for`] 同源。
pub fn meta(provider: &str) -> (bool, i64) {
    match cached(provider) {
        Some(entry) => (true, entry.refreshed_at),
        None => (false, 0),
    }
}

/// 完整快照 `{models, meta}`（未命中返回 `null`；= 上游 `rpc_cache::snapshot`）。
pub fn snapshot(provider: &str) -> Value {
    match cached(provider) {
        Some(entry) => entry.snapshot(),
        None => Value::Null,
    }
}

/// 摘除一个 provider 的目录（卸载 / 停用；= 上游 `rpc_cache::forget`）。
pub fn remove(provider: &str) {
    let had = cached(provider).is_some();
    if let Ok(mut table) = table().lock() {
        table.remove(provider);
    }
    if let (true, Some(store)) = (had, store_slot().get()) {
        let _ = store.delete(&key(provider));
    }
}

/// 插件渠道的模型来源：`(provider, models, refreshed_at)`。
///
/// `collect_available_models` / `model_owner_index` / `build_view` 三处共用
/// 这一份口径（否则客户 API 与控制台会分叉）：
/// - provider = `channel.plugin_provider`（空 = 渠道未配置，返回空模型）；
/// - 模型优先渠道声明（`effective_models`），留空回落该 provider 目录的
///   id 列表（目录未命中同样为空 —— **不编造**插件没给过的模型）；
/// - `refreshed_at` = 目录最近刷新时刻（无目录 = 0）。
pub fn plugin_channel_source(channel: &crate::channel::Channel) -> (String, Vec<String>, i64) {
    let provider = channel.plugin_provider.trim().to_string();
    if provider.is_empty() {
        return (provider, Vec::new(), 0);
    }
    let declared: Vec<String> = channel
        .effective_models()
        .iter()
        .filter(|model| !model.trim().is_empty())
        .cloned()
        .collect();
    if !declared.is_empty() {
        return (provider, declared, 0);
    }
    let (_, refreshed_at) = meta(&provider);
    let ids = model_ids(&provider);
    (provider, ids, refreshed_at)
}

/// 插件通知模型变化后，同一 provider 的多次通知合并为一次触发的时间窗。
pub const MODELS_CHANGED_DEBOUNCE: Duration = Duration::from_millis(500);

/// 去抖后真正执行刷新的触发器（`provider, account`）。
pub type ModelsChangedTrigger = Arc<dyn Fn(&str, &str) + Send + Sync>;

fn trigger_slot() -> &'static OnceLock<ModelsChangedTrigger> {
    static TRIGGER: OnceLock<ModelsChangedTrigger> = OnceLock::new();
    &TRIGGER
}

fn pending_slot() -> &'static Mutex<HashSet<String>> {
    static PENDING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// 注册 models_changed 的真实刷新触发器（启动装配调用；重复调用忽略）。
///
/// 触发器是上层把「通知」接到现有刷新入口（`models.refresh` /
/// `handle_sync_account_catalog`）的唯一接缝——本模块不持宿主句柄，不在
/// 同步回调里跨宿主调用，避免与 RPC 读循环互等。
#[allow(dead_code)] // 接线在启动装配（main.rs）；本模块先冻结入口
pub fn install_models_changed_trigger(trigger: ModelsChangedTrigger) {
    let _ = trigger_slot().set(trigger);
}

/// 插件 `notify{kind:"models_changed"}` → 去抖触发一次模型目录刷新。
///
/// 对齐上游 `bootstrap.rs`/`model_sync.rs`：上游收到通知后调
/// `model_sync::notify`；AIGX 的目录刷新入口在管理面（`models.refresh`），
/// 由 [`install_models_changed_trigger`] 装进来的触发器承接。同一 provider
/// 在 [`MODELS_CHANGED_DEBOUNCE`] 窗口内的重复通知只触发**一次**——插件在
/// 一次探测里常按账号连发多条。
pub fn notify_models_changed(provider: &str, account: &str) {
    notify_models_changed_after(provider, account, MODELS_CHANGED_DEBOUNCE);
}

/// 带窗口参数的入口（单测用短窗口；生产走 [`notify_models_changed`]）。
pub(crate) fn notify_models_changed_after(provider: &str, account: &str, window: Duration) {
    let provider = provider.trim();
    if provider.is_empty() {
        return;
    }
    let provider = provider.to_string();
    let account = account.trim().to_string();
    {
        // 已在窗口内排队 = 合并，不重复起任务。
        let mut pending = pending_slot()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !pending.insert(provider.clone()) {
            return;
        }
    }
    // 回调用独立任务：同步回调（`CallbackHandler::handle`）不能 await，
    // 也不该阻塞 RPC 读循环；去抖窗口结束后再触发一次。
    tokio::spawn(async move {
        tokio::time::sleep(window).await;
        pending_slot()
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&provider);
        match trigger_slot().get() {
            Some(trigger) => trigger(&provider, &account),
            None => {
                tracing::debug!("[plugin] models_changed: {provider}/{account}（未注册刷新触发器）")
            }
        }
    });
}

/// 测试共用存储：进程内只装一次，所有目录相关用例用它 + 唯一 provider 名，
/// 避免并行用例互相覆盖（`install` 是 OnceLock，晚到的实参会被忽略）。
#[cfg(test)]
pub(crate) fn test_store() -> Arc<FileStore> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static STORE: OnceLock<Arc<FileStore>> = OnceLock::new();
    STORE
        .get_or_init(|| {
            static SEQ: AtomicU64 = AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "aigx-plugin-catalog-shared-{}-{seq}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let store = Arc::new(FileStore::new(dir));
            install(store.clone());
            store
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn model(id: &str) -> Value {
        json!({ "id": id, "supportsToolCall": true })
    }

    fn unique(tag: &str) -> String {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        format!("unit-cat-{tag}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
    }

    /// sink 发布 → FileStore 回读同形（`models` 原样、`meta.remote = true`）。
    #[test]
    fn sink_publish_roundtrips_through_store() {
        let store = super::test_store();
        let provider = unique("publish");
        let sink = FileCatalogSink::new();
        sink.publish(&provider, vec![model("m1"), model("m2")], 1_700_000_000_123);
        let raw: Value = store.get(&key(&provider)).unwrap().unwrap();
        assert_eq!(raw["meta"]["remote"], true);
        assert_eq!(raw["meta"]["refreshed_at"], 1_700_000_000_123i64);
        assert_eq!(raw["models"].as_array().unwrap().len(), 2);
        assert_eq!(raw["models"][0]["id"], "m1");
        // 进程内读取面与落盘同形
        let snap = snapshot(&provider);
        assert_eq!(snap["models"].as_array().unwrap().len(), 2);
        assert_eq!(models_for(&provider).unwrap().len(), 2);
        assert_eq!(
            model_ids(&provider),
            vec!["m1".to_string(), "m2".to_string()]
        );
        assert_eq!(meta(&provider), (true, 1_700_000_000_123));
        remove(&provider);
        assert!(store.get::<Value>(&key(&provider)).unwrap().is_none());
    }

    /// 空目录**保留 remote**：它不是「没刷新」，而是「刷新了、确实没有模型」。
    #[test]
    fn empty_catalog_keeps_remote() {
        let store = super::test_store();
        let provider = unique("empty");
        FileCatalogSink::new().publish(&provider, Vec::new(), 0);
        let raw: Value = store.get(&key(&provider)).unwrap().unwrap();
        assert_eq!(raw["meta"]["remote"], true);
        assert!(
            raw["meta"]["refreshed_at"].as_i64().unwrap_or(0) > 0,
            "0 时刻应归一为 now（表达「本次真的算了」）"
        );
        assert!(raw["models"].as_array().unwrap().is_empty());
        // 读数面：Some(空) ≠ None（未命中）
        assert_eq!(models_for(&provider), Some(Vec::new()));
        assert!(model_ids(&provider).is_empty());
        assert!(meta(&provider).0);
        remove(&provider);
    }

    /// 未命中：snapshot = null、meta = (false, 0)、models_for = None。
    #[test]
    fn missing_catalog_reads_as_absent() {
        let provider = unique("absent");
        assert!(snapshot(&provider).is_null());
        assert_eq!(models_for(&provider), None);
        assert_eq!(meta(&provider), (false, 0));
        assert!(model_ids(&provider).is_empty());
    }

    /// remove 后读数面立刻不可见（内存与 KV 一起摘）。
    #[test]
    fn remove_clears_memory() {
        let provider = unique("remove");
        set_remote(&provider, vec![model("m")], 10);
        assert!(snapshot(&provider).is_object());
        remove(&provider);
        assert!(snapshot(&provider).is_null());
        assert_eq!(models_for(&provider), None);
        assert_eq!(meta(&provider), (false, 0));
    }

    /// 插件渠道来源口径：声明优先，空则回落目录，provider 空则不贡献。
    #[test]
    fn plugin_channel_source_prefers_declared_then_catalog() {
        fn channel(provider: &str, models: Vec<&str>) -> crate::channel::Channel {
            serde_json::from_value(json!({
                "id": "ch-unit",
                "name": "unit",
                "channel_type": "plugin",
                "plugin_provider": provider,
                "status": "enabled",
                "models": models,
            }))
            .unwrap()
        }
        let provider = unique("channel");
        let mut ch = channel(&provider, Vec::new());
        // 无声明 + 无目录 → 空
        assert_eq!(
            plugin_channel_source(&ch),
            (provider.clone(), Vec::new(), 0)
        );
        // 目录先发布 → 回落目录
        set_remote(&provider, vec![model("cat-a")], 42);
        let (p, models, at) = plugin_channel_source(&ch);
        assert_eq!(p, provider);
        assert_eq!(models, vec!["cat-a".to_string()]);
        assert_eq!(at, 42);
        // 渠道声明优先
        ch.models = vec!["declared-b".to_string()];
        let (_, models, at) = plugin_channel_source(&ch);
        assert_eq!(models, vec!["declared-b".to_string()]);
        assert_eq!(at, 0);
        // plugin_provider 空 → 不贡献模型
        ch.plugin_provider.clear();
        let (p, models, _) = plugin_channel_source(&ch);
        assert!(p.is_empty());
        assert!(models.is_empty());
        remove(&provider);
    }

    /// 轮询等待条件成立（最多 2s，每 5ms 查一次）。
    ///
    /// 测试里**不要用固定 sleep 当同步**：spawn/后台任务在并发负载下随时可能
    /// 超过固定等待，造成假红；空载时又白等。判据保留在调用点（超时即失败）。
    async fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "等待条件在 2s 内未成立（疑似后台任务未执行或去抖窗口未到）"
            );
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    }

    /// 通知 → 去抖 → 触发一次：同一 provider 在窗口内的多次 `models_changed`
    /// 合并；窗口过后再通知会再次触发。
    #[tokio::test]
    async fn models_changed_debounces_to_single_trigger() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static HITS: AtomicUsize = AtomicUsize::new(0);
        // 触发器是进程级 OnceLock：本用例装一次即可（其它用例不装）。
        install_models_changed_trigger(Arc::new(|provider: &str, _account: &str| {
            if provider.starts_with("unit-cat-debounce") {
                HITS.fetch_add(1, Ordering::SeqCst);
            }
        }));
        let provider = unique("debounce");
        for _ in 0..6 {
            notify_models_changed_after(&provider, "acc", Duration::from_millis(20));
        }
        // 轮询等「触发发生了」而不是固定睡 100ms：去抖是后台任务，窗口到点后
        // 还要排到执行，固定等待在负载下会假红（本仓已有同类先例）。
        wait_until(|| HITS.load(Ordering::SeqCst) == 1).await;
        assert_eq!(
            HITS.load(Ordering::SeqCst),
            1,
            "窗口内 6 次通知应合并为 1 次触发"
        );
        // 新窗口 → 再次触发（去抖只合并窗口内，不是永久去重）
        notify_models_changed_after(&provider, "acc", Duration::from_millis(10));
        wait_until(|| HITS.load(Ordering::SeqCst) == 2).await;
        assert_eq!(HITS.load(Ordering::SeqCst), 2, "新窗口应再次触发");
        // 空 provider 直接忽略：这里必须**等**一段固定时间再断言「计数没变」
        // ——「没有发生」无法轮询观测，只能等一个上界（与前两处不同）。
        notify_models_changed_after("   ", "acc", Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(HITS.load(Ordering::SeqCst), 2, "空 provider 不应触发");
    }
}
