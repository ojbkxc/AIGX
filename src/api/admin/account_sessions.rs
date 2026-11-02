//! 账号型渠道账号池管理 API（`/api/account-sessions`）。
//!
//! 账号级调度（优先级 / 权重 / 冷却 / 并发）由 `crate::account_session` 承担，
//! 本模块只做管理面：CRUD、批量导入、单号探活、清冷却、聚合统计。
//!
//! 与渠道管理的关系：账号必须挂在 `channel_type = account` 的渠道下；
//! 渠道删除时级联删除账号（见 `channels::delete_channel_core`）。

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::super::openai::AppState;
use super::channels::mask_secret;
use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};
use crate::account_session::adapter;
use crate::account_session::model::AccountSession;
use crate::account_session::registry;
use crate::account_session::rotate::RotationPolicy;
use crate::bridge::{BridgeContext, ChatFormat, ChatMessage, Role};
use crate::channel::{Channel, ChannelTestResult, ChannelType};

/// 批量导入单次上限（防一次性灌爆存储）。
const MAX_BULK_IMPORT: usize = 500;
/// 单号探活超时（秒）。
const PROBE_TIMEOUT_SECS: u64 = 30;
/// 渠道级探活最多试几个账号。
const CHANNEL_PROBE_ACCOUNTS: usize = 3;

fn default_bulk_weight() -> u32 {
    1
}

fn default_bulk_concurrency() -> u32 {
    3
}

// ── 请求体 ──────────────────────────────────────────────────────────

/// 新增 / 更新账号请求。
///
/// `api_key` 三态（与渠道 api_key 同口径）：
/// - 缺省 = 更新时保持原值（新增 = 空凭证）
/// - `""` = 清除凭证
/// - 非空 = 设置新凭证
#[derive(Debug, Deserialize)]
pub struct AccountSessionRequest {
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub provider: String,
    /// 账号级上游地址（空 = 用 provider 目录默认值）。
    #[serde(default)]
    pub base_url: String,
    /// 账号级对话路径覆盖（如 `/api/chat`）；空 = 默认 `<base>/chat/completions`。
    #[serde(default)]
    pub chat_path: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub status: String,
    /// 优先级。三态：缺省 = 保持原值（PUT 只提交部分字段时不清零）。
    #[serde(default)]
    pub priority: Option<i64>,
    /// 权重。三态：缺省 = 保持原值。
    #[serde(default)]
    pub weight: Option<u32>,
    /// 单账号并发上限。三态：缺省 = 保持原值（0 = 不限制，是合法值，
    /// 不能与「字段缺省」混为一谈）。
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub models: Vec<String>,
    /// 账号级额外请求头（登录态型供应商补专用头；值支持 `{credential}` 模板）。
    #[serde(default)]
    pub extra_headers: Option<HashMap<String, String>>,
    /// 续期令牌（refreshToken / Coding Plan JWT / deviceId 等）。三态：缺省 =
    /// 保持原值；空串 = 清除；非空 = 设置。
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// 凭证形态（api_key / session / none）。缺省 = 自动判定（登录态型供应商
    /// 落 session，其余 api_key）；显式传 "none" = 该上游无需鉴权（Ollama /
    /// OpenCode Zen 免费档，转发时一个鉴权头都不发）。
    #[serde(default)]
    pub credential_kind: Option<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// 账号部分更新请求（PATCH）：只应用显式给出的字段。
///
/// 与 PUT（整表单替换）区分：PATCH 不触碰 base_url / models / 代理 / 凭证 /
/// 到期时间，供控制台开关与 AI 工具做「启停 / 调优先级」这类小操作。
#[derive(Debug, Default, Deserialize)]
pub struct AccountSessionPatch {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<i64>,
    #[serde(default)]
    pub weight: Option<u32>,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
}

impl AccountSessionPatch {
    /// 仅设置运行状态（MCP/Agent 启停工具的快捷路径）。
    pub fn status_only(status: &str) -> Self {
        Self {
            status: Some(status.to_string()),
            ..Self::default()
        }
    }
}

/// 批量导入请求。
#[derive(Debug, Deserialize)]
pub struct BulkImportRequest {
    pub channel_id: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub name_prefix: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_bulk_weight")]
    pub weight: u32,
    #[serde(default = "default_bulk_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub models: Vec<String>,
    /// 每行一条凭证：`名称,Key` 或仅 `Key`；`#` 开头与空行忽略。
    pub credentials: String,
}

// ── 辅助 ────────────────────────────────────────────────────────────

/// 校验渠道存在且是账号型渠道。
fn ensure_account_channel(
    state: &AppState,
    channel_id: &str,
) -> Result<Channel, (StatusCode, Json<Value>)> {
    let channel = state
        .channel_store
        .get(channel_id)
        .ok_or_else(|| error_response("Channel not found", StatusCode::NOT_FOUND))?;
    if channel.channel_type != ChannelType::Account {
        return Err(error_response(
            "Channel type must be 'account' to hold account sessions",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(channel)
}

/// 把请求字段写入账号（`api_key` 三态 / 空字段保持原值）。
fn apply_request(mut session: AccountSession, body: &AccountSessionRequest) -> AccountSession {
    if !body.name.trim().is_empty() {
        session.name = body.name.trim().to_string();
    }
    if !body.provider.trim().is_empty() {
        session.provider = registry::normalize_kind(&body.provider);
    }
    session.base_url = body.base_url.trim().to_string();
    if let Some(path) = &body.chat_path {
        // 空 = 清除覆盖；拒绝路径穿越；缺前导斜杠时补上
        let trimmed = path.trim();
        session.chat_path = if trimmed.is_empty() || trimmed.contains("..") {
            None
        } else if trimmed.starts_with('/') {
            Some(trimmed.to_string())
        } else {
            Some(format!("/{trimmed}"))
        };
    }
    match &body.api_key {
        None => {}
        Some(secret) => session.credential.secret = secret.clone(),
    }
    // 显式凭证形态（none = 免鉴权，转发时一个鉴权头都不发；Ollama /
    // OpenCode Zen 免费档）。白名单外的值忽略（防止乱填 kind 导致鉴权错乱）。
    if let Some(kind) = &body.credential_kind {
        let k = kind.trim();
        if matches!(
            k,
            crate::account_session::model::CREDENTIAL_KIND_API_KEY
                | crate::account_session::model::CREDENTIAL_KIND_SESSION
                | crate::account_session::model::CREDENTIAL_KIND_NONE
        ) {
            session.credential.kind = k.to_string();
        }
    }
    // 续期令牌三态：缺省保持、空串清除、非空设置（落盘编码在 store.upsert 里做）
    if let Some(token) = &body.refresh_token {
        session.refresh_secret = token.trim().to_string();
    }
    if let Some(proxy) = &body.proxy {
        let trimmed = proxy.trim();
        session.proxy = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }
    if !body.status.trim().is_empty() {
        session.status = body.status.trim().to_string();
    }
    // 三态字段：缺省（None）= 保持原值，不覆盖。PUT 只提交部分字段时
    // 不会把优先级/权重/并发清零（与 MCP/Agent 工具的小操作路径兼容）。
    if let Some(priority) = body.priority {
        session.priority = priority;
    }
    if let Some(weight) = body.weight {
        if weight > 0 {
            session.weight = weight;
        }
    }
    if let Some(max_concurrency) = body.max_concurrency {
        session.max_concurrency = max_concurrency;
    }
    session.expires_at = body.expires_at.filter(|at| *at > 0);
    session.models = Vec::new();
    for model in &body.models {
        let name = model.trim();
        if !name.is_empty() {
            session.models.push(name.to_string());
        }
    }
    if let Some(headers) = &body.extra_headers {
        let mut cleaned = sanitize_headers(headers);
        // 敏感头被脱敏成 `****` 后原样提交回来：保留原值
        for (name, value) in cleaned.iter_mut() {
            if value == "****" {
                if let Some(original) = session
                    .extra_headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v.clone())
                {
                    *value = original;
                }
            }
        }
        session.extra_headers = cleaned;
    }
    session
}

/// 额外请求头清洗：去空名、去控制字符、限制 20 条（同名后写覆盖）。
fn sanitize_headers(raw: &HashMap<String, String>) -> HashMap<String, String> {
    let mut cleaned: HashMap<String, String> = HashMap::new();
    for (name, value) in raw {
        let name = name.trim();
        if name.is_empty() || name.chars().any(char::is_control) {
            continue;
        }
        let value = value.trim();
        if value.chars().any(|c| c == '\r' || c == '\n') {
            continue;
        }
        if cleaned.len() >= 20 {
            break;
        }
        cleaned.insert(name.to_string(), value.to_string());
    }
    cleaned
}

/// provider 展示名（默认账号名用）。
fn provider_label(kind: &str) -> &'static str {
    registry::find(kind)
        .map(|spec| spec.label)
        .unwrap_or("account")
}

/// 关键词匹配（名称/provider/地址/最近错误）。
fn matches_search(session: &AccountSession, needle: &str) -> bool {
    if session.name.to_lowercase().contains(needle)
        || session.provider.to_lowercase().contains(needle)
        || session.base_url.to_lowercase().contains(needle)
    {
        return true;
    }
    session
        .last_error
        .as_deref()
        .unwrap_or_default()
        .to_lowercase()
        .contains(needle)
}

/// 冷却视图：仅保留尚未到期的条目，值为剩余秒数。
fn cooldowns_json(session: &AccountSession, now: i64) -> Value {
    let mut map = Map::new();
    for (model, until) in &session.cooldowns {
        if *until > now {
            map.insert(model.clone(), json!(*until - now));
        }
    }
    Value::Object(map)
}

/// 代理串脱敏：`http://user:pass@host` → `http://***@host`（无 userinfo 原样返回）。
fn mask_proxy(proxy: &str) -> String {
    let Some((userinfo, host)) = proxy.rsplit_once('@') else {
        return proxy.to_string();
    };
    let scheme_end = userinfo.find("://").map(|index| index + 3).unwrap_or(0);
    let scheme = &userinfo[..scheme_end];
    format!("{scheme}***@{host}")
}

/// 渠道展示名（找不到返回空串）。
fn channel_name_for(channels: &[Channel], channel_id: &str) -> String {
    for channel in channels {
        if channel.id == channel_id {
            return channel.name.clone();
        }
    }
    String::new()
}

/// 账号 JSON 视图（凭证脱敏）。
fn session_json(channels: &[Channel], session: &AccountSession, now: i64) -> Value {
    let channel_name = channel_name_for(channels, &session.channel_id);
    json!({
        "id": session.id,
        "channel_id": session.channel_id,
        "channel_name": channel_name,
        "name": session.name,
        "provider": session.provider,
        "base_url": session.base_url,
        "chat_path": session.chat_path,
        "api_key": mask_secret(&session.credential.secret_plain()),
        "proxy": session.proxy.as_deref().map(mask_proxy),
        "status": session.runtime_status(now),
        "configured_status": session.status,
        "priority": session.priority,
        "weight": session.weight,
        "max_concurrency": session.max_concurrency,
        "models": session.models,
        "extra_headers": crate::channel::mask_sensitive_headers(&session.extra_headers),
        // 续期令牌只暴露「有没有」与脱敏形态，不回明文
        "has_refresh_token": !session.refresh_secret.is_empty(),
        "refresh_token": if session.refresh_secret.is_empty() {
            String::new()
        } else {
            mask_secret(&crate::account_session::secret::open_secret(
                &session.refresh_secret,
                &crate::account_session::secret::data_dir(),
            ))
        },
        "in_flight": session.in_flight,
        "success_count": session.success_count,
        "error_count": session.error_count,
        "consecutive_errors": session.consecutive_errors,
        "avg_latency_ms": session.avg_latency_ms(),
        "cooldowns": cooldowns_json(session, now),
        "last_error": session.last_error,
        "last_used_at": session.last_used_at,
        "expires_at": session.expires_at,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
    })
}

// ── 列表 ────────────────────────────────────────────────────────────

/// GET /api/account-sessions?channel_id=&search=
pub async fn handle_list_account_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let channel_filter = params.get("channel_id").cloned().unwrap_or_default();
    let search = params
        .get("search")
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let channels = state.channel_store.list();
    let mut sessions = state.account_sessions.all();
    sessions.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.created_at.cmp(&b.created_at))
    });
    let now = chrono::Utc::now().timestamp();
    let mut data: Vec<Value> = Vec::with_capacity(sessions.len());
    for session in &sessions {
        if !channel_filter.is_empty() && session.channel_id != channel_filter {
            continue;
        }
        if !search.is_empty() {
            let channel_name = channel_name_for(&channels, &session.channel_id);
            if !matches_search(session, &search) && !channel_name.to_lowercase().contains(&search) {
                continue;
            }
        }
        data.push(session_json(&channels, session, now));
    }
    let total = data.len();
    Ok(Json(
        json!({ "success": true, "data": data, "total": total }),
    ))
}

// ── 新增 ────────────────────────────────────────────────────────────

/// POST /api/account-sessions
pub async fn handle_create_account_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AccountSessionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    // SSRF 防护：账号级 base_url 同样会被网关主动请求（探活/签到/转发）
    if !body.base_url.trim().is_empty() {
        if let Err(rej) = crate::net_guard::validate_outbound_url(
            &body.base_url,
            config.usage.allow_internal_upstream,
        ) {
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    let channel = ensure_account_channel(&state, body.channel_id.trim())?;
    let mut session = AccountSession::new(channel.id.clone(), String::new());
    session = apply_request(session, &body);
    // 登录态型供应商：凭证形态标记为 session（Cookie/JWT），与 API Key 区分。
    // 显式 credential_kind 入参优先（apply_request 已应用），不覆盖。
    if body.credential_kind.is_none() && registry::session_meta(&session.provider).is_some() {
        session.credential.kind =
            crate::account_session::model::CREDENTIAL_KIND_SESSION.to_string();
    }
    if session.name.is_empty() {
        let count = state.account_sessions.for_channel(&channel.id).len();
        session.name = format!("{}#{}", provider_label(&session.provider), count + 1);
    }
    let created = state.account_sessions.upsert(session).map_err(|e| {
        error_response(
            &format!("Failed to save account session: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "create_account_session",
        &format!("channel={} id={}", created.channel_id, created.id),
        None,
        Some(json!({ "name": created.name, "provider": created.provider })),
    );
    let now = chrono::Utc::now().timestamp();
    let view = session_json(&state.channel_store.list(), &created, now);
    Ok(Json(json!({ "success": true, "data": view })))
}

// ── 更新 ────────────────────────────────────────────────────────────

/// PUT /api/account-sessions/:id
pub async fn handle_update_account_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AccountSessionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    // SSRF 防护（与新增同口径）
    if !body.base_url.trim().is_empty() {
        if let Err(rej) = crate::net_guard::validate_outbound_url(
            &body.base_url,
            config.usage.allow_internal_upstream,
        ) {
            return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
        }
    }
    let Some(prev) = state.account_sessions.get(&id) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    // 凭证 KV 以 channel_id 为键，不允许变更归属渠道
    let wanted_channel = body.channel_id.trim();
    if !wanted_channel.is_empty() && wanted_channel != prev.channel_id.as_str() {
        return Err(error_response(
            "Channel cannot be changed; delete and re-create the account instead",
            StatusCode::BAD_REQUEST,
        ));
    }
    ensure_account_channel(&state, &prev.channel_id)?;
    let mut session = apply_request(prev.clone(), &body);
    // 切换供应商时凭证形态跟随：登录态型标 session，其余回到 api_key
    session.credential.kind = if registry::session_meta(&session.provider).is_some() {
        crate::account_session::model::CREDENTIAL_KIND_SESSION.to_string()
    } else {
        crate::account_session::model::CREDENTIAL_KIND_API_KEY.to_string()
    };
    session.id = prev.id.clone();
    session.channel_id = prev.channel_id.clone();
    session.created_at = prev.created_at;
    // 运行态统计不随表单覆盖
    session.success_count = prev.success_count;
    session.error_count = prev.error_count;
    session.total_latency_ms = prev.total_latency_ms;
    session.last_used_at = prev.last_used_at;
    session.cooldowns = prev.cooldowns;
    let saved = state.account_sessions.upsert(session).map_err(|e| {
        error_response(
            &format!("Failed to update account session: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "update_account_session",
        &format!("id={id}"),
        Some(json!({ "name": prev.name, "status": prev.status })),
        Some(json!({ "name": saved.name, "status": saved.status })),
    );
    let now = chrono::Utc::now().timestamp();
    let view = session_json(&state.channel_store.list(), &saved, now);
    Ok(Json(json!({ "success": true, "data": view })))
}

// ── 部分更新（PATCH）────────────────────────────────────────────────

/// PATCH /api/account-sessions/:id —— 运营型部分更新（名称/状态/优先级/权重/并发）。
///
/// 与 PUT（整表单替换）区分：PATCH 只应用显式给出的字段，不会清空 base_url、
/// models、代理、凭证与到期时间，适合控制台开关与 AI 工具的小操作。
pub async fn handle_patch_account_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AccountSessionPatch>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let Some(prev) = state.account_sessions.get(&id) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    let mut next = prev.clone();
    if let Some(name) = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        next.name = name.to_string();
    }
    if let Some(status) = body
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if status != "enabled" && status != "disabled" {
            return Err(error_response(
                "status must be 'enabled' or 'disabled'",
                StatusCode::BAD_REQUEST,
            ));
        }
        next.status = status.to_string();
    }
    if let Some(priority) = body.priority {
        next.priority = priority;
    }
    if let Some(weight) = body.weight {
        next.weight = weight.max(1);
    }
    if let Some(max_concurrency) = body.max_concurrency {
        next.max_concurrency = max_concurrency;
    }
    let saved = state.account_sessions.upsert(next).map_err(|e| {
        error_response(
            &format!("Failed to patch account session: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "patch_account_session",
        &format!("id={id}"),
        Some(json!({ "status": prev.status, "priority": prev.priority })),
        Some(json!({ "status": saved.status, "priority": saved.priority })),
    );
    let now = chrono::Utc::now().timestamp();
    let view = session_json(&state.channel_store.list(), &saved, now);
    Ok(Json(json!({ "success": true, "data": view })))
}

// ── 删除 ────────────────────────────────────────────────────────────

/// DELETE /api/account-sessions/:id
pub async fn handle_delete_account_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.account_sessions.remove(&id).map_err(|e| {
        error_response(
            &format!("Failed to delete account session: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let Some(removed) = removed else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "delete_account_session",
        &format!("channel={} id={}", removed.channel_id, removed.id),
        Some(json!({ "name": removed.name, "provider": removed.provider })),
        None,
    );
    Ok(Json(json!({ "success": true, "data": null })))
}

// ── 批量导入 ────────────────────────────────────────────────────────

/// POST /api/account-sessions/bulk
pub async fn handle_bulk_import_account_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkImportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let channel = ensure_account_channel(&state, body.channel_id.trim())?;
    let provider = if body.provider.trim().is_empty() {
        registry::DEFAULT_PROVIDER_KIND.to_string()
    } else {
        registry::normalize_kind(&body.provider)
    };
    let proxy = body
        .proxy
        .as_ref()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    let prefix = if body.name_prefix.trim().is_empty() {
        "account".to_string()
    } else {
        body.name_prefix.trim().to_string()
    };
    let base_count = state.account_sessions.for_channel(&channel.id).len();
    let mut created: Vec<Value> = Vec::new();
    let mut skipped = 0usize;
    for line in body.credentials.lines() {
        let raw = line.trim();
        if raw.is_empty() || raw.starts_with('#') {
            continue;
        }
        if created.len() >= MAX_BULK_IMPORT {
            skipped += 1;
            continue;
        }
        let mut parts = raw.splitn(2, ',');
        let first = parts.next().unwrap_or_default().trim();
        let second = parts.next().map(str::trim);
        let (name, secret) = match second {
            Some(secret) if !secret.is_empty() => (first.to_string(), secret.to_string()),
            _ => (String::new(), first.to_string()),
        };
        if secret.is_empty() {
            skipped += 1;
            continue;
        }
        let mut session = AccountSession::new(channel.id.clone(), String::new());
        session.provider = provider.clone();
        session.base_url = body.base_url.trim().to_string();
        session.proxy = proxy.clone();
        session.priority = body.priority;
        session.weight = body.weight.max(1);
        session.max_concurrency = body.max_concurrency;
        session.credential.secret = secret;
        for model in &body.models {
            let trimmed = model.trim();
            if !trimmed.is_empty() {
                session.models.push(trimmed.to_string());
            }
        }
        session.name = if name.is_empty() {
            format!("{prefix}{}", base_count + created.len() + 1)
        } else {
            name
        };
        match state.account_sessions.upsert(session) {
            Ok(saved) => created.push(json!({ "id": saved.id, "name": saved.name })),
            Err(e) => {
                skipped += 1;
                tracing::warn!("bulk import account session failed: {e}");
            }
        }
    }
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "bulk_import_account_sessions",
        &format!("channel={}", channel.id),
        None,
        Some(json!({ "created": created.len(), "skipped": skipped })),
    );
    Ok(Json(json!({
        "success": true,
        "data": { "created": created.len(), "skipped": skipped, "items": created },
    })))
}

// ── 探活 ────────────────────────────────────────────────────────────

/// 单号探活结果。
pub struct ProbeOutcome {
    pub ok: bool,
    pub latency_ms: u64,
    pub model: String,
    pub message: String,
}

/// 探活使用的模型：账号白名单 → 渠道 models / 发现快照，最后统一走渠道映射
///（与数据面「上游真名」口径一致：映射对它自己产出的名字是恒等变换，安全）。
fn resolve_probe_model(state: &AppState, session: &AccountSession) -> Option<String> {
    let channel = state.channel_store.get(&session.channel_id);
    let raw = match session.models.first() {
        Some(model) => model.clone(),
        None => channel.as_ref()?.effective_models().first()?.clone(),
    };
    let mapped = channel.and_then(|ch| ch.resolve_channel_mapping(&raw));
    Some(mapped.unwrap_or(raw))
}

/// 走真实推理链路探活（复用账号的出口 Client 与原生 Bridge）。
pub async fn probe_session(state: &AppState, session: &AccountSession) -> ProbeOutcome {
    let Some(model) = resolve_probe_model(state, session) else {
        return ProbeOutcome {
            ok: false,
            latency_ms: 0,
            model: String::new(),
            message: "该账号与渠道都未配置模型，请先声明渠道 models".to_string(),
        };
    };
    let bridge = match adapter::build_bridge(session, &state.account_sessions) {
        Ok(bridge) => bridge,
        Err(e) => {
            return ProbeOutcome {
                ok: false,
                latency_ms: 0,
                model,
                message: e.to_string(),
            }
        }
    };
    let request_id = format!("account-probe-{}", uuid::Uuid::new_v4());
    let ctx = BridgeContext::new(request_id, model.clone())
        .with_deadline(Duration::from_secs(PROBE_TIMEOUT_SECS));
    let req = ChatFormat {
        model: model.clone(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: Some("ping".to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }],
        tools: None,
        max_tokens: Some(1),
        temperature: Some(0.0),
        top_p: None,
        stream: false,
        top_k: None,
        stop: None,
        tool_choice: None,
        reasoning_effort: None,
        web_search_options: None,
        extra: None,
    };
    let started = Instant::now();
    // 客户端超时 300s 对探活太久：用短预算收敛（渠道级探活最多试 3 个号）
    let budget = Duration::from_secs(PROBE_TIMEOUT_SECS);
    let outcome = tokio::time::timeout(budget, bridge.chat(&req, &ctx)).await;
    match outcome {
        Ok(Ok(response)) => {
            let latency_ms = started.elapsed().as_millis() as u64;
            // 探活成功 = 人工确认已恢复：清该模型冷却并记一次成功（不改状态）
            state
                .account_sessions
                .record_success(&session.id, &model, latency_ms);
            ProbeOutcome {
                ok: true,
                latency_ms,
                model,
                message: format!("OK (finish={:?})", response.finish_reason),
            }
        }
        Ok(Err(e)) => ProbeOutcome {
            ok: false,
            latency_ms: started.elapsed().as_millis() as u64,
            model,
            message: e.to_string(),
        },
        Err(_) => ProbeOutcome {
            ok: false,
            latency_ms: started.elapsed().as_millis() as u64,
            model,
            message: format!("timed out after {PROBE_TIMEOUT_SECS}s"),
        },
    }
}

/// POST /api/account-sessions/:id/test
pub async fn handle_test_account_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Some(session) = state.account_sessions.get(&id) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    let outcome = probe_session(&state, &session).await;
    Ok(Json(json!({
        "success": true,
        "data": {
            "success": outcome.ok,
            "latency_ms": outcome.latency_ms,
            "model": outcome.model,
            "message": outcome.message,
        },
    })))
}

/// GET /api/account-sessions/:id/balance —— 账号级余额/额度查询。
///
/// 按 provider 分派（qoder / autoclaw / 小浣熊 / zcode），归一形状见
/// `account_session::balance`。查不到总量时 `available` 为 null（≠ 0）。
pub async fn handle_account_balance(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Some(session) = state.account_sessions.get(&id) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    if !crate::account_session::balance::supports_balance(&session.provider) {
        return Err(error_response(
            "该供应商暂不支持余额查询",
            StatusCode::BAD_REQUEST,
        ));
    }
    match crate::account_session::balance::query_balance(&state.http_client, &session).await {
        Ok(balance) => Ok(Json(json!({
            "success": true,
            "data": {
                "available": balance.available,
                "unit": balance.unit,
                "wallets": balance.wallets,
                "subscription": balance.subscription,
            },
        }))),
        Err(message) => Ok(Json(json!({
            "success": false,
            "error": message,
        }))),
    }
}

/// POST /api/account-sessions/:id/refresh 的请求体。
#[derive(Debug, serde::Deserialize)]
pub struct RefreshBody {
    /// true = 无条件刷新；false（默认）= 仅临期才刷，否则 skip。
    #[serde(default)]
    pub force: bool,
}

/// POST /api/account-sessions/:id/refresh —— 手动刷新账号 token。
///
/// 进程内直调 `account_session::refresh::refresh_one`
///（qoder/autoclaw/trae/codearts/officeace），
/// 失败不改写凭据；`force=false` 时未临期直接返回 skip。
pub async fn handle_account_refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RefreshBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Some(session) = state.account_sessions.get(&id) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    if !crate::account_session::refresh::supports_refresh(&session.provider) {
        return Err(error_response(
            "该供应商暂不支持 token 刷新",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !body.force {
        let now = chrono::Utc::now().timestamp();
        if !crate::account_session::refresh::needs_refresh(&session, now) {
            return Ok(Json(json!({
                "success": true,
                "data": { "outcome": "skip", "message": "token 未临期（force=true 可强制刷新）" },
            })));
        }
    }
    match crate::account_session::refresh::refresh_one(
        &state.account_sessions,
        &state.http_client,
        &session,
    )
    .await
    {
        crate::account_session::refresh::RefreshOutcome::Refreshed(message) => Ok(Json(json!({
            "success": true,
            "data": { "outcome": "refreshed", "message": message },
        }))),
        crate::account_session::refresh::RefreshOutcome::Stale => Ok(Json(json!({
            "success": true,
            "data": { "outcome": "stale", "message": "凭据已被其他路径更新，未覆盖" },
        }))),
        crate::account_session::refresh::RefreshOutcome::Invalid(message) => Ok(Json(json!({
            "success": false,
            "error": format!("refresh token 失效：{message}"),
        }))),
        crate::account_session::refresh::RefreshOutcome::Failed(message) => Ok(Json(json!({
            "success": false,
            "error": message,
        }))),
    }
}

/// 账号型渠道的渠道级探活（供 `handle_test_channel` 复用）。
pub async fn probe_channel(state: &AppState, channel: &Channel) -> ChannelTestResult {
    let started = Instant::now();
    let sessions: Vec<AccountSession> = state
        .account_sessions
        .for_channel(&channel.id)
        .into_iter()
        .filter(|s| s.is_enabled())
        .take(CHANNEL_PROBE_ACCOUNTS)
        .collect();
    let mut message = "该渠道下没有启用的账号".to_string();
    for session in &sessions {
        let outcome = probe_session(state, session).await;
        if outcome.ok {
            return ChannelTestResult {
                success: true,
                message: format!("账号 {} 可用: {}", session.name, outcome.message),
                latency_ms: started.elapsed().as_millis() as u64,
            };
        }
        message = format!("账号 {} 不可用: {}", session.name, outcome.message);
    }
    ChannelTestResult {
        success: false,
        message,
        latency_ms: started.elapsed().as_millis() as u64,
    }
}

// ── 清冷却 ──────────────────────────────────────────────────────────

/// POST /api/account-sessions/:id/reset-cooldown
pub async fn handle_reset_account_session_cooldowns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if !state.account_sessions.reset_cooldowns(&id) {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    }
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "reset_account_session_cooldown",
        &format!("id={id}"),
        None,
        None,
    );
    Ok(Json(json!({ "success": true, "data": null })))
}

// ── provider 目录 / 统计 ────────────────────────────────────────────

/// GET /api/account-sessions/providers
pub async fn handle_list_account_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut data: Vec<Value> = Vec::new();
    for spec in registry::all() {
        let session = registry::session_meta(spec.kind);
        data.push(json!({
            "kind": spec.kind,
            "label": spec.label,
            "protocol": spec.protocol.as_str(),
            "default_base_url": spec.default_base_url,
            // 登录态型供应商：鉴权方式与凭证粘贴提示（前端据此改变输入提示）
            "auth": session.map(|m| m.auth.as_str()),
            "credential_hint": session.map(|m| m.credential_hint),
            // 登录态型供应商的默认对话路径与私有协议标记
            "chat_path": session.map(|m| m.chat_path).filter(|p| !p.is_empty()),
            "openai_compatible": session.map(|m| m.openai_compatible),
            // 上游非 OpenAI 协议、但 AIGX 已内置专用适配（可直接转发）
            "native_supported": session
                .map(|m| !m.openai_compatible && crate::bridge::session::supports(spec.kind)),
            // 无默认地址 = 必须由账号自己填写上游地址
            "requires_base_url": spec.default_base_url.is_empty(),
        }));
    }
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 账号路由（轮换策略）请求体：缺省字段保持原值。
#[derive(Debug, Deserialize)]
pub struct RotationPolicyRequest {
    pub base_cooldown_secs: Option<u64>,
    pub max_cooldown_secs: Option<u64>,
    pub auth_cooldown_secs: Option<u64>,
    pub max_attempts: Option<usize>,
}

/// GET /api/account-sessions/rotation —— 当前账号路由参数。
pub async fn handle_get_rotation_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let policy = state.account_sessions.policy();
    Ok(Json(json!({
        "success": true,
        "data": {
            "base_cooldown_secs": policy.base_cooldown_secs,
            "max_cooldown_secs": policy.max_cooldown_secs,
            "auth_cooldown_secs": policy.auth_cooldown_secs,
            "max_attempts": policy.max_attempts,
        }
    })))
}

/// PUT /api/account-sessions/rotation —— 保存账号路由参数（归一化后立即生效）。
pub async fn handle_set_rotation_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RotationPolicyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let current = state.account_sessions.policy();
    let next = RotationPolicy {
        base_cooldown_secs: body
            .base_cooldown_secs
            .unwrap_or(current.base_cooldown_secs),
        max_cooldown_secs: body.max_cooldown_secs.unwrap_or(current.max_cooldown_secs),
        auth_cooldown_secs: body
            .auth_cooldown_secs
            .unwrap_or(current.auth_cooldown_secs),
        max_attempts: body.max_attempts.unwrap_or(current.max_attempts),
    };
    match state.account_sessions.set_policy(next) {
        Ok(saved) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "update_account_rotation",
                "account-session",
                Some(json!({
                    "base_cooldown_secs": current.base_cooldown_secs,
                    "max_cooldown_secs": current.max_cooldown_secs,
                    "auth_cooldown_secs": current.auth_cooldown_secs,
                    "max_attempts": current.max_attempts,
                })),
                Some(json!({
                    "base_cooldown_secs": saved.base_cooldown_secs,
                    "max_cooldown_secs": saved.max_cooldown_secs,
                    "auth_cooldown_secs": saved.auth_cooldown_secs,
                    "max_attempts": saved.max_attempts,
                })),
            );
            Ok(Json(json!({
                "success": true,
                "data": {
                    "base_cooldown_secs": saved.base_cooldown_secs,
                    "max_cooldown_secs": saved.max_cooldown_secs,
                    "auth_cooldown_secs": saved.auth_cooldown_secs,
                    "max_attempts": saved.max_attempts,
                }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save rotation policy: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Default)]
struct SessionStats {
    total: u64,
    enabled: u64,
    cooling: u64,
    in_flight: u64,
    success: u64,
    error: u64,
}

/// GET /api/account-sessions/stats —— 按渠道聚合（总数/启用/冷却/并发/成败）。
pub async fn handle_account_session_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    let mut per_channel: HashMap<String, SessionStats> = HashMap::new();
    let sessions = state.account_sessions.all();
    for session in &sessions {
        let entry = per_channel.entry(session.channel_id.clone()).or_default();
        entry.total += 1;
        if session.runtime_status(now) == "enabled" {
            entry.enabled += 1;
        }
        if session.cooldowns.values().any(|until| *until > now) {
            entry.cooling += 1;
        }
        entry.in_flight += session.in_flight as u64;
        entry.success += session.success_count;
        entry.error += session.error_count;
    }
    let channels = state.channel_store.list();
    let mut data: Vec<Value> = Vec::new();
    for (channel_id, stats) in &per_channel {
        let mut channel_name = String::new();
        for channel in &channels {
            if channel.id == *channel_id {
                channel_name = channel.name.clone();
                break;
            }
        }
        data.push(json!({
            "channel_id": channel_id,
            "channel_name": channel_name,
            "total": stats.total,
            "enabled": stats.enabled,
            "cooling": stats.cooling,
            "in_flight": stats.in_flight,
            "success": stats.success,
            "error": stats.error,
        }));
    }
    Ok(Json(json!({
        "success": true,
        "data": { "channels": data, "total": sessions.len() },
    })))
}

// ── 模型目录发现 ────────────────────────────────────────────────────

/// POST /api/account-sessions/catalog/sync 请求体。
#[derive(Debug, Deserialize)]
pub struct CatalogSyncRequest {
    /// 指定账号 id（省略 = 按 provider 同步该家全部账号）。
    #[serde(default)]
    pub account_id: Option<String>,
    /// 按供应商同步时的供应商 kind。
    #[serde(default)]
    pub provider: Option<String>,
    /// 跳过缓存强制回源。
    #[serde(default)]
    pub force: bool,
    /// 是否把结果写回账号的模型白名单。
    #[serde(default)]
    pub apply: bool,
    /// 写回时是否覆盖（false = 只追加目录新增的模型）。
    #[serde(default)]
    pub replace: bool,
}

/// POST /api/account-sessions/catalog/sync —— 同步上游模型目录。
///
/// 单个账号失败不影响其他账号：逐账号返回结果，前端展示成表格。
pub async fn handle_sync_account_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CatalogSyncRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let sessions = state.account_sessions.clone();
    let mut results: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    // 目标账号集合：指定账号 id，或按 provider 展开
    let targets: Vec<crate::account_session::model::AccountSession> = match &body.account_id {
        Some(id) if !id.is_empty() => match sessions.get(id) {
            Some(s) => vec![s],
            None => {
                return Err(error_response(
                    &format!("账号 {id} 不存在"),
                    StatusCode::NOT_FOUND,
                ))
            }
        },
        _ => {
            let provider = body.provider.clone().unwrap_or_default();
            if provider.is_empty() {
                return Err(error_response(
                    "需要指定 account_id 或 provider",
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !crate::account_session::catalog::supports(&provider) {
                return Err(error_response(
                    &format!("供应商 {provider} 暂不支持模型目录发现"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            sessions
                .all()
                .into_iter()
                .filter(|s| s.provider == provider)
                .collect()
        }
    };

    if targets.is_empty() {
        return Err(error_response("没有匹配的账号", StatusCode::NOT_FOUND));
    }

    for session in targets {
        let name = if session.name.is_empty() {
            session.id.clone()
        } else {
            session.name.clone()
        };
        match crate::account_session::catalog::sync_account_catalog(&sessions, &session, body.force)
            .await
        {
            Ok(snapshot) => {
                let mut applied = false;
                let mut apply_error: Option<String> = None;
                if body.apply {
                    let ids: Vec<String> = snapshot.models.iter().map(|m| m.id.clone()).collect();
                    match crate::account_session::catalog::apply_catalog_to_session(
                        &sessions,
                        &session.id,
                        &ids,
                        body.replace,
                    ) {
                        Ok(()) => applied = true,
                        Err(e) => apply_error = Some(e),
                    }
                }
                results.push(json!({
                    "account_id": session.id,
                    "name": name,
                    "provider": session.provider,
                    "source": snapshot.source,
                    "fetched_at": snapshot.fetched_at,
                    "count": snapshot.models.len(),
                    "models": snapshot.models,
                    "applied": applied,
                    "apply_error": apply_error,
                }));
            }
            Err(e) => {
                errors.push(json!({
                    "account_id": session.id,
                    "name": name,
                    "provider": session.provider,
                    "error": e,
                }));
            }
        }
    }

    Ok(Json(json!({
        "success": true,
        "data": {
            "results": results,
            "errors": errors,
            "ok_count": results.len(),
            "error_count": errors.len(),
        }
    })))
}

/// GET /api/account-sessions/catalog/providers —— 支持目录发现的供应商清单。
pub async fn handle_catalog_providers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let providers: Vec<Value> = crate::account_session::catalog::supported_providers()
        .into_iter()
        .map(|kind| {
            let label = registry::find(kind)
                .map(|s| s.label.to_string())
                .unwrap_or_else(|| kind.to_string());
            json!({ "kind": kind, "label": label })
        })
        .collect();
    Ok(Json(
        json!({ "success": true, "data": { "providers": providers } }),
    ))
}

// ── 从本机客户端导入账号（cc-switch）────────────────────────────────

/// GET /api/account-sessions/import/sources —— 扫描本机可导入的供应商。
///
/// 永不失败：错误在响应的 `available` / `reason` 里。
pub async fn handle_scan_import_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // 本地已有的渠道名（同名跳过判据）
    let existing: std::collections::HashSet<String> = state
        .channel_store
        .list()
        .into_iter()
        .map(|c| c.name)
        .collect();
    let scan = crate::account_session::import::scan(&existing);
    Ok(Json(json!({
        "success": true,
        "data": {
            "available": scan.available,
            "path": scan.path,
            "reason": scan.reason,
            "total": scan.total,
            "providers": scan.providers,
        }
    })))
}

/// POST /api/account-sessions/import —— 执行导入（勾选的条目 → 渠道）。
#[derive(Debug, Deserialize)]
pub struct ImportRunRequest {
    /// 待导入的条目。
    pub entries: Vec<crate::account_session::import::ImportEntry>,
    /// 导入后的渠道状态（缺省 enabled）。
    #[serde(default)]
    pub status: Option<String>,
}

/// POST /api/account-sessions/import —— 把勾选的条目导入为渠道。
pub async fn handle_run_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImportRunRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.entries.is_empty() {
        return Err(error_response(
            "没有选择要导入的条目",
            StatusCode::BAD_REQUEST,
        ));
    }

    let mut created: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for entry in body.entries {
        let base_url = entry.base_url.trim();
        let api_key = entry.api_key.trim();
        if base_url.is_empty() || api_key.is_empty() {
            errors.push(json!({
                "id": entry.id,
                "name": entry.name,
                "error": "端点或凭证缺失，已跳过",
            }));
            continue;
        }
        // 协议 → 渠道类型
        let channel_type = match entry.protocol.as_str() {
            "anthropic" => "anthropic",
            "openai_compatible" => "openai_compatible",
            other => {
                errors.push(json!({
                    "id": entry.id,
                    "name": entry.name,
                    "error": format!("不支持的协议 {other}"),
                }));
                continue;
            }
        };

        let model_mapping: std::collections::HashMap<String, String> = entry
            .model_mappings
            .iter()
            .filter_map(|m| {
                let alias = m.get("alias")?.as_str()?.trim().to_string();
                let target = m.get("target")?.as_str()?.trim().to_string();
                if alias.is_empty() || target.is_empty() {
                    return None;
                }
                Some((alias, target))
            })
            .collect();

        let request = super::channels::ChannelRequest {
            name: entry.name.clone(),
            channel_type: channel_type.to_string(),
            base_url: base_url.to_string(),
            api_key: Some(api_key.to_string()),
            priority: 0,
            weight: 1,
            status: body.status.clone().unwrap_or_else(|| "enabled".to_string()),
            models: entry.models.clone(),
            account_id: String::new(),
            model_mapping,
            cost_pricing: Default::default(),
            quirks: None,
            balance_scheme: None,
        };
        let channel = request.to_channel(uuid::Uuid::new_v4().to_string());
        match state.channel_store.add(channel) {
            Ok(ch) => created.push(json!({
                "id": ch.id,
                "name": ch.name,
                "base_url": ch.base_url,
                "models": ch.models,
            })),
            Err(e) => errors.push(json!({
                "id": entry.id,
                "name": entry.name,
                "error": format!("创建渠道失败：{e}"),
            })),
        }
    }

    Ok(Json(json!({
        "success": true,
        "data": {
            "created": created,
            "errors": errors,
            "created_count": created.len(),
            "error_count": errors.len(),
        }
    })))
}

/// POST /api/account-sessions/import/agent2api —— 从 Agent2API 账号库导入到账号池。
///
/// 输入二选一：
/// - `path`：A2A 的 SQLite 库路径（服务器本地文件，只读打开；A2A 可同时在跑）
/// - `records`：A2A `accounts.data` 的 JSON 数组（手工导出的扁平记录）
///
/// 幂等键是 A2A 记录 id：重复导入命中同一条账号并更新凭证字段，
/// 不会产生副本（「AIGX 账号池跟着 A2A 账号更新」的落地路径）。
#[derive(Debug, Deserialize)]
pub struct ImportAgent2ApiRequest {
    /// 目标账号型渠道 id。
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// A2A SQLite 库路径（与 records 二选一）。
    #[serde(default)]
    pub path: Option<String>,
    /// A2A 账号记录 JSON 数组（与 path 二选一）。
    #[serde(default)]
    pub records: Option<Vec<Value>>,
}

pub async fn handle_import_agent2api(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImportAgent2ApiRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    // 目标渠道必须是账号型渠道
    let channel = state
        .channel_store
        .get(&body.channel_id)
        .ok_or_else(|| error_response("目标渠道不存在", StatusCode::NOT_FOUND))?;
    if !matches!(channel.channel_type, crate::channel::ChannelType::Account) {
        return Err(error_response(
            "目标渠道不是账号型渠道（channel_type 须为 account）",
            StatusCode::BAD_REQUEST,
        ));
    }

    // 两种输入之一
    let (records, source) = match (&body.path, &body.records) {
        (Some(path), None) => {
            #[cfg(feature = "sqlite-kv")]
            {
                let path = std::path::PathBuf::from(path.trim());
                let records = crate::account_session::import::read_agent2api_db(&path)
                    .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
                (records, path.to_string_lossy().to_string())
            }
            #[cfg(not(feature = "sqlite-kv"))]
            {
                let _ = path;
                return Err(error_response(
                    "当前构建未启用 SQLite 后端，无法用 path 读取；请改用 records 传入 JSON",
                    StatusCode::BAD_REQUEST,
                ));
            }
        }
        (None, Some(records)) => (records.clone(), "records".to_string()),
        _ => {
            return Err(error_response(
                "path 与 records 必须二选一（且只给一个）",
                StatusCode::BAD_REQUEST,
            ));
        }
    };

    let mut report = crate::account_session::import::Agent2ApiImportReport {
        source,
        ..Default::default()
    };
    for record in &records {
        let Some(draft) =
            crate::account_session::import::map_agent2api_record(&body.channel_id, record)
        else {
            report.skipped += 1;
            continue;
        };
        let exists = state.account_sessions.get(&draft.id).is_some();
        if let Err(e) = state.account_sessions.upsert(draft) {
            report.errors.push(format!("落盘失败：{e}"));
            continue;
        }
        if exists {
            report.updated += 1;
        } else {
            report.imported += 1;
        }
    }

    Ok(Json(json!({
        "success": true,
        "data": report,
    })))
}

// ── 导出 / 导入（JSON 迁移闭环）────────────────────────────────────

/// GET /api/account-sessions/export —— 导出全部账号（**含明文凭证**）。
///
/// 用途：迁移/备份。仅管理员可调（管理面统一 verify_admin）。
/// 凭证在存储层是 enc1: 加密形态，导出时解密为明文——导出文件必须当
/// 密码本保管，这与 Agent2API 的导出语义一致。
pub async fn handle_export_account_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let channels = state.channel_store.list();
    let data_dir = crate::account_session::secret::data_dir();
    let accounts: Vec<Value> = state
        .account_sessions
        .all()
        .iter()
        .map(|s| {
            let channel_name = channels
                .iter()
                .find(|c| c.id == s.channel_id)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            // 具名凭证逐名解密回明文（多层凭证家庭：Cookie+JWT 等）
            let fields: serde_json::Map<String, Value> = s
                .credential
                .field_names()
                .iter()
                .map(|name| ((*name).to_string(), json!(s.credential.field_plain(name))))
                .collect();
            let refresh_token = if s.refresh_secret.is_empty() {
                String::new()
            } else {
                crate::account_session::secret::open_secret(&s.refresh_secret, &data_dir)
            };
            json!({
                "name": s.name,
                "provider": s.provider,
                "channel_id": s.channel_id,
                "channel_name": channel_name,
                "base_url": s.base_url,
                "chat_path": s.chat_path,
                "proxy": s.proxy,
                "status": s.status,
                "priority": s.priority,
                "weight": s.weight,
                "max_concurrency": s.max_concurrency,
                "models": s.models,
                "extra_headers": s.extra_headers,
                "expires_at": s.expires_at,
                "credential_kind": s.credential.kind,
                "api_key": s.credential.secret_plain(),
                "credential_fields": fields,
                "refresh_token": refresh_token,
            })
        })
        .collect();
    let now = chrono::Utc::now().timestamp();
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "export_account_sessions",
        &format!("count={}", accounts.len()),
        None,
        None,
    );
    Ok(Json(json!({
        "success": true,
        "data": {
            "version": 1,
            "exported_at": now,
            "accounts": accounts,
        }
    })))
}

/// POST /api/account-sessions/import/json —— 导入导出文件中的账号。
///
/// 落点解析（逐条）：body.channel_id（统一落点）> entry.channel_id（本实例
/// 存在且为账号型渠道）> entry.channel_name（匹配账号型渠道名）。
/// 同渠道同名已存在 → skip（幂等，不覆盖在线凭证）。
pub async fn handle_import_account_sessions_json(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let fallback_channel = body
        .get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let entries = body
        .get("entries")
        .or_else(|| body.get("accounts"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if entries.is_empty() {
        return Err(error_response(
            "entries 为空：请提供 accounts 数组",
            StatusCode::BAD_REQUEST,
        ));
    }
    if entries.len() > 500 {
        return Err(error_response(
            "单次最多导入 500 个账号",
            StatusCode::BAD_REQUEST,
        ));
    }

    let mut created = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for entry in &entries {
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        // 目标渠道解析：统一落点 > entry 内 channel_id > channel_name 匹配
        let entry_channel_id = entry
            .get("channel_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let entry_channel_name = entry
            .get("channel_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let channel_id = if !fallback_channel.is_empty() {
            fallback_channel.clone()
        } else if !entry_channel_id.is_empty() {
            entry_channel_id
        } else if !entry_channel_name.is_empty() {
            state
                .channel_store
                .list()
                .iter()
                .find(|c| {
                    matches!(c.channel_type, crate::channel::ChannelType::Account)
                        && c.name == entry_channel_name
                })
                .map(|c| c.id.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        if channel_id.is_empty() {
            skipped += 1;
            continue;
        }
        let channel = match ensure_account_channel(&state, &channel_id) {
            Ok(c) => c,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        // 幂等：同渠道同名跳过
        let dup = state
            .account_sessions
            .for_channel(&channel.id)
            .iter()
            .any(|s| !name.is_empty() && s.name == name);
        if dup {
            skipped += 1;
            continue;
        }
        // SSRF 防护与 create 一致
        let base_url = entry
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !base_url.is_empty() {
            if let Err(rej) = crate::net_guard::validate_outbound_url(
                &base_url,
                config.usage.allow_internal_upstream,
            ) {
                errors.push(format!("{name}: {}", rej.message()));
                continue;
            }
        }
        // 复用 create 的字段归一化：构造 AccountSessionRequest 走 apply_request
        let req = AccountSessionRequest {
            channel_id: channel.id.clone(),
            name: name.clone(),
            provider: entry
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            base_url,
            chat_path: entry
                .get("chat_path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            api_key: entry
                .get("api_key")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            refresh_token: entry
                .get("refresh_token")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            credential_kind: entry
                .get("credential_kind")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            proxy: entry
                .get("proxy")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            status: entry
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("enabled")
                .to_string(),
            priority: entry.get("priority").and_then(|v| v.as_i64()),
            weight: entry
                .get("weight")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            max_concurrency: entry
                .get("max_concurrency")
                .and_then(|v| v.as_u64())
                .map(|v| v as u32),
            models: entry
                .get("models")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|m| m.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default(),
            extra_headers: entry.get("extra_headers").and_then(|v| {
                v.as_object().map(|obj| {
                    obj.iter()
                        .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
            }),
            expires_at: entry.get("expires_at").and_then(|v| v.as_i64()),
        };
        let mut session = AccountSession::new(channel.id.clone(), String::new());
        session = apply_request(session, &req);
        // 具名凭证逐名落库（set_field 内部按口径编码）
        if let Some(fields) = entry.get("credential_fields").and_then(|v| v.as_object()) {
            for (fname, fval) in fields {
                if let Some(text) = fval.as_str() {
                    session.credential.set_field(fname, text);
                }
            }
        }
        if session.name.is_empty() {
            let count = state.account_sessions.for_channel(&channel.id).len();
            session.name = format!("{}#{}", provider_label(&session.provider), count + 1);
        }
        match state.account_sessions.upsert(session) {
            Ok(_) => created += 1,
            Err(e) => errors.push(format!("{name}: 落盘失败：{e}")),
        }
    }

    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "import_account_sessions_json",
        &format!(
            "created={created} skipped={skipped} errors={}",
            errors.len()
        ),
        None,
        None,
    );
    Ok(Json(json!({
        "success": true,
        "data": { "created": created, "skipped": skipped, "errors": errors },
    })))
}
