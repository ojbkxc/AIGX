//! 工单 API — 用户侧提交/查看/回复/关闭，管理员侧列表/回复/关闭。
//!
//! 参照 v2board TicketController：
//! - 用户列表/详情（`?id=` 时返回详情+消息）、提交（subject/level/message）、回复、关闭。
//! - 管理员列表（分页 + status/reply_status/email 过滤）、详情、回复（可重开）、关闭。

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin, verify_user};

use crate::ticket::Ticket;

/// 消息 JSON 形状（is_me 对齐 v2board：发起人视角标记）
fn message_json(ticket: &Ticket, msg: &crate::ticket::TicketMessage) -> Value {
    json!({
        "id": msg.id,
        "ticket_id": msg.ticket_id,
        "user_id": msg.user_id,
        "message": msg.message,
        "created_at": msg.created_at,
        "is_me": msg.user_id == ticket.user_id,
    })
}

/// 工单 JSON 形状（不含消息）
fn ticket_json(t: &Ticket) -> Value {
    json!({
        "id": t.id,
        "user_id": t.user_id,
        "subject": t.subject,
        "level": t.level,
        "status": t.status,
        "reply_status": t.reply_status,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}

// ── 用户侧 ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct UserTicketQuery {
    #[serde(default)]
    pub id: String,
}

/// GET /api/tickets - 用户工单列表（带 id 则返回详情+消息）
pub async fn handle_user_list_tickets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<UserTicketQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    if !q.id.is_empty() {
        let ticket = state
            .ticket_store
            .get(&q.id)
            .ok_or_else(|| error_response("工单不存在", StatusCode::NOT_FOUND))?;
        if ticket.user_id != me.id {
            return Err(error_response("工单不存在", StatusCode::NOT_FOUND));
        }
        let messages: Vec<Value> = state
            .ticket_store
            .messages(&ticket.id)
            .iter()
            .map(|m| message_json(&ticket, m))
            .collect();
        let mut data = ticket_json(&ticket);
        if let Some(obj) = data.as_object_mut() {
            obj.insert("message".into(), Value::Array(messages));
        }
        return Ok(Json(json!({ "success": true, "data": data })));
    }
    let data: Vec<Value> = state
        .ticket_store
        .list_by_user(&me.id)
        .iter()
        .map(ticket_json)
        .collect();
    Ok(Json(json!({ "success": true, "data": data })))
}

#[derive(Debug, Deserialize)]
pub struct SaveTicketRequest {
    pub subject: String,
    pub level: i32,
    pub message: String,
}

/// POST /api/tickets - 用户提交工单
pub async fn handle_user_save_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SaveTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    if body.subject.trim().is_empty() {
        return Err(error_response("工单主题不能为空", StatusCode::BAD_REQUEST));
    }
    if body.message.trim().is_empty() {
        return Err(error_response("工单内容不能为空", StatusCode::BAD_REQUEST));
    }
    if !(0..=2).contains(&body.level) {
        return Err(error_response(
            "工单优先级格式错误",
            StatusCode::BAD_REQUEST,
        ));
    }
    let ticket = state
        .ticket_store
        .create(&me.id, body.subject.trim(), body.level, body.message.trim())
        .map_err(|e| {
            error_response(
                &format!("提交工单失败: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    Ok(Json(
        json!({ "success": true, "data": ticket_json(&ticket) }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct ReplyTicketRequest {
    pub id: String,
    pub message: String,
}

/// POST /api/tickets/reply - 用户回复工单
pub async fn handle_user_reply_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReplyTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    if body.message.trim().is_empty() {
        return Err(error_response("回复内容不能为空", StatusCode::BAD_REQUEST));
    }
    let ticket = state
        .ticket_store
        .get(&body.id)
        .ok_or_else(|| error_response("工单不存在", StatusCode::NOT_FOUND))?;
    if ticket.user_id != me.id {
        return Err(error_response("工单不存在", StatusCode::NOT_FOUND));
    }
    state
        .ticket_store
        .reply(&body.id, &me.id, body.message.trim())
        .map(|t| Json(json!({ "success": true, "data": ticket_json(&t) })))
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))
}

#[derive(Debug, Deserialize)]
pub struct CloseTicketRequest {
    pub id: String,
}

/// POST /api/tickets/close - 用户关闭工单
pub async fn handle_user_close_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CloseTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let me = verify_user(&state, &headers).await?;
    let ticket = state
        .ticket_store
        .get(&body.id)
        .ok_or_else(|| error_response("工单不存在", StatusCode::NOT_FOUND))?;
    if ticket.user_id != me.id {
        return Err(error_response("工单不存在", StatusCode::NOT_FOUND));
    }
    state
        .ticket_store
        .close(&body.id)
        .map(|t| Json(json!({ "success": true, "data": ticket_json(&t) })))
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))
}

// ── 管理员侧 ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct AdminTicketQuery {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub page: Option<usize>,
    #[serde(default)]
    pub page_size: Option<usize>,
    #[serde(default)]
    pub status: Option<i32>,
    #[serde(default)]
    pub reply_status: Option<i32>,
    #[serde(default)]
    pub email: Option<String>,
}

/// GET /api/admin/tickets - 管理员工单列表/详情
pub async fn handle_admin_list_tickets(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AdminTicketQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    if !q.id.is_empty() {
        let ticket = state
            .ticket_store
            .get(&q.id)
            .ok_or_else(|| error_response("工单不存在", StatusCode::NOT_FOUND))?;
        let messages: Vec<Value> = state
            .ticket_store
            .messages(&ticket.id)
            .iter()
            .map(|m| message_json(&ticket, m))
            .collect();
        let mut data = ticket_json(&ticket);
        if let Some(obj) = data.as_object_mut() {
            obj.insert("message".into(), Value::Array(messages));
        }
        return Ok(Json(json!({ "success": true, "data": data })));
    }
    let mut all: Vec<Ticket> = state.ticket_store.list_all();
    if let Some(s) = q.status {
        all.retain(|t| t.status == s);
    }
    if let Some(r) = q.reply_status {
        all.retain(|t| t.reply_status == r);
    }
    if let Some(email) = q.email.as_deref() {
        let email = email.trim();
        if !email.is_empty() {
            if let Some(u) = state.user_store.get_by_email(email) {
                all.retain(|t| t.user_id == u.id);
            } else {
                all.clear();
            }
        }
    }
    let total = all.len();
    let page = q.page.unwrap_or(1).max(1);
    let page_size = q.page_size.unwrap_or(10).max(1).min(100);
    let start = (page - 1) * page_size;
    let data: Vec<Value> = if start >= total {
        Vec::new()
    } else {
        all[start..total.min(start + page_size)]
            .iter()
            .map(ticket_json)
            .collect()
    };
    Ok(Json(json!({
        "success": true,
        "data": data,
        "items": data,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// POST /api/admin/tickets/reply - 管理员回复（可重开已关闭工单）
pub async fn handle_admin_reply_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReplyTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let admin = verify_user(&state, &headers).await?;
    if !admin.is_admin() {
        return Err(error_response(
            "Admin access required",
            StatusCode::FORBIDDEN,
        ));
    }
    if body.message.trim().is_empty() {
        return Err(error_response("回复内容不能为空", StatusCode::BAD_REQUEST));
    }
    state
        .ticket_store
        .reply_by_admin(&body.id, &admin.id, body.message.trim())
        .map(|t| Json(json!({ "success": true, "data": ticket_json(&t) })))
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))
}

/// POST /api/admin/tickets/close - 管理员关闭工单
pub async fn handle_admin_close_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CloseTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    state
        .ticket_store
        .close(&body.id)
        .map(|t| Json(json!({ "success": true, "data": ticket_json(&t) })))
        .map_err(|e| error_response(&e.to_string(), StatusCode::BAD_REQUEST))
}
