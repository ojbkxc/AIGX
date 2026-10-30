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
    pub mappings: std::collections::HashMap<String, String>,
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
    pub max_retries: Option<u32>,
    pub retry_interval_ms: Option<u64>,
    pub max_request_body_mb: Option<usize>,
    /// 请求日志快照开关（log_body）
    pub log_body: Option<bool>,
    /// 按次计费每次固定扣减额度（billing_mode=count 的订阅生效）
    pub billing_flat_quota: Option<i64>,
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
            "max_retries": config.usage.max_retries,
            "retry_interval_ms": config.usage.retry_interval_ms,
            "max_request_body_mb": config.usage.max_request_body_mb,
            "log_body": config.usage.log_body,
            "billing_flat_quota": config.usage.billing_flat_quota,
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
    if let Some(v) = body.retry_interval_ms {
        config.usage.retry_interval_ms = v.min(10_000);
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
    if let Some(v) = body.billing_flat_quota {
        if v > 0 {
            config.usage.billing_flat_quota = v;
            // 同步进程级快照（数据面按次计费同步读取，立即生效）
            crate::config::set_billing_flat_quota(v);
        }
    }
    // 换号重试间隔立即生效（账号池运行时读取）
    state
        .account_sessions
        .set_retry_interval_ms(config.usage.retry_interval_ms);
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

fn agent_settings_json(config: &crate::config::AgentConfig) -> Value {
    let tools: Vec<Value> = crate::agent::tools::tool_specs()
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "risk": spec.risk.as_str(),
            })
        })
        .collect();
    json!({
        "enabled": config.enabled,
        "model": config.model,
        "channel": config.channel,
        "max_turns": config.max_turns,
        "approval_timeout_secs": config.approval_timeout_secs,
        "max_history": config.max_history,
        "reasoning_effort": config.reasoning_effort,
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

// ── 网关设置（监听地址 / CORS / 反代信任）──────────────────────────

/// 网关设置请求体（缺省字段保持原值）。
#[derive(Debug, Deserialize)]
pub struct GatewaySettingsRequest {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub cors_origins: Option<Vec<String>>,
    pub trust_proxy_headers: Option<bool>,
}

fn gateway_settings_json(config: &crate::config::AppConfig) -> Value {
    json!({
        "host": config.server.host,
        "port": config.server.port,
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
            // "*" 或合法 http(s) 来源；其它视为配置错误
            if origin == "*" || origin.starts_with("http://") || origin.starts_with("https://") {
                cleaned.push(origin);
            } else {
                return Err(error_response(
                    "CORS 来源必须是 http(s):// 开头或 *",
                    StatusCode::BAD_REQUEST,
                ));
            }
        }
        config.cors_origins = cleaned;
    }
    if let Some(v) = body.trust_proxy_headers {
        config.trust_proxy_headers = v;
    }
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
