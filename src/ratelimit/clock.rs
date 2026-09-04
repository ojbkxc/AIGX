//! 可注入时钟 — 借鉴 aisix-ratelimit/src/clock.rs
//!
//! 固定窗口计数器只需要秒级精度。生产环境用 SystemClock 委托给
//! SystemTime::now(); 测试环境用 TestClock 手动推进。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// 秒级时钟 trait。固定窗口计数器按分钟/天边界分桶，只需要秒级精度。
pub trait Clock: Send + Sync + 'static {
    fn unix_secs(&self) -> u64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

/// 测试用时钟。调用 advance() 在操作之间跳转，无需等待真实时间流逝。
#[derive(Debug, Clone, Default)]
pub struct TestClock {
    now: Arc<AtomicU64>,
}

impl TestClock {
    pub fn new(initial_secs: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(initial_secs)),
        }
    }

    pub fn advance(&self, secs: u64) {
        self.now.fetch_add(secs, Ordering::SeqCst);
    }

    pub fn set(&self, secs: u64) {
        self.now.store(secs, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn unix_secs(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_positive_now() {
        assert!(SystemClock.unix_secs() > 0);
    }

    #[test]
    fn test_clock_advances_and_sets() {
        let c = TestClock::new(100);
        assert_eq!(c.unix_secs(), 100);
        c.advance(30);
        assert_eq!(c.unix_secs(), 130);
        c.set(500);
        assert_eq!(c.unix_secs(), 500);
    }
}
