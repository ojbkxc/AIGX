//! CF quota monitoring ? periodic neurons consumption check + alerts.
//!
//! ?? new-api channel-billing.go ??????????
//! ???? graphql::query_usage_summary ??? CF ?? neurons ???
//! ??????? NotifyService ?????

use std::sync::Arc;
use std::time::Duration;

use crate::account::AccountPool;
use crate::notify::NotifyService;

/// CF Workers AI ???? neurons ????? CF ??????
/// ???? CF ???????????
const NEURONS_DAILY_LIMIT: u64 = 10_000;
const NEURONS_ALERT_THRESHOLD: f64 = 0.8; // 80% ????

/// ???? quota ?????? 5 ????????
pub fn spawn_monitor(
    account_pool: Arc<AccountPool>,
    notify_service: Arc<NotifyService>,
    http_client: reqwest::Client,
) {
    tokio::spawn(async move {
        let interval = Duration::from_secs(300);
        loop {
            tokio::time::sleep(interval).await;
            monitor_once(&account_pool, &notify_service, &http_client).await;
        }
    });
}

async fn monitor_once(
    account_pool: &AccountPool,
    notify_service: &Arc<NotifyService>,
    http_client: &reqwest::Client,
) {
    let accounts = account_pool.list();
    for account in &accounts {
        if account.status != "active" {
            continue;
        }
        match crate::graphql::query_usage_summary(account, http_client).await {
            Ok(summary) => {
                let usage_ratio = summary.today_neurons as f64 / NEURONS_DAILY_LIMIT as f64;
                if usage_ratio >= NEURONS_ALERT_THRESHOLD {
                    tracing::warn!(
                        "CF account {} neurons usage: {}/{} ({:.1}%)",
                        account.name,
                        summary.today_neurons,
                        NEURONS_DAILY_LIMIT,
                        usage_ratio * 100.0
                    );
                    notify_service.notify_spawn(crate::notify::NotifyEvent::ChannelFailure {
                        channel_name: format!("CF-{} quota", account.name),
                        error: format!(
                            "neurons usage {}/{} ({:.0}%) approaching limit",
                            summary.today_neurons, NEURONS_DAILY_LIMIT, usage_ratio * 100.0
                        ),
                    });
                }
                tracing::debug!(
                    "CF account {} quota check: today neurons={}, month neurons={}",
                    account.name, summary.today_neurons, summary.neurons
                );
            }
            Err(e) => {
                tracing::warn!("CF quota query failed for {}: {e}", account.name);
            }
        }
    }
}
