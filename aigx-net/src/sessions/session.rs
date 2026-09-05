//! 会话实现模块
//!
//! 具体的会话对象实现和操作

use super::{SessionInfo, SessionState, AICloudProvider};
use std::sync::{Arc, RwLock};
use std::time::{Duration, UNIX_EPOCH};
use std::collections::VecDeque;
use anyhow::{Result, Context};
use tracing::{debug, error, info, trace, warn};

/// 会话传输层
pub trait TransportLayer: Send + Sync {
    /// 发送消息
    async fn send(&self, message: &[u8]) -> Result<Vec<u8>>;

    /// 接收消息
    async fn receive(&self, buffer: &mut [u8]) -> Result<usize>;
}

/// 请求消息
#[derive(Debug, Clone)]
pub struct RequestMessage {
    pub id: String,
    pub messages: Vec<String>,
    pub request_metaHashMap<String, String>,
    pub timestamp: i64,
}

impl RequestMessage {
    pub fn new(id: String, messages: Vec<String>) -> Self {
        Self {
            id,
            messages,
            request_metaDefault::default(),
            timestamp: UNIX_EPOCH.elapsed()?.as_millis() as i64,
        }
    }
}

/// 会话
///
/// 管理单个AI服务的会话状态和消息
pub struct Session {
    /// 会话ID
    id: String,
    /// 服务提供商
    provider: AICloudProvider,
    /// 模型ID
    model_id: String,
    /// 会话配置
    config: SessionConfig,
    /// 状态
    state: RwLock<SessionState>,
    /// 元数据
    metaHashMap<String, String>,
    /// 会话信息
    info: SessionInfo,
    /// 消息历史
    messages: VecDeque<String>,
    /// 传输层
    transport: Arc<dyn TransportLayer>,
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
        transport: Arc<dyn TransportLayer>,
    ) -> Result<Self> {
        let created_at = UNIX_EPOCH.elapsed()?.as_millis() as i64;

        Ok(Self {
            id,
            provider,
            model_id,
            config,
            state: RwLock::new(SessionState::Creating),
            metaDefault::default(),
            info: SessionInfo {
                id: id.clone(),
                provider,
                model_id: model_id.clone(),
                state: SessionState::Creating,
                message_count: 0,
                last_message_id: 0,
                created_at,
                last_used_at: created_at,
            },
            messages: VecDeque::with_capacity(config.max_messages),
            transport,
            connected_at: created_at,
        })
    }

    /// 过期时间
    pub fn expiry_time(&self) -> i64 {
        self.connected_at + self.config.session_ttl.as_secs() as i64
    }

    /// 检查是否过期
    pub fn is_expired(&self) -> bool {
        UNIX_EPOCH.elapsed()?.as_secs() as i64 > self.expiry_time()
    }

    /// 检查是否达到最大消息数
    pub fn is_full(&self) -> bool {
        self.messages.len() >= self.config.max_messages
    }

    /// 消息数
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// 最后使用时间
    pub fn last_used(&self) -> i64 {
        self.info.last_used_at
    }

    /// 当前延迟（模拟）
    pub fn current_latency(&self) -> Option<u64> {
        // 模拟延迟值，实际使用时应该从metrics获取
        Some(rand::random::<u64>() % 500) // 0-500ms
    }

    /// 当前权重（模拟）
    pub fn current_weight(&self) -> u8 {
        let success_rate = self.success_rate();
        ((success_rate * 100.0) as u8).max(1).min(255) // 1-255
    }

    /// 成功率（模拟）
    pub fn success_rate(&self) -> f64 {
        // 模拟成功率，实际使用时应该从历史记录计算
        0.95 + (rand::random::<f64>() * 0.04) // 95.0-99.0%
    }

    /// 当前负载（模拟）
    pub fn current_load(&self) -> f64 {
        self.messages.len() as f64 / self.config.max_messages as f64
    }

    /// 错误计数（模拟）
    pub fn error_count(&self) -> u32 {
        self.messages.iter()
            .filter(|&&msg| msg == "<ERROR>")
            .count() as u32
    }

    /// 增加消息
    pub fn add_message(&mut self, message: String) {
        self.messages.push_back(message);
        self.info.message_count += 1;
        self.info.last_message_id += 1;
        self.info.last_used_at = UNIX_EPOCH.elapsed()?.as_millis() as i64;
    }

    /// 获取缓冲区消息
    pub fn get_buffer_messages(&self) -> Vec<String> {
        self.messages.iter().rev().cloned().collect()
    }

    /// 重置消息缓冲区
    pub fn reset_buffer(&mut self) {
        self.messages.clear();
        self.info.message_count = 0;
        self.info.last_message_id = 0;
    }

    /// 设置元数据
    pub fn set_metadata(&mut self, key: String, value: String) {
        self.metadata.insert(key, value);
    }

    /// 获取元数据
    pub fn get_metadata(&self, key: &str) -> Option<String> {
        self.metadata.get(key).cloned()
    }

    /// 获取所有元数据
    pub fn all_metadata(&self) -> &HashMap<String, String> {
        &self.metadata
    }

    /// 上次心跳时间
    pub fn last_heartbeat(&self) -> i64 {
        self.info.last_used_at
    }

    /// 心跳
    pub async fn heartbeat(&self) -> Result<()> {
        // 在实际实现中，这里应该发送心跳包
        // 简化版本：仅更新时间戳
        trace!("Sending heartbeat for session: {}", self.id);
        self.info.last_used_at = UNIX_EPOCH.elapsed()?.as_millis() as i64;
        Ok(())
    }

    /// 开始会话
    pub async fn start(&self) -> Result<()> {
        let mut state = self.state.write()?;

        if !state.can_transition_to(SessionState::Active) {
            return Err(anyhow::anyhow!("Invalid state transition: {:?} -> {:?}", *state, SessionState::Active));
        }

        info!(
            "Starting session: {} (provider: {:?}, model: {})",
            self.id, self.provider, self.model_id
        );

        *state = SessionState::Active;

        // 标记为活跃使用状态
        drop(state);
        self.set_using();

        // 初始化传输层
        self.transport.initialize().await?;

        Ok(())
    }

    /// 设置为使用中
    pub fn set_using(&self) {
        *self.state.write().unwrap() = SessionState::ActiveUsing;
    }

    /// 设置为空闲
    pub fn set_idle(&self) {
        *self.state.write().unwrap() = SessionState::Active;
    }

    /// 销毁会话
    pub async fn destroy(&self) -> Result<()> {
        let mut state = self.state.write()?;

        if !state.can_transition_to(SessionState::Destroying) {
            return Err(anyhow::anyhow!("Invalid state transition: {:?} -> {:?}", *state, SessionState::Destroying));
        }

        info!("Destroying session: {}", self.id);
        *state = SessionState::Destroying;

        drop(state);

        // 关闭传输层
        self.transport.shutdown().await?;

        *self.state.write().unwrap() = SessionState::Destroyed;
        Ok(())
    }

    /// 检查是否可以被使用
    pub async fn is_available(&self) -> bool {
        let state = self.state.read()?;

        if *state != SessionState::Active && *state != SessionState::ActiveUsing {
            return false;
        }

        if self.is_expired() {
            return false;
        }

        if self.is_full() {
            return false;
        }

        // 心跳检查
        self.heartbeat().await.is_ok()
    }

    /// 会话信息
    pub fn info(&self) -> SessionInfo {
        self.info.clone()
    }

    /// 循环引用（用于链表）
    pub fn clone(&self) -> Arc<Self> {
        Arc::new(Self {
            id: self.id.clone(),
            provider: self.provider,
            model_id: self.model_id.clone(),
            config: self.config.clone(),
            state: self.state.clone(),
            metaself.metadata.clone(),
            info: self.info,
            messages: self.messages.clone(),
            transport: self.transport.clone(),
            connected_at: self.connected_at,
        })
    }
}

impl Default for Session {
    fn default() -> Self {
        panic!("Session requires provider, model_id, and transport layer");
    }
}

/// 模拟传输层
struct MockTransport;

impl TransportLayer for MockTransport {
    async fn send(&self, message: &[u8]) -> Result<Vec<u8>> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(vec![b"OK"[0]])
    }

    async fn receive(&self, buffer: &mut [u8]) -> Result<usize> {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(0) // 空响应用于演示
    }
}

/// 扩展方法：状态转换判断
impl SessionState {
    /// 是否可以转换为其他状态
    fn can_transition_to(&self, new_state: SessionState) -> bool {
        matches!(
            (self, new_state),
            (SessionState::Creating, SessionState::Active) |
            (SessionState::Active, SessionState::ActiveUsing) |
            (SessionState::ActiveUsing, SessionState::Active) |
            (SessionState::Active, SessionState::Destroying) |
            (SessionState::ActiveUsing, SessionState::Destroying) |
            (SessionState::Creating, SessionState::Destroyed) |
            (SessionState::ActiveUsing, SessionState::Destroyed)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_creation() {
        let provider = AICloudProvider::OpenAI;
        let model_id = "gpt-4".to_string();
        let config = SessionConfig::default();
        let transport = Arc::new(MockTransport);

        let session = Session::new(
            "test-session".to_string(),
            provider,
            model_id,
            config,
            transport
        ).unwrap();

        assert_eq!(session.id(), "test-session");
        assert_eq!(session.provider(), provider);
        assert_eq!(session.model_id(), "gpt-4");
    }

    #[tokio::test]
    async fn test_session_state_transitions() {
        let provider = AICloudProvider::OpenAI;
        let model_id = "gpt-4".to_string();
        let config = SessionConfig::default();
        let transport = Arc::new(MockTransport);

        let session = Session::new(
            "test-session".to_string(),
            provider,
            model_id,
            config,
            transport
        ).unwrap();

        session.start().await.unwrap();

        // 验证状态转换
        let state = session.state();
        assert_eq!(state, SessionState::ActiveUsing);
    }
}