//! ModelScope（魔搭）与 AtomCode 的**粘贴式登录**。
//!
//! ── ModelScope（`modelscope` 国内站 / `modelscope-intl` 国际站）──────
//! 登录态是一枚 `ms-` 前缀的访问令牌。两站是**两个 provider**、令牌不通用
//! （对齐上游 `modelscope/region.rs`：地区做成 provider 而非账号字段，界面上
//! 两张卡片、签到按站隔离）。网关不打电话探活（上游把令牌探活搬到了插件），
//! 只做形态校验（`ms-` 前缀 + 长度），通过后落账号。
//!
//! ── AtomCode ──────────────────────────────────────────────────
//! 登录态是 `accessToken`（可能伴随 `refreshToken`，对齐上游
//! `atomcode_accounts.rs` 的字段形状）。同样 best-effort 校验：长度 + 若为
//! JWT 则解码载荷取续期令牌。续期端点随实现搬到插件，网关这里不实现换证。
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

// ── ModelScope ───────────────────────────────────────────────────

/// 用粘贴的 `ms-` 令牌校验 ModelScope 登录态，返回可直接落账号的凭证。
///
/// `provider` 传 `modelscope`（国内站）或 `modelscope-intl`（国际站）——
/// 两站令牌不通用，展示名据此区分。base_url 由 SessionMeta 提供（本函数不设）。
pub async fn verify_modelscope(
    _client: &reqwest::Client,
    provider: &str,
    raw_token: &str,
) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "ModelScope 令牌")?;
    // ModelScope 令牌是 `ms-` 前缀的访问令牌；前缀认不出就拒绝（避免把说明
    // 文字粘进来），与 CommandCode 的 `user_` 前缀校验同款。
    if !token.starts_with("ms-") {
        return Err("ModelScope 的令牌应以 ms- 开头（请粘贴访问令牌 accessToken）".to_string());
    }
    if token.chars().count() < 16 {
        return Err("ModelScope 的令牌过短，请粘贴完整的 ms- 访问令牌".to_string());
    }
    let intl = provider == "modelscope-intl";
    let name = if intl {
        "ModelScope 国际版".to_string()
    } else {
        "ModelScope".to_string()
    };
    Ok(Credentials {
        name,
        secret: token,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── AtomCode ─────────────────────────────────────────────────────

/// 用粘贴的 accessToken 校验 AtomCode 登录态，返回可直接落账号的凭证。
///
/// best-effort 校验：长度 + 若为 JWT 则解码载荷取展示名（`userId` 等）。
/// 续期令牌随实现搬到插件（上游 `atomcode_accounts.rs` 的 `refreshToken` 字段
/// 形状在此仅作参照），网关这里不提取、不换证 —— 与「令牌存储」口径一致。
pub async fn verify_atomcode(
    _client: &reqwest::Client,
    raw_token: &str,
) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "AtomCode accessToken")?;
    if token.chars().count() < 16 {
        return Err("AtomCode 的令牌过短，请粘贴完整的 accessToken".to_string());
    }
    let payload = decode_jwt_payload(&token);
    let user_id = payload
        .as_ref()
        .map(|value| text_of(value, &["userId", "user_id", "uid", "username", "name"]))
        .unwrap_or_default();
    let name = if user_id.is_empty() {
        "AtomCode".to_string()
    } else {
        format!("AtomCode {user_id}")
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
    async fn verify_modelscope_accepts_ms_token() {
        let creds = verify_modelscope(&reqwest::Client::new(), "modelscope", "ms-abcdef0123456789")
            .await
            .expect("ms- 令牌应当通过");
        assert_eq!(creds.secret, "ms-abcdef0123456789");
        assert_eq!(creds.name, "ModelScope");
    }

    #[tokio::test]
    async fn verify_modelscope_intl_label() {
        let creds = verify_modelscope(
            &reqwest::Client::new(),
            "modelscope-intl",
            "ms-abcdef0123456789",
        )
        .await
        .expect("国际站令牌应当通过");
        assert_eq!(creds.name, "ModelScope 国际版");
    }

    #[tokio::test]
    async fn verify_modelscope_rejects_wrong_prefix() {
        let error = verify_modelscope(&reqwest::Client::new(), "modelscope", "not-ms-prefix")
            .await
            .expect_err("非 ms- 前缀应当拒绝");
        assert!(error.contains("ms-"), "错误文案应当指出 ms- 前缀：{error}");
    }

    #[tokio::test]
    async fn verify_modelscope_rejects_empty_and_huge() {
        assert!(verify_modelscope(&reqwest::Client::new(), "modelscope", "")
            .await
            .is_err());
        assert!(verify_modelscope(
            &reqwest::Client::new(),
            "modelscope",
            &"a".repeat(MAX_SECRET_LENGTH + 1)
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn verify_atomcode_extracts_name_from_jwt() {
        // {"userId":"u-9"}
        let payload = "eyJ1c2VySWQiOiJ1LTkifQ";
        let token = format!("h.{payload}.s");
        let creds = verify_atomcode(&reqwest::Client::new(), &token)
            .await
            .expect("JWT 应当通过校验");
        assert_eq!(creds.secret, token);
        assert_eq!(creds.name, "AtomCode u-9");
        // 续期令牌不提取（refresh 随实现搬到插件）
        assert!(creds.extra_headers.is_empty());
    }

    #[tokio::test]
    async fn verify_atomcode_without_refresh_still_passes() {
        let creds = verify_atomcode(&reqwest::Client::new(), "opaque-token-value-1234567890")
            .await
            .expect("非 JWT 令牌也应当通过（best-effort）");
        assert!(!creds.extra_headers.contains_key("refresh_token"));
        assert_eq!(creds.name, "AtomCode");
    }

    #[tokio::test]
    async fn verify_atomcode_rejects_empty_and_short() {
        assert!(verify_atomcode(&reqwest::Client::new(), "").await.is_err());
        assert!(verify_atomcode(&reqwest::Client::new(), "short")
            .await
            .is_err());
    }
}
