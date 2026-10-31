//! Command Code（`api.commandcode.ai`）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/commandcode`：
//! - `POST {base}/alpha/generate`，`Authorization: Bearer <user_ API Key>`；
//! - 请求体是 **8 键信封**（`config` / `memory` / `taste` / `skills` /
//!   `permissionMode` / `threadId` / `mode` / `params`），system 提到
//!   `params.system` 块数组，`params.stream` 恒 true；
//! - 响应是 **NDJSON**（一行一个 JSON、HTTP 恒 200，错误是流内
//!   `{"type":"error"}` 事件）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};

const PROTOCOL_VERSION: &str = "1.53.1";
const CLI_USER_AGENT: &str = "cli";
const CLI_ENVIRONMENT: &str = "production";
const GENERATE_PATH: &str = "/alpha/generate";

pub struct CommandCodeBridge {
    base: String,
    api_key: String,
    client: reqwest::Client,
}

impl CommandCodeBridge {
    pub fn new(
        base: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let api_key = api_key.into();
        let api_key = super::normalize_credential(&api_key, &["authorization:", "bearer"]);
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client,
        }
    }

    async fn send(&self, body: &Value, session_id: &str) -> Result<reqwest::Response, BridgeError> {
        let resp = self
            .client
            .post(format!("{}{GENERATE_PATH}", self.base))
            .header("Content-Type", "application/json")
            .header("Accept", "application/x-ndjson")
            .header("User-Agent", CLI_USER_AGENT)
            .header("x-command-code-version", PROTOCOL_VERSION)
            .header("x-cli-environment", CLI_ENVIRONMENT)
            .header("x-project-slug", "commandcode")
            .header("x-taste-learning", "false")
            .header("x-session-id", session_id)
            .header("traceparent", traceparent())
            .header("Authorization", format!("Bearer {}", self.api_key.trim()))
            .json(body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Command Code 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        Ok(resp)
    }
}

#[async_trait]
impl Bridge for CommandCodeBridge {
    fn name(&self) -> &'static str {
        "commandcode"
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
            id: format!("commandcode-{}", uuid::Uuid::new_v4()),
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
        let session_id = uuid::Uuid::new_v4().to_string();
        let body = build_envelope(req, &session_id);
        let resp = self.send(&body, &session_id).await?;

        let id = format!("commandcode-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                Vec::<u8>::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    ended: false,
                    saw_terminal: false,
                },
            ),
            |(mut bytes, mut buffer, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (bytes, buffer, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n')
                            {
                                let line: Vec<u8> = buffer.drain(..=newline).collect();
                                let text = String::from_utf8_lossy(&line).trim().to_string();
                                if text.is_empty() {
                                    continue;
                                }
                                match decode_line(&text, &flow.id, &flow.model) {
                                    Decoded::Nothing => {}
                                    Decoded::Chunk(chunk) => pending.push_back(Ok(chunk)),
                                    Decoded::Done(reason, usage) => {
                                        flow.saw_terminal = true;
                                        pending.push_back(Ok(ChatChunk {
                                            id: flow.id.clone(),
                                            model: flow.model.clone(),
                                            delta: ChatDelta::default(),
                                            finish_reason: Some(reason),
                                            usage,
                                        }));
                                    }
                                    Decoded::Err(err) => {
                                        pending.push_back(Err(err));
                                        flow.ended = true;
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            // 尾行（无换行）
                            if !buffer.is_empty() {
                                let text = String::from_utf8_lossy(&buffer).trim().to_string();
                                buffer.clear();
                                if !text.is_empty() {
                                    if let Decoded::Chunk(chunk) =
                                        decode_line(&text, &flow.id, &flow.model)
                                    {
                                        pending.push_back(Ok(chunk));
                                    }
                                }
                            }
                            // 没有任何完成信号就 EOF：按截断报错（不伪造 finish）
                            if !flow.saw_terminal {
                                pending.push_back(Err(BridgeError::upstream_status(
                                    502,
                                    "Command Code 流未收到 finish 事件即结束（响应被截断）",
                                )));
                            }
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
    saw_terminal: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done(FinishReason, Option<UsageStats>),
    Err(BridgeError),
}

fn decode_line(line: &str, id: &str, model: &str) -> Decoded {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Decoded::Nothing;
    };
    let event = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match event.as_str() {
        "text-delta" => {
            let text = value
                .get("text")
                .or_else(|| value.get("delta"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.is_empty() {
                return Decoded::Nothing;
            }
            Decoded::Chunk(delta_chunk(id, model, Some(text.to_string()), None))
        }
        "reasoning-delta" => {
            let text = value
                .get("text")
                .or_else(|| value.get("delta"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.is_empty() {
                return Decoded::Nothing;
            }
            Decoded::Chunk(delta_chunk(id, model, None, Some(text.to_string())))
        }
        "tool-call" => {
            let name = value
                .get("toolName")
                .or_else(|| value.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                return Decoded::Nothing;
            }
            let call_id = value
                .get("toolCallId")
                .or_else(|| value.get("id"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let args = value
                .get("input")
                .or_else(|| value.get("arguments"))
                .map(|raw| {
                    raw.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| raw.to_string())
                });
            Decoded::Chunk(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta {
                    content: None,
                    reasoning: None,
                    tool_calls: Some(vec![ToolCallDelta {
                        index: 0,
                        id: call_id,
                        function_name: Some(name),
                        arguments: args,
                    }]),
                },
                finish_reason: None,
                usage: None,
            })
        }
        "finish-step" | "finish" => {
            let reason = map_finish(
                value
                    .get("finishReason")
                    .or_else(|| value.get("finish_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            let usage = value.get("usage").map(usage_of);
            if event == "finish" {
                // 终态：先给 finish 帧，再结束流
                Decoded::Done(reason, usage)
            } else {
                Decoded::Chunk(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: ChatDelta::default(),
                    finish_reason: None,
                    usage,
                })
            }
        }
        "error" => {
            let status = value
                .get("statusCode")
                .or_else(|| value.get("status_code"))
                .and_then(Value::as_i64)
                .unwrap_or(502);
            let message = value
                .get("message")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("上游错误");
            Decoded::Err(BridgeError::upstream_status(
                map_error_status(status),
                format!("Command Code 流内错误：[{status}] {message}"),
            ))
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

fn map_finish(raw: &str) -> FinishReason {
    match raw.trim().to_ascii_lowercase().as_str() {
        "tool-calls" | "tool_calls" | "tool_use" => FinishReason::ToolCalls,
        "length" | "max_tokens" | "max_output_tokens" | "model_context_window_exceeded" => {
            FinishReason::Length
        }
        _ => FinishReason::Stop,
    }
}

fn map_error_status(status: i64) -> u16 {
    match status {
        400 | 422 => 400,
        401 | 403 => 401,
        402 | 429 => 429,
        404 => 404,
        503 => 503,
        _ => 502,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["inputTokens", "input_tokens", "promptTokens"]);
    let completion = number(&["outputTokens", "output_tokens", "completionTokens"]);
    let total = number(&["totalTokens", "total_tokens"]);
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

/// ChatFormat → Command Code 的 8 键信封。
pub fn build_envelope(req: &ChatFormat, session_id: &str) -> Value {
    let mut system_blocks: Vec<Value> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        if text.trim().is_empty() {
            continue;
        }
        match message.role {
            Role::System => system_blocks.push(json!({ "type": "text", "text": text })),
            Role::Assistant => messages.push(json!({
                "role": "assistant",
                "content": [{ "type": "text", "text": text }],
            })),
            Role::Tool => messages.push(json!({
                "role": "tool",
                "content": [{ "type": "text", "text": text }],
            })),
            _ => messages.push(json!({
                "role": "user",
                "content": [{ "type": "text", "text": text }],
            })),
        }
    }
    // 没有 system 时发空格占位（上游硬要求）
    if system_blocks.is_empty() {
        system_blocks.push(json!({ "type": "text", "text": " " }));
    }

    let mut params = Map::new();
    params.insert("model".to_string(), json!(req.model.clone()));
    params.insert("messages".to_string(), Value::Array(messages));
    params.insert(
        "max_tokens".to_string(),
        json!(req.max_tokens.unwrap_or(64_000)),
    );
    params.insert("stream".to_string(), json!(true));
    params.insert("system".to_string(), Value::Array(system_blocks));
    if let Some(temperature) = req.temperature {
        params.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(effort) = req.reasoning_effort.clone() {
        params.insert("reasoning_effort".to_string(), json!(effort));
    }
    params.insert(
        "tools".to_string(),
        req.tools.clone().map(Value::Array).unwrap_or(json!([])),
    );

    let mut envelope = Map::new();
    envelope.insert(
        "config".to_string(),
        json!({
            "workingDir": "/tmp/commandcode",
            "date": chrono::Utc::now().format("%Y-%m-%d").to_string(),
            "environment": CLI_ENVIRONMENT,
            "structure": [],
            "isGitRepo": false,
            "currentBranch": "",
            "mainBranch": "",
            "gitStatus": "",
            "recentCommits": [],
        }),
    );
    envelope.insert("memory".to_string(), Value::Null);
    envelope.insert("taste".to_string(), Value::Null);
    envelope.insert("skills".to_string(), Value::Null);
    envelope.insert("permissionMode".to_string(), json!("standard"));
    if let Ok(uuid) = uuid::Uuid::parse_str(session_id) {
        envelope.insert("threadId".to_string(), json!(uuid.to_string()));
    }
    envelope.insert("mode".to_string(), json!("agent"));
    envelope.insert("params".to_string(), Value::Object(params));
    Value::Object(envelope)
}

/// W3C traceparent（32 hex trace + 16 hex span）。
fn traceparent() -> String {
    let trace = uuid::Uuid::new_v4().simple().to_string();
    let span_full = uuid::Uuid::new_v4().simple().to_string();
    format!("00-{trace}-{}-01", &span_full[..16])
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
            model: "deepseek/deepseek-v4-flash".into(),
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
    fn envelope_has_eight_keys_and_system_placeholder() {
        let envelope = build_envelope(&format(vec![user("你好")]), "session-1");
        assert_eq!(envelope["memory"], Value::Null);
        assert_eq!(envelope["mode"], "agent");
        assert_eq!(envelope["permissionMode"], "standard");
        assert_eq!(envelope["params"]["system"][0]["text"], " ");
        assert_eq!(envelope["params"]["stream"], true);
    }

    #[test]
    fn text_delta_becomes_content() {
        let line = r#"{"type":"text-delta","text":"你"}"#;
        match decode_line(line, "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.content.as_deref(), Some("你")),
            _ => panic!("text-delta 应产出正文增量"),
        }
    }

    #[test]
    fn error_event_maps_to_bridge_error() {
        let line = r#"{"type":"error","statusCode":401,"message":"bad key"}"#;
        match decode_line(line, "x", "m") {
            Decoded::Err(BridgeError::UpstreamStatus {
                status, message, ..
            }) => {
                assert_eq!(status, 401);
                assert!(message.contains("bad key"));
            }
            _ => panic!("error 事件应转上游错误"),
        }
    }
}
