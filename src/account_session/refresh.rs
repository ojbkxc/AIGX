//! 账号级 token 自动刷新 —— 表驱动，按 provider 分派。
//!
//! 协议逐家移植自 Agent2API（`providers/{qoder,autoclaw,trae}/refresh.rs` 与
//! `core/credential_maintenance.rs`，MIT + 使用声明，禁止商用）。设计取向：
//!
//! - **失败绝不改写凭据**（含 refresh_token）：旧快照仍可重试，不会把账号刷废；
//! - 成功时 access/refresh/过期时间**一次 CAS 落盘**（refresh_token 会轮换，
//!   与 access 分开写会把账号弄丢）；
//! - 过期窗口按家不同：qoder/autoclaw 提前 5 分钟，trae 提前 24 小时；
//! - 不做单飞与指数退避：调度是 10 分钟一轮的串行巡检，重复刷新幂等
//!   （refresh_token 轮换后旧的失效，但 CAS 保证只有一次写入生效）。
//!
//! 刷新令牌来源：`AccountSession.refresh_secret`（管理面可粘贴；登录流程
//! 捕获 refresh_token 的补齐见 docs/agent2api-porting.md 路线图）。

use reqwest::Client;
use serde_json::{json, Value};

use super::model::AccountSession;
use super::store::{AccountSessionStore, CasOutcome};

/// 刷新请求超时。
const TIMEOUT_MS: u64 = 30_000;

/// 各家提前刷新窗口（秒）。
const QODER_MARGIN: i64 = 300;
const AUTOCLAW_MARGIN: i64 = 300;
const TRAE_MARGIN: i64 = 86400;

/// 一次刷新的结果。`Invalid` = refresh_token 也死了，只能重新登录。
#[derive(Debug)]
pub enum RefreshOutcome {
    Refreshed(String),
    Stale,
    Invalid(String),
    Failed(String),
}

/// 新令牌三件套（时间戳统一为 unix **秒**，与 `AccountSession.expires_at` 同口径）。
struct TokenSet {
    access: String,
    refresh: String,
    expires_at: Option<i64>,
}

/// 支持自动刷新的 provider。
pub fn supports_refresh(provider: &str) -> bool {
    matches!(
        provider,
        "qoder" | "qoder-intl" | "autoclaw" | "autoclaw-intl" | "trae"
    )
}

/// refresh_secret 的明文（落盘是 `enc1:`/`enc:` 编码，读前必须解码）。
fn refresh_plain(session: &AccountSession) -> String {
    crate::account_session::secret::open_secret(
        &session.refresh_secret,
        &crate::account_session::secret::data_dir(),
    )
}

/// 是否临期需要刷新（无 refresh_secret 或未知过期时间则不动）。
pub fn needs_refresh(session: &AccountSession, now: i64) -> bool {
    if !supports_refresh(&session.provider) {
        return false;
    }
    if refresh_plain(session).trim().is_empty() {
        return false;
    }
    let Some(exp) = session.expires_at.filter(|e| *e > 0) else {
        return false;
    };
    let margin = match session.provider.as_str() {
        "trae" => TRAE_MARGIN,
        "autoclaw" | "autoclaw-intl" => AUTOCLAW_MARGIN,
        _ => QODER_MARGIN,
    };
    exp - now <= margin
}

/// 巡检一轮：把所有临期账号串行刷新（每轮一个账号只刷一次）。
/// 返回成功刷新数。
pub async fn refresh_expiring(pool: &AccountSessionStore, client: &Client) -> u64 {
    let now = chrono::Utc::now().timestamp();
    let targets: Vec<AccountSession> = pool
        .all()
        .into_iter()
        .filter(|s| needs_refresh(s, now))
        .collect();
    let mut done = 0u64;
    for session in targets {
        match refresh_one(pool, client, &session).await {
            RefreshOutcome::Refreshed(msg) => {
                tracing::info!("account {} token refreshed: {msg}", session.id);
                done += 1;
            }
            RefreshOutcome::Invalid(msg) => {
                tracing::warn!("account {} refresh token invalid: {msg}", session.id);
            }
            RefreshOutcome::Failed(msg) => {
                tracing::warn!("account {} refresh failed: {msg}", session.id);
            }
            RefreshOutcome::Stale => {}
        }
    }
    done
}

/// 刷新单个账号并 CAS 落盘。失败不写任何字段。
pub async fn refresh_one(
    pool: &AccountSessionStore,
    client: &Client,
    session: &AccountSession,
) -> RefreshOutcome {
    let expected = session.credential.secret_plain();
    if expected.trim().is_empty() {
        return RefreshOutcome::Failed("账号没有 access token".to_string());
    }
    let result = match session.provider.as_str() {
        "qoder" | "qoder-intl" => refresh_qoder(client, session).await,
        "autoclaw" | "autoclaw-intl" => refresh_autoclaw(client, session).await,
        "trae" => refresh_trae(client, session).await,
        other => {
            return RefreshOutcome::Failed(format!("{other} 暂不支持自动刷新"));
        }
    };
    let tokens = match result {
        Ok(tokens) => tokens,
        Err(outcome) => return outcome,
    };
    let mut next = session.credential.clone();
    next.secret = tokens.access.clone();
    match pool.update_tokens_cas(
        &session.id,
        &expected,
        next,
        &tokens.refresh,
        tokens.expires_at,
    ) {
        CasOutcome::Updated => RefreshOutcome::Refreshed(format!(
            "token 已续期{}",
            tokens
                .expires_at
                .map(|e| format!("（新过期时间 {e}）"))
                .unwrap_or_default()
        )),
        CasOutcome::Stale => RefreshOutcome::Stale,
        CasOutcome::Missing => RefreshOutcome::Failed("账号已被删除".to_string()),
    }
}

/// 发 JSON 请求，返回 (status, body)。
async fn post_json(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
    body: Value,
) -> Result<(u16, Value), RefreshOutcome> {
    let mut request = client
        .post(url)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .json(&body)
        .send()
        .await
        .map_err(|e| RefreshOutcome::Failed(format!("请求失败：{e}")))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

/// 绝对过期时间解析：`< 1e11` 视为秒、否则毫秒；最终统一成秒。
fn normalize_expire_ts(raw: f64) -> i64 {
    let ms = if raw < 1e11 { raw * 1000.0 } else { raw };
    (ms / 1000.0) as i64
}

fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn first_num(value: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(n) = value.get(*key).and_then(Value::as_f64) {
            return Some(n);
        }
        if let Some(text) = value.get(*key).and_then(Value::as_str) {
            if let Ok(n) = text.trim().parse::<f64>() {
                return Some(n);
            }
        }
    }
    None
}

/// JWT payload 的 `exp`（秒）。
fn jwt_exp(token: &str) -> Option<i64> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value.get("exp").and_then(Value::as_i64)
}

// ── Qoder ────────────────────────────────────────────────────────────
//
// 来源：Agent2API `providers/qoder/refresh.rs`。无签名无鉴权头；
// refresh_token 前缀决定端点（`jrt-` → jobToken，其余 → deviceToken），
// 响应里 refresh_token 每次轮换；过期只认绝对时间字段（不猜 expires_in）。

async fn refresh_qoder(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    // 落库可能是 "{rt}|{uid}|{mid}" 复合串，请求只带第一段
    let stored = refresh_plain(session);
    let rt = stored.split('|').next().unwrap_or("").trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let host = if session.provider == "qoder-intl" {
        "https://openapi.qoder.sh"
    } else {
        "https://openapi.qoder.com.cn"
    };
    let path = if rt.starts_with("jrt-") {
        "/api/v1/jobToken/refresh"
    } else {
        "/api/v1/deviceToken/refresh"
    };
    let headers = vec![
        ("User-Agent".to_string(), "qoder-local-proxy".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    let (status, value) = post_json(
        client,
        &format!("{host}{path}"),
        &headers,
        json!({ "refresh_token": rt }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "Qoder 刷新令牌已失效或与地区不符，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "Qoder 刷新被拒（HTTP {status}）"
        )));
    }
    let access = first_str(
        &value,
        &["device_token", "deviceToken", "token", "access_token"],
    )
    .ok_or_else(|| RefreshOutcome::Failed("Qoder 刷新响应缺 access token".to_string()))?;
    let refresh =
        first_str(&value, &["refresh_token", "refreshToken"]).unwrap_or_else(|| rt.to_string());
    // 只认绝对时间；缺字段兜底 30 天
    let expires_at = first_num(
        &value,
        &["expires_at", "expiresAt", "expire_time", "expireTime"],
    )
    .map(normalize_expire_ts)
    .or_else(|| Some(chrono::Utc::now().timestamp() + 30 * 24 * 3600));
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── AutoClaw ─────────────────────────────────────────────────────────
//
// 来源：Agent2API `providers/autoclaw/refresh.rs`。签名与签到同一套
// MD5（appId&ts&appKey）；refresh_token 轮换；过期从新 access 的 JWT exp 解。
// `code == 400002`（签名校验失败）降级 `agent-refresh` 再试一次。

const AUTOCLAW_APPID: &str = "100003";
const AUTOCLAW_APPKEY: &str = "38d2391985e2369a5fb8227d8e6cd5e5";

/// AutoClaw 鉴权签名头（MD5 appId&ts&appKey）——刷新/余额/签到共用一套。
pub(crate) fn autoclaw_headers(access: &str) -> Vec<(String, String)> {
    let ts = chrono::Utc::now().timestamp().to_string();
    let sign = format!(
        "{:x}",
        md5::compute(format!("{AUTOCLAW_APPID}&{ts}&{AUTOCLAW_APPKEY}"))
    );
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        ("X-Version".to_string(), "1.18.5".to_string()),
        ("X-Product".to_string(), "autoclaw".to_string()),
        ("X-Client-Type".to_string(), "pc".to_string()),
        ("X-Harness-Type".to_string(), "zcode".to_string()),
        ("X-Tm".to_string(), "win".to_string()),
        ("X-Lang".to_string(), "zh-CN".to_string()),
        ("X-Channel".to_string(), "official".to_string()),
        ("X-Auth-Appid".to_string(), AUTOCLAW_APPID.to_string()),
        ("X-Auth-TimeStamp".to_string(), ts),
        ("X-Auth-Sign".to_string(), sign),
        ("X-Trace-Id".to_string(), uuid::Uuid::new_v4().to_string()),
    ];
    if !access.trim().is_empty() {
        headers.push(("authorization".to_string(), format!("Bearer {access}")));
    }
    headers
}

async fn refresh_autoclaw(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let default = if session.provider == "autoclaw-intl" {
        "https://autoglm-api.autoglm.ai"
    } else {
        "https://autoglm-acceleration-api.zhipuai.cn"
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| default.to_string());
    let root = base
        .trim_end_matches('/')
        .trim_end_matches("/autoclaw")
        .trim_end_matches("/proxy");
    let access = session.credential.secret_plain();
    let mut body = json!({ "refresh_token": rt, "source_id": "autoclaw" });
    // device_id 若登录时记在 extra_headers 里就带上（上游有则加）
    if let Some(device) = session.extra_headers.get("device_id") {
        if !device.trim().is_empty() {
            body["device_id"] = json!(device.trim());
        }
    }

    async fn try_path(
        client: &Client,
        root: &str,
        path: &str,
        access: &str,
        body: &Value,
    ) -> Result<(u16, Value), RefreshOutcome> {
        let url = format!("{root}/userapi/v1/{path}");
        let headers = autoclaw_headers(access);
        post_json(client, &url, &headers, body.clone()).await
    }

    let (status, value) = match try_path(client, root, "refresh", &access, &body).await {
        Ok(pair) => pair,
        Err(e) => return Err(e),
    };
    // 400002 = 签名校验失败 → 降级 agent-refresh 再试一次
    let (status, value) = if value.get("code").and_then(Value::as_i64) == Some(400002) {
        match try_path(client, root, "agent-refresh", &access, &body).await {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        }
    } else {
        (status, value)
    };
    if status == 401 {
        return Err(RefreshOutcome::Invalid(
            "AutoClaw 登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "AutoClaw 刷新被拒（HTTP {status}）"
        )));
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code == 401 || code == 410000 {
        return Err(RefreshOutcome::Invalid(
            "AutoClaw 登录态已失效，请重新登录".to_string(),
        ));
    }
    if code != 0 {
        return Err(RefreshOutcome::Failed(format!(
            "AutoClaw 刷新失败（code={code}）"
        )));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let access = first_str(&data, &["access_token"])
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("AutoClaw 刷新响应缺 access token".to_string()))?;
    let refresh = first_str(&data, &["refresh_token"]).unwrap_or_else(|| rt.to_string());
    let expires_at = jwt_exp(&access);
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── Trae ─────────────────────────────────────────────────────────────
//
// 来源：Agent2API `providers/trae/refresh.rs`。四个键一个不能少
// （ClientSecret 是占位 "-"）；refresh_token 轮换；400+归属错误换另一把
// ClientID 重试；401 = 令牌作废；失败不改写任何字段。

const TRAE_CLIENT_MAIN: &str = "ono9krqynydwx5";
const TRAE_CLIENT_ALT: &str = "en1oxy7wnw8j9n";

async fn refresh_trae(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let host = "https://api.trae.cn";
    let url = format!("{host}/cloudide/api/v3/trae/oauth/ExchangeToken");
    let headers = vec![
        ("User-Agent".to_string(), "Trae/0.1.61".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let attempt = |client_id: &str| {
        post_json(
            client,
            &url,
            &headers,
            json!({
                "ClientID": client_id,
                "RefreshToken": rt,
                "ClientSecret": "-",
                "UserID": "",
            }),
        )
    };

    let (status, value) = match attempt(TRAE_CLIENT_MAIN).await {
        Ok(pair) => pair,
        Err(e) => return Err(e),
    };
    let attribution_reject = status == 400
        && (value.to_string().contains("10101")
            || value.to_string().contains("not matched to the client"));
    let (status, value) = if attribution_reject {
        match attempt(TRAE_CLIENT_ALT).await {
            Ok(pair) => pair,
            Err(e) => return Err(e),
        }
    } else {
        (status, value)
    };
    if status == 401 {
        return Err(RefreshOutcome::Invalid(
            "Trae 刷新令牌已作废，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "Trae 刷新被拒（HTTP {status}）"
        )));
    }
    let result = value
        .get("Result")
        .or_else(|| value.get("result"))
        .cloned()
        .unwrap_or(value.clone());
    let access = first_str(&result, &["Token", "token", "AccessToken", "accessToken"])
        .ok_or_else(|| RefreshOutcome::Failed("Trae 刷新响应缺 access token".to_string()))?;
    let refresh = first_str(&result, &["RefreshToken", "refreshToken", "refresh_token"])
        .unwrap_or_else(|| rt.to_string());
    let expires_at = first_num(
        &result,
        &["TokenExpireAt", "tokenExpireAt", "ExpiresAt", "expiresAt"],
    )
    .map(normalize_expire_ts)
    .or_else(|| jwt_exp(&access));
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_seconds_and_millis() {
        assert_eq!(normalize_expire_ts(1_700_000_000.0), 1_700_000_000);
        assert_eq!(normalize_expire_ts(2_591_999_994.0), 2_591_999_994);
        assert_eq!(normalize_expire_ts(1_700_000_000_000.0), 1_700_000_000);
    }

    #[test]
    fn refresh_support_whitelist() {
        assert!(supports_refresh("qoder"));
        assert!(supports_refresh("autoclaw-intl"));
        assert!(supports_refresh("trae"));
        assert!(!supports_refresh("loomy"));
        assert!(!supports_refresh("openai"));
    }

    #[test]
    fn needs_refresh_respects_margin() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "qoder".to_string();
        session.refresh_secret = "drt-xxx".to_string();
        session.expires_at = Some(1_000_000);
        assert!(!needs_refresh(&session, 1_000_000 - 3_600));
        assert!(needs_refresh(&session, 1_000_000 - 60));
        // trae 窗口 24h
        session.provider = "trae".to_string();
        assert!(needs_refresh(&session, 1_000_000 - 3_600));
        // 无 refresh_secret 不动
        session.refresh_secret = String::new();
        assert!(!needs_refresh(&session, 1_000_000));
    }
}
