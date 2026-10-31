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
    if status < 200 || status >= 300 {
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
    if status < 200 || status >= 300 || code != 0 {
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
        if status < 200 || status >= 300 {
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
    if status < 200 || status >= 300 {
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
    if status < 200 || status >= 300 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_support_whitelist() {
        assert!(supports_balance("qoder"));
        assert!(supports_balance("autoclaw-intl"));
        assert!(supports_balance("xiaohuanxiong"));
        assert!(supports_balance("zcode"));
        assert!(!supports_balance("loomy"));
        assert!(!supports_balance("openai"));
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
