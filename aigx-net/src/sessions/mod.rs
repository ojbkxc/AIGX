//! 会话管理模块
//!
//! 管理AI服务的会话生命周期和智能路由
//!
//! 特性：
//! - 会话池复用
//! - 智能路由策略
//! - 生命周期管理
//! - 会话健康度监控

pub mod session;
pub mod session_pool;
pub mod router;
pub use session::*;
pub use session_pool::*;
pub use router::*;

use std::time::{Duration, UNIX_EPOCH};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio::sync::RwLock as AsyncRwLock;
use dashmap::DashMap;
use anyhow::{Result, Context};
use tracing::{debug, error, info, warn};

/// 会话状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 创建中
    Creating,
    /// 已创建
    Active,
    /// 活动中
    ActiveUsing,
    /// 销毁中
    Destroying,
    /// 已销毁
    Destroyed,
}

/// AI服务提供商类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AICloudProvider {
    OpenAI,
    Anthropic,
    Google,
    DeepSeek,
    Custom(String),
}

/// 会话配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// 服务提供商
    pub provider: AICloudProvider,
    /// 模型ID
    pub model_id: String,
    /// 会话名称
    pub session_name: Option<String>,
    /// 最大消息数
    pub max_messages: usize,
    /// 会话超时时间（秒）
    pub session_ttl: Duration,
    /// 消息回溯间隔
    pub message_chunk_size: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            provider: AICloudProvider::OpenAI,
            model_id: "gpt-3.5-turbo".to_string(),
            session_name: None,
            max_messages: 50,
            session_ttl: Duration::from_secs(72 * 3600), // 72小时
            message_chunk_size: 10,
        }
    }
}

/// 会话信息
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub provider: AICloudProvider,
    pub model_id: String,
    pub state: SessionState,
    pub message_count: usize,
    pub last_message_id: i64,
    pub created_at: i64,
    pub last_used_at: i64,
    pub metaHashMap<String, String>,
}

/// 智能会话路由
pub struct SmartRouter {
    /// 日志记录器
    logger: Arc<dyn Logging + Send + Sync>,
    /// 路由策略
    strategy: RouterStrategy,
    /// 负载均衡器
    load_balancer: LoadBalancer,
}

/// 日志接口
pub trait Logging: Send + Sync {
    fn log(&self, message: String, level: LogLevel);
}

/// 日志级别
#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// 路由策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterStrategy {
    /// 基于延迟
    LatencyAware,
    /// 基于权重
    Weighted,
    /// 基于成功率
    SuccessRate,
    /// 随机选择
    Random,
    /// 最近最少使用
    LeastRecentlyUsed,
}

impl SmartRouter {
    /// 创建新的智能路由
    pub fn new() -> Self {
        Self {
            logger: Arc::new(DefaultLogger),
            strategy: RouterStrategy::LatencyAware,
            load_balancer: LoadBalancer::default(),
        }
    }

    /// 选择最佳会话
    pub async fn select_session(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        if sessions.is_empty() {
            return None;
        }

        match self.strategy {
            RouterStrategy::LatencyAware => {
                self.latency_aware_selection(sessions)
            }
            RouterStrategy::Weighted => {
                self.weighted_selection(sessions)
            }
            RouterStrategy::SuccessRate => {
                self.success_rate_selection(sessions)
            }
            RouterStrategy::Random => {
                self.random_selection(sessions)
            }
            RouterStrategy::LeastRecentlyUsed => {
                self.lru_selection(sessions)
            }
        }
    }

    /// 延迟感知选择
    fn latency_aware_selection(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        let mut sessions_by_latency: Vec<_> = sessions.iter()
            .filter_map(|s| s.current_latency().map(|latency| (latency, s)))
            .collect();

        if sessions_by_latency.is_empty() {
            return None;
        }

        sessions_by_latency.sort_by_key(|(latency, _)| *latency);
        sessions_by_latency[0].1.clone()
    }

    /// 权重选择
    fn weighted_selection(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        let total_weight: u64 = sessions.iter()
            .map(|s| s.current_weight() as u64)
            .sum();

        if total_weight == 0 {
            return None;
        }

        let random = rand::random::<u64>() % total_weight;
        let mut accumulated = 0;

        for session in sessions {
            accumulated += session.current_weight() as u64;
            if random < accumulated {
                return Some(Arc::clone(session));
            }
        }

        // Fallback: return random session
        sessions[rand::random::<usize>() % sessions.len()].clone()
    }

    /// 成功率选择
    fn success_rate_selection(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        sessions.iter()
            .max_by_key(|s| s.success_rate())
            .cloned()
    }

    /// 随机选择
    fn random_selection(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        if sessions.is_empty() {
            return None;
        }

        sessions[rand::random::<usize>() % sessions.len()].clone()
    }

    /// LRU选择
    fn lru_selection(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        sessions.iter()
            .min_by_key(|s| s.last_used().unwrap_or(i64::MAX))
            .cloned()
    }

    /// 设置路由策略
    pub fn set_strategy(&mut self, strategy: RouterStrategy) {
        self.strategy = strategy;
    }

    /// 获取当前策略
    pub fn strategy(&self) -> RouterStrategy {
        self.strategy
    }
}

/// 负载均衡器
pub struct LoadBalancer {
    /// 策略
    strategy: LoadBalanceStrategy,
    /// 负载均衡器名称
    name: String,
}

impl LoadBalancer {
    /// 创建负载均衡器
    pub fn new(strategy: LoadBalanceStrategy, name: impl Into<String>) -> Self {
        Self {
            strategy,
            name: name.into(),
        }
    }

    /// 计算最优会话
    pub fn balance(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                let idx = (self.name.len() as usize % sessions.len());
                Some(sessions[idx].clone())
            }
            LoadBalanceStrategy::LeastLoaded => {
                sessions.iter()
                    .min_by_key(|s| s.current_load())
                    .cloned()
            }
            LoadBalanceStrategy::LeastFailed => {
                sessions.iter()
                    .min_by(|a, b| a.error_count().cmp(&b.error_count()))
                    .cloned()
            }
        }
    }
}

/// 负载均衡策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalanceStrategy {
    /// 轮询
    RoundRobin,
    /// 最空闲
    LeastLoaded,
    /// 最少失败
    LeastFailed,
}

impl Default for LoadBalancer {
    fn default() -> Self {
        Self::new(LoadBalanceStrategy::LeastLoaded, "default")
    }
}

/// 默认日志实现
struct DefaultLogger;
impl Logging for DefaultLogger {
    fn log(&self, message: String, level: LogLevel) {
        match level {
            LogLevel::Debug => debug!("{}", message),
            LogLevel::Info => info!("{}", message),
            LogLevel::Warn => warn!("{}", message),
            LogLevel::Error => error!("{}", message),
        }
    }
}

/// 模拟状态转换表
pub struct SessionTransitionTable {
    /// 转换规则
    transitions: Vec<(String, String, bool)>,
}

impl SessionTransitionTable {
    pub fn new() -> Self {
        Self {
            transitions: vec![
                ("creating".to_string(), "active".to_string(), true),
                ("creating".to_string(), "destroyed".to_string(), true),
                ("active".to_string(), "active_using".to_string(), true),
                ("active_using".to_string(), "active".to_string(), true),
                ("active_using".to_string(), "destroying".to_string(), true),
                ("active".to_string(), "destroying".to_string(), true),
                ("destroying".to_string(), "destroyed".to_string(), true),
                ("active_using".to_string(), "destroyed".to_string(), true),
                ("active_using".to_string(), "active".to_string(), true), // 也会回到active
            ],
        }
    }

    pub fn can_transition(&self, from: &str, to: &str) -> bool {
        self.transitions.iter()
            .any(|(f, t, _)| f == from && t == to)
    }

    pub fn transitions(&self) -> Vec<(String, String)> {
        self.transitions.iter()
            .filter(|(_, _, valid)| *valid)
            .map(|(f, t, _)| (f.clone(), t.clone()))
            .collect()
    }
}