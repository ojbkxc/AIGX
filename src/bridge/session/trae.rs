//! Trae（字节跳动 AI IDE）SOLO 通道登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/trae`：
//! - `POST {base}/api/agent/v3/llm_utils_chat`；
//! - 鉴权是 `Authorization: Cloud-IDE-JWT <token>` + 一批客户端身份头
//!   （`X-Cloudide-Token` / `X-Uid` / `X-Machine-Id` / `X-Device-Id` / `X-App-Id`…），
//!   身份头来自 provider 默认表 + 账号级额外请求头（账号没填的就不发）；
//! - 请求体是**白名单重建**的 SOLO 信封（`function=solo_work_lite`），
//!   多带字段会被上游拒绝；
//! - 响应 SSE 用 `event:` 区分帧（`output` / `token_usage` / `done` / `error`），
//!   **上游不发 `[DONE]`**。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};

const CHAT_PATH: &str = "/api/agent/v3/llm_utils_chat";
const DEFAULT_MODEL: &str = "glm-5.2";
const DEFAULT_MAX_TOKENS: i64 = 1_000_000;
const DEFAULT_USER_AGENT: &str = "Trae/1.0 antigravity-cockpit-tools";

pub struct TraeBridge {
    base: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
}

impl TraeBridge {
    pub fn new(
        base: impl Into<String>,
        _credential: impl Into<String>,
        headers: Vec<(String, String)>,
        client: reqwest::Client,
    ) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        let mut merged = headers;
        if !merged
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        {
            merged.push(("user-agent".to_string(), DEFAULT_USER_AGENT.to_string()));
        }
        Self {
            base,
            client,
            headers: merged,
        }
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}{CHAT_PATH}", self.base))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");
        for (name, value) in &self.headers {
            // 私有桥自己管理 Content-Type / Accept，避免账号级额外头覆盖它们
            if name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("accept") {
                continue;
            }
            req = req.header(name.as_str(), value.as_str());
        }
        req.json(body)
    }
}

#[async_trait]
impl Bridge for TraeBridge {
    fn name(&self) -> &'static str {
        "trae"
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
            id: format!("trae-{}", uuid::Uuid::new_v4()),
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
        let body = build_body(req);
        let resp = self
            .request(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Trae 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("trae-{}", uuid::Uuid::new_v4());
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
                    pending_usage: None,
                    event: String::new(),
                    data: String::new(),
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
                                let text = String::from_utf8_lossy(&line)
                                    .trim_end_matches(['\r', '\n'])
                                    .to_string();
                                if let Some((event, data)) = scan_line(&text, &mut flow) {
                                    apply_event(
                                        &event,
                                        &data,
                                        &flow.id,
                                        &flow.model,
                                        &mut flow.pending_usage,
                                        &mut pending,
                                        &mut flow.ended,
                                    );
                                }
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            // 没见到 done 就 EOF：补一条结束帧（上游不保证发 done）
                            pending.push_back(Ok(finish_chunk(&flow.id, &flow.model, flow.pending_usage.take())));
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
    pending_usage: Option<UsageStats>,
    event: String,
    data: String,
}

/// 喂一行，攒出完整事件时返回 `(event, data)`。
fn scan_line(line: &str, flow: &mut Flow) -> Option<(String, String)> {
    if line.is_empty() {
        if flow.event.is_empty() && flow.data.trim().is_empty() {
            return None;
        }
        let event = std::mem::take(&mut flow.event);
        let data = std::mem::take(&mut flow.data);
        return Some((event, data));
    }
    if let Some(rest) = line.strip_prefix("event:") {
        flow.event = rest.trim().to_string();
    } else if let Some(rest) = line.strip_prefix("data:") {
        // 上游多行 data 直接拼接（参考实现同口径）
        flow.data.push_str(rest);
    }
    None
}

fn apply_event(
    event: &str,
    data: &str,
    id: &str,
    model: &str,
    pending_usage: &mut Option<UsageStats>,
    pending: &mut VecDeque<Result<ChatChunk, BridgeError>>,
    ended: &mut bool,
) {
    if data.trim().is_empty() {
        if event == "done" {
            pending.push_back(Ok(finish_chunk(id, model, pending_usage.take())));
            *ended = true;
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    match event {
        "output" => {
            let text = value
                .get("response")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reasoning = value
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let calls = value.get("tool_calls").and_then(Value::as_array);
            let mut delta = ChatDelta::default();
            if !text.is_empty() {
                delta.content = Some(text.to_string());
            }
            if !reasoning.is_empty() {
                delta.reasoning = Some(reasoning.to_string());
            }
            if let Some(calls) = calls {
                let parsed: Vec<ToolCallDelta> = calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, call)| {
                        let function = call.get("function")?;
                        Some(ToolCallDelta {
                            index,
                            id: call
                                .get("id")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            function_name: function
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            arguments: function.get("arguments").map(|args| {
                                args.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| args.to_string())
                            }),
                        })
                    })
                    .collect();
                if !parsed.is_empty() {
                    delta.tool_calls = Some(parsed);
                }
            }
            if delta.content.is_some() || delta.reasoning.is_some() || delta.tool_calls.is_some() {
                pending.push_back(Ok(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta,
                    finish_reason: None,
                    usage: None,
                }));
            }
        }
        "token_usage" => {
            *pending_usage = Some(usage_of(&value));
        }
        "done" => {
            let finish = map_finish(
                value
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            pending.push_back(Ok(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta::default(),
                finish_reason: Some(finish),
                usage: pending_usage.take(),
            }));
            *ended = true;
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游错误");
            pending.push_back(Err(BridgeError::upstream_status(
                502,
                format!("Trae 流内错误：[{code}] {message}"),
            )));
            *ended = true;
        }
        _ => {}
    }
}

fn finish_chunk(id: &str, model: &str, usage: Option<UsageStats>) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage,
    }
}

fn map_finish(raw: &str) -> FinishReason {
    match raw.trim().to_ascii_lowercase().as_str() {
        "length" | "max_tokens" => FinishReason::Length,
        "tool_calls" | "tool-calls" => FinishReason::ToolCalls,
        "content_filter" | "safety" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["prompt_tokens", "input_tokens"]);
    let completion = number(&["completion_tokens", "output_tokens"]);
    let total = number(&["total_tokens"]);
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

/// ChatFormat → SOLO 信封（白名单重建：只搬上游认的键）。
pub fn build_body(req: &ChatFormat) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        let role = match message.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            _ => "user",
        };
        let mut entry = Map::new();
        entry.insert("role".to_string(), json!(role));
        entry.insert("content".to_string(), json!(text));
        if message.role == Role::Tool {
            if let Some(id) = &message.tool_call_id {
                entry.insert("tool_call_id".to_string(), json!(id));
            }
        }
        if let Some(calls) = &message.tool_calls {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function_call": {
                            "name": call.function_name,
                            "arguments": call.arguments,
                        },
                    })
                })
                .collect();
            entry.insert("tool_calls".to_string(), Value::Array(calls));
        }
        messages.push(Value::Object(entry));
    }

    let model = req.model.trim();
    let model = if model.is_empty() {
        DEFAULT_MODEL
    } else {
        model
    };

    let mut out = Map::new();
    out.insert("messages".to_string(), Value::Array(messages));
    out.insert("function".to_string(), json!("solo_work_lite"));
    out.insert("stream".to_string(), json!(true));
    out.insert("config_name".to_string(), json!(model));
    out.insert("model".to_string(), json!(model));
    if let Some(tools) = &req.tools {
        // SOLO 要求 `function.parameters` 是 JSON 字符串
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if let Some(parameters) = tool
                    .get_mut("function")
                    .and_then(|function| function.get_mut("parameters"))
                {
                    if parameters.is_object() {
                        let text = parameters.to_string();
                        *parameters = Value::String(text);
                    }
                }
                tool
            })
            .collect();
        out.insert("tools".to_string(), Value::Array(tools));
    }
    if let Some(temperature) = req.temperature {
        out.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        out.insert("top_p".to_string(), json!(top_p));
    }
    out.insert(
        "max_tokens".to_string(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS as u32)),
    );
    if let Some(effort) = &req.reasoning_effort {
        let level = effort.trim().to_lowercase();
        if !matches!(level.as_str(), "" | "auto" | "none" | "off") {
            out.insert("reasoning_effort".to_string(), json!(effort));
        }
    }
    Value::Object(out)
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
            model: "glm-5.2".into(),
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
    fn body_uses_solo_whitelist() {
        let body = build_body(&format(vec![user("你好")]));
        assert_eq!(body["function"], "solo_work_lite");
        assert_eq!(body["stream"], true);
        assert_eq!(body["config_name"], "glm-5.2");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("top_k").is_none());
    }

    #[test]
    fn output_event_becomes_delta() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "output",
            r#"{"response":"你","reasoning_content":"想"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.delta.content.as_deref(), Some("你"));
                assert_eq!(chunk.delta.reasoning.as_deref(), Some("想"));
            }
            _ => panic!("output 应产出增量"),
        }
    }

    #[test]
    fn done_event_finishes_with_pending_usage() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "token_usage",
            r#"{"prompt_tokens":3,"completion_tokens":2}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        apply_event(
            "done",
            r#"{"finish_reason":"stop"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
                assert_eq!(chunk.usage.map(|u| u.prompt_tokens), Some(3));
            }
            _ => panic!("done 应产出结束帧"),
        }
        assert!(ended);
    }
}
