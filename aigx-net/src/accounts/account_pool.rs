//! 账号池管理
//!
//! 提供多账号的负载均衡和智能调度
//!
//! 特性：
//! - DashMap 无锁并发访问
//! - 多种负载均衡策略
//! - 账号状态实时跟踪
//! - 自动故障恢复

use super::account::{Account, AccountConfig, AccountStatus, AccountType};
use anyhow::Result;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn};

/// 负载均衡策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalanceStrategy {
    /// 轮询
    RoundRobin,
    /// 权重均衡
    Weighted,
    /// 延迟感知
    LatencyAware,
    /// 最空闲优先（空闲最久优先）
    LeastLoaded,
    /// 随机
    Random,
}

/// 账号池配置
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// 负载均衡策略
    pub strategy: LoadBalanceStrategy,
    /// 最小空闲账号数
    pub min_capacity: usize,
    /// 最大账号数
    pub max_capacity: usize,
    /// 健康检查间隔（秒）
    pub health_check_interval: u64,
    /// 账号最大错误次数（超过后标记 Invalid）
    pub max_error_count: u16,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            strategy: LoadBalanceStrategy::Weighted,
            min_capacity: 2,
            max_capacity: 10,
            health_check_interval: 30,
            max_error_count: 5,
        }
    }
}

/// 账号池状态信息
#[derive(Debug, Clone)]
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

/// 单账号状态快照（供管理面板展示）
#[derive(Debug, Clone)]
pub struct AccountStatusSnapshot {
    pub id: String,
    pub state: super::account::AccountStatus,
    pub last_used_ms: i64,
    pub error_count: u8,
    pub consecutive_errors: u16,
    pub priority: u8,
    pub last_error_time_ms: Option<i64>,
}

/// 账号池
///
/// 管理多个账号的状态和调度
pub struct AccountPool {
    accounts: DashMap<String, Arc<Account>>,
    config: std::sync::RwLock<PoolConfig>,
    total_requests: AtomicU64,
    failed_requests: AtomicU64,
    rr_counter: AtomicU64,
}

impl AccountPool {
    /// 用指定配置创建账号池
    pub fn new(config: PoolConfig) -> Self {
        Self {
            accounts: DashMap::new(),
            config: std::sync::RwLock::new(config),
            total_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            rr_counter: AtomicU64::new(0),
        }
    }

    /// 从配置列表初始化账号池
    pub fn init(&self, configs: &[AccountConfig]) -> Result<()> {
        for config in configs {
            self.add_account(config)?;
        }
        Ok(())
    }

    /// 添加账号到池中（已存在则跳过）
    pub fn add_account(&self, config: &AccountConfig) -> Result<()> {
        if self.accounts.get(&config.id).is_some() {
            return Ok(());
        }

        let mut account = Account::new(&config.id, &config.password, AccountType::Direct);
        account.priority = config.priority;
        self.accounts.insert(config.id.clone(), Arc::new(account));
        info!("Added account to pool: {}", config.id);
        Ok(())
    }

    /// 从池中移除账号
    pub fn remove_account(&self, id: &str) -> bool {
        let removed = self.accounts.remove(id).is_some();
        if removed {
            info!("Removed account from pool: {id}");
        }
        removed
    }

    /// 按策略获取一个可用账号（只读借用语义：返回 Arc 快照）
    pub fn get_account(&self) -> Option<Arc<Account>> {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        let strategy = self.config.read().unwrap().strategy;
        let available: Vec<Arc<Account>> = self
            .accounts
            .iter()
            .filter(|e| e.value().is_available())
            .map(|e| e.value().clone())
            .collect();

        if available.is_empty() {
            warn!("No available account in pool");
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
            return None;
        }

        let picked = match strategy {
            LoadBalanceStrategy::RoundRobin => {
                let idx =
                    self.rr_counter.fetch_add(1, Ordering::Relaxed) as usize % available.len();
                available[idx].clone()
            }
            LoadBalanceStrategy::Weighted => {
                // 基于优先级的加权选择
                let total_weight: u64 = available.iter().map(|a| a.priority as u64 + 1).sum();
                let mut pick =
                    self.rr_counter.fetch_add(1, Ordering::Relaxed) as u64 % total_weight;
                let mut chosen = available[0].clone();
                for acc in &available {
                    pick = pick.saturating_sub(acc.priority as u64 + 1);
                    if pick == 0 {
                        chosen = acc.clone();
                        break;
                    }
                }
                chosen
            }
            LoadBalanceStrategy::LatencyAware => {
                // 延迟感知：成功率最高者优先（近似）
                available
                    .iter()
                    .max_by(|a, b| a.total_requests.cmp(&b.total_requests))
                    .cloned()
                    .unwrap_or_else(|| available[0].clone())
            }
            LoadBalanceStrategy::LeastLoaded => available[0].clone(),
            LoadBalanceStrategy::Random => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos() as usize;
                available[nanos % available.len()].clone()
            }
        };

        Some(picked)
    }

    /// 标记账号出错
    pub fn mark_error(&self, id: &str) {
        if self.accounts.contains_key(id) {
            self.accounts.alter(id, |_, mut acc| {
                acc.increase_failure_count();
                acc
            });
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 重置账号状态
    pub fn reset_account(&self, id: &str) {
        if self.accounts.contains_key(id) {
            self.accounts.alter(id, |_, mut acc| {
                acc.reset_failure();
                acc
            });
        }
    }

    /// 获取池状态
    pub fn status(&self) -> PoolStatus {
        let mut available = 0;
        let mut error = 0;
        let mut invalid = 0;
        let mut busy = 0;

        for entry in self.accounts.iter() {
            match entry.value().status {
                super::account::AccountStatus::Active => available += 1,
                super::account::AccountStatus::Error => error += 1,
                super::account::AccountStatus::Pending => invalid += 1,
                super::account::AccountStatus::Maintenance => busy += 1,
                super::account::AccountStatus::Inactive => invalid += 1,
            }
        }

        PoolStatus {
            total_accounts: self.accounts.len(),
            available_accounts: available,
            busy_accounts: busy,
            error_accounts: error,
            invalid_accounts: invalid,
            total_requests: self.total_requests.load(Ordering::Relaxed),
            failed_requests: self.failed_requests.load(Ordering::Relaxed),
            pool_strategy: self.config.read().unwrap().strategy,
        }
    }

    /// 池内账号快照列表
    pub fn list_status(&self) -> Vec<AccountStatusSnapshot> {
        self.accounts
            .iter()
            .map(|e| {
                let a = e.value();
                AccountStatusSnapshot {
                    id: a.id.clone(),
                    state: a.status.clone(),
                    last_used_ms: a.last_used_at.map(|t| t.timestamp_millis()).unwrap_or(0),
                    error_count: a.failed_requests.min(u8::MAX as u64) as u8,
                    consecutive_errors: a.failed_requests.min(u16::MAX as u64) as u16,
                    priority: a.priority,
                    last_error_time_ms: a.last_error_time.map(|t| t.timestamp_millis()),
                }
            })
            .collect()
    }

    /// 调整负载均衡策略
    pub fn set_strategy(&self, new_strategy: LoadBalanceStrategy) {
        let mut config = self.config.write().unwrap();
        config.strategy = new_strategy;
        info!("Pool strategy changed to: {new_strategy:?}");
    }
}

impl Default for AccountPool {
    fn default() -> Self {
        Self::new(PoolConfig::default())
    }
}
