//! 小浣熊的网页登录（`office-raccoon://auth/callback` 授权码换凭证）。
//!
//! ```text
//! ① 打开 {origin}/code/authorize?login_source=desktop&appname=办公小浣熊客户端&state=<state>
//! ② 用户登录成功 → 官方页面跳 office-raccoon://auth/callback?code=…&state=…
//! ③ POST {origin}/api/web/auth/v1/login_with_authorization_code  body {"authorization_code": code}
//!    → data.access_token / data.refresh_token / office_*
//! ```
//!
//! 浏览器跳的是自定义协议，远程部署到不了网关；前端把地址栏里的回调地址
//! 粘回来即可（`submit_callback` 逐项校验 scheme/host/path 与 state）。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Credentials, LoginFlow, Started};

const AUTH_ORIGIN: &str = "https://xiaohuanxiong.com";
const AUTHORIZE_PATH: &str = "/code/authorize";
const AUTH_API_BASE: &str = "https://xiaohuanxiong.com/api/web/auth/v1";
const APP_NAME: &str = "办公小浣熊客户端";

pub struct RaccoonFlow {
    client: reqwest::Client,
    state: String,
}

#[async_trait]
impl LoginFlow for RaccoonFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        // 这家没有服务端轮询通道：等前端把自定义协议回调地址粘回来
        Ok(None)
    }

    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed = url::Url::parse(url.trim()).map_err(|_| "登录回调地址无效".to_string())?;
        let host_matches = parsed
            .host_str()
            .map(str::to_ascii_lowercase)
            .is_some_and(|host| host == "auth");
        if parsed.scheme() != "office-raccoon" || !host_matches || parsed.path() != "/callback" {
            return Err("登录回调地址无效（应为 office-raccoon://auth/callback?code=…&state=…）"
                .to_string());
        }
        let param = |name: &str| -> String {
            parsed
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.trim().to_string())
                .unwrap_or_default()
        };
        let code = param("code");
        let state = param("state");
        if code.is_empty() {
            return Err("登录回调里没有授权码".to_string());
        }
        if state.is_empty() || state != self.state {
            return Err("登录回调 state 校验失败，请重新发起登录".to_string());
        }
        let url = format!("{AUTH_API_BASE}/login_with_authorization_code");
        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&json!({ "authorization_code": code }))
            .send()
            .await
            .map_err(|error| format!("网页登录换取凭证失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let data = payload.get("data").cloned().unwrap_or(Value::Null);
        let access_token = text_of(&data, &["access_token", "accessToken"]);
        if access_token.is_empty() {
            let message = payload
                .get("message")
                .or_else(|| payload.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let message = if message.is_empty() {
                format!("网页登录换取凭证失败（HTTP {status}）")
            } else {
                message.to_string()
            };
            return Err(message);
        }
        let name = text_of(&data, &["office_org_name", "office_identity", "office_org_role"]);
        Ok(Some(Credentials {
            name: if name.is_empty() {
                "小浣熊".to_string()
            } else {
                name
            },
            secret: access_token,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: None,
        }))
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let state = uuid::Uuid::new_v4().to_string();
    let mut auth_url =
        url::Url::parse(AUTH_ORIGIN).map_err(|_| "小浣熊主站地址无效".to_string())?;
    auth_url.set_path(AUTHORIZE_PATH);
    auth_url
        .query_pairs_mut()
        .append_pair("login_source", "desktop")
        .append_pair("appname", APP_NAME)
        .append_pair("state", &state);
    Ok(Started {
        state: state.clone(),
        auth_url: auth_url.to_string(),
        flow: Arc::new(RaccoonFlow {
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
