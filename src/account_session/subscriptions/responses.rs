//! Chat Completions ⇄ OpenAI **Responses** 协议的最小转换（纯函数、无 IO）。
//!
//! 订阅型上游（chatgpt-subscription / codex-subscription）只认 Responses
//! 协议（`POST .../responses`），而 AIGX 数据面统一走 `ChatFormat`。本模块
//! 负责：
//! - [`chat_to_responses`]：`ChatFormat` → Responses 请求体；
//! - [`responses_to_chat`]：非流式 Responses JSON → `ChatResponse`；
//! - [`sse_event_to_chunks`]：Responses SSE 事件 → 若干 `ChatChunk`。
//!
//! 取向与 Agent2API 一致：**不编造**。未知的输出项 / 工具类型直接跳过，
//! 不静默改写用户语义；`store` 恒为 `false`（订阅通道不保存服务端历史，
//! 需在 `input` 里发完整历史）。
//!
//! 说明：本模块只处理「订阅数据面」这条线所需的子集，不复用
//! `api::openai::responses_convert`（那是**入站** Responses→Chat 的方向，
//! 与这里相反）。

use serde_json::{json, Map, Value};

use crate::bridge::{
    ChatChunk, ChatDelta, ChatFormat, ChatMessage, ChatResponse, FinishReason, Role, ToolCall,
    ToolCallDelta, UsageStats,
};

/// 取一条消息的纯文本（优先 typed content blocks 里的文本块）。
fn message_text(msg: &ChatMessage) -> String {
    if let Some(blocks) = &msg.content_blocks {
        let mut out = String::new();
        for block in blocks {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            if kind.contains("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
        // 有文本块就返回；纯图片块（无文本）时回退到 content 字符串。
        if !out.is_empty() || msg.content.is_none() {
            return out;
        }
    }
    msg.content_str().to_string()
}

/// OpenAI Chat 工具定义 → Responses 工具定义；非 `function` 工具丢弃。
fn map_tool(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    let name = function.get("name")?.as_str()?;
    let mut out = Map::new();
    out.insert("type".to_string(), json!("function"));
    out.insert("name".to_string(), json!(name));
    if let Some(description) = function.get("description").and_then(Value::as_str) {
        out.insert("description".to_string(), json!(description));
    }
    if let Some(parameters) = function.get("parameters") {
        out.insert("parameters".to_string(), parameters.clone());
    }
    Some(Value::Object(out))
}

/// `tool_choice` 两种写法都保留：字符串原样，`{function:{name}}` 翻成
/// Responses 的 `{type:"function", name}`。
fn map_tool_choice(choice: &Value) -> Option<Value> {
    if let Some(text) = choice.as_str() {
        return Some(json!(text));
    }
    let name = choice.pointer("/function/name").and_then(Value::as_str)?;
    Some(json!({ "type": "function", "name": name }))
}

/// `ChatFormat` → Responses 请求体。
///
/// `stream` 由调用方按入口决定（非流式入口传 false）。
pub fn chat_to_responses(req: &ChatFormat, stream: bool) -> Value {
    let mut instructions = String::new();
    let mut input: Vec<Value> = Vec::new();
    for msg in &req.messages {
        match msg.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&message_text(msg));
            }
            Role::User => input.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": message_text(msg) }],
            })),
            Role::Assistant => {
                let text = message_text(msg);
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                if let Some(calls) = &msg.tool_calls {
                    for call in calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.id.clone(),
                            "name": call.function_name.clone(),
                            "arguments": call.arguments.clone(),
                        }));
                    }
                }
            }
            Role::Tool => input.push(json!({
                "type": "function_call_output",
                "call_id": msg.tool_call_id.clone().unwrap_or_default(),
                "output": message_text(msg),
            })),
        }
    }

    let mut body = Map::new();
    body.insert("model".to_string(), json!(req.model.clone()));
    body.insert("input".to_string(), Value::Array(input));
    body.insert("stream".to_string(), json!(stream));
    // 订阅通道不保存服务端历史：每次都发完整 input。
    body.insert("store".to_string(), json!(false));
    if !instructions.is_empty() {
        body.insert("instructions".to_string(), json!(instructions));
    }
    if let Some(max_tokens) = req.max_tokens {
        body.insert("max_output_tokens".to_string(), json!(max_tokens));
    }
    if let Some(temperature) = req.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        body.insert("top_p".to_string(), json!(top_p));
    }
    if let Some(effort) = req.reasoning_effort.as_deref().filter(|e| !e.is_empty()) {
        body.insert("reasoning".to_string(), json!({ "effort": effort }));
    }
    if let Some(choice) = req.tool_choice.as_ref().and_then(map_tool_choice) {
        body.insert("tool_choice".to_string(), choice);
    }
    let mut tools: Vec<Value> = req
        .tools
        .as_deref()
        .map(|list| list.iter().filter_map(map_tool).collect())
        .unwrap_or_default();
    // OpenAI `web_search_options` → Responses 内置 `web_search` 工具。
    if req.web_search_options.is_some() {
        tools.push(json!({ "type": "web_search" }));
    }
    if !tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(tools));
    }
    Value::Object(body)
}

/// 解析 Responses usage（`input_tokens` / `output_tokens` 口径）。
fn parse_usage(usage: Option<&Value>) -> UsageStats {
    let Some(usage) = usage else {
        return UsageStats::default();
    };
    let prompt = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completion = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt.saturating_add(completion));
    let cached = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_prompt_tokens: cached,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 判定 finish reason（工具调用优先；`incomplete` 且原因是长度 → Length）。
fn finish_reason(value: &Value, has_tools: bool) -> FinishReason {
    if has_tools {
        return FinishReason::ToolCalls;
    }
    if value.get("status").and_then(Value::as_str) == Some("incomplete") {
        let reason = value
            .pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("");
        if reason.contains("max") || reason.contains("token") {
            return FinishReason::Length;
        }
    }
    FinishReason::Stop
}

/// 非流式 Responses JSON → `ChatResponse`。
pub fn responses_to_chat(value: &Value, fallback_model: &str) -> Result<ChatResponse, String> {
    if value.get("error").is_some_and(|e| !e.is_null()) {
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("订阅上游返回错误");
        return Err(message.to_string());
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or(fallback_model)
        .to_string();
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(output) = value.get("output").and_then(Value::as_array) {
        for item in output {
            match item.get("type").and_then(Value::as_str).unwrap_or("") {
                "message" => {
                    if let Some(parts) = item.get("content").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                content.push_str(text);
                            } else if let Some(refusal) =
                                part.get("refusal").and_then(Value::as_str)
                            {
                                content.push_str(refusal);
                            }
                        }
                    }
                }
                "reasoning" => {
                    if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(Value::as_str) {
                                reasoning.push_str(text);
                            }
                        }
                    }
                }
                "function_call" => {
                    let call_id = item
                        .get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = item
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let arguments = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    tool_calls.push(ToolCall {
                        id: call_id,
                        function_name: name,
                        arguments,
                    });
                }
                _ => {}
            }
        }
    }
    let finish = finish_reason(value, !tool_calls.is_empty());
    let usage = parse_usage(value.get("usage"));
    let message = ChatMessage {
        role: Role::Assistant,
        content: (!content.is_empty()).then_some(content),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        reasoning: (!reasoning.is_empty()).then_some(reasoning),
    };
    Ok(ChatResponse {
        id,
        model,
        message,
        finish_reason: finish,
        usage,
    })
}

/// 流式解析的跨事件状态（id/model 从首个事件补全；跟踪是否出现过工具调用）。
#[derive(Default)]
pub struct StreamState {
    pub id: String,
    pub model: String,
    pub tool_seen: bool,
    pub finished: bool,
}

/// Responses SSE 事件 → 若干 `ChatChunk`（可能为空 = 该事件无客户端可见增量）。
pub fn sse_event_to_chunks(event: &Value, state: &mut StreamState) -> Vec<ChatChunk> {
    let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "response.created" => {
            if let Some(response) = event.get("response") {
                if let Some(id) = response.get("id").and_then(Value::as_str) {
                    state.id = id.to_string();
                }
                if let Some(model) = response.get("model").and_then(Value::as_str) {
                    state.model = model.to_string();
                }
            }
            Vec::new()
        }
        "response.output_text.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            if delta.is_empty() {
                return Vec::new();
            }
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta {
                    content: Some(delta.to_string()),
                    ..Default::default()
                },
                finish_reason: None,
                usage: None,
            }]
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            if delta.is_empty() {
                return Vec::new();
            }
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta {
                    reasoning: Some(delta.to_string()),
                    ..Default::default()
                },
                finish_reason: None,
                usage: None,
            }]
        }
        "response.output_item.added" => {
            let item = event.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) != Some("function_call") {
                return Vec::new();
            }
            state.tool_seen = true;
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let call_id = item
                .get("call_id")
                .or_else(|| item.get("id"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta {
                    tool_calls: Some(vec![ToolCallDelta {
                        index,
                        id: Some(call_id),
                        function_name: Some(name),
                        arguments: Some(String::new()),
                        signature: None,
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
                usage: None,
            }]
        }
        "response.function_call_arguments.delta" => {
            let index = event
                .get("output_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let delta = event.get("delta").and_then(Value::as_str).unwrap_or("");
            if delta.is_empty() {
                return Vec::new();
            }
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta {
                    tool_calls: Some(vec![ToolCallDelta {
                        index,
                        id: None,
                        function_name: None,
                        arguments: Some(delta.to_string()),
                        signature: None,
                    }]),
                    ..Default::default()
                },
                finish_reason: None,
                usage: None,
            }]
        }
        "response.completed" => {
            state.finished = true;
            let response = event.get("response").unwrap_or(&Value::Null);
            let usage = parse_usage(response.get("usage"));
            let finish = finish_reason(response, state.tool_seen);
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta::default(),
                finish_reason: Some(finish),
                usage: Some(usage),
            }]
        }
        "response.incomplete" => {
            state.finished = true;
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta::default(),
                finish_reason: Some(FinishReason::Length),
                usage: None,
            }]
        }
        "response.failed" => {
            state.finished = true;
            vec![ChatChunk {
                id: state.id.clone(),
                model: state.model.clone(),
                delta: ChatDelta::default(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
            }]
        }
        _ => Vec::new(),
    }
}

/// 把一个 SSE 字节流（`reqwest::Response::bytes_stream`）翻译成 `ChatChunkStream`。
///
/// 供订阅型与自定义 `responses` 协议共用：两者都是「POST /responses →
/// text/event-stream → ChatChunk」。行缓冲与事件解析都在这里，避免各写一份。
pub fn stream_from_bytes<S>(inner: S) -> crate::bridge::ChatChunkStream
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures::StreamExt;

    struct Flow {
        inner: futures::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
        buffer: String,
        sse: StreamState,
        pending: std::collections::VecDeque<ChatChunk>,
        done: bool,
    }

    let flow = Flow {
        inner: inner.boxed(),
        buffer: String::new(),
        sse: StreamState::default(),
        pending: std::collections::VecDeque::new(),
        done: false,
    };
    futures::stream::try_unfold(flow, |mut flow| async move {
        loop {
            if let Some(chunk) = flow.pending.pop_front() {
                return Ok(Some((chunk, flow)));
            }
            if flow.done {
                return Ok(None);
            }
            if let Some(block) = take_sse_block(&mut flow.buffer) {
                for payload in sse_payloads(&block) {
                    if payload.trim() == "[DONE]" {
                        flow.done = true;
                        continue;
                    }
                    if let Ok(event) = serde_json::from_str::<Value>(&payload) {
                        for chunk in sse_event_to_chunks(&event, &mut flow.sse) {
                            flow.pending.push_back(chunk);
                        }
                        if flow.sse.finished {
                            flow.done = true;
                        }
                    }
                }
                continue;
            }
            match flow.inner.next().await {
                Some(Ok(bytes)) => flow.buffer.push_str(&String::from_utf8_lossy(&bytes)),
                Some(Err(error)) => {
                    return Err(crate::bridge::BridgeError::Transport(error.to_string()))
                }
                None => return Ok(None),
            }
        }
    })
    .boxed()
}

/// 从缓冲里取出一个完整 SSE 块（`\n\n` 或 `\r\n\r\n` 分隔）。
fn take_sse_block(buffer: &mut String) -> Option<String> {
    let bytes = buffer.as_bytes();
    let mut split: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            split = Some((i, 2));
            break;
        }
        if i + 3 < bytes.len() && &bytes[i..i + 4] == b"\r\n\r\n" {
            split = Some((i, 4));
            break;
        }
        i += 1;
    }
    let (pos, len) = split?;
    let block = buffer[..pos].to_string();
    buffer.replace_range(..pos + len, "");
    Some(block)
}

/// 取一个 SSE 块里的 `data:` 载荷（多行用 `\n` 拼接）。
fn sse_payloads(block: &str) -> Vec<String> {
    let mut payload = String::new();
    for line in block.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            if !payload.is_empty() {
                payload.push('\n');
            }
            payload.push_str(rest.trim_start());
        }
    }
    if payload.is_empty() {
        Vec::new()
    } else {
        vec![payload]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    #[test]
    fn chat_to_responses_maps_system_to_instructions_and_store_false() {
        let req = ChatFormat {
            model: "gpt-5".to_string(),
            messages: vec![ChatMessage::system("be brief"), user("hi")],
            tools: None,
            max_tokens: Some(256),
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: Some("high".to_string()),
            web_search_options: None,
            extra: None,
        };
        let body = chat_to_responses(&req, false);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], false);
        assert_eq!(body["max_output_tokens"], 256);
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["input"][0]["role"], "user");
        assert_eq!(body["input"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn responses_to_chat_extracts_text_tools_and_usage() {
        let value = json!({
            "id": "resp_1", "model": "gpt-5", "status": "completed",
            "output": [
                {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "hello"}]},
                {"type": "function_call", "call_id": "call_1", "name": "f", "arguments": "{}"}
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
        });
        let resp = responses_to_chat(&value, "gpt-5").unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("hello"));
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        let calls = resp.message.tool_calls.unwrap();
        assert_eq!(calls[0].function_name, "f");
        assert_eq!(calls[0].id, "call_1");
    }

    #[test]
    fn sse_delta_and_completion_produce_chunks() {
        let mut state = StreamState::default();
        let created =
            json!({"type": "response.created", "response": {"id": "r1", "model": "gpt-5"}});
        assert!(sse_event_to_chunks(&created, &mut state).is_empty());
        assert_eq!(state.id, "r1");
        let delta = json!({"type": "response.output_text.delta", "delta": "hi"});
        let chunks = sse_event_to_chunks(&delta, &mut state);
        assert_eq!(chunks[0].delta.content.as_deref(), Some("hi"));
        let done = json!({"type": "response.completed", "response": {"status": "completed", "usage": {"input_tokens": 1, "output_tokens": 2}}});
        let chunks = sse_event_to_chunks(&done, &mut state);
        assert_eq!(chunks[0].finish_reason, Some(FinishReason::Stop));
        assert!(state.finished);
    }
}
