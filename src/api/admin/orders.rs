//! 订单管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供充值订单的创建、查询、删除功能。

use axum::{
    extract::{Path, Query, State},
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
        .get(&amount.to_string())
        .filter(|d| **d > 0.0)
        .unwrap_or(&1.0);
    let money = (amount as f64) * discount;
    (money * 100.0).round() / 100.0
}

/// F02（契约2）：入账额度 = amount × price。
///
/// `epay.price` 的语义就是「1 元可购买多少额度」：生产配置 `price = 200` 表示
/// 1 元 = 200 额度。**不要再乘 200**——1.0.73 曾在 price 之外又乘了一次
/// 「1 元 = 200 额度」的换算系数，实际到账变成 `amount × price × 200`
/// （充 10 元到账 40 万额度），与充值页显示差 100 倍。
///
/// **折扣只作用于实付金额**（见 [`pay_money`]）：充 100 元打 9 折 = 实付 90 元、
/// 到账仍按 100 元面值（100 × price）。此前两边都乘 discount，等于「少付多少
/// 就少给多少」，档位越大反而到账越少，折扣形同虚设。
fn topup_quota(epay: &EpayConfig, amount: i64) -> i64 {
    ((amount as f64) * epay.price + 0.999999) as i64
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

/// 订单 JSON 形状（列表/详情共用）
fn order_json(o: &TopUpOrder) -> Value {
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
}

/// 解析 new-api 风格分页参数（p/page/page_size/ps/size 全兼容）
fn page_params(params: &std::collections::HashMap<String, String>) -> (usize, usize) {
    let page = ["p", "page"]
        .iter()
        .find_map(|k| params.get(*k).and_then(|v| v.parse::<usize>().ok()))
        .filter(|&p| p >= 1)
        .unwrap_or(1);
    let page_size = ["page_size", "ps", "size"]
        .iter()
        .find_map(|k| params.get(*k).and_then(|v| v.parse::<usize>().ok()))
        .filter(|&s| s > 0)
        .unwrap_or(10)
        .min(100);
    (page, page_size)
}

/// GET /api/orders - 管理端订单列表（分页 + 关键字搜索）。
///
/// 对齐 new-api GetAllTopUps：`?p=&page_size=&keyword=`，返回
/// `{ items, total, page, page_size }`（PageInfo 契约），同时保留
/// `data` 同值兼容旧前端。
pub async fn handle_list_orders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (page, page_size) = page_params(&params);
    let keyword = params.get("keyword").map(|s| s.as_str());
    let (orders, total) = state.order_store.list_paged(keyword, page, page_size);
    let items: Vec<Value> = orders.iter().map(order_json).collect();
    Ok(Json(json!({
        "success": true,
        "data": items,
        "items": items,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
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
            "data": order_json(&order),
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

/// POST /api/orders/:trade_no/complete - 管理端手动补单。
///
/// 对齐 new-api AdminCompleteTopUp：线下收款（如银行转账/微信截图）后
/// 管理员将 pending 订单标记为 paid 并立即入账。幂等——非 pending 订单
/// 直接报错，不重复入账。
pub async fn handle_complete_order(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(trade_no): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.order_store.complete_if_pending(&trade_no) {
        Some(order) => {
            // 入账：用户配额 += 订单锁定配额（F02：下单时锁定的值）
            if let Err(e) = state.user_store.add_quota(&order.user_id, order.quota) {
                tracing::error!(
                    "Manual complete: failed to add quota for order {} user {}: {e}",
                    trade_no,
                    order.user_id
                );
                // 回滚订单状态，允许重试
                if let Err(re) = state.order_store.reopen(&trade_no) {
                    tracing::error!(
                        "CRITICAL: failed to reopen order {trade_no} after quota add failure: {re}"
                    );
                }
                return Err(error_response(
                    "Failed to add quota",
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
            }
            let admin_id = super::common::admin_id_from_session(&state, &headers).await;
            super::common::record_audit(
                &state,
                &admin_id,
                "order.complete",
                &format!("order:{trade_no}"),
                None,
                Some(json!({ "user_id": order.user_id, "quota": order.quota })),
            );
            Ok(Json(json!({ "success": true, "data": order_json(&order) })))
        }
        None => Err(error_response(
            "Order not found or not pending",
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// POST /api/topup/amount - 充值试算（对齐 new-api RequestAmount）。
///
/// 前端充值页实时显示「支付金额/到账配额」：amount × discount 折后
/// pay_money，配额按下单锁定的 topup_quota 公式。Epay 未配置时 400。
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct AmountRequest {
    pub amount: i64,
    #[serde(default)]
    pub payment_method: String,
}

pub async fn handle_topup_amount(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AmountRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;
    let config = state.config_manager.get().await;
    let epay = crate::payment::EpayClient::new(config.epay.clone());
    if !epay.config().ready() {
        return Err(error_response(
            "Epay not configured",
            StatusCode::BAD_REQUEST,
        ));
    }
    if body.amount < epay.config().min_topup {
        return Err(error_response(
            "Amount below minimum",
            StatusCode::BAD_REQUEST,
        ));
    }
    let pay_money = pay_money(epay.config(), body.amount);
    if pay_money < 0.01 {
        return Err(error_response("Amount too low", StatusCode::BAD_REQUEST));
    }
    let quota = topup_quota(epay.config(), body.amount);
    Ok(Json(json!({
        "success": true,
        "data": {
            "amount": body.amount,
            "pay_money": (pay_money * 100.0).round() / 100.0,
            "quota": quota,
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn epay() -> EpayConfig {
        EpayConfig {
            price: 200.0,
            amount_discount: HashMap::from([("100".to_string(), 0.9)]),
            min_topup: 1,
            ..Default::default()
        }
    }

    #[test]
    fn quota_is_amount_times_price_once() {
        // price = 200（1 元 = 200 额度）：充 10 元 → 2000 额度 = ¥10。
        // 回归 1.0.73 的重复换算：曾得出 10 × 200 × 200 = 400000。
        assert_eq!(topup_quota(&epay(), 10), 2000);
        assert_eq!(topup_quota(&epay(), 1), 200);
        assert_eq!(topup_quota(&epay(), 100), 20_000);
    }

    #[test]
    fn discount_applies_to_paid_money_not_quota() {
        // 充 100 打 9 折：实付 90，到账仍按面值 100 × 200。
        assert_eq!(pay_money(&epay(), 100), 90.0);
        assert_eq!(topup_quota(&epay(), 100), 20_000);
        // 无折扣档位：实付=面值。
        assert_eq!(pay_money(&epay(), 10), 10.0);
    }

    #[test]
    fn zero_or_negative_discount_is_ignored() {
        let mut cfg = epay();
        cfg.amount_discount.insert("10".into(), 0.0);
        cfg.amount_discount.insert("20".into(), -0.5);
        assert_eq!(pay_money(&cfg, 10), 10.0);
        assert_eq!(pay_money(&cfg, 20), 20.0);
    }
}
