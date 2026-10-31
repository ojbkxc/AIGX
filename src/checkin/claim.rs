//! 各 provider 的签到执行体。
//!
//! 每个实现把「上游怎么判断成功」翻译成统一的 [`ClaimResult`]。判据只写在
//! 这里一份，调度、台账、界面都消费同一个结果，不会出现「调度说成功、
//! 台账说失败」的分叉。
//!
//! ## 支持范围（白名单）
//!
//! 只接入 AIGX 已有登录态凭证、且上游协议已被验证过的 provider：
//! `loomy` / `kuku` / `qoder` / `qoder-intl` / `xiaohuanxiong` /
//! `autoclaw` / `autoclaw-intl` / `workbuddy` / `workbuddy-intl` /
//! `trae` / `codearts` / `officeace`。
//!
//! 签到协议逐家移植自 Agent2API（`core/providers/*/checkin.rs`、`billing/`，
//! MIT + 使用声明）。未列进白名单的 provider（`zcode` / `catpaw` / `accio` 等）
//! **不接入**：它们没有每日签到通道，或协议尚未验证 —— 白名单默认不签，
//! 最坏情况是「没签」，不会制造失败噪音。见 `supports_checkin`。
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
pub const CHECKIN_PROVIDERS: &[&str] = &[
    "loomy",
    "kuku",
    "qoder",
    "qoder-intl",
    "xiaohuanxiong",
    "autoclaw",
    "autoclaw-intl",
    "workbuddy",
    "workbuddy-intl",
    "trae",
    "codearts",
    "officeace",
];

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
    if session.credential.kind != CREDENTIAL_KIND_SESSION {
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
        "autoclaw" | "autoclaw-intl" => autoclaw(client, session, &secret).await,
        "workbuddy" | "workbuddy-intl" => workbuddy(client, session, &secret).await,
        "trae" => trae(client, session, &secret).await,
        "codearts" => codearts(client, session, &secret).await,
        "officeace" => officeace(client, session).await,
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
async fn kuku(client: &Client, _session: &AccountSession, secret: &str) -> ClaimResult {
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

// ── AutoClaw（国内 / 国际共用，仅域名不同）────────────────────────────
//
// 来源：Agent2API `providers/autoclaw/checkin.rs`（MIT + 使用声明，禁止商用）。
// 签名是静态 MD5：`md5("{appId}&{ts}&{appKey}")` 小写 hex（`refresh.rs` 常量）。

const AUTOCLAW_APPID: &str = "100003";
const AUTOCLAW_APPKEY: &str = "38d2391985e2369a5fb8227d8e6cd5e5";

async fn autoclaw(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let default = if session.provider == "autoclaw-intl" {
        "https://autoglm-api.autoglm.ai"
    } else {
        "https://autoglm-acceleration-api.zhipuai.cn"
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| default.to_string());
    // 账号 base_url 可能带尾巴 `/autoclaw-proxy/proxy/autoclaw`，签到要打到 proxy 根
    let root = base
        .trim_end_matches('/')
        .trim_end_matches("/autoclaw")
        .trim_end_matches("/proxy");
    let url = format!("{root}/autoclaw-proxy/proxy/autoclaw-task-complete");
    let ts = chrono::Utc::now().timestamp().to_string();
    let sign = format!(
        "{:x}",
        md5::compute(format!("{AUTOCLAW_APPID}&{ts}&{AUTOCLAW_APPKEY}"))
    );
    let trace = uuid::Uuid::new_v4().to_string();
    let bearer = format!("Bearer {secret}");
    let headers = vec![
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
        ("X-Trace-Id".to_string(), trace),
        ("authorization".to_string(), bearer),
    ];
    let (status, value) = match send_json(
        client,
        reqwest::Method::POST,
        &url,
        &headers,
        Some(json!({ "task_id": "daily_signin" })),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if status == 401 || status == 403 {
        return failed("AutoClaw 登录态已失效，请重新登录该账号");
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code == 410000 || code == 400000 {
        return failed("AutoClaw 登录态已失效，请重新登录该账号");
    }
    if code != 0 {
        let message = upstream_message(&value);
        return failed(&format!(
            "AutoClaw 签到失败（code={code}）{}",
            if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            }
        ));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    if data
        .get("already_completed")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return already("今天已签到");
    }
    let reward = data
        .get("reward_points")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if reward > 0 {
        return ok(format!("签到成功，获得 {reward} 积分"));
    }
    if data
        .get("success")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return ok("签到成功".to_string());
    }
    let message = upstream_message(&value);
    if message.is_empty() {
        failed("AutoClaw 签到未完成")
    } else {
        failed(&format!("AutoClaw 签到未完成：{message}"))
    }
}

// ── WorkBuddy（国内 / 国际共用；国际版多一步活跃状态探测）──────────────
//
// 来源：Agent2API `billing/request.rs` + `billing/activity.rs`（MIT + 使用声明）。
// 国内：`POST {ep}/v2/billing/meter/daily-checkin`；已签到 = HTTP 400 +「已签到」。
// 国际：先 `POST {ep}/billing/meter/checkin-activity-status`（**无 /v2**），
//       `today_checked_in` → 已领；`!active` → 活动未开（中性）；否则走同一条领取。

/// 从 JWT payload 里取账号 uid（`sub` / `uid` / `userId`），不校验签名。
fn jwt_subject(token: &str) -> Option<String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    for key in ["sub", "uid", "userId", "user_id"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if !text.is_empty() {
                return Some(text.to_string());
            }
        }
        if let Some(num) = value.get(key).and_then(Value::as_i64) {
            return Some(num.to_string());
        }
    }
    None
}

async fn workbuddy(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    let intl = session.provider == "workbuddy-intl";
    let default = if intl {
        "https://www.workbuddy.ai"
    } else {
        "https://copilot.tencent.com"
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| default.to_string());
    let base = base.trim_end_matches('/');
    // uid：JWT 声明优先，兜底取备注名里的「WorkBuddy {uid}」
    let uid = jwt_subject(secret).unwrap_or_else(|| {
        session
            .name
            .strip_prefix("WorkBuddy ")
            .unwrap_or("")
            .trim()
            .to_string()
    });
    let mut headers = vec![
        ("Accept".to_string(), "application/json".to_string()),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {secret}")),
    ];
    if !uid.is_empty() {
        headers.push(("X-User-Id".to_string(), uid));
    }

    if intl {
        let status_url = format!("{base}/billing/meter/checkin-activity-status");
        let (status, value) = match send_json(
            client,
            reqwest::Method::POST,
            &status_url,
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
            return failed("WorkBuddy 登录态已失效，请重新登录该账号");
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let checked = data
            .get("today_checked_in")
            .or_else(|| data.get("todayCheckedIn"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if checked {
            return already("今日已领取");
        }
        if !data.get("active").and_then(Value::as_bool).unwrap_or(false) {
            return pending("每日活跃活动未开启");
        }
    }

    let claim_url = format!("{base}/v2/billing/meter/daily-checkin");
    let (status, value) = match send_json(
        client,
        reqwest::Method::POST,
        &claim_url,
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
        return failed("WorkBuddy 登录态已失效，请重新登录该账号");
    }
    let message = upstream_message(&value);
    if (status == 400 || status == 409) && message.contains("已签到") {
        return already("今天已签到");
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 || status >= 400 {
        return failed(&format!(
            "WorkBuddy 签到失败（HTTP {status}，code={code}）{}",
            if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            }
        ));
    }
    let data = value.get("data").cloned().unwrap_or(Value::Null);
    if data.is_null() {
        return failed("WorkBuddy 签到返回异常（data 为空）");
    }
    let days = data
        .get("continuousDays")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let points = data.get("points").and_then(Value::as_i64).unwrap_or(0);
    if days > 0 && points > 0 {
        ok(format!("签到成功，连续 {days} 天，获得 {points} 积分"))
    } else if days > 0 {
        ok(format!("签到成功，连续 {days} 天"))
    } else {
        ok("签到成功".to_string())
    }
}

// ── Trae ─────────────────────────────────────────────────────────────
//
// 来源：Agent2API `providers/trae/checkin.rs`（MIT + 使用声明）。
// 一轮 status → claim → 回查共用**一个全新 16 位数字设备号**（风控要求，
// 凭证自带 device_id 只做门禁）；回查翻转才算成功，失败文案不得含「已签到」。

async fn trae(client: &Client, _session: &AccountSession, secret: &str) -> ClaimResult {
    use rand::Rng;
    let device: String = (0..16)
        .map(|_| char::from(b'0' + rand::thread_rng().gen_range(0..10u8)))
        .collect();
    let request_id = uuid::Uuid::new_v4().to_string();
    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
        (
            "User-Agent".to_string(),
            "VSCode 1.107.1 (windows)".to_string(),
        ),
        ("X-User-Region".to_string(), "CN".to_string()),
        ("Accept-Language".to_string(), "zh-CN".to_string()),
        ("Package-Type".to_string(), "stable_cn".to_string()),
        ("X-Lgw-Req-Sdk-Type".to_string(), "3".to_string()),
        (
            "X-Market-Client-Id".to_string(),
            "VSCode 1.107.1".to_string(),
        ),
        ("X-Device-Brand".to_string(), "83DG".to_string()),
        ("X-Device-Type".to_string(), "windows".to_string()),
        ("X-OS-Version".to_string(), "Windows 11 Pro".to_string()),
        ("App-Version".to_string(), "0.1.61".to_string()),
        ("X-Request-Id".to_string(), request_id),
        ("X-Device-Id".to_string(), device.clone()),
        (
            "Authorization".to_string(),
            format!("Cloud-IDE-JWT {secret}"),
        ),
    ];
    let host = "https://api.trae.cn";
    let status_url = format!("{host}/trae/api/v2/ug/checkin_credits/status");
    let claim_url = format!("{host}/trae/api/v2/ug/checkin_credits/claim");

    // 完成判定：同设备号回查的状态字段（顶层字段，不在 data 里）
    let done_today = |value: &Value| {
        value
            .get("checked_in")
            .or_else(|| value.get("did_checked_in"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || value
                .get("did_checked_in")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    };

    let (status, value) = match send_json(
        client,
        reqwest::Method::POST,
        &status_url,
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
        return failed("Trae 登录态已失效，请重新登录该账号");
    }
    let code = value.get("code").and_then(Value::as_i64).unwrap_or(0);
    if code != 0 {
        let message = upstream_message(&value);
        return failed(&format!("Trae 状态读取失败（code={code}）：{message}"));
    }
    if !value
        .get("enable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return pending("Trae 签到未开放");
    }
    if done_today(&value) {
        return already("今日已签到");
    }

    let (claim_status, body) = match send_json(
        client,
        reqwest::Method::POST,
        &claim_url,
        &headers,
        Some(json!({})),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if claim_status == 401 || claim_status == 403 {
        return failed("Trae 登录态已失效，请重新登录该账号");
    }
    let claim_code = body.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if claim_code == 9074 {
        return pending("Trae 签到参与人数过多，请稍后再试");
    }
    if claim_code == 1001 {
        return failed("Trae 登录令牌已作废，请重新登录该账号");
    }
    if claim_code != 0 {
        let message = upstream_message(&body);
        return failed(&format!("Trae 领取失败（code={claim_code}）：{message}"));
    }

    // 同设备号回查：翻转才算数
    let (_, recheck) = match send_json(
        client,
        reqwest::Method::POST,
        &status_url,
        &headers,
        Some(json!({})),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if done_today(&recheck) {
        ok("Trae 签到成功".to_string())
    } else {
        failed("Trae 领取已提交但未确认，请稍后重试")
    }
}

// ── CodeArts 福利（每日登录赠送，非传统签到）──────────────────────────
//
// 来源：Agent2API `providers/codearts/welfare.rs`（MIT + 使用声明）。
// 四步：delivery → claim → confirm → 回读确认；签名复用聊天桥的
// SDK-HMAC-SHA256（`bridge/session/codearts.rs`，登录/对话已在用，已验证）。

async fn codearts(client: &Client, session: &AccountSession, secret: &str) -> ClaimResult {
    use crate::bridge::session::codearts::CodeArtsBridge;
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://snap-access.cn-north-4.myhuaweicloud.com".to_string());
    let base = base.trim_end_matches('/');
    let bridge = CodeArtsBridge::new(base, secret, client.clone());

    async fn send_signed(
        client: &Client,
        bridge: &CodeArtsBridge,
        method: reqwest::Method,
        url: &str,
        body: Value,
    ) -> Result<(u16, Value), String> {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Agent-Type".to_string(), "PromptCenter".to_string()),
            ("X-Language".to_string(), "en-us".to_string()),
        ];
        let is_get = method == reqwest::Method::GET;
        let body_text = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
        if let Some(auth) = bridge.sign(method.as_str(), url, &headers, body_text.as_bytes()) {
            headers.push(("Authorization".to_string(), auth));
        }
        send_json(
            client,
            method,
            url,
            &headers,
            if is_get { None } else { Some(body) },
            None,
        )
        .await
    }

    let delivery_url = format!("{base}/v1/ops/delivery?channel=IDE");
    let (_, first) = match send_signed(
        client,
        &bridge,
        reqwest::Method::GET,
        &delivery_url,
        json!({}),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    let items = first
        .pointer("/data/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() && first.pointer("/data/items").is_none() {
        return failed("CodeArts 福利列表读取失败（上游未返回 items）");
    }
    // 只认「每日登录送积分」这一类
    let target = items.iter().find(|item| {
        item.get("type").and_then(Value::as_str) == Some("USER_LOGIN")
            && item.get("benefitUnit").and_then(Value::as_str) == Some("CREDIT")
    });
    let Some(item) = target else {
        return already("今日福利已领取或无活动");
    };
    let confirmed = item.get("claimable").and_then(Value::as_bool) == Some(false)
        && matches!(
            item.get("status").and_then(Value::as_str),
            Some("CONFIRMED") | Some("CONSUMED")
        );
    if confirmed {
        return already("今日福利已领取");
    }
    let campaign = item.get("campaignId").cloned().unwrap_or(Value::Null);
    if campaign.is_null() {
        return failed("CodeArts 福利条目缺少活动标识");
    }
    let idempotent = format!(
        "claim_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (_, claim) = match send_signed(
        client,
        &bridge,
        reqwest::Method::POST,
        &format!("{base}/v1/ops/claim"),
        json!({
            "campaignId": campaign.clone(),
            "idempotentKey": idempotent,
            "channel": "IDE",
        }),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    let claim_code = claim.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if claim_code != 0 {
        let message = upstream_message(&claim);
        return failed(&format!(
            "CodeArts 领取失败（code={claim_code}）：{message}"
        ));
    }
    let (_, confirm) = match send_signed(
        client,
        &bridge,
        reqwest::Method::POST,
        &format!("{base}/v1/ops/confirm"),
        json!({ "campaignId": campaign }),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if confirm.get("code").and_then(Value::as_i64).unwrap_or(-1) != 0 {
        return pending("CodeArts 领取已提交，确认接口报错，请稍后看台账");
    }
    // 回读 delivery 二次确认（上游硬要求：不确认不算到账）
    let (_, second) = match send_signed(
        client,
        &bridge,
        reqwest::Method::GET,
        &delivery_url,
        json!({}),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    let after = second
        .pointer("/data/items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ok_now = after.iter().any(|item| {
        item.get("type").and_then(Value::as_str) == Some("USER_LOGIN")
            && item.get("benefitUnit").and_then(Value::as_str) == Some("CREDIT")
            && item.get("claimable").and_then(Value::as_bool) == Some(false)
            && matches!(
                item.get("status").and_then(Value::as_str),
                Some("CONFIRMED") | Some("CONSUMED")
            )
    });
    if ok_now {
        ok("CodeArts 每日福利已到账".to_string())
    } else {
        pending("请求已提交，但官方活动列表尚未确认到账，请稍后刷新台账")
    }
}

// ── OfficeAce（每日奖励领取）──────────────────────────────────────────
//
// 来源：Agent2API `providers/officeace/{checkin,subscription,signer}.rs`
// （MIT + 使用声明）。控制面走 **V11-HMAC-SHA256**（华为 APIG），签名密钥派生
// 用「hex 字符串」参与 HMAC（upstream signer.rs 文件头第 3 条的坑）。
// 凭据取 `refresh_secret` 里的临时 AK/SK/STS JSON（登录流程留存），
// 或 `secret` 的 `ak:sk:sts` 形态（手工粘贴）。上游一天一活动重复领不报错，
// 恒按成功处理，文案按领取前后积分增量生成。

pub(crate) fn hmac_sha256(key: &[u8], message: &str) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(message.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let out = hasher.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// V11-HMAC-SHA256 签名（faithful 移植 upstream `officeace/signer.rs`）。
#[allow(clippy::too_many_arguments)]
pub(crate) fn sign_v11(
    method: &str,
    url: &str,
    extra: &[(String, String)],
    body: &[u8],
    ak: &str,
    sk: &str,
    sts: &str,
    project: &str,
) -> Vec<(String, String)> {
    let parsed = url::Url::parse(url).unwrap_or_else(|_| url::Url::parse("https://x/").unwrap());
    let authority = match parsed.port() {
        Some(port) => format!("{}:{port}", parsed.host_str().unwrap_or("")),
        None => parsed.host_str().unwrap_or("").to_string(),
    };
    let date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut items: Vec<(String, String)> = extra.to_vec();
    items.push(("Host".to_string(), authority));
    items.push(("X-Sdk-Date".to_string(), date.clone()));
    if !sts.is_empty() {
        items.push(("X-Security-Token".to_string(), sts.to_string()));
    }
    if !project.is_empty() {
        items.push(("X-Project-ID".to_string(), project.to_string()));
    }
    let mut canonical: Vec<(String, String)> = items
        .iter()
        .map(|(name, value)| {
            (
                name.to_ascii_lowercase(),
                value.split_whitespace().collect::<Vec<_>>().join(" "),
            )
        })
        .collect();
    canonical.sort_by(|left, right| left.0.cmp(&right.0));
    let canonical_headers: String = canonical
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    let signed_headers = canonical
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");
    let payload_hash = sha256_hex(body);
    let path = parsed.path().to_string();
    let canonical_request = [
        method.to_ascii_uppercase(),
        path,
        String::new(),
        canonical_headers,
        signed_headers.clone(),
        payload_hash,
    ]
    .join("\n");
    let region = "cn-southwest-2";
    let scope = format!("{}/{}/apic", &date[..8], region);
    let string_to_sign = [
        "V11-HMAC-SHA256",
        date.as_str(),
        scope.as_str(),
        sha256_hex(canonical_request.as_bytes()).as_str(),
    ]
    .join("\n");
    // 派生链：step1 = HMAC(AK bytes, SK 字符串) → seed = scope+0x01 →
    // derived[..32] 的 **hex 字符串** 当最终密钥
    let step1 = hmac_sha256(ak.as_bytes(), sk);
    let mut seed = scope.clone().into_bytes();
    seed.push(1);
    let derived = &hmac_sha256(&step1, &String::from_utf8_lossy(&seed))[..32];
    let signing_key_hex: String = derived.iter().map(|b| format!("{b:02x}")).collect();
    let signature = {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(signing_key_hex.as_bytes()).expect("HMAC key");
        mac.update(string_to_sign.as_bytes());
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    items.push((
        "Authorization".to_string(),
        format!(
            "V11-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders={signed_headers}, Signature={signature}"
        ),
    ));
    items
}

pub(crate) struct OfficeAceCred {
    pub(crate) ak: String,
    pub(crate) sk: String,
    pub(crate) sts: String,
    pub(crate) project: String,
}

/// 取 OfficeAce 临时凭据：`refresh_secret` JSON 优先，`secret` 的 `ak:sk:sts` 兜底。
pub(crate) fn officeace_temp_creds(session: &AccountSession) -> Option<OfficeAceCred> {
    // refresh_secret 落盘/内存都是 `enc:` 编码，先解码再解析 JSON
    let raw = crate::channel::Channel::decode_api_key_value(&session.refresh_secret);
    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
        let read = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let (ak, sk) = (read("access_key_id"), read("secret_access_key"));
        if !ak.is_empty() && !sk.is_empty() {
            return Some(OfficeAceCred {
                ak,
                sk,
                sts: read("security_token"),
                project: read("project_id"),
            });
        }
    }
    let secret = session.credential.secret_plain();
    let parts: Vec<&str> = secret.split(':').map(str::trim).collect();
    if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        return Some(OfficeAceCred {
            ak: parts[0].to_string(),
            sk: parts[1].to_string(),
            sts: parts.get(2).copied().unwrap_or("").to_string(),
            project: parts.get(3).copied().unwrap_or("").to_string(),
        });
    }
    None
}

/// 读订阅面板里的累计积分（`bonus_skus[].points` 之和，数值是字符串）。
fn officeace_points(value: &Value) -> i64 {
    value
        .pointer("/data/bonus_skus")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.get("points")
                        .and_then(Value::as_str)
                        .and_then(|s| s.trim().parse::<i64>().ok())
                        .or_else(|| row.get("points").and_then(Value::as_i64))
                        .unwrap_or(0)
                })
                .sum()
        })
        .unwrap_or(0)
}

async fn officeace(client: &Client, session: &AccountSession) -> ClaimResult {
    let Some(cred) = officeace_temp_creds(session) else {
        return failed(
            "OfficeAce 签到需要临时 AK/SK（登录时留存于 refresh_secret），当前账号没有，请重新登录",
        );
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| String::new());
    if base.is_empty() {
        return failed("OfficeAce 账号缺少上游地址");
    }
    // 网关根 = base 去掉尾巴 /v2
    let gateway = base.trim_end_matches('/').trim_end_matches("/v2");
    let sub_url = format!("{gateway}/v1/subscription");
    let claim_url = format!("{gateway}/v1/subscription/bonus/claim");
    let extra = vec![("x-subscription-type".to_string(), "v2".to_string())];

    async fn read_sub(
        client: &Client,
        url: &str,
        extra: &[(String, String)],
        cred: &OfficeAceCred,
    ) -> Result<(u16, Value), String> {
        let headers = sign_v11(
            "GET",
            url,
            extra,
            b"",
            &cred.ak,
            &cred.sk,
            &cred.sts,
            &cred.project,
        );
        send_json(client, reqwest::Method::GET, url, &headers, None, None).await
    }

    let before = match read_sub(client, &sub_url, &extra, &cred).await {
        Ok((status, value)) if (200..300).contains(&status) => officeace_points(&value),
        Ok((status, _)) if status == 401 || status == 403 => {
            return failed("OfficeAce 控制面临时 AK/SK 已失效，请重新登录该账号");
        }
        _ => -1,
    };

    let body = json!({});
    let headers = sign_v11(
        "POST",
        &claim_url,
        &extra,
        b"{}",
        &cred.ak,
        &cred.sk,
        &cred.sts,
        &cred.project,
    );
    let (status, value) = match send_json(
        client,
        reqwest::Method::POST,
        &claim_url,
        &headers,
        Some(body),
        None,
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return failed(&e),
    };
    if status == 401 || status == 403 {
        return failed("OfficeAce 控制面临时 AK/SK 已失效，请重新登录该账号");
    }
    if status >= 400 {
        let message = upstream_message(&value);
        return failed(&format!(
            "OfficeAce 领取失败（HTTP {status}）{}",
            if message.is_empty() {
                String::new()
            } else {
                format!("：{message}")
            }
        ));
    }
    // 上游重复领不报错：按前后增量出文案，恒算成功
    let after = match read_sub(client, &sub_url, &extra, &cred).await {
        Ok((status, value)) if (200..300).contains(&status) => officeace_points(&value),
        _ => -1,
    };
    if before < 0 {
        return ok("已签到".to_string());
    }
    let gained = (after - before).max(0);
    if gained > 0 {
        ok(format!("已领取 {gained} 积分"))
    } else {
        ok("今天没有可领的奖励（或已领过）".to_string())
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
                    "loomy"
                        | "kuku"
                        | "qoder"
                        | "qoder-intl"
                        | "xiaohuanxiong"
                        | "autoclaw"
                        | "autoclaw-intl"
                        | "workbuddy"
                        | "workbuddy-intl"
                        | "trae"
                        | "codearts"
                        | "officeace"
                ),
                "provider {provider} 在白名单里但没有实现"
            );
        }
    }

    #[test]
    fn unknown_provider_not_supported() {
        assert!(!supports_checkin("openai"));
        assert!(!supports_checkin("zcode"));
        assert!(!supports_checkin("catpaw"));
        assert!(supports_checkin("loomy"));
        assert!(supports_checkin("trae"));
    }
}
