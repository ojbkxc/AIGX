//! 会话池管理
//!
//! 管理多个会话的复用和智能调度

use super::router::SmartRouter;
use super::session::Session;
use super::{AICloudProvider, SessionConfig, SessionState};
use anyhow::Result;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// 会话池配置
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// 池大小上限
    pub max_sessions: usize,
    /// 最小空闲会话数
    pub min_idle_sessions: usize,
    /// 清理间隔（毫秒）
    pub cleanup_interval_ms: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_sessions: 50,
            min_idle_sessions: 5,
            cleanup_interval_ms: 300_000, // 5 分钟
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

    /// 用指定配置创建会话池
    pub fn with_config(config: PoolConfig) -> Self {
        Self {
            config,
            sessions: DashMap::new(),
            router: Arc::new(SmartRouter::new()),
        }
    }

    /// 初始化会话池（预热最小空闲会话）
    pub async fn initialize(&self) -> Result<()> {
        info!(
            "Initializing session pool with {} max sessions",
            self.config.max_sessions
        );

        for i in 0..self.config.min_idle_sessions {
            self.create_session(&format!("session_{i}"))?;
        }

        Ok(())
    }

    /// 获取可用会话（优先复用，池满时返回错误）
    pub async fn acquire_session(&self) -> Result<Arc<Session>> {
        if let Some(session) = self.find_available_session() {
            session.set_idle();
            return Ok(session);
        }

        let id = format!("session_{}", uuid::Uuid::new_v4());
        self.create_session(&id)
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

        for entry in self.sessions.iter() {
            let state = entry.value().state();
            total += 1;

            match state {
                SessionState::ActiveUsing => active += 1,
                SessionState::Active => idle += 1,
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
    fn create_session(&self, id: &str) -> Result<Arc<Session>> {
        if self.sessions.len() >= self.config.max_sessions {
            anyhow::bail!("Session pool is at maximum capacity");
        }

        let session = Arc::new(Session::new(
            id.to_string(),
            AICloudProvider::OpenAI,
            "gpt-3.5-turbo".to_string(),
            SessionConfig::default(),
        ));
        session.set_idle();

        self.sessions.insert(id.to_string(), session.clone());
        info!(
            "Created session: {} (pool size: {})",
            id,
            self.sessions.len()
        );

        Ok(session)
    }

    /// 查找可用会话
    fn find_available_session(&self) -> Option<Arc<Session>> {
        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.is_available() && !session.is_expired() {
                return Some(session.clone());
            }
        }
        None
    }

    /// 清理过期会话
    pub async fn cleanup(&self) {
        let mut to_remove = Vec::new();

        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.state() == SessionState::ActiveUsing {
                continue;
            }
            if session.is_expired() {
                to_remove.push(session.id().to_string());
            }
        }

        for id in &to_remove {
            self.sessions.remove(id);
            debug!("Removed expired session: {id}");
        }

        let status = self.status();
        info!("Session pool cleanup complete: {status:?}");
    }
}

impl Default for SessionPool {
    fn default() -> Self {
        Self::new()
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

        let session = pool.acquire_session().await.unwrap();
        assert!(!session.id().is_empty());

        pool.release_session(session).await;

        let status = pool.status();
        assert!(status.total_sessions > 0);

        pool.cleanup().await;
        let status = pool.status();
        assert!(status.total_sessions >= status.idle_sessions);
    }
}
