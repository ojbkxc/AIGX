//! CodeArts 新人礼（一次性）—— 与每日福利同一条上游链，只是挑的活动不同。
//!
//! 来源：Agent2API `providers/codearts/onboarding.rs`。日常签到挑
//! `USER_LOGIN`（每日登录积分），这里挑 `NEW_USER_REGISTER`（新人注册礼）。
//! 链路一致：`GET /v1/ops/delivery` → `POST /v1/ops/claim` →
//! `POST /v1/ops/confirm` → **回读 delivery 二次确认**才算成功。
//!
//! 另外两档（学生认证 / 邀请好友）刻意不做：上游要额外材料，网关代跑没有意义。

use serde_json::{json, Value};

use super::{send_json, BenefitResult};
use crate::account_session::model::AccountSession;

/// 新人礼活动类型。
const NEWBIE_KIND: &str = "NEW_USER_REGISTER";
/// 每日福利活动类型（只在日志/说明里对照用）。
const DAILY_KIND: &str = "USER_LOGIN";

/// 领取新人礼。
pub async fn claim_newbie(client: &reqwest::Client, session: &AccountSession) -> BenefitResult {
    use crate::bridge::session::codearts::CodeArtsBridge;

    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://snap-access.cn-north-4.myhuaweicloud.com".to_string());
    let base = base.trim_end_matches('/');
    let secret = super::secret_of(session);
    if secret.trim().is_empty() {
        return BenefitResult::failed("CodeArts 账号没有凭据，请重新登录");
    }
    let bridge = CodeArtsBridge::new(base, &secret, client.clone());

    let delivery = match read_delivery(client, &bridge, base).await {
        Ok(value) => value,
        Err(message) => return BenefitResult::failed(message),
    };
    let Some(item) = pick(&delivery, NEWBIE_KIND) else {
        return BenefitResult::already("没有可领的新人礼（未注册或已过期）");
    };
    // 「已领」判定与每日链同口径：不可领 + 已确认/已消费
    let claimed = item.get("claimable").and_then(Value::as_bool) == Some(false)
        && matches!(
            item.get("status").and_then(Value::as_str),
            Some("CONFIRMED") | Some("CONSUMED")
        );
    if claimed {
        return BenefitResult::already("CodeArts 新人礼已领取过");
    }
    let campaign = item.get("campaignId").cloned().unwrap_or(Value::Null);
    if campaign.is_null() {
        return BenefitResult::failed("新人礼条目缺少活动标识（campaignId）");
    }
    let idempotent = format!(
        "newbie_{}_{}",
        chrono::Utc::now().timestamp_millis(),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let (_, claim) = match signed(
        client,
        &bridge,
        reqwest::Method::POST,
        &format!("{base}/v1/ops/claim"),
        json!({
            "campaignId": campaign,
            "idempotentKey": idempotent,
            "channel": "IDE",
        }),
    )
    .await
    {
        Ok(pair) => pair,
        Err(message) => return BenefitResult::failed(message),
    };
    if let Some(code) = claim.get("code").and_then(Value::as_str) {
        if code != "0" {
            let message = claim
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游拒绝");
            return BenefitResult::failed(format!("CodeArts 新人礼领取失败：{message}"));
        }
    }
    // confirm 失败不算领取失败：claim 已提交，回读确认会给出最终结论
    let _ = signed(
        client,
        &bridge,
        reqwest::Method::POST,
        &format!("{base}/v1/ops/confirm"),
        json!({ "campaignId": item.get("campaignId").cloned().unwrap_or(Value::Null) }),
    )
    .await;

    // 回读二次确认：只有上游确实记上了才算领到
    match read_delivery(client, &bridge, base).await {
        Ok(after) => {
            let still = pick(&after, NEWBIE_KIND)
                .map(|row| row.get("claimable").and_then(Value::as_bool) != Some(false));
            match still {
                Some(false) => BenefitResult::done("CodeArts 新人礼已领取到账"),
                _ => BenefitResult::pending("请求已提交，上游尚未确认到账（稍后重试）"),
            }
        }
        Err(_) => BenefitResult::pending("请求已提交，回读确认失败（稍后重试）"),
    }
}

/// 读福利列表。
async fn read_delivery(
    client: &reqwest::Client,
    bridge: &crate::bridge::session::codearts::CodeArtsBridge,
    base: &str,
) -> Result<Value, String> {
    signed(
        client,
        bridge,
        reqwest::Method::GET,
        &format!("{base}/v1/ops/delivery?channel=IDE"),
        json!({}),
    )
    .await
    .map(|(_, value)| value)
}

/// 从列表里挑指定类型的一条。
fn pick(delivery: &Value, kind: &str) -> Option<Value> {
    delivery
        .pointer("/data/items")
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get("type").and_then(Value::as_str) == Some(kind))
        .cloned()
}

/// 发一次带华为云 SDK-HMAC-SHA256 签名的请求。
async fn signed(
    client: &reqwest::Client,
    bridge: &crate::bridge::session::codearts::CodeArtsBridge,
    method: reqwest::Method,
    url: &str,
    body: Value,
) -> Result<(u16, Value), String> {
    let mut headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Agent-Type".to_string(), "PromptCenter".to_string()),
        ("X-Language".to_string(), "en-us".to_string()),
    ];
    let body_text = serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string());
    match bridge.sign(method.as_str(), url, &headers, body_text.as_bytes()) {
        Some(auth) => headers.push(("Authorization".to_string(), auth)),
        None => return Err("CodeArts 凭据缺少 AccessKey/SecretKey，无法签名".to_string()),
    }
    let is_get = method == reqwest::Method::GET;
    send_json(
        client,
        method,
        url,
        &headers,
        if is_get { None } else { Some(&body) },
    )
    .await
}

/// 导出每日链用的活动类型常量（供契约测试对照，避免两处写死不同的字符串）。
pub const DAILY_ACTIVITY: &str = DAILY_KIND;
