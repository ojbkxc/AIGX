//! ZCode 限时套餐领取（zcode / zcode-intl）。
//!
//! 来源：Agent2API `providers/zcode/{claim,activation,region}.rs`。要点：
//!
//! - **两个地区共用同一个平台域** `zcode.z.ai`（地区只影响推理平面）；
//! - `X-Device-Mid` 是**硬要求**：必须是稳定 UUID，缺了或每次变都会被
//!   上游判 3001 参数错误 —— 所以按账号落盘一枚，跨请求复用；
//! - 领取要过**阿里云验证码**，上游只收「前端 webview 用官方 SDK 求解出来的
//!   一串参数」；后端只做转发（`X-Aliyun-Captcha-Verify-Param`）。因此
//!   `mode=claim` 需要前端把求解结果一起带上，纯后台无法自动领；
//! - 领取资格前置是两条**无鉴权**的激活事件（`app_launch` / `app_daily_active`），
//!   探测为空时补报一次再重探（失败不阻断）。
//!
//! 幂等：已领过的套餐（1003）也落盘，否则界面会永远显示「未领取」。

use std::sync::Arc;

use serde_json::{json, Value};

use super::{load_memory, memory_key, save_memory, send_json, BenefitResult};
use crate::account_session::model::AccountSession;
use crate::storage::FileStore;

/// 平台域（两地区共用；推理域才分国内/国际）。
const PLAN_HOST: &str = "https://zcode.z.ai";
/// 客户端标识（上游按 app_version/platform 做兼容分支）。
const APP_VERSION: &str = "1.0.0";
const PLATFORM: &str = "web";

/// 执行：`params.mode` 取 `preview`（默认，只读）/ `config`（风控配置）/ `claim`。
pub async fn run(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    session: &AccountSession,
    params: &Value,
) -> BenefitResult {
    let mode = params
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("preview");
    let secret = super::secret_of(session);
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if !secret.trim().is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {secret}")));
    }
    // 设备指纹：按账号稳定落盘，缺了上游一律拒
    let device_mid = device_mid(store, &session.id);
    headers.push(("X-Device-Mid".to_string(), device_mid));

    match mode {
        "config" => captcha_config(client, &headers).await,
        "claim" => claim(store, client, &mut headers, session, params).await,
        _ => preview(store, client, &headers, session).await,
    }
}

/// 套餐预览（只读）。
async fn preview(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    headers: &[(String, String)],
    session: &AccountSession,
) -> BenefitResult {
    let url = format!(
        "{PLAN_HOST}/api/v1/zcode-plan/billing/preview?app_version={APP_VERSION}&platform={PLATFORM}"
    );
    let (status, body) = match send_json(client, reqwest::Method::GET, &url, headers, None).await {
        Ok(pair) => pair,
        Err(e) => return BenefitResult::failed(e),
    };
    if status == 401 || status == 403 {
        return BenefitResult::failed("ZCode 登录态已失效，请重新登录");
    }
    if !(200..300).contains(&status) {
        return BenefitResult::failed(format!("ZCode 套餐预览失败（HTTP {status}）"));
    }
    let plans = body
        .pointer("/data/plans")
        .or_else(|| body.get("plans"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // 已领取记录（1003 也落盘，否则界面永远显示未领）
    let memory =
        load_memory(store, &memory_key(&session.id, "zcode_plan")).unwrap_or_else(|| json!({}));
    let claimed = memory
        .get("claimed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let data = json!({
        "plans": plans,
        "claimed": claimed,
        "deployed": body.pointer("/data/deployed").cloned().unwrap_or(Value::Bool(true)),
        "activated": body.pointer("/data/activated").cloned().unwrap_or(Value::Null),
    });
    if plans.is_empty() {
        return BenefitResult::already("当前没有可领的限时套餐").with_data(data);
    }
    BenefitResult::done(format!("发现 {} 份限时套餐", plans.len())).with_data(data)
}

/// 风控（验证码）配置：前端据此决定弹不弹滑块。
async fn captcha_config(client: &reqwest::Client, headers: &[(String, String)]) -> BenefitResult {
    let url =
        format!("{PLAN_HOST}/api/v1/client/configs?app_version={APP_VERSION}&platform={PLATFORM}");
    let (status, body) = match send_json(client, reqwest::Method::GET, &url, headers, None).await {
        Ok(pair) => pair,
        Err(e) => return BenefitResult::failed(e),
    };
    if !(200..300).contains(&status) {
        return BenefitResult::failed(format!("验证码配置读取失败（HTTP {status}）"));
    }
    let captcha = body
        .pointer("/data/configs/captcha")
        .cloned()
        .unwrap_or(Value::Null);
    BenefitResult::done("验证码配置已读取").with_data(json!({ "captcha": captcha }))
}

/// 领取一份套餐（需要前端求解出的验证码参数）。
async fn claim(
    store: &Arc<FileStore>,
    client: &reqwest::Client,
    headers: &mut Vec<(String, String)>,
    session: &AccountSession,
    params: &Value,
) -> BenefitResult {
    let plan_id = params.get("plan_id").cloned().unwrap_or(Value::Null);
    if plan_id.is_null() {
        return BenefitResult::failed("领取需要指定 plan_id（先调用 preview）");
    }
    if let Some(verify) = params
        .get("captcha_verify_param")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        headers.push((
            "X-Aliyun-Captcha-Verify-Param".to_string(),
            verify.to_string(),
        ));
    }

    // 资格前置：补报激活事件（无鉴权，失败不阻断）
    let uid = super::jwt_claim_str(&super::secret_of(session), &["sub", "uid", "userId"])
        .unwrap_or_default();
    for event in ["app_launch", "app_daily_active"] {
        let _ = send_json(
            client,
            reqwest::Method::POST,
            &format!("{PLAN_HOST}/api/v1/event/report"),
            &[("Content-Type".to_string(), "application/json".to_string())],
            Some(&json!({ "event": event, "user_id": uid, "device_mid": device_mid(store, &session.id) })),
        )
        .await;
    }

    let url = format!("{PLAN_HOST}/api/v1/zcode-plan/billing/claim");
    let (status, body) = match send_json(
        client,
        reqwest::Method::POST,
        &url,
        headers,
        Some(&json!({ "plan_id": plan_id })),
    )
    .await
    {
        Ok(pair) => pair,
        Err(e) => return BenefitResult::failed(e),
    };
    if status == 401 || status == 403 {
        return BenefitResult::failed("ZCode 登录态已失效，请重新登录");
    }
    let code = body
        .get("code")
        .and_then(Value::as_i64)
        .or_else(|| body.get("code").and_then(Value::as_str)?.parse().ok())
        .unwrap_or(0);
    // 已领 / 不可领 / 不符资格 / 验证码失败 —— 各自给明确文案
    match code {
        0 => {
            remember_claim(store, session, &plan_id);
            BenefitResult::done("套餐已领取").with_data(body)
        }
        1003 => {
            remember_claim(store, session, &plan_id);
            BenefitResult::already("该套餐此前已领取").with_data(body)
        }
        1002 => BenefitResult::already("该套餐当前不可领").with_data(body),
        1004 => BenefitResult::failed("账号不符合该套餐的领取资格").with_data(body),
        1005 => BenefitResult::pending("当日领取额度已用尽，明天再试").with_data(body),
        3007 => BenefitResult::failed("验证码校验未通过，请重新求解").with_data(body),
        3001 => BenefitResult::failed("请求参数被拒（设备指纹或 plan_id 不对）").with_data(body),
        other => BenefitResult::failed(format!("领取失败（code={other}）")).with_data(body),
    }
}

/// 记录一次已领（含 1003 重复领取），避免界面永远显示未领。
fn remember_claim(store: &FileStore, session: &AccountSession, plan_id: &Value) {
    let key = memory_key(&session.id, "zcode_plan");
    let mut memory = load_memory(store, &key).unwrap_or_else(|| json!({}));
    let mut claimed = memory
        .get("claimed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !claimed.iter().any(|item| item == plan_id) {
        claimed.push(plan_id.clone());
    }
    memory["claimed"] = json!(claimed);
    memory["claimed_at"] = json!(chrono::Utc::now().timestamp());
    let _ = save_memory(store, &key, &memory);
}

/// 取（或生成并落盘）该账号的稳定设备指纹。
fn device_mid(store: &FileStore, account_id: &str) -> String {
    let key = memory_key(account_id, "zcode_device_mid");
    if let Some(existing) = load_memory(store, &key).and_then(|v| {
        v.get("mid")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    }) {
        return existing;
    }
    let mid = uuid::Uuid::new_v4().to_string();
    let _ = save_memory(store, &key, &json!({ "mid": mid }));
    mid
}
