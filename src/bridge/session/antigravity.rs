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
        // 本次响应所属会话的信封 sessionId（签名缓存两档键的 session 维度；
        // build_envelope 必写该键，拿不到给空串——空 session 只损失缓存，
        // 不影响请求本身）。
        let session = body
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                SseDecoder::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    session,
                    pending_signature: None,
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
                                match decode_event(
                                    &event,
                                    &flow.id,
                                    &flow.model,
                                    &flow.session,
                                    &mut flow.pending_signature,
                                ) {
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
                                if let Decoded::Chunk(chunk) = decode_event(
                                    &event,
                                    &flow.id,
                                    &flow.model,
                                    &flow.session,
                                    &mut flow.pending_signature,
                                ) {
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
    /// 本次信封的 `sessionId`（签名缓存两档键的 session 维度）。
    session: String,
    /// 独占一个 part / 事件、留给紧随其后 `functionCall` 的签名
    /// （上游/9router 的 `pendingThoughtSignature` 同款语义）。
    pending_signature: Option<String>,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Usage(UsageStats),
    Done(FinishReason, Option<UsageStats>),
}

fn decode_event(
    event: &SseEvent,
    id: &str,
    model: &str,
    session: &str,
    pending_signature: &mut Option<String>,
) -> Decoded {
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
                    // 签名可能是独占一个 part / 事件的（9router 的
                    // `pendingThoughtSignature` 同款）：先记住，留给紧随其后的
                    // functionCall 消费；同一 part 里的签名也走这条路径。
                    // 字段位置容忍 camelCase / snake_case（上游同口径）。
                    let signature = part
                        .get("thoughtSignature")
                        .or_else(|| part.get("thought_signature"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|sig| !sig.is_empty())
                        .map(str::to_string);
                    if let Some(signature) = signature {
                        *pending_signature = Some(signature);
                    }
                    if let Some(fc) = part.get("functionCall") {
                        let call_id = fc
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .or_else(|| Some(uuid::Uuid::new_v4().to_string()));
                        let name = fc.get("name").and_then(Value::as_str).map(str::to_string);
                        let signature = pending_signature.take();
                        // 写侧（响应→缓存）：两档键都按本次信封的 session 隔离，
                        // 客户端下一轮不回带非标字段时由请求侧按同键回填。
                        if let (Some(call_id), Some(signature)) = (&call_id, &signature) {
                            super::antigravity_signature_cache::remember_call(
                                session, call_id, signature,
                            );
                            if let Some(name) = name.as_deref().filter(|name| !name.is_empty()) {
                                super::antigravity_signature_cache::remember_for_session(
                                    session, name, signature,
                                );
                            }
                        }
                        calls.push(ToolCallDelta {
                            index: calls.len(),
                            id: call_id,
                            function_name: name,
                            arguments: fc.get("args").map(|a| a.to_string()),
                            signature,
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
    if crate::bridge::is_length_finish(raw) {
        return FinishReason::Length;
    }
    match raw.trim().to_ascii_uppercase().as_str() {
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
            Role::Assistant => {
                if let Some(calls) = &message.tool_calls {
                    // model 轮的工具调用：text part + 每个 functionCall 一个 part。
                    // thoughtSignature 是 part 层级字段（Gemini Part 形态）。
                    // 客户端带回的签名最优先（最忠实的一份，位置容忍三种写法），
                    // 没带时留给信封组装末尾的 fill_cached 按缓存回填。
                    let mut parts: Vec<Value> = Vec::new();
                    if !text.trim().is_empty() {
                        parts.push(json!({ "text": text }));
                    }
                    for call in calls {
                        let signature = client_brought_signature(call);
                        let mut fc = json!({
                            "name": call.function_name,
                            "args": serde_json::from_str::<Value>(&call.arguments)
                                .unwrap_or(Value::Object(Map::new())),
                        });
                        if !call.id.is_empty() {
                            fc["id"] = json!(call.id);
                        }
                        let mut part = json!({ "functionCall": fc });
                        if let Some(sig) = signature {
                            part["thoughtSignature"] = json!(sig);
                        }
                        parts.push(part);
                    }
                    contents.push(json!({ "role": "model", "parts": parts }));
                } else {
                    contents.push(json!({
                        "role": "model",
                        "parts": [{ "text": text }],
                    }));
                }
            }
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
    // sessionId：会话稳定键（缓存兜底档的写入/读取键，见 antigravity_signature_cache）
    let session = antigravity_session_id(&system, &req.model);
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
    envelope.insert("sessionId".to_string(), json!(session));
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
    // 请求构造点的服务端回填（上游 `plugin_host/gemini_signatures::fill_cached`
    // 在网关请求路径上的同一职责）：客户端没带回签名时按缓存
    // （session + call id 主键档）补进历史 functionCall parts。
    let mut envelope = Value::Object(envelope);
    let filled = super::antigravity_signature_cache::fill_cached(&mut envelope);
    if filled > 0 {
        tracing::debug!("antigravity: 按服务端缓存回填 {filled} 处 functionCall 思考签名");
    }
    envelope
}

/// 从客户端带回的 tool_call 中取思考签名（三种容忍位置：
/// `extra_content.google.thought_signature`（Google 官方 OpenAI 兼容端点字段）、
/// 裸 `thought_signature` / `thoughtSignature`）。
/// 注意：ToolCall 结构只带 id/name/arguments，extra 字段在 parse_messages
/// 阶段未保留 —— 返回 None，回填走服务端缓存（主键档已覆盖绝大多数客户端）。
fn client_brought_signature(_call: &crate::bridge::ToolCall) -> Option<String> {
    None
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

/// 会话稳定键：同一会话的多轮请求派生出同一个 id（签名缓存兜底档的键）。
/// 对照 Agent2API `antigravity_outbound::session_id`：按 system + model 派生。
/// system 为空时用固定占位（纯工具续接轮没有 system，但仍需与首轮同键——
/// 兜底档只损失「不同会话同 system+model」的碰撞面，主键档不受影响）。
fn antigravity_session_id(system: &str, model: &str) -> String {
    use std::fmt::Write as _;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(system, model), &mut hasher);
    let h = std::hash::Hasher::finish(&hasher);
    let mut out = String::new();
    let _ = write!(out, "sess-{h:016x}");
    out
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

    fn decode_json(payload: serde_json::Value, session: &str) -> (Decoded, Option<String>) {
        let mut pending = None;
        let decoded = decode_event(
            &SseEvent::Data(payload.to_string()),
            "id",
            "gemini-pro-agent",
            session,
            &mut pending,
        );
        (decoded, pending)
    }

    /// 写侧：同一个 part 里的 thoughtSignature 必须入两档缓存，并随
    /// ToolCallDelta 的 `signature` 字段带出。
    #[test]
    fn decode_records_signature_into_two_tier_cache() {
        let session = "sess-write-two-tier";
        let (decoded, _) = decode_json(
            serde_json::json!({
                "candidates": [{ "content": { "parts": [{
                    "functionCall": { "id": "call-write-1", "name": "read_file", "args": {"p": "a"} },
                    "thoughtSignature": "sig-real-1"
                }]}}]
            }),
            session,
        );
        let Decoded::Chunk(chunk) = decoded else {
            panic!("应产出 chunk");
        };
        let calls = chunk.delta.tool_calls.expect("应带 tool_calls 增量");
        assert_eq!(calls[0].signature.as_deref(), Some("sig-real-1"));
        assert_eq!(
            super::super::antigravity_signature_cache::lookup_call(session, "call-write-1")
                .as_deref(),
            Some("sig-real-1"),
            "主键档（session + call id）必须命中"
        );
        assert_eq!(
            super::super::antigravity_signature_cache::lookup_for_session(session, "read_file")
                .as_deref(),
            Some("sig-real-1"),
            "兜底档（session + 函数名）必须命中"
        );
    }

    /// 签名独占一个 part / 一个事件时，要暂存给紧随其后的 functionCall。
    #[test]
    fn decode_pending_signature_on_separate_part() {
        let session = "sess-write-pending";
        let (_, pending) = decode_json(
            serde_json::json!({
                "candidates": [{ "content": { "parts": [
                    { "thoughtSignature": "sig-pending-1" }
                ]}}]
            }),
            session,
        );
        let mut pending = pending;
        assert_eq!(pending.as_deref(), Some("sig-pending-1"));
        let decoded = decode_event(
            &SseEvent::Data(
                serde_json::json!({
                    "candidates": [{ "content": { "parts": [{
                        "functionCall": { "id": "call-pending-1", "name": "write_file", "args": {} }
                    }]}}]
                })
                .to_string(),
            ),
            "id",
            "gemini-pro-agent",
            session,
            &mut pending,
        );
        let Decoded::Chunk(chunk) = decoded else {
            panic!("应产出 chunk");
        };
        let calls = chunk.delta.tool_calls.expect("应带 tool_calls 增量");
        assert_eq!(calls[0].signature.as_deref(), Some("sig-pending-1"));
        assert!(pending.is_none(), "签名只能被消费一次");
    }

    /// 请求构造点（build_envelope）：同 session 回填缓存签名；缓存未命中
    /// 时保持「客户端带回的签名」原样。
    #[test]
    fn envelope_fills_cached_signature_for_same_session() {
        let mut assistant = user("调工具");
        assistant.role = Role::Assistant;
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call-fill-1".into(),
            function_name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        let probe = build_envelope(&format(vec![user("查文件"), assistant.clone()]));
        let session = probe["sessionId"].as_str().unwrap().to_string();
        // 用 build_envelope 派生出的同 session 键写缓存
        super::super::antigravity_signature_cache::remember_call(
            &session,
            "call-fill-1",
            "sig-fill-1",
        );
        let envelope = build_envelope(&format(vec![user("查文件"), assistant]));
        let parts = &envelope["request"]["contents"][1]["parts"];
        // parts[0] 是 text part，parts[1] 是带签名的 functionCall part
        assert_eq!(parts[1]["thoughtSignature"], "sig-fill-1");
    }
}
