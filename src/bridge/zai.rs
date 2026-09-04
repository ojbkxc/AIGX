//! 智谱 AI（Z.AI）Bridge — 对接智谱 AI Anthropic 兼容 API。
//!
//! 参照 burncloud `crates/router/src/adaptor/zai.rs` 的协议适配 + AIGX
//! `bridge::anthropic` 的 Bridge trait 实现模式：
//! - 鉴权头：`Authorization: Bearer <key>`（与 Anthropic 的 `x-api-key` 不同）
//! - 端点：`{base_url}/messages`（Anthropic 兼容形状）
//! - 请求体：`{model, messages, max_tokens, system?, temperature?, stream?}`
//! - `system` 提升为顶层字段，不混入 messages
//! - `max_tokens` 必填，缺失时兜底 4096
//! - 响应：`{content: [{type: "text", text: "..."}], usage: {input_tokens, output_tokens}}`
//! - 流式：Anthropic 风格 SSE（`event: content_block_delta` + `data: {...}`）
//!
//! 与 `AnthropicBridge` 的区别：智谱 AI 用 Bearer 鉴权而非 `x-api-key`，
//! 不需要 `anthropic-version` 头。协议形状与 Anthropic 兼容，
//! 但鉴权方式不同，故独立实现。

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::{
    capture_upstream_error_http, Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream,
    ChatDelta, ChatFormat, ChatMessage, ChatResponse, FinishReason, Role, UpstreamErrorView,
    UpstreamWire, UsageStats,
};

/// 智谱 AI 默认 base_url（Anthropic 兼容端点）。
const DEFAULT_ZAI_BASE_URL: &str = "https://api.z.ai/api/v2";

/// 智谱 AI 原生上游 Bridge。
///
/// 持有上游 base_url 与 api_key，将归一化 `ChatFormat` 转为智谱 AI
/// `/messages` 请求体（Anthropic 兼容形状），执行 HTTP 调用并解析响应。
pub struct ZaiBridge {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl ZaiBridge {
    /// 构造 Bridge，复用外部传入的 `reqwest::Client`（应来自 AppState.http_client）。
    pub fn with_client(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let url = base_url.into();
        let base_url = if url.trim().is_empty() {
            DEFAULT_ZAI_BASE_URL.to_string()
        } else {
            url.trim_end_matches('/').to_string()
        };
        Self {
            base_url,
            api_key: api_key.into(),
            client,
        }
    }

    /// 消息端点：`{base_url}/messages`
    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }

    /// 构建带鉴权头的 POST 请求（`Authorization: Bearer`）。
    ///
    /// 与 AnthropicBridge 不同：智谱 AI 用 Bearer 而非 `x-api-key`，
    /// 不需要 `anthropic-version` 头。
    fn post(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self.client.post(url).json(body);
        if !self.api_key.is_empty() {
            req = req.header("authorization", format!("Bearer {}", self.api_key));
        }
        req
    }

    /// 将归一化 `ChatFormat` 转为智谱 AI `/messages` 请求体。
    ///
    /// 参照 burncloud `ZaiAdaptor::convert_request`：
    /// - `system` 消息提升为顶层 `system` 字段
    /// - 其他消息保留 `role` + `content`（不转 content blocks）
    /// - `max_tokens` 必填，缺失兜底 4096
    /// - `temperature` 可选
    /// - `stream` 标志透传
    fn build_body(&self, req: &ChatFormat, stream: bool) -> Value {
        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for m in &req.messages {
            match m.role {
                Role::System => {
                    system_parts.push(m.content_str().to_string());
                }
                Role::User | Role::Assistant | Role::Tool => {
                    let role = match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                        Role::Tool => "user",
                        _ => "user",
                    };
                    let content = m.content_str().to_string();
                    let mut msg = serde_json::json!({
                        "role": role,
                        "content": content,
                    });
                    // 工具调用回放（assistant 消息携带 tool_calls）
                    if let Some(tool_calls) = &m.tool_calls {
                        let content_blocks: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "type": "tool_use",
                                    "id": tc.id,
                                    "name": tc.function_name,
                                    "input": serde_json::from_str::<Value>(&tc.arguments)
                                        .unwrap_or(Value::Null),
                                })
                            })
                            .collect();
                        msg["content"] = Value::Array(content_blocks);
                    }
                    // tool 结果消息
                    if matches!(m.role, Role::Tool) {
                        let tool_use_id = m.tool_call_id.as_deref().unwrap_or("");
                        msg = serde_json::json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tool_use_id,
                                "content": content,
                            }]
                        });
                    }
                    messages.push(msg);
                }
            }
        }

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            // 智谱 AI 要求 max_tokens 必填；缺失兜底 4096
            "max_tokens": req.max_tokens.unwrap_or(4096),
            "stream": stream,
        });

        // 系统提示
        if !system_parts.is_empty() {
            body["system"] = Value::String(system_parts.join("\n\n"));
        }

        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        if let Some(k) = req.top_k {
            body["top_k"] = serde_json::json!(k);
        }
        if let Some(stop) = &req.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }

        // 透传额外字段
        if let Some(extra) = &req.extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }

        body
    }
}

#[async_trait]
impl Bridge for ZaiBridge {
    fn name(&self) -> &'static str {
        "zai"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let body = self.build_body(req, false);
        let resp = self
            .post(&self.messages_url(), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Anthropic,
                parse_zai_error,
            )
            .await);
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;

        Ok(parse_response(&json, &req.model))
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let body = self.build_body(req, true);
        let resp = self
            .post(&self.messages_url(), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Anthropic,
                parse_zai_error,
            )
            .await);
        }

        let id = format!("zai_{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let byte_stream = resp.bytes_stream();

        // 参照 anthropic.rs 的 unfold 模式
        let stream = futures::stream::unfold(
            (
                byte_stream,
                crate::sse::SseDecoder::new(),
                StreamState {
                    id: id.clone(),
                    model: model.clone(),
                    ..Default::default()
                },
                id,
                model,
                Vec::<std::result::Result<ChatChunk, BridgeError>>::new(),
            ),
            |(mut byte_stream, mut decoder, mut state, id, model, mut pending)| async move {
                use futures::StreamExt;
                loop {
                    if !pending.is_empty() {
                        let first = pending.remove(0);
                        return Some((first, (byte_stream, decoder, state, id, model, pending)));
                    }
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            let events = decoder.feed(bytes.as_ref());
                            for ev in events {
                                match ev {
                                    crate::sse::SseEvent::Data(payload) => {
                                        if let Some(chunk) =
                                            parse_stream_event(&payload, &id, &model, &mut state)
                                        {
                                            match chunk {
                                                Ok(c) => pending.push(Ok(c)),
                                                Err(e) => pending.push(Err(e)),
                                            }
                                        }
                                    }
                                    crate::sse::SseEvent::Done => {
                                        // 流结束
                                        if !state.sent_finish {
                                            pending.push(Ok(ChatChunk {
                                                id: state.id.clone(),
                                                model: state.model.clone(),
                                                delta: ChatDelta::default(),
                                                finish_reason: Some(FinishReason::Stop),
                                                usage: None,
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(BridgeError::Transport(e.to_string())),
                                (byte_stream, decoder, state, id, model, pending),
                            ));
                        }
                        None => {
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 流式状态：跨事件携带 id / model / usage 计数
#[derive(Clone, Default)]
struct StreamState {
    id: String,
    model: String,
    input_tokens: u64,
    sent_finish: bool,
}

/// 智谱 AI stop_reason → 内部 FinishReason
///
/// 与 Anthropic 兼容：`end_turn`/`stop_sequence` → stop，
/// `max_tokens` → length，`tool_use` → tool_calls。
fn parse_stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

/// 解析智谱 AI 非流式响应为 `ChatResponse`。
///
/// 参照 burncloud `ZaiAdaptor::convert_response`：
/// - `content` 数组中 `type: "text"` 块的 `text` 拼接为 message.content
/// - `stop_reason` → FinishReason
/// - `usage: {input_tokens, output_tokens}` → UsageStats
fn parse_response(json: &Value, fallback_model: &str) -> ChatResponse {
    // 拼接所有 text 块
    let text = json
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    // 工具调用
    let tool_calls = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            let calls: Vec<super::ToolCall> = arr
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        return None;
                    }
                    Some(super::ToolCall {
                        id: b
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        function_name: b
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: b
                            .get("input")
                            .map(|i| match i {
                                Value::Null => "{}".to_string(),
                                other => serde_json::to_string(other)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            })
                            .unwrap_or_else(|| "{}".to_string()),
                    })
                })
                .collect();
            if calls.is_empty() {
                None
            } else {
                Some(calls)
            }
        })
        .map(|calls: Vec<super::ToolCall>| {
            calls
                .into_iter()
                .map(|mut tc| {
                    tc.arguments = super::tool_repair::repair_tool_arguments(&tc.arguments);
                    tc
                })
                .collect()
        });

    let mut msg = ChatMessage::assistant(text);
    msg.tool_calls = tool_calls;

    let finish_reason = json
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .map(parse_stop_reason)
        .unwrap_or(FinishReason::Stop);

    let usage = parse_usage(json);

    ChatResponse {
        id: json
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("zai")
            .to_string(),
        model: json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(fallback_model)
            .to_string(),
        message: msg,
        finish_reason,
        usage,
    }
}

/// 从智谱 AI 响应解析 usage
///
/// 智谱 AI 用 Anthropic 形状：`{input_tokens, output_tokens}`。
fn parse_usage(json: &Value) -> UsageStats {
    let u = json.get("usage");
    let prompt = u
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = u
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 解析智谱 AI 错误响应：`{type: "error", error: {type, message}}`
///
/// 与 Anthropic 错误形状一致。
fn parse_zai_error(body: &[u8]) -> Option<UpstreamErrorView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    Some(UpstreamErrorView {
        kind: err.get("type").and_then(|t| t.as_str()).map(String::from),
        message: err
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from),
        code: None,
        param: None,
    })
}

/// 解析单条智谱 AI SSE 事件 JSON 为 0 或 1 个 `ChatChunk`。
///
/// 参照 burncloud `ZaiAdaptor::convert_stream_chunk`：
/// - `content_block_delta` + `delta.text_delta.text` → delta.content
/// - `content_block_delta` + `delta.input_json_delta` → tool_calls 增量
/// - `message_delta` + `delta.stop_reason` → finish_reason
/// - `message_stop` → finish_reason=Stop
/// - `message_start` → 捕获 input_tokens
fn parse_stream_event(
    payload: &str,
    id: &str,
    model: &str,
    state: &mut StreamState,
) -> Option<std::result::Result<ChatChunk, BridgeError>> {
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(BridgeError::UpstreamDecode(format!(
                "failed to parse ZAI SSE event: {e}"
            ))));
        }
    };

    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    // message_start：捕获 input_tokens
    if event_type == "message_start" {
        if let Some(message) = v.get("message") {
            state.id = message
                .get("id")
                .and_then(|i| i.as_str())
                .unwrap_or(id)
                .to_string();
            state.model = message
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(model)
                .to_string();
            if let Some(usage) = message.get("usage") {
                state.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
            }
        }
        return None;
    }

    // message_delta：产出 finish_reason + usage
    if event_type == "message_delta" {
        let finish = v
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(|s| s.as_str())
            .map(parse_stop_reason);
        let completion = v
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|t| t.as_u64());
        let usage = completion.map(|c| UsageStats {
            prompt_tokens: state.input_tokens,
            completion_tokens: c,
            total_tokens: state.input_tokens + c,
            cached_prompt_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        });
        if finish.is_none() && usage.is_none() {
            return None;
        }
        if finish.is_some() {
            state.sent_finish = true;
        }
        return Some(Ok(ChatChunk {
            id: state.id.clone(),
            model: state.model.clone(),
            delta: ChatDelta::default(),
            finish_reason: finish,
            usage,
        }));
    }

    // message_stop：流结束
    if event_type == "message_stop" {
        if !state.sent_finish {
            state.sent_finish = true;
            return Some(Ok(ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta::default(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
            }));
        }
        return None;
    }

    // 流内 error 帧
    if event_type == "error" {
        let err = v.get("error").cloned().unwrap_or(Value::Null);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("upstream reported a stream error")
            .to_string();
        return Some(Err(BridgeError::UpstreamStatus {
            status: 502,
            message,
            parsed: None,
            wire: UpstreamWire::Anthropic,
            retry_after: None,
        }));
    }

    // content_block_delta：文本增量或工具调用增量
    if event_type == "content_block_delta" {
        if let Some(delta) = v.get("delta") {
            match delta.get("type").and_then(|t| t.as_str()) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        return Some(Ok(ChatChunk {
                            id: state.id.clone(),
                            model: state.model.clone(),
                            delta: ChatDelta {
                                content: Some(text.to_string()),
                                tool_calls: None,
                                reasoning: None,
                            },
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                Some("input_json_delta") => {
                    // tool_use 参数增量
                    if let Some(pj) = delta.get("partial_json").and_then(|p| p.as_str()) {
                        let block_index =
                            v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        return Some(Ok(ChatChunk {
                            id: state.id.clone(),
                            model: state.model.clone(),
                            delta: ChatDelta {
                                content: None,
                                tool_calls: Some(vec![super::ToolCallDelta {
                                    index: block_index,
                                    id: None,
                                    function_name: None,
                                    arguments: Some(pj.to_string()),
                                }]),
                                reasoning: None,
                            },
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    // content_block_start（tool_use）：开启工具调用
    if event_type == "content_block_start" {
        if let Some(block) = v.get("content_block") {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let block_index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let id = block.get("id").and_then(|i| i.as_str()).map(String::from);
                let name = block.get("name").and_then(|n| n.as_str()).map(String::from);
                return Some(Ok(ChatChunk {
                    id: state.id.clone(),
                    model: state.model.clone(),
                    delta: ChatDelta {
                        content: None,
                        tool_calls: Some(vec![super::ToolCallDelta {
                            index: block_index,
                            id,
                            function_name: name,
                            arguments: None,
                        }]),
                        reasoning: None,
                    },
                    finish_reason: None,
                    usage: None,
                }));
            }
        }
    }

    None
}

/// 构造一个智谱 AI Bridge 的 `Arc` 实例（供 ChannelStore/Hub 调度使用）。
pub fn make_bridge(base_url: &str, api_key: &str, client: &reqwest::Client) -> Arc<dyn Bridge> {
    Arc::new(ZaiBridge::with_client(base_url, api_key, client.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_body_separates_system_and_messages() {
        let req = ChatFormat {
            model: "glm-5".to_string(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: Some("Be helpful".to_string()),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                },
                ChatMessage {
                    role: Role::User,
                    content: Some("Hello".to_string()),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                },
            ],
            tools: None,
            max_tokens: Some(100),
            temperature: Some(0.7),
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };

        let bridge = ZaiBridge::with_client("", "test-key", reqwest::Client::new());
        let body = bridge.build_body(&req, false);

        assert_eq!(body["model"], "glm-5");
        assert_eq!(body["system"], "Be helpful");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["temperature"], 0.7);
        let messages = body["messages"]
            .as_array()
            .expect("messages should be array");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn build_body_defaults_max_tokens_to_4096() {
        let req = ChatFormat {
            model: "glm-5".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("Hi".to_string()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };

        let bridge = ZaiBridge::with_client("", "key", reqwest::Client::new());
        let body = bridge.build_body(&req, false);
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn build_body_includes_stream_flag() {
        let req = ChatFormat {
            model: "glm-5".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("Hi".to_string()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: Some(100),
            temperature: None,
            top_p: None,
            stream: true,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };

        let bridge = ZaiBridge::with_client("", "key", reqwest::Client::new());
        let body = bridge.build_body(&req, true);
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn parse_response_extracts_text_and_usage() {
        let zai_resp = json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "glm-5",
            "content": [{
                "type": "text",
                "text": "Hello! How can I help you today?"
            }],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 7,
                "output_tokens": 10
            }
        });

        let resp = parse_response(&zai_resp, "glm-5");
        assert_eq!(
            resp.message.content.as_deref(),
            Some("Hello! How can I help you today?")
        );
        assert_eq!(resp.model, "glm-5");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.prompt_tokens, 7);
        assert_eq!(resp.usage.completion_tokens, 10);
        assert_eq!(resp.usage.total_tokens, 17);
    }

    #[test]
    fn parse_stop_reason_maps_anthropic_codes() {
        assert_eq!(parse_stop_reason("end_turn"), FinishReason::Stop);
        assert_eq!(parse_stop_reason("stop_sequence"), FinishReason::Stop);
        assert_eq!(parse_stop_reason("max_tokens"), FinishReason::Length);
        assert_eq!(parse_stop_reason("tool_use"), FinishReason::ToolCalls);
        assert_eq!(parse_stop_reason("other"), FinishReason::Stop);
    }

    #[test]
    fn parse_zai_error_extracts_fields() {
        let err_body = br#"{"type":"error","error":{"type":"invalid_request_error","message":"Invalid model"}}"#;
        let view = parse_zai_error(err_body).expect("should parse");
        assert_eq!(view.kind.as_deref(), Some("invalid_request_error"));
        assert_eq!(view.message.as_deref(), Some("Invalid model"));
    }

    #[test]
    fn parse_stream_event_text_delta() {
        let payload = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let mut state = StreamState {
            id: "test".to_string(),
            model: "glm-5".to_string(),
            input_tokens: 0,
            sent_finish: false,
        };
        let chunk = parse_stream_event(payload, "test", "glm-5", &mut state)
            .expect("should parse")
            .expect("should be ok");
        assert_eq!(chunk.delta.content.as_deref(), Some("Hello"));
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn parse_stream_event_message_delta_with_finish_and_usage() {
        let payload = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":15}}"#;
        let mut state = StreamState {
            id: "test".to_string(),
            model: "glm-5".to_string(),
            input_tokens: 5,
            sent_finish: false,
        };
        let chunk = parse_stream_event(payload, "test", "glm-5", &mut state)
            .expect("should parse")
            .expect("should be ok");
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
        assert_eq!(chunk.usage.as_ref().unwrap().prompt_tokens, 5);
        assert_eq!(chunk.usage.as_ref().unwrap().completion_tokens, 15);
        assert!(state.sent_finish);
    }

    #[test]
    fn parse_stream_event_message_start_captures_input_tokens() {
        let payload = r#"{"type":"message_start","message":{"id":"msg_abc","model":"glm-5","usage":{"input_tokens":10}}}"#;
        let mut state = StreamState::default();
        // message_start 不产出 chunk，但更新 state
        let result = parse_stream_event(payload, "test", "glm-5", &mut state);
        assert!(result.is_none());
        assert_eq!(state.id, "msg_abc");
        assert_eq!(state.model, "glm-5");
        assert_eq!(state.input_tokens, 10);
    }

    #[test]
    fn parse_stream_event_returns_none_for_unhandled_events() {
        let payload =
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#;
        let mut state = StreamState::default();
        // text 类型的 content_block_start 不产出 chunk（仅 tool_use 才产出）
        assert!(parse_stream_event(payload, "test", "glm-5", &mut state).is_none());
    }

    #[test]
    fn messages_url_format() {
        let bridge = ZaiBridge::with_client("", "key", reqwest::Client::new());
        assert_eq!(bridge.messages_url(), "https://api.z.ai/api/v2/messages");
    }

    #[test]
    fn with_client_uses_default_base_url_when_empty() {
        let bridge = ZaiBridge::with_client("", "key", reqwest::Client::new());
        assert_eq!(bridge.base_url, "https://api.z.ai/api/v2");
    }

    #[test]
    fn with_client_preserves_custom_base_url() {
        let bridge = ZaiBridge::with_client(
            "https://custom.example.com/v1/",
            "key",
            reqwest::Client::new(),
        );
        assert_eq!(bridge.base_url, "https://custom.example.com/v1");
    }
}
