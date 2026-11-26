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

/// 工单 JSON + 提交人邮箱（管理员视图）：按 `user_id` 从 user_store 取 email 注入
/// `email` 字段，供「全部工单」列表/详情展示提交人具体邮箱。查不到用户时不加该键，
/// 前端回退显示 `user_id`（`tk.email || tk.user_id`）。
fn ticket_json_with_email(state: &AppState, t: &Ticket) -> Value {
    let mut value = ticket_json(t);
    if let Some(obj) = value.as_object_mut() {
        if let Some(user) = state.user_store.get_by_id(&t.user_id) {
            obj.insert("email".into(), Value::String(user.email));
        }
    }
    value
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
    /// `size` 别名（前端两种写法都认；`page_size` 优先）
    #[serde(default)]
    pub size: Option<usize>,
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
    let page_size = q.page_size.or(q.size).unwrap_or(10).clamp(1, 100);
    let start = (page - 1) * page_size;
    let data: Vec<Value> = if start >= total {
        Vec::new()
    } else {
        all[start..total.min(start + page_size)]
            .iter()
            .map(|t| ticket_json_with_email(&state, t))
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
///
/// 权限走 `verify_admin`（不是 `verify_user` + `is_admin()`）：后者虽然也挡住
/// 非管理员，但**绕过首登强制改密门禁**——`must_change_password` 的管理员
/// 能在这里提交任意回复，而同一管理面的其他接口一律 403
/// `PASSWORD_CHANGE_REQUIRED`。判定口径与本文件其余管理端点保持一致。
pub async fn handle_admin_reply_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReplyTicketRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 消息行里的 `user_id` 是用户 UUID（`append_message` 直接落库），不能拿
    // 会话邮箱充数——否则前端按 id 关联用户时认不出这条管理员回复。
    let email = crate::api::admin::common::admin_id_from_session(&state, &headers).await;
    let admin_id = state
        .user_store
        .get_by_email(&email)
        .map(|u| u.id)
        .ok_or_else(|| error_response("Admin not found", StatusCode::UNAUTHORIZED))?;
    if body.message.trim().is_empty() {
        return Err(error_response("回复内容不能为空", StatusCode::BAD_REQUEST));
    }
    state
        .ticket_store
        .reply_by_admin(&body.id, &admin_id, body.message.trim())
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
