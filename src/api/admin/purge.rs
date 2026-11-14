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

fn payload(state: &AppState) -> Value {
    let settings = state.purge.snapshot();
    json!({
        "success": true,
        "data": {
            "settings": settings.as_ref(),
            "stats": crate::purge::snapshot(),
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

    cfg.normalize_strip_prefixes = cfg
        .normalize_strip_prefixes
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .take(64)
        .collect();

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
