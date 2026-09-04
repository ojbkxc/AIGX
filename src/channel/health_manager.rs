//! 渠道健康管理 — 追踪 per-channel / per-model 的错误率、延迟与最近错误。
//!
//! 参照 burncloud `crates/router/src/channel_state.rs` 的 `ChannelStateTracker`：
//! - 每个渠道维护一份 `ChannelState`（认证/余额/账号限流 + per-model 状态）
//! - 每个 `(channel, model)` 维护一份 `ModelState`（成功率、延迟 EMA、最近错误）
//! - `is_available` 综合渠道级与模型级状态判断可用性
//! - `record_error` / `record_success` 在请求失败/成功时调用
//!
//! 与 `circuit_breaker` 互补：断路器管"是否放行"，健康管理器管"健康分与诊断信息"。
//! 调度器可用健康分做加权排序，运维可通过 `get_health` 查看渠道健康汇总。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use super::circuit_breaker::{FailureType, RateLimitScope};

/// 延迟 EMA 平滑因子（越小越平滑）。
const LATENCY_EMA_ALPHA: f64 = 0.2;
/// 无 `retry_after` 时的默认限流时长（秒）。
const DEFAULT_RATE_LIMIT_RETRY_SECS: u64 = 60;

/// 渠道账号余额状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BalanceStatus {
    /// 余额充足
    Ok,
    /// 余额偏低
    Low,
    /// 余额耗尽
    Exhausted,
    /// 未知
    #[default]
    Unknown,
}

/// 模型运营状态。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelStatus {
    /// 可用
    #[default]
    Available,
    /// 被限流
    RateLimited,
    /// 配额耗尽
    QuotaExhausted,
    /// 模型不存在
    ModelNotFound,
    /// 临时不可用
    TemporarilyDown,
}

/// `(channel, model)` 维度的运营状态。
#[derive(Debug, Clone)]
pub struct ModelState {
    /// 模型名
    pub model: String,
    /// 渠道 ID
    pub channel_id: String,
    /// 运营状态
    pub status: ModelStatus,
    /// 限流到期时间（被 429 时设置）
    pub rate_limit_until: Option<Instant>,
    /// 最近错误信息
    pub last_error: Option<String>,
    /// 最近错误时间
    pub last_error_time: Option<Instant>,
    /// 成功请求计数
    pub success_count: u64,
    /// 失败请求计数
    pub failure_count: u64,
    /// 延迟 EMA（毫秒）
    pub avg_latency_ms: f64,
}

impl ModelState {
    fn new(model: String, channel_id: String) -> Self {
        Self {
            model,
            channel_id,
            status: ModelStatus::default(),
            rate_limit_until: None,
            last_error: None,
            last_error_time: None,
            success_count: 0,
            failure_count: 0,
            avg_latency_ms: 0.0,
        }
    }

    /// 错误率（[0, 1]）。无请求时返回 0。
    pub fn error_rate(&self) -> f64 {
        let total = self.success_count + self.failure_count;
        if total == 0 {
            0.0
        } else {
            self.failure_count as f64 / total as f64
        }
    }

    /// 记录一次成功，更新延迟 EMA 与成功计数。
    fn record_success(&mut self, latency_ms: u64) {
        self.success_count += 1;
        // EMA: new = alpha * sample + (1 - alpha) * old
        let sample = latency_ms as f64;
        self.avg_latency_ms =
            LATENCY_EMA_ALPHA * sample + (1.0 - LATENCY_EMA_ALPHA) * self.avg_latency_ms;
        // 成功后清除临时错误状态
        if matches!(
            self.status,
            ModelStatus::TemporarilyDown | ModelStatus::RateLimited
        ) {
            self.status = ModelStatus::Available;
            self.rate_limit_until = None;
        }
    }
}

/// 渠道级状态（认证 + 余额 + 账号限流 + per-model 状态）。
#[derive(Debug, Clone)]
pub struct ChannelState {
    /// 渠道 ID
    pub channel_id: String,
    /// 认证是否有效
    pub auth_ok: bool,
    /// 余额状态
    pub balance_status: BalanceStatus,
    /// 账号级限流到期时间
    pub account_rate_limit_until: Option<Instant>,
    /// per-model 状态
    pub models: HashMap<String, ModelState>,
}

impl ChannelState {
    fn new(channel_id: String) -> Self {
        Self {
            channel_id,
            auth_ok: true,
            balance_status: BalanceStatus::default(),
            account_rate_limit_until: None,
            models: HashMap::new(),
        }
    }

    /// 取或创建 model 状态（entry API 单次哈希查找）。
    fn get_or_create_model(&mut self, model_name: &str, channel_id: &str) -> &mut ModelState {
        use std::collections::hash_map::Entry;
        match self.models.entry(model_name.to_string()) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => {
                let key = e.key().clone();
                e.insert(ModelState::new(key, channel_id.to_string()))
            }
        }
    }
}

/// 渠道健康汇总（监控/管理面用）。
#[derive(Debug, Clone, Serialize)]
pub struct ChannelHealthSummary {
    pub channel_id: String,
    pub auth_ok: bool,
    pub balance_status: BalanceStatus,
    /// 综合错误率（所有模型的失败总和 / 请求总和）
    pub overall_error_rate: f64,
    /// 综合平均延迟（毫秒）
    pub overall_avg_latency_ms: f64,
    /// per-model 错误率
    pub model_error_rates: HashMap<String, f64>,
    /// 最近错误信息
    pub last_error: Option<String>,
}

/// 全局渠道状态追踪器 — DashMap per-channel。
///
/// 线程安全：DashMap 分片锁，热路径 `is_available` 只读。
pub struct ChannelStateTracker {
    channel_states: DashMap<String, ChannelState>,
}

impl Default for ChannelStateTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelStateTracker {
    pub fn new() -> Self {
        Self {
            channel_states: DashMap::new(),
        }
    }

    /// 判断 `(channel_id, model)` 是否可用。
    ///
    /// - 渠道未知 → 视为可用（不阻塞未知渠道）
    /// - 渠道级：认证失败 / 余额耗尽 / 账号限流未到期 → 不可用
    /// - 模型级：状态非 Available 且限流未到期 → 不可用
    pub fn is_available(&self, channel_id: &str, model: Option<&str>) -> bool {
        let now = Instant::now();
        let channel_state = match self.channel_states.get(channel_id) {
            Some(s) => s,
            None => return true,
        };

        if !channel_state.auth_ok {
            return false;
        }
        if channel_state.balance_status == BalanceStatus::Exhausted {
            return false;
        }
        if let Some(rl) = channel_state.account_rate_limit_until {
            if rl > now {
                return false;
            }
        }

        let model_name = match model {
            Some(m) => m,
            None => return true,
        };

        if let Some(ms) = channel_state.models.get(model_name) {
            match ms.status {
                ModelStatus::Available => {}
                ModelStatus::RateLimited | ModelStatus::TemporarilyDown => {
                    // 限流到期则放试探，未到期则拒绝
                    if let Some(rl) = ms.rate_limit_until {
                        if rl > now {
                            return false;
                        }
                    } else {
                        return false; // 无到期时间 → 持续阻塞
                    }
                }
                ModelStatus::QuotaExhausted | ModelStatus::ModelNotFound => {
                    return false; // 永久性
                }
            }
            if let Some(rl) = ms.rate_limit_until {
                if rl > now {
                    return false;
                }
            }
        }
        true
    }

    /// 记入一次错误（按 `FailureType` 分级更新渠道/模型状态）。
    pub fn record_error(
        &self,
        channel_id: &str,
        model: Option<&str>,
        failure_type: &FailureType,
        error_message: &str,
    ) {
        let mut channel_state = self
            .channel_states
            .entry(channel_id.to_string())
            .or_insert_with(|| ChannelState::new(channel_id.to_string()));
        let now = Instant::now();

        match failure_type {
            FailureType::AuthFailed => {
                channel_state.auth_ok = false;
                // 认证失败波及所有模型
                for m in channel_state.models.values_mut() {
                    m.status = ModelStatus::TemporarilyDown;
                    m.last_error = Some(error_message.to_string());
                    m.last_error_time = Some(now);
                    m.failure_count += 1;
                }
            }
            FailureType::PaymentRequired => {
                channel_state.balance_status = BalanceStatus::Exhausted;
            }
            FailureType::RateLimited { scope, retry_after } => {
                let dur = retry_after
                    .as_ref()
                    .map(|r| Duration::from_secs(*r))
                    .unwrap_or_else(|| Duration::from_secs(DEFAULT_RATE_LIMIT_RETRY_SECS));
                let retry_until = now + dur;
                match scope {
                    RateLimitScope::Account => {
                        channel_state.account_rate_limit_until = Some(retry_until);
                    }
                    RateLimitScope::Model => {
                        if let Some(mn) = model {
                            let ms = channel_state.get_or_create_model(mn, channel_id);
                            ms.status = ModelStatus::RateLimited;
                            ms.rate_limit_until = Some(retry_until);
                            ms.last_error = Some(error_message.to_string());
                            ms.last_error_time = Some(now);
                            ms.failure_count += 1;
                        }
                    }
                    RateLimitScope::Unknown => {
                        // 未知作用域按账号级保守处理
                        channel_state.account_rate_limit_until = Some(retry_until);
                        if let Some(mn) = model {
                            let ms = channel_state.get_or_create_model(mn, channel_id);
                            ms.failure_count += 1;
                        }
                    }
                }
            }
            FailureType::ModelNotFound => {
                if let Some(mn) = model {
                    let ms = channel_state.get_or_create_model(mn, channel_id);
                    ms.status = ModelStatus::ModelNotFound;
                    ms.last_error = Some(error_message.to_string());
                    ms.last_error_time = Some(now);
                    ms.failure_count += 1;
                }
            }
            FailureType::EmptyResponse => {
                // 空响应视为临时故障
                if let Some(mn) = model {
                    let ms = channel_state.get_or_create_model(mn, channel_id);
                    ms.status = ModelStatus::TemporarilyDown;
                    ms.last_error = Some(error_message.to_string());
                    ms.last_error_time = Some(now);
                    ms.failure_count += 1;
                }
            }
            FailureType::ServerError | FailureType::Timeout | FailureType::ConnectionError => {
                // 瞬时故障：更新模型状态但不永久标记
                if let Some(mn) = model {
                    let ms = channel_state.get_or_create_model(mn, channel_id);
                    ms.last_error = Some(error_message.to_string());
                    ms.last_error_time = Some(now);
                    ms.failure_count += 1;
                }
            }
        }
    }

    /// 记入一次成功 — 清除临时错误状态，更新延迟 EMA。
    pub fn record_success(&self, channel_id: &str, model: Option<&str>, latency_ms: u64) {
        let mut channel_state = match self.channel_states.get_mut(channel_id) {
            Some(s) => s,
            None => return,
        };
        // 成功后恢复认证与余额状态
        channel_state.auth_ok = true;
        if channel_state.balance_status == BalanceStatus::Exhausted {
            channel_state.balance_status = BalanceStatus::Ok;
        }
        if let Some(mn) = model {
            let ms = channel_state.get_or_create_model(mn, channel_id);
            ms.record_success(latency_ms);
        }
    }

    /// 获取渠道健康汇总（监控/管理面用）。
    pub fn get_health(&self, channel_id: &str) -> Option<ChannelHealthSummary> {
        let state = self.channel_states.get(channel_id)?;
        let mut total_success = 0u64;
        let mut total_failure = 0u64;
        let mut total_latency = 0.0_f64;
        let mut model_error_rates = HashMap::new();
        let mut last_error = None;
        let mut last_error_time: Option<Instant> = None;

        for (mn, ms) in &state.models {
            total_success += ms.success_count;
            total_failure += ms.failure_count;
            total_latency += ms.avg_latency_ms;
            model_error_rates.insert(mn.clone(), ms.error_rate());
            if ms.last_error_time > last_error_time {
                last_error_time = ms.last_error_time;
                last_error = ms.last_error.clone();
            }
        }

        let total = total_success + total_failure;
        let overall_error_rate = if total == 0 {
            0.0
        } else {
            total_failure as f64 / total as f64
        };
        let overall_avg_latency_ms = if state.models.is_empty() {
            0.0
        } else {
            total_latency / state.models.len() as f64
        };

        Some(ChannelHealthSummary {
            channel_id: channel_id.to_string(),
            auth_ok: state.auth_ok,
            balance_status: state.balance_status.clone(),
            overall_error_rate,
            overall_avg_latency_ms,
            model_error_rates,
            last_error,
        })
    }

    /// 手动重置渠道状态（管理面用）。
    pub fn reset(&self, channel_id: &str) {
        if let Some(mut s) = self.channel_states.get_mut(channel_id) {
            *s = ChannelState::new(channel_id.to_string());
        }
    }

    /// 已追踪的渠道数。
    pub fn tracked_count(&self) -> usize {
        self.channel_states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_is_available() {
        let t = ChannelStateTracker::new();
        assert!(t.is_available("unknown", Some("m")));
    }

    #[test]
    fn auth_failed_blocks_channel() {
        let t = ChannelStateTracker::new();
        t.record_error("c1", Some("m"), &FailureType::AuthFailed, "bad key");
        assert!(!t.is_available("c1", Some("m")));
        assert!(!t.is_available("c1", Some("other-model")));
    }

    #[test]
    fn payment_required_blocks_channel() {
        let t = ChannelStateTracker::new();
        t.record_error("c1", None, &FailureType::PaymentRequired, "no balance");
        assert!(!t.is_available("c1", Some("m")));
    }

    #[test]
    fn model_not_found_blocks_only_that_model() {
        let t = ChannelStateTracker::new();
        t.record_error("c1", Some("m1"), &FailureType::ModelNotFound, "404");
        assert!(!t.is_available("c1", Some("m1")));
        assert!(t.is_available("c1", Some("m2")));
    }

    #[test]
    fn success_recovers_temporary_state() {
        let t = ChannelStateTracker::new();
        t.record_error("c1", Some("m"), &FailureType::ServerError, "boom");
        // ServerError 不永久标记，仍可用
        assert!(t.is_available("c1", Some("m")));
        t.record_success("c1", Some("m"), 100);
        let h = t.get_health("c1").unwrap();
        assert!(h.auth_ok);
        assert_eq!(h.overall_error_rate, 0.0);
    }

    #[test]
    fn health_summary_aggregates() {
        let t = ChannelStateTracker::new();
        t.record_success("c1", Some("m1"), 100);
        t.record_success("c1", Some("m1"), 200);
        t.record_error("c1", Some("m1"), &FailureType::ServerError, "x");
        let h = t.get_health("c1").unwrap();
        // 2 成功 1 失败 → 错误率 1/3
        assert!((h.overall_error_rate - 1.0 / 3.0).abs() < 1e-9);
    }
}
