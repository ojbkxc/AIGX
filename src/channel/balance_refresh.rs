//! 渠道型余额周期刷新巡检 —— 按渠道独立间隔（`balance_query_interval_secs`）
//! 定频拉取上游余额，写回渠道记录供界面展示。
//!
//! 对照账号池的 `account_session::usage_query`（per-account `usageQuery`）：
//! 每个渠道自定频（缺省 `None` = 关闭，opt-in），由巡检按「上次查询时间 +
//! 间隔」判到期后触发。设计要点与账号池同构：
//! - **失败不清快照**：查失败时保留上一次成功的余额，只更新
//!   `balance_last_error`（防上游抖动把界面上本来有数的余额擦成空白）；
//! - 账号型渠道（`channel_type = Account`）没有渠道级凭证，余额由账号池
//!   逐号查询（手动端点也拒，见 `api/admin/legacy.rs`），巡检一律跳过；
//! - 无可查凭证（api_key 空且方案要求的 quirks 凭证缺失）跳过，不发无意义请求；
//! - 有界并发（余额接口普遍 20s 超时，并发太高会把上游打疼）；
//! - 查询与手动端点共用 `ChannelStore::query_balance`（方案分派 / 凭证 /
//!   超时全复用），写回走 `save_balance_refresh`（不 bump `updated_at`）。

use futures::StreamExt;
use reqwest::Client;

use super::balance;
use super::{Channel, ChannelStore, ChannelType};

/// 一轮巡检里并发查询的渠道数（余额接口普遍 20s 超时，别把上游打疼）。
/// 与账号池 `usage_query::CONCURRENCY` 同口径。
const CONCURRENCY: usize = 3;

/// 该渠道是否到期需要查余额（纯函数，单测重点）。
///
/// 判定口径（对照 `usage_query::is_due`）：
/// - 间隔缺省 / 0 = 关闭（opt-in），一律跳过；
/// - 账号型渠道跳过（余额由账号池逐号查）；
/// - 无可查凭证跳过（不发必然失败的请求）；
/// - 从未查过 → 首次即查；否则按「上次查询时间 + 间隔」判到期。
pub fn is_due(ch: &Channel, now: i64) -> bool {
    let Some(interval) = ch.balance_query_interval_secs.filter(|v| *v > 0) else {
        return false;
    };
    if ch.channel_type == ChannelType::Account {
        return false;
    }
    if !has_balance_credentials(ch) {
        return false;
    }
    match ch.balance_last_query_at {
        None => true,
        Some(at) => now.saturating_sub(at) >= interval as i64,
    }
}

/// 是否有可用于余额查询的凭证：api_key 非空，或方案在 `quirks.headers`
/// 里约定的控制台凭证非空（对照账号池 `supports_balance` 的过滤口径；
/// 凭证约定见 `balance::credential_field`）。
fn has_balance_credentials(ch: &Channel) -> bool {
    if !ch.api_key.trim().is_empty() {
        return true;
    }
    balance::credential_field(ch.balance_scheme.trim()).is_some_and(|field| {
        ch.quirks
            .headers
            .get(field)
            .is_some_and(|v| !v.trim().is_empty())
    })
}

/// 巡检一轮：查所有到期渠道的余额并写回。返回成功查询数。
///
/// 单渠道失败不影响其他渠道（`buffer_unordered` 逐项独立处理）。
pub async fn sweep(store: &ChannelStore, client: &Client) -> u64 {
    let now = chrono::Utc::now().timestamp();
    let targets: Vec<Channel> = store
        .list()
        .into_iter()
        .filter(|ch| is_due(ch, now))
        .collect();
    if targets.is_empty() {
        return 0;
    }

    let results = futures::stream::iter(targets)
        .map(|ch| {
            let client = client.clone();
            async move {
                let outcome = store.query_balance(&ch, &client).await;
                (ch.id, outcome)
            }
        })
        .buffer_unordered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut done = 0u64;
    for (id, outcome) in results {
        // 写回前不再重读：查询期间渠道被删时 save_balance_refresh 静默跳过；
        // 渠道配置若被改（间隔/凭证），本轮仍按 list 快照的口径查，下轮生效。
        store.save_balance_refresh(&id, now, outcome);
        if outcome.is_ok() {
            done += 1;
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::super::ChannelQuirks;
    use super::*;
    use crate::storage::FileStore;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::TempDir;

    fn store() -> ChannelStore {
        ChannelStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    /// 测试渠道：api_key 预编码成 `enc:` 旧格式（纯 base64 解码，
    /// 不依赖 data_dir；`add()` 见 `enc:` 前缀也不再走 seal）。
    fn sample(id: &str, base_url: &str) -> Channel {
        Channel {
            id: id.to_string(),
            name: format!("ch-{id}"),
            channel_type: ChannelType::OpenaiCompatible,
            base_url: base_url.to_string(),
            api_key: Channel::encode_api_key("sk-test"),
            priority: 10,
            weight: 1,
            status: "enabled".to_string(),
            models: vec!["deepseek-chat".to_string()],
            model_mapping: HashMap::new(),
            cost_pricing: HashMap::new(),
            account_id: String::new(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            response_time: None,
            test_time: None,
            balance: None,
            credit: None,
            quirks: ChannelQuirks::default(),
            balance_scheme: String::new(),
            balance_unit: String::new(),
            balance_query_interval_secs: None,
            balance_last_query_at: None,
            balance_last_error: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    /// 配了间隔的渠道（首次即查）。
    fn due_channel(id: &str) -> Channel {
        let mut ch = sample(id, "https://api.deepseek.com");
        ch.balance_query_interval_secs = Some(3600);
        ch
    }

    // ── is_due 纯函数（§3 第 1-6 条）─────────────────────────────

    #[test]
    fn is_due_skips_when_no_interval() {
        // 缺省（None）= 关闭：永不到期
        let ch = sample("a", "https://api.deepseek.com");
        assert!(!is_due(&ch, 1_000_000));
        // 显式 0 = 关闭
        let mut ch0 = ch;
        ch0.balance_query_interval_secs = Some(0);
        assert!(!is_due(&ch0, 1_000_000));
    }

    #[test]
    fn is_due_first_run_when_never_queried() {
        let ch = due_channel("a");
        assert!(is_due(&ch, 1_000_000));
    }

    #[test]
    fn is_due_elapsed_boundary() {
        let mut ch = due_channel("a");
        ch.balance_last_query_at = Some(1_000_000);
        // 未到期（差 1 秒）
        assert!(!is_due(&ch, 1_000_000 + 3_599));
        // 到期边界（正好 == interval）
        assert!(is_due(&ch, 1_000_000 + 3_600));
        // 超期更到期
        assert!(is_due(&ch, 1_000_000 + 3_601));
    }

    #[test]
    fn is_due_skips_account_type() {
        // 账号型渠道即使配了间隔也不查（余额由账号池逐号查）
        let mut ch = due_channel("a");
        ch.channel_type = ChannelType::Account;
        assert!(!is_due(&ch, 1_000_000));
    }

    #[test]
    fn is_due_skips_without_credentials() {
        // api_key 空 + 空 scheme（走 OpenAI 计费端点需要 key）→ 不查
        let mut ch = due_channel("a");
        ch.api_key = String::new();
        assert!(!is_due(&ch, 1_000_000));
        // 有 key → 查
        assert!(is_due(&due_channel("b"), 1_000_000));
        // 方案需要 quirks 凭证（new_api）：没配 → 不查
        let mut ch2 = due_channel("c");
        ch2.api_key = String::new();
        ch2.balance_scheme = "new_api".to_string();
        assert!(!is_due(&ch2, 1_000_000));
        // 配上了 → 查
        let mut headers = BTreeMap::new();
        headers.insert("x-balance-access-token".to_string(), "tok".to_string());
        ch2.quirks.headers = headers;
        assert!(is_due(&ch2, 1_000_000));
    }

    // ── sweep 全链路（§3 第 7-10 条）────────────────────────────

    /// 本地 HTTP mock：固定状态行 + 体（可选延迟），返回 base_url 与
    /// 在飞峰值计数器。空 scheme 走 OpenAI 计费端点（首个端点
    /// `/dashboard/billing/subscription`，见 balance.rs）。
    struct Mock {
        base_url: String,
        in_flight_max: Arc<AtomicUsize>,
    }

    fn spawn_mock(status_line: &'static str, body: &'static str, delay_ms: u64) -> Mock {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let in_flight = Arc::new(AtomicUsize::new(0));
        let in_flight_max = Arc::new(AtomicUsize::new(0));
        let (if_cur, if_max) = (Arc::clone(&in_flight), Arc::clone(&in_flight_max));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let if_cur = Arc::clone(&if_cur);
                let if_max = Arc::clone(&if_max);
                let body = body.to_string();
                std::thread::spawn(move || {
                    read_request_head(&mut stream);
                    let cur = if_cur.fetch_add(1, Ordering::Relaxed) + 1;
                    let mut prev = if_max.load(Ordering::Relaxed);
                    while cur > prev {
                        match if_max.compare_exchange_weak(
                            prev,
                            cur,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(actual) => prev = actual,
                        }
                    }
                    if delay_ms > 0 {
                        std::thread::sleep(Duration::from_millis(delay_ms));
                    }
                    let response = format!(
                        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                    if_cur.fetch_sub(1, Ordering::Relaxed);
                });
            }
        });
        Mock {
            base_url: format!("http://127.0.0.1:{port}"),
            in_flight_max,
        }
    }

    /// 读到请求头结束（\r\n\r\n）为止——GET 无体，读到即完整。
    fn read_request_head(stream: &mut std::net::TcpStream) {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => n,
            };
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                return;
            }
        }
    }

    #[tokio::test]
    async fn sweep_queries_only_due_channels() {
        let mock = spawn_mock(
            "HTTP/1.1 200 OK",
            "{\"total_granted\":100,\"total_used\":40}",
            0,
        );
        let store = store();
        let mut due = sample("due", &mock.base_url);
        due.balance_query_interval_secs = Some(3600);
        store.add(due).unwrap();
        let mut not_due = sample("notdue", &mock.base_url);
        not_due.balance_query_interval_secs = Some(3600);
        not_due.balance_last_query_at = Some(chrono::Utc::now().timestamp());
        store.add(not_due).unwrap();

        let client = reqwest::Client::new();
        let done = sweep(&store, &client).await;
        assert_eq!(done, 1);

        let due_ch = store.get("due").unwrap();
        assert_eq!(due_ch.balance, Some(60.0));
        assert_eq!(due_ch.balance_unit, "USD");
        assert!(due_ch.balance_last_query_at.is_some());
        assert!(due_ch.balance_last_error.is_none());
        // 未到期的那条没被查：余额快照保持原样
        let nd = store.get("notdue").unwrap();
        assert!(nd.balance.is_none());
    }

    #[tokio::test]
    async fn sweep_failure_keeps_last_snapshot() {
        let mock = spawn_mock("HTTP/1.1 500 Internal Server Error", "boom", 0);
        let store = store();
        let mut ch = sample("a", &mock.base_url);
        ch.balance_query_interval_secs = Some(3600);
        ch.balance = Some(50.0); // 上次成功查到的余额
        ch.balance_unit = "USD".to_string();
        store.add(ch).unwrap();

        let client = reqwest::Client::new();
        let done = sweep(&store, &client).await;
        assert_eq!(done, 0);

        let ch = store.get("a").unwrap();
        assert_eq!(ch.balance, Some(50.0)); // 失败不清快照
        assert_eq!(ch.balance_unit, "USD");
        assert!(ch.balance_last_error.is_some()); // 记录失败原因
        assert!(ch.balance_last_query_at.is_some()); // 时间照记（防每轮重打）
    }

    #[tokio::test]
    async fn sweep_concurrency_capped() {
        // 5 条渠道同时到期：并发上限 3（mock 延迟拉长观察窗口）
        let mock = spawn_mock(
            "HTTP/1.1 200 OK",
            "{\"total_granted\":100,\"total_used\":0}",
            150,
        );
        let store = store();
        for i in 0..5 {
            let mut ch = sample(&format!("ch{i}"), &mock.base_url);
            ch.balance_query_interval_secs = Some(3600);
            store.add(ch).unwrap();
        }

        let client = reqwest::Client::new();
        let done = sweep(&store, &client).await;
        assert_eq!(done, 5);
        assert!(
            mock.in_flight_max.load(Ordering::Relaxed) <= 3,
            "并发峰值应 ≤ 3，实测 {}",
            mock.in_flight_max.load(Ordering::Relaxed)
        );
    }

    #[tokio::test]
    async fn sweep_survives_channel_deleted_mid_round() {
        let mock = spawn_mock(
            "HTTP/1.1 200 OK",
            "{\"total_granted\":100,\"total_used\":0}",
            200,
        );
        let store = store();
        let mut ch = sample("gone", &mock.base_url);
        ch.balance_query_interval_secs = Some(3600);
        store.add(ch).unwrap();

        let client = reqwest::Client::new();
        let sweep_future = sweep(&store, &client);
        // 查询在途时删掉渠道：写回找不到记录应静默跳过
        std::thread::sleep(Duration::from_millis(50));
        store.remove("gone").unwrap();
        let done = sweep_future.await;
        assert_eq!(done, 1); // 查询本身成功
        assert!(store.get("gone").is_none());
    }

    // ── serde 兼容与显式 null 契约（§3 第 11 条）─────────────────

    #[test]
    fn channel_serde_backward_compatible_and_explicit_null() {
        // 旧 JSON（无三个新键）反序列化 → 全 None
        let old = serde_json::json!({
            "id": "x", "name": "n", "base_url": "https://api.deepseek.com",
            "api_key": "sk", "priority": 1, "weight": 1, "status": "enabled",
            "models": ["m"], "created_at": 1, "updated_at": 1
        });
        let ch: Channel = serde_json::from_value(old).unwrap();
        assert!(ch.balance_query_interval_secs.is_none());
        assert!(ch.balance_last_query_at.is_none());
        assert!(ch.balance_last_error.is_none());

        // 未配间隔：键恒在（显式 null，前端契约——None = 跳过 = opt-in
        // 关闭，与「旧数据无此键」同一状态）
        let none_ch = sample("x", "https://api.deepseek.com");
        let json = serde_json::to_value(&none_ch).unwrap();
        assert_eq!(json["balance_query_interval_secs"], serde_json::Value::Null);
        assert_eq!(json["balance_last_query_at"], serde_json::Value::Null);
        assert_eq!(json["balance_last_error"], serde_json::Value::Null);

        // 新键往返一致
        let mut ch = due_channel("x");
        ch.balance_query_interval_secs = Some(7200);
        ch.balance_last_query_at = Some(123);
        ch.balance_last_error = Some("boom".to_string());
        let json = serde_json::to_value(&ch).unwrap();
        assert_eq!(json["balance_query_interval_secs"], 7200);
        assert_eq!(json["balance_last_query_at"], 123);
        assert_eq!(json["balance_last_error"], "boom");
        let back: Channel = serde_json::from_value(json).unwrap();
        assert_eq!(back.balance_query_interval_secs, Some(7200));
        assert_eq!(back.balance_last_query_at, Some(123));
        assert_eq!(back.balance_last_error.as_deref(), Some("boom"));
    }

    // ── save_balance_refresh 两分支（§3 第 12 条）────────────────

    #[test]
    fn save_balance_refresh_ok_branch_without_bumping_updated_at() {
        let store = store();
        let ch = sample("a", "https://api.deepseek.com");
        store.add(ch).unwrap();
        let before = store.get("a").unwrap().updated_at;

        let reading = super::super::balance::BalanceReading {
            available: Some(60.0),
            unit: "USD".to_string(),
            wallets: vec![],
            raw: serde_json::json!({}),
        };
        store.save_balance_refresh("a", 1_000_000, Ok(reading));

        let ch = store.get("a").unwrap();
        assert_eq!(ch.balance, Some(60.0));
        assert_eq!(ch.credit, None);
        assert_eq!(ch.balance_unit, "USD");
        assert!(ch.balance_last_error.is_none()); // 成功后清空为 None
        assert_eq!(ch.balance_last_query_at, Some(1_000_000));
        // 关键回归钉：周期读数不 bump updated_at（lead 拍板）
        assert_eq!(ch.updated_at, before);
    }

    #[test]
    fn save_balance_refresh_err_branch_keeps_snapshot() {
        let store = store();
        let ch = sample("a", "https://api.deepseek.com");
        store.add(ch).unwrap();
        let before = store.get("a").unwrap().updated_at;

        store.save_balance_refresh("a", 1_000_000, Err("上游返回 HTTP 500".to_string()));

        let ch = store.get("a").unwrap();
        assert_eq!(ch.balance_last_error.as_deref(), Some("上游返回 HTTP 500"));
        assert_eq!(ch.balance_last_query_at, Some(1_000_000));
        assert_eq!(ch.updated_at, before); // 失败也不 bump
    }

    #[test]
    fn save_balance_refresh_unknown_id_is_noop() {
        let store = store();
        // 渠道在查询期间被删：写回静默跳过，不 panic
        store.save_balance_refresh("nope", 1_000_000, Err("x".to_string()));
    }
}
