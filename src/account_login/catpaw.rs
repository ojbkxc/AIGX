//! CatPaw 的网页登录（passport 登录入口 + `poll-token` 兜底轮询）。
//!
//! ```text
//! ① GET  {gateway}/api/gateway/passport/login-config
//!      → data.loginEntryUrl
//! ② 给登录入口拼上 state / redirect / sid 后打开（redirect 必须是 loopback 地址）
//! ③ 每 1 秒 GET {gateway}/api/gateway/passport/poll-token?sid=<sid>
//!      就绪 → data 直接是 token 字符串（不经过浏览器回调，远程部署也能用）
//! ④ GET {gateway}/api/gateway/passport/current-user（X-Auth-Token）补 uid / 昵称
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

use super::{Credentials, LoginFlow, Started};

const GATEWAY_BASE: &str = "https://catx.nocode.cn";
const LOGIN_CONFIG_PATH: &str = "/api/gateway/passport/login-config";
const POLL_TOKEN_PATH: &str = "/api/gateway/passport/poll-token";
const CURRENT_USER_PATH: &str = "/api/gateway/passport/current-user";

pub struct CatpawFlow {
    client: reqwest::Client,
    sid: String,
}

#[async_trait]
impl LoginFlow for CatpawFlow {
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(1)
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let url = format!("{GATEWAY_BASE}{POLL_TOKEN_PATH}?sid={}", self.sid);
        let response = match self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        if !response.status().is_success() {
            return Ok(None);
        }
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let token = match payload.get("data") {
            Some(Value::String(value)) => value.trim().to_string(),
            Some(Value::Object(data)) => data
                .get("token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            _ => String::new(),
        };
        if token.is_empty() {
            return Ok(None);
        }
        let (name, uid) = self.profile(&token).await;
        let name = if name.is_empty() {
            if uid.is_empty() {
                "CatPaw".to_string()
            } else {
                format!("CatPaw {uid}")
            }
        } else {
            name
        };
        let mut headers = HashMap::new();
        if !uid.is_empty() {
            headers.insert("x-uid".to_string(), uid);
        }
        Ok(Some(Credentials {
            name,
            secret: token,
            base_url: None,
            extra_headers: headers,
            expires_at: None,
        }))
    }
}

impl CatpawFlow {
    async fn profile(&self, token: &str) -> (String, String) {
        let url = format!("{GATEWAY_BASE}{CURRENT_USER_PATH}");
        let response = match self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .header("X-Auth-Token", token)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            _ => return (String::new(), String::new()),
        };
        let payload = response.json::<Value>().await.unwrap_or(Value::Null);
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        let uid = data
            .get("userId")
            .map(|value| match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
            .trim()
            .to_string();
        let name = data
            .get("userName")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        (name, if uid == "null" { String::new() } else { uid })
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let url = format!("{GATEWAY_BASE}{LOGIN_CONFIG_PATH}");
    let payload = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| format!("无法连接 CatPaw 登录服务：{error}"))?
        .json::<Value>()
        .await
        .unwrap_or(Value::Null);
    let entry = payload
        .get("data")
        .and_then(|data| data.get("loginEntryUrl"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "CatPaw 登录服务未返回登录入口地址".to_string())?
        .to_string();
    let state = random_hex();
    let sid = random_hex();
    // 上游只校验 redirect 是否为 loopback（端口不校验），远程可用
    let redirect = "http://127.0.0.1:1/api/session/login/catpaw-callback";
    let mut auth_url =
        url::Url::parse(&entry).map_err(|_| "CatPaw 登录入口地址无效".to_string())?;
    auth_url
        .query_pairs_mut()
        .append_pair("state", &state)
        .append_pair("redirect", redirect)
        .append_pair("sid", &sid);
    Ok(Started {
        state,
        auth_url: auth_url.to_string(),
        flow: Arc::new(CatpawFlow {
            client: client.clone(),
            sid,
        }),
    })
}

fn random_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}
