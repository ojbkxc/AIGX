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
    pub fn ready(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }
}

/// GitHub OAuth access token response
#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    access_token: String,
    #[serde(default)]
    token_type: String,
}

/// GitHub user info
#[derive(Debug, Deserialize)]
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
        return Err(anyhow::anyhow!("GitHub token exchange error {status}: {body}"));
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
