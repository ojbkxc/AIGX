//! 内置订阅 provider 的数据面适配（claude / chatgpt / codex-subscription）。
//!
//! ## 为什么单独一层
//!
//! 订阅型账号不是「用 API Key 直连某家的 OpenAI/Anthropic 兼容端点」：
//! - chatgpt / codex 只认 **Responses** 协议（`/responses`），且鉴权头、
//!   账号头、令牌续期各有约定；
//! - claude 必须经**官方 Claude Code CLI** 转发，不能把订阅令牌当 API Key 用。
//!
//! 这些都不是 `registry::SessionMeta`（头部模板 + `openai_compatible`）能表达的，
//! 因此本模块独立承接，由 `bridge/account.rs` 在构造账号 Bridge 前拦截。
//!
//! ## 接入点
//!
//! [`build_bridge`] 返回 `Some(..)` 表示「这是订阅 provider，交给我」；
//! 返回 `None` 表示「不是订阅型，走既有 `adapter::build_bridge`」。

pub mod backend;
mod claude;
mod credentials;
pub(crate) mod responses;

use std::sync::Arc;

use crate::account_session::model::AccountSession;
use crate::account_session::store::AccountSessionStore;
use crate::bridge::{Bridge, BridgeError};

use self::backend::Flavor;

/// 为订阅账号构造 Bridge。
///
/// - 非订阅型 → `None`（调用方回退到 `adapter::build_bridge`）；
/// - 订阅型 → `Some(Ok(bridge))` 或 `Some(Err(..))`（配置/凭证问题）。
pub fn build_bridge(
    session: &AccountSession,
    sessions: &AccountSessionStore,
) -> Option<Result<Arc<dyn Bridge>, BridgeError>> {
    let provider = session.provider.trim().to_ascii_lowercase();
    match provider.as_str() {
        "chatgpt-subscription" => Some(backend::build(session, sessions, Flavor::Chatgpt)),
        "codex-subscription" => Some(backend::build(session, sessions, Flavor::Codex)),
        "claude-subscription" => Some(Ok(Arc::new(claude::ClaudeSubscriptionBridge::new()))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_subscription_providers_are_handled() {
        use crate::account_session::registry::is_subscription;
        assert!(is_subscription("chatgpt-subscription"));
        assert!(is_subscription("Codex-Subscription"));
        assert!(is_subscription(" claude-subscription "));
        assert!(!is_subscription("openai"));
        assert!(!is_subscription("codex"));
    }
}
