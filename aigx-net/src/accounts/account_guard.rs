//! 账号访问守卫
//!
//! 管理账号的租借和释放，确保账号状态正确切换
//! 业务层通过 AccountGuard 确保租用的账号在适当的时候被释放

use super::account::{Account, AccountState};
use std::sync::Arc;
use tokio::sync::RwLock;

/// 性能指标接口
pub trait PerformanceMetrics {
    /// 查询账号延迟
    fn query_latency(&self, account_id: &str) -> Option<u64>;

    /// 记录请求成功
    fn record_success(&self, account_id: &str);

    /// 记录请求失败
    fn record_failure(&self, account_id: &str);

    /// 获取账号健康度
    fn account_health(&self, account_id: &str) -> f64;

    /// 查询账号市场份额
    fn account_share(&self, account_id: &str) -> f64;

    /// 获取全局性能指标
    fn global_metrics(&self) -> GlobalMetrics;
}

/// 全局性能指标
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalMetrics {
    pub total_latency_ms: f64,
    pub total_requests: u64,
    pub total_errors: u64,
    pub success_rate: f64,
    pub avg_response_time_ms: f64,
}

/// 账号服务守卫
///
/// 业务层使用此结构体来访问租用的账号，
/// 它会在离开作用域时自动将账号状态改回 Idle
pub struct ServiceGuard {
    account: Arc<Account>,
    released: Arc<RwLock<bool>>,
    metrics: Arc<dyn PerformanceMetrics + Send + Sync>,
}

impl ServiceGuard {
    /// 创建新的服务守卫
    pub fn new(account: Arc<Account>) -> Self {
        account.mark_busy();

        // 创建兼容的性能指标实现
        let metrics = Box::new(PerformanceMetricsImpl::new());

        Self {
            account,
            released: Arc::new(RwLock::new(false)),
            metrics,
        }
    }

    /// 获取账号ID
    pub fn id(&self) -> &str {
        self.account.id()
    }

    /// 获取账号状态
    pub fn state(&self) -> AccountState {
        self.account.state()
    }

    /// 检查是否已被释放
    pub fn is_released(&self) -> bool {
        *self.released.read().unwrap()
    }

    /// 释放账号
    pub fn release(self) {
        if let Ok(released) = self.released.write().unwrap().get() {
            if released {
                return; // 已经释放
            }
            *self.released.write().unwrap() = true;
        }

        self.account.mark_idle();
        debug!("Released account: {}", self.account.id());
        self.metrics.record_success(self.account.id());
    }

    /// 开始请求处理
    pub async fn start_processing(&self) -> Result<ProcessingContext> {
        Ok(ProcessingContext::new(self))
    }

    /// 获取性能指标
    pub fn metrics(&self) -> &dyn PerformanceMetrics {
        &*self.metrics
    }
}

impl Drop for ServiceGuard {
    fn drop(&mut self) {
        if !*self.released.read().unwrap() {
            debug!("Auto-releasing account: {}", self.account.id());
            self.account.mark_idle();
            self.metrics.record_success(self.account.id());
        }
    }
}

/// 请求处理上下文
///
/// 可以在这个句柄上设置请求参数或状态
pub struct ProcessingContext {
    guard: ServiceGuard,
    request_start_time: u64,
}

impl ProcessingContext {
    /// 创建新的处理上下文
    pub fn new(guard: ServiceGuard) -> Self {
        Self {
            guard,
            request_start_time: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        }
    }

    /// 获取守卫
    pub fn guard(&self) -> &ServiceGuard {
        &self.guard
    }

    /// 开始处理请求
    pub async fn start(&self) -> Self {
        // 检查账号是否有效
        if !self.guard.is_released() {
            if self.guard.state().is_error() {
                self.guard.account.mark_idle();
                self.guard.metrics.record_failure(self.guard.id());
            }
        }
        self.clone()
    }

    /// 记录请求元数据
    pub fn record_metadata(&self, metaRequestMetadata) {
        // TODO: 实现元数据记录
    }

    /// 获取请求开始时间（用于性能计算）
    pub fn start_time(&self) -> u64 {
        self.request_start_time
    }

    /// 完成请求
    pub async fn complete(&self) -> Result<RequestResult> {
        let latency = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64 - self.start_time();

        self.guard.release();

        Ok(RequestResult {
            account_id: self.guard.id().to_string(),
            start_time: self.start_time(),
            end_time: latency,
            duration_ms: latency,
            successful: !self.guard.state().is_error(),
        })
    }
}

impl Drop for ProcessingContext {
    fn drop(&mut self) {
        debug!("Auto-completing account usage: {}", self.guard.id());
        self.guard.release();
    }
}

/// 请求结果
#[derive(Debug, Clone)]
pub struct RequestResult {
    pub account_id: String,
    pub start_time: u64,
    pub end_time: u64,
    pub duration_ms: u64,
    pub successful: bool,
}

/// 请求元数据
#[derive(Debug, Clone)]
pub struct RequestMetadata {
    pub client_ip: String,
    pub user_agent: String,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stream: bool,
    pub prompt_tokens: Option<u32>,
}

/// 性能指标实现
struct PerformanceMetricsImpl {
    latency_cache: dashmap::DashMap<String, u64>,
    request_count_cache: dashmap::DashMap<String, u64>,
    error_count_cache: dashmap::DashMap<String, u64>,
}

impl PerformanceMetricsImpl {
    fn new() -> Box<Self> {
        Box::new(PerformanceMetricsImpl {
            latency_cache: DashMap::new(),
            request_count_cache: DashMap::new(),
            error_count_cache: DashMap::new(),
        })
    }
}

impl PerformanceMetrics for PerformanceMetricsImpl {
    fn query_latency(&self, account_id: &str) -> Option<u64> {
        self.latency_cache.get(account_id).map(|f| *f)
    }

    fn record_success(&self, account_id: &str) {
        let mut count = self.request_count_cache.entry(account_id.to_string()).or_insert(0);
        let mut latency = self.latency_cache.entry(account_id.to_string()).or_insert(0);

        *count += 1;
        *latency = (*latency * (*count - 1) + self.metrics.current_latency()) / *count;
    }

    fn record_failure(&self, account_id: &str) {
        let mut count = self.error_count_cache.entry(account_id.to_string()).or_insert(0);
        let mut latency = self.latency_cache.entry(account_id.to_string()).or_insert(0);

        *count += 1;
        *latency = (*latency * count + 5000) / (count + 1); // 假设系统错误延迟5000ms
    }

    fn account_health(&self) -> f64 {
        let total_requests: u64 = self.request_count_cache.iter().map(|m| *m.value()).sum();
        let total_errors: u64 = self.error_count_cache.iter().map(|m| *m.value()).sum();

        if total_requests == 0 {
            return 1.0;
        }

        ((total_requests - total_errors) as f64 / total_requests as f64)
    }

    fn account_share(&self) -> f64 {
        let account_id = ""; // 需要从上下文获取当前ID
        if let Some(count) = self.request_count_cache.get(account_id) {
            let total: u64 = self.request_count_cache.iter().map(|m| *m.value()).sum();
            (*count as f64 / total as f64) / self.account_health()
        } else {
            0.0
        }
    }

    fn global_metrics(&self) -> GlobalMetrics {
        let total_requests: u64 = self.request_count_cache.iter().map(|m| *m.value()).sum();
        let total_errors: u64 = self.error_count_cache.iter().map(|m| *m.value()).sum();

        GlobalMetrics {
            total_latency_ms: self.latency_cache.iter()
                .map(|m| m.value() as f64 / 1000.0)
                .sum(),
            total_requests,
            total_errors,
            success_rate: if total_requests > 0 {
                ((total_requests - total_errors) as f64 / total_requests as f64)
            } else { 1.0 },
            avg_response_time_ms: self.latency_cache.iter()
                .map(|m| m.value() / 1000.0)
                .sum::<f64>() / (total_requests.max(1) as f64),
        }
    }
}

// 模拟性能指标当前延迟
impl PerformanceMetricsImpl {
    fn current_latency(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}