//! Cline 的设备授权登录（WorkOS RFC 8628）。
//!
//! ```text
//! ① POST https://api.workos.com/user_management/authorize/device
//!      body: client_id=<WORKOS_CLIENT_ID>
//!    → device_code / user_code / verification_uri_complete / interval / expires_in
//! ② 打开 verification_uri_complete（用户确认设备码）
//! ③ POST https://api.workos.com/user_management/authenticate（轮询）
//!      grant_type=urn:ietf:params:oauth:grant-type:device_code
//!      &device_code=…&client_id=…
//!    authorization_pending / slow_down = 继续等；expired_token / access_denied = 终止
//! ④ POST https://api.cline.bot/api/v1/auth/register 把 WorkOS 令牌换成 Cline 会话令牌
//!    （不能省：拿 WorkOS 令牌直接打 LLM 端点会被拒）
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Credentials, LoginFlow, Started};

const WORKOS_BASE_URL: &str = "https://api.workos.com";
const WORKOS_CLIENT_ID: &str = "client_01K3A541FN8TA3EPPHTD2325AR";
const CLINE_API_BASE: &str = "https://api.cline.bot/api/v1";
const CLIENT_TYPE: &str = "cline-sdk";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

pub struct ClineFlow {
    client: reqwest::Client,
    device_code: String,
    interval: Duration,
}

#[async_trait]
impl LoginFlow for ClineFlow {
    fn poll_interval(&self) -> Duration {
        self.interval
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let response = self
            .client
            .post(format!("{WORKOS_BASE_URL}/user_management/authenticate"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(format!(
                "grant_type={DEVICE_GRANT}&device_code={}&client_id={WORKOS_CLIENT_ID}",
                self.device_code
            ))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            let access = payload
                .get("access_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            let refresh = payload
                .get("refresh_token")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if access.is_empty() {
                return Err("Cline 登录响应缺少 access_token".to_string());
            }
            let session = self.register_session(&access, &refresh).await?;
            return Ok(Some(Credentials {
                name: "Cline".to_string(),
                secret: session.0,
                base_url: None,
                extra_headers: HashMap::new(),
                expires_at: None,
            }));
        }
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        match error {
            "authorization_pending" | "slow_down" | "" => Ok(None),
            "expired_token" => Err("Cline 设备授权已过期，请重新发起".to_string()),
            "access_denied" => Err("Cline 授权被拒绝".to_string()),
            other => Err(format!("Cline 授权失败：{other}")),
        }
    }
}

impl ClineFlow {
    async fn register_session(
        &self,
        access: &str,
        refresh: &str,
    ) -> Result<(String, String), String> {
        let response = self
            .client
            .post(format!("{CLINE_API_BASE}/auth/register"))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-CLIENT-TYPE", CLIENT_TYPE)
            .json(&json!({
                "accessToken": access,
                "refreshToken": refresh,
            }))
            .send()
            .await
            .map_err(|error| format!("Cline 令牌登记失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            return Err(format!("Cline 令牌登记失败（HTTP {status}）"));
        }
        let data = payload.get("data").cloned().unwrap_or(payload);
        let access = data
            .get("accessToken")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let refresh = data
            .get("refreshToken")
            .and_then(Value::as_str)
            .unwrap_or(refresh)
            .trim()
            .to_string();
        if access.is_empty() {
            return Err("Cline 令牌登记响应缺少 accessToken".to_string());
        }
        Ok((ensure_workos_prefix(&access), refresh))
    }
}

fn ensure_workos_prefix(token: &str) -> String {
    let token = token.trim();
    if token.to_ascii_lowercase().starts_with("workos:") {
        token.to_string()
    } else {
        format!("workos:{token}")
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let response = client
        .post(format!(
            "{WORKOS_BASE_URL}/user_management/authorize/device"
        ))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(format!("client_id={WORKOS_CLIENT_ID}"))
        .send()
        .await
        .map_err(|error| format!("Cline 设备授权请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let detail = payload
            .get("error_description")
            .or_else(|| payload.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let detail = if detail.is_empty() {
            format!("HTTP {status}")
        } else {
            detail
        };
        return Err(format!("Cline 设备授权失败：{detail}"));
    }
    let device_code = payload
        .get("device_code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if device_code.is_empty() {
        return Err("Cline 设备授权响应缺少 device_code".to_string());
    }
    let auth_url = payload
        .get("verification_uri_complete")
        .or_else(|| payload.get("verification_uri"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "Cline 设备授权响应缺少授权地址".to_string())?;
    let interval = Duration::from_secs(
        payload
            .get("interval")
            .and_then(Value::as_u64)
            .unwrap_or(5)
            .max(1),
    );
    Ok(Started {
        state: device_code.clone(),
        auth_url,
        flow: Arc::new(ClineFlow {
            client: client.clone(),
            device_code,
            interval,
        }),
    })
}
