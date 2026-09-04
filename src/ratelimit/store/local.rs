//! 进程内计数器存储 — 借鉴 aisix-ratelimit/src/store/local.rs
//!
//! 行为与 aisix 的 LocalStore 一致：DashMap 持有每 key 的固定窗口
//! 计数器，每个 key 由一个 parking_lot::Mutex 保护。状态是单副本的，
//! 不跨进程共享。

use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::Arc;

use super::{RateStore, DAY_SECS, HOUR_SECS, MINUTE_SECS, SECOND_SECS};
use crate::ratelimit::clock::{Clock, SystemClock};
use crate::ratelimit::error::RateLimitError;
use crate::ratelimit::limiter::RateLimitStatus;
use crate::ratelimit::window::{FixedWindowCounter, WindowCheck};

use super::RateLimit;

/// 每 key 状态，由单个 mutex 保护。热路径每请求锁一次，每次操作 O(1)。
#[derive(Debug)]
struct KeyState {
    rps: FixedWindowCounter,
    rpm: FixedWindowCounter,
    rph: FixedWindowCounter,
    rpd: FixedWindowCounter,
    tpm: FixedWindowCounter,
    tpd: FixedWindowCounter,
    in_flight: u32,
}

impl KeyState {
    fn new() -> Self {
        Self {
            rps: FixedWindowCounter::new(SECOND_SECS),
            rpm: FixedWindowCounter::new(MINUTE_SECS),
            rph: FixedWindowCounter::new(HOUR_SECS),
            rpd: FixedWindowCounter::new(DAY_SECS),
            tpm: FixedWindowCounter::new(MINUTE_SECS),
            tpd: FixedWindowCounter::new(DAY_SECS),
            in_flight: 0,
        }
    }
}

/// 进程内固定窗口存储。
pub struct LocalStore<C: Clock = SystemClock> {
    states: DashMap<String, Arc<Mutex<KeyState>>>,
    clock: C,
}

impl LocalStore<SystemClock> {
    pub fn new() -> Self {
        Self::with_clock(SystemClock)
    }
}

impl Default for LocalStore<SystemClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C: Clock> LocalStore<C> {
    pub fn with_clock(clock: C) -> Self {
        Self {
            states: DashMap::new(),
            clock,
        }
    }

    fn state_for(&self, key: &str) -> Arc<Mutex<KeyState>> {
        if let Some(entry) = self.states.get(key) {
            return entry.clone();
        }
        self.states
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(KeyState::new())))
            .clone()
    }
}

#[async_trait]
impl<C: Clock> RateStore for LocalStore<C> {
    async fn acquire(
        &self,
        key: &str,
        limits: &RateLimit,
        _member: &str,
    ) -> Result<(), RateLimitError> {
        let now = self.clock.unix_secs();
        let state = self.state_for(key);
        let mut s = state.lock();

        // 并发优先 — 最便宜且从不消耗窗口槽位。
        if let Some(max) = limits.concurrency {
            if s.in_flight >= max {
                return Err(RateLimitError::Concurrency);
            }
        }

        // Token 限制 — 只检查不递增。如果上一分钟/天已经超限，拒绝新请求。
        if let Some(max) = limits.tpm {
            if let Some(retry) = s.tpm.is_exceeded(now, max) {
                return Err(RateLimitError::Tokens {
                    scope: crate::ratelimit::error::RateLimitScope::Tokens,
                    retry_after_secs: retry,
                });
            }
        }
        if let Some(max) = limits.tpd {
            if let Some(retry) = s.tpd.is_exceeded(now, max) {
                return Err(RateLimitError::Tokens {
                    scope: crate::ratelimit::error::RateLimitScope::Tokens,
                    retry_after_secs: retry,
                });
            }
        }

        // 请求限制 — 检查并递增。分层链（rps → rpm → rph → rpd），
        // 更紧的窗口短路更松的窗口，不消耗其槽位。如果后续层拒绝，
        // 之前递增的计数器全部回滚。
        let mut rps_incremented = false;
        if let Some(max) = limits.rps {
            if let WindowCheck::Full { retry_after_secs } = s.rps.check_and_increment(now, 1, max) {
                return Err(RateLimitError::Requests {
                    scope: crate::ratelimit::error::RateLimitScope::Requests,
                    retry_after_secs,
                });
            }
            rps_incremented = true;
        }
        let mut rpm_incremented = false;
        if let Some(max) = limits.rpm {
            if let WindowCheck::Full { retry_after_secs } = s.rpm.check_and_increment(now, 1, max) {
                if rps_incremented {
                    s.rps.decrement(now, 1);
                }
                return Err(RateLimitError::Requests {
                    scope: crate::ratelimit::error::RateLimitScope::Requests,
                    retry_after_secs,
                });
            }
            rpm_incremented = true;
        }
        let mut rph_incremented = false;
        if let Some(max) = limits.rph {
            if let WindowCheck::Full { retry_after_secs } = s.rph.check_and_increment(now, 1, max) {
                if rpm_incremented {
                    s.rpm.decrement(now, 1);
                }
                if rps_incremented {
                    s.rps.decrement(now, 1);
                }
                return Err(RateLimitError::Requests {
                    scope: crate::ratelimit::error::RateLimitScope::Requests,
                    retry_after_secs,
                });
            }
            rph_incremented = true;
        }
        if let Some(max) = limits.rpd {
            if let WindowCheck::Full { retry_after_secs } = s.rpd.check_and_increment(now, 1, max) {
                if rph_incremented {
                    s.rph.decrement(now, 1);
                }
                if rpm_incremented {
                    s.rpm.decrement(now, 1);
                }
                if rps_incremented {
                    s.rps.decrement(now, 1);
                }
                return Err(RateLimitError::Requests {
                    scope: crate::ratelimit::error::RateLimitScope::Requests,
                    retry_after_secs,
                });
            }
        }

        s.in_flight += 1;
        Ok(())
    }

    async fn commit(&self, key: &str, tokens: u64, _member: &str) {
        let now = self.clock.unix_secs();
        let state = self.state_for(key);
        let mut s = state.lock();
        s.tpm.add(now, tokens);
        s.tpd.add(now, tokens);
        s.in_flight = s.in_flight.saturating_sub(1);
    }

    fn release(&self, key: &str, _member: &str) {
        // 非插入式：对从未 acquire 的桶释放是空操作，避免 happy path 上
        // 的清理操作污染本地 map。
        if let Some(state) = self.states.get(key) {
            let mut s = state.lock();
            s.in_flight = s.in_flight.saturating_sub(1);
        }
    }

    fn add_tokens(&self, key: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        let now = self.clock.unix_secs();
        let state = self.state_for(key);
        let mut s = state.lock();
        s.tpm.add(now, tokens);
        s.tpd.add(now, tokens);
    }

    async fn peek(&self, key: &str, limits: &RateLimit) -> Option<RateLimitStatus> {
        let now = self.clock.unix_secs();
        let state = self.states.get(key)?;
        let mut s = state.lock();

        let rpm_used = s.rpm.current(now);
        let tpm_used = s.tpm.current(now);
        let in_flight = s.in_flight;

        // 当前分钟窗口剩余秒数
        let minute_reset = MINUTE_SECS - (now % MINUTE_SECS);

        Some(RateLimitStatus {
            rpm_limit: limits.rpm,
            rpm_used,
            rpm_reset_secs: minute_reset,
            tpm_limit: limits.tpm,
            tpm_used,
            tpm_reset_secs: minute_reset,
            concurrency_limit: limits.concurrency,
            in_flight,
        })
    }
}
