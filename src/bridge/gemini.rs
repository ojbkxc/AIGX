//! Google Gemini 原生 Bridge — 对接 Google Gemini `/v1beta/models/{model}:generateContent`。
//!
//! 参照 burncloud `crates/router/src/adaptor/gemini.rs` 的协议适配 + AIGX
//! `bridge::anthropic` 的 Bridge trait 实现模式：
//! - 鉴权头：`x-goog-api-key: <key>`（现代方式，等价于 `?key=<key>` query param）
//! - 端点：`{base_url}/models/{model}:generateContent`（非流式）/
//!   `:streamGenerateContent?alt=sse`（流式 SSE）
//! - 请求体：`{contents: [{role, parts: [{text}]}], generationConfig: {...}}`
//! - 角色：OpenAI `assistant` → Gemini `model`，`system` 提升为顶层
//!   `systemInstruction`（Gemini 1.5+ 支持）
//! - 响应：`{candidates: [{content: {parts: [{text}], role}, finishReason, index}],
//!   usageMetadata: {promptTokenCount, candidatesTokenCount, totalTokenCount}}`
//! - 流式：SSE `data: {...}`，每帧是一个完整 candidate 对象（非数组），
//!   `finishReason` 为 `STOP`/`MAX_TOKENS`/`SAFETY` → stop/length/content_filter
//!
//! 与 `OpenaiCompatibleBridge` 的区别：Gemini 用原生协议（contents/parts/generationConfig），
//! 不是 OpenAI chat.completions 形状，必须经 `ChatFormat` 归一化后翻译。

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::{
    capture_upstream_error_http, Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream,
    ChatDelta, ChatFormat, ChatMessage, ChatResponse, FinishReason, Role, UpstreamErrorView,
    UpstreamWire, UsageStats,
};

/// Gemini 默认 base_url（含 `/v1beta` 版本前缀）。
const DEFAULT_GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Gemini 原生上游 Bridge。
///
/// 持有上游 base_url 与 api_key，将归一化 `ChatFormat` 转为 Gemini
/// `generateContent` 请求体，执行 HTTP 调用并解析响应（含 SSE 流式）。
pub struct GeminiBridge {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl GeminiBridge {
    /// 构造 Bridge，复用外部传入的 `reqwest::Client`（应来自 AppState.http_client）。
    pub fn with_client(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let url = base_url.into();
        let base_url = if url.trim().is_empty() {
            DEFAULT_GEMINI_BASE_URL.to_string()
        } else {
            url.trim_end_matches('/').to_string()
        };
        Self {
            base_url,
            api_key: api_key.into(),
            client,
        }
    }

    /// 非流式端点：`{base_url}/models/{model}:generateContent`
    fn generate_url(&self, model: &str) -> String {
        format!(
            "{}/models/{}:generateContent",
            self.base_url.trim_end_matches('/'),
            model
        )
    }

    /// 流式端点：`{base_url}/models/{model}:streamGenerateContent?alt=sse`
    ///
    /// `alt=sse` 让 Gemini 返回标准 SSE 格式（`data: {...}\n\n`），
    /// 否则返回 JSON 数组流（每帧是数组元素，非 SSE）。
    fn stream_url(&self, model: &str) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.base_url.trim_end_matches('/'),
            model
        )
    }

    /// 构建带鉴权头的 POST 请求（`x-goog-api-key`）。
    fn post(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self.client.post(url).json(body);
        if !self.api_key.is_empty() {
            req = req.header("x-goog-api-key", &self.api_key);
        }
        req
    }

    /// 将归一化 `ChatFormat` 转为 Gemini `generateContent` 请求体。
    ///
    /// 参照 burncloud `GeminiAdaptor::convert_request` + Gemini 官方文档：
    /// - `system` 消息提升为顶层 `systemInstruction`（Gemini 1.5+）
    /// - `assistant` → `model`，其他角色 → `user`
    /// - `content` → `parts: [{text}]`
    /// - `temperature`/`max_tokens` → `generationConfig`（camelCase）
    fn build_body(&self, req: &ChatFormat, stream: bool) -> Value {
        let mut system_parts: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for m in &req.messages {
            match m.role {
                Role::System => {
                    // 系统消息合并为顶层 systemInstruction
                    system_parts.push(m.content_str().to_string());
                }
                Role::User | Role::Assistant | Role::Tool => {
                    // Gemini 角色：assistant → model，其他 → user
                    let role = if matches!(m.role, Role::Assistant) {
                        "model"
                    } else {
                        "user"
                    };
                    let text = m.content_str().to_string();
                    contents.push(serde_json::json!({
                        "role": role,
                        "parts": [{"text": text}]
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "contents": contents,
        });

        // 系统指令（Gemini 1.5+ 支持）
        if !system_parts.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": system_parts.join("\n\n")}]
            });
        }

        // 生成配置（camelCase）
        let mut gen_config = serde_json::Map::new();
        if let Some(t) = req.temperature {
            gen_config.insert("temperature".into(), serde_json::json!(t));
        }
        if let Some(mt) = req.max_tokens {
            gen_config.insert("maxOutputTokens".into(), serde_json::json!(mt));
        }
        if let Some(p) = req.top_p {
            gen_config.insert("topP".into(), serde_json::json!(p));
        }
        if let Some(stop) = &req.stop {
            gen_config.insert("stopSequences".into(), serde_json::json!(stop));
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = Value::Object(gen_config);
        }

        // 流式标志（Gemini 用 URL 区分，但部分代理需要 body 中 stream=true）
        if stream {
            body["stream"] = serde_json::json!(true);
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
impl Bridge for GeminiBridge {
    fn name(&self) -> &'static str {
        "gemini"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let body = self.build_body(req, false);
        let url = self.generate_url(&req.model);
        let resp = self
            .post(&url, &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Vertex,
                parse_gemini_error,
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
        let url = self.stream_url(&req.model);
        let resp = self
            .post(&url, &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Vertex,
                parse_gemini_error,
            )
            .await);
        }

        let id = format!("gemini_{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let byte_stream = resp.bytes_stream();

        // 参照 anthropic.rs 的 unfold 模式：把 SSE 解码器与流状态作为种子
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
                    // 优先吐出上一批积压的 chunk
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
                                        // 流结束：产出带 finish_reason=Stop 的终帧
                                        // （若尚未发过 finish_reason）
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
    /// 是否已发送 finish_reason（避免 [DONE] 时重复发）
    sent_finish: bool,
}

/// Gemini finishReason → 内部 FinishReason
///
/// 参照 burncloud `gemini.rs`：
/// - `STOP` → stop
/// - `MAX_TOKENS` → length
/// - `SAFETY` → content_filter
/// - 其他 → stop
fn parse_finish_reason(s: &str) -> FinishReason {
    match s {
        "STOP" => FinishReason::Stop,
        "MAX_TOKENS" => FinishReason::Length,
        "SAFETY" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

/// 从 Gemini 响应解析 usage（usageMetadata）
///
/// Gemini 字段：`promptTokenCount` / `candidatesTokenCount` / `totalTokenCount`
/// （camelCase，与 OpenAI 的 snake_case 不同）。
fn parse_usage(json: &Value) -> UsageStats {
    let u = json.get("usageMetadata");
    let prompt = u
        .and_then(|u| u.get("promptTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = u
        .and_then(|u| u.get("candidatesTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = u
        .and_then(|u| u.get("totalTokenCount"))
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt + completion);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 解析 Gemini 非流式响应为 `ChatResponse`。
///
/// 参照 burncloud `GeminiAdaptor::convert_response`：
/// - `candidates[0].content.parts[*].text` 拼接为 message.content
/// - `candidates[0].finishReason` → FinishReason
/// - `usageMetadata` → UsageStats
/// - 错误响应 `{error: {code, message, status}}` → BridgeError（由调用方处理）
fn parse_response(json: &Value, fallback_model: &str) -> ChatResponse {
    // 错误响应由 capture_upstream_error_http 处理，此处仅解析成功响应
    let candidate = json.get("candidates").and_then(|c| c.get(0));

    // 拼接所有 parts 的 text（多 part 情况）
    let text = candidate
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();

    let finish_reason = candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(|s| s.as_str())
        .map(parse_finish_reason)
        .unwrap_or(FinishReason::Stop);

    let usage = parse_usage(json);

    let msg = ChatMessage::assistant(text);

    ChatResponse {
        id: format!("gemini_{}", uuid::Uuid::new_v4()),
        model: fallback_model.to_string(),
        message: msg,
        finish_reason,
        usage,
    }
}

/// 解析 Gemini 错误响应：`{error: {code, message, status}}`
///
/// Gemini 错误形状与 Vertex AI 一致（`status` 为 gRPC 规范码字符串，
/// 如 `INVALID_ARGUMENT`、`PERMISSION_DENIED`）。
fn parse_gemini_error(body: &[u8]) -> Option<UpstreamErrorView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    Some(UpstreamErrorView {
        kind: err.get("status").and_then(|t| t.as_str()).map(String::from),
        message: err
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from),
        code: err
            .get("code")
            .and_then(|c| c.as_i64())
            .map(|i| i.to_string()),
        param: None,
    })
}

/// 解析单条 Gemini SSE 事件 JSON 为 0 或 1 个 `ChatChunk`。
///
/// 参照 burncloud `GeminiAdaptor::convert_stream_response`：
/// - 每帧是完整 candidate 对象（非数组，`alt=sse` 模式下）
/// - `candidates[0].content.parts[0].text` → delta.content
/// - `candidates[0].finishReason` → finish_reason
/// - `usageMetadata` → usage（终帧携带）
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
                "failed to parse Gemini SSE event: {e}"
            ))));
        }
    };

    // 流内 error 帧
    if let Some(err) = v.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("upstream reported a stream error")
            .to_string();
        return Some(Err(BridgeError::UpstreamStatus {
            status: 502,
            message,
            parsed: None,
            wire: UpstreamWire::Vertex,
            retry_after: None,
        }));
    }

    let candidate = v.get("candidates").and_then(|c| c.get(0));

    // 提取文本增量
    let text = candidate
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .and_then(|parts| {
            parts
                .iter()
                .find_map(|p| p.get("text").and_then(|t| t.as_str()).map(String::from))
        });

    // 提取 finishReason
    let finish = candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(|s| s.as_str())
        .map(parse_finish_reason);

    // 提取 usageMetadata（终帧携带）
    let usage = v.get("usageMetadata").map(parse_usage_from_value);

    // 跳过空帧（无文本、无 finish、无 usage）
    if text.is_none() && finish.is_none() && usage.is_none() {
        return None;
    }

    // 标记已发送 finish_reason
    if finish.is_some() {
        state.sent_finish = true;
    }

    Some(Ok(ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta {
            content: text,
            tool_calls: None,
            reasoning: None,
        },
        finish_reason: finish,
        usage,
    }))
}

/// 从 usageMetadata 子对象解析（流式终帧）
fn parse_usage_from_value(u: &Value) -> UsageStats {
    let prompt = u
        .get("promptTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = u
        .get("candidatesTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total = u
        .get("totalTokenCount")
        .and_then(|v| v.as_u64())
        .unwrap_or(prompt + completion);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 构造一个 Gemini 原生 Bridge 的 `Arc` 实例（供 ChannelStore/Hub 调度使用）。
pub fn make_bridge(base_url: &str, api_key: &str, client: &reqwest::Client) -> Arc<dyn Bridge> {
    Arc::new(GeminiBridge::with_client(base_url, api_key, client.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_body_converts_messages_to_contents() {
        let req = ChatFormat {
            model: "gemini-pro".to_string(),
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
                ChatMessage {
                    role: Role::Assistant,
                    content: Some("Hi there".to_string()),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                },
            ],
            tools: None,
            max_tokens: Some(100),
            temperature: Some(0.5),
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };

        let bridge = GeminiBridge::with_client("", "test-key", reqwest::Client::new());
        let body = bridge.build_body(&req, false);

        // 系统消息提升为 systemInstruction
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "Be helpful");
        // user → user
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "Hello");
        // assistant → model
        assert_eq!(body["contents"][1]["role"], "model");
        assert_eq!(body["contents"][1]["parts"][0]["text"], "Hi there");
        // generationConfig（camelCase）
        assert_eq!(body["generationConfig"]["temperature"], 0.5);
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 100);
    }

    #[test]
    fn parse_response_extracts_text_and_usage() {
        let gemini_resp = json!({
            "candidates": [
                {
                    "content": {
                        "parts": [{"text": "Hi there!"}],
                        "role": "model"
                    },
                    "finishReason": "STOP",
                    "index": 0
                }
            ],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 10,
                "totalTokenCount": 15
            }
        });

        let resp = parse_response(&gemini_resp, "gemini-pro");
        assert_eq!(resp.message.content.as_deref(), Some("Hi there!"));
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        assert_eq!(resp.usage.prompt_tokens, 5);
        assert_eq!(resp.usage.completion_tokens, 10);
        assert_eq!(resp.usage.total_tokens, 15);
    }

    #[test]
    fn parse_finish_reason_maps_gemini_codes() {
        assert_eq!(parse_finish_reason("STOP"), FinishReason::Stop);
        assert_eq!(parse_finish_reason("MAX_TOKENS"), FinishReason::Length);
        assert_eq!(parse_finish_reason("SAFETY"), FinishReason::ContentFilter);
        // 未知 → stop
        assert_eq!(parse_finish_reason("OTHER"), FinishReason::Stop);
    }

    #[test]
    fn parse_gemini_error_extracts_fields() {
        let err_body = br#"{"error": {"code": 400, "message": "API key not valid", "status": "INVALID_ARGUMENT"}}"#;
        let view = parse_gemini_error(err_body).expect("should parse");
        assert_eq!(view.kind.as_deref(), Some("INVALID_ARGUMENT"));
        assert_eq!(view.message.as_deref(), Some("API key not valid"));
        assert_eq!(view.code.as_deref(), Some("400"));
    }

    #[test]
    fn parse_stream_event_extracts_delta_text() {
        let payload = r#"{"candidates": [{"content": {"parts": [{"text": "Hello"}], "role": "model"}, "index": 0}]}"#;
        let mut state = StreamState {
            id: "test".to_string(),
            model: "gemini-pro".to_string(),
            sent_finish: false,
        };
        let chunk = parse_stream_event(payload, "test", "gemini-pro", &mut state)
            .expect("should parse")
            .expect("should be ok");
        assert_eq!(chunk.delta.content.as_deref(), Some("Hello"));
        assert!(chunk.finish_reason.is_none());
    }

    #[test]
    fn parse_stream_event_extracts_finish_and_usage() {
        let payload = r#"{"candidates": [{"finishReason": "STOP", "index": 0}], "usageMetadata": {"promptTokenCount": 5, "candidatesTokenCount": 10, "totalTokenCount": 15}}"#;
        let mut state = StreamState {
            id: "test".to_string(),
            model: "gemini-pro".to_string(),
            sent_finish: false,
        };
        let chunk = parse_stream_event(payload, "test", "gemini-pro", &mut state)
            .expect("should parse")
            .expect("should be ok");
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
        assert_eq!(chunk.usage.as_ref().unwrap().prompt_tokens, 5);
        assert_eq!(chunk.usage.as_ref().unwrap().completion_tokens, 10);
        assert!(state.sent_finish);
    }

    #[test]
    fn parse_stream_event_returns_none_for_empty_chunk() {
        let payload = r#"{"candidates": [{"index": 0}]}"#;
        let mut state = StreamState::default();
        assert!(parse_stream_event(payload, "test", "gemini-pro", &mut state).is_none());
    }

    #[test]
    fn generate_url_and_stream_url_format() {
        let bridge = GeminiBridge::with_client("", "key", reqwest::Client::new());
        assert_eq!(
            bridge.generate_url("gemini-pro"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-pro:generateContent"
        );
        assert_eq!(
            bridge.stream_url("gemini-pro"),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn with_client_uses_default_base_url_when_empty() {
        let bridge = GeminiBridge::with_client("", "key", reqwest::Client::new());
        assert_eq!(
            bridge.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn with_client_preserves_custom_base_url() {
        let bridge = GeminiBridge::with_client(
            "https://custom.example.com/v1beta/",
            "key",
            reqwest::Client::new(),
        );
        assert_eq!(bridge.base_url, "https://custom.example.com/v1beta");
    }
}
