//! 破甲控制台管理面（`/api/purge`）。
//!
//! 与 `prompt` / `sanitize` 同级：GET 读配置 + 进程统计，PUT 全量写配置并热生效。
//! 统计是进程内原子计数（重启归零），不做持久化——它反映的是当前进程的
//! 真实请求行为，不是历史报表。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

use crate::bridge::{ChatMessage, Role};
use crate::purge::{PurgeSettings, RewriteRule};

/// GET /api/purge —— 读取破甲配置、统计快照与进程运行信息。
pub async fn handle_get_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(payload(&state)))
}

/// PUT /api/purge —— 全量保存配置（热生效）。
///
/// 请求体既可以是配置对象本身，也可以是 `{ "settings": {...} }`；
/// 数值字段统一 clamp，空改写规则（from 为空）会被丢弃，避免页面误存无效词。
pub async fn handle_update_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let raw = body
        .get("settings")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(body);
    let parsed: PurgeSettings = serde_json::from_value(raw).map_err(|e| {
        error_response(
            &format!("Invalid purge settings: {e}"),
            StatusCode::BAD_REQUEST,
        )
    })?;

    state.purge.update(normalize(parsed)).map_err(|e| {
        error_response(
            &format!("Failed to save purge settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;

    Ok(Json(payload(&state)))
}

/// POST /api/purge/preview —— 改写试跑（dry-run）。
///
/// 请求体：`{ "messages": [{"role":"user","content":"..."}], "settings"?: {...} }`
/// - `settings` 缺省时用当前生效配置；
/// - 返回应用全部已启用治理项后的消息列表，**不写统计、不落库**。
///
/// 供运维在保存前验证改写规则 / 模板 / 技能注入的实际效果。
pub async fn handle_preview_purge(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let cfg = match body.get("settings").filter(|v| !v.is_null()) {
        Some(raw) => {
            let parsed: PurgeSettings = serde_json::from_value(raw.clone()).map_err(|e| {
                error_response(
                    &format!("Invalid purge settings: {e}"),
                    StatusCode::BAD_REQUEST,
                )
            })?;
            normalize(parsed)
        }
        None => state.purge.snapshot().as_ref().clone(),
    };

    let raw_messages = body
        .get("messages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            error_response(
                "Missing messages array: expected [{role, content}]",
                StatusCode::BAD_REQUEST,
            )
        })?;
    if raw_messages.is_empty() {
        return Err(error_response(
            "messages array must not be empty",
            StatusCode::BAD_REQUEST,
        ));
    }
    let mut messages: Vec<ChatMessage> = raw_messages
        .iter()
        .map(|m| {
            let role = match m.get("role").and_then(|v| v.as_str()).unwrap_or("user") {
                "system" => Role::System,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            let content = m
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            ChatMessage {
                role,
                content: Some(content),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }
        })
        .collect();

    let applied = crate::purge::preview_messages(&mut messages, &cfg);

    let out: Vec<Value> = messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            json!({ "role": role, "content": m.content })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "applied": { "changed": applied.changed, "skill_hit": applied.skill_hit },
            "messages": out,
        }
    })))
}

fn payload(state: &AppState) -> Value {
    let settings = state.purge.snapshot();
    json!({
        "success": true,
        "data": {
            "settings": settings.as_ref(),
            "stats": crate::purge::snapshot(),
            // 配置体检：「开了但不生效 / 自相矛盾」的静态检查结果
            "config_warnings": crate::purge::config_warnings(&settings),
            // 技能目录扫描快照（名称+关键词+正文字符数；skill_inject 配置的可验证性）
            "skills": crate::purge::skill_briefs(&settings.skill_dir),
            "runtime": {
                "pid": crate::purge::pid(),
                "uptime_secs": crate::purge::uptime_secs(),
                "upstream": settings.upstream_label,
                // 当前在途请求数（真实并发，进程内即时值；非累计计数）
                "inflight": state.inflight.len(),
            }
        }
    })
}

fn normalize(mut cfg: PurgeSettings) -> PurgeSettings {
    cfg.retry_limit = cfg.retry_limit.clamp(1, 32);
    cfg.fallback_trigger_attempt = cfg.fallback_trigger_attempt.clamp(1, 32);
    cfg.skill_inject_limit = cfg.skill_inject_limit.clamp(16, 20_000);

    cfg.reasoning_floor = trim_or(&cfg.reasoning_floor, "low");
    cfg.skill_dir = cfg.skill_dir.trim().to_string();
    if cfg.skill_dir.is_empty() {
        cfg.skill_dir = crate::purge::DEFAULT_SKILL_DIR.to_string();
    }
    cfg.upstream_label = cfg.upstream_label.trim().to_string();

    cfg.fallback_models = cfg
        .fallback_models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
        .take(16)
        .collect();

    cfg.rewrite_rules = cfg
        .rewrite_rules
        .into_iter()
        .filter_map(|rule| {
            let from = rule.from.trim().to_string();
            if from.is_empty() {
                return None;
            }
            Some(RewriteRule {
                from,
                to: rule.to.trim().to_string(),
            })
        })
        .take(200)
        .collect();

    cfg.synonym_pairs = cfg
        .synonym_pairs
        .into_iter()
        .filter_map(|rule| {
            let from = rule.from.trim().to_string();
            if from.is_empty() {
                return None;
            }
            Some(RewriteRule {
                from,
                to: rule.to.trim().to_string(),
            })
        })
        .take(200)
        .collect();

    cfg.normalize_strip_prefixes = cfg
        .normalize_strip_prefixes
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 拒绝话术词表：trim、去空、截断 64（空表语义 = 关闭字面拒绝检测，保留空表）
    cfg.refusal_patterns = cfg
        .refusal_patterns
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 失败分类词表：同上（空表 = 该类不参与字面判定）
    cfg.failure_filter_keywords = cfg
        .failure_filter_keywords
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();
    cfg.failure_empty_keywords = cfg
        .failure_empty_keywords
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

    // 结构化模板：空名丢弃、截断 32 条、文本保留原样（仅去尾随空白）
    cfg.structured_templates = cfg
        .structured_templates
        .into_iter()
        .filter_map(|tpl| {
            let name = tpl.name.trim().to_string();
            if name.is_empty() {
                return None;
            }
            Some(crate::purge::PurgeTemplate {
                name,
                text: tpl.text.trim_end().to_string(),
            })
        })
        .take(32)
        .collect();
    cfg.active_template = cfg.active_template.trim().to_string();
    // 选中的模板不存在 → 清空（回退 structured_suffix），避免静默悬空引用
    if !cfg.active_template.is_empty()
        && !cfg
            .structured_templates
            .iter()
            .any(|tpl| tpl.name == cfg.active_template)
    {
        cfg.active_template = String::new();
    }

    cfg
}

fn trim_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}
