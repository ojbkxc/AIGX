//! Accio（阿里 Phoenix 网关）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/accio`：
//! - 推理：`POST {gateway}/api/adk/llm/generateContent?sg_k=<md5(requestId)>`
//! - 鉴权**不走 Authorization**：账号 token 放在请求体的 `token` 字段，
//!   请求头只带客户端身份（`appKey` / `x-app-version` / `x-package-region`）。
//! - 请求体是 Gemini 风格信封（`contents` / `system_instruction`），
//!   响应是 SSE：`content.parts`（`thought: true` = 思考）、
//!   `turn_complete` + `finish_reason` + `usage_metadata`。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use md5::{Digest, Md5};
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

const DEFAULT_APP_KEY: &str = "35298846";
const APP_VERSION: &str = "0.32.6";
const TENANT: &str = "accio-agent";
const IAI_TAG: &str = "phoenix-desktop";

pub struct AccioBridge {
    /// 业务网关基址（默认 `https://phoenix-gw.alibaba.com`）。
    base: String,
    token: String,
    client: reqwest::Client,
}

impl AccioBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        }
    }

    fn generate_url(&self, request_id: &str) -> String {
        let mut hasher = Md5::new();
        hasher.update(request_id.as_bytes());
        format!(
            "{}/api/adk/llm/generateContent?sg_k={:x}",
            self.base,
            hasher.finalize()
        )
    }

    async fn send(&self, body: &Value, request_id: &str) -> Result<reqwest::Response, BridgeError> {
        let resp = self
            .client
            .post(self.generate_url(request_id))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream")
            .header("x-language", "en")
            .header("x-app-version", APP_VERSION)
            .header("x-package-region", "CN")
            .header("appKey", DEFAULT_APP_KEY)
            .header("version", APP_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Accio 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        Ok(resp)
    }
}

#[async_trait]
impl Bridge for AccioBridge {
    fn name(&self) -> &'static str {
        "accio"
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
            id: format!("accio-{}", uuid::Uuid::new_v4()),
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
        let request_id = uuid::Uuid::new_v4().to_string();
        let body = build_body(req, &self.token, &request_id);
        let resp = self.send(&body, &request_id).await?;

        let id = format!("accio-{}", uuid::Uuid::new_v4());
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
                                    Decoded::Err(err) => {
                                        pending.push_back(Err(err));
                                        flow.ended = true;
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

struct Flow {
    id: String,
    model: String,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done(FinishReason, Option<UsageStats>),
    Err(BridgeError),
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done(FinishReason::Stop, None),
        SseEvent::Data(payload) => {
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                return Decoded::Nothing;
            };
            let error_code = text_of(&value, "error_code", "errorCode");
            let error_message = text_of(&value, "error_message", "errorMessage");
            if !error_code.is_empty() || !error_message.is_empty() {
                return Decoded::Err(BridgeError::upstream_status(
                    502,
                    format!("Accio 流内错误：[{error_code}] {error_message}"),
                ));
            }
            let mut delta = ChatDelta::default();
            if let Some(parts) = value
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
            {
                let mut text = String::new();
                let mut thought = String::new();
                let mut calls: Vec<ToolCallDelta> = Vec::new();
                for part in parts {
                    if let Some(fc) = part
                        .get("functionCall")
                        .or_else(|| part.get("function_call"))
                    {
                        let args = fc
                            .get("argsJson")
                            .or_else(|| fc.get("args_json"))
                            .or_else(|| fc.get("args"))
                            .map(|raw| {
                                raw.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| raw.to_string())
                            })
                            .unwrap_or_default();
                        calls.push(ToolCallDelta {
                            index: calls.len(),
                            id: fc
                                .get("id")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            function_name: fc
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            arguments: Some(args),
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
            let has_delta = delta.content.is_some()
                || delta.reasoning.is_some()
                || delta.tool_calls.is_some();
            let turn_complete = value
                .get("turn_complete")
                .or_else(|| value.get("turnComplete"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let finish = if turn_complete {
                Some(map_finish_reason(&text_of(
                    &value,
                    "finish_reason",
                    "finishReason",
                )))
            } else {
                None
            };
            let usage = value
                .get("usage_metadata")
                .or_else(|| value.get("usageMetadata"))
                .map(usage_of);
            if let Some(reason) = finish {
                return Decoded::Done(reason, usage);
            }
            if !has_delta {
                return match usage {
                    Some(stats) => Decoded::Chunk(ChatChunk {
                        id: id.to_string(),
                        model: model.to_string(),
                        delta: ChatDelta::default(),
                        finish_reason: None,
                        usage: Some(stats),
                    }),
                    None => Decoded::Nothing,
                };
            }
            Decoded::Chunk(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta,
                finish_reason: None,
                usage: None,
            })
        }
    }
}

fn text_of(value: &Value, camel: &str, snake: &str) -> String {
    value
        .get(camel)
        .or_else(|| value.get(snake))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn map_finish_reason(raw: &str) -> FinishReason {
    match raw.trim().to_ascii_uppercase().as_str() {
        "MAX_TOKENS" | "LENGTH" => FinishReason::Length,
        "SAFETY" | "CONTENT_FILTER" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |camel: &str, snake: &str| -> u64 {
        usage
            .get(camel)
            .or_else(|| usage.get(snake))
            .and_then(Value::as_i64)
            .unwrap_or(0) as u64
    };
    let prompt = number("promptTokenCount", "prompt_token_count");
    let completion = number("candidatesTokenCount", "candidates_token_count");
    let total = number("totalTokenCount", "total_token_count");
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + completion
        },
        cached_prompt_tokens: number("cachedContentTokenCount", "cached_content_token_count"),
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// ChatFormat → Accio 的 Gemini 风格信封。
pub fn build_body(req: &ChatFormat, token: &str, request_id: &str) -> Value {
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
                    system.push('\n');
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
    if contents.is_empty() {
        contents.push(json!({ "role": "user", "parts": [{ "text": "..." }] }));
    }

    let mut payload = Map::new();
    payload.insert("model".to_string(), json!(req.model.clone()));
    payload.insert("tenant".to_string(), json!(TENANT));
    payload.insert("iai_tag".to_string(), json!(IAI_TAG));
    payload.insert("empid".to_string(), json!(""));
    payload.insert("request_id".to_string(), json!(request_id));
    payload.insert("msg_id".to_string(), json!(format!("msg-{request_id}")));
    payload.insert("token".to_string(), json!(token));
    payload.insert("contents".to_string(), Value::Array(contents));
    payload.insert("system_instruction".to_string(), json!(system));
    if let Some(max_tokens) = req.max_tokens.filter(|value| *value > 0) {
        payload.insert("max_output_tokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        payload.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        payload.insert("top_p".to_string(), json!(top_p));
    }
    payload.insert(
        "properties".to_string(),
        json!({ "normalized_response": "true" }),
    );
    Value::Object(payload)
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
            model: "qwen3-max".into(),
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
    fn body_carries_token_and_gemini_contents() {
        let body = build_body(&format(vec![user("你好")]), "TOKEN", "rid");
        assert_eq!(body["token"], "TOKEN");
        assert_eq!(body["request_id"], "rid");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "你好");
        assert_eq!(body["tenant"], TENANT);
    }

    #[test]
    fn thought_part_becomes_reasoning_delta() {
        let payload =
            r#"{"content":{"role":"model","parts":[{"thought":true,"text":"想…"}]}}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.reasoning.as_deref(), Some("想…")),
            _ => panic!("思考 part 应转 reasoning"),
        }
    }

    #[test]
    fn error_frame_surfaces_as_bridge_error() {
        let payload = r#"{"error_code":"QUOTA","error_message":"额度不足"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Err(BridgeError::UpstreamStatus { message, .. }) => {
                assert!(message.contains("额度不足"));
            }
            _ => panic!("错误帧应转上游错误"),
        }
    }
}
