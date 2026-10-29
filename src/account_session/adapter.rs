//! 账号 → 上游 Bridge 的适配层。
//!
//! 请求构造不重复实现：按 provider 协议族复用既有原生 Bridge
//! （`bridge::openai` / `bridge::anthropic` / `bridge::gemini`）。
//! 本层只负责解析生效地址、解密凭证、取出口 Client。

use std::sync::Arc;

use super::model::AccountSession;
use super::registry::{self, Protocol};
use super::store::AccountSessionStore;
use crate::bridge::{Bridge, BridgeError};

/// 生效的上游地址：账号级地址优先，空则用 provider 目录默认值。
pub fn effective_base_url(session: &AccountSession) -> Result<String, BridgeError> {
    let raw = session.base_url.trim();
    if !raw.is_empty() {
        return Ok(raw.to_string());
    }
    let spec = registry::find(&session.provider).ok_or_else(|| {
        BridgeError::Config(format!(
            "unknown account provider '{}' for account {}",
            session.provider, session.id
        ))
    })?;
    if spec.default_base_url.is_empty() {
        return Err(BridgeError::Config(format!(
            "account provider '{}' requires an explicit base_url",
            spec.kind
        )));
    }
    Ok(spec.default_base_url.to_string())
}

/// 按账号凭证构造上游 Bridge（按账号代理选出口 Client）。
pub fn build_bridge(
    session: &AccountSession,
    sessions: &AccountSessionStore,
) -> Result<Arc<dyn Bridge>, BridgeError> {
    let spec = registry::find(&session.provider).ok_or_else(|| {
        BridgeError::Config(format!("unknown account provider '{}'", session.provider))
    })?;
    let base = effective_base_url(session)?;
    let secret = session.credential.secret_plain();
    let client = sessions.client_for(session)?;
    let bridge: Arc<dyn Bridge> = match spec.protocol {
        Protocol::Openai => crate::bridge::openai::make_bridge(&base, &secret, &client),
        Protocol::Anthropic => crate::bridge::anthropic::make_bridge(&base, &secret, &client),
        Protocol::Gemini => crate::bridge::gemini::make_bridge(&base, &secret, &client),
    };
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::model::AccountSession;

    #[test]
    fn explicit_base_url_wins() {
        let mut session = AccountSession::new("ch", "acct");
        session.base_url = "https://example.com/v1".to_string();
        assert_eq!(
            effective_base_url(&session).unwrap(),
            "https://example.com/v1"
        );
    }

    #[test]
    fn provider_default_applies_when_base_url_empty() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "deepseek".to_string();
        assert_eq!(
            effective_base_url(&session).unwrap(),
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn openai_compatible_requires_explicit_base_url() {
        let session = AccountSession::new("ch", "acct");
        assert!(effective_base_url(&session).is_err());
    }
}
