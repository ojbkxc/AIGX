//! 会话实现模块
//!
//! 具体的会话对象实现和操作

use super::{AICloudProvider, SessionConfig, SessionInfo, SessionState};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::Result;

/// 会话传输层
#[async_trait::async_trait]
pub trait TransportLayer: Send + Sync {
    /// 发送消息
    async fn send(&self, message: &[u8]) -> Result<Vec<u8>>;

    /// 接收消息
    async fn receive(&self, buffer: &mut [u8]) -> Result<usize>;
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// 会话
///
/// 管理单个 AI 服务的会话状态和消息
pub struct Session {
    /// 会话 ID
    id: String,
    /// 服务提供商
    provider: AICloudProvider,
    /// 模型 ID
    model_id: String,
    /// 会话配置
    config: SessionConfig,
    /// 状态（原子操作避免锁）
    state: AtomicU8,
    /// 元数据
    metadata: std::sync::RwLock<HashMap<String, String>>,
    /// 最后使用时间（毫秒）
    last_used_at: AtomicI64,
    /// 连接时间戳
    connected_at: i64,
}

impl Session {
    /// 创建新会话
    pub fn new(
        id: String,
        provider: AICloudProvider,
        model_id: String,
        config: SessionConfig,
    ) -> Self {
        let created_at = now_ms();

        Self {
            id,
            provider,
            model_id,
            config,
            state: AtomicU8::new(SessionState::Creating as u8),
            metadata: std::sync::RwLock::new(HashMap::new()),
            last_used_at: AtomicI64::new(created_at),
            connected_at: created_at,
        }
    }

    /// 会话 ID
    pub fn id(&self) -> &str {
        &self.id
    }

    /// 服务提供商
    pub fn provider(&self) -> &AICloudProvider {
        &self.provider
    }

    /// 模型 ID
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// 当前状态
    pub fn state(&self) -> SessionState {
        SessionState::from_u8(self.state.load(Ordering::Relaxed))
    }

    fn set_state(&self, new_state: SessionState) {
        self.state.store(new_state as u8, Ordering::Relaxed);
    }

    /// 过期时间（unix 秒）
    pub fn expiry_time(&self) -> i64 {
        self.connected_at + self.config.session_ttl.as_secs() as i64 * 1000
    }

    /// 检查是否过期
    pub fn is_expired(&self) -> bool {
        now_ms() > self.expiry_time()
    }

    /// 检查是否达到最大消息数
    pub fn is_full(&self) -> bool {
        // 简化实现：会话级消息计数由传输层驱动
        false
    }

    /// 最后使用时间
    pub fn last_used(&self) -> i64 {
        self.last_used_at.load(Ordering::Relaxed)
    }

    /// 触摸活跃时间
    fn touch(&self) {
        self.last_used_at.store(now_ms(), Ordering::Relaxed);
    }

    /// 开始会话
    pub async fn start(&self) -> Result<()> {
        self.set_state(SessionState::Active);
        self.touch();
        Ok(())
    }

    /// 设置为使用中
    pub fn set_using(&self) {
        self.set_state(SessionState::ActiveUsing);
        self.touch();
    }

    /// 设置为空闲
    pub fn set_idle(&self) {
        self.set_state(SessionState::Active);
        self.touch();
    }

    /// 销毁会话
    pub async fn destroy(&self) -> Result<()> {
        self.set_state(SessionState::Destroyed);
        Ok(())
    }

    /// 检查是否可以被使用
    pub fn is_available(&self) -> bool {
        let state = self.state();
        (state == SessionState::Active || state == SessionState::ActiveUsing)
            && !self.is_expired()
            && !self.is_full()
    }

    /// 会话信息快照
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            provider: self.provider.clone(),
            model_id: self.model_id.clone(),
            state: self.state(),
            message_count: 0,
            last_message_id: 0,
            created_at: self.connected_at,
            last_used_at: self.last_used(),
            metadata: self
                .metadata
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}

impl SessionState {
    /// 从 u8 还原状态
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Creating,
            1 => Self::Active,
            2 => Self::ActiveUsing,
            3 => Self::Destroying,
            _ => Self::Destroyed,
        }
    }
}

/// 模拟传输层（演示用）
pub struct MockTransport;

#[async_trait::async_trait]
impl TransportLayer for MockTransport {
    async fn send(&self, _message: &[u8]) -> Result<Vec<u8>> {
        Ok(vec![b"O"[0], b"K"[0]])
    }

    async fn receive(&self, _buffer: &mut [u8]) -> Result<usize> {
        Ok(0)
    }
}
