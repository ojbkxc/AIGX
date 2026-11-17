// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 余额查询异步任务 —— `POST /usage/start`、`POST /usage/cancel`、`GET /usage`。
//!
//! 对齐 A2A `core/usage_query/jobs.rs` 的可观察语义：
//!
//! - `start` 只登记任务并立刻返回任务号（不等待上游，界面不卡住）；
//! - 同一账号已有 pending 任务时：`replace=false` 复用旧任务，
//!   `replace=true` 把旧任务标成 `superseded` 再起新的；
//! - `cancel` 必须同时给账号 id 与任务 requestId（防误杀新任务）；
//! - 任务结论保留 5 分钟（`RETAIN_SECONDS`），供界面补齐最后一拍轮询。
//!
//! ## 为什么不用 tokio_util 的 CancellationToken
//!
//! AIGX 的依赖里没有 tokio-util（也不在本任务的 Cargo.toml 权限内）。
//! `tokio::sync::watch` 的布尔通道就是最小等价物：`select!` 等它翻转，
//! 与 A2A 的 token 语义（只单向取消、不可复用）一致。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::watch;

use super::balance::{self, AccountBalance};
use super::model::AccountSession;
use super::store::AccountSessionStore;

/// 任务结论保留窗口（秒）。A2A `RETAIN_SECONDS` 同值。
pub const RETAIN_SECONDS: i64 = 300;

struct Job {
    account_id: String,
    request_id: String,
    started_at_ms: i64,
    timeout_seconds: u64,
    cancel: watch::Sender<bool>,
    result: watch::Sender<Option<Value>>,
}

impl Job {
    fn pending(&self) -> bool {
        self.result.borrow().is_none()
    }

    fn state(&self) -> Value {
        let result = self.result.borrow();
        let status = result
            .as_ref()
            .and_then(|row| row.get("queryStatus"))
            .and_then(Value::as_str)
            .unwrap_or("running");
        json!({
            "id": self.account_id,
            "requestId": self.request_id,
            "status": status,
            "error": result.as_ref().and_then(|row| row.get("error")),
            "startedAt": self.started_at_ms,
            "deadlineAt": self.started_at_ms + self.timeout_seconds as i64 * 1000,
        })
    }

    fn stop(&self, status: &str, message: Option<&str>) {
        if !self.pending() {
            return;
        }
        let _ = self.cancel.send(true);
        self.result.send_replace(Some(json!({
            "id": self.account_id,
            "requestId": self.request_id,
            "queryStatus": status,
            "usage": Value::Null,
            "error": message,
            "at": now_ms(),
        })));
    }

    fn finish(&self, outcome: Result<Value, String>) {
        if !self.pending() {
            return;
        }
        let row = match outcome {
            Ok(usage) => json!({
                "id": self.account_id,
                "requestId": self.request_id,
                "queryStatus": "success",
                "usage": usage,
                "error": Value::Null,
                "at": now_ms(),
            }),
            Err(message) => {
                let status = if message.contains("超时") {
                    "timeout"
                } else {
                    "failed"
                };
                json!({
                    "id": self.account_id,
                    "requestId": self.request_id,
                    "queryStatus": status,
                    "usage": Value::Null,
                    "error": message,
                    "at": now_ms(),
                })
            }
        };
        self.result.send_replace(Some(row));
    }
}

#[derive(Default)]
struct Registry {
    sequence: u64,
    jobs: HashMap<String, Arc<Job>>,
}

fn registry() -> MutexGuard<'static, Registry> {
    static STATE: OnceLock<Mutex<Registry>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(Registry::default()))
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn prune(registry: &mut Registry) {
    let now = now_ms();
    registry
        .jobs
        .retain(|_, job| job.pending() || now - job.started_at_ms < RETAIN_SECONDS * 1000);
}

/// 单次查询（带超时 + 重试）；任一次成功即收口。
async fn query_with_retries(
    client: &reqwest::Client,
    session: &AccountSession,
    retries: u32,
    timeout_seconds: u64,
) -> Result<AccountBalance, String> {
    let mut last = Err("余额查询未执行".to_string());
    for attempt in 0..=retries {
        match tokio::time::timeout(
            Duration::from_secs(timeout_seconds.max(1)),
            balance::query_balance(client, session),
        )
        .await
        {
            Ok(Ok(balance)) => return Ok(balance),
            Ok(Err(message)) => last = Err(message),
            Err(_) => last = Err(format!("余额查询超时（{timeout_seconds}s）")),
        }
        if attempt < retries {
            tokio::time::sleep(Duration::from_millis(300 * u64::from(attempt + 1))).await;
        }
    }
    last
}

async fn wait_cancelled(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// 起一个后台查询任务；返回 `(是否新建, 状态快照)`。
fn spawn_job(
    pool: &AccountSessionStore,
    client: &reqwest::Client,
    session: AccountSession,
    replace: bool,
) -> (bool, Value) {
    let mut guard = registry();
    prune(&mut guard);
    let config = session.usage_query.normalized();
    if let Some(previous) = guard.jobs.get(&session.id) {
        if previous.pending() && !replace {
            return (false, previous.state());
        }
        previous.stop("superseded", None);
    }
    guard.sequence = guard.sequence.wrapping_add(1);
    let request_id = format!("{}-{}", now_ms(), guard.sequence);
    let (cancel, cancel_rx) = watch::channel(false);
    let (result, _) = watch::channel(None);
    let job = Arc::new(Job {
        account_id: session.id.clone(),
        request_id: request_id.clone(),
        started_at_ms: now_ms(),
        timeout_seconds: config.timeout_seconds,
        cancel,
        result,
    });
    guard.jobs.insert(session.id.clone(), job.clone());
    let state = job.state();
    drop(guard);

    let pool = pool.clone();
    let client = client.clone();
    let mut cancel_rx = cancel_rx;
    tokio::spawn(async move {
        let outcome = tokio::select! {
            _ = wait_cancelled(&mut cancel_rx) => None,
            queried = query_with_retries(&client, &session, config.retries, config.timeout_seconds) => Some(queried),
        };
        match outcome {
            None => job.stop("cancelled", None),
            Some(Ok(balance)) => {
                let usage = usage_json(&balance);
                persist_outcome(&pool, &session.id, Ok(usage.clone()), now_ms() / 1000);
                job.finish(Ok(usage));
            }
            Some(Err(message)) => {
                persist_outcome(&pool, &session.id, Err(message.clone()), now_ms() / 1000);
                job.finish(Err(message));
            }
        }
    });
    (true, state)
}

/// 归一读数的快照形状（与 `GET /:id/balance` 的 data 同构）。
fn usage_json(balance: &AccountBalance) -> Value {
    json!({
        "available": balance.available,
        "unit": balance.unit,
        "wallets": balance.wallets,
        "subscription": balance.subscription,
    })
}

/// 查询结论写回（失败不清快照；写回前重读防冲掉数据面更新）。
fn persist_outcome(pool: &AccountSessionStore, id: &str, outcome: Result<Value, String>, now: i64) {
    let Some(mut current) = pool.get(id) else {
        return;
    };
    current.last_balance_at = Some(now);
    match outcome {
        Ok(usage) => {
            current.last_balance = Some(usage);
            current.last_balance_error = None;
        }
        Err(message) => current.last_balance_error = Some(message),
    }
    if let Err(error) = pool.upsert(current) {
        tracing::warn!("account {id} balance snapshot not saved: {error}");
    }
}

/// `POST /usage/start`：`{id?, replace?}` → `{jobs: [...], started: n}`。
pub fn start(
    pool: &AccountSessionStore,
    client: &reqwest::Client,
    id: Option<&str>,
    replace: bool,
) -> Value {
    let id = id.map(str::trim).filter(|id| !id.is_empty());
    let targets: Vec<AccountSession> = match id {
        Some(id) => pool
            .get(id)
            .filter(|session| balance::supports_balance(&session.provider))
            .into_iter()
            .collect(),
        None => pool
            .all()
            .into_iter()
            .filter(|session| balance::supports_balance(&session.provider))
            .collect(),
    };
    if id.is_some() && targets.is_empty() {
        return json!({
            "jobs": [],
            "started": 0,
            "error": "账号不存在或该供应商暂不支持余额查询",
        });
    }
    let mut jobs = Vec::with_capacity(targets.len());
    let mut started = 0usize;
    for session in targets {
        let (created, state) = spawn_job(pool, client, session, replace);
        if created {
            started += 1;
        }
        jobs.push(state);
    }
    json!({ "jobs": jobs, "started": started })
}

/// `POST /usage/cancel`：`{id, requestId}` → `{cancelled}`。
pub fn cancel(id: &str, request_id: &str) -> bool {
    let id = id.trim();
    let request_id = request_id.trim();
    if id.is_empty() || request_id.is_empty() {
        return false;
    }
    let guard = registry();
    let Some(job) = guard
        .jobs
        .get(id)
        .filter(|job| job.request_id == request_id)
    else {
        return false;
    };
    let pending = job.pending();
    job.stop("cancelled", None);
    pending
}

/// `GET /usage`：全部任务状态（含最近 5 分钟的终态）＋最近读数快照。
pub fn states(pool: &AccountSessionStore) -> Value {
    let mut guard = registry();
    prune(&mut guard);
    let jobs: Vec<Value> = guard.jobs.values().map(|job| job.state()).collect();
    drop(guard);
    let (results, skipped) = super::usage_query::snapshot_all(pool);
    json!({
        "at": chrono::Utc::now().timestamp(),
        "jobs": jobs,
        "results": results,
        "skipped": skipped,
    })
}

/// 外部写路径（重新登录/改凭证）使该账号的进行中任务失效（A2A 同语义）。
pub fn invalidate_account(id: &str) {
    if let Some(job) = registry().jobs.get(id) {
        job.stop("config_changed", Some("账号凭证已变更，任务作废"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_requires_matching_request_id() {
        // 不起真实上游：直接验证注册表语义（start 依赖支持余额的 provider）
        let pool = {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("aigx-usage-jobs-{}-{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            AccountSessionStore::new(Arc::new(crate::storage::FileStore::new(dir)))
        };
        // 单测里不构造 reqwest Client：本仓库的 rustls 走 `rustls-no-provider`
        // 特性，Client::new() 在测试进程里会 panic（运行时由 main 安装
        // crypto provider）。这里只钉注册表侧语义：未知账号/任务号取消恒 false，
        // 且不产生任何残留任务；上游查询路径由集成环境覆盖。
        assert!(!cancel("nope", "any"));
        assert!(!cancel("", "r"));
        assert!(states(&pool)["jobs"].as_array().unwrap().is_empty());
    }

    #[test]
    fn states_are_pruned_by_retention() {
        let mut registry = Registry::default();
        let (cancel, _rx) = watch::channel(false);
        let (result, _) = watch::channel(None);
        let job = Arc::new(Job {
            account_id: "a".into(),
            request_id: "r".into(),
            started_at_ms: now_ms() - (RETAIN_SECONDS + 1) * 1000,
            timeout_seconds: 1,
            cancel,
            result,
        });
        job.stop("success", None); // 已终态但超保留窗
        registry.jobs.insert("a".into(), job);
        prune(&mut registry);
        assert!(registry.jobs.is_empty());
    }
}
