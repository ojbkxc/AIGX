//! 模型定价管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供模型定价的查询、删除功能，以及价格同步和汇率更新。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

// 这里需要引用主 crate 的定价相关类型
use crate::pricing::ModelPrice;
use crate::token_estimate;

#[derive(Debug, Deserialize)]
pub struct PriceRequest {
    pub model_name: String,
    #[serde(default)]
    pub input_price: f64,
    #[serde(default)]
    pub output_price: f64,
    #[serde(default)]
    pub cache_price: Option<f64>,
    #[serde(default = "default_price_type")]
    pub price_type: String,
}

fn default_price_type() -> String {
    "token".to_string()
}

impl PriceRequest {
    pub(crate) fn to_model_price(&self) -> ModelPrice {
        let now = chrono::Utc::now().timestamp();
        ModelPrice {
            model_name: self.model_name.clone(),
            input_price: self.input_price,
            output_price: self.output_price,
            cache_price: self.cache_price,
            price_type: self.price_type.clone(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// 列出所有模型定价
pub async fn handle_list_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let prices: Vec<Value> = state
        .pricing_store
        .list_prices()
        .iter()
        .map(|p| {
            json!({
                "model_name": p.model_name,
                "input_price": p.input_price,
                "output_price": p.output_price,
                "cache_price": p.cache_price,
                "price_type": p.price_type,
                "created_at": p.created_at,
                "updated_at": p.updated_at,
            })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": prices })))
}

/// 添加模型定价
pub async fn handle_add_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PriceRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mp = body.to_model_price();
    match state.pricing_store.upsert_price(mp.clone()) {
        Ok(_) => Ok(Json(json!({
            "success": true,
            "data": json!({
                "model_name": mp.model_name,
                "input_price": mp.input_price,
                "output_price": mp.output_price,
                "cache_price": mp.cache_price,
                "price_type": mp.price_type,
                "created_at": mp.created_at,
                "updated_at": mp.updated_at,
            })
        }))),
        Err(e) => Err(error_response(
            &format!("Failed to add pricing: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 删除模型定价
pub async fn handle_delete_pricing(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model_name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.pricing_store.delete_price(&model_name) {
        Ok(_) => Ok(Json(json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete pricing: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

// ── 成本预估器（P1）─────────────────────────────────────────────────

/// 成本预估请求（B2）。
///
/// 两种用法，后端兼容并存：
/// - **消息形态（推荐）**：只传 `model` + `messages`（OpenAI wire 形状），
///   网关侧用 `token_estimate::count_chat_prompt` 估算 prompt token——
///   口径与数据面预留计费完全一致，前端无需自带 tokenizer。
/// - **token 形态（旧客户端）**：传 `input_tokens` / `output_tokens` 直接计价。
#[derive(Debug, Deserialize)]
pub struct CostEstimateRequest {
    pub model: String,
    #[serde(default)]
    pub messages: Option<Vec<Value>>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default = "default_group")]
    pub group: String,
}

fn default_group() -> String {
    "default".to_string()
}

/// 从请求体解析预估 token 对（纯逻辑，供 handler 与单测复用）。
///
/// 消息形态：`input` 为 `count_chat_prompt` 的精确估算，
/// `output` 默认 256（与数据面预留计费的保守 completion 估算一致）。
fn resolve_estimate_tokens(body: &CostEstimateRequest) -> Result<(u64, u64, &'static str), String> {
    if let Some(raw_messages) = &body.messages {
        let messages =
            super::super::openai::parse_messages(Some(&Value::Array(raw_messages.clone())))
                .unwrap_or_default();
        if messages.is_empty() {
            return Err("no valid messages in estimate request".to_string());
        }
        let chat = crate::bridge::ChatFormat {
            model: body.model.clone(),
            messages,
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let prompt = token_estimate::count_chat_prompt(&body.model, &chat);
        Ok((
            u64::from(prompt),
            body.output_tokens.unwrap_or(256),
            "messages",
        ))
    } else {
        match (body.input_tokens, body.output_tokens) {
            (Some(input), Some(output)) => Ok((input, output, "explicit")),
            (Some(input), None) => Ok((input, 256, "explicit")),
            _ => Err("provide either messages or input_tokens/output_tokens".to_string()),
        }
    }
}

/// POST /api/pricing/estimate - 成本预估器（P1）
///
/// 根据模型、token 数量和用户分组预估请求成本。
/// 优先使用原始 `messages` 做网关侧 token 估算；否则回退到显式 token 数。
/// 用于 Playground/聊天输入实时显示预计消耗。
pub async fn handle_cost_estimate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CostEstimateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = super::common::verify_user(&state, &headers).await?;

    // 与数据面一致：优先按原始消息估算 prompt token。
    let (input_tokens, output_tokens, token_source) = match resolve_estimate_tokens(&body) {
        Ok(v) => v,
        Err(e) => {
            return Err(error_response(&e, StatusCode::BAD_REQUEST));
        }
    };

    let price = match state.pricing_store.get_price(&body.model) {
        Some(p) => p,
        None => {
            return Err(error_response(
                &format!("Model '{}' not found in pricing catalog", body.model),
                StatusCode::NOT_FOUND,
            ));
        }
    };

    let cost = match state.pricing_store.calculate_cost(
        &body.model,
        input_tokens,
        output_tokens,
        &body.group,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Err(error_response(
                &format!("Failed to calculate cost: {e}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    };

    let cost_quoted = match state.pricing_store.calculate_cost_quoted(
        &body.model,
        input_tokens,
        output_tokens,
        &body.group,
    ) {
        Ok(c) => c,
        Err(e) => {
            return Err(error_response(
                &format!("Failed to calculate quoted cost: {e}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    };

    Ok(Json(json!({
        "success": true,
        "data": {
            "model": body.model,
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "token_source": token_source,
            "group": body.group,
            "cost_usd": cost,
            "cost_quoted": cost_quoted,
            "pricing": {
                "input_price": price.input_price,
                "output_price": price.output_price,
                "cache_price": price.cache_price,
                "price_type": price.price_type,
            }
        }
    })))
}

// ── 缺失模型检测（P1「模型元信息同步」）──────────────────────────────

/// GET /api/pricing/missing-models - 检测未配定价的可用模型
///
/// 聚合所有启用渠道声明/发现的模型（含别名映射对外名），
/// 与定价目录对比，返回缺少定价条目的模型及其推断元信息。
/// 用于管理端「漏配价提醒」——B09 语义下未配价模型请求会被拒，
/// 此端点帮助管理员及时发现漏配。
pub async fn handle_missing_pricing_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    let mut seen = std::collections::HashSet::new();
    // (model, channel_owned_by) 有序去重
    let mut candidates: Vec<(String, Option<&'static str>)> = Vec::new();
    for ch in state.channel_store.list() {
        if !ch.is_enabled() {
            continue;
        }
        let owned_by = crate::model::metadata::owned_by_for_channel_type(ch.channel_type.as_str());
        for m in ch.models.iter().chain(ch.discovered_models.iter()) {
            if !m.is_empty() && seen.insert(m.clone()) {
                candidates.push((m.clone(), owned_by));
            }
        }
    }
    for name in state.model_mapper.all_mappings().keys() {
        if seen.insert(name.clone()) {
            candidates.push((name.clone(), None));
        }
    }

    let missing: Vec<Value> = candidates
        .into_iter()
        .filter(|(m, _)| state.pricing_store.get_price(m).is_none())
        .map(|(m, owned_by)| {
            let meta = state.model_metadata.get(&m, owned_by);
            json!({
                "model": m,
                "owned_by": meta.owned_by,
                "context_length": meta.context_length,
                "capabilities": meta.capabilities,
            })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "count": missing.len(),
            "missing": missing,
        }
    })))
}

// ── 模型元信息覆盖管理（P1）──────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct ModelMetaRequest {
    pub owned_by: String,
    #[serde(default)]
    pub context_length: Option<u32>,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// GET /api/models/meta - 全部模型元信息覆盖项
pub async fn handle_model_meta_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let overrides = state.model_metadata.all_overrides();
    Ok(Json(json!({ "success": true, "data": overrides })))
}

/// PUT /api/models/meta/:model - 设置某模型的元信息覆盖
pub async fn handle_model_meta_set(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
    Json(body): Json<ModelMetaRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let meta = crate::model::metadata::ModelMetadata {
        owned_by: body.owned_by,
        context_length: body.context_length,
        capabilities: body.capabilities,
    };
    match state
        .model_metadata
        .set_override(model.clone(), meta.clone())
    {
        Ok(()) => Ok(Json(json!({ "success": true, "data": meta }))),
        Err(e) => Err(error_response(
            &format!("Failed to save model metadata: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// DELETE /api/models/meta/:model - 删除某模型的元信息覆盖
pub async fn handle_model_meta_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(model): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.model_metadata.remove_override(&model) {
        Ok(()) => Ok(Json(json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete model metadata: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_messages(msgs: Value) -> CostEstimateRequest {
        CostEstimateRequest {
            model: "gpt-4".to_string(),
            messages: Some(msgs.as_array().unwrap().clone()),
            input_tokens: None,
            output_tokens: None,
            group: "default".to_string(),
        }
    }

    #[test]
    fn messages_form_estimates_prompt() {
        let req = req_with_messages(json!([
            {"role": "user", "content": "Hello"}
        ]));
        let (input, output, source) = resolve_estimate_tokens(&req).unwrap();
        // 3 (per-message) + 1 ("user") + 1 ("Hello") + 3 (reply priming) = 8
        assert_eq!(input, 8);
        assert_eq!(output, 256);
        assert_eq!(source, "messages");
    }

    #[test]
    fn messages_form_honors_explicit_output() {
        let mut req = req_with_messages(json!([
            {"role": "user", "content": "Hello"}
        ]));
        req.output_tokens = Some(64);
        let (input, output, _) = resolve_estimate_tokens(&req).unwrap();
        assert_eq!(input, 8);
        assert_eq!(output, 64);
    }

    #[test]
    fn messages_form_rejects_invalid_roles() {
        let req = req_with_messages(json!([
            {"role": "nonsense", "content": "x"}
        ]));
        assert!(resolve_estimate_tokens(&req).is_err());
    }

    #[test]
    fn explicit_tokens_pass_through() {
        let req = CostEstimateRequest {
            model: "gpt-4".to_string(),
            messages: None,
            input_tokens: Some(100),
            output_tokens: Some(50),
            group: "default".to_string(),
        };
        let (input, output, source) = resolve_estimate_tokens(&req).unwrap();
        assert_eq!((input, output, source), (100, 50, "explicit"));
    }

    #[test]
    fn explicit_input_only_defaults_output() {
        let req = CostEstimateRequest {
            model: "gpt-4".to_string(),
            messages: None,
            input_tokens: Some(100),
            output_tokens: None,
            group: "default".to_string(),
        };
        let (input, output, _) = resolve_estimate_tokens(&req).unwrap();
        assert_eq!(input, 100);
        assert_eq!(output, 256);
    }

    #[test]
    fn empty_request_rejected() {
        let req = CostEstimateRequest {
            model: "gpt-4".to_string(),
            messages: None,
            input_tokens: None,
            output_tokens: None,
            group: "default".to_string(),
        };
        assert!(resolve_estimate_tokens(&req).is_err());
    }
}
