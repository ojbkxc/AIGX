//! 告警系统模块
//!
//! 提供告警配置、发送和通知功能。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{Duration, Instant};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertLevel {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertSeverity {
    Critical,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub title: String,
    pub message: String,
    pub level: AlertLevel,
    pub severity: AlertSeverity,
    pub source: String,
    pub timestamp: DateTime<Utc>,
    pub resolved: bool,
    pub metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertConfig {
    pub enabled: bool,
    pub channels: Vec<AlertChannel>,
    pub thresholds: AlertThresholds,
    pub cooldown_duration: Duration,
    pub retry_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertThresholds {
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub disk_usage: f32,
    pub error_rate: f64,
    pub response_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlertChannel {
    Email,
    Slack,
    Telegram,
    Webhook,
    SMS,
}

#[derive(Debug, Clone)]
pub struct AlertSender {
    config: AlertConfig,
    alert_buffer: Arc<RwLock<Vec<Alert>>>,
    cooldown_buffer: Arc<RwLock<std::collections::HashMap<String, Instant>>>,
    metrics_collector: Arc<dyn MetricsCollector>,
}

impl AlertSender {
    pub fn new(config: AlertConfig, metrics_collector: Arc<dyn MetricsCollector>) -> Self {
        Self {
            config,
            alert_buffer: Arc::new(RwLock::new(vec![])),
            cooldown_buffer: Arc::new(std::collections::HashMap::new()),
            metrics_collector,
        }
    }

    pub async fn send_alert(&self, alert: Alert) -> anyhow::Result<()> {
        // 检查告警冷却
        if !self.is_authorized(alert.id).await {
            info!("Alert {} is in cooldown", alert.id);
            return Ok(());
        }

        // 添加到缓冲区
        let mut buffer = self.alert_buffer.write().await;
        buffer.push(alert);

        // 立即发送，不等待定时发送
        self.flush_alerts().await?;

        Ok(())
    }

    async fn flush_alerts(&self) -> anyhow::Result<()> {
        let mut buffer = self.alert_buffer.write().await;
        let alerts = std::mem::take(&mut *buffer);

        for alert in alerts {
            self.dispatch_alert(alert).await?;
            self.update_cooldown(alert.id).await;
        }

        Ok(())
    }

    async fn dispatch_alert(&self, alert: Alert) -> anyhow::Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        info!("Dispatching alert: {} ({})", alert.id, alert.title);

        // 多通道发送
        for channel in &self.config.channels {
            match channel {
                AlertChannel::Email => self.send_via_email(&alert).await,
                AlertChannel::Slack => self.send_via_slack(&alert).await,
                AlertChannel::Telegram => self.send_via_telegram(&alert).await,
                AlertChannel::Webhook => self.send_via_webhook(&alert).await,
                AlertChannel::SMS => self.send_via_sms(&alert).await,
            };
        }

        // 发送其他持久化告警通知
        self.persist_alert(&alert).await?;

        Ok(())
    }

    async fn send_via_email(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现 Email 发送逻辑
        info!("Email alert: {}", alert.title);
        Ok(())
    }

    async fn send_via_slack(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现 Slack 发送逻辑
        info!("Slack alert: {}", alert.title);
        Ok(())
    }

    async fn send_via_telegram(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现 Telegram 发送逻辑
        info!("Telegram alert: {}", alert.title);
        Ok(())
    }

    async fn send_via_webhook(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现 Webhook 发送逻辑
        info!("Webhook alert: {}", alert.title);
        Ok(())
    }

    async fn send_via_sms(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现 SMS 发送逻辑
        info!("SMS alert: {}", alert.title);
        Ok(())
    }

    async fn persist_alert(&self, alert: &Alert) -> anyhow::Result<()> {
        // 实现告警持久化
        info!("Alert persisted: {}", alert.id);
        Ok(())
    }

    async fn is_authorized(&self, alert_id: String) -> bool {
        let cooldown_buffer = self.cooldown_buffer.read().await;
        match cooldown_buffer.get(&alert_id) {
            Some(last_sent) if last_sent.elapsed() < self.config.cooldown_duration => false,
            _ => true,
        }
    }

    async fn update_cooldown(&self, alert_id: String) {
        let mut cooldown_buffer = self.cooldown_buffer.write().await;
        cooldown_buffer.insert(alert_id, Instant::now());
    }

    pub fn check_system_health(&self) -> Result<Vec<Alert>, String> {
        let metrics = self.metrics_collector.get_metrics();
        let mut alerts = vec![];

        // 检查 CPU 使用率
        if metrics.cpu_usage > self.config.thresholds.cpu_usage {
            alerts.push(Alert {
                id: uuid::Uuid::new_v4().to_string(),
                title: "CPU 使用率过高".to_string(),
                message: format!(
                    "CPU 使用率为 {}%，超过阈值 {}%",
                    metrics.cpu_usage, self.config.thresholds.cpu_usage
                ),
                level: AlertLevel::Warning,
                severity: if metrics.cpu_usage > self.config.thresholds.cpu_usage * 1.2 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
                source: "system".to_string(),
                timestamp: Utc::now(),
                resolved: false,
                metadata: std::collections::HashMap::from([
                    ("cpu_usage".to_string(), metrics.cpu_usage.to_string()),
                    (
                        "threshold".to_string(),
                        self.config.thresholds.cpu_usage.to_string(),
                    ),
                ]),
            });
        }

        // 检查内存使用率
        if metrics.memory_usage > self.config.thresholds.memory_usage {
            alerts.push(Alert {
                id: uuid::Uuid::new_v4().to_string(),
                title: "内存使用率过高".to_string(),
                message: format!(
                    "内存使用率为 {}%，超过阈值 {}%",
                    metrics.memory_usage, self.config.thresholds.memory_usage
                ),
                level: AlertLevel::Warning,
                severity: if metrics.memory_usage > self.config.thresholds.memory_usage * 1.2 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
                source: "system".to_string(),
                timestamp: Utc::now(),
                resolved: false,
                metadata: std::collections::HashMap::from([
                    ("memory_usage".to_string(), metrics.memory_usage.to_string()),
                    (
                        "threshold".to_string(),
                        self.config.thresholds.memory_usage.to_string(),
                    ),
                ]),
            });
        }

        // 检查磁盘使用率
        if metrics.disk_usage > self.config.thresholds.disk_usage {
            alerts.push(Alert {
                id: uuid::Uuid::new_v4().to_string(),
                title: "磁盘使用率过高".to_string(),
                message: format!(
                    "磁盘使用率为 {}%，超过阈值 {}%",
                    metrics.disk_usage, self.config.thresholds.disk_usage
                ),
                level: AlertLevel::Error,
                severity: if metrics.disk_usage > self.config.thresholds.disk_usage * 1.2 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
                source: "system".to_string(),
                timestamp: Utc::now(),
                resolved: false,
                metadata: std::collections::HashMap::from([
                    ("disk_usage".to_string(), metrics.disk_usage.to_string()),
                    (
                        "threshold".to_string(),
                        self.config.thresholds.disk_usage.to_string(),
                    ),
                ]),
            });
        }

        // 检查错误率
        if metrics.error_rate > self.config.thresholds.error_rate {
            alerts.push(Alert {
                id: uuid::Uuid::new_v4().to_string(),
                title: "错误率过高".to_string(),
                message: format!(
                    "错误率为 {:.2}%，超过阈值 {}%",
                    metrics.error_rate,
                    self.config.thresholds.error_rate * 100.0
                ),
                level: AlertLevel::Error,
                severity: if metrics.error_rate > self.config.thresholds.error_rate * 1.5 {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                },
                source: "performance".to_string(),
                timestamp: Utc::now(),
                resolved: false,
                metadata: std::collections::HashMap::from([
                    ("error_rate".to_string(), metrics.error_rate.to_string()),
                    (
                        "threshold".to_string(),
                        (self.config.thresholds.error_rate * 100.0).to_string(),
                    ),
                ]),
            });
        }

        Ok(alerts)
    }
}

// Mock MetricsCollector for tests
pub trait MetricsCollector {
    fn get_metrics(&self) -> crate::monitoring::metrics::Metrics;
}

struct MockMetricsCollector;

impl MetricsCollector for MockMetricsCollector {
    fn get_metrics(&self) -> crate::monitoring::metrics::Metrics {
        crate::monitoring::metrics::Metrics::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_alert_cooldown() {
        let config = AlertConfig {
            enabled: true,
            channels: vec![],
            thresholds: AlertThresholds {
                cpu_usage: 50.0,
                memory_usage: 70.0,
                disk_usage: 80.0,
                error_rate: 0.05,
                response_time_ms: 100,
            },
            cooldown_duration: Duration::from_secs(60),
            retry_count: 3,
        };

        // 这里可以添加更详细的测试用例
    }
}
