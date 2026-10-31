//! CodeArts 的网页登录（华为云 OAuth + PKCE + DPoP，授权码换临时 AK/SK/STS）。
//!
//! ```text
//! ① 打开 https://codearts.huaweicloud.com/portal/authorize?…
//!    （client_id=vscode-codebot / port=<回调端口> / code_challenge(SHA-256) /
//!    ticket_id / plugin-name=snap_vscode / plugin-version=26.9.101）
//! ② 登录完成 → portal 把浏览器送回 http://127.0.0.1:<port>/oauth/callback?code=…
//! ③ POST https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens（表单）
//!    + DPoP: <proof>（ES256 JWT，头里带公钥 JWK）
//!    → credentials.access_key_id / secret_access_key / security_token / refresh_token
//! ```
//!
//! 网关的 CodeArts 桥只认 `AK:SK[:STS]`，因此落账号时把三元组拼成冒号串；
//! 临时凭据约一小时到期，过期后重新登录。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use p256::ecdsa::signature::hazmat::PrehashSigner;
use p256::ecdsa::{Signature, SigningKey};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{Credentials, LoginFlow, Started};

const CLIENT_ID: &str = "vscode-codebot";
const WEB_LOGIN_BASE: &str = "https://codearts.huaweicloud.com";
const TOKEN_URL: &str = "https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens";
const PLUGIN_NAME: &str = "snap_vscode";
const PLUGIN_VERSION: &str = "26.9.101";

struct DpopKey {
    signing: SigningKey,
    jwk: Jwk,
}

#[derive(Clone, Serialize)]
struct Jwk {
    kty: &'static str,
    crv: &'static str,
    x: String,
    y: String,
}

#[derive(Serialize)]
struct DpopHeader<'a> {
    alg: &'a str,
    typ: &'a str,
    jwk: &'a Jwk,
}

#[derive(Serialize)]
struct DpopPayload<'a> {
    htm: String,
    htu: &'a str,
    iat: i64,
    jti: String,
}

impl DpopKey {
    fn generate() -> Result<Self, String> {
        for _ in 0..8 {
            let seed = uuid::Uuid::new_v4().simple().to_string();
            let bytes = hex::decode(&seed).map_err(|_| "随机数不可用".to_string())?;
            let Ok(signing) = SigningKey::from_slice(&bytes) else {
                continue;
            };
            let point = signing.verifying_key().to_encoded_point(false);
            let x = point
                .x()
                .map(|value| URL_SAFE_NO_PAD.encode(value))
                .unwrap_or_default();
            let y = point
                .y()
                .map(|value| URL_SAFE_NO_PAD.encode(value))
                .unwrap_or_default();
            return Ok(Self {
                signing,
                jwk: Jwk {
                    kty: "EC",
                    crv: "P-256",
                    x,
                    y,
                },
            });
        }
        Err("无法生成本机 P-256 密钥".to_string())
    }

    fn proof(&self, method: &str, endpoint: &str) -> Result<String, String> {
        let header = DpopHeader {
            alg: "ES256",
            typ: "dpop+jwt",
            jwk: &self.jwk,
        };
        let header_json =
            serde_json::to_vec(&header).map_err(|error| format!("DPoP 头部序列化失败：{error}"))?;
        let payload = DpopPayload {
            htm: method.to_ascii_uppercase(),
            htu: endpoint,
            iat: chrono::Utc::now().timestamp(),
            jti: uuid::Uuid::new_v4().simple().to_string(),
        };
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|error| format!("DPoP 载荷序列化失败：{error}"))?;
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(&header_json),
            URL_SAFE_NO_PAD.encode(&payload_json)
        );
        let digest = Sha256::digest(signing_input.as_bytes());
        let signature: Signature = self
            .signing
            .sign_prehash(&digest)
            .map_err(|error| format!("DPoP 签名失败：{error}"))?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        ))
    }
}

pub struct CodeartsFlow {
    client: reqwest::Client,
    state: String,
    callback_url: String,
    code_verifier: String,
    dpop: DpopKey,
}

#[async_trait]
impl LoginFlow for CodeartsFlow {
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        Ok(None)
    }

    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed =
            url::Url::parse(url.trim()).map_err(|_| "CodeArts 登录回调地址无效".to_string())?;
        let mut code = String::new();
        let mut state = String::new();
        let mut secret = String::new();
        for (key, value) in parsed.query_pairs() {
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match key.as_ref() {
                "code" => code = value,
                "ticket_id" | "state" => state = value,
                "secret" => secret = value,
                _ => {}
            }
        }
        if !state.is_empty() && state != self.state {
            return Err("回调的 ticket_id 与当前 CodeArts 登录任务不匹配".to_string());
        }
        if code.is_empty() {
            if !secret.is_empty() {
                return Err(
                    "这个回调只有 secret，还没有最终授权码：请在登录页完成授权后再次复制最终回调地址"
                        .to_string(),
                );
            }
            return Err("回调没有携带授权码".to_string());
        }
        let proof = self.dpop.proof("POST", TOKEN_URL)?;
        let form = [
            ("client_id", CLIENT_ID),
            ("code", code.as_str()),
            ("code_verifier", self.code_verifier.as_str()),
            ("grant_type", "authorization_code"),
            ("redirect_uri", self.callback_url.as_str()),
        ];
        let response = self
            .client
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("DPoP", proof)
            .form(&form)
            .send()
            .await
            .map_err(|error| format!("CodeArts 换证请求失败：{error}"))?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        if status >= 400 {
            let message = payload
                .get("error_description")
                .or_else(|| payload.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("");
            return Err(if message.is_empty() {
                format!("CodeArts 换证失败（HTTP {status}）")
            } else {
                format!("CodeArts 换证失败：{message}")
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
        let ak = read("access_key_id");
        let sk = read("secret_access_key");
        let sts = read("security_token");
        if ak.is_empty() || sk.is_empty() {
            return Err("CodeArts 令牌响应缺少 access_key_id / secret_access_key".to_string());
        }
        let secret = if sts.is_empty() {
            format!("{ak}:{sk}")
        } else {
            format!("{ak}:{sk}:{sts}")
        };
        Ok(Some(Credentials {
            name: "CodeArts".to_string(),
            secret,
            base_url: None,
            extra_headers: HashMap::new(),
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
        }))
    }
}

pub async fn start(client: &reqwest::Client) -> Result<Started, String> {
    let state = uuid::Uuid::new_v4().simple().to_string();
    // 64 字符 ASCII → 落在 RFC 7636 的 43–128 区间
    let code_verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let dpop = DpopKey::generate()?;
    let callback_url = "http://127.0.0.1:1/oauth/callback".to_string();
    let port = "1";
    let query = [
        ("theme", "2".to_string()),
        ("locale", "zh-cn".to_string()),
        ("uri_scheme", CLIENT_ID.to_string()),
        ("client_id", CLIENT_ID.to_string()),
        ("port", port.to_string()),
        ("code_challenge", challenge),
        ("code_challenge_method", "SHA-256".to_string()),
        ("ticket_id", state.clone()),
        ("auth_callback_url", callback_url.clone()),
        ("plugin-name", PLUGIN_NAME.to_string()),
        ("plugin-version", PLUGIN_VERSION.to_string()),
    ]
    .iter()
    .map(|(key, value)| format!("{key}={}", encode_component(value)))
    .collect::<Vec<_>>()
    .join("&");
    let auth_url = format!("{WEB_LOGIN_BASE}/portal/authorize?{query}");
    Ok(Started {
        state: state.clone(),
        auth_url,
        flow: Arc::new(CodeartsFlow {
            client: client.clone(),
            state,
            callback_url,
            code_verifier,
            dpop,
        }),
    })
}

/// JS `encodeURIComponent` 口径（未保留集比 RFC3986 多 `!'()*`）。
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
