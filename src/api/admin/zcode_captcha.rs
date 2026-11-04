//! ZCode 人机验证令牌池 API（对照 Agent2API `api/zcode_captcha.rs` +
//! `api/zcode_captcha_pool.rs`，两者在 AIGX 合并成一个模块）。
//!
//! - `GET    /api/zcode/captcha`          池子概况 + 铸造节奏 + 「该不该铸」
//! - `POST   /api/zcode/captcha`          令牌入池（前端 SDK 铸好一个推一个）
//! - `GET    /api/zcode/captcha/config`   上游风控配置（`{sceneId,prefix,region}`）
//! - `GET    /api/zcode/captcha-pool`     铸造节奏设置
//! - `PUT    /api/zcode/captcha-pool`     改铸造节奏（落库 + 进程快照热更）
//!
//! ── 为什么「该不该铸」由网关算 ─────────────────────────────────
//! 前端铸造器每 2.5 秒问一次概况，它**没有**账号表也不知道谁在用这条通道。
//! 把「有没有 zcode 账号」这个判据放在网关侧，"没有账号用这条路时一个都不铸"
//! 才成立 —— 阿里云对铸造频率有风控，白铸纯属烧配额（见 `zcode_captcha` 模块头）。
//!
//! ── 令牌池与「领套餐」滑块的关系 ───────────────────────────────
//! 令牌是**机器对机器的门禁令牌**（后台转发/后台领取用，无痕铸造、用户看不见），
//! 与用户点「领套餐」时拖的那个滑块不是一回事：后者是**用户动作**，必须让用户
//! 看见、确认（前端 `aliyunCaptcha.solve()`）。两者共用同一份上游风控配置
//! （本模块的 `/config` 就是给前端取那份配置用的）。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::config::{
    set_zcode_captcha_pool_settings, zcode_captcha_pool_settings, ZCODE_POOL_PER_ROUND_MAX,
    ZCODE_POOL_PER_ROUND_MIN, ZCODE_POOL_POLL_MAX_MS, ZCODE_POOL_POLL_MIN_MS,
    ZCODE_POOL_TARGET_MAX, ZCODE_POOL_TARGET_MIN,
};
use crate::zcode_captcha;

/// ZCode 两地的 provider id（平台域相同，令牌池全局一份、不绑地区）。
const ZCODE_PROVIDERS: [&str; 2] = ["zcode", "zcode-intl"];

/// 单次推送最多几枚（前端单轮上限是 1~5，留点余量）
const MAX_TOKENS_PER_PUSH: usize = 8;
/// 单个验证串的长度上限：实测约 280 字符，8192 是防御性上界（防手滑粘一整页）
const MAX_PARAM_LEN: usize = 8192;
/// region 字符串长度上限（`cn` / `sgp` 这种短码）
const MAX_REGION_LEN: usize = 32;

fn is_zcode_provider(provider: &str) -> bool {
    ZCODE_PROVIDERS.contains(&provider)
}

/// 第一个 zcode 账号的 id（前端拿它去取上游风控配置）。
///
/// 「第一个」而非「最好的」：风控配置是**平台级**的，与具体账号无关
/// （同一份 `configs.captcha`），任何一个 zcode 账号的鉴权头都问得出来。
fn any_zcode_account(state: &AppState) -> Option<String> {
    state
        .account_sessions
        .all()
        .into_iter()
        .find(|session| is_zcode_provider(&session.provider))
        .map(|session| session.id)
}

/// 池子概况（GET /api/zcode/captcha 的主体，也是铸造器每轮的输入）。
fn pool_view(state: &AppState) -> Value {
    let (target, poll_ms, max_per_round) = zcode_captcha_pool_settings();
    let account_id = any_zcode_account(state);
    let ready = zcode_captcha::ready();
    let mut view = zcode_captcha::stats();
    if let Some(object) = view.as_object_mut() {
        object.insert("target".to_string(), json!(target));
        object.insert("pollMs".to_string(), json!(poll_ms));
        object.insert("maxPerRound".to_string(), json!(max_per_round));
        // 有账号在用这条通道 = 有 zcode 账号；库存不足目标才需要补
        object.insert("hasAccounts".to_string(), json!(account_id.is_some()));
        object.insert(
            "needsTokens".to_string(),
            json!(account_id.is_some() && ready < target as usize),
        );
        object.insert(
            "captchaAccountId".to_string(),
            json!(account_id.unwrap_or_default()),
        );
    }
    view
}

/// GET /api/zcode/captcha —— 池子概况 + 铸造节奏 + 该不该铸。
pub async fn handle_get_zcode_captcha(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({ "success": true, "data": pool_view(&state) })))
}

/// POST /api/zcode/captcha —— 令牌入池。
///
/// 收两种形状：`{"tokens":[{"param":"…","region":"cn"} …]}`（铸造器的批量形态）
/// 与 `{"param":"…","region":"cn"}`（面板里手动粘贴单枚，排障用）。
/// 空串 / 超长一律 400：入池的空串会让下一次请求带一个空头出去（上游当缺失
/// 处理，回 3007），白费一次往返还把库存读数变成假的。
pub async fn handle_push_zcode_captcha(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    // 归一成待入池列表
    let mut incoming: Vec<(String, String)> = Vec::new();
    if let Some(list) = payload.get("tokens").and_then(Value::as_array) {
        if list.len() > MAX_TOKENS_PER_PUSH {
            return Err(error_response(
                &format!("单次最多推送 {MAX_TOKENS_PER_PUSH} 枚令牌"),
                StatusCode::BAD_REQUEST,
            ));
        }
        for token in list {
            let param = token
                .get("param")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            let region = token
                .get("region")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if !param.is_empty() {
                incoming.push((param, region));
            }
        }
    } else if let Some(param) = payload.get("param").and_then(Value::as_str) {
        let region = payload
            .get("region")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        incoming.push((param.trim().to_string(), region));
    }

    if incoming.is_empty() {
        return Err(error_response(
            "缺少 param（或 tokens 数组里没有任何非空 param）",
            StatusCode::BAD_REQUEST,
        ));
    }
    for (param, region) in &incoming {
        if param.is_empty() {
            return Err(error_response("param 不能为空", StatusCode::BAD_REQUEST));
        }
        if param.len() > MAX_PARAM_LEN {
            return Err(error_response(
                "param 过长（不像阿里云验证码串，请检查是否粘错内容）",
                StatusCode::BAD_REQUEST,
            ));
        }
        if region.len() > MAX_REGION_LEN {
            return Err(error_response("region 过长", StatusCode::BAD_REQUEST));
        }
    }

    let mut pushed = 0usize;
    for (param, region) in &incoming {
        zcode_captcha::push(param, region);
        pushed += 1;
    }
    tracing::info!(
        "ZCode 验证码令牌入池 {pushed} 枚（当前库存 {}）",
        zcode_captcha::ready()
    );
    Ok(Json(json!({
        "success": true,
        "data": { "pushed": pushed, "ready": zcode_captcha::ready() }
    })))
}

/// GET /api/zcode/captcha/config?account_id=… —— 上游风控配置。
///
/// 前端求解器与铸造器都要它：`enabled:false` 那一刻**不该**弹滑块、也不该铸
/// （上游此刻不要验证码，铸了就是白烧风控配额）。`account_id` 省略时自动挑一个
/// zcode 账号（配置是平台级的，与具体账号无关）。
pub async fn handle_get_zcode_captcha_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CaptchaConfigQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let account_id = query
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .or_else(|| any_zcode_account(&state))
        .ok_or_else(|| {
            error_response(
                "没有 ZCode 账号，无法读取上游风控配置",
                StatusCode::NOT_FOUND,
            )
        })?;
    let Some(session) = state.account_sessions.get(&account_id) else {
        return Err(error_response("账号不存在", StatusCode::NOT_FOUND));
    };
    if !is_zcode_provider(&session.provider) {
        return Err(error_response(
            "该账号不是 ZCode 账号，没有验证码配置",
            StatusCode::BAD_REQUEST,
        ));
    }
    let client = state
        .account_sessions
        .egress()
        .client(session.proxy.as_deref())
        .map_err(|e| error_response(&format!("出口配置错误：{e}"), StatusCode::BAD_REQUEST))?;
    // 复用福利通道的 config 分支（同一份端点与鉴权，不另写一套请求构造）
    let result = crate::checkin::benefits::run(
        &state.account_sessions,
        &state.alert_store,
        &client,
        &session,
        "plan",
        &json!({ "mode": "config" }),
    )
    .await;
    if !result.ok {
        return Err(error_response(&result.message, StatusCode::BAD_GATEWAY));
    }
    let captcha = result.data.get("captcha").cloned().unwrap_or(Value::Null);
    Ok(Json(json!({
        "success": true,
        "data": {
            "account_id": account_id,
            "captcha": captcha,
        }
    })))
}

#[derive(Debug, Deserialize)]
pub struct CaptchaConfigQuery {
    #[serde(default)]
    pub account_id: Option<String>,
}

/// 铸造节奏的当前值 + 合法范围（界面据此渲染输入框的 min/max）。
fn pool_settings_view() -> Value {
    let (target, poll_ms, max_per_round) = zcode_captcha_pool_settings();
    json!({
        "target": target,
        "pollMs": poll_ms,
        "maxPerRound": max_per_round,
        "ranges": {
            "target": [ZCODE_POOL_TARGET_MIN, ZCODE_POOL_TARGET_MAX],
            "pollMs": [ZCODE_POOL_POLL_MIN_MS, ZCODE_POOL_POLL_MAX_MS],
            "maxPerRound": [ZCODE_POOL_PER_ROUND_MIN, ZCODE_POOL_PER_ROUND_MAX],
        },
        "ttlMs": zcode_captcha::TOKEN_TTL_MS,
        "ready": zcode_captcha::ready(),
    })
}

/// GET /api/zcode/captcha-pool —— 铸造节奏设置。
pub async fn handle_get_zcode_captcha_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(
        json!({ "success": true, "data": pool_settings_view() }),
    ))
}

/// PUT /api/zcode/captcha-pool —— 改铸造节奏（落库 + 进程级快照热更）。
///
/// 三项都是可选的（缺省 = 不改）。越界值**夹进合法范围**而不是 400：这三个数
/// 是纯节奏参数，夹一下的后果只是「比你以为的慢/快一点」，而 400 会让设置页
/// 整个保存失败 —— 代价不对称。
pub async fn handle_update_zcode_captcha_pool(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    let Some(object) = payload.as_object() else {
        return Err(error_response(
            "请求体必须是 JSON 对象",
            StatusCode::BAD_REQUEST,
        ));
    };
    if let Some(value) = object.get("target").and_then(Value::as_u64) {
        config.usage.zcode_captcha_pool_target =
            value.clamp(ZCODE_POOL_TARGET_MIN as u64, ZCODE_POOL_TARGET_MAX as u64) as u8;
    }
    if let Some(value) = object.get("pollMs").and_then(Value::as_u64) {
        config.usage.zcode_captcha_pool_poll_ms =
            value.clamp(ZCODE_POOL_POLL_MIN_MS as u64, ZCODE_POOL_POLL_MAX_MS as u64) as u32;
    }
    if let Some(value) = object.get("maxPerRound").and_then(Value::as_u64) {
        config.usage.zcode_captcha_pool_max_per_round = value.clamp(
            ZCODE_POOL_PER_ROUND_MIN as u64,
            ZCODE_POOL_PER_ROUND_MAX as u64,
        ) as u8;
    }
    // 落库前先把生效值记下来：`update` 按值吃掉 config（与 cline_headers 同形），
    // 快照热更用的这三个数不能从已移走的 config 里再读。
    let applied = (
        config.usage.zcode_captcha_pool_target,
        config.usage.zcode_captcha_pool_poll_ms,
        config.usage.zcode_captcha_pool_max_per_round,
    );
    state.config_manager.update(config).await.map_err(|e| {
        error_response(
            &format!("验证码令牌池设置持久化失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    // 快照热更：前端铸造器下一轮（≤ pollMs）就会读到新节奏，无需重启
    set_zcode_captcha_pool_settings(applied.0, applied.1, applied.2);
    tracing::info!(
        "ZCode 验证码令牌池节奏已更新（target={} pollMs={} maxPerRound={}）",
        applied.0,
        applied.1,
        applied.2
    );
    Ok(Json(json!({
        "success": true,
        "data": pool_settings_view()
    })))
}
