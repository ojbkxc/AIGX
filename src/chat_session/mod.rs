//! 聊天会话持久化——普通用户的「对话记录」，与 Agent 会话同套机制但**按
//! 用户隔离**（Agent 是管理员共享的运维会话，聊天是个人对话）。
//!
//! 键设计（user_id 进键 = 归属校验的物理边界）：
//! - `chat_session:{user_id}:{id}` → 会话元信息（标题/模型/时间）
//! - `chat_msg:{user_id}:{id}:{seq}` → 单条消息（role/content + 扩展位）
//!
//! 与 `agent::session` 同源：seq 8 位零填充保证字典序 = 时间序。
//! 这样 Chat 页的对话记录与运维页一样——换浏览器/清缓存都不丢。
//!
//! # 会话 fork（#37）
//! `POST /api/chat/sessions/:id/fork` 从父会话第 `message_seq` 条消息处
//! （**含该条**，0-based，与 GET 返回的 messages 下标一致）分叉出新会话：
//! 复制 `[0, message_seq]` 的消息（seq 从 0 重新编号，原 created_at 保留），
//! 子会话带 `parent_session_id` / `forked_from_seq` 可追溯。
//!
//! 复制有界（[`CHAT_FORK_MAX_MESSAGES`] 条 / [`CHAT_FORK_MAX_BYTES`] 字节，
//! ROUTE_REPLAY_MAX_* 同口径）：超限保留**距分叉点最近**的部分（分叉语义
//! 里尾部上下文最重要），丢弃条数在响应 `dropped` 里如实返回，不静默丢。
//!
//! ## 删除语义（有意为之）
//! **删父不级联**：子会话是独立对话，父删除后子会话照常可用，
//! `parent_session_id` 变为悬空引用（前端展示时容忍即可）。理由：
//! 级联删除对「个人对话」过于危险（误删即整棵分支树消失），且两套键
//! 空间物理独立、无外键，悬空引用无副作用。
//!
//! # 标题来源（#37）
//! `title_source` 三态：`"user"`（手工改名，永不被自动覆盖）/
//! `"auto"`（首条消息截 30 字，可被 LLM 标题接手）/"llm"（LLM 生成）。
//! 旧数据无此字段（None）：仅当标题为空/「新会话」时视同 auto，
//! 否则视作用户命名——宁可少触发，不可覆盖用户标题。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::admin::common::{error_response, verify_user};
use crate::api::openai::AppState;
use crate::storage::FileStore;

pub mod title;

const SESSION_PREFIX: &str = "chat_session:";
const MSG_PREFIX: &str = "chat_msg:";

/// fork 复制消息条数上限（ROUTE_REPLAY_MAX_ATTEMPTS 同口径的有界纪律）。
/// 超限保留距分叉点最近的部分，丢弃条数在响应 `dropped` 里如实返回。
pub const CHAT_FORK_MAX_MESSAGES: usize = 200;
/// fork 复制消息总字节上限（ROUTE_REPLAY_MAX_BYTES 同口径，含 extra 序列化）。
pub const CHAT_FORK_MAX_BYTES: usize = 128 * 1024;

/// 标题缺省值（与前端约定的新会话初始标题）。
const DEFAULT_TITLE: &str = "新会话";

/// 标题来源三态（语义见模块头）。
pub const TITLE_SOURCE_USER: &str = "user";
pub const TITLE_SOURCE_AUTO: &str = "auto";
pub const TITLE_SOURCE_LLM: &str = "llm";

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
    /// fork 来源父会话 id（非 fork 会话为 None；父删除后悬空，见模块头）。
    #[serde(default)]
    pub parent_session_id: Option<String>,
    /// fork 时复制到的父会话消息序号（含该条；非 fork 为 None）。
    #[serde(default)]
    pub forked_from_seq: Option<u64>,
    /// 标题来源三态，语义见模块头（None = 旧数据，按标题内容保守判定）。
    #[serde(default)]
    pub title_source: Option<String>,
}

/// fork 结果：新会话 + 有界复制的统计（`dropped` 如实标注，不静默丢）。
pub struct ForkOutcome {
    pub session: ChatSession,
    pub copied: usize,
    pub dropped: usize,
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
    /// LLM 标题任务在飞标记（进程内去重：同一会话并发多条消息只 spawn 一次）。
    title_inflight: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

impl ChatSessionStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            title_inflight: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
        }
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
            parent_session_id: None,
            forked_from_seq: None,
            title_source: None,
        };
        self.store.put(&Self::session_key(user_id, id), &s)?;
        Ok(s)
    }

    /// 读会话（带归属语义：只在该用户的键空间里找）。
    pub fn get(&self, user_id: &str, id: &str) -> anyhow::Result<Option<ChatSession>> {
        self.store.get(&Self::session_key(user_id, id))
    }

    /// 更新标题（用户手工改名 → title_source="user"，永不被自动标题覆盖）。
    pub fn rename(&self, user_id: &str, id: &str, title: &str) -> anyhow::Result<()> {
        self.set_title(user_id, id, title, TITLE_SOURCE_USER)
    }

    /// 内部统一标题写入（带来源标记）。
    fn set_title(&self, user_id: &str, id: &str, title: &str, source: &str) -> anyhow::Result<()> {
        if let Some(mut s) = self.get(user_id, id)? {
            s.title = title.to_string();
            s.title_source = Some(source.to_string());
            s.updated_at = now_ts();
            self.store.put(&Self::session_key(user_id, id), &s)?;
        }
        Ok(())
    }

    /// 标题仍是缺省值时用首条消息自动命名（截 30 字符，title_source="auto"，
    /// 可被 LLM 标题接手；用户 rename 过的不动）。
    pub fn rename_if_default(
        &self,
        user_id: &str,
        id: &str,
        first_message: &str,
    ) -> anyhow::Result<()> {
        if let Some(s) = self.get(user_id, id)? {
            if s.title.trim() == DEFAULT_TITLE || s.title.trim().is_empty() {
                let title: String = first_message.trim().chars().take(30).collect();
                if !title.is_empty() {
                    self.set_title(user_id, id, &title, TITLE_SOURCE_AUTO)?;
                }
            }
        }
        Ok(())
    }

    /// 标题是否还处于「可被 LLM 接手」状态（auto 或旧数据的缺省值）。
    /// 用户改过名（"user"）或已被 LLM 生成过（"llm"）都返回 false。
    fn title_llm_eligible(s: &ChatSession) -> bool {
        match s.title_source.as_deref() {
            Some(TITLE_SOURCE_AUTO) => true,
            Some(TITLE_SOURCE_USER) | Some(TITLE_SOURCE_LLM) => false,
            // 旧数据无来源字段：仅缺省标题视同 auto，否则按用户命名保守处理
            _ => s.title.trim() == DEFAULT_TITLE || s.title.trim().is_empty(),
        }
    }

    /// LLM 标题写回（带竞态守卫：飞行中用户 rename 过则放弃覆盖）。
    /// 返回是否实际写入。
    pub fn try_set_llm_title(&self, user_id: &str, id: &str, title: &str) -> anyhow::Result<bool> {
        let Some(s) = self.get(user_id, id)? else {
            return Ok(false);
        };
        if !Self::title_llm_eligible(&s) {
            return Ok(false);
        }
        self.set_title(user_id, id, title, TITLE_SOURCE_LLM)?;
        Ok(true)
    }

    /// 标记标题任务在飞（同会话去重）；已在飞返回 false。
    pub fn begin_title_task(&self, user_id: &str, id: &str) -> bool {
        self.title_inflight
            .lock()
            .expect("title_inflight poisoned")
            .insert(format!("{user_id}:{id}"))
    }

    /// 释放标题任务在飞标记。
    pub fn end_title_task(&self, user_id: &str, id: &str) {
        self.title_inflight
            .lock()
            .expect("title_inflight poisoned")
            .remove(&format!("{user_id}:{id}"));
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

    /// 从父会话第 `upto_seq` 条消息处（**含该条**）分叉新会话。
    ///
    /// - 父会话不存在 → `Ok(None)`（HTTP 404）；
    /// - `upto_seq` 越界 → `Err`（HTTP 400）；
    /// - 复制有界：超 [`CHAT_FORK_MAX_MESSAGES`] / [`CHAT_FORK_MAX_BYTES`]
    ///   时保留距分叉点最近的部分，丢弃条数进 `dropped`（见模块头）。
    /// - 标题与 `title_source` 继承父会话："user" 永不被覆盖，"auto" 仍可被
    ///   LLM 接手（子会话下一条用户消息照常触发）。
    pub fn fork(
        &self,
        user_id: &str,
        parent_id: &str,
        upto_seq: u64,
    ) -> anyhow::Result<Option<ForkOutcome>> {
        let parent = match self.get(user_id, parent_id)? {
            Some(s) => s,
            None => return Ok(None),
        };
        let msgs = self.messages(user_id, parent_id)?;
        if upto_seq >= msgs.len() as u64 {
            anyhow::bail!(
                "message_seq {upto_seq} out of range (parent has {} messages)",
                msgs.len()
            );
        }
        let mut kept: Vec<ChatMessage> = msgs[..=(upto_seq as usize)].to_vec();
        let mut dropped = 0usize;
        // 条数上限：保留尾部（距分叉点最近）
        if kept.len() > CHAT_FORK_MAX_MESSAGES {
            let cut = kept.len() - CHAT_FORK_MAX_MESSAGES;
            kept.drain(..cut);
            dropped += cut;
        }
        // 字节上限：从最早开始丢到不超限（至少保 1 条，保分叉点）
        let size_of =
            |m: &ChatMessage| -> usize { serde_json::to_vec(m).map(|v| v.len()).unwrap_or(0) };
        let mut total: usize = kept.iter().map(size_of).sum();
        while kept.len() > 1 && total > CHAT_FORK_MAX_BYTES {
            total -= size_of(&kept[0]);
            kept.remove(0);
            dropped += 1;
        }
        let child_id = new_id();
        let session = ChatSession {
            id: child_id.clone(),
            user_id: user_id.to_string(),
            title: parent.title.clone(),
            model: parent.model.clone(),
            created_at: now_ts(),
            updated_at: now_ts(),
            parent_session_id: Some(parent_id.to_string()),
            forked_from_seq: Some(upto_seq),
            title_source: parent.title_source.clone(),
        };
        self.store
            .put(&Self::session_key(user_id, &child_id), &session)?;
        for (seq, m) in kept.iter().enumerate() {
            self.store
                .put(&Self::msg_key(user_id, &child_id, seq as u64), m)?;
        }
        Ok(Some(ForkOutcome {
            session,
            copied: kept.len(),
            dropped,
        }))
    }

    /// 删除会话及其消息（不级联子会话，见模块头「删除语义」）。
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

/// POST /api/chat/sessions/:id/fork —— 从指定消息处分叉新会话。
///
/// 契约（前端对接用）：
/// - 请求体：`{ "message_seq": u64 }`（0-based，**含该条**，与 GET 详情返回
///   的 messages 数组下标一致；想「不含某条」传该条下标 - 1）。
/// - 成功：`{ "success": true, "data": { "session": <ChatSession>,
///   "copied": <复制条数>, "dropped": <因超上限丢弃条数> } }`；
///   `dropped > 0` 表示上下文头部被截，前端应如实提示。
/// - 新会话带 `parent_session_id` / `forked_from_seq` 可追溯；
///   删除父会话不级联（见模块头）。
#[derive(Debug, Deserialize)]
pub struct ForkChatSessionBody {
    pub message_seq: u64,
}

pub async fn handle_fork_chat_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ForkChatSessionBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    match state.chat_sessions.fork(&user.id, &id, body.message_seq) {
        Ok(Some(out)) => Ok(Json(json!({
            "success": true,
            "data": {
                "session": out.session,
                "copied": out.copied,
                "dropped": out.dropped,
            }
        }))),
        Ok(None) => Err(error_response("会话不存在", StatusCode::NOT_FOUND)),
        Err(e) if e.to_string().contains("out of range") => {
            Err(error_response(&e.to_string(), StatusCode::BAD_REQUEST))
        }
        Err(e) => Err(error_response(
            &e.to_string(),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
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
    // 首条用户消息自动命名（同步截断兜底）+ 异步 LLM 标题（可开关，
    // 失败静默降级；spawn 前占位防同会话并发重复任务）
    if seq == 0 && msg.role == "user" {
        let _ = state
            .chat_sessions
            .rename_if_default(&user.id, &id, &msg.content);
        if state.chat_sessions.begin_title_task(&user.id, &id) {
            title::spawn_auto_title(state.clone(), user.id.clone(), id.clone(), msg.content);
        }
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
    // 标题仍为缺省时用首条用户消息自动命名 + 异步 LLM 标题（同 append 口径）
    if session.title.trim() == "新会话" || session.title.trim().is_empty() {
        if let Some(first) = msgs
            .iter()
            .find(|m| m.role == "user" && !m.content.trim().is_empty())
        {
            let _ = state
                .chat_sessions
                .rename_if_default(&user.id, &id, &first.content);
            if state.chat_sessions.begin_title_task(&user.id, &id) {
                title::spawn_auto_title(
                    state.clone(),
                    user.id.clone(),
                    id.clone(),
                    first.content.clone(),
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> ChatSessionStore {
        ChatSessionStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            extra: None,
            created_at: 0,
        }
    }

    #[test]
    fn fork_copies_prefix_and_links_parent() {
        let st = store();
        st.create("u1", "p", "父会话", "gpt-x", None).unwrap();
        let msgs: Vec<ChatMessage> = (0..4).map(|i| msg("user", &format!("m{i}"))).collect();
        st.replace_messages("u1", "p", &msgs, true).unwrap();
        let out = st.fork("u1", "p", 1).unwrap().unwrap();
        assert_eq!(out.copied, 2);
        assert_eq!(out.dropped, 0);
        assert_eq!(out.session.parent_session_id.as_deref(), Some("p"));
        assert_eq!(out.session.forked_from_seq, Some(1));
        let child_msgs = st.messages("u1", &out.session.id).unwrap();
        assert_eq!(child_msgs.len(), 2);
        assert_eq!(child_msgs[1].content, "m1");
    }

    #[test]
    fn fork_missing_parent_is_none() {
        let st = store();
        assert!(st.fork("u1", "ghost", 0).unwrap().is_none());
    }

    #[test]
    fn fork_out_of_range_rejects() {
        let st = store();
        st.create("u1", "p", "t", "", None).unwrap();
        st.replace_messages("u1", "p", &[msg("user", "only")], true)
            .unwrap();
        assert!(st.fork("u1", "p", 1).is_err());
        assert!(st.fork("u1", "p", 0).is_ok());
    }

    #[test]
    fn fork_over_message_limit_keeps_tail_and_counts_dropped() {
        let st = store();
        st.create("u1", "p", "t", "", None).unwrap();
        let total = CHAT_FORK_MAX_MESSAGES + 50;
        let msgs: Vec<ChatMessage> = (0..total).map(|i| msg("user", &format!("m{i}"))).collect();
        st.replace_messages("u1", "p", &msgs, true).unwrap();
        let out = st.fork("u1", "p", (total - 1) as u64).unwrap().unwrap();
        assert_eq!(out.copied, CHAT_FORK_MAX_MESSAGES);
        assert_eq!(out.dropped, 50);
        let child_msgs = st.messages("u1", &out.session.id).unwrap();
        // 尾部保留：第一条是被丢 50 条后的 m50，最后一条是分叉点
        assert_eq!(child_msgs[0].content, "m50");
        assert_eq!(child_msgs.last().unwrap().content, format!("m{}", total - 1));
    }

    #[test]
    fn fork_over_byte_limit_drops_head_keeps_fork_point() {
        let st = store();
        st.create("u1", "p", "t", "", None).unwrap();
        let big = "x".repeat(8 * 1024);
        let msgs: Vec<ChatMessage> = (0..250).map(|_| msg("user", &big)).collect();
        st.replace_messages("u1", "p", &msgs, true).unwrap();
        let out = st.fork("u1", "p", 249).unwrap().unwrap();
        assert!(out.dropped > 0);
        let child_msgs = st.messages("u1", &out.session.id).unwrap();
        let total: usize = child_msgs
            .iter()
            .map(|m| serde_json::to_vec(m).unwrap().len())
            .sum();
        assert!(total <= CHAT_FORK_MAX_BYTES);
        // 至少保住分叉点那条
        assert_eq!(child_msgs.last().unwrap().content, big);
    }

    #[test]
    fn llm_title_never_overrides_user_rename() {
        let st = store();
        // 用户手工改名 → title_source=user，LLM 拒绝覆盖
        st.create("u1", "s", DEFAULT_TITLE, "", None).unwrap();
        st.rename("u1", "s", "我的对话").unwrap();
        assert!(!st.try_set_llm_title("u1", "s", "LLM 标题").unwrap());
        let s = st.get("u1", "s").unwrap().unwrap();
        assert_eq!(s.title, "我的对话");
        assert_eq!(s.title_source.as_deref(), Some(TITLE_SOURCE_USER));

        // auto 标题（截断命名）可被 LLM 接手，写回后置 llm 不再重复覆盖
        st.create("u1", "s2", DEFAULT_TITLE, "", None).unwrap();
        st.rename_if_default("u1", "s2", "第一条消息").unwrap();
        let s2 = st.get("u1", "s2").unwrap().unwrap();
        assert_eq!(s2.title, "第一条消息");
        assert_eq!(s2.title_source.as_deref(), Some(TITLE_SOURCE_AUTO));
        assert!(st.try_set_llm_title("u1", "s2", "LLM 标题").unwrap());
        let s2 = st.get("u1", "s2").unwrap().unwrap();
        assert_eq!(s2.title, "LLM 标题");
        assert_eq!(s2.title_source.as_deref(), Some(TITLE_SOURCE_LLM));
        assert!(!st.try_set_llm_title("u1", "s2", "另一个").unwrap());
    }

    #[test]
    fn legacy_session_without_source_is_conservative() {
        let st = store();
        // 旧数据（title_source=None）+ 非缺省标题：按用户命名处理，不覆盖
        st.create("u1", "s", "旧标题可能是用户改的", "", None)
            .unwrap();
        assert!(!st.try_set_llm_title("u1", "s", "LLM").unwrap());
        // 旧数据 + 缺省标题：视同 auto，可接手
        st.create("u1", "s3", DEFAULT_TITLE, "", None).unwrap();
        assert!(st.try_set_llm_title("u1", "s3", "LLM").unwrap());
    }

    #[test]
    fn title_task_dedup_marks_inflight() {
        let st = store();
        assert!(st.begin_title_task("u1", "s"));
        assert!(!st.begin_title_task("u1", "s"));
        st.end_title_task("u1", "s");
        assert!(st.begin_title_task("u1", "s"));
    }
}
