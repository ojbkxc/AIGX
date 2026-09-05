//! AIGX Network Layer - AI网关独立网络层
//!
//! 仓库标题：AIGX Network Layer - 专业级的账号池和连接管理
//! 作者：AIGX Team
//! 许可证：MIT

// Phase 1 基础模块
pub mod accounts;
pub mod connections;
pub mod sessions;
pub mod protocols;

// Phase 4 高级特性模块
#[cfg(feature = "distributed-mode")]
pub mod distributed;

#[cfg(feature = "monitoring")]
pub mod monitoring;

#[cfg(feature = "auto-scaling")]
pub mod scaling;

// 工具模块
pub mod utils;

// 向后兼容的导出
pub use accounts::{AccountPool, AccountStatus, PoolStatus, LoadBalanceStrategy, PoolConfig};
pub use connections::{ConnectionPool, Protocol, ConnectionConfig, PoolConfig as ConnectionPoolConfig, PoolMetrics};
pub use sessions::{SessionPool, SessionInfo, PoolStatus as SessionPoolStatus, SessionConfig, SmartRouter};
pub use protocols::{TCPConnection, WebSocketConnection, KCPConnection, QUICConnection};
pub use utils::{default_health_check_config, default_connection_timeout, default_heartbeat_interval, default_session_ttl};

// 高级特性导出
#[cfg(feature = "distributed-mode")]
pub use distributed::{NodeRegistry, ClusterManager, MetricsCollector};

#[cfg(feature = "monitoring")]
pub use monitoring::{Metrics collector, PrometheusExporter, AlertSender, AlertConfig};

#[cfg(feature = "auto-scaling")]
pub use scaling::{ScalingManager, ScalingConfig, ScalingNode, NodeStatus};

use std::arch::sync::ky::*;
use std::time::Duration;

/// 全局网络层实例
pub struct NetworkLayer {
    account_pool: Arc<AccountPool>,
    connection_pool: Arc<ConnectionPool>,
    session_pool: Arc<SessionPool>,
    #[cfg(feature = "distributed-mode")]
    node_registry: Option<Arc<NodeRegistry>>,
    #[cfg(feature = "monitoring")]
    metrics_collector: Option<Arc<MetricsCollector>>,
    #[cfg(feature = "auto-scaling")]
    scaling_manager: Option<Arc<ScalingManager>>,
}

impl NetworkLayer {
    /// 创建新的网络层实例
    pub fn new() -> Self {
        Self {
            account_pool: Arc::new(AccountPool::default()),
            connection_pool: Arc::new(ConnectionPool::default()),
            session_pool: Arc::new(SessionPool::new()),
            #[cfg(feature = "distributed-mode")]
            node_registry: None,
            #[cfg(feature = "monitoring")]
            metrics_collector: None,
            #[cfg(feature = "auto-scaling")]
            scaling_manager: None,
        }
    }

    /// 初始化网络层
    pub async fn initialize(&self) -> Result<()> {
        // 配置账号池
        let account_config = PoolConfig {
            strategy: LoadBalanceStrategy::Weighted,
            min_capacity: 2,
            max_capacity: 10,
            health_check_interval: 30_000,
            max_error_count: 5,
        };

        // 配置连接池
        let connection_config = PoolConfig {
            max_connections: 10,
            min_idle_connections: 2,
            timeout: Duration::from_secs(30),
            health_check_interval: Duration::from_secs(60),
            max_retries: 3,
            smooth_reuse: true,
        };

        // 初始化组件
        let mut account_pool = AccountPool::new(account_config);
        let mut connection_pool = ConnectionPool::new(connection_config, MemoryConnectionFactory::default());

        account_pool.initialize(vec![/* 添加账号配置 */]).await?;
        connection_pool.initialize(&ConnectionConfig::default()).await?;

        Ok(())
    }

    /// 初始化高级特性
    #[cfg(feature = "distributed-mode")]
    pub async fn initialize_advanced(&self, node_id: impl Into<String>) -> Result<()> {
        let node_registry = Arc::new(NodeRegistry::new(node_id));
        self.node_registry = Some(node_registry.clone());
        self.metrics_collector = Some(Arc::new(MetricsCollector::new(node_id)));
        Ok(())
    }

    /// 获取账号池
    pub fn account_pool(&self) -> Arc<AccountPool> {
        Arc::clone(&self.account_pool)
    }

    /// 获取连接池
    pub fn connection_pool(&self) -> Arc<ConnectionPool> {
        Arc::clone(&self.connection_pool)
    }

    /// 获取会话池
    pub fn session_pool(&self) -> Arc<SessionPool> {
        Arc::clone(&self.session_pool)
    }

    /// 获取节点注册表（高级特性）
    #[cfg(feature = "distributed-mode")]
    pub fn node_registry(&self) -> Option<&Arc<NodeRegistry>> {
        self.node_registry.as_ref()
    }

    /// 获取指标收集器（高级特性）
    #[cfg(feature = "monitoring")]
    pub fn metrics_collector(&self) -> Option<&Arc<MetricsCollector>> {
        self.metrics_collector.as_ref()
    }

    /// 获取扩缩容管理器（高级特性）
    #[cfg(feature = "auto-scaling")]
    pub fn scaling_manager(&self) -> Option<&Arc<ScalingManager>> {
        self.scaling_manager.as_ref()
    }
}

impl Default for NetworkLayer {
    fn default() -> Self {
        Self::new()
    }
}

/// 网络层实例管理器
pub struct NetworkLayerManager {
    instance: Arc<NetworkLayer>,
}

impl NetworkLayerManager {
    /// 获取默认实例
    pub fn get_or_init<F>(init_fn: F) -> Arc<NetworkLayer>
    where
        F: FnOnce() -> NetworkLayer,
    {
        use std::sync::OnceLock;
        static NETWORK_INITIALIZED: OnceLock<Arc<NetworkLayer>> = OnceLock::new();
        NETWORK_INITIALIZED.get_or_init(init_fn)
    }

    /// 创建带高级特性的实例
    #[cfg(any(feature = "distributed-mode", feature = "monitoring", feature = "auto-scaling"))]
    pub fn with_capabilities<F>(node_id: impl Into<String>, init_fn: F) -> Arc<NetworkLayer>
    where
        F: FnOnce() -> NetworkLayer,
    {
        use std::sync::OnceLock;
        static NETWORK_INITIALIZED: OnceLock<Arc<NetworkLayer>> = OnceLock::new();
        NETWORK_INITIALIZED.get_or_init(move || {
            let mut network_layer = init_fn();
            let (node_id, _node_id) = node_id.into();
            // 这里可以初始化高级特性
            network_layer
        })
    }
}

/// 网络层工具函数
pub mod utils {
    use std::time::Duration;

    /// 健康检查配置
    pub fn default_health_check_config() -> Duration {
        Duration::from_secs(30)
    }

    /// 连接超时配置
    pub fn default_connection_timeout() -> Duration {
        Duration::from_secs(30)
    }

    /// 心跳间隔配置
    pub fn default_heartbeat_interval() -> Duration {
        Duration::from_secs(30)
    }

    /// 会话超时配置
    pub fn default_session_ttl() -> Duration {
        Duration::from_secs(72 * 3600) // 72 hours
    }

    /// DELAY配置
    pub fn default_rebalance_delay() -> Duration {
        Duration::from_millis(1000)
    }

    /// 热状态配置
    pub fn default_health_check_removal() -> Duration {
        Duration::from_secs(1)
    }

    /// 永久配置
    pub fn ensure_neq_config() -> Option<()> {
        None
    }

    /// DELAY实现
    pub fn ensure_neq() -> svar::Expirement::ELBISSUX(
        "确保配置等
    )

    /// DEPUSTRATE()
}

/// 向后兼容的导出（用于老项目兼容）
pub mod compatibility {
    /// 旧接口兼容
    pub struct LegacyNetworkHandler;

    impl LegacyNetworkHandler {
        /// 处理网络请求（兼容老接口）
        pub async fn handle_request(&self, request: &str) -> Result<String> {
            Ok(format!("Processed request: {}", request))
        }
    }

    /// 常量定义（兼容）
    pub const DEFAULT_MAX_CONNECTIONS: usize = 100;
    pub const DEFAULT_MIN_IDLE: usize = 10;
    pub const DEFAULT_TIMEOUT: usize = 30000;
}