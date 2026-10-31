//! ZCode 的 CLI 轮询登录（国内版 bigmodel 平面）。
//!
//! ```text
//! ① POST {zcode}/api/v1/oauth/cli/init  body {"provider":"bigmodel"}（Bearer <poll token>）
//!    → data.flow_id / data.authorize_url / data.poll_interval_sec
//! ② 给 authorize_url 补上中转页参数（redirect=<中转页>，否则服务端记不下结果）
//! ③ GET {zcode}/api/v1/oauth/cli/poll/{flow_id}（Bearer <poll token>）
//!    status=ready → data.bigmodel.access_token（推理令牌）+ token（套餐 JWT）+ user.user_id
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Credentials, LoginFlow, Started};

const ZCODE_ORIGIN: &str = "https://zcode.z.ai";
const UPSTREAM_PROVIDER: &str = "bigmodel";
const INIT_PATH: &str = "/api/v1/oauth/cli/init";
const APP_VERSION: &str = "3.14.0";

pub struct ZcodeFlow {
    client: reqwest::Client,
    flow_id: String,
    poll_token: String,
    interval: Duration,
}

#[async_trait]
impl LoginFlow for ZcodeFlow {
    fn poll_interval(&self) -> Duration {
        self.interval
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let url = format!("{ZCODE_ORIGIN}/api/v1/oauth/cli/poll/{}", self.flow_id);
        let response = match self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .header("Authorization", format!("Bearer {}", self.poll_token))
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => return Ok(None),
        };
        let status = response.status().as_u16();
        if status >= 500 || status == 408 || status == 429 {
            return Ok(None);
        }
        let text = response.text().await.unwrap_or_default();
        let payload = match serde_json::from_str::<Value>(&text) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if let Some(code) = payload.get("code").and_then(Value::as_i64) {
            if code != 0 {
                let message = payload
                    .get("msg")
                    .or_else(|| payload.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("上游拒绝");
                return Err(format!("ZCode 登录失败（{code}）：{message}"));
            }
        }
        if status >= 400 {
            return Err(format!("ZCode 登录轮询被拒（HTTP {status}）"));
        }
        let data = match payload.get("data") {
            Some(data) => data,
            None => return Ok(None),
        };
        match data.get("status").and_then(Value::as_str).unwrap_or("") {
            "ready" => {}
            "failed" => return Err("ZCode 授权被拒绝或已失效，请重新发起登录".to_string()),
            _ => return Ok(None),
        }
        let access_token = data
            .get(UPSTREAM_PROVIDER)
            .and_then(|value| value.get("access_token"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if access_token.is_empty() {
            return Err(format!(
                "ZCode 登录响应缺少 {UPSTREAM_PROVIDER}.access_token"
            ));
        }
        let user_id = data
            .get("user")
            .and_then(|value| value.get("user_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let name = if user_id.is_empty() {
            "ZCode".to_string()
        } else {
            format!("ZCode {user_id}")
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
    let poll_token = random_hex_32();
    let url = format!("{ZCODE_ORIGIN}{INIT_PATH}");
    let response = client
        .post(&url)
        .header("Authorization", format!("Bearer {poll_token}"))
        .header("Content-Type", "application/json")
        .json(&json!({ "provider": UPSTREAM_PROVIDER }))
        .send()
        .await
        .map_err(|error| format!("ZCode 登录发起失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|error| error.to_string())?;
    let payload = serde_json::from_str::<Value>(&text)
        .map_err(|_| "ZCode 登录发起：上游响应不是 JSON".to_string())?;
    if let Some(code) = payload.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = payload
                .get("msg")
                .or_else(|| payload.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("上游拒绝");
            return Err(format!("ZCode 登录发起失败（{code}）：{message}"));
        }
    }
    if status >= 400 {
        return Err(format!("ZCode 登录发起失败（HTTP {status}）"));
    }
    let data = payload
        .get("data")
        .ok_or_else(|| "ZCode 登录发起：响应缺少 data".to_string())?;
    let flow_id = data
        .get("flow_id")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ZCode 登录响应缺少 flow_id".to_string())?;
    let authorize_url = data
        .get("authorize_url")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "ZCode 登录响应缺少 authorize_url".to_string())?;
    let auth_url = apply_interstitial(&authorize_url);
    let interval = Duration::from_secs(
        data.get("poll_interval_sec")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
            .unwrap_or(3)
            .max(1) as u64,
    );
    Ok(Started {
        state: flow_id.clone(),
        auth_url,
        flow: Arc::new(ZcodeFlow {
            client: client.clone(),
            flow_id,
            poll_token,
            interval,
        }),
    })
}


/// 把中转页参数替换进 `authorize_url`（国内版参数名是 `redirect`）。
fn apply_interstitial(authorize_url: &str) -> String {
    let target = encode_component(&format!(
        "{ZCODE_ORIGIN}/app/oauth/login?redirect=zcode%3A%2F%2Foauth%2Fcallback&app_version={APP_VERSION}"
    ));
    let prefix = "redirect=";
    let replacement = format!("{prefix}{target}");
    let (head, fragment) = match authorize_url.split_once('#') {
        Some((head, fragment)) => (head, Some(fragment)),
        None => (authorize_url, None),
    };
    let (path, query) = match head.split_once('?') {
        Some((path, query)) => (path, query),
        None => (head, ""),
    };
    let mut replaced = false;
    let mut segments: Vec<String> = Vec::new();
    for segment in query.split('&').filter(|value| !value.is_empty()) {
        if segment.starts_with(prefix) {
            if !replaced {
                segments.push(replacement.clone());
                replaced = true;
            }
            continue;
        }
        segments.push(segment.to_string());
    }
    if !replaced {
        segments.push(replacement);
    }
    let mut out = String::from(path);
    out.push('?');
    out.push_str(&segments.join("&"));
    if let Some(fragment) = fragment {
        out.push('#');
        out.push_str(fragment);
    }
    out
}

fn random_hex_32() -> String {
    let mut out = String::with_capacity(64);
    for _ in 0..2 {
        out.push_str(&uuid::Uuid::new_v4().simple().to_string());
    }
    out
}

fn encode_component(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric()
            || matches!(ch, '-' | '_' | '.' | '!' | '~' | '*' | '\'' | '(' | ')')
        {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
