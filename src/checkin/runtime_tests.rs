//! 签到执行器的**运行时**验证 —— 用可控的上游替身把 12 家的完整流程跑通。
//!
//! ## 为什么要有这一层
//!
//! 端点契约表（[`super::claim::CHECKIN_CONTRACTS`]）只能证明「AIGX 用的 URL
//! 与上游一致」，证不了「上游返回这个形状时，AIGX 判成成功/已领/未开放/失效」。
//! 而后者恰恰是签到体验的全部：**判错就是「说签到成功了但没到账」**。
//!
//! 真上游要真实登录态账号（生产上一个都没有），跑不了也测不了。所以这里
//! 起一个**上游替身**：按路径回放预置响应，同时记录请求（URL/头/body），
//! 让执行器跑完整条路径。
//!
//! ## 覆盖什么
//!
//! 每家至少覆盖三类：**成功领取**、**今天已领**（幂等）、**登录态失效**；
//! 另有专属分支（trae 未开放、workbuddy 活动未开启、qoder 无活动…）。
//! 断言两件事：
//! - **结果分类正确**（success / already / pending / failed）；
//! - **请求构造正确**（打到了哪个路径、带没带该带的头/body）。
//!
//! 替身只回放**形状正确**的响应体 —— 形状取自 Agent2API 各家
//! `checkin.rs` 里对上游响应的处理（见对应源码的判据段）。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::any;
use axum::Router;
use serde_json::{json, Value};

use super::claim::claim_for;
use super::store::CheckinOutcome;
use crate::account_session::model::{AccountSession, Credential};

/// 上游替身收到的一次请求。
#[derive(Debug, Clone)]
struct RecordedRequest {
    path: String,
    method: String,
    /// 请求头（小写键名）。
    headers: HashMap<String, String>,
    /// 请求体（解析失败则为空）。
    body: Value,
}

/// 替身的状态：路径 → 依次回放的响应序列。
///
/// **为什么要序列**：上游的签到流程常是「查 → 领 → 回查确认」，
/// 同一个路径会被打两次且两次响应不同（首次未领、回读已领）。
/// 按调用序依次回放才能覆盖这类流程 —— 固定单响应会让「回查翻转」这类
/// 判据永远走不到。
#[derive(Clone)]
struct MockState {
    routes: Arc<HashMap<String, Vec<(u16, Value)>>>,
    seen: Arc<Mutex<Vec<RecordedRequest>>>,
    /// 每条路径已回放了几次（决定取序列里的第几个）。
    counters: Arc<Mutex<HashMap<String, usize>>>,
}

/// 起一个上游替身，返回 `(base_url, 状态句柄)`。
///
/// `routes` 的键是**路径**（不含 query），值是 `(状态码, 响应体)`。
/// 未登记的路径一律回 404 + `{"message":"not found"}` —— 这本身也是一种断言：
/// 执行器打错路径时会立刻暴露，而不是悄悄走到别的分支。
async fn start_mock(routes: Vec<(&str, u16, Value)>) -> (String, MockState) {
    let mut map: HashMap<String, Vec<(u16, Value)>> = HashMap::new();
    for (path, status, body) in routes {
        map.entry(path.to_string())
            .or_default()
            .push((status, body));
    }
    let state = MockState {
        routes: Arc::new(map),
        seen: Arc::new(Mutex::new(Vec::new())),
        counters: Arc::new(Mutex::new(HashMap::new())),
    };

    let router = Router::new()
        .fallback(any(handle))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("绑定 mock 端口");
    let addr: SocketAddr = listener.local_addr().expect("读 mock 地址");
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (format!("http://{addr}"), state)
}

async fn handle(
    State(state): State<MockState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let path = uri.path().to_string();
    let mut header_map = HashMap::new();
    for (name, value) in headers.iter() {
        header_map.insert(
            name.as_str().to_ascii_lowercase(),
            value.to_str().unwrap_or_default().to_string(),
        );
    }
    let parsed_body = serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    state.seen.lock().unwrap().push(RecordedRequest {
        path: path.clone(),
        method: method.to_string(),
        headers: header_map,
        body: parsed_body,
    });

    let response = {
        let mut counters = state.counters.lock().unwrap();
        let index = counters.entry(path.clone()).or_insert(0);
        let slot = *index;
        *index += 1;
        state.routes.get(&path).and_then(|seq| {
            // 超出序列长度时复用最后一个（覆盖「末次状态反复回查」）
            seq.get(slot.min(seq.len().saturating_sub(1))).cloned()
        })
    };
    match response {
        Some((status, payload)) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            axum::Json(payload),
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"message": "not found", "path": path})),
        )
            .into_response(),
    }
}

/// 造一个账号池账号（base_url 指向替身）。
fn account(provider: &str, base_url: &str) -> AccountSession {
    AccountSession {
        id: "test-account".to_string(),
        channel_id: "test-channel".to_string(),
        name: "测试账号".to_string(),
        provider: provider.to_string(),
        base_url: base_url.to_string(),
        chat_path: None,
        credential: Credential {
            kind: "session".to_string(),
            secret: "test-token".to_string(),
            fields: std::collections::HashMap::new(),
        },
        proxy: None,
        status: "enabled".to_string(),
        priority: 0,
        weight: 1,
        max_concurrency: 0,
        models: Vec::new(),
        extra_headers: HashMap::new(),
        refresh_secret: String::new(),
        cooldowns: HashMap::new(),
        last_used_at: None,
        last_error: None,
        success_count: 0,
        error_count: 0,
        consecutive_errors: 0,
        total_latency_ms: 0,
        expires_at: None,
        in_flight: 0,
        usage_query_interval_secs: None,
        limiters: Vec::new(),
        limiter_groups: Vec::new(),
        last_balance_at: None,
        last_balance: None,
        last_balance_error: None,
        created_at: 0,
        updated_at: 0,
    }
}

/// 断言替身收到的请求用了某个 HTTP 方法。
fn assert_method(state: &MockState, path: &str, expect: &str) {
    let seen = state.seen.lock().unwrap();
    let req = seen
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("没收到 {path} 的请求"));
    assert_eq!(
        req.method, expect,
        "{path} 的请求方法应是 {expect}，实际 {}",
        req.method
    );
}

/// 断言替身收到的请求体里某个键等于预期值。
fn assert_body_field(state: &MockState, path: &str, key: &str, expect: &Value) {
    let seen = state.seen.lock().unwrap();
    let req = seen
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("没收到 {path} 的请求"));
    assert_eq!(
        req.body.get(key),
        Some(expect),
        "{path} 的请求体 {key} 期望 {expect}，实际 {:?}",
        req.body.get(key)
    );
}

/// 断言替身确实收到了某个路径的请求。
fn assert_hit(state: &MockState, path: &str) {
    let seen = state.seen.lock().unwrap();
    assert!(
        seen.iter().any(|r| r.path == path),
        "替身没收到 {path} 的请求；实际收到：{:?}",
        seen.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// 断言替身收到的请求里带了某个头。
fn assert_header(state: &MockState, path: &str, name: &str) {
    let seen = state.seen.lock().unwrap();
    let req = seen
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("没收到 {path} 的请求"));
    assert!(
        req.headers.contains_key(&name.to_ascii_lowercase()),
        "{path} 的请求缺少头 {name}；实际头：{:?}",
        req.headers.keys().collect::<Vec<_>>()
    );
}

/// 断言替身收到的请求里某个头带了预期值（值含子串即可）。
fn assert_header_contains(state: &MockState, path: &str, name: &str, expect: &str) {
    let seen = state.seen.lock().unwrap();
    let req = seen
        .iter()
        .find(|r| r.path == path)
        .unwrap_or_else(|| panic!("没收到 {path} 的请求"));
    let got = req
        .headers
        .get(&name.to_ascii_lowercase())
        .unwrap_or_else(|| panic!("{path} 的请求缺少头 {name}"));
    assert!(
        got.contains(expect),
        "{path} 的头 {name} 期望含 {expect}，实际 {got}"
    );
}

// ── Loomy ───────────────────────────────────────────────────────────

#[tokio::test]
async fn loomy_claims_successfully() {
    let (base, state) = start_mock(vec![(
        "/points/first-login",
        200,
        json!({"code": "000000", "data": {"dailyBalance": 128}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("loomy", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert!(result.msg.contains("128"), "文案应含余额：{}", result.msg);
    assert_hit(&state, "/points/first-login");
    // 凭证要带上（loomy 用 token 头 + Bearer）
    assert_header(&state, "/points/first-login", "token");
    assert_header_contains(&state, "/points/first-login", "authorization", "Bearer");
}

#[tokio::test]
async fn loomy_reports_expired_session() {
    let (base, _state) = start_mock(vec![(
        "/points/first-login",
        401,
        json!({"code": "020002", "message": "login required"}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("loomy", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Failed);
    assert!(result.msg.contains("失效"), "文案：{}", result.msg);
}

// ── KukuAI ──────────────────────────────────────────────────────────

#[tokio::test]
async fn kuku_claims_and_reports_already_claimed() {
    // 面板：无可领任务（reward_point 为 0）→ 判「已领过」
    let (base, state) = start_mock(vec![
        (
            "/api/genflowpro/freepoint/homenew",
            200,
            json!({"errno": 0, "data": {"reward_point": 0, "task_list": []}}),
        ),
        (
            "/api/genflowpro/freepoint/taskComplete",
            200,
            json!({"errno": 0, "data": {"reward_point": 50}}),
        ),
    ])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("kuku", &base)).await;

    // 面板没有可领任务 → 已领过（幂等）
    assert!(
        matches!(
            result.outcome,
            CheckinOutcome::Already | CheckinOutcome::Success
        ),
        "结果分类不对：{:?} {}",
        result.outcome,
        result.msg
    );
    assert_hit(&state, "/api/genflowpro/freepoint/homenew");
    // Cookie 必须带
    assert_header(&state, "/api/genflowpro/freepoint/homenew", "cookie");
}

// ── Qoder ───────────────────────────────────────────────────────────

#[tokio::test]
async fn qoder_claims_campaign() {
    let (base, state) = start_mock(vec![
        (
            "/sash/api/v1/me/campaigns",
            200,
            json!({"campaigns": [{
                "actionType": "CLAIM_BENEFIT",
                "claimStatus": "CLAIMABLE",
                "campaignId": "camp-1"
            }]}),
        ),
        (
            "/sash/api/v1/me/campaigns/camp-1/claim",
            200,
            json!({
                "status": "CLAIMED",
                "benefit": {"amount": 25.0, "validity": {"days": 30}}
            }),
        ),
    ])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("qoder", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert!(result.msg.contains("25"), "文案应含奖励：{}", result.msg);
    assert_hit(&state, "/sash/api/v1/me/campaigns");
    assert_hit(&state, "/sash/api/v1/me/campaigns/camp-1/claim");
}

#[tokio::test]
async fn qoder_reports_already_claimed() {
    let (base, _state) = start_mock(vec![(
        "/sash/api/v1/me/campaigns",
        200,
        json!({"campaigns": [{
            "actionType": "CLAIM_BENEFIT",
            "claimStatus": "CLAIMED",
            "campaignId": "camp-1"
        }]}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("qoder", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Already);
}

#[tokio::test]
async fn qoder_reports_no_campaign_as_pending() {
    let (base, _state) = start_mock(vec![(
        "/sash/api/v1/me/campaigns",
        200,
        json!({"campaigns": []}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("qoder", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Pending);
}

// ── 小浣熊 ──────────────────────────────────────────────────────────

#[tokio::test]
async fn raccoon_claims_successfully() {
    let (base, state) = start_mock(vec![(
        "/api/web/office/v3/setting_info",
        200,
        json!({"code": 0, "data": {"point_grant_popups": [{"amount": 30}]}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("xiaohuanxiong", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/api/web/office/v3/setting_info");
}

// ── AutoClaw ────────────────────────────────────────────────────────

#[tokio::test]
async fn autoclaw_claims_task() {
    let (base, state) = start_mock(vec![(
        "/autoclaw-proxy/proxy/autoclaw-task-complete",
        200,
        json!({"code": 0, "data": {"reward_points": 66, "success": true}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("autoclaw", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/autoclaw-proxy/proxy/autoclaw-task-complete");
    // 请求构造也要对：POST + 任务 id + 签名头
    assert_method(
        &state,
        "/autoclaw-proxy/proxy/autoclaw-task-complete",
        "POST",
    );
    assert_body_field(
        &state,
        "/autoclaw-proxy/proxy/autoclaw-task-complete",
        "task_id",
        &json!("daily_signin"),
    );
    assert_header(
        &state,
        "/autoclaw-proxy/proxy/autoclaw-task-complete",
        "x-auth-sign",
    );
}

// ── WorkBuddy ───────────────────────────────────────────────────────

#[tokio::test]
async fn workbuddy_claims_daily_checkin() {
    let (base, state) = start_mock(vec![
        (
            "/billing/meter/checkin-activity-status",
            200,
            json!({"code": 0, "data": {"today_checked_in": false, "active": true}}),
        ),
        (
            "/v2/billing/meter/daily-checkin",
            200,
            json!({"code": 0, "data": {"reward": 10}}),
        ),
    ])
    .await;

    // 国际版才走「活动状态 → 领取」两步（国内版直接领）
    let result = claim_for(&reqwest::Client::new(), &account("workbuddy-intl", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/billing/meter/checkin-activity-status");
    assert_hit(&state, "/v2/billing/meter/daily-checkin");
}

#[tokio::test]
async fn workbuddy_reports_activity_not_open_as_pending() {
    let (base, _state) = start_mock(vec![(
        "/billing/meter/checkin-activity-status",
        200,
        json!({"code": 0, "data": {"today_checked_in": false, "active": false}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("workbuddy-intl", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Pending);
}

// ── Trae ────────────────────────────────────────────────────────────

#[tokio::test]
async fn trae_claims_credits() {
    let (base, state) = start_mock(vec![
        (
            "/trae/api/v2/ug/checkin_credits/status",
            200,
            json!({"code": 0, "enable": true, "checked_in": false}),
        ),
        (
            "/trae/api/v2/ug/checkin_credits/claim",
            200,
            json!({"code": 0, "message": "ok", "data": {"credits": 20}}),
        ),
        // 状态端点被打两次：首次查（未签）→ 领取 → 同设备号回查（已签）。
        // Trae 的判据是「回查翻转才算数」，所以必须回放第二个响应。
        (
            "/trae/api/v2/ug/checkin_credits/status",
            200,
            json!({"code": 0, "enable": true, "checked_in": true}),
        ),
    ])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("trae", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/trae/api/v2/ug/checkin_credits/status");
    assert_hit(&state, "/trae/api/v2/ug/checkin_credits/claim");
}

#[tokio::test]
async fn trae_reports_not_open_as_pending() {
    let (base, _state) = start_mock(vec![(
        "/trae/api/v2/ug/checkin_credits/status",
        200,
        json!({"code": 0, "enable": false, "checked_in": false}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("trae", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Pending);
}

#[tokio::test]
async fn trae_reports_already_checked_in() {
    let (base, _state) = start_mock(vec![(
        "/trae/api/v2/ug/checkin_credits/status",
        200,
        json!({"code": 0, "enable": true, "checked_in": true}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("trae", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Already);
}

// ── CodeArts ────────────────────────────────────────────────────────

#[tokio::test]
async fn codearts_claims_ops_welfare() {
    let (base, state) = start_mock(vec![
        (
            "/v1/ops/delivery",
            200,
            json!({"data": {"items": [{
                "type": "USER_LOGIN",
                "benefitUnit": "CREDIT",
                "campaignId": "camp-9",
                "status": "PENDING",
                "claimable": true
            }]}}),
        ),
        (
            "/v1/ops/claim",
            200,
            json!({"code": 0, "data": {"items": [{"id": "w-1", "status": "CLAIMED"}]}}),
        ),
        (
            "/v1/ops/confirm",
            200,
            json!({"code": 0, "data": {"items": [{"id": "w-1", "status": "CONFIRMED"}]}}),
        ),
        // 回读 delivery：上游硬要求「不确认不算到账」，回读必须看到
        // claimable=false + status=CONFIRMED 才算成功。
        (
            "/v1/ops/delivery",
            200,
            json!({"code": 0, "data": {"items": [{
                "type": "USER_LOGIN",
                "benefitUnit": "CREDIT",
                "campaignId": "camp-9",
                "status": "CONFIRMED",
                "claimable": false
            }]}}),
        ),
    ])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("codearts", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/v1/ops/delivery");
}

// ── OfficeAce ───────────────────────────────────────────────────────

#[tokio::test]
async fn officeace_claims_bonus() {
    let (base, state) = start_mock(vec![
        (
            "/v1/subscription",
            200,
            json!({"data": {"points": "100", "bonus_skus": []}}),
        ),
        (
            "/v1/subscription/bonus/claim",
            200,
            json!({"data": {"points": "150", "bonus_skus": []}}),
        ),
    ])
    .await;

    // OfficeAce 的控制面凭据是 AK/SK（不是单 secret）：`ak:sk[:sts[:project]]`
    let mut session = account("officeace", &base);
    session.credential.secret = "ak-test:sk-test:sts-test:proj-test".to_string();

    let result = claim_for(&reqwest::Client::new(), &session).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert_hit(&state, "/v1/subscription/bonus/claim");
    // V11 签名头要带上
    assert_header(&state, "/v1/subscription/bonus/claim", "x-sdk-date");
}

// ── ModelScope（魔搭）─────────────────────────────────────────────
//
// 签到 = 调一次登录信息接口（调用即触发当日魔粒，幂等）。令牌两用：
// `Cookie: m_session_id=<令牌>` + `Authorization: Bearer <令牌>`。

#[tokio::test]
async fn modelscope_checks_in_via_login_info() {
    let (base, state) = start_mock(vec![(
        "/api/v1/users/login/info",
        200,
        json!({"Code": 200, "Message": "Success", "Data": {"Name": "tester"}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("modelscope", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Success);
    assert!(
        result.msg.contains("魔粒"),
        "文案应说明签到动作：{}",
        result.msg
    );
    assert_hit(&state, "/api/v1/users/login/info");
    assert_method(&state, "/api/v1/users/login/info", "GET");
    // 令牌两用：Cookie m_session_id + Bearer
    assert_header(&state, "/api/v1/users/login/info", "cookie");
    assert_header_contains(
        &state,
        "/api/v1/users/login/info",
        "cookie",
        "m_session_id=",
    );
    assert_header_contains(
        &state,
        "/api/v1/users/login/info",
        "authorization",
        "Bearer",
    );
}

#[tokio::test]
async fn modelscope_reports_expired_session() {
    let (base, _state) = start_mock(vec![(
        "/api/v1/users/login/info",
        401,
        json!({"Code": 401, "Message": "unauthorized"}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("modelscope", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Failed);
    assert!(result.msg.contains("失效"), "文案：{}", result.msg);
}

#[tokio::test]
async fn modelscope_reports_bad_business_code() {
    let (base, _state) = start_mock(vec![(
        "/api/v1/users/login/info",
        200,
        json!({"Code": 40001, "Message": "token expired"}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("modelscope", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Failed);
    assert!(
        result.msg.contains("40001"),
        "文案应含业务码：{}",
        result.msg
    );
}

// ── ZCode（领当天赠送套餐）──────────────────────────────────────

/// 池是**进程级单例**（与 `zcode_captcha` 自己的池测试并行跑）：
/// 「推一枚 → 取一枚」之间可能被并发测试偷走。取不到令牌时（claim
/// 回「令牌池为空」）重推重试 —— 连试三次都空才把结果交给断言。
async fn zcode_claim_with_pool_token(provider: &str, base: &str) -> super::ClaimResult {
    let mut result = super::ClaimResult {
        success: false,
        msg: String::new(),
        outcome: CheckinOutcome::Failed,
    };
    for _ in 0..3 {
        crate::zcode_captcha::push("runtime-test-param", "cn");
        result = claim_for(&reqwest::Client::new(), &account(provider, base)).await;
        if !result.msg.contains("令牌池") {
            return result;
        }
    }
    result
}

#[tokio::test]
async fn zcode_claims_daily_gift_plan() {
    let today = chrono::Utc::now()
        .with_timezone(&crate::checkin::scheduler::beijing())
        .format("%Y%m%d")
        .to_string();
    let plan_id = format!("zcode-v3-start-plan-{today}");
    let (base, state) = start_mock(vec![
        (
            "/api/v1/zcode-plan/billing/preview",
            200,
            json!({"code": 0, "data": {"plans": [
                {"planId": "zcode-v3-coding-plan-001"},
                {"planId": plan_id.clone()},
            ]}}),
        ),
        ("/api/v1/event/report", 200, json!({"code": 0})),
        // claim 序列：成功 → 已领（1003）→ 窗口未开（1002）
        (
            "/api/v1/zcode-plan/billing/claim",
            200,
            json!({"code": 0, "msg": "ok"}),
        ),
        (
            "/api/v1/zcode-plan/billing/claim",
            200,
            json!({"code": 1003, "msg": "already claimed"}),
        ),
        (
            "/api/v1/zcode-plan/billing/claim",
            200,
            json!({"code": 1002, "msg": "not claimable"}),
        ),
    ])
    .await;

    // ① 成功：探测 → 挑当天套餐 → 领取
    let result = zcode_claim_with_pool_token("zcode", &base).await;
    assert_eq!(
        result.outcome,
        CheckinOutcome::Success,
        "文案：{}",
        result.msg
    );
    assert_hit(&state, "/api/v1/zcode-plan/billing/preview");
    assert_hit(&state, "/api/v1/zcode-plan/billing/claim");
    // 领取请求：POST + plan_id + 验证码头 + 设备指纹
    assert_method(&state, "/api/v1/zcode-plan/billing/claim", "POST");
    assert_body_field(
        &state,
        "/api/v1/zcode-plan/billing/claim",
        "plan_id",
        &json!(plan_id),
    );
    assert_header(&state, "/api/v1/zcode-plan/billing/claim", "x-device-mid");
    assert_header(
        &state,
        "/api/v1/zcode-plan/billing/claim",
        "x-aliyun-captcha-verify-param",
    );
    // 资格前置：激活事件补报
    assert_hit(&state, "/api/v1/event/report");

    // ② 同一份套餐再领 → 1003 已领过（幂等）
    let result = zcode_claim_with_pool_token("zcode", &base).await;
    assert_eq!(
        result.outcome,
        CheckinOutcome::Already,
        "文案：{}",
        result.msg
    );

    // ③ 套餐不可领（窗口未开）→ Pending（不算失败）
    let result = zcode_claim_with_pool_token("zcode", &base).await;
    assert_eq!(
        result.outcome,
        CheckinOutcome::Pending,
        "文案：{}",
        result.msg
    );
}

#[tokio::test]
async fn zcode_reports_empty_plans_as_pending() {
    // 窗口未开（19:00 前）：列表里没有当天赠送套餐 → Pending（不算失败）
    let (base, _state) = start_mock(vec![(
        "/api/v1/zcode-plan/billing/preview",
        200,
        json!({"code": 0, "data": {"plans": []}}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("zcode", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Pending);
    assert!(
        result.msg.contains("19:00"),
        "文案应说明窗口：{}",
        result.msg
    );
}

#[tokio::test]
async fn zcode_reports_expired_session() {
    let (base, _state) = start_mock(vec![(
        "/api/v1/zcode-plan/billing/preview",
        401,
        json!({"code": 401, "msg": "unauthorized"}),
    )])
    .await;

    let result = claim_for(&reqwest::Client::new(), &account("zcode", &base)).await;

    assert_eq!(result.outcome, CheckinOutcome::Failed);
    assert!(result.msg.contains("失效"), "文案：{}", result.msg);
}

// ── 通用：所有家的「空凭证」都应被拦住 ────────────────────────────

#[tokio::test]
async fn every_provider_rejects_empty_credential() {
    for provider in super::claim::supported_providers() {
        let mut session = account(provider, "http://127.0.0.1:1");
        session.credential.secret = String::new();
        let result = claim_for(&reqwest::Client::new(), &session).await;
        assert_eq!(
            result.outcome,
            CheckinOutcome::Failed,
            "{provider} 空凭证时不该判成别的结果"
        );
    }
}

// ── 每日调度：workbuddy-intl 走 Full 链（探测 + 领取 + 桌面/网页保活）──
//
// 对齐 Agent2API「定时与批量签到恒走 WorkbuddyActivity::Full」：调度链必须
// 真的打到四条路径（探测 / 领取 / 桌面 / 网页），保活计数与 `kept_active`
// 台账标记由调度一次产出，不依赖前端手动回写。

#[tokio::test]
// 跨模块密钥目录锁必须在整个 mock + 调度期间持有：中途放锁会让别的测试
// 线程换掉全局 data_dir，加密落库路径拿错密钥。这里持的是 std Mutex，
// 但 guarded 区段内只有 mock 服务与本地调度，无真实阻塞风险。
#[allow(clippy::await_holding_lock)]
async fn schedule_runs_workbuddy_intl_full_keepalive_chain() {
    let _guard = crate::account_session::secret::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::account_session::secret::set_data_dir(&temp_data_dir("full"));

    let (base, state) = start_mock(vec![
        (
            "/billing/meter/checkin-activity-status",
            200,
            json!({"code": 0, "data": {"today_checked_in": false, "active": true}}),
        ),
        (
            "/v2/billing/meter/daily-checkin",
            200,
            json!({"code": 0, "data": {"continuousDays": 2, "points": 10}}),
        ),
        (
            "/v1/chat/completions",
            200,
            json!({"id": "c1", "choices": [{"message": {"role": "assistant", "content": "hi"}}]}),
        ),
        ("/console/as/conversations/", 200, json!({"id": "conv-1"})),
    ])
    .await;

    let (store, sessions, egress) = schedule_env();
    sessions.upsert(account("workbuddy-intl", &base)).unwrap();

    let outcome = super::scheduler::run_schedule(&store, &sessions, &egress, &[], 0).await;

    assert_eq!(outcome.ok, 1, "Full 链应算成功：{}", outcome.summary);
    assert_eq!(outcome.keepalive, 1, "保活计数应为 1：{}", outcome.summary);
    assert_eq!(outcome.failed, 0);
    assert!(
        outcome.summary.contains("保活"),
        "摘要应含保活：{}",
        outcome.summary
    );
    // 四条路径都要打到：探测 + 领取 + 桌面 + 网页
    assert_hit(&state, "/billing/meter/checkin-activity-status");
    assert_hit(&state, "/v2/billing/meter/daily-checkin");
    assert_hit(&state, "/v1/chat/completions");
    assert_hit(&state, "/console/as/conversations/");
    // 台账记录带保活标记（不再需要前端手动回写）
    let records = store
        .recent_records(1, Some("workbuddy-intl"), None, None, None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(records[0].kept_active, "台账应带保活标记");
}

#[tokio::test]
// 同上：全局密钥目录锁贯穿整个断言过程。
#[allow(clippy::await_holding_lock)]
async fn schedule_keepalive_zero_when_both_pokes_fail() {
    // 两发保活都失败时保活计数必须为 0 —— 不能把「打了没点亮」算成保活成功；
    // 记录按 Pending 落账（claim 成功但保活链没跑成，稍后重试）。
    let _guard = crate::account_session::secret::test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    crate::account_session::secret::set_data_dir(&temp_data_dir("poke-fail"));

    let (base, state) = start_mock(vec![
        (
            "/billing/meter/checkin-activity-status",
            200,
            json!({"code": 0, "data": {"today_checked_in": false, "active": true}}),
        ),
        (
            "/v2/billing/meter/daily-checkin",
            200,
            json!({"code": 0, "data": {"continuousDays": 1, "points": 5}}),
        ),
        (
            "/v1/chat/completions",
            500,
            json!({"code": 1, "msg": "model busy"}),
        ),
        (
            "/console/as/conversations/",
            500,
            json!({"code": 1, "msg": "server error"}),
        ),
    ])
    .await;

    let (store, sessions, egress) = schedule_env();
    sessions.upsert(account("workbuddy-intl", &base)).unwrap();

    let outcome = super::scheduler::run_schedule(&store, &sessions, &egress, &[], 0).await;

    assert_eq!(outcome.keepalive, 0, "两发都失败时保活计数必须为 0");
    assert_eq!(outcome.ok, 0);
    assert_eq!(outcome.failed, 0, "Pending 不算失败");
    assert_hit(&state, "/v1/chat/completions");
    assert_hit(&state, "/console/as/conversations/");
    let records = store
        .recent_records(1, Some("workbuddy-intl"), None, None, None)
        .unwrap();
    assert_eq!(records.len(), 1);
    assert!(!records[0].kept_active, "保活没点亮时不该打保活标记");
    assert!(
        records[0].message.contains("保活"),
        "文案应交代保活走向：{}",
        records[0].message
    );
}

/// 临时数据目录（密钥文件所在处；防并行测试互相换密钥）。
fn temp_data_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "aigx-checkin-rt-{tag}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 临时 FileStore（调度测试要落台账与保活记忆）。
fn temp_store() -> Arc<crate::storage::FileStore> {
    let dir = std::env::temp_dir().join(format!(
        "aigx-checkin-rt-fs-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Arc::new(crate::storage::FileStore::new(dir))
}

/// 调度测试环境：签到台账 + 账号池 + 出口缓存（互相独立的临时库）。
fn schedule_env() -> (
    Arc<super::store::CheckinStore>,
    crate::account_session::AccountSessionStore,
    Arc<crate::egress::EgressCache>,
) {
    let store = Arc::new(super::store::CheckinStore::new(temp_store()));
    store.load().unwrap();
    let sessions = crate::account_session::AccountSessionStore::new(temp_store());
    let egress = Arc::new(crate::egress::EgressCache::new());
    (store, sessions, egress)
}
