//! WorkBuddy 的网页登录（上游 `auth/state` + 轮询 `auth/token`）。
//!
//! 协议逐条移植自 Agent2API `core/login.rs` 的 `login_interactive`：
//!   ① POST {endpoint}/v2/plugin/auth/state?platform=workbuddy（匿名）
//!        → data.authUrl + data.state
//!   ② 用户在浏览器打开 authUrl 完成登录
//!   ③ 每 2 秒 GET {endpoint}/v2/plugin/auth/token?state=<state>
//!        → data.accessToken（未完成时上游返回 code=11217，继续等待）
//!   ④ GET {endpoint}/v2/plugin/login/account?state=  （Bearer）
//!   ⑤ 回退 GET {endpoint}/v2/plugin/accounts            （Bearer）
//!   ⑥ 再回退 access token 的 JWT 声明（sub 即账号 uid）

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::Value;

use super::{Credentials, LoginFlow, Started};

const ENDPOINT: &str = "https://copilot.tencent.com";
const PREFIX: &str = "/v2/plugin";
const PLATFORM: &str = "workbuddy";
/// 上游「token 未就绪」的业务码
const RETRY_FETCH_TOKEN: i64 = 11217;

fn auth_url(path: &str) -> String {
    format!("{ENDPOINT}{PREFIX}{path}")
}

fn anonymous_headers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("X-No-Authorization", "true"),
        ("X-No-User-Id", "true"),
        ("X-No-Enterprise-Id", "true"),
        ("X-No-Department-Info", "true"),
    ]
}

pub struct WorkBuddyFlow {
    client: reqwest::Client,
    state: String,
    poll_url: String,
    account_url: String,
}

impl WorkBuddyFlow {
    async fn fetch_text(&self, url: &str, bearer: Option<&str>) -> Result<(u16, Value), String> {
        let mut request = self.client.get(url).header("Accept", "application/json");
        for (key, value) in anonymous_headers() {
            request = request.header(key, value);
        }
        if let Some(token) = bearer {
            request = request.header("Authorization", format!("Bearer {token}"));
            request = request.header("X-No-User-Id", "true");
            request = request.header("X-No-Enterprise-Id", "true");
            request = request.header("X-No-Department-Info", "true");
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|error| error.to_string())?;
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        Ok((status, value))
    }

    async fn account_of(&self, token: &str) -> Value {
        let url = format!(
            "{}?state={}",
            self.account_url,
            encode_component(&self.state)
        );
        match self.fetch_text(&url, Some(token)).await {
            Ok((status, value)) if status < 400 => {
                let data = value.get("data").cloned().unwrap_or(Value::Null);
                if data.is_object() {
                    return data;
                }
            }
            _ => {}
        }
        let accounts_url = auth_url("/accounts");
        if let Ok((status, value)) = self.fetch_text(&accounts_url, Some(token)).await {
            if status < 400 {
                if let Some(list) = value
                    .get("data")
                    .and_then(|data| data.get("accounts"))
                    .and_then(Value::as_array)
                {
                    if let Some(found) = list
                        .iter()
                        .find(|item| item.get("lastLogin").map(|v| !v.is_null()).unwrap_or(false))
                        .or_else(|| list.first())
                    {
                        return found.clone();
                    }
                }
            }
        }
        Value::Null
    }
}

#[async_trait]
impl LoginFlow for WorkBuddyFlow {
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(2)
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let (status, value) = self.fetch_text(&self.poll_url, None).await?;
        if status == 202 || status == 404 {
            return Ok(None);
        }
        let code = value.get("code").and_then(Value::as_i64);
        if code == Some(RETRY_FETCH_TOKEN) {
            return Ok(None);
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let token = data
            .get("accessToken")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if token.is_empty() {
            return Ok(None);
        }
        let account = self.account_of(token).await;
        let uid = text_of(&account, &["uid", "userId"]);
        let mut name = text_of(&account, &["nickname", "name", "userName"]);
        if name.is_empty() {
            if let Some(claims) = decode_claims(token) {
                if uid.is_empty() {
                    name = text_of(&claims, &["sub", "nickname", "name", "preferred_username", "email"]);
                } else {
                    name = text_of(&claims, &["nickname", "name", "preferred_username", "email"]);
                }
            }
        }
        if name.is_empty() {
            name = if uid.is_empty() {
                "WorkBuddy".to_string()
            } else {
                format!("WorkBuddy {uid}")
            };
        }
        let expires_at = data
            .get("expiresAt")
            .and_then(Value::as_i64)
            .map(|value| if value > 1_000_000_000_000 { value / 1000 } else { value })
            .or_else(|| {
                data.get("expiresIn")
                    .and_then(Value::as_i64)
                    .map(|seconds| chrono::Utc::now().timestamp() + seconds)
            });
        Ok(Some(Credentials {
            name,
            secret: token.to_string(),
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at,
        }))
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let url = auth_url(&format!("/auth/state?platform={PLATFORM}"));
    let mut request = client
        .post(&url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .body("{}");
    for (key, value) in anonymous_headers() {
        request = request.header(key, value);
    }
    let response = request.send().await.map_err(|error| {
        format!("无法连接 WorkBuddy 登录服务：{error}")
    })?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|error| error.to_string())?;
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if status >= 400 {
        return Err(format!("WorkBuddy auth/state 失败（HTTP {status}）"));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let state = data
        .get("state")
        .or_else(|| data.get("authState"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let login_url = data
        .get("authUrl")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if state.is_empty() || login_url.is_empty() {
        return Err("WorkBuddy auth/state 未返回 state/authUrl".to_string());
    }
    let poll_url = auth_url_of_token(&state);
    Ok(Started {
        state: state.clone(),
        auth_url: login_url,
        flow: Arc::new(WorkBuddyFlow {
            client: client.clone(),
            state,
            poll_url,
            account_url: auth_url("/login/account"),
        }),
    })
}

fn auth_url_of_token(state: &str) -> String {
    auth_url(&format!("/auth/token?state={}", encode_component(state)))
}

pub fn resume(_client: &reqwest::Client) -> Result<Arc<dyn LoginFlow>, String> {
    Err("WorkBuddy 网页登录不支持回调地址回填：请等浏览器登录完成后自动入池".to_string())
}

fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
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

fn decode_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = STANDARD.decode(payload).ok().or_else(|| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
    })?;
    serde_json::from_slice(&bytes).ok()
}
