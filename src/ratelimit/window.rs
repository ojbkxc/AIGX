//! 固定窗口计数器 — 借鉴 aisix-ratelimit/src/window.rs
//!
//! 每分钟/每天等窗口维度各持一个计数器,窗口过期自动重置。
//! 不是线程安全的 — 调用方需持有外部锁。

/// 窗口检查结果
#[derive(Debug, PartialEq, Eq)]
pub enum WindowCheck {
    Ok,
    Full { retry_after_secs: u64 },
}

#[derive(Debug)]
pub struct FixedWindowCounter {
    window_secs: u64,
    window_start: u64,
    count: u64,
}

impl FixedWindowCounter {
    pub fn new(window_secs: u64) -> Self {
        assert!(window_secs > 0, "window_secs must be positive");
        Self {
            window_secs,
            window_start: 0,
            count: 0,
        }
    }

    fn roll_if_stale(&mut self, now_secs: u64) {
        let bucket_start = (now_secs / self.window_secs) * self.window_secs;
        if bucket_start != self.window_start {
            self.window_start = bucket_start;
            self.count = 0;
        }
    }

    /// 检查并递增:如果 delta 放入后不超过 limit,则递增并返回 Ok。
    pub fn check_and_increment(&mut self, now_secs: u64, delta: u64, limit: u64) -> WindowCheck {
        self.roll_if_stale(now_secs);
        let would_be = self.count.saturating_add(delta);
        if would_be > limit {
            let remainder = self
                .window_secs
                .saturating_sub(now_secs.saturating_sub(self.window_start));
            return WindowCheck::Full {
                retry_after_secs: remainder.max(1),
            };
        }
        self.count = would_be;
        WindowCheck::Ok
    }

    /// 事后追加(不检查上限),用于 TPM/TPD 事后扣费。
    pub fn add(&mut self, now_secs: u64, delta: u64) {
        self.roll_if_stale(now_secs);
        self.count = self.count.saturating_add(delta);
    }

    /// 仅检查是否已超限,不递增。用于 TPM 预检。
    pub fn is_exceeded(&mut self, now_secs: u64, limit: u64) -> Option<u64> {
        self.roll_if_stale(now_secs);
        if self.count > limit {
            let remainder = self
                .window_secs
                .saturating_sub(now_secs.saturating_sub(self.window_start));
            Some(remainder.max(1))
        } else {
            None
        }
    }

    /// 回滚计数器（用于分层限流补偿）
    pub fn decrement(&mut self, now_secs: u64, delta: u64) {
        self.roll_if_stale(now_secs);
        self.count = self.count.saturating_sub(delta);
    }

    pub fn current(&mut self, now_secs: u64) -> u64 {
        self.roll_if_stale(now_secs);
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_increments_fit_then_subsequent_block() {
        let mut w = FixedWindowCounter::new(60);
        assert_eq!(w.check_and_increment(100, 1, 3), WindowCheck::Ok);
        assert_eq!(w.check_and_increment(100, 1, 3), WindowCheck::Ok);
        assert_eq!(w.check_and_increment(100, 1, 3), WindowCheck::Ok);
        match w.check_and_increment(100, 1, 3) {
            WindowCheck::Full { retry_after_secs } => {
                assert!(retry_after_secs > 0 && retry_after_secs <= 60);
            }
            _ => panic!("expected Full"),
        }
    }

    #[test]
    fn counter_rolls_over_at_window_boundary() {
        let mut w = FixedWindowCounter::new(60);
        for _ in 0..3 {
            w.check_and_increment(100, 1, 3);
        }
        assert_eq!(w.check_and_increment(161, 1, 3), WindowCheck::Ok);
        assert_eq!(w.current(161), 1);
    }

    #[test]
    fn add_records_post_deduct_usage() {
        let mut w = FixedWindowCounter::new(60);
        w.add(100, 1_000);
        w.add(101, 500);
        assert_eq!(w.current(101), 1_500);
        assert!(w.is_exceeded(101, 2_000).is_none());
        assert!(w.is_exceeded(101, 1_000).is_some());
    }
}
