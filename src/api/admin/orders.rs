//! 订单管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供充值订单的创建、查询、删除功能。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin, verify_user};

// 这里需要引用主 crate 的订单相关类型
use crate::payment::{EpayConfig, TopUpOrder};
use crate::user::new_trade_no;

#[derive(Debug, Deserialize)]
pub struct TopupRequest {
    /// 充值数量（元，按 price 转换为配额）
    pub amount: i64,
    pub payment_method: String,
}

/// F02（契约2）：实付金额 = amount × discount。
fn pay_money(epay: &EpayConfig, amount: i64) -> f64 {
    let discount = *epay
        .amount_discount
        .get(&amount)
        .filter(|d| **d > 0.0)
        .unwrap_or(&1.0);
    let money = (amount as f64) * discount;
    (money * 100.0).round() / 100.0
}

/// F02（契约2）：入账配额 = amount × price × discount
fn topup_quota(epay: &EpayConfig, amount: i64) -> i64 {
    let discount = *epay
        .amount_discount
        .get(&amount)
        .filter(|d| **d > 0.0)
        .unwrap_or(&1.0);
    ((amount as f64) * epay.price * discount + 0.999999) as i64
}

/// 处理回调地址
fn callback_address(_state: &AppState, config: &crate::config::AppConfig) -> String {
    if !config.epay.custom_callback_address.is_empty() {
        return config.epay.custom_callback_address.clone();
    }
    config.server_address.clone()
}

/// 生成返回路径
fn make_return_path(suffix: &str) -> String {
    let base = "/wallet";
    if suffix.is_empty() {
        base.to_string()
    } else {
        format!("{base}?pay={suffix}")
    }
}

/// POST /api/topup - 用户发起充值
pub async fn handle_topup_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TopupRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let config = state.config_manager.get().await;
    let epay = crate::payment::EpayClient::new(config.epay.clone());
    if !epay.config().ready() {
        return error_response("Epay not configured", StatusCode::BAD_REQUEST).into_response();
    }
    if body.amount < epay.config().min_topup {
        return error_response("Amount below minimum", StatusCode::BAD_REQUEST).into_response();
    }
    if !epay.config().contains_pay_method(&body.payment_method) {
        return error_response("Payment method not supported", StatusCode::BAD_REQUEST)
            .into_response();
    }
    let money = pay_money(epay.config(), body.amount);
    if money < 0.01 {
        return error_response("Amount too low", StatusCode::BAD_REQUEST).into_response();
    }
    let callback = callback_address(&state, &config);
    let return_url = format!("{}{}", callback, make_return_path(""));
    let notify_url = format!("{}/api/user/epay/notify", callback.trim_end_matches('/'));
    let trade_no = new_trade_no("USR", &user.id);
    let money_str = format!("{money:.2}");
    let order = TopUpOrder {
        trade_no: trade_no.clone(),
        user_id: user.id.clone(),
        amount: body.amount,
        money,
        // F02（契约2）：下单时锁定入账配额，回调直接使用
        quota: topup_quota(epay.config(), body.amount),
        payment_method: body.payment_method.clone(),
        status: "pending".into(),
        create_time: chrono::Utc::now().timestamp(),
        paid_time: None,
    };

    // 调 EpayClient::purchase 下单（mapi.php 优先，submit.php 回退），
    // 返回已签名的提交地址 url + params——前端 Wallet 据此构造 POST 表单跳转。
    let client_ip =
        super::common::extract_client_ip(&headers).unwrap_or_else(|| "127.0.0.1".to_string());
    let purchase = epay
        .purchase(&crate::payment::PurchaseArgs {
            pay_type: body.payment_method.clone(),
            out_trade_no: trade_no.clone(),
            name: format!("AIGX Topup {}", body.amount),
            money: money_str,
            notify_url: notify_url.clone(),
            return_url: return_url.clone(),
            clientip: client_ip,
            device: crate::payment::Device::PC,
        })
        .await;
    if let Err(e) = purchase {
        tracing::error!("EPay purchase failed for {}: {e}", trade_no);
        return error_response(
            &format!("Failed to create payment: {e}"),
            StatusCode::BAD_GATEWAY,
        )
        .into_response();
    }

    if let Err(e) = state.order_store.insert(&order) {
        tracing::error!("Failed to create order: {e}");
        return error_response("Failed to create order", StatusCode::INTERNAL_SERVER_ERROR)
            .into_response();
    }

    let purchase = purchase.unwrap();
    // 前端契约：data.url = 表单提交地址，其余字段为隐藏表单项
    let mut data = json!({
        "trade_no": trade_no,
        "amount": body.amount,
        "money": money,
        "quota": order.quota,
        "payment_method": body.payment_method,
        "status": "pending",
        "notify_url": notify_url,
        "return_url": return_url,
    });
    if let Some(obj) = data.as_object_mut() {
        for (k, v) in &purchase.params {
            obj.insert(k.clone(), Value::String(v.clone()));
        }
    }
    // serde_json 无对象展开语法：直接把 url 并入 data
    if let Some(obj) = data.as_object_mut() {
        obj.insert("url".into(), Value::String(purchase.url.clone()));
    }
    Json(json!({ "success": true, "data": data })).into_response()
}

/// GET /api/orders - 列出所有订单
pub async fn handle_list_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let orders: Vec<Value> = state
        .order_store
        .list_all()
        .iter()
        .map(|o| {
            json!({
                "trade_no": o.trade_no,
                "user_id": o.user_id,
                "amount": o.amount,
                "money": o.money,
                "quota": o.quota,
                "payment_method": o.payment_method,
                "status": o.status,
                "create_time": o.create_time,
                "paid_time": o.paid_time,
            })
        })
        .collect();
    Ok(Json(
        json!({ "success": true, "data": orders, "total": orders.len() }),
    ))
}

/// GET /api/orders/:trade_no - 查询订单详情
pub async fn handle_get_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(trade_no): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if let Some(order) = state.order_store.get(&trade_no) {
        Ok(Json(json!({
            "success": true,
            "data": json!({
                "trade_no": order.trade_no,
                "user_id": order.user_id,
                "amount": order.amount,
                "money": order.money,
                "quota": order.quota,
                "payment_method": order.payment_method,
                "status": order.status,
                "create_time": order.create_time,
                "paid_time": order.paid_time,
            })
        })))
    } else {
        Err(error_response("Order not found", StatusCode::NOT_FOUND))
    }
}

/// DELETE /api/orders/:trade_no - 删除订单
pub async fn handle_delete_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(trade_no): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if state.order_store.delete(&trade_no) {
        Ok(Json(json!({ "success": true, "data": null })))
    } else {
        Err(error_response("Order not found", StatusCode::NOT_FOUND))
    }
}
