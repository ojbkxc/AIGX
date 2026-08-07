//! 可插拔计数器后端 — 借鉴 aisix-ratelimit/src/store/mod.rs
//!
//! Limiter 只关心桶（一个 opaque key）和 RateLimit；计数器实际存在哪里
//! 由 RateStore 决定。
//!
//! - `local::LocalStore`：进程内内存计数器（DashMap + 固定窗口），默认后端。
//!
//! 两阶段对应 Limiter 的契约：
//! - **acquire**（请求路径，异步）：并发门控 + token 预检 + 请求计数递增
//! - **commit**（请求成功，异步）：事后扣 token + 释放并发槽位
//! - **release** / **add_tokens**（事后，同步）：Drop 时释放并发槽位，
//!   流式 SSE 完成后的 token 追加

use async_trait::async_trait;

use crate::ratelimit::error::RateLimitError;
use crate::ratelimit::limiter::RateLimitStatus;

pub mod local;

/// 窗口常量
pub(crate) const SECOND_SECS: u64 = 1;
pub(crate) const MINUTE_SECS: u64 = 60;
pub(crate) const HOUR_SECS: u64 = 60 * 60;
pub(crate) const DAY_SECS: u64 = 24 * 60 * 60;

/// 限流配置（简化版，参考 aisix_core::RateLimit）
#[derive(Debug, Clone, Default)]
pub struct RateLimit {
    /// 每秒请求数
    pub rps: Option<u64>,
    /// 每分钟请求数
    pub rpm: Option<u64>,
    /// 每小时请求数
    pub rph: Option<u64>,
    /// 每天请求数
    pub rpd: Option<u64>,
    /// 每分钟 token 数
    pub tpm: Option<u64>,
    /// 每天 token 数
    pub tpd: Option<u64>,
    /// 最大并发数
    pub concurrency: Option<u32>,
}

/// 持有桶计数器的后端。
///
/// `member` 是进程唯一的预留 ID，用于分布式后端追踪并发集合中的
/// 精确一个槽位；本地后端忽略它（in_flight 是普通计数器）。
#[async_trait]
pub trait RateStore: Send + Sync + 'static {
    /// 预提交获取单个桶。原子地（每桶）：
    /// 门控并发、检查（但不递增）token 窗口、然后检查并递增每个请求窗口。
    /// 全有或全无：拒绝时什么都不递增，并发槽位也不占用。
    async fn acquire(
        &self,
        key: &str,
        limits: &RateLimit,
        member: &str,
    ) -> Result<(), RateLimitError>;

    /// 事后扣费：将 `tokens` 加到 tpm/tpd 窗口并释放并发槽位。
    async fn commit(&self, key: &str, tokens: u64, member: &str);

    /// 释放并发槽位（不记录 token）。同步，可从 Drop 运行。
    fn release(&self, key: &str, member: &str);

    /// 流式事后 token 记账：只加 tpm/tpd（不改变并发）。同步，
    /// 可从同步 SSE 完成回调运行。
    fn add_tokens(&self, key: &str, tokens: u64);

    /// 只读快照，用于 x-ratelimit-* 响应头。返回 None 表示无有意义数据。
    async fn peek(&self, key: &str, limits: &RateLimit) -> Option<RateLimitStatus>;
}