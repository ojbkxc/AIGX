//! 智能会话路由
//!
//! 实现多种路由策略，选择最佳会话

use super::Session;
use super::SessionConfig;
use super::SessionState;

/// 智能路由策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// 基于延迟 - 选择延迟最低的会话
    LatencyAware,

    /// 基于权重 - 选择权重最高的会话
    Weighted,

    /// 基于成功率 - 选择成功率最高的会话
    SuccessRate,

    /// 随机选择 - 从可用会话中随机选择
    Random,

    /// 最近最少使用 - 选择最近最少使用的会话
    LeastRecentlyUsed,

    /// 最空闲 - 选择内存占用最低的会话
    LeastLoaded,
}

impl Strategy {
    /// 基于策略选择会话
    pub fn select_session(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        let available: Vec<_> = sessions.iter()
            .filter(|s| s.state().is_available())
            .cloned()
            .collect();

        if available.is_empty() {
            return None;
        }

        match self {
            Strategy::LatencyAware => {
                available.into_iter()
                    .min_by_key(|s| s.current_latency().unwrap_or(u64::MAX))
                    .map(Arc::clone)
            }
            Strategy::Weighted => {
                available.into_iter()
                    .max_by_key(|s| s.current_weight())
                    .map(Arc::clone)
            }
            Strategy::SuccessRate => {
                available.into_iter()
                    .max_by_key(|s| s.success_rate())
                    .map(Arc::clone)
            }
            Strategy::Random => {
                available.into_iter()
                    .nth(rand::random::<usize>() % available.len())
                    .map(Arc::clone)
            }
            Strategy::LeastRecentlyUsed => {
                available.into_iter()
                    .min_by_key(|s| s.last_used().unwrap_or(i64::MAX))
                    .map(Arc::clone)
            }
            Strategy::LeastLoaded => {
                available.into_iter()
                    .min_by_key(|s| s.current_load())
                    .map(Arc::clone)
            }
        }
    }

    /// 设置路由策略
    pub fn set_strategy(&mut self, new_strategy: Strategy) {
        *self = new_strategy;
    }
}

/// 智能路由器
pub struct SmartRouter {
    /// 当前策略
    strategy: Strategy,

    /// 负载均衡器
    load_balancer: LoadBalancer,

    /// 记录器
    logger: Arc<dyn ForagingLogger>,
}

/// 调度日志
pub trait RoutingLogger: Send + Sync {
    /// 记录路由决策
    fn log_routing_decision(&self, strategy: &Strategy, chosen: &str, reason: &str);
}

impl SmartRouter {
    /// 创建新的智能路由器
    pub fn new() -> Self {
        Self {
            strategy: Strategy::LatencyAware,
            load_factorStrategy,
        }

        /// 选择策略名称
        pub fn strategy_name(&self) -> &str {
            match self.strategy {
                Strategy::LatencyAware => "latency-aware",
                Strategy::Weighted => "weighted",
                Strategy::SuccessRate => "success-rate",
                Strategy::Random => "random",
                Strategy::LeastRecentlyUsed => "lru",
                Strategy::LeastLoaded => "least-loaded",
            }
        }
    }

    /// 设置路由策略
    pub fn set_strategy(&mut self, strategy: Strategy) {
        self.strategy = strategy;
        self.logger.routing_decision(&self.strategy, "unknown", "strategy-changed");
    }

    /// 选择最佳会话
    pub fn select_session(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        // 首先尝试智能路由
        let chosen = self.strategy.select_session(sessions);

        // 如果策略路由失败，回退到负载均衡
        let session = chosen.or_else(|| self.load_balancer.balance(sessions));

        // 记录日志
        if let Some(s) = &session {
            self.logger.routing_decision(&self.strategy, &s.id(), "strategy-selected");
        }

        session
    }

    /// 获取当前策略
    pub fn current_strategy(&self) -> &Strategy {
        &self.strategy
    }
}

/// 负载均衡器
pub struct LoadBalancer {
    /// 当前策略
    strategy: LoadBalanceStrategy,
}

/// 负载均衡策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalanceStrategy {
    /// 轮询
    RoundRobin,

    /// 最空闲
    LeastLoaded,

    /// 随机
    Random,
}

impl LoadBalancer {
    /// 计算最优会话
    pub fn balance(&self, sessions: &[Arc<Session>]) -> Option<Arc<Session>> {
        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                let index = sessions.len() % sessions.len();
                Some(sessions[index].clone())
            }
            LoadBalanceStrategy::LeastLoaded => {
                sessions.iter()
                    .min_by_key(|s| s.current_load())
                    .cloned()
            }
            LoadBalanceStrategy::Random => {
                sessions.into_iter()
                    .nth(rand::random::<usize>() % sessions.len())
                    .map(Arc::clone)
            }
        }
    }

    /// 设置策略
    pub fn set_strategy(&mut self, strategy: LoadBalanceStrategy) {
        self.strategy = strategy;
    }
}