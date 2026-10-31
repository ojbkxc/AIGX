//! TRAE（SOLO 通道）的网页登录（CN 站点）。
//!
//! ```text
//! ① POST {guidance} GetLoginGuidance → LoginHost（全挂时兜 https://api.trae.cn）
//! ② 打开 {loginHost}/authorization?…（PKCE + 设备指纹 + 回调地址）
//! ③ 浏览器回到 http://127.0.0.1:<port>/authorize?authCode=…（远程部署把地址栏粘回来）
//! ④ POST {host}/cloudide/api/v3/trae/oauth/ExchangeToken  body:
//!    {ClientID, AuthCode, CodeVerifier, IDEVersion, DeviceInfo{…DevicePublicKey…}}
//!    → Result.Token / RefreshToken / TokenExpireAt
//! ⑤ POST GetUserInfo（X-Cloudide-Token）取 uid / 昵称（best-effort）
//! ```
//!
//! 设备公钥是**每次登录现生成**的 EC P-256 SPKI PEM：空值会被上游判设备绑定
//! 拒绝（2xxxx）。私钥只用于这一轮登录，落账号时不再需要。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

const PLUGIN_VERSION: &str = "1.0.0";
const IDE_VERSION: &str = "0.1.61";
const DEVICE_NAME: &str = "DESKTOP-CPASOLO";
const DEVICE_TYPE: &str = "windows";
const DEVICE_BRAND: &str = "83DG";
const OS_VERSION: &str = "Windows 11 Pro";
const APP_TYPE: &str = "trae";
const SOLO_CLIENT_ID: &str = "en1oxy7wnw8j9n";
const CALLBACK_PATH: &str = "/authorize";
const EXCHANGE_PATH: &str = "/cloudide/api/v3/trae/oauth/ExchangeToken";
const USER_INFO_PATH: &str = "/cloudide/api/v3/trae/GetUserInfo";
const DEFAULT_LOGIN_HOST: &str = "https://api.trae.cn";
const GUIDANCE_URLS: [&str; 3] = [
    "https://api.trae.cn/cloudide/api/v3/trae/GetLoginGuidance",
    "https://api.trae.com.cn/cloudide/api/v3/trae/GetLoginGuidance",
    "https://www.trae.cn/cloudide/api/v3/trae/GetLoginGuidance",
];
const ACCOUNT_API_ORIGINS: [&str; 2] = ["https://api.trae.cn", "https://api.trae.com.cn"];
const CN_DEFAULT_ORIGINS: [&str; 3] = [
    "https://api.trae.cn",
    "https://api.trae.com.cn",
    "https://www.trae.cn",
];
/// 远程部署到不了网关 loopback；回调地址由前端手工粘回。
const REDIRECT_URI: &str = "http://127.0.0.1:1/authorize";

struct LoginContext {
    login_trace_id: String,
    code_verifier: String,
    code_challenge: String,
    device_id: String,
    machine_id: String,
    device_public_pem: String,
}

pub struct TraeFlow {
    client: reqwest::Client,
    login_host: String,
    context: LoginContext,
}

impl TraeFlow {
    async fn user_info(&self, token: &str) -> (String, String) {
        let body = json!({ "ReqSource": "IDE", "IDEVersion": IDE_VERSION });
        let url = format!("{}{USER_INFO_PATH}", self.login_host.trim_end_matches('/'));
        let response = match self
            .client
            .post(&url)
            .header("X-Cloudide-Token", token)
            .header(
                "User-Agent",
                format!("Trae/{PLUGIN_VERSION} antigravity-cockpit-tools"),
            )
            .json(&body)
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response,
            _ => return (String::new(), String::new()),
        };
        let payload = response.json::<Value>().await.unwrap_or(Value::Null);
        let source = payload
            .get("Result")
            .or_else(|| payload.get("result"))
            .filter(|value| value.is_object())
            .unwrap_or(&payload);
        let uid = text_of(source, &["UserID", "userId", "uid", "UID", "user_id"]);
        let name = text_of(
            source,
            &[
                "ScreenName",
                "screenName",
                "Nickname",
                "nickname",
                "Name",
                "name",
                "displayName",
            ],
        );
        (uid, name)
    }
}

#[async_trait]
impl LoginFlow for TraeFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        Ok(None)
    }

    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed =
            url::Url::parse(url.trim()).map_err(|_| "Trae 登录回调地址无效".to_string())?;
        if parsed.path() != CALLBACK_PATH {
            return Err("Trae 登录回调地址无效（应为 /authorize）".to_string());
        }
        let mut auth_code = String::new();
        let mut login_host = String::new();
        for (key, value) in parsed.query_pairs() {
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match key.as_ref() {
                "authCode" | "auth_code" | "AuthCode" | "authorization_code" | "code" => {
                    if auth_code.is_empty() {
                        auth_code = value;
                    }
                }
                "loginHost" | "login_host" | "LoginHost" | "host" | "consoleHost" => {
                    login_host = value;
                }
                "error" | "error_code" | "err" | "errorCode" => {
                    return Err(format!("Trae 授权回调错误：{key}={value}"))
                }
                _ => {}
            }
        }
        if auth_code.is_empty() {
            return Err("Trae 回调里没有授权码".to_string());
        }
        let host = if login_host.is_empty() {
            self.login_host.clone()
        } else if login_host.starts_with("http://") || login_host.starts_with("https://") {
            login_host
        } else {
            format!("https://{login_host}")
        };
        let urls = exchange_urls(&host);
        let body = json!({
            "ClientID": SOLO_CLIENT_ID,
            "AuthCode": auth_code,
            "CodeVerifier": self.context.code_verifier,
            "IDEVersion": IDE_VERSION,
            "DeviceInfo": device_info(&self.context),
        });
        let mut last_error = String::new();
        for url in urls {
            let response = match self
                .client
                .post(&url)
                .header("x-cloudide-token", "")
                .header(
                    "User-Agent",
                    format!("Trae/{PLUGIN_VERSION} antigravity-cockpit-tools"),
                )
                .json(&body)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("{url} => {error}");
                    continue;
                }
            };
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            if status >= 400 {
                last_error = format!("{url} => HTTP {status}");
                continue;
            }
            let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
            let result = payload
                .get("Result")
                .or_else(|| payload.get("result"))
                .filter(|value| value.is_object());
            let Some(result) = result else {
                last_error = format!("{url} => 换证响应缺少 Result");
                continue;
            };
            let token = text_of(result, &["Token", "token", "AccessToken", "accessToken"]);
            if token.is_empty() {
                last_error = format!("{url} => 换证响应缺少 access token");
                continue;
            }
            let expires_at = normalize_expires_at(
                result
                    .get("TokenExpireAt")
                    .or_else(|| result.get("tokenExpireAt"))
                    .or_else(|| result.get("ExpiresAt"))
                    .or_else(|| result.get("expiresAt"))
                    .and_then(timestamp_of)
                    .unwrap_or(0),
            );
            let (uid, nickname) = self.user_info(&token).await;
            let name = if nickname.is_empty() {
                if uid.is_empty() {
                    "Trae".to_string()
                } else {
                    format!("Trae {uid}")
                }
            } else {
                nickname
            };
            let mut headers = HashMap::new();
            headers.insert("x-machine-id".to_string(), self.context.machine_id.clone());
            headers.insert("x-device-id".to_string(), self.context.device_id.clone());
            if !uid.is_empty() {
                headers.insert("x-uid".to_string(), uid);
            }
            return Ok(Some(Credentials {
                name,
                secret: token,
                base_url: None,
                extra_headers: headers,
                expires_at,
            }));
        }
        Err(if last_error.is_empty() {
            "Trae 授权码换证失败".to_string()
        } else {
            format!("Trae 授权码换证失败：{last_error}")
        })
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let login_host = request_guidance(client).await;
    let context = LoginContext {
        login_trace_id: uuid::Uuid::new_v4().to_string(),
        code_verifier: String::new(),
        code_challenge: String::new(),
        device_id: random_digits(16),
        machine_id: uuid::Uuid::new_v4().to_string(),
        device_public_pem: String::new(),
    };
    // PKCE 与设备公钥都需要随机源；失败时直接报错而不是发一个假登录
    let mut context = context;
    let (verifier, challenge) = pkce_pair();
    context.code_verifier = verifier;
    context.code_challenge = challenge;
    context.device_public_pem =
        device_public_pem().ok_or_else(|| "生成设备密钥对失败（系统随机源不可用）".to_string())?;
    let state = uuid::Uuid::new_v4().simple().to_string();
    let auth_url = verification_uri(&login_host, &context);
    Ok(Started {
        state: state.clone(),
        auth_url,
        flow: Arc::new(TraeFlow {
            client: client.clone(),
            login_host,
            context,
        }),
    })
}

fn device_info(context: &LoginContext) -> Value {
    json!({
        "DeviceID": context.device_id,
        "MachineID": context.machine_id,
        "PlatformCode": "windows",
        "DeviceType": "PC",
        "DeviceName": DEVICE_NAME,
        "DeviceModel": DEVICE_BRAND,
        "ClientVersion": IDE_VERSION,
        "DevicePublicKey": context.device_public_pem,
        "DeviceBrand": "Microsoft",
        "DeviceCPU": "",
        "OSInfo": DEVICE_TYPE,
        "OSVersion": OS_VERSION,
    })
}

fn verification_uri(login_host: &str, context: &LoginContext) -> String {
    let host = ensure_https_scheme(login_host);
    let mut query: Vec<String> = Vec::new();
    let push = |query: &mut Vec<String>, key: &str, value: &str, encode: bool| {
        query.push(format!(
            "{key}={}",
            if encode {
                url_encode(value)
            } else {
                value.to_string()
            }
        ));
    };
    push(&mut query, "login_version", "1", false);
    push(&mut query, "auth_from", "solo", false);
    push(&mut query, "login_channel", "native_ide", false);
    push(&mut query, "plugin_version", PLUGIN_VERSION, true);
    push(&mut query, "auth_type", "local", false);
    push(&mut query, "client_id", SOLO_CLIENT_ID, false);
    push(&mut query, "redirect", "0", false);
    push(&mut query, "login_trace_id", &context.login_trace_id, true);
    push(&mut query, "auth_callback_url", REDIRECT_URI, false);
    push(&mut query, "machine_id", &context.machine_id, true);
    push(&mut query, "device_id", &context.device_id, true);
    push(&mut query, "x_device_id", &context.device_id, true);
    push(&mut query, "x_machine_id", &context.machine_id, true);
    push(&mut query, "x_device_brand", DEVICE_BRAND, true);
    push(&mut query, "x_device_type", DEVICE_TYPE, true);
    push(&mut query, "x_os_version", OS_VERSION, true);
    push(&mut query, "x_env", "prod", true);
    push(&mut query, "x_app_version", IDE_VERSION, true);
    push(&mut query, "x_app_type", APP_TYPE, true);
    push(&mut query, "code_challenge", &context.code_challenge, true);
    push(&mut query, "code_challenge_method", "S256", false);
    push(&mut query, "hide_saas_login", "true", false);
    format!(
        "{}/authorization?{}",
        host.trim_end_matches('/'),
        query.join("&")
    )
}

async fn request_guidance(client: &reqwest::Client) -> String {
    let body = json!({"loginTraceID": "", "login_trace_id": ""});
    let headers = [(
        "User-Agent",
        format!("Trae/{PLUGIN_VERSION} antigravity-cockpit-tools"),
    )];
    for url in GUIDANCE_URLS {
        let response = client
            .post(url)
            .header("User-Agent", headers[0].1.clone())
            .timeout(Duration::from_secs(5))
            .json(&body)
            .send()
            .await;
        let Ok(response) = response else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let text = response.text().await.unwrap_or_default();
        if let Some(host) = extract_login_host(&text) {
            return host;
        }
    }
    DEFAULT_LOGIN_HOST.to_string()
}

fn extract_login_host(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    const KEYS: [&str; 5] = [
        "LoginHost",
        "loginHost",
        "LoginURL",
        "loginUrl",
        "login_url",
    ];
    let pick = |value: &Value| {
        KEYS.iter()
            .find_map(|key| {
                value
                    .get(*key)
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
            })
            .map(str::to_string)
    };
    if let Some(found) = pick(&parsed) {
        return Some(found);
    }
    for outer in ["Result", "result", "data"] {
        let Some(value) = parsed.get(outer).filter(|value| value.is_object()) else {
            continue;
        };
        if let Some(found) = pick(value) {
            return Some(found);
        }
        for inner in ["Result", "result"] {
            if let Some(nested) = value.get(inner) {
                if let Some(found) = pick(nested) {
                    return Some(found);
                }
            }
        }
    }
    None
}

fn exchange_urls(login_host: &str) -> Vec<String> {
    let onto = |origin: &str| format!("{}{EXCHANGE_PATH}", origin.trim_end_matches('/'));
    let mut urls: Vec<String> = ACCOUNT_API_ORIGINS
        .iter()
        .map(|origin| onto(origin))
        .collect();
    let derived = ensure_https_scheme(login_host);
    let host = host_only(&derived);
    if !host.is_empty() {
        let origin = format!("{}://{}", scheme_of(&derived), host);
        urls.push(onto(&origin));
        if let Some(stripped) = host.strip_prefix("www.") {
            urls.push(onto(&format!("{}://api.{stripped}", scheme_of(&derived))));
        }
    }
    for origin in CN_DEFAULT_ORIGINS {
        urls.push(onto(origin));
    }
    urls.retain(|value| !value.is_empty());
    urls.dedup();
    urls
}

fn ensure_https_scheme(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }
    format!("https://{}", trimmed.trim_start_matches('/'))
}

fn host_only(url: &str) -> String {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string()
}

fn scheme_of(url: &str) -> &str {
    match url.find("://") {
        Some(index) if index > 0 => &url[..index],
        _ => "https",
    }
}

fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn pkce_pair() -> (String, String) {
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let digest = Sha256::digest(verifier.as_bytes());
    (
        verifier,
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest),
    )
}

fn random_digits(length: usize) -> String {
    let mut out = String::with_capacity(length);
    let mut seed = uuid::Uuid::new_v4().simple().to_string();
    while out.len() < length {
        for ch in seed.chars() {
            if ch.is_ascii_hexdigit() {
                out.push(char::from(b'0' + (ch.to_digit(16).unwrap_or(0) as u8 % 10)));
                if out.len() == length {
                    break;
                }
            }
        }
        seed = uuid::Uuid::new_v4().simple().to_string();
    }
    out
}

fn device_public_pem() -> Option<String> {
    use p256::pkcs8::EncodePublicKey;
    for _ in 0..16 {
        let secret = p256::SecretKey::random(&mut rand::rngs::OsRng);
        let key = p256::ecdsa::SigningKey::from(secret);
        if let Ok(der) = key.verifying_key().to_public_key_der() {
            return Some(pem("PUBLIC KEY", der.as_bytes()));
        }
    }
    None
}

fn pem(label: &str, der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut out = String::with_capacity(encoded.len() + 64);
    out.push_str("-----BEGIN ");
    out.push_str(label);
    out.push_str("-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push('\n');
    }
    out.push_str("-----END ");
    out.push_str(label);
    out.push_str("-----\n");
    out
}

fn normalize_expires_at(value: i64) -> Option<i64> {
    if value <= 0 {
        return None;
    }
    Some(if value < 100_000_000_000 {
        value
    } else {
        value / 1000
    })
}

fn timestamp_of(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|raw| raw as i64)),
        Value::String(text) => text.trim().parse::<f64>().ok().map(|raw| raw as i64),
        _ => None,
    }
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
