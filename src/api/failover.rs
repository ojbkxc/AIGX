//! failover 的总预算与候选数上限（连接稳定性）。
//!
//! 数据面 failover 循环会按优先级依次尝试候选渠道；每个流式尝试最多等
//! `sse_first_event_deadline()`（默认 30s）才判上游挂起。候选多且都挂时，
//! 单个用户请求会在多个渠道上串行挂住数分钟，客户端体感为「卡死」。
//!
//! 本模块给出统一的判据：**最多尝试 N 个候选** + **总时间预算**。
//! Anthropic（`api/anthropic.rs`）与 OpenAI（`api/openai/chat.rs`、
//! `api/openai/responses.rs`）两侧 failover 循环共用，口径一致。

use std::time::{Duration, Instant};

/// 单次请求的 failover 最多尝试几个候选渠道（含第一个）。
///
/// 只尝试按 priority/weight 排序后的前 N 个；其余候选本轮不尝试。
/// 取 5：覆盖多渠道路由的常见冗余度，同时把最坏等待收敛到 5 个首事件窗口内。
pub const FAILOVER_MAX_ATTEMPTS: usize = 5;

/// failover 循环的总时间预算（从循环开始计）的**地板值**。
///
/// 取 90s：覆盖「前几个候选各撞一次首事件超时」的常见情况，又不至于让单个
/// 请求无限期挂住。注意预算只约束「尝试新候选」；已经建立的流不受影响
/// （流式请求的读空闲与 24h 上限由 bridge 层负责）。
///
/// **它不是实际预算**：实际预算见 [`default_total_budget`]——首个候选的等待
/// 窗口是可配的（`sse_first_event_deadline`，默认 30s），固定 90s 会在
/// 「5 个候选 × 30s = 150s」的情况下于第 3 个候选后提前截断，让
/// [`FAILOVER_MAX_ATTEMPTS`] 这个「5」名不副实。
pub const FAILOVER_TOTAL_BUDGET: Duration = Duration::from_secs(90);

/// 实际总预算：`max(地板 90s, 候选上限 × 当前首事件窗口)`。
///
/// 两个上限（候选数 / 总时长）必须**同源**，否则会互相掩盖：配置把首事件
/// 窗口调到 60s 时，固定 90s 预算只够试 1 个候选就耗尽，用户在面板上改的
/// 「最多尝试 5 个」不可能生效，而且现场只看到「预算耗尽」，看不出是被谁截断。
pub fn default_total_budget() -> Duration {
    let per_attempt = crate::bridge::sse_first_event_deadline();
    let needed = per_attempt.saturating_mul(FAILOVER_MAX_ATTEMPTS as u32);
    FAILOVER_TOTAL_BUDGET.max(needed)
}

/// failover 预算状态：记录循环起点与已尝试次数。
#[derive(Debug, Clone)]
pub struct FailoverBudget {
    started: Instant,
    attempts: usize,
    max_attempts: usize,
    total_budget: Duration,
}

impl FailoverBudget {
    /// 用默认口径（[`FAILOVER_MAX_ATTEMPTS`] / [`default_total_budget`]）新建。
    pub fn new() -> Self {
        Self::with_limits(FAILOVER_MAX_ATTEMPTS, default_total_budget())
    }

    /// 显式指定上限（单测 / 未来可配置化用）。
    pub fn with_limits(max_attempts: usize, total_budget: Duration) -> Self {
        Self {
            started: Instant::now(),
            attempts: 0,
            max_attempts: max_attempts.max(1),
            total_budget,
        }
    }

    /// 是否还允许再尝试一个候选渠道。
    ///
    /// 调用方在进入每次候选尝试前判断；返回 false 时应停止 failover，
    /// 把 `last_error` 按既有失败路径返回。
    pub fn can_attempt(&self) -> bool {
        self.attempts < self.max_attempts && self.started.elapsed() < self.total_budget
    }

    /// 记录一次候选尝试，返回记录后是否还能继续（false = 已达上限）。
    pub fn record_attempt(&mut self) -> bool {
        self.attempts += 1;
        self.can_attempt()
    }

    /// 已尝试的候选数。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn attempts(&self) -> usize {
        self.attempts
    }

    /// 预算剩余时间（耗尽后为 0）。
    pub fn remaining(&self) -> Duration {
        self.total_budget.saturating_sub(self.started.elapsed())
    }

    /// 因预算停止时给日志/错误用的一行说明。
    pub fn exhausted_reason(&self) -> String {
        format!(
            "failover budget exhausted after {} attempt(s) (max {} / {}ms)",
            self.attempts,
            self.max_attempts,
            self.total_budget.as_millis()
        )
    }
}

impl Default for FailoverBudget {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_allow_expected_attempts() {
        let budget = FailoverBudget::new();
        assert!(budget.can_attempt());
        assert_eq!(budget.attempts(), 0);
    }

    /// 两个上限必须同源：默认预算至少够把「候选上限」次尝试走完。
    #[test]
    fn default_budget_covers_every_candidate_attempt() {
        let per_attempt = crate::bridge::sse_first_event_deadline();
        let needed = per_attempt.saturating_mul(FAILOVER_MAX_ATTEMPTS as u32);
        // 注意：format 字符串的参数捕获只支持标识符/路径，函数调用必须先在
        // 语句里求值（`{default_total_budget():?}` 是编译错误）。
        let budget = default_total_budget();
        assert!(
            budget >= needed,
            "预算 {budget:?} 必须 ≥ {FAILOVER_MAX_ATTEMPTS} × {per_attempt:?}",
        );
        assert!(
            default_total_budget() >= FAILOVER_TOTAL_BUDGET,
            "预算不得低于 90s 地板",
        );
    }

    #[test]
    fn stops_after_max_attempts() {
        let mut budget = FailoverBudget::with_limits(2, Duration::from_secs(3600));
        assert!(budget.can_attempt());
        assert!(budget.record_attempt(), "第 1 次尝试后仍可继续");
        assert!(budget.can_attempt(), "第 2 次尝试前仍可");
        assert!(!budget.record_attempt(), "第 2 次后达到上限");
        assert!(!budget.can_attempt(), "达到候选上限后不得再尝试");
        assert_eq!(budget.attempts(), 2);
    }

    #[test]
    fn stops_when_total_budget_elapsed() {
        // 预算 0：首次判断即耗尽。
        let budget = FailoverBudget::with_limits(5, Duration::from_millis(0));
        assert!(!budget.can_attempt());
        assert_eq!(budget.remaining(), Duration::ZERO);
    }

    #[test]
    fn max_attempts_is_at_least_one() {
        let budget = FailoverBudget::with_limits(0, Duration::from_secs(1));
        assert!(budget.can_attempt(), "上限 0 按 1 处理，至少尝试一个候选");
    }

    #[test]
    fn exhausted_reason_mentions_limits() {
        let budget = FailoverBudget::with_limits(3, Duration::from_secs(1));
        let reason = budget.exhausted_reason();
        assert!(reason.contains("max 3"), "{reason}");
    }
}
