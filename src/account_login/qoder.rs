//! Qoder 的 PKCE 设备授权（中国版站点：qoder.com.cn / openapi.qoder.com.cn）。
//!
//! ```text
//! ① 本地生成 verifier，challenge = base64url(sha256(verifier))
//! ② 打开 {web}/device/selectAccounts?challenge=&challenge_method=S256&machine_id=&nonce=
//! ③ 每 2 秒 GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256
//!    202 / 404 = 等待中；data.token 即访问令牌（dt- 前缀）
//! ④ GET {openapi}/api/v1/userinfo（Bearer）补齐昵称（失败不影响落账号）
//! ```

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

const WEB_ORIGIN: &str = "https://qoder.com.cn";
const OPEN_API: &str = "https://openapi.qoder.com.cn";
const DEVICE_LOGIN_PATH: &str = "/device/selectAccounts";
const DEVICE_POLL_PATH: &str = "/api/v1/deviceToken/poll";
const USER_INFO_PATH: &str = "/api/v1/userinfo";

fn machine_id() -> String {
    static MACHINE: OnceLock<String> = OnceLock::new();
    MACHINE
        .get_or_init(|| uuid::Uuid::new_v4().to_string())
        .clone()
}

fn random_secret() -> String {
    // 32 字节随机（uuid v4 两枚拼起来足够不可预测）
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn open_api_headers(token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        ("Cosy-Version".to_string(), "1.0.1".to_string()),
        ("Cosy-ClientType".to_string(), "5".to_string()),
        ("User-Agent".to_string(), "qoder-local-proxy".to_string()),
    ];
    if let Some(token) = token {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    headers
}

pub struct QoderFlow {
    client: reqwest::Client,
    poll_url: String,
}

#[async_trait]
impl LoginFlow for QoderFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let mut request = self
            .client
            .get(&self.poll_url)
            .header("Accept", "application/json");
        for (key, value) in open_api_headers(None) {
            request = request.header(key, value);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        if status == 202 || status == 404 {
            return Ok(None);
        }
        let text = response.text().await.map_err(|error| error.to_string())?;
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let data = value.get("data").cloned().unwrap_or(value.clone());
        let token = data
            .get("token")
            .or_else(|| data.get("access_token"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if token.is_empty() {
            return Ok(None);
        }
        let user_id = text_of(&data, &["user_id", "userId"]);
        let mut name = text_of(&data, &["nickname", "name", "username", "email"]);
        if name.is_empty() {
            name = self.profile_name(token).await;
        }
        if name.is_empty() {
            name = if user_id.is_empty() {
                "Qoder".to_string()
            } else {
                format!("Qoder {user_id}")
            };
        }
        let expires_at = data
            .get("expires_at")
            .and_then(Value::as_i64)
            .map(|value| {
                if value > 1_000_000_000_000 {
                    value / 1000
                } else {
                    value
                }
            })
            .or_else(|| {
                data.get("expires_in")
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

impl QoderFlow {
    async fn profile_name(&self, token: &str) -> String {
        let url = format!("{OPEN_API}{USER_INFO_PATH}");
        let mut request = self.client.get(&url).header("Accept", "application/json");
        for (key, value) in open_api_headers(Some(token)) {
            request = request.header(key, value);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                let value = response.json::<Value>().await.unwrap_or(Value::Null);
                let data = value.get("data").cloned().unwrap_or(value);
                text_of(&data, &["nickname", "name", "username", "email"])
            }
            _ => String::new(),
        }
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let verifier = random_secret();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let machine = machine_id();
    let state = random_secret();
    let auth_url = format!(
        "{WEB_ORIGIN}{DEVICE_LOGIN_PATH}?challenge={}&challenge_method=S256&machine_id={}&nonce={}",
        encode_component(&challenge),
        encode_component(&machine),
        encode_component(&nonce)
    );
    let poll_url = format!(
        "{OPEN_API}{DEVICE_POLL_PATH}?nonce={}&verifier={}&challenge_method=S256",
        encode_component(&nonce),
        encode_component(&verifier)
    );
    Ok(Started {
        state,
        auth_url,
        flow: Arc::new(QoderFlow {
            client: client.clone(),
            poll_url,
        }),
    })
}

pub fn resume(_client: &reqwest::Client) -> Result<Arc<dyn LoginFlow>, String> {
    Err("Qoder 设备授权不支持回调地址回填：请等浏览器授权完成后自动入池".to_string())
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
