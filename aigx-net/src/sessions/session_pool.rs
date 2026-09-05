//! 会话池管理
//!
//! 管理多个会话的复用和智能路由

use super::{Session, SessionConfig, SessionState};
use super::router::SmartRouter;
use std::collections::HashMap;
use std::sync::Arc;
use dashmap::DashMap;
use tokio::time::{Duration, Instant};
use anyhow::{Result, Context};
use tracing::{debug, info, warn};

/// 会话池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 池大小
    pub max_sessions: usize,
    /// 最小空闲会话
    pub min_idle_sessions: usize,
    /// 会话清理间隔
    pub cleanup_interval: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_sessions: 50,
            min_idle_sessions: 5,
            cleanup_interval: Duration::from_secs(300), // 5分钟
        }
    }
}

/// 会话池状态
#[derive(Debug, Clone)]
pub struct PoolStatus {
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub idle_sessions: usize,
}

/// 会话池
pub struct SessionPool {
    config: PoolConfig,
    sessions: DashMap<String, Arc<Session>>,
    router: Arc<SmartRouter>,
}

impl SessionPool {
    /// 创建新会话池
    pub fn new() -> Self {
        Self {
            config: PoolConfig::default(),
            sessions: DashMap::new(),
            router: Arc::new(SmartRouter::new()),
        }
    }

    /// 初始化会话池
    pub async fn initialize(&self) -> Result<()> {
        info!("Initializing session pool with {} max sessions", self.config.max_sessions);

        // 创建初始会话
        for i in 0..self.config.min_idle_sessions {
            self.create_session(&format!("session_{}", i)).await?;
        }

        Ok(())
    }

    /// 获取可用会话
    pub async fn acquire_session(&self) -> Result<Arc<Session>> {
        // 尝试从池中获取
        if let Some(session) = self.find_available_session()? {
            info!("Reusing existing session: {}", session.id());
            session.set_idle();
            return Ok(session);
        }

        // 创建新会话
        let id = format!("session_{}", uuid::Uuid::new_v4());
        self.create_session(&id).await
    }

    /// 释放会话
    pub async fn release_session(&self, session: Arc<Session>) {
        if session.state() == SessionState::ActiveUsing {
            session.set_idle();
            debug!("Released session: {}", session.id());
        }
    }

    /// 获取池状态
    pub fn status(&self) -> PoolStatus {
        let mut total = 0;
        let mut active = 0;
        let mut idle = 0;

        for session_arc in self.sessions.iter() {
            let state = session_arc.state();
            total += 1;

            match state {
                SessionState::Active => idle += 1,
                SessionState::ActiveUsing => {
                    active += 1;
                    idle += 1;
                }
                _ => {}
            }
        }

        PoolStatus {
            total_sessions: total,
            active_sessions: active,
            idle_sessions: idle,
        }
    }

    /// 创建单个会话
    async fn create_session(&self, id: &str) -> Result<Arc<Session>> {
        if self.sessions.len() >= self.config.max_sessions {
            return Err(anyhow::anyhow!("Session pool is at maximum capacity"));
        }

        let session = Arc::new(Session::new(
            id.to_string(),
            AICloudProvider::OpenAI,
            "gpt-3.5-turbo".to_string(),
            SessionConfig::default(),
            Arc::new(MockTransport),
        ).unwrap());

        self.sessions.insert(id.to_string(), session.clone());
        info!("Created session: {} (pool size: {})", id, self.sessions.len());

        Ok(session)
    }

    /// 查找可用会话
    fn find_available_session(&self) -> Result<Option<Arc<Session>>> {
        for session_arc in self.sessions.iter() {
            if session_arc.state().is_available() {
                // 检查是否过期
                if !session_arc.is_expired() {
                    return Ok(Some(session_arc.clone()));
                }
            }
        }
        Ok(None)
    }

    /// 战略性清理
    pub async fn cleanup(&self) {
        let mut sessions_to_remove = Vec::new();

        for session_arc in self.sessions.iter() {
            if session_arc.state() == SessionState::ActiveUsing {
                // 活动中的会话保留
                continue;
            }

            if session_arc.is_expired() {
                sessions_to_remove.push(session_arc.id().to_string());
            }
        }

        for id in &sessions_to_remove {
            self.sessions.remove(id);
            debug!("Removed expired session: {}", id);
        }

        let status = self.status();
        info!("Session pool cleanup complete. Current status: {:?}", status);
    }
}

/// 模拟传输层
struct MockTransport;

#[async_trait::async_trait]
impl TransportLayer for MockTransport {
    async fn send(&self, message: &[u8]) -> Result<Vec<u8>> {
        Ok(vec![b"OK"[0]])
    }

    async fn receive(&self, buffer: &mut [u8]) -> Result<usize> {
        Ok(0) // 演示版本，返回空
    }
}

/// 会话池测试
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_basic_pool_operations() {
        let pool = SessionPool::new();

        pool.initialize().await.unwrap();

        // 测试获取会话
        let session = pool.acquire_session().await.unwrap();
        assert_ne!(session.id(), String::new());

        // 测试释放会话
        pool.release_session(session).await;

        // 测试池状态
        let status = pool.status();
        assert!(status.total_sessions > 0);

        // 测试清理
        pool.cleanup().await;
        let status = pool.status();
        assert!(status.total_sessions >= status.idle_sessions);
    }
}