//! Codex 订阅登录（官方设备授权，OAuth device flow）。
//!
//! 对齐 Agent2API `codex_subscription/login.rs`：
//! ① `POST https://auth.openai.com/api/accounts/deviceauth/usercode` 取验证码；
//! ② 用户在官方页面输入验证码后，轮询
//!    `POST .../api/accounts/deviceauth/token`；
//! ③ 拿到 `authorization_code` + `code_verifier` 后到
//!    `POST https://auth.openai.com/oauth/token` 换 access/refresh/id token。
//!
//! 只接收 ChatGPT 订阅登录态；API Key / 其它登录方式的凭据一律拒绝
//! （`authMode` 固定 `chatgpt`）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

const USERCODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// 官方公开的 Codex OAuth 客户端（与上游 `codex_subscription/http.rs` 一致）。
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_PAGE: &str = "https://auth.openai.com/codex/device";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(25);

/// 一次设备授权流程。
pub struct CodexDeviceFlow {
    client: reqwest::Client,
    device_id: String,
    code: String,
    interval: Duration,
    done: Mutex<Option<Result<Credentials, String>>>,
    fields: Mutex<HashMap<String, String>>,
    refresh: Mutex<String>,
}

fn jwt_claims(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn claim_str(claims: &Option<Value>, keys: &[&str]) -> String {
    let Some(claims) = claims else {
        return String::new();
    };
    for key in keys {
        if let Some(value) = claims.get(*key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return value.trim().to_string();
            }
        }
    }
    String::new()
}

/// 把毫秒/秒时间戳归一到**秒**。
fn timestamp_secs(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    let raw = value
        .as_i64()
        .or_else(|| value.as_str()?.trim().parse().ok())?;
    if raw <= 0 {
        return None;
    }
    Some(if raw > 100_000_000_000 {
        raw / 1000
    } else {
        raw
    })
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

impl CodexDeviceFlow {
    /// 轮询一次设备授权；`None` = 用户还没确认。
    async fn try_complete(&self) -> Result<Option<Credentials>, String> {
        let response = self
            .client
            .post(DEVICE_TOKEN_URL)
            .header("originator", "codex_cli_rs")
            .json(&json!({
                "device_auth_id": self.device_id.clone(),
                "user_code": self.code.clone(),
            }))
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("Codex 查询授权进度失败：{error}"))?;
        let status = response.status().as_u16();
        // 未确认时上游回 403/404：继续等。
        if matches!(status, 403 | 404) {
            return Ok(None);
        }
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("Codex 授权响应无法解析：{error}"))?;
        if !(200..300).contains(&status) {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex 设备授权失败");
            return Err(message.to_string());
        }
        let authorization_code = text(&value, "authorization_code");
        let code_verifier = text(&value, "code_verifier");
        if authorization_code.is_empty() || code_verifier.is_empty() {
            return Err("Codex 授权响应缺少换取凭据所需的信息".to_string());
        }
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", authorization_code),
            ("code_verifier", code_verifier),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
        ];
        let response = self
            .client
            .post(TOKEN_URL)
            .header("originator", "codex_cli_rs")
            .form(&form)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("Codex 换取登录凭据失败：{error}"))?;
        let status = response.status().as_u16();
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("Codex 令牌响应无法解析：{error}"))?;
        if !(200..300).contains(&status) {
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("Codex 换取登录凭据失败");
            return Err(message.to_string());
        }
        self.parse_tokens(&value).map(Some)
    }

    fn parse_tokens(&self, value: &Value) -> Result<Credentials, String> {
        let access = text(value, "access_token");
        let refresh = text(value, "refresh_token");
        let id_token = text(value, "id_token");
        if access.is_empty() || access.starts_with("sk-") {
            return Err("Codex 未返回有效的 ChatGPT 订阅访问令牌".to_string());
        }
        let id_claims = jwt_claims(id_token);
        let access_claims = jwt_claims(access);
        let auth_claim = |claims: &Option<Value>, key: &str| -> String {
            claims
                .as_ref()
                .and_then(|c| c.get("https://api.openai.com/auth"))
                .and_then(|auth| auth.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let account_id = {
            let from_body = text(value, "accountId");
            if !from_body.is_empty() {
                from_body.to_string()
            } else {
                let from_id = auth_claim(&id_claims, "chatgpt_account_id");
                if from_id.is_empty() {
                    auth_claim(&access_claims, "chatgpt_account_id")
                } else {
                    from_id
                }
            }
        };
        if account_id.is_empty() {
            return Err("Codex 凭据缺少 account_id，请重新授权".to_string());
        }
        let user_id = [
            auth_claim(&id_claims, "chatgpt_user_id"),
            auth_claim(&access_claims, "chatgpt_user_id"),
            claim_str(&id_claims, &["sub"]),
            claim_str(&access_claims, &["sub"]),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default();
        let email = [
            claim_str(&id_claims, &["email"]),
            claim_str(&access_claims, &["email"]),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default();
        let plan = [
            auth_claim(&id_claims, "chatgpt_plan_type"),
            auth_claim(&access_claims, "chatgpt_plan_type"),
        ]
        .into_iter()
        .find(|value| !value.is_empty())
        .unwrap_or_default();
        let expires_at =
            timestamp_secs(access_claims.as_ref().and_then(|c| c.get("exp"))).or_else(|| {
                value
                    .get("expires_in")
                    .and_then(Value::as_i64)
                    .filter(|seconds| *seconds > 0)
                    .map(|seconds| chrono::Utc::now().timestamp().saturating_add(seconds))
            });
        // 身份串与上游同形：accountId + 用户身份（userId 优先，退邮箱）。
        let person = if user_id.is_empty() {
            email.to_ascii_lowercase()
        } else {
            user_id.clone()
        };
        let uid = format!(
            "codex-{:x}",
            Sha256::digest(format!("{account_id}\0{person}").as_bytes())
        );
        {
            let mut fields = self.fields.lock().unwrap_or_else(|e| e.into_inner());
            fields.insert("authMode".to_string(), "chatgpt".to_string());
            if !id_token.is_empty() {
                fields.insert("idToken".to_string(), id_token.to_string());
            }
            fields.insert("accountId".to_string(), account_id);
            fields.insert("userId".to_string(), user_id.clone());
            fields.insert("uid".to_string(), uid);
            fields.insert("email".to_string(), email.clone());
            fields.insert("planType".to_string(), plan);
        }
        {
            let mut slot = self.refresh.lock().unwrap_or_else(|e| e.into_inner());
            *slot = refresh.to_string();
        }
        let name = if email.is_empty() {
            "Codex 订阅".to_string()
        } else {
            email
        };
        Ok(Credentials {
            name,
            secret: access.to_string(),
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at,
        })
    }
}

#[async_trait]
impl LoginFlow for CodexDeviceFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        {
            let done = self.done.lock().unwrap_or_else(|e| e.into_inner());
            match &*done {
                Some(Ok(credentials)) => return Ok(Some(credentials.clone())),
                Some(Err(message)) => return Err(message.clone()),
                None => {}
            }
        }
        match self.try_complete().await {
            Ok(Some(credentials)) => {
                *self.done.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(Ok(credentials.clone()));
                Ok(Some(credentials))
            }
            Ok(None) => Ok(None),
            Err(message) => Err(message),
        }
    }

    fn poll_interval(&self) -> Duration {
        self.interval
    }

    fn extra_fields(&self) -> HashMap<String, String> {
        self.fields
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn refresh_token(&self) -> String {
        self.refresh
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// 发起 Codex 设备授权登录。
pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let response = client
        .post(USERCODE_URL)
        .header("originator", "codex_cli_rs")
        .header(
            "User-Agent",
            format!("codex_cli_rs/0.171.0 AIGX/{}", env!("CARGO_PKG_VERSION")),
        )
        .json(&json!({ "client_id": CLIENT_ID }))
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|error| format!("Codex 获取设备验证码失败：{error}"))?;
    let status = response.status().as_u16();
    if status == 404 {
        return Err(
            "当前账号或工作区未开放 Codex 设备授权，请在 ChatGPT 中开启，或导入已有 Codex 凭据"
                .to_string(),
        );
    }
    let value: Value = response
        .json()
        .await
        .map_err(|error| format!("Codex 设备验证码响应无法解析：{error}"))?;
    if !(200..300).contains(&status) {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("Codex 获取设备验证码失败");
        return Err(message.to_string());
    }
    let device_id = text(&value, "device_auth_id").to_string();
    let code = text(&value, "user_code").to_string();
    if device_id.is_empty() || code.is_empty() {
        return Err("Codex 没有返回有效的设备验证码".to_string());
    }
    let seconds = value
        .get("interval")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|seconds| *seconds > 0)
        .unwrap_or(5)
        .clamp(1, 30);
    let state = uuid::Uuid::new_v4().simple().to_string();
    let flow = Arc::new(CodexDeviceFlow {
        client: client.clone(),
        device_id,
        code: code.clone(),
        interval: Duration::from_secs(seconds),
        done: Mutex::new(None),
        fields: Mutex::new(HashMap::new()),
        refresh: Mutex::new(String::new()),
    });
    Ok(Started {
        state,
        // 验证码随地址一并带给前端（官方页面本身忽略该 query，仅用于展示）。
        // 真正的「把验证码展示给用户」还需登录接口回传 instructions（见报告待接线）。
        auth_url: format!("{DEVICE_PAGE}?user_code={code}"),
        flow,
    })
}
