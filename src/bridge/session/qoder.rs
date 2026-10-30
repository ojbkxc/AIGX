//! Qoder 登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/qoder`：
//! - 推理：`POST {gateway}/algo/api/v2/service/pro/sse/agent_chat_generation`
//!   （`FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`）；
//! - 鉴权是 COSY 自签名（见 [`super::qoder_cosy`]），**请求体先编码再签名**；
//! - 响应 SSE 多包一层信封 `{statusCodeValue, body}`，内层才是 OpenAI chunk。
//!
//! 账号身份（uid / email / name / machine_id）从访问令牌的 JWT 声明里读，
//! 读不到时用令牌哈希派生稳定值（上游只把它当展示与设备标识）。

use std::collections::VecDeque;

use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

use super::qoder_cosy::{self, CosyIdentity};

const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";
const CHAT_QUERY: &str = "FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
const SESSION_TYPE: &str = "qodercli";
const AGENT_ID: &str = "agent_common";
const CHAT_TASK: &str = "FREE_INPUT";
const MAX_OUTPUT_TOKENS: u32 = 32_000;

pub struct QoderBridge {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl QoderBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        }
    }

    fn url(&self) -> String {
        format!("{}{CHAT_PATH}?{CHAT_QUERY}", self.base)
    }
}

#[async_trait]
impl Bridge for QoderBridge {
    fn name(&self) -> &'static str {
        "qoder"
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
            id: format!("qoder-{}", uuid::Uuid::new_v4()),
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
        let identity = identity_of(&self.token);
        let body = build_body(req, &identity.user_id);
        let encoded = qoder_cosy::encode_body(body.to_string().as_bytes());
        let url = self.url();
        let mut headers = qoder_cosy::build_auth_headers(
            Some(&encoded),
            &url,
            &CosyIdentity {
                user_id: &identity.user_id,
                auth_token: &self.token,
                name: &identity.name,
                email: &identity.email,
                machine_id: &identity.machine_id,
            },
        )
        .map_err(BridgeError::Config)?;
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        headers.push(("Accept".to_string(), "text/event-stream".to_string()));
        headers.push(("Cache-Control".to_string(), "no-cache".to_string()));
        headers.push(("Accept-Encoding".to_string(), "identity".to_string()));
        headers.push(("X-Model-Key".to_string(), req.model.clone()));
        headers.push(("X-Model-Source".to_string(), "model".to_string()));

        let mut builder = self.client.post(&url).body(encoded);
        for (name, value) in &headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Qoder 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("qoder-{}", uuid::Uuid::new_v4());
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
                                    Decoded::Done => {
                                        pending.push_back(Ok(ChatChunk {
                                            id: flow.id.clone(),
                                            model: flow.model.clone(),
                                            delta: ChatDelta::default(),
                                            finish_reason: Some(FinishReason::Stop),
                                            usage: None,
                                        }));
                                        flow.ended = true;
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
    Done,
    Err(BridgeError),
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done,
        SseEvent::Data(payload) => {
            let trimmed = payload.trim();
            if trimmed.is_empty() {
                return Decoded::Nothing;
            }
            if trimmed == "[DONE]" {
                return Decoded::Done;
            }
            let Ok(envelope) = serde_json::from_str::<Value>(trimmed) else {
                return Decoded::Nothing;
            };
            if let Some(status) = envelope.get("statusCodeValue").and_then(Value::as_i64) {
                if status != 200 {
                    let raw = envelope
                        .get("body")
                        .map(|body| {
                            body.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| body.to_string())
                        })
                        .unwrap_or_default();
                    return Decoded::Err(BridgeError::upstream_status(
                        status as u16,
                        format!("Qoder 上游错误（{status}）：{}", head(&raw)),
                    ));
                }
            }
            let Some(inner) = envelope.get("body") else {
                return Decoded::Nothing;
            };
            if inner.as_str() == Some("[DONE]") {
                return Decoded::Done;
            }
            let chunk = match inner {
                Value::String(text) => match serde_json::from_str::<Value>(text) {
                    Ok(parsed) => parsed,
                    Err(_) => return Decoded::Nothing,
                },
                other => other.clone(),
            };
            chunk_to_delta(&chunk, id, model)
        }
    }
}

fn chunk_to_delta(chunk: &Value, id: &str, model: &str) -> Decoded {
    let choice = chunk.get("choices").and_then(|c| c.get(0));
    let mut delta = ChatDelta::default();
    if let Some(inner) = choice.and_then(|c| c.get("delta")) {
        if let Some(text) = inner.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                delta.content = Some(text.to_string());
            }
        }
        if let Some(text) = inner.get("reasoning_content").and_then(Value::as_str) {
            if !text.is_empty() {
                delta.reasoning = Some(text.to_string());
            }
        }
        if let Some(calls) = inner.get("tool_calls").and_then(Value::as_array) {
            let parsed: Vec<ToolCallDelta> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| ToolCallDelta {
                    index: call
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                        .unwrap_or(index),
                    id: call.get("id").and_then(Value::as_str).map(str::to_string),
                    function_name: call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    arguments: call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            if !parsed.is_empty() {
                delta.tool_calls = Some(parsed);
            }
        }
    }
    let usage = chunk.get("usage").map(usage_of);
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(map_finish);
    if let Some(reason) = finish {
        return Decoded::Chunk(ChatChunk {
            id: id.to_string(),
            model: model.to_string(),
            delta,
            finish_reason: Some(reason),
            usage,
        });
    }
    if delta.content.is_none() && delta.reasoning.is_none() && delta.tool_calls.is_none() {
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
        usage,
    })
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
    let number = |key: &str| -> u64 {
        usage
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0) as u64
    };
    let prompt = number("prompt_tokens");
    let completion = number("completion_tokens");
    let total = number("total_tokens");
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

/// 账号身份（uid / name / email / machine_id）。
struct Identity {
    user_id: String,
    name: String,
    email: String,
    machine_id: String,
}

/// 从访问令牌解出身份；解不出时用令牌哈希派生稳定值。
fn identity_of(token: &str) -> Identity {
    let claims = decode_jwt_claims(token);
    let text = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| claims.get(*key).and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    let mut user_id = text(&["uid", "user_id", "userId", "sub"]);
    if user_id.is_empty() {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"qoder-uid");
        hasher.update(token.as_bytes());
        user_id = format!("{:x}", hasher.finalize())[..24].to_string();
    }
    let machine_id = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"qoder-machine");
        hasher.update(token.as_bytes());
        format!("{:x}", hasher.finalize())[..32].to_string()
    };
    Identity {
        user_id,
        name: text(&["name", "nickname"]),
        email: text(&["email"]),
        machine_id,
    }
}

fn decode_jwt_claims(token: &str) -> Value {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Value::Null;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload));
    let Ok(bytes) = decoded else {
        return Value::Null;
    };
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// ChatFormat → Qoder 固定信封。
pub fn build_body(req: &ChatFormat, user_id: &str) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        if text.trim().is_empty() {
            continue;
        }
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
                        "function": {
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

    let max_tokens = req
        .max_tokens
        .filter(|value| *value > 0)
        .unwrap_or(MAX_OUTPUT_TOKENS)
        .min(MAX_OUTPUT_TOKENS);

    let mut parameters = Map::new();
    parameters.insert("max_tokens".to_string(), json!(max_tokens));
    if let Some(effort) = &req.reasoning_effort {
        let level = effort.trim().to_lowercase();
        if !matches!(level.as_str(), "" | "auto" | "none" | "off") {
            parameters.insert("enable_thinking".to_string(), json!(true));
            parameters.insert("reasoning_effort".to_string(), json!(level));
        }
    }

    let last_user_text = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    let session_id = session_id_for(user_id, &req.model);
    let request_id = uuid::Uuid::new_v4().to_string();

    json!({
        "request_id": request_id,
        "request_set_id": request_id,
        "chat_record_id": request_id,
        "session_id": session_id,
        "stream": true,
        "chat_task": CHAT_TASK,
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": SESSION_TYPE,
        "agent_id": AGENT_ID,
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": Value::Null,
        "aliyun_user_type": "",
        "system": "",
        "messages": messages,
        "tools": req.tools.clone().map(Value::Array).unwrap_or(json!([])),
        "parameters": Value::Object(parameters),
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": Value::Null,
            "extra": {
                "context": [],
                "modelConfig": { "key": req.model },
                "originalContent": last_user_text,
            },
            "features": [],
            "text": last_user_text,
        },
        "model_config": { "key": req.model },
        "business": {
            "product": "cli",
            "version": "1.0.0",
            "type": "agent",
            "stage": "start",
            "id": uuid::Uuid::new_v4().to_string(),
            "name": last_user_text.chars().take(30).collect::<String>(),
            "begin_at": chrono::Utc::now().timestamp_millis(),
        },
    })
}

fn session_id_for(user_id: &str, model: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"qoder-session");
    hasher.update([0u8]);
    hasher.update(user_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(model.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!(
        "{}-{}-{}-{}-{}",
        &hash[..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..32]
    )
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
            model: "qwen3.8-max".into(),
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
    fn body_carries_fixed_envelope_fields() {
        let body = build_body(&format(vec![user("你好")]), "uid-1");
        assert_eq!(body["session_type"], "qodercli");
        assert_eq!(body["agent_id"], "agent_common");
        assert_eq!(body["chat_task"], "FREE_INPUT");
        assert_eq!(body["stream"], true);
        assert_eq!(body["chat_context"]["text"], "你好");
        assert_eq!(body["parameters"]["max_tokens"], MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn session_id_is_stable_per_user_and_model() {
        let a = session_id_for("uid-1", "qwen3.8-max");
        let b = session_id_for("uid-1", "qwen3.8-max");
        let c = session_id_for("uid-2", "qwen3.8-max");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn envelope_body_is_unwrapped_and_parsed() {
        let payload = r#"{"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"你\"}}]}"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.content.as_deref(), Some("你")),
            _ => panic!("信封内层应是 OpenAI chunk"),
        }
    }

    #[test]
    fn business_error_in_envelope_becomes_bridge_error() {
        let payload = r#"{"statusCodeValue":429,"body":"{\"message\":\"quota\"}"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Err(BridgeError::UpstreamStatus { status, .. }) => assert_eq!(status, 429),
            _ => panic!("信封业务错误应转上游错误"),
        }
    }

    #[test]
    fn identity_falls_back_to_token_hash_without_jwt() {
        let identity = identity_of("not-a-jwt-token");
        assert!(!identity.user_id.is_empty());
        assert_eq!(identity.machine_id.len(), 32);
    }
}
