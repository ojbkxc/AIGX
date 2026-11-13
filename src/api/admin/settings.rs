//! 系统设置 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供模型映射、限额配置等系统设置功能。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

#[derive(Debug, Deserialize)]
pub struct SettingsRequest {
    /// 模型映射表。缺省为空表（不涉及映射时前端可不传）。
    #[serde(default)]
    pub mappings: std::collections::HashMap<String, String>,
    /// 是否整表替换（true）还是增量合并（false）。缺省 false = 增量，
    /// 避免「只传部分映射」时把既有映射清空。
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Deserialize)]
pub struct LimitsRequest {
    pub daily_limit: Option<u64>,
    pub monthly_limit: Option<u64>,
    pub register_quota: Option<i64>,
    pub register_enabled: Option<bool>,
    pub quota_for_inviter: Option<i64>,
    pub quota_for_invitee: Option<i64>,
    pub email_whitelist_enable: Option<bool>,
    pub email_whitelist_suffix: Option<Vec<String>>,
    /// 签到设置（对齐 new-api CheckinSetting）
    pub checkin: Option<CheckinSettingBody>,
    pub threshold: Option<f64>,
    pub api_timeout_secs: Option<u64>,
    pub connect_timeout_secs: Option<u64>,
    pub headers_timeout_secs: Option<u64>,
    pub stream_idle_secs: Option<u64>,
    /// 非流式响应体超时（秒）
    pub body_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    /// 同一账号内的重试次数（0~10）
    pub retry_self_count: Option<u32>,
    /// 切换账号重试次数（0~10，默认 5）——单次请求最多尝试几个账号
    pub retry_switch_count: Option<u32>,
    pub retry_interval_ms: Option<u64>,
    pub retry_on_status: Option<Vec<u16>>,
    pub max_request_body_mb: Option<usize>,
    /// 请求日志快照开关（log_body）
    pub log_body: Option<bool>,
    /// 路由回放快照开关（route_replay）：resolve 过滤原因/策略快照/账号级
    /// 跳过细因落请求日志，供 /api/routing/replay 回放。默认关闭零开销。
    pub route_replay: Option<bool>,
    /// 聊天助手工具回路开关（#34；默认关闭）
    pub assistant_tools: Option<bool>,
    /// 调试模式（上游原始报文）开关
    pub debug_traffic: Option<bool>,
    /// 丢弃上游换行保活帧开关
    pub strip_newline_keepalive: Option<bool>,
    /// 按次计费每次固定扣减额度（billing_mode=count 的订阅生效）
    pub billing_flat_quota: Option<i64>,
    /// 是否允许上游地址指向内网（SSRF 防护逃生阀，默认 false）
    pub allow_internal_upstream: Option<bool>,
    /// 排队退避最多等待次数（0~10，默认 2；0 = 排队即 503 快速失败）
    pub queue_max_waits: Option<u8>,
    /// 排队单次等待秒数（0~120，默认 0 = 跟随上游建议）
    pub queue_wait_seconds: Option<u8>,
    /// 截断自动续写总开关（默认 false；关闭时数据面不做任何包装）
    pub auto_continue: Option<bool>,
    /// 续写最多额外轮数（1~8，默认 3）
    pub auto_continue_max_rounds: Option<u8>,
    /// 出站指纹脱敏开关（请求体侧；表头侧名单见 /api/sanitize）
    pub sanitize_fingerprints: Option<bool>,
}

/// 签到设置请求体（enabled + 奖励区间）
#[derive(Debug, Deserialize)]
pub struct CheckinSettingBody {
    pub enabled: Option<bool>,
    pub min_quota: Option<i64>,
    pub max_quota: Option<i64>,
}

/// 获取系统设置
pub async fn handle_get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mappings = state.model_mapper.all_mappings();
    Ok(Json(json!({
        "success": true,
        "data": {
            "mappings": mappings
        }
    })))
}

/// 保存模型映射
pub async fn handle_update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if body.replace_all {
        // 替换所有映射
        if let Err(e) = state.model_mapper.reset() {
            tracing::error!("Failed to reset model mappings: {}", e);
        }
        for (source, target) in &body.mappings {
            if let Err(e) = state
                .model_mapper
                .set_custom(source.clone(), target.clone())
            {
                tracing::error!("Failed to set mapping {} -> {}: {}", source, target, e);
            }
        }
    } else {
        // 逐个添加/更新
        for (source, target) in &body.mappings {
            if let Err(e) = state
                .model_mapper
                .set_custom(source.clone(), target.clone())
            {
                tracing::error!("Failed to set mapping {} -> {}: {}", source, target, e);
            }
        }
    }
    Ok(Json(json!({
        "success": true,
        "data": null
    })))
}

/// 获取限额配置
pub async fn handle_get_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let daily = state.usage_tracker.today_stats();
    let monthly = state.usage_tracker.monthly_stats();
    Ok(Json(json!({
        "success": true,
        "data": json!({
            "daily_limit": config.usage.daily_limit,
            "daily_used": daily.total(),
            "monthly_limit": config.usage.monthly_limit,
            "monthly_used": monthly.total(),
            "register_quota": config.usage.register_quota,
            "register_enabled": config.usage.register_enabled,
            "quota_for_inviter": config.usage.quota_for_inviter,
            "quota_for_invitee": config.usage.quota_for_invitee,
            "email_whitelist_enable": config.usage.email_whitelist_enable,
            "email_whitelist_suffix": config.usage.email_whitelist_suffix,
            "checkin": config.usage.checkin,
            "threshold": config.usage.threshold,
            "api_timeout_secs": config.usage.api_timeout_secs,
            "connect_timeout_secs": config.usage.connect_timeout_secs,
            "headers_timeout_secs": config.usage.headers_timeout_secs,
            "stream_idle_secs": config.usage.stream_idle_secs,
            "body_timeout_secs": config.usage.body_timeout_secs,
            "max_retries": config.usage.max_retries,
            "retry_self_count": config.usage.retry_self_count,
            "retry_switch_count": config.usage.retry_switch_count,
            "retry_interval_ms": config.usage.retry_interval_ms,
            "retry_on_status": config.usage.retry_on_status,
            "max_request_body_mb": config.usage.max_request_body_mb,
            "log_body": config.usage.log_body,
            "route_replay": config.usage.route_replay,
            "assistant_tools": config.usage.assistant_tools,
            "debug_traffic": config.usage.debug_traffic,
            "strip_newline_keepalive": config.usage.strip_newline_keepalive,
            "billing_flat_quota": config.usage.billing_flat_quota,
            "allow_internal_upstream": config.usage.allow_internal_upstream,
            "queue_max_waits": config.usage.queue_max_waits,
            "queue_wait_seconds": config.usage.queue_wait_seconds,
            "auto_continue": config.usage.auto_continue,
            "auto_continue_max_rounds": config.usage.auto_continue_max_rounds,
            "sanitize_fingerprints": config.usage.sanitize_fingerprints,
        })
    })))
}

/// 更新限额配置
pub async fn handle_update_limits(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LimitsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.daily_limit {
        config.usage.daily_limit = v;
    }
    if let Some(v) = body.monthly_limit {
        config.usage.monthly_limit = v;
    }
    if let Some(v) = body.register_quota {
        config.usage.register_quota = v;
    }
    if let Some(v) = body.register_enabled {
        config.usage.register_enabled = v;
    }
    if let Some(v) = body.quota_for_inviter {
        config.usage.quota_for_inviter = v;
    }
    if let Some(v) = body.quota_for_invitee {
        config.usage.quota_for_invitee = v;
    }
    if let Some(v) = body.email_whitelist_enable {
        config.usage.email_whitelist_enable = v;
    }
    if let Some(v) = body.email_whitelist_suffix {
        config.usage.email_whitelist_suffix = v;
    }
    if let Some(c) = body.checkin {
        if let Some(v) = c.enabled {
            config.usage.checkin.enabled = v;
        }
        if let Some(v) = c.min_quota {
            config.usage.checkin.min_quota = v;
        }
        if let Some(v) = c.max_quota {
            config.usage.checkin.max_quota = v;
        }
    }
    if let Some(v) = body.threshold {
        config.usage.threshold = v;
    }
    if let Some(v) = body.api_timeout_secs {
        config.usage.api_timeout_secs = v.clamp(1, 3_600);
    }
    if let Some(v) = body.connect_timeout_secs {
        config.usage.connect_timeout_secs = v.clamp(1, 300);
    }
    if let Some(v) = body.headers_timeout_secs {
        config.usage.headers_timeout_secs = v.clamp(1, 600);
        // 首事件守卫实时生效（其余客户端超时下次启动生效，见页面提示）
        crate::bridge::set_sse_first_event_deadline(std::time::Duration::from_secs(
            config.usage.headers_timeout_secs,
        ));
    }
    if let Some(v) = body.stream_idle_secs {
        config.usage.stream_idle_secs = v.clamp(1, 600);
    }
    if let Some(v) = body.body_timeout_secs {
        config.usage.body_timeout_secs = v.clamp(1, 3_600);
    }
    if let Some(v) = body.retry_self_count {
        config.usage.retry_self_count = v.min(10);
    }
    if let Some(v) = body.retry_switch_count {
        config.usage.retry_switch_count = v.min(10);
    }
    if let Some(v) = body.retry_interval_ms {
        config.usage.retry_interval_ms = v.min(10_000);
    }
    if let Some(v) = body.retry_on_status {
        let mut codes: Vec<u16> = Vec::new();
        for code in v {
            if !(100..=599).contains(&code) {
                return Err(error_response(
                    "直接换号状态码必须在 100-599 之间",
                    StatusCode::BAD_REQUEST,
                ));
            }
            if !codes.contains(&code) {
                codes.push(code);
            }
            if codes.len() >= 50 {
                break;
            }
        }
        config.usage.retry_on_status = codes;
    }
    if let Some(v) = body.max_retries {
        config.usage.max_retries = v.min(10);
    }
    if let Some(v) = body.max_request_body_mb {
        if v > 0 {
            config.usage.max_request_body_mb = v;
        }
    }
    if let Some(v) = body.log_body {
        config.usage.log_body = v;
        // 同步进程级快照（数据面同步路径读取，立即生效）
        crate::config::set_log_body(v);
    }
    if let Some(v) = body.route_replay {
        config.usage.route_replay = v;
        // 同步进程级快照（resolve/选号路径读取，立即生效）
        crate::config::set_route_replay(v);
    }
    if let Some(v) = body.assistant_tools {
        config.usage.assistant_tools = v;
        crate::config::set_assistant_tools(v);
    }
    if let Some(v) = body.debug_traffic {
        config.usage.debug_traffic = v;
        // 同步进程级快照（桥接层每个请求读取，保存即生效、无需重启）
        crate::config::set_debug_traffic(v);
        // 关闭时顺手清空缓冲：避免关掉开关后旧报文还留在管理页上看得到
        // （开启前的报文同样不该在开启后突然出现）。
        if !v {
            crate::debug_traffic::clear();
        }
    }
    if let Some(v) = body.strip_newline_keepalive {
        config.usage.strip_newline_keepalive = v;
        // 同步进程级快照（桥接层每条 SSE 帧读取，保存即生效、无需重启）
        crate::config::set_strip_newline_keepalive(v);
    }
    if let Some(v) = body.billing_flat_quota {
        if v > 0 {
            config.usage.billing_flat_quota = v;
            // 同步进程级快照（数据面按次计费同步读取，立即生效）
            crate::config::set_billing_flat_quota(v);
        }
    }
    if let Some(v) = body.allow_internal_upstream {
        // SSRF 防护逃生阀：开启后允许上游地址指向内网（自托管 Ollama 等）。
        // 保存后立即生效（渠道/账号保存时实时读配置快照）。
        config.usage.allow_internal_upstream = v;
    }
    if let Some(v) = body.queue_max_waits {
        config.usage.queue_max_waits = v.min(10);
        crate::config::set_queue_settings(
            config.usage.queue_max_waits as u64,
            config.usage.queue_wait_seconds as u64,
        );
    }
    if let Some(v) = body.queue_wait_seconds {
        config.usage.queue_wait_seconds = v.min(120);
        crate::config::set_queue_settings(
            config.usage.queue_max_waits as u64,
            config.usage.queue_wait_seconds as u64,
        );
    }
    if body.auto_continue.is_some() || body.auto_continue_max_rounds.is_some() {
        if let Some(v) = body.auto_continue {
            config.usage.auto_continue = v;
        }
        if let Some(v) = body.auto_continue_max_rounds {
            config.usage.auto_continue_max_rounds = v.clamp(1, 8);
        }
        // 同步进程级快照（数据面每个请求同步读取，立即生效）
        crate::config::set_auto_continue_settings(
            config.usage.auto_continue,
            config.usage.auto_continue_max_rounds as u64,
        );
    }
    if let Some(v) = body.sanitize_fingerprints {
        config.usage.sanitize_fingerprints = v;
        // 同步进程级快照（转发层每个请求同步读取，保存即生效、无需重启）
        crate::config::set_sanitize_fingerprints(v);
    }
    // 换号重试间隔立即生效（账号池运行时读取）
    state
        .account_sessions
        .set_retry_interval_ms(config.usage.retry_interval_ms);
    state
        .account_sessions
        .set_retry_on_status(config.usage.retry_on_status.clone());
    // 渠道级重试次数立即生效（选路时读取进程级快照）
    crate::config::set_max_retries(config.usage.max_retries);
    // 账号池「同账号重试」档与非流式响应体超时立即生效
    crate::config::set_retry_self_count(config.usage.retry_self_count);
    // 切换账号重试次数立即生效：映射到 RotationPolicy.max_attempts
    // （max_attempts = retry_switch_count + 1，即「原账号 + 切换次数」）
    crate::config::set_retry_switch_count(config.usage.retry_switch_count);
    let mut policy = state.account_sessions.policy();
    policy.max_attempts = (config.usage.retry_switch_count as usize + 1).clamp(1, 20);
    let _ = state.account_sessions.set_policy(policy);
    crate::config::set_body_timeout_secs(config.usage.body_timeout_secs);
    // 直接换号/换渠道状态码名单立即生效
    crate::config::set_retry_on_status_codes(config.usage.retry_on_status.clone());
    match state.config_manager.update(config).await {
        Ok(_) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": updated.usage
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save limits: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── AI 助手（Agent）设置 ────────────────────────────────────────

/// Agent 设置请求体（缺省字段保持原值）。
#[derive(Debug, Deserialize)]
pub struct AgentSettingsRequest {
    pub enabled: Option<bool>,
    pub model: Option<String>,
    pub channel: Option<String>,
    pub max_turns: Option<usize>,
    pub approval_timeout_secs: Option<u64>,
    pub max_history: Option<usize>,
    pub reasoning_effort: Option<String>,
    pub system_prompt: Option<String>,
    pub disabled_tools: Option<Vec<String>>,
}

/// Agent 配置的规范 JSON 形状（camelCase，与 `/api/agent/settings` 完全一致）。
///
/// 被 `/api/agent/config`（运行期只读视图）复用，保证两个端点字段命名统一，
/// 前端无需维护两套契约。
pub(crate) fn agent_settings_json(config: &crate::config::AgentConfig) -> Value {
    let tools: Vec<Value> = crate::agent::tools::tool_specs()
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "label": spec.label,
                "description": spec.description,
                "risk": spec.risk.as_str(),
            })
        })
        .collect();
    json!({
        "enabled": config.enabled,
        "model": config.model,
        "channel": config.channel,
        "maxTurns": config.max_turns,
        "approvalTimeoutSecs": config.approval_timeout_secs,
        "maxHistory": config.max_history,
        "reasoningEffort": config.reasoning_effort,
        "systemPrompt": config.system_prompt,
        "disabledTools": config.disabled_tools,
        "tools": tools,
    })
}

/// GET /api/agent/settings —— AI 助手设置（含工具清单与风险等级）。
pub async fn handle_get_agent_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": agent_settings_json(&config.agent),
    })))
}

/// PUT /api/agent/settings —— 保存 AI 助手设置（模型/提示词/工具开关/轮数）。
pub async fn handle_update_agent_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AgentSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.enabled {
        config.agent.enabled = v;
    }
    if let Some(v) = body.model.as_deref() {
        config.agent.model = v.trim().to_string();
    }
    if let Some(v) = body.channel.as_deref() {
        config.agent.channel = v.trim().to_string();
    }
    if let Some(v) = body.max_turns {
        config.agent.max_turns = v.clamp(1, 50);
    }
    if let Some(v) = body.approval_timeout_secs {
        config.agent.approval_timeout_secs = v.min(3_600);
    }
    if let Some(v) = body.max_history {
        config.agent.max_history = v.clamp(1, 200);
    }
    if let Some(v) = body.reasoning_effort.as_deref() {
        let effort = v.trim().to_ascii_lowercase();
        config.agent.reasoning_effort = match effort.as_str() {
            "" | "off" => None,
            "low" | "medium" | "high" => Some(effort),
            _ => {
                return Err(error_response(
                    "思考强度应为 off/low/medium/high",
                    StatusCode::BAD_REQUEST,
                ))
            }
        };
    }
    if let Some(v) = body.system_prompt.as_deref() {
        config.agent.system_prompt = v.trim().to_string();
    }
    if let Some(v) = body.disabled_tools {
        if v.len() > 200 {
            return Err(error_response("禁用的工具过多", StatusCode::BAD_REQUEST));
        }
        config.agent.disabled_tools = v
            .into_iter()
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
    }
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            // 运行期 Agent 配置同步（切换 enabled / 模型 / 提示词 / 工具开关都立即生效）
            if let Some(agent) = state.agent_state.as_deref() {
                agent.set_config(updated.agent.clone());
            }
            Ok(Json(json!({
                "success": true,
                "data": agent_settings_json(&updated.agent),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save agent settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── 聊天设置（`[chat]` 段：LLM 自动标题）────────────────────────

/// 聊天设置请求体（缺省字段保持原值；键名 snake_case，与 agent 面同口径）。
#[derive(Debug, Deserialize)]
pub struct ChatSettingsRequest {
    pub auto_title: Option<bool>,
    pub title_model: Option<String>,
    pub title_channel: Option<String>,
    pub title_prompt_chars: Option<usize>,
}

/// 聊天配置的规范 JSON 形状（camelCase，与 `agent_settings_json` 同款手构）。
fn chat_settings_json(config: &crate::config::ChatConfig) -> Value {
    json!({
        "autoTitle": config.auto_title,
        "titleModel": config.title_model,
        "titleChannel": config.title_channel,
        "titlePromptChars": config.title_prompt_chars,
    })
}

/// 危险组合校验：开启自动标题但提示词截断长度为 0 → 标题永不生成。
/// 其他取值原样存（前端负责兜底），与仓库既有惯例一致。
fn validate_chat_combo(config: &crate::config::ChatConfig) -> Result<(), &'static str> {
    if config.auto_title && config.title_prompt_chars == 0 {
        Err("开启自动标题时，标题提示字符数不能为 0")
    } else {
        Ok(())
    }
}

/// GET /api/chat/settings —— 查询聊天设置（LLM 自动标题开关与模型/渠道）。
pub async fn handle_get_chat_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": chat_settings_json(&config.chat),
    })))
}

/// PUT /api/chat/settings —— 保存聊天设置（写 config.toml 并热生效：
/// `chat_session/title.rs` 每次生成标题前都 `config_manager.get()` 取最新值）。
pub async fn handle_update_chat_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChatSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.auto_title {
        config.chat.auto_title = v;
    }
    if let Some(v) = body.title_model.as_deref() {
        config.chat.title_model = v.trim().to_string();
    }
    if let Some(v) = body.title_channel.as_deref() {
        config.chat.title_channel = v.trim().to_string();
    }
    if let Some(v) = body.title_prompt_chars {
        config.chat.title_prompt_chars = v;
    }
    if let Err(msg) = validate_chat_combo(&config.chat) {
        return Err(error_response(msg, StatusCode::BAD_REQUEST));
    }
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": chat_settings_json(&updated.chat),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save chat settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── 报表设置（`[report]` 段：报表日界时区，#20 / 生产体检 P3-14）────────

/// 报表设置请求体（缺省字段保持原值；键名 snake_case，与 chat/agent 面同口径，
/// 额外收 camelCase 别名是因为前端面板习惯 camelCase —— 两种都认，文档只写一种）。
#[derive(Debug, Deserialize)]
pub struct ReportSettingsRequest {
    #[serde(alias = "timezoneOffset")]
    pub timezone_offset: Option<i32>,
}

/// 报表配置的规范 JSON 形状（camelCase，与 `chat_settings_json` 同款手构）。
///
/// 三个"口径"字段各自回答不同问题，混用会误导运维，故都吐出去：
/// - `timezoneOffset`：**配置文件里存的值**（本次 PUT 想要的口径）；
/// - `effectiveOffset`：**进程当前生效值**（原子快照，读路径实际用的那个）。
///   两者正常相同；不同说明有写入路径绕过了本端点（例如将来新增的通用 config
///   导入），前端据此提示"需重启"；
/// - `storedOffset`：**存量聚合行**落盘时的口径（`None` = 老库没记过口径标记）。
///   它与 `effectiveOffset` 不等 ⇒ 报表日界已切但历史还没重算，需要迁移。
fn report_settings_json(config: &crate::config::ReportConfig, stored_offset: Option<i32>) -> Value {
    let effective = crate::config::report_tz_offset();
    json!({
        "timezoneOffset": config.timezone_offset,
        "effectiveOffset": effective,
        "storedOffset": stored_offset,
        // 老库无标记（None）保守按「需要迁移」提示：迁移本身幂等，空库只落标记
        "needsMigration": stored_offset != Some(effective),
        "minOffset": crate::config::REPORT_TZ_OFFSET_MIN,
        "maxOffset": crate::config::REPORT_TZ_OFFSET_MAX,
    })
}

/// GET /api/report/settings —— 查询报表设置（日界时区偏移 + 存量聚合行口径）。
pub async fn handle_get_report_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let stored = state.log_store.daily.tz_marker();
    Ok(Json(json!({
        "success": true,
        "data": report_settings_json(&config.report, stored),
    })))
}

/// PUT /api/report/settings —— 保存报表设置（写 config.toml 并**热生效**）。
///
/// 生效范围只到「切换之后到达的日志」：日聚合键（`log::daily_agg::date_key`）、
/// 24h 小时桶（`dashboard::hour_key_at`）、今日窗口（`diagnostics::today_start_ts`）
/// 都读同一个原子快照，保存即刻切。
///
/// ⚠️ **存量聚合行不在本端点职责内**（生产机不做隐式重写库）：需要管理员显式调
/// `POST /api/logs/agg/tz-migrate` 重算历史，响应里的 `needsMigration` 就是给
/// 前端画提示条用的判据。
pub async fn handle_update_report_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReportSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.timezone_offset {
        if !crate::config::report_tz_offset_valid(v) {
            return Err(error_response(
                &format!(
                    "报表时区偏移必须在 {}~{} 之间（整数小时，UTC 偏移）",
                    crate::config::REPORT_TZ_OFFSET_MIN,
                    crate::config::REPORT_TZ_OFFSET_MAX
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
        config.report.timezone_offset = v;
    }
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            // 保存即生效：报表读路径（日键/小时桶/日窗口）全部读这个快照
            crate::config::set_report_tz_offset(updated.report.timezone_offset);
            let stored = state.log_store.daily.tz_marker();
            Ok(Json(json!({
                "success": true,
                "data": report_settings_json(&updated.report, stored),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save report settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── 网关设置（监听地址 / CORS / 反代信任）──────────────────────────

/// 网关设置请求体（缺省字段保持原值）。
#[derive(Debug, Deserialize)]
pub struct GatewaySettingsRequest {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub cors_origins: Option<Vec<String>>,
    pub trust_proxy_headers: Option<bool>,
    pub log_dir: Option<String>,
    pub request_stats_dir: Option<String>,
    pub debug_dir: Option<String>,
}

fn gateway_settings_json(config: &crate::config::AppConfig) -> Value {
    let dirs = crate::config::storage_dirs(config);
    json!({
        "host": config.server.host,
        "port": config.server.port,
        "dataDir": config.server.data_dir,
        "logDir": config.server.log_dir.clone().unwrap_or_else(|| dirs.log_dir.to_string_lossy().into_owned()),
        "requestStatsDir": config.server.request_stats_dir.clone().unwrap_or_else(|| dirs.request_stats_dir.to_string_lossy().into_owned()),
        "debugDir": config.server.debug_dir.clone().unwrap_or_else(|| dirs.debug_dir.to_string_lossy().into_owned()),
        "corsOrigins": config.cors_origins,
        "trustProxyHeaders": config.trust_proxy_headers,
        "restartRequired": true,
    })
}

/// GET /api/gateway/settings —— 网关设置（监听地址 / CORS / 反代信任）。
pub async fn handle_get_gateway_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": gateway_settings_json(&config),
    })))
}

/// PUT /api/gateway/settings —— 保存网关设置（重启服务后生效）。
pub async fn handle_update_gateway_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<GatewaySettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(host) = body.host.as_deref() {
        let host = host.trim();
        if host.is_empty() || host.chars().any(char::is_whitespace) {
            return Err(error_response(
                "监听地址不能为空且不能包含空白字符",
                StatusCode::BAD_REQUEST,
            ));
        }
        config.server.host = host.to_string();
    }
    if let Some(port) = body.port {
        if port == 0 {
            return Err(error_response(
                "端口必须在 1-65535 之间",
                StatusCode::BAD_REQUEST,
            ));
        }
        config.server.port = port;
    }
    if let Some(origins) = body.cors_origins {
        let mut cleaned: Vec<String> = Vec::new();
        for origin in origins {
            let origin = origin.trim().to_string();
            if origin.is_empty() {
                continue;
            }
            // 拒绝 `*`：CORS 层用 `AllowOrigin::list`（精确匹配），`*` 在这里
            // 不会变成通配、只会静默不匹配任何来源，与「允许所有来源」的直觉
            // 相反 —— 让用户以为配通了却全部被浏览器拦截。要放开就显式列出。
            if origin == "*" {
                return Err(error_response(
                    "CORS 来源不支持 *（请显式列出允许的来源，如 https://example.com）",
                    StatusCode::BAD_REQUEST,
                ));
            }
            if origin.starts_with("http://") || origin.starts_with("https://") {
                cleaned.push(origin);
            } else {
                return Err(error_response(
                    "CORS 来源必须是 http(s):// 开头",
                    StatusCode::BAD_REQUEST,
                ));
            }
        }
        config.cors_origins = cleaned;
    }
    if let Some(v) = body.trust_proxy_headers {
        config.trust_proxy_headers = v;
    }
    // 目录配置（logDir / requestStatsDir / debugDir）：空串 = 清除自定义、回落默认
    for (field, label) in [
        (&body.log_dir, "log_dir"),
        (&body.request_stats_dir, "request_stats_dir"),
        (&body.debug_dir, "debug_dir"),
    ] {
        if let Some(raw) = field {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                // 空串 = 清除自定义设置
                if label == "log_dir" {
                    config.server.log_dir = None;
                } else if label == "request_stats_dir" {
                    config.server.request_stats_dir = None;
                } else {
                    config.server.debug_dir = None;
                }
            } else {
                let path = crate::config::expand_path(trimmed);
                if !path.is_absolute() {
                    return Err(error_response(
                        &format!("{label} 必须是绝对路径"),
                        StatusCode::BAD_REQUEST,
                    ));
                }
                if label == "log_dir" {
                    config.server.log_dir = Some(path.to_string_lossy().into_owned());
                } else if label == "request_stats_dir" {
                    config.server.request_stats_dir = Some(path.to_string_lossy().into_owned());
                } else {
                    config.server.debug_dir = Some(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    // 信任反代头立即生效（客户端 IP 提取走进程级快照）；
    // 监听地址/端口/CORS 需重启服务后生效。
    crate::config::set_trust_proxy_headers(config.trust_proxy_headers);
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": gateway_settings_json(&updated),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save gateway settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// ── 安全策略（登录失败锁定 / 登录限流）────────────────────────────

/// 安全策略请求体（缺省字段保持原值）。
#[derive(Debug, Deserialize)]
pub struct SecurityPolicyRequest {
    pub lockout_enabled: Option<bool>,
    pub fail_threshold: Option<u32>,
    pub lock_minutes: Option<u64>,
    pub rate_limit_per_minute: Option<u32>,
    /// 机器人校验（ALTCHA proof-of-work）开关
    pub captcha_enabled: Option<bool>,
}

fn security_policy_json(config: &crate::config::SecurityPolicyConfig) -> Value {
    json!({
        "lockoutEnabled": config.lockout_enabled,
        "failThreshold": config.fail_threshold,
        "lockMinutes": config.lock_minutes,
        "rateLimitPerMinute": config.rate_limit_per_minute,
        "captchaEnabled": config.captcha_enabled,
        // `lockMinutes` 是失败计数缓存的 TTL，建缓存时固定 → 改后需重启服务生效；
        // 其余走原子快照，保存即生效。
        "restartRequiredFields": ["lockMinutes"],
    })
}

/// GET /api/security/policy —— 安全策略（登录失败锁定 / 限流阈值）。
pub async fn handle_get_security_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": security_policy_json(&config.security),
    })))
}

/// PUT /api/security/policy —— 保存安全策略。
///
/// 越界输入按 `SecurityPolicyConfig::normalized` 夹到合法区间（阈值 1-100、
/// 锁定时长 1-1440 分钟、每分钟上限 1-1000）后落盘。
pub async fn handle_update_security_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SecurityPolicyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.lockout_enabled {
        config.security.lockout_enabled = v;
    }
    if let Some(v) = body.fail_threshold {
        config.security.fail_threshold = v;
    }
    if let Some(v) = body.lock_minutes {
        config.security.lock_minutes = v;
    }
    if let Some(v) = body.rate_limit_per_minute {
        config.security.rate_limit_per_minute = v;
    }
    if let Some(v) = body.captcha_enabled {
        config.security.captcha_enabled = v;
    }
    config.security = config.security.normalized();
    // 阈值 / 限流 / 开关立即生效（登录 handler 读进程级快照）
    crate::config::set_security_policy(
        config.security.lockout_enabled,
        config.security.fail_threshold,
        config.security.rate_limit_per_minute,
    );
    // 机器人校验开关走进程级快照，保存即生效
    crate::config::set_security_captcha(config.security.captcha_enabled);
    match state.config_manager.update(config).await {
        Ok(()) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": security_policy_json(&updated.security),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save security policy: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct OauthConfigRequest {
    pub github: Option<OauthProviderFields>,
    pub google: Option<OauthProviderFields>,
    pub linuxdo: Option<OauthProviderFields>,
}

#[derive(Debug, Deserialize)]
pub struct OauthProviderFields {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub redirect_uri: Option<String>,
}

/// client_secret 展示脱敏：已配置返回 "..."（前端无消费方，
/// 拿到原值也没有用途，避免浏览器网络面板/日志泄露凭据）
fn mask_secret(s: &str) -> &str {
    if s.is_empty() {
        ""
    } else {
        "..."
    }
}

/// 获取 GitHub / Google / LinuxDO OAuth 配置与就绪状态
pub async fn handle_get_oauth_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "github": {
                "client_id": config.github_oauth.client_id,
                "client_secret": mask_secret(&config.github_oauth.client_secret),
                "redirect_uri": config.github_oauth.redirect_uri,
                "ready": config.github_oauth.ready(),
            },
            "google": {
                "client_id": config.google_oauth.client_id,
                "client_secret": mask_secret(&config.google_oauth.client_secret),
                "redirect_uri": config.google_oauth.redirect_uri,
                "ready": config.google_oauth.ready(),
            },
            "linuxdo": {
                "client_id": config.linuxdo_oauth.client_id,
                "client_secret": mask_secret(&config.linuxdo_oauth.client_secret),
                "redirect_uri": config.linuxdo_oauth.redirect_uri,
                "ready": config.linuxdo_oauth.ready(),
            },
        }
    })))
}

/// 脱敏占位符守卫：GET 返回 "..."，前端若原样提交会毁掉真实 secret，
/// 与 notify 的 *** / ... 守卫同语义。
fn is_masked_secret(v: &str) -> bool {
    v.trim() == "..." || v.contains("***")
}

/// 更新 GitHub / Google / LinuxDO OAuth 配置（未提供的字段保持原值）
pub async fn handle_update_oauth_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<OauthConfigRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(g) = body.github {
        if let Some(v) = g.client_id {
            config.github_oauth.client_id = v;
        }
        if let Some(v) = g.client_secret {
            if !is_masked_secret(&v) {
                config.github_oauth.client_secret = v;
            }
        }
        if let Some(v) = g.redirect_uri {
            config.github_oauth.redirect_uri = v;
        }
    }
    if let Some(g) = body.google {
        if let Some(v) = g.client_id {
            config.google_oauth.client_id = v;
        }
        if let Some(v) = g.client_secret {
            if !is_masked_secret(&v) {
                config.google_oauth.client_secret = v;
            }
        }
        if let Some(v) = g.redirect_uri {
            config.google_oauth.redirect_uri = v;
        }
    }
    if let Some(g) = body.linuxdo {
        if let Some(v) = g.client_id {
            config.linuxdo_oauth.client_id = v;
        }
        if let Some(v) = g.client_secret {
            if !is_masked_secret(&v) {
                config.linuxdo_oauth.client_secret = v;
            }
        }
        if let Some(v) = g.redirect_uri {
            config.linuxdo_oauth.redirect_uri = v;
        }
    }
    match state.config_manager.update(config).await {
        Ok(_) => {
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": {
                    "github": {
                        "client_id": updated.github_oauth.client_id,
                        "client_secret": mask_secret(&updated.github_oauth.client_secret),
                        "redirect_uri": updated.github_oauth.redirect_uri,
                        "ready": updated.github_oauth.ready(),
                    },
                    "google": {
                        "client_id": updated.google_oauth.client_id,
                        "client_secret": mask_secret(&updated.google_oauth.client_secret),
                        "redirect_uri": updated.google_oauth.redirect_uri,
                        "ready": updated.google_oauth.ready(),
                    },
                    "linuxdo": {
                        "client_id": updated.linuxdo_oauth.client_id,
                        "client_secret": mask_secret(&updated.linuxdo_oauth.client_secret),
                        "redirect_uri": updated.linuxdo_oauth.redirect_uri,
                        "ready": updated.linuxdo_oauth.ready(),
                    },
                }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save oauth config: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// GET /api/gateway/prompt —— 网关系统提示词（对照 Agent2API「设置 → 网关 → 系统提示词」）。
pub async fn handle_get_gateway_prompt(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let cfg = state.gateway_prompt.snapshot();
    Ok(Json(json!({
        "success": true,
        "data": {
            "mode": cfg.mode,
            "text": cfg.text,
            "models": cfg.models,
            "developer_role_compat": cfg.developer_role_compat,
        }
    })))
}

/// PUT /api/gateway/prompt —— 保存系统提示词（热生效：下一请求即按新策略注入）。
pub async fn handle_update_gateway_prompt(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    // 读-改-写：未出现的字段保留现值
    let mut cfg = state.gateway_prompt.snapshot().as_ref().clone();
    if let Some(mode) = body.get("mode").and_then(|m| m.as_str()) {
        match crate::prompt::PromptMode::parse(mode) {
            Some(v) => cfg.mode = v,
            None => {
                return Err(error_response(
                    &format!("无效的提示词模式：{mode}（可选 passthrough / custom / append）"),
                    StatusCode::BAD_REQUEST,
                ))
            }
        }
    }
    if let Some(text) = body.get("text").and_then(|t| t.as_str()) {
        if text.chars().count() > 64_000 {
            return Err(error_response(
                "提示词过长（上限 64000 字符）",
                StatusCode::BAD_REQUEST,
            ));
        }
        cfg.text = text.to_string();
    }
    if let Some(v) = body.get("developer_role_compat").and_then(|v| v.as_bool()) {
        cfg.developer_role_compat = v;
    }
    // 按模型覆盖：整块替换（前端提交完整表；键 = 模型名，null/空文本 = 删除该项）
    if let Some(models) = body.get("models") {
        if !models.is_object() && !models.is_null() {
            return Err(error_response("models 必须是对象", StatusCode::BAD_REQUEST));
        }
        let mut next = std::collections::BTreeMap::new();
        if let Some(map) = models.as_object() {
            for (model, value) in map {
                if value.is_null() {
                    continue;
                }
                let mode = value
                    .get("mode")
                    .and_then(|m| m.as_str())
                    .and_then(crate::prompt::PromptMode::parse)
                    .unwrap_or(crate::prompt::PromptMode::Passthrough);
                let text = value
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                if text.chars().count() > 64_000 {
                    return Err(error_response(
                        &format!("模型 {model} 的提示词过长（上限 64000 字符）"),
                        StatusCode::BAD_REQUEST,
                    ));
                }
                next.insert(
                    model.clone(),
                    crate::prompt::ModelPromptOverride { mode, text },
                );
            }
        }
        cfg.models = next;
    }

    state.gateway_prompt.update(cfg.clone()).map_err(|e| {
        error_response(
            &format!("提示词持久化失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    tracing::info!(
        "网关系统提示词已更新: mode={} 长度 {}",
        cfg.mode.as_str(),
        cfg.text.chars().count()
    );

    Ok(Json(json!({
        "success": true,
        "data": {
            "mode": cfg.mode,
            "text": cfg.text,
            "developer_role_compat": cfg.developer_role_compat,
            "models": cfg.models,
        }
    })))
}

/// 脱敏请求头名单的持久化键。
pub const SANITIZE_HEADERS_KEY: &str = "sanitize_header_blacklist";

/// 用户脱敏词表的持久化键（SPEC-1.0.49 §8）。
pub const SANITIZE_RULES_KEY: &str = "sanitize_rules";

/// 脱敏 provider 作用域的持久化键（SPEC-1.0.49 §8）。
///
/// 与 A2A 的 `sanitizeFingerprintRules` / `sanitizeFingerprintProviders` 同构：
/// 词表与作用域是**两个键**，作用域是顶层开关而不是逐条规则字段。
pub const SANITIZE_PROVIDERS_KEY: &str = "sanitize_providers";

/// GET /api/sanitize —— 读取「额外脱敏请求头」名单 + 用户脱敏词表
/// （对照 Agent2API 设置 → 网关 → 指纹脱敏）。
///
/// 只返回管理员额外补充的部分；内置名单（authorization/cookie/x-*-token 等）
/// 由前端固定展示为只读默认项。`rules` 语义（SPEC-1.0.49 §8）：`null` = 未配置
/// （走内置硬编码规则）；`[]` = 关闭全部脱敏；非空数组 = 只用用户规则。
pub async fn handle_get_sanitize(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "extra_headers": crate::config::extra_sensitive_headers(),
            "rules": crate::sanitize::rules(),
            "providers": crate::sanitize::providers(),
            "stats": crate::sanitize::snapshot(),
        }
    })))
}

/// PUT /api/sanitize —— 保存额外脱敏头名单 + 用户脱敏词表（热生效：下一请求即按
/// 新配置脱敏）。
///
/// `rules` 字段语义（SPEC-1.0.49 §8）：**键缺省** = 不改动现有词表（兼容只发
/// `extra_headers` 的旧前端）；`null` = 恢复内置；`[]` = 关闭脱敏；数组 = 用户
/// 词表（上限 200 条，超限报错而不静默截断）。
pub async fn handle_update_sanitize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let list: Vec<String> = body
        .get("extra_headers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| item.as_str())
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    if list.len() > 200 {
        return Err(error_response(
            "额外脱敏头最多 200 条",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 头名合法性：只允许 token 字符与尾部通配 `*`，避免写入无法匹配的垃圾项。
    for name in &list {
        let body_part = name.trim_end_matches('*');
        if body_part.is_empty()
            || !body_part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(error_response(
                &format!("非法的请求头名：{name}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }

    // 落盘 + 进程级快照（两处都写：重启后从盘恢复，运行期走原子快照）
    state
        .alert_store
        .put(SANITIZE_HEADERS_KEY, &list)
        .map_err(|e| {
            error_response(
                &format!("Failed to save sanitize headers: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    crate::config::set_extra_sensitive_headers(list.clone());
    tracing::info!("脱敏请求头名单已更新: {} 条", list.len());

    // 用户脱敏词表（SPEC-1.0.49 §8）：键缺省 = 不动；null = 内置；数组 = 用户词表。
    if let Some(rules_value) = body.get("rules") {
        let rules: Option<Vec<crate::sanitize::SanitizeRule>> = match rules_value {
            Value::Null => None,
            Value::Array(_) => {
                let parsed = serde_json::from_value::<Vec<crate::sanitize::SanitizeRule>>(
                    rules_value.clone(),
                )
                .map_err(|e| {
                    error_response(&format!("脱敏规则格式非法：{e}"), StatusCode::BAD_REQUEST)
                })?;
                Some(parsed)
            }
            _ => {
                return Err(error_response(
                    "rules 必须是数组或 null",
                    StatusCode::BAD_REQUEST,
                ));
            }
        };
        // 校验 + 落快照：超 200 条在此返回 Err（400），不会静默截断。
        crate::sanitize::set_rules(rules.clone())
            .map_err(|e| error_response(&e, StatusCode::BAD_REQUEST))?;
        state
            .alert_store
            .put(SANITIZE_RULES_KEY, &rules)
            .map_err(|e| {
                error_response(
                    &format!("Failed to save sanitize rules: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
        tracing::info!(
            "脱敏词表已更新: {}",
            rules
                .as_ref()
                .map(|r| format!("{} 条", r.len()))
                .unwrap_or_else(|| "内置".to_string())
        );
    }

    // 脱敏 provider 作用域（SPEC-1.0.49 §8）：键缺省 = 不动；`[]` = 全部 provider；
    // 数组 = 只对这些家脱敏（判定在数据面「即将发送的那一家」处生效）。
    if let Some(providers_value) = body.get("providers") {
        let providers: Vec<String> = match providers_value {
            Value::Array(arr) => {
                let mut out: Vec<String> = Vec::new();
                for item in arr {
                    let Some(raw) = item.as_str() else {
                        return Err(error_response(
                            "providers 必须是字符串数组",
                            StatusCode::BAD_REQUEST,
                        ));
                    };
                    let name = raw.trim();
                    // 去空串 + 去重：空串永远不可能匹配任何家，留着只会误导。
                    if !name.is_empty() && !out.iter().any(|p| p.eq_ignore_ascii_case(name)) {
                        out.push(name.to_string());
                    }
                }
                out
            }
            _ => {
                return Err(error_response(
                    "providers 必须是数组",
                    StatusCode::BAD_REQUEST,
                ));
            }
        };
        crate::sanitize::set_providers(providers.clone());
        state
            .alert_store
            .put(SANITIZE_PROVIDERS_KEY, &providers)
            .map_err(|e| {
                error_response(
                    &format!("Failed to save sanitize providers: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
        tracing::info!(
            "脱敏 provider 作用域已更新: {}",
            if providers.is_empty() {
                "全部".to_string()
            } else {
                providers.join(" / ")
            }
        );
    }

    Ok(Json(json!({
        "success": true,
        // 保存回显带上 stats：前端保存后会拿本次响应覆盖整个面板状态，
        // 这里不返回的话命中计数会在保存后凭空消失（下一次刷新才回来）。
        "data": {
            "extra_headers": list,
            "rules": crate::sanitize::rules(),
            "providers": crate::sanitize::providers(),
            "stats": crate::sanitize::snapshot()
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_settings_json_shape_is_camel_case() {
        let cfg = crate::config::ChatConfig {
            auto_title: true,
            title_model: "gpt-4o-mini".to_string(),
            title_channel: "ch-1".to_string(),
            title_prompt_chars: 300,
        };
        assert_eq!(
            chat_settings_json(&cfg),
            json!({
                "autoTitle": true,
                "titleModel": "gpt-4o-mini",
                "titleChannel": "ch-1",
                "titlePromptChars": 300,
            })
        );
    }

    #[test]
    fn chat_settings_request_accepts_snake_case_partial_body() {
        // 契约：PUT body 为 snake_case，缺省字段保持原值（None）。
        let body: ChatSettingsRequest =
            serde_json::from_str(r#"{"auto_title": true, "title_prompt_chars": 800}"#).unwrap();
        assert_eq!(body.auto_title, Some(true));
        assert_eq!(body.title_prompt_chars, Some(800));
        assert_eq!(body.title_model, None);
        assert_eq!(body.title_channel, None);
    }

    /// 报表设置响应形状：三个"口径"字段语义不同，谁等谁不等决定 `needsMigration`。
    ///
    /// 断言一律拿 `report_tz_offset()` 现值当基准，**不写死 8** —— 快照是进程级
    /// 原子，写死数值等于假设"本测试二进制里没人改过它"，那是会随测试顺序烂掉的
    /// 间歇红（同 #20 批1 的 offset=0 教训）。当前全仓只有 `main.rs` 启动注入与
    /// 本端点会写它，二者都不在单元测试二进制里跑，故现值恒等于缺省。
    #[test]
    fn report_settings_json_shape_and_migration_flag() {
        use crate::config::{ReportConfig, REPORT_TZ_OFFSET_MAX, REPORT_TZ_OFFSET_MIN};
        let live = crate::config::report_tz_offset();
        let cfg = ReportConfig {
            timezone_offset: live,
        };

        let same = report_settings_json(&cfg, Some(live));
        assert_eq!(same["timezoneOffset"], json!(live));
        assert_eq!(same["effectiveOffset"], json!(live));
        assert_eq!(
            same["needsMigration"],
            json!(false),
            "存量口径 = 生效口径 ⇒ 不该催迁移"
        );

        // 存量行停在别的口径（含老库 None）⇒ 一律提示需要迁移
        let stale = report_settings_json(&cfg, Some(live + 1));
        assert_eq!(stale["needsMigration"], json!(true));
        assert_eq!(stale["storedOffset"], json!(live + 1));
        let legacy = report_settings_json(&cfg, None);
        assert_eq!(
            legacy["needsMigration"],
            json!(true),
            "老库无口径标记，保守按需要迁移（迁移幂等，空库只落标记）"
        );
        assert!(legacy["storedOffset"].is_null());

        // 边界随响应一起吐给前端，免得前端自己抄一份常量
        assert_eq!(same["minOffset"], json!(REPORT_TZ_OFFSET_MIN));
        assert_eq!(same["maxOffset"], json!(REPORT_TZ_OFFSET_MAX));
    }

    /// 契约：PUT body 主用 snake_case（与 chat/agent 面一致），camelCase 只作别名
    /// 兼容前端手写；缺省字段保持原值。两种拼法都钉住，否则改一边就把另一端打挂。
    #[test]
    fn report_settings_request_accepts_both_cases_and_partial_body() {
        let snake: ReportSettingsRequest =
            serde_json::from_str(r#"{"timezone_offset": -3}"#).unwrap();
        assert_eq!(snake.timezone_offset, Some(-3));
        let camel: ReportSettingsRequest =
            serde_json::from_str(r#"{"timezoneOffset": 14}"#).unwrap();
        assert_eq!(camel.timezone_offset, Some(14));
        let empty: ReportSettingsRequest = serde_json::from_str(r#"{}"#).unwrap();
        assert_eq!(
            empty.timezone_offset, None,
            "缺省字段必须保持原值，不能被零值覆盖"
        );
    }

    #[test]
    fn validate_chat_combo_rejects_zero_with_auto_title() {
        // 危险组合（开自动标题 + 截断 0）必须拒；其余组合原样放行。
        let zero_off = crate::config::ChatConfig {
            auto_title: false,
            title_prompt_chars: 0,
            ..Default::default()
        };
        let zero_on = crate::config::ChatConfig {
            auto_title: true,
            title_prompt_chars: 0,
            ..Default::default()
        };
        let one_on = crate::config::ChatConfig {
            auto_title: true,
            title_prompt_chars: 1,
            ..Default::default()
        };
        assert!(
            validate_chat_combo(&zero_off).is_ok(),
            "关闭自动标题时 0 无碍"
        );
        assert!(validate_chat_combo(&zero_on).is_err(), "危险组合必须 400");
        assert!(validate_chat_combo(&one_on).is_ok());
    }

    #[tokio::test]
    async fn update_config_manager_hot_effect_and_persist() {
        // PUT 的热生效机制等价验证：config_manager.update 写内存 + 原子落盘，
        // 之后 get()（title.rs 每次生成标题前的取法）拿到的必须是最新值。
        let dir = tempfile::TempDir::new().unwrap();
        let cm = crate::config::ConfigManager::new(Some(dir.path().join("config.toml"))).await;
        let mut cfg = cm.get().await;
        assert!(!cfg.chat.auto_title, "默认必须是关闭");
        cfg.chat.auto_title = true;
        cfg.chat.title_model = "deepseek-chat".to_string();
        cfg.chat.title_prompt_chars = 123;
        cm.update(cfg).await.unwrap();

        let after = cm.get().await;
        assert!(after.chat.auto_title);
        assert_eq!(after.chat.title_model, "deepseek-chat");
        assert_eq!(after.chat.title_prompt_chars, 123);

        // 重启口径：从磁盘重载仍须是同一份值（config.toml 已持久化）。
        let reloaded =
            crate::config::ConfigManager::new(Some(dir.path().join("config.toml"))).await;
        let disk = reloaded.get().await;
        assert!(disk.chat.auto_title);
        assert_eq!(disk.chat.title_prompt_chars, 123);
    }
}
