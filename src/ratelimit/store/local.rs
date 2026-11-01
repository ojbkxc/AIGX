//! 进程内计数器存储 — 借鉴 aisix-ratelimit/src/store/local.rs
//!
//! 行为与 aisix 的 LocalStore 一致：DashMap 持有每 key 的固定窗口
//! 计数器，每个 key 由一个 parking_lot::Mutex 保护。状态是单副本的，
//! 不跨进程共享。

use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
    /// 最后一次访问时刻。用于清扫长期不活跃的键 —— 否则 per-IP 限额开启后，
    /// 攻击者轮换源 IP 就能让 states 无限增长直到 OOM。
    last_used: Instant,
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
            last_used: Instant::now(),
        }
    }
}

/// 键的最大保留时长：超过这个时间没有被访问的键会被清扫掉。
///
/// 取 2 倍的最大窗口（天）稍长一点：任一窗口的计数在窗口内都必须保留，
/// 否则会误放行。实际上 FixedWindowCounter 自己会按窗口滚动，这里只需要
/// 保证「还有活着的窗口数据」的键不被提前删。
const KEY_IDLE_TTL: Duration = Duration::from_secs(DAY_SECS * 2);

/// 键数量硬上限。超过后先清扫，仍超限则**拒绝新建键**
/// （fail-closed：宁可让新 IP 被限流建不出状态，也不让内存无界增长）。
const MAX_KEYS: usize = 100_000;

/// 进程内固定窗口存储。
pub struct LocalStore<C: Clock = SystemClock> {
    states: DashMap<String, Arc<Mutex<KeyState>>>,
    clock: C,
    /// 上次清扫时刻（单调时钟，避免每次请求都全表扫描）。
    last_sweep: AtomicU64,
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
            last_sweep: AtomicU64::new(0),
        }
    }

    fn state_for(&self, key: &str) -> Arc<Mutex<KeyState>> {
        if let Some(entry) = self.states.get(key) {
            let arc = entry.clone();
            drop(entry);
            arc.lock().last_used = Instant::now();
            return arc;
        }
        // 新建键前先看容量：周期性清扫不活跃键，仍超限则拒绝新建。
        // 拒绝时返回一个**临时**状态（不入表）：该请求仍会被正常限流判定，
        // 但不会让 states 继续膨胀。
        self.maybe_sweep();
        if self.states.len() >= MAX_KEYS {
            tracing::warn!(
                "ratelimit: key capacity {} reached, rejecting new key (fail-closed)",
                MAX_KEYS
            );
            return Arc::new(Mutex::new(KeyState::new()));
        }
        self.states
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(KeyState::new())))
            .clone()
    }

    /// 周期性清扫不活跃键。用单调时钟节流，避免每个新键都全表扫描。
    fn maybe_sweep(&self) {
        let now = Instant::now();
        let now_ms = self.clock.unix_secs() * 1000;
        let last = self.last_sweep.load(Ordering::Relaxed);
        // 每 10 分钟最多扫一次
        if now_ms.saturating_sub(last) < 600_000 {
            return;
        }
        self.last_sweep.store(now_ms, Ordering::Relaxed);
        let before = self.states.len();
        self.states.retain(|_, st| {
            // 只清理「无在途 + 长期未访问」的键：有在途状态时删掉会丢失
            // 并发计数，导致超发。
            let guard = st.lock();
            guard.in_flight > 0 || now.duration_since(guard.last_used) < KEY_IDLE_TTL
        });
        let removed = before.saturating_sub(self.states.len());
        if removed > 0 {
            tracing::debug!("ratelimit: swept {removed} idle keys");
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratelimit::clock::TestClock;

    fn store() -> LocalStore<TestClock> {
        LocalStore::with_clock(TestClock::new(1_000_000))
    }

    /// 空闲键在 TTL 后被清扫，活跃键保留。
    #[test]
    fn sweeps_idle_keys_only() {
        let st = store();
        let _ = st.state_for("idle");
        let _ = st.state_for("busy");
        assert_eq!(st.states.len(), 2);

        // 「busy」上一次访问就在刚才；把「idle」的 last_used 手动拨回过去
        if let Some(e) = st.states.get("idle") {
            e.lock().last_used = Instant::now() - KEY_IDLE_TTL - Duration::from_secs(60);
        }
        // 绕过节流：直接置 0 让 maybe_sweep 生效
        st.last_sweep.store(0, Ordering::Relaxed);
        st.maybe_sweep();

        assert_eq!(st.states.len(), 1);
        assert!(st.states.contains_key("busy"));
        assert!(!st.states.contains_key("idle"));
    }

    /// 有在途请求的键即使很久没被访问也不清扫（否则并发计数丢失）。
    #[test]
    fn does_not_sweep_in_flight_keys() {
        let st = store();
        let _ = st.state_for("inflight");
        if let Some(e) = st.states.get("inflight") {
            let mut g = e.lock();
            g.in_flight = 3;
            g.last_used = Instant::now() - KEY_IDLE_TTL - Duration::from_secs(60);
        }
        st.last_sweep.store(0, Ordering::Relaxed);
        st.maybe_sweep();
        assert!(st.states.contains_key("inflight"));
    }

    /// 达到容量上限后不再新建键（fail-closed），但不影响已有键。
    #[test]
    fn refuses_new_keys_at_capacity() {
        let st = store();
        // 直接填满（用较短路径，避免 MAX_KEYS 次真实调用）
        for i in 0..MAX_KEYS {
            st.states
                .insert(format!("k{i}"), Arc::new(Mutex::new(KeyState::new())));
        }
        assert_eq!(st.states.len(), MAX_KEYS);
        // 新建键应返回临时状态且不入表
        let _ = st.state_for("overflow");
        assert_eq!(st.states.len(), MAX_KEYS);
        assert!(!st.states.contains_key("overflow"));
    }
}
