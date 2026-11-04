//! AutoClaw 国际版 OAuth 网页登录（Zai / Google）。
//!
//! ── 协议（三步）──────────────────────────────────────────────
//! ```text
//! ① POST {intl}/userapi/overseasv1/oauth-captcha-config   {}
//!      → data {enabled, region, prefix, scene_id, captcha_supplier}
//! ② POST {intl}/userapi/overseasv1/{zai|google}-oauth-url
//!      {source_id, device_id, navigate_uri, ali_captcha_verify_param}
//!      → data.oauth_url（官方登录页）
//! ③ 浏览器登录 → 302 到 {navigate_uri}?code=…&state=…
//!    POST {intl}/userapi/overseasv1/{zai|google}-oauth-login
//!      {source_id, device_id, code, state, navigate_uri}
//!      → data {access_token, refresh_token, user_id, user_name, first_login}
//! ```
//!
//! ── 卡点：第 ② 步强制要求阿里云风控验证码 ────────────────────
//! 不带 `ali_captcha_verify_param` 得到 `631002`（误导性错误码）。
//! 验证码是**浏览器端 JS SDK**，网关不解析它，只原样转交上游。
//!
//! ── 第 ③ 步不需要验证码 ──────────────────────────────────────
//! 换码这一跳不要求风控参数，可以完全由网关完成。
//!
//! ── navigate_uri ────────────────────────────────────────────
//! 形如 `http://localhost:<port>/auth/callback-{vendor}`。
//! Google 接受任意 loopback 端口；Zai 只接受官方客户端登记的四个端口
//! （18432/19654/19723/53699），网关端口会被拒。
//! AIGX 是 Web 网关（非桌面壳），不绑定那四个端口——Zai 登录在网关
//! 端口上可能失败，Google 不受影响。用户可改用「填写凭证」粘贴登录态。
//!
//! ── 回调处理 ──────────────────────────────────────────────────
//! 浏览器 302 到 `navigate_uri?code=…&state=…`，命中网关的
//! `GET /auth/callback-{vendor}` 路由。该路由提取 code/state，
//! 找到匹配的进行中任务，调用 `exchange_code` 换凭证，落账号，
//! 返回 HTML 成功页。任务关联靠 provider + vendor 匹配（URL 里
//! 不带 task state——与上游 state 参数名冲突）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Credentials, LoginFlow, Started};

// ── 上游地址 ──────────────────────────────────────────────────

/// 上游地址（Cn 国内版 / Intl 国际版）
pub const CN_USERAPI: &str = "https://autoglm-acceleration-api.zhipuai.cn";
pub const INTL_USERAPI: &str = "https://autoglm-api.autoglm.ai";

/// AutoClaw 上游签名常量（与 refresh.rs 共用同一套 appId/appKey）。
const APPID: &str = "100003";
const APPKEY: &str = "38d2391985e2369a5fb8227d8e6cd5e5";

/// OAuth 端点路径
const CAPTCHA_CONFIG_PATH: &str = "/userapi/overseasv1/oauth-captcha-config";
const CALLBACK_PATH_PREFIX: &str = "/auth/callback-";

/// 请求超时（与客户端同档：15 秒）
const REQUEST_TIMEOUT_MS: u64 = 15_000;

/// OAuth 变体
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vendor {
    /// Zai（智谱自家账号体系）
    Zai,
    /// Google
    Google,
}

impl Vendor {
    pub const ALL: [Vendor; 2] = [Vendor::Zai, Vendor::Google];

    /// 请求体 / 回调路径里用的标识
    pub fn id(self) -> &'static str {
        match self {
            Self::Zai => "zai",
            Self::Google => "google",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.id() == value)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Zai => "Zai",
            Self::Google => "Google",
        }
    }

    fn url_path(self) -> &'static str {
        match self {
            Self::Zai => "/userapi/overseasv1/zai-oauth-url",
            Self::Google => "/userapi/overseasv1/google-oauth-url",
        }
    }

    fn login_path(self) -> &'static str {
        match self {
            Self::Zai => "/userapi/overseasv1/zai-oauth-login",
            Self::Google => "/userapi/overseasv1/google-oauth-login",
        }
    }
}

/// 拼回调地址（同时也是交给上游的 `navigate_uri`）。
pub fn navigate_uri(callback_base: &str, vendor: Vendor) -> String {
    format!(
        "{}{}{}",
        callback_base.trim_end_matches('/'),
        CALLBACK_PATH_PREFIX,
        vendor.id()
    )
}

/// AutoClaw 上游签名头（匿名形态，access 为空）。
fn signed_headers() -> Vec<(String, String)> {
    let ts = chrono::Utc::now().timestamp().to_string();
    let sign = format!(
        "{:x}",
        md5::compute(format!("{APPID}&{ts}&{APPKEY}"))
    );
    vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        ("X-Version".to_string(), "1.18.5".to_string()),
        ("X-Product".to_string(), "autoclaw".to_string()),
        ("X-Client-Type".to_string(), "pc".to_string()),
        ("X-Harness-Type".to_string(), "zcode".to_string()),
        ("X-Tm".to_string(), "win".to_string()),
        ("X-Lang".to_string(), "zh-CN".to_string()),
        ("X-Channel".to_string(), "official".to_string()),
        ("X-Auth-Appid".to_string(), APPID.to_string()),
        ("X-Auth-TimeStamp".to_string(), ts),
        ("X-Auth-Sign".to_string(), sign),
        ("X-Trace-Id".to_string(), uuid::Uuid::new_v4().to_string()),
    ]
}

/// 上游业务码 → 用户能看懂的原因
fn describe_oauth_error(code: i64, upstream_msg: &str) -> String {
    match code {
        631_002 => "本次登录缺少风控验证（验证码未通过或已过期），请重新点击登录再试一次".to_string(),
        630_014 => "风控验证未通过，请重新点击登录再试一次".to_string(),
        631_001 => "授权码无效或已过期，请重新点击登录".to_string(),
        400_001 => "请求参数有误（登录链路内部错误，请重试）".to_string(),
        400_002 => "请求签名校验失败（本机时钟可能有偏差），请校准系统时间后重试".to_string(),
        _ => {
            if upstream_msg.trim().is_empty() {
                format!("登录失败（上游 code={code}）")
            } else {
                format!("登录失败：{}", upstream_msg.trim())
            }
        }
    }
}

/// 带签名的 POST（登录链路上还没有 token，走匿名形态）。
async fn post_signed(
    userapi: &str,
    path: &str,
    body: &Value,
    what: &str,
    client: &reqwest::Client,
) -> Result<Value, String> {
    let url = format!("{userapi}{path}");
    let mut header_map = reqwest::header::HeaderMap::new();
    for (key, value) in signed_headers() {
        let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) else {
            continue;
        };
        let Ok(val) = reqwest::header::HeaderValue::from_str(&value) else {
            continue;
        };
        header_map.insert(name, val);
    }
    let response = client
        .post(&url)
        .headers(header_map)
        .timeout(Duration::from_millis(REQUEST_TIMEOUT_MS))
        .json(body)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                format!("{what}超时")
            } else {
                format!("{what}失败: {error}")
            }
        })?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("{what}返回 HTTP {status}"));
    }
    let payload: Value = response.json().await.unwrap_or(Value::Null);
    Ok(payload)
}

/// 取验证码配置（前端据此初始化阿里云 SDK）。
///
/// 返回 `{enabled, region, prefix, sceneId, supplier}`。
/// `enabled: false`（国内版）不是错误：前端据此不渲染 OAuth 按钮。
pub async fn captcha_config(
    userapi: &str,
    client: &reqwest::Client,
) -> Result<Value, String> {
    let payload = post_signed(userapi, CAPTCHA_CONFIG_PATH, &json!({}), "获取风控验证配置", client).await?;
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let msg = payload.get("msg").and_then(Value::as_str).unwrap_or("");
        return Err(describe_oauth_error(code, msg));
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let text = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string()
    };
    Ok(json!({
        "enabled": data.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        "region": text("region"),
        "prefix": text("prefix"),
        "sceneId": text("scene_id"),
        "supplier": text("captcha_supplier"),
    }))
}

/// 取 OAuth 授权地址（需要验证码参数）。
async fn request_oauth_url(
    userapi: &str,
    vendor: Vendor,
    navigate_uri: &str,
    captcha: &str,
    device_id: &str,
    client: &reqwest::Client,
) -> Result<String, String> {
    let captcha = captcha.trim();
    if captcha.is_empty() {
        return Err("缺少风控验证参数，请先完成验证码".to_string());
    }
    let body = json!({
        "source_id": "autoclaw",
        "device_id": device_id,
        "navigate_uri": navigate_uri,
        "ali_captcha_verify_param": captcha,
    });
    let what = format!("获取 {} 授权地址", vendor.label());
    let payload = post_signed(userapi, vendor.url_path(), &body, &what, client).await?;
    let code = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let msg = payload.get("msg").and_then(Value::as_str).unwrap_or("");
        return Err(describe_oauth_error(code, msg));
    }
    let oauth_url = payload
        .get("data")
        .and_then(|d| d.get("oauth_url"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if oauth_url.is_empty() {
        return Err("上游没有返回授权地址，请重试".to_string());
    }
    Ok(oauth_url)
}

/// 用授权码换凭证。
///
/// 返回 `{token, refreshToken, userId, userName}`（形状与手机验证码登录一致）。
async fn exchange_code(
    userapi: &str,
    vendor: Vendor,
    code: &str,
    state: &str,
    navigate_uri: &str,
    device_id: &str,
    client: &reqwest::Client,
) -> Result<Value, String> {
    let body = json!({
        "source_id": "autoclaw",
        "device_id": device_id,
        "code": code,
        "state": state,
        "navigate_uri": navigate_uri,
    });
    let what = format!("{} 登录换取凭证", vendor.label());
    let payload = post_signed(userapi, vendor.login_path(), &body, &what, client).await?;
    let status = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
    if status != 0 {
        let msg = payload.get("msg").and_then(Value::as_str).unwrap_or("");
        return Err(describe_oauth_error(status, msg));
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let token = data
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("登录成功但上游没有返回 access_token".to_string());
    }
    let refresh_token = data
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let user_id = data
        .get("user_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let user_name = data
        .get("user_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    Ok(json!({
        "token": token,
        "refreshToken": refresh_token,
        "userId": user_id,
        "userName": user_name,
    }))
}

/// 生成设备 ID（64 位十六进制串）。
fn device_id() -> String {
    let mut bytes = [0u8; 32];
    if getrandom::getrandom(&mut bytes).is_err() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = ((nanos >> (i * 8)) as u8) ^ (counter as u8) ^ (i as u8);
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// AutoClaw OAuth 登录流程。
///
/// `refresh_token` 在 `submit_callback` / `handle_browser_callback` 换到凭证时
/// 写入（interior mutability），`refresh_token()` 读取——对齐 CodeArts 的写法
/// （续期令牌必须随账号落盘，否则过期后无法自动续期）。
pub struct AutoclawOauthFlow {
    client: reqwest::Client,
    userapi: String,
    vendor: Vendor,
    navigate_uri: String,
    device_id: String,
    refresh_token: Mutex<Option<String>>,
}

#[async_trait]
impl LoginFlow for AutoclawOauthFlow {
    fn poll_interval(&self) -> Duration {
        Duration::from_secs(3)
    }

    /// 轮询模式：AutoClaw OAuth 是回调型流程，没有后台轮询。
    /// `poll()` 永远返回 `Ok(None)`（继续等待），直到超时兜底。
    async fn poll(&self) -> Result<Option<Credentials>, String> {
        Ok(None)
    }

    /// 手工提交回调地址：解析 URL 提取 code/state，换凭证。
    ///
    /// 换到凭证后把 refresh_token 写入内部槽位（`refresh_token()` 读），
    /// 对齐 CodeArts：续期令牌必须随账号落盘。
    async fn submit_callback(&self, url: &str) -> Result<Option<Credentials>, String> {
        let parsed = url::Url::parse(url).map_err(|_| "回调地址无效".to_string())?;
        let code = parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())
            .unwrap_or_default();
        if code.trim().is_empty() {
            return Err("回调没有携带授权码".to_string());
        }
        let creds_data = exchange_code(
            &self.userapi,
            self.vendor,
            &code,
            &state,
            &self.navigate_uri,
            &self.device_id,
            &self.client,
        )
        .await?;
        let token = creds_data
            .get("token")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let refresh = creds_data
            .get("refreshToken")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // 存入内部槽位（interior mutability），`refresh_token()` 读取
        if !refresh.is_empty() {
            if let Ok(mut slot) = self.refresh_token.lock() {
                *slot = Some(refresh);
            }
        }
        let name = creds_data
            .get("userName")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("AutoClaw")
            .to_string();
        let user_id = creds_data
            .get("userId")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("");
        let name = if user_id.is_empty() {
            name
        } else {
            format!("{name} ({user_id})")
        };
        let mut extra_headers = HashMap::new();
        if !user_id.is_empty() {
            extra_headers.insert("x-uid".to_string(), user_id.to_string());
        }
        Ok(Some(Credentials {
            name,
            secret: token,
            base_url: None,
            extra_headers,
            expires_at: None,
        }))
    }

    fn refresh_token(&self) -> String {
        self.refresh_token
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .unwrap_or_default()
    }

    fn extra_fields(&self) -> HashMap<String, String> {
        HashMap::new()
    }
}

/// 发起 AutoClaw OAuth 登录。
///
/// `userapi` 是上游 userapi 基址（Cn 或 Intl）。
/// `callback_base` 是网关自己的 loopback 基址（`http://localhost:<port>`）。
/// `captcha_verify_param` 由前端跑完阿里云 SDK 得到。
pub async fn start(
    client: &reqwest::Client,
    userapi: &str,
    vendor: Vendor,
    captcha_verify_param: &str,
    callback_base: &str,
) -> Result<Started, String> {
    let device = device_id();
    let navigate = navigate_uri(callback_base, vendor);
    let auth_url = request_oauth_url(
        userapi,
        vendor,
        &navigate,
        captcha_verify_param,
        &device,
        client,
    )
    .await?;
    let state = uuid::Uuid::new_v4().simple().to_string();
    Ok(Started {
        state,
        auth_url,
        flow: Arc::new(AutoclawOauthFlow {
            client: client.clone(),
            userapi: userapi.to_string(),
            vendor,
            navigate_uri: navigate,
            device_id: device,
            refresh_token: Mutex::new(None),
        }),
    })
}
