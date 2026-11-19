//! 插件生命周期操作（Operation）—— 异步进度模型 + 批量安装/更新/启停/卸载。
//!
//! 对齐 Agent2API v3.5.0-beta.2 `plugin_host::operations`：
//!
//! - 每个包在任意时刻**至多一个**在途操作（按包 id 隔离），并发请求直接拒绝；
//! - 操作在后台任务里执行，阶段流转
//!   `waiting → downloading → verifying → installing → done|failed`，
//!   下载阶段按到达字节回报 `received/total/percent`；
//! - 状态落在进程内表里（权威），并持久化到 `FileStore` 的
//!   `pluginOperation:{id}` 键，用于崩溃后如实展示「上次中断」；
//! - 生命周期变更（启停/卸载/重载）经全局 [`mutation_guard`] 串行，避免
//!   两次宿主切换交错。
//!
//! 与上游的差异只在「谁来执行」：上游 `spawn_task` 调它的 `activation`
//! 模块；AIGX 直接调 [`PluginHostManager`] 的既有 `enable/disable/uninstall`
//! 与 `install_bytes`，语义一致。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};

use crate::storage::FileStore;

use super::PluginHostManager;

/// 操作记录键前缀（持久化用；与上游 `pluginOperation:` 逐字一致）。
const RECORD_PREFIX: &str = "pluginOperation:";

static OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// 一个插件生命周期操作（对齐上游 `Operation`，camelCase 序列化给前端）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Operation {
    /// 包 id（同一包同一时刻至多一个在途操作，故也是表键）。
    pub id: String,
    /// 本次操作唯一标识（同一包连续两次操作不会混淆）。
    pub operation_id: String,
    /// 动作：`install` / `update` / `enable` / `disable` / `reload` / `uninstall`。
    pub action: String,
    /// 阶段：`waiting` / `downloading` / `verifying` / `installing` / `done` / `failed`。
    pub stage: String,
    /// 是否仍在执行。
    pub active: bool,
    /// 是否已成功完成。
    pub done: bool,
    /// 已下载字节。
    pub received: u64,
    /// 总字节（服务端未给 `Content-Length` 时为 0）。
    pub total: u64,
    /// 百分比（`total == 0` 时为 `None`，前端显示不确定进度）。
    pub percent: Option<f64>,
    /// 目标版本（已知时）。
    pub version: String,
    /// 是否为必需更新（本仓暂无强制更新通道，恒 false，保留契约）。
    pub required: bool,
    /// 必需更新的目标版本（同上，恒 None）。
    pub required_version: Option<String>,
    /// 失败原因（本地阶段错误）。
    pub error: Option<String>,
    /// 宿主启用阶段错误（下载/安装成功但宿主加载失败）。
    pub host_error: Option<String>,
    /// 最近更新时间（毫秒）。
    pub updated_at: i64,
    /// 成功结果（如 `{ "pack": { "id", "version" }, "enabled": true }`）。
    pub result: Option<Value>,
}

fn tasks() -> &'static Mutex<BTreeMap<String, Operation>> {
    static TASKS: OnceLock<Mutex<BTreeMap<String, Operation>>> = OnceLock::new();
    TASKS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn mutation_lock() -> &'static AsyncMutex<()> {
    static LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| AsyncMutex::new(()))
}

/// 串行化插件生命周期变更（启停/卸载/重载）。安装/下载不占此锁，但同包
/// 的并发由 [`active`] 的「一个在途操作」约束兜住。
pub async fn mutation_guard() -> MutexGuard<'static, ()> {
    mutation_lock().lock().await
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn operation_id() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_ms(),
        OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

fn record_key(id: &str) -> String {
    format!("{RECORD_PREFIX}{id}")
}

/// 就地修改一个在途操作（不存在则忽略）。
fn mutate(id: &str, update: impl FnOnce(&mut Operation)) {
    if let Ok(mut tasks) = tasks().lock() {
        if let Some(task) = tasks.get_mut(id) {
            update(task);
            task.updated_at = now_ms();
        }
    }
}

/// 持久化当前操作到 `FileStore`（尽力而为，失败不影响内存态）。
fn persist(store: &FileStore, id: &str) {
    let operation = tasks().lock().ok().and_then(|tasks| tasks.get(id).cloned());
    if let Some(operation) = operation {
        let _ = store.put(&record_key(id), &operation);
    }
}

/// 该包是否有在途操作。
pub fn active(id: &str) -> bool {
    tasks()
        .lock()
        .ok()
        .and_then(|tasks| tasks.get(id).map(|task| task.active))
        .unwrap_or(false)
}

fn insert(mut operation: Operation, store: &FileStore) -> Operation {
    operation.updated_at = now_ms();
    if let Ok(mut tasks) = tasks().lock() {
        tasks.insert(operation.id.clone(), operation.clone());
    }
    persist(store, &operation.id);
    operation
}

fn set_stage(id: &str, store: &FileStore, stage: &str) {
    let changed = tasks()
        .lock()
        .map(|mut tasks| {
            let Some(task) = tasks.get_mut(id) else {
                return false;
            };
            if task.stage == stage {
                return false;
            }
            task.stage = stage.to_string();
            task.percent = None;
            task.updated_at = now_ms();
            true
        })
        .unwrap_or(false);
    if changed {
        persist(store, id);
    }
}

fn finalize(id: &str, store: &FileStore, outcome: Result<Value, String>) {
    let ok = outcome.is_ok();
    let value = outcome.as_ref().ok().cloned();
    let error = outcome.err();
    mutate(id, |operation| {
        operation.active = false;
        operation.done = ok;
        operation.stage = if ok { "done" } else { "failed" }.to_string();
        if ok {
            operation.percent = Some(100.0);
            operation.error = None;
        } else {
            operation.error = error.clone();
        }
        if let Some(value) = value.clone() {
            // 启用阶段错误单独落 hostError，其余并入 result。
            operation.host_error = value
                .get("hostError")
                .and_then(Value::as_str)
                .map(str::to_string);
            operation.result = Some(value);
        }
    });
    persist(store, id);
}

/// 中断守卫：后台任务被取消/ panic 时，把仍标记 active 的操作落成失败，
/// 避免前端永远转圈（对齐上游 `OperationGuard`）。
struct OperationGuard {
    id: String,
    operation_id: String,
    store: Arc<FileStore>,
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        let current = tasks().lock().ok().is_some_and(|tasks| {
            tasks
                .get(&self.id)
                .is_some_and(|op| op.active && op.operation_id == self.operation_id)
        });
        if current {
            finalize(
                &self.id,
                &self.store,
                Err("插件操作中断，请刷新后查看插件状态再重试".to_string()),
            );
        }
    }
}

// ── 启动入口 ────────────────────────────────────────────────────────

fn begin(
    store: &FileStore,
    id: &str,
    action: &str,
    stage: &str,
    version: &str,
) -> Result<Operation, String> {
    if id.trim().is_empty() {
        return Err("插件包 id 不能为空".to_string());
    }
    if active(id) {
        return Err(format!("插件包「{id}」正在下载或安装"));
    }
    Ok(insert(
        Operation {
            id: id.to_string(),
            operation_id: operation_id(),
            action: action.to_string(),
            stage: stage.to_string(),
            active: true,
            version: version.to_string(),
            ..Default::default()
        },
        store,
    ))
}

/// 启动一次安装/更新（下载 → 校验安装），后台执行，立即返回初始操作快照。
pub fn start_install(
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
    id: String,
    action: String,
    url: String,
    sha256: Option<String>,
) -> Result<Operation, String> {
    if url.trim().is_empty() {
        return Err("安装地址（url）不能为空".to_string());
    }
    let operation = begin(&store, &id, &action, "waiting", "")?;
    let guard = OperationGuard {
        id: id.clone(),
        operation_id: operation.operation_id.clone(),
        store: store.clone(),
    };
    tokio::spawn(async move {
        run_install(manager, store, id, url, sha256).await;
        drop(guard);
    });
    Ok(operation)
}

async fn run_install(
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
    id: String,
    url: String,
    sha256: Option<String>,
) {
    set_stage(&id, &store, "downloading");
    let progress_id = id.clone();
    let download = manager
        .download_with_progress(&url, move |received, total| {
            mutate(&progress_id, |operation| {
                operation.received = received;
                operation.total = total;
                operation.percent = if total > 0 {
                    Some((received as f64 / total as f64 * 100.0).min(100.0))
                } else {
                    None
                };
            });
        })
        .await;
    let bytes = match download {
        Ok(bytes) => bytes,
        Err(error) => {
            finalize(&id, &store, Err(error.to_string()));
            return;
        }
    };
    set_stage(&id, &store, "verifying");
    let mgr = manager.clone();
    let payload = bytes.clone();
    let expect = sha256.clone();
    set_stage(&id, &store, "installing");
    let installed =
        tokio::task::spawn_blocking(move || mgr.install_bytes(&payload, expect.as_deref()))
            .await
            .map_err(|error| format!("安装任务中断：{error}"))
            .and_then(|result| result.map_err(|error| error.to_string()));
    match installed {
        Ok(meta) => {
            mutate(&id, |operation| {
                operation.version = meta.version.clone();
            });
            finalize(
                &id,
                &store,
                Ok(json!({ "pack": { "id": meta.id, "version": meta.version } })),
            );
        }
        Err(error) => finalize(&id, &store, Err(error)),
    }
}

/// 生命周期动作（启停/卸载/重载）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Enable,
    Disable,
    Reload,
    Uninstall,
}

impl Lifecycle {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "enable" => Some(Self::Enable),
            "disable" => Some(Self::Disable),
            "reload" => Some(Self::Reload),
            "uninstall" => Some(Self::Uninstall),
            _ => None,
        }
    }

    fn action(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
            Self::Reload => "reload",
            Self::Uninstall => "uninstall",
        }
    }
}

/// 启动一次生命周期变更，后台执行，立即返回初始操作快照。
pub fn start_lifecycle(
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
    id: String,
    action: Lifecycle,
) -> Result<Operation, String> {
    let version = manager
        .registry()
        .get(&id)
        .map(|meta| meta.version)
        .unwrap_or_default();
    let operation = begin(&store, &id, action.action(), "waiting", &version)?;
    let guard = OperationGuard {
        id: id.clone(),
        operation_id: operation.operation_id.clone(),
        store: store.clone(),
    };
    tokio::spawn(async move {
        run_lifecycle(manager, store, id, action).await;
        drop(guard);
    });
    Ok(operation)
}

async fn run_lifecycle(
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
    id: String,
    action: Lifecycle,
) {
    let result: Result<Value, String> = {
        // 生命周期变更必须串行：两次宿主切换交错会留下半个状态。
        let _serial = mutation_guard().await;
        match action {
            Lifecycle::Enable => manager
                .enable(&id)
                .await
                .map(|providers| json!({ "enabled": true, "providers": providers.len() })),
            Lifecycle::Disable => manager
                .disable(&id)
                .await
                .map(|_| json!({ "enabled": false })),
            Lifecycle::Reload => match manager.disable(&id).await {
                Ok(()) => manager
                    .enable(&id)
                    .await
                    .map(|providers| json!({ "reloaded": true, "providers": providers.len() })),
                Err(error) => Err(error),
            },
            Lifecycle::Uninstall => manager
                .uninstall(&id)
                .await
                .map(|_| json!({ "uninstalled": true })),
        }
    };
    finalize(&id, &store, result);
}

// ── 批量 ────────────────────────────────────────────────────────────

/// 一条批量条目（顺序执行；启停/卸载/重载经全局互斥串行，安装并发受
/// 同包「一个在途操作」约束）。
///
/// `update` 有两种形态：
/// - 给 `url`：直接按该地址更新（与 `install` 同形）；
/// - 不给 `url` 但给 `catalog`：从目录条目解析目标版本与平台 URL（对齐上游
///   `POST /api/plugins/batch {action:"update", ids}`）；已在最新版即跳过。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchItem {
    pub id: String,
    pub action: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    /// 目录条目（`{version, platforms:[{target,url,size,sha256}]}`），
    /// `update` 且缺 `url` 时用于解析目标版本与下载地址。
    #[serde(default)]
    pub catalog: Option<Value>,
}

/// 每平台的目标三元组（前端 `target` 是 OS 名，目录里可能是完整三元组）。
fn platform_matches(target: &str) -> bool {
    let os = std::env::consts::OS;
    target == os
        || target.starts_with(match os {
            "windows" => "windows",
            "macos" => "darwin",
            "linux" => "linux",
            _ => os,
        })
        || target == "wasm32-unknown-unknown"
}

/// 从目录条目解析「目标版本 + 当前平台下载地址 + sha256」。
fn resolve_update(
    catalog: &Value,
    installed_version: &str,
) -> Result<(String, String, Option<String>), String> {
    let version = catalog["version"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "目录条目缺少版本".to_string())?
        .to_string();
    if !installed_version.is_empty() {
        match crate::update::compare_versions(&version, installed_version) {
            Some(order) if order <= 0 => return Err("已经是最新版本".to_string()),
            None => {}
            Some(_) => {}
        }
    }
    let platforms = catalog["platforms"]
        .as_array()
        .ok_or_else(|| "目录条目缺少平台下载地址".to_string())?;
    let platform = platforms
        .iter()
        .find(|item| item["target"].as_str().is_some_and(platform_matches))
        .ok_or_else(|| "当前平台暂无安装包".to_string())?;
    let url = platform["url"]
        .as_str()
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| "当前平台暂无安装包".to_string())?
        .to_string();
    let sha256 = platform["sha256"].as_str().map(str::to_string);
    Ok((version, url, sha256))
}

/// 执行一批生命周期操作，返回每条目的初始操作快照（前端随后轮询
/// `/api/plugins/progress` 取进度与结果）。
pub fn batch(
    manager: Arc<PluginHostManager>,
    store: Arc<FileStore>,
    items: Vec<BatchItem>,
) -> Value {
    let mut started: Vec<Value> = Vec::with_capacity(items.len());
    for item in items {
        let result = match item.action.as_str() {
            "install" => {
                let url = item.url.clone().unwrap_or_default();
                if url.trim().is_empty() {
                    Err("缺少安装地址（url）".to_string())
                } else {
                    start_install(
                        manager.clone(),
                        store.clone(),
                        item.id.clone(),
                        item.action.clone(),
                        url,
                        item.sha256.clone(),
                    )
                }
            }
            "update" => {
                let explicit_url = item.url.clone().filter(|url| !url.trim().is_empty());
                match explicit_url {
                    Some(url) => start_install(
                        manager.clone(),
                        store.clone(),
                        item.id.clone(),
                        item.action.clone(),
                        url,
                        item.sha256.clone(),
                    ),
                    None => {
                        let Some(catalog) = item.catalog.clone() else {
                            return_or_push_error(
                                &mut started,
                                &item.id,
                                &item.action,
                                "更新缺少下载地址（url）或目录条目（catalog）".to_string(),
                            );
                            continue;
                        };
                        let installed = manager
                            .registry()
                            .get(&item.id)
                            .map(|meta| meta.version)
                            .unwrap_or_default();
                        match resolve_update(&catalog, &installed) {
                            Ok((version, url, sha256)) => start_install(
                                manager.clone(),
                                store.clone(),
                                item.id.clone(),
                                item.action.clone(),
                                url,
                                sha256,
                            )
                            .map(|mut operation| {
                                operation.version = version;
                                operation
                            }),
                            Err(reason) => Err(reason),
                        }
                    }
                }
            }
            other => match Lifecycle::parse(other) {
                Some(action) => {
                    start_lifecycle(manager.clone(), store.clone(), item.id.clone(), action)
                }
                None => Err(format!("未知的插件动作：{other}")),
            },
        };
        match result {
            Ok(operation) => started.push(serde_json::to_value(&operation).unwrap_or(Value::Null)),
            Err(error) => started.push(json!({
                "id": item.id, "action": item.action, "active": false, "done": false,
                "stage": "failed", "error": error,
            })),
        }
    }
    json!({ "operations": started })
}

/// 批量条目即时失败的统一出口（不中断后续条目）。
fn return_or_push_error(started: &mut Vec<Value>, id: &str, action: &str, error: String) {
    started.push(json!({
        "id": id, "action": action, "active": false, "done": false,
        "stage": "failed", "error": error,
    }));
}

// ── 快照 ────────────────────────────────────────────────────────────

/// 在途/近期操作快照（内存权威 + 持久化的历史条目补全）。
///
/// 持久化补全的语义与上游一致：只在内存里没有该 id 时补入；补入时若原本
/// 标记为 active（进程中途退出），改判为 `failed` 并注明「上次中断」。
pub fn snapshot(store: Option<&FileStore>) -> Value {
    let mut operations = tasks()
        .lock()
        .map(|tasks| tasks.clone())
        .unwrap_or_default();
    if let Some(store) = store {
        if let Ok(keys) = store.list(RECORD_PREFIX) {
            for key in keys {
                let Some(id) = key.strip_prefix(RECORD_PREFIX) else {
                    continue;
                };
                if operations.contains_key(id) {
                    continue;
                }
                let Ok(Some(mut operation)) = store.get::<Operation>(&key) else {
                    continue;
                };
                if operation.active {
                    operation.active = false;
                    operation.done = false;
                    operation.stage = "failed".to_string();
                    operation.error = Some("上次插件操作中断，请重试".to_string());
                }
                operations.insert(id.to_string(), operation);
            }
        }
    }
    json!({ "operations": operations.into_values().collect::<Vec<_>>() })
}

/// 清空进程内表（测试用）。
#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(mut tasks) = tasks().lock() {
        tasks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_store() -> Arc<FileStore> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-plugin-ops-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(FileStore::new(dir))
    }

    #[test]
    fn lifecycle_parse_covers_known_actions() {
        assert_eq!(Lifecycle::parse("enable"), Some(Lifecycle::Enable));
        assert_eq!(Lifecycle::parse("disable"), Some(Lifecycle::Disable));
        assert_eq!(Lifecycle::parse("reload"), Some(Lifecycle::Reload));
        assert_eq!(Lifecycle::parse("uninstall"), Some(Lifecycle::Uninstall));
        assert_eq!(Lifecycle::parse("bogus"), None);
    }

    #[test]
    fn operation_lifecycle_mutate_and_finalize() {
        reset_for_test();
        let store = temp_store();
        let op = insert(
            Operation {
                id: "pack-a".into(),
                operation_id: operation_id(),
                action: "install".into(),
                stage: "waiting".into(),
                active: true,
                ..Default::default()
            },
            &store,
        );
        assert!(active("pack-a"));
        set_stage("pack-a", &store, "downloading");
        mutate("pack-a", |operation| {
            operation.received = 50;
            operation.total = 100;
            operation.percent = Some(50.0);
        });
        finalize("pack-a", &store, Ok(json!({ "pack": { "id": "pack-a" } })));
        let value = snapshot(None);
        let entry = value["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["id"] == "pack-a")
            .cloned()
            .unwrap();
        assert_eq!(entry["done"], true);
        assert_eq!(entry["stage"], "done");
        assert_eq!(entry["percent"], 100.0);
        assert!(!active("pack-a"));
        // 持久化可被 snapshot(store) 读回
        let persisted = snapshot(Some(&store));
        assert!(persisted["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value["id"] == "pack-a"));
        let _ = op;
    }

    #[test]
    fn duplicate_in_flight_operation_is_rejected() {
        reset_for_test();
        let store = temp_store();
        begin(&store, "pack-b", "install", "waiting", "").unwrap();
        let err = begin(&store, "pack-b", "install", "waiting", "").unwrap_err();
        assert!(err.contains("正在下载或安装"));
    }

    #[test]
    fn interrupted_persisted_operation_reads_back_as_failed() {
        reset_for_test();
        let store = temp_store();
        // 直接写一条 active=true 的历史记录（模拟进程中途退出后重启）
        let op = Operation {
            id: "pack-c".into(),
            operation_id: operation_id(),
            action: "install".into(),
            stage: "downloading".into(),
            active: true,
            ..Default::default()
        };
        store.put(&record_key("pack-c"), &op).unwrap();
        let snapshot = snapshot(Some(&store));
        let entry = snapshot["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["id"] == "pack-c")
            .cloned()
            .unwrap();
        assert_eq!(entry["active"], false);
        assert_eq!(entry["stage"], "failed");
    }

    #[test]
    fn resolve_update_rejects_same_version() {
        let catalog = json!({
            "version": "1.2.3",
            "platforms": [{ "target": std::env::consts::OS, "url": "https://x/p.zip" }]
        });
        let err = resolve_update(&catalog, "1.2.3").unwrap_err();
        assert!(err.contains("最新版本"));
    }

    #[test]
    fn resolve_update_rejects_older_version() {
        let catalog = json!({
            "version": "1.2.2",
            "platforms": [{ "target": std::env::consts::OS, "url": "https://x/p.zip" }]
        });
        let err = resolve_update(&catalog, "1.2.3").unwrap_err();
        assert!(err.contains("最新版本"));
    }

    #[test]
    fn resolve_update_picks_current_platform_when_newer() {
        let catalog = json!({
            "version": "1.3.0",
            "platforms": [
                { "target": "plan9", "url": "https://x/other.zip" },
                { "target": std::env::consts::OS, "url": "https://x/p.zip", "sha256": "abc" }
            ]
        });
        let (version, url, sha) = resolve_update(&catalog, "1.2.3").unwrap();
        assert_eq!(version, "1.3.0");
        assert_eq!(url, "https://x/p.zip");
        assert_eq!(sha.as_deref(), Some("abc"));
    }

    #[test]
    fn resolve_update_errors_without_platform() {
        let catalog = json!({ "version": "9.9.9", "platforms": [] });
        let err = resolve_update(&catalog, "1.0.0").unwrap_err();
        assert!(err.contains("当前平台"));
    }

    #[test]
    fn resolve_update_allows_fresh_install_without_installed_version() {
        // 未安装（installed_version 为空）：不判「最新」，直接取平台地址（更新语义的边界）
        let catalog = json!({
            "version": "1.0.0",
            "platforms": [{ "target": std::env::consts::OS, "url": "https://x/p.zip" }]
        });
        assert!(resolve_update(&catalog, "").is_ok());
    }
}
