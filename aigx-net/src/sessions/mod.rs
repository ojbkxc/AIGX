//! 会话管理模块
//!
//! 管理 AI 服务的会话生命周期和智能路由
//!
//! 特性：
//! - 会话池复用
//! - 智能路由策略
//! - 生命周期管理
//! - 会话健康度监控

pub mod router;
pub mod session;
pub mod session_pool;
pub use router::*;
pub use session::*;
pub use session_pool::*;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// 会话状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 创建中
    Creating,
    /// 已创建（空闲可用）
    Active,
    /// 使用中
    ActiveUsing,
    /// 销毁中
    Destroying,
    /// 已销毁
    Destroyed,
}

impl SessionState {
    /// 状态是否可被调度
    pub fn is_available(self) -> bool {
        matches!(self, Self::Active | Self::ActiveUsing)
    }

    /// 机器可读的状态名
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Active => "active",
            Self::ActiveUsing => "active_using",
            Self::Destroying => "destroying",
            Self::Destroyed => "destroyed",
        }
    }
}

/// AI 服务提供商类型
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
    /// 模型 ID
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
            session_ttl: Duration::from_secs(72 * 3600), // 72 小时
            message_chunk_size: 10,
        }
    }
}

/// 会话信息快照
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
    pub metadata: HashMap<String, String>,
}

/// 路由策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterStrategy {
    /// 基于延迟
    LatencyAware,
    /// 基于最近使用（LRU）
    LeastRecentlyUsed,
    /// 随机选择
    Random,
}

/// 智能会话路由
pub struct SmartRouter {
    strategy: RouterStrategy,
}

impl SmartRouter {
    /// 创建新的智能路由（默认延迟感知）
    pub fn new() -> Self {
        Self {
            strategy: RouterStrategy::LatencyAware,
        }
    }

    /// 选择最佳会话（延迟感知 = 最近使用的连接复用度最高）
    pub fn select_session(
        &self,
        sessions: &[std::sync::Arc<Session>],
    ) -> Option<std::sync::Arc<Session>> {
        let available: Vec<_> = sessions
            .iter()
            .filter(|s| s.is_available())
            .cloned()
            .collect();

        if available.is_empty() {
            return None;
        }

        match self.strategy {
            RouterStrategy::LatencyAware => available
                .into_iter()
                .max_by_key(|s| s.last_used())
                .map(std::sync::Arc::clone),
            RouterStrategy::LeastRecentlyUsed => available
                .into_iter()
                .min_by_key(|s| s.last_used())
                .map(std::sync::Arc::clone),
            RouterStrategy::Random => available
                .into_iter()
                .nth(fastrand_usize(available.len()))
                .map(std::sync::Arc::clone),
        }
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

impl Default for SmartRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// 轻量随机（避免引入 rand 依赖：基于时间的伪随机下标）
fn fastrand_usize(bound: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    if bound == 0 {
        return 0;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as usize;
    nanos % bound
}

/// 会话状态转换表（供诊断/展示使用）
pub struct SessionTransitionTable {
    transitions: Vec<(String, String)>,
}

impl SessionTransitionTable {
    pub fn new() -> Self {
        Self {
            transitions: vec![
                ("creating", "active"),
                ("creating", "destroyed"),
                ("active", "active_using"),
                ("active_using", "active"),
                ("active", "destroying"),
                ("active_using", "destroying"),
                ("destroying", "destroyed"),
            ]
            .into_iter()
            .map(|(f, t)| (f.to_string(), t.to_string()))
            .collect(),
        }
    }

    pub fn can_transition(&self, from: &str, to: &str) -> bool {
        self.transitions.iter().any(|(f, t)| f == from && t == to)
    }

    pub fn transitions(&self) -> Vec<(String, String)> {
        self.transitions.clone()
    }
}
