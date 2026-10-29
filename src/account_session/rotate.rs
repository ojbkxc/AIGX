//! 账号轮换策略 —— 冷却时长与尝试次数。
//!
//! 失败分类**复用** `ChannelStore::classify_bridge_error`（把 `BridgeError`
//! 映射为 `circuit_breaker::FailureType`），本模块只负责「分类 → 冷却秒数」。
//! 这样渠道级熔断与账号级冷却对同一上游错误保持一致的语义。

use crate::channel::circuit_breaker::FailureType;

/// 默认基础冷却（秒）。
const DEFAULT_BASE_COOLDOWN_SECS: u64 = 60;
/// 默认冷却上限（秒）。
const DEFAULT_MAX_COOLDOWN_SECS: u64 = 1800;
/// 认证失败的冷却（秒）：密钥失效需人工处理，冷却窗口给管理员留观察期。
const DEFAULT_AUTH_COOLDOWN_SECS: u64 = 900;
/// 指数退避的指数上限（2^5 = 32 倍基础值）。
const MAX_BACKOFF_EXPONENT: u32 = 5;
/// 单次请求最多的账号尝试次数。
const DEFAULT_MAX_ATTEMPTS: usize = 5;

/// 账号级轮换参数。
#[derive(Debug, Clone, Copy)]
pub struct RotationPolicy {
    /// 基础冷却（秒）。
    pub base_cooldown_secs: u64,
    /// 冷却上限（秒）。
    pub max_cooldown_secs: u64,
    /// 认证失败冷却（秒）。
    pub auth_cooldown_secs: u64,
    /// 单次请求最多尝试几个账号。
    pub max_attempts: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            base_cooldown_secs: DEFAULT_BASE_COOLDOWN_SECS,
            max_cooldown_secs: DEFAULT_MAX_COOLDOWN_SECS,
            auth_cooldown_secs: DEFAULT_AUTH_COOLDOWN_SECS,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
        }
    }
}

/// 计算某次失败在该账号上触发的冷却秒数。
///
/// - 认证失败 / 余额不足：长冷却（需人工介入）
/// - 限流：优先听上游 `Retry-After`
/// - 其余（5xx / 超时 / 连接失败 / 空响应）：按连续错误次数指数退避
/// - 模型不存在：短冷却（换账号大概率同样无该模型，但保留恢复机会）
pub fn cooldown_secs(
    policy: &RotationPolicy,
    failure: &FailureType,
    consecutive_errors: u64,
    retry_after: Option<u64>,
) -> u64 {
    let base = policy.base_cooldown_secs.max(1);
    let cap = policy.max_cooldown_secs.max(base);
    let raw = match failure {
        FailureType::AuthFailed => policy.auth_cooldown_secs.max(base),
        FailureType::PaymentRequired => cap,
        FailureType::RateLimited {
            retry_after: header_hint,
            ..
        } => retry_after.or(*header_hint).unwrap_or(base).max(base),
        FailureType::ModelNotFound => base,
        FailureType::ServerError
        | FailureType::Timeout
        | FailureType::ConnectionError
        | FailureType::EmptyResponse => {
            let exponent = consecutive_errors.min(u64::from(MAX_BACKOFF_EXPONENT));
            base.saturating_mul(1u64 << exponent)
        }
    };
    raw.clamp(base, cap)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RotationPolicy {
        RotationPolicy::default()
    }

    #[test]
    fn rate_limit_prefers_upstream_retry_after() {
        let p = policy();
        let failure = FailureType::RateLimited {
            scope: crate::channel::circuit_breaker::RateLimitScope::Unknown,
            retry_after: None,
        };
        assert_eq!(cooldown_secs(&p, &failure, 1, Some(120)), 120);
        // 无 Retry-After 时回落到基础冷却
        assert_eq!(cooldown_secs(&p, &failure, 1, None), p.base_cooldown_secs);
    }

    #[test]
    fn server_errors_backoff_exponentially_and_stay_capped() {
        let p = policy();
        let first = cooldown_secs(&p, &FailureType::ServerError, 1, None);
        let third = cooldown_secs(&p, &FailureType::ServerError, 3, None);
        assert!(third > first);
        assert_eq!(
            cooldown_secs(&p, &FailureType::ServerError, 99, None),
            p.max_cooldown_secs
        );
    }

    #[test]
    fn auth_failure_uses_dedicated_window() {
        let p = policy();
        assert_eq!(
            cooldown_secs(&p, &FailureType::AuthFailed, 1, None),
            p.auth_cooldown_secs
        );
    }
}
