//! 上游 SSE 协议 → OpenAI Chat SSE 的流式增量转换 + OpenAI 请求体 → 上游请求体。
//!
//! ## 为什么网关做转换（而不是脚本自己做）
//!
//! 脚本宿主的默认 conversation 管线（`host/provider.mjs`）只认识「上游原始
//! 字节」与「OpenAI SSE 字节」两个边界：端点/鉴权/模型名是编排，协议差异
//! 交给网关。好处是简单脚本包（只声明 base_url + models 的 API-key 家）
//! 不需要自己实现 Anthropic/Gemini 的流式事件机；需要特殊行为的包仍可
//! 用自己的 `conversation` 钩子完全接管（钩子优先）。
//!
//! ## 三个回调的语义
//!
//! - `script.protocol.request {protocol, body}` → `{body?, path?, headers?}`
//!   把 OpenAI chat 请求体转成上游协议请求体（一次性、无状态）；
//! - `script.protocol.chunk {protocol, stream, data(b64)}` → `{data(b64)}`
//!   把上游 SSE 字节增量转成 OpenAI `chat.completion.chunk` 行；`stream`
//!   是转换器会话键（同一次 conversation 的 RPC id）；
//! - `script.protocol.finish {stream}` → `{data?}` 冲刷转换器尾巴（上游
//!   收尾事件可能不在 chunk 边界上），未收尾的流补 `finish_reason=stop`。
//!
//! ## 并发与回收
//!
//! 转换器表上限 [`MAX_CONVERTERS`] 128（与上游口径一致）；超限拒绝新的
//! 流式调用（防止脚本忘记 finish 时内存无界）。每个转换器记录 last_used，
//! 插入时顺带回收 10 分钟未动的条目。
//!
//! ## 对齐来源
//!
//! 事件字段名与 AIGX 既有 bridge 的解析口径一致（`bridge/anthropic.rs` /
//! `bridge/gemini.rs` / `api/openai/responses_convert.rs`），保证同一上游
//! 字节在「原生渠道」与「脚本渠道」得到相同的 OpenAI 流。

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::plugin_host::rpc::RpcError;

/// 同时在途的转换器上限。
pub const MAX_CONVERTERS: usize = 128;

/// 空闲转换器回收阈值（插入时顺带清理）。
const IDLE_EVICT: Duration = Duration::from_secs(600);

/// 转换器注册表（每个宿主进程一份——`stream` 键是该宿主的 RPC id，
/// 跨宿主重启会复用，不能全局共享）。
pub struct ProtocolRegistry {
    streams: Mutex<HashMap<i64, ConverterEntry>>,
}

struct ConverterEntry {
    converter: Converter,
    last_used: Instant,
}

/// 一套协议的增量状态机。
enum Converter {
    Anthropic(AnthropicState),
    Responses(ResponsesState),
    Gemini(GeminiState),
}

impl Converter {
    fn new(protocol: &str) -> Result<Self, RpcError> {
        match protocol {
            "anthropic" => Ok(Self::Anthropic(AnthropicState::default())),
            "responses" => Ok(Self::Responses(ResponsesState::default())),
            "gemini" => Ok(Self::Gemini(GeminiState::default())),
            other => Err(bad_request(format!(
                "protocol converter for '{other}' unavailable"
            ))),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Anthropic(s) => s.push(bytes),
            Self::Responses(s) => s.push(bytes),
            Self::Gemini(s) => s.push(bytes),
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        match self {
            Self::Anthropic(s) => s.finish(),
            Self::Responses(s) => s.finish(),
            Self::Gemini(s) => s.finish(),
        }
    }
}

impl ProtocolRegistry {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            streams: Mutex::new(HashMap::new()),
        })
    }

    /// `script.protocol.chunk`：喂一段上游字节，返回 OpenAI SSE 字节（b64）。
    pub async fn chunk(&self, protocol: &str, stream: i64, data: &[u8]) -> Result<Value, RpcError> {
        let now = Instant::now();
        let mut map = self.streams.lock().await;
        map.retain(|_, e| now.duration_since(e.last_used) < IDLE_EVICT);
        let entry = match map.get_mut(&stream) {
            Some(entry) => entry,
            None => {
                if map.len() >= MAX_CONVERTERS {
                    return Err(RpcError {
                        message: format!("too many protocol converters (max {MAX_CONVERTERS})"),
                        code: Some("too_many".into()),
                    });
                }
                map.insert(
                    stream,
                    ConverterEntry {
                        converter: Converter::new(protocol)?,
                        last_used: now,
                    },
                );
                map.get_mut(&stream).expect("just inserted")
            }
        };
        entry.last_used = now;
        let out = entry.converter.push(data);
        Ok(if out.is_empty() {
            json!({})
        } else {
            json!({ "data": STANDARD.encode(&out) })
        })
    }

    /// `script.protocol.finish`：冲刷转换器尾部并移除会话。
    pub async fn finish(&self, stream: i64) -> Result<Value, RpcError> {
        let Some(mut entry) = self.streams.lock().await.remove(&stream) else {
            // 没有登记（脚本没走过 chunk 或已收尾）：幂等返回空
            return Ok(json!({}));
        };
        let out = entry.converter.finish();
        Ok(if out.is_empty() {
            json!({})
        } else {
            json!({ "data": STANDARD.encode(&out) })
        })
    }

    /// 当前在途转换器数（测试 / 诊断）。
    pub async fn active(&self) -> usize {
        self.streams.lock().await.len()
    }

    /// `script.protocol.request`：OpenAI chat 请求体 → 上游协议请求体。
    ///
    /// 纯函数（无状态）。`chat` 协议原样返回（不需要转换）。
    pub fn request(protocol: &str, body: &Value) -> Result<Value, RpcError> {
        match protocol {
            "chat" => Ok(json!({ "body": body })),
            "anthropic" => Ok(json!({
                "body": to_anthropic_body(body),
                "path": "v1/messages",
                "headers": { "anthropic-version": "2023-06-01" },
            })),
            "responses" => Ok(json!({
                "body": to_responses_body(body),
                "path": "v1/responses",
            })),
            "gemini" => Ok(json!({
                "body": to_gemini_body(body),
            })),
            other => Err(bad_request(format!(
                "protocol converter for '{other}' unavailable"
            ))),
        }
    }
}

// ── SSE 行拆分与输出（三个转换器共用）──────────────────────────────

/// 按行切分并处理完整行，返回剩余尾巴。
fn drain_lines(buf: &mut Vec<u8>, mut on_line: impl FnMut(&str)) {
    while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(&buf[..pos]).to_string();
        buf.drain(..=pos);
        on_line(line.trim_end_matches('\r'));
    }
}

/// 从一行取 JSON 负载。
///
/// 主形态是 SSE 的 `data: {...}`；**纯 JSON 行也收**（脚本自定义
/// `conversation` 钩子可能直接推未加 `data:` 前缀的 JSON 行，与
/// `bridge/plugin.rs` 的 SseLineParser 同口径）。`event:` / `id:` /
/// 注释行等一律 None。
fn data_payload(line: &str) -> Option<&str> {
    if let Some(payload) = line.strip_prefix("data:") {
        return Some(payload.trim());
    }
    let trimmed = line.trim();
    if trimmed.starts_with('{') {
        Some(trimmed)
    } else {
        None
    }
}

/// 一行 OpenAI `chat.completion.chunk`（`data: {json}\n\n`）。
fn chunk_line(
    id: &str,
    model: &str,
    delta: Value,
    finish: Option<&str>,
    usage: Option<Value>,
) -> String {
    let choice = json!({
        "index": 0,
        "delta": delta,
        "finish_reason": finish,
    });
    let mut obj = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": unix_now(),
        "model": model,
        "choices": [choice],
    });
    if let Some(usage) = usage {
        obj["usage"] = usage;
    }
    format!("data: {obj}\n\n")
}

fn done_line() -> &'static str {
    "data: [DONE]\n\n"
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("bad_request".into()),
    }
}

fn text_delta(content: &str) -> Value {
    json!({ "content": content })
}

fn reasoning_delta(text: &str) -> Value {
    json!({ "reasoning_content": text })
}

fn tool_start(index: usize, id: &str, name: &str) -> Value {
    json!({
        "tool_calls": [{
            "index": index,
            "id": id,
            "type": "function",
            "function": { "name": name, "arguments": "" },
        }]
    })
}

fn tool_args(index: usize, args: &str) -> Value {
    json!({
        "tool_calls": [{ "index": index, "function": { "arguments": args } }]
    })
}

fn usage_json(prompt: u64, completion: u64) -> Value {
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
    })
}

// ── Anthropic ─────────────────────────────────────────────────────

#[derive(Default)]
struct AnthropicState {
    buf: Vec<u8>,
    id: String,
    model: String,
    input_tokens: u64,
    sent_finish: bool,
    sent_done: bool,
    tool_index_by_block: BTreeMap<usize, usize>,
    next_tool_index: usize,
}

impl AnthropicState {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = String::new();
        let mut events: Vec<Value> = Vec::new();
        drain_lines(&mut self.buf, |line| {
            if let Some(payload) = data_payload(line) {
                if payload.is_empty() || payload == "[DONE]" {
                    return;
                }
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    events.push(v);
                }
            }
        });
        for event in events {
            self.handle_event(&event, &mut out);
        }
        out.into_bytes()
    }

    fn handle_event(&mut self, v: &Value, out: &mut String) {
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if let Some(message) = v.get("message") {
                    if let Some(id) = message.get("id").and_then(Value::as_str) {
                        self.id = id.to_string();
                    }
                    if let Some(model) = message.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                    if let Some(usage) = message.get("usage") {
                        self.input_tokens = usage
                            .get("input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                    }
                }
                out.push_str(&chunk_line(
                    self.chat_id(),
                    self.chat_model(),
                    json!({ "role": "assistant", "content": "" }),
                    None,
                    None,
                ));
            }
            "content_block_start" => {
                if let Some(block) = v.get("content_block") {
                    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                        let block_index =
                            v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                        let tool_index = self.tool_index(block_index);
                        let id = block.get("id").and_then(Value::as_str).unwrap_or("");
                        let name = block.get("name").and_then(Value::as_str).unwrap_or("");
                        out.push_str(&chunk_line(
                            self.chat_id(),
                            self.chat_model(),
                            tool_start(tool_index, id, name),
                            None,
                            None,
                        ));
                    }
                }
            }
            "content_block_delta" => {
                let Some(delta) = v.get("delta") else { return };
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(text) = delta.get("text").and_then(Value::as_str) {
                            out.push_str(&chunk_line(
                                self.chat_id(),
                                self.chat_model(),
                                text_delta(text),
                                None,
                                None,
                            ));
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(text) = delta.get("thinking").and_then(Value::as_str) {
                            out.push_str(&chunk_line(
                                self.chat_id(),
                                self.chat_model(),
                                reasoning_delta(text),
                                None,
                                None,
                            ));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(args) = delta.get("partial_json").and_then(Value::as_str) {
                            let block_index =
                                v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                            let tool_index = self.tool_index(block_index);
                            out.push_str(&chunk_line(
                                self.chat_id(),
                                self.chat_model(),
                                tool_args(tool_index, args),
                                None,
                                None,
                            ));
                        }
                    }
                    // signature / citations：OpenAI 侧无承载位，跳过
                    _ => {}
                }
            }
            "message_delta" => {
                let finish = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(anthropic_finish);
                let completion = v
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if finish.is_some() {
                    self.sent_finish = true;
                }
                if let Some(finish) = finish {
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        json!({}),
                        Some(finish),
                        Some(usage_json(self.input_tokens, completion)),
                    ));
                    out.push_str(done_line());
                    self.sent_done = true;
                }
            }
            "message_stop" => {
                if !self.sent_finish {
                    self.sent_finish = true;
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        json!({}),
                        Some("stop"),
                        None,
                    ));
                }
                if !self.sent_done {
                    out.push_str(done_line());
                    self.sent_done = true;
                }
            }
            "error" => {
                // 流内错误：以 OpenAI error 对象的 SSE 行透出（桥侧按错误体解析）
                out.push_str(&error_sse_line(
                    v.get("error"),
                    "upstream reported a stream error",
                ));
            }
            _ => {}
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        let mut out = String::new();
        // 尾巴里可能还有最后一行（无换行）
        if !self.buf.is_empty() {
            let tail = String::from_utf8_lossy(&self.buf).trim().to_string();
            self.buf.clear();
            if let Some(payload) = data_payload(&tail) {
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    self.handle_event(&v, &mut out);
                }
            }
        }
        if !self.sent_finish {
            self.sent_finish = true;
            out.push_str(&chunk_line(
                self.chat_id(),
                self.chat_model(),
                json!({}),
                Some("stop"),
                None,
            ));
        }
        if !self.sent_done {
            out.push_str(done_line());
            self.sent_done = true;
        }
        out.into_bytes()
    }

    fn chat_id(&self) -> &str {
        if self.id.is_empty() {
            "chatcmpl-plugin"
        } else {
            &self.id
        }
    }

    fn chat_model(&self) -> &str {
        if self.model.is_empty() {
            "plugin"
        } else {
            &self.model
        }
    }

    /// content_block index → 密集 tool_calls index（文本块与工具块共用
    /// 序号空间，直接透传会在下游 tool_calls 数组里留空洞）。
    fn tool_index(&mut self, block_index: usize) -> usize {
        if let Some(idx) = self.tool_index_by_block.get(&block_index) {
            return *idx;
        }
        let idx = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_index_by_block.insert(block_index, idx);
        idx
    }
}

fn anthropic_finish(reason: &str) -> &'static str {
    if reason == "max_tokens" || reason == "pause_turn" {
        return "length";
    }
    match reason {
        "tool_use" => "tool_calls",
        "refusal" => "content_filter",
        _ => "stop",
    }
}

// ── OpenAI Responses ──────────────────────────────────────────────

#[derive(Default)]
struct ResponsesState {
    buf: Vec<u8>,
    id: String,
    model: String,
    prompt_tokens: u64,
    sent_finish: bool,
    sent_done: bool,
    /// item id / call_id → 密集 tool_calls index。
    tool_index: BTreeMap<String, usize>,
    next_tool_index: usize,
}

impl ResponsesState {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = String::new();
        let mut events: Vec<Value> = Vec::new();
        drain_lines(&mut self.buf, |line| {
            if let Some(payload) = data_payload(line) {
                if payload.is_empty() || payload == "[DONE]" {
                    return;
                }
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    events.push(v);
                }
            }
        });
        for event in events {
            self.handle_event(&event, &mut out);
        }
        out.into_bytes()
    }

    fn handle_event(&mut self, v: &Value, out: &mut String) {
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "response.created" => {
                if let Some(resp) = v.get("response") {
                    if let Some(id) = resp.get("id").and_then(Value::as_str) {
                        self.id = id.to_string();
                    }
                    if let Some(model) = resp.get("model").and_then(Value::as_str) {
                        self.model = model.to_string();
                    }
                }
                out.push_str(&chunk_line(
                    self.chat_id(),
                    self.chat_model(),
                    json!({ "role": "assistant", "content": "" }),
                    None,
                    None,
                ));
            }
            "response.output_item.added" => {
                let Some(item) = v.get("item") else { return };
                if item.get("type").and_then(Value::as_str) != Some("function_call") {
                    return;
                }
                let key = item
                    .get("id")
                    .or_else(|| item.get("call_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let index = self.index_for(&key);
                let call_id = item.get("call_id").and_then(Value::as_str).unwrap_or(&key);
                let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                out.push_str(&chunk_line(
                    self.chat_id(),
                    self.chat_model(),
                    tool_start(index, call_id, name),
                    None,
                    None,
                ));
            }
            "response.output_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(Value::as_str) {
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        text_delta(delta),
                        None,
                        None,
                    ));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = v.get("delta").and_then(Value::as_str) {
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        reasoning_delta(delta),
                        None,
                        None,
                    ));
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(delta) = v.get("delta").and_then(Value::as_str) {
                    let key = v
                        .get("item_id")
                        .or_else(|| v.get("call_id"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let index = self.index_for(&key);
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        tool_args(index, delta),
                        None,
                        None,
                    ));
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                if let Some(resp) = v.get("response") {
                    if let Some(usage) = resp.get("usage") {
                        self.prompt_tokens = usage
                            .get("input_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(self.prompt_tokens);
                        let completion = usage
                            .get("output_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0);
                        let finish = v.get("type").and_then(Value::as_str)
                            == Some("response.failed")
                            || resp.get("status").and_then(Value::as_str) == Some("failed");
                        if finish {
                            out.push_str(&error_sse_line(
                                resp.get("error"),
                                "upstream response failed",
                            ));
                        }
                        self.sent_finish = true;
                        out.push_str(&chunk_line(
                            self.chat_id(),
                            self.chat_model(),
                            json!({}),
                            Some("stop"),
                            Some(usage_json(self.prompt_tokens, completion)),
                        ));
                    } else {
                        self.sent_finish = true;
                        out.push_str(&chunk_line(
                            self.chat_id(),
                            self.chat_model(),
                            json!({}),
                            Some("stop"),
                            None,
                        ));
                    }
                }
                if !self.sent_done {
                    out.push_str(done_line());
                    self.sent_done = true;
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        let mut out = String::new();
        if !self.buf.is_empty() {
            let tail = String::from_utf8_lossy(&self.buf).trim().to_string();
            self.buf.clear();
            if let Some(payload) = data_payload(&tail) {
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    self.handle_event(&v, &mut out);
                }
            }
        }
        if !self.sent_finish {
            self.sent_finish = true;
            out.push_str(&chunk_line(
                self.chat_id(),
                self.chat_model(),
                json!({}),
                Some("stop"),
                None,
            ));
        }
        if !self.sent_done {
            out.push_str(done_line());
            self.sent_done = true;
        }
        out.into_bytes()
    }

    fn chat_id(&self) -> &str {
        if self.id.is_empty() {
            "chatcmpl-plugin"
        } else {
            &self.id
        }
    }

    fn chat_model(&self) -> &str {
        if self.model.is_empty() {
            "plugin"
        } else {
            &self.model
        }
    }

    fn index_for(&mut self, key: &str) -> usize {
        if let Some(idx) = self.tool_index.get(key) {
            return *idx;
        }
        let idx = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_index.insert(key.to_string(), idx);
        idx
    }
}

// ── Gemini ────────────────────────────────────────────────────────

#[derive(Default)]
struct GeminiState {
    buf: Vec<u8>,
    model: String,
    sent_finish: bool,
    sent_done: bool,
    next_tool_index: usize,
}

impl GeminiState {
    fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.buf.extend_from_slice(bytes);
        let mut out = String::new();
        let mut events: Vec<Value> = Vec::new();
        drain_lines(&mut self.buf, |line| {
            if let Some(payload) = data_payload(line) {
                if payload.is_empty() || payload == "[DONE]" {
                    return;
                }
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    events.push(v);
                }
            }
        });
        for event in events {
            self.handle_event(&event, &mut out);
        }
        out.into_bytes()
    }

    fn handle_event(&mut self, v: &Value, out: &mut String) {
        if let Some(model) = v.get("modelVersion").and_then(Value::as_str) {
            self.model = model.to_string();
        }
        let Some(candidate) = v.get("candidates").and_then(|c| c.get(0)) else {
            // 只有 usageMetadata 的收尾帧
            if let Some(usage) = v.get("usageMetadata") {
                self.emit_finish(out, Some(usage));
            }
            return;
        };
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    let delta = if part.get("thought").and_then(Value::as_bool) == Some(true) {
                        reasoning_delta(text)
                    } else {
                        text_delta(text)
                    };
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        delta,
                        None,
                        None,
                    ));
                }
                if let Some(call) = part.get("functionCall") {
                    let index = self.next_tool_index;
                    self.next_tool_index += 1;
                    let name = call.get("name").and_then(Value::as_str).unwrap_or("");
                    let id = format!("call-{index}");
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        tool_start(index, &id, name),
                        None,
                        None,
                    ));
                    if let Some(args) = call.get("args") {
                        let encoded = serde_json::to_string(args).unwrap_or_default();
                        out.push_str(&chunk_line(
                            self.chat_id(),
                            self.chat_model(),
                            tool_args(index, &encoded),
                            None,
                            None,
                        ));
                    }
                }
            }
        }
        if candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.is_empty())
        {
            self.emit_finish(out, v.get("usageMetadata"));
        }
    }

    fn emit_finish(&mut self, out: &mut String, usage: Option<&Value>) {
        if self.sent_finish {
            return;
        }
        self.sent_finish = true;
        let usage = usage.map(|u| {
            let prompt = u
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let completion = u
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            usage_json(prompt, completion)
        });
        out.push_str(&chunk_line(
            self.chat_id(),
            self.chat_model(),
            json!({}),
            Some("stop"),
            usage,
        ));
        if !self.sent_done {
            out.push_str(done_line());
            self.sent_done = true;
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        let mut out = String::new();
        if !self.buf.is_empty() {
            let tail = String::from_utf8_lossy(&self.buf).trim().to_string();
            self.buf.clear();
            if let Some(payload) = data_payload(&tail) {
                if let Ok(v) = serde_json::from_str::<Value>(payload) {
                    self.handle_event(&v, &mut out);
                }
            }
        }
        self.emit_finish(&mut out, None);
        out.into_bytes()
    }

    fn chat_id(&self) -> &str {
        "chatcmpl-plugin"
    }

    fn chat_model(&self) -> &str {
        if self.model.is_empty() {
            "plugin"
        } else {
            &self.model
        }
    }
}

/// OpenAI 形状的流内错误行（桥侧 `parse_error_body` 认得）。
fn error_sse_line(err: Option<&Value>, fallback: &str) -> String {
    let message = err
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .unwrap_or(fallback);
    format!("data: {}\n\n", json!({ "error": { "message": message } }))
}

// ── 请求体转换（OpenAI chat → 上游）───────────────────────────────

/// OpenAI messages 里的文本内容（string 或 typed-block 数组的 text 部分）。
fn message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn to_anthropic_body(body: &Value) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut system_parts: Vec<String> = Vec::new();
    let mut out_messages: Vec<Value> = Vec::new();
    for msg in &messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
            "tool" => {
                let tool_use_id = msg
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                out_messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": message_text(msg.get("content")),
                    }],
                }));
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let args = call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .and_then(|a| serde_json::from_str::<Value>(a).ok())
                            .unwrap_or_else(|| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                            "name": call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                            "input": args,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    out_messages.push(json!({ "role": "assistant", "content": blocks }));
                }
            }
            _ => {
                let text = message_text(msg.get("content"));
                out_messages.push(json!({ "role": "user", "content": text }));
            }
        }
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "max_tokens": body
            .get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .cloned()
            .unwrap_or(json!(4096)),
        "messages": out_messages,
        "stream": true,
    });
    if !system_parts.is_empty() {
        out["system"] = json!(system_parts.join("\n\n"));
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| t.get("function"))
            .map(|f| {
                json!({
                    "name": f.get("name").cloned().unwrap_or(Value::Null),
                    "description": f.get("description").cloned().unwrap_or(Value::Null),
                    "input_schema": f
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })
            })
            .collect();
        if !converted.is_empty() {
            out["tools"] = json!(converted);
        }
    }
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) => match s.as_str() {
                "required" => json!({ "type": "any" }),
                "none" => json!({ "type": "none" }),
                _ => json!({ "type": "auto" }),
            },
            Value::Object(o) => o
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(|name| json!({ "type": "tool", "name": name }))
                .unwrap_or_else(|| json!({ "type": "auto" })),
            _ => json!({ "type": "auto" }),
        };
        out["tool_choice"] = mapped;
    }
    for key in ["temperature", "top_p"] {
        if let Some(v) = body.get(key) {
            out[key] = v.clone();
        }
    }
    if let Some(stop) = body.get("stop") {
        out["stop_sequences"] = match stop {
            Value::String(s) => json!([s]),
            other => other.clone(),
        };
    }
    out
}

fn to_responses_body(body: &Value) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for msg in &messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    instructions.push(text);
                }
            }
            "tool" => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": msg.get("tool_call_id").cloned().unwrap_or(Value::Null),
                    "output": message_text(msg.get("content")),
                }));
            }
            "assistant" => {
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": text }],
                    }));
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": call.get("id").cloned().unwrap_or(Value::Null),
                            "name": call
                                .get("function")
                                .and_then(|f| f.get("name"))
                                .cloned()
                                .unwrap_or(Value::Null),
                            "arguments": call
                                .get("function")
                                .and_then(|f| f.get("arguments"))
                                .cloned()
                                .unwrap_or(json!("{}")),
                        }));
                    }
                }
            }
            _ => {
                input.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": message_text(msg.get("content")) }],
                }));
            }
        }
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "input": input,
        "stream": true,
    });
    if !instructions.is_empty() {
        out["instructions"] = json!(instructions.join("\n\n"));
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools
            .iter()
            .filter_map(|t| t.get("function"))
            .map(|f| {
                json!({
                    "type": "function",
                    "name": f.get("name").cloned().unwrap_or(Value::Null),
                    "description": f.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": f
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })
            })
            .collect();
        if !converted.is_empty() {
            out["tools"] = json!(converted);
        }
    }
    for (from, to) in [
        ("max_tokens", "max_output_tokens"),
        ("max_completion_tokens", "max_output_tokens"),
        ("temperature", "temperature"),
        ("top_p", "top_p"),
    ] {
        if let Some(v) = body.get(from) {
            out[to] = v.clone();
        }
    }
    out
}

fn to_gemini_body(body: &Value) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    for msg in &messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    system_parts.push(json!({ "text": text }));
                }
            }
            "tool" => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": msg
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool"),
                            "response": { "content": message_text(msg.get("content")) },
                        }
                    }],
                }));
            }
            "assistant" => {
                let mut parts: Vec<Value> = Vec::new();
                let text = message_text(msg.get("content"));
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let args = call
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .and_then(|a| serde_json::from_str::<Value>(a).ok())
                            .unwrap_or_else(|| json!({}));
                        parts.push(json!({
                            "functionCall": {
                                "name": call
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(Value::as_str)
                                    .unwrap_or(""),
                                "args": args,
                            }
                        }));
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({ "role": "model", "parts": parts }));
                }
            }
            _ => {
                contents.push(json!({
                    "role": "user",
                    "parts": [{ "text": message_text(msg.get("content")) }],
                }));
            }
        }
    }
    let mut out = json!({ "contents": contents });
    if !system_parts.is_empty() {
        out["systemInstruction"] = json!({ "parts": system_parts });
    }
    let mut generation = serde_json::Map::new();
    for (from, to) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("max_tokens", "maxOutputTokens"),
        ("max_completion_tokens", "maxOutputTokens"),
    ] {
        if let Some(v) = body.get(from) {
            generation.insert(to.to_string(), v.clone());
        }
    }
    if let Some(stop) = body.get("stop") {
        let list = match stop {
            Value::String(s) => json!([s]),
            other => other.clone(),
        };
        generation.insert("stopSequences".to_string(), list);
    }
    if !generation.is_empty() {
        out["generationConfig"] = Value::Object(generation);
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let decls: Vec<Value> = tools
            .iter()
            .filter_map(|t| t.get("function"))
            .map(|f| {
                json!({
                    "name": f.get("name").cloned().unwrap_or(Value::Null),
                    "description": f.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": f
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                })
            })
            .collect();
        if !decls.is_empty() {
            out["tools"] = json!([{ "functionDeclarations": decls }]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把转换器输出拆成 JSON 事件列表（跳过 `[DONE]`）。
    fn parse_out(bytes: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter_map(|line| {
                let payload = line.strip_prefix("data: ")?;
                if payload == "[DONE]" {
                    return None;
                }
                serde_json::from_str::<Value>(payload).ok()
            })
            .collect()
    }

    fn has_done(bytes: &[u8]) -> bool {
        String::from_utf8_lossy(bytes).contains("data: [DONE]")
    }

    #[tokio::test]
    async fn anthropic_text_stream_to_openai_chunks() {
        let reg = ProtocolRegistry::new();
        let ev1 = br#"{"type":"message_start","message":{"id":"msg_1","model":"claude-x","usage":{"input_tokens":7}}}"#;
        // 字节串必须 ASCII：中文用普通 str 的 UTF-8 字节
        let ev2 = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}}"#.as_bytes();
        let ev3 = br#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}"#;
        let mut input = Vec::new();
        input.extend_from_slice(ev1);
        input.extend_from_slice(b"\n\n");
        input.extend_from_slice(ev2);
        input.extend_from_slice(b"\n\n");
        input.extend_from_slice(ev3);
        input.extend_from_slice(b"\n\n");
        let out = reg.chunk("anthropic", 1, &input).await.unwrap();
        let b64 = out["data"].as_str().unwrap();
        let bytes = STANDARD.decode(b64).unwrap();
        let events = parse_out(&bytes);
        assert!(
            events.len() >= 3,
            "至少 role/content/finish 三帧: {events:?}"
        );
        assert_eq!(events[0]["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(events[1]["choices"][0]["delta"]["content"], "你好");
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(last["usage"]["prompt_tokens"], 7);
        assert_eq!(last["usage"]["completion_tokens"], 3);
        assert_eq!(events[0]["id"], "msg_1");
        assert_eq!(events[0]["model"], "claude-x");
        assert!(has_done(&bytes), "finish 后应发 [DONE]");
    }

    #[tokio::test]
    async fn anthropic_split_lines_buffered_across_pushes() {
        let reg = ProtocolRegistry::new();
        let first =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"he"#;
        let second = br#"llo"}}"#;
        let out1 = reg.chunk("anthropic", 9, first).await.unwrap();
        assert!(out1.get("data").is_none(), "半行不应产出: {out1}");
        let mut second_full = second.to_vec();
        second_full.push(b'\n');
        let out2 = reg.chunk("anthropic", 9, &second_full).await.unwrap();
        let bytes = STANDARD.decode(out2["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(events[0]["choices"][0]["delta"]["content"], "hello");
    }

    #[tokio::test]
    async fn anthropic_tool_use_dense_index_and_args() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"get"}}"#,
            "\n",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}"#,
            "\n",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}"#,
            "\n"
        );
        let out = reg.chunk("anthropic", 2, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(
            events[0]["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        assert_eq!(
            events[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "get"
        );
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"a\":"
        );
        assert_eq!(
            events[2]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "1}"
        );
    }

    #[tokio::test]
    async fn responses_text_and_finish() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
            "\n",
            r#"{"type":"response.output_text.delta","delta":"hi"}"#,
            "\n",
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":2,"output_tokens":1}}}"#,
            "\n"
        );
        let out = reg.chunk("responses", 3, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(events[0]["id"], "resp_1");
        assert_eq!(events[1]["choices"][0]["delta"]["content"], "hi");
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(last["usage"]["total_tokens"], 3);
        assert!(has_done(&bytes));
    }

    #[tokio::test]
    async fn gemini_parts_and_finish() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"candidates":[{"content":{"parts":[{"text":"g"}]}}]}"#,
            "\n",
            r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"f","args":{"x":1}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":4,"candidatesTokenCount":2}}"#,
            "\n"
        );
        let out = reg.chunk("gemini", 4, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(events[0]["choices"][0]["delta"]["content"], "g");
        let tool_start = events
            .iter()
            .find(|e| e["choices"][0]["delta"]["tool_calls"][0]["function"]["name"] == "f")
            .expect("应有 tool 起始帧");
        assert_eq!(
            tool_start["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "stop");
        assert_eq!(last["usage"]["prompt_tokens"], 4);
        assert!(has_done(&bytes));
    }

    #[tokio::test]
    async fn finish_flushes_tail_and_synthesizes_stop() {
        let reg = ProtocolRegistry::new();
        // 没有换行的尾巴 + 上游没给 finish 帧
        let tail =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#;
        reg.chunk("anthropic", 5, tail).await.unwrap();
        let out = reg.finish(5).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(events[0]["choices"][0]["delta"]["content"], "x");
        assert_eq!(
            events.last().unwrap()["choices"][0]["finish_reason"],
            "stop"
        );
        assert!(has_done(&bytes));
        // finish 后会话已移除：再 finish 幂等返回空
        assert_eq!(reg.finish(5).await.unwrap(), json!({}));
        assert_eq!(reg.active().await, 0);
    }

    #[tokio::test]
    async fn unknown_protocol_is_rejected() {
        let reg = ProtocolRegistry::new();
        let err = reg.chunk("grpc", 1, b"x").await.unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
        assert!(ProtocolRegistry::request("grpc", &json!({})).is_err());
    }

    #[test]
    fn request_conversion_anthropic_keeps_tools_and_system() {
        let body = json!({
            "model": "claude-x",
            "max_tokens": 128,
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{\"a\":1}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "ok"}
            ],
            "tools": [{"type": "function", "function": {"name": "f", "description": "d", "parameters": {"type": "object"}}}],
            "tool_choice": "required",
            "temperature": 0.2,
            "stop": "END"
        });
        let out = ProtocolRegistry::request("anthropic", &body).unwrap();
        assert_eq!(out["path"], "v1/messages");
        let b = &out["body"];
        assert_eq!(b["system"], "be nice");
        assert_eq!(b["max_tokens"], 128);
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][1]["role"], "assistant");
        assert_eq!(b["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(b["messages"][1]["content"][0]["input"]["a"], 1);
        assert_eq!(b["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(b["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(b["tool_choice"]["type"], "any");
        assert_eq!(b["stop_sequences"], json!(["END"]));
        assert_eq!(b["stream"], true);
    }

    #[test]
    fn request_conversion_responses_and_gemini_shapes() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "u"}
            ],
            "max_tokens": 9
        });
        let r = ProtocolRegistry::request("responses", &body).unwrap();
        assert_eq!(r["path"], "v1/responses");
        assert_eq!(r["body"]["instructions"], "sys");
        assert_eq!(r["body"]["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(r["body"]["max_output_tokens"], 9);

        let g = ProtocolRegistry::request("gemini", &body).unwrap();
        assert_eq!(g["body"]["contents"][0]["parts"][0]["text"], "u");
        assert_eq!(g["body"]["systemInstruction"]["parts"][0]["text"], "sys");
        assert_eq!(g["body"]["generationConfig"]["maxOutputTokens"], 9);

        // chat 协议原样透传
        let c = ProtocolRegistry::request("chat", &body).unwrap();
        assert_eq!(c["body"], body);
    }
}
