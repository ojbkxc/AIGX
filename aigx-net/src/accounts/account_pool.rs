//! 账号池管理
//!
//! 提供多账号的负载均衡和智能调度
//!
//! 特性：
//! - DashMap 无锁并发访问
//! - 多种负载均衡策略
//! - 账号状态实时跟踪
//! - 自动故障恢复
//! - 基于权重的调度

use super::account::{Account, AccountConfig, AccountErrorTracker};
use super::account_guard::{AccountGuard, GuardedStream};
use super::AccountState;
use std::time::{SystemTime, UNIX_EPOCH};
use dashmap::DashMap;
use std::sync::{Arc, RwLock};
use anyhow::{Context, Result};
use tracing::{debug, error, info, warn};

/// 负载均衡策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalanceStrategy {
    RoundRobin,      // 轮询
    Weighted,        // 权重均衡
    LatencyAware,    // 延迟感知
    LeastLoaded,     // 最空闲优先
    SeedRandom,      // 种子随机
}

/// 账号池配置
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// 负载均衡策略
    pub strategy: LoadBalanceStrategy,
    /// 最小空闲账号数（低于此数量触发扩容）
    pub min_capacity: usize,
    /// 最大账号数（超过此数量触发缩容）
    pub max_capacity: usize,
    /// 账号可用性检查间隔
    pub health_check_interval: u64,
    /// 账号最大错误次数（超过后重置状态）
    pub max_error_count: u16,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            strategy: LoadBalanceStrategy::Weighted,
            min_capacity: 2,
            max_capacity: 10,
            health_check_interval: 30000, // 30秒
            max_error_count: 5,
        }
    }
}

/// 账号池状态信息
#[derive(Debug, Clone, Serialize)]
pub struct PoolStatus {
    pub total_accounts: usize,
    pub available_accounts: usize,
    pub busy_accounts: usize,
    pub error_accounts: usize,
    pub invalid_accounts: usize,
    pub total_requests: u64,
    pub failed_requests: u64,
    pub pool_strategy: LoadBalanceStrategy,
}

/// 账号池
///
/// 管理多个账号的状态和调度
pub struct AccountPool {
    accounts: DashMap<String, Arc<Account>>,
    config: PoolConfig,
    pool_stats: Arc<RwLock<PoolStats>>,
    strategy: Arc<RwLock<LoadBalanceStrategy>>,
    health_check_task: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
}

/// 池统计信息
#[derive(Debug, Clone)]
struct PoolStats {
    total_requests: u64,
    failed_requests: u64,
}

impl AccountPool {
    /// 创建新的账号池
    pub fn new(config: PoolConfig) -> Self {
        Self {
            accounts: DashMap::new(),
            config,
            pool_stats: Arc::new(RwLock::new(PoolStats {
                total_requests: 0,
                failed_requests: 0,
            })),
            strategy: Arc::new(RwLock::new(config.strategy)),
            health_check_task: Arc::new(RwLock::new(None)),
        }
    }

    /// 从配置列表初始化账号池
    pub async fn from_configs(&self, configs: &[AccountConfig]) -> Result<()> {
        for config in configs {
            self.add_account(config).await?;
        }
        self.start_health_check().await;
        Ok(())
    }

    /// 添加账号到池中
    pub async fn add_account(&self, config: &AccountConfig) -> Result<()> {
        let account = Account::new(config.clone());

        // 检查是否已存在
        if self.accounts.get(&config.id).is_some() {
            return Ok(()); // 跳过已存在的账号
        }

        self.accounts.insert(config.id.clone(), Arc::new(account));
        info!("Added account to pool: {}", config.id);
        self.update_pool_status();

        Ok(())
    }

    /// 从池中移除账号
    pub fn remove_account(&self, id: &str) -> bool {
        let removed = self.accounts.remove(id).is_some();
        if removed {
            info!("Removed account from pool: {}", id);
            self.update_pool_status();
        }
        removed
    }

    /// 获取账号（基于策略）
    pub async fn get_account(&self) -> Result<Option<AccountGuard>> {
        *self.pool_stats.write().unwrap().total_requests += 1;

        let strategy = *self.strategy.read().unwrap();
        let account_opt = match strategy {
            LoadBalanceStrategy::RoundRobin => self.select_round_robin(),
            LoadBalanceStrategy::Weighted => self.select_weighted(),
            LoadBalanceStrategy::LatencyAware => self.select_latency_aware(),
            LoadBalanceStrategy::LeastLoaded => self.select_least_loaded(),
            LoadBalanceStrategy::SeedRandom => self.select_seed_random(),
        };

        if let Some(account) = account_opt {
            let guard = ServiceGuard::new(account).await?;
            Ok(Some(GuardedStream::new(
                guard.clone(),
                self.performance_metrics.clone(),
            )))
        } else {
            warn!("No available account in pool");
            *self.pool_stats.write().unwrap().failed_requests += 1;
            Ok(None)
        }
    }

    /// 最优账号获取（带超时）
    pub async fn get_account_with_timeout(&self, timeout_ms: u64) -> Result<Option<AccountGuard>> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_millis(timeout_ms);

        loop {
            match self.get_account().await {
                Ok(Some(guard)) => return Ok(Some(guard)),
                Ok(None) => {
                    // 无可用账号，等待轮询
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if start.elapsed() >= timeout {
                        return Ok(None);
                    }
                }
                Err(e) => {
                    error!("Failed to get account: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    if start.elapsed() >= timeout {
                        return Err(e);
                    }
                }
            }
        }
    }

    /// 标记账号出错
    pub fn mark_error(&self, id: &str) {
        if let Some(account_arc) = self.accounts.get(id) {
            account_arc.mark_error();
            self.update_pool_status();
        }
    }

    /// 重置账号状态
    pub fn reset_account(&self, id: &str) {
        if let Some(account_arc) = self.accounts.get(id) {
            account_arc.reset_error();
            account_arc.mark_idle();
            self.update_pool_status();
        }
    }

    /// 获取池状态
    pub fn status(&self) -> PoolStatus {
        let mut total = 0;
        let mut available = 0;
        let mut busy = 0;
        let mut error = 0;
        let mut invalid = 0;

        for account_arc in self.accounts.iter() {
            let state = account_arc.state();
            total += 1;
            match state {
                AccountState::Idle => available += 1,
                AccountState::Busy => busy += 1,
                AccountState::Error => error += 1,
                AccountState::Invalid => invalid += 1,
            }
        }

        let stats = *self.pool_stats.read().unwrap();

        PoolStatus {
            total_accounts: total,
            available_accounts: available,
            busy_accounts: busy,
            error_accounts: error,
            invalid_accounts: invalid,
            total_requests: stats.total_requests,
            failed_requests: stats.failed_requests,
            pool_strategy: *self.strategy.read().unwrap(),
        }
    }

    /// 账号健康检查
    pub async fn health_check(&self) -> Result<()> {
        for account_arc in self.accounts.iter() {
            let account_id = account_arc.id();
            let account = account_arc.value();
            let state = account.state();

            // 检查长时间错误状态
            if state == AccountState::Error {
                let error/time/spent = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                if !account.needs_relogin() {
                    // 重置状态
                    account.mark_idle();
                    debug!("Reset account state: {}", account_id);
                }
            }
        }

        self.update_pool_status();
        Ok(())
    }

    /// 启动健康检查任务
    pub async fn start_health_check(&self) {
        if let Some(handle) = *self.health_check_task.read().unwrap() {
            handle.abort();
        }

        let health_checker = self.health_check.clone();
        let handle = tokio::spawn(async move {
            let interval = std::time::Duration::from_millis(health_checker.config.health_check_interval);

            loop {
                tokio::time::sleep(interval).await;
                match health_checker.health_check().await {
                    Ok(()) => {}
                    Err(e) => error!("Health check failed: {}", e),
                }
            }
        });

        *self.health_check_task.write().unwrap() = Some(handle);
        info!("Started health check task");
    }

    /// 调整负载均衡策略
    pub async fn set_strategy(&self, new_strategy: LoadBalanceStrategy) {
        if *self.strategy.read().unwrap() == new_strategy {
            return;
        }

        *self.strategy.write().unwrap() = new_strategy;
        info!("Pool strategy changed to: {:?}", new_strategy);
        self.update_pool_status();
    }

    /// 私有：选择轮询策略
    fn select_round_robin(&self) -> Vec<Arc<Account>> {
        let mut available: Vec<_> = self.accounts
            .iter()
            .filter(|a| a.state().is_available())
            .map(|a| a.value().clone())
            .collect();

        if available.is_empty() {
            return Vec::new();
        }

        // 简化的轮询实现 - 实际应该使用递增计数器
        let index = (std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as usize) % available.len();

        vec![available[index].clone()]
    }

    /// 私有：选择权重策略
    fn select_weighted(&self) -> Vec<Arc<Account>> {
        let mut available: Vec<_> = self.accounts
            .iter()
            .filter(|a| a.state().is_available())
            .map(|a| {
                let account = a.value();
                let weight = account.get_weight();
                (account.clone(), weight)
            })
            .collect();

        if available.is_empty() {
            return Vec::new();
        }

        // 简化的权重实现 - 实际应该使用线性分布
        let total_weight: u8 = available.iter()
            .map(|(account, weight)| (account.get_weight() as usize) * (*weight as usize))
            .sum();

        if total_weight == 0 {
            return self.select_round_robin();
        }

        let random = (std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as usize) % total_weight;

        let mut accumulated = 0;
        for (account, weight) in &available {
            accumulated += (account.get_weight() as usize) * (*weight as usize);
            if random < accumulated {
                return vec![account.clone()];
            }
        }

        // Fallback
        vec![available[0].0.clone()]
    }

    /// 私有：选择延迟感知策略
    fn select_latency_aware(&self) -> Vec<Arc<Account>> {
        let mut available: Vec<_> = self.accounts
            .iter()
            .filter(|a| a.state().is_available())
            .map(|a| {
                let account = a.value();
                let latency = self.performance_metrics.query_latency(account.id()).unwrap_or(0);
                let weight = account.get_weight();
                (account.clone(), latency, weight)
            })
            .collect();

        available.sort_by(|a, b| a.1.cmp(&b.1)); // 按延迟排序

        if available.is_empty() {
            return Vec::new();
        }

        let top_latency = available[0].1;
        let best_accounts: Vec<_> = available
            .into_iter()
            .take(3) // 取最快的3个账号
            .map(|(account, _latency, weight)| account)
            .collect();

        best_accounts
    }

    /// 更新池状态统计
    fn update_pool_status(&self) {
        let _status = self.status();
    }

    /// 性能指标引用（这部分需要根据实现添加）
    performance_metrics: std::sync::Arc<dyn PerformanceMetrics + Send + Sync>,
}

impl Default for AccountPool {
    fn default() -> Self {
        Self::new(PoolConfig::default())
    }
}