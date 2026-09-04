//! AIMD 限流器 — Additive Increase / Multiplicative Decrease 自适应限流。
//!
//! 参照 burncloud `crates/router/src/aimd_limiter.rs`：
//! - 从响应头学习上游真实限流上限
//! - 按成功/失败模式动态调整请求速率
//! - 限流时进入冷却期，冷却过后试探恢复
//!
//! 状态机：
//! - `Learning`：初始学习阶段，逐步探测上限
//! - `Stable`：稳定运行，已知上限
//! - `Cooldown`：被限流后冷却，到期后回 `Learning`
//!
//! 与 AIGX 既有 `ratelimit::RateLimiter`（固定阈值）共存：AIMD 是 per-channel
//! 自适应补充，由调度器/代理层按需调用 `on_success` / `on_rate_limited`。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 调度决策用的只读快照。
#[derive(Debug, Clone, Default)]
pub struct AimdSnapshot {
    pub current_limit: u32,
    pub state: RateLimitState,
}

/// 自适应限流状态机。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateLimitState {
    /// 初始 — 学习真实上限
    #[default]
    Learning,
    /// 稳定 — 在已知上限内运行
    Stable,
    /// 冷却 — 从限流错误中恢复
    Cooldown,
}

/// 无自适应数据时的默认初始 RPM 上限。
/// 单一来源 — `AimdConfig::default()` 与调度器共用。
pub const DEFAULT_INITIAL_LIMIT: u32 = 10;

/// 限流事件时对当前上限的乘数（保留 80%）。
const RATE_LIMIT_REDUCTION_RATIO: f64 = 0.8;

/// 自适应限流器配置。
#[derive(Debug, Clone)]
pub struct AimdConfig {
    /// 从 Learning 转 Stable 所需请求数
    pub learning_duration: u32,
    /// 初始请求上限（保守起点）
    pub initial_limit: u32,
    /// 上下调整的步长
    pub adjustment_step: u32,
    /// 增加上限前所需连续成功数
    pub success_threshold: u32,
    /// 进入冷却前所需连续失败数
    pub failure_threshold: u32,
    /// 冷却持续时长
    pub cooldown_duration: Duration,
    /// 从冷却恢复时对上限的保留比例（如 0.5 = 保留 50%）
    pub recovery_ratio: f64,
    /// 最大允许上限
    pub max_limit: u32,
}

impl Default for AimdConfig {
    fn default() -> Self {
        Self {
            learning_duration: 10,
            initial_limit: DEFAULT_INITIAL_LIMIT,
            adjustment_step: 5,
            success_threshold: 5,
            failure_threshold: 2,
            cooldown_duration: Duration::from_secs(30),
            recovery_ratio: 0.5,
            max_limit: 1000,
        }
    }
}

/// 自适应限流器 — 学习并调整到上游上限。
///
/// 维护单个上游端点/模型的状态，跟踪学习到的上限并按成功/失败调整。
#[derive(Debug, Clone)]
pub struct AimdController {
    /// 从上游响应头学到的上限
    pub learned_limit: Option<u32>,
    /// 当前生效上限
    pub current_limit: u32,
    /// 状态机当前状态
    pub state: RateLimitState,
    /// 连续成功数
    pub success_streak: u32,
    /// 连续失败数（限流错误）
    pub failure_streak: u32,
    /// 冷却结束时间（Cooldown 状态时）
    pub cooldown_until: Option<Instant>,
    /// 限流到期时间（来自 429 响应）
    pub rate_limit_until: Option<Instant>,
    /// 上次调整时间
    pub last_adjusted_at: Option<Instant>,
    /// 配置
    config: AimdConfig,
    /// 已处理请求数（用于 learning_duration 判断）
    request_count: u32,
}

impl AimdController {
    /// 用指定配置构造。
    pub fn new(config: AimdConfig) -> Self {
        Self {
            learned_limit: None,
            current_limit: config.initial_limit,
            state: RateLimitState::Learning,
            success_streak: 0,
            failure_streak: 0,
            cooldown_until: None,
            rate_limit_until: None,
            last_adjusted_at: None,
            config,
            request_count: 0,
        }
    }

    /// 用默认配置构造。
    pub fn with_defaults() -> Self {
        Self::new(AimdConfig::default())
    }

    /// 请求成功时调用。
    ///
    /// - 从响应头学习限流上限
    /// - 更新成功连胜计数
    /// - Learning → Stable 转换
    /// - Learning 状态下尝试增加上限
    ///
    /// # 参数
    /// - `upstream_limit`：从响应头学到的可选限流上限
    pub fn on_success(&mut self, upstream_limit: Option<u32>) {
        let now = Instant::now();

        // 学习上游上限
        if let Some(limit) = upstream_limit {
            if limit > 0 {
                self.learned_limit = Some(limit);
                if limit > self.current_limit {
                    self.current_limit = limit.min(self.config.max_limit);
                    self.last_adjusted_at = Some(now);
                }
            }
        }

        self.request_count += 1;
        self.success_streak += 1;
        self.failure_streak = 0;

        match self.state {
            RateLimitState::Learning => {
                if self.request_count >= self.config.learning_duration {
                    self.state = RateLimitState::Stable;
                    tracing::info!(
                        "AdaptiveLimit: 处理 {} 个请求后转为 Stable",
                        self.request_count
                    );
                }
                if self.success_streak >= self.config.success_threshold {
                    self.try_increase_limit(now);
                }
            }
            RateLimitState::Stable => {
                if self.success_streak >= self.config.success_threshold {
                    self.try_increase_limit(now);
                }
            }
            RateLimitState::Cooldown => {
                if let Some(cooldown_until) = self.cooldown_until {
                    if now >= cooldown_until {
                        self.recover_from_cooldown();
                    }
                }
            }
        }

        // 清除已过期的限流到期标记
        if let Some(rate_limit_until) = self.rate_limit_until {
            if now >= rate_limit_until {
                self.rate_limit_until = None;
            }
        }
    }

    /// 遇到限流错误（429）时调用。
    ///
    /// - 更新失败连胜计数
    /// - 当前上限降至 80%
    /// - 检查是否进入 Cooldown
    /// - 记录限流到期时间
    ///
    /// # 参数
    /// - `retry_after`：距允许重试的秒数
    pub fn on_rate_limited(&mut self, retry_after: Option<u64>) {
        let now = Instant::now();

        self.failure_streak += 1;
        self.success_streak = 0;

        if let Some(seconds) = retry_after {
            self.rate_limit_until = Some(now + Duration::from_secs(seconds));
        }

        // 保留 80%
        let new_limit = (self.current_limit as f64 * RATE_LIMIT_REDUCTION_RATIO).ceil() as u32;
        self.current_limit = new_limit.max(1);
        self.last_adjusted_at = Some(now);

        tracing::warn!("AdaptiveLimit: 限流后上限降至 {}", self.current_limit);

        if self.failure_streak >= self.config.failure_threshold {
            self.enter_cooldown(now);
        }
    }

    /// 检查当前是否允许请求。
    ///
    /// 返回 false 当：
    /// - 处于 Cooldown 且冷却未到期
    /// - 限流到期时间未过
    pub fn check_available(&self) -> bool {
        let now = Instant::now();

        if self.state == RateLimitState::Cooldown {
            if let Some(cooldown_until) = self.cooldown_until {
                if now < cooldown_until {
                    return false;
                }
                // 冷却到期 → 放试探请求通过
                // 完整恢复在 on_success() 试探成功时发生
                // 试探若再 429，on_rate_limited() 重新进 Cooldown
            }
        }

        if let Some(rate_limit_until) = self.rate_limit_until {
            if now < rate_limit_until {
                return false;
            }
        }

        true
    }

    /// 取当前生效上限。
    pub fn get_current_limit(&self) -> u32 {
        self.current_limit
    }

    /// 取学习到的上游上限。
    pub fn get_learned_limit(&self) -> Option<u32> {
        self.learned_limit
    }

    /// 取当前状态。
    pub fn get_state(&self) -> &RateLimitState {
        &self.state
    }

    /// 取调度决策用快照。
    pub fn snapshot(&self) -> AimdSnapshot {
        AimdSnapshot {
            current_limit: self.current_limit,
            state: self.state,
        }
    }

    /// 进入冷却。
    fn enter_cooldown(&mut self, now: Instant) {
        self.state = RateLimitState::Cooldown;
        self.cooldown_until = Some(now + self.config.cooldown_duration);
        tracing::warn!(
            "AdaptiveLimit: 进入 Cooldown {}s",
            self.config.cooldown_duration.as_secs()
        );
    }

    /// 从冷却恢复 — 状态置 Learning，上限按 recovery_ratio 缩减。
    fn recover_from_cooldown(&mut self) {
        let now = Instant::now();
        let new_limit = (self.current_limit as f64 * self.config.recovery_ratio).ceil() as u32;
        self.current_limit = new_limit.max(1);

        self.state = RateLimitState::Learning;
        self.cooldown_until = None;
        self.failure_streak = 0;
        self.success_streak = 0;
        self.last_adjusted_at = Some(now);

        tracing::info!(
            "AdaptiveLimit: 从 Cooldown 恢复，新上限: {}",
            self.current_limit
        );
    }

    /// 尝试增加当前上限。
    fn try_increase_limit(&mut self, now: Instant) {
        let max_allowed = self.learned_limit.unwrap_or(self.config.max_limit);
        let max_allowed = max_allowed.min(self.config.max_limit);

        if self.current_limit < max_allowed {
            let new_limit = (self.current_limit + self.config.adjustment_step).min(max_allowed);
            self.current_limit = new_limit;
            self.last_adjusted_at = Some(now);
            self.success_streak = 0; // 调整后重置连胜

            tracing::debug!("AdaptiveLimit: 上限增至 {}", self.current_limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_learning_with_default_limit() {
        let limiter = AimdController::with_defaults();
        assert_eq!(limiter.state, RateLimitState::Learning);
        assert_eq!(limiter.current_limit, DEFAULT_INITIAL_LIMIT);
        assert!(limiter.check_available());
    }

    #[test]
    fn learning_to_stable_transition() {
        let mut limiter = AimdController::new(AimdConfig {
            learning_duration: 5,
            ..Default::default()
        });
        for _ in 0..5 {
            limiter.on_success(None);
        }
        assert_eq!(limiter.state, RateLimitState::Stable);
    }

    #[test]
    fn rate_limited_reduces_limit() {
        let mut limiter = AimdController::with_defaults();
        let initial = limiter.current_limit;
        limiter.on_rate_limited(None);
        assert!(limiter.current_limit < initial);
        assert_eq!(
            limiter.current_limit,
            (initial as f64 * RATE_LIMIT_REDUCTION_RATIO).ceil() as u32
        );
    }

    #[test]
    fn cooldown_state_blocks_requests() {
        let mut limiter = AimdController::new(AimdConfig {
            failure_threshold: 1,
            cooldown_duration: Duration::from_secs(1),
            ..Default::default()
        });
        limiter.on_rate_limited(None);
        assert_eq!(limiter.state, RateLimitState::Cooldown);
        assert!(!limiter.check_available());
    }

    #[test]
    fn recovery_from_cooldown() {
        let mut limiter = AimdController::new(AimdConfig {
            failure_threshold: 1,
            cooldown_duration: Duration::from_millis(10),
            ..Default::default()
        });
        let initial = limiter.current_limit;
        limiter.on_rate_limited(None);
        assert_eq!(limiter.state, RateLimitState::Cooldown);
        // 等待冷却过期
        std::thread::sleep(Duration::from_millis(20));
        limiter.check_available();
        // 通过 on_success 触发恢复
        limiter.on_success(None);
        assert_eq!(limiter.state, RateLimitState::Learning);
        // 上限应被 recovery_ratio (50%) 进一步缩减
        assert!(
            limiter.current_limit
                <= (initial as f64 * RATE_LIMIT_REDUCTION_RATIO * 0.5).ceil() as u32
        );
    }

    #[test]
    fn learn_upstream_limit_from_header() {
        let mut limiter = AimdController::with_defaults();
        limiter.on_success(Some(100));
        assert_eq!(limiter.learned_limit, Some(100));
        assert_eq!(limiter.current_limit, 100);
    }

    #[test]
    fn success_streak_increases_limit() {
        let mut limiter = AimdController::new(AimdConfig {
            success_threshold: 3,
            adjustment_step: 5,
            learning_duration: 100, // 保持 Learning
            ..Default::default()
        });
        let initial = limiter.current_limit;
        for _ in 0..3 {
            limiter.on_success(None);
        }
        assert_eq!(limiter.current_limit, initial + 5);
    }

    #[test]
    fn rate_limit_expiry_allows_after_timeout() {
        let mut limiter = AimdController::with_defaults();
        limiter.on_rate_limited(Some(0)); // 0 秒
        assert!(limiter.check_available());
    }

    #[test]
    fn snapshot_captures_current_state() {
        let mut limiter = AimdController::with_defaults();
        limiter.on_success(Some(50));
        let snap = limiter.snapshot();
        assert_eq!(snap.current_limit, 50);
        assert_eq!(snap.state, RateLimitState::Learning);
    }

    #[test]
    fn learned_limit_caps_at_max_limit() {
        let mut limiter = AimdController::new(AimdConfig {
            max_limit: 200,
            ..Default::default()
        });
        // 上游报告 1000，但 max_limit=200
        limiter.on_success(Some(1000));
        assert_eq!(limiter.learned_limit, Some(1000)); // 学到的原值记录
        assert_eq!(limiter.current_limit, 200); // 但生效上限被 cap
    }

    #[test]
    fn current_limit_never_drops_below_one() {
        let mut limiter = AimdController::with_defaults();
        // 反复限流
        for _ in 0..100 {
            limiter.on_rate_limited(None);
        }
        assert!(limiter.current_limit >= 1);
    }
}
