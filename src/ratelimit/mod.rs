//! 限流模块 — 借鉴 aisix-ratelimit
//!
//! 提供两阶段 RPM/TPM/并发限流器。
//! 请求中间件在分发前调用 `Limiter::pre_commit()`；
//! 返回的 `Reservation` 在上游响应完成后通过 `Reservation::commit_tokens()` 完成。
//!
//! 限制配置来自 `RateLimit` 结构。RPM/RPD 预先检查并递增，
//! 使突发流量快速失败；TPM/TPD 预先只检查，在 commit 时递增，
//! 因为 token 消耗只有在上游响应返回后才能确定。

pub mod clock;
pub mod error;
pub mod limiter;
pub mod store;
pub mod window;

pub use clock::{Clock, SystemClock, TestClock};
pub use error::{RateLimitError, RateLimitScope};
pub use limiter::{Limiter, RateLimitStatus, Reservation};
pub use store::local::LocalStore;
pub use store::{RateLimit, RateStore};
pub use window::{FixedWindowCounter, WindowCheck};