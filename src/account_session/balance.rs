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
        return Err(format!("KukuAI 积分查询失败（status.code={code}：{message}）"));
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
        data.get(key)
            .and_then(|v| {
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
    if let (Some(remaining), Some(limit)) = (pick("dailyRemainingPoints"), pick("dailyLimitPoints")) {
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
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_default();
    let base = base.trim().trim_end_matches('/');
    let base = base.strip_suffix("/v2").unwrap_or(base).trim_end_matches('/');
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
        for quota in sku.get("quotas").and_then(Value::as_array).into_iter().flatten() {
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
                    let sku_name = sku.get("sku_name").and_then(Value::as_str).unwrap_or("").trim();
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
    let bonus_total = num_of(value.pointer("/data/bonus/total").or_else(|| value.pointer("/bonus/total")));
    let bonus_used = num_of(value.pointer("/data/bonus/used").or_else(|| value.pointer("/bonus/used")));
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
    if !wallets.iter().any(|w| w.get("type") == Some(&json!("plan"))) {
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
    let (status, quota) = get_json(client, &format!("{base}/api/entitlement/quota"), &headers).await?;
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
    let subscription = match get_json(client, &format!("{base}/api/entitlement/currentSubscription"), &headers).await {
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
