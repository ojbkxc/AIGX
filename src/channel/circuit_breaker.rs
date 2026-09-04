//! 渠道断路器 — per-channel 故障自动熔断。
//!
//! 参照 burncloud `crates/router/src/circuit_breaker.rs` 的设计：
//! - 每个渠道维护一份 `UpstreamState`（失败计数 + 最近失败时间 + 失败类型 + 限流到期时间）
//! - 失败次数超过阈值（默认 5）则打开断路器
//! - 冷却期（默认 30 秒）过后进入 HalfOpen，放一个试探请求
//! - AuthFailed / PaymentRequired 视为永久故障，强制长期冷却（30 分钟）
//! - RateLimited 记入限流到期窗口，期间拒绝请求
//!
//! 与 AIGX 既有 `ChannelStore::mark_cooldown` 互补：mark_cooldown 是粗粒度
//! 冷却（任意错误 60s），断路器是细粒度按失败类型分级处理。集成时二者并存，
//! 断路器在 `select_for_model` 过滤阶段生效，mark_cooldown 仍由调用方按需触发。

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// 限流作用域，参照 burncloud `RateLimitScope`。
///
/// 用于区分账号级限流（影响该渠道所有模型）与模型级限流（仅影响特定模型）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateLimitScope {
    /// 账号级限流（影响渠道下所有模型）
    Account,
    /// 模型级限流（仅影响特定模型）
    Model,
    /// 作用域未知（按账号级保守处理）
    #[default]
    Unknown,
}

/// 渠道失败类型，参照 burncloud `FailureType`。
///
/// 不同失败类型在断路器中触发不同行为：
/// - `AuthFailed` / `PaymentRequired`：视为永久故障，强制 30 分钟冷却
/// - `RateLimited`：记录限流到期时间，期间拒绝请求
/// - 其他类型：累加失败计数，达到阈值后打开断路器
#[derive(Debug, Clone)]
pub enum FailureType {
    /// 认证失败（401）
    AuthFailed,
    /// 余额不足 / 配额耗尽（402）
    PaymentRequired,
    /// 被上游限流（429）
    RateLimited {
        /// 限流作用域
        scope: RateLimitScope,
        /// 距限流重置的秒数（无则用默认 60s）
        retry_after: Option<u64>,
    },
    /// 上游不提供该模型（404）
    ModelNotFound,
    /// 上游服务端错误（5xx）
    ServerError,
    /// 请求超时
    Timeout,
    /// 连接失败（DNS / TCP / TLS）
    ConnectionError,
    /// 空响应（HTTP 200 但零 token）
    EmptyResponse,
}

/// 单个渠道的断路器状态。
#[derive(Debug)]
struct UpstreamState {
    /// 连续失败计数（成功时清零）
    failure_count: AtomicU32,
    /// 最近一次失败时间（用于冷却期判断）
    last_failure_time: Option<Instant>,
    /// 最近一次失败类型
    failure_type: Option<FailureType>,
    /// 限流到期时间（被 429 时设置）
    rate_limit_until: Option<Instant>,
}

impl Default for UpstreamState {
    fn default() -> Self {
        Self {
            failure_count: AtomicU32::new(0),
            last_failure_time: None,
            failure_type: None,
            rate_limit_until: None,
        }
    }
}

/// 无 `retry_after` 头时的默认限流时长（秒）。
const DEFAULT_RATE_LIMIT_RETRY_SECS: u64 = 60;
/// AuthFailed / PaymentRequired 的强制冷却时长（30 分钟）。
const PERMANENT_FAILURE_COOLDOWN_SECS: u64 = 1800;

/// 渠道断路器 — per-channel 状态机（Closed / Open / HalfOpen）。
///
/// 线程安全：内部用 `DashMap` 存储 per-channel 状态，失败计数用 `AtomicU32`。
/// 热路径 `allow_request` 只读，不创建条目，健康渠道零开销。
pub struct CircuitBreaker {
    /// channel_id → 状态
    states: DashMap<String, UpstreamState>,
    /// 打开断路器的失败次数阈值
    failure_threshold: u32,
    /// 打开后的冷却时长
    cooldown_duration: Duration,
}

impl CircuitBreaker {
    /// 构造断路器。
    ///
    /// - `failure_threshold`：连续失败多少次打开断路器（burncloud 默认 5）
    /// - `cooldown_seconds`：打开后冷却秒数（burncloud 默认 30）
    pub fn new(failure_threshold: u32, cooldown_seconds: u64) -> Self {
        Self {
            states: DashMap::new(),
            failure_threshold,
            cooldown_duration: Duration::from_secs(cooldown_seconds),
        }
    }

    /// 默认配置：阈值 5，冷却 30s（与 burncloud 对齐）。
    pub fn with_defaults() -> Self {
        Self::new(5, 30)
    }

    /// 判断指定渠道当前是否允许请求。
    ///
    /// - 渠道无状态（从未失败）→ 允许
    /// - 限流未到期 → 拒绝
    /// - 失败计数 < 阈值 → 允许（Closed）
    /// - 失败计数 ≥ 阈值且冷却未过 → 拒绝（Open）
    /// - 失败计数 ≥ 阈值且冷却已过 → 允许（HalfOpen 试探）
    pub fn allow_request(&self, channel_id: &str) -> bool {
        let entry = match self.states.get(channel_id) {
            Some(e) => e,
            None => return true, // 无状态 = 从未失败 = 放行
        };

        // 限流未到期 → 拒绝
        if let Some(rate_limit_until) = entry.rate_limit_until {
            if rate_limit_until > Instant::now() {
                return false;
            }
        }

        let current_failures = entry.failure_count.load(Ordering::Relaxed);
        if current_failures < self.failure_threshold {
            return true; // Closed
        }

        // Open：检查冷却是否已过
        if let Some(last_failure) = entry.last_failure_time {
            if last_failure.elapsed() >= self.cooldown_duration {
                return true; // HalfOpen 放一个试探
            }
        }

        false // Open
    }

    /// 记入一次成功 — 清零失败计数与限流状态。
    pub fn record_success(&self, channel_id: &str) {
        if let Some(mut entry) = self.states.get_mut(channel_id) {
            entry.failure_count.store(0, Ordering::Relaxed);
            entry.last_failure_time = None;
            entry.failure_type = None;
            entry.rate_limit_until = None;
        }
    }

    /// 计入一次失败（按类型分级处理）。
    ///
    /// - `AuthFailed` / `PaymentRequired`：失败计数 ×10 + 30 分钟冷却
    /// - `RateLimited`：设置限流到期时间 + 失败计数 +1
    /// - 其他：失败计数 +1
    pub fn record_failure(&self, channel_id: &str, failure_type: FailureType) {
        let mut entry = self.states.entry(channel_id.to_string()).or_default();
        entry.failure_type = Some(failure_type.clone());
        entry.last_failure_time = Some(Instant::now());

        match &failure_type {
            FailureType::AuthFailed | FailureType::PaymentRequired => {
                // 永久性故障：放大计数 + 长冷却，避免反复重试坏密钥/空余额渠道
                entry
                    .failure_count
                    .store(self.failure_threshold.saturating_mul(10), Ordering::Relaxed);
                entry.last_failure_time = Some(Instant::now());
                entry.rate_limit_until =
                    Some(Instant::now() + Duration::from_secs(PERMANENT_FAILURE_COOLDOWN_SECS));
                tracing::warn!(
                    "CircuitBreaker: 渠道 {} 认证/余额故障 ({:?}) — 断路器打开 30 分钟",
                    channel_id,
                    failure_type
                );
            }
            FailureType::RateLimited { retry_after, .. } => {
                let duration = retry_after
                    .as_ref()
                    .map(|r| Duration::from_secs(*r))
                    .unwrap_or_else(|| Duration::from_secs(DEFAULT_RATE_LIMIT_RETRY_SECS));
                entry.rate_limit_until = Some(Instant::now() + duration);
                let new_count = entry.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if new_count >= self.failure_threshold {
                    tracing::warn!(
                        "CircuitBreaker: 渠道 {} 因限流打开断路器 (失败计数 {})",
                        channel_id,
                        new_count
                    );
                }
            }
            _ => {
                let new_count = entry.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if new_count >= self.failure_threshold {
                    tracing::warn!(
                        "CircuitBreaker: 渠道 {} 打开断路器 (失败计数 {})",
                        channel_id,
                        new_count
                    );
                }
            }
        }
    }

    /// 紧急熔断：强制所有已知渠道进入 Open 状态。
    ///
    /// 返回被熔断的渠道 ID 列表。用于全局故障（如所有上游不可达）场景。
    pub fn trip_all(&self) -> Vec<String> {
        let mut tripped = Vec::new();
        for mut entry in self.states.iter_mut() {
            entry
                .failure_count
                .store(self.failure_threshold, Ordering::Relaxed);
            entry.last_failure_time = Some(Instant::now());
            entry.failure_type = Some(FailureType::ServerError);
            entry.rate_limit_until = None;
            tripped.push(entry.key().clone());
        }
        tracing::warn!(
            "CircuitBreaker: 紧急全量熔断 — {} 个渠道强制 Open",
            tripped.len()
        );
        tripped
    }

    /// 获取所有渠道的断路器状态（监控/管理面用）。
    pub fn get_status_map(&self) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        for r in self.states.iter() {
            let count = r.value().failure_count.load(Ordering::Relaxed);
            let status = if count >= self.failure_threshold {
                if let Some(last) = r.value().last_failure_time {
                    if last.elapsed() < self.cooldown_duration {
                        format!(
                            "Open (剩余 {}s)",
                            (self.cooldown_duration - last.elapsed()).as_secs()
                        )
                    } else {
                        "HalfOpen (试探中)".to_string()
                    }
                } else {
                    "Open".to_string()
                }
            } else {
                "Closed (健康)".to_string()
            };
            map.insert(r.key().clone(), status);
        }
        map
    }

    /// 手动重置指定渠道的断路器（管理面用）。
    pub fn reset(&self, channel_id: &str) {
        if let Some(mut entry) = self.states.get_mut(channel_id) {
            entry.failure_count.store(0, Ordering::Relaxed);
            entry.last_failure_time = None;
            entry.failure_type = None;
            entry.rate_limit_until = None;
        }
    }

    /// 当前正在被断路器阻断的渠道数量（监控用）。
    pub fn open_count(&self) -> usize {
        self.states
            .iter()
            .filter(|r| !self.allow_request(r.key()))
            .count()
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_is_allowed() {
        let cb = CircuitBreaker::with_defaults();
        assert!(cb.allow_request("unknown"));
    }

    #[test]
    fn threshold_opens_circuit() {
        let cb = CircuitBreaker::new(3, 30);
        for _ in 0..3 {
            cb.record_failure("ch1", FailureType::ServerError);
        }
        assert!(!cb.allow_request("ch1"));
    }

    #[test]
    fn success_resets_circuit() {
        let cb = CircuitBreaker::new(3, 30);
        cb.record_failure("ch1", FailureType::ServerError);
        cb.record_failure("ch1", FailureType::ServerError);
        cb.record_success("ch1");
        assert!(cb.allow_request("ch1"));
    }

    #[test]
    fn auth_failed_trips_immediately() {
        let cb = CircuitBreaker::new(5, 30);
        cb.record_failure("ch1", FailureType::AuthFailed);
        assert!(!cb.allow_request("ch1"), "AuthFailed 应立即打开断路器");
    }

    #[test]
    fn rate_limited_blocks_until_expiry() {
        let cb = CircuitBreaker::new(5, 30);
        cb.record_failure(
            "ch1",
            FailureType::RateLimited {
                scope: RateLimitScope::Model,
                retry_after: Some(60),
            },
        );
        assert!(!cb.allow_request("ch1"), "限流期间应拒绝");
    }

    #[test]
    fn cooldown_allows_probe() {
        let cb = CircuitBreaker::new(1, 0); // 冷却 0s → 立即 HalfOpen
        cb.record_failure("ch1", FailureType::ServerError);
        // 冷却 0s，elapsed >= 0 总成立 → 放试探
        assert!(cb.allow_request("ch1"));
    }
}
