//! 连接健康检查模块
//!
//! 监控连接的健康状态并执行必要的恢复操作

use super::Connection;
use std::sync::Arc;
use tokio::time::{Duration, Instant};
use tracing::{debug, error, warn};

/// 健康检查器
pub struct HealthChecker {
    /// 检查间隔
    interval: Duration,
    /// 最大容忍的错误次数
    max_errors: u16,
    /// 重新连接超时
    reconnect_timeout: Duration,
}

impl Default for HealthChecker {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            max_errors: 3,
            reconnect_timeout: Duration::from_secs(10),
        }
    }
}

impl HealthChecker {
    /// 创建新的健康检查器
    pub fn new(interval: Duration, max_errors: u16, reconnect_timeout: Duration) -> Self {
        Self {
            interval,
            max_errors,
            reconnect_timeout,
        }
    }

    /// 执行健康检查
    pub async fn check(&self, connection: Arc<dyn Connection>) -> Result<bool> {
        let mut errors = 0;
        let mut is_healthy = true;

        // 心跳检查
        match self.heartbeat(&connection).await {
            Ok(_) => {
                debug!("Heartbeat check passed for connection: {}", connection.id());
            }
            Err(e) => {
                warn!("Heartbeat failed for {}: {}", connection.id(), e);
                errors += 1;
            }
        }

        // 数据完整性检查
        match self.data_integrity_check(&connection).await {
            Ok(_) => {
                debug!(
                    "Data integrity check passed for connection: {}",
                    connection.id()
                );
            }
            Err(e) => {
                warn!("Data integrity check failed for {}: {}", connection.id(), e);
                errors += 1;
            }
        }

        // 判断是否健康
        is_healthy = errors == 0;

        if !is_healthy && errors >= self.max_errors {
            error!(
                "Connection {} marked as unhealthy after {} errors",
                connection.id(),
                errors
            );
            // 自动重连逻辑应该在连接池调用
        }

        Ok(is_healthy)
    }

    /// 心跳检查
    async fn heartbeat(&self, connection: &Arc<dyn Connection>) -> Result<()> {
        // 在实际实现中，这里应该发送心跳包并验证响应
        // 简化版本：仅检查连接状态

        if !connection.is_active() {
            return Err(anyhow::anyhow!("Connection is not active"));
        }

        Ok(())
    }

    /// 数据完整性检查
    async fn data_integrity_check(&self, connection: &Arc<dyn Connection>) -> Result<()> {
        // 在实际实现中，这里应该发送测试数据包并验证响应
        Ok(())
    }
}

/// 连接状态管理器
pub struct ConnectionStateManager {
    /// 状态转换表
    allowed_transitions: Arc<dyn ConnectionStateTable>,
}

impl ConnectionStateManager {
    /// 创建新的状态管理器
    pub fn new(allowed_transitions: Arc<dyn ConnectionStateTable>) -> Self {
        Self {
            allowed_transitions,
        }
    }

    /// 检查状态转换是否允许
    pub fn can_transition(&self, from: &str, to: &str) -> bool {
        self.allowed_transitions.can_transition(from, to)
    }

    /// 获取状态转换定义
    pub fn transitions(&self) -> Vec<(String, String)> {
        self.allowed_transitions.transitions()
    }
}

/// 状态转换表
pub trait ConnectionStateTable: Send + Sync {
    /// 状态转换是否允许
    fn can_transition(&self, from: &str, to: &str) -> bool;

    /// 获取所有允许的转换
    fn transitions(&self) -> Vec<(String, String)>;
}

impl Default for ConnectionStateManager {
    fn default() -> Self {
        Self::new(Arc::new(StateTransitionTable {
            transitions: default_connection_transitions(),
        }))
    }
}

/// 状态转换表实现
struct StateTransitionTable {
    transitions: Vec<(String, String)>,
}

impl ConnectionStateTable for StateTransitionTable {
    fn can_transition(&self, from: &str, to: &str) -> bool {
        self.transitions.iter().any(|(f, t)| f == from && t == to)
    }

    fn transitions(&self) -> Vec<(String, String)> {
        self.transitions.clone()
    }
}

/// 健康监控指标
pub struct HealthMetrics {
    pub last_check_time: Instant,
    pub is_healthy: bool,
    pub consecutive_failures: u16,
    pub total_failures: u16,
    pub average_latency_ms: f64,
    pub response_times: Vec<u64>,
}

impl HealthMetrics {
    /// 创建新的健康指标
    pub fn new() -> Self {
        Self {
            last_check_time: Instant::now(),
            is_healthy: true,
            consecutive_failures: 0,
            total_failures: 0,
            average_latency_ms: 0.0,
            response_times: Vec::new(),
        }
    }

    /// 记录检查结果
    pub fn record_check(&mut self, healthy: bool, latency_ms: u64) {
        if !healthy {
            self.consecutive_failures += 1;
            self.total_failures += 1;
            self.is_healthy = false;
        } else {
            // 恢复健康状态时重置计数器
            if self.consecutive_failures > 0 {
                debug!(
                    "Connection recovered after {} consecutive failures",
                    self.consecutive_failures
                );
                self.consecutive_failures = 0;
                self.is_healthy = true;
            }
        }

        self.last_check_time = Instant::now();

        // 计算平均延迟
        self.response_times.push(latency_ms);
        if self.response_times.len() > 10 {
            self.response_times.remove(0);
        }

        let sum: u64 = self.response_times.iter().sum();
        self.average_latency_ms = if !self.response_times.is_empty() {
            sum as f64 / self.response_times.len() as f64
        } else {
            0.0
        };
    }

    /// 获取健康度评分（0.0 - 100.0）
    pub fn health_score(&self) -> f64 {
        if !self.is_healthy {
            return self.consecutive_failures as f64 * 2;
        }

        let latency_score = (1.0 - (self.average_latency_ms / 1000.0).min(1.0)) * 100.0;
        let consistency_score = (1.0 - (self.consecutive_failures as f64 / 10.0)).max(0.0) * 100.0;

        latency_score * 0.6 + consistency_score * 0.4
    }
}
/// 默认连接状态转换表
pub fn default_connection_transitions() -> Vec<(String, String)> {
    vec![
        ("idle".to_string(), "connecting".to_string()),
        ("connecting".to_string(), "connected".to_string()),
        ("connected".to_string(), "idle".to_string()),
        ("connected".to_string(), "reconnecting".to_string()),
        ("reconnecting".to_string(), "connected".to_string()),
        ("reconnecting".to_string(), "idle".to_string()),
        ("connected".to_string(), "disconnected".to_string()),
        ("disconnected".to_string(), "connecting".to_string()),
        ("idle".to_string(), "disconnected".to_string()),
        ("error".to_string(), "connecting".to_string()),
        ("error".to_string(), "disconnected".to_string()),
    ]
}
