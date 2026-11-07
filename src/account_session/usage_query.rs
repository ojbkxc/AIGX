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

use super::balance::{self, AccountBalance};
use super::model::AccountSession;
use super::store::AccountSessionStore;

/// 一轮巡检里并发查询的账号数（余额接口普遍 20s 超时，并发太高会把上游打疼）。
const CONCURRENCY: usize = 3;

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

    let results = futures::stream::iter(targets)
        .map(|session| {
            let client = client.clone();
            async move {
                let outcome = balance::query_balance(&client, &session).await;
                (session.id, outcome)
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

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
/// 有界并发复用 [`CONCURRENCY`]（余额接口普遍 20s 超时，别把上游打疼）；
/// 逐账号写回快照（同 [`sweep`] 口径）。返回 (results, skipped)——skipped
/// 是「凭证不完整等被跳过」的数量，与上游同义。
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
    let results = futures::stream::iter(targets)
        .map(|session| {
            let client = client.clone();
            async move {
                let outcome = balance::query_balance(&client, &session).await;
                (session.id, session.name, session.provider, outcome)
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
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
    let mut results = Vec::new();
    let mut skipped = 0usize;
    for session in pool.all() {
        if !balance::supports_balance(&session.provider) {
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
}
