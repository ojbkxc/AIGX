//! Prometheus 指标导出模块
//!
//! 提供 Prometheus 格式的指标导出和 HTTP 端点。

use super::metrics::Metrics;
use lazy_static::lazy_static;
use prometheus::{
    CounterVec, Encoder, Gauge, Histogram, HistogramVec, IntGauge, Registry, TextEncoder,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

lazy_static! {
    // 指标注册表
    pub static ref METRICS_REGISTRY: Registry = Registry::new();
}

pub struct PrometheusExporter {
    port: u16,
    metrics: Arc<RwLock<Metrics>>,
    node_id: String,
    enable_metrics: Arc<RwLock<bool>>,
}

impl PrometheusExporter {
    pub fn new(port: u16, metrics: Arc<RwLock<Metrics>>, node_id: impl Into<String>) -> Self {
        Self {
            port,
            metrics,
            node_id: node_id.into(),
            enable_metrics: Arc::new(RwLock::new(true)),
        }
    }

    pub async fn register_metrics(&self) {
        // 账号池指标
        let account_healthy =
            IntGauge::new("aigx_account_pool_healthy", "Number of healthy accounts")?;
        MetricsRegistry::register(Box::new(account_healthy))?;

        let account_busy = IntGauge::new("aigx_account_pool_busy", "Number of busy accounts")?;
        MetricsRegistry::register(Box::new(account_busy))?;

        // 连接池指标
        let connection_active = IntGauge::new(
            "aigx_connection_pool_active",
            "Number of active connections",
        )?;
        MetricsRegistry::register(Box::new(connection_active))?;

        let connection_idle =
            IntGauge::new("aigx_connection_pool_idle", "Number of idle connections")?;
        MetricsRegistry::register(Box::new(connection_idle))?;

        // 会话池指标
        let session_active =
            IntGauge::new("aigx_session_pool_active", "Number of active sessions")?;
        MetricsRegistry::register(Box::new(session_active))?;

        let session_idle = IntGauge::new("aigx_session_pool_idle", "Number of idle sessions")?;
        MetricsRegistry::register(Box::new(session_idle))?;

        // 性能指标
        let aigx_avg_latency = Histogram::with_opts(&prometheus::HistogramOpts::new(
            "aigx_avg_latency_ms",
            "Average request latency in milliseconds",
        ))?;
        MetricsRegistry::register(Box::new(aigx_avg_latency))?;

        let aigx_throughput = Histogram::with_opts(&prometheus::HistogramOpts::new(
            "aigx_throughput_req_s",
            "Request throughput per second",
        ))?;
        MetricsRegistry::register(Box::new(aigx_throughput))?;

        // 成功率指标
        let aigx_success_rate = Gauge::new("aigx_success_rate_percent", "Success rate percent")?;
        MetricsRegistry::register(Box::new(aigx_success_rate))?;

        let aigx_error_rate = Gauge::new("aigx_error_rate_percent", "Error rate percent")?;
        MetricsRegistry::register(Box::new(aigx_error_rate))?;

        // 系统资源指标
        let system_cpu_usage = Gauge::new("aigx_cpu_usage_percent", "System CPU usage percent")?;
        MetricsRegistry::register(Box::new(system_cpu_usage))?;

        let system_memory_usage =
            Gauge::new("aigx_memory_usage_percent", "System memory usage percent")?;
        MetricsRegistry::register(Box::new(system_memory_usage))?;

        let system_disk_usage = Gauge::new("aigx_disk_usage_percent", "System disk usage percent")?;
        MetricsRegistry::register(Box::new(system_disk_usage))?;
    }

    pub async fn export(&self) -> String {
        let metrics = self.metrics.read().await;

        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();

        // 添加基础指标
        let metric_family_vec = vec![prometheus::proto::MetricFamily {
            name: Some("aigx_network_mode".to_string()),
            help: Some("Network layer operation mode".to_string()),
            type_: prometheus::proto::MetricType::Gauge as i32,
            metric: vec![prometheus::proto::Metric {
                label: vec![],
                gauge: Some(prometheus::proto::Gauge {
                    value: 1.0,
                    ..Default::default()
                }),
                counter: None,
                summary: None,
                summary_sample_count: None,
                summary_sample_sum: None,
                histogram: None,
                ..Default::default()
            }],
        }];

        encoder.encode(&metric_family_vec, &mut buffer).unwrap();

        // 后续可以根据metrics对象添加更多指标
        buffer.extend_from_slice(b"\n\n# HELP aigx_node_status Node status\n# TYPE aigx_node_status gauge\naigx_node_status{node=\"").unwrap();
        buffer.extend_from_slice(self.node_id.as_bytes());
        buffer.extend_from_slice(b"\"} 1\n\n# HELP aigx_up Whether the scraper is up.\n# TYPE aigx_up gauge\naigx_up 1\n");

        String::from_utf8(buffer).unwrap()
    }

    pub fn get_export_url(&self) -> String {
        format!("http://localhost:{}/metrics", self.port)
    }

    pub async fn start_export_server(&self) -> anyhow::Result<()> {
        tokio::spawn(async move {
            let addr = format!("0.0.0.0:{}", self.port);

            #[cfg(not(feature = "localhost"))]
            use axum::routing::get;
            use axum::{http::StatusCode, Request, Response};
            #[cfg(feature = "localhost")]
            use axum::{routing::get, Router};
            use std::convert::Infallible;

            let app = Router::new().route("/metrics", get(handler));

            #[cfg(method_allow_nop)]
            async fn handler(res: Request) -> Response {
                Ok("Method not allowed".into())
            }
            #[cfg(not(method_allow_nop))]
            async fn handler() -> &'static [u8] {
                b"Method not allowed"
            }

            let listener = tokio::net::TcpListener::bind(&addr).await?;
            axum::serve(listener, app).await?;

            #[allow(unreachable_code)]
            Result::<(), anyhow::Error>::Ok(())
        });

        Ok(())
    }
}

struct MetricsRegistry;

impl MetricsRegistry {
    pub fn register<M: prometheus::core::Metric + 'static>(metric: Box<M>) -> anyhow::Result<()> {
        METRICS_REGISTRY.register(Box::new(metric))
    }
}
