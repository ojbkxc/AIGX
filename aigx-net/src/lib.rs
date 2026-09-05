//! AIGX Network Layer - AI网关独立网络层
//!
//! 仓库标题：AIGX Network Layer - 专业级的账号池和连接管理
//! 作者：AIGX Team
//! 许可证：MIT

// Phase 1 基础模块
pub mod accounts;
pub mod connections;
pub mod sessions;

// Phase 4 高级特性模块（feature-gated，默认关闭）
#[cfg(feature = "distributed-mode")]
pub mod distributed;

#[cfg(feature = "monitoring")]
pub mod monitoring;

#[cfg(feature = "auto-scaling")]
pub mod scaling;

// 工具模块（内联定义）
pub mod utils {
    use std::time::Duration;

    /// 健康检查间隔
    pub fn default_health_check_config() -> Duration {
        Duration::from_secs(30)
    }

    /// 连接超时
    pub fn default_connection_timeout() -> Duration {
        Duration::from_secs(30)
    }

    /// 心跳间隔
    pub fn default_heartbeat_interval() -> Duration {
        Duration::from_secs(30)
    }

    /// 会话 TTL（72 小时，与上游会话生命周期对齐）
    pub fn default_session_ttl() -> Duration {
        Duration::from_secs(72 * 3600)
    }

    /// 重平衡延迟
    pub fn default_rebalance_delay() -> Duration {
        Duration::from_millis(1000)
    }
}

// 向后兼容的导出
pub use accounts::{AccountPool, AccountStatus, LoadBalanceStrategy, PoolConfig, PoolStatus};
pub use connections::{
    ConnectionConfig, ConnectionPool, PoolConfig as ConnectionPoolConfig, PoolMetrics, Protocol,
};
pub use sessions::{
    PoolStatus as SessionPoolStatus, SessionConfig, SessionInfo, SessionPool, SmartRouter,
};

use std::sync::Arc;
use std::time::Duration;

/// 全局网络层实例
pub struct NetworkLayer {
    account_pool: Arc<AccountPool>,
    connection_pool: Arc<ConnectionPool>,
    session_pool: Arc<SessionPool>,
}

impl NetworkLayer {
    /// 创建新的网络层实例
    pub fn new() -> Self {
        Self {
            account_pool: Arc::new(AccountPool::default()),
            connection_pool: Arc::new(ConnectionPool::default()),
            session_pool: Arc::new(SessionPool::new()),
        }
    }

    /// 初始化网络层
    pub async fn initialize(&self) -> anyhow::Result<()> {
        let account_config = PoolConfig::default();
        let connection_config = ConnectionPoolConfig::default();
        let _ = (account_config, connection_config);
        // 组件初始化（账号/连接配置由运行时注入）
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
}

impl Default for NetworkLayer {
    fn default() -> Self {
        Self::new()
    }
}

/// 网络层实例管理器
pub struct NetworkLayerManager;

impl NetworkLayerManager {
    /// 获取默认实例（进程级单例）
    pub fn get_or_init<F>(init_fn: F) -> Arc<NetworkLayer>
    where
        F: FnOnce() -> NetworkLayer,
    {
        use std::sync::OnceLock;
        static NETWORK_INITIALIZED: OnceLock<Arc<NetworkLayer>> = OnceLock::new();
        NETWORK_INITIALIZED
            .get_or_init(|| Arc::new(init_fn()))
            .clone()
    }
}

/// 向后兼容的导出（用于老项目兼容）
pub mod compatibility {
    /// 旧接口兼容
    pub struct LegacyNetworkHandler;

    impl LegacyNetworkHandler {
        /// 处理网络请求（兼容老接口）
        pub async fn handle_request(&self, request: &str) -> anyhow::Result<String> {
            Ok(format!("Processed request: {}", request))
        }
    }

    /// 常量定义（兼容）
    pub const DEFAULT_MAX_CONNECTIONS: usize = 100;
    pub const DEFAULT_MIN_IDLE: usize = 10;
    pub const DEFAULT_TIMEOUT: usize = 30000;
}
