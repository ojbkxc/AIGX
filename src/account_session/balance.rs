//! 账号级余额/额度查询 —— 按 provider 分派，归一成统一形状。
//!
//! 协议移植自 Agent2API（`providers/{qoder,autoclaw,raccoon,zcode}/balance.rs`
//! 与 `ProviderAdapter::query_usage` 的归一契约，MIT + 使用声明）。要点：
//!
//! - 读不出总量时 `available = None`（**不写 0**）——0 会被「余额不足跳号」
//!   误判，`None` 表示「不知道」，与上游约定一致；
//! - 401/登录态失效原样透出，调用方可先走 `refresh` 再重试；
//! - 响应归一为 `{ available, unit, wallets[], subscription? }`，
//!   `wallets[]` 是明细桶（每日积分 / 资源包 / 套餐额度…）。

use reqwest::Client;
use serde_json::{json, Value};

use super::model::AccountSession;

const TIMEOUT_MS: u64 = 20_000;

/// 归一化余额读数。
#[derive(Debug, Default)]
pub struct AccountBalance {
    /// 可用总量（None = 判不出，勿当 0 用）。
    pub available: Option<f64>,
    /// 展示单位（积分 / token / 额度…）。
    pub unit: String,
    /// 明细桶：`{type, displayName, balance, total?, used?}`。
    pub wallets: Vec<Value>,
    /// 订阅信息（判不出不放）。
    pub subscription: Option<Value>,
}

/// 支持余额查询的 provider。
pub fn supports_balance(provider: &str) -> bool {
    matches!(
        provider,
        "qoder"
            | "qoder-intl"
            | "autoclaw"
            | "autoclaw-intl"
            | "xiaohuanxiong"
            | "zcode"
            | "zcode-intl"
            | "kuku"
            | "loomy"
            | "officeace"
            | "accio"
            | "accio-cn"
            | "catpaw"
            | "orcaterm"
            | "cline"
            | "cline-free"
            | "cline-pass"
            | "antigravity"
            | "workbuddy"
            | "workbuddy-intl"
            | "trae"
            | "codearts"
    )
}

/// 查询单账号余额。`Err` 文案可直接展示（含「请重新登录」类指引）。
pub async fn query_balance(
    client: &Client,
    session: &AccountSession,
) -> Result<AccountBalance, String> {
    let secret = session.credential.secret_plain();
    if secret.trim().is_empty() {
        return Err("账号没有凭证，请重新登录或粘贴".to_string());
    }
    match session.provider.as_str() {
        "qoder" | "qoder-intl" => qoder(client, session, &secret).await,
        "autoclaw" | "autoclaw-intl" => autoclaw(client, session, &secret).await,
        "xiaohuanxiong" => xiaohuanxiong(client, session, &secret).await,
        "zcode" | "zcode-intl" => zcode(client, session, &secret).await,
        "kuku" => kuku(client, session, &secret).await,
        "loomy" => loomy(client, session, &secret).await,
        "officeace" => officeace(client, session, &secret).await,
        "accio" | "accio-cn" => accio(client, session, &secret).await,
        "catpaw" => catpaw(client, session, &secret).await,
        "orcaterm" => orcaterm(client, session, &secret).await,
        "cline" | "cline-free" | "cline-pass" => cline(client, session, &secret).await,
        "antigravity" => antigravity(client, session, &secret).await,
        "workbuddy" | "workbuddy-intl" => workbuddy(client, session, &secret).await,
        "trae" => trae(client, session, &secret).await,
        "codearts" => codearts(client, session, &secret).await,
        other => Err(format!("{other} 暂不支持余额查询")),
    }
}

async fn get_json(
    client: &Client,
    url: &str,
    headers: &[(String, String)],
) -> Result<(u16, Value), String> {
    let mut request = client
        .get(url)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await.map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

fn num(value: &Value, path: &str) -> Option<f64> {
    value.pointer(path).and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

/// 发一次带 body 的请求（POST/GET 通用），返回 (状态码, JSON)。
async fn send_json(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    headers: &[(String, String)],
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let mut request = client
        .request(method, url)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(payload) = body {
        request = request.json(payload);
    }
    let response = request.send().await.map_err(|e| format!("请求失败：{e}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

/// 解 JWT payload（不验签，只读声明）。用于取 uid / 账号 id / 过期时间。
fn jwt_payload(token: &str) -> Option<Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let part = token.trim().split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(part).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 从 JWT payload 里按候选键取字符串（数字型声明也转成字符串）。
fn jwt_str(token: &str, keys: &[&str]) -> Option<String> {
    let payload = jwt_payload(token)?;
    keys.iter().find_map(|key| {
        payload.get(*key).and_then(|v| match v {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
    })
}

/// 数字可能以字符串形式返回（各上游口径不一）。
fn as_num(value: Option<&Value>) -> Option<f64> {
    value.and_then(|v| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    })
}

/// 登录态失效的统一文案。
fn auth_expired(provider: &str) -> String {
    format!("{provider} 登录态已失效，请重新登录该账号")
}

// ── Qoder ────────────────────────────────────────────────────────────
//
// GET {openapi}/api/v2/quota/usage；Cosy 头 + Bearer（额度接口无 COSY 签名）。
// available = userQuota.remaining + addOnQuota.remaining + 可用组织包 remaining。

async fn qoder(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let host = if session.provider == "qoder-intl" {
        "https://openapi.qoder.sh"
    } else {
        "https://openapi.qoder.com.cn"
    };
    let headers = vec![
        ("Cosy-Version".to_string(), "1.0.1".to_string()),
        ("Cosy-ClientType".to_string(), "5".to_string()),
        ("User-Agent".to_string(), "qoder-local-proxy".to_string()),
        ("Authorization".to_string(), format!("Bearer {secret}")),
    ];
    let (status, value) = get_json(client, &format!("{host}/api/v2/quota/usage"), &headers).await?;
    if status == 401 || status == 403 {
        return Err("Qoder 登录态已失效，请重新登录该账号".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("Qoder 额度查询失败（HTTP {status}）"));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let unit = data
        .pointer("/userQuota/unit")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("额度")
        .to_string();
    let user_remaining = num(&data, "/userQuota/remaining").unwrap_or(0.0);
    let addon_remaining = num(&data, "/addOnQuota/remaining").unwrap_or(0.0);
    let org_available = data
        .pointer("/orgResourcePackage/available")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let org_remaining = if org_available {
        num(&data, "/orgResourcePackage/remaining").unwrap_or(0.0)
    } else {
        0.0
    };
    // 三个桶全缺 = 没读到数（None，不写 0）
    let any_bucket = data.get("userQuota").is_some()
        || data.get("addOnQuota").is_some()
        || data.get("orgResourcePackage").is_some();
    let mut wallets = Vec::new();
    if let Some(total) = num(&data, "/userQuota/total") {
        wallets.push(json!({
            "type": "user",
            "displayName": "套餐额度",
            "balance": user_remaining,
            "total": total,
            "used": num(&data, "/userQuota/used").unwrap_or(0.0),
        }));
    }
    if let Some(total) = num(&data, "/addOnQuota/total") {
        if total > 0.0 {
            wallets.push(json!({
                "type": "addon",
                "displayName": "个人资源包",
                "balance": addon_remaining,
                "total": total,
            }));
        }
    }
    if org_available {
        if let Some(total) = num(&data, "/orgResourcePackage/cap")
            .or_else(|| num(&data, "/orgResourcePackage/total"))
        {
            wallets.push(json!({
                "type": "org",
                "displayName": "组织资源包",
                "balance": org_remaining,
                "total": total,
            }));
        }
    }
    Ok(AccountBalance {
        available: any_bucket.then_some(user_remaining + addon_remaining + org_remaining),
        unit,
        wallets,
        subscription: None,
    })
}

// ── AutoClaw ─────────────────────────────────────────────────────────
//
// GET {userapi}/agent-assetmgr/api/v2/wallets?biz_app_id=autoclaw（v1 兜底）。
// 签名头与刷新/签到同一套（`refresh::autoclaw_headers`）。

async fn autoclaw(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
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
    let headers = super::refresh::autoclaw_headers(secret);
    let (status, value) = get_json(
        client,
        &format!("{root}/agent-assetmgr/api/v2/wallets?biz_app_id=autoclaw"),
        &headers,
    )
    .await?;
    if status == 401 || status == 403 {
        return Err("AutoClaw 登录态已失效，请重新登录该账号".to_string());
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code == 410000 || code == 400000 {
        return Err("AutoClaw 登录态已失效，请重新登录该账号".to_string());
    }
    if !(200..300).contains(&status) || code != 0 {
        // v1 兜底（部分老账号只有 v1 钱包）
        let (status, value) = get_json(
            client,
            &format!(
                "{root}/agent-assetmgr/api/v1/wallet-instances?wallet_type=all&wallet_scope=all"
            ),
            &headers,
        )
        .await?;
        if status == 401 || status == 403 {
            return Err("AutoClaw 登录态已失效，请重新登录该账号".to_string());
        }
        if !(200..300).contains(&status) {
            return Err(format!("AutoClaw 余额查询失败（HTTP {status}）"));
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let available = num(&data, "/total_balance");
        let wallets = data
            .pointer("/wallet_instances")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        json!({
                            "type": row.get("wallet_scope").cloned().unwrap_or(Value::Null),
                            "displayName": row.get("display").cloned().unwrap_or(Value::Null),
                            "balance": row.get("balance").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        return Ok(AccountBalance {
            available,
            unit: "积分".to_string(),
            wallets,
            subscription: None,
        });
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let available = num(&data, "/total_balance");
    let wallets = data
        .pointer("/wallets")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    json!({
                        "type": row.get("public_wallet_type").cloned().unwrap_or(Value::Null),
                        "displayName": row.get("display_name").cloned().unwrap_or(Value::Null),
                        "balance": row.get("balance").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(AccountBalance {
        available,
        unit: "积分".to_string(),
        wallets,
        subscription: None,
    })
}

// ── 小浣熊（raccoon）────────────────────────────────────────────────
//
// GET {origin}/api/web/points/v1/balance：available_points + 四类积分桶。

async fn xiaohuanxiong(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://xiaohuanxiong.com".to_string());
    let origin = base
        .split("/api/")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("https://xiaohuanxiong.com")
        .trim_end_matches('/');
    let token = secret.trim_start_matches("Bearer ").trim();
    let headers = vec![
        ("Accept".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {token}")),
    ];
    let (status, value) = get_json(
        client,
        &format!("{origin}/api/web/points/v1/balance"),
        &headers,
    )
    .await?;
    if status == 401 || status == 403 {
        return Err("小浣熊登录态已失效，请重新登录该账号".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("小浣熊余额查询失败（HTTP {status}）"));
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        return Err(format!("小浣熊余额查询失败（code={code}）：{message}"));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let mut wallets = Vec::new();
    for (key, label) in [
        ("daily_points", "每日积分"),
        ("monthly_points", "每月积分"),
        ("reward_points", "奖励积分"),
        ("topup_points", "充值积分"),
    ] {
        if let Some(n) = num(&data, &format!("/{key}")) {
            wallets.push(json!({ "type": key, "displayName": label, "balance": n }));
        }
    }
    Ok(AccountBalance {
        available: num(&data, "/available_points"),
        unit: "积分".to_string(),
        wallets,
        subscription: None,
    })
}

// ── ZCode ────────────────────────────────────────────────────────────
//
// GET https://zcode.z.ai/api/v1/zcode-plan/billing/balance（两地同一平面）。
// 需要 `X-Device-Mid`（UUID）：优先账号 extra_headers 里的 device_mid，
// 否则按账号 id 派生一个稳定 UUID（上游缺/非 UUID 会被拒 3001）。
// 无桶时 available = None（不写 0）。

fn stable_uuid(seed: &str) -> String {
    let digest = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        hasher.finalize()
    };
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4 形态，仅取稳定随机位
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

async fn zcode(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let mid = session
        .extra_headers
        .get("device_mid")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| stable_uuid(&session.id));
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("Accept".to_string(), "application/json".to_string()),
        ("X-Device-Mid".to_string(), mid),
    ];
    let url = "https://zcode.z.ai/api/v1/zcode-plan/billing/balance?app_version=3.14.0&platform=win32-x64";
    let (status, value) = get_json(client, url, &headers).await?;
    if status == 401 || value.get("code").and_then(Value::as_i64) == Some(401) {
        return Err("ZCode 套餐令牌已失效，请重新登录该账号".to_string());
    }
    if status == 429 {
        return Err("ZCode 额度查询被限流，请稍后再试".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("ZCode 额度查询失败（HTTP {status}）"));
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        return Err(format!("ZCode 额度查询失败（code={code}）"));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let unit = data
        .pointer("/balances/0/unit_type")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("token")
        .to_string();
    let rows = data
        .pointer("/balances")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut wallets = Vec::new();
    let mut sum = 0.0;
    for row in &rows {
        let remaining = num(row, "/remaining_units").unwrap_or(0.0);
        sum += remaining;
        wallets.push(json!({
            "type": row.get("bucket_id").cloned().unwrap_or(Value::Null),
            "displayName": row.get("show_name").cloned().unwrap_or(Value::Null),
            "balance": remaining,
            "total": num(row, "/total_units"),
            "used": num(row, "/used_units"),
        }));
    }
    let subscription = data
        .pointer("/plans/0/name")
        .and_then(Value::as_str)
        .map(|name| {
            json!({
                "planName": name,
                "expireAt": data.pointer("/plans/0/ends_at").cloned().unwrap_or(Value::Null),
            })
        });
    Ok(AccountBalance {
        // 无桶 = 不知道（None），不是 0
        available: (!rows.is_empty()).then_some(sum),
        unit,
        wallets,
        subscription,
    })
}

// ── Kuku ─────────────────────────────────────────────────────────────
//
// 移植自 Agent2API `providers/kuku/balance.rs`（MIT + 使用声明）。
// `GET /bizapi/gfpro/getgfvipremain` → 顶层包络 `status.code === 0`；
// 积分取 `data.list` 里 `assetName == "token"` 的 `totalPoint`（字符串）。

async fn kuku(
    client: &Client,
    _session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let base = "https://kuku.baidu.com";
    let query = "clienttype=400&app_id=123971023&web=1&channel=chunlei&version=1.4.4";
    let headers = vec![
        ("Cookie".to_string(), secret.to_string()),
        ("User-Agent".to_string(), "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36".to_string()),
        ("Referer".to_string(), format!("{base}/genflowpro")),
        ("Origin".to_string(), base.to_string()),
        ("Accept-Language".to_string(), "zh-CN,zh;q=0.9".to_string()),
    ];
    let url = format!("{base}/bizapi/gfpro/getgfvipremain?{query}");
    let (status, value) = get_json(client, &url, &headers).await?;
    if status == 401 || status == 403 {
        return Err("KukuAI 登录态已失效，请重新登录该账号".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("KukuAI 积分查询失败（HTTP {status}）"));
    }
    // 顶层包络：判据是 status.code（不是双层 data.status.code——初版踩过的坑）
    let code = value
        .pointer("/status/code")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    if code != 0 {
        let message = value
            .pointer("/status/msg")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(format!(
            "KukuAI 积分查询失败（status.code={code}：{message}）"
        ));
    }
    // data.list 各类资产里取 token 那项（找不到回落第一项），totalPoint 是字符串
    let list = value
        .pointer("/data/list")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let token_item = list
        .iter()
        .find(|item| item.get("assetName").and_then(Value::as_str) == Some("token"))
        .cloned()
        .or_else(|| list.first().cloned())
        .unwrap_or(Value::Null);
    let available = token_item
        .get("totalPoint")
        .and_then(Value::as_str)
        .and_then(|s| s.trim().parse::<f64>().ok())
        .or_else(|| token_item.get("totalPoint").and_then(Value::as_f64));
    Ok(AccountBalance {
        available,
        unit: "积分".to_string(),
        wallets: vec![json!({
            "type": "points",
            "displayName": "积分",
            "balance": available,
        })],
        subscription: None,
    })
}

// ── Loomy ────────────────────────────────────────────────────────────
//
// 移植自 Agent2API `providers/loomy/balance.rs`。没有独立余额接口：
// 流水接口 `/api/v2/points/records?pageSize=1` 的 data 同时带账户摘要
// `balance`（永久）与 `dailyBalance`（每日赠送）；available = 两者之和。

async fn loomy(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://loomyad.xunfei.cn/api/v1".to_string());
    let base = base.trim_end_matches('/');
    let headers = vec![
        ("token".to_string(), secret.to_string()),
        ("authorization".to_string(), format!("Bearer {secret}")),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    // pageSize=1 只为拿摘要，不看流水本身
    let url = format!("{base}/points/records?pageNo=1&pageSize=1&direction=debit&granularity=chat");
    let (status, value) = get_json(client, &url, &headers).await?;
    if status == 401 || status == 403 {
        return Err("Loomy 登录态已失效，无法查询积分（请重新登录）".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("Loomy 积分查询失败（HTTP {status}）"));
    }
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if code != "000000" {
        let message = value
            .pointer("/data/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(if message.trim().is_empty() {
            format!("Loomy 积分查询失败（code={code}）")
        } else {
            format!("Loomy 积分查询失败：{message}")
        });
    }
    // 摘要可能在 data 或 data.data 两层（V2 接口形态在不同版本出现过两种）
    let data = value
        .pointer("/data/data")
        .filter(|d| !d.is_null())
        .or_else(|| value.get("data"))
        .cloned()
        .unwrap_or(Value::Null);
    let pick = |key: &str| -> Option<f64> {
        data.get(key).and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
    };
    let permanent = pick("balance").unwrap_or(0.0);
    let daily = pick("dailyBalance").unwrap_or(0.0);
    let mut daily_wallet = json!({
        "type": "daily_points",
        "displayName": "每日赠送积分",
        "balance": daily,
    });
    // 赠送额度的「剩余/上限」是独立展示口径，有就带上，没有不编造
    if let (Some(remaining), Some(limit)) = (pick("dailyRemainingPoints"), pick("dailyLimitPoints"))
    {
        if let Some(obj) = daily_wallet.as_object_mut() {
            obj.insert("remaining".to_string(), json!(remaining));
            obj.insert("limit".to_string(), json!(limit));
        }
    }
    Ok(AccountBalance {
        available: Some(permanent + daily),
        unit: "积分".to_string(),
        wallets: vec![
            json!({
                "type": "permanent_points",
                "displayName": "永久积分",
                "balance": permanent,
            }),
            daily_wallet,
        ],
        subscription: None,
    })
}

// ── OfficeAce ────────────────────────────────────────────────────────
//
// 移植自 Agent2API `providers/officeace/{balance,subscription}.rs`。
// `GET /v1/subscription`（网关根，V11 签名 + `x-subscription-type: v2`），
// 从 `skus[].quotas[]` 聚合套餐积分与联网搜索次数。控制面临时 AK/SK 缺失时
// 给中性指引（与签到同口径：提示重登以留存凭据）。

async fn officeace(
    client: &Client,
    session: &AccountSession,
    _secret: &str,
) -> Result<AccountBalance, String> {
    let Some(cred) = crate::checkin::claim::officeace_temp_creds(session) else {
        return Err(
            "OfficeAce 余额查询需要临时 AK/SK（登录时留存于 refresh_secret），当前账号没有，请重新登录"
                .to_string(),
        );
    };
    // 网关根 = 模型网关基址去掉尾 /v2（订阅族接口挂在网关根上）
    let base = crate::account_session::adapter::effective_base_url(session).unwrap_or_default();
    let base = base.trim().trim_end_matches('/');
    let base = base
        .strip_suffix("/v2")
        .unwrap_or(base)
        .trim_end_matches('/');
    if base.is_empty() {
        return Err("OfficeAce 账号缺少模型网关基址".to_string());
    }
    let url = format!("{base}/v1/subscription");
    let extra = vec![("x-subscription-type".to_string(), "v2".to_string())];
    let headers = crate::checkin::claim::sign_v11(
        "GET",
        &url,
        &extra,
        &[],
        &cred.ak,
        &cred.sk,
        &cred.sts,
        &cred.project,
    );
    let (status, value) = get_json(client, &url, &headers).await?;
    if status == 401 || status == 403 {
        return Err("OfficeAce 登录态已失效，请重新登录该账号".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("OfficeAce 订阅查询失败（HTTP {status}）"));
    }
    // 从 skus[].quotas[] 聚合：officeace_points=套餐积分，online_search_count=搜索次数；
    // 过期配额不计入读数。数值可能是字符串（实测 "sku_value": "500"）
    let now_ms = chrono::Utc::now().timestamp_millis();
    let parse_ms = |v: Option<&Value>| -> i64 {
        v.and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .unwrap_or(0)
    };
    let num_of = |v: Option<&Value>| -> f64 {
        v.and_then(|v| {
            v.as_f64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        })
        .unwrap_or(0.0)
    };
    let mut plan_total = 0.0;
    let mut plan_used = 0.0;
    let mut search_total = 0.0;
    let mut search_used = 0.0;
    let mut plan_name = String::new();
    let mut plan_expire = 0i64;
    let mut wallets = Vec::new();
    let skus = value
        .pointer("/data/skus")
        .or_else(|| value.get("skus"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for sku in &skus {
        if plan_name.is_empty() {
            plan_name = sku
                .get("sku_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
        }
        let expire = parse_ms(sku.get("expired_time"));
        if expire > 0 && (plan_expire == 0 || expire < plan_expire) {
            plan_expire = expire;
        }
        for quota in sku
            .get("quotas")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let expired = parse_ms(quota.get("expired_time"));
            if expired > 0 && expired <= now_ms {
                continue;
            }
            match quota.get("sku_attr_code").and_then(Value::as_str) {
                Some("officeace_points") => {
                    plan_total += num_of(quota.get("sku_value"));
                    plan_used += num_of(quota.get("current_value"));
                    let total = num_of(quota.get("sku_value"));
                    let used = num_of(quota.get("current_value"));
                    let sku_name = sku
                        .get("sku_name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim();
                    wallets.push(json!({
                        "type": "plan",
                        "displayName": if sku_name.is_empty() { "套餐积分" } else { sku_name },
                        "balance": (total - used).floor().max(0.0),
                        "total": total,
                        "used": used,
                    }));
                }
                Some("online_search_count") => {
                    search_total += num_of(quota.get("sku_value"));
                    search_used += num_of(quota.get("current_value"));
                }
                _ => {}
            }
        }
    }
    // 奖励积分：bonuses（全量字段名宽口径）
    let bonus_total = num_of(
        value
            .pointer("/data/bonus/total")
            .or_else(|| value.pointer("/bonus/total")),
    );
    let bonus_used = num_of(
        value
            .pointer("/data/bonus/used")
            .or_else(|| value.pointer("/bonus/used")),
    );
    let bonus_expire = parse_ms(
        value
            .pointer("/data/bonus/expires_at")
            .or_else(|| value.pointer("/bonus/expires_at")),
    );
    let bonus_valid = bonus_expire == 0 || bonus_expire > now_ms;
    let plan_remaining = (plan_total - plan_used).floor().max(0.0);
    let bonus_remaining = if bonus_valid {
        (bonus_total - bonus_used).floor().max(0.0)
    } else {
        0.0
    };
    // 套餐钱包没有逐条配额时至少给一行聚合读数（与上游 summarize 的 wallets 形状对齐）
    if !wallets
        .iter()
        .any(|w| w.get("type") == Some(&json!("plan")))
    {
        wallets.insert(
            0,
            json!({
                "type": "plan",
                "displayName": if plan_name.is_empty() { "套餐积分" } else { plan_name.as_str() },
                "balance": plan_remaining,
            }),
        );
    }
    if bonus_total > 0.0 || bonus_remaining > 0.0 {
        wallets.push(json!({
            "type": "bonus",
            "displayName": "奖励积分",
            "balance": bonus_remaining,
        }));
    }
    if search_total > 0.0 && search_total.is_finite() {
        wallets.push(json!({
            "type": "search",
            "displayName": "联网搜索",
            "unit": "次",
            "balance": (search_total - search_used).floor().max(0.0),
            "total": search_total,
            "used": search_used,
        }));
    }
    let subscription = json!({
        "planName": if plan_name.is_empty() { "OfficeAce".to_string() } else { plan_name },
        "expireAt": plan_expire,
        "remainQuota": plan_remaining,
        "totalQuota": plan_total,
        "searches": if search_total > 0.0 {
            json!({ "remaining": (search_total - search_used).floor().max(0.0), "total": search_total })
        } else {
            Value::Null
        },
    });
    Ok(AccountBalance {
        available: Some(plan_remaining + bonus_remaining),
        unit: "积分".to_string(),
        wallets,
        subscription: Some(subscription),
    })
}

// ── Accio ────────────────────────────────────────────────────────────
//
// 移植自 Agent2API `providers/accio/balance.rs`。上游只暴露已用百分比
// （`GET /api/entitlement/quota`），没有绝对剩余量：available = None
// （契约：判不出勿写 0），unit = "%"，wallets 给「额度余量 = 100 - 已用」。
// 订阅接口可选（免费账号没有），失败不影响额度展示。

async fn accio(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://accio.cn/api".to_string());
    let base = base.trim_end_matches('/');
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, quota) =
        get_json(client, &format!("{base}/api/entitlement/quota"), &headers).await?;
    if status == 401 || status == 403 {
        return Err("Accio 凭证已失效，请重新登录".to_string());
    }
    if !(200..300).contains(&status) {
        return Err(format!("Accio 额度查询失败（HTTP {status}）"));
    }
    let usage_percent = quota
        .get("usagePercent")
        .or_else(|| quota.get("usage_percent"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
        .clamp(0.0, 100.0);
    let countdown = quota
        .get("refreshCountdownSeconds")
        .or_else(|| quota.get("refresh_countdown_seconds"))
        .and_then(Value::as_f64);
    let remain = (100.0 - usage_percent).max(0.0);
    let mut wallet = json!({
        "type": "entitlement",
        "displayName": "额度余量",
        "balance": remain,
        "unit": "%",
        "usedPercent": usage_percent,
    });
    if let Some(seconds) = countdown.filter(|v| *v > 0.0) {
        let total = seconds as i64;
        let days = total / 86_400;
        let hours = (total % 86_400) / 3_600;
        let minutes = (total % 3_600) / 60;
        let text = if days > 0 {
            format!("{days} 天 {hours} 小时")
        } else if hours > 0 {
            format!("{hours} 小时 {minutes} 分")
        } else if minutes > 0 {
            format!("{minutes} 分")
        } else {
            format!("{total} 秒")
        };
        wallet["detail"] = Value::String(format!("{text} 后重置"));
    }
    // 订阅可选：失败当「没有」
    let subscription = match get_json(
        client,
        &format!("{base}/api/entitlement/currentSubscription"),
        &headers,
    )
    .await
    {
        Ok((s, v)) if (200..300).contains(&s) => {
            let text = |keys: &[&str]| -> Value {
                keys.iter()
                    .find_map(|k| v.get(*k).and_then(Value::as_str))
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| Value::String(s.to_string()))
                    .unwrap_or(Value::Null)
            };
            let number = |keys: &[&str]| -> Value {
                keys.iter()
                    .find_map(|k| v.get(*k).and_then(Value::as_f64))
                    .map(|value| json!(value))
                    .unwrap_or(Value::Null)
            };
            json!({
                "planName": text(&["planName", "plan_name", "productName", "name"]),
                "status": text(&["status", "state"]),
                "expireAt": v.get("expireAt").or_else(|| v.get("expiresAt")).cloned().unwrap_or(Value::Null),
                "remainQuota": number(&["remainQuota", "remain", "remaining", "leftQuota"]),
                "totalQuota": number(&["totalQuota", "total", "quota"]),
            })
        }
        _ => Value::Null,
    };
    Ok(AccountBalance {
        available: None, // 上游只有百分比，没有绝对剩余量
        unit: "%".to_string(),
        wallets: vec![wallet],
        // 免费账号没有订阅：null 也是合法值（前端当「没有」显示）
        subscription: Some(subscription),
    })
}

// ── CatPaw ───────────────────────────────────────────────────────────
//
// 协议移植自 Agent2API `providers/catpaw/balance.rs`：
// GET https://catx.nocode.cn/api/gateway/credit/balance，只认 `X-Auth-Token`
// （值就是转发用的同一个 accessToken，不需要「第二个 token」）。

async fn catpaw(
    client: &Client,
    _session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let headers = vec![
        ("X-Auth-Token".to_string(), secret.to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("gray-set".to_string(), "new-agent-sdk".to_string()),
    ];
    let (status, value) = get_json(
        client,
        "https://catx.nocode.cn/api/gateway/credit/balance",
        &headers,
    )
    .await?;
    // 业务侧 4010/4011 也表示凭证失效
    let code = value.get("code").and_then(Value::as_i64);
    if status == 401 || status == 403 || matches!(code, Some(4010) | Some(4011)) {
        return Err(auth_expired("CatPaw"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("CatPaw 余额查询失败（HTTP {status}）"));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    let credits = as_num(data.get("availableCredits"));
    let wallets = match credits {
        Some(balance) => vec![json!({
            "type": "available",
            "displayName": "可用积分",
            "balance": balance,
        })],
        None => Vec::new(),
    };
    let subscription = data
        .get("userPlan")
        .filter(|p| !p.is_null())
        .map(|p| {
            json!({
                "planName": p.get("planName").cloned().unwrap_or(Value::Null),
                "status": if p.get("pro").and_then(Value::as_bool) == Some(true) { json!("pro") } else { Value::Null },
                "expireAt": p.get("expireTime").cloned().unwrap_or(Value::Null),
                "remainQuota": Value::Null,
                "totalQuota": Value::Null,
            })
        });
    Ok(AccountBalance {
        available: credits,
        unit: "credits".to_string(),
        wallets,
        subscription,
    })
}

// ── OrcaTerm ─────────────────────────────────────────────────────────
//
// 协议移植自 Agent2API `providers/orcaterm/balance.rs`：
// POST `{cgi}?action=DescribeUserBillingStatus&uin={uid}`，Bearer JWT；
// 只取 `feature == "hunyuan_ai"` 一项计量。`uin` 来自 JWT 的 `userId`。

async fn orcaterm(
    client: &Client,
    _session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let uid = jwt_str(secret, &["userId", "uin", "uid"])
        .ok_or_else(|| "OrcaTerm 凭证里读不到用户 ID，请重新登录".to_string())?;
    let url = format!(
        "https://api.orcaterm.cloud.tencent.com/cgi/api?action=DescribeUserBillingStatus&uin={uid}"
    );
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {secret}")),
        (
            "Origin".to_string(),
            "https://orcaterm.cloud.tencent.com".to_string(),
        ),
        (
            "Referer".to_string(),
            "https://orcaterm.cloud.tencent.com/".to_string(),
        ),
    ];
    let body = json!({ "action": "DescribeUserBillingStatus", "data": {} });
    let (status, value) =
        send_json(client, reqwest::Method::POST, &url, &headers, Some(&body)).await?;
    if status == 401 || status == 403 {
        return Err(auth_expired("OrcaTerm"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("OrcaTerm 余额查询失败（HTTP {status}）"));
    }
    let response = value
        .pointer("/data/Response")
        .cloned()
        .unwrap_or(Value::Null);
    if response.is_null() {
        return Err("OrcaTerm 余额响应异常（缺少 Response 字段）".to_string());
    }
    let stats = response
        .get("usageStats")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let target = stats
        .iter()
        .find(|item| item.get("feature").and_then(Value::as_str) == Some("hunyuan_ai"));
    let Some(item) = target else {
        return Ok(AccountBalance {
            available: None,
            unit: "次".to_string(),
            wallets: Vec::new(),
            subscription: None,
        });
    };
    let limit = as_num(item.get("quotaLimit"));
    let used = as_num(item.get("quotaUsed")).unwrap_or(0.0);
    let remain = limit.map(|l| (l - used).max(0.0));
    let role = response
        .get("roleType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let plan_name = match role {
        "professional" => "专业版",
        "basic" => "基础版",
        _ => "",
    };
    let wallets = match remain {
        Some(balance) => vec![json!({
            "type": "hunyuan_ai",
            "displayName": "混元额度",
            "balance": balance,
            "total": limit,
            "used": used,
        })],
        None => Vec::new(),
    };
    Ok(AccountBalance {
        available: remain,
        unit: "次".to_string(),
        wallets,
        subscription: Some(json!({
            "planName": if plan_name.is_empty() { Value::Null } else { json!(plan_name) },
            "status": "active",
            "expireAt": item.get("quotaEndDate").cloned().unwrap_or(Value::Null),
            "remainQuota": remain,
            "totalQuota": limit,
        })),
    })
}

// ── Cline（cline / cline-free / cline-pass 同一套协议）───────────────
//
// 协议移植自 Agent2API `providers/cline/balance.rs`：
// `GET {api}/users/{userId}/balance`（余额是微 credit，÷1e6）+ `GET /users/me/plan`
// （没订阅返回 404，视作「无订阅」不报错）。token 必须带 `workos:` 前缀。

async fn cline(
    client: &Client,
    _session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    const API: &str = "https://api.cline.bot/api/v1";
    let raw = secret.trim();
    let token = if raw.starts_with("workos:") {
        raw.to_string()
    } else {
        format!("workos:{raw}")
    };
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {token}")),
        ("X-CLIENT-TYPE".to_string(), "cline-sdk".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    // userId：优先解 JWT，解不出再问 /users/me
    let mut user_id = jwt_str(secret, &["external_id", "sub"]).unwrap_or_default();
    if user_id.is_empty() {
        let (status, me) = get_json(client, &format!("{API}/users/me"), &headers).await?;
        if status == 401 || status == 403 {
            return Err(auth_expired("Cline"));
        }
        if (200..300).contains(&status) {
            user_id = me
                .pointer("/data/userId")
                .or_else(|| me.pointer("/data/id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
    }
    if user_id.is_empty() {
        return Err("Cline 凭证里读不到账号 ID，请重新登录".to_string());
    }

    let (status, value) =
        get_json(client, &format!("{API}/users/{user_id}/balance"), &headers).await?;
    if status == 401 || status == 403 {
        return Err(auth_expired("Cline"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("Cline 余额查询失败（HTTP {status}）"));
    }
    // 官方前端口径：balance 是微 credit，展示时 ÷1e6（负值=欠费，如实保留）
    let credits = as_num(value.pointer("/data/balance")).map(|micro| micro / 1_000_000.0);

    // 套餐 best-effort：404 表示没有订阅，不该让整个查询失败
    let subscription = match get_json(client, &format!("{API}/users/me/plan"), &headers).await {
        Ok((s, v)) if (200..300).contains(&s) => {
            v.pointer("/data").filter(|d| !d.is_null()).map(|d| {
                let text = |keys: &[&str]| -> Value {
                    keys.iter()
                        .find_map(|k| d.get(*k).and_then(Value::as_str))
                        .filter(|s| !s.trim().is_empty())
                        .map(|s| Value::String(s.to_string()))
                        .unwrap_or(Value::Null)
                };
                json!({
                    "planName": text(&["displayName", "name"]),
                    "status": text(&["status"]),
                    "expireAt": d
                        .get("currentPeriodEnd")
                        .or_else(|| d.get("cancelAt"))
                        .cloned()
                        .unwrap_or(Value::Null),
                    "remainQuota": Value::Null,
                    "totalQuota": Value::Null,
                })
            })
        }
        _ => None,
    };

    Ok(AccountBalance {
        available: credits,
        unit: "credit".to_string(),
        wallets: match credits {
            Some(balance) => vec![json!({
                "type": "cline_credit",
                "displayName": "Cline 额度",
                "balance": balance,
            })],
            None => Vec::new(),
        },
        subscription,
    })
}

// ── Antigravity ──────────────────────────────────────────────────────
//
// 协议移植自 Agent2API `providers/antigravity/balance.rs`：
// `POST {base}:retrieveUserQuotaSummary`，三个环境依序尝试。上游**只给比例**
// （remainingFraction），所以不写 `available`（写 0 会触发「余额不足跳号」误剔）。

async fn antigravity(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    const BASES: [&str; 3] = [
        "https://daily-cloudcode-pa.sandbox.googleapis.com/v1internal",
        "https://daily-cloudcode-pa.googleapis.com/v1internal",
        "https://cloudcode-pa.googleapis.com/v1internal",
    ];
    let project = session.credential.field_plain("projectId");
    let body = if project.trim().is_empty() {
        json!({})
    } else {
        json!({ "project": project.trim() })
    };
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        (
            "User-Agent".to_string(),
            "vscode/1.99.0 (Antigravity/4.3.0)".to_string(),
        ),
        ("x-client-name".to_string(), "antigravity".to_string()),
        ("x-client-version".to_string(), "4.3.0".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    if !project.trim().is_empty() {
        headers.push((
            "x-goog-user-project".to_string(),
            project.trim().to_string(),
        ));
    }

    let mut last_error = String::from("上游未返回可用额度");
    for base in BASES {
        let url = format!("{base}:retrieveUserQuotaSummary");
        match send_json(client, reqwest::Method::POST, &url, &headers, Some(&body)).await {
            Ok((status, _)) if status == 401 || status == 403 => {
                return Err(auth_expired("Antigravity"));
            }
            Ok((status, value)) if (200..300).contains(&status) => {
                return Ok(antigravity_from(&value));
            }
            Ok((status, _)) => last_error = format!("HTTP {status}"),
            Err(e) => last_error = e,
        }
    }
    Err(format!("Antigravity 额度查询失败：{last_error}"))
}

/// 把 `groups[].buckets[]` 归一成「每个窗口一行」的钱包。
fn antigravity_from(value: &Value) -> AccountBalance {
    let mut wallets: Vec<Value> = Vec::new();
    let mut tightest: Option<f64> = None;
    if let Some(groups) = value.get("groups").and_then(Value::as_array) {
        for group in groups {
            let group_name = group
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(buckets) = group.get("buckets").and_then(Value::as_array) else {
                continue;
            };
            for bucket in buckets {
                let fraction = bucket.get("remainingFraction").and_then(Value::as_f64);
                let percent = fraction.map(|f| (f * 100.0).clamp(0.0, 100.0));
                if let Some(p) = percent {
                    tightest = Some(tightest.map_or(p, |current| current.min(p)));
                }
                let name = bucket
                    .get("displayName")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(group_name);
                wallets.push(json!({
                    "type": "quota_window",
                    "displayName": name,
                    "balance": Value::Null,
                    "unit": "%",
                    "remainingPercent": percent,
                    "window": bucket.get("window").cloned().unwrap_or(Value::Null),
                    "resetTime": bucket.get("resetTime").cloned().unwrap_or(Value::Null),
                    "detail": percent.map(|p| format!("剩 {p:.0}%")).unwrap_or_default(),
                }));
            }
        }
    }
    let balance = AccountBalance {
        // 上游只有比例：不写 available，避免被「余额不足」逻辑误判
        available: None,
        unit: "%".to_string(),
        wallets,
        subscription: None,
    };
    balance.with_tightest(tightest)
}

impl AccountBalance {
    /// 记录「最紧窗口剩余百分比」到订阅槽（前端当概览显示，不参与跳号判定）。
    fn with_tightest(mut self, tightest: Option<f64>) -> Self {
        if let Some(percent) = tightest {
            self.subscription = Some(json!({
                "planName": Value::Null,
                "status": "active",
                "expireAt": Value::Null,
                "remainQuota": Value::Null,
                "totalQuota": Value::Null,
                "tightestPercent": percent,
            }));
        }
        self
    }
}

// ── WorkBuddy（workbuddy / workbuddy-intl 同一套协议）────────────────

async fn workbuddy(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    let default_base = if session.provider == "workbuddy-intl" {
        "https://www.workbuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| default_base.to_string());
    let base = base.trim_end_matches('/');
    let uid = jwt_str(secret, &["sub", "uid", "userId"])
        .ok_or_else(|| "WorkBuddy 凭证里读不到用户 ID，请重新登录".to_string())?;
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("X-User-Id".to_string(), uid),
        ("Accept".to_string(), "application/json".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
    ];
    let body = json!({
        "PageNumber": 1,
        "PageSize": 100,
        "ProductCode": "p_tcaca",
        "Status": [0, 3],
        "OnlyValidPeriod": true,
    });
    let (status, value) = send_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/v2/billing/meter/get-user-resource"),
        &headers,
        Some(&body),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(auth_expired("WorkBuddy"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("WorkBuddy 额度查询失败（HTTP {status}）"));
    }
    let accounts = value
        .pointer("/data/Response/Data/Accounts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut total = 0.0_f64;
    let mut remain = 0.0_f64;
    let mut wallets: Vec<Value> = Vec::new();
    for pack in &accounts {
        let cap = as_num(pack.get("CycleCapacitySizePrecise")).unwrap_or(0.0);
        let left = as_num(pack.get("CycleCapacityRemainPrecise")).unwrap_or(0.0);
        total += cap;
        remain += left;
        let name = pack
            .get("PackageName")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("资源包");
        wallets.push(json!({
            "type": "resource_pack",
            "displayName": name,
            "balance": left,
            "total": cap,
            "used": (cap - left).max(0.0),
        }));
    }
    if wallets.is_empty() {
        return Ok(AccountBalance {
            available: None,
            unit: "额度".to_string(),
            wallets,
            subscription: None,
        });
    }
    Ok(AccountBalance {
        available: Some(remain),
        unit: "额度".to_string(),
        wallets,
        subscription: Some(json!({
            "planName": Value::Null,
            "status": "active",
            "expireAt": Value::Null,
            "remainQuota": remain,
            "totalQuota": total,
        })),
    })
}

// ── Trae ─────────────────────────────────────────────────────────────
//
// 协议移植自 Agent2API `providers/trae/usage.rs`（只取「积分」这一路读数，
// 其余窗口/速通次数属展示细节）。认证是 `Cloud-IDE-JWT`，不是 Bearer。

async fn trae(
    client: &Client,
    _session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    const URL: &str = "https://api.trae.cn/trae/api/v2/pay/ide_user_ent_usage";
    let headers = vec![
        (
            "Authorization".to_string(),
            format!("Cloud-IDE-JWT {secret}"),
        ),
        ("Content-Type".to_string(), "application/json".to_string()),
        (
            "User-Agent".to_string(),
            "VSCode 1.107.1 (AIGX)".to_string(),
        ),
        ("X-User-Region".to_string(), "CN".to_string()),
        ("Accept-Language".to_string(), "zh-CN".to_string()),
        ("X-Lgw-Req-Sdk-Type".to_string(), "3".to_string()),
        ("X-Request-Id".to_string(), uuid::Uuid::new_v4().to_string()),
    ];
    let (status, value) = send_json(
        client,
        reqwest::Method::POST,
        URL,
        &headers,
        Some(&json!({})),
    )
    .await?;
    if status == 401 || status == 403 {
        return Err(auth_expired("Trae"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("Trae 额度查询失败（HTTP {status}）"));
    }
    let packs = value
        .get("user_entitlement_pack_list")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut limit_total = 0.0_f64;
    let mut used_total = 0.0_f64;
    let mut seen = false;
    let mut expire_at = Value::Null;
    let mut plan_name = Value::Null;
    for pack in &packs {
        if let Some(info) = pack.get("entitlement_base_info") {
            if expire_at.is_null() {
                expire_at = info
                    .get("end_time")
                    .or_else(|| info.get("expire_time"))
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            if plan_name.is_null() {
                plan_name = info.get("product_type").cloned().unwrap_or(Value::Null);
            }
        }
        // 三档额度：基础 / 订阅 / 资源包，逐档累加 credits_limit
        for section in ["base_quota", "subscription_quota", "package_quota"] {
            if let Some(limit) = as_num(pack.pointer(&format!("/{section}/credits_limit"))) {
                limit_total += limit;
                seen = true;
            }
        }
        if let Some(used) = as_num(pack.pointer("/usage/credits_amount")) {
            used_total += used;
        }
    }
    if !seen {
        return Ok(AccountBalance {
            available: None,
            unit: "积分".to_string(),
            wallets: Vec::new(),
            subscription: None,
        });
    }
    let remain = (limit_total - used_total).max(0.0);
    Ok(AccountBalance {
        available: Some(remain),
        unit: "积分".to_string(),
        wallets: vec![json!({
            "type": "credits_pool",
            "displayName": "积分池",
            "balance": remain,
            "total": limit_total,
            "used": used_total,
        })],
        subscription: Some(json!({
            "planName": plan_name,
            "status": "active",
            "expireAt": expire_at,
            "remainQuota": remain,
            "totalQuota": limit_total,
        })),
    })
}

// ── CodeArts ─────────────────────────────────────────────────────────
//
// 协议移植自 Agent2API `providers/codearts/balance.rs`：统计接口走华为云
// SDK-HMAC-SHA256 签名。AIGX 已有同一套签名器（`CodeArtsBridge::sign`），
// 此处直接复用，避免算法分叉；福利余额走另一个 host，属 best-effort。

async fn codearts(
    client: &Client,
    session: &AccountSession,
    secret: &str,
) -> Result<AccountBalance, String> {
    use crate::bridge::session::codearts::CodeArtsBridge;
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://snap-access.cn-north-4.myhuaweicloud.com".to_string());
    let base = base.trim_end_matches('/');
    let bridge = CodeArtsBridge::new(base, secret, client.clone());
    let url = format!("{base}/snap-manager/v1/statistics/plugin");
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
        ("X-Language".to_string(), "zh-cn".to_string()),
        ("plugin-name".to_string(), "PromptCenter".to_string()),
        ("plugin-version".to_string(), "1.0.0".to_string()),
        (
            "X-Sdk-Date".to_string(),
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string(),
        ),
    ];
    let mut signed = headers.clone();
    match bridge.sign("GET", &url, &headers, b"") {
        Some(auth) => signed.push(("Authorization".to_string(), auth)),
        None => return Err("CodeArts 凭据缺少 AccessKey/SecretKey，无法签名".to_string()),
    }
    let (status, value) = send_json(client, reqwest::Method::GET, &url, &signed, None).await?;
    if status == 401 || status == 403 {
        return Err(auth_expired("CodeArts"));
    }
    if !(200..300).contains(&status) {
        return Err(format!("CodeArts 额度查询失败（HTTP {status}）"));
    }

    let mut wallets: Vec<Value> = Vec::new();
    let mut remain_total = 0.0_f64;
    let mut has_credit = false;
    if let Some(metrics) = value.get("metrics").and_then(Value::as_array) {
        for metric in metrics {
            let remain = as_num(metric.get("package_credit_remain"));
            let used = as_num(metric.get("package_credit_used"));
            let total = as_num(metric.get("package_credit_amount"));
            let Some(remain) = remain else { continue };
            // 汇总行（usageTotalPackageCredit）与分项重复，跳过汇总避免双计
            let name = metric
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if name.eq_ignore_ascii_case("usageTotalPackageCredit") {
                continue;
            }
            has_credit = true;
            remain_total += remain;
            wallets.push(json!({
                "type": "package_credit",
                "displayName": if name.is_empty() { "套餐积分" } else { name },
                "balance": remain,
                "total": total,
                "used": used,
            }));
        }
    }
    let plan_name = value
        .pointer("/package/package_name_cn")
        .or_else(|| value.pointer("/package/package_name_en"))
        .cloned()
        .unwrap_or(Value::Null);
    let expire_at = value
        .pointer("/package/end_date")
        .or_else(|| value.get("end_date"))
        .cloned()
        .unwrap_or(Value::Null);

    Ok(AccountBalance {
        available: if has_credit { Some(remain_total) } else { None },
        unit: "积分".to_string(),
        wallets,
        subscription: Some(json!({
            "planName": plan_name,
            "status": "active",
            "expireAt": expire_at,
            "remainQuota": if has_credit { json!(remain_total) } else { Value::Null },
            "totalQuota": Value::Null,
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_support_whitelist() {
        assert!(supports_balance("qoder"));
        assert!(supports_balance("autoclaw-intl"));
        assert!(supports_balance("xiaohuanxiong"));
        assert!(supports_balance("zcode"));
        assert!(supports_balance("kuku"));
        assert!(supports_balance("loomy"));
        assert!(supports_balance("officeace"));
        assert!(supports_balance("accio"));
        assert!(supports_balance("accio-cn"));
        assert!(supports_balance("catpaw"));
        assert!(supports_balance("orcaterm"));
        assert!(supports_balance("cline-free"));
        assert!(supports_balance("cline-pass"));
        assert!(supports_balance("antigravity"));
        assert!(supports_balance("workbuddy"));
        assert!(supports_balance("workbuddy-intl"));
        assert!(supports_balance("trae"));
        assert!(supports_balance("codearts"));
        assert!(!supports_balance("openai"));
        assert!(!supports_balance("monkeycode"));
        assert!(!supports_balance("commandcode"));
    }

    #[test]
    fn stable_uuid_shape() {
        let id = stable_uuid("account-1");
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        // 同种子稳定
        assert_eq!(id, stable_uuid("account-1"));
    }
}
