//! MonkeyCode（长亭科技 AI 开发平台）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/monkeycode`：
//! - REST：`GET /api/v1/users/models`（模型目录）→ `POST /api/v1/users/tasks` 建任务；
//! - 任务流：`wss://<站点>/api/v1/users/tasks/stream?id=<task>&mode=` 的 WebSocket，
//!   连接成功后客户端**必须先发** `auto-approve` + `user-input`；
//! - 下行是 ACP 事件（`task-started` / `task-running` / `task-ended` / `task-error`），
//!   `task-running` 里 `kind=acp_event` 的 `data` 是 JSON 字符串，再按
//!   `agent_message_chunk` / `agent_thought_chunk` / `tool_call` 分出正文/思考/工具。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::OnceLock;

use async_trait::async_trait;
use base64::Engine as _;
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};

const STATUS_PATH: &str = "/api/v1/users/status";
const MODELS_PATH: &str = "/api/v1/users/models";
const TASK_CREATE_PATH: &str = "/api/v1/users/tasks";
const TASK_STREAM_PATH: &str = "/api/v1/users/tasks/stream";
const SESSION_COOKIE: &str = "monkeycode_ai_session";
const USER_AGENT: &str = "monkeycode-local-proxy";
const DEFAULT_HOST_ID: &str = "public_host";
const CATALOG_TTL_MS: i64 = 300_000;

pub struct MonkeyCodeBridge {
    base: String,
    session: String,
    client: reqwest::Client,
}

impl MonkeyCodeBridge {
    pub fn new(
        base: impl Into<String>,
        session: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let session = session.into();
        let session = super::normalize_credential(
            &session,
            &["cookie:", "monkeycode_ai_session=", "session="],
        );
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            session: session.to_string(),
            client,
        }
    }

    fn json_headers(&self, accept: &str) -> Vec<(String, String)> {
        vec![
            ("Accept".to_string(), accept.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            (
                "Cookie".to_string(),
                format!("{SESSION_COOKIE}={}", self.session.trim()),
            ),
            ("Origin".to_string(), self.base.clone()),
        ]
    }

    async fn get_json(&self, path: &str) -> Result<Value, BridgeError> {
        let mut req = self.client.get(format!("{}{path}", self.base));
        for (name, value) in self.json_headers("application/json") {
            req = req.header(name, value);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("MonkeyCode {path} 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        resp.json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<Value, BridgeError> {
        let mut req = self.client.post(format!("{}{path}", self.base));
        for (name, value) in self.json_headers("application/json") {
            req = req.header(name, value);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("MonkeyCode {path} 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        resp.json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))
    }

    /// 模型目录（进程级缓存 5 分钟）。
    async fn catalog(&self) -> Result<Value, BridgeError> {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(cached) = cached_catalog(&self.base, now) {
            return Ok(cached);
        }
        let value = self.get_json(MODELS_PATH).await?;
        store_catalog(&self.base, &value, now);
        Ok(value)
    }

    /// 解析建任务需要的三元组（model_id / cli_name / image_id）。
    async fn resolve_target(&self, model: &str) -> Result<(String, String, String), BridgeError> {
        let status = self.get_json(STATUS_PATH).await.ok();
        let image_id = status
            .as_ref()
            .and_then(|value| {
                value
                    .get("data")
                    .and_then(|data| data.get("image_id"))
                    .or_else(|| value.get("data").and_then(|data| data.get("imageId")))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();
        let catalog = self.catalog().await?;
        let rows = catalog
            .get("data")
            .and_then(|data| data.get("models"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let matched = rows
            .iter()
            .find(|row| text_of(row, "upstreamId").eq_ignore_ascii_case(model.trim()))
            .or_else(|| {
                rows.iter().find(|row| {
                    text_of(row, "name").eq_ignore_ascii_case(model.trim())
                        || text_of(row, "id").eq_ignore_ascii_case(model.trim())
                })
            })
            .cloned()
            .ok_or_else(|| {
                BridgeError::ModelNotFound(format!(
                    "MonkeyCode 模型目录里没有 {model}（可用 GET /v1/models 查看）"
                ))
            })?;
        let model_id = text_of(&matched, "upstreamId");
        if model_id.is_empty() {
            return Err(BridgeError::upstream_status(
                502,
                "MonkeyCode 模型目录缺少 upstreamId（上游响应异常）",
            ));
        }
        let interface_type = text_of(&matched, "interfaceType");
        let cli_name = match interface_type.to_ascii_lowercase().as_str() {
            "openai_responses" => "codex",
            "anthropic" => "claude",
            _ => "opencode",
        }
        .to_string();
        Ok((model_id, cli_name, image_id))
    }
}

#[async_trait]
impl Bridge for MonkeyCodeBridge {
    fn name(&self) -> &'static str {
        "monkeycode"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut usage = UsageStats::default();
        let mut finish = FinishReason::Stop;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
            if let Some(text) = chunk.delta.reasoning {
                reasoning.push_str(&text);
            }
            if let Some(deltas) = chunk.delta.tool_calls {
                for delta in deltas {
                    let Some(name) = delta.function_name else {
                        continue;
                    };
                    calls.push(ToolCall {
                        id: delta.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        function_name: name,
                        arguments: delta.arguments.unwrap_or_default(),
                    });
                }
            }
            if let Some(reason) = chunk.finish_reason {
                finish = reason;
            }
            if let Some(stats) = chunk.usage {
                usage = stats;
            }
        }
        let mut message = ChatMessage::assistant(content);
        message.reasoning = (!reasoning.is_empty()).then_some(reasoning);
        message.tool_calls = (!calls.is_empty()).then_some(calls);
        Ok(ChatResponse {
            id: format!("monkeycode-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message,
            finish_reason: finish,
            usage,
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let prompt = build_prompt(req);
        let (model_id, cli_name, image_id) = self.resolve_target(&req.model).await?;

        let body = json!({
            "content": prompt,
            "host_id": DEFAULT_HOST_ID,
            "image_id": image_id,
            "model_id": model_id,
            "cli_name": cli_name,
            "resource": { "core": 1, "memory": 1073741824, "life": 3600 },
            "repo": { "repo_url": "", "branch": "master", "repo_filename": "", "zip_url": "" },
        });
        let created = self.post_json(TASK_CREATE_PATH, &body).await?;
        let task_id = created
            .get("data")
            .and_then(|data| data.get("id").or_else(|| data.get("task_id")))
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                BridgeError::upstream_status(502, "MonkeyCode 建任务返回缺少 task id")
            })?;

        let url = format!(
            "{}{TASK_STREAM_PATH}?id={task_id}&mode=",
            self.base
                .replacen("https://", "wss://", 1)
                .replacen("http://", "ws://", 1)
        );
        let mut request = url
            .into_client_request()
            .map_err(|e| BridgeError::Config(format!("MonkeyCode WS 地址无效：{e}")))?;
        {
            let cookie = format!("{SESSION_COOKIE}={}", self.session.trim());
            let headers = request.headers_mut();
            if let Ok(value) = http::HeaderValue::from_bytes(USER_AGENT.as_bytes()) {
                headers.insert("User-Agent", value);
            }
            if let Ok(value) = http::HeaderValue::from_bytes(self.base.as_bytes()) {
                headers.insert("Origin", value);
            }
            if let Ok(value) = http::HeaderValue::from_bytes(cookie.as_bytes()) {
                headers.insert("Cookie", value);
            }
        }
        let (socket, _) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| BridgeError::Transport(format!("MonkeyCode WS 连接失败：{e}")))?;
        let (mut write, read) = socket.split();

        // 连接建立后必须先发 auto-approve + user-input，上游才开始执行
        write
            .send(Message::Text(json!({ "type": "auto-approve" }).to_string()))
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let payload = json!({
            "type": "user-input",
            "data": json!({
                "content": base64::engine::general_purpose::STANDARD.encode(prompt.as_bytes()),
                "attachments": [],
            })
            .to_string(),
        });
        write
            .send(Message::Text(payload.to_string()))
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        let id = format!("monkeycode-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let stream = futures::stream::unfold(
            (
                read,
                write,
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    ended: false,
                },
            ),
            |(mut read, mut write, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (read, write, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    let next = read.next().await;
                    let text = match next {
                        Some(Ok(Message::Text(text))) => text.to_string(),
                        Some(Ok(Message::Binary(_))) => continue,
                        Some(Ok(Message::Close(_))) => {
                            pending.push_back(Ok(finish_chunk(&flow.id, &flow.model)));
                            flow.ended = true;
                            continue;
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                            continue;
                        }
                        None => {
                            pending.push_back(Ok(finish_chunk(&flow.id, &flow.model)));
                            flow.ended = true;
                            continue;
                        }
                    };
                    match decode_frame(&text, &flow.id, &flow.model) {
                        Decoded::Nothing => {}
                        Decoded::Chunk(chunk) => pending.push_back(Ok(chunk)),
                        Decoded::Reply(payload) => {
                            let _ = write.send(Message::Text(payload)).await;
                        }
                        Decoded::Done => {
                            pending.push_back(Ok(finish_chunk(&flow.id, &flow.model)));
                            flow.ended = true;
                        }
                        Decoded::Err(err) => {
                            pending.push_back(Err(err));
                            flow.ended = true;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

struct Flow {
    id: String,
    model: String,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Reply(String),
    Done,
    Err(BridgeError),
}

fn decode_frame(text: &str, id: &str, model: &str) -> Decoded {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Decoded::Nothing;
    };
    match value.get("type").and_then(Value::as_str).unwrap_or("") {
        "ping" => Decoded::Reply(json!({ "type": "ping" }).to_string()),
        "task-started" => Decoded::Nothing,
        "task-ended" => Decoded::Done,
        "task-error" => {
            let message = value
                .get("message")
                .or_else(|| value.get("data"))
                .and_then(Value::as_str)
                .unwrap_or("任务失败");
            Decoded::Err(BridgeError::upstream_status(
                502,
                format!("MonkeyCode 任务错误：{message}"),
            ))
        }
        "task-running" => decode_running(&value, id, model),
        _ => Decoded::Nothing,
    }
}

fn decode_running(value: &Value, id: &str, model: &str) -> Decoded {
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "acp_event" {
        return Decoded::Nothing;
    }
    let Some(raw) = value.get("data").and_then(Value::as_str) else {
        return Decoded::Nothing;
    };
    let Ok(acp) = serde_json::from_str::<Value>(raw) else {
        return Decoded::Nothing;
    };
    match acp.get("type").and_then(Value::as_str).unwrap_or("") {
        "agent_message_chunk" => {
            let text = chunk_text(&acp);
            if text.is_empty() {
                return Decoded::Nothing;
            }
            Decoded::Chunk(delta_chunk(id, model, Some(text), None))
        }
        "agent_thought_chunk" => {
            let text = chunk_text(&acp);
            if text.is_empty() {
                return Decoded::Nothing;
            }
            Decoded::Chunk(delta_chunk(id, model, None, Some(text)))
        }
        "tool_call" => {
            let name = acp
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let arguments = text_of_value(acp.get("tool_input"));
            Decoded::Chunk(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta {
                    content: None,
                    reasoning: None,
                    tool_calls: Some(vec![ToolCallDelta {
                        index: 0,
                        id: Some(uuid::Uuid::new_v4().to_string()),
                        function_name: Some(name),
                        arguments: Some(arguments),
                    }]),
                },
                finish_reason: None,
                usage: None,
            })
        }
        "usage_update" => {
            let usage = acp.get("usage").map(usage_of);
            match usage {
                Some(stats) => Decoded::Chunk(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: ChatDelta::default(),
                    finish_reason: None,
                    usage: Some(stats),
                }),
                None => Decoded::Nothing,
            }
        }
        _ => Decoded::Nothing,
    }
}

fn delta_chunk(
    id: &str,
    model: &str,
    content: Option<String>,
    reasoning: Option<String>,
) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta {
            content,
            reasoning,
            tool_calls: None,
        },
        finish_reason: None,
        usage: None,
    }
}

fn finish_chunk(id: &str, model: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
    }
}

fn chunk_text(acp: &Value) -> String {
    let Some(content) = acp.get("content") else {
        return text_of_value(acp.get("text"));
    };
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        other => other.to_string(),
    }
}

fn text_of_value(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn text_of(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["input_tokens", "prompt_tokens", "inputTokens"]);
    let completion = number(&["output_tokens", "completion_tokens", "outputTokens"]);
    let total = number(&["total_tokens", "totalTokens"]);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + completion
        },
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// OpenAI messages 拍平成单条任务 prompt。
pub fn build_prompt(req: &ChatFormat) -> String {
    let messages: Vec<&ChatMessage> = req
        .messages
        .iter()
        .filter(|m| !m.content_str().trim().is_empty())
        .collect();
    if messages.is_empty() {
        return "你好".to_string();
    }
    if messages.len() == 1 {
        return messages[0].content_str().to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push("[以下是本轮之前的对话记录，供你了解上下文]".to_string());
    for message in messages {
        let role = match message.role {
            Role::System => "系统指令",
            Role::Assistant => "助手",
            _ => "用户",
        };
        lines.push(format!("[{role}] {}", message.content_str()));
    }
    lines.push("[对话记录结束；请回应其中最后一条用户消息]".to_string());
    lines.join("\n\n")
}

fn slots() -> &'static Mutex<(String, i64, Value)> {
    static SLOT: OnceLock<Mutex<(String, i64, Value)>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new((String::new(), 0, Value::Null)))
}

fn cached_catalog(base: &str, now: i64) -> Option<Value> {
    let guard = slots().lock().ok()?;
    if guard.0 != base || now - guard.1 >= CATALOG_TTL_MS || guard.2.is_null() {
        return None;
    }
    Some(guard.2.clone())
}

fn store_catalog(base: &str, value: &Value, now: i64) {
    if let Ok(mut guard) = slots().lock() {
        *guard = (base.to_string(), now, value.clone());
    }
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn format(messages: Vec<ChatMessage>) -> ChatFormat {
        ChatFormat {
            model: "gpt-4".into(),
            messages,
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: true,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        }
    }

    #[test]
    fn ping_frame_requests_reply() {
        match decode_frame(r#"{"type":"ping"}"#, "x", "m") {
            Decoded::Reply(payload) => assert!(payload.contains("ping")),
            _ => panic!("ping 帧应回一条 ping"),
        }
    }

    #[test]
    fn agent_message_chunk_becomes_content() {
        let frame = json!({
            "type": "task-running",
            "kind": "acp_event",
            "data": json!({ "type": "agent_message_chunk", "content": "你好" }).to_string(),
        });
        match decode_frame(&frame.to_string(), "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.content.as_deref(), Some("你好")),
            _ => panic!("agent_message_chunk 应转成正增量"),
        }
    }

    #[test]
    fn task_ended_finishes_stream() {
        match decode_frame(r#"{"type":"task-ended"}"#, "x", "m") {
            Decoded::Done => {}
            _ => panic!("task-ended 应结束流"),
        }
    }

    #[test]
    fn prompt_flattens_history() {
        let prompt = build_prompt(&format(vec![user("第一问"), user("第二问")]));
        assert!(prompt.contains("[用户] 第一问"));
    }
}
