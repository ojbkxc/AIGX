//! Accio 的网页登录（OAuth 授权码 + PKCE，回调带 `code/state` 查询串）。
//!
//! ```text
//! ① 生成 verifier / challenge（base64url(sha256(verifier))）与 state
//! ② 打开 {login}/login?return_url=<loopback>&state=&code_challenge=&code_challenge_method=S256
//!        &client_id=accio-work
//! ③ 登录页把 ?code=…&state=… 拼到 return_url 上跳回
//! ④ POST {gw}/api/oauth/token  {code, codeVerifier, clientId, redirectUri}
//! ```
//!
//! 远程部署到不了网关 loopback 时，前端把地址栏里的回调地址粘回来即可。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

const LOGIN_BASE: &str = "https://www.accio.com";
const GATEWAY_BASE: &str = "https://phoenix-gw.alibaba.com";
const OAUTH_TOKEN_PATH: &str = "/api/oauth/token";
const CLIENT_ID: &str = "accio-work";
const APP_VERSION: &str = "0.32.6";
/// 回调必须与发起时逐字相同（换码要原样回传）；远程部署时浏览器到不了这个端口，
/// 用户把地址栏里的回调地址粘回来即可。
const REDIRECT_URI: &str = "http://127.0.0.1:1/auth/callback-accio";

pub struct AccioFlow {
    client: reqwest::Client,
    state: String,
    verifier: String,
}

#[async_trait]
impl LoginFlow for AccioFlow {
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
            return Err("回调地址与当前 Accio 登录任务不匹配".to_string());
        }
        let body = json!({
            "code": code,
            "codeVerifier": self.verifier,
            "clientId": CLIENT_ID,
            "redirectUri": REDIRECT_URI,
        });
        let response = self
            .client
            .post(format!("{GATEWAY_BASE}{OAUTH_TOKEN_PATH}"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("Accept-Language", "en")
            .header("x-language", "en")
            .header("x-platform", "desktop")
            .header("x-os", std::env::consts::OS)
            .header("x-app-version", APP_VERSION)
            .header("x-package-region", "GLOBAL")
            .json(&body)
            .send()
            .await
            .map_err(|error| format!("Accio 换码请求失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        let access_token = text_of(&data, &["accessToken", "access_token", "token"]);
        if access_token.is_empty() {
            return Err(format!("Accio 换码响应缺少 accessToken（HTTP {status}）"));
        }
        let email = text_of(&data, &["email"]);
        let name = if email.is_empty() {
            "Accio".to_string()
        } else {
            format!("Accio {email}")
        };
        Ok(Some(Credentials {
            name,
            secret: access_token,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: None,
        }))
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let verifier = random_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut auth_url = url::Url::parse(&format!("{LOGIN_BASE}/login"))
        .map_err(|_| "Accio 登录地址无效".to_string())?;
    auth_url
        .query_pairs_mut()
        .append_pair("return_url", REDIRECT_URI)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("client_id", CLIENT_ID);
    Ok(Started {
        state: state.clone(),
        auth_url: auth_url.to_string(),
        flow: Arc::new(AccioFlow {
            client: client.clone(),
            state,
            verifier,
        }),
    })
}


fn random_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
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
