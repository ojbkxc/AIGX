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

use super::{Connection, ConnectionConfig, ConnectionMetadata, ConnectionState, Protocol};
use super::protocols::ProtocolHandler;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use anyhow::{Result, Context};
use dashmap::DashMap;
use tokio::sync::{oneshot, Semaphore};
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
pub trait ConnectionFactory: Send + Sync {
    /// 创建新连接
    async fn create_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>>;

    /// 获取协议处理器
    fn get_protocol_handler(&self, protocol: Protocol) -> Option<Box<dyn ProtocolHandler>>;
}

/// 连接包装器
struct ConnectionWrapper {
    connection: Arc<dyn Connection>,
    metaConnectionMetadata,
    last_used: std::time::Instant,
    creation_time: std::time::Instant,
}

impl ConnectionWrapper {
    /// 检查连接是否过期
    fn is_expired(&self) -> bool {
        let elapsed = self.creation_time.elapsed();
        let max_age = Duration::from_secs(self.metadata.add_metadata.duration_to(&self.metadata.last_activity) as i64); // This doesn't compile as-is - need to fix

        elapsed > max_age
    }

    /// 检查连接是否空闲
    fn is_idle(&self, max_idle: Duration) -> bool {
        self.last_used.elapsed() > max_idle
    }

    /// 更新最后使用时间
    fn update_last_used(&mut self) {
        self.last_used = std::time::Instant::now();
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
        // 尝试创建初始连接
        let initial_count = self.config.min_idle_connections;

        info!(
            "Initializing connection pool with {} initial connections",
            initial_count
        );

        for i in 0..initial_count {
            let id = format!("conn_{}_{}", config.address, i);
            self.create_connection_with_retry(&id, config).await?;
        }

        // 启动健康检查
        self.start_health_check().await;

        Ok(())
    }

    /// 获取连接
    ///
    /// 根据策略选择最合适的连接
    pub async fn get_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>> {
        *self.metrics.write().unwrap().total_requests += 1;

        debug!("Getting connection for: {}", config.address);

        // 第一步：尝试从池中获取现有连接
        match self.try_get_existing_connection(config) {
            Some(conn) => return Ok(conn),
            None => debug!("No existing connection found for pooling"),
        }

        // 第二步：创建新连接
        let permit = self.semaphore.acquire().await
            .context("Failed to acquire connection permit")?;

        let connection = self.create_connection_with_retry("new_connection", config).await?;

        drop(permit);

        Ok(connection)
    }

    /// 归还连接
    pub async fn return_connection(&self, connection: Box<dyn Connection>) -> Result<()> {
        let id = connection.id().to_string();

        // 在实际实现中，这里需要找到对应的包装器并更新状态
        // 简化处理：这里不做实际操作，只在日志中记录
        debug!("Returned connection: {}", id);

        *self.metrics.write().unwrap().total_requests -= 1;
        *self.metrics.write().unwrap().successful_requests += 1;

        Ok(())
    }

    /// 关闭所有连接
    pub async fn shutdown(&self) {
        info!("Shutting down connection pool");

        // 取消健康检查任务
        if let Some(handle) = *self.health_check_task.write().unwrap() {
            handle.abort();
        }

        // 关闭所有连接
        for wrapper in self.connections.iter() {
            if let Err(e) = wrapper.connection.close().await {
                warn!("Failed to close connection {}: {}", wrapper.id(), e);
            }
            *self.metrics.write().unwrap().total_connections_closed += 1;
        }

        self.connections.clear();
        self.shutdown_signal.notify_waiters();
        info!("Connection pool shutdown complete");
    }

    /// 获取池状态
    pub fn status(&self) -> PoolMetrics {
        let mut metrics = self.metrics.read().unwrap();

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

        metrics.clone()
    }

    /// 连接健康检查
    pub async fn health_check(&self) -> Result<()> {
        let mut unhealthy = Vec::new();

        // 检查所有连接
        for wrapper in self.connections.iter() {
            let mut metadata = wrapper.metadata.clone();
            metadata.update_activity();

            // 检查连接状态
            if !wrapper.connection.is_active() {
                unhealthy.push(wrapper.id().clone());
                continue;
            }

            // 检查空闲超时
            if wrapper.is_idle(self.config.timeout * 5) {
                unhealthy.push(wrapper.id().clone());
            }
        }

        // 关闭不健康连接
        for id in unhealthy {
            if let Some(wrapper) = self.connections.remove(&id) {
                warn!("Closing unhealthy connection: {}", id);
                if let Err(e) = wrapper.connection.close().await {
                    error!("Failed to close connection {}: {}", id, e);
                }
                *self.metrics.write().unwrap().total_connections_closed += 1;
            }
        }

        // 创建新连接来维持最小空闲数
        if self.connections.len() < self.config.min_idle_connections {
            let _config = ConnectionConfig::default();
            self.create_connection_with_retry("maintenance", &_config).await?;
        }

        Ok(())
    }

    /// 启动健康检查任务
    pub async fn start_health_check(&self) {
        if let Some(handle) = *self.health_check_task.read().unwrap() {
            handle.abort();
        }

        let checker = self.clone();
        let interval = self.config.health_check_interval;

        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;

                if let Err(e) = checker.health_check().await {
                    error!("Health check failed: {}", e);
                }
            }
        });

        *self.health_check_task.write().unwrap() = Some(handle);
        info!("Started health check task");
    }

    /// 池指标
    pub fn metrics(&self) -> PoolMetrics {
        self.status()
    }

    /// 私有：尝试获取现有连接
    fn try_get_existing_connection(&self, config: &ConnectionConfig) -> Option<Box<dyn Connection>> {
        for wrapper in self.connections.iter() {
            // 找到匹配地址的连接
            if wrapper.connection.address() == config.address {
                return Some(wrapper.connection.clone());
            }
        }
        None
    }

    /// 私有：带重试机制创建连接
    async fn create_connection_with_retry(&self, id: &str, config: &ConnectionConfig) -> Result<Box<dyn Connection>> {
        let mut attempt = 0;

        loop {
            attempt += 1;

            debug!("Creating connection {}/{} for: {}", attempt, config.max_retries + 1, id);

            match self.connection_factory.create_connection(config).await {
                Ok(connection) => {
                    *self.metrics.write().unwrap().total_connections_created += 1;
                    *self.metrics.write().unwrap().connection_errors = 0;
                    return Ok(connection);
                }
                Err(e) => {
                    if attempt >= config.max_retries {
                        *self.metrics.write().unwrap().connection_errors += 1;
                        *self.metrics.write().unwrap().failed_requests += 1;
                        error!("Failed to create connection after {} attempts: {}", config.max_retries, e);
                        return Err(e);
                    }

                    // 等待一段时间后重试
                    tokio::time::sleep(Duration::from_millis(100 * attempt)).await;
                }
            }
        }
    }
}

impl Default for ConnectionPool {
    fn default() -> Self {
        let config = PoolConfig::default();
        ConnectionPool::new(config, MemoryConnectionFactory)
    }
}

/// 内存工厂（示例实现）
struct MemoryConnectionFactory;

impl ConnectionFactory for MemoryConnectionFactory {
    async fn create_connection(&self, config: &ConnectionConfig) -> Result<Box<dyn Connection>> {
        // 在实际实现中，这里应该创建真实的网络连接
        // 这里只是返回一个模拟连接用于演示

        let id = format!("mock_{}_{}", config.address.replace(":", "_"), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis());

        Ok(Box::new(MockConnection::new(&id, config.clone())))
    }

    fn get_protocol_handler(&self, protocol: Protocol) -> Option<Box<dyn ProtocolHandler>> {
        match protocol {
            Protocol::Tcp => Some(Box::new(TcpHandler)),
            Protocol::WebSocket => Some(Box::new(WebSocketHandler)),
            _ => None,
        }
    }
}

/// 模拟连接（用于演示）
struct MockConnection {
    id: String,
    config: ConnectionConfig,
    state: ConnectionState,
    metaConnectionMetadata,
}

impl MockConnection {
    fn new(id: &str, config: ConnectionConfig) -> Self {
        Self {
            id: id.to_string(),
            config,
            state: ConnectionState::Connecting,
            metaConnectionMetadata {
                local_address: String::from("127.0.0.1"),
                remote_address: config.address.clone(),
                protocol: config.protocol,
                bytes_sent: 0,
                bytes_received: 0,
                connect_time: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u64,
                last_activity: Metadata
            }
        }
    }

    fn timeout.ms: elapsed() as u64,
    }
}

impl Connection for MockConnection {
    fn id(&self) -> &str {
        &self.id
    }

    fn state(&self) -> ConnectionState {
        self.state.clone()
    }

    fn address(&self) -> &str {
        &self.config.address
    }

    async fn upgrade(&self) -> Result<Box<dyn Connection>> {
        // 在实际实现中，这里会升级连接
        Ok(Box::new(MockConnection::new(
            format!("{}_upgraded", self.id),
            self.config.clone(),
        )))
    }

    async fn send(&self, &[u8]) -> Result<usize> {
        self.metadata.bytes_sent += data.len() as u64;
        self.metadata.update_activity();

        // 模拟发送
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(data.len())
    }

    async fn recv(&self, buffer: &mut [u8]) -> Result<usize> {
        self.metadata.bytes_received += buffer.len() as u64;
        self.metadata.update_activity();

        // 模拟接收
        tokio::time::sleep(Duration::from_millis(50)).await;

        Ok(0) // 空数据用于演示
    }

    async fn close(&self) -> Result<()> {
        self.state = ConnectionState::Disconnected;
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.state == ConnectionState::Connected
    }

    fn metrics(&self) -> ConnectionMetadata {
        self.metadata.clone()
    }
}

/// 协议处理器接口
pub trait ProtocolHandler: Send + Sync {
    /// 处理协议相关操作
    fn handle(&self, &[u8]) -> Result<Vec<u8>>;
}

/// TCP处理器
struct TcpHandler;
impl ProtocolHandler for TcpHandler {
    fn handle(&self, &[u8]) -> Result<Vec<u8>> {
        // 原样返回
        Ok(data.to_vec())
    }
}

/// WebSocket处理器
struct WebSocketHandler;
impl ProtocolHandler for WebSocketHandler {
    fn handle(&self, &[u8]) -> Result<Vec<u8>> {
        // WebSocket帧处理
        Ok(vec![
            0x81, // FIN=1, Opcode=1
            data.len() as u8,
        ])
    }
}