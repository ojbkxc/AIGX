//! Antigravity（Google AI IDE）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/antigravity`：推理走 Google Cloud Code Assist
//! 的 `v1internal`，鉴权是 Google OAuth 的 `Authorization: Bearer <access_token>`。
//!
//! 请求信封：
//! ```jsonc
//! { "model": "…", "userAgent": "antigravity", "requestId": "agent/<ts>/<hex>",
//!   "request": { "contents": [...], "systemInstruction": {...},
//!                "generationConfig": { ... } } }
//! ```
//! 响应是 SSE，每帧真正的 Gemini 响应在 `response` 键下（缺省回退顶层），
//! `candidates[0].content.parts` 里 `thought == true` 的文本是思考内容。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

/// 推理路径（`{base}/v1internal:streamGenerateContent?alt=sse`）。
const CHAT_PATH: &str = "/v1internal:streamGenerateContent?alt=sse";
const USER_AGENT: &str = "antigravity/1.0";

pub struct AntigravityBridge {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl AntigravityBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        let token = token.into();
        let token = super::normalize_credential(&token, &["authorization:", "bearer"]);
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.to_string(),
            client,
        }
    }

    fn chat_url(&self) -> String {
        format!("{}{CHAT_PATH}", self.base)
    }

    async fn send(&self, body: &Value) -> Result<reqwest::Response, BridgeError> {
        let resp = self
            .client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("User-Agent", USER_AGENT)
            .json(body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Antigravity 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        Ok(resp)
    }
}

#[async_trait]
impl Bridge for AntigravityBridge {
    fn name(&self) -> &'static str {
        "antigravity"
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
        let mut finish = FinishReason::Stop;
        let mut usage = UsageStats::default();
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
                    if let (Some(id), Some(name)) = (delta.id.clone(), delta.function_name.clone())
                    {
                        calls.push(ToolCall {
                            id,
                            function_name: name,
                            arguments: delta.arguments.unwrap_or_default(),
                        });
                    }
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
            id: format!("antigravity-{}", uuid::Uuid::new_v4()),
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
        let body = build_envelope(req);
        let resp = self.send(&body).await?;

        let id = format!("antigravity-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                SseDecoder::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    ended: false,
                },
            ),
            |(mut bytes, mut decoder, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (bytes, decoder, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            let events = decoder.feed(chunk.as_ref());
                            for event in events {
                                match decode_event(&event, &flow.id, &flow.model) {
                                    Decoded::Nothing => {}
                                    Decoded::Chunk(chunk) => pending.push_back(Ok(chunk)),
                                    Decoded::Usage(usage) => {
                                        pending.push_back(Ok(ChatChunk {
                                            id: flow.id.clone(),
                                            model: flow.model.clone(),
                                            delta: ChatDelta::default(),
                                            finish_reason: None,
                                            usage: Some(usage),
                                        }));
                                    }
                                    Decoded::Done(reason, usage) => {
                                        pending.push_back(Ok(ChatChunk {
                                            id: flow.id.clone(),
                                            model: flow.model.clone(),
                                            delta: ChatDelta::default(),
                                            finish_reason: Some(reason),
                                            usage,
                                        }));
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
                            if let Some(event) = decoder.finish() {
                                if let Decoded::Chunk(chunk) =
                                    decode_event(&event, &flow.id, &flow.model)
                                {
                                    pending.push_back(Ok(chunk));
                                }
                            }
                            pending.push_back(Ok(ChatChunk {
                                id: flow.id.clone(),
                                model: flow.model.clone(),
                                delta: ChatDelta::default(),
                                finish_reason: Some(FinishReason::Stop),
                                usage: None,
                            }));
                            flow.ended = true;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

/// 流式跨事件状态。
struct Flow {
    id: String,
    model: String,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Usage(UsageStats),
    Done(FinishReason, Option<UsageStats>),
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done(FinishReason::Stop, None),
        SseEvent::Data(payload) => {
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                return Decoded::Nothing;
            };
            let response = value.get("response").unwrap_or(&value);
            let usage = response.get("usageMetadata").map(usage_from);
            let candidate = response
                .get("candidates")
                .and_then(|c| c.get(0))
                .cloned()
                .unwrap_or(Value::Null);
            let finish = candidate
                .get("finishReason")
                .and_then(Value::as_str)
                .map(map_finish_reason);
            let mut delta = ChatDelta::default();
            if let Some(parts) = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                let mut calls: Vec<ToolCallDelta> = Vec::new();
                let (mut text, mut thought) = (String::new(), String::new());
                for part in parts {
                    if let Some(fc) = part.get("functionCall") {
                        calls.push(ToolCallDelta {
                            index: calls.len(),
                            id: fc
                                .get("id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                                .or_else(|| Some(uuid::Uuid::new_v4().to_string())),
                            function_name: fc
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            arguments: fc.get("args").map(|a| a.to_string()),
                        });
                        continue;
                    }
                    let Some(part_text) = part.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    if part
                        .get("thought")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        thought.push_str(part_text);
                    } else {
                        text.push_str(part_text);
                    }
                }
                if !text.is_empty() {
                    delta.content = Some(text);
                }
                if !thought.is_empty() {
                    delta.reasoning = Some(thought);
                }
                if !calls.is_empty() {
                    delta.tool_calls = Some(calls);
                }
            }
            let has_delta =
                delta.content.is_some() || delta.reasoning.is_some() || delta.tool_calls.is_some();
            if let Some(reason) = finish {
                return Decoded::Done(reason, usage);
            }
            if has_delta {
                return Decoded::Chunk(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta,
                    finish_reason: None,
                    usage: None,
                });
            }
            match usage {
                Some(stats) => Decoded::Usage(stats),
                None => Decoded::Nothing,
            }
        }
    }
}

fn map_finish_reason(raw: &str) -> FinishReason {
    match raw.trim().to_ascii_uppercase().as_str() {
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_from(usage: &Value) -> UsageStats {
    let number = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0);
    let prompt = number("promptTokenCount") as u64;
    let thoughts = number("thoughtsTokenCount") as u64;
    let mut candidates = number("candidatesTokenCount") as u64;
    let total = number("totalTokenCount") as u64;
    if candidates == 0 && total > prompt + thoughts {
        candidates = total - prompt - thoughts;
    }
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: candidates + thoughts,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + candidates + thoughts
        },
        cached_prompt_tokens: number("cachedContentTokenCount") as u64,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// chat 请求 → v1internal 信封。
pub fn build_envelope(req: &ChatFormat) -> Value {
    let mut system = String::new();
    let mut contents: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        if text.trim().is_empty() {
            continue;
        }
        match message.role {
            Role::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(text);
            }
            Role::Assistant => contents.push(json!({
                "role": "model",
                "parts": [{ "text": text }],
            })),
            _ => contents.push(json!({
                "role": "user",
                "parts": [{ "text": text }],
            })),
        }
    }
    // `contents` 必须以 user 结尾（上游硬要求），末尾是 model 时补一条续写提示
    if contents
        .last()
        .and_then(|c| c.get("role"))
        .and_then(Value::as_str)
        == Some("model")
    {
        contents.push(json!({
            "role": "user",
            "parts": [{ "text": "Please continue your analysis." }],
        }));
    }
    if contents.is_empty() {
        contents.push(json!({ "role": "user", "parts": [{ "text": "..." }] }));
    }

    let mut generation = Map::new();
    if let Some(value) = req.temperature {
        generation.insert("temperature".to_string(), json!(value));
    }
    if let Some(value) = req.top_p {
        generation.insert("topP".to_string(), json!(value));
    }
    if let Some(value) = req.max_tokens {
        generation.insert("maxOutputTokens".to_string(), json!(value));
    }

    let mut request = Map::new();
    request.insert("contents".to_string(), Value::Array(contents));
    if !system.is_empty() {
        request.insert(
            "systemInstruction".to_string(),
            json!({ "role": "user", "parts": [{ "text": system }] }),
        );
    }
    if !generation.is_empty() {
        request.insert("generationConfig".to_string(), Value::Object(generation));
    }

    let mut envelope = Map::new();
    envelope.insert("model".to_string(), json!(req.model.clone()));
    envelope.insert("userAgent".to_string(), json!("antigravity"));
    let short = uuid::Uuid::new_v4().simple().to_string();
    envelope.insert(
        "requestId".to_string(),
        json!(format!(
            "agent/{}/{}",
            chrono::Utc::now().timestamp_millis(),
            &short[..8]
        )),
    );
    envelope.insert("request".to_string(), Value::Object(request));
    // `project`（cloudaicompanionProject）由调用方通过 body 额外字段透传；
    // 没给时不发该键（上游可能放宽按需校验，Agent2API 同口径）。
    if let Some(project) = req
        .extra
        .as_ref()
        .and_then(|extra| extra.get("project"))
        .filter(|value| !value.is_null())
    {
        envelope.insert("project".to_string(), project.clone());
    }
    Value::Object(envelope)
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
            model: "gemini-pro-agent".into(),
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
    fn envelope_keeps_system_instruction_out_of_contents() {
        let mut sys = user("你是助手");
        sys.role = Role::System;
        let envelope = build_envelope(&format(vec![sys, user("你好")]));
        let request = &envelope["request"];
        assert_eq!(request["systemInstruction"]["parts"][0]["text"], "你是助手");
        assert_eq!(request["contents"].as_array().unwrap().len(), 1);
        assert_eq!(request["contents"][0]["role"], "user");
    }

    #[test]
    fn envelope_appends_user_turn_after_model_turn() {
        let mut assistant = user("上一条回答");
        assistant.role = Role::Assistant;
        let envelope = build_envelope(&format(vec![user("问"), assistant]));
        let contents = envelope["request"]["contents"].as_array().unwrap();
        assert_eq!(contents.last().unwrap()["role"], "user");
    }

    #[test]
    fn finish_reasons_are_normalized() {
        assert_eq!(map_finish_reason("MAX_TOKENS"), FinishReason::Length);
        assert_eq!(map_finish_reason("SAFETY"), FinishReason::ContentFilter);
        assert_eq!(map_finish_reason("STOP"), FinishReason::Stop);
    }
}
