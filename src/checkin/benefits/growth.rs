//! WorkBuddy 国内版「成长任务」。
//!
//! 来源：Agent2API `providers/workbuddy/growth/`。它是一棵任务树：每个任务
//! 先 `accept` 再 `claim`，任务之间还要间隔 ≥1200ms（秒发会被上游风控）。
//! 一次全量跑完很容易超过管理面 60s 转发超时，所以这里**按条执行**：
//! `params.code` 给了就跑那一条，没给只做只读盘点。
//!
//! 未做的部分（如实标注）：开学季（school，另一套域与小程序 UA）与
//! 猫猫旅行（travel）需要各自的端点与指纹头，且只能靠真实账号验证，
//! 这里不冒进实现 —— 盘点里会把它们的状态照实返回，便于后续补齐。

use serde_json::{json, Value};

use super::{send_json, BenefitResult};
use crate::account_session::model::AccountSession;

/// 写动作之间的最小间隔（毫秒）—— 防秒发风控。
const WRITE_GAP_MS: u64 = 1200;

/// 执行成长任务：`params.code` 有值则跑那一条，否则只盘点。
pub async fn run(
    client: &reqwest::Client,
    session: &AccountSession,
    params: &Value,
) -> BenefitResult {
    // 只有国内版有成长任务体系（国际版报 400）
    if session.provider != "workbuddy" {
        return BenefitResult::skipped("成长任务只在国内版 WorkBuddy 提供");
    }
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://copilot.tencent.com".to_string());
    let base = base.trim_end_matches('/');
    let secret = super::secret_of(session);
    if secret.trim().is_empty() {
        return BenefitResult::failed("WorkBuddy 账号没有凭据，请重新登录");
    }
    let uid = super::jwt_claim_str(&secret, &["sub", "uid", "userId"]).unwrap_or_default();
    let mut headers = vec![
        ("Authorization".to_string(), format!("Bearer {secret}")),
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    if !uid.is_empty() {
        headers.push(("X-User-Id".to_string(), uid));
    }

    let code = params
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(code) = code {
        return run_one(client, base, &headers, code).await;
    }
    status(client, base, &headers).await
}

/// 只读盘点：任务列表 + 能量 + 连续天数。
async fn status(
    client: &reqwest::Client,
    base: &str,
    headers: &[(String, String)],
) -> BenefitResult {
    let tasks = send_json(
        client,
        reqwest::Method::GET,
        &format!("{base}/v2/activity/growth/tasks"),
        headers,
        None,
    )
    .await;
    let (status, body) = match tasks {
        Ok(pair) => pair,
        Err(e) => return BenefitResult::failed(e),
    };
    if status == 401 || status == 403 {
        return BenefitResult::failed("WorkBuddy 登录态已失效，请重新登录");
    }
    if !(200..300).contains(&status) {
        return BenefitResult::failed(format!("成长任务读取失败（HTTP {status}）"));
    }
    let rows = body
        .pointer("/data/tasks")
        .or_else(|| body.get("tasks"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // 能量与连续天数属装饰性读数：拉不到不该让整个盘点失败
    let energy = read_number(
        client,
        &format!("{base}/v2/activity/growth/energy"),
        headers,
    )
    .await;
    let streak = read_number(client, &format!("{base}/activity/growth/streak"), headers).await;

    let pending = rows
        .iter()
        .filter(|row| {
            row.get("done").and_then(Value::as_bool) != Some(true)
                && row.get("locked").and_then(Value::as_bool) != Some(true)
        })
        .count();
    let data = json!({
        "tasks": rows,
        "energy": energy,
        "streak": streak,
        "pending": pending,
        "write_gap_ms": WRITE_GAP_MS,
        "unsupported": ["school", "travel"],
    });
    if pending == 0 {
        BenefitResult::already("成长任务已全部完成").with_data(data)
    } else {
        BenefitResult::done(format!("成长任务还有 {pending} 条可做")).with_data(data)
    }
}

/// 跑一条任务：accept → 等一下 → claim。
async fn run_one(
    client: &reqwest::Client,
    base: &str,
    headers: &[(String, String)],
    code: &str,
) -> BenefitResult {
    let accept = send_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/v2/activity/growth/tasks/accept"),
        headers,
        Some(&json!({ "code": code })),
    )
    .await;
    match &accept {
        Ok((status, _)) if *status == 401 || *status == 403 => {
            return BenefitResult::failed("WorkBuddy 登录态已失效，请重新登录");
        }
        Ok((status, _)) if !(200..300).contains(status) => {
            return BenefitResult::failed(format!("任务 {code} 受理失败（HTTP {status}）"));
        }
        Err(e) => return BenefitResult::failed(e.clone()),
        _ => {}
    }
    // 写动作之间强制间隔，避免秒发被风控
    tokio::time::sleep(std::time::Duration::from_millis(WRITE_GAP_MS)).await;

    let claim = send_json(
        client,
        reqwest::Method::POST,
        &format!("{base}/activity/growth/tasks/{code}/claim"),
        headers,
        Some(&json!({})),
    )
    .await;
    match claim {
        Ok((status, body)) if (200..300).contains(&status) => {
            BenefitResult::done(format!("任务 {code} 已领取")).with_data(body)
        }
        Ok((status, body)) => {
            BenefitResult::pending(format!("任务 {code} 受理成功但领取未确认（HTTP {status}）"))
                .with_data(body)
        }
        Err(e) => BenefitResult::pending(format!("任务 {code} 受理成功但领取失败：{e}")),
    }
}

/// 读一个数字型读数（失败给 `null`，不当作错误）。
async fn read_number(client: &reqwest::Client, url: &str, headers: &[(String, String)]) -> Value {
    match send_json(client, reqwest::Method::GET, url, headers, None).await {
        Ok((status, body)) if (200..300).contains(&status) => {
            body.pointer("/data").cloned().unwrap_or(Value::Null)
        }
        _ => Value::Null,
    }
}
