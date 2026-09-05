//! 告警巡检后台任务——参照 burncloud alert service 的周期评估模式。
//!
//! 每 60s 扫描一次：
//! 1. 渠道健康快照（circuit_breaker 状态 + health_manager 错误率/延迟 EMA）
//!    → AlertRuleEvaluator 评估（静默期压制重复告警）
//!    → 触发的 Alert 分发：Telegram/Email（NotifyService）
//!      + Slack（级别颜色）+ 通用 Webhook（结构化 JSON + HMAC 签名）
//! 2. 进程内存（RSS via /proc 或 Windows API 不可得时的保守 0）
//!    → MemoryHigh 规则评估
//!
//! 评估器状态（active_alerts 静默期表）由 Mutex 保护，管理 API
//! （admin.rs 的告警规则 CRUD）共享同一实例。

use std::sync::{Arc, Mutex};

use crate::channel::ChannelStore;
use crate::notify::alert::{AlertKind, AlertLevel, AlertRule, AlertRuleEvaluator};
use crate::notify::NotifyService;

/// 共享告警评估器（管理 API 与巡检任务共用）
pub type SharedAlertEvaluator = Arc<Mutex<AlertRuleEvaluator>>;

/// 创建默认评估器（AlertRule::defaults 规则集）
pub fn shared_evaluator() -> SharedAlertEvaluator {
    Arc::new(Mutex::new(AlertRuleEvaluator::new(AlertRule::defaults())))
}

/// 启动告警巡检协程
pub fn spawn_alert_patrol(
    channel_store: Arc<ChannelStore>,
    notify_service: Arc<NotifyService>,
    evaluator: SharedAlertEvaluator,
) {
    tokio::spawn(async move {
        let interval = tokio::time::Duration::from_secs(60);
        loop {
            tokio::time::sleep(interval).await;
            patrol_once(&channel_store, &notify_service, &evaluator).await;
        }
    });
}

async fn patrol_once(
    channel_store: &ChannelStore,
    notify_service: &Arc<NotifyService>,
    evaluator: &SharedAlertEvaluator,
) {
    // ── 渠道健康评估 ──────────────────────────────────────────────
    // 遍历全部渠道，取 health_manager 汇总（错误率/延迟）与断路器状态
    let cb_status = channel_store.circuit_breaker().get_status_map();
    for channel in channel_store.list() {
        let hm = channel_store.health_manager();
        let summary = hm.get_health(&channel.id);
        // 断路器状态：Open = 已达失败阈值，视为持续故障信号（threshold 已到，
        // 用规则阈值 1 触发告警语义；Closed/HalfOpen 不触发）
        let cb_open = cb_status
            .get(&channel.id)
            .map(|s| s == "Open")
            .unwrap_or(false);

        // 渠道连续失败（circuit breaker 打开时告警）
        let kind_failure = AlertKind::ChannelFailure {
            channel_id: channel.id.clone(),
        };
        if cb_open {
            if let Some(alert) = evaluator.lock().unwrap().evaluate(&kind_failure, 1) {
                dispatch_alert(notify_service, &alert.level, &alert.message).await;
            }
        }

        // 渠道延迟（health_manager 的模型平均延迟 EMA → 渠道均值 ms）
        if let Some(s) = &summary {
            let kind_latency = AlertKind::ChannelHighLatency {
                channel_id: channel.id.clone(),
            };
            let avg_ms = s.overall_avg_latency_ms as u64;
            if let Some(alert) = evaluator.lock().unwrap().evaluate(&kind_latency, avg_ms) {
                dispatch_alert(notify_service, &alert.level, &alert.message).await;
            }
        }
    }

    // ── 内存评估 ────────────────────────────────────────────────
    // 无 sysinfo 依赖时的保守实现：读取 /proc/self/status VmRSS（Linux），
    // 非 Linux 平台计算 usage_percent 不可得则跳过（evaluate 传入 0 永不触发）
    let mem_percent = memory_usage_percent();
    if mem_percent > 0 {
        if let Some(alert) = evaluator
            .lock()
            .unwrap()
            .evaluate(&AlertKind::MemoryHigh, mem_percent)
        {
            dispatch_alert(notify_service, &alert.level, &alert.message).await;
        }
    }
}

/// 分发一条告警到全部已配置渠道
async fn dispatch_alert(notify_service: &Arc<NotifyService>, level: &AlertLevel, message: &str) {
    let level_str = match level {
        AlertLevel::Info => "info",
        AlertLevel::Warning => "warning",
        AlertLevel::Critical => "critical",
    };
    tracing::warn!(level = level_str, "alert triggered: {message}");

    // Telegram + Email（复用统一事件分发）
    notify_service.notify_spawn(crate::notify::NotifyEvent::AlertTriggered {
        level: level_str.to_string(),
        message: message.to_string(),
    });

    // Slack（attachments 颜色）
    let cfg = notify_service.get_config().await;
    if cfg.slack_ready() {
        if let Err(e) = notify_service.send_slack(level_str, message).await {
            tracing::warn!("Alert slack failed: {e}");
        }
    }

    // 通用 Webhook（结构化载荷 + HMAC 签名）
    if cfg.webhook_ready() {
        let payload = serde_json::json!({
            "source": "aigx",
            "level": level_str,
            "message": message,
            "triggered_at": chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = notify_service.send_webhook(&payload).await {
            tracing::warn!("Alert webhook failed: {e}");
        }
    }
}

/// 进程视角的内存使用率（百分比）。
/// Linux：VmRSS / MemTotal；其他平台返回 0（不评估 MemoryHigh）。
fn memory_usage_percent() -> u64 {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let rss_kb: u64 = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let total_kb: u64 = meminfo
            .lines()
            .find(|l| l.starts_with("MemTotal:"))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if total_kb == 0 {
            return 0;
        }
        // 单进程 RSS / 系统总内存，上限 100
        (rss_kb * 100 / total_kb).min(100)
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluator_shared_lock_works() {
        let ev = shared_evaluator();
        let k = AlertKind::MemoryHigh;
        assert!(ev.lock().unwrap().evaluate(&k, 90).is_some());
        // 静默期内第二次被压制
        assert!(ev.lock().unwrap().evaluate(&k, 95).is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn memory_percent_bounds() {
        let p = memory_usage_percent();
        assert!(p <= 100);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn memory_percent_zero_offline() {
        assert_eq!(memory_usage_percent(), 0);
    }
}
