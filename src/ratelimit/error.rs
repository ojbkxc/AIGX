//! 限流错误类型 — 借鉴 aisix-ratelimit/src/error.rs
//!
//! 提供结构化的限流错误，包含 retry_after 提示，方便上游直接渲染为
//! OpenAI 兼容的 429 响应。

/// 限流作用域
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitScope {
    /// 请求次数限制
    Requests,
    /// Token 数量限制
    Tokens,
}

impl std::fmt::Display for RateLimitScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitScope::Requests => write!(f, "requests"),
            RateLimitScope::Tokens => write!(f, "tokens"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RateLimitError {
    #[error("request limit exceeded ({scope})")]
    Requests {
        scope: RateLimitScope,
        retry_after_secs: u64,
    },
    #[error("token limit exceeded ({scope})")]
    Tokens {
        scope: RateLimitScope,
        retry_after_secs: u64,
    },
    #[error("concurrency limit exceeded")]
    Concurrency,
}

impl RateLimitError {
    pub fn scope(&self) -> RateLimitScope {
        match self {
            RateLimitError::Requests { scope, .. } => *scope,
            RateLimitError::Tokens { scope, .. } => *scope,
            RateLimitError::Concurrency => RateLimitScope::Requests,
        }
    }

    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            RateLimitError::Requests {
                retry_after_secs, ..
            }
            | RateLimitError::Tokens {
                retry_after_secs, ..
            } => Some(*retry_after_secs),
            RateLimitError::Concurrency => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_scope_preserved_on_access() {
        let e = RateLimitError::Requests {
            scope: RateLimitScope::Requests,
            retry_after_secs: 42,
        };
        assert_eq!(e.scope(), RateLimitScope::Requests);
        assert_eq!(e.retry_after_secs(), Some(42));
    }

    #[test]
    fn concurrency_has_no_retry_after_hint() {
        let e = RateLimitError::Concurrency;
        assert_eq!(e.scope(), RateLimitScope::Requests);
        assert!(e.retry_after_secs().is_none());
    }
}
