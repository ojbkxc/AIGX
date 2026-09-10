//! 套餐管理 API — 套餐模板 CRUD + 按套餐发放 API Key。
//!
//! 业务流：卖家（管理员）定义套餐模板 → 买家付款 → 卖家调
//! `POST /api/plans/:id/issue` 按模板生成带额度+有效期的 key 交付买家。
//! key 即套餐凭证（cf-ai-gw 模式），无需独立订单/交付实体。

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};

use crate::plan::Plan;

/// 套餐创建/更新请求（id 留空 = 创建）
#[derive(Debug, Deserialize)]
pub struct PlanRequest {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub price: f64,
    #[serde(default)]
    pub quota: i64,
    #[serde(default = "default_duration_days")]
    pub duration_days: i64,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    // ── 订阅化扩展（#85）────────────────────────────────────────────────
    /// once（默认，按量发 key）/ subscription（时长订阅）
    #[serde(default)]
    pub plan_type: String,
    #[serde(default = "default_duration_unit")]
    pub duration_unit: String,
    #[serde(default = "default_duration_value")]
    pub duration_value: i64,
    #[serde(default)]
    pub custom_seconds: i64,
    /// 订阅总配额（0 = 不限）
    #[serde(default)]
    pub total_amount: i64,
    #[serde(default = "default_reset_period")]
    pub quota_reset_period: String,
    #[serde(default)]
    pub quota_reset_custom_seconds: i64,
    #[serde(default = "default_allow_balance_pay")]
    pub allow_balance_pay: bool,
    #[serde(default = "default_allow_wallet_overflow")]
    pub allow_wallet_overflow: bool,
    #[serde(default)]
    pub max_purchase_per_user: i64,
    #[serde(default)]
    pub upgrade_group: String,
    #[serde(default)]
    pub downgrade_group: String,
    #[serde(default)]
    pub sort_order: i64,
}

fn default_duration_days() -> i64 {
    30
}

fn default_enabled() -> bool {
    true
}

fn default_duration_unit() -> String {
    "month".to_string()
}

fn default_duration_value() -> i64 {
    1
}

fn default_reset_period() -> String {
    "never".to_string()
}

fn default_allow_balance_pay() -> bool {
    true
}

fn default_allow_wallet_overflow() -> bool {
    true
}

impl PlanRequest {
    fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("plan name is required".to_string());
        }
        if self.quota < 0 {
            return Err("quota must be >= 0".to_string());
        }
        if self.duration_days < 0 {
            return Err("duration_days must be >= 0".to_string());
        }
        let is_sub = self.plan_type == "subscription";
        if is_sub {
            match self.duration_unit.as_str() {
                "year" | "month" | "day" | "hour" => {
                    if self.duration_value <= 0 {
                        return Err("duration_value must be > 0".to_string());
                    }
                }
                "custom" => {
                    if self.custom_seconds <= 0 {
                        return Err("custom_seconds must be > 0".to_string());
                    }
                }
                other => {
                    return Err(format!("invalid duration_unit: {other}"));
                }
            }
            if self.total_amount < 0 {
                return Err("total_amount must be >= 0".to_string());
            }
            match self.quota_reset_period.as_str() {
                "never" | "daily" | "weekly" | "monthly" | "custom" => {}
                other => return Err(format!("invalid quota_reset_period: {other}")),
            }
            if self.quota_reset_period == "custom" && self.quota_reset_custom_seconds <= 0 {
                return Err("quota_reset_custom_seconds must be > 0".to_string());
            }
            if self.max_purchase_per_user < 0 {
                return Err("max_purchase_per_user must be >= 0".to_string());
            }
        }
        Ok(())
    }
}

/// 按套餐发 key 请求
#[derive(Debug, Deserialize)]
pub struct IssueKeyRequest {
    /// 令牌名（买家标识/备注），默认「套餐名-序号」
    #[serde(default)]
    pub name: String,
}

/// 列出套餐
pub async fn handle_list_plans(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let plans = state.plan_store.list();
    Ok(Json(json!({ "success": true, "data": plans })))
}

/// 创建/更新套餐
pub async fn handle_upsert_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PlanRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if let Err(msg) = body.validate() {
        return Err(error_response(&msg, StatusCode::BAD_REQUEST));
    }
    let plan = Plan {
        id: body.id.clone(),
        name: body.name.trim().to_string(),
        price: body.price,
        quota: body.quota,
        duration_days: body.duration_days,
        group: if body.group.is_empty() {
            "default".to_string()
        } else {
            body.group.clone()
        },
        allowed_models: body.allowed_models,
        description: body.description,
        enabled: body.enabled,
        issued_count: 0,
        plan_type: body.plan_type.clone(),
        duration_unit: body.duration_unit.clone(),
        duration_value: body.duration_value,
        custom_seconds: body.custom_seconds,
        total_amount: body.total_amount,
        quota_reset_period: body.quota_reset_period.clone(),
        quota_reset_custom_seconds: body.quota_reset_custom_seconds,
        allow_balance_pay: body.allow_balance_pay,
        allow_wallet_overflow: body.allow_wallet_overflow,
        max_purchase_per_user: body.max_purchase_per_user,
        upgrade_group: body.upgrade_group.trim().to_string(),
        downgrade_group: body.downgrade_group.trim().to_string(),
        sort_order: body.sort_order,
        created_at: 0,
        updated_at: 0,
    };
    let is_create = body.id.is_empty();
    match state.plan_store.upsert(plan) {
        Ok(p) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                if is_create { "create" } else { "update" },
                &format!("plan:{}", p.id),
                None,
                Some(serde_json::to_value(&p).unwrap_or(Value::Null)),
            );
            Ok(Json(json!({ "success": true, "data": p })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save plan: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 删除套餐（不影响已发放的 key）
pub async fn handle_delete_plan(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.plan_store.delete(&id) {
        Ok(_) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "delete",
                &format!("plan:{id}"),
                None,
                None,
            );
            Ok(Json(json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to delete plan: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 按套餐发放 API Key（核心交付入口）。
///
/// 套餐模板 → CreateApiKeyOptions → 生成 key；响应一次性返回明文密钥
/// （卖家复制交付买家，之后仅可经 /api/tokens/:id/key 按需取回）。
pub async fn handle_issue_plan_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<IssueKeyRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let plan = state
        .plan_store
        .get(&id)
        .ok_or_else(|| error_response("Plan not found", StatusCode::NOT_FOUND))?;
    // 订阅套餐走余额购买端点（POST /api/subscription/balance/pay），不发 key
    if plan.is_subscription() {
        return Err(error_response(
            "Subscription plans are purchased with balance, not key issuance",
            StatusCode::BAD_REQUEST,
        ));
    }
    let key_name = if body.name.trim().is_empty() {
        format!("{}-{}", plan.name, plan.issued_count + 1)
    } else {
        body.name.trim().to_string()
    };
    let opts = state
        .plan_store
        .build_key_options(&id, &key_name)
        .ok_or_else(|| error_response("Plan disabled", StatusCode::BAD_REQUEST))?;
    match state.api_key_store.generate_with_options(opts) {
        Ok(k) => {
            state.plan_store.record_issue(&id);
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "plan.issue_key",
                &format!("plan:{id}"),
                None,
                Some(json!({ "key_id": k.id, "key_name": k.name })),
            );
            // 明文密钥一次性下发（与 POST /api/tokens 创建契约一致）
            Ok(Json(json!({
                "success": true,
                "data": {
                    "id": k.id,
                    "plain_key": k.key,
                    "name": k.name,
                    "group": k.group,
                    "quota_limit": k.quota_limit,
                    "expires_at": k.expires_at,
                }
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to issue key: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}
