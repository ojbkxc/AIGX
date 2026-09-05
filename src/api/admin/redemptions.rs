//! 兑换码 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供批量生成、查询和删除兑换码，以及用户兑换功能。

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::Value;

use super::super::openai::AppState;
use super::common::{
    admin_id_from_session, default_page, default_size, error_response, record_audit, verify_admin,
    verify_user,
};

/// 批量生成兑换码请求
#[derive(Debug, Deserialize)]
pub struct BatchRedemptionRequest {
    pub count: usize,
    pub quota: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub expires_at: i64,
}

/// 兑换码查询参数
#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
}

/// 兑换请求
#[derive(Debug, Deserialize)]
pub struct RedeemRequest {
    pub code: String,
}

/// 批量生成兑换码
pub async fn handle_batch_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchRedemptionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state
        .redemption_store
        .batch_generate(body.count, body.quota, &body.name, body.expires_at)
    {
        Ok(codes) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "create",
                "redemptions:batch",
                None,
                Some(serde_json::json!({ "count": body.count, "quota": body.quota })),
            );
            Ok(Json(serde_json::json!({ "success": true, "data": codes })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to generate redemptions: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 列出兑换码
pub async fn handle_list_redemptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (items, total) = state.redemption_store.list_paged(q.page, q.size);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": items,
        "total": total,
        "page": q.page,
        "size": q.size,
    })))
}

/// 删除兑换码
pub async fn handle_delete_redemption(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.redemption_store.delete(&id) {
        Ok(_) => {
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "delete",
                &format!("redemption:{id}"),
                None,
                None,
            );
            Ok(Json(serde_json::json!({ "success": true, "data": null })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to delete redemption: {e}"),
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 兑换兑换码
pub async fn handle_redeem(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RedeemRequest>,
) -> Response {
    let user = match verify_user(&state, &headers).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state.redemption_store.redeem(&body.code, &user.id) {
        Ok(quota) => {
            if let Err(e) = state.user_store.add_quota(&user.id, quota) {
                tracing::error!("Failed to add quota after redemption: {e}");
                // B03 修复：入账失败时回滚兑换码为 unused，允许用户重试，
                // 避免配额永久丢失后反复报 already used。
                if let Err(re) = state.redemption_store.rollback_redeem(&body.code, &user.id) {
                    tracing::error!(
                        "CRITICAL: failed to rollback redemption {}: {re} (quota={} user={}), manual compensation required",
                        body.code, quota, user.id
                    );
                }
                return error_response("Failed to add quota", StatusCode::INTERNAL_SERVER_ERROR)
                    .into_response();
            }
            let admin_id = admin_id_from_session(&state, &headers).await;
            record_audit(
                &state,
                &admin_id,
                "redeem",
                &format!("redemption:{}", body.code),
                None,
                Some(serde_json::json!({ "user_id": user.id, "quota": quota })),
            );
            Json(serde_json::json!({
                "success": true,
                "data": { "quota": quota },
                "message": format!("兑换成功，获得 {} 配额", quota),
            }))
            .into_response()
        }
        Err(e) => error_response(&e.to_string(), StatusCode::BAD_REQUEST).into_response(),
    }
}
