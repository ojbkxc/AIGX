//! 渠道级**余额查询**：七种方案的编排（认证、打哪个地址、读成什么）。
//!
//! 移植自 Agent2API `providers/custom/balance.rs` + `balance_parse.rs`
//! （原实现在账号池上，AIGX 的渠道自带 api_key，因此凭证来源收敛成
//! `Channel::decode_api_key()`）。方案由渠道记录的 `balance_scheme` 决定
//! （预置供应商卡片写入；手工新建 = 空 = 只试 OpenAI 计费端点）。
//!
//! | 方案 | 打哪里 | 认证 | 主读数 |
//! |---|---|---|---|
//! | `sub2api` | `{base}/v1/usage` | 渠道 API Key（Bearer） | 剩余额度 |
//! | `new_api` | `{base}/api/user/self` + `{base}/api/status` | 控制台访问令牌 | `data.quota` |
//! | `deepseek` | `{base}/user/balance` | 渠道 API Key（Bearer） | `balance_infos[].total_balance` |
//! | `opencode` | `{base}/v1/usage` | 渠道 API Key（Bearer） | rolling/weekly/monthly 剩余百分比 |
//! | `sensenova` | `platform.sensenova.cn` 控制面 | 控制台账号密码 | 双积分池窗口剩余百分比 |
//! | `zhipu` | `bigmodel.cn` 控制台 | 控制台令牌 | 账户余额（人民币元） |
//! | `zhipu_zai` | `api.z.ai` 控制面 | API Key 铸 SIGN 签名 | 账户余额（美元） |
//!
//! 前四个打的是**渠道填的 base**；后三个打的是**各自的控制台域**。
//!
//! `new_api` / `sensenova` / `zhipu` 需要额外凭证（访问令牌 / 账号密码 /
//! 控制台令牌），落在渠道记录的 `quirks.headers` 里约定的键（见
//! [`credential_field`]）；没配时返回明确的可读错误。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use serde_json::{json, Map, Value};

/// 方案 id（与渠道记录 `balance_scheme` 逐字一致）
pub const SCHEME_SUB2API: &str = "sub2api";
pub const SCHEME_NEW_API: &str = "new_api";
pub const SCHEME_DEEPSEEK: &str = "deepseek";
pub const SCHEME_OPENCODE: &str = "opencode";
pub const SCHEME_SENSENOVA: &str = "sensenova";
pub const SCHEME_ZHIPU: &str = "zhipu";
pub const SCHEME_ZHIPU_ZAI: &str = "zhipu_zai";

/// 全部合法方案（校验与前端下拉共用）
pub const SCHEMES: &[&str] = &[
    SCHEME_SUB2API,
    SCHEME_NEW_API,
    SCHEME_DEEPSEEK,
    SCHEME_OPENCODE,
    SCHEME_SENSENOVA,
    SCHEME_ZHIPU,
    SCHEME_ZHIPU_ZAI,
];

/// 智谱控制台域（余额记在控制台账户上，与推理端点无关）
const ZHIPU_CONSOLE_BASE: &str = "https://bigmodel.cn";
/// 智谱国际站（z.ai）的账户报告接口
const ZHIPU_ZAI_BALANCE_URL: &str =
    "https://api.z.ai/api/biz/account/query-customer-account-report";
/// 商汤控制面
const SENSENOVA_PLATFORM_BASE: &str = "https://platform.sensenova.cn";
const SENSENOVA_PLATFORM_LITE_BASE: &str = "https://platform-lite.sensenova.cn";

/// 余额请求超时
const REQUEST_TIMEOUT_SECS: u64 = 20;

/// 需要额外凭证的方案：该方案要在 `quirks.headers` 里补哪个键
///
/// 用 `quirks.headers` 承载是刻意的 —— 它本来就是「这家是谁」的派生配置，
/// 渠道记录已经持久化它，不必再为余额查询单开一个字段。
pub fn credential_field(scheme: &str) -> Option<&'static str> {
    match scheme {
        SCHEME_NEW_API => Some("x-balance-access-token"),
        SCHEME_ZHIPU => Some("x-balance-token"),
        SCHEME_SENSENOVA => Some("x-balance-username"),
        _ => None,
    }
}

/// 方案展示名（错误文案里说清是哪一条链路）
pub fn scheme_label(scheme: &str) -> &'static str {
    match scheme {
        SCHEME_SUB2API => "Sub2API",
        SCHEME_NEW_API => "NewAPI",
        SCHEME_DEEPSEEK => "DeepSeek",
        SCHEME_OPENCODE => "OpenCode Go",
        SCHEME_SENSENOVA => "商汤日日新",
        SCHEME_ZHIPU => "智谱",
        SCHEME_ZHIPU_ZAI => "智谱（z.ai 国际）",
        _ => "未知方案",
    }
}

/// 缺凭证时的提示
fn credential_hint(scheme: &str) -> &'static str {
    match scheme {
        SCHEME_NEW_API => "渠道的余额访问令牌（请求头 x-balance-access-token）",
        SCHEME_ZHIPU => "渠道的智谱控制台令牌（请求头 x-balance-token）",
        SCHEME_SENSENOVA => {
            "渠道的商汤账号与密码（请求头 x-balance-username / x-balance-password）"
        }
        _ => "渠道的 API Key",
    }
}

/// 查询结果：主读数 + 展示单位 + 明细（缺键不写，`None` 不是 0）
#[derive(Debug, Clone, Default)]
pub struct BalanceReading {
    pub available: Option<f64>,
    pub unit: String,
    pub wallets: Vec<Value>,
    /// 上游原始响应（排障用）
    pub raw: Value,
}

impl BalanceReading {
    /// 转成对外的 JSON（前端表格只读 available/unit/wallets）
    pub fn to_json(&self) -> Value {
        json!({
            "available": self.available,
            "unit": self.unit,
            "wallets": self.wallets,
            "raw": self.raw,
        })
    }
}

/// 渠道余额查询的统一入口。
///
/// - `scheme` 为空：回落到 OpenAI 计费端点（历史行为）
/// - 方案需要额外凭证但没配：返回 `Err`（可读文案）
pub async fn query(
    ch: &crate::channel::Channel,
    client: &reqwest::Client,
) -> Result<BalanceReading, String> {
    let scheme = ch.balance_scheme.trim();
    if scheme.is_empty() {
        return query_openai_billing(ch, client).await;
    }
    if !SCHEMES.contains(&scheme) {
        return Err(format!(
            "未知的余额查询方案 {scheme}（渠道记录可能被手改过）"
        ));
    }
    let key = ch.decode_api_key();
    let base = ch.base_url.trim_end_matches('/').to_string();
    let timeout = std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS);
    let client = client.clone();

    match scheme {
        SCHEME_SUB2API => {
            let url = format!("{base}/v1/usage");
            let payload = get_json(&client, &url, &bearer(&key), timeout).await?;
            parse_sub2api(&payload).ok_or_else(|| invalid_response(scheme))
        }
        SCHEME_DEEPSEEK => {
            let url = format!("{base}/user/balance");
            let payload = get_json(&client, &url, &bearer(&key), timeout).await?;
            parse_deepseek(&payload).ok_or_else(|| invalid_response(scheme))
        }
        SCHEME_OPENCODE => {
            let url = format!("{base}/v1/usage");
            let payload = get_json(&client, &url, &bearer(&key), timeout).await?;
            parse_opencode(&payload).ok_or_else(|| invalid_response(scheme))
        }
        SCHEME_NEW_API => {
            let token = ch
                .quirks
                .headers
                .get("x-balance-access-token")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("{} 缺少{}", scheme_label(scheme), credential_hint(scheme))
                })?;
            query_new_api(&client, &base, &token, ch, timeout).await
        }
        SCHEME_ZHIPU => {
            let token = ch
                .quirks
                .headers
                .get("x-balance-token")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    format!("{} 缺少{}", scheme_label(scheme), credential_hint(scheme))
                })?;
            let url = format!("{ZHIPU_CONSOLE_BASE}/api/biz/account/query-customer-account-report");
            // 这个接口认的是**裸令牌**头（不是 Bearer）
            let headers = vec![("Authorization".to_string(), token)];
            let payload = get_json(&client, &url, &headers, timeout).await?;
            parse_zhipu(&payload, "CNY", "智谱控制台令牌无效或已过期")
        }
        SCHEME_ZHIPU_ZAI => {
            let jwt = zhipu_sign_jwt(&key).ok_or_else(|| {
                "渠道 API Key 不是智谱的「id.secret」形态，无法用它查询余额".to_string()
            })?;
            let headers = vec![("Authorization".to_string(), jwt)];
            let payload = get_json(&client, ZHIPU_ZAI_BALANCE_URL, &headers, timeout).await?;
            parse_zhipu(&payload, "USD", "智谱 API Key 签名无效或已过期")
        }
        SCHEME_SENSENOVA => query_sensenova(&client, ch, timeout).await,
        _ => Err(format!("未知的余额查询方案 {scheme}")),
    }
}

/// 历史行为：OpenAI 兼容上游的 dashboard/billing 系列端点
async fn query_openai_billing(
    ch: &crate::channel::Channel,
    client: &reqwest::Client,
) -> Result<BalanceReading, String> {
    let base = ch.base_url.trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err("渠道没有填写 Base URL".to_string());
    }
    let key = ch.decode_api_key();
    let endpoints = [
        format!("{base}/dashboard/billing/subscription"),
        format!("{base}/v1/dashboard/billing/subscription"),
        format!("{base}/dashboard/billing/credit_grants"),
    ];
    for url in &endpoints {
        let payload = match get_json(
            client,
            url,
            &bearer(&key),
            std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await
        {
            Ok(payload) => payload,
            Err(_) => continue,
        };
        if let Some(limit) = payload.get("hard_limit_usd").and_then(Value::as_f64) {
            return Ok(BalanceReading {
                available: None,
                unit: "USD".to_string(),
                wallets: vec![json!({
                    "type": "credit", "displayName": "总额度", "balance": limit, "unit": "USD"
                })],
                raw: payload,
            });
        }
        let granted = payload.get("total_granted").and_then(Value::as_f64);
        let used = payload.get("total_used").and_then(Value::as_f64);
        if granted.is_some() || used.is_some() {
            let remaining = (granted.unwrap_or(0.0) - used.unwrap_or(0.0)).max(0.0);
            return Ok(BalanceReading {
                available: Some(remaining),
                unit: "USD".to_string(),
                wallets: vec![json!({
                    "type": "remaining", "displayName": "剩余额度", "balance": remaining, "unit": "USD"
                })],
                raw: payload,
            });
        }
    }
    Err("上游未提供余额接口".to_string())
}

/// NewAPI：账户余额（`/api/user/self`）+ 显示口径（`/api/status`）
async fn query_new_api(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    ch: &crate::channel::Channel,
    timeout: std::time::Duration,
) -> Result<BalanceReading, String> {
    let self_url = format!("{base_url}/api/user/self");
    let status_url = format!("{base_url}/api/status");
    let mut headers = vec![("Authorization".to_string(), format!("Bearer {token}"))];
    if let Some(user_id) = ch
        .quirks
        .headers
        .get("x-balance-user-id")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        headers.push(("new-api-user".to_string(), user_id.to_string()));
    }
    let account = get_json(client, &self_url, &headers, timeout).await?;
    let Some((remaining_raw, used_raw)) = parse_new_api_account(&account) else {
        return Err(invalid_response(SCHEME_NEW_API));
    };
    // 状态接口是公开的：拿不到只影响显示口径，不该让整次查询失败
    let status = get_json(client, &status_url, &[], timeout).await.ok();
    let display = status.as_ref().and_then(new_api_display);
    let (remaining, unit) = new_api_display_amount(remaining_raw, display.as_ref());
    let used = used_raw.map(|used| new_api_display_amount(used, display.as_ref()).0);
    let wallets = vec![
        wallet("remaining", "剩余配额", Some(remaining), &unit),
        wallet("used", "已用配额", used, &unit),
    ]
    .into_iter()
    .flatten()
    .collect();
    Ok(BalanceReading {
        available: Some(remaining),
        unit,
        wallets,
        raw: account,
    })
}

/// 商汤日日新：控制面双积分池用量（账号密码登录换 JWT）
async fn query_sensenova(
    client: &reqwest::Client,
    ch: &crate::channel::Channel,
    timeout: std::time::Duration,
) -> Result<BalanceReading, String> {
    let username = ch
        .quirks
        .headers
        .get("x-balance-username")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("商汤日日新 缺少{}", credential_hint(SCHEME_SENSENOVA)))?;
    let password = ch
        .quirks
        .headers
        .get("x-balance-password")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("商汤日日新 缺少{}", credential_hint(SCHEME_SENSENOVA)))?;
    let jwt = sensenova_login(client, &username, &password, timeout).await?;
    let tenant = jwt_tenant_id(&jwt)
        .ok_or_else(|| "商汤登录票里没有 tenant_id，无法定位账户".to_string())?;
    let account_param: String = url::form_urlencoded::byte_serialize(tenant.as_bytes()).collect();
    let url = format!(
        "{SENSENOVA_PLATFORM_LITE_BASE}/console/v1/tokenplan/pool-usage?account_id={account_param}"
    );
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {jwt}")),
        (
            "Referer".to_string(),
            format!("{SENSENOVA_PLATFORM_BASE}/console"),
        ),
    ];
    let payload = get_json(client, &url, &headers, timeout).await?;
    parse_sensenova(&payload).ok_or_else(|| invalid_response(SCHEME_SENSENOVA))
}

// ── HTTP 与凭证 ─────────────────────────────────────────────────────

fn bearer(key: &str) -> Vec<(String, String)> {
    vec![("Authorization".to_string(), format!("Bearer {key}"))]
}

fn invalid_response(scheme: &str) -> String {
    format!("{} 的余额响应里没有可读的余额字段", scheme_label(scheme))
}

/// GET JSON（非 2xx 与解析失败都给可读错误）
async fn get_json(
    client: &reqwest::Client,
    url: &str,
    headers: &[(String, String)],
    timeout: std::time::Duration,
) -> Result<Value, String> {
    let mut req = client.get(url).timeout(timeout);
    for (name, value) in headers {
        req = req.header(name.as_str(), value.as_str());
    }
    let resp = req.send().await.map_err(|e| format!("请求失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("上游返回 HTTP {status}：{}", shorten(&text, 160)));
    }
    serde_json::from_str(&text).map_err(|_| "上游返回的不是 JSON".to_string())
}

/// 智谱 API Key（`{id}.{secret}`）→ 控制台类接口认的 SIGN 签名 JWT
fn zhipu_sign_jwt(api_key: &str) -> Option<String> {
    use base64::Engine;
    use hmac::Mac;
    type HmacSha256 = hmac::Hmac<sha2::Sha256>;
    let (id, secret) = api_key.trim().split_once('.')?;
    let (id, secret) = (id.trim(), secret.trim());
    if id.is_empty() || secret.is_empty() {
        return None;
    }
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let header = engine.encode(br#"{"alg":"HS256","sign_type":"SIGN"}"#);
    let now = chrono::Utc::now().timestamp_millis();
    let payload = engine.encode(
        serde_json::to_vec(&json!({
            "api_key": id,
            "exp": now + 3_600_000,
            "timestamp": now,
        }))
        .ok()?,
    );
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(format!("{header}.{payload}").as_bytes());
    let signature = engine.encode(mac.finalize().into_bytes());
    Some(format!("{header}.{payload}.{signature}"))
}

/// 商汤控制面登录（换取 JWT）
async fn sensenova_login(
    client: &reqwest::Client,
    username: &str,
    password: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let url = format!("{SENSENOVA_PLATFORM_BASE}/api/v1/login");
    let body = json!({ "username": username, "password": password });
    let resp = client
        .post(&url)
        .timeout(timeout)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("商汤登录请求失败：{e}"))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!(
            "商汤登录失败（HTTP {status}）：{}",
            shorten(&text, 120)
        ));
    }
    let payload: Value =
        serde_json::from_str(&text).map_err(|_| "商汤登录响应不是 JSON".to_string())?;
    payload
        .get("data")
        .and_then(|d| d.get("access_token"))
        .or_else(|| payload.get("access_token"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "商汤登录响应里没有 access_token".to_string())
}

/// 从 JWT 的 payload 段读 `tenant_id`
fn jwt_tenant_id(jwt: &str) -> Option<String> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let bytes = engine.decode(payload).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("tenant_id")
        .or_else(|| value.get("tenantId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ── 解析（纯函数）─────────────────────────────────────────────────

fn number_of(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|v| v.is_finite()),
        Value::String(text) => text.trim().parse::<f64>().ok().filter(|v| v.is_finite()),
        _ => None,
    }
}

fn number_at(map: &Map<String, Value>, key: &str) -> Option<f64> {
    number_of(map.get(key))
}

fn wallet(kind: &str, display_name: &str, balance: Option<f64>, unit: &str) -> Option<Value> {
    let balance = balance?;
    Some(json!({
        "type": kind,
        "displayName": display_name,
        "balance": balance,
        "unit": unit,
    }))
}

fn document(
    available: Option<f64>,
    unit: &str,
    wallets: Vec<Value>,
    raw: &Value,
) -> BalanceReading {
    BalanceReading {
        available,
        unit: unit.to_string(),
        wallets,
        raw: raw.clone(),
    }
}

/// Sub2API `/v1/usage`：`{ quota: {limit, used}, remaining|balance, unit }`
fn parse_sub2api(payload: &Value) -> Option<BalanceReading> {
    let data = payload.as_object()?;
    let quota = data.get("quota").and_then(Value::as_object);
    let total = quota.and_then(|m| number_at(m, "limit"));
    let used = quota.and_then(|m| number_at(m, "used"));
    let remaining = number_at(data, "remaining")
        .or_else(|| number_at(data, "balance"))
        .or_else(|| quota.and_then(|m| number_at(m, "remaining")))
        .or_else(|| match (total, used) {
            (Some(total), Some(used)) => Some(total - used),
            _ => None,
        });
    if remaining.is_none() && total.is_none() && used.is_none() {
        return None;
    }
    let unit = data
        .get("unit")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("USD")
        .to_string();
    let wallets = vec![
        wallet("remaining", "剩余额度", remaining, &unit),
        wallet("used", "已用额度", used, &unit),
        wallet("total", "总额度", total, &unit),
    ]
    .into_iter()
    .flatten()
    .collect();
    Some(document(remaining, &unit, wallets, payload))
}

/// DeepSeek `/user/balance`：`{ balance_infos: [{currency, total_balance, …}] }`
fn parse_deepseek(payload: &Value) -> Option<BalanceReading> {
    let data = payload.as_object()?;
    let infos = data.get("balance_infos")?.as_array()?;
    let mut wallets = Vec::new();
    let mut primary: Option<(f64, String)> = None;
    for item in infos {
        let Some(info) = item.as_object() else {
            continue;
        };
        let Some(amount) = number_at(info, "total_balance") else {
            continue;
        };
        let currency = info
            .get("currency")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("CNY")
            .to_string();
        if primary.is_none() {
            primary = Some((amount, currency.clone()));
        }
        if let Some(row) = wallet("total_balance", &currency, Some(amount), &currency) {
            wallets.push(row);
        }
        if let Some(row) = wallet(
            "granted_balance",
            &format!("{currency} · 赠送余额"),
            number_at(info, "granted_balance"),
            &currency,
        ) {
            wallets.push(row);
        }
        if let Some(row) = wallet(
            "topped_up_balance",
            &format!("{currency} · 充值余额"),
            number_at(info, "topped_up_balance"),
            &currency,
        ) {
            wallets.push(row);
        }
    }
    let (available, unit) = match primary {
        Some((amount, currency)) => (Some(amount), currency),
        None => (None, "CNY".to_string()),
    };
    available?;
    Some(document(available, &unit, wallets, payload))
}

/// OpenCode `/v1/usage`：`{ usage: { rolling, weekly, monthly } }`
fn parse_opencode(payload: &Value) -> Option<BalanceReading> {
    let data = payload.as_object()?;
    let usage = data.get("usage")?.as_object()?;
    struct Window {
        remaining: Option<f64>,
        used: Option<f64>,
    }
    let read = |window: &Map<String, Value>| -> Window {
        let percent = number_at(window, "percent")
            .or_else(|| number_at(window, "usage_percent"))
            .or_else(|| number_at(window, "percent_used"));
        let used_raw = number_at(window, "used")
            .or_else(|| number_at(window, "used_quota"))
            .or_else(|| number_at(window, "usage"))
            .or_else(|| number_at(window, "amount_used"));
        let limit = number_at(window, "limit")
            .or_else(|| number_at(window, "total"))
            .or_else(|| number_at(window, "quota"))
            .or_else(|| number_at(window, "allowance"))
            .or_else(|| number_at(window, "total_quota"));
        let remaining_raw = number_at(window, "remaining")
            .or_else(|| number_at(window, "remaining_quota"))
            .or_else(|| number_at(window, "left"));
        let used = percent.or_else(|| match (used_raw, limit) {
            (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit * 100.0),
            _ => match (remaining_raw, limit) {
                (Some(remaining), Some(limit)) if limit > 0.0 => {
                    Some(((limit - remaining) / limit * 100.0).max(0.0))
                }
                _ => None,
            },
        });
        Window {
            remaining: used.map(|used| (100.0 - used).clamp(0.0, 100.0)),
            used,
        }
    };
    let mut reads: Vec<(&str, &str, Window)> = Vec::new();
    for (key, label) in [
        ("rolling", "5 小时"),
        ("weekly", "每周"),
        ("monthly", "每月"),
    ] {
        let Some(window) = usage.get(key).and_then(Value::as_object) else {
            continue;
        };
        reads.push((key, label, read(window)));
    }
    if reads.is_empty() {
        return None;
    }
    let exhausted = reads.iter().any(|(key, _, w)| {
        (*key == "weekly" || *key == "monthly") && w.used.map(|u| u >= 100.0) == Some(true)
    });
    let rolling = reads.iter().find(|(key, _, _)| *key == "rolling");
    let primary = rolling.or_else(|| reads.first());
    let available = if exhausted {
        Some(0.0)
    } else {
        primary.and_then(|(_, _, w)| w.remaining)
    };
    let wallets: Vec<Value> = reads
        .iter()
        .filter_map(|(key, label, w)| wallet(key, label, w.remaining, "%"))
        .collect();
    Some(document(available, "%", wallets, payload))
}

/// 智谱 `query-customer-account-report`：`{code, msg, data: {balance, …}}`
fn parse_zhipu(
    payload: &Value,
    unit: &str,
    invalid_credential: &str,
) -> Result<BalanceReading, String> {
    let body = payload
        .as_object()
        .ok_or_else(|| "智谱余额响应不是 JSON 对象".to_string())?;
    let code = number_at(body, "code");
    if code != Some(200.0) {
        let message = body
            .get("msg")
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .map(|t| shorten(t, 120))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "未知错误".to_string());
        return Err(
            if matches!(code, Some(401.0) | Some(1000.0) | Some(1001.0)) {
                format!("{invalid_credential}：{message}")
            } else {
                format!(
                    "智谱余额接口返回错误（code={}）：{message}",
                    code.map(|v| v.to_string())
                        .unwrap_or_else(|| "?".to_string())
                )
            },
        );
    }
    let data = body.get("data").and_then(Value::as_object);
    let balance = data
        .and_then(|m| number_at(m, "balance"))
        .or_else(|| data.and_then(|m| number_at(m, "availableBalance")))
        .ok_or_else(|| "智谱余额响应里没有余额字段".to_string())?;
    let wallets = data
        .map(|m| {
            vec![
                wallet(
                    "rechargeAmount",
                    "充值余额",
                    number_at(m, "rechargeAmount"),
                    unit,
                ),
                wallet("giveAmount", "赠送余额", number_at(m, "giveAmount"), unit),
                wallet(
                    "frozenBalance",
                    "冻结金额",
                    number_at(m, "frozenBalance"),
                    unit,
                ),
            ]
            .into_iter()
            .flatten()
            .collect()
        })
        .unwrap_or_default();
    Ok(document(Some(balance), unit, wallets, payload))
}

/// 商汤 `tokenplan/pool-usage`：`{ plan, pools: [{…}] }`
fn parse_sensenova(payload: &Value) -> Option<BalanceReading> {
    let data = payload.as_object()?;
    let pools_raw = data.get("pools")?.as_array()?;
    struct Pool {
        name: String,
        pool_type: String,
        windows: Vec<(String, String, Option<Value>)>,
        grant_balance: Option<f64>,
    }
    let mut pools: Vec<Pool> = Vec::new();
    for item in pools_raw {
        let Some(map) = item.as_object() else {
            continue;
        };
        let window_5h = read_sensenova_window(map.get("window_5h"));
        let window_7d = read_sensenova_window(map.get("window_7d"));
        if window_5h.is_none() && window_7d.is_none() {
            continue;
        }
        pools.push(Pool {
            name: map
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or("积分池")
                .to_string(),
            pool_type: map
                .get("pool_type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            windows: vec![
                ("5h".to_string(), "5 小时".to_string(), window_5h),
                ("7d".to_string(), "7 天".to_string(), window_7d),
            ],
            grant_balance: number_at(map, "grant_balance"),
        });
    }
    if pools.is_empty() {
        return None;
    }
    let general = pools
        .iter()
        .find(|p| p.pool_type == "default")
        .or_else(|| pools.first())?;
    let available = general
        .windows
        .iter()
        .find_map(|(key, _, w)| (key == "5h").then(|| window_remaining_percent(w.as_ref())))
        .flatten()
        .or_else(|| window_remaining_percent(general.windows[1].2.as_ref()));
    let mut wallets = Vec::new();
    for pool in &pools {
        for (key, label, window) in &pool.windows {
            if let Some(row) = wallet(
                key,
                &format!("{} · {}", pool.name, label),
                window_remaining_percent(window.as_ref()),
                "%",
            ) {
                wallets.push(row);
            }
        }
        if let Some(row) = wallet(
            "grant",
            &format!("{} · 活动返赠", pool.name),
            pool.grant_balance,
            "积分",
        ) {
            wallets.push(row);
        }
    }
    Some(document(available, "%", wallets, payload))
}

fn read_sensenova_window(value: Option<&Value>) -> Option<Value> {
    let map = value.and_then(Value::as_object)?;
    let limit = number_at(map, "limit");
    let used = number_at(map, "used");
    let remaining = number_at(map, "remaining");
    if limit.is_none() && used.is_none() && remaining.is_none() {
        return None;
    }
    let mut out = Map::new();
    if let Some(limit) = limit {
        out.insert("limit".to_string(), json!(limit));
    }
    if let Some(used) = used {
        out.insert("used".to_string(), json!(used));
    }
    if let Some(remaining) = remaining {
        out.insert("remaining".to_string(), json!(remaining));
    }
    Some(Value::Object(out))
}

fn window_remaining_percent(window: Option<&Value>) -> Option<f64> {
    let window = window.and_then(Value::as_object)?;
    let limit = number_at(window, "limit").filter(|v| *v > 0.0)?;
    let remaining = number_at(window, "remaining")?;
    Some((remaining / limit * 100.0).clamp(0.0, 100.0))
}

/// NewAPI `/api/status` 的配额显示口径
struct QuotaDisplay {
    display_type: String,
    quota_per_unit: f64,
    usd_exchange_rate: f64,
    custom_currency_symbol: String,
    custom_currency_exchange_rate: f64,
}

fn new_api_display(payload: &Value) -> Option<QuotaDisplay> {
    let data = payload.get("data").and_then(Value::as_object)?;
    let quota_per_unit = number_at(data, "quota_per_unit").filter(|v| *v > 0.0)?;
    let mut display_type = data
        .get("quota_display_type")
        .and_then(Value::as_str)
        .map(|t| t.trim().to_ascii_uppercase())
        .unwrap_or_default();
    if !["USD", "CNY", "TOKENS", "CUSTOM"].contains(&display_type.as_str()) {
        display_type = if data.get("display_in_currency").and_then(Value::as_bool) == Some(false) {
            "TOKENS".to_string()
        } else {
            "USD".to_string()
        };
    }
    let usd_exchange_rate = number_at(data, "usd_exchange_rate").unwrap_or(1.0);
    let custom_currency_exchange_rate =
        number_at(data, "custom_currency_exchange_rate").unwrap_or(1.0);
    if display_type == "CNY" && !(usd_exchange_rate > 0.0) {
        return None;
    }
    if display_type == "CUSTOM" && !(custom_currency_exchange_rate > 0.0) {
        return None;
    }
    Some(QuotaDisplay {
        display_type,
        quota_per_unit,
        usd_exchange_rate,
        custom_currency_symbol: data
            .get("custom_currency_symbol")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string(),
        custom_currency_exchange_rate,
    })
}

fn new_api_display_amount(quota: f64, display: Option<&QuotaDisplay>) -> (f64, String) {
    let Some(display) = display else {
        return (quota, "quota".to_string());
    };
    if display.quota_per_unit <= 0.0 {
        return (quota, "quota".to_string());
    }
    let amount_usd = quota / display.quota_per_unit;
    match display.display_type.as_str() {
        "CNY" if display.usd_exchange_rate > 0.0 => {
            (amount_usd * display.usd_exchange_rate, "CNY".to_string())
        }
        "CUSTOM" if display.custom_currency_exchange_rate > 0.0 => (
            amount_usd * display.custom_currency_exchange_rate,
            if display.custom_currency_symbol.is_empty() {
                "CUSTOM".to_string()
            } else {
                display.custom_currency_symbol.clone()
            },
        ),
        "TOKENS" => (quota, "quota".to_string()),
        _ => (amount_usd, "USD".to_string()),
    }
}

fn parse_new_api_account(payload: &Value) -> Option<(f64, Option<f64>)> {
    let data = payload.get("data").and_then(Value::as_object)?;
    let remaining = number_at(data, "quota")?;
    Some((remaining, number_at(data, "used_quota")))
}

fn shorten(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max.saturating_sub(3)).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_parses_primary_currency() {
        let payload = json!({
            "balance_infos": [
                { "currency": "CNY", "total_balance": "12.5", "granted_balance": "2", "topped_up_balance": "10.5" }
            ]
        });
        let reading = parse_deepseek(&payload).expect("parse");
        assert_eq!(reading.available, Some(12.5));
        assert_eq!(reading.unit, "CNY");
        assert_eq!(reading.wallets.len(), 3);
    }

    #[test]
    fn sub2api_derives_remaining_from_limit_minus_used() {
        let payload = json!({ "quota": { "limit": 100, "used": 30 } });
        let reading = parse_sub2api(&payload).expect("parse");
        assert_eq!(reading.available, Some(70.0));
    }

    #[test]
    fn opencode_zeroes_out_when_weekly_exhausted() {
        let payload = json!({
            "usage": {
                "rolling": { "used": 10, "limit": 100 },
                "weekly": { "used": 100, "limit": 100 },
            }
        });
        let reading = parse_opencode(&payload).expect("parse");
        assert_eq!(reading.available, Some(0.0));
    }

    #[test]
    fn zhipu_rejects_bad_code() {
        let payload = json!({ "code": 1001, "msg": "missing authorization" });
        assert!(parse_zhipu(&payload, "CNY", "令牌无效").is_err());
    }

    #[test]
    fn zhipu_sign_jwt_requires_id_secret_shape() {
        assert!(zhipu_sign_jwt("no-dot-key").is_none());
        assert!(zhipu_sign_jwt("abc.def").is_some());
    }

    #[test]
    fn new_api_display_amount_converts_usd() {
        let display = QuotaDisplay {
            display_type: "USD".to_string(),
            quota_per_unit: 500_000.0,
            usd_exchange_rate: 1.0,
            custom_currency_symbol: String::new(),
            custom_currency_exchange_rate: 1.0,
        };
        let (amount, unit) = new_api_display_amount(1_000_000.0, Some(&display));
        assert_eq!(amount, 2.0);
        assert_eq!(unit, "USD");
    }
}
