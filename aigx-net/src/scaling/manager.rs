//! 自动扩缩容模块
//!
//! 提供基于负载的自动扩缩容功能。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ScalingMode {
    None,
    Manual,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScalingAction {
    ScaleUp,
    ScaleDown,
    None,
}

#[derive(Clone)]
pub struct ScalingConfig {
    pub enabled: bool,
    pub min_nodes: usize,
    pub max_nodes: usize,
    pub current_nodes: Vec<ScalingNode>,
    pub mode: ScalingMode,
    pub thresholds: ScalingThresholds,
    pub cooldown_period: Duration,
    pub load_balance_mode: LoadBalanceMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingThresholds {
    pub cpu_high_threshold: f32,
    pub cpu_low_threshold: f32,
    pub memory_high_threshold: f32,
    pub memory_low_threshold: f32,
    pub request_rate_high_threshold: f64,
    pub request_rate_low_threshold: f64,
    pub concurrent_connections_high_threshold: usize,
    pub concurrent_connections_low_threshold: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LoadBalanceMode {
    Random,
    Latency,
    LeastLoaded,
    Weighted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScalingNode {
    pub id: String,
    pub name: String,
    pub status: NodeStatus,
    pub load: f32,
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub connections: usize,
    pub last_stats_time: std::time::SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    Ready,
    Busy,
    Down,
    Scaling,
}

#[derive(Clone)]
pub struct ScalingManager {
    config: Arc<RwLock<ScalingConfig>>,
    scaling_history: Arc<RwLock<Vec<ScalingRecord>>>,
    last_scaled_time: Arc<RwLock<Instant>>,
}

impl ScalingManager {
    pub fn new(initial_config: ScalingConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(initial_config)),
            scaling_history: Arc::new(RwLock::new(vec![])),
            last_scaled_time: Arc::new(Instant::now()),
        }
    }

    pub async fn assess_scaling_needs(&self) -> Option<ScalingAction> {
        let config = self.config.read().await;
        if !config.enabled {
            return None;
        }

        let avg_load = self.calculate_average_load().await;
        let metrics = self.get_current_metrics().await;

        // 根据平均负载和当前配置判断扩缩容需求
        match config.mode {
            ScalingMode::Auto => {
                if metrics.total_load > config.thresholds.cpu_high_threshold {
                    self.request_scaling(ScalingAction::ScaleUp).await
                } else if metrics.total_load < config.thresholds.cpu_low_threshold
                    && config.current_nodes.len() > config.min_nodes
                {
                    self.request_scaling(ScalingAction::ScaleDown).await
                } else {
                    None
                }
            }
            ScalingMode::Manual => None,
            ScalingMode::None => None,
        }
    }

    async fn request_scaling(&self, action: ScalingAction) -> Option<ScalingAction> {
        // 检查冷却期
        let last_scaled = self.last_scaled_time.read().await;
        let config = self.config.read().await;
        if last_scaled.elapsed() < config.cooldown_period {
            info!(
                "Scaling action {} is not allowed yet due to cooldown",
                action
            );
            return None;
        }

        let new_config = match action {
            ScalingAction::ScaleUp => {
                if config.current_nodes.len() < config.max_nodes {
                    let mut new_config = config.clone();
                    if let Some(template) = new_config.current_nodes.first().cloned() {
                        new_config.current_nodes.push(template);
                    }
                    new_config
                } else {
                    return None;
                }
            }
            ScalingAction::ScaleDown => {
                if config.current_nodes.len() > config.min_nodes {
                    let mut new_config = config.clone();
                    new_config.current_nodes.pop();
                    new_config
                } else {
                    return None;
                }
            }
            ScalingAction::None => return None,
        };

        self.update_config(new_config).await;
        Some(action)
    }

    async fn calculate_average_load(&self) -> f32 {
        let config = self.config.read().await;
        if config.current_nodes.is_empty() {
            return 0.0;
        }

        let total_cpu = config
            .current_nodes
            .iter()
            .map(|node| node.cpu_usage)
            .sum::<f32>();
        (total_cpu / config.current_nodes.len() as f32)
    }

    async fn get_current_metrics(&self) -> ScalingMetrics {
        let config = self.config.read().await;
        ScalingMetrics {
            total_load: config
                .current_nodes
                .iter()
                .map(|node| node.cpu_usage + node.memory_usage / 2.0)
                .sum::<f32>(),
            request_rate: config
                .current_nodes
                .iter()
                .map(|node| node.connections as f64)
                .sum::<f64>(),
            concurrent_connections: config
                .current_nodes
                .iter()
                .map(|node| node.connections)
                .sum::<usize>(),
        }
    }

    async fn update_config(&self, new_config: ScalingConfig) {
        let mut config = self.config.write().await;
        *config = new_config;

        // 记录扩缩容历史
        let current_nodes = config.current_nodes.len();
        let record = ScalingRecord {
            id: uuid::Uuid::new_v4().to_string(),
            action: ScalingAction::ScaleUp, // 或者 ScaleDown
            timestamp: Utc::now(),
            nodes_count: current_nodes,
            reason: "auto scaling".to_string(),
        };

        let mut history = self.scaling_history.write().await;
        history.push(record);

        *self.last_scaled_time.write().await = Instant::now();

        info!("Scaling updated to {} nodes", current_nodes);
    }

    pub async fn get_scaling_status(&self) -> ScalingStatus {
        let config = self.config.read().await;
        let current_nodes = config.current_nodes.len();
        let avg_load = self.calculate_average_load().await;
        let metrics = self.get_current_metrics().await;
        let cooldown_remaining = config
            .cooldown_period
            .saturating_sub(self.last_scaled_time.read().await.elapsed());

        ScalingStatus {
            current_nodes,
            max_nodes: config.max_nodes,
            min_nodes: config.min_nodes,
            average_load: avg_load,
            is_scaling: self.is_scaling().await,
            cooldown_remaining_seconds: cooldown_remaining.as_secs(),
            scaling_history_len: self.scaling_history.read().await.len(),
            appropriate_action: None, // 可以根据负载智能判断
        }
    }

    async fn is_scaling(&self) -> bool {
        let config = self.config.read().await;
        config
            .current_nodes
            .iter()
            .any(|node| node.status == NodeStatus::Scaling)
    }

    pub async fn predict_scaling(&self) -> ScalingPrediction {
        let config = self.config.read().await;
        let current_nodes = config.current_nodes.len();
        let metrics = self.get_current_metrics().await;
        let predicted_load = (current_nodes as f32) * (metrics.total_load / current_nodes as f32);

        let is_underutilized = metrics.request_rate < config.thresholds.request_rate_low_threshold
            && current_nodes > config.min_nodes;

        let is_overloaded = metrics.request_rate > config.thresholds.request_rate_high_threshold;

        let recommended_action = if is_overloaded {
            ScalingAction::ScaleUp
        } else if is_underutilized {
            ScalingAction::ScaleDown
        } else {
            None
        };

        ScalingPrediction {
            current_nodes,
            target_nodes: match recommended_action {
                Some(ScalingAction::ScaleUp) => (current_nodes + 1).min(config.max_nodes),
                Some(ScalingAction::ScaleDown) => {
                    current_nodes.saturating_sub(1).max(config.min_nodes)
                }
                _ => current_nodes,
            },
            reason: if is_overloaded {
                "request_rate_high".to_string()
            } else if is_underutilized {
                "request_rate_low".to_string()
            } else {
                "no_action".to_string()
            },
            estimated_time: recommended_action.map(|_| Instant::now()), // 简化计算
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScalingMetrics {
    pub total_load: f32,
    pub request_rate: f64,
    pub concurrent_connections: usize,
}

#[derive(Debug, Clone)]
pub struct ScalingStatus {
    pub current_nodes: usize,
    pub min_nodes: usize,
    pub max_nodes: usize,
    pub average_load: f32,
    pub is_scaling: bool,
    pub cooldown_remaining_seconds: u64,
    pub scaling_history_len: usize,
    pub appropriate_action: Option<ScalingAction>,
}

#[derive(Debug, Clone)]
pub struct ScalingRecord {
    pub id: String,
    pub action: ScalingAction,
    pub timestamp: DateTime<Utc>,
    pub nodes_count: usize,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ScalingPrediction {
    pub current_nodes: usize,
    pub target_nodes: usize,
    pub reason: String,
    pub estimated_time: Option<Instant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scaling_on_high_load() {
        let config = ScalingConfig {
            enabled: true,
            min_nodes: 1,
            max_nodes: 5,
            current_nodes: vec![ScalingNode {
                id: "1".to_string(),
                name: "Node 1".to_string(),
                status: NodeStatus::Ready,
                load: 80.0,
                cpu_usage: 80.0,
                memory_usage: 80.0,
                connections: 1000,
                last_stats_time: SystemTime::now(),
            }],
            mode: ScalingMode::Auto,
            thresholds: ScalingThresholds {
                cpu_high_threshold: 60.0,
                cpu_low_threshold: 20.0,
                memory_high_threshold: 70.0,
                memory_low_threshold: 30.0,
                request_rate_high_threshold: 500.0,
                request_rate_low_threshold: 200.0,
                concurrent_connections_high_threshold: 800,
                concurrent_connections_low_threshold: 500,
            },
            cooldown_period: Duration::from_secs(300),
            load_balance_mode: LoadBalanceMode::Latency,
        };

        let manager = ScalingManager::new(config);
        let action = manager.assess_scaling_needs().await;

        assert_eq!(action, Some(ScalingAction::ScaleUp));
    }

    #[tokio::test]
    async fn test_scaling_off_for_manual_mode() {
        let config = ScalingConfig {
            enabled: true,
            min_nodes: 1,
            max_nodes: 5,
            current_nodes: vec![],
            mode: ScalingMode::Manual,
            thresholds: ScalingThresholds {
                cpu_high_threshold: 60.0,
                cpu_low_threshold: 20.0,
                memory_high_threshold: 70.0,
                memory_low_threshold: 30.0,
                request_rate_high_threshold: 500.0,
                request_rate_low_threshold: 200.0,
                concurrent_connections_high_threshold: 800,
                concurrent_connections_low_threshold: 500,
            },
            cooldown_period: Duration::from_secs(300),
            load_balance_mode: LoadBalanceMode::Latency,
        };

        let manager = ScalingManager::new(config);
        let action = manager.assess_scaling_needs().await;

        assert_eq!(action, None);
    }

    #[tokio::test]
    async fn test_scaling_prediction() {
        let config = ScalingConfig {
            enabled: true,
            min_nodes: 1,
            max_nodes: 5,
            current_nodes: vec![ScalingNode {
                id: "1".to_string(),
                name: "Node 1".to_string(),
                status: NodeStatus::Ready,
                load: 80.0,
                cpu_usage: 80.0,
                memory_usage: 60.0,
                connections: 1000,
                last_stats_time: SystemTime::now(),
            }],
            mode: ScalingMode::Auto,
            thresholds: ScalingThresholds {
                cpu_high_threshold: 60.0,
                cpu_low_threshold: 20.0,
                memory_high_threshold: 70.0,
                memory_low_threshold: 30.0,
                request_rate_high_threshold: 500.0,
                request_rate_low_threshold: 200.0,
                concurrent_connections_high_threshold: 800,
                concurrent_connections_low_threshold: 500,
            },
            cooldown_period: Duration::from_secs(300),
            load_balance_mode: LoadBalanceMode::Latency,
        };

        let manager = ScalingManager::new(config);
        let prediction = manager.predict_scaling().await;

        assert_eq!(prediction.current_nodes, 1);
        assert_eq!(prediction.reason, "request_rate_high");
        assert_eq!(prediction.target_nodes, 2);
    }
}
