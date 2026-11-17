// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 特殊路由（压缩 + 标题）管理面。对照 A2A `api/compact_routes.rs`、
//! `api/title_routes.rs`：GET 返回当前配置 + 可选模型 + 修订号；PUT 整块保存
//! （修订号 CAS：不符 → 409）。客户端名单由后端下发，前端不各写一份。
//!
//! ── AIGX 侧差异 ───────────────────────────────────────────────
//! · 模型清单来自 `state.model_mapper` 的活动别名 + 启用渠道的模型（AIGX 的
//!   `collect_available_models` 是 `openai` 私有模块、管理面不可达），故此处
//!   按任务许可的「退回模型清单」口径聚合，不新造目录。
//! · A2A 用 `catalog::session_models` 过滤掉 image 类；AIGX 的聚合清单无
//!   kind 字段，不做该过滤（记账/可用性由选路侧把关）。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::route_special::{self, compact, title};

/// 可用模型清单：活动别名 + 启用渠道声明的模型（大小写不敏感去重）。
fn available_model_ids(state: &AppState) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for name in state.model_mapper.active_mappings().keys() {
        if name.is_empty() || !seen.insert(name.to_ascii_lowercase()) {
            continue;
        }
        out.push(name.clone());
    }
    for channel in state.channel_store.list() {
        if !channel.is_enabled() {
            continue;
        }
        for model in channel.effective_models() {
            if model.is_empty() || !seen.insert(model.to_ascii_lowercase()) {
                continue;
            }
            out.push(model.clone());
        }
    }
    out
}

fn models_view(state: &AppState) -> Vec<Value> {
    available_model_ids(state)
        .into_iter()
        .map(|id| json!({ "id": id }))
        .collect()
}

fn is_available(ids: &[String], name: &str) -> bool {
    let key = compact::model_key(name);
    ids.iter().any(|m| compact::model_key(m) == key)
}

/// 保存错误 → HTTP 响应。
fn save_error(error: route_special::SaveError) -> (StatusCode, Json<Value>) {
    match error {
        route_special::SaveError::Conflict(message) => {
            error_response(&message, StatusCode::CONFLICT)
        }
        route_special::SaveError::BadRequest(message) => {
            error_response(&message, StatusCode::BAD_REQUEST)
        }
        route_special::SaveError::Internal(message) => {
            error_response(&message, StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

// ── 压缩路由 ──────────────────────────────────────────────────────────

fn compact_view(state: &AppState) -> Value {
    let snapshot = route_special::compact_current();
    let (config, error) = match snapshot.settings.as_ref() {
        Ok(settings) => (
            serde_json::to_value(settings).unwrap_or(Value::Null),
            Value::Null,
        ),
        Err(message) => (Value::Null, Value::String(message.clone())),
    };
    json!({
        "config": config,
        "error": error,
        "revision": snapshot.revision,
        "clients": [{ "id": route_special::CLIENT_ZCODE, "label": "ZCode" }],
        "models": models_view(state),
        "maxRules": compact::MAX_COMPACT_ROUTES,
    })
}

/// GET /api/route-special/compact
pub async fn handle_get_compact_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(
        json!({ "success": true, "data": compact_view(&state) }),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveCompactRequest {
    config: compact::CompactRoutes,
    revision: String,
}

/// 模型存在性校验：允许保留「上次已保存但当前不可用」的模型（否则用户连关闭
/// 都存不下去）。
fn validate_compact_models(
    state: &AppState,
    settings: &compact::CompactRoutes,
) -> Result<(), String> {
    let ids = available_model_ids(state);
    let previous = route_special::compact_current();
    let prev = previous.settings.as_ref().ok();
    for rule in &settings.rules {
        let existing = prev.and_then(|old| {
            old.rules.iter().find(|item| {
                item.client == rule.client
                    && compact::model_key(&item.source) == compact::model_key(&rule.source)
            })
        });
        if !is_available(&ids, &rule.source) && existing.is_none() {
            return Err(format!(
                "来源模型「{}」当前不可用，请刷新模型列表后重试",
                rule.source
            ));
        }
        // 已失效的旧规则允许保留、关闭或删除，但不能把失效目标当作新选择或重新启用。
        let preserve_unavailable = existing.is_some_and(|old| {
            compact::model_key(&old.target) == compact::model_key(&rule.target)
                && (!settings.enabled
                    || !rule.enabled
                    || (old.enabled && prev.is_some_and(|config| config.enabled)))
        });
        if !is_available(&ids, &rule.target) && !preserve_unavailable {
            return Err(format!(
                "压缩模型「{}」当前不可用，请重新选择或关闭该规则",
                rule.target
            ));
        }
    }
    Ok(())
}

/// PUT /api/route-special/compact
pub async fn handle_put_compact_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let payload = serde_json::from_value::<SaveCompactRequest>(payload).map_err(|error| {
        error_response(
            &format!("压缩路由设置不完整：{error}"),
            StatusCode::BAD_REQUEST,
        )
    })?;
    validate_compact_models(&state, &payload.config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    route_special::save_compact(payload.config, &payload.revision).map_err(save_error)?;
    Ok(Json(
        json!({ "success": true, "data": compact_view(&state) }),
    ))
}

// ── 标题路由 ──────────────────────────────────────────────────────────

fn title_view(state: &AppState) -> Value {
    let snapshot = route_special::title_current();
    let (config, error) = match snapshot.settings.as_ref() {
        Ok(settings) => (
            serde_json::to_value(settings).unwrap_or(Value::Null),
            Value::Null,
        ),
        Err(message) => (Value::Null, Value::String(message.clone())),
    };
    json!({
        "config": config,
        "error": error,
        "revision": snapshot.revision,
        "clients": route_special::CLIENTS.iter().map(|(id, label)| json!({
            "id": id, "label": label, "supported": true,
        })).collect::<Vec<_>>(),
        "models": models_view(state),
        "fallbackModes": ["original", "error"],
        "maxRules": title::MAX_RULES,
    })
}

/// GET /api/route-special/title
pub async fn handle_get_title_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({ "success": true, "data": title_view(&state) })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveTitleRequest {
    config: title::TitleRoutes,
    revision: String,
}

/// 模型存在性校验：允许保留上次已保存的（失效也允许原样保存，好让用户改回）。
fn validate_title_models(state: &AppState, settings: &title::TitleRoutes) -> Result<(), String> {
    let ids = available_model_ids(state);
    let previous = route_special::title_current();
    let before: Vec<String> = previous
        .settings
        .as_ref()
        .map(|old| {
            old.rules
                .iter()
                .map(|rule| title::model_key(&rule.model))
                .collect()
        })
        .unwrap_or_default();
    for (index, rule) in settings.rules.iter().enumerate() {
        let model = rule.model.trim();
        if model.is_empty() {
            continue;
        }
        if is_available(&ids, model) || before.contains(&title::model_key(model)) {
            continue;
        }
        return Err(format!(
            "第 {} 条规则的标题模型「{model}」当前不可用，请刷新模型列表后重试",
            index + 1
        ));
    }
    Ok(())
}

/// PUT /api/route-special/title
pub async fn handle_put_title_routes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let payload = serde_json::from_value::<SaveTitleRequest>(payload).map_err(|error| {
        error_response(
            &format!("标题生成路由设置不完整：{error}"),
            StatusCode::BAD_REQUEST,
        )
    })?;
    validate_title_models(&state, &payload.config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    route_special::save_title(payload.config, &payload.revision).map_err(save_error)?;
    Ok(Json(json!({ "success": true, "data": title_view(&state) })))
}
