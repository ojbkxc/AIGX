//! ChatGPT 订阅登录（Sign in with ChatGPT，OAuth + PKCE）。
//!
//! 对齐 Agent2API `chatgpt_subscription/{login,oidc,http}.rs`：
//! - authorize：`https://auth.openai.com/api/accounts/authorize`，PKCE(S256)；
//! - token：`https://auth.openai.com/api/accounts/oauth/token`，`grant_type=authorization_code`；
//! - scope 必须含 `chatgpt.tokens.use.direct`（授权本应用使用套餐）。
//!
//! ## 与上游的差异
//! - 上游在进程内绑定 loopback（默认 1455）自动收回调；AIGX 的登录引擎用
//!   `submit_callback`（粘贴回调地址）兜底，因此这里固定用
//!   `http://127.0.0.1:1455/auth/callback` 作为 redirect —— 浏览器打开失败也没关系，
//!   用户把地址栏里的完整回调 URL 粘回来即可。
//! - 上游用 JWKS 验签 ID token；AIGX 没有对应的验签缓存层，这里**只解码**
//!   ID token 取 sub/email/plan 作为展示与身份，不把它当作安全边界 ——
//!   真正的授权有效性由后续上游请求验证（令牌无效会 401）。
//!
//! 注意：普通 OpenAI API Key、Codex auth.json 均不被本流程接受。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

/// 授权端点 / token 端点 / 资源 / scope（对齐上游 `chatgpt_subscription/mod.rs`）。
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const DIRECT_SCOPE: &str = "chatgpt.tokens.use.direct";
/// AIGX 未绑定 loopback：固定一个 loopback 回调地址供粘贴。
const REDIRECT: &str = "http://127.0.0.1:1455/auth/callback";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

fn random_token(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    if getrandom::getrandom(&mut buffer).is_err() {
        // 极端情况下退化：用 uuid 的随机性，仍然不可预测到足以做 CSRF/PKCE。
        return uuid::Uuid::new_v4().simple().to_string();
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buffer)
}

/// 不带验签的 JWT payload 解码（仅取展示字段；见模块头说明）。
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

/// 登录进度：pending / 完成 / 失败。
enum Progress {
    Pending,
    Done(Credentials),
    Failed(String),
}

/// 一次进行中的 ChatGPT 登录。
pub struct ChatgptFlow {
    client: reqwest::Client,
    verifier: String,
    state: String,
    redirect: String,
    progress: Mutex<Progress>,
    /// 登录成功后要随账号落盘的具名字段（在 exchange 阶段填好）。
    fields: Mutex<HashMap<String, String>>,
    refresh: Mutex<String>,
}

impl ChatgptFlow {
    async fn exchange(&self, code: &str, client_id: &str) -> Result<Credentials, String> {
        let form = [
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", self.verifier.as_str()),
            ("redirect_uri", self.redirect.as_str()),
            ("resource", RESOURCE),
        ];
        let response = self
            .client
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .form(&form)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|error| format!("ChatGPT 换取登录凭证失败：{error}"))?;
        let status = response.status().as_u16();
        let value: Value = response
            .json()
            .await
            .map_err(|error| format!("ChatGPT 令牌响应无法解析：{error}"))?;
        if !(200..300).contains(&status) {
            let message = value
                .get("error_description")
                .and_then(Value::as_str)
                .or_else(|| value.get("error").and_then(Value::as_str))
                .unwrap_or("ChatGPT 授权失败");
            return Err(message.to_string());
        }
        let access = value
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "OpenAI 未返回访问令牌".to_string())?
            .to_string();
        let refresh = value
            .get("refresh_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id_token = value
            .get("id_token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let expires_in = value
            .get("expires_in")
            .and_then(Value::as_f64)
            .filter(|seconds| *seconds > 0.0)
            .ok_or_else(|| "OpenAI 未返回有效期".to_string())?;
        let scope = value.get("scope").and_then(Value::as_str).unwrap_or("");
        let scopes: Vec<String> = scope.split_whitespace().map(str::to_string).collect();
        let authorized = scopes.iter().any(|item| item == DIRECT_SCOPE);
        let expires_at = (chrono::Utc::now().timestamp() as f64 + expires_in) as i64;

        let claims = jwt_claims(&id_token);
        let subject = claim_str(&claims, &["sub"]);
        let email = claim_str(&claims, &["email"]);
        let plan = claims
            .as_ref()
            .and_then(|c| c.get("https://api.openai.com/auth"))
            .and_then(|auth| auth.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // 名字：邮箱优先（与上游 `email_as_name` 一致）。
        let name = if email.is_empty() {
            "ChatGPT".to_string()
        } else {
            email.clone()
        };

        {
            let mut fields = self.fields.lock().unwrap_or_else(|e| e.into_inner());
            fields.insert("issuer".to_string(), ISSUER.to_string());
            fields.insert("clientId".to_string(), client_id.to_string());
            fields.insert(
                "hostId".to_string(),
                // 上游用宿主指纹隔离；AIGX 用固定标识即可（仅展示/日志用）。
                "aigx-gateway".to_string(),
            );
            fields.insert("sub".to_string(), subject);
            fields.insert("email".to_string(), email);
            if !id_token.is_empty() {
                fields.insert("idToken".to_string(), id_token);
            }
            fields.insert("scopes".to_string(), json!(scopes).to_string());
            fields.insert("plan".to_string(), plan);
            fields.insert(
                "needsRelogin".to_string(),
                if authorized { "false" } else { "true" }.to_string(),
            );
        }
        {
            let mut slot = self.refresh.lock().unwrap_or_else(|e| e.into_inner());
            *slot = refresh.clone();
        }
        Ok(Credentials {
            name,
            secret: access,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: Some(expires_at),
        })
    }
}

#[async_trait]
impl LoginFlow for ChatgptFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        match &*progress {
            Progress::Pending => Ok(None),
            Progress::Done(creds) => Ok(Some(creds.clone())),
            Progress::Failed(message) => Err(message.clone()),
        }
    }

    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed =
            url::Url::parse(url.trim()).map_err(|_| "请粘贴完整的 ChatGPT 回调地址".to_string())?;
        let mut code = None;
        let mut state = None;
        let mut client_id = None;
        let mut error = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "client_id" => client_id = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                _ => {}
            }
        }
        if let Some(error) = error {
            let message = format!("ChatGPT 授权被拒绝：{error}");
            *self.progress.lock().unwrap_or_else(|e| e.into_inner()) =
                Progress::Failed(message.clone());
            return Err(message);
        }
        if state.as_deref() != Some(self.state.as_str()) {
            return Err("回调地址与本次登录不匹配（state 不一致），请重新复制完整地址".to_string());
        }
        let code = code
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "回调地址里没有授权码（code），请重新复制完整地址".to_string())?;
        let client_id = client_id
            .filter(|value| !value.is_empty() && value != DYNAMIC_CLIENT)
            .unwrap_or_else(|| DYNAMIC_CLIENT.to_string());
        let credentials = self.exchange(&code, &client_id).await?;
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) =
            Progress::Done(credentials.clone());
        Ok(Some(credentials))
    }

    fn poll_interval(&self) -> Duration {
        // 粘贴回调型：轮询只是把 submit_callback 的结果带回任务表。
        Duration::from_millis(500)
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

/// 发起 ChatGPT 订阅登录。
pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let state = random_token(24);
    let verifier = random_token(48);
    let nonce = random_token(24);
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    let mut url = url::Url::parse(AUTHORIZE_URL).map_err(|_| "ChatGPT 授权地址无效".to_string())?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", DYNAMIC_CLIENT)
            .append_pair("ext_agent_host_id", "aigx-gateway")
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", REDIRECT)
            .append_pair("resource", RESOURCE)
            .append_pair("scope", SCOPES)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("agent_name_hint", "AIGX");
    }
    let flow = Arc::new(ChatgptFlow {
        client: client.clone(),
        verifier,
        state: state.clone(),
        redirect: REDIRECT.to_string(),
        progress: Mutex::new(Progress::Pending),
        fields: Mutex::new(HashMap::new()),
        refresh: Mutex::new(String::new()),
    });
    Ok(Started {
        state,
        auth_url: url.to_string(),
        flow,
    })
}
