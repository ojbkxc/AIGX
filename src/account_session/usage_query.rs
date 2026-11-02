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

use reqwest::Client;

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
