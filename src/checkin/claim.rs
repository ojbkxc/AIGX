//! 各 provider 的签到执行体。
//!
//! 每个实现把「上游怎么判断成功」翻译成统一的 [`ClaimResult`]。判据只写在
//! 这里一份，调度、台账、界面都消费同一个结果，不会出现「调度说成功、
//! 台账说失败」的分叉。
//!
//! ## 支持范围（白名单）
//!
//! 只接入 AIGX 已有登录态凭证、且上游协议已被验证过的 provider：
//! `loomy` / `kuku` / `qoder` / `qoder-intl` / `xiaohuanxiong`。
//!
//! 其余登录态 provider（`trae` / `codearts` / `officeace` / `autoclaw` /
//! `workbuddy` / `zcode` / `catpaw` / `accio` 等）**暂不接入**：它们的签到
//! 需要各自的签名算法（COSY / V11 / HMAC）、设备指纹或风控头，照抄一份
//! 未经验证的实现只会每天给上游发一堆失败请求。见 `supports_checkin`。
//!
//! ## 为什么不用「黑名单」
//!
//! 新增 provider 时黑名单默认放行 = 每天对一个未知上游发请求。白名单默认
//! 不签 = 最坏情况是「没签」，管理员看到列表里没有它即可，不会制造噪音。

use reqwest::Client;
use serde_json::{json, Value};

use super::store::CheckinOutcome;
use super::ClaimResult;
use crate::account_session::model::{AccountSession, CREDENTIAL_KIND_SESSION};

/// 签到请求超时（管理类接口，30 秒足够）。
const TIMEOUT_MS: u64 = 30_000;

/// 支持签到的 provider 白名单（与 `claim_for` 的分支一一对应）。
pub const CHECKIN_PROVIDERS: &[&str] = &["loomy", "kuku", "qoder", "qoder-intl", "xiaohuanxiong"];

/// 该 provider 是否支持签到。
pub fn supports_checkin(provider: &str) -> bool {
    CHECKIN_PROVIDERS.contains(&provider)
}

/// 全部支持的 provider（管理面下拉）。
pub fn supported_providers() -> &'static [&'static str] {
    CHECKIN_PROVIDERS
}

/// 执行一次签到。
pub async fn claim_for(client: &Client, session: &AccountSession) -> ClaimResult {
    if session.credential.kind != CREDENTIAL_KIND_SESSION
        && !matches!(
            session.provider.as_str(),
            "loomy" | "kuku" | "qoder" | "qoder-intl" | "xiaohuanxiong"
        )
    {
        return failed("该账号不是登录态凭证，无法签到");
    }
    let secret = session.credential.secret_plain();
    if secret.trim().is_empty() {
        return failed("账号没有凭证，请重新登录或粘贴");
    }
    match session.provider.as_str() {
        "loomy" => loomy(client, session, &secret).await,
        "kuku" => kuku(client, session, &secret).await,
        "qoder" | "qoder-intl" => qoder(client, session, &secret).await,
        "xiaohuanxiong" => xiaohuanxiong(client, session, &secret).await,
        other => failed(&format!("{other} 暂不支持自动签到")),
    }
}

fn failed(msg: &str) -> ClaimResult {
    ClaimResult {
        success: false,
        msg: msg.to_string(),
        outcome: CheckinOutcome::Failed,
    }
}

fn pending(msg: &str) -> ClaimResult {
    ClaimResult {
        success: false,
        msg: msg.to_string(),
        outcome: CheckinOutcome::Pending,
    }
}

fn already(msg: &str) -> ClaimResult {
    ClaimResult {
        success: false,
        msg: msg.to_string(),
        outcome: CheckinOutcome::Already,
    }
}

fn ok(msg: String) -> ClaimResult {
    ClaimResult {
        success: true,
        msg,
        outcome: CheckinOutcome::Success,
    }
}

/// 发送并解析 JSON；非 2xx 返回错误文案。
async fn send_json(
    client: &Client,
    method: reqwest::Method,
    url: &str,
    headers: &[(String, String)],
    body: Option<Value>,
    form: Option<&[(&str, &str)]>,
) -> Result<(u16, Value), String> {
    let mut request = client
        .request(method, url)
        .timeout(std::time::Duration::from_millis(TIMEOUT_MS));
    for (name, value) in headers {
        request = request.header(name, value);
    }
    if let Some(payload) = body {
        request = request.json(&payload);
    } else if let Some(fields) = form {
        request = request.form(fields);
    }
    let response = request.send().await.map_err(|e| {
        if e.is_timeout() {
            "请求超时".to_string()
        } else {
            format!("请求失败：{e}")
        }
    })?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let value = serde_json::from_str::<Value>(&text)
        .unwrap_or_else(|_| json!({ "raw": text.chars().take(300).collect::<String>() }));
    Ok((status, value))
}

/// 取上游错误文案（`msg` / `message` / `desc` / `error`）。
fn upstream_message(value: &Value) -> String {
    for key in ["msg", "message", "desc", "errmsg", "show_msg", "error"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

// ── Loomy ───────────────────────────────────────────────────────────

/// Loomy：`POST {网关}/api/v1/points/first-login`，`token` 头带 session。
///
/// 上游没有独立签到接口，每日赠送积分靠这个「首次登录」调用按天刷回。
/// 成功码 `000000`；重复调用按天幂等。
async fn loomy(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://loomyad.xunfei.cn/api/v1".to_string());
    let base = base.trim_end_matches('/');
    let url = format!("{base}/points/first-login");
    let headers = vec![
        ("token".to_string(), secret.to_string()),
        ("authorization".to_string(), format!("Bearer {secret}")),
        ("content-type".to_string(), "application/json".to_string()),
    ];
    let (status, value) = match send_json(
        client,
        reqwest::Method::POST,
        &url,
        &headers,
        Some(json!({})),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if status == 401 || status == 403 {
        return failed("Loomy 登录态已失效，请重新登录该账号");
    }
    let code = value
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if code == "000000" {
        let balance = value
            .pointer("/data/dailyBalance")
            .and_then(Value::as_i64)
            .or_else(|| {
                value
                    .pointer("/data/data/dailyBalance")
                    .and_then(Value::as_i64)
            });
        return ok(match balance {
            Some(n) => format!("每日赠送积分已刷新（当前 {n}）"),
            None => "每日赠送积分已刷新".to_string(),
        });
    }
    if code == "020002" || code == "100002" {
        return failed("Loomy 登录态已失效，请重新登录该账号");
    }
    let message = upstream_message(&value);
    if message.is_empty() {
        failed(&format!("Loomy 刷新失败（HTTP {status}，code={code}）"))
    } else {
        failed(&format!("Loomy 刷新失败：{message}"))
    }
}

// ── KukuAI ──────────────────────────────────────────────────────────

/// KukuAI 网页形态的 query（对话与 freepoint 共用）。
const KUKU_WEB_QUERY: &str = "clienttype=400&app_id=123971023&web=1&channel=chunlei&version=1.4.4";
const KUKU_BASE: &str = "https://kuku.baidu.com";
const KUKU_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36";

/// KukuAI：`freepoint/homenew` 探面板 + `freepoint/taskComplete` 逐任务领取。
///
/// 每日可自助完成的是 `LOGIN` 与 `CHAT`；`taskComplete` 幂等（重复领
/// `reward_point=0`），所以「已领过」按 `reward_point == 0` 判。
async fn kuku(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let headers = vec![
        ("Cookie".to_string(), secret.to_string()),
        ("User-Agent".to_string(), KUKU_UA.to_string()),
        ("Referer".to_string(), format!("{KUKU_BASE}/genflowpro")),
        ("Origin".to_string(), KUKU_BASE.to_string()),
        ("Accept-Language".to_string(), "zh-CN,zh;q=0.9".to_string()),
    ];
    let panel_url = format!("{KUKU_BASE}/api/genflowpro/freepoint/homenew?{KUKU_WEB_QUERY}");
    let (status, panel) = match send_json(
        client,
        reqwest::Method::GET,
        &panel_url,
        &headers,
        None,
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if status == 401 || status == 403 {
        return failed("KukuAI 登录态已失效，请重新登录该账号");
    }
    let errno = panel.get("errno").and_then(Value::as_i64).unwrap_or(-1);
    if errno != 0 {
        let message = upstream_message(&panel);
        return failed(&format!(
            "KukuAI 面板读取失败（errno={errno}{}）",
            if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            }
        ));
    }

    let tasks = [("LOGIN", "每日登录"), ("CHAT", "完成一次对话")];
    let mut gained: Vec<String> = Vec::new();
    let mut any_ok = false;
    let mut last_error = String::new();
    for (task_type, label) in tasks {
        let url = format!("{KUKU_BASE}/api/genflowpro/freepoint/taskComplete?{KUKU_WEB_QUERY}");
        let result = send_json(
            client,
            reqwest::Method::POST,
            &url,
            &headers,
            None,
            Some(&[("task_type", task_type)]),
        )
        .await;
        let (_, value) = match result {
            Ok(pair) => pair,
            Err(e) => {
                last_error = format!("{label}：{e}");
                continue;
            }
        };
        let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(-1);
        if errno != 0 {
            last_error = format!("{label}：errno={errno} {}", upstream_message(&value));
            continue;
        }
        any_ok = true;
        let reward = value
            .pointer("/data/reward_point")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if reward > 0 {
            gained.push(format!("{label}+{reward}"));
        }
    }

    if !gained.is_empty() {
        return ok(format!("领取成功：{}", gained.join("，")));
    }
    if any_ok {
        return already("今日任务已领取（无新增积分）");
    }
    failed(&format!("KukuAI 领取失败：{last_error}"))
}

// ── Qoder ───────────────────────────────────────────────────────────

const QODER_CAMPAIGNS_PATH: &str = "/sash/api/v1/me/campaigns";
const QODER_CAMPAIGNS_QUERY: &str = "?forceRefresh=true";

/// Qoder：读活动列表找 `CLAIM_BENEFIT`，再逐条 claim。
///
/// 判据只认 `claimStatus` 的翻转（`CLAIMABLE` → `CLAIMED`），不看顶层
/// `claimable` 标志（领完后它仍可能是 true）。`replayed=true` 与 409 都算
/// 「今天已领」。
async fn qoder(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let (open_api, origin) = if session.provider == "qoder-intl" {
        ("https://openapi.qoder.sh", "https://openapi.qoder.sh")
    } else {
        (
            "https://openapi.qoder.com.cn",
            "https://openapi.qoder.com.cn",
        )
    };
    let headers = vec![
        ("authorization".to_string(), format!("Bearer {secret}")),
        ("cosy-clienttype".to_string(), "5".to_string()),
        ("accept".to_string(), "application/json".to_string()),
        ("accept-language".to_string(), "zh-CN".to_string()),
        ("user-agent".to_string(), "qoder-local-proxy".to_string()),
    ];

    let list_url = format!("{open_api}{QODER_CAMPAIGNS_PATH}{QODER_CAMPAIGNS_QUERY}");
    let (status, list) = match send_json(
        client,
        reqwest::Method::GET,
        &list_url,
        &headers,
        None,
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if status == 401 || status == 403 {
        return failed("Qoder 登录态已失效，请重新登录该账号");
    }
    if status >= 400 {
        let message = upstream_message(&list);
        return failed(&format!(
            "Qoder 活动列表读取失败（HTTP {status}）：{message}"
        ));
    }

    let rows = list
        .get("campaigns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut claimable: Option<&Value> = None;
    let mut claimed = false;
    for row in &rows {
        if row.get("actionType").and_then(Value::as_str) != Some("CLAIM_BENEFIT") {
            continue;
        }
        match row.get("claimStatus").and_then(Value::as_str) {
            Some("CLAIMABLE") if claimable.is_none() => claimable = Some(row),
            Some("CLAIMED") => claimed = true,
            _ => {}
        }
    }

    let Some(target) = claimable else {
        return if claimed {
            already("今日已领取")
        } else {
            pending("今天没有可领的签到活动（Free 套餐可能不下发）")
        };
    };

    let campaign_id = target
        .get("campaignId")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if campaign_id.is_empty() {
        return failed("Qoder 签到活动缺少活动标识");
    }
    let claim_url = format!("{open_api}{QODER_CAMPAIGNS_PATH}/{campaign_id}/claim");
    let mut claim_headers = headers.clone();
    claim_headers.push(("content-type".to_string(), "application/json".to_string()));
    claim_headers.push(("origin".to_string(), origin.to_string()));
    let (claim_status, body) = match send_json(
        client,
        reqwest::Method::POST,
        &claim_url,
        &claim_headers,
        Some(json!({})),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if claim_status == 409 || body.get("result").and_then(Value::as_str) == Some("ALREADY_CLAIMED")
    {
        return already("今日已领取");
    }
    if claim_status == 401 || claim_status == 403 {
        return failed("Qoder 登录态已失效，请重新登录该账号");
    }
    if claim_status >= 400 {
        let message = upstream_message(&body);
        return failed(&format!("Qoder 领取失败（HTTP {claim_status}）：{message}"));
    }
    if body.get("status").and_then(Value::as_str) != Some("CLAIMED") {
        let status_text = body.get("status").and_then(Value::as_str).unwrap_or("未知");
        return failed(&format!("Qoder 签到未完成（上游状态 {status_text}）"));
    }
    if body
        .get("replayed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return already("今日已领取");
    }
    let reward = body
        .pointer("/benefit/amount")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let days = body
        .pointer("/benefit/validity/days")
        .and_then(Value::as_f64);
    let msg = match days {
        Some(d) if d > 0.0 => format!("获得 {reward} credits，{d} 天有效"),
        _ => format!("获得 {reward} credits"),
    };
    ok(msg)
}

// ── 小浣熊 ──────────────────────────────────────────────────────────

/// 小浣熊：`GET /api/web/office/v3/setting_info` 触发每日积分发放（按天幂等）。
///
/// 成功判据是「HTTP 通且返回了发放通知」—— 上游没有单独的签到接口，
/// 这个读接口本身就是触发器。重复调用不会重复入账。
async fn xiaohuanxiong(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://xiaohuanxiong.com".to_string());
    // 账号的 base_url 是 `/api/web/llm/v2`，setting_info 在主站根下
    let origin = base
        .split("/api/")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("https://xiaohuanxiong.com")
        .trim_end_matches('/');
    let url = format!("{origin}/api/web/office/v3/setting_info");
    let headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("X-Raccoon-Language".to_string(), "zh".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let (status, value) =
        match send_json(client, reqwest::Method::GET, &url, &headers, None, None).await {
            Ok(pair) => pair,
            Err(e) => return failed(&e),
        };
    if status == 401 || status == 403 {
        return failed("小浣熊登录态已失效，请重新登录该账号");
    }
    if status >= 400 {
        let message = upstream_message(&value);
        return failed(&format!("小浣熊签到失败（HTTP {status}）：{message}"));
    }
    // 发放通知：有 popup/toast 说明今天有入账；没有则是已经领过
    let has_grant = value
        .get("point_grant_popups")
        .is_some_and(|v| !v.is_null())
        || value.get("point_grant_toast").is_some_and(|v| !v.is_null());
    if has_grant {
        ok("每日积分已发放".to_string())
    } else {
        already("今日积分已发放过（无新增）")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_matches_branches() {
        // 白名单里的每一项都必须有 claim 分支，否则会每天发一次「不支持」
        for provider in supported_providers() {
            assert!(
                matches!(
                    *provider,
                    "loomy" | "kuku" | "qoder" | "qoder-intl" | "xiaohuanxiong"
                ),
                "provider {provider} 在白名单里但没有实现"
            );
        }
    }

    #[test]
    fn unknown_provider_not_supported() {
        assert!(!supports_checkin("openai"));
        assert!(!supports_checkin("trae"));
        assert!(supports_checkin("loomy"));
    }
}
