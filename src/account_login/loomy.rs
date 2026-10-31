//! Loomy 的手机验证码登录（讯飞 CAccount 签名接口）。
//!
//! ```text
//! POST {xfyun}/login/phone/sendMsgCode  {base, param:{ccode:"86", phone, expire:300}}
//!   → data.msgid（验证码会话 id）
//! POST {xfyun}/login/phone/checkCode    {base, param:{ccode, phone, mcode, msgid, expire:1209600}}
//!   → data.session / data.userid（登录态）
//! ```
//!
//! 两个请求都带 HMAC-SHA1 签名头（Authorization / Date / Nonce / Content-MD5）。
//! 签名密钥随客户端分发，这里照抄官方客户端的口径。

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha1::Sha1;

use super::Credentials;

const XFYUN_BASE: &str = "https://account.xfinfr.com";
const SEND_CODE_PATH: &str = "/login/phone/sendMsgCode";
const LOGIN_PATH: &str = "/login/phone/checkCode";
const ACCESS_KEY_ID: &str = "2thryby66wxi53sk";
const ACCESS_KEY_SECRET: &str = "zsak6eadrbawz683wf5r3m2snrwj868r";
const APP_ID: &str = "GM3LOOMY";
const SUCCESS_CODE: &str = "000000";

fn normalize_phone(raw: &str) -> Result<String, String> {
    let trimmed: String = raw
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-')
        .collect();
    let digits = trimmed.strip_prefix("+86").unwrap_or(&trimmed);
    let digits = digits
        .strip_prefix("86")
        .filter(|rest| rest.len() == 11)
        .unwrap_or(digits);
    if digits.len() == 11
        && digits.starts_with('1')
        && digits
            .chars()
            .nth(1)
            .is_some_and(|ch| ('3'..='9').contains(&ch))
        && digits.chars().all(|ch| ch.is_ascii_digit())
    {
        Ok(digits.to_string())
    } else {
        Err("请填写 11 位中国大陆手机号".to_string())
    }
}

fn normalize_code(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.len() == 6 && trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        Ok(trimmed.to_string())
    } else {
        Err("请填写 6 位数字验证码".to_string())
    }
}

fn make_base() -> Value {
    json!({
        "appid": APP_ID,
        "modelid": "Web",
        "version": "1.0.0",
        "devid": "web",
        "ua": "Loomy|Desktop|Electron|macOS",
        "traceid": uuid::Uuid::new_v4().simple().to_string(),
    })
}

fn escape_path_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn build_escaped_path(path: &str) -> String {
    let mut clean = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    if clean.len() > 1 && clean.ends_with('/') {
        clean.pop();
    }
    clean
        .split('/')
        .map(escape_path_segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn signed_headers(method: &str, path: &str, body: &Value) -> Result<Vec<(String, String)>, String> {
    let body_string = body.to_string();
    // Content-MD5 = base64(md5 原始 16 字节)，不是 base64(hex 文本)。
    // md5 0.7 只给 hex 形态，先 decode 回字节再 base64。
    let hex_digest = format!("{:x}", md5::compute(body_string.as_bytes()));
    let md5 = base64::engine::general_purpose::STANDARD
        .encode(hex::decode(hex_digest).unwrap_or_default());
    let date = chrono::Utc::now()
        .format("%a, %d %b %Y %H:%M:%S GMT")
        .to_string();
    let nonce = uuid::Uuid::new_v4().to_string();
    let string_to_sign = [
        method.to_uppercase(),
        build_escaped_path(path),
        String::new(),
        md5.clone(),
        "application/json".to_string(),
        date.clone(),
        nonce.clone(),
        String::new(),
        String::new(),
    ]
    .join("\n");
    let mut mac = Hmac::<Sha1>::new_from_slice(ACCESS_KEY_SECRET.as_bytes())
        .map_err(|_| "Loomy 签名初始化失败".to_string())?;
    mac.update(string_to_sign.as_bytes());
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    Ok(vec![
        (
            "Authorization".to_string(),
            format!("account {ACCESS_KEY_ID}:{signature}"),
        ),
        ("Date".to_string(), date),
        ("Nonce".to_string(), nonce),
        ("Content-MD5".to_string(), md5),
    ])
}

async fn signed_post(client: &reqwest::Client, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{XFYUN_BASE}{path}");
    let headers = signed_headers("POST", path, body)?;
    let mut request = client.post(&url).header("Content-Type", "application/json");
    for (key, value) in headers {
        request = request.header(key, value);
    }
    let response = request
        .body(body.to_string())
        .send()
        .await
        .map_err(|error| format!("请求 Loomy 账号服务失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if status >= 400 {
        return Err(format!("Loomy 账号服务返回 HTTP {status}"));
    }
    Ok(payload)
}

fn business_code(payload: &Value) -> String {
    payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn upstream_message(payload: &Value) -> String {
    payload
        .get("desc")
        .and_then(Value::as_str)
        .or_else(|| payload.get("message").and_then(Value::as_str))
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

fn extract_msgid(payload: &Value) -> String {
    for pointer in ["/data/msgid", "/data/msgId", "/data/data/msgid", "/data/data/msgId"] {
        let text = payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !text.is_empty() {
            return text.to_string();
        }
    }
    String::new()
}

/// 发送验证码，返回 `msgid`（下一次登录请求要带回）。
pub async fn send_code(client: &reqwest::Client, phone: &str) -> Result<String, String> {
    let phone = normalize_phone(phone)?;
    let body = json!({
        "base": make_base(),
        "param": { "ccode": "86", "phone": phone, "expire": 300 },
    });
    let payload = signed_post(client, SEND_CODE_PATH, &body).await?;
    let code = business_code(&payload);
    if code != SUCCESS_CODE {
        let message = upstream_message(&payload);
        return Err(if message.is_empty() {
            format!("验证码发送失败（{code}）")
        } else {
            format!("验证码发送失败：{message}")
        });
    }
    let msgid = extract_msgid(&payload);
    if msgid.is_empty() {
        return Err("验证码已发出但上游没有返回 msgid，请稍后重试".to_string());
    }
    Ok(msgid)
}

/// 手机号 + 验证码登录，返回可落库的凭证。
pub async fn login_with_code(
    client: &reqwest::Client,
    phone: &str,
    code: &str,
    msgid: &str,
    name: Option<&str>,
) -> Result<Credentials, String> {
    let phone = normalize_phone(phone)?;
    let code = normalize_code(code)?;
    if msgid.trim().is_empty() {
        return Err("缺少 msgid：请先点「发送验证码」".to_string());
    }
    let body = json!({
        "base": make_base(),
        "param": {
            "ccode": "86",
            "phone": phone,
            "mcode": code,
            "msgid": msgid.trim(),
            "expire": 14 * 24 * 3600,
        },
    });
    let payload = signed_post(client, LOGIN_PATH, &body).await?;
    let status = business_code(&payload);
    if status != SUCCESS_CODE {
        let message = upstream_message(&payload);
        return Err(if message.is_empty() {
            format!("登录失败（上游 code={status}）")
        } else {
            format!("登录失败：{message}")
        });
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let session = data
        .get("session")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if session.is_empty() {
        return Err("登录成功但上游没有返回 session".to_string());
    }
    let userid = data
        .get("userid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let display = name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if userid.is_empty() {
                "Loomy".to_string()
            } else {
                format!("Loomy {userid}")
            }
        });
    Ok(Credentials {
        name: display,
        secret: session,
        base_url: None,
        extra_headers: Default::default(),
        expires_at: None,
    })
}
