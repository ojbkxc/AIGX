//! 分布式节点管理
//!
//! 提供集群节点发现、注册和管理功能。

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributedNode {
    pub id: String,
    pub name: String,
    pub address: String,
    pub status: NodeStatus,
    pub version: String,
    pub health_score: u8,
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub replication_status: Vec<NodeReplicationStatus>,
    pub last_heartbeat: Option<i64>,
    pub last_seen: Instant,
    pub data_center: Option<String>,
    pub is_leader: bool,
    pub additional_metadata: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NodeStatus {
    Online,
    Offline,
    syncing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeReplicationStatus {
    pub channel_id: String,
    pub status: ReplicationStatus,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum ReplicationStatus {
    Synced,
    Syncing,
    SyncFailed,
}

#[derive(Clone)]
pub struct NodeRegistry {
    nodes: Arc<RwLock<std::collections::HashMap<String, DistributedNode>>>,
    my_id: String,
}

impl NodeRegistry {
    pub fn new(my_id: impl Into<String>) -> Self {
        Self {
            nodes: Arc::new(RwLock::new(std::collections::HashMap::new())),
            my_id: my_id.into(),
        }
    }

    pub async fn register(&self, node: DistributedNode) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        nodes.insert(node.id.clone(), node);
        Ok(())
    }

    pub async fn get_node(&self, id: &str) -> Option<DistributedNode> {
        let nodes = self.nodes.read().await;
        nodes.get(id).map(|n| n.clone())
    }

    pub async fn get_all_nodes(&self) -> Vec<DistributedNode> {
        let nodes = self.nodes.read().await;
        nodes.values().cloned().collect()
    }

    pub async fn get_online_nodes(&self) -> Vec<DistributedNode> {
        let nodes = self.nodes.read().await;
        nodes
            .values()
            .filter(|n| matches!(n.status, NodeStatus::Online))
            .cloned()
            .collect()
    }

    pub async fn get_leader(&self) -> Option<DistributedNode> {
        let nodes = self.nodes.read().await;
        nodes
            .values()
            .find(|n| n.is_leader && matches!(n.status, NodeStatus::Online))
            .cloned()
    }

    pub async fn update_status(&self, id: &str, status: NodeStatus) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        if let Some(node) = nodes.get_mut(id) {
            node.status = status;
            Ok(())
        } else {
            Err(format!("Node {} not found", id))
        }
    }

    pub async fn update_heartbeat(&self, id: &str) -> Result<(), String> {
        let mut nodes = self.nodes.write().await;
        let now = SystemTime::now();
        let timestamp = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        if let Some(node) = nodes.get_mut(id) {
            node.last_heartbeat = Some(timestamp);
            node.last_seen = Instant::now();

            if matches!(node.status, NodeStatus::Offline) {
                node.status = NodeStatus::Online;
            }

            // 更新健康分数
            self.update_health_score(node).await;
            Ok(())
        } else {
            Err(format!("Node {} not found", id))
        }
    }

    pub async fn heartbeat_timeout(&self, timeout: Duration) -> Vec<String> {
        let mut timeout_nodes = vec![];
        let mut nodes = self.nodes.write().await;

        for (id, node) in nodes.iter_mut() {
            let elapsed = node.last_seen.elapsed();
            if elapsed > timeout && elapsed > Duration::from_secs(30) {
                node.status = NodeStatus::Offline;
                timeout_nodes.push(id.clone());
            }
        }

        timeout_nodes
    }

    async fn update_health_score(&self, _node: &mut DistributedNode) {
        // 这里可以添加更复杂的健康评分逻辑
        // 目前简化为基于在线状态的分数
        let online_nodes = self.get_online_nodes().await.len();
        let total_nodes = self.nodes.read().await.len();
        let health_score = if total_nodes > 0 {
            (online_nodes as f32 / total_nodes as f32 * 100.0) as u8
        } else {
            50
        };
        _node.health_score = health_score;
    }

    pub fn get_my_id(&self) -> &str {
        &self.my_id
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let registry = NodeRegistry::new("node-1");

    // 注册节点
    let node = DistributedNode {
        id: "node-1".to_string(),
        name: "Master Node".to_string(),
        address: "127.0.0.1:9527".to_string(),
        status: NodeStatus::Online,
        version: "1.0.0".to_string(),
        health_score: 100,
        cpu_usage: 30.0,
        memory_usage: 50.0,
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

    registry.register(node).await?;

    // 模拟心跳
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    loop {
        interval.tick().await;
        registry.update_heartbeat("node-1").await?;
        println!("Node heartbeat updated");
    }
}
