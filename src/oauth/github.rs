use anyhow::Result;
use serde::{Deserialize, Serialize};

/// GitHub OAuth configuration
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubOauthConfig {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
}

impl GithubOauthConfig {
    /// 配置是否就绪。用 `trim()` 判空：手写的 config 里常见只有空格的
    /// "占位值"，旧实现会把它当作已配置 —— 界面显示登录按钮，换 token 必失败。
    pub fn ready(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
}

/// GitHub OAuth access token response
#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    access_token: String,
    #[serde(default)]
    #[allow(dead_code)] // 反序列化容忍字段：GitHub 返回但当前登录流程不消费
    token_type: String,
}

/// GitHub user info
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct GithubUserInfo {
    pub id: i64,
    pub login: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// Exchange authorization code for access token
pub async fn exchange_code(
    config: &GithubOauthConfig,
    code: &str,
    http_client: &reqwest::Client,
) -> Result<String> {
    let resp = http_client
        .post("https://github.com/login/oauth/access_token")
        .header("Accept", "application/json")
        .json(&serde_json::json!({
            "client_id": config.client_id,
            "client_secret": config.client_secret,
            "code": code,
            "redirect_uri": config.redirect_uri,
        }))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("GitHub token exchange failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!(
            "GitHub token exchange error {status}: {body}"
        ));
    }

    let token: GithubTokenResponse = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse GitHub token response: {e}"))?;

    Ok(token.access_token)
}

/// Fetch GitHub user info
pub async fn get_user_info(
    access_token: &str,
    http_client: &reqwest::Client,
) -> Result<GithubUserInfo> {
    let resp = http_client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "AIGX")
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("GitHub user info request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow::anyhow!("GitHub user info error {status}: {body}"));
    }

    let info: GithubUserInfo = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse GitHub user info: {e}"))?;

    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(id: &str, secret: &str) -> GithubOauthConfig {
        GithubOauthConfig {
            client_id: id.to_string(),
            client_secret: secret.to_string(),
            redirect_uri: "https://gw.example/api/auth/oauth/github/callback".to_string(),
        }
    }

    #[test]
    fn ready_requires_both_credentials() {
        assert!(!GithubOauthConfig::default().ready());
        assert!(!cfg("id", "").ready());
        assert!(!cfg("", "secret").ready());
        assert!(cfg("id", "secret").ready());
    }

    /// 手写配置里常见的「只有空格」占位值不能被当成已配置，否则界面显示
    /// 登录按钮但换 token 必失败。
    #[test]
    fn ready_rejects_whitespace_only_placeholders() {
        assert!(!cfg("   ", "secret").ready());
        assert!(!cfg("id", "\t\n").ready());
        assert!(cfg(" id ", "\tsecret ").ready());
    }

    #[test]
    fn token_response_tolerates_missing_token_type() {
        let r: GithubTokenResponse = serde_json::from_str(r#"{"access_token":"t"}"#).unwrap();
        assert_eq!(r.access_token, "t");
        assert!(r.token_type.is_empty());
        // access_token 缺失必须报错，而不是静默拿到空串
        assert!(serde_json::from_str::<GithubTokenResponse>(r#"{"token_type":"bearer"}"#).is_err());
    }

    #[test]
    fn user_info_requires_id_and_login() {
        let ok: GithubUserInfo = serde_json::from_str(r#"{"id":1,"login":"octocat"}"#).unwrap();
        assert_eq!(ok.id, 1);
        assert_eq!(ok.login, "octocat");
        assert!(ok.email.is_none());
        // 缺 login / id 都要失败（否则会把空用户名写进用户表）
        assert!(serde_json::from_str::<GithubUserInfo>(r#"{"id":1}"#).is_err());
        assert!(serde_json::from_str::<GithubUserInfo>(r#"{"login":"x"}"#).is_err());
    }
}
