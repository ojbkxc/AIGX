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

/// 批量导入共享 base_url 的 SSRF 校验（空串 = 沿用 provider 默认地址，放行）。
///
/// 批量导入无法逐条给不同地址，因此校验一次；失败时**整批跳过并计数**，
/// 不返回 400 —— 与「单条失败不拖垮整批」的导入语义一致。
fn bulk_base_url_allowed(base_url: &str, allow_internal: bool) -> bool {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return true;
    }
    crate::net_guard::validate_outbound_url(trimmed, allow_internal).is_ok()
}

/// 校验 Agent2API 导入的 SQLite 路径（S6）。
///
/// 只允许读取运行数据目录或系统临时目录下的文件：
/// 1. 先拒绝含 `..` 的相对组件（即使目标存在也不放行）；
/// 2. `canonicalize` 解析真实路径（消解符号链接）；
/// 3. 结果必须落在任一允许根目录之内，否则拒绝。
#[cfg(feature = "sqlite-kv")]
fn validate_import_db_path(raw: &str) -> Result<std::path::PathBuf, String> {
    use std::path::{Component, Path, PathBuf};

    let raw = raw.trim();
    if raw.is_empty() {
        return Err("导入路径不能为空".to_string());
    }
    let requested = Path::new(raw);
    if requested
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err("导入路径不允许包含 `..`（防目录穿越）".to_string());
    }
    let canonical = std::fs::canonicalize(requested)
        .map_err(|e| format!("导入路径不可访问（{}）：{e}", requested.display()))?;

    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::fs::canonicalize(crate::account_session::secret::data_dir()) {
        roots.push(p);
    }
    if let Ok(p) = std::fs::canonicalize(std::env::temp_dir()) {
        roots.push(p);
    }
    if roots.iter().any(|root| canonical.starts_with(root)) {
        Ok(canonical)
    } else {
        Err("导入路径必须位于数据目录或系统临时目录下".to_string())
    }
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
    /// 余额自动查询间隔（秒）。三态：缺省 = 保持原值；0 = 关闭；>0 = 定频查询。
    #[serde(default)]
    pub usage_query_interval_secs: Option<u64>,
    /// 账号限制规则（余额 / Token 两类，见 `AccountLimiterRule`）。
    ///
    /// 三态：缺省 = 保持原值（PUT 只提交部分字段时不清规则）；
    /// 空数组 = 清空全部规则。校验失败整单拒绝（400）。
    #[serde(default)]
    pub limiters: Option<Vec<crate::account_session::model::AccountLimiterRule>>,
    /// 自动创建/复用承载渠道（渠道并入账号池添加流）。
    ///
    /// 语义：`channel_id` 为空且给了 `auto_channel` 时，服务端**原子地**
    /// 建（或幂等复用）一个账号型渠道，再把账号挂上去——替代前端此前的
    /// 「先 POST /api/channels 再 POST /api/account-sessions」两段式。
    /// `channel_id` 仍优先；`auto_channel` 缺省 = 现行为不变。
    #[serde(default)]
    pub auto_channel: Option<AutoChannelSpec>,
}

/// 「预置 API」一键建号的渠道规格（随 `AccountSessionRequest.auto_channel`）。
#[derive(Debug, Clone, Deserialize)]
pub struct AutoChannelSpec {
    /// 渠道名（空/缺省 = `{provider 展示名} 账号池`，与前端旧兜底同名）。
    #[serde(default)]
    pub name: Option<String>,
    /// 渠道级上游地址（账号型渠道通常留空，地址由账号携带）。
    #[serde(default)]
    pub base_url: Option<String>,
    /// 预置供应商卡片写入的上游特判（urlSuffix / 静态头 / anthropicToolType /
    /// clientEmulation）。
    #[serde(default)]
    pub quirks: Option<crate::channel::ChannelQuirks>,
    /// 余额查询方案（见 `channel::balance::SCHEMES`）。
    #[serde(default)]
    pub balance_scheme: Option<String>,
    /// 渠道级模型映射（请求名 → 上游真名）。
    #[serde(default)]
    pub model_mapping: HashMap<String, String>,
    /// 渠道级成本价。
    #[serde(default)]
    pub cost_pricing: HashMap<String, crate::pricing::ModelPrice>,
}

fn default_true() -> bool {
    true
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
    /// 账号限制规则（余额 / Token 两类）。三态：缺省 = 保持原值；
    /// 空数组 = 清空。整块替换（对齐前端「规则列表一次全量提交」）。
    #[serde(default)]
    pub limiters: Option<Vec<crate::account_session::model::AccountLimiterRule>>,
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

/// 校验请求里的限制规则（条数上限 + 逐条白名单 / 阈值 / 周期范围）。
///
/// 任何一条非法就整单拒绝：半保存的限制器（部分生效部分忽略）比不保存
/// 更难排查。校验逻辑在 `account_session::model::validate_limiter_rules`。
fn validate_limiters(
    rules: &[crate::account_session::model::AccountLimiterRule],
) -> Result<(), (StatusCode, Json<Value>)> {
    crate::account_session::model::validate_limiter_rules(rules)
        .map_err(|msg| error_response(&msg, StatusCode::BAD_REQUEST))
}

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

/// 「预置 API」一键建号的渠道解析：幂等复用或新建账号型渠道。
///
/// 幂等规则：已存在「同名 + 账号型」渠道时直接复用（且用本请求的
/// base_url/quirks/balance_scheme 等覆盖其渠道级字段，保证预置卡规格
/// 权威），避免反复添加产生「XX 账号池 1/2/3」碎片渠道。找不到才新建，
/// 渠道名缺省 `{provider 展示名} 账号池`（与前端旧兜底同名）。
///
/// 事务语义（对齐上游 api/custom_providers.rs 的「新建必须一次带首个账号」）：
/// 账号落库失败时渠道保留——渠道本身是合法的空池，前端重试会幂等复用，
/// 不会产生碎片；比「失败回滚删渠道」少了删除瞬间的竞态窗口。
fn resolve_or_create_auto_channel(
    state: &AppState,
    provider: &str,
    spec: &AutoChannelSpec,
) -> Result<Channel, (StatusCode, Json<Value>)> {
    let kind = registry::normalize_kind(provider);
    let default_name = format!("{} 账号池", provider_label(&kind));
    let name = spec
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&default_name)
        .to_string();
    let base_url = spec.base_url.as_deref().unwrap_or_default().trim();

    // 幂等复用：同名账号型渠道（有则取第一个，按创建序无强约束——同名本身即用户意图）
    let existing = state
        .channel_store
        .list()
        .into_iter()
        .find(|ch| ch.channel_type == ChannelType::Account && ch.name == name);
    if let Some(ch) = existing {
        // 预置规格权威覆盖：只覆盖渠道级运维字段，不动 id/priority/统计
        let mut updated = ch.clone();
        if !base_url.is_empty() {
            updated.base_url = base_url.to_string();
        }
        if let Some(quirks) = &spec.quirks {
            updated.quirks = quirks.clone();
        }
        if let Some(scheme) = &spec.balance_scheme {
            updated.balance_scheme = scheme.clone();
        }
        if !spec.model_mapping.is_empty() {
            updated.model_mapping = spec.model_mapping.clone();
        }
        if !spec.cost_pricing.is_empty() {
            updated.cost_pricing = spec.cost_pricing.clone();
        }
        return state.channel_store.update(&ch.id, updated).map_err(|e| {
            error_response(
                &format!("Failed to update auto channel: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        });
    }

    let mut ch = Channel {
        id: String::new(),
        name: name.clone(),
        channel_type: ChannelType::Account,
        base_url: base_url.to_string(),
        api_key: String::new(),
        priority: 0,
        weight: 1,
        status: "enabled".to_string(),
        models: Vec::new(),
        account_id: String::new(),
        model_mapping: spec.model_mapping.clone(),
        cost_pricing: spec.cost_pricing.clone(),
        last_error: None,
        last_used_at: None,
        discovered_models: Vec::new(),
        response_time: None,
        test_time: None,
        balance: None,
        credit: None,
        quirks: spec.quirks.clone().unwrap_or_default(),
        balance_scheme: spec.balance_scheme.clone().unwrap_or_default(),
        balance_unit: String::new(),
        created_at: 0,
        updated_at: 0,
    };
    // 与 ChannelRequest 同口径：非空 base_url 过 SSRF 校验（账号型渠道
    // base_url 通常为空，但预置卡可能带渠道级地址）
    ch.balance_scheme = ch.balance_scheme.trim().to_string();
    state.channel_store.add(ch).map_err(|e| {
        error_response(
            &format!("Failed to create auto channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })
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
    // 余额自动查询间隔：三态（缺省保持 / 0 关闭 / >0 定频）。上限 30 天，
    // 防止误填一个巨大值把「下次查询」推到永远。
    if let Some(interval) = body.usage_query_interval_secs {
        session.usage_query_interval_secs = match interval {
            0 => None,
            v => Some(v.min(30 * 86_400)),
        };
    }
    // 限制规则整块替换（缺省 = 保持原值，空数组 = 清空）；合法性由调用方
    // 在进本函数前用 validate_limiters 挡下（本函数不返回 Result）。
    if let Some(rules) = &body.limiters {
        session.limiters = rules.clone();
    }
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
        // 账号限制规则（余额 / Token 两类）；编辑弹窗读它回填规则列表
        "limiters": session.limiters,
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
        // 积分/套餐最早到期时刻（余额快照 wallets[] 里最早的 expireAt；
        // None = 余额没查过或家不报到期）。列表「凭证有效期列 / 按到期排序」用。
        "credit_expires_at": earliest_wallet_expiry(&session.last_balance),
        // 余额自动查询（间隔 + 上次读数快照；查询失败时保留上一次成功值）
        "usage_query_interval_secs": session.usage_query_interval_secs,
        "last_balance_at": session.last_balance_at,
        "last_balance": session.last_balance,
        "last_balance_error": session.last_balance_error,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
    })
}

/// 从余额快照的 `wallets[]` 里取**最早**的 `expireAt`（unix 秒）。
///
/// 「还能用多久」由最先到期的那份额度决定。字段名兼容 `expireAt` / `expire_at`；
/// 无到期语义（或没查过余额）给 None。字符串数字与毫秒秒都收（各家快照口径
/// 不一，> 10^12 视为毫秒）。
fn earliest_wallet_expiry(balance: &Option<serde_json::Value>) -> Option<i64> {
    let balance = balance.as_ref()?;
    let wallets = balance.get("wallets")?.as_array()?;
    let mut earliest: Option<i64> = None;
    for wallet in wallets {
        let raw = wallet
            .get("expireAt")
            .or_else(|| wallet.get("expire_at"))
            .and_then(serde_json::Value::as_i64)
            .or_else(|| {
                wallet
                    .get("expireAt")
                    .or_else(|| wallet.get("expire_at"))
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
            });
        let Some(at) = raw else { continue };
        // 毫秒口径归一到秒
        let at = if at > 10_000_000_000 { at / 1000 } else { at };
        if at > 0 {
            earliest = Some(earliest.map_or(at, |current| current.min(at)));
        }
    }
    earliest
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
    // auto_channel 的渠道级地址同样会被转发/探活用到，与 handle_add_channel
    // 同口径过 SSRF 校验（空 = 沿用 provider 默认，放行）
    if let Some(spec) = &body.auto_channel {
        let ch_base = spec.base_url.as_deref().unwrap_or_default().trim();
        if !ch_base.is_empty() {
            if let Err(rej) = crate::net_guard::validate_outbound_url(
                ch_base,
                config.usage.allow_internal_upstream,
            ) {
                return Err(error_response(&rej.message(), StatusCode::BAD_REQUEST));
            }
        }
    }
    // auto_channel：channel_id 为空时原子「建/复用渠道 + 建账号」，替代
    // 前端两段式；channel_id 优先，同时给出以 channel_id 为准。
    let channel = if body.channel_id.trim().is_empty() {
        match &body.auto_channel {
            Some(spec) => resolve_or_create_auto_channel(&state, &body.provider, spec)?,
            None => {
                return Err(error_response(
                    "需要指定 channel_id 或 auto_channel",
                    StatusCode::BAD_REQUEST,
                ))
            }
        }
    } else {
        ensure_account_channel(&state, body.channel_id.trim())?
    };
    // 限制规则先校验再应用：非法就整单拒绝，不产生「存了一半」的账号
    validate_limiters(body.limiters.as_deref().unwrap_or_default())?;
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
        Some(json!({
            "name": created.name,
            "provider": created.provider,
            // auto_channel 建号时记录渠道来源（新建 or 复用），便于排查碎片渠道
            "auto_channel": body.channel_id.trim().is_empty() && body.auto_channel.is_some(),
        })),
    );
    // 新账号没填模型白名单时，模型管理里就一行都看不到（账号型渠道的模型
    // 只来自 `session.models`，落回 `channel.effective_models()` 也是空）。
    // 这里异步补一次目录，让「加完账号就能在模型管理右栏筛出来」成立；
    // 后台跑 + 失败静默：探测失败不该让建号请求失败（离线/限流都可能）。
    if created.models.is_empty() {
        let probe_state = state.clone();
        let probe_id = created.id.clone();
        tokio::spawn(async move {
            let Some(session) = probe_state.account_sessions.get(&probe_id) else {
                return;
            };
            let models = match crate::account_session::catalog::sync_account_catalog(
                &probe_state.account_sessions,
                &session,
                true,
            )
            .await
            {
                Ok(snapshot) => snapshot
                    .models
                    .into_iter()
                    .map(|m| m.id)
                    .collect::<Vec<_>>(),
                Err(_) => {
                    // 登录态供应商没有目录描述时走通用探测；两者都失败就放弃
                    match crate::account_session::catalog::discover_models_plain(
                        &probe_state.account_sessions,
                        &session,
                    )
                    .await
                    {
                        Ok(models) => models,
                        Err(e) => {
                            tracing::debug!("账号 {probe_id} 自动模型发现失败：{e}");
                            return;
                        }
                    }
                }
            };
            if models.is_empty() {
                return;
            }
            if let Err(e) = crate::account_session::catalog::apply_catalog_to_session(
                &probe_state.account_sessions,
                &probe_id,
                &models,
                false,
            ) {
                tracing::debug!("账号 {probe_id} 写入模型清单失败：{e}");
            }
        });
    }
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
    // 限制规则先校验再应用（与新增同口径）
    validate_limiters(body.limiters.as_deref().unwrap_or_default())?;
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
    // 限制规则：缺省 = 保持原值；给了就校验后整块替换（空数组 = 清空）
    if let Some(rules) = &body.limiters {
        validate_limiters(rules)?;
        next.limiters = rules.clone();
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
    // 删除保护（与批量 delete 同一口径）：账号被渠道 account_id 显式绑定时拒删
    let referenced = state
        .channel_store
        .list()
        .into_iter()
        .any(|c| c.account_id == id);
    if referenced {
        return Err(error_response(
            "referenced_by_channel",
            StatusCode::CONFLICT,
        ));
    }
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

// ── 批量操作六类（Phase1-A1，对齐上游 accounts_batch.rs）────────────

/// POST /api/account-sessions/batch 请求体（契约与 fe-lists-fix 13-gap §7.3 对齐）。
#[derive(Debug, Deserialize)]
pub struct BatchAccountsRequest {
    /// 操作：`enable` / `disable` / `set_proxy` / `set_max_concurrency` /
    /// `query_settings` / `limiter` / `delete`；未知 op 直接 400（不做部分执行）
    pub op: String,
    /// 目标账号 id 列表（逐条回执，未知 id 记 `not_found` 不中断）
    pub ids: Vec<String>,
    /// op=set_proxy：出口代理（空串 = 清除代理）
    #[serde(default)]
    pub proxy: Option<String>,
    /// op=set_max_concurrency：0 = 不限
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// op=query_settings：自动余额查询开关（false = 关闭 = interval 置 None）
    #[serde(default)]
    pub usage_query_enabled: Option<bool>,
    /// op=query_settings：查询间隔秒（enabled=true 时缺省沿用现值，再缺省 3600）
    #[serde(default)]
    pub usage_query_interval_secs: Option<u64>,
    /// op=limiter：`append` / `replace` / `enable` / `disable` / `clear`
    #[serde(default)]
    pub limiter_mode: Option<String>,
    /// op=limiter：append/replace 时必带（整组通用额度口径）
    #[serde(default)]
    pub limiters: Option<Vec<crate::account_session::model::AccountLimiterRule>>,
}

/// 批量操作六类。逐账号回执：部分失败不影响成功项，成功也回执（ok:true）。
/// 删除保护：账号被渠道 `account_id` 显式绑定时拒删（`referenced_by_channel`），
/// 与单删语义同一判定，避免批量绕过。
///
/// 性能影响：温路径（管理页批量运维，非数据面热路径）；逐条 upsert 走既有
/// 异步落盘旁路，无新增锁/IO 模式；ids 规模有界（前端分批），每条一次
/// AccountSession 克隆 + 序列化。
pub async fn handle_batch_account_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchAccountsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 未知 op：整体 400，不做部分执行（契约①）
    let known = matches!(
        body.op.as_str(),
        "enable"
            | "disable"
            | "set_proxy"
            | "set_max_concurrency"
            | "query_settings"
            | "limiter"
            | "delete"
    );
    if !known {
        return Err(error_response(
            &format!("unknown batch op: {}", body.op),
            StatusCode::BAD_REQUEST,
        ));
    }
    // limiter op 的参数前置校验（append/replace 需带规则并通过 validate）
    let limiter_rules = if body.op == "limiter" {
        let mode = body.limiter_mode.as_deref().ok_or_else(|| {
            error_response(
                "limiter_mode is required for op=limiter",
                StatusCode::BAD_REQUEST,
            )
        })?;
        match mode {
            "append" | "replace" => {
                let rules = body.limiters.clone().ok_or_else(|| {
                    error_response(
                        "limiters is required for append/replace",
                        StatusCode::BAD_REQUEST,
                    )
                })?;
                validate_limiters(&rules)?;
                Some(rules)
            }
            "enable" | "disable" | "clear" => None,
            other => {
                return Err(error_response(
                    &format!("unknown limiter_mode: {other}"),
                    StatusCode::BAD_REQUEST,
                ))
            }
        }
    } else {
        None
    };
    if body.ids.is_empty() {
        return Err(error_response(
            "ids must not be empty",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 删除保护参照表：渠道显式绑定的 account_id 集合（一次快照，O(渠道数)）
    let bound_account_ids: std::collections::HashSet<String> = if body.op == "delete" {
        state
            .channel_store
            .list()
            .into_iter()
            .filter(|c| !c.account_id.is_empty())
            .map(|c| c.account_id)
            .collect()
    } else {
        Default::default()
    };

    let mut receipts: Vec<Value> = Vec::with_capacity(body.ids.len());
    let mut ok_count = 0usize;
    for id in &body.ids {
        let Some(prev) = state.account_sessions.get(id) else {
            receipts.push(json!({ "id": id, "ok": false, "error": "not_found" }));
            continue;
        };
        // 批量 delete 的引用保护（契约②）
        if body.op == "delete" && bound_account_ids.contains(id) {
            receipts.push(json!({
                "id": id, "ok": false, "error": "referenced_by_channel",
            }));
            continue;
        }
        let result: Result<(), String> = (|| match body.op.as_str() {
            "enable" | "disable" => {
                let mut next = prev.clone();
                next.status = if body.op == "enable" {
                    "enabled"
                } else {
                    "disabled"
                }
                .to_string();
                state.account_sessions.upsert(next)
            }
            "set_proxy" => {
                let mut next = prev.clone();
                next.proxy = match body.proxy.as_deref() {
                    Some("") | None => None,
                    Some(p) => Some(p.to_string()),
                };
                state.account_sessions.upsert(next)
            }
            "set_max_concurrency" => {
                let Some(v) = body.max_concurrency else {
                    return Err("max_concurrency is required".to_string());
                };
                let mut next = prev.clone();
                next.max_concurrency = v;
                state.account_sessions.upsert(next)
            }
            "query_settings" => {
                let Some(enabled) = body.usage_query_enabled else {
                    return Err("usage_query_enabled is required".to_string());
                };
                let mut next = prev.clone();
                next.usage_query_interval_secs = if enabled {
                    Some(
                        body.usage_query_interval_secs
                            .or(next.usage_query_interval_secs)
                            .unwrap_or(3600),
                    )
                } else {
                    None
                };
                state.account_sessions.upsert(next)
            }
            "limiter" => {
                let mode = body.limiter_mode.as_deref().unwrap_or_default();
                let mut next = prev.clone();
                next.limiters = match mode {
                    "append" => {
                        let mut rules = next.limiters.clone();
                        rules.extend(limiter_rules.clone().unwrap_or_default());
                        rules
                    }
                    "replace" => limiter_rules.clone().unwrap_or_default(),
                    "enable" => {
                        let mut rules = next.limiters.clone();
                        for r in rules.iter_mut() {
                            r.enabled = true;
                        }
                        rules
                    }
                    "disable" => {
                        let mut rules = next.limiters.clone();
                        for r in rules.iter_mut() {
                            r.enabled = false;
                        }
                        rules
                    }
                    "clear" => Vec::new(),
                    _ => return Err("unknown limiter_mode".to_string()),
                };
                state.account_sessions.upsert(next)
            }
            "delete" => state
                .account_sessions
                .remove(id)
                .map(|_| ())
                .map_err(|e| e.to_string()),
            _ => Err("unreachable".to_string()),
        })();
        match result {
            Ok(()) => {
                ok_count += 1;
                receipts.push(json!({ "id": id, "ok": true }));
            }
            Err(e) => receipts.push(json!({ "id": id, "ok": false, "error": e })),
        }
    }
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        &format!("batch_account_{}", body.op),
        &format!("count={} ok={}", body.ids.len(), ok_count),
        Some(json!({ "ids": body.ids })),
        None,
    );
    Ok(Json(json!({ "success": true, "data": receipts })))
}

// ── 批量导入 ────────────────────────────────────────────────────────

/// POST /api/account-sessions/bulk
pub async fn handle_bulk_import_account_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkImportRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
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
    let base_url = body.base_url.trim().to_string();
    // S5：批量导入此前直接落库 body.base_url，未过 SSRF 校验。
    let base_url_allowed = bulk_base_url_allowed(&base_url, config.usage.allow_internal_upstream);
    if !base_url_allowed {
        tracing::warn!("bulk import base_url rejected by SSRF guard: {base_url}");
    }
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
        if !base_url_allowed {
            // 共享地址不合规：跳过该条并计数（不让整批失败）。
            skipped += 1;
            continue;
        }
        let mut session = AccountSession::new(channel.id.clone(), String::new());
        session.provider = provider.clone();
        session.base_url = base_url.clone();
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
///
/// 可选 body `{model: "..."}` 只清该模型的冷却（`"*"` = 整号冷却），
/// 不带 model 或 body = 全部清除（旧调用方行为不变）。
pub async fn handle_reset_account_session_cooldowns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let model = body
        .as_ref()
        .and_then(|Json(v)| v.get("model"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let Some(cleared) = state.account_sessions.clear_cooldown(&id, model.as_deref()) else {
        return Err(error_response(
            "Account session not found",
            StatusCode::NOT_FOUND,
        ));
    };
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "reset_account_session_cooldown",
        &format!(
            "id={id} model={} cleared={cleared}",
            model.as_deref().unwrap_or("*all*")
        ),
        None,
        None,
    );
    Ok(Json(
        json!({ "success": true, "data": { "cleared": cleared } }),
    ))
}

// ── provider 目录 / 统计 ────────────────────────────────────────────

/// GET /api/account-sessions/connections —— 各账号实时并发占用快照。
///
/// 前端连接数列 2 秒轮询这里（对齐 Agent2API `getAccountConnections`）：
/// 汇总内存池里的运行态计数 `in_flight`（不落盘，天然是实时值），
/// 形状 `{data: {counts: {<sessionId>: n}}}`；不在表里的账号等价于 0 并发，
/// 前端按 Record 读取。
pub async fn handle_account_session_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut counts = Map::new();
    for session in state.account_sessions.all() {
        counts.insert(session.id, json!(session.in_flight));
    }
    Ok(Json(
        json!({ "success": true, "data": { "counts": counts } }),
    ))
}

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
            // 分类（web = 网页端产品 / agent = 编码工具形态的登录态家 /
            // api = API Key 直连）：前端「添加账号」弹窗据此分三段展示，
            // 对齐上游 Agent2API provider_category。转发链路不读它。
            "category": registry::category(spec.kind),
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
    /// 是否在拉取成功后自动清理失效映射（target 不在存活集的全局映射）。
    ///
    /// 安全边界（硬要求）：只有**至少一个账号本轮成功回源到非空目录**
    /// （`source == "remote"`）时才执行清理；缓存命中 / 静态兜底 / 拉取失败
    /// 一律不动，防止一次网络抖动把用户映射删光。缺省 = true（对齐用户
    /// 诉求「拉取模型后把遗留的失效映射清掉」）；前端可显式传 false 关闭。
    #[serde(default = "default_true")]
    pub cleanup: bool,
}
// `default_true` 定义在文件前部（AutoChannelSpec 附近），此处复用

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
    // 清理判定用：本轮成功回源（source=remote）且目录非空的账号数。
    // ≥1 才允许触发失效映射清理 —— 拉取失败/缓存/兜底一律不清理。
    let mut remote_ok_accounts: usize = 0;
    // 本轮拉回的目录条目全集（id ∪ upstream 真名），清理时并入存活集
    let mut fresh_catalog: std::collections::HashSet<String> = std::collections::HashSet::new();

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
                // 只有「成功回源且目录非空」才计入清理触发条件并收集条目：
                // cache/fallback 意味着本轮没有真实的新鲜数据，不清
                if snapshot.source == "remote" && !snapshot.models.is_empty() {
                    remote_ok_accounts += 1;
                    for entry in &snapshot.models {
                        fresh_catalog.insert(entry.id.clone());
                        if let Some(up) = &entry.upstream {
                            fresh_catalog.insert(up.clone());
                        }
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

    // ── 失效映射自动清理 ─────────────────────────────────────────────
    //
    // 用户诉求：「新获取的模型里没有老模型了，之前遗留的映射要清掉」。
    // 三重安全闸门（缺一不可）：
    // 1. 请求方显式要求清理（cleanup，缺省 true）；
    // 2. 本轮至少一个账号**成功回源**到非空目录（remote_ok_accounts ≥ 1）——
    //    全部失败 / 只命中缓存 / 只拿到静态兜底时绝不清理；
    // 3. 存活集非空（渠道/定价全空且目录为空的冷启动态不清）。
    let mut removed_mappings: Vec<(String, String)> = Vec::new();
    if body.cleanup && remote_ok_accounts > 0 {
        let alive = stale_mapping_alive_set(&state, &fresh_catalog);
        if !alive.is_empty() {
            match state.model_mapper.prune_stale_mappings(&alive) {
                Ok(removed) if !removed.is_empty() => {
                    tracing::info!(
                        "目录同步后自动清理失效映射：{} 条 → {:?}",
                        removed.len(),
                        removed
                            .iter()
                            .map(|(a, t)| format!("{a}→{t}"))
                            .collect::<Vec<_>>()
                    );
                    removed_mappings = removed;
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("失效映射清理落盘失败：{e}");
                }
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
            // 自动清理结果（前端弹窗展示；cleaned=false 表示闸门未通过未执行）
            "removed_mappings": removed_mappings
                .iter()
                .map(|(alias, target)| json!({ "alias": alias, "target": target }))
                .collect::<Vec<_>>(),
            "removed_mapping_count": removed_mappings.len(),
            "cleanup_ran": body.cleanup && remote_ok_accounts > 0,
        }
    })))
}

/// 失效映射判定的「存活集」：映射 target 出现在其中任意一处即视为存活。
///
/// 口径对齐数据面 `model_declared_by_channel`（resolve.rs）+ 补充三类引用源，
/// 宁可漏删不可误删：
/// - 启用渠道/账号声明的模型（含 discovered_models 发现快照）
/// - 定价表（pricing_store）与渠道级成本价（cost_pricing 的 key 与 value）
/// - 自定义模型登记（管理员手动登记的模型，绝不清理）
/// - 本轮拉回的目录条目（id ∪ upstream 真名）
/// - 全部现有映射的 target（链式映射 a→b→c 中 b 被 a 引用，不可删）
pub(crate) fn stale_mapping_alive_set(
    state: &AppState,
    fresh_catalog: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    let mut alive: std::collections::HashSet<String> = fresh_catalog.clone();
    for ch in state.channel_store.list() {
        if !ch.is_enabled() {
            continue;
        }
        for m in ch.effective_models() {
            alive.insert(m.clone());
        }
        // 账号型渠道的模型声明在账号上（同 model_declared_by_channel 口径）
        if ch.channel_type == ChannelType::Account {
            for s in state.account_sessions.for_channel(&ch.id) {
                if s.status == "enabled" {
                    for m in &s.models {
                        alive.insert(m.clone());
                    }
                }
            }
        }
        // 渠道级成本价的 key（= 映射后的上游名）与 value（映射目标名也算声明）
        for name in ch.cost_pricing.keys() {
            alive.insert(name.clone());
        }
    }
    for price in state.pricing_store.list_prices() {
        alive.insert(price.model_name);
    }
    for entry in state.model_mapper.custom_models() {
        alive.insert(entry.id);
    }
    // 现有映射的 target 整体并入：被其他别名引用的中间名不可删
    for target in state.model_mapper.all_mappings().into_values() {
        alive.insert(target);
    }
    alive
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
                // S6：路径白名单，避免把任意本机文件当 SQLite 打开。
                let path = validate_import_db_path(path)
                    .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
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
        // 账号限制规则（余额 / Token 两类）：从 entry 解析并按 create 同口径校验
        // （validate_limiter_rules），非法规则整条拒绝——与 create 的「半保存比
        // 不保存更难排查」一致。JSON null / 缺省 = 不带规则。
        let entry_limiters = match entry.get("limiters").filter(|v| !v.is_null()) {
            Some(v) => match serde_json::from_value::<
                Vec<crate::account_session::model::AccountLimiterRule>,
            >(v.clone())
            {
                Ok(rules) => match crate::account_session::model::validate_limiter_rules(&rules) {
                    Ok(()) => Some(rules),
                    Err(msg) => {
                        errors.push(format!("{name}: {msg}"));
                        continue;
                    }
                },
                Err(e) => {
                    errors.push(format!("{name}: limiters 解析失败：{e}"));
                    continue;
                }
            },
            None => None,
        };
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
            usage_query_interval_secs: None,
            limiters: entry_limiters,
            // Agent2API 导入不走自动建渠道（导入流程自己管理渠道归属）
            auto_channel: None,
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

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn bulk_import_rejects_internal_base_url() {
        // S5：环回与云元数据地址在批量导入中必须被跳过。
        assert!(!bulk_base_url_allowed("http://127.0.0.1/x", false));
        assert!(!bulk_base_url_allowed("http://169.254.169.254/", false));
        // 公网地址与空串（沿用 provider 默认）放行。
        // 用公网 IP 字面量，避免离线环境 DNS 解析失败造成假阴性。
        assert!(bulk_base_url_allowed("https://1.1.1.1/v1", false));
        assert!(bulk_base_url_allowed("", false));
    }

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn import_db_path_rejects_traversal_and_outside_roots() {
        // S6：`..` 组件直接拒绝，不依赖文件是否存在。
        assert!(validate_import_db_path("../../etc/passwd").is_err());
        // 系统根目录下的任意文件不在白名单根内。
        #[cfg(unix)]
        assert!(validate_import_db_path("/etc/passwd").is_err());
    }

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn import_db_path_accepts_file_under_temp_dir() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!("aigx-import-{}.db", std::process::id()));
        std::fs::write(&file, b"sqlite-placeholder").expect("write temp file");
        let result = validate_import_db_path(file.to_string_lossy().as_ref());
        let _ = std::fs::remove_file(&file);
        assert!(
            result.is_ok(),
            "temp-dir file should be allowed: {result:?}"
        );
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn earliest_wallet_expiry_variants() {
        // 没查过余额 / 无 wallets 字段
        assert_eq!(earliest_wallet_expiry(&None), None);
        assert_eq!(earliest_wallet_expiry(&Some(json!({"available": 1}))), None);
        // camelCase 秒
        let snap = json!({
            "wallets": [
                { "total": 100, "expireAt": 1799000000 },
                { "total": 50, "expireAt": 1798000000 },
            ]
        });
        assert_eq!(
            earliest_wallet_expiry(&Some(snap)),
            Some(1798000000) // 取最早
        );
        // snake_case / 字符串数字 / 毫秒归一
        let snap = json!({
            "wallets": [
                { "expire_at": "1798000000" },
                { "expireAt": 1_798_000_000_123i64 },
            ]
        });
        assert_eq!(earliest_wallet_expiry(&Some(snap)), Some(1798000000));
        // 0 = 无到期语义
        let snap = json!({ "wallets": [ { "expireAt": 0 } ] });
        assert_eq!(earliest_wallet_expiry(&Some(snap)), None);
    }
}
