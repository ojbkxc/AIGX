//! 聊天会话持久化——普通用户的「对话记录」，与 Agent 会话同套机制但**按
//! 用户隔离**（Agent 是管理员共享的运维会话，聊天是个人对话）。
//!
//! 键设计（user_id 进键 = 归属校验的物理边界）：
//! - `chat_session:{user_id}:{id}` → 会话元信息（标题/模型/时间）
//! - `chat_msg:{user_id}:{id}:{seq}` → 单条消息（role/content + 扩展位）
//!
//! 与 `agent::session` 同源：seq 8 位零填充保证字典序 = 时间序。
//! 这样 Chat 页的对话记录与运维页一样——换浏览器/清缓存都不丢。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::admin::common::{error_response, verify_user};
use crate::api::openai::AppState;
use crate::storage::FileStore;

const SESSION_PREFIX: &str = "chat_session:";
const MSG_PREFIX: &str = "chat_msg:";

/// 聊天会话元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: String,
    /// 归属用户（进键 + 校验双重隔离）。
    pub user_id: String,
    pub title: String,
    pub model: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 聊天消息（content 之外带扩展位，reasoning/附件等原样存取）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: String,
    /// 扩展位（reasoning / 附件 / 工具轨迹等），前端原样读写。
    #[serde(default)]
    pub extra: Option<Value>,
    #[serde(default)]
    pub created_at: i64,
}

/// 聊天会话存储（按用户隔离）。
#[derive(Clone)]
pub struct ChatSessionStore {
    store: Arc<FileStore>,
}

impl ChatSessionStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }

    fn session_key(user_id: &str, id: &str) -> String {
        format!("{SESSION_PREFIX}{user_id}:{id}")
    }

    fn msg_key(user_id: &str, session_id: &str, seq: u64) -> String {
        format!("{MSG_PREFIX}{user_id}:{session_id}:{seq:08}")
    }

    /// 创建会话。
    pub fn create(
        &self,
        user_id: &str,
        id: &str,
        title: &str,
        model: &str,
        updated_at: Option<i64>,
    ) -> anyhow::Result<ChatSession> {
        let now = now_ts();
        let ts = updated_at.filter(|t| *t > 0).unwrap_or(now);
        let s = ChatSession {
            id: id.to_string(),
            user_id: user_id.to_string(),
            title: title.to_string(),
            model: model.to_string(),
            created_at: ts,
            updated_at: ts,
        };
        self.store.put(&Self::session_key(user_id, id), &s)?;
        Ok(s)
    }

    /// 读会话（带归属语义：只在该用户的键空间里找）。
    pub fn get(&self, user_id: &str, id: &str) -> anyhow::Result<Option<ChatSession>> {
        self.store.get(&Self::session_key(user_id, id))
    }

    /// 更新标题。
    pub fn rename(&self, user_id: &str, id: &str, title: &str) -> anyhow::Result<()> {
        if let Some(mut s) = self.get(user_id, id)? {
            s.title = title.to_string();
            s.updated_at = now_ts();
            self.store.put(&Self::session_key(user_id, id), &s)?;
        }
        Ok(())
    }

    /// 标题仍是缺省时用首条消息自动命名（截 30 字符）。
    pub fn rename_if_default(
        &self,
        user_id: &str,
        id: &str,
        first_message: &str,
    ) -> anyhow::Result<()> {
        if let Some(s) = self.get(user_id, id)? {
            if s.title.trim() == "新会话" || s.title.trim().is_empty() {
                let title: String = first_message.trim().chars().take(30).collect();
                if !title.is_empty() {
                    self.rename(user_id, id, &title)?;
                }
            }
        }
        Ok(())
    }

    /// 列出该用户的会话（按更新时间倒序）。
    pub fn list_for_user(&self, user_id: &str) -> anyhow::Result<Vec<ChatSession>> {
        let keys = self.store.list(&format!("{SESSION_PREFIX}{user_id}:"))?;
        let mut out = Vec::new();
        for k in keys {
            if let Some(s) = self.store.get::<ChatSession>(&k)? {
                out.push(s);
            }
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(out)
    }

    /// 追加一条消息（顺带刷新会话 updated_at）。
    pub fn append_message(
        &self,
        user_id: &str,
        session_id: &str,
        msg: &ChatMessage,
    ) -> anyhow::Result<u64> {
        let seq = self.next_seq(user_id, session_id)?;
        self.store
            .put(&Self::msg_key(user_id, session_id, seq), msg)?;
        if let Some(mut s) = self.get(user_id, session_id)? {
            s.updated_at = now_ts();
            self.store
                .put(&Self::session_key(user_id, session_id), &s)?;
        }
        Ok(seq)
    }

    /// 读会话全部消息（按 seq 升序）。
    pub fn messages(&self, user_id: &str, session_id: &str) -> anyhow::Result<Vec<ChatMessage>> {
        let keys = self
            .store
            .list(&format!("{MSG_PREFIX}{user_id}:{session_id}:"))?;
        let mut out = Vec::new();
        for k in keys {
            if let Some(m) = self.store.get::<ChatMessage>(&k)? {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// 整表替换消息（前端 debounce 保存：编辑/重发会改写历史，append 不够）。
    pub fn replace_messages(
        &self,
        user_id: &str,
        session_id: &str,
        msgs: &[ChatMessage],
        touch: bool,
    ) -> anyhow::Result<()> {
        // 先清旧消息再写新的（简单可靠；会话消息量级小）
        for k in self
            .store
            .list(&format!("{MSG_PREFIX}{user_id}:{session_id}:"))?
        {
            self.store.delete(&k)?;
        }
        for (seq, msg) in msgs.iter().enumerate() {
            self.store
                .put(&Self::msg_key(user_id, session_id, seq as u64), msg)?;
        }
        if touch {
            if let Some(mut s) = self.get(user_id, session_id)? {
                s.updated_at = now_ts();
                self.store
                    .put(&Self::session_key(user_id, session_id), &s)?;
            }
        }
        Ok(())
    }

    fn next_seq(&self, user_id: &str, session_id: &str) -> anyhow::Result<u64> {
        let keys = self
            .store
            .list(&format!("{MSG_PREFIX}{user_id}:{session_id}:"))?;
        Ok(keys.len() as u64)
    }

    /// 删除会话及其消息。
    pub fn delete(&self, user_id: &str, id: &str) -> anyhow::Result<()> {
        for k in self.store.list(&format!("{MSG_PREFIX}{user_id}:{id}:"))? {
            self.store.delete(&k)?;
        }
        self.store.delete(&Self::session_key(user_id, id))
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ── HTTP API（全部 verify_user + 归属校验；管理员也不例外——这是私人对话）──

/// GET /api/chat/sessions —— 列出我的会话。
pub async fn handle_list_chat_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let sessions = state
        .chat_sessions
        .list_for_user(&user.id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "success": true, "data": sessions })))
}

/// POST /api/chat/sessions —— 新建会话。
#[derive(Debug, Deserialize)]
pub struct CreateChatSessionBody {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// 迁移旧会话时保留原时间（unix 秒；缺省用当前时间）。
    #[serde(default)]
    pub updated_at: Option<i64>,
}

pub async fn handle_create_chat_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateChatSessionBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let title = body
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| "新会话".to_string());
    let session = state
        .chat_sessions
        .create(
            &user.id,
            &new_id(),
            &title,
            body.model.as_deref().unwrap_or(""),
            body.updated_at,
        )
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "success": true, "data": session })))
}

/// GET /api/chat/sessions/:id —— 会话详情 + 全部消息。
pub async fn handle_get_chat_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let Some(session) = state
        .chat_sessions
        .get(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
    else {
        return Err(error_response("会话不存在", StatusCode::NOT_FOUND));
    };
    let messages = state
        .chat_sessions
        .messages(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(
        json!({ "success": true, "data": { "session": session, "messages": messages } }),
    ))
}

/// POST /api/chat/sessions/:id/rename —— 改标题。
#[derive(Debug, Deserialize)]
pub struct RenameChatSessionBody {
    pub title: String,
}

pub async fn handle_rename_chat_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RenameChatSessionBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    if state
        .chat_sessions
        .get(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
        .is_none()
    {
        return Err(error_response("会话不存在", StatusCode::NOT_FOUND));
    }
    state
        .chat_sessions
        .rename(&user.id, &id, body.title.trim())
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "success": true, "data": null })))
}

/// POST /api/chat/sessions/:id/messages —— 追加消息（前端流式结束后落盘）。
#[derive(Debug, Deserialize)]
pub struct AppendChatMessageBody {
    pub role: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub extra: Option<Value>,
}

pub async fn handle_append_chat_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AppendChatMessageBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let Some(_session) = state
        .chat_sessions
        .get(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
    else {
        return Err(error_response("会话不存在", StatusCode::NOT_FOUND));
    };
    let msg = ChatMessage {
        role: body.role,
        content: body.content,
        extra: body.extra,
        created_at: now_ts(),
    };
    let seq = state
        .chat_sessions
        .append_message(&user.id, &id, &msg)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    // 首条用户消息自动命名
    if seq == 0 && msg.role == "user" {
        let _ = state
            .chat_sessions
            .rename_if_default(&user.id, &id, &msg.content);
    }
    Ok(Json(json!({ "success": true, "data": { "seq": seq } })))
}

/// PUT /api/chat/sessions/:id/messages —— 整表替换消息（debounce 保存）。
#[derive(Debug, Deserialize)]
pub struct ReplaceChatMessagesBody {
    pub messages: Vec<AppendChatMessageBody>,
    /// false = 不刷新 updated_at（迁移旧会话时保留原排序时间；默认 true）。
    #[serde(default = "default_touch")]
    pub touch: bool,
}

fn default_touch() -> bool {
    true
}

pub async fn handle_replace_chat_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ReplaceChatMessagesBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let Some(session) = state
        .chat_sessions
        .get(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
    else {
        return Err(error_response("会话不存在", StatusCode::NOT_FOUND));
    };
    let msgs: Vec<ChatMessage> = body
        .messages
        .into_iter()
        .map(|m| ChatMessage {
            role: m.role,
            content: m.content,
            extra: m.extra,
            created_at: now_ts(),
        })
        .collect();
    state
        .chat_sessions
        .replace_messages(&user.id, &id, &msgs, body.touch)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    // 标题仍为缺省时用首条用户消息自动命名
    if session.title.trim() == "新会话" || session.title.trim().is_empty() {
        if let Some(first) = msgs
            .iter()
            .find(|m| m.role == "user" && !m.content.trim().is_empty())
        {
            let _ = state
                .chat_sessions
                .rename_if_default(&user.id, &id, &first.content);
        }
    }
    Ok(Json(
        json!({ "success": true, "data": { "count": msgs.len() } }),
    ))
}

/// DELETE /api/chat/sessions/:id —— 删会话。
pub async fn handle_delete_chat_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    if state
        .chat_sessions
        .get(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?
        .is_none()
    {
        return Err(error_response("会话不存在", StatusCode::NOT_FOUND));
    }
    state
        .chat_sessions
        .delete(&user.id, &id)
        .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
    Ok(Json(json!({ "success": true, "data": null })))
}
