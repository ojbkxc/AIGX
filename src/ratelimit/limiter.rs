//! 两阶段限流器 — 借鉴 aisix-ratelimit/src/limiter.rs
//!
//! 阶段 1 — **预提交**，在上游请求发出前调用：
//! - 检查并发（获取槽位或失败）
//! - 检查并递增 RPS/RPM/RPH/RPD 计数器
//! - *仅检查* TPM/TPD（此时还不知道 token 消耗）
//!
//! 阶段 2 — **事后扣费**，在上游响应完成后调用：
//! - 将实际 token 数加到 TPM/TPD
//! - 释放并发槽位
//!
//! 返回的 Reservation 句柄在未调用 commit_tokens 就 drop 时自动释放
//! 并发槽位，确保错误路径不会泄漏。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::ratelimit::clock::Clock;
use crate::ratelimit::error::RateLimitError;
use crate::ratelimit::store::local::LocalStore;
use crate::ratelimit::store::{RateLimit, RateStore};

/// 单个 key 的当前窗口状态，由 Limiter::peek 返回。
/// 用于注入 x-ratelimit-* 响应头。
#[derive(Debug, Clone)]
pub struct RateLimitStatus {
    pub rpm_limit: Option<u64>,
    pub rpm_used: u64,
    pub rpm_reset_secs: u64,
    pub tpm_limit: Option<u64>,
    pub tpm_used: u64,
    pub tpm_reset_secs: u64,
    pub concurrency_limit: Option<u32>,
    pub in_flight: u32,
}

impl RateLimitStatus {
    pub fn rpm_remaining(&self) -> Option<u64> {
        self.rpm_limit.map(|lim| lim.saturating_sub(self.rpm_used))
    }
    pub fn tpm_remaining(&self) -> Option<u64> {
        self.tpm_limit.map(|lim| lim.saturating_sub(self.tpm_used))
    }
}

/// 两阶段限流器，基于共享或本地 RateStore。
pub struct Limiter {
    store: Arc<dyn RateStore>,
    /// 进程唯一预留 ID 前缀（`<uuid>:`），使并发 member 在多副本的
    /// 共享存储中全局唯一。
    member_prefix: String,
    seq: AtomicU64,
}

impl Limiter {
    /// 默认进程内限流器（内存 LocalStore）。
    pub fn new() -> Self {
        Self::with_store(Arc::new(LocalStore::new()))
    }

    /// 基于特定存储构建。
    pub fn with_store(store: Arc<dyn RateStore>) -> Self {
        Self {
            store,
            member_prefix: format!("{}:", uuid::Uuid::new_v4().simple()),
            seq: AtomicU64::new(0),
        }
    }

    /// 测试辅助：由可注入时钟驱动的本地存储。
    pub fn local_with_clock<C: Clock>(clock: C) -> Self {
        Self::with_store(Arc::new(LocalStore::with_clock(clock)))
    }

    fn next_member(&self) -> String {
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        format!("{}{n}", self.member_prefix)
    }

    /// 预提交阶段。返回 Reservation，必须通过 commit_tokens 完成
    /// 或 drop 以自动释放并发槽位。
    pub async fn pre_commit(
        &self,
        key: &str,
        limits: &RateLimit,
    ) -> Result<Reservation, RateLimitError> {
        let member = self.next_member();
        self.store.acquire(key, limits, &member).await?;
        Ok(Reservation {
            store: Arc::clone(&self.store),
            key: key.to_string(),
            member,
            committed: false,
        })
    }

    /// 流式路径事后 token 记账：不经过 Reservation，直接加到 tpm/tpd。
    /// 当 tokens 为 0 时是空操作。
    pub fn add_tokens_post_stream(&self, key: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        self.store.add_tokens(key, tokens);
    }

    /// 某 key 当前限流状态快照，用于注入 x-ratelimit-* 响应头。
    /// 返回 None 表示无有意义数据。只读 — 不影响任何计数器。
    pub async fn peek(&self, key: &str, limits: &RateLimit) -> Option<RateLimitStatus> {
        self.store.peek(key, limits).await
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

/// 预留句柄。不调用 commit_tokens 就 drop 是安全的 — 并发槽位会释放，
/// 只是不记录 token。
///
/// Clone 语义：克隆体与原件恰好一方 commit（记账+不释放槽位），
/// 另一方 Drop 时释放并发槽位；由调用方保证 commit 只发生一次。
#[derive(Clone)]
pub struct Reservation {
    store: Arc<dyn RateStore>,
    key: String,
    member: String,
    committed: bool,
}

impl std::fmt::Debug for Reservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reservation")
            .field("key", &self.key)
            .field("committed", &self.committed)
            .finish()
    }
}

impl Reservation {
    /// 事后扣费阶段。记录实际 token 消耗到 TPM/TPD 并释放并发槽位。
    pub async fn commit_tokens(mut self, tokens: u64) {
        self.store.commit(&self.key, tokens, &self.member).await;
        self.committed = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        self.store.release(&self.key, &self.member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratelimit::clock::TestClock;

    fn limits(rpm: Option<u64>, tpm: Option<u64>, concurrency: Option<u32>) -> RateLimit {
        RateLimit {
            rps: None,
            rpm,
            rph: None,
            rpd: None,
            tpm,
            tpd: None,
            concurrency,
        }
    }

    #[tokio::test]
    async fn rpm_caps_request_count_in_window() {
        let clock = TestClock::new(100);
        let limiter = Limiter::local_with_clock(clock.clone());
        let l = limits(Some(2), None, None);

        let _r1 = limiter.pre_commit("k1", &l).await.unwrap();
        let _r2 = limiter.pre_commit("k1", &l).await.unwrap();
        let err = limiter.pre_commit("k1", &l).await.unwrap_err();
        match err {
            RateLimitError::Requests {
                retry_after_secs, ..
            } => {
                assert!(retry_after_secs > 0);
            }
            other => panic!("expected Requests, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rpm_resets_after_window_rollover() {
        let clock = TestClock::new(100);
        let limiter = Limiter::local_with_clock(clock.clone());
        let l = limits(Some(1), None, None);

        let _r1 = limiter.pre_commit("k1", &l).await.unwrap();
        assert!(limiter.pre_commit("k1", &l).await.is_err());

        // 跳到分钟边界之后
        clock.advance(61);
        let _r2 = limiter.pre_commit("k1", &l).await.unwrap();
    }

    #[tokio::test]
    async fn concurrency_limit_blocks_new_reservations() {
        let clock = TestClock::new(0);
        let limiter = Limiter::local_with_clock(clock.clone());
        let l = limits(None, None, Some(2));

        let r1 = limiter.pre_commit("k1", &l).await.unwrap();
        let r2 = limiter.pre_commit("k1", &l).await.unwrap();
        assert!(matches!(
            limiter.pre_commit("k1", &l).await.unwrap_err(),
            RateLimitError::Concurrency,
        ));

        drop(r1);
        drop(r2);
    }

    #[tokio::test]
    async fn reservation_drop_releases_concurrency() {
        let clock = TestClock::new(0);
        let limiter = Limiter::local_with_clock(clock.clone());
        let l = limits(None, None, Some(1));

        let r = limiter.pre_commit("k1", &l).await.unwrap();
        drop(r);
        let _r2 = limiter.pre_commit("k1", &l).await.unwrap();
    }
}
