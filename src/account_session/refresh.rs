//! 账号级 token 自动刷新 —— 表驱动，按 provider 分派。
//!
//! 协议逐家移植自 Agent2API（`providers/{qoder,autoclaw,trae,codearts,officeace}/
//! refresh.rs` 与 `core/credential_maintenance.rs`，MIT + 使用声明，禁止商用）。设计取向：
//!
//! - **失败绝不改写凭据**（含 refresh_token）：旧快照仍可重试，不会把账号刷废；
//! - 成功时 access/refresh/过期时间**一次 CAS 落盘**（refresh_token 会轮换，
//!   与 access 分开写会把账号弄丢）；
//! - 过期窗口按家不同：qoder/autoclaw 提前 5 分钟，trae 提前 24 小时，
//!   codearts 提前 15 分钟，officeace 提前 30 分钟；
//! - **单飞**（`account_session::flight`）：同一份凭据的并发刷新只发一次请求。
//!   CodeArts / OfficeAce 的 refresh_token 是**一次性**的，并发换证会把它烧掉 ——
//!   CAS 只保证「写回一次生效」，拦不住令牌被消耗两次，所以并发必须在发请求前
//!   收口（巡检轮次与管理面强制刷新端点会撞车）。不做指数退避：调度是 10 分钟
//!   一轮的串行巡检。
//!
//! 刷新令牌来源：`AccountSession.refresh_secret`（管理面可粘贴；登录流程也会
//! 捕获 —— 见 `LoginFlow::refresh_token`）。CodeArts 续期除 refresh_token 还要
//! 登录期那把 DPoP 私钥与 PKCE verifier；OfficeAce 同样要那把 DPoP 私钥，且
//! `refresh_secret` 装的是一个 JSON（控制面凭据 + refresh token），见
//! `refresh_officeace`。（两者均走 `Credential.fields`，见 `LoginFlow::extra_fields`）

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
/// CodeArts 临时凭据寿命短（约 1 小时），对齐 Agent2API 提前 15 分钟续。
const CODEARTS_MARGIN: i64 = 900;
/// OfficeAce 控制面临时凭据寿命 2 小时，对齐上游提前 30 分钟续。
const OFFICEACE_MARGIN: i64 = 1800;
/// Accio / WorkBuddy / 小浣熊：对齐 Agent2API 提前 5 分钟。
const ACCIO_MARGIN: i64 = 300;
const WORKBUDDY_MARGIN: i64 = 300;
const XIAOHUANXIONG_MARGIN: i64 = 300;
/// Cline：官方 25 分钟轮换周期，取 10 分钟提前量。
const CLINE_MARGIN: i64 = 600;
/// OrcaTerm JWT 约 2 小时，提前 10 分钟续。
const ORCATERN_MARGIN: i64 = 600;
/// Antigravity（Google OAuth）access token 1 小时，提前 15 分钟续。
const ANTIGRAVITY_MARGIN: i64 = 900;
/// 智谱清言 JWT 约 2 小时，提前 10 分钟续（与 OrcaTerm 同窗口）。
const CHATGLM_MARGIN: i64 = 600;

/// CodeArts 换证端点与客户端标识（与登录换证同一套，见 `account_login::codearts`）。
const CODEARTS_TOKEN_URL: &str = "https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens";
const CODEARTS_CLIENT_ID: &str = "vscode-codebot";

/// OfficeAce 换证端点与客户端标识（同一个 STS 主机，见 `account_login::officeace`）。
const OFFICEACE_TOKEN_URL: &str = "https://sts.cn-north-4.myhuaweicloud.com/v1/oauth2/tokens";
const OFFICEACE_CLIENT_ID: &str = "pdp5_for_agentarts";

/// 一次刷新的结果。`Invalid` = refresh_token 也死了，只能重新登录。
#[derive(Debug, Clone)]
pub enum RefreshOutcome {
    Refreshed(String),
    Stale,
    Invalid(String),
    Failed(String),
}

/// 进程级单飞表：key = provider + 账号 id + 凭据指纹（见 `flight::key`）。
///
/// 载荷是 `RefreshOutcome` —— 等待者直接复用 leader 的结果，不再重复换证。
static REFRESH_FLIGHTS: crate::account_session::flight::Table<RefreshOutcome> =
    crate::account_session::flight::Table::new();

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
        "qoder"
            | "qoder-intl"
            | "autoclaw"
            | "autoclaw-intl"
            | "trae"
            | "codearts"
            | "officeace"
            | "accio"
            | "accio-cn"
            | "workbuddy"
            | "workbuddy-intl"
            | "xiaohuanxiong"
            | "cline"
            | "cline-free"
            | "cline-pass"
            | "orcaterm"
            | "antigravity"
            | "chatglm"
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
    // Antigravity 没有 access token 时（只有 refresh token）必须先刷一次才能用
    if session.provider == "antigravity" && session.expires_at.filter(|e| *e > 0).is_none() {
        return true;
    }
    let Some(exp) = session.expires_at.filter(|e| *e > 0) else {
        return false;
    };
    let margin = match session.provider.as_str() {
        "trae" => TRAE_MARGIN,
        "autoclaw" | "autoclaw-intl" => AUTOCLAW_MARGIN,
        "codearts" => CODEARTS_MARGIN,
        "officeace" => OFFICEACE_MARGIN,
        "accio" | "accio-cn" => ACCIO_MARGIN,
        "workbuddy" | "workbuddy-intl" => WORKBUDDY_MARGIN,
        "xiaohuanxiong" => XIAOHUANXIONG_MARGIN,
        "cline" | "cline-free" | "cline-pass" => CLINE_MARGIN,
        "orcaterm" => ORCATERN_MARGIN,
        "antigravity" => ANTIGRAVITY_MARGIN,
        "chatglm" => CHATGLM_MARGIN,
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
///
/// 走**单飞**：同一份凭据的并发刷新只发一次真实请求，等待者复用 leader 的结果。
/// 这是正确性要求而非优化 —— CodeArts / OfficeAce 的 refresh_token 一次性，
/// 并发换证会把令牌烧掉（CAS 拦不住令牌被消耗两次）。
pub async fn refresh_one(
    pool: &AccountSessionStore,
    client: &Client,
    session: &AccountSession,
) -> RefreshOutcome {
    let key = crate::account_session::flight::key(
        &session.provider,
        &session.id,
        &session.refresh_secret,
    );
    match crate::account_session::flight::fly(&REFRESH_FLIGHTS, &key, move || {
        refresh_one_inner(pool, client, session)
    })
    .await
    {
        Ok(outcome) => outcome,
        // 单飞层的错误（等待超时 / leader 被取消）当作一次失败：不改写凭据，
        // 下一轮巡检或用户重试都会重新发起。
        Err(error) => RefreshOutcome::Failed(error),
    }
}

/// 真正干活的路径（由单飞的 leader 执行）：换证 → CAS 落盘。
///
/// 返回 `Err` 只用于单飞层能识别的异常；业务上的失败（令牌失效、网络错误）
/// 一律是 `Ok(RefreshOutcome::Invalid/Failed)` —— 它们不该被当成「单飞崩了」。
async fn refresh_one_inner(
    pool: &AccountSessionStore,
    client: &Client,
    session: &AccountSession,
) -> Result<RefreshOutcome, String> {
    let expected = session.credential.secret_plain();
    // Antigravity 可能只有 refresh token（access token 还没换过或已清空）——
    // 这种账号必须先刷一次才能用，不能因为「没有 access token」就跳过。
    if expected.trim().is_empty() && session.provider != "antigravity" {
        return Ok(RefreshOutcome::Failed("账号没有 access token".to_string()));
    }
    let result = match session.provider.as_str() {
        "qoder" | "qoder-intl" => refresh_qoder(client, session).await,
        "autoclaw" | "autoclaw-intl" => refresh_autoclaw(client, session).await,
        "trae" => refresh_trae(client, session).await,
        "codearts" => refresh_codearts(client, session).await,
        "officeace" => refresh_officeace(client, session).await,
        "accio" | "accio-cn" => refresh_accio(client, session).await,
        "workbuddy" | "workbuddy-intl" => refresh_workbuddy(client, session).await,
        "xiaohuanxiong" => refresh_xiaohuanxiong(client, session).await,
        "cline" | "cline-free" | "cline-pass" => refresh_cline(client, session).await,
        "orcaterm" => refresh_orcaterm(client, session).await,
        "antigravity" => refresh_antigravity(client, session).await,
        "chatglm" => refresh_chatglm(client, session).await,
        other => {
            return Ok(RefreshOutcome::Failed(format!("{other} 暂不支持自动刷新")));
        }
    };
    let tokens = match result {
        Ok(tokens) => tokens,
        Err(outcome) => return Ok(outcome),
    };
    let mut next = session.credential.clone();
    next.secret = tokens.access.clone();
    Ok(
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
        },
    )
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

/// CodeArts 续期：用落盘的 DPoP 私钥 + PKCE verifier + refresh_token 再换一份临时
/// AK/SK/STS（对齐 Agent2API `providers/codearts/{refresh,oauth}.rs`）。
///
/// 表单与登录换证同一套（`grant_type=refresh_token`），且**必须**带 DPoP proof ——
/// 上游用 proof 头里的公钥验签，换一把密钥就续不了，所以登录时那把私钥要随账号落盘
/// （见 `account_login::codearts` 的 `extra_fields`）。
async fn refresh_codearts(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let refresh = refresh_plain(session);
    let refresh = refresh.trim().to_string();
    if refresh.is_empty() {
        return Err(RefreshOutcome::Failed(
            "CodeArts 账号缺少 refresh token，请重新登录".to_string(),
        ));
    }
    let dpop_scalar = session.credential.field_plain("dpop_d");
    let code_verifier = session.credential.field_plain("code_verifier");
    if dpop_scalar.trim().is_empty() || code_verifier.trim().is_empty() {
        return Err(RefreshOutcome::Failed(
            "CodeArts 账号缺少续期上下文（DPoP 私钥 / PKCE verifier），请重新登录".to_string(),
        ));
    }
    let proof = crate::account_login::codearts::dpop_proof_from_scalar(
        &dpop_scalar,
        "POST",
        CODEARTS_TOKEN_URL,
    )
    .map_err(RefreshOutcome::Failed)?;
    let form = [
        ("client_id", CODEARTS_CLIENT_ID),
        ("code_verifier", code_verifier.trim()),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
    ];
    let response = client
        .post(CODEARTS_TOKEN_URL)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("DPoP", proof)
        .form(&form)
        .send()
        .await
        .map_err(|e| RefreshOutcome::Failed(format!("CodeArts 续期请求失败：{e}")))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if status == 400 || status == 401 {
        return Err(RefreshOutcome::Invalid(
            "CodeArts 刷新令牌已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "CodeArts 续期被拒（HTTP {status}）"
        )));
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
        return Err(RefreshOutcome::Failed(
            "CodeArts 续期响应缺少 access_key_id / secret_access_key".to_string(),
        ));
    }
    let access = if sts.is_empty() {
        format!("{ak}:{sk}")
    } else {
        format!("{ak}:{sk}:{sts}")
    };
    // refresh_token 会轮换：响应没给就沿用旧的（对齐 Agent2API 的 first_non_empty）
    let rotated = read("refresh_token");
    let next_refresh = if rotated.is_empty() {
        refresh.clone()
    } else {
        rotated
    };
    // 过期时间：响应给的是 RFC3339 字符串；缺失时按临时凭据 1 小时兜底
    let expires_at = first_str(&payload, &["expiration", "expires_at", "expiresAt"])
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|dt| dt.timestamp())
        .or_else(|| Some(chrono::Utc::now().timestamp() + 3600));
    Ok(TokenSet {
        access,
        refresh: next_refresh,
        expires_at,
    })
}

/// OfficeAce 续期：用落盘的 refresh_token + 登录期那把 DPoP 私钥再换一份临时
/// AK/SK/STS（对齐 Agent2API `providers/officeace/oauth.rs::refresh`）。
///
/// ── 与 CodeArts 的异同 ──────────────────────────────────────
/// 同一个 STS 端点、同样的 `DPoP:` proof 头，**表单少一个 `code_verifier`**
/// （OfficeAce 的 refresh grant 不校验 PKCE）。
///
/// 续的是**控制面**凭据 —— 转发的网关凭据（`credential.secret` =
/// base64(model_app_key:model_app_secret)）不过期，本函数一个字都不动它，
/// 因此 `TokenSet.access` 原样带回旧值仅供 CAS 比对。新的临时凭据与轮换后的
/// refresh_token 一起按 JSON 写回 `refresh_secret`（`checkin::claim::
/// officeace_temp_creds` 的读取形态），签到/余额因此不再在登录 2 小时后失效。
async fn refresh_officeace(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let raw = refresh_plain(session);
    let stored: Value = serde_json::from_str(&raw).map_err(|_| {
        RefreshOutcome::Failed("OfficeAce 账号续期载荷不是 JSON，请重新登录".to_string())
    })?;
    let read_stored = |key: &str| {
        stored
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    let refresh = read_stored("refresh_token");
    if refresh.is_empty() {
        return Err(RefreshOutcome::Failed(
            "OfficeAce 账号缺少 refresh token，请重新登录".to_string(),
        ));
    }
    let dpop_scalar = session.credential.field_plain("dpop_d");
    if dpop_scalar.trim().is_empty() {
        return Err(RefreshOutcome::Failed(
            "OfficeAce 账号缺少续期上下文（DPoP 私钥），请重新登录".to_string(),
        ));
    }
    let proof = crate::account_login::codearts::dpop_proof_from_scalar(
        &dpop_scalar,
        "POST",
        OFFICEACE_TOKEN_URL,
    )
    .map_err(RefreshOutcome::Failed)?;
    let form = [
        ("client_id", OFFICEACE_CLIENT_ID),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
    ];
    let response = client
        .post(OFFICEACE_TOKEN_URL)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .header("DPoP", proof)
        .form(&form)
        .send()
        .await
        .map_err(|e| RefreshOutcome::Failed(format!("OfficeAce 续期请求失败：{e}")))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    if status == 400 || status == 401 {
        return Err(RefreshOutcome::Invalid(
            "OfficeAce 刷新令牌已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "OfficeAce 续期被拒（HTTP {status}）"
        )));
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
    if ak.is_empty() || sk.is_empty() {
        return Err(RefreshOutcome::Failed(
            "OfficeAce 续期响应缺少临时 AK/SK".to_string(),
        ));
    }
    // refresh_token 一次性轮换：响应没给就沿用旧的（对齐 Agent2API 的兜底）
    let rotated = read("refresh_token");
    let next_refresh = if rotated.is_empty() {
        refresh.clone()
    } else {
        rotated
    };
    // security_token / project_id 可能不回传：沿用上一次的值，别把已有信息抹掉
    let keep = |fresh: String, key: &str| {
        let previous = read_stored(key);
        if fresh.is_empty() {
            previous
        } else {
            fresh
        }
    };
    let renewal = json!({
        "access_key_id": ak,
        "secret_access_key": sk,
        "security_token": keep(read("security_token"), "security_token"),
        "project_id": keep(read("project_id"), "project_id"),
        "refresh_token": next_refresh,
    })
    .to_string();
    // 过期时间：响应给的是 RFC3339 字符串；缺失时按控制面凭据 2 小时兜底
    let expires_at = first_str(&payload, &["expiration", "expires_at", "expiresAt"])
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text.trim()).ok())
        .map(|dt| dt.timestamp())
        .or_else(|| Some(chrono::Utc::now().timestamp() + 2 * 3600));
    Ok(TokenSet {
        // 转发凭据（网关 Basic）不过期，原样带回归 CAS 比对
        access: session.credential.secret_plain(),
        refresh: renewal,
        expires_at,
    })
}

/// JWT payload 里的字符串声明（用于取 uid 这类标识）。
fn jwt_claim_str(token: &str, keys: &[&str]) -> Option<String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

/// 发表单请求（`application/x-www-form-urlencoded`），返回 (status, body)。
///
/// Google OAuth 的 token 端点只吃表单，与 JSON 版分开。
async fn post_form(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
    form: &[(&str, &str)],
) -> Result<(u16, Value), RefreshOutcome> {
    let mut request = client
        .post(url)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request
        .form(form)
        .send()
        .await
        .map_err(|e| RefreshOutcome::Failed(format!("请求失败：{e}")))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

// ── Accio（accio / accio-cn 同一套协议）──────────────────────────────
//
// 来源：Agent2API `providers/accio/refresh.rs`。access/refresh 都放 JSON body，
// 没有 Authorization 头；两个地区只差 `x-package-region`（GLOBAL / CN）。
// 响应缺 expiresAt 时按 1 小时兜底（与上游默认 TTL 一致）。

async fn refresh_accio(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let access = session.credential.secret_plain();
    let region = if session.provider == "accio-cn" {
        "CN"
    } else {
        "GLOBAL"
    };
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("x-package-region".to_string(), region.to_string()),
        ("x-platform".to_string(), "web".to_string()),
    ];
    let (status, value) = post_json(
        client,
        "https://phoenix-gw.alibaba.com/api/auth/refresh_token",
        &headers,
        json!({ "accessToken": access, "refreshToken": rt }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "Accio 登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "Accio 刷新被拒（HTTP {status}）"
        )));
    }
    // 成功信封可能是 {success,data} 也可能是顶层对象
    let data = value
        .get("data")
        .filter(|d| d.is_object())
        .cloned()
        .unwrap_or_else(|| value.clone());
    let new_access = first_str(&data, &["accessToken", "access_token", "token"])
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("Accio 刷新响应缺 access token".to_string()))?;
    let new_refresh =
        first_str(&data, &["refreshToken", "refresh_token"]).unwrap_or_else(|| rt.to_string());
    let expires_at = first_num(&data, &["expiresAt", "expires_at"])
        .map(normalize_expire_ts)
        .or_else(|| jwt_exp(&new_access));
    Ok(TokenSet {
        access: new_access,
        refresh: new_refresh,
        expires_at,
    })
}

// ── WorkBuddy（workbuddy / workbuddy-intl 同一套协议）────────────────
//
// 来源：Agent2API `core/auth.rs::refresh_session`。refresh_token 走
// `X-Refresh-Token` 请求头（不是 body），附带 `X-Auth-Refresh-Source: plugin`
// 作为客户端身份标记；body 为空对象。

async fn refresh_workbuddy(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let default_base = if session.provider == "workbuddy-intl" {
        "https://www.workbuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| default_base.to_string());
    let url = format!(
        "{}/v2/plugin/auth/token/refresh",
        base.trim_end_matches('/')
    );
    let headers = vec![
        ("X-Refresh-Token".to_string(), rt.to_string()),
        ("X-Auth-Refresh-Source".to_string(), "plugin".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, value) = post_json(client, &url, &headers, json!({})).await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "WorkBuddy 登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "WorkBuddy 刷新被拒（HTTP {status}）"
        )));
    }
    let code = value.get("code").and_then(Value::as_i64);
    if code.is_some_and(|c| c != 0) {
        return Err(RefreshOutcome::Failed(format!(
            "WorkBuddy 刷新失败（code={}）",
            code.unwrap_or_default()
        )));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let access = first_str(&data, &["accessToken", "access_token"])
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("WorkBuddy 刷新响应缺 access token".to_string()))?;
    // 响应里 refreshToken 可能为空 —— 空则沿用旧值，别把账号刷废
    let refresh = first_str(&data, &["refreshToken", "refresh_token"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| rt.to_string());
    let expires_at = first_num(&data, &["expiresAt", "expires_at"])
        .map(normalize_expire_ts)
        .or_else(|| {
            first_num(&data, &["expiresIn", "expires_in"])
                .map(|secs| chrono::Utc::now().timestamp() + secs as i64)
        })
        .or_else(|| jwt_exp(&access));
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── 小浣熊（xiaohuanxiong）───────────────────────────────────────────
//
// 来源：Agent2API `providers/raccoon/credentials.rs::call_refresh_api`。
// 只把 refresh_token 放 body；响应没有 expires_in —— 过期时间从新 token 的
// JWT `exp` 解析，解析不出就保留旧值（写 None 会让下次判定无从下手）。

async fn refresh_xiaohuanxiong(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, value) = post_json(
        client,
        "https://xiaohuanxiong.com/api/web/auth/v1/refresh",
        &headers,
        json!({ "refresh_token": rt }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "小浣熊登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "小浣熊刷新被拒（HTTP {status}）"
        )));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let access = first_str(&data, &["access_token", "accessToken"])
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("小浣熊刷新响应缺 access token".to_string()))?;
    let refresh = first_str(&data, &["refresh_token", "refreshToken"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| rt.to_string());
    let expires_at = jwt_exp(&access).or(session.expires_at);
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── Cline（cline / cline-free / cline-pass 同一套协议）───────────────
//
// 来源：Agent2API `providers/cline/refresh.rs`。三个坑：refreshToken 放 body 且
// `grantType` 是 camelCase（不是 OAuth 标准的 grant_type）；响应里的 accessToken
// 是**裸 JWT**，写回前必须补 `workos:` 前缀；`expiresAt` 是 ISO 8601 字符串。
// refresh_token 在服务端是一次性轮换语义 —— 并发必须在单飞里收口（本模块已做）。

async fn refresh_cline(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, value) = post_json(
        client,
        "https://api.cline.bot/api/v1/auth/refresh",
        &headers,
        json!({ "refreshToken": rt, "grantType": "refresh_token" }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "Cline 登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "Cline 刷新被拒（HTTP {status}）"
        )));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let raw_access = first_str(&data, &["accessToken", "access_token"])
        .ok_or_else(|| RefreshOutcome::Failed("Cline 刷新响应缺 access token".to_string()))?;
    // 前缀是硬要求：去掉会 401，重复则幂等
    let access = if raw_access.starts_with("workos:") {
        raw_access
    } else {
        format!("workos:{raw_access}")
    };
    let refresh = first_str(&data, &["refreshToken", "refresh_token"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| rt.to_string());
    // expiresAt 可能是 ISO 字符串或毫秒数字；都解不出再回退 JWT exp
    let expires_at = first_str(&data, &["expiresAt", "expires_at"])
        .and_then(|s| {
            chrono::DateTime::parse_from_rfc3339(s.trim())
                .ok()
                .map(|dt| dt.timestamp())
        })
        .or_else(|| first_num(&data, &["expiresAt", "expires_at"]).map(normalize_expire_ts))
        .or_else(|| jwt_exp(&access));
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── OrcaTerm ─────────────────────────────────────────────────────────
//
// 来源：Agent2API `providers/orcaterm/credentials.rs::call_refresh_api`。
// 非标准：**没有 refresh_token 概念**，用旧 accessToken 换新 accessToken；
// `uin` 放 query，Bearer 旧 token；响应恒 HTTP 200，业务码在
// `data.Response.Code`（0 = 成功），新 token 在 `data.Response.AccessToken`。

async fn refresh_orcaterm(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let access = session.credential.secret_plain();
    if access.trim().is_empty() {
        return Err(RefreshOutcome::Failed("缺少 access token".to_string()));
    }
    let uid = jwt_claim_str(&access, &["userId", "uin", "uid"])
        .ok_or_else(|| RefreshOutcome::Failed("OrcaTerm 凭证里读不到用户 ID".to_string()))?;
    let url = format!(
        "https://api.orcaterm.cloud.tencent.com/cgi/api?action=OAuthRefreshToken&uin={uid}"
    );
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {access}")),
        (
            "Origin".to_string(),
            "https://orcaterm.cloud.tencent.com".to_string(),
        ),
        (
            "Referer".to_string(),
            "https://orcaterm.cloud.tencent.com/".to_string(),
        ),
    ];
    let (status, value) = post_json(
        client,
        &url,
        &headers,
        json!({ "action": "OAuthRefreshToken", "data": { "accessToken": access } }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "OrcaTerm 登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "OrcaTerm 刷新被拒（HTTP {status}）"
        )));
    }
    let code = value
        .pointer("/data/Response/Code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        let message = value
            .pointer("/data/Response/Message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(RefreshOutcome::Failed(format!(
            "OrcaTerm 刷新失败（{message}）"
        )));
    }
    let new_access = value
        .pointer("/data/Response/AccessToken")
        .and_then(Value::as_str)
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("OrcaTerm 刷新响应缺新 token".to_string()))?;
    let expires_at = jwt_exp(&new_access);
    Ok(TokenSet {
        access: new_access,
        // 没有 refresh_token 这一路：原样保留，别清空
        refresh: refresh_plain(session),
        expires_at,
    })
}

// ── Antigravity（Google OAuth）───────────────────────────────────────
//
// 来源：Agent2API `providers/antigravity/oauth.rs::refresh_access_token`。
// 标准 Google refresh_token grant（form 编码 + 公开的 client_id/secret）；
// UA 必须含 `antigravity` 且版本不低于 4.3.0，否则上游 403。
// refresh_token 是长寿命主凭证（不是一次性），丢了只能重新授权。
//
// client_id/secret 复用登录流程那份（`account_login::antigravity`）——
// 官方 IDE 随包分发的公开值，拆段常量规避 secret scanning，勿在此重复定义。

const ANTIGRAVITY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

async fn refresh_antigravity(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Invalid(
            "Antigravity 缺少 refresh token，请重新授权".to_string(),
        ));
    }
    let headers = vec![
        (
            "User-Agent".to_string(),
            "vscode/1.99.0 (Antigravity/4.3.0)".to_string(),
        ),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let client_id = crate::account_login::antigravity::client_id();
    let client_secret = crate::account_login::antigravity::client_secret();
    let form = [
        ("client_id", client_id.as_str()),
        ("client_secret", client_secret.as_str()),
        ("refresh_token", rt),
        ("grant_type", "refresh_token"),
    ];
    let (status, value) = post_form(client, ANTIGRAVITY_TOKEN_URL, &headers, &form).await?;
    if status == 400 || status == 401 || status == 403 {
        // invalid_grant / invalid_client 都只能重新授权
        let error = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("invalid_grant");
        return Err(RefreshOutcome::Invalid(format!(
            "Antigravity 授权已失效（{error}），请重新授权"
        )));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "Antigravity 刷新被拒（HTTP {status}）"
        )));
    }
    let access = first_str(&value, &["access_token"])
        .ok_or_else(|| RefreshOutcome::Failed("Antigravity 刷新响应缺 access token".to_string()))?;
    let refresh = first_str(&value, &["refresh_token"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| rt.to_string());
    // expires_in 越界（0 / 超过一天 / 缺失）时按 Google 默认 1 小时兜底
    let ttl = first_num(&value, &["expires_in"])
        .filter(|secs| *secs > 0.0 && *secs < 86_400.0)
        .map(|secs| secs as i64)
        .unwrap_or(3_600);
    let expires_at = Some(chrono::Utc::now().timestamp() + ttl);
    Ok(TokenSet {
        access,
        refresh,
        expires_at,
    })
}

// ── 智谱清言 ─────────────────────────────────────────────────────
//
// 续期端点 `POST /chatglm/user-api/user/refresh`（chatglm.cn，实测可用）：
// body `{refresh_token}` → 新 JWT（+ 可能轮换的 refresh_token）。响应可能是
// 平铺 JSON 或 `{code, data}` 信封，两种都认；过期时间优先取响应字段，
// 否则解码新 JWT 的 exp，再回退会话旧值。

async fn refresh_chatglm(
    client: &Client,
    session: &AccountSession,
) -> Result<TokenSet, RefreshOutcome> {
    let rt_owned = refresh_plain(session);
    let rt = rt_owned.trim();
    if rt.is_empty() {
        return Err(RefreshOutcome::Failed("缺少 refresh token".to_string()));
    }
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, value) = post_json(
        client,
        "https://chatglm.cn/chatglm/user-api/user/refresh",
        &headers,
        json!({ "refresh_token": rt }),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(RefreshOutcome::Invalid(
            "智谱清言登录态已失效，请重新登录".to_string(),
        ));
    }
    if !(200..300).contains(&status) {
        return Err(RefreshOutcome::Failed(format!(
            "智谱清言刷新被拒（HTTP {status}）"
        )));
    }
    // 响应可能是平铺 JSON 或 `{code, data}` 信封；取 data（没有就用顶层）。
    let data = value
        .get("data")
        .filter(|d| d.as_object().is_some_and(|o| !o.is_empty()))
        .cloned()
        .unwrap_or(value);
    let access = first_str(&data, &["access_token", "accessToken", "token", "jwt"])
        .map(|t| t.trim_start_matches("Bearer ").to_string())
        .ok_or_else(|| RefreshOutcome::Failed("智谱清言刷新响应缺 access token".to_string()))?;
    let refresh = first_str(&data, &["refresh_token", "refreshToken"])
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| rt.to_string());
    // 过期时间：响应字段优先，否则解码新 JWT 的 exp，再回退会话旧值。
    let expires_at = first_num(&data, &["expires_in"])
        .filter(|secs| *secs > 0.0 && *secs < 86_400.0)
        .map(|secs| chrono::Utc::now().timestamp() + secs as i64)
        .or_else(|| first_num(&data, &["expires_at", "expiresAt"]).map(normalize_expire_ts))
        .or_else(|| jwt_exp(&access))
        .or(session.expires_at);
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
        assert!(supports_refresh("codearts"));
        assert!(supports_refresh("officeace"));
        assert!(supports_refresh("accio"));
        assert!(supports_refresh("accio-cn"));
        assert!(supports_refresh("workbuddy"));
        assert!(supports_refresh("workbuddy-intl"));
        assert!(supports_refresh("xiaohuanxiong"));
        assert!(supports_refresh("cline-free"));
        assert!(supports_refresh("cline-pass"));
        assert!(supports_refresh("orcaterm"));
        assert!(supports_refresh("antigravity"));
        assert!(supports_refresh("chatglm"));
        assert!(!supports_refresh("loomy"));
        assert!(!supports_refresh("kuku"));
        assert!(!supports_refresh("catpaw"));
        assert!(!supports_refresh("openai"));
    }

    #[test]
    fn antigravity_without_expiry_always_needs_refresh() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "antigravity".to_string();
        session.refresh_secret = "rt".to_string();
        session.expires_at = None;
        assert!(needs_refresh(&session, 1_000_000));
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
        // officeace 窗口 30min：剩 20 分钟算临期、剩 2 小时不算
        session.provider = "officeace".to_string();
        assert!(needs_refresh(&session, 1_000_000 - 1_200));
        assert!(!needs_refresh(&session, 1_000_000 - 7_200));
        // 无 refresh_secret 不动
        session.refresh_secret = String::new();
        assert!(!needs_refresh(&session, 1_000_000));
    }
}
