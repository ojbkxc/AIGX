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
//! ## 两份目录，同一张读取面
//!
//! 目录有**两个**独立的写入方，覆盖的账号集合不同，不能混写：
//!
//! | 写入方 | 入口 | KV 键 | 覆盖谁 |
//! |---|---|---|---|
//! | 插件/脚本宿主 | [`set_remote`]（sink） | `plugin_catalog:{provider}` | 插件自有账号 |
//! | 网关账号目录 | [`set_account_remote`] | `account_catalog:{provider}` | 网关直连账号 |
//!
//! 分开存的理由：宿主对「没有插件账号的 provider」会发布**空**并集（它按
//! 插件账号算并集，那些 provider 一个插件账号都没有），若与账号目录共用一格，
//! 一次空发布就把账号目录刷出来的上限整片抹掉。读取面按模型合并两份，
//! 插件那份逐字段优先（它是上游对插件账号的直接申报）。
//!
//! **读取面在 KV 里复活上一份并集**（每个 provider 每进程一次，见
//! [`hydrate`]）：不复活的话，进程重启后目录为空直到下一次刷新，而
//! 「目录是旧的但看起来正常」与「从未刷新」在管理页上无法区分 —— 前者是
//! 真的、后者是空的，静默的区别比不复活更糟。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::storage::FileStore;

use super::wasm::catalog::CatalogSink;

/// KV 键前缀（provider 粒度）：插件/脚本宿主发布的并集。
const KEY_PREFIX: &str = "plugin_catalog:";

/// KV 键前缀：网关账号目录（`account_session::catalog`）转投的并集。
///
/// 与 [`KEY_PREFIX`] **刻意分格**：两者覆盖的账号集合不同，共用一格时宿主
/// 对「无插件账号」provider 发布的空并集会抹掉账号目录（见模块头）。
const ACCOUNT_KEY_PREFIX: &str = "account_catalog:";

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
    /// 预算索引：模型 id（小写）→ 上下文/输出上限。
    ///
    /// 与 `models` 同一次写入构造（见 [`entry`]），**不随读次数重建**：
    /// `/api/models/manage` 会遍历几百个模型，逐个去 models 数组里线性找
    /// 字段是 O(模型数 × 目录条数)，且每次都要 `Value` 反序列化取值。
    budget: Arc<HashMap<String, ModelBudget>>,
}

impl Entry {
    fn snapshot(&self) -> Value {
        json!({
            "models": self.models,
            "meta": { "remote": true, "refreshed_at": self.refreshed_at },
        })
    }
}

/// 插件申报的「上下文窗口 / 最大输出」上限。
///
/// 这是模型元信息链路里**唯一**能拿到账号型供应商（小浣熊等只在插件目录里
/// 申报上限的家）真实值的来源：数据由插件自己 `models.refresh` 回报，经
/// `wasm::catalog::merge` 按 provider 取各账号最大值后落到这里。
/// 两个字段各自独立取用 —— 只申报了上下文的家不会把输出上限写成 0。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModelBudget {
    /// 上下文窗口（token）。
    pub context_length: Option<u32>,
    /// 最大输出 token。
    pub max_output_tokens: Option<u32>,
}

impl ModelBudget {
    /// 合并两份申报（逐字段取大；`None` 不覆盖已有值）。
    fn absorb(&mut self, other: ModelBudget) {
        if let Some(value) = other.context_length {
            self.context_length = Some(match self.context_length {
                Some(current) => current.max(value),
                None => value,
            });
        }
        if let Some(value) = other.max_output_tokens {
            self.max_output_tokens = Some(match self.max_output_tokens {
                Some(current) => current.max(value),
                None => value,
            });
        }
    }

    fn is_empty(&self) -> bool {
        self.context_length.is_none() && self.max_output_tokens.is_none()
    }
}

/// 目录条目里的上下文上限键（上游 `ModelList` 口径 + 账号目录的落盘形态）。
///
/// `context_length` / `contextLength` / `context_window` 是网关账号目录
/// （`account_session::catalog::CatalogEntry.context_length`）与各家上游
/// 原始目录用的键 —— 账号目录转投过来时字段名若不被认，就会出现
/// 「值进了目录、预算索引却是空的」（`budget_for` 恒 `None`）。
const CONTEXT_KEYS: &[&str] = &[
    "maxInputTokens",
    "max_input_tokens",
    "context_length",
    "contextLength",
    "context_window",
    "maxContextLength",
];
/// 目录条目里的输出上限键。
const OUTPUT_KEYS: &[&str] = &["maxOutputTokens", "max_output_tokens"];

/// 从目录条目对象里取一个正整数上限（0 / 超 u32 一律当未申报）。
fn pick_limit(model: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter().find_map(|key| {
        model
            .get(*key)
            .and_then(Value::as_u64)
            .filter(|n| *n > 0 && *n <= u32::MAX as u64)
            .map(|n| n as u32)
    })
}

/// 目录数组 → 预算索引（模型 id 小写 → 上限）。
fn budget_index(models: &[Value]) -> Arc<HashMap<String, ModelBudget>> {
    let mut table: HashMap<String, ModelBudget> = HashMap::new();
    for model in models {
        let Some(id) = model["id"].as_str() else {
            continue;
        };
        let budget = ModelBudget {
            context_length: pick_limit(model, CONTEXT_KEYS),
            max_output_tokens: pick_limit(model, OUTPUT_KEYS),
        };
        if budget.is_empty() {
            continue;
        }
        table
            .entry(id.trim().to_ascii_lowercase())
            .and_modify(|slot| slot.absorb(budget))
            .or_insert(budget);
    }
    Arc::new(table)
}

/// provider 键归一：去空白 + 小写。
///
/// 写入方与读取方的 provider 串来自不同层（渠道配置 / 账号记录 / 插件清单），
/// 大小写或尾随空白的任何差异都会让精确匹配落空 —— 而落空的读数与「这家确实
/// 没申报」长得一模一样。归一只做在这一处，两边用同一把刀。
fn normalize_provider(provider: &str) -> Option<String> {
    let trimmed = provider.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// 插件/脚本宿主发布的目录（进程内）。
fn catalog_table() -> &'static Mutex<HashMap<String, Arc<Entry>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Arc<Entry>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 网关账号目录转投的并集（进程内；与 [`catalog_table`] 分格）。
fn account_table() -> &'static Mutex<HashMap<String, Arc<Entry>>> {
    static TABLE: OnceLock<Mutex<HashMap<String, Arc<Entry>>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 进程内缓存（未命中返回 `None`；调用方负责先 [`hydrate`]）。
fn cached_in(
    table: &'static Mutex<HashMap<String, Arc<Entry>>>,
    provider: &str,
) -> Option<Arc<Entry>> {
    let provider = normalize_provider(provider)?;
    table.lock().ok()?.get(&provider).cloned()
}

/// 已经尝试过 KV 复活的 provider（负缓存：避免每次读都打一次存储）。
fn attempted_slot() -> &'static Mutex<HashSet<String>> {
    static ATTEMPTED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ATTEMPTED.get_or_init(|| Mutex::new(HashSet::new()))
}

fn lock_attempted() -> std::sync::MutexGuard<'static, HashSet<String>> {
    attempted_slot()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn key(provider: &str) -> String {
    format!("{KEY_PREFIX}{provider}")
}

fn account_key(provider: &str) -> String {
    format!("{ACCOUNT_KEY_PREFIX}{provider}")
}

/// 从一个 KV 键复活目录条目（写进对应进程内表；已有条目不动）。
fn restore_value(
    store: &FileStore,
    stored_key: &str,
    provider: &str,
    table: &'static Mutex<HashMap<String, Arc<Entry>>>,
) {
    let raw = match store.get::<Value>(stored_key) {
        Ok(Some(raw)) => raw,
        _ => return,
    };
    let refreshed_at = raw["meta"]["refreshed_at"].as_i64().unwrap_or(0);
    let models = raw["models"].as_array().cloned().unwrap_or_default();
    let Some(entry) = entry_at(models, refreshed_at, false) else {
        return;
    };
    if let Ok(mut table) = table.lock() {
        table.entry(provider.to_string()).or_insert(entry);
    }
}

/// 读穿一次：内存未命中时从 KV 复活这个 provider 的两份目录。
///
/// 每进程每 provider 只尝试一次（`attempted` 负缓存）—— 否则 `/api/models/manage`
/// 遍历几百个模型时每次 `budget_for` 都会打一次存储读。
fn hydrate(provider: &str) {
    let Some(provider) = normalize_provider(provider) else {
        return;
    };
    // 存储尚未装入（启动早期）时不标记 attempted：装入后还要能复活。
    let Some(store) = store_slot().get() else {
        return;
    };
    if !lock_attempted().insert(provider.clone()) {
        return;
    }
    restore_value(store, &key(&provider), &provider, catalog_table());
    restore_value(store, &account_key(&provider), &provider, account_table());
}

/// 全量复活一次（`budget_any` 没有 provider 维度，只能扫全表）。
fn hydrate_all() {
    static DONE: OnceLock<Mutex<bool>> = OnceLock::new();
    let Some(store) = store_slot().get() else {
        return;
    };
    let done = DONE.get_or_init(|| Mutex::new(false));
    {
        let mut done = done.lock().unwrap_or_else(|error| error.into_inner());
        if *done {
            return;
        }
        *done = true;
    }
    for (prefix, table) in [
        (KEY_PREFIX, catalog_table()),
        (ACCOUNT_KEY_PREFIX, account_table()),
    ] {
        let Ok(keys) = store.list(prefix) else {
            continue;
        };
        for stored_key in keys {
            let raw = stored_key.trim_start_matches(prefix).trim();
            let Some(provider) = normalize_provider(raw) else {
                continue;
            };
            lock_attempted().insert(provider.clone());
            restore_value(store, &stored_key, &provider, table);
        }
    }
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
    entry_at(models, refreshed_at, true)
}

/// `default_now = true` 时把非正时刻归一为 now（「本次真的算了」）；
/// 复活 KV 里的旧条目用 `false` —— 旧条目没给时刻就是没给，不要编一个。
fn entry_at(models: Vec<Value>, refreshed_at: i64, default_now: bool) -> Option<Arc<Entry>> {
    if models.len() > 4096 {
        return None;
    }
    let budget = budget_index(&models);
    let refreshed_at = if refreshed_at > 0 {
        refreshed_at
    } else if default_now {
        super::wasm::now_ms()
    } else {
        0
    };
    Some(Arc::new(Entry {
        models,
        refreshed_at,
        budget,
    }))
}

/// 写入一个 provider 的目录（上游 `rpc_cache::set_remote` 语义）。
///
/// 空目录照存（见模块头）：`remote:true` 表达「刷新成功且确实没有模型」，
/// 与「从未刷新」（缓存未命中 / KV 无值）是两回事 —— 上层据此决定是否回落
/// 渠道声明。未装入存储时只留进程内缓存，读数语义不变。
pub fn set_remote(provider: &str, models: Vec<Value>, refreshed_at: i64) {
    publish_into(catalog_table(), KEY_PREFIX, provider, models, refreshed_at);
}

/// 写入网关账号目录（`account_session::catalog`）的 provider 并集。
///
/// 与 [`set_remote`] **分开落格**（见模块头）：宿主对「无插件账号」的 provider
/// 会发布空并集，共用一格时这条空发布会把账号目录刷出来的上限整片抹掉。
/// 两个入口的写入形态一致（模型对象数组 + 毫秒时刻），读取面按模型合并。
pub fn set_account_remote(provider: &str, models: Vec<Value>, refreshed_at: i64) {
    publish_into(
        account_table(),
        ACCOUNT_KEY_PREFIX,
        provider,
        models,
        refreshed_at,
    );
}

fn publish_into(
    table: &'static Mutex<HashMap<String, Arc<Entry>>>,
    prefix: &str,
    provider: &str,
    models: Vec<Value>,
    refreshed_at: i64,
) {
    let Some(provider) = normalize_provider(provider) else {
        return;
    };
    let Some(entry) = entry(models, refreshed_at) else {
        return;
    };
    if let Some(store) = store_slot().get() {
        // 落盘失败不影响本次读数（目录是可重建的展示数据，与
        // `account_session::catalog::save_cache` 同取向）。
        let _ = store.put(&format!("{prefix}{provider}"), &entry.snapshot());
    }
    if let Ok(mut table) = table.lock() {
        table.insert(provider.clone(), entry);
    }
    // 刚写入的就是最新事实：不需要再从 KV 复活这个 provider。
    lock_attempted().insert(provider);
}

/// 目录的模型对象数组（未命中返回 `None`；空目录返回 `Some(vec![])`）。
///
/// **只看插件/脚本宿主那份**：这是插件自己的目录（`settings.get("modelCatalog")`
/// 与插件渠道的模型回落都读它），把网关账号目录并进来会让插件看到自己路由不了
/// 的模型。网关账号目录只参与 [`budget_for`] 的上限查询（元信息层用）。
pub fn models_for(provider: &str) -> Option<Vec<Value>> {
    hydrate(provider);
    cached_in(catalog_table(), provider).map(|entry| entry.models.clone())
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
/// `remote=true` 只在「本进程算过并集（或从 KV 复活过）」时为真 ——
/// 与 [`models_for`] 同源（插件/脚本宿主那份）。
pub fn meta(provider: &str) -> (bool, i64) {
    hydrate(provider);
    match cached_in(catalog_table(), provider) {
        Some(entry) => (true, entry.refreshed_at),
        None => (false, 0),
    }
}

/// 某 provider 对某模型的上下文/输出申报（精确匹配；未命中返回 `None`）。
///
/// 模型 id 大小写不敏感（与 `wasm::catalog::aggregate` 的并集键同一归一：
/// 都是 `to_ascii_lowercase`）。`None` 的含义是「这家没申报过」，**不是**
/// 「申报了 0」—— 调用方据此决定是否回落下一层（models.dev / 内置表）。
///
/// 有 provider 维度时必须用这个函数：同一模型 id 在不同 provider 下上限
/// 可以不同（渠道 A 的 `glm-5.3` 是 128k、账号池 B 是 200k）。
///
/// 两份目录都命中时逐字段取大：插件那份是上游对插件账号的直接申报，账号
/// 目录那份是网关直连账号的申报，两者的最大值就是「这家上游给得出来的最大」。
pub fn budget_for(provider: &str, model: &str) -> Option<ModelBudget> {
    let key = model.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    hydrate(provider);
    let plugin =
        cached_in(catalog_table(), provider).and_then(|entry| entry.budget.get(&key).copied());
    let account =
        cached_in(account_table(), provider).and_then(|entry| entry.budget.get(&key).copied());
    match (plugin, account) {
        (Some(mut plugin), Some(account)) => {
            plugin.absorb(account);
            Some(plugin)
        }
        (Some(plugin), None) => Some(plugin),
        (None, account) => account,
    }
}

/// 跨全部 provider 的申报（逐字段取大；未命中返回 `None`）。
///
/// 只在**拿不到 provider 维度**的调用点用（例如只按模型名解析的旧路径）。
/// 语义是「任意一家申报过就算」—— 多 provider 同时申报时取最大值，可能
/// 高于当前这条链自己的上限；拿得到 provider 时优先 [`budget_for`]。
pub fn budget_any(model: &str) -> Option<ModelBudget> {
    let key = model.trim().to_ascii_lowercase();
    if key.is_empty() {
        return None;
    }
    hydrate_all();
    let mut merged: Option<ModelBudget> = None;
    for table in [catalog_table(), account_table()] {
        let Ok(table) = table.lock() else {
            continue;
        };
        for entry in table.values() {
            if let Some(budget) = entry.budget.get(&key) {
                match &mut merged {
                    Some(slot) => slot.absorb(*budget),
                    None => merged = Some(*budget),
                }
            }
        }
    }
    merged
}

/// 完整快照 `{models, meta}`（未命中返回 `null`；= 上游 `rpc_cache::snapshot`）。
pub fn snapshot(provider: &str) -> Value {
    let Some(models) = models_for(provider) else {
        return Value::Null;
    };
    json!({
        "models": models,
        "meta": { "remote": true, "refreshed_at": meta(provider).1 },
    })
}

/// 摘除一个 provider 的目录（卸载 / 停用；= 上游 `rpc_cache::forget`）。
pub fn remove(provider: &str) {
    let Some(provider) = normalize_provider(provider) else {
        return;
    };
    for table in [catalog_table(), account_table()] {
        if let Ok(mut table) = table.lock() {
            table.remove(&provider);
        }
    }
    if let Some(store) = store_slot().get() {
        let _ = store.delete(&key(&provider));
        let _ = store.delete(&account_key(&provider));
    }
    lock_attempted().remove(&provider);
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

/// 清掉某 provider 的进程内表与负缓存（测试用：模拟进程重启）。
#[cfg(test)]
pub(crate) fn forget_memory_for_test(provider: &str) {
    let Some(provider) = normalize_provider(provider) else {
        return;
    };
    for table in [catalog_table(), account_table()] {
        if let Ok(mut table) = table.lock() {
            table.remove(&provider);
        }
    }
    lock_attempted().remove(&provider);
}

/// 读账号目录那份的落盘形态（测试用：核对转投后的键名与取值）。
#[cfg(test)]
pub(crate) fn account_snapshot_for_test(provider: &str) -> Option<Value> {
    let provider = normalize_provider(provider)?;
    cached_in(account_table(), &provider).map(|entry| entry.snapshot())
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

    /// 预算索引：按 provider 精确取上限（模型 id 大小写不敏感）、只申报一个
    /// 字段时另一个保持 `None`、未命中 / 未发布的 provider 一律 `None`。
    ///
    /// 这是元信息层（`model::metadata::resolve_with_sources_for`）取「这家
    /// 上游申报的上下文/输出」的唯一入口，所以形状与三态语义在这里钉死。
    #[test]
    fn budget_index_reads_provider_scoped_limits() {
        let provider = unique("budget");
        // 模型名带唯一后缀：`budget_any` 扫全局表，别家同名条目会污染取大结果
        // （大小写归一由大小写变体覆盖，不靠通用名）。
        let upper = format!("GLM-{}-Budget", unique("model"));
        let lower = upper.to_lowercase();
        set_remote(
            &provider,
            vec![
                json!({ "id": upper, "maxInputTokens": 131_072,
                    "maxOutputTokens": 8_192 }),
                // 只申报上下文：输出必须保持 None（不是 0）
                json!({ "id": "ctx-only", "maxInputTokens": 32_000 }),
                // 两个字段都没申报：不进索引
                json!({ "id": "no-limits" }),
                // 大小写不同的同一模型：索引键归一，逐字段取大
                json!({ "id": lower, "maxInputTokens": 200_000 }),
            ],
            1,
        );
        assert_eq!(
            budget_for(&provider, &lower),
            Some(ModelBudget {
                context_length: Some(200_000),
                max_output_tokens: Some(8_192),
            }),
            "索引键归一后逐字段取大；输出只申报过一次，仍是 8_192"
        );
        assert_eq!(
            budget_for(&provider, &upper),
            budget_for(&provider, &lower),
            "模型 id 大小写不敏感"
        );
        assert_eq!(
            budget_for(&provider, "CTX-ONLY"),
            Some(ModelBudget {
                context_length: Some(32_000),
                max_output_tokens: None,
            }),
            "只申报上下文的家不得凭空得到输出上限"
        );
        assert_eq!(budget_for(&provider, "no-limits"), None);
        assert_eq!(budget_for(&provider, "missing-model"), None);
        // 未发布的 provider / 空 provider / 空模型名 → 都没有申报
        assert_eq!(budget_for("unit-cat-never-published", &lower), None);
        assert_eq!(budget_for("   ", &lower), None);
        assert_eq!(budget_for(&provider, "  "), None);
        // 跨 provider 查询也能读到（已知限制路径：无 provider 维度时取大）
        assert!(budget_any(&lower).is_some_and(|b| b.context_length == Some(200_000)));
        // 摘除目录后立即不可见
        remove(&provider);
        assert_eq!(budget_for(&provider, &lower), None);
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

    // ── 账号目录转投 + KV 复活（本次修复的回归面）─────────────────────

    /// 账号目录（`account_session::catalog`）转投的上限必须能被 `budget_for`
    /// 读到 —— 宿主对同一 provider 发布的是**空**并集（它只算插件账号），
    /// 修正前元信息层只有宿主那一份可查，于是账号目录刷得再全也读不出值。
    #[test]
    fn account_catalog_feeds_budgets_when_plugin_catalog_is_empty() {
        let provider = unique("account-empty-plugin");
        // 宿主发布：刷新成功但没有插件账号的模型（线上 26 个 provider 的真实形态）
        set_remote(&provider, Vec::new(), 1);
        assert_eq!(budget_for(&provider, "raccoon-8c4485"), None);

        // 账号目录转投：上下文 + 输出
        set_account_remote(
            &provider,
            vec![json!({ "id": "Raccoon-8C4485", "maxInputTokens": 1_000_000,
                "maxOutputTokens": 100_000, "supportsImages": true })],
            2,
        );
        assert_eq!(
            budget_for(&provider, "raccoon-8c4485"),
            Some(ModelBudget {
                context_length: Some(1_000_000),
                max_output_tokens: Some(100_000),
            })
        );
        // `models_for` 仍是插件自己那份：它是插件 `settings.get("modelCatalog")`
        // 与插件渠道模型回落的读面，混进网关账号的模型会让插件路由不了
        // 却「看起来声明过」。
        assert_eq!(models_for(&provider), Some(Vec::new()));
        assert!(model_ids(&provider).is_empty());
        remove(&provider);
    }

    /// 两份目录同一 provider 都有申报时，上限逐字段取大 ——
    /// 插件账号与网关账号各写一半，任一份被丢掉都会少一截。
    #[test]
    fn plugin_and_account_budgets_merge_per_field() {
        let provider = unique("merge");
        set_account_remote(
            &provider,
            vec![
                json!({ "id": "shared", "maxInputTokens": 128_000 }),
                json!({ "id": "account-only", "maxOutputTokens": 4_096 }),
            ],
            10,
        );
        set_remote(
            &provider,
            vec![
                json!({ "id": "SHARED", "maxOutputTokens": 8_192, "supportsToolCall": true }),
                json!({ "id": "plugin-only", "maxInputTokens": 64_000 }),
            ],
            20,
        );
        assert_eq!(
            budget_for(&provider, "shared"),
            Some(ModelBudget {
                context_length: Some(128_000),
                max_output_tokens: Some(8_192),
            }),
            "上下文来自账号目录、输出来自插件目录，两条链各自的那一半都要留"
        );
        assert_eq!(
            budget_for(&provider, "account-only"),
            Some(ModelBudget {
                context_length: None,
                max_output_tokens: Some(4_096),
            })
        );
        assert_eq!(
            budget_for(&provider, "plugin-only"),
            Some(ModelBudget {
                context_length: Some(64_000),
                max_output_tokens: None,
            })
        );
        // 插件读面不受账号目录影响
        assert_eq!(model_ids(&provider).len(), 2);
        assert_eq!(meta(&provider), (true, 20));
        remove(&provider);
    }

    /// 账号目录落盘形态的键名（`context_length` / `max_output_tokens`）也要认：
    /// 字段名对不上时「值进了目录、预算索引却是空的」，与没转投一模一样。
    #[test]
    fn budget_index_accepts_account_catalog_field_names() {
        let provider = unique("aliases");
        set_account_remote(
            &provider,
            vec![json!({ "id": "ctx-alias", "context_length": 262_144,
                "max_output_tokens": 32_768 })],
            3,
        );
        assert_eq!(
            budget_for(&provider, "CTX-ALIAS"),
            Some(ModelBudget {
                context_length: Some(262_144),
                max_output_tokens: Some(32_768),
            })
        );
        remove(&provider);
    }

    /// provider 键归一：写入带大写 / 尾随空白，读取用小写也能命中。
    #[test]
    fn provider_keys_are_normalized() {
        let provider = unique("Case");
        set_remote(
            &format!("  {}  ", provider.to_uppercase()),
            vec![json!({ "id": "m", "maxInputTokens": 1_024 })],
            5,
        );
        assert_eq!(
            budget_for(&provider, "m"),
            Some(ModelBudget {
                context_length: Some(1_024),
                max_output_tokens: None,
            })
        );
        remove(&provider);
    }

    /// 进程内表被清空（= 进程重启）后，读取面从 KV 复活上一份并集 ——
    /// 否则重启后「目录本来就有」会静默变成「目录是空的」。
    #[test]
    fn catalog_restores_from_store_after_restart() {
        let store = super::test_store();
        let provider = unique("restore");
        set_account_remote(
            &provider,
            vec![json!({ "id": "persisted", "maxInputTokens": 200_000,
                "maxOutputTokens": 16_384 })],
            1_700_000_000_000,
        );
        // 落盘确实写了账号目录那一格
        assert!(store
            .get::<Value>(&account_key(&provider))
            .unwrap()
            .is_some());
        // 模拟重启：两份进程内表都空、负缓存也清掉
        forget_memory_for_test(&provider);
        // 下一读穿 KV（并行的 `budget_any` 也可能触发全量复活，两者都从 KV 来）
        assert_eq!(
            budget_for(&provider, "persisted"),
            Some(ModelBudget {
                context_length: Some(200_000),
                max_output_tokens: Some(16_384),
            })
        );
        remove(&provider);
        assert!(store
            .get::<Value>(&account_key(&provider))
            .unwrap()
            .is_none());
    }

    /// 负向对照：KV 里也没有时，读穿不能编出条目（`None` 仍是 `None`）。
    #[test]
    fn hydrate_does_not_invent_missing_catalog() {
        let provider = unique("hydrate-miss");
        forget_memory_for_test(&provider);
        assert_eq!(models_for(&provider), None);
        assert_eq!(budget_for(&provider, "nope"), None);
        assert_eq!(meta(&provider), (false, 0));
    }
}
