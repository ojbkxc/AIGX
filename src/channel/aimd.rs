//! AIMD 自适应限流 — per-channel 学习上游限速并动态调整。
//!
//! 参照 burncloud `crates/router/src/aimd_limiter.rs`：
//! - AIMD（Additive Increase / Multiplicative Decrease，TCP 拥塞控制家族）
//! - 三态状态机：Learning（学习期）→ Stable（稳定期）⇄ Cooldown（冷却期）
//! - 成功连击 ≥ 阈值 → 线性增加限额（+adjustment_step，不超过 max_limit）
//! - 429 → 限额乘 0.8（至少保 1），连续失败 ≥ 阈值 → 进入 Cooldown
//! - Cooldown 到期后放试探请求，试探成功 → 限额减半恢复 Learning
//!
//! 与 `circuit_breaker` 互补：断路器管"故障熔断"（5xx/认证/超时），
//! AIMD 管"限速适应"（429/速率）。

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// 自适应限速状态快照（调度决策用，只读）。
#[derive(Debug, Clone, Default)]
pub struct AimdSnapshot {
    pub current_limit: u32,
    pub state: RateLimitState,
}

/// AIMD 状态机状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateLimitState {
    /// 学习期——正在探测上游实际限额
    #[default]
    Learning,
    /// 稳定期——在已知限额内运行
    Stable,
    /// 冷却期——从限速错误恢复中
    Cooldown,
}

/// 无自适应数据渠道的默认初始 RPM 限额。
pub const DEFAULT_INITIAL_LIMIT: u32 = 10;

/// 429 时的限额缩减系数（保留 80%）。
const RATE_LIMIT_REDUCTION_RATIO: f64 = 0.8;

/// AIMD 配置。
#[derive(Debug, Clone)]
pub struct AimdConfig {
    /// 学习期请求数（超过后转 Stable）
    pub learning_duration: u32,
    /// 初始请求限额（保守起点）
    pub initial_limit: u32,
    /// 限额上调步长
    pub adjustment_step: u32,
    /// 连续成功多少次后上调限额
    pub success_threshold: u32,
    /// 连续失败多少次后进入冷却
    pub failure_threshold: u32,
    /// 冷却时长
    pub cooldown_duration: Duration,
    /// 冷却恢复时限额再缩减比例（0.5 = 再减半）
    pub recovery_ratio: f64,
    /// 限额上限
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

/// 单渠道 AIMD 控制器。
#[derive(Debug, Clone)]
pub struct AimdController {
    /// 从上游响应头学到的限额
    pub learned_limit: Option<u32>,
    /// 当前生效限额
    pub current_limit: u32,
    /// 状态机当前状态
    pub state: RateLimitState,
    /// 连续成功计数
    pub success_streak: u32,
    /// 连续失败计数（429）
    pub failure_streak: u32,
    /// 冷却截止时间
    pub cooldown_until: Option<Instant>,
    /// 限速截止时间（来自 429 的 Retry-After）
    pub rate_limit_until: Option<Instant>,
    /// 最近一次限额调整时间
    pub last_adjusted_at: Option<Instant>,
    config: AimdConfig,
    request_count: u32,
}

impl AimdController {
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

    pub fn with_defaults() -> Self {
        Self::new(AimdConfig::default())
    }

    /// 请求成功时调用。
    ///
    /// - 学习上游限额（若响应头提供且更高则采用）
    /// - Learning → Stable 转换
    /// - 连续成功达阈值后上调限额
    pub fn on_success(&mut self, upstream_limit: Option<u32>) {
        let now = Instant::now();

        if let Some(limit) = upstream_limit {
            if limit > 0 && limit <= self.config.max_limit {
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
                    tracing::info!("AIMD: 渠道进入 Stable（{} 次请求后）", self.request_count);
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

        if let Some(rate_limit_until) = self.rate_limit_until {
            if now >= rate_limit_until {
                self.rate_limit_until = None;
            }
        }
    }

    /// 429 限速错误时调用。
    ///
    /// - 限额乘 0.8（至少保 1）
    /// - 连续失败达阈值 → 进入 Cooldown
    pub fn on_rate_limited(&mut self, retry_after: Option<u64>) {
        let now = Instant::now();

        self.failure_streak += 1;
        self.success_streak = 0;

        if let Some(seconds) = retry_after {
            self.rate_limit_until = Some(now + Duration::from_secs(seconds));
        }

        let new_limit = (self.current_limit as f64 * RATE_LIMIT_REDUCTION_RATIO).ceil() as u32;
        self.current_limit = new_limit.max(1);
        self.last_adjusted_at = Some(now);

        tracing::warn!("AIMD: 429 后限额降至 {}", self.current_limit);

        if self.failure_streak >= self.config.failure_threshold {
            self.enter_cooldown(now);
        }
    }

    /// 当前是否放行请求。
    pub fn check_available(&self) -> bool {
        let now = Instant::now();

        if self.state == RateLimitState::Cooldown {
            if let Some(cooldown_until) = self.cooldown_until {
                if now < cooldown_until {
                    return false;
                }
                // 冷却到期 → 放试探请求；试探成功后 on_success 完全恢复，
                // 试探再 429 则 on_rate_limited 重新进入 Cooldown。
            }
        }

        if let Some(rate_limit_until) = self.rate_limit_until {
            if now < rate_limit_until {
                return false;
            }
        }

        true
    }

    /// 当前生效限额。
    pub fn get_current_limit(&self) -> u32 {
        self.current_limit
    }

    /// 已学习的上游限额。
    pub fn get_learned_limit(&self) -> Option<u32> {
        self.learned_limit
    }

    /// 状态机当前状态。
    pub fn get_state(&self) -> &RateLimitState {
        &self.state
    }

    /// 只读快照（调度用）。
    pub fn snapshot(&self) -> AimdSnapshot {
        AimdSnapshot {
            current_limit: self.current_limit,
            state: self.state,
        }
    }

    fn enter_cooldown(&mut self, now: Instant) {
        self.state = RateLimitState::Cooldown;
        self.cooldown_until = Some(now + self.config.cooldown_duration);
        tracing::warn!(
            "AIMD: 进入 Cooldown {}s",
            self.config.cooldown_duration.as_secs()
        );
    }

    /// 从 Cooldown 恢复：限额再缩减 recovery_ratio，状态回 Learning。
    fn recover_from_cooldown(&mut self) {
        let now = Instant::now();
        let new_limit = (self.current_limit as f64 * self.config.recovery_ratio).ceil() as u32;
        self.current_limit = new_limit.max(1);
        self.state = RateLimitState::Learning;
        self.cooldown_until = None;
        self.failure_streak = 0;
        self.success_streak = 0;
        self.last_adjusted_at = Some(now);
        tracing::info!("AIMD: Cooldown 恢复，新限额 {}", self.current_limit);
    }

    /// 尝试上调限额（不超过已学习限额与配置上限）。
    fn try_increase_limit(&mut self, now: Instant) {
        let max_allowed = self.learned_limit.unwrap_or(self.config.max_limit);
        let max_allowed = max_allowed.min(self.config.max_limit);

        if self.current_limit < max_allowed {
            let new_limit = (self.current_limit + self.config.adjustment_step).min(max_allowed);
            self.current_limit = new_limit;
            self.last_adjusted_at = Some(now);
            self.success_streak = 0;
            tracing::debug!("AIMD: 限额升至 {}", self.current_limit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state_is_learning() {
        let c = AimdController::with_defaults();
        assert_eq!(c.state, RateLimitState::Learning);
        assert_eq!(c.current_limit, 10);
        assert!(c.check_available());
    }

    #[test]
    fn learning_to_stable_transition() {
        let mut c = AimdController::new(AimdConfig {
            learning_duration: 5,
            ..Default::default()
        });
        for _ in 0..5 {
            c.on_success(None);
        }
        assert_eq!(c.state, RateLimitState::Stable);
    }

    #[test]
    fn rate_limited_reduces_limit() {
        let mut c = AimdController::with_defaults();
        let initial = c.current_limit;
        c.on_rate_limited(None);
        assert!(c.current_limit < initial);
        assert_eq!(
            c.current_limit,
            (initial as f64 * RATE_LIMIT_REDUCTION_RATIO).ceil() as u32
        );
    }

    #[test]
    fn cooldown_blocks_until_expiry() {
        let mut c = AimdController::new(AimdConfig {
            failure_threshold: 1,
            cooldown_duration: Duration::from_secs(1),
            ..Default::default()
        });
        c.on_rate_limited(None);
        assert_eq!(c.state, RateLimitState::Cooldown);
        assert!(!c.check_available());
    }

    #[test]
    fn recovery_from_cooldown() {
        let mut c = AimdController::new(AimdConfig {
            failure_threshold: 1,
            cooldown_duration: Duration::from_millis(10),
            ..Default::default()
        });
        let initial = c.current_limit;
        c.on_rate_limited(None);
        assert_eq!(c.state, RateLimitState::Cooldown);
        std::thread::sleep(Duration::from_millis(20));
        c.check_available();
        c.on_success(None);
        assert_eq!(c.state, RateLimitState::Learning);
        assert!(
            c.current_limit <= (initial as f64 * RATE_LIMIT_REDUCTION_RATIO * 0.5).ceil() as u32
        );
    }

    #[test]
    fn learns_upstream_limit() {
        let mut c = AimdController::with_defaults();
        c.on_success(Some(100));
        assert_eq!(c.learned_limit, Some(100));
        assert_eq!(c.current_limit, 100);
    }

    #[test]
    fn success_streak_increases_limit() {
        let mut c = AimdController::new(AimdConfig {
            success_threshold: 3,
            adjustment_step: 5,
            learning_duration: 100,
            ..Default::default()
        });
        let initial = c.current_limit;
        for _ in 0..3 {
            c.on_success(None);
        }
        assert_eq!(c.current_limit, initial + 5);
    }

    #[test]
    fn zero_retry_after_allows_immediately() {
        let mut c = AimdController::with_defaults();
        c.on_rate_limited(Some(0));
        assert!(c.check_available());
    }
}
