//! 集群管理模块
//!
//! 提供集群发现、节点选择和协调功能。

use super::node::{DistributedNode, NodeRegistry, NodeStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct ClusterManager {
    registry: Arc<NodeRegistry>,
    election_timeout: Duration,
    heartbeat_interval: Duration,
}

impl ClusterManager {
    pub fn new(registry: Arc<NodeRegistry>) -> Self {
        Self {
            registry,
            election_timeout: Duration::from_secs(15),
            heartbeat_interval: Duration::from_secs(5),
        }
    }

    pub async fn elect_leader(&self) -> Option<DistributedNode> {
        let nodes = self.registry.get_all_nodes().await;

        // 简单的领导者选举：选择健康且ip地址最小的节点
        if let Some(leader) = nodes
            .into_iter()
            .filter(|n| n.status == NodeStatus::Online && n.is_leader)
            .min_by_key(|n| n.address.clone())
        {
            return Some(leader.clone());
        }

        None
    }

    pub async fn start_election(&self) {
        let registry = self.registry.clone();
        let timeout = self.election_timeout;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            let mut last_propagate_time = Instant::now();

            loop {
                interval.tick().await;

                // 检查选举超时
                if last_propagate_time.elapsed() > timeout
                    && std::thread::park_timeout(Duration::from_millis(10)).was_interrupted()
                {
                    let nodes = registry.get_all_nodes().await;

                    // 只有当前节点在线时才参与选举
                    if nodes.iter().any(|n| n.id == registry.get_my_id())
                        && !nodes.iter().any(|n| n.is_leader)
                    {
                        self.maybe_become_leader(nodes).await;
                    }

                    last_propagate_time = Instant::now();
                } else {
                    last_propagate_time = Instant::now();
                }

                // 定期更新状态，通知领导者
                self.update_cluster_status().await;
            }
        });
    }

    async fn maybe_become_leader(&self, nodes: Vec<DistributedNode>) {
        // 简单的领导者选举算法
        // 实际项目中可以使用 Raft 等算法

        let available_nodes = nodes
            .iter()
            .filter(|n| n.status == NodeStatus::Online)
            .collect::<Vec<_>>();

        if available_nodes.len() < 3 {
            // 节点太少，不能承担领导者职责
            return;
        }

        // 选择健康分数最高的作为领导者
        if let Some(highest_score) = available_nodes
            .iter()
            .filter(|n| n.health_score >= 80 && n.cpu_usage <= 30 && n.memory_usage <= 70)
            .max_by_key(|n| n.health_score)
        {
            if highest_score.id == self.registry.get_my_id() {
                println!("Node {} is becoming leader", self.registry.get_my_id());
                // 请求成为领导者
                self.request_become_leader().await;
            }
        }
    }

    async fn request_become_leader(&self) {
        // 这里可以实现 Leader Election 协议
        // 例如：提交心跳、等待仲裁等
        println!("Leader request sent");

        // 简化实现：假设总是成功
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 更新本节点状态
        let mut nodes = self.registry.nodes.write().await;
        if let Some(node) = nodes.get_mut(self.registry.get_my_id()) {
            node.is_leader = true;
        }
    }

    async fn update_cluster_status(&self) {
        // 定期更新集群状态
        debug!("Cluster status update");
    }

    pub async fn get_best_node_for_task(
        &self,
        requirements: &TaskRequirements,
    ) -> Option<DistributedNode> {
        let nodes = self.registry.get_online_nodes().await;

        nodes
            .into_iter()
            .filter(|n| self.meets_requirements(n, requirements))
            .min_by_key(|n| self.calculate_node_score(n, requirements))
            .cloned()
    }

    fn meets_requirements(
        &self,
        _node: &DistributedNode,
        _requirements: &TaskRequirements,
    ) -> bool {
        // 实现节点要求检查
        // 例如：星座空间要求、延迟要求、负载要求等
        true
    }

    fn calculate_node_score(&self, node: &DistributedNode, requirements: &TaskRequirements) -> f64 {
        // 计算节点得分，优先级：
        // 1. 健康分数 (0-80%)
        // 2. CPU 使用率 (40%权重)
        // 3. 内存使用率 (30%权重)
        // 4. 延迟 (30%权重)

        let mut score = node.health_score as f64 * 80.0 / 100.0;

        // CPU 使用率惩罚（越高越低分）
        score -= node.cpu_usage as f64 * 40.0;
        // 内存使用率惩罚（越高越低分）
        score -= node.memory_usage as f64 * 30.0;

        // 注意：这里需要根据实际距离计算延迟
        // score -= requirements.distance as f64 * 30.0;

        score.max(0.0).min(100.0)
    }
}

#[derive(Clone)]
pub struct TaskRequirements {
    pub max_cpu_usage: f32,
    pub max_memory_usage: f32,
    pub max_latency_ms: u64,
    pub min_health_score: u8,
    pub required_replication_factor: usize,
    pub regions: Vec<String>,
}

impl Default for TaskRequirements {
    fn default() -> Self {
        Self {
            max_cpu_usage: 70.0,
            max_memory_usage: 80.0,
            max_latency_ms: 50,
            min_health_score: 70,
            required_replication_factor: 3,
            regions: vec!["default".to_string()], // 默认区域
        }
    }
}

pub struct ClusterHealth {
    pub total_nodes: usize,
    pub healthy_nodes: usize,
    pub offline_nodes: usize,
    pub leader_nodes: usize,
    pub average_cpu: f32,
    pub average_memory: f32,
    pub average_latency_ms: f64,
    pub last_check: Instant,
}

pub trait ClusterHealthChecker {
    async fn check_health(&self) -> ClusterHealth {
        let nodes = self.registry.get_all_nodes().await;

        ClusterHealth {
            total_nodes: nodes.len(),
            healthy_nodes: nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Online && n.health_score >= 80)
                .count(),
            offline_nodes: nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Offline)
                .count(),
            leader_nodes: nodes.iter().filter(|n| n.is_leader).count(),
            average_cpu: nodes.iter().map(|n| n.cpu_usage).sum::<f32>() / nodes.len() as f32,
            average_memory: nodes.iter().map(|n| n.memory_usage).sum::<f32>() / nodes.len() as f32,
            average_latency_ms: 0.0,
            last_check: Instant::now(),
        }
    }
}

impl ClusterHealthChecker for ClusterManager {
    async fn check_health(&self) -> ClusterHealth {
        let nodes = self.registry.get_all_nodes().await;

        ClusterHealth {
            total_nodes: nodes.len(),
            healthy_nodes: nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Online && n.health_score >= 80)
                .count(),
            offline_nodes: nodes
                .iter()
                .filter(|n| n.status == NodeStatus::Offline)
                .count(),
            leader_nodes: nodes.iter().filter(|n| n.is_leader).count(),
            average_cpu: nodes.iter().map(|n| n.cpu_usage).sum::<f32>() / nodes.len() as f32,
            average_memory: nodes.iter().map(|n| n.memory_usage).sum::<f32>() / nodes.len() as f32,
            average_latency_ms: 0.0,
            last_check: Instant::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cluster_election() {
        let registry = NodeRegistry::new("test-node-1");
        let manager = ClusterManager::new(Arc::new(registry));

        let node = DistributedNode {
            id: "test-node-1".to_string(),
            name: "Test Node 1".to_string(),
            address: "127.0.0.1:8001".to_string(),
            status: NodeStatus::Online,
            version: "1.0.0".to_string(),
            health_score: 100,
            cpu_usage: 20.0,
            memory_usage: 40.0,
            replication_status: vec![],
            last_heartbeat: Some(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs() as i64,
            ),
            last_seen: Instant::now(),
            data_center: Some("dc1".to_string()),
            is_leader: true,
            additional_metadata: std::collections::HashMap::new(),
        };

        registry.register(node).await.unwrap();

        let leader = manager.elect_leader().await;
        assert_eq!(leader.unwrap().id, "test-node-1");
    }
}

use std::time::{SystemTime, UNIX_EPOCH};
