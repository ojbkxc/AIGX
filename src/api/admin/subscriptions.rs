//! 订阅 API — 用户侧订阅（套餐列表/余额购买/我的订阅）+ 管理端绑定/取消。
//!
//! 对齐 new-api controller/subscription.go：
//! - GET  /api/subscription/plans       上架订阅套餐列表（登录用户可见）
//! - GET  /api/subscription/self        当前用户订阅列表（含过期）
//! - POST /api/subscription/balance/pay 余额购买订阅（对齐 PurchaseSubscriptionWithBalance）
//! - POST /api/subscription/admin/bind  管理员给用户绑定订阅（对齐 AdminBindSubscription）
//! - GET  /api/subscription/admin/users/:id/subscriptions 查看用户订阅
//! - POST /api/subscription/admin/subscriptions/:id/cancel 取消订阅并回退分组

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{
    admin_id_from_session, error_response, record_audit, verify_admin, verify_user,
};

use crate::plan::subscription::{
    calc_next_reset, calc_plan_end, downgrade_user_group, UserSubscription,
};
use crate::plan::Plan;

/// 余额购买请求
#[derive(Debug, Deserialize)]
pub struct BalancePayRequest {
    pub plan_id: String,
}

/// 管理端绑定请求
#[derive(Debug, Deserialize)]
pub struct AdminBindRequest {
    pub user_id: String,
    pub plan_id: String,
}

/// 订阅套餐的对外 JSON 形状（用户侧购买卡片）
fn plan_json(p: &Plan) -> Value {
    json!({
        "id": p.id,
        "name": p.name,
        "price": p.price,
        "description": p.description,
        "duration_unit": p.duration_unit,
        "duration_value": p.duration_value,
        "custom_seconds": p.custom_seconds,
        "total_amount": p.total_amount,
        "quota_reset_period": p.quota_reset_period,
        "allow_balance_pay": p.allow_balance_pay,
        "allow_wallet_overflow": p.allow_wallet_overflow,
        "upgrade_group": p.upgrade_group,
        "sort_order": p.sort_order,
    })
}

/// 订阅实例的对外 JSON 形状（附加套餐快照展示字段）
fn sub_json(s: &UserSubscription, plan: Option<&Plan>) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        if let Some(p) = plan {
            obj.insert("plan_name".into(), json!(p.name));
            obj.insert("plan_price".into(), json!(p.price));
        }
    }
    v
}

/// GET /api/subscription/plans — 上架的订阅套餐（登录用户可见）。
pub async fn handle_subscription_plans(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _user = verify_user(&state, &headers).await?;
    let plans = state.plan_store.list_subscription_enabled();
    let items: Vec<Value> = plans.iter().map(plan_json).collect();
    Ok(Json(json!({ "success": true, "data": items })))
}

/// GET /api/subscription/self — 当前用户全部订阅（含过期/取消）。
pub async fn handle_subscription_self(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let subs = state.subscription_store.list_by_user(&user.id);
    let items: Vec<Value> = subs
        .iter()
        .map(|s| sub_json(s, state.plan_store.get(&s.plan_id).as_ref()))
        .collect();
    Ok(Json(json!({ "success": true, "data": items })))
}

/// POST /api/subscription/balance/pay — 余额购买订阅。
///
/// 对齐 new-api PurchaseSubscriptionWithBalance：
/// 校验（enabled / allow_balance_pay / max_purchase_per_user）→ 元转配额
/// （price × epay.price 向上取整，AIGX 口径无 QuotaPerUnit）→ 扣余额 →
/// 建订阅快照（到期时间/重置锚点/分组升级）。
pub async fn handle_subscription_balance_pay(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BalancePayRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let plan = state
        .plan_store
        .get(&body.plan_id)
        .ok_or_else(|| error_response("套餐不存在", StatusCode::NOT_FOUND))?;
    if !plan.is_subscription() {
        return Err(error_response(
            "该套餐不是订阅套餐",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !plan.enabled {
        return Err(error_response("套餐未上架", StatusCode::BAD_REQUEST));
    }
    if !plan.allow_balance_pay {
        return Err(error_response(
            "该套餐不允许使用余额兑换",
            StatusCode::BAD_REQUEST,
        ));
    }
    if plan.max_purchase_per_user > 0 {
        let count = state.subscription_store.count_by_plan(&user.id, &plan.id);
        if count >= plan.max_purchase_per_user {
            return Err(error_response(
                "已达到该套餐购买上限",
                StatusCode::BAD_REQUEST,
            ));
        }
    }

    // 元 → 配额：AIGX 以 epay.price 为准（1 元 = price 配额），向上取整
    let config = state.config_manager.get().await;
    let unit_price = if config.epay.price > 0.0 {
        config.epay.price
    } else {
        1.0
    };
    let required_quota = if plan.price > 0.0 {
        (plan.price * unit_price + 0.999999) as i64
    } else {
        0
    };

    let now = chrono::Utc::now().timestamp();
    let end_time = calc_plan_end(now, &plan)
        .map_err(|e| error_response(&format!("套餐时长配置无效: {e}"), StatusCode::BAD_REQUEST))?;
    let next_reset = calc_next_reset(now, &plan, end_time);

    // 升级分组：先记 prev 快照再改（当前分组已是目标分组时不记快照）
    let upgrade_group = plan.upgrade_group.trim().to_string();
    let mut prev_user_group = String::new();
    if !upgrade_group.is_empty() && user.group != upgrade_group {
        prev_user_group = user.group.clone();
        state
            .user_store
            .update(&user.id, |u| u.group = upgrade_group.clone())
            .map_err(|e| {
                error_response(
                    &format!("分组升级失败: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
    }

    // 扣余额（先扣后建：扣失败无副作用；建订阅失败回滚余额）。
    // add_quota(-x) 对余额不足返回 Err（update 链路校验 remaining），
    // 这里直接以剩余额度预检 + add_quota(-x) 双保险。
    if required_quota > 0 {
        let current = state.user_store.get_by_id(&user.id);
        let balance = current.map(|u| u.remaining()).unwrap_or(0);
        if balance < required_quota {
            return Err(error_response(
                &format!("余额不足：需要 {required_quota}，当前 {balance}"),
                StatusCode::BAD_REQUEST,
            ));
        }
        state
            .user_store
            .add_quota(&user.id, -required_quota)
            .map_err(|e| {
                tracing::error!("balance pay add_quota failed for {}: {e}", user.id);
                error_response("余额不足", StatusCode::BAD_REQUEST)
            })?;
    }

    let sub = UserSubscription {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user.id.clone(),
        plan_id: plan.id.clone(),
        amount_total: plan.total_amount,
        amount_used: 0,
        start_time: now,
        end_time,
        status: "active".into(),
        source: "balance".into(),
        last_reset_time: if next_reset > 0 { now } else { 0 },
        next_reset_time: next_reset,
        upgrade_group: upgrade_group.clone(),
        prev_user_group: prev_user_group.clone(),
        downgrade_group: plan.downgrade_group.trim().to_string(),
        allow_wallet_overflow: plan.allow_wallet_overflow,
        created_at: now,
        updated_at: now,
    };

    match state.subscription_store.create(sub) {
        Ok(created) => {
            tracing::info!(
                "Subscription purchased: user {} plan {} ({}) charged {} quota, ends {}",
                user.id,
                plan.id,
                plan.name,
                required_quota,
                end_time
            );
            Ok(Json(json!({
                "success": true,
                "data": sub_json(&created, Some(&plan)),
            })))
        }
        Err(e) => {
            // 回滚余额与分组升级
            if required_quota > 0 {
                let _ = state.user_store.add_quota(&user.id, required_quota);
            }
            if !prev_user_group.is_empty() {
                let pg = prev_user_group.clone();
                let _ = state.user_store.update(&user.id, move |u| u.group = pg);
            }
            Err(error_response(
                &format!("订阅创建失败: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    }
}

/// POST /api/subscription/admin/bind — 管理员给用户绑定订阅（对齐 AdminBindSubscription）。
pub async fn handle_subscription_admin_bind(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AdminBindRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let plan = state
        .plan_store
        .get(&body.plan_id)
        .ok_or_else(|| error_response("套餐不存在", StatusCode::NOT_FOUND))?;
    if !plan.is_subscription() {
        return Err(error_response(
            "该套餐不是订阅套餐",
            StatusCode::BAD_REQUEST,
        ));
    }
    let user = state
        .user_store
        .get_by_id(&body.user_id)
        .ok_or_else(|| error_response("用户不存在", StatusCode::NOT_FOUND))?;

    let now = chrono::Utc::now().timestamp();
    let end_time = calc_plan_end(now, &plan)
        .map_err(|e| error_response(&format!("套餐时长配置无效: {e}"), StatusCode::BAD_REQUEST))?;
    let next_reset = calc_next_reset(now, &plan, end_time);

    let upgrade_group = plan.upgrade_group.trim().to_string();
    let mut prev_user_group = String::new();
    if !upgrade_group.is_empty() && user.group != upgrade_group {
        prev_user_group = user.group.clone();
        state
            .user_store
            .update(&user.id, |u| u.group = upgrade_group.clone())
            .map_err(|e| {
                error_response(
                    &format!("分组升级失败: {e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
    }

    let sub = UserSubscription {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user.id.clone(),
        plan_id: plan.id.clone(),
        amount_total: plan.total_amount,
        amount_used: 0,
        start_time: now,
        end_time,
        status: "active".into(),
        source: "admin".into(),
        last_reset_time: if next_reset > 0 { now } else { 0 },
        next_reset_time: next_reset,
        upgrade_group,
        prev_user_group,
        downgrade_group: plan.downgrade_group.trim().to_string(),
        allow_wallet_overflow: plan.allow_wallet_overflow,
        created_at: now,
        updated_at: now,
    };

    match state.subscription_store.create(sub) {
        Ok(created) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "subscription.bind",
                &format!("usersub:{}", created.id),
                None,
                Some(json!({ "user_id": user.id, "plan_id": plan.id })),
            );
            Ok(Json(json!({
                "success": true,
                "data": sub_json(&created, Some(&plan)),
            })))
        }
        Err(e) => Err(error_response(
            &format!("订阅绑定失败: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// GET /api/subscription/admin/users/:id/subscriptions — 查看指定用户的订阅。
pub async fn handle_subscription_admin_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if state.user_store.get_by_id(&user_id).is_none() {
        return Err(error_response("用户不存在", StatusCode::NOT_FOUND));
    }
    let subs = state.subscription_store.list_by_user(&user_id);
    let items: Vec<Value> = subs
        .iter()
        .map(|s| sub_json(s, state.plan_store.get(&s.plan_id).as_ref()))
        .collect();
    Ok(Json(json!({ "success": true, "data": items })))
}

/// POST /api/subscription/admin/subscriptions/:id/cancel — 取消订阅
/// 并立即回退分组（对齐 AdminInvalidateUserSubscription）。
pub async fn handle_subscription_admin_cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    let sub = state
        .subscription_store
        .get(&id)
        .ok_or_else(|| error_response("订阅不存在", StatusCode::NOT_FOUND))?;

    let cancelled = state.subscription_store.cancel(&id, now).map_err(|e| {
        error_response(&format!("取消失败: {e}"), StatusCode::INTERNAL_SERVER_ERROR)
    })?;
    // 分组回退（best-effort，失败不阻断取消结果）
    if let Err(e) = downgrade_user_group(
        &state.user_store,
        &state.subscription_store,
        &cancelled,
        now,
    ) {
        tracing::error!("subscription cancel group downgrade failed for {id}: {e}");
    }

    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "subscription.cancel",
        &format!("usersub:{id}"),
        Some(json!({ "status": sub.status, "end_time": sub.end_time })),
        Some(json!({ "status": "cancelled", "end_time": now })),
    );
    Ok(Json(json!({
        "success": true,
        "data": sub_json(&cancelled, state.plan_store.get(&cancelled.plan_id).as_ref()),
    })))
}
