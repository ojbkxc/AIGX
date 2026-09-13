//! 只读诊断端点套件 — AI 值守运维三件套（AI 可运维第①步）。
//!
//! 定位：让运维 AI 不需要 SSH + 读源码，一次 HTTP 调用看清系统健康：
//! - `GET /api/diagnostics/summary`   系统体检摘要（值守快速判断"有没有事"）
//! - `GET /api/diagnostics/channels`   渠道批量测活（并发探测，只探测不落状态）
//! - `GET /api/diagnostics/breakers`   熔断器状态快照（含 HalfOpen 单飞租约）
//!
//! 只读边界（全部端点）：不写断路器、不改渠道状态、不落测试结果、
//! 不触发计费/限流；写操作（断路器 reset、渠道修复）属第二步，本模块
//! 一个写操作都不做。
//!
//! 数据源复用（不新造统计口径）：
//! - 渠道统计：`ChannelStore::list` + `CircuitBreaker::get_status_map`
//!   + `ChannelStore::is_in_cooldown`（与 `dashboard/channel_health` 同源）
//! - 今日用量：`RequestLogStore::all_sorted_asc` 内存聚合（与 `dashboard.rs`
//!   看板聚合同口径）
//! - 渠道测活：`ChannelStore::test`（该函数本身只发 HTTP 探测并返回结果，
//!   无任何状态写入；`/api/channels/:id/test` 落库的 save_test_result/
//!   mark_healthy/mark_unhealthy 均在其 handler 层，此处不复制）
//! - 熔断快照：`CircuitBreaker::snapshot_all`（circuit_breaker.rs 只读方法）

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use futures::future::join_all;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::channel::ChannelTestResult;
use chrono::TimeZone;

/// 渠道批量测活的整体 deadline（秒）。
///
/// 单渠道沿用 `ChannelStore::test` 内置的 15s reqwest 超时；并发执行下
/// 正常一轮 ≈ 15s 内完成，30s 是外层兜底（防 client 构建等极端场景
/// 拖死管理面请求）。超时返回 504，不返回部分结果。
const PROBE_BATCH_DEADLINE_SECS: u64 = 30;

/// 错误概况最多展示的错误类型数。
const TOP_ERRORS_LIMIT: usize = 5;

/// 单条错误消息截断长度（防上游返回超长错误体撑爆响应）。
const ERROR_MSG_MAX_LEN: usize = 200;

/// GET /api/diagnostics/summary — 系统体检摘要。
///
/// 一次调用聚合四类信息，供值守 AI 快速判断系统是否正常：
/// 1. 渠道统计（总数/启用/熔断中/半开/冷却中）
/// 2. 今日用量摘要（请求数/请求数/tokens/成功率，UTC 当天）
/// 3. 错误概况（近 24h 按 HTTP 状态码分组的失败 top）
/// 4. 运行信息（版本 + 进程运行时长）
pub async fn handle_diagnostics_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    // ── 1. 渠道统计（ChannelStore + CircuitBreaker 聚合）──
    let channels = state.channel_store.list();
    let status_map = state.channel_store.circuit_breaker().get_status_map();
    let breaker_open = status_map.values().filter(|s| *s == "open").count();
    let breaker_halfopen = status_map.values().filter(|s| *s == "halfopen").count();
    let in_cooldown = channels
        .iter()
        .filter(|c| state.channel_store.is_in_cooldown(&c.id))
        .count();
    let channels_stats = json!({
        "total": channels.len(),
        "enabled": channels.iter().filter(|c| c.is_enabled()).count(),
        "circuit_open": breaker_open,
        "circuit_halfopen": breaker_halfopen,
        "cooldown": in_cooldown,
    });

    // ── 2. 今日用量摘要（dashboard.rs 同口径：内存遍历请求日志）──
    let now_ts = chrono::Utc::now().timestamp();
    let today_start = today_start_ts(now_ts);
    let logs = state.log_store.requests.all_sorted_asc();
    let mut today_requests: u64 = 0;
    let mut today_success: u64 = 0;
    let mut today_input_tokens: u64 = 0;
    let mut today_output_tokens: u64 = 0;
    for l in &logs {
        if l.created_at < today_start {
            continue;
        }
        today_requests += 1;
        today_input_tokens = today_input_tokens.saturating_add(l.input_tokens);
        today_output_tokens = today_output_tokens.saturating_add(l.output_tokens);
        if l.status_code < 400 {
            today_success += 1;
        }
    }
    let today_usage = json!({
        "requests": today_requests,
        "success": today_success,
        // 无请求时 0.0（与 dashboard/realtime 的 error_rate 空数据口径一致）
        "success_rate": success_rate_pct(today_success, today_requests),
        "input_tokens": today_input_tokens,
        "output_tokens": today_output_tokens,
    });

    // ── 3. 错误概况（近 24h 失败请求按状态码分组）──
    let day_ago = now_ts - 24 * 3600;
    let failures: Vec<&crate::log::RequestLog> = logs
        .iter()
        .filter(|l| l.created_at >= day_ago && l.status_code >= 400)
        .collect();
    let top_errors = top_errors_from(&failures, TOP_ERRORS_LIMIT, ERROR_MSG_MAX_LEN);

    // ── 4. 运行信息（版本 + 进程运行时长）──
    let uptime_secs = process_uptime_secs();
    let runtime = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "uptime_secs": uptime_secs,
        // 非 Linux 平台取不到 /proc（与 monitor.rs 的降级语义一致）
        "uptime_available": uptime_secs.is_some(),
    });

    Ok(Json(json!({
        "success": true,
        "data": {
            "generated_at": now_ts,
            "channels": channels_stats,
            "today_usage": today_usage,
            "errors_24h": {
                "total_failures": failures.len(),
                "top": top_errors,
            },
            "runtime": runtime,
        }
    })))
}

/// GET /api/diagnostics/channels — 渠道批量测活。
///
/// 对全部启用渠道并发执行轻量探测，返回逐渠道存活状态。**只探测不修改**：
/// 直接调用 `ChannelStore::test`（该函数只发 HTTP 请求返回结果，不写
/// 断路器/不 mark 渠道状态/不落库），探测是诊断动作不是流量。
///
/// 与 `/api/channels/:id/test` 的差异：单渠道测试端点会持久化测试结果并
/// 联动渠道启停（mark_healthy/mark_unhealthy），批量测活不产生这些副作用。
pub async fn handle_diagnostics_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let enabled: Vec<crate::channel::Channel> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|c| c.is_enabled())
        .collect();

    let store = state.channel_store.clone();
    let probes = enabled.iter().map(|ch| {
        let store = Arc::clone(&store);
        async move {
            let result = store.test(ch).await;
            probe_item(ch, &result)
        }
    });

    let deadline = std::time::Duration::from_secs(PROBE_BATCH_DEADLINE_SECS);
    let items = match tokio::time::timeout(deadline, join_all(probes)).await {
        Ok(items) => items,
        Err(_) => {
            return Err(error_response(
                "Channel probe batch deadline exceeded (30s)",
                StatusCode::GATEWAY_TIMEOUT,
            ));
        }
    };

    let alive = items.iter().filter(|i| i["result"] == "alive").count();
    Ok(Json(json!({
        "success": true,
        "data": {
            "probed": items.len(),
            "alive": alive,
            "deadline_secs": PROBE_BATCH_DEADLINE_SECS,
            "channels": items,
        }
    })))
}

/// GET /api/diagnostics/breakers — 熔断器状态快照。
///
/// 逐渠道返回断路器完整事实（三态/失败计数/失败类型/剩余冷却/限流窗口/
/// HalfOpen 单飞租约）。只读：`snapshot_all` 仅遍历内部状态表，不改
/// 状态机；reset 写操作已有 `/api/channels/:id/reset-circuit`（不在此做）。
///
/// 注意：从未失败的渠道没有断路器条目（= 隐含 Closed），不出现在列表中；
/// 列表为空说明全部渠道从未触发熔断。
pub async fn handle_diagnostics_breakers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let snapshots = state.channel_store.circuit_breaker().snapshot_all();
    let breakers: Vec<Value> = snapshots
        .iter()
        .map(|s| {
            json!({
                "channel_id": s.channel_id,
                "state": s.state,
                "failure_count": s.failure_count,
                "failure_type": s.failure_type,
                "cooldown_remaining_secs": s.cooldown_remaining_secs,
                "rate_limit_remaining_secs": s.rate_limit_remaining_secs,
                "probe_in_flight": s.probe_in_flight,
                // ChannelStore 粗粒度冷却（mark_cooldown 60s 级）是否也在期
                "in_store_cooldown": state.channel_store.is_in_cooldown(&s.channel_id),
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": {
            "tracked": breakers.len(),
            "open": breakers.iter().filter(|b| b["state"] == "open").count(),
            "halfopen": breakers.iter().filter(|b| b["state"] == "halfopen").count(),
            "breakers": breakers,
        }
    })))
}

// ── 纯逻辑（可单测） ────────────────────────────────────────────────

/// UTC 当天 0 点的 unix 时间戳（失败时回退到 24h 前，保证聚合不空转）。
fn today_start_ts(now_ts: i64) -> i64 {
    let midnight = chrono::Utc
        .timestamp_opt(now_ts, 0)
        .single()
        .and_then(|dt| dt.date_naive().and_hms_opt(0, 0, 0))
        .and_then(|nd| chrono::Utc.from_local_datetime(&nd).single())
        .map(|dt| dt.timestamp());
    midnight.unwrap_or(now_ts - 24 * 3600)
}

/// 成功率（百分比，0~100）。无请求时 0.0（与现有看板空数据口径一致）。
fn success_rate_pct(success: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        success as f64 / total as f64 * 100.0
    }
}

/// 近段失败请求按 HTTP 状态码聚合 top 错误概况（count 降序）。
///
/// 每组保留最近一条错误消息（截断到 `max_len`）；`error_msg` 为空的
/// 失败只计数不占消息位（不虚构错误文本）。
fn top_errors_from<'a>(
    failures: &[&'a crate::log::RequestLog],
    limit: usize,
    max_len: usize,
) -> Vec<Value> {
    use std::collections::BTreeMap;
    // status_code → (count, 最近错误消息)
    let mut by_code: BTreeMap<u16, (u64, Option<String>)> = BTreeMap::new();
    for l in failures {
        let entry = by_code.entry(l.status_code).or_insert((0, None));
        entry.0 += 1;
        if let Some(msg) = l.error_msg.as_deref() {
            if !msg.is_empty() {
                // 按字符截断（字节切片会在 UTF-8 多字节字符中间 panic）
                let mut truncated: String = msg.chars().take(max_len).collect();
                if truncated.len() < msg.len() {
                    truncated.push('…');
                }
                entry.1 = Some(truncated);
            }
        }
    }
    let mut groups: Vec<(u16, (u64, Option<String>))> = by_code.into_iter().collect();
    groups.sort_by(|a, b| (b.1).0.cmp(&(a.1).0).then(a.0.cmp(&b.0)));

    groups
        .into_iter()
        .take(limit)
        .map(|(code, (count, last_msg))| {
            json!({
                "status_code": code,
                "count": count,
                "last_message": last_msg,
            })
        })
        .collect()
}

/// 测活结果三分类：alive / timeout / dead。
///
/// reqwest 超时的错误消息含 "timed out"（`test` 的 Err 分支原样透传），
/// 以此区分"上游慢"与"上游坏"；其余失败一律 dead。
fn classify_probe(result: &ChannelTestResult) -> &'static str {
    if result.success {
        "alive"
    } else {
        let m = result.message.to_lowercase();
        if m.contains("timed out") || m.contains("timeout") {
            "timeout"
        } else {
            "dead"
        }
    }
}

/// 单渠道测活响应条目。
fn probe_item(ch: &crate::channel::Channel, result: &ChannelTestResult) -> Value {
    json!({
        "channel_id": ch.id,
        "name": ch.name,
        "enabled": ch.is_enabled(),
        "result": classify_probe(result),
        "latency_ms": result.latency_ms,
        "message": result.message,
    })
}

/// 进程真实运行时长（秒）。
///
/// Linux：`/proc/uptime` − `/proc/self/stat` 的 starttime（tick/100）。
/// 注意与 `monitor.rs::process_info` 的 starttime 字段语义不同——那里
/// 是"进程启动时刻距 boot 的秒数"，本函数换算为真实运行时长。
/// 非 Linux：无 /proc 可读，返回 None（响应以 `uptime_available: false`
/// 降级标注，与 monitor.rs 非 Linux 降级语义一致）。
fn process_uptime_secs() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let uptime: f64 = std::fs::read_to_string("/proc/uptime")
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()?;
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // starttime 是 ')' 之后第 19 个空白分隔字段（1-based 第 22 项），单位 tick
        let start_ticks: f64 = stat
            .rsplit(')')
            .next()?
            .split_whitespace()
            .nth(19)?
            .parse()
            .ok()?;
        Some((uptime - start_ticks / 100.0).max(0.0) as u64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

// ── 测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn log_with(code: u16, msg: Option<&str>, at: i64) -> crate::log::RequestLog {
        let mut l = crate::log::RequestLog::new();
        l.status_code = code;
        l.error_msg = msg.map(|s| s.to_string());
        l.created_at = at;
        l
    }

    #[test]
    fn today_start_is_midnight_utc_or_fallback() {
        // 2026-01-15 12:34:56 UTC → 当天 0 点
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 1, 15, 12, 34, 56)
            .unwrap()
            .timestamp();
        let start = today_start_ts(now);
        let expect = chrono::Utc
            .with_ymd_and_hms(2026, 1, 15, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(start, expect);
    }

    #[test]
    fn success_rate_handles_empty() {
        assert_eq!(success_rate_pct(0, 0), 0.0);
        let rate = success_rate_pct(8, 10);
        assert!((rate - 80.0).abs() < f64::EPSILON);
    }

    #[test]
    fn top_errors_groups_by_status_code() {
        let l1 = log_with(429, Some("rate limited"), 1);
        let l2 = log_with(429, Some("rate limited again"), 2);
        let l3 = log_with(500, None, 3);
        let failures = vec![&l1, &l2, &l3];
        let top = top_errors_from(&failures, 5, 200);
        assert_eq!(top.len(), 2);
        // count 降序：429 组在前
        assert_eq!(top[0]["status_code"], 429);
        assert_eq!(top[0]["count"], 2);
        // 保留最近一条消息
        assert_eq!(top[0]["last_message"], "rate limited again");
        // 无 error_msg 的失败：计数在、消息为 null（不虚构文本）
        assert_eq!(top[1]["status_code"], 500);
        assert_eq!(top[1]["last_message"], serde_json::Value::Null);
    }

    #[test]
    fn top_errors_truncates_long_message() {
        let long_msg = "x".repeat(500);
        let l = log_with(502, Some(&long_msg), 1);
        let top = top_errors_from(&[&l], 5, 200);
        let msg = top[0]["last_message"].as_str().unwrap();
        assert_eq!(
            msg.chars().count(),
            ERROR_MSG_MAX_LEN + 1,
            "截断 200 字符 + 省略号"
        );
        assert!(msg.ends_with('…'));
    }

    #[test]
    fn top_errors_truncates_utf8_safely() {
        // 多字节字符中间截断不应 panic（字节切片会越界）
        let long_msg = "错误".repeat(150);
        let l = log_with(500, Some(&long_msg), 1);
        let top = top_errors_from(&[&l], 5, 200);
        assert!(top[0]["last_message"].as_str().unwrap().ends_with('…'));
    }

    #[test]
    fn classify_probe_three_way() {
        let alive = ChannelTestResult {
            success: true,
            message: "Channel reachable".into(),
            latency_ms: 120,
        };
        assert_eq!(classify_probe(&alive), "alive");

        let timeout = ChannelTestResult {
            success: false,
            message: "Request failed: operation timed out".into(),
            latency_ms: 15000,
        };
        assert_eq!(classify_probe(&timeout), "timeout");

        let dead = ChannelTestResult {
            success: false,
            message: "Auth failed: HTTP 401 (invalid api key?)".into(),
            latency_ms: 80,
        };
        assert_eq!(classify_probe(&dead), "dead");
    }

    #[test]
    fn probe_item_shape() {
        // 同 prober.rs 测试的 Channel 构造模式（非 default 字段仅 id/name/status）
        let ch: crate::channel::Channel = serde_json::from_value(serde_json::json!({
            "id": "ch1",
            "name": "上游A",
            "status": "enabled",
        }))
        .expect("channel from json");
        let result = ChannelTestResult {
            success: true,
            message: "ok".into(),
            latency_ms: 42,
        };
        let item = probe_item(&ch, &result);
        assert_eq!(item["channel_id"], "ch1");
        assert_eq!(item["enabled"], true);
        assert_eq!(item["result"], "alive");
        assert_eq!(item["latency_ms"], 42);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn process_uptime_available_on_linux() {
        let up = process_uptime_secs();
        // CI（ubuntu）上 /proc 必在；值就是本测试进程的运行秒数（≥0）
        assert!(up.is_some());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn process_uptime_unavailable_off_linux() {
        assert_eq!(process_uptime_secs(), None);
    }
}
