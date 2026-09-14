//! Agent 工作台 API 端点（阶段 1c + 阶段 2 审批）。
//!
//! - `GET /api/agent/sessions`：会话列表
//! - `POST /api/agent/sessions`：建会话
//! - `GET /api/agent/sessions/:id`：会话详情（含消息）
//! - `POST /api/agent/sessions/:id/chat`：对话（SSE 流式，推 AgentEvent）
//! - `POST /api/agent/approvals/:request_id`：审批响应（allow/deny/remember）
//!
//! 全部管理面鉴权（verify_admin），Agent 未启用（`[agent].enabled=false`）
//! 时返回 503，提示先在 config 开启。

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Json, Response};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::agent::approval::ApprovalResult;
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
            axum::routing::get(handle_get_session).delete(handle_delete_session),
        )
        .route(
            "/api/agent/sessions/:id/chat",
            axum::routing::post(handle_chat),
        )
        .route(
            "/api/agent/approvals/:request_id",
            axum::routing::post(handle_approval),
        )
        .route("/api/agent/config", axum::routing::get(handle_get_config))
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
    /// 覆盖 `[agent].model`（前端模型选择器）。空/缺省用配置值。
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApprovalRequest {
    /// allow / deny / remember（本会话总是允许）
    pub action: String,
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

/// DELETE /api/agent/sessions/:id —— 删除会话及其消息。
pub async fn handle_delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    agent.session_store.delete(&id).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;
    Ok(Json(json!({ "success": true, "data": null })))
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

    // 会话角色（观察员/运维员），决定写工具是否放行
    let session_role = agent
        .session_store
        .get(&id)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e.to_string() })),
            )
        })?
        .map(|s| s.role)
        .unwrap_or_default();

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
    // 上下文压缩：超出 max_history 时只保留最近 N 条（防长会话撑爆上下文窗口）
    let max_history = agent.config.max_history.max(1);
    if convo.len() > max_history {
        convo = convo.split_off(convo.len() - max_history);
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

    // 会话标题仍是缺省"新会话"时，用首条用户消息自动命名（截 30 字符）
    let _ = agent.session_store.rename_if_default(&id, &body.message);

    let mut config = agent.config.clone();
    // 前端模型选择器覆盖：请求带 model 时替换配置快照中的模型
    //（渠道锁定逻辑不变——若 [agent].channel 非空仍只走该渠道）
    if let Some(m) = body.model.as_deref() {
        if !m.trim().is_empty() {
            config.model = m.trim().to_string();
        }
    }
    let state2 = state.clone();
    let headers2 = headers.clone();
    let agent2 = agent.clone();
    let approvals2 = agent.approvals.clone();
    let session_id = id.clone();

    // 后台跑 runner，事件经 mpsc 转发为 SSE 流；同时累积工具轨迹做决策回放。
    let (tx, rx) = mpsc::channel::<String>(64);
    let trace: std::sync::Arc<tokio::sync::Mutex<Vec<Value>>> =
        std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let trace2 = trace.clone();
    let sink: runner::EventSink = std::sync::Arc::new(move |ev| {
        let tx = tx.clone();
        let agent = agent2.clone();
        let sid = session_id.clone();
        let trace = trace2.clone();
        Box::pin(async move {
            // 累积工具调用/结果/审批事件，Final 时一并落库（决策回放）
            match &ev {
                AgentEvent::ToolCall { name, arguments } => {
                    trace
                        .lock()
                        .await
                        .push(json!({ "type": "tool_call", "name": name, "arguments": arguments }));
                }
                AgentEvent::ToolResult { name, ok, text } => {
                    trace.lock().await.push(
                        json!({ "type": "tool_result", "name": name, "ok": ok, "text": text }),
                    );
                }
                AgentEvent::ApprovalRequest {
                    request_id,
                    name,
                    arguments,
                } => {
                    trace.lock().await.push(json!({ "type": "approval_request", "request_id": request_id, "name": name, "arguments": arguments }));
                }
                AgentEvent::ApprovalResolved { name, approved } => {
                    trace.lock().await.push(
                        json!({ "type": "approval_resolved", "name": name, "approved": approved }),
                    );
                }
                _ => {}
            }
            let json = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".to_string());
            let _ = tx.send(format!("data: {json}\n\n")).await;
            if let AgentEvent::Final { content } = &ev {
                let tool_trace = trace.lock().await;
                let tool_calls = if tool_trace.is_empty() {
                    None
                } else {
                    Some(Value::Array(tool_trace.clone()))
                };
                let _ = agent.session_store.append_message(
                    &sid,
                    &crate::agent::session::AgentMessage {
                        role: "assistant".to_string(),
                        content: content.clone(),
                        tool_calls,
                        tool_result: None,
                    },
                );
            }
        })
    });
    let sink2 = sink.clone();
    tokio::spawn(async move {
        runner::run(
            &state2,
            &headers2,
            &config,
            convo,
            &approvals2,
            &id,
            session_role,
            &sink2,
        )
        .await;
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

/// POST /api/agent/approvals/:request_id —— 审批响应。
pub async fn handle_approval(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    Json(body): Json<ApprovalRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    let result = match body.action.as_str() {
        "allow" => ApprovalResult::Approved,
        "remember" => ApprovalResult::RememberAllow,
        "deny" => ApprovalResult::Denied,
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "action 必须为 allow/deny/remember" })),
            ))
        }
    };
    let ok = agent.approvals.resolve(&request_id, result);
    // 审批动作本身留痕（谁在何时点了允许/拒绝）——工具执行侧的审计
    // 由 runner 落（含决策来源），此处只记录 resolve 是否命中挂起请求
    if ok {
        let action = format!("agent_approval_{}", body.action);
        crate::api::admin::common::record_audit(
            &state,
            &crate::api::admin::common::admin_id_from_session(&state, &headers).await,
            &action,
            &format!("request_id={request_id}"),
            None,
            Some(json!({ "resolved": true })),
        );
    }
    Ok(Json(json!({ "success": ok, "data": null })))
}

/// GET /api/agent/config —— Agent 配置只读视图（前端模型选择器用）。
///
/// 返回当前 `[agent]` 的 model/channel（channel 锁定时模型选择器
/// 仍可换模型，但渠道不换），与启用状态。
pub async fn handle_get_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let agent = agent_state(&state)?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "model": agent.config.model,
            "channel": agent.config.channel,
            "max_turns": agent.config.max_turns,
            "approval_timeout_secs": agent.config.approval_timeout_secs,
        }
    })))
}
