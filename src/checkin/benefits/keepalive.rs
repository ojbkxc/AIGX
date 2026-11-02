//! WorkBuddy 国际版「日活保活链」。
//!
//! 来源：Agent2API `core/billing/{keepalive,activity}.rs` +
//! `providers/workbuddy/keepalive.rs`。它做三件事，缺一件都会让国际版的
//! 「每日活跃奖励」断掉：
//!
//! 1. **探测**活动是否开启、今天是否已领（`checkin-activity-status`）；
//! 2. 未领则**领取**（`daily-checkin`）；
//! 3. 用**免费模型**各打一发最小对话 —— 桌面端 `chat/completions`
//!    与网页端 `POST /console/as/conversations/`。点亮「今天活跃」的是
//!    **网页通道**那一下，桌面那发是维持账号热度，两者都要发。
//!
//! 免费模型链可配置（KV `checkin_keepalive`），因为「哪些模型不扣积分」是
//! 运营态事实，上游随时会变；留空 = 用内置默认链。

use std::sync::Arc;

use serde_json::{json, Value};

use super::{load_memory, memory_key, save_memory, send_json, BenefitResult};
use crate::account_session::model::AccountSession;
use crate::storage::FileStore;

/// 免费模型链的配置键。
pub const KEEPALIVE_KEY: &str = "checkin_keepalive";

/// 内置免费模型链（实测不消耗积分，按可用性排序）。
const DEFAULT_FREE_MODELS: &[&str] =
    &["deepseek-v4.1-flash", "hy3", "hy4-preview", "hy4-preview-f"];

/// 读取免费模型链（空 = 回落内置默认）。
pub fn free_models(store: &FileStore) -> Vec<String> {
    let configured: Vec<String> = store
        .get::<Value>(KEEPALIVE_KEY)
        .ok()
        .flatten()
        .and_then(|value| {
            value.get("models").and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    if configured.is_empty() {
        DEFAULT_FREE_MODELS.iter().map(|s| s.to_string()).collect()
    } else {
        configured
    }
}

/// 执行一条保活链。
///
/// `mode` 三档手动粒度（对齐 Agent2API `WorkbuddyActivity::parse`）：
/// - `"claim"`：只探测 + 领取日活奖励（不打最小对话）；
/// - `"keepalive"`：只打免费模型最小对话（不探测不领取）；
/// - `"full"`：探测 + 领取 + 最小对话（定时与批量走这一档）。
pub async fn run(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
    mode: &str,
) -> BenefitResult {
    let (with_claim, with_keepalive) = match mode {
        "claim" => (true, false),
        "keepalive" => (false, true),
        _ => (true, true),
    };
    let base = crate::account_session::adapter::effective_base_url(session)
        .unwrap_or_else(|_| "https://www.workbuddy.ai".to_string());
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
        headers.push(("X-User-Id".to_string(), uid.clone()));
    }

    // ① 探测活动是否开启、今天是否已领；未领则领取
    let mut steps: Vec<Value> = Vec::new();
    let mut claimed_now = false;
    if with_claim {
        let probe = send_json(
            client,
            reqwest::Method::POST,
            &format!("{base}/billing/meter/checkin-activity-status"),
            &headers,
            Some(&json!({})),
        )
        .await;
        match &probe {
            Ok((status, body)) if (200..300).contains(status) => {
                let checked = body
                    .pointer("/data/today_checked_in")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let active = body
                    .pointer("/data/active")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                steps.push(json!({
                    "step": "probe",
                    "checked_in_today": checked,
                    "active": active,
                }));
                // ② 未领取则领取（今日已领就跳过，别重复打上游）
                if !checked && active {
                    match send_json(
                        client,
                        reqwest::Method::POST,
                        &format!("{base}/v2/billing/meter/daily-checkin"),
                        &headers,
                        Some(&json!({})),
                    )
                    .await
                    {
                        Ok((code, body)) => {
                            claimed_now = (200..300).contains(&code);
                            steps.push(json!({
                                "step": "claim",
                                "http": code,
                                "ok": claimed_now,
                                "body": body.get("msg").or_else(|| body.get("message")).cloned()
                                    .unwrap_or(Value::Null),
                            }));
                        }
                        Err(e) => steps.push(json!({ "step": "claim", "ok": false, "error": e })),
                    }
                }
            }
            Ok((status, _)) => steps.push(json!({ "step": "probe", "http": status })),
            Err(e) => steps.push(json!({ "step": "probe", "error": e })),
        }
    }

    // ③ 免费模型最小对话保活（桌面 + 网页各一发）
    let models = free_models(store);
    let model = models.first().cloned().unwrap_or_default();
    let mut keepalive_ok = 0usize;
    if with_keepalive && !model.is_empty() {
        // 桌面通道：标准 chat/completions，max_tokens 极小
        let desktop = send_json(
            client,
            reqwest::Method::POST,
            &format!("{base}/v1/chat/completions"),
            &headers,
            Some(&json!({
                "model": model,
                "messages": [{ "role": "user", "content": "hi" }],
                "max_tokens": 16,
                "stream": false,
            })),
        )
        .await;
        if matches!(&desktop, Ok((status, _)) if (200..300).contains(status)) {
            keepalive_ok += 1;
        }
        steps.push(json!({
            "step": "keepalive_desktop",
            "model": model,
            "ok": matches!(&desktop, Ok((status, _)) if (200..300).contains(status)),
        }));

        // 网页通道：点亮「今天活跃」的是这一发（剥掉桌面指纹，只留 Bearer + X-User-Id）
        let web_headers: Vec<(String, String)> = headers
            .iter()
            .filter(|(name, _)| {
                let lower = name.to_ascii_lowercase();
                lower == "authorization" || lower == "x-user-id"
            })
            .cloned()
            .collect();
        let web = send_json(
            client,
            reqwest::Method::POST,
            &format!("{base}/console/as/conversations/"),
            &web_headers,
            Some(&json!({
                "prompt": "Hi",
                "conversationOrigin": "web",
                "plugins": [],
            })),
        )
        .await;
        if matches!(&web, Ok((status, _)) if (200..300).contains(status)) {
            keepalive_ok += 1;
        }
        steps.push(json!({
            "step": "keepalive_web",
            "ok": matches!(&web, Ok((status, _)) if (200..300).contains(status)),
        }));
    }

    let today = chrono::Utc::now().format("%Y%m%d").to_string();
    let key = memory_key(&session.id, "keepalive");
    let memory = json!({
        "date": today,
        "model": model,
        "keepalive_ok": keepalive_ok,
        "steps": steps,
    });
    let _ = save_memory(store, &key, &memory);

    let summary = if claimed_now {
        format!("日活奖励已领取，保活 {keepalive_ok}/2 成功")
    } else {
        format!("日活奖励今日已领或未开启，保活 {keepalive_ok}/2 成功")
    };
    if keepalive_ok == 0 && with_keepalive {
        BenefitResult::pending(format!("{summary}（保活请求都没成功，稍后重试）")).with_data(memory)
    } else if claimed_now {
        BenefitResult::done(summary).with_data(memory)
    } else {
        BenefitResult::already(summary).with_data(memory)
    }
}

/// 当前的保活配置视图（供设置页回显）。
pub fn config_view(store: &FileStore) -> Value {
    json!({
        "models": free_models(store),
        "default_models": DEFAULT_FREE_MODELS,
    })
}

/// 保存保活配置（空清单 = 恢复内置默认）。
pub fn save_config(store: &FileStore, models: Vec<String>) -> anyhow::Result<Value> {
    let cleaned: Vec<String> = models
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(20)
        .collect();
    store.put(KEEPALIVE_KEY, &json!({ "models": cleaned }))?;
    Ok(config_view(store))
}

/// 上次保活执行留下的记忆（供界面展示）。
pub fn memory_view(store: &FileStore, account_id: &str) -> Option<Value> {
    load_memory(store, &memory_key(account_id, "keepalive"))
}
