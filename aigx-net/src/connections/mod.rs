//! 连接池管理模块
//!
//! 提供网络连接的复用、健康检查和故障转移功能
//!
//! 特性：
//! - 多传输协议支持（TCP/KCP/WebSocket/QUIC）
//! - 连接池管理和复用
//! - 心跳检测和自动重连
//! - TLS 1.3 双向认证
//! - 智能连接策略

pub mod connection;
pub mod connection_pool;
pub mod protocols;
pub mod health_check;

pub use connection::*;
pub use connection_pool::*;
pub use protocols::*;
pub use health_check::*;

use std::time::Duration;
use anyhow::{Result, Context};
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

/// 连接协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    Tcp,           // 标准 TCP
    Kcp,           // KCP 传输协议
    WebSocket,     // WebSocket 连接
    Quic,          // QUIC 协议
    Http1,         // HTTP/1.1
    Http3,         // HTTP/3
}

/// 网络连接配置
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// 连接地址
    pub address: String,
    /// 连接协议
    pub protocol: Protocol,
    /// 目标端口（可选）
    pub port: Option<u16>,
    /// 是否启用 TLS
    pub tls: bool,
    /// TLS 证书路径（可选）
    pub cert_path: Option<String>,
    /// TLS 密钥路径（可选）
    pub key_path: Option<String>,
    /// 心跳间隔（秒）
    pub heartbeat_interval: u64,
    /// 最大空闲时间（秒）
    pub max_idle_time: u64,
    /// 最大重试次数
    pub max_retries: u16,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            address: "127.0.0.1".to_string(),
            port: Some(443),
            protocol: Protocol::Tcp,
            tls: true,
            cert_path: None,
            key_path: None,
            heartbeat_interval: 30,
            max_idle_time: 3600,
            max_retries: 3,
        }
    }
}

/// 连接类型
#[derive(Debug, Clone)]
pub enum ConnectionType {
    /// 管道连接
    Pipe,
    /// 升级连接 (HTTP/2)
    Stream,
    /// 长连接
    Long,
}

/// 连接状态
#[derive(Debug, Clone)]
pub enum ConnectionState {
    /// 创建中
    Connecting,
    /// 已连接
    Connected,
    /// 重连中
    Reconnecting,
    /// 已断开
    Disconnected,
    /// 错误状态
    Error(String),
}

/// 连接元数据
#[derive(Debug, Clone)]
pub struct ConnectionMetadata {
    pub local_address: String,
    pub remote_address: String,
    pub protocol: Protocol,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub connect_time: u64,
    pub last_activity: u64,
    pub error_count: u16,
}

impl ConnectionMetadata {
    /// 更新活跃时间
    pub fn update_activity(&mut self) {
        self.last_activity = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
    }

    /// 检查是否超时
    pub fn is_idle_timeout(&self, max_idle: Duration) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let idle_seconds = now - self.last_activity / 1000;

        idle_seconds > max_idle.as_secs()
    }

    /// 获取连接质量评分
    pub fn quality_score(&self) -> f64 {
        let current_time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let idle_seconds = (current_time - self.last_activity / 1000) as f64;

        // 基础分 1.0，越空闲分数越高
        let freshness = 1.0 - (idle_seconds.min(3600.0) / 3600.0) * 0.5;

        // 根据错误次数降低分数
        let error_factor = (1.0 - self.error_count as f64 / 10.0).max(0.2);

        // 数据吞吐量因子（简单的估计）
        let stream_factor = if self.bytes_sent > 0 && self.bytes_received > 0 {
            let ratio = self.bytes_received as f64 / (self.bytes_sent as f64 + 1.0);
            ratio.min(1.2)
        } else {
            1.0
        };

        freshness * error_factor * stream_factor
    }
}

/// 连接接口
///
/// 定义所有连接类型共有的操作
pub trait Connection: Send + Sync {
    /// 连接标识
    fn id(&self) -> &str;

    /// 连接状态
    fn state(&self) -> ConnectionState;

    /// 连接地址
    fn address(&self) -> &str;

    /// 升级连接
    async fn upgrade(&self) -> Result<Box<dyn Connection>>;

    /// 发送数据
    async fn send(&self, &[u8]) -> Result<usize>;

    /// 接收数据
    async fn recv(&self, buffer: &mut [u8]) -> Result<usize>;

    /// 关闭连接
    async fn close(&self) -> Result<()>;

    /// 是否活跃
    fn is_active(&self) -> bool;

    /// 指标收集
    fn metrics(&self) -> ConnectionMetadata;
}

/// 创建连接配置
pub fn create_connection_config(
    address: impl Into<String>,
    protocol: Protocol,
    tls: bool,
) -> ConnectionConfig {
    ConnectionConfig {
        address: address.into(),
        protocol,
        tls,
        ..Default::default()
    }
}

/// 创建HTTP连接配置
pub fn create_http_connection_config(
    address: impl Into<String>,
) -> ConnectionConfig {
    ConnectionConfig {
        address: address.into(),
        protocol: Protocol::Http1,
        tls: true,
        ..Default::default()
    }
}

/// 创建WebSocket连接配置
pub fn create_websocket_connection_config(
    address: impl Into<String>,
) -> ConnectionConfig {
    ConnectionConfig {
        address: address.into(),
        protocol: Protocol::WebSocket,
        tls: true,
        heartbeat_interval: 20,
        max_idle_time: 1800, // 30分钟
        ..Default::default()
    }
}