//! LobsterAI 的**粘贴式登录**。
//!
//! ── 登录方式（上游 account_store 揭示的字段形状）───────────────
//! 登录态是 `accessToken`（对齐上游 `lobsterai_accounts.rs`：凭证落在顶层
//! `accessToken` / `refreshToken` / `uuid` / `firstKeyfrom` / `latestKeyfrom`）。
//! 网关能做的是：用户自己登录后把 accessToken 粘进来，网关**归一化**
//! （剥掉 `Cookie:` / `Bearer ` / 引号等包装）、**校验形态**（长度 + 若为
//! JWT 则解码载荷取展示名），通过后才落账号 —— 与 MonkeyCode / KukuAI 同一
//! 取舍：粘错东西在添加时就有明确报错。
//!
//! ── 续期 ────────────────────────────────────────────────────
//! 上游的换码/续期已整体搬到提供商插件（`agent2api-providers` 的 `lobsterai/`），
//! 不随本仓库发布。网关这里只做令牌粘贴落库，不实现换证（续期端点随插件走）。
//!
//! ── 与登录任务表的关系 ────────────────────────────────────
//! 本模块**不**创建 `LoginFlow` 任务，直接返回 `Credentials`，由调用方走
//! `save_credentials` 落账号。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use serde_json::Value;

use super::paste::normalize_token;
use super::Credentials;

/// 粘贴的凭证长度上限（超过这个长度一定是粘错了东西）
const MAX_SECRET_LENGTH: usize = 8192;

fn check_length(secret: &str, label: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err(format!("请粘贴 {label} 的凭证"));
    }
    if secret.chars().count() > MAX_SECRET_LENGTH {
        return Err(format!("{label} 的凭证过长，请确认粘贴的是正确的值"));
    }
    Ok(())
}

fn text_of(object: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = object.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

/// 解码 JWT 载荷（中间段，base64url 无填充）；不是合法 JWT 返回 `None`。
fn decode_jwt_payload(token: &str) -> Option<Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    serde_json::from_slice(&raw).ok()
}

/// 用粘贴的 accessToken 校验 LobsterAI 登录态，返回可直接落账号的凭证。
///
/// best-effort 校验：长度 + 若为 JWT 则解码载荷取展示名（`uuid` 等）。
/// 续期令牌与 keyfrom 字段随实现搬到插件，网关这里不提取、不换证。
pub async fn verify_lobsterai(
    _client: &reqwest::Client,
    raw_token: &str,
) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "LobsterAI accessToken")?;
    if token.chars().count() < 16 {
        return Err("LobsterAI 的令牌过短，请粘贴完整的 accessToken".to_string());
    }
    let payload = decode_jwt_payload(&token);
    let user_id = payload
        .as_ref()
        .map(|value| text_of(value, &["uuid", "userId", "user_id", "uid", "username"]))
        .unwrap_or_default();
    let name = if user_id.is_empty() {
        "LobsterAI".to_string()
    } else {
        format!("LobsterAI {user_id}")
    };
    Ok(Credentials {
        name,
        secret: token,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn verify_lobsterai_accepts_opaque_token() {
        let creds = verify_lobsterai(&reqwest::Client::new(), "opaque-token-value-1234567890")
            .await
            .expect("非 JWT 令牌也应当通过（best-effort）");
        assert_eq!(creds.secret, "opaque-token-value-1234567890");
        assert_eq!(creds.name, "LobsterAI");
    }

    #[tokio::test]
    async fn verify_lobsterai_extracts_uuid_from_jwt() {
        // {"uuid":"u-42"}
        let payload = "eyJ1dWlkIjoidS00MiJ9";
        let token = format!("h.{payload}.s");
        let creds = verify_lobsterai(&reqwest::Client::new(), &token)
            .await
            .expect("JWT 应当通过校验");
        assert_eq!(creds.secret, token);
        assert_eq!(creds.name, "LobsterAI u-42");
    }

    #[tokio::test]
    async fn verify_lobsterai_rejects_empty_and_short() {
        assert!(verify_lobsterai(&reqwest::Client::new(), "").await.is_err());
        assert!(verify_lobsterai(&reqwest::Client::new(), "short")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn verify_lobsterai_rejects_huge() {
        assert!(
            verify_lobsterai(&reqwest::Client::new(), &"a".repeat(MAX_SECRET_LENGTH + 1))
                .await
                .is_err()
        );
    }
}
