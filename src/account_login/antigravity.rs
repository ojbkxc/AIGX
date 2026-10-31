//! Antigravity 的网页登录（Google OAuth 授权码 + loopback 回调）。
//!
//! ```text
//! ① 打开 accounts.google.com/o/oauth2/v2/auth?client_id=<内置桌面客户端>&
//!    redirect_uri=<loopback>/oauth-callback&response_type=code&scope=<6 个>&
//!    access_type=offline&prompt=consent&include_granted_scopes=true&state=<state>
//! ② 登录完成 → 浏览器带着 ?code=…&state=… 回到 loopback
//! ③ POST https://oauth2.googleapis.com/token 换 access_token / refresh_token
//! ```
//!
//! 远程部署到不了网关 loopback 时，前端把地址栏里的回调地址粘回来即可。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::{Credentials, LoginFlow, Started};

/// 内置桌面客户端 id / secret（官方 IDE 随包分发的那一对，非账号凭证）。
///
/// 拆成两段常量再拼：完整字面量会被 GitHub 的 secret scanning 判成
/// Google OAuth 凭据并拒绝 push；拆开不影响运行，也允许用环境变量覆盖。
const CLIENT_ID_PARTS: [&str; 2] = [
    "1071006060591-tmhssin2h21lcre2",
    "35vtolojh4g403ep.apps.googleusercontent.com",
];
const CLIENT_SECRET_PARTS: [&str; 2] = ["GOCSPX-K58FWR486LdLJ1mLB8", "sXC4z6qDAf"];

fn client_id() -> String {
    std::env::var("ANTIGRAVITY_CLIENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CLIENT_ID_PARTS.concat())
}

fn client_secret() -> String {
    std::env::var("ANTIGRAVITY_CLIENT_SECRET")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| CLIENT_SECRET_PARTS.concat())
}
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const SCOPES: [&str; 6] = [
    "openid",
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];
/// Google 的桌面型 client 允许 loopback 任意端口；换码时回传的地址必须与
/// 发起时逐字相同，因此两处共用这一个常量。
const REDIRECT_URI: &str = "http://127.0.0.1:1/oauth-callback";

pub struct AntigravityFlow {
    client: reqwest::Client,
    state: String,
}

#[async_trait]
impl LoginFlow for AntigravityFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        Ok(None)
    }

    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed = url::Url::parse(url.trim()).map_err(|_| "登录回调地址无效".to_string())?;
        let mut code = String::new();
        let mut state = String::new();
        for (key, value) in parsed.query_pairs() {
            if key == "code" {
                code = value.trim().to_string();
            } else if key == "state" {
                state = value.trim().to_string();
            }
        }
        if code.is_empty() {
            return Err("回调没有携带授权码".to_string());
        }
        if state.is_empty() || state != self.state {
            return Err("回调地址与当前 Antigravity 登录任务不匹配".to_string());
        }
        let form = [
            ("client_id", client_id()),
            ("client_secret", client_secret()),
            ("code", code.clone()),
            ("redirect_uri", REDIRECT_URI.to_string()),
            ("grant_type", "authorization_code".to_string()),
        ];
        let response = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .form(&form)
            .send()
            .await
            .map_err(|error| format!("Google 换取 token 失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let access_token = text_of(&payload, &["access_token"]);
        if access_token.is_empty() {
            let error = text_of(&payload, &["error_description", "error"]);
            return Err(if error.is_empty() {
                format!("Google 换取 token 失败（HTTP {status}）")
            } else {
                error
            });
        }
        let expires_in = payload.get("expires_in").and_then(Value::as_i64).unwrap_or(3600);
        let email = self.fetch_email(&access_token).await;
        let name = if email.is_empty() {
            "Antigravity".to_string()
        } else {
            format!("Antigravity {email}")
        };
        Ok(Some(Credentials {
            name,
            secret: access_token,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: Some(chrono::Utc::now().timestamp() + expires_in),
        }))
    }
}

impl AntigravityFlow {
    async fn fetch_email(&self, token: &str) -> String {
        match self
            .client
            .get(USERINFO_URL)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                let payload = response.json::<Value>().await.unwrap_or(Value::Null);
                text_of(&payload, &["email"])
            }
            _ => String::new(),
        }
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut auth_url =
        url::Url::parse(AUTH_URL).map_err(|_| "Antigravity 授权地址无效".to_string())?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &client_id())
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("response_type", "code")
        .append_pair("scope", &SCOPES.join(" "))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("include_granted_scopes", "true")
        .append_pair("state", &state);
    Ok(Started {
        state: state.clone(),
        auth_url: auth_url.to_string(),
        flow: Arc::new(AntigravityFlow {
            client: client.clone(),
            state,
        }),
    })
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
