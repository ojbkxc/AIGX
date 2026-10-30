//! 华为 CodeArts（`snap-access`）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/codearts`：
//! - `POST {base}/api/v2/chat/completions`，请求体是 OpenAI 形状；
//! - 鉴权是华为云 **SDK-HMAC-SHA256** 签名（AK/SK + 可选 STS 临时令牌），
//!   另外上游把模型名当请求头（`model-id` / `model-name` / `x-model-id` 三件套）；
//! - 响应是 OpenAI SSE（流内错误用 `code` / `message` 信封表达）。
//!
//! 凭证格式：`AK:SK[:SecurityToken]`（账号页粘贴三段，缺 STS 时只签前两段）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

const CHAT_PATH: &str = "/api/v2/chat/completions";
const PLUGIN_NAME: &str = "snap_vscode";
const PLUGIN_VERSION: &str = "26.9.101";
const LANGUAGE: &str = "en-us";
const MAX_OUTPUT_TOKENS: i64 = 65_536;
const SIGNING_ALGORITHM: &str = "SDK-HMAC-SHA256";

pub struct CodeArtsBridge {
    base: String,
    access_key: String,
    secret_key: String,
    security_token: String,
    client: reqwest::Client,
}

impl CodeArtsBridge {
    pub fn new(base: impl Into<String>, credential: &str, client: reqwest::Client) -> Self {
        let parts: Vec<&str> = credential
            .split([':', '\n'])
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        let access_key = parts.first().copied().unwrap_or("").to_string();
        let secret_key = parts.get(1).copied().unwrap_or("").to_string();
        let security_token = parts.get(2).copied().unwrap_or("").to_string();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            access_key,
            secret_key,
            security_token,
            client,
        }
    }

    fn headers(&self, model: &str, date: &str, body: &[u8], url: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "text/event-stream".to_string()),
            (
                "client_version".to_string(),
                format!("Vscode_{PLUGIN_VERSION}"),
            ),
            ("Agent-Type".to_string(), "ChatAgent".to_string()),
            ("X-Language".to_string(), LANGUAGE.to_string()),
            ("is_confidential".to_string(), "false".to_string()),
            ("plugin-name".to_string(), PLUGIN_NAME.to_string()),
            ("plugin-version".to_string(), PLUGIN_VERSION.to_string()),
            ("model-id".to_string(), model.to_string()),
            ("model-name".to_string(), model.to_string()),
            ("x-model-id".to_string(), model.to_string()),
            ("X-Sdk-Date".to_string(), date.to_string()),
        ];
        if !self.security_token.is_empty() {
            headers.push(("X-Security-Token".to_string(), self.security_token.clone()));
        }
        if let Ok(parsed) = url::Url::parse(url) {
            if let Some(host) = parsed.host_str() {
                let authority = match parsed.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_string(),
                };
                headers.push(("Host".to_string(), authority));
            }
        }
        if let Some(authorization) = self.sign("POST", url, &headers, body) {
            headers.push(("Authorization".to_string(), authorization));
        }
        headers
    }

    /// SDK-HMAC-SHA256：返回 `Authorization` 头值。
    fn sign(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Option<String> {
        if self.access_key.is_empty() || self.secret_key.is_empty() {
            return None;
        }
        let parsed = url::Url::parse(url).ok()?;

        let mut canonical: Vec<(String, String)> = headers
            .iter()
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        // 同名按「后到覆盖」合并
        canonical.sort_by(|a, b| a.0.cmp(&b.0));
        canonical.dedup_by(|later, earlier| {
            if later.0 == earlier.0 {
                earlier.1 = later.1.clone();
                true
            } else {
                false
            }
        });
        let canonical_headers: String = canonical
            .iter()
            .map(|(name, value)| format!("{name}:{value}\n"))
            .collect();
        let signed_headers = canonical
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(";");

        let payload_hash = sha256_hex(body);
        let canonical_request = [
            method,
            &canonical_uri(parsed.path()),
            &canonical_query(parsed.query().unwrap_or("")),
            &canonical_headers,
            &signed_headers,
            &payload_hash,
        ]
        .join("\n");
        let date = canonical
            .iter()
            .find(|(name, _)| name == "x-sdk-date")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let string_to_sign = [
            SIGNING_ALGORITHM,
            &date,
            &sha256_hex(canonical_request.as_bytes()),
        ]
        .join("\n");
        let signature = hmac_sha256_hex(self.secret_key.as_bytes(), &string_to_sign);
        Some(format!(
            "{SIGNING_ALGORITHM} Access={}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        ))
    }
}

#[async_trait]
impl Bridge for CodeArtsBridge {
    fn name(&self) -> &'static str {
        "codearts"
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
            id: format!("codearts-{}", uuid::Uuid::new_v4()),
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
        let bytes = serde_json::to_vec(&body).map_err(|e| BridgeError::Config(e.to_string()))?;
        let url = format!("{}{CHAT_PATH}", self.base);
        let headers = self.headers(&req.model, &sdk_date(), &bytes, &url);

        let mut builder = self.client.post(&url).body(bytes);
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
                format!("CodeArts 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("codearts-{}", uuid::Uuid::new_v4());
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
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                return Decoded::Nothing;
            };
            // 流内错误信封（HTTP 恒 200）
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message"))
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
            {
                let code = value
                    .get("code")
                    .or_else(|| value.get("error").and_then(|error| error.get("code")))
                    .and_then(|code| {
                        code.as_str()
                            .map(str::to_string)
                            .or_else(|| code.as_i64().map(|n| n.to_string()))
                    })
                    .unwrap_or_default();
                return Decoded::Err(BridgeError::upstream_status(
                    502,
                    format!("CodeArts 流内错误：[{code}] {message}"),
                ));
            }
            let choice = value.get("choices").and_then(|c| c.get(0));
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
            let usage = value.get("usage").map(usage_of);
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
            if delta.content.is_none() && delta.reasoning.is_none() && delta.tool_calls.is_none()
            {
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
    }
}

fn map_finish(raw: &str) -> FinishReason {
    match raw.trim().to_ascii_lowercase().as_str() {
        "length" | "max_tokens" => FinishReason::Length,
        "tool_calls" | "tool-calls" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |key: &str| -> u64 { usage.get(key).and_then(Value::as_i64).unwrap_or(0) as u64 };
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

/// ChatFormat → OpenAI 形状的请求体（max_tokens 钳到上游硬顶）。
pub fn build_body(req: &ChatFormat) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                Role::System => "system",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
                _ => "user",
            };
            let mut entry = Map::new();
            entry.insert("role".to_string(), json!(role));
            entry.insert("content".to_string(), json!(message.content_str()));
            if let Some(id) = &message.tool_call_id {
                entry.insert("tool_call_id".to_string(), json!(id));
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
            Value::Object(entry)
        })
        .collect();

    let max_tokens = req
        .max_tokens
        .map(|value| (value as i64).min(MAX_OUTPUT_TOKENS))
        .unwrap_or(MAX_OUTPUT_TOKENS);

    let mut body = Map::new();
    body.insert("model".to_string(), json!(req.model.clone()));
    body.insert("messages".to_string(), Value::Array(messages));
    body.insert("stream".to_string(), json!(true));
    body.insert(
        "stream_options".to_string(),
        json!({ "include_usage": true }),
    );
    body.insert("max_tokens".to_string(), json!(max_tokens));
    if let Some(tools) = &req.tools {
        body.insert("tools".to_string(), Value::Array(tools.clone()));
    }
    if let Some(temperature) = req.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(effort) = &req.reasoning_effort {
        body.insert("reasoning_effort".to_string(), json!(effort));
    }
    Value::Object(body)
}

fn sdk_date() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn hmac_sha256_hex(key: &[u8], message: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(message.as_bytes());
    format!("{:x}", mac.finalize().into_bytes())
}

/// 未保留字符集编码（A-Z a-z 0-9 - . _ ~ 原样，其余 `%XX` 大写）。
fn encode_component(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for byte in bytes {
        let ch = *byte;
        if ch.is_ascii_alphanumeric() || matches!(ch, b'-' | b'.' | b'_' | b'~') {
            out.push(ch as char);
        } else {
            out.push_str(&format!("%{ch:02X}"));
        }
    }
    out
}

/// `canonicalURI`：逐段编码并保证以 `/` 结尾。
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    let mut escaped: String = path
        .split('/')
        .map(|segment| encode_component(segment.as_bytes()))
        .collect::<Vec<_>>()
        .join("/");
    if !escaped.ends_with('/') {
        escaped.push('/');
    }
    escaped
}

/// `canonicalQueryString`：解码后重新编码，按键再按值排序。
fn canonical_query(raw: &str) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for part in raw.split('&') {
        if part.is_empty() || part.contains(';') {
            continue;
        }
        let (key, value) = match part.split_once('=') {
            Some((key, value)) => (key, value),
            None => (part, ""),
        };
        let Some(key) = percent_decode(key) else {
            continue;
        };
        let Some(value) = percent_decode(value) else {
            continue;
        };
        pairs.push((
            encode_component(key.as_bytes()),
            encode_component(value.as_bytes()),
        ));
    }
    pairs.sort();
    pairs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_decode(raw: &str) -> Option<String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = (bytes[index + 1] as char).to_digit(16)?;
                let low = (bytes[index + 2] as char).to_digit(16)?;
                out.push(((high << 4) | low) as u8);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
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
            model: "CodeArts".into(),
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
    fn credential_splits_into_three_parts() {
        let bridge = CodeArtsBridge::new(
            "https://example.invalid",
            "AK:SK:STS",
            reqwest::Client::new(),
        );
        assert_eq!(bridge.access_key, "AK");
        assert_eq!(bridge.secret_key, "SK");
        assert_eq!(bridge.security_token, "STS");
    }

    #[test]
    fn body_clamps_max_tokens_and_requests_usage() {
        let mut req = format(vec![user("你好")]);
        req.max_tokens = Some(100_000);
        let body = build_body(&req);
        assert_eq!(body["max_tokens"], MAX_OUTPUT_TOKENS);
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn canonical_uri_keeps_trailing_slash() {
        assert_eq!(canonical_uri("/api/v2/chat/completions"), "/api/v2/chat/completions/");
        assert_eq!(canonical_uri(""), "/");
    }

    #[test]
    fn canonical_query_sorts_pairs() {
        assert_eq!(canonical_query("b=2&a=1"), "a=1&b=2");
    }

    #[test]
    fn signature_header_has_expected_shape() {
        let bridge =
            CodeArtsBridge::new("https://example.invalid", "AK:SK", reqwest::Client::new());
        let headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("X-Sdk-Date".to_string(), "20260101T000000Z".to_string()),
        ];
        let signature = bridge
            .sign(
                "POST",
                "https://example.invalid/api/v2/chat/completions",
                &headers,
                b"{}",
            )
            .expect("应能签名");
        assert!(signature.starts_with("SDK-HMAC-SHA256 Access=AK"));
        assert!(signature.contains("SignedHeaders="));
        assert!(signature.contains("Signature="));
    }
}
