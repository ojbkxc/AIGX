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
