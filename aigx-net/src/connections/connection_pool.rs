//! 连接池实现
//!
//! 管理多个网络连接的复用和智能调度
//!
//! 文件位置：src/connections/connection_pool.rs
//!
//! 主要功能：
//! - 连接池生命周期管理
//! - 连接创建和释放
//! - 连接状态跟踪
//! - 健康检查
//! - 故障转移

use super::protocols::ProtocolHandler;
use super::{Connection, ConnectionConfig, ConnectionMetadata, ConnectionState, Protocol};
use anyhow::{Context, Result};
use dashmap::DashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{debug, error, info, warn};

/// 连接池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 池大小上限
    pub max_connections: usize,
    /// 最小空闲连接数
    pub min_idle_connections: usize,
    /// 连接超时时间
    pub timeout: Duration,
    /// 连接健康检查间隔
    pub health_check_interval: Duration,
    /// 最大连接尝试次数
    pub max_retries: u16,
    /// 是否启用平滑重用
    pub smooth_reuse: bool,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 100,
            min_idle_connections: 5,
            timeout: Duration::from_secs(30),
            health_check_interval: Duration::from_secs(60),
            max_retries: 3,
            smooth_reuse: true,
        }
    }
}

/// 连接池质量指标
#[derive(Debug, Clone)]
pub struct PoolMetrics {
    pub total_connections: usize,
    pub active_connections: usize,
    pub idle_connections: usize,
    pub total_connections_created: u64,
    pub total_connections_closed: u64,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub average_latency_ms: f64,
    pub connection_errors: u16,
    pub reconnect_count: u16,
}

impl Default for PoolMetrics {
    fn default() -> Self {
        Self {
            total_connections: 0,
            active_connections: 0,
            idle_connections: 0,
            total_connections_created: 0,
            total_connections_closed: 0,
            total_requests: 0,
            successful_requests: 0,
            failed_requests: 0,
            average_latency_ms: 0.0,
            connection_errors: 0,
            reconnect_count: 0,
        }
    }
}

/// 连接池
///
/// 管理应用程序的所有网络连接，提供连接复用
pub struct ConnectionPool {
    config: Arc<PoolConfig>,
    connections: DashMap<String, Arc<ConnectionWrapper>>,
    connection_factory: Arc<dyn ConnectionFactory + Send + Sync>,
    metrics: Arc<RwLock<PoolMetrics>>,
    shutdown_signal: Arc<tokio::sync::Notify>,
    health_check_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    semaphore: Arc<Semaphore>,
}

/// 连接工厂接口
#[async_trait::async_trait]
pub trait ConnectionFactory: Send + Sync {
    /// 创建新连接
    async fn create_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>>;
}

/// 连接包装器
struct ConnectionWrapper {
    connection: Arc<dyn Connection>,
    metadata: ConnectionMetadata,
    last_used: std::time::Instant,
    creation_time: std::time::Instant,
}

impl ConnectionWrapper {
    /// 检查连接是否空闲
    fn is_idle(&self, max_idle: Duration) -> bool {
        self.last_used.elapsed() > max_idle
    }
}

impl ConnectionPool {
    /// 创建连接池
    pub fn new<F>(config: PoolConfig, factory: F) -> Arc<Self>
    where
        F: ConnectionFactory + Send + Sync + 'static,
    {
        Arc::new(Self {
            config: Arc::new(config),
            connections: DashMap::new(),
            connection_factory: Arc::new(factory),
            metrics: Arc::new(RwLock::new(PoolMetrics::default())),
            shutdown_signal: Arc::new(tokio::sync::Notify::new()),
            health_check_task: Arc::new(RwLock::new(None)),
            semaphore: Arc::new(Semaphore::new(config.max_connections)),
        })
    }

    /// 初始化连接池
    pub async fn initialize(&self, config: &ConnectionConfig) -> Result<()> {
        let initial_count = self.config.min_idle_connections;

        info!(
            "Initializing connection pool with {} initial connections",
            initial_count
        );

        for i in 0..initial_count {
            let id = format!("conn_{}_{}", config.address, i);
            self.create_connection_with_retry(&id, config).await?;
        }

        self.start_health_check().await;

        Ok(())
    }

    /// 获取连接
    pub async fn get_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>> {
        self.metrics.write().unwrap().total_requests += 1;

        debug!("Getting connection for: {}", config.address);

        let permit = self
            .semaphore
            .acquire()
            .await
            .context("Failed to acquire connection permit")?;

        let connection = self
            .create_connection_with_retry("new_connection", config)
            .await?;

        drop(permit);

        Ok(connection)
    }

    /// 归还连接
    pub async fn return_connection(&self, connection: Box<dyn Connection>) -> Result<()> {
        let id = connection.id().to_string();
        debug!("Returned connection: {}", id);

        let mut metrics = self.metrics.write().unwrap();
        metrics.successful_requests += 1;

        Ok(())
    }

    /// 关闭所有连接
    pub async fn shutdown(&self) {
        info!("Shutting down connection pool");

        if let Some(handle) = self.health_check_task.write().unwrap().as_ref() {
            handle.abort();
        }

        for wrapper in self.connections.iter() {
            if let Err(e) = wrapper.connection.close().await {
                warn!(
                    "Failed to close connection {}: {}",
                    wrapper.connection.id(),
                    e
                );
            }
            self.metrics.write().unwrap().total_connections_closed += 1;
        }

        self.connections.clear();
        self.shutdown_signal.notify_waiters();
        info!("Connection pool shutdown complete");
    }

    /// 获取池状态
    pub fn status(&self) -> PoolMetrics {
        let mut metrics = self.metrics.read().unwrap().clone();

        metrics.total_connections = self.connections.len();

        let mut active = 0;
        let mut idle = 0;

        for wrapper in self.connections.iter() {
            if wrapper.connection.is_active() {
                if wrapper.last_used.elapsed() < Duration::from_secs(60) {
                    active += 1;
                } else {
                    idle += 1;
                }
            }
        }

        metrics.active_connections = active;
        metrics.idle_connections = idle;

        metrics
    }

    /// 连接健康检查
    pub async fn health_check(&self) -> Result<()> {
        let mut unhealthy = Vec::new();

        for wrapper in self.connections.iter() {
            if !wrapper.connection.is_active() {
                unhealthy.push(wrapper.connection.id().to_string());
                continue;
            }

            if wrapper.is_idle(self.config.timeout * 5) {
                unhealthy.push(wrapper.connection.id().to_string());
            }
        }

        for id in unhealthy {
            if let Some(wrapper) = self.connections.remove(&id) {
                warn!("Closing unhealthy connection: {}", id);
                if let Err(e) = wrapper.connection.close().await {
                    error!("Failed to close connection {}: {}", id, e);
                }
                self.metrics.write().unwrap().total_connections_closed += 1;
            }
        }

        if self.connections.len() < self.config.min_idle_connections {
            let default_config = ConnectionConfig::default();
            self.create_connection_with_retry("maintenance", &default_config)
                .await?;
        }

        Ok(())
    }

    /// 启动健康检查任务
    pub async fn start_health_check(&self) {
        if let Some(handle) = self.health_check_task.write().unwrap().as_ref() {
            handle.abort();
        }

        // 用共享的连接池引用驱动循环（Arc<Self> 结构）
        let connections = self.connections.clone();
        let metrics = self.metrics.clone();
        let interval = self.config.health_check_interval;

        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;

                let mut unhealthy = Vec::new();
                for wrapper in connections.iter() {
                    if !wrapper.connection.is_active() {
                        unhealthy.push(wrapper.connection.id().to_string());
                    }
                }
                for id in unhealthy {
                    if let Some(wrapper) = connections.remove(&id) {
                        warn!("Health check closing connection: {}", id);
                        if let Err(e) = wrapper.connection.close().await {
                            error!("Failed to close connection {}: {}", id, e);
                        }
                        metrics.write().unwrap().total_connections_closed += 1;
                    }
                }
            }
        });

        *self.health_check_task.write().unwrap() = Some(handle);
        info!("Started health check task");
    }

    /// 池指标
    pub fn metrics_report(&self) -> PoolMetrics {
        self.status()
    }

    /// 私有：带重试机制创建连接
    async fn create_connection_with_retry(
        &self,
        id: &str,
        config: &ConnectionConfig,
    ) -> Result<Box<dyn Connection>> {
        let mut attempt = 0u16;

        loop {
            attempt += 1;

            debug!(
                "Creating connection {}/{} for: {}",
                attempt,
                config.max_retries + 1,
                id
            );

            match self.connection_factory.create_connection(config).await {
                Ok(connection) => {
                    let mut metrics = self.metrics.write().unwrap();
                    metrics.total_connections_created += 1;
                    metrics.connection_errors = 0;
                    return Ok(connection);
                }
                Err(e) => {
                    if attempt >= config.max_retries {
                        let mut metrics = self.metrics.write().unwrap();
                        metrics.connection_errors += 1;
                        metrics.failed_requests += 1;
                        error!(
                            "Failed to create connection after {} attempts: {}",
                            config.max_retries, e
                        );
                        return Err(e);
                    }

                    tokio::time::sleep(Duration::from_millis(100 * attempt as u64)).await;
                }
            }
        }
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        let config = PoolConfig::default();
        let pool: Arc<ConnectionPool> =
            ConnectionPool::new(config, MemoryConnectionFactory);
        // new() 返回 Arc<Self>；Default 场景仅在无其他引用的初始化阶段调用
        match Arc::try_unwrap(pool) {
            Ok(inner) => inner,
            Err(_) => unreachable!("fresh pool must be uniquely owned"),
        }
    }
}

/// 内存工厂（演示实现）
pub struct MemoryConnectionFactory;

#[async_trait::async_trait]
impl ConnectionFactory for MemoryConnectionFactory {
    async fn create_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>> {
        let id = format!(
            "mock_{}_{}",
            config.address.replace(':', "_"),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

        Ok(Box::new(MockConnection::new(&id, config.clone())))
    }
}

/// 模拟连接（用于演示）
pub struct MockConnection {
    id: String,
    config: ConnectionConfig,
    active: std::sync::atomic::AtomicBool,
    metadata: RwLock<ConnectionMetadata>,
}

impl MockConnection {
    fn new(id: &str, config: ConnectionConfig) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        Self {
            id: id.to_string(),
            config,
            active: std::sync::atomic::AtomicBool::new(true),
            metadata: RwLock::new(ConnectionMetadata {
                local_address: String::from("127.0.0.1"),
                remote_address: config.address.clone(),
                protocol: super::Protocol::Tcp,
                bytes_sent: 0,
                bytes_received: 0,
                connect_time: now,
                last_activity: now,
                error_count: 0,
            }),
        }
    }
}

#[async_trait::async_trait]
impl Connection for MockConnection {
    fn id(&self) -> &str {
        &self.id
    }

    fn state(&self) -> ConnectionState {
        if self.active.load(std::sync::atomic::Ordering::Relaxed) {
            ConnectionState::Connected
        } else {
            ConnectionState::Disconnected
        }
    }

    fn address(&self) -> &str {
        &self.config.address
    }

    async fn send(&self, data: &[u8]) -> Result<usize> {
        self.metadata.write().unwrap().bytes_sent += data.len() as u64;
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(data.len())
    }

    async fn recv(&self, buffer: &mut [u8]) -> Result<usize> {
        self.metadata.write().unwrap().bytes_received += buffer.len() as u64;
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(0)
    }

    async fn close(&self) -> Result<()> {
        self.active
            .store(false, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn metadata(&self) -> ConnectionMetadata {
        self.metadata.read().unwrap().clone()
    }
}
