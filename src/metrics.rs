//! Prometheus 指标暴露 — 自实现无依赖统计聚合 + /metrics 文本格式输出。
//!
//! 参照 aisix/new-api 的 prometheus 集成思路，但不引入额外依赖：
//! 用原子计数器聚合关键业务指标，`/metrics` 输出 Prometheus 文本协议
//! （OpenMetrics text/plain；version=0.0.4）。
//!
//! 指标清单：
//! - `aigx_requests_total{model,channel,status}` 请求计数（含流式/非流式）
//! - `aigx_request_errors_total{model,channel,error_type}` 错误类型计数
//!   （timeout/upstream_error/rate_limit_error…，区分 429/5xx/超时）
//! - `aigx_tokens_total{model,type}` token 用量（prompt/completion）
//! - `aigx_latency_ms_sum{model}` 延迟毫秒累计（供 rate 计算）
//! - `aigx_latency_ms_count{model}` 延迟采样数
//! - `aigx_request_latency_bucket{model,le}` 延迟直方图分桶（p95 数据源）
//! - `aigx_channel_requests_total{channel,result}` 渠道级结果计数
//!   （成功率 = ok/(ok+error)）
//! - `aigx_channel_latency_bucket{channel,le}` 渠道级延迟直方图（渠道 p95）
//! - `aigx_ttft_ms_bucket{model,le}` 首字节延迟直方图（TTFT）
//! - `aigx_requests_inflight` 当前在途请求数（队列深度代理指标）
//! - `aigx_health_level{model}` 模型健康等级（0/1/2）
//! - `aigx_channels{status}` 渠道数量（enabled/disabled）
//! - `aigx_channels_cooldown` 冷却中渠道数
//! - `aigx_cost_usd_total` / `aigx_cost_cny_total` 累计请求成本（美元/人民币）
//!
//! 全部线程安全、无锁读取。`Metrics::record_request` 由请求处理路径在
//! 收尾处调用（见 main.rs 挂载的 tower middleware）。
//!
//! P1 性能：延迟三件套（sum/count/hist）合并为一把锁（record_request 从
//! 4 把全局锁降到 2 把）；所有标签组合 map 加序列上限防无界增长（超限的
//! 新组合跳过并告警一次，存量序列不受影响）。

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// 模型健康等级说明（与 health::HealthLevel 对齐，仅文档用途）。
#[allow(dead_code)]
const HEALTH_LEVELS: [&str; 3] = ["healthy", "degraded", "down"];

/// 延迟直方图分桶边界（毫秒）。参照 Prometheus 客户端常用桶：
/// 覆盖 10ms ~ 60s 的推理延迟谱系，便于 Grafana p95/p99 计算。
const LATENCY_BUCKETS_MS: [u64; 12] = [
    10, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000, 20_000, 40_000, 60_000,
];

/// 单指标序列上限（防标签组合无界增长：模型×渠道×状态理论上可随渠道
/// 增删无限膨胀）。超限的**新**组合跳过记录（存量序列不受影响），
/// 损失的是个别新组合的计数精度，换取内存上界。
const MAX_LABEL_SERIES: usize = 4096;

/// 全局指标注册表（进程级单例）。
static METRICS: std::sync::OnceLock<Metrics> = std::sync::OnceLock::new();

pub fn global() -> &'static Metrics {
    METRICS.get_or_init(Metrics::new)
}

/// 请求计数键：(model, channel, status)。
/// 用 BTreeMap 保证输出顺序稳定，且无需并发锁读（写入在锁内）。
type ReqKey = (String, String, String);

/// 延迟聚合条目：sum/count/直方图分桶合并为一把锁的存储单元。
struct LatencyEntry {
    sum: AtomicU64,
    count: AtomicU64,
    buckets: Vec<AtomicU64>,
}

impl LatencyEntry {
    fn new() -> Self {
        Self {
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
            buckets: LATENCY_BUCKETS_MS
                .iter()
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }
}

/// 渠道级延迟聚合（成功率 + p95 数据源）。
struct ChannelLatencyEntry {
    success: AtomicU64,
    count: AtomicU64,
    sum: AtomicU64,
    buckets: Vec<AtomicU64>,
}

impl ChannelLatencyEntry {
    fn new() -> Self {
        Self {
            success: AtomicU64::new(0),
            count: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            buckets: LATENCY_BUCKETS_MS
                .iter()
                .map(|_| AtomicU64::new(0))
                .collect(),
        }
    }
}

/// 指标聚合器。
pub struct Metrics {
    requests: parking_lot::Mutex<BTreeMap<ReqKey, AtomicU64>>,
    tokens: parking_lot::Mutex<BTreeMap<(String, String), AtomicU64>>,
    /// 延迟聚合（sum/count/直方图分桶合并为一把锁）：record_request 原本
    /// 每请求 4 把全局锁（requests/sum/count/hist），合并后 2 把。
    latency: parking_lot::Mutex<BTreeMap<String, LatencyEntry>>,
    /// 渠道级结果（成功率/p95 数据源）：成功样本计延迟分桶；失败样本只计
    /// 次数（record_channel_failure 无延迟参数，避免改签名波及其他调用方；
    /// 成功率指标不受影响）。
    channel_results: parking_lot::Mutex<BTreeMap<String, ChannelLatencyEntry>>,
    /// 错误类型计数（error_type 标签：timeout/upstream_error/rate_limit_error…）
    errors: parking_lot::Mutex<BTreeMap<(String, String, String), AtomicU64>>,
    /// TTFT 分桶（model → 分桶聚合）
    ttft: parking_lot::Mutex<BTreeMap<String, LatencyEntry>>,
    health: parking_lot::Mutex<BTreeMap<String, u8>>,
    inflight: AtomicI64,
    channels_enabled: AtomicU64,
    channels_disabled: AtomicU64,
    /// 冷却中渠道数（push 式快照，由 channel/mod.rs 的冷却事件刷新）
    channels_cooldown: AtomicU64,
    cost_usd: AtomicU64,
    cost_cny: AtomicU64,
    /// 标签组合超限告警（每进程一次，防刷屏）
    series_overflow_warned: AtomicBool,
}

impl Metrics {
    fn new() -> Self {
        Self {
            requests: parking_lot::Mutex::new(BTreeMap::new()),
            tokens: parking_lot::Mutex::new(BTreeMap::new()),
            latency: parking_lot::Mutex::new(BTreeMap::new()),
            channel_results: parking_lot::Mutex::new(BTreeMap::new()),
            errors: parking_lot::Mutex::new(BTreeMap::new()),
            ttft: parking_lot::Mutex::new(BTreeMap::new()),
            health: parking_lot::Mutex::new(BTreeMap::new()),
            inflight: AtomicI64::new(0),
            channels_enabled: AtomicU64::new(0),
            channels_disabled: AtomicU64::new(0),
            channels_cooldown: AtomicU64::new(0),
            cost_usd: AtomicU64::new(0),
            cost_cny: AtomicU64::new(0),
            series_overflow_warned: AtomicBool::new(false),
        }
    }

    /// 标签组合超限告警（每进程一次）。
    fn warn_series_overflow(&self) {
        if !self.series_overflow_warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                "metrics label series overflow: new combinations beyond MAX_LABEL_SERIES are skipped"
            );
        }
    }

    /// 记录一次请求。`status` 取 `"ok"` / `"error"`；`channel` 为空时用 `"unknown"`。
    pub fn record_request(&self, model: &str, channel: &str, status: &str, latency_ms: u64) {
        let model = model.to_string();
        let channel = if channel.is_empty() {
            "unknown".to_string()
        } else {
            channel.to_string()
        };
        let status = status.to_string();
        let key = (model.clone(), channel, status);
        {
            let mut map = self.requests.lock();
            if map.len() < MAX_LABEL_SERIES || map.contains_key(&key) {
                map.entry(key).or_default().fetch_add(1, Ordering::Relaxed);
            } else {
                self.warn_series_overflow();
            }
        }
        {
            let mut map = self.latency.lock();
            let entry = map.entry(model).or_insert_with(LatencyEntry::new);
            entry.sum.fetch_add(latency_ms, Ordering::Relaxed);
            entry.count.fetch_add(1, Ordering::Relaxed);
            for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
                if latency_ms <= *bound {
                    entry.buckets[i].fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// 记录错误类型计数（error_type 标签）。由请求处理路径的失败分支调用，
    /// `error_type` 取 `BridgeError::error_type()`（timeout/upstream_error/
    /// rate_limit_error/transport_error…）。
    pub fn record_request_error(&self, model: &str, channel: &str, error_type: &str) {
        let mut map = self.errors.lock();
        if map.len() < MAX_LABEL_SERIES {
            map.entry((
                model.to_string(),
                channel.to_string(),
                error_type.to_string(),
            ))
            .or_default()
            .fetch_add(1, Ordering::Relaxed);
        } else {
            self.warn_series_overflow();
        }
    }

    /// 记录渠道级结果（成功率/p95 数据源）：`success=true` 计 ok 并计延迟
    /// 分桶，`false` 只计次数（不计延迟——失败路径无延迟参数）。
    /// 由 channel/mod.rs 的渠道成功/失败记录点调用。
    pub fn record_channel_result(&self, channel: &str, success: bool, latency_ms: u64) {
        let mut map = self.channel_results.lock();
        if map.len() >= MAX_LABEL_SERIES && !map.contains_key(channel) {
            self.warn_series_overflow();
            return;
        }
        let entry = map
            .entry(channel.to_string())
            .or_insert_with(ChannelLatencyEntry::new);
        entry.count.fetch_add(1, Ordering::Relaxed);
        if success {
            entry.success.fetch_add(1, Ordering::Relaxed);
            entry.sum.fetch_add(latency_ms, Ordering::Relaxed);
            for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
                if latency_ms <= *bound {
                    entry.buckets[i].fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// 记录 TTFT（首字节延迟毫秒）。
    pub fn record_ttft(&self, model: &str, ttft_ms: u64) {
        let mut map = self.ttft.lock();
        if map.len() >= MAX_LABEL_SERIES && !map.contains_key(model) {
            self.warn_series_overflow();
            return;
        }
        let entry = map
            .entry(model.to_string())
            .or_insert_with(LatencyEntry::new);
        entry.sum.fetch_add(ttft_ms, Ordering::Relaxed);
        entry.count.fetch_add(1, Ordering::Relaxed);
        for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
            if ttft_ms <= *bound {
                entry.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// 冷却中渠道数快照（push 式：由 channel/mod.rs 的冷却事件刷新）。
    pub fn set_channels_cooldown(&self, n: u64) {
        self.channels_cooldown.store(n, Ordering::Relaxed);
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
        let key = (model.to_string(), ty.to_string());
        let mut map = self.tokens.lock();
        if map.len() < MAX_LABEL_SERIES || map.contains_key(&key) {
            map.entry(key)
                .or_default()
                .fetch_add(tokens, Ordering::Relaxed);
        } else {
            self.warn_series_overflow();
        }
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
        self.health.lock().insert(model.to_string(), level.min(2));
    }

    /// 用完整快照替换健康等级表（后台定时刷新用）。
    ///
    /// 与逐条 `set_health` 不同：已从 HealthTracker 消失的模型会被移除，
    /// 避免运行期长期暴露过期模型的 `aigx_health_level` 序列。
    pub fn replace_health<I>(&self, levels: I)
    where
        I: IntoIterator<Item = (String, u8)>,
    {
        let mut map = self.health.lock();
        map.clear();
        for (model, level) in levels {
            map.insert(model, level.min(2));
        }
    }

    /// 渲染 Prometheus 文本格式。
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);
        out.push_str("# HELP aigx_requests_total Total number of AI requests.\n");
        out.push_str("# TYPE aigx_requests_total counter\n");
        {
            let map = self.requests.lock();
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
        out.push_str("# HELP aigx_request_errors_total Requests by error type.\n");
        out.push_str("# TYPE aigx_request_errors_total counter\n");
        {
            let map = self.errors.lock();
            for ((model, channel, error_type), v) in map.iter() {
                out.push_str(&format!(
                    "aigx_request_errors_total{{model=\"{}\",channel=\"{}\",error_type=\"{}\"}} {}\n",
                    escape(model),
                    escape(channel),
                    escape(error_type),
                    v.load(Ordering::Relaxed)
                ));
            }
        }
        out.push_str("# HELP aigx_tokens_total Total tokens used.\n");
        out.push_str("# TYPE aigx_tokens_total counter\n");
        {
            let map = self.tokens.lock();
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
        out.push_str("# HELP aigx_request_latency_bucket Cumulative latency buckets.\n");
        out.push_str("# TYPE aigx_request_latency histogram\n");
        {
            let latency = self.latency.lock();
            for (model, entry) in latency.iter() {
                let sum = entry.sum.load(Ordering::Relaxed);
                let count = entry.count.load(Ordering::Relaxed);
                out.push_str(&format!(
                    "aigx_latency_ms_sum{{model=\"{}\"}} {}\n",
                    escape(model),
                    sum
                ));
                out.push_str(&format!(
                    "aigx_latency_ms_count{{model=\"{}\"}} {}\n",
                    escape(model),
                    count
                ));
                for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
                    out.push_str(&format!(
                        "aigx_request_latency_bucket{{model=\"{}\",le=\"{}\"}} {}\n",
                        escape(model),
                        bound,
                        entry.buckets[i].load(Ordering::Relaxed)
                    ));
                }
                out.push_str(&format!(
                    "aigx_request_latency_bucket{{model=\"{}\",le=\"+Inf\"}} {}\n",
                    escape(model),
                    count
                ));
                out.push_str(&format!(
                    "aigx_request_latency_sum{{model=\"{}\"}} {}\n",
                    escape(model),
                    sum
                ));
                out.push_str(&format!(
                    "aigx_request_latency_count{{model=\"{}\"}} {}\n",
                    escape(model),
                    count
                ));
            }
        }
        out.push_str(
            "# HELP aigx_channel_requests_total Channel-level request results (success rate derivable).\n",
        );
        out.push_str("# TYPE aigx_channel_requests_total counter\n");
        out.push_str("# HELP aigx_channel_latency Channel latency histogram (p95 via buckets).\n");
        out.push_str("# TYPE aigx_channel_latency histogram\n");
        {
            let map = self.channel_results.lock();
            for (channel, e) in map.iter() {
                let ok = e.success.load(Ordering::Relaxed);
                let total = e.count.load(Ordering::Relaxed);
                out.push_str(&format!(
                    "aigx_channel_requests_total{{channel=\"{}\",result=\"ok\"}} {}\n",
                    escape(channel),
                    ok
                ));
                out.push_str(&format!(
                    "aigx_channel_requests_total{{channel=\"{}\",result=\"error\"}} {}\n",
                    escape(channel),
                    total.saturating_sub(ok)
                ));
                for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
                    out.push_str(&format!(
                        "aigx_channel_latency_bucket{{channel=\"{}\",le=\"{}\"}} {}\n",
                        escape(channel),
                        bound,
                        e.buckets[i].load(Ordering::Relaxed)
                    ));
                }
                out.push_str(&format!(
                    "aigx_channel_latency_bucket{{channel=\"{}\",le=\"+Inf\"}} {}\n",
                    escape(channel),
                    total
                ));
                out.push_str(&format!(
                    "aigx_channel_latency_sum{{channel=\"{}\"}} {}\n",
                    escape(channel),
                    e.sum.load(Ordering::Relaxed)
                ));
                out.push_str(&format!(
                    "aigx_channel_latency_count{{channel=\"{}\"}} {}\n",
                    escape(channel),
                    total
                ));
            }
        }
        out.push_str("# HELP aigx_ttft_ms Time to first byte in milliseconds.\n");
        out.push_str("# TYPE aigx_ttft_ms histogram\n");
        {
            let map = self.ttft.lock();
            for (model, entry) in map.iter() {
                let sum = entry.sum.load(Ordering::Relaxed);
                let count = entry.count.load(Ordering::Relaxed);
                for (i, bound) in LATENCY_BUCKETS_MS.iter().enumerate() {
                    out.push_str(&format!(
                        "aigx_ttft_ms_bucket{{model=\"{}\",le=\"{}\"}} {}\n",
                        escape(model),
                        bound,
                        entry.buckets[i].load(Ordering::Relaxed)
                    ));
                }
                out.push_str(&format!(
                    "aigx_ttft_ms_bucket{{model=\"{}\",le=\"+Inf\"}} {}\n",
                    escape(model),
                    count
                ));
                out.push_str(&format!(
                    "aigx_ttft_ms_sum{{model=\"{}\"}} {}\n",
                    escape(model),
                    sum
                ));
                out.push_str(&format!(
                    "aigx_ttft_ms_count{{model=\"{}\"}} {}\n",
                    escape(model),
                    count
                ));
            }
        }
        out.push_str(
            "# HELP aigx_health_level Model health level (0=healthy,1=degraded,2=down).\n",
        );
        out.push_str("# TYPE aigx_health_level gauge\n");
        {
            let map = self.health.lock();
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
        out.push_str(&format!(
            "# TYPE aigx_channels_cooldown gauge\naigx_channels_cooldown {}\n",
            self.channels_cooldown.load(Ordering::Relaxed)
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
        assert!(
            out.contains("aigx_requests_total{model=\"gpt-4\",channel=\"ch1\",status=\"ok\"} 2")
        );
        assert!(out.contains(
            "aigx_requests_total{model=\"claude-3\",channel=\"ch2\",status=\"error\"} 1"
        ));
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

    #[test]
    fn replace_health_drops_stale_models() {
        let m = Metrics::new();
        m.set_health("old-model", 2);
        m.replace_health(vec![("new-model".to_string(), 1u8)]);
        let out = m.render();
        // 已被替换掉的模型不再输出
        assert!(!out.contains("aigx_health_level{model=\"old-model\"}"));
        assert!(out.contains("aigx_health_level{model=\"new-model\"} 1"));
    }

    #[test]
    fn latency_histogram_buckets() {
        let m = Metrics::new();
        // 120ms 落入 le=250 及以上所有桶
        m.record_request("gpt-4", "ch1", "ok", 120);
        // 5ms 只落入 le=10 桶（含全部更大桶）
        m.record_request("gpt-4", "ch1", "ok", 5);
        let out = m.render();
        assert!(out.contains("aigx_request_latency_bucket{model=\"gpt-4\",le=\"10\"} 1"));
        assert!(out.contains("aigx_request_latency_bucket{model=\"gpt-4\",le=\"250\"} 2"));
        assert!(out.contains("aigx_request_latency_bucket{model=\"gpt-4\",le=\"+Inf\"} 2"));
        assert!(out.contains("aigx_request_latency_count{model=\"gpt-4\"} 2"));
        assert!(out.contains("aigx_request_latency_sum{model=\"gpt-4\"} 125"));
    }

    #[test]
    fn channel_result_metrics_and_cooldown_gauge() {
        let m = Metrics::new();
        m.record_channel_result("ch1", true, 120);
        m.record_channel_result("ch1", true, 80);
        m.record_channel_result("ch1", false, 0);
        m.record_channel_result("ch2", false, 0);
        m.set_channels_cooldown(2);
        let out = m.render();
        assert!(out.contains("aigx_channel_requests_total{channel=\"ch1\",result=\"ok\"} 2"));
        assert!(out.contains("aigx_channel_requests_total{channel=\"ch1\",result=\"error\"} 1"));
        assert!(out.contains("aigx_channel_requests_total{channel=\"ch2\",result=\"error\"} 1"));
        assert!(out.contains("aigx_channel_latency_bucket{channel=\"ch1\",le=\"250\"} 2"));
        assert!(out.contains("aigx_channel_latency_count{channel=\"ch1\"} 3"));
        assert!(out.contains("aigx_channels_cooldown 2"));
    }

    #[test]
    fn error_type_and_ttft_metrics() {
        let m = Metrics::new();
        m.record_request_error("gpt-4", "ch1", "rate_limit_error");
        m.record_request_error("gpt-4", "ch1", "upstream_error");
        m.record_ttft("gpt-4", 150);
        m.record_ttft("gpt-4", 5);
        let out = m.render();
        assert!(out.contains(
            "aigx_request_errors_total{model=\"gpt-4\",channel=\"ch1\",error_type=\"rate_limit_error\"} 1"
        ));
        assert!(out.contains(
            "aigx_request_errors_total{model=\"gpt-4\",channel=\"ch1\",error_type=\"upstream_error\"} 1"
        ));
        assert!(out.contains("aigx_ttft_ms_bucket{model=\"gpt-4\",le=\"250\"} 2"));
        assert!(out.contains("aigx_ttft_ms_bucket{model=\"gpt-4\",le=\"10\"} 1"));
        assert!(out.contains("aigx_ttft_ms_count{model=\"gpt-4\"} 2"));
        assert!(out.contains("aigx_ttft_ms_sum{model=\"gpt-4\"} 155"));
    }
}
