//! Google OAuth 模块 — 参照 `github.rs` 模式实现。
//!
//! 提供：
//! - 授权 URL 构造（`build_authorize_url`）
//! - 授权码换取 access token（`exchange_code`）
//! - 拉取用户信息（`get_user_info`）
//!
//! 参照 burncloud `crates/server/src/api/auth.rs::oauth_google`，
//! 在 AIGX 中以单 crate + axum 0.7 方式实现等价能力。

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Google OAuth 配置（参照 `GithubOauthConfig` 结构）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleOauthConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
    /// 请求的权限范围，默认 `openid email profile`
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "openid email profile".to_string()
}

impl GoogleOauthConfig {
    /// 配置是否就绪（client_id / client_secret / redirect_uri 均非空）。
    /// 用 `trim()` 判空：只有空格的占位值不应被当作已配置。
    pub fn ready(&self) -> bool {
        !self.client_id.trim().is_empty()
            && !self.client_secret.trim().is_empty()
            && !self.redirect_uri.trim().is_empty()
    }
}

/// 构造 Google OAuth 授权页跳转 URL。
///
/// 参照 Google Identity Platform OAuth 2.0 端点：
/// `https://accounts.google.com/o/oauth2/v2/auth`
pub fn build_authorize_url(config: &GoogleOauthConfig, state: &str) -> String {
    format!(
        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}",
        config.client_id,
        config.redirect_uri,
        config.scope,
        state,
    )
}

/// Google access token 响应（application/json）
#[derive(Debug, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    #[serde(default)]
    #[allow(dead_code)]
    token_type: String,
    #[serde(default)]
    #[allow(dead_code)]
    expires_in: i64,
    #[serde(default)]
    #[allow(dead_code)]
    refresh_token: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    id_token: Option<String>,
}

/// Google 用户信息（OpenID Connect userinfo 端点）
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct GoogleUserInfo {
    /// Google 用户唯一 ID（sub）
    pub sub: String,
    /// 邮箱（已启用 email scope 时返回）
    pub email: Option<String>,
    /// 邮箱是否已验证
    #[serde(default)]
    pub email_verified: Option<bool>,
    /// 显示名
    pub name: Option<String>,
    /// 头像 URL
    pub picture: Option<String>,
}

/// 用授权码换取 access token。
///
/// 端点：`https://oauth2.googleapis.com/token`
pub async fn exchange_code(
    config: &GoogleOauthConfig,
    code: &str,
    http_client: &reqwest::Client,
) -> Result<String> {
    let resp = http_client
        .post("https://oauth2.googleapis.com/token")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "client_id": config.client_id,
            "client_secret": config.client_secret,
            "code": code,
            "redirect_uri": config.redirect_uri,
            "grant_type": "authorization_code",
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Google token exchange failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "Google token exchange error {status}: {body}"
        ));
    }

    let token: GoogleTokenResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse Google token response: {e}"))?;

    Ok(token.access_token)
}

/// 拉取 Google 用户信息。
///
/// 端点：`https://openidconnect.googleapis.com/v1/userinfo`
pub async fn get_user_info(
    access_token: &str,
    http_client: &reqwest::Client,
) -> Result<GoogleUserInfo> {
    let resp = http_client
        .get("https://openidconnect.googleapis.com/v1/userinfo")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Google user info request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("Google user info error {status}: {body}"));
    }

    let info: GoogleUserInfo = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse Google user info: {e}"))?;

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, secret: &str, redirect: &str) -> GoogleOauthConfig {
        GoogleOauthConfig {
            client_id: id.to_string(),
            client_secret: secret.to_string(),
            redirect_uri: redirect.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn ready_requires_all_three_fields() {
        assert!(!GoogleOauthConfig::default().ready());
        assert!(!cfg("id", "secret", "").ready());
        assert!(!cfg("id", "", "https://x/cb").ready());
        assert!(cfg("id", "secret", "https://x/cb").ready());
    }

    #[test]
    fn ready_rejects_whitespace_only_placeholders() {
        assert!(!cfg(" ", "secret", "https://x/cb").ready());
        assert!(!cfg("id", "secret", "  \t").ready());
    }

    #[test]
    fn token_response_tolerates_missing_optional_fields() {
        let r: GoogleTokenResponse = serde_json::from_str(r#"{"access_token":"t"}"#).unwrap();
        assert_eq!(r.access_token, "t");
        assert_eq!(r.expires_in, 0);
        assert!(r.refresh_token.is_none());
        assert!(serde_json::from_str::<GoogleTokenResponse>(r#"{"token_type":"bearer"}"#).is_err());
    }

    #[test]
    fn user_info_requires_sub() {
        // Google 的稳定用户标识是 sub；缺了它就无法区分用户
        assert!(serde_json::from_str::<GoogleUserInfo>(r#"{"email":"a@b.c"}"#).is_err());
        let ok: GoogleUserInfo = serde_json::from_str(r#"{"sub":"123"}"#).unwrap();
        assert_eq!(ok.sub, "123");
        assert!(ok.email.is_none());
    }
}
