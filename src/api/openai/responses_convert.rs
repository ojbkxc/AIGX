//! `/v1/responses` ↔ Chat Completions 的入站/出站转换（纯函数、无 IO）。
//!
//! 背景：`Bridge::responses_passthrough` 的默认实现对不认 Responses 上游的
//! 桥（qoder / trae / cmdcode … 等 session 型渠道）返回
//! `BridgeError::Config`，导致 `/v1/responses` 在这些渠道上直接失败。
//! `responses.rs` 的 failover 循环会在收到该 Config 错误时降级到本模块：
//! 把客户端 Responses 请求翻成 Chat 请求，走 `Bridge::chat` / `chat_stream`，
//! 再把 Chat 响应翻回 Responses 形状。
//!
//! 冻结契约（SPEC-1.0.49 §3）：
//!   - [`request_to_chat`]：Responses 请求体 → `ChatFormat`
//!   - [`chat_to_response`]：Chat 非流式响应 → Responses `response` 对象
//!   - [`chat_chunk_to_response_events`]：Chat 流式 chunk → 若干 Responses 事件
//!   - [`done_response_object`]：流式终态 `response.completed` 的 `response` 对象
//!
//! 命名说明：SPEC 原文写的入参类型是 `ChatStreamChunk`，本仓该项即
//! `crate::bridge::ChatChunk`（`ChatChunkStream` 的元素类型），此处按实际
//! 类型落签，语义一致。
//!
//! 有意收窄的部分（不静默编造）：`Reasoning` / 其它未知 `input[]` 项、
//! 非 `function` 的顶层 `tools` 条目一律丢弃并 `tracing::debug!` 记类型；
//! 连续的多条 assistant 项会合并成一条（避免严格上游拒收相邻 assistant）。

use crate::bridge::{ChatChunk, ChatFormat, ChatMessage, ChatResponse, Role, ToolCall, UsageStats};
use serde_json::{json, Value};

/// 一次入站转换的结果：Chat 请求体 + 客户端是否要流式。
///
/// `stream` 与 `chat.stream` 同值；单独带出是方便调用方在构造
/// `Bridge::chat` / `chat_stream` 前就地读取。
#[derive(Debug, Clone)]
pub(crate) struct ChatFromResponses {
    pub chat: ChatFormat,
    pub stream: bool,
}

/// Responses 请求体 → `ChatFormat`。
///
/// 缺 `model` **不兜底**：返回 `Err`（调用方据此回 400），避免把无模型名的
/// 请求误发到上游。`input` 必须是字符串或 item 数组，否则同样 `Err`。
pub(crate) fn request_to_chat(body: &Value) -> Result<ChatFromResponses, String> {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| "missing model field".to_string())?;

    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let mut messages: Vec<ChatMessage> = Vec::new();
    // instructions（字符串）→ 首条 system 消息
    if let Some(instructions) = body.get("instructions").and_then(Value::as_str) {
        if !instructions.trim().is_empty() {
            messages.push(text_message(Role::System, instructions));
        }
    }
    match body.get("input") {
        Some(Value::String(text)) => messages.push(text_message(Role::User, text)),
        Some(Value::Array(items)) => {
            for item in items {
                push_input_item(&mut messages, item);
            }
        }
        Some(Value::Object(_)) => {
            // 少数客户端直接给单个 message 对象（非数组）也接受
            if let Some(item) = body.get("input") {
                push_input_item(&mut messages, item);
            }
        }
        _ => return Err("input field is required".to_string()),
    }
    let messages = merge_successive_assistants(messages);

    // tools：只认 `type == "function"`；其它类型（web_search / file_search 等
    // 宿主工具）在本网关的 Chat 出口没有对应表达，丢弃并记录类型，不编造。
    let tools = body
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(tool_to_chat).collect::<Vec<_>>())
        .filter(|v| !v.is_empty());

    // 透传给上游的额外顶层字段：ChatFormat 无对应槽位的 parallel_tool_calls
    // 与 metadata 原样合并进上游请求体。
    let mut extra = serde_json::Map::new();
    if let Some(value) = body.get("parallel_tool_calls").filter(|v| !v.is_null()) {
        extra.insert("parallel_tool_calls".to_string(), value.clone());
    }
    if let Some(value) = body.get("metadata").filter(|v| !v.is_null()) {
        extra.insert("metadata".to_string(), value.clone());
    }

    let chat = ChatFormat {
        model: model.to_string(),
        messages,
        tools,
        max_tokens: body
            .get("max_output_tokens")
            .and_then(Value::as_u64)
            .map(|v| v.min(u32::MAX as u64) as u32),
        temperature: body.get("temperature").and_then(Value::as_f64),
        top_p: body.get("top_p").and_then(Value::as_f64),
        stream,
        top_k: None,
        stop: None,
        tool_choice: body
            .get("tool_choice")
            .filter(|v| !v.is_null())
            .map(tool_choice_to_chat),
        reasoning_effort: body
            .pointer("/reasoning/effort")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        web_search_options: None,
        extra: if extra.is_empty() {
            None
        } else {
            Some(Value::Object(extra))
        },
    };
    Ok(ChatFromResponses { chat, stream })
}

/// 一个 `input[]` 项 → 零到一条 Chat 消息（追加进 `messages`）。
fn push_input_item(messages: &mut Vec<ChatMessage>, item: &Value) {
    if let Some(text) = item.as_str() {
        messages.push(text_message(Role::User, text));
        return;
    }
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match kind {
        "message" => {
            let role = match item
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .to_ascii_lowercase()
                .as_str()
            {
                "system" | "developer" => Role::System,
                "assistant" => Role::Assistant,
                "tool" => Role::Tool,
                _ => Role::User,
            };
            let content = item.get("content").or_else(|| item.get("text"));
            let (text, blocks) = content_to_chat(content);
            let mut message = ChatMessage {
                role,
                content: text,
                content_blocks: blocks,
                name: item.get("name").and_then(Value::as_str).map(str::to_string),
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            };
            if role == Role::Tool {
                message.tool_call_id = item
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            messages.push(message);
        }
        // 历史里的工具调用：还原成带 tool_calls 的 assistant 消息，
        // 否则多轮对话里模型看不到自己上一步调过什么。
        "function_call" => {
            let call_id = call_id_of(item);
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let arguments = arguments_of(item.get("arguments"));
            messages.push(ChatMessage {
                role: Role::Assistant,
                content: None,
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: call_id,
                    function_name: name,
                    arguments,
                }]),
                reasoning: None,
            });
        }
        // 工具结果：`output` 为字符串或文本块数组 → tool 消息的 content。
        "function_call_output" => {
            let call_id = call_id_of(item);
            let text = output_text(item.get("output"));
            messages.push(ChatMessage {
                role: Role::Tool,
                content: Some(text),
                content_blocks: None,
                name: None,
                tool_call_id: Some(call_id),
                tool_calls: None,
                reasoning: None,
            });
        }
        // 裸内容块：包成一条 user 消息
        "input_text" | "output_text" | "text" => {
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            messages.push(text_message(Role::User, text));
        }
        "input_image" | "image_url" => {
            let (text, blocks) = content_to_chat(Some(&Value::Array(vec![item.clone()])));
            messages.push(ChatMessage {
                role: Role::User,
                content: text,
                content_blocks: blocks,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            });
        }
        other => {
            tracing::debug!("responses_convert: dropping unsupported input item type '{other}'");
        }
    }
}

/// 构造一条纯文本消息（bare-string content，不带 content_blocks）。
fn text_message(role: Role, text: &str) -> ChatMessage {
    ChatMessage {
        role,
        content: Some(text.to_string()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    }
}

/// Responses 消息 `content` → `(纯文本, content_blocks)`。
///
/// 字符串 → bare-string content（不换形状）；数组 → 逐块映射：
/// `text` / `input_text` / `output_text` → `{"type":"text","text":...}`，
/// `input_image` / `image_url` → `{"type":"image_url","image_url":{"url":...}}`，
/// 未知块丢弃并 debug。文本同时拼进 `content`（无文本则 `None`）。
fn content_to_chat(content: Option<&Value>) -> (Option<String>, Option<Vec<Value>>) {
    match content {
        None | Some(Value::Null) => (None, None),
        Some(Value::String(s)) => (Some(s.clone()), None),
        Some(Value::Array(parts)) => {
            let mut texts: Vec<String> = Vec::new();
            let mut blocks: Vec<Value> = Vec::new();
            for part in parts {
                if let Some(s) = part.as_str() {
                    texts.push(s.to_string());
                    blocks.push(json!({ "type": "text", "text": s }));
                    continue;
                }
                match part.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text" | "input_text" | "output_text" => {
                        let text = part.get("text").and_then(Value::as_str).unwrap_or("");
                        if !text.is_empty() {
                            texts.push(text.to_string());
                        }
                        blocks.push(json!({ "type": "text", "text": text }));
                    }
                    "input_image" | "image_url" => {
                        blocks.push(json!({
                            "type": "image_url",
                            "image_url": { "url": image_url_of(part) },
                        }));
                    }
                    other => {
                        tracing::debug!(
                            "responses_convert: dropping unsupported content block type '{other}'"
                        );
                    }
                }
            }
            let text = if texts.is_empty() {
                None
            } else {
                Some(texts.join("\n"))
            };
            let blocks = if blocks.is_empty() {
                None
            } else {
                Some(blocks)
            };
            (text, blocks)
        }
        Some(other) => (Some(other.to_string()), None),
    }
}

/// 图片块的 url（`image_url` 可为 URL 字符串、`{url}` 对象，或块自带 `url`）。
fn image_url_of(part: &Value) -> String {
    match part.get("image_url") {
        Some(Value::String(url)) => url.clone(),
        Some(Value::Object(_)) => part
            .pointer("/image_url/url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => part
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    }
}

/// 工具调用的 call id：`call_id` 优先，退到 `id`，都没有则自造一个。
fn call_id_of(item: &Value) -> String {
    item.get("call_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| item.get("id").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4().simple()))
}

/// 工具调用的 `arguments`（字符串原样；对象等结构化值序列化；缺省 `{}`）。
fn arguments_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Null) | None => "{}".to_string(),
        Some(other) => other.to_string(),
    }
}

/// 工具结果 `output` → 文本（字符串 / 文本块数组 / 其它拍平）。
fn output_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                p.as_str()
                    .map(str::to_string)
                    .or_else(|| p.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Responses `tools[]` → OpenAI Chat 工具（扁平 → 嵌套 `function`）。
///
/// `type == "function"`（或未标 type 但带 name）才转换；其它类型返回 `None`
/// 并由调用方 `filter_map` 丢弃（已在此记 debug）。
fn tool_to_chat(tool: &Value) -> Option<Value> {
    let kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
    if !kind.is_empty() && kind != "function" {
        tracing::debug!("responses_convert: dropping non-function tool type '{kind}'");
        return None;
    }
    // 已是 OpenAI 嵌套形态：原样保留
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut function = serde_json::Map::new();
    function.insert("name".to_string(), Value::String(name.to_string()));
    for key in ["description", "parameters", "strict"] {
        if let Some(value) = tool.get(key).filter(|v| !v.is_null()) {
            function.insert(key.to_string(), value.clone());
        }
    }
    Some(json!({ "type": "function", "function": Value::Object(function) }))
}

/// Responses `tool_choice` → Chat `tool_choice`（扁平 `{type,name}` → 嵌套）。
fn tool_choice_to_chat(choice: &Value) -> Value {
    if let Some(name) = choice.get("name").and_then(Value::as_str) {
        if choice.get("function").is_none() {
            return json!({ "type": "function", "function": { "name": name } });
        }
    }
    choice.clone()
}

/// 合并**连续**的 assistant 消息（content 换行拼接、tool_calls 相加、
/// reasoning 取先到者）。严格上游（DeepSeek thinking）拒收相邻 assistant，
/// 而 Codex 走 Responses 时一轮常派出多条平级 assistant 项。
fn merge_successive_assistants(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for message in messages {
        let can_merge = message.role == Role::Assistant
            && out
                .last()
                .map(|m| m.role == Role::Assistant)
                .unwrap_or(false);
        if !can_merge {
            out.push(message);
            continue;
        }
        let Some(last) = out.last_mut() else {
            out.push(message);
            continue;
        };
        match (&last.content, &message.content) {
            (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => {
                last.content = Some(format!("{a}\n{b}"));
            }
            (None, Some(b)) => last.content = Some(b.clone()),
            _ => {}
        }
        if let Some(calls) = message.tool_calls {
            match &mut last.tool_calls {
                Some(existing) => existing.extend(calls),
                None => last.tool_calls = Some(calls),
            }
        }
        if last.reasoning.is_none() {
            last.reasoning = message.reasoning;
        }
        if last.content_blocks.is_none() {
            last.content_blocks = message.content_blocks;
        }
    }
    out
}

/// Chat 非流式响应 → Responses `response` 对象（SPEC §3.5）。
///
/// `reasoning` 存在时附 `{"type":"reasoning",…}` 于 `output` **首位**；
/// 随后是 message 项（无工具调用或正文非空时）、每个工具调用一项。
/// `model` 由调用方传入（客户端可见的模型名）。
pub(crate) fn chat_to_response(resp: &ChatResponse, model: &str) -> Value {
    let mut output: Vec<Value> = Vec::new();
    if let Some(reasoning) = resp.message.reasoning.as_deref().filter(|r| !r.is_empty()) {
        output.push(json!({
            "type": "reasoning",
            "id": format!("rs-{}", uuid::Uuid::new_v4().simple()),
            "summary": [{ "type": "summary_text", "text": reasoning }],
        }));
    }
    let text = resp.message.content_str();
    let tool_calls = resp.message.tool_calls.as_deref().unwrap_or(&[]);
    if !text.is_empty() || tool_calls.is_empty() {
        output.push(json!({
            "type": "message",
            "id": format!("msg-{}", uuid::Uuid::new_v4().simple()),
            "role": "assistant",
            "status": "completed",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        }));
    }
    for call in tool_calls {
        let arguments = if call.arguments.is_empty() {
            "{}".to_string()
        } else {
            call.arguments.clone()
        };
        output.push(json!({
            "type": "function_call",
            "id": format!("fc-{}", uuid::Uuid::new_v4().simple()),
            "call_id": call.id,
            "name": call.function_name,
            "arguments": arguments,
        }));
    }
    let (input_tokens, _cached) = resp.usage.billing_tokens();
    let output_tokens = resp.usage.completion_tokens;
    json!({
        "id": format!("resp-{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "status": "completed",
        "output": output,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    })
}

/// 把一批 Responses 事件对象序列化成 SSE 帧 `event: <type>\ndata: <json>\n\n`。
pub(crate) fn sse_frame(event: &Value) -> String {
    let kind = event
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    format!("event: {kind}\ndata: {event}\n\n")
}

/// 一个 Chat 流式 chunk → 零到多个 Responses 事件（SPEC §3.6）。
///
/// `seq` 是跨 chunk 单调递增的事件序号计数器（同时充当「本流是否已发过
/// `response.created`」的状态：`*seq == 0` 即尚未发过）。每个事件带
/// `type` 与 `sequence_number`。终态 `response.completed` 在 chunk 带
/// `finish_reason` 时发出。item id 由 `chunk.id`（上游响应 id，全流稳定）
/// 派生，保证同一 item 的 `output_item.added` 与后续 delta 引用一致。
pub(crate) fn chat_chunk_to_response_events(chunk: &ChatChunk, seq: &mut u64) -> Vec<Value> {
    let mut events: Vec<Value> = Vec::new();
    let response_id = format!("resp-{}", chunk.id);
    let message_id = format!("msg-{}", chunk.id);

    if *seq == 0 {
        events.push(next_event(
            seq,
            "response.created",
            json!({
                "response": {
                    "id": response_id,
                    "object": "response",
                    "created_at": chrono::Utc::now().timestamp(),
                    "model": chunk.model,
                    "status": "in_progress",
                    "output": [],
                },
            }),
        ));
        events.push(next_event(
            seq,
            "response.output_item.added",
            json!({
                "output_index": 0,
                "item": {
                    "type": "message",
                    "id": message_id,
                    "status": "in_progress",
                    "role": "assistant",
                    "content": [],
                },
            }),
        ));
    }
    if let Some(reasoning) = chunk.delta.reasoning.as_deref().filter(|r| !r.is_empty()) {
        events.push(next_event(
            seq,
            "response.reasoning_summary_text.delta",
            json!({
                "output_index": 0,
                "item_id": format!("rs-{}", chunk.id),
                "summary_index": 0,
                "delta": reasoning,
            }),
        ));
    }
    if let Some(text) = chunk.delta.content.as_deref().filter(|t| !t.is_empty()) {
        events.push(next_event(
            seq,
            "response.output_text.delta",
            json!({
                "output_index": 0,
                "item_id": message_id,
                "content_index": 0,
                "delta": text,
            }),
        ));
    }
    if let Some(calls) = &chunk.delta.tool_calls {
        for call in calls {
            let call_id = call
                .id
                .clone()
                .unwrap_or_else(|| format!("call-{}-{}", chunk.id, call.index));
            let item_id = format!("fc-{call_id}");
            if call.function_name.is_some() {
                events.push(next_event(
                    seq,
                    "response.output_item.added",
                    json!({
                        "output_index": 1 + call.index,
                        "item": {
                            "type": "function_call",
                            "id": item_id,
                            "status": "in_progress",
                            "call_id": call_id,
                            "name": call.function_name,
                            "arguments": "",
                        },
                    }),
                ));
            }
            if let Some(arguments) = call.arguments.as_deref().filter(|a| !a.is_empty()) {
                events.push(next_event(
                    seq,
                    "response.function_call_arguments.delta",
                    json!({
                        "output_index": 1 + call.index,
                        "item_id": item_id,
                        "call_id": call_id,
                        "delta": arguments,
                    }),
                ));
            }
        }
    }
    if chunk.finish_reason.is_some() {
        // 终态 response 的 id 与开头 response.created 保持一致（客户端按 id 对号）
        let mut response = done_response_object(&chunk.model, chunk.usage.as_ref());
        if let Some(map) = response.as_object_mut() {
            map.insert("id".to_string(), Value::String(response_id.clone()));
        }
        events.push(next_event(
            seq,
            "response.completed",
            json!({ "response": response }),
        ));
    }
    events
}

/// 流式终态 `response.completed` 的 `response` 对象（SPEC §3.6）。
///
/// `usage` 为 `None` 时 `usage` 记 `null`（未拿到权威量，不编造 0）。
pub(crate) fn done_response_object(model: &str, usage: Option<&UsageStats>) -> Value {
    let usage_value = match usage {
        Some(usage) => {
            let (input_tokens, _cached) = usage.billing_tokens();
            let output_tokens = usage.completion_tokens;
            json!({
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "total_tokens": input_tokens + output_tokens,
            })
        }
        None => Value::Null,
    };
    json!({
        "id": format!("resp-{}", uuid::Uuid::new_v4().simple()),
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "status": "completed",
        "output": [],
        "usage": usage_value,
    })
}

/// 构造一个带 `type` / `sequence_number` 的事件对象并推进序号。
fn next_event(seq: &mut u64, kind: &str, mut data: Value) -> Value {
    let current = *seq;
    *seq += 1;
    if let Some(map) = data.as_object_mut() {
        map.insert("type".to_string(), Value::String(kind.to_string()));
        map.insert("sequence_number".to_string(), Value::from(current));
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatDelta, FinishReason};
    use serde_json::{json, Value};

    fn assistant_message(
        content: Option<&str>,
        reasoning: Option<&str>,
        tool_calls: Option<Vec<ToolCall>>,
    ) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: content.map(str::to_string),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls,
            reasoning: reasoning.map(str::to_string),
        }
    }

    fn chunk_with(content: Option<&str>, finish: Option<FinishReason>) -> ChatChunk {
        ChatChunk {
            id: "chatcmpl-test".to_string(),
            model: "m1".to_string(),
            delta: ChatDelta {
                content: content.map(str::to_string),
                tool_calls: None,
                reasoning: None,
            },
            finish_reason: finish,
            usage: None,
        }
    }

    #[test]
    fn request_maps_input_string_and_instructions() {
        let body = json!({
            "model": "m1",
            "instructions": "be nice",
            "input": "hello",
            "stream": true,
            "max_output_tokens": 128,
            "temperature": 0.2,
            "top_p": 0.9,
        });
        let out = request_to_chat(&body).expect("应当转换成功");
        assert!(out.stream, "stream 应为 true");
        assert_eq!(out.chat.model, "m1");
        assert_eq!(out.chat.messages.len(), 2);
        assert_eq!(out.chat.messages[0].role, Role::System);
        assert_eq!(out.chat.messages[0].content.as_deref(), Some("be nice"));
        assert_eq!(out.chat.messages[1].role, Role::User);
        assert_eq!(out.chat.messages[1].content.as_deref(), Some("hello"));
        // 纯文本 message 仍是 bare-string：不误加 content_blocks
        assert!(out.chat.messages[1].content_blocks.is_none());
        assert_eq!(out.chat.max_tokens, Some(128));
        assert_eq!(out.chat.temperature, Some(0.2));
        assert_eq!(out.chat.top_p, Some(0.9));
    }

    #[test]
    fn request_maps_input_array_items_and_image_block() {
        let body = json!({
            "model": "m1",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        { "type": "input_text", "text": "look" },
                        { "type": "input_image", "image_url": "data:image/png;base64,AAA" },
                    ],
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"sf\"}",
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "sunny",
                },
            ],
        });
        let out = request_to_chat(&body).expect("应当转换成功");
        let user = &out.chat.messages[0];
        assert_eq!(user.role, Role::User);
        assert_eq!(user.content.as_deref(), Some("look"));
        let blocks = user
            .content_blocks
            .as_ref()
            .expect("图片应触发 content_blocks");
        assert_eq!(blocks[0], json!({ "type": "text", "text": "look" }));
        assert_eq!(
            blocks[1],
            json!({ "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } })
        );
        let assistant = &out.chat.messages[1];
        assert_eq!(assistant.role, Role::Assistant);
        let calls = assistant.tool_calls.as_ref().expect("应有 tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function_name, "get_weather");
        assert_eq!(calls[0].arguments, "{\"city\":\"sf\"}");
        let tool = &out.chat.messages[2];
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(tool.content.as_deref(), Some("sunny"));
    }

    #[test]
    fn request_maps_function_tools_and_effort() {
        let body = json!({
            "model": "m1",
            "input": "hi",
            "reasoning": { "effort": "high" },
            "parallel_tool_calls": false,
            "metadata": { "trace": "abc" },
            "tools": [
                {
                    "type": "function",
                    "name": "get_weather",
                    "description": "w",
                    "parameters": { "type": "object" },
                },
                { "type": "web_search" },
            ],
            "tool_choice": { "type": "function", "name": "get_weather" },
        });
        let out = request_to_chat(&body).expect("应当转换成功");
        assert_eq!(out.chat.reasoning_effort.as_deref(), Some("high"));
        let tools = out.chat.tools.as_ref().expect("应有工具");
        assert_eq!(tools.len(), 1, "非 function 工具应被丢弃");
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "get_weather");
        assert_eq!(tools[0]["function"]["description"], "w");
        assert_eq!(
            out.chat.tool_choice.as_ref().unwrap(),
            &json!({ "type": "function", "function": { "name": "get_weather" } })
        );
        let extra = out
            .chat
            .extra
            .as_ref()
            .expect("parallel_tool_calls 应进 extra");
        assert_eq!(extra["parallel_tool_calls"], json!(false));
        assert_eq!(extra["metadata"]["trace"], json!("abc"));
    }

    #[test]
    fn response_wraps_text_and_function_calls_with_reasoning_first() {
        let resp = ChatResponse {
            id: "chatcmpl-1".to_string(),
            model: "m1".to_string(),
            message: assistant_message(
                Some("hi there"),
                Some("thinking"),
                Some(vec![ToolCall {
                    id: "call_1".to_string(),
                    function_name: "get_weather".to_string(),
                    arguments: "{\"city\":\"sf\"}".to_string(),
                }]),
            ),
            finish_reason: FinishReason::ToolCalls,
            usage: UsageStats::new(10, 5),
        };
        let out = chat_to_response(&resp, "m1");
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["model"], "m1");
        assert!(out["id"].as_str().unwrap().starts_with("resp-"));
        let output = out["output"].as_array().expect("output 应为数组");
        // reasoning 在首位
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["summary"][0]["text"], "thinking");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[1]["content"][0]["type"], "output_text");
        assert_eq!(output[1]["content"][0]["text"], "hi there");
        assert_eq!(output[2]["type"], "function_call");
        assert_eq!(output[2]["call_id"], "call_1");
        assert_eq!(output[2]["name"], "get_weather");
        assert_eq!(output[2]["arguments"], "{\"city\":\"sf\"}");
        assert_eq!(out["usage"]["input_tokens"], 10);
        assert_eq!(out["usage"]["output_tokens"], 5);
        assert_eq!(out["usage"]["total_tokens"], 15);
    }

    #[test]
    fn stream_events_are_well_formed() {
        let chunks = vec![
            chunk_with(Some("Hel"), None),
            chunk_with(Some("lo"), None),
            chunk_with(None, Some(FinishReason::Stop)),
        ];
        let mut seq: u64 = 0;
        let mut events: Vec<Value> = Vec::new();
        for chunk in &chunks {
            events.extend(chat_chunk_to_response_events(chunk, &mut seq));
        }

        let kinds: Vec<&str> = events
            .iter()
            .map(|e| e["type"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(kinds.first(), Some(&"response.created"));
        assert!(kinds.contains(&"response.output_item.added"));
        assert_eq!(kinds.last(), Some(&"response.completed"));
        let first_delta = kinds
            .iter()
            .position(|k| *k == "response.output_text.delta")
            .expect("应有 output_text.delta");
        let created = kinds.iter().position(|k| *k == "response.created").unwrap();
        let completed = kinds
            .iter()
            .position(|k| *k == "response.completed")
            .unwrap();
        assert!(
            created < first_delta && first_delta < completed,
            "顺序应为 created→delta→completed"
        );
        // created 与 completed 的 response id 必须一致（客户端按 id 对号）
        assert_eq!(
            events[created]["response"]["id"],
            events[completed]["response"]["id"]
        );

        // sequence_number 单调递增
        let seqs: Vec<u64> = events
            .iter()
            .map(|e| e["sequence_number"].as_u64().unwrap())
            .collect();
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "sequence_number 应连续递增: {seqs:?}"
        );

        // 每个事件都能序列化成可解析的 SSE 帧
        for event in &events {
            let frame = sse_frame(event);
            assert!(frame.starts_with("event: "), "帧应以 event: 开头");
            let kind = frame
                .strip_prefix("event: ")
                .and_then(|rest| rest.split('\n').next())
                .unwrap();
            assert_eq!(kind, event["type"].as_str().unwrap());
            let data_line = frame
                .lines()
                .find_map(|line| line.strip_prefix("data: "))
                .expect("帧应有 data: 行");
            let parsed: Value = serde_json::from_str(data_line).expect("data 应为可解析 JSON");
            assert_eq!(parsed["type"], event["type"]);
            assert!(frame.ends_with("\n\n"), "帧应以空行结束");
        }
    }

    #[test]
    fn request_missing_model_returns_err() {
        // 反向对照：缺 model 的 body 必须 Err（不得静默兜底）
        let without_model = json!({ "input": "hi" });
        assert!(
            request_to_chat(&without_model).is_err(),
            "缺 model 必须返回 Err"
        );
        // 正向对照：同形 body 补上 model 即可成功
        let with_model = json!({ "model": "m1", "input": "hi" });
        assert!(request_to_chat(&with_model).is_ok());
    }
}
