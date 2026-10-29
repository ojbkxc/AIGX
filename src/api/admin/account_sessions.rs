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
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub proxy: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub weight: u32,
    #[serde(default)]
    pub max_concurrency: u32,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<i64>,
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
    match &body.api_key {
        None => {}
        Some(secret) => session.credential.secret = secret.clone(),
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
    session.priority = body.priority;
    if body.weight > 0 {
        session.weight = body.weight;
    }
    session.max_concurrency = body.max_concurrency;
    session.expires_at = body.expires_at.filter(|at| *at > 0);
    session.models = Vec::new();
    for model in &body.models {
        let name = model.trim();
        if !name.is_empty() {
            session.models.push(name.to_string());
        }
    }
    session
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
        "api_key": mask_secret(&session.credential.secret_plain()),
        "proxy": session.proxy.as_deref().map(mask_proxy),
        "status": session.runtime_status(now),
        "configured_status": session.status,
        "priority": session.priority,
        "weight": session.weight,
        "max_concurrency": session.max_concurrency,
        "models": session.models,
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
    let _config = verify_admin(&state, &headers).await?;
    let channel = ensure_account_channel(&state, body.channel_id.trim())?;
    let mut session = AccountSession::new(channel.id.clone(), String::new());
    session = apply_request(session, &body);
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
    let _config = verify_admin(&state, &headers).await?;
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
        Ok(Ok(response)) => ProbeOutcome {
            ok: true,
            latency_ms: started.elapsed().as_millis() as u64,
            model,
            message: format!("OK (finish={:?})", response.finish_reason),
        },
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
        data.push(json!({
            "kind": spec.kind,
            "label": spec.label,
            "protocol": spec.protocol.as_str(),
            "default_base_url": spec.default_base_url,
        }));
    }
    Ok(Json(json!({ "success": true, "data": data })))
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
