//! OfficeAce（果办）的网页登录：华为云 OAuth + PKCE + DPoP，轮询云端取授权码。
//!
//! ```text
//! ① POST {cloud}/v1/claw/auth/state                → state（无鉴权）
//! ② 本地生成 PKCE(S256) 与一对 P-256 DPoP 密钥
//! ③ 打开 https://auth.huaweicloud.com/authui/login.html?service=<内层 authorize>
//!      内层：/authui/v1/oauth2/authorize?client_id=pdp5_for_agentarts
//!            &code_challenge(SHA-256)&state&scope=openid&response_type=code
//!            &redirect_uri=<云端回调>（**不往本机跳**，所以只能轮询）
//! ④ GET  {cloud}/v1/claw/auth/code?state=<state>
//!      202 = 还没登完；404 = state 作废；200 = 授权码
//! ⑤ POST {sts}/v1/oauth2/tokens（表单 + `DPoP:` 头）→ 临时 AK/SK/STS
//! ⑥ GET  {cloud}/v1/claw/client-permission-validate（SDK-HMAC-SHA256 签名）
//!      → 身份 + `subscription.model_info`（可能为空 = 尚未开通）
//! ⑥a GET {iam}/v3/projects（仅 project_id 为空时）按 region 补项目号
//! ⑥b POST {cloud}/v1/claw/client-subscription（model_info 为空时开通）
//! ⑦ 落账号：`secret` = base64(model_app_key:model_app_secret)（Basic 不过期），
//!    `base_url` = 模型网关地址（规范化到 `/v2`）
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::codearts::DpopKey;
use super::{Credentials, LoginFlow, Started};

/// 云端总控基址（`/v1/claw/*` 都挂在这里）。
const CLOUD_BASE: &str = "https://agentarts.cn-southwest-2.myhuaweicloud.com";
/// 华为云登录页基址。
const AUTH_BASE: &str = "https://auth.huaweicloud.com";
/// 固定的 client_id（官方实现同款）。
const CLIENT_ID: &str = "pdp5_for_agentarts";
/// 授权回调**指向云端**（不是本机 loopback）。
const REDIRECT_URI: &str =
    "https://agentarts.cn-southwest-2.myhuaweicloud.com/v1/claw/auth/callback";
/// 令牌端点（与 CodeArts 同一个 STS 主机）。
const TOKEN_URL: &str = "https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens";
/// IAM keystone 基址（补查 project_id）。
const IAM_BASE: &str = "https://iam.myhuaweicloud.com";
/// 控制面签名用的区域。
const REGION: &str = "cn-southwest-2";
/// 官方实现 2.5 秒；这里取整到 3 秒。
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// 控制面订阅族的通行头。
const SUBSCRIPTION_TYPE: (&str, &str) = ("x-subscription-type", "v2");
const SIGNING_ALGORITHM: &str = "SDK-HMAC-SHA256";
/// `sha256("")`，无体请求固定签这个。
const EMPTY_BODY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// 换来的临时控制面凭据（约 2 小时过期，只用于这次登录的最后两步）。
struct TempCredential {
    access_key_id: String,
    secret_access_key: String,
    security_token: String,
    project_id: String,
}

pub struct OfficeaceFlow {
    client: reqwest::Client,
    state: String,
    code_verifier: String,
    dpop: DpopKey,
}

#[async_trait]
impl LoginFlow for OfficeaceFlow {
    fn poll_interval(&self) -> Duration {
        POLL_INTERVAL
    }

    /// state 作废要立刻结束，别拖到任务超时。
    fn fatal(&self, error: &str) -> bool {
        error.contains("授权码已失效")
    }

    async fn poll(&self) -> Result<Option<Credentials>, String> {
        let url = format!(
            "{CLOUD_BASE}/v1/claw/auth/code?state={}",
            encode_component(self.state.as_bytes())
        );
        let response = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| format!("轮询授权码失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        match status {
            // 用户还没在浏览器里点完 —— 正常，继续等
            202 => return Ok(None),
            // state 不认识或过期：立即判作废
            404 => {
                return Err("授权码已失效或 state 不认识，请重新发起登录".to_string());
            }
            code_status if !(200..300).contains(&code_status) => {
                return Err(format!("取授权码返回 HTTP {code_status}"));
            }
            _ => {}
        }
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if let Some(error) = payload.get("error").and_then(Value::as_str) {
            let message = payload
                .get("error_msg")
                .and_then(Value::as_str)
                .unwrap_or(error);
            return Err(format!("云端拒绝：{message}"));
        }
        let code = payload
            .get("code")
            .and_then(Value::as_str)
            .or_else(|| payload.get("authorization_code").and_then(Value::as_str))
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if code.is_empty() {
            return Err("云端说好了，但没给授权码".to_string());
        }
        self.finish(&code).await.map(Some)
    }
}

impl OfficeaceFlow {
    /// ⑤⑥⑦：授权码 → 临时凭据 → 权限校验 →（未开通就开通）→ 网关 Basic 那对。
    async fn finish(&self, code: &str) -> Result<Credentials, String> {
        let (mut temporary, payload) = self.exchange_code(code).await?;
        if temporary.project_id.is_empty() {
            if let Some(project_id) = resolve_project_id(&self.client, &temporary).await {
                temporary.project_id = project_id;
            }
        }
        let (account_id, permission_principal, model_info) =
            self.fetch_permission(&temporary).await?;
        // `model_info` 为空 = 账号尚未开通（正常状态，不是错误）
        let model_info = if model_info.is_object() {
            model_info
        } else {
            subscribe_model_info(&self.client, &temporary).await?
        };
        let (base_url, app_key, app_secret) = gateway_of(&model_info)?;
        let secret = STANDARD.encode(format!("{app_key}:{app_secret}").as_bytes());
        let (user_name, token_principal) = response_identity(&payload);
        let name = first_non_empty(&[
            user_name,
            token_principal,
            permission_principal,
            account_id,
            "OfficeAce".to_string(),
        ]);
        Ok(Credentials {
            name,
            secret,
            base_url: Some(base_url),
            extra_headers: HashMap::new(),
            expires_at: None,
        })
    }

    async fn exchange_code(&self, code: &str) -> Result<(TempCredential, Value), String> {
        let proof = self.dpop.proof("POST", TOKEN_URL)?;
        let form = [
            ("client_id", CLIENT_ID),
            ("code", code.trim()),
            ("code_verifier", self.code_verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", REDIRECT_URI),
        ];
        let response = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .header("DPoP", proof)
            .form(&form)
            .send()
            .await
            .map_err(|error| format!("换取临时凭据失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let detail = payload
                .get("error_description")
                .and_then(Value::as_str)
                .or_else(|| payload.get("error").and_then(Value::as_str))
                .unwrap_or("");
            return Err(if detail.is_empty() {
                format!("令牌端点被拒（HTTP {status}）")
            } else {
                format!("令牌端点被拒（HTTP {status}）：{detail}")
            });
        }
        let credentials = payload.get("credentials").cloned().unwrap_or(Value::Null);
        let read = |name: &str| {
            credentials
                .get(name)
                .or_else(|| payload.get(name))
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        let access_key_id = read("access_key_id");
        let secret_access_key = read("secret_access_key");
        if access_key_id.is_empty() || secret_access_key.is_empty() {
            return Err(format!("令牌端点没返回临时凭据（HTTP {status}）"));
        }
        Ok((
            TempCredential {
                access_key_id,
                secret_access_key,
                security_token: read("security_token"),
                project_id: read("project_id"),
            },
            payload,
        ))
    }

    /// ⑥ 权限校验：返回 `(account_id, principal_id, model_info)`。
    async fn fetch_permission(
        &self,
        temporary: &TempCredential,
    ) -> Result<(String, String, Value), String> {
        let url = format!("{CLOUD_BASE}/v1/claw/client-permission-validate");
        let headers = sign(
            "GET",
            &url,
            &[(
                SUBSCRIPTION_TYPE.0.to_string(),
                SUBSCRIPTION_TYPE.1.to_string(),
            )],
            b"",
            temporary,
        )?;
        let mut request = self.client.get(&url);
        for (name, value) in &headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|error| format!("权限校验失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let detail = payload
                .get("error_msg")
                .and_then(Value::as_str)
                .or_else(|| payload.get("error_code").and_then(Value::as_str))
                .unwrap_or("");
            return Err(if detail.is_empty() {
                format!("权限校验失败（HTTP {status}）")
            } else {
                format!("权限校验失败（HTTP {status}）：{detail}")
            });
        }
        // `model_info` 的两种位置（实测）：validate 嵌在 `subscription.model_info`；
        // 开通那一发在顶层。两层都兜，非对象一律当「没有」。
        let model_info = payload
            .get("model_info")
            .or_else(|| {
                payload
                    .get("subscription")
                    .and_then(|value| value.get("model_info"))
            })
            .or_else(|| payload.get("modelInfo"))
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or(Value::Null);
        let account_id = payload
            .get("account_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let principal_id = payload
            .get("principal_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        Ok((account_id, principal_id, model_info))
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let state = fetch_state(client).await?;
    // PKCE：verifier 是 43–128 位随机串；challenge = base64url(sha256(verifier))
    let code_verifier = URL_SAFE_NO_PAD.encode(random_bytes()?);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let dpop = DpopKey::generate()?;
    let auth_url = build_authorize_url(&state, &challenge);
    Ok(Started {
        state: state.clone(),
        auth_url,
        flow: Arc::new(OfficeaceFlow {
            client: client.clone(),
            state,
            code_verifier,
            dpop,
        }),
    })
}

/// ① 向云端要一个 state（**无鉴权**）。
async fn fetch_state(client: &reqwest::Client) -> Result<String, String> {
    let url = format!("{CLOUD_BASE}/v1/claw/auth/state");
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body("{}")
        .send()
        .await
        .map_err(|error| format!("向云端申请 state 失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let state = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|payload| {
            payload
                .get("state")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        });
    state.ok_or_else(|| format!("云端没给出 state（HTTP {status}）"))
}

/// ③ 拼授权地址（官方写法：外层 `login.html?service=<内层 authorize>`）。
fn build_authorize_url(state: &str, challenge: &str) -> String {
    let inner = format!(
        "{AUTH_BASE}/authui/v1/oauth2/authorize?client_id={CLIENT_ID}\
         &code_challenge={}&code_challenge_method=SHA-256&state={}\
         &scope=openid&redirect_uri={}&response_type=code",
        encode_component(challenge.as_bytes()),
        encode_component(state.as_bytes()),
        encode_component(REDIRECT_URI.as_bytes()),
    );
    format!(
        "{AUTH_BASE}/authui/login.html?service={}",
        encode_component(inner.as_bytes())
    )
}

/// ⑥a 补查 `project_id`：`GET {iam}/v3/projects` 里按 region 名匹配启用中的项目。
///
/// `None` = 没查到（凭据不可用 / 网络失败 / 没有该 region 的项目），不阻断登录。
async fn resolve_project_id(
    client: &reqwest::Client,
    credential: &TempCredential,
) -> Option<String> {
    let url = format!("{IAM_BASE}/v3/projects");
    let headers = sign("GET", &url, &[], b"", credential).ok()?;
    let mut request = client.get(&url);
    for (name, value) in &headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request.send().await.ok()?;
    if !(200..300).contains(&response.status().as_u16()) {
        return None;
    }
    let payload = response.json::<Value>().await.ok()?;
    let projects = payload.get("projects").and_then(Value::as_array)?;
    projects.iter().find_map(|project| {
        let name = project.get("name").and_then(Value::as_str)?;
        let id = project.get("id").and_then(Value::as_str)?;
        let enabled = project
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        (name == REGION && enabled && !id.is_empty()).then(|| id.to_string())
    })
}

/// ⑥b 开通：`POST {cloud}/v1/claw/client-subscription`，成功返回**顶层** `model_info`。
async fn subscribe_model_info(
    client: &reqwest::Client,
    credential: &TempCredential,
) -> Result<Value, String> {
    let url = format!("{CLOUD_BASE}/v1/claw/client-subscription");
    let body = "{}";
    let headers = sign(
        "POST",
        &url,
        &[
            (
                "Content-Type".to_string(),
                "application/json;charset=utf8".to_string(),
            ),
            (
                SUBSCRIPTION_TYPE.0.to_string(),
                SUBSCRIPTION_TYPE.1.to_string(),
            ),
        ],
        body.as_bytes(),
        credential,
    )?;
    let mut request = client.post(&url).body(body);
    for (name, value) in &headers {
        request = request.header(name.as_str(), value.as_str());
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("开通请求失败：{error}"))?;
    let status = response.status().as_u16();
    let payload = response.json::<Value>().await.unwrap_or(Value::Null);
    if !(200..300).contains(&status) {
        let error_code = payload
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("");
        let detail = payload
            .get("error_msg")
            .and_then(Value::as_str)
            .or_else(|| payload.get("error_message").and_then(Value::as_str))
            .unwrap_or("");
        let shown = if detail.is_empty() {
            error_code
        } else {
            detail
        };
        return Err(if shown.is_empty() {
            format!("开通失败（HTTP {status}）")
        } else {
            format!("开通失败（HTTP {status}）：{shown}")
        });
    }
    payload
        .get("model_info")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| "开通成功但云端仍未返回模型网关信息，请稍后重试".to_string())
}

/// 从 `model_info` 解网关三件套：基址（规范化到 `/v2`）+ app key/secret。
fn gateway_of(model_info: &Value) -> Result<(String, String, String), String> {
    let auth_info = model_info.get("model_auth_info").unwrap_or(&Value::Null);
    let app_key = auth_info
        .get("model_app_key")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let app_secret = auth_info
        .get("model_app_secret")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if app_key.is_empty() || app_secret.is_empty() {
        return Err(
            "云端返回了模型信息，但缺少模型网关凭据（model_app_key / model_app_secret）"
                .to_string(),
        );
    }
    let host = model_info
        .get("model_api_url_base")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if host.is_empty() {
        return Err("云端返回的模型网关地址为空".to_string());
    }
    let base_url = if host.ends_with("/v2") {
        format!("https://{host}")
    } else {
        format!("https://{host}/v2")
    };
    Ok((base_url, app_key, app_secret))
}

/// SDK-HMAC-SHA256 签名：返回**应当发出去**的完整头集合（含 `Authorization`）。
fn sign(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &[u8],
    credential: &TempCredential,
) -> Result<Vec<(String, String)>, String> {
    if credential.access_key_id.is_empty() || credential.secret_access_key.is_empty() {
        return Err("临时凭据缺少 AK/SK".to_string());
    }
    let parsed = url::Url::parse(url).map_err(|error| format!("地址无效：{error}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| "地址缺少主机名".to_string())?;
    let authority = match parsed.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    };
    let date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut items: Vec<(String, String)> = headers.to_vec();
    items.push(("Host".to_string(), authority));
    items.push(("X-Sdk-Date".to_string(), date.clone()));
    if !credential.security_token.is_empty() {
        items.push((
            "X-Security-Token".to_string(),
            credential.security_token.clone(),
        ));
    }
    if !credential.project_id.is_empty() {
        items.push(("X-Project-ID".to_string(), credential.project_id.clone()));
    }

    let mut canonical: Vec<(String, String)> = items
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    canonical.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_headers: String = canonical
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = canonical
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let payload_hash = if body.is_empty() {
        EMPTY_BODY_SHA256.to_string()
    } else {
        sha256_hex(body)
    };
    let canonical_request = [
        method.to_ascii_uppercase(),
        canonical_uri(parsed.path()),
        canonical_query(parsed.query().unwrap_or("")),
        canonical_headers,
        signed_headers.clone(),
        payload_hash,
    ]
    .join("\n");
    let digest = sha256_hex(canonical_request.as_bytes());
    let string_to_sign = [SIGNING_ALGORITHM, date.as_str(), digest.as_str()].join("\n");
    let signature = hmac_sha256_hex(credential.secret_access_key.as_bytes(), &string_to_sign);
    items.push((
        "Authorization".to_string(),
        format!(
            "{SIGNING_ALGORITHM} Access={}, SignedHeaders={signed_headers}, Signature={signature}",
            credential.access_key_id
        ),
    ));
    Ok(items)
}

/// 取第一个非空字符串（调用方保证列表末尾有兜底）。
fn first_non_empty(values: &[String]) -> String {
    values
        .iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or("")
        .to_string()
}

fn random_bytes() -> Result<Vec<u8>, String> {
    let seed = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    hex::decode(seed).map_err(|_| "随机数不可用".to_string())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hmac_sha256_hex(key: &[u8], message: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(message.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

/// 未保留字符集编码（A-Z a-z 0-9 - . _ ~ 原样，其余 `%XX` 大写）。
fn encode_component(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn percent_decode(raw: &str) -> Option<String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = (bytes[index + 1] as char).to_digit(16)?;
                let low = (bytes[index + 2] as char).to_digit(16)?;
                out.push(((high << 4) | low) as u8);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// `canonicalURI`：逐段编码并保证以 `/` 结尾。
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut escaped: String = path
        .split('/')
        .map(|segment| encode_component(segment.as_bytes()))
        .collect::<Vec<_>>()
        .join("/");
    if !escaped.ends_with('/') {
        escaped.push('/');
    }
    escaped
}

/// `canonicalQueryString`：解码后重新编码，按键再按值排序。
fn canonical_query(raw: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for part in raw.split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = match part.split_once('=') {
            Some((key, value)) => (key, value),
            None => (part, ""),
        };
        let Some(key) = percent_decode(key) else {
            continue;
        };
        let Some(value) = percent_decode(value) else {
            continue;
        };
        pairs.push((
            encode_component(key.as_bytes()),
            encode_component(value.as_bytes()),
        ));
    }
    pairs.sort();
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// 从令牌端点的响应体里取身份：`id_token` 优先，其次 `refresh_token`。
fn response_identity(payload: &Value) -> (String, String) {
    let mut principal_only = String::new();
    for field in ["id_token", "refresh_token"] {
        let token = payload.get(field).and_then(Value::as_str).unwrap_or("");
        let (user_name, principal_id) = jwt_identity(token);
        if !user_name.is_empty() {
            return (user_name, principal_id);
        }
        if principal_only.is_empty() && !principal_id.is_empty() {
            principal_only = principal_id;
        }
    }
    (String::new(), principal_only)
}

/// 从一枚 JWT（`id_token` / `refresh_token`）的 payload 取显示名与用户级 principal id。
fn jwt_identity(token: &str) -> (String, String) {
    let Some(claims) = jwt_claims(token) else {
        return (String::new(), String::new());
    };
    let profile = claims
        .get("user_profile")
        .and_then(Value::as_str)
        .and_then(decode_profile);
    let principal_id = profile
        .as_ref()
        .and_then(|value| text_of(value, "principal_id"))
        .or_else(|| text_of(&claims, "principal_id"))
        .unwrap_or_default();
    for field in ["preferred_username", "name"] {
        if let Some(text) = text_of(&claims, field) {
            return (text, principal_id);
        }
    }
    let user_name = profile
        .as_ref()
        .and_then(|value| text_of(value, "account_name"))
        .unwrap_or_default();
    (user_name, principal_id)
}

/// 解 JWT 的 payload 段（不验签，只取展示名）。
fn jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('=').as_bytes())
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()
}

/// 解 `user_profile`：一段无填充 base64 的 JSON（两种字母表都试）。
fn decode_profile(text: &str) -> Option<Value> {
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    let body = text.trim().trim_end_matches('=');
    if body.is_empty() {
        return None;
    }
    for candidate in [URL_SAFE_NO_PAD.decode(body), STANDARD_NO_PAD.decode(body)] {
        let Ok(bytes) = candidate else { continue };
        if let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) {
            return Some(parsed);
        }
    }
    None
}

fn text_of(container: &Value, field: &str) -> Option<String> {
    container
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorize_url_points_at_the_cloud_callback() {
        let url = build_authorize_url("st123", "ch456");
        assert!(
            url.starts_with("https://auth.huaweicloud.com/authui/login.html?service="),
            "{url}"
        );
        assert!(url.contains("client_id%3Dpdp5_for_agentarts"), "{url}");
        assert!(url.contains("state%3Dst123"), "{url}");
        assert!(url.contains("code_challenge%3Dch456"), "{url}");
        assert!(
            url.contains("agentarts.cn-southwest-2.myhuaweicloud.com"),
            "{url}"
        );
        assert!(!url.contains("127.0.0.1"), "回调不能是本机 loopback：{url}");
    }

    #[test]
    fn gateway_base_url_is_normalized_to_v2() {
        let model_info = serde_json::json!({
            "model_auth_info": {"model_app_key": "k", "model_app_secret": "s"},
            "model_api_url_base": "https://modelgw.example.com",
        });
        let (base, key, secret) = gateway_of(&model_info).expect("应能解析");
        assert_eq!("https://modelgw.example.com/v2", base);
        assert_eq!("k", key);
        assert_eq!("s", secret);

        let model_info = serde_json::json!({
            "model_auth_info": {"model_app_key": "k", "model_app_secret": "s"},
            "model_api_url_base": "https://modelgw.example.com/v2",
        });
        let (base, _, _) = gateway_of(&model_info).expect("应能解析");
        assert_eq!("https://modelgw.example.com/v2", base);
    }

    #[test]
    fn signature_header_has_expected_shape() {
        let credential = TempCredential {
            access_key_id: "AK".to_string(),
            secret_access_key: "SK".to_string(),
            security_token: "STS".to_string(),
            project_id: "PID".to_string(),
        };
        let headers = sign(
            "POST",
            "https://example.invalid/v1/claw/client-subscription",
            &[(
                "Content-Type".to_string(),
                "application/json;charset=utf8".to_string(),
            )],
            b"{}",
            &credential,
        )
        .expect("应能签名");
        let authorization = headers
            .iter()
            .find(|(name, _)| name == "Authorization")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        assert!(
            authorization.starts_with("SDK-HMAC-SHA256 Access=AK"),
            "{authorization}"
        );
        assert!(authorization.contains("SignedHeaders="));
        assert!(authorization.contains("Signature="));
        assert!(headers.iter().any(|(name, _)| name == "X-Security-Token"));
        assert!(headers.iter().any(|(name, _)| name == "X-Project-ID"));
    }

    #[test]
    fn canonical_uri_and_query_follow_the_official_shape() {
        assert_eq!(
            canonical_uri("/v1/claw/client-permission-validate"),
            "/v1/claw/client-permission-validate/"
        );
        assert_eq!(canonical_uri(""), "/");
        assert_eq!(canonical_query("b=2&a=1"), "a=1&b=2");
    }

    #[test]
    fn identity_reads_the_nested_user_profile() {
        let profile = URL_SAFE_NO_PAD
            .encode(br#"{"account_name":"hid_synthetic-1","principal_id":"prin_1"}"#);
        let claims = format!(r#"{{"user_profile":"{profile}"}}"#);
        let token = format!("h.{}.s", URL_SAFE_NO_PAD.encode(claims.as_bytes()));
        let (name, principal) = jwt_identity(&token);
        assert_eq!("hid_synthetic-1", name);
        assert_eq!("prin_1", principal);

        let bare = format!("h.{}.s", URL_SAFE_NO_PAD.encode(br#"{"sub":"u1"}"#));
        let (name, principal) = jwt_identity(&bare);
        assert!(name.is_empty() && principal.is_empty());
    }
}
