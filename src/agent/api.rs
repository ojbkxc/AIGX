//! Agent 工作台 API 端点（阶段 1c）。
//!
//! - `GET /api/agent/sessions`：会话列表
//! - `POST /api/agent/sessions`：建会话
//! - `GET /api/agent/sessions/:id`：会话详情（含消息）
//! - `POST /api/agent/sessions/:id/chat`：对话（SSE 流式，推 AgentEvent）
//!
//! 全部管理面鉴权（verify_admin），Agent 未启用（`[agent].enabled=false`）
//! 时返回 503，提示先在 config 开启。

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::agent::runner::{self, AgentEvent};
use crate::api::admin::common::verify_admin;
use crate::api::openai::AppState;

/// Agent 路由（挂在 main.rs，无状态 Router，与其它路由组一致，最后统一 with_state）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/agent/sessions",
            axum::routing::get(handle_list_sessions).post(handle_create_session),
        )
        .route(
            "/api/agent/sessions/:id",
            axum::routing::get(handle_get_session),
        )
        .route(
            "/api/agent/sessions/:id/chat",
            axum::routing::post(handle_chat),
        )
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
}

/// 取 Agent 状态；未启用返回 503。
fn agent_state(state: &AppState) -> Result<&crate::agent::AgentState, (StatusCode, Json<Value>)> {
    state.agent_state.as_deref().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "success": false, "error": "AI 运维 Agent 未启用（config [agent].enabled）" })),
        )
    })
}

/// GET /api/agent/sessions
pub async fn handle_list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    let sessions = agent.session_store.list().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(json!({ "success": true, "data": sessions })))
}

/// POST /api/agent/sessions
pub async fn handle_create_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateSessionRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    let id = format!("{:032x}", rand::random::<u128>());
    let title = if body.title.trim().is_empty() {
        "新会话".to_string()
    } else {
        body.title.clone()
    };
    let role = match body.role.as_deref() {
        Some("operator") => crate::agent::session::AgentRole::Operator,
        _ => crate::agent::session::AgentRole::Observer,
    };
    agent
        .session_store
        .create(
            &id,
            &title,
            &agent.config.model,
            &agent.config.channel,
            role,
        )
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?;
    Ok(Json(
        json!({ "success": true, "data": { "id": id, "title": title } }),
    ))
}

/// GET /api/agent/sessions/:id
pub async fn handle_get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    let session = agent
        .session_store
        .get(&id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "Session not found" })),
            )
        })?;
    let messages = agent.session_store.messages(&id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(
        json!({ "success": true, "data": { "session": session, "messages": messages } }),
    ))
}

/// POST /api/agent/sessions/:id/chat —— SSE 流式推 AgentEvent。
pub async fn handle_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChatRequest>,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;

    // 读会话历史消息，组装 LLM 对话上下文
    let history = agent.session_store.messages(&id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let mut convo = Vec::new();
    for m in history {
        let role = match m.role.as_str() {
            "user" => crate::bridge::Role::User,
            "assistant" => crate::bridge::Role::Assistant,
            _ => continue,
        };
        convo.push(crate::bridge::ChatMessage {
            role,
            content: Some(m.content),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        });
    }
    // 追加本轮用户输入
    convo.push(crate::agent::llm::user_message(body.message.clone()));

    // 落库：先存用户消息
    let _ = agent.session_store.append_message(
        &id,
        &crate::agent::session::AgentMessage {
            role: "user".to_string(),
            content: body.message.clone(),
            tool_calls: None,
            tool_result: None,
        },
    );

    let config = agent.config.clone();
    let state2 = state.clone();
    let headers2 = headers.clone();
    let agent2 = agent.clone();
    let session_id = id.clone();

    // 后台跑 runner，事件经 mpsc 转发为 SSE 流
    let (tx, rx) = mpsc::channel::<String>(32);
    tokio::spawn(async move {
        let outcome = runner::run(&state2, &headers2, &config, convo).await;
        for ev in outcome.events {
            let json = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
            if tx.send(format!("data: {json}\n\n")).await.is_err() {
                break;
            }
            if let AgentEvent::Final { content } = &ev {
                let _ = agent2.session_store.append_message(
                    &session_id,
                    &crate::agent::session::AgentMessage {
                        role: "assistant".to_string(),
                        content: content.clone(),
                        tool_calls: None,
                        tool_result: None,
                    },
                );
            }
        }
        let _ = tx.send("data: [DONE]\n\n".to_string()).await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|s| Ok::<axum::body::Bytes, std::convert::Infallible>(axum::body::Bytes::from(s)));
    let body = Body::from_stream(stream);
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    Ok(resp)
}
