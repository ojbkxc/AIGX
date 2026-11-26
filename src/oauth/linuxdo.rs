//! LinuxDO OAuth 模块 — 参照 `github.rs` 模式实现。
//!
//! 提供：
//! - 授权码换取 access token（`exchange_code`）
//! - 拉取用户信息（`get_user_info`）
//!
//! 端点细节对齐 new-api `oauth/linuxdo.go`（connect.linux.do，标准 OAuth2）：
//! - token 交换：POST form-urlencoded + HTTP Basic auth（区别于 GitHub 的 JSON body）
//! - user info：`/api/user` 无 email 字段，回调侧统一用 `{username}@linuxdo.local` 伪邮箱

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// LinuxDO OAuth configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LinuxDoOauthConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
}

impl LinuxDoOauthConfig {
    /// 配置是否就绪。`trim()` 判空：只有空格的占位值不应被当作已配置。
    pub fn ready(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

/// LinuxDO OAuth access token response
#[derive(Debug, Deserialize)]
struct LinuxDoTokenResponse {
    access_token: String,
    #[serde(default)]
    #[allow(dead_code)] // 反序列化容忍字段：LinuxDO 返回但当前登录流程不消费
    token_type: String,
}

/// LinuxDO user info（对齐 new-api `linuxdoUser`；name/active/trust_level/silenced
/// AIGX 登录流程不消费，serde 忽略未知字段，不在此声明）
#[derive(Debug, Deserialize)]
pub struct LinuxDoUserInfo {
    pub id: i64,
    pub username: String,
}

/// Exchange authorization code for access token
///
/// LinuxDO token 端点是标准 OAuth2：HTTP Basic 认证 + form 请求体，
/// 不接受 GitHub 式的 JSON body（对齐 new-api `ExchangeToken`）。
pub async fn exchange_code(
    config: &LinuxDoOauthConfig,
    code: &str,
    http_client: &reqwest::Client,
) -> Result<String> {
    let resp = http_client
        .post("https://connect.linux.do/oauth2/token")
        .basic_auth(&config.client_id, Some(&config.client_secret))
        .header("Accept", "application/json")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", config.redirect_uri.as_str()),
        ])
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("LinuxDO token exchange failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "LinuxDO token exchange error {status}: {body}"
        ));
    }

    let token: LinuxDoTokenResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse LinuxDO token response: {e}"))?;

    Ok(token.access_token)
}

/// Fetch LinuxDO user info
pub async fn get_user_info(
    access_token: &str,
    http_client: &reqwest::Client,
) -> Result<LinuxDoUserInfo> {
    let resp = http_client
        .get("https://connect.linux.do/api/user")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("LinuxDO user info request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("LinuxDO user info error {status}: {body}"));
    }

    let info: LinuxDoUserInfo = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse LinuxDO user info: {e}"))?;

    if info.id == 0 {
        return Err(anyhow::anyhow!("LinuxDO user info: invalid user id"));
    }

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, secret: &str) -> LinuxDoOauthConfig {
        LinuxDoOauthConfig {
            client_id: id.to_string(),
            client_secret: secret.to_string(),
            redirect_uri: String::new(),
        }
    }

    #[test]
    fn ready_requires_both_credentials() {
        assert!(!LinuxDoOauthConfig::default().ready());
        assert!(!cfg("id", "").ready());
        assert!(cfg("id", "secret").ready());
    }

    #[test]
    fn ready_rejects_whitespace_only_placeholders() {
        assert!(!cfg("  ", "secret").ready());
        assert!(!cfg("id", "\t").ready());
    }

    #[test]
    fn token_response_tolerates_missing_token_type() {
        let r: LinuxDoTokenResponse = serde_json::from_str(r#"{"access_token":"t"}"#).unwrap();
        assert_eq!(r.access_token, "t");
        assert!(
            serde_json::from_str::<LinuxDoTokenResponse>(r#"{"token_type":"bearer"}"#).is_err()
        );
    }

    #[test]
    fn user_info_requires_id_and_username() {
        let ok: LinuxDoUserInfo = serde_json::from_str(r#"{"id":7,"username":"neo"}"#).unwrap();
        assert_eq!(ok.id, 7);
        assert_eq!(ok.username, "neo");
        assert!(serde_json::from_str::<LinuxDoUserInfo>(r#"{"id":7}"#).is_err());
    }
}
