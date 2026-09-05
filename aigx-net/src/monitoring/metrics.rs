//! 指标收集模块
//!
//! 提供系统性能指标、网络指标和应用指标收集。

use lazy_static::lazy_static;
use prometheus::{
    Counter, CounterVec, Encoder, Histogram, HistogramOpts, HistogramVec, Opts, Registry,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::debug;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Metrics {
    // 系统指标
    pub cpu_usage: f32,
    pub memory_usage: f32,
    pub disk_usage: f32,
    pub cpu_temp: Option<f32>,
    pub memory_temp: Option<f32>,

    // 网络指标
    pub network_tx: u64, // 上传字节
    pub network_rx: u64, // 下载字节
    pub active_connections: usize,
    pub total_requests: u64,
    pub failed_requests: u64,

    // 性能指标
    pub avg_latency: f64,
    pub throughput: f64,
    pub error_rate: f64,
    pub success_rate: f64,

    // 资源指标
    pub uptime: u64, // 运行时间（秒）
    pub memory: MemoryInfo,
    pub disk: DiskInfo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub free_bytes: u64,
    pub used_percent: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_bytes: u64,
    pub used_percent: f32,
}

#[derive(Clone)]
pub struct MetricsCollector {
    registry: Arc<Registry>,
    metrics: Arc<RwLock<Metrics>>,
    start_time: Instant,
    last_network_stats: Arc<RwLock<HashMap<String, u64>>>,
    node_id: String,
    metrics_enabled: Arc<RwLock<bool>>,
}

impl MetricsCollector {
    pub fn new(node_id: impl Into<String>, registry: Option<Arc<Registry>>) -> Self {
        let registry = registry.unwrap_or_else(|| {
            let mut r = Registry::new();
            Self::register_metrics(&r);
            Arc::new(r)
        });

        Self {
            registry,
            metrics: Arc::new(RwLock::new(Metrics::default())),
            start_time: Instant::now(),
            last_network_stats: Arc::new(RwLock::new(HashMap::new())),
            node_id: node_id.into(),
            metrics_enabled: Arc::new(RwLock::new(true)),
        }
    }

    fn register_metrics(registry: &Registry) {
        // 计数器
        let request_total = CounterVec::new(
            Opts::new("aigx_request_total", "Total number of requests"),
            &["method", "status"],
        )
        .unwrap();
        _ = registry.register(Box::new(request_total));

        let failed_requests = CounterVec::new(
            Opts::new(
                "aigx_failed_requests_total",
                "Total number of failed requests",
            ),
            &["error_type"],
        )
        .unwrap();
        _ = registry.register(Box::new(failed_requests));

        // 直方图
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "aigx_request_duration_seconds",
                "Request duration in seconds",
            ),
            &["method", "endpoint"],
        )
        .unwrap();
        _ = registry.register(Box::new(request_duration));

        // 指标计数器
        request_total.with_label_values(&["GET", "200"]).inc();
    }

    pub async fn update_system_metrics(&self) {
        let mut metrics = self.metrics.write().await;

        // 系统指标模拟
        metrics.cpu_usage = Self::generate_cpu_usage();
        metrics.memory_usage = Self::generate_memory_usage();
        metrics.disk_usage = Self::generate_disk_usage();

        // 更新指标
        self.update_prometheus_metrics().await;

        // 计算网络吞吐量
        self.update_throughput().await;
    }

    async fn update_throughput(&self) {
        let mut last_stats = self.last_network_stats.write().await;
        let current_stats = Self::generate_network_stats();

        let tx_diff = current_stats
            .tx
            .saturating_sub(last_stats.get("tx").copied().unwrap_or(0));
        let rx_diff = current_stats
            .rx
            .saturating_sub(last_stats.get("rx").copied().unwrap_or(0));

        let total_bytes = tx_diff + rx_diff;
        if total_bytes > 0 {
            let duration = 10; // 假设每10秒更新一次
            let throughput = total_bytes as f64 / duration as f64;
            metrics.throughput = throughput;
        }

        *last_stats = current_stats;
    }

    async fn update_prometheus_metrics(&self) {
        let metrics = self.metrics.read().await;

        // 更新各个prometheus指标
        debug!("Prometheus metrics updated");
    }

    pub fn get_metrics(&self) -> Metrics {
        let metrics = self.metrics.read().await;
        *metrics
    }

    pub fn record_request(&self, method: &str, status: u16) {
        let counter = self
            .registry
            .with_label_values(&["GET", "200"])
            .expect("Failed to get counter");

        counter.inc();
    }

    pub fn record_failure(&self, error_type: &str) {
        let counter = self
            .registry
            .with_label_values(&[error_type])
            .expect("Failed to get failure counter");

        counter.inc();
    }

    pub fn record_request_duration(&self, method: &str, endpoint: &str, duration_secs: f64) {
        let histogram = self
            .registry
            .with_label_values(&[method, endpoint])
            .expect("Failed to get histogram");

        histogram.observe(duration_secs);
    }

    pub fn set_metrics_enabled(&self, enabled: bool) {
        let mut enabled_mutex = self.metrics_enabled.write().await;
        *enabled_mutex = enabled;
    }

    pub fn get_metrics_registry(&self) -> Arc<Registry> {
        self.registry.clone()
    }

    pub fn get_node_id(&self) -> &str {
        &self.node_id
    }

    // 模拟指标生成函数
    fn generate_cpu_usage() -> f32 {
        // 模拟CPU使用率: 20-60%
        (20.0 + rand::random::<f32>() * 40.0).round() / 1.0
    }

    fn generate_memory_usage() -> f32 {
        // 模拟内存使用率: 40-80%
        (40.0 + rand::random::<f32>() * 40.0).round() / 1.0
    }

    fn generate_disk_usage() -> f32 {
        // 模拟磁盘使用率: 50-90%
        (50.0 + rand::random::<f32>() * 40.0).round() / 1.0
    }

    fn generate_network_stats() -> (u64, u64) {
        // 模拟网络统计
        let tx = (1000 + rand::random::<u32>() * 9000) as u64;
        let rx = (800 + rand::random::<u32>() * 82000) as u64;

        (tx, rx)
    }
}

impl GPU {
    fn default() -> Self {
        Self {
            gpu_usage: HashMap::new(),
            memory_usage: HashMap::new(),
        }
    }
}

impl MFU {
    pub fn collect(self) -> Self {
        #[allow(unused_local_variables)]
        let _node = nodes.lock().await;
        Self::default()
    }
}

use super::促进::debug;
