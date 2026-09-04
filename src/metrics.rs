//! Prometheus 指标暴露 — 自实现无依赖统计聚合 + /metrics 文本格式输出。
//!
//! 参照 aisix/new-api 的 prometheus 集成思路，但不引入额外依赖：
//! 用原子计数器聚合关键业务指标，`/metrics` 输出 Prometheus 文本协议
//! （OpenMetrics text/plain；version=0.0.4）。
//!
//! 指标清单：
//! - `aigx_requests_total{model,channel,status}` 请求计数（含流式/非流式）
//! - `aigx_tokens_total{model,type}` token 用量（prompt/completion）
//! - `aigx_latency_ms_sum{model}` 延迟毫秒累计（供 rate 计算）
//! - `aigx_latency_ms_count{model}` 延迟采样数
//! - `aigx_requests_inflight` 当前在途请求数
//! - `aigx_health_level{model}` 模型健康等级（0/1/2）
//! - `aigx_channels{status}` 渠道数量（enabled/disabled）
//! - `aigx_cost_usd_total` / `aigx_cost_cny_total` 累计请求成本（美元/人民币）
//!
//! 全部线程安全、无锁读取。`Metrics::record_request` 由请求处理路径在
//! 收尾处调用（见 main.rs 挂载的 tower middleware）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// 模型健康等级说明（与 health::HealthLevel 对齐，仅文档用途）。
#[allow(dead_code)]
const HEALTH_LEVELS: [&str; 3] = ["healthy", "degraded", "down"];

/// 全局指标注册表（进程级单例）。
static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();

pub fn global() -> &'static Metrics {
    METRICS.get_or_init(Metrics::new)
}

/// 请求计数键：(model, channel, status)。
/// 用 BTreeMap 保证输出顺序稳定，且无需并发锁读（写入在锁内）。
type ReqKey = (String, String, String);

/// 指标聚合器。
pub struct Metrics {
    requests: std::sync::Mutex<BTreeMap<ReqKey, AtomicU64>>,
    tokens: std::sync::Mutex<BTreeMap<(String, String), AtomicU64>>,
    latency_sum_ms: std::sync::Mutex<BTreeMap<String, AtomicU64>>,
    latency_count: std::sync::Mutex<BTreeMap<String, AtomicU64>>,
    health: std::sync::Mutex<BTreeMap<String, u8>>,
    inflight: AtomicI64,
    channels_enabled: AtomicU64,
    channels_disabled: AtomicU64,
    cost_usd: AtomicU64,
    cost_cny: AtomicU64,
}

impl Metrics {
    fn new() -> Self {
        Self {
            requests: std::sync::Mutex::new(BTreeMap::new()),
            tokens: std::sync::Mutex::new(BTreeMap::new()),
            latency_sum_ms: std::sync::Mutex::new(BTreeMap::new()),
            latency_count: std::sync::Mutex::new(BTreeMap::new()),
            health: std::sync::Mutex::new(BTreeMap::new()),
            inflight: AtomicI64::new(0),
            channels_enabled: AtomicU64::new(0),
            channels_disabled: AtomicU64::new(0),
            cost_usd: AtomicU64::new(0),
            cost_cny: AtomicU64::new(0),
        }
    }

    /// 记录一次请求。`status` 取 `"ok"` / `"error"`；`channel` 为空时用 `"unknown"`。
    pub fn record_request(&self, model: &str, channel: &str, status: &str, latency_ms: u64) {
        let model = model.to_string();
        let channel = if channel.is_empty() { "unknown".to_string() } else { channel.to_string() };
        let status = status.to_string();
        {
            let mut map = self.requests.lock().unwrap();
            map.entry((model.clone(), channel, status)).or_default().fetch_add(1, Ordering::Relaxed);
        }
        {
            let mut sum = self.latency_sum_ms.lock().unwrap();
            sum.entry(model.clone()).or_default().fetch_add(latency_ms, Ordering::Relaxed);
            let mut count = self.latency_count.lock().unwrap();
            count.entry(model).or_default().fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录一次请求开始（in-flight 计数 +1）。
    pub fn request_started(&self) {
        self.inflight.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录一次请求结束（in-flight 计数 -1）。
    pub fn request_finished(&self) {
        self.inflight.fetch_sub(1, Ordering::Relaxed);
    }

    /// 记录 token 用量。`ty` 取 `"prompt"` / `"completion"`。
    pub fn record_tokens(&self, model: &str, ty: &str, tokens: u64) {
        let mut map = self.tokens.lock().unwrap();
        map.entry((model.to_string(), ty.to_string())).or_default().fetch_add(tokens, Ordering::Relaxed);
    }

    /// 记录一次请求成本（微单位）。`currency` 取 `"usd"` / `"cny"`。
    /// 成本在微单位累加（1e-6 美元 / 1e-6 人民币），`render` 时按 1e-6 换算展示。
    pub fn record_cost(&self, currency: &str, cost_micro: u64) {
        if currency == "cny" {
            self.cost_cny.fetch_add(cost_micro, Ordering::Relaxed);
        } else {
            self.cost_usd.fetch_add(cost_micro, Ordering::Relaxed);
        }
    }

    /// 渠道数量快照（调度层在每次渠道增删后刷新）。
    pub fn set_channels(&self, enabled: u64, disabled: u64) {
        self.channels_enabled.store(enabled, Ordering::Relaxed);
        self.channels_disabled.store(disabled, Ordering::Relaxed);
    }

    /// 模型健康等级（0/1/2）快照，由健康追踪层刷新。
    pub fn set_health(&self, model: &str, level: u8) {
        self.health.lock().unwrap().insert(model.to_string(), level.min(2));
    }

    /// 渲染 Prometheus 文本格式。
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("# HELP aigx_requests_total Total number of AI requests.\n");
        out.push_str("# TYPE aigx_requests_total counter\n");
        {
            let map = self.requests.lock().unwrap();
            for ((model, channel, status), v) in map.iter() {
                out.push_str(&format!(
                    "aigx_requests_total{{model=\"{}\",channel=\"{}\",status=\"{}\"}} {}\n",
                    escape(model),
                    escape(channel),
                    escape(status),
                    v.load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str("# HELP aigx_tokens_total Total tokens used.\n");
        out.push_str("# TYPE aigx_tokens_total counter\n");
        {
            let map = self.tokens.lock().unwrap();
            for ((model, ty), v) in map.iter() {
                out.push_str(&format!(
                    "aigx_tokens_total{{model=\"{}\",type=\"{}\"}} {}\n",
                    escape(model),
                    escape(ty),
                    v.load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str("# HELP aigx_latency_ms Request latency in milliseconds.\n");
        out.push_str("# TYPE aigx_latency_ms summary\n");
        {
            let sum = self.latency_sum_ms.lock().unwrap();
            for (model, v) in sum.iter() {
                let count = self.latency_count.lock().unwrap().get(model).map(|c| c.load(Ordering::Relaxed)).unwrap_or(0);
                out.push_str(&format!(
                    "aigx_latency_ms_sum{{model=\"{}\"}} {}\n",
                    escape(model),
                    v.load(Ordering::Relaxed)
                ));
                out.push_str(&format!(
                    "aigx_latency_ms_count{{model=\"{}\"}} {}\n",
                    escape(model),
                    count
                ));
            }
        }
        out.push_str("# HELP aigx_health_level Model health level (0=healthy,1=degraded,2=down).\n");
        out.push_str("# TYPE aigx_health_level gauge\n");
        {
            let map = self.health.lock().unwrap();
            for (model, level) in map.iter() {
                out.push_str(&format!(
                    "aigx_health_level{{model=\"{}\"}} {}\n",
                    escape(model),
                    level
                ));
            }
        }
        out.push_str(&format!(
            "# TYPE aigx_requests_inflight gauge\naigx_requests_inflight {}\n",
            self.inflight.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "# TYPE aigx_channels_enabled gauge\naigx_channels_enabled {}\n",
            self.channels_enabled.load(Ordering::Relaxed)
        ));
        out.push_str(&format!(
            "# TYPE aigx_channels_disabled gauge\naigx_channels_disabled {}\n",
            self.channels_disabled.load(Ordering::Relaxed)
        ));
        let usd = self.cost_usd.load(Ordering::Relaxed) as f64 / 1e6;
        let cny = self.cost_cny.load(Ordering::Relaxed) as f64 / 1e6;
        out.push_str("# HELP aigx_cost_usd_total Accumulated request cost in USD.\n");
        out.push_str(&format!(
            "# TYPE aigx_cost_usd_total counter\naigx_cost_usd_total {}\n",
            usd
        ));
        out.push_str("# HELP aigx_cost_cny_total Accumulated request cost in CNY.\n");
        out.push_str(&format!(
            "# TYPE aigx_cost_cny_total counter\naigx_cost_cny_total {}\n",
            cny
        ));
        out
    }
}

/// 转义 label 值中的双引号与反斜杠，避免破坏 Prometheus 文本协议。
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_contains_expected_metrics() {
        let m = Metrics::new();
        m.record_request("gpt-4", "ch1", "ok", 120);
        m.record_request("gpt-4", "ch1", "ok", 80);
        m.record_request("claude-3", "ch2", "error", 500);
        m.record_tokens("gpt-4", "prompt", 1000);
        m.record_tokens("gpt-4", "completion", 500);
        m.request_started();
        m.request_finished();
        m.set_channels(3, 1);
        m.record_cost("usd", 500_000);

        let out = m.render();
        assert!(out.contains("aigx_requests_total{model=\"gpt-4\",channel=\"ch1\",status=\"ok\"} 2"));
        assert!(out.contains("aigx_requests_total{model=\"claude-3\",channel=\"ch2\",status=\"error\"} 1"));
        assert!(out.contains("aigx_tokens_total{model=\"gpt-4\",type=\"prompt\"} 1000"));
        assert!(out.contains("aigx_latency_ms_sum{model=\"gpt-4\"} 200"));
        assert!(out.contains("aigx_latency_ms_count{model=\"gpt-4\"} 2"));
        assert!(out.contains("aigx_requests_inflight 0"));
        assert!(out.contains("aigx_channels_enabled 3"));
        assert!(out.contains("aigx_channels_disabled 1"));
        assert!(out.contains("aigx_cost_usd_total 0.5"));
        assert!(out.contains("aigx_cost_cny_total 0"));
    }

    #[test]
    fn label_escaping() {
        let m = Metrics::new();
        m.record_request("model\"x", "ch\"1", "ok", 1);
        let out = m.render();
        assert!(out.contains("model=\"model\\\"x\""));
    }
}