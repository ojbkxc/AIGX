//! 账号 → 上游 Bridge 的适配层。
//!
//! 请求构造不重复实现：按 provider 协议族复用既有原生 Bridge
//! （`bridge::openai` / `bridge::anthropic` / `bridge::gemini`）。
//! 本层只负责解析生效地址、解密凭证、取出口 Client。

use std::sync::Arc;

use super::model::{AccountSession, CREDENTIAL_KIND_NONE};
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

/// 合并请求头：provider 默认头 + 账号级额外头（同名以账号级为准）。
fn session_headers(
    session: &AccountSession,
    meta: &registry::SessionMeta,
) -> Vec<(String, String)> {
    let mut merged: Vec<(String, String)> = meta
        .headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    for (name, value) in &session.extra_headers {
        let lower = name.to_ascii_lowercase();
        merged.retain(|(existing, _)| existing.to_ascii_lowercase() != lower);
        merged.push((name.clone(), value.clone()));
    }
    merged
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
    // `kind = none` = 免鉴权上游（IP 白名单等），此时不发送任何鉴权头
    let secret = if session.credential.kind == CREDENTIAL_KIND_NONE {
        String::new()
    } else {
        session.credential.secret_plain()
    };
    let client = sessions.client_for(session)?;
    let bridge: Arc<dyn Bridge> = match spec.protocol {
        Protocol::Openai => {
            // 登录态型供应商：Cookie / JWT + 专用请求头（不走 Bearer API Key）
            if let Some(meta) = registry::session_meta(spec.kind) {
                let auth_mode = match meta.auth {
                    registry::SessionAuth::Bearer => crate::bridge::openai::AuthMode::Bearer,
                    registry::SessionAuth::Cookie => crate::bridge::openai::AuthMode::Cookie,
                    registry::SessionAuth::Headers => crate::bridge::openai::AuthMode::Headers,
                };
                Arc::new(
                    crate::bridge::openai::OpenaiCompatibleBridge::with_session_auth(
                        &base,
                        &secret,
                        client.as_ref().clone(),
                        auth_mode,
                        session_headers(session, meta),
                        session.chat_path.clone(),
                    ),
                )
            } else {
                crate::bridge::openai::make_bridge(&base, &secret, &client)
            }
        }
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

    #[test]
    fn account_headers_override_provider_defaults_case_insensitively() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "qoder".to_string();
        session
            .extra_headers
            .insert("User-Agent".to_string(), "custom/1.0".to_string());
        session
            .extra_headers
            .insert("x-token".to_string(), "{credential}".to_string());
        let meta = registry::session_meta("qoder").unwrap();
        let merged = session_headers(&session, meta);
        let ua: Vec<&(String, String)> = merged
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            .collect();
        assert_eq!(ua.len(), 1, "同名请求头应只保留账号级一条");
        assert_eq!(ua[0].1, "custom/1.0");
        assert!(merged
            .iter()
            .any(|(name, value)| name == "x-token" && value == "{credential}"));
    }
}
