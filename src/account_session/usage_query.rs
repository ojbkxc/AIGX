//! 余额自动查询巡检 —— 按账号独立间隔定频拉取上游余额。
//!
//! 对照 Agent2API 的 per-account `usageQuery`（`core/usage_query.rs` 的
//! `spawn_sweeper`）：每个账号自定频（[`AccountSession::usage_query_interval_secs`]），
//! 由本模块的巡检按「上次查询时间 + 间隔」判到期后触发。
//!
//! 设计要点：
//! - **失败不清快照**：查失败时保留上一次成功的结果，只更新 `last_balance_error`，
//!   避免上游抖动把界面上本来有数的余额擦成空白；
//! - **写回前重读**：查询期间数据面可能更新了冷却/统计，直接整条覆盖会把它们冲掉，
//!   所以按 id 重读后再补两个字段；
//! - 只处理 `supports_balance` 的家，其余账号即使配了间隔也跳过（不发无意义请求）。

use futures::StreamExt;
use reqwest::Client;

use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use super::balance::{self, AccountBalance};
use super::model::AccountSession;
use super::store::AccountSessionStore;

/// 一轮巡检里并发查询的账号数（余额接口普遍 20s 超时，并发太高会把上游打疼）。
const CONCURRENCY: usize = 3;

/// 单家限流闸：保证同一 provider 相邻两次查询的**启动**间隔不小于申报值。
///
/// 进程级、按 provider 一实例（见 [`gate_for`]）——**所有**发起余额查询的路径
/// （自动巡检 [`sweep`]、手动 [`query_manual`]、异步任务 `usage_jobs`）共用同一
/// 把闸。此前每轮 `sweep` 内 new 一个：既让手动路径完全绕开限流，也让
/// `min_interval` 只约束「本轮内相邻启动」，跨轮不生效。
///
/// 只锁「取号」这一段（几微秒），请求本身在锁外并发执行 —— 它限制的是启动
/// 间隔与在飞数量，不是串行化整轮查询。`min_interval = 0` 时直接放行（未申报
/// 策略的家零额外开销）。
///
/// `pub(super)`：`usage_jobs` 经 [`gate_for`] 拿到 `Arc<ProviderGate>` 再调
/// `wait_turn`。类型若保持私有，`gate_for` 的返回类型就是 `private type`
///（E0603，且 `private_interfaces` 告警在 `-D warnings` 下同样致命）。
pub(super) struct ProviderGate {
    /// 申报的最小启动间隔（毫秒）。用原子量而非构造期常量：运行中插件重新
    /// 申报策略时，[`gate_for`] 会就地更新，不必重建闸（重建会把排队者丢掉）。
    min_interval_ms: std::sync::atomic::AtomicU64,
    /// 下一次可启动时刻（预占推进）。只存 Instant，不跨 await 持锁。
    next_ready: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl ProviderGate {
    fn new(min_interval_ms: u64) -> Self {
        Self {
            min_interval_ms: std::sync::atomic::AtomicU64::new(min_interval_ms),
            next_ready: tokio::sync::Mutex::new(None),
        }
    }

    fn set_min_interval(&self, min_interval_ms: u64) {
        self.min_interval_ms
            .store(min_interval_ms, std::sync::atomic::Ordering::Relaxed);
    }

    /// 取一个启动号：锁内只推进 `next_ready`，锁外 sleep。
    ///
    /// 之前的实现把 `sleep` 放在持锁段里，等待方全部串行堵在锁上 —— 语义上
    /// 仍是 0/120/240… 的错峰，但每个等待者都要排队拿锁，抖动更大。现在每个
    /// 调用者先把自己要等的时长算出来（并预占下一个窗口），再各自 sleep。
    ///
    /// 可见性 `pub(super)`：`usage_jobs`（同一父模块下的兄弟）也要用同一把闸，
    /// 三条路径（巡检 / 手动 / 异步任务）对同一家上游的启动节奏必须一致。
    pub(super) async fn wait_turn(&self) {
        let interval = self
            .min_interval_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        if interval == 0 {
            return;
        }
        let wait = {
            let mut guard = self.next_ready.lock().await;
            let now = tokio::time::Instant::now();
            let ready = match *guard {
                Some(previous) => (previous + Duration::from_millis(interval)).max(now),
                None => now,
            };
            *guard = Some(ready);
            ready.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }
}

/// 取该 provider 的进程级闸（首次创建，之后复用；顺带刷新申报值）。
///
/// `min_interval_ms` 每次从 `usage_policy` 重读：插件在运行中新装/卸载或改申报
/// 时，闸的行为立刻跟着变，不需要重启进程。
pub(super) fn gate_for(provider: &str) -> Arc<ProviderGate> {
    static GATES: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Arc<ProviderGate>>>,
    > = std::sync::OnceLock::new();
    let declared = crate::plugin_host::usage_policy::for_provider(provider).min_interval_ms;
    let gates = GATES.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let mut guard = gates.lock().unwrap_or_else(|error| error.into_inner());
    let gate = guard
        .entry(provider.to_string())
        .or_insert_with(|| Arc::new(ProviderGate::new(declared)))
        .clone();
    drop(guard);
    gate.set_min_interval(declared);
    gate
}

/// 该 provider 的并发上限：`min(宿主 CONCURRENCY, 插件申报 concurrency)`。
pub(super) fn provider_cap(provider: &str) -> usize {
    let declared = crate::plugin_host::usage_policy::for_provider(provider).concurrency;
    declared
        .map_or(CONCURRENCY, |value| value.min(CONCURRENCY))
        .max(1)
}

/// 单账号查询（带「每账号」的超时 + 重试；口径与 `usage_jobs` 的
/// `query_with_retries` 一致，任一次成功即收口）。
///
/// 巡检（[`sweep`]）与手动查询（[`query_manual`]）都走这里：`usageQuery`
/// 的 `retries` / `timeoutSeconds` 对所有查询路径同源生效（上游同义）。
async fn query_with_settings(
    client: &Client,
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

/// 该账号是否到期需要查余额。
pub fn is_due(session: &AccountSession, now: i64) -> bool {
    let Some(interval) = session.usage_query_interval_secs.filter(|v| *v > 0) else {
        return false;
    };
    if !balance::supports_balance(&session.provider) {
        return false;
    }
    match session.last_balance_at {
        None => true,
        Some(at) => now.saturating_sub(at) >= interval as i64,
    }
}

/// 归一读数的快照形状（与 `GET /api/account-sessions/:id/balance` 的 data 同构）。
fn snapshot(balance: &AccountBalance) -> serde_json::Value {
    serde_json::json!({
        "available": balance.available,
        "unit": balance.unit,
        "wallets": balance.wallets,
        "subscription": balance.subscription,
    })
}

/// 巡检一轮：查所有到期账号的余额并写回快照。返回成功查询数。
///
/// **按家限流**（对齐上游 v3.5.1 `usage_policy`）：到期账号先按 provider 分组，
/// 同一家的并发上限 = `min(宿主 CONCURRENCY, 插件申报 concurrency)`，相邻两次
/// 查询启动间隔 ≥ 插件申报的 `minIntervalMs`。组间顺序执行，未申报策略的家行为
/// 与从前逐字节一致。闸是**进程级**的（[`gate_for`]），与手动查询、`usage_jobs`
/// 共用 —— 三条路径对同一家上游的启动节奏因此一致。
pub async fn sweep(pool: &AccountSessionStore, client: &Client) -> u64 {
    let now = chrono::Utc::now().timestamp();
    let targets: Vec<AccountSession> = pool
        .all()
        .into_iter()
        .filter(|session| is_due(session, now))
        .collect();
    if targets.is_empty() {
        return 0;
    }

    let mut by_provider: std::collections::BTreeMap<String, Vec<AccountSession>> =
        std::collections::BTreeMap::new();
    for session in targets {
        by_provider
            .entry(session.provider.clone())
            .or_default()
            .push(session);
    }

    let mut results = Vec::new();
    for (provider, group) in by_provider {
        // 进程级闸 + 按家并发上限：与手动查询、异步任务共用同一套口径。
        let cap = provider_cap(&provider);
        let gate = gate_for(&provider);
        let batch = futures::stream::iter(group)
            .map(|session| {
                let client = client.clone();
                let gate = gate.clone();
                async move {
                    // 同一家的相邻请求先排队（间隔不够就补足），再并发执行
                    gate.wait_turn().await;
                    let config = session.usage_query.normalized();
                    let outcome = query_with_settings(
                        &client,
                        &session,
                        config.retries,
                        config.timeout_seconds,
                    )
                    .await;
                    (session.id, outcome)
                }
            })
            .buffer_unordered(cap)
            .collect::<Vec<_>>()
            .await;
        results.extend(batch);
    }

    let mut done = 0u64;
    for (id, outcome) in results {
        // 重读：查询期间数据面可能更新了冷却/成功率，整条覆盖会把它们冲掉
        let Some(mut current) = pool.get(&id) else {
            continue;
        };
        current.last_balance_at = Some(now);
        match outcome {
            Ok(balance) => {
                current.last_balance = Some(snapshot(&balance));
                current.last_balance_error = None;
                done += 1;
            }
            Err(message) => {
                tracing::warn!("account {id} balance query failed: {message}");
                current.last_balance_error = Some(message);
            }
        }
        if let Err(error) = pool.upsert(current) {
            tracing::warn!("account {id} balance snapshot not saved: {error}");
        }
    }
    done
}

// ── 手动查询与快照（Phase1-A3，对齐上游 accounts_usage.rs）─────────

/// 一行查询结论（手动与快照同形，快照行多一个自己的 `at`）。
fn result_row(
    id: &str,
    name: &str,
    provider: &str,
    usage: Option<Value>,
    error: Option<String>,
) -> Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "provider": provider,
        "usage": usage,
        "error": error,
    })
}

/// 写回一行查询结论（失败不清快照；写回前重读防冲掉数据面更新）。
fn persist_outcome(
    pool: &AccountSessionStore,
    id: &str,
    outcome: Result<AccountBalance, String>,
    now: i64,
) {
    let Some(mut current) = pool.get(id) else {
        return;
    };
    current.last_balance_at = Some(now);
    match outcome {
        Ok(balance) => {
            current.last_balance = Some(snapshot(&balance));
            current.last_balance_error = None;
        }
        Err(message) => {
            current.last_balance_error = Some(message);
        }
    }
    if let Err(error) = pool.upsert(current) {
        tracing::warn!("account {id} balance snapshot not saved: {error}");
    }
}

/// 手动批量/单账号余额查询（对齐上游 `query_all` 语义）：
/// - `id` 缺省 = 全部**支持查询**的账号（不看启用状态——禁用只管转发，
///   与余额查询无关，照抄上游说明）；
/// - `id` 给定 = 只查这一个（单行「余额」按钮），未知 id 记 not_found 行。
///
/// 有界并发**按家**限流（与 [`sweep`] 同口径）：组内
/// `min(CONCURRENCY, 申报 concurrency)`，相邻启动间隔 ≥ 申报值，组间顺序执行。
/// 逐账号写回快照。返回 (results, skipped)——skipped 是「凭证不完整等被跳过」
/// 的数量，与上游同义。
pub async fn query_manual(
    pool: &AccountSessionStore,
    client: &Client,
    id: Option<&str>,
) -> (Vec<Value>, usize) {
    let now = chrono::Utc::now().timestamp();
    let targets: Vec<AccountSession> = match id {
        Some(id) => pool
            .get(id)
            .filter(|s| balance::supports_balance(&s.provider))
            .into_iter()
            .collect(),
        None => pool
            .all()
            .into_iter()
            .filter(|s| balance::supports_balance(&s.provider))
            .collect(),
    };
    if id.is_some() && targets.is_empty() {
        return (
            vec![result_row(
                id.unwrap_or_default(),
                "",
                "",
                None,
                Some("账号不存在或该供应商暂不支持余额查询".to_string()),
            )],
            0,
        );
    }
    // 按家分组后组内限流的**并发**执行：手动路径此前是全局 `CONCURRENCY`
    // 一把梭 —— 200 个账号点「查询全部余额」= 200 路打向十几家上游，而定时
    // 巡检却是按家限流的（同产品里两条口径互相矛盾）。这里与 [`sweep`] 同源：
    // 组内 `min(CONCURRENCY, 申报 concurrency)` + 相邻启动间隔 ≥ 申报值。
    let mut by_provider: std::collections::BTreeMap<String, Vec<AccountSession>> =
        std::collections::BTreeMap::new();
    for session in targets {
        by_provider
            .entry(session.provider.clone())
            .or_default()
            .push(session);
    }
    let mut collected = Vec::new();
    for (provider, group) in by_provider {
        let cap = provider_cap(&provider);
        let gate = gate_for(&provider);
        let batch = futures::stream::iter(group)
            .map(|session| {
                let client = client.clone();
                let gate = gate.clone();
                async move {
                    gate.wait_turn().await;
                    let config = session.usage_query.normalized();
                    let outcome = query_with_settings(
                        &client,
                        &session,
                        config.retries,
                        config.timeout_seconds,
                    )
                    .await;
                    (session.id, session.name, session.provider, outcome)
                }
            })
            .buffer_unordered(cap)
            .collect::<Vec<_>>()
            .await;
        collected.extend(batch);
    }
    let results = collected;
    let mut rows = Vec::with_capacity(results.len());
    for (id, name, provider, outcome) in results {
        let (usage, error) = match &outcome {
            Ok(balance) => (Some(snapshot(balance)), None),
            Err(message) => (None, Some(message.clone())),
        };
        persist_outcome(pool, &id, outcome, now);
        rows.push(result_row(&id, &name, &provider, usage, error));
    }
    (rows, 0)
}

/// 各账号最近一次查询结论快照（对齐上游 `usage_query::snapshot`）：
/// 界面轮询这一份即可跟上自动/手动两条来源的节奏。行带自己的 `at`
///（unix 秒；None = 从未查过，不出现在 results）。skipped = 支持查询
/// 但从未查过的账号数（界面可显示「n 个未查询」）。
pub fn snapshot_all(pool: &AccountSessionStore) -> (Vec<Value>, usize) {
    snapshot_rows(pool.all(), None)
}

/// 指定 id 集合的最近结论快照（上游 `GET /api/accounts/usage/snapshot?ids=`
/// 与 `POST /api/accounts/usage/snapshot {ids}` 的 scoped 形态）。
///
/// 只回给定的 id；未知 id 自然不在结果里，**支持余额但从未查过**的计入
/// `skipped`（与全量快照同义）。A2A 的 `stale`（查询配置指纹漂移）在 AIGX
/// 无对应事实（不存指纹），不伪造该字段。
pub fn snapshot_scoped(pool: &AccountSessionStore, ids: &[String]) -> (Vec<Value>, usize) {
    let wanted: std::collections::HashSet<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| !id.is_empty())
        .collect();
    snapshot_rows(pool.all(), Some(&wanted))
}

fn snapshot_rows(
    sessions: Vec<AccountSession>,
    wanted: Option<&std::collections::HashSet<&str>>,
) -> (Vec<Value>, usize) {
    let mut results = Vec::new();
    let mut skipped = 0usize;
    for session in sessions {
        if let Some(wanted) = wanted {
            if !wanted.contains(session.id.as_str()) {
                continue;
            }
            // scoped 请求里「不支持余额」的账号也算跳过（让界面能说清为什么没数）
            if !balance::supports_balance(&session.provider) {
                skipped += 1;
                continue;
            }
        } else if !balance::supports_balance(&session.provider) {
            continue;
        }
        match (session.last_balance_at, session.last_balance.clone()) {
            (Some(at), usage) => results.push(serde_json::json!({
                "id": session.id,
                "name": session.name,
                "provider": session.provider,
                "usage": usage,
                "error": session.last_balance_error,
                "at": at,
            })),
            (None, _) => skipped += 1,
        }
    }
    (results, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_only_when_interval_set_and_supported() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "qoder".to_string();
        // 没配间隔：从来不查
        assert!(!is_due(&session, 1_000_000));
        session.usage_query_interval_secs = Some(3600);
        // 从没查过 → 立刻到期
        assert!(is_due(&session, 1_000_000));
        session.last_balance_at = Some(1_000_000);
        assert!(!is_due(&session, 1_000_000 + 3_599));
        assert!(is_due(&session, 1_000_000 + 3_600));
        // 不支持余额的家即使配了间隔也不查
        session.provider = "openai".to_string();
        assert!(!is_due(&session, 1_000_000 + 100_000));
    }

    /// 单家限流闸：第二次放行必须等满 min_interval。
    #[tokio::test]
    async fn provider_gate_enforces_min_interval() {
        let gate = Arc::new(ProviderGate::new(120));
        let started = tokio::time::Instant::now();
        gate.wait_turn().await;
        let first = started.elapsed();
        gate.wait_turn().await;
        let second = started.elapsed();
        assert!(first < Duration::from_millis(80), "首次不该等待: {first:?}");
        assert!(
            second >= Duration::from_millis(120),
            "第二次必须等满间隔: {second:?}"
        );
        assert!(
            second < Duration::from_millis(400),
            "不该被额外拖长: {second:?}"
        );
    }

    /// 并发等待者按号次错峰，且都不在持锁段里睡（三个一起抢）。
    #[tokio::test]
    async fn provider_gate_spaces_concurrent_waiters() {
        let gate = Arc::new(ProviderGate::new(60));
        let start = tokio::time::Instant::now();
        let mut handles = Vec::new();
        for _ in 0..3 {
            let gate = gate.clone();
            handles.push(tokio::spawn(async move {
                gate.wait_turn().await;
                start.elapsed()
            }));
        }
        let mut elapsed = Vec::new();
        for handle in handles {
            if let Ok(value) = handle.await {
                elapsed.push(value);
            }
        }
        elapsed.sort();
        assert_eq!(elapsed.len(), 3, "三个等待者都必须放行");
        assert!(
            elapsed[1] >= Duration::from_millis(50),
            "第二个必须错峰: {elapsed:?}"
        );
        assert!(
            elapsed[2] >= Duration::from_millis(110),
            "第三个必须再错一格: {elapsed:?}"
        );
    }

    /// 间隔为 0（未申报策略）→ 直接放行，零等待。
    #[tokio::test]
    async fn provider_gate_zero_interval_never_waits() {
        let gate = Arc::new(ProviderGate::new(0));
        let started = tokio::time::Instant::now();
        for _ in 0..50 {
            gate.wait_turn().await;
        }
        assert!(
            started.elapsed() < Duration::from_millis(60),
            "0 间隔不该累积等待: {:?}",
            started.elapsed()
        );
    }
}
