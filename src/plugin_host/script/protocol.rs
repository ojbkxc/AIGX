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

use std::collections::{BTreeMap, BTreeSet, HashMap};
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
        // 即使本次没有产出也回显 `"data": ""`：provider.mjs 在 `data`
        // 缺失时会回退为「透传上游原始字节」，半行 Anthropic/Gemini SSE
        // 会混进 OpenAI 流并打乱下游行缓冲（丢事件）。空串 = 已转换、无输出。
        Ok(json!({ "data": STANDARD.encode(&out) }))
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

/// 带缓存命中明细的 usage。
///
/// OpenAI 口径：`prompt_tokens_details.cached_tokens` 是 `prompt_tokens`
/// 的**子集**（网关 billing 依赖该子集按缓存价打折），因此调用方必须把
/// 上游的缓存量并进 `prompt_tokens` 之后再放明细。
fn usage_json_cached(prompt: u64, completion: u64, cached: u64) -> Value {
    let mut usage = usage_json(prompt, completion);
    if cached > 0 {
        usage["prompt_tokens_details"] = json!({ "cached_tokens": cached.min(prompt) });
    }
    usage
}

// ── Anthropic ─────────────────────────────────────────────────────

#[derive(Default)]
struct AnthropicState {
    buf: Vec<u8>,
    id: String,
    model: String,
    input_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
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
                        self.absorb_usage(usage);
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
                    match block.get("type").and_then(Value::as_str) {
                        Some("tool_use") => {
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
                            // 部分 Anthropic 兼容上游把完整 input 放在
                            // content_block_start 里、之后不再发 input_json_delta：
                            // 只在 start 带上工具参数，丢掉就等于丢整个入参。
                            if let Some(input) = block
                                .get("input")
                                .filter(|i| i.as_object().is_some_and(|m| !m.is_empty()))
                            {
                                let encoded = serde_json::to_string(input).unwrap_or_default();
                                out.push_str(&chunk_line(
                                    self.chat_id(),
                                    self.chat_model(),
                                    tool_args(tool_index, &encoded),
                                    None,
                                    None,
                                ));
                            }
                        }
                        Some("text") => {
                            // 同理：初始文本一般为空串，非空时不能吞掉。
                            if let Some(text) = block
                                .get("text")
                                .and_then(Value::as_str)
                                .filter(|t| !t.is_empty())
                            {
                                out.push_str(&chunk_line(
                                    self.chat_id(),
                                    self.chat_model(),
                                    text_delta(text),
                                    None,
                                    None,
                                ));
                            }
                        }
                        _ => {}
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
                if let Some(usage) = v.get("usage") {
                    // 部分后端只在 message_delta 补齐 input/cache 计数（max-wins）
                    self.absorb_usage(usage);
                }
                let finish = v
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(anthropic_finish);
                let completion = v
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64);
                // usage 只在 output_tokens 出现时产出（没有 usage 帧就别伪造 0）
                let usage = completion
                    .map(|c| usage_json_cached(self.prompt_tokens(), c, self.cache_read_tokens));
                if let Some(finish) = finish {
                    self.sent_finish = true;
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        json!({}),
                        Some(finish),
                        usage,
                    ));
                    out.push_str(done_line());
                    self.sent_done = true;
                } else if let Some(usage) = usage {
                    // 只有 usage、没有 stop_reason（部分兼容上游单独一帧给
                    // usage）：先落 usage，finish 等 message_stop 兜底补发。
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        json!({}),
                        None,
                        Some(usage),
                    ));
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

    /// 记录 input/cache 计数（max-wins：不同上游把它们分散在
    /// message_start / message_delta 两帧里）。
    fn absorb_usage(&mut self, usage: &Value) {
        if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
            self.input_tokens = self.input_tokens.max(v);
        }
        if let Some(v) = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
        {
            self.cache_creation_tokens = self.cache_creation_tokens.max(v);
        }
        if let Some(v) = usage.get("cache_read_input_tokens").and_then(Value::as_u64) {
            self.cache_read_tokens = self.cache_read_tokens.max(v);
        }
    }

    /// OpenAI 口径的 prompt 总量。
    ///
    /// Anthropic 的 `input_tokens` **不含**缓存量：`cache_creation_input_tokens`
    /// 与 `cache_read_input_tokens` 是 prompt 之外的追加量，必须并入
    /// `prompt_tokens`（否则缓存命中越多、账单越低）。缓存读取量另放
    /// `prompt_tokens_details.cached_tokens` 作折扣子集。
    fn prompt_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.cache_read_tokens)
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
    /// 是否输出过 function_call（终态 finish_reason 据此映射 tool_calls）。
    saw_tool_call: bool,
    /// 已收到增量参数（`response.function_call_arguments.delta`）的
    /// item id / call_id；用于避免 `*.done` 兜底再吐一遍完整 arguments。
    args_streamed: BTreeSet<String>,
    /// 键缺失时（上游没给 item id / call_id）是否已收到增量参数。
    unkeyed_args_streamed: bool,
    /// 键缺失时最近分配的 tool index（参数增量跟着最近的 item 走）。
    last_unkeyed_index: Option<usize>,
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
                let index = self.index_for(&key, true);
                self.saw_tool_call = true;
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
                    let index = self.index_for(&key, false);
                    self.mark_args_streamed(&key);
                    out.push_str(&chunk_line(
                        self.chat_id(),
                        self.chat_model(),
                        tool_args(index, delta),
                        None,
                        None,
                    ));
                }
            }
            // 部分实现不发参数增量、只在 .done 里给完整 arguments：
            // 未见增量的 item 用它兜底，否则客户端拿到空入参。
            "response.function_call_arguments.done" => {
                let Some(arguments) = v.get("arguments").and_then(Value::as_str) else {
                    return;
                };
                let key = v
                    .get("item_id")
                    .or_else(|| v.get("call_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.emit_pending_arguments(&key, arguments, out);
            }
            "response.output_item.done" => {
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
                if let Some(arguments) = item.get("arguments").and_then(Value::as_str) {
                    self.emit_pending_arguments(&key, arguments, out);
                }
            }
            "response.completed" | "response.incomplete" | "response.failed" => {
                let event_type = v.get("type").and_then(Value::as_str).unwrap_or("");
                let resp = v.get("response");
                let failed = event_type == "response.failed"
                    || resp.and_then(|r| r.get("status")).and_then(Value::as_str) == Some("failed");
                if failed {
                    out.push_str(&error_sse_line(
                        resp.and_then(|r| r.get("error")),
                        "upstream response failed",
                    ));
                }
                let usage = resp.and_then(|r| r.get("usage")).map(|usage| {
                    if let Some(v) = usage.get("input_tokens").and_then(Value::as_u64) {
                        self.prompt_tokens = self.prompt_tokens.max(v);
                    }
                    let completion = usage
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    let cached = usage
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    usage_json_cached(self.prompt_tokens, completion, cached)
                });
                // finish_reason 映射：failed → stop（错误行已透出）；
                // incomplete（截断/内容过滤）→ length / content_filter；
                // 本轮输出过 function_call → tool_calls；其余 → stop。
                let incomplete = event_type == "response.incomplete"
                    || resp.and_then(|r| r.get("status")).and_then(Value::as_str)
                        == Some("incomplete");
                let reason = if failed {
                    "stop"
                } else if incomplete {
                    match resp
                        .and_then(|r| r.get("incomplete_details"))
                        .and_then(|d| d.get("reason"))
                        .and_then(Value::as_str)
                    {
                        Some("content_filter") => "content_filter",
                        _ => "length",
                    }
                } else if self.saw_tool_call {
                    "tool_calls"
                } else {
                    "stop"
                };
                self.sent_finish = true;
                out.push_str(&chunk_line(
                    self.chat_id(),
                    self.chat_model(),
                    json!({}),
                    Some(reason),
                    usage,
                ));
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

    fn index_for(&mut self, key: &str, fresh_item: bool) -> usize {
        if key.is_empty() {
            // 没有 item id / call_id：`output_item.added` 视为新 item 分配新
            // index，参数增量则沿用最近分配的 index。不做这层区分的话，
            // 多个无 id 的调用会全撞到 index 0，客户端按 index 合并参数时
            // 把两份 JSON 拼坏。
            if fresh_item || self.last_unkeyed_index.is_none() {
                let idx = self.next_tool_index;
                self.next_tool_index += 1;
                self.last_unkeyed_index = Some(idx);
                return idx;
            }
            return self.last_unkeyed_index.unwrap_or(0);
        }
        if let Some(idx) = self.tool_index.get(key) {
            return *idx;
        }
        let idx = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_index.insert(key.to_string(), idx);
        idx
    }

    fn mark_args_streamed(&mut self, key: &str) {
        if key.is_empty() {
            self.unkeyed_args_streamed = true;
        } else {
            self.args_streamed.insert(key.to_string());
        }
    }

    fn args_already_streamed(&self, key: &str) -> bool {
        if key.is_empty() {
            self.unkeyed_args_streamed
        } else {
            self.args_streamed.contains(key)
        }
    }

    /// `.done` 类事件携带的完整 arguments 兜底：仅在该 item 没走过增量时发。
    fn emit_pending_arguments(&mut self, key: &str, arguments: &str, out: &mut String) {
        if arguments.is_empty() || self.args_already_streamed(key) {
            return;
        }
        let index = self.index_for(key, false);
        self.mark_args_streamed(key);
        out.push_str(&chunk_line(
            self.chat_id(),
            self.chat_model(),
            tool_args(index, arguments),
            None,
            None,
        ));
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
        // 提示词被安全策略拦截：帧里没有 candidates，只有 promptFeedback。
        // 不映射就会以默认 stop 收尾，下游误以为是完整回答。
        if let Some(block) = v
            .get("promptFeedback")
            .and_then(|f| f.get("blockReason"))
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            let reason = if block == "SAFETY" {
                "content_filter"
            } else {
                "stop"
            };
            self.emit_finish(out, v.get("usageMetadata"), reason);
            return;
        }
        let Some(candidate) = v.get("candidates").and_then(|c| c.get(0)) else {
            // 只有 usageMetadata 的收尾帧
            if let Some(usage) = v.get("usageMetadata") {
                self.emit_finish(out, Some(usage), "stop");
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
        if let Some(reason) = candidate
            .get("finishReason")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            self.emit_finish(out, v.get("usageMetadata"), gemini_finish(reason));
        }
    }

    fn emit_finish(&mut self, out: &mut String, usage: Option<&Value>, reason: &'static str) {
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
            // cachedContentTokenCount 是 promptTokenCount 的子集（OpenAI 口径）
            let cached = u
                .get("cachedContentTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            usage_json_cached(prompt, completion, cached)
        });
        out.push_str(&chunk_line(
            self.chat_id(),
            self.chat_model(),
            json!({}),
            Some(reason),
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
        self.emit_finish(&mut out, None, "stop");
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

/// Gemini `finishReason` → OpenAI `finish_reason`。
///
/// 与 `bridge/gemini.rs::parse_finish_reason` 同口径：长度别名族（含
/// `MAX_TOKENS`，大小写不敏感）→ length，`SAFETY` → content_filter，
/// 其余（`STOP` / `RECITATION` / 未知）→ stop。
fn gemini_finish(reason: &str) -> &'static str {
    if crate::bridge::is_length_finish(reason) {
        return "length";
    }
    match reason {
        "SAFETY" => "content_filter",
        _ => "stop",
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

/// 把 tool_result 块合并进前一条 user 轮次（并行工具结果的规范形状）。
/// 没有可合并的 user 轮次时返回 false，由调用方新起一条消息。
///
/// 只就地改写最后一条消息的内容（不改变外层消息数组长度），故收 `&mut [Value]`。
fn merge_tool_result(messages: &mut [Value], block: &Value) -> bool {
    let Some(last) = messages.last_mut() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    if let Some(arr) = last.get_mut("content").and_then(Value::as_array_mut) {
        arr.push(block.clone());
        return true;
    }
    let prior_text = last
        .get("content")
        .and_then(Value::as_str)
        .map(String::from);
    if let Some(text) = prior_text {
        last["content"] = json!([{ "type": "text", "text": text }, block.clone()]);
        return true;
    }
    false
}

/// user 消息 content → Anthropic content。
///
/// 数组内容保留 text 与 base64 图片/文档块；http(s) 图片 URL 需异步下载，
/// 同步转换里丢弃（与 `bridge/anthropic.rs` 同口径）。其余情况用拼接文本，
/// 空内容兜底 `"..."`（Anthropic 拒绝空 content）。
fn anthropic_user_content(content: Option<&Value>) -> Value {
    if let Some(Value::Array(blocks)) = content {
        let mut converted: Vec<Value> = Vec::new();
        for b in blocks {
            match b.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = b.get("text").and_then(Value::as_str) {
                        if !text.is_empty() {
                            converted.push(json!({ "type": "text", "text": text }));
                        }
                    }
                }
                Some("image_url") | Some("input_image") => {
                    if let Some(block) = anthropic_media_block(b) {
                        converted.push(block);
                    }
                }
                _ => {}
            }
        }
        if !converted.is_empty() {
            return Value::Array(converted);
        }
    }
    let text = message_text(content);
    Value::String(if text.is_empty() { "...".into() } else { text })
}

/// OpenAI 图片块 → Anthropic `image` / `document` 块（仅 base64 data URI）。
fn anthropic_media_block(block: &Value) -> Option<Value> {
    let url = block.get("image_url").and_then(|u| {
        u.as_str()
            .map(String::from)
            .or_else(|| u.get("url").and_then(Value::as_str).map(String::from))
    })?;
    let rest = url.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    let media_type = meta.strip_suffix(";base64")?;
    if media_type.is_empty() || payload.is_empty() {
        return None;
    }
    let block_type = if media_type == "application/pdf" {
        "document"
    } else {
        "image"
    };
    Some(json!({
        "type": block_type,
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": payload,
        },
    }))
}

fn to_anthropic_body(body: &Value) -> Value {
    let messages = body
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut system_parts: Vec<String> = Vec::new();
    let mut out_messages: Vec<Value> = Vec::new();
    let mut seen_non_system = false;
    for msg in &messages {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
        match role {
            "system" | "developer" => {
                let text = message_text(msg.get("content"));
                if text.is_empty() {
                    continue;
                }
                if seen_non_system {
                    // Anthropic 只有顶层 system；会话中途的 system 指令降级为
                    // user 轮次（保留语义，与 bridge/anthropic.rs 同口径）
                    out_messages.push(json!({ "role": "user", "content": text }));
                } else {
                    system_parts.push(text);
                }
            }
            "tool" => {
                seen_non_system = true;
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": msg
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    "content": message_text(msg.get("content")),
                });
                // 并行 tool_calls 的多个 tool 结果属于同一条 user 消息里的
                // 多个 tool_result 块；连续同角色轮次 Anthropic 会 400
                if !merge_tool_result(&mut out_messages, &block) {
                    out_messages.push(json!({ "role": "user", "content": [block] }));
                }
            }
            "assistant" => {
                seen_non_system = true;
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
                seen_non_system = true;
                out_messages.push(json!({
                    "role": "user",
                    "content": anthropic_user_content(msg.get("content")),
                }));
            }
        }
    }
    // Anthropic 要求首条消息为 user；首条是 assistant 时补占位 user
    let first_role = out_messages
        .first()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str);
    if !out_messages.is_empty() && first_role != Some("user") {
        out_messages.insert(
            0,
            json!({ "role": "user", "content": [{ "type": "text", "text": "..." }] }),
        );
    }
    let mut out = json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        // 显式 null 不算给了 max_tokens（OpenAI SDK 会把未设值序列化成 null）
        "max_tokens": body
            .get("max_tokens")
            .filter(|v| !v.is_null())
            .or_else(|| body.get("max_completion_tokens").filter(|v| !v.is_null()))
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
    if let Some(tc) = body.get("tool_choice").filter(|v| !v.is_null()) {
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
        if let Some(v) = body.get(key).filter(|v| !v.is_null()) {
            out[key] = v.clone();
        }
    }
    if let Some(stop) = body.get("stop").filter(|v| !v.is_null()) {
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
        if let Some(v) = body.get(from).filter(|v| !v.is_null()) {
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
    // OpenAI tool 消息只带 tool_call_id、不带函数名，而 Gemini 的
    // functionResponse.name 必须与 functionCall 对齐（否则上游 400）。
    // 先从 assistant 轮的 tool_calls 建 call id → name 映射。
    let mut call_names: HashMap<String, String> = HashMap::new();
    for msg in &messages {
        if msg.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            if let (Some(id), Some(name)) = (
                call.get("id").and_then(Value::as_str),
                call.get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str),
            ) {
                call_names.insert(id.to_string(), name.to_string());
            }
        }
    }
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
                let tool_call_id = msg
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let name = msg
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| call_names.get(tool_call_id).map(String::as_str))
                    .unwrap_or("tool");
                let text = message_text(msg.get("content"));
                // Gemini 的 response 是 Struct：内容本身是 JSON 对象时原样
                // 回传，否则包一层（与 bridge/gemini.rs 同口径）。
                let response = serde_json::from_str::<Value>(&text)
                    .ok()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({ "result": text }));
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": { "name": name, "response": response }
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
        if let Some(v) = body.get(from).filter(|v| !v.is_null()) {
            generation.insert(to.to_string(), v.clone());
        }
    }
    if let Some(stop) = body.get("stop").filter(|v| !v.is_null()) {
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
        assert_eq!(
            out1["data"], "",
            "半行时应显式返回空 data（不能缺键）: {out1}"
        );
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

    // ── 审查修复的回归测试 ─────────────────────────────────────────

    /// 半行缓冲的 chunk 必须回显 `"data": ""`：provider.mjs 在 `data`
    /// 缺失时回退为透传上游原始字节——半行 Anthropic SSE 混进 OpenAI 流，
    /// 还会打乱下游 SseLineParser 的行缓冲（丢下一帧）。
    #[tokio::test]
    async fn partial_line_yields_empty_data_not_raw_passthrough() {
        let reg = ProtocolRegistry::new();
        let out = reg
            .chunk("anthropic", 21, br#"data: {"type":"message_start","#)
            .await
            .unwrap();
        assert!(out["data"].is_string(), "必须显式回显空串: {out}");
        assert_eq!(out["data"], "");
        assert_eq!(reg.active().await, 1, "半行缓冲的会话必须保留");
        reg.finish(21).await.unwrap();
        assert_eq!(reg.active().await, 0);
    }

    /// 一次读回多个事件、且在一个事件 JSON 的正中间切开：逐行缓冲不能丢帧。
    #[tokio::test]
    async fn anthropic_multiple_events_split_at_arbitrary_offsets() {
        let reg = ProtocolRegistry::new();
        let wire = concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"model\":\"c\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ab\"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"cd\"}}\n\n"
        );
        let cut = wire.find("\"ab\"").unwrap() + 1;
        let out1 = reg
            .chunk("anthropic", 22, &wire.as_bytes()[..cut])
            .await
            .unwrap();
        let out2 = reg
            .chunk("anthropic", 22, &wire.as_bytes()[cut..])
            .await
            .unwrap();
        let mut all = Vec::new();
        for out in [out1, out2] {
            let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
            all.extend(parse_out(&bytes));
        }
        let content: String = all
            .iter()
            .filter_map(|e| e["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(content, "abcd", "切开的事件复原后不丢帧: {all:?}");
        assert_eq!(all[0]["choices"][0]["delta"]["role"], "assistant");
    }

    /// Anthropic 的 input_tokens 不含缓存；cache_creation/cache_read 是
    /// prompt 之外的追加量，必须并进 OpenAI 的 prompt_tokens，且读取量
    /// 要放进 prompt_tokens_details.cached_tokens（网关按子集算折扣）。
    #[tokio::test]
    async fn anthropic_cache_tokens_counted_in_prompt() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"message_start","message":{"id":"m","model":"c","usage":{"input_tokens":5,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000}}}"#,
            "\n",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}"#,
            "\n"
        );
        let out = reg.chunk("anthropic", 23, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        let finish = events.last().unwrap();
        assert_eq!(finish["usage"]["prompt_tokens"], 1105);
        assert_eq!(finish["usage"]["completion_tokens"], 7);
        assert_eq!(finish["usage"]["total_tokens"], 1112);
        assert_eq!(
            finish["usage"]["prompt_tokens_details"]["cached_tokens"],
            1000
        );
    }

    /// message_delta 只有 usage、没有 stop_reason 时不能把 usage 丢掉
    /// （finish 由 message_stop 兜底补发）。
    #[tokio::test]
    async fn anthropic_usage_frame_without_stop_reason_kept() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"message_start","message":{"id":"m","model":"c","usage":{"input_tokens":2}}}"#,
            "\n",
            r#"{"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":4}}"#,
            "\n",
            r#"{"type":"message_stop"}"#,
            "\n"
        );
        let out = reg.chunk("anthropic", 24, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        let with_usage = events
            .iter()
            .find(|e| e.get("usage").is_some())
            .expect("usage 不能丢");
        assert_eq!(with_usage["usage"]["prompt_tokens"], 2);
        assert_eq!(with_usage["usage"]["completion_tokens"], 4);
        assert_eq!(
            events.last().unwrap()["choices"][0]["finish_reason"],
            "stop"
        );
        assert!(has_done(&bytes));
    }

    /// 部分兼容上游把完整 input 放在 content_block_start（不再发
    /// input_json_delta）：入参不能丢。
    #[tokio::test]
    async fn anthropic_tool_input_in_content_block_start() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"toolu_9","name":"f","input":{"a":1}}}"#,
            "\n",
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}"#,
            "\n"
        );
        let out = reg.chunk("anthropic", 25, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(
            events[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "f"
        );
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"a\":1}"
        );
        assert_eq!(
            events.last().unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
    }

    /// Gemini finishReason 必须映射：MAX_TOKENS → length、SAFETY →
    /// content_filter（原实现一律 stop，截断/拦截被伪装成完整回答）。
    #[tokio::test]
    async fn gemini_finish_reason_aliases_mapped() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"candidates":[{"content":{"parts":[{"text":"x"}]},"finishReason":"MAX_TOKENS"}],"usageMetadata":{"promptTokenCount":5,"candidatesTokenCount":2,"cachedContentTokenCount":4}}"#,
            "\n"
        );
        let out = reg.chunk("gemini", 31, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "length");
        assert_eq!(last["usage"]["prompt_tokens"], 5);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 4);
        assert!(has_done(&bytes));

        let out = reg
            .chunk(
                "gemini",
                32,
                b"{\"candidates\":[{\"finishReason\":\"SAFETY\"}]}\n",
            )
            .await
            .unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(
            events.last().unwrap()["choices"][0]["finish_reason"],
            "content_filter"
        );
        assert!(has_done(&bytes));
    }

    /// promptFeedback.blockReason 是「提示词被拦截」的终态：没有 candidates，
    /// 必须映射成 content_filter 收尾，而不是默认 stop。
    #[tokio::test]
    async fn gemini_prompt_feedback_blocks_with_content_filter() {
        let reg = ProtocolRegistry::new();
        let out = reg
            .chunk(
                "gemini",
                33,
                b"{\"promptFeedback\":{\"blockReason\":\"SAFETY\"},\"usageMetadata\":{\"promptTokenCount\":3}}\n",
            )
            .await
            .unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "content_filter");
        assert_eq!(last["usage"]["prompt_tokens"], 3);
        assert!(has_done(&bytes));
    }

    /// Responses：输出过 function_call 时 finish_reason 应为 tool_calls；
    /// 参数增量跨帧拼接正确；`.done` 携带的完整 arguments 不与增量重复；
    /// usage 的 cached_tokens 映射到 prompt_tokens_details。
    #[tokio::test]
    async fn responses_tool_calls_finish_and_args() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"response.created","response":{"id":"resp_2","model":"m"}}"#,
            "\n",
            r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"get"}}"#,
            "\n",
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"a\":"}"#,
            "\n",
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"1}"}"#,
            "\n",
            r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","arguments":"{\"a\":1}"}"#,
            "\n",
            r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4,"output_tokens":2,"input_tokens_details":{"cached_tokens":3}}}}"#,
            "\n"
        );
        let out = reg.chunk("responses", 41, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        let start = events
            .iter()
            .find(|e| !e["choices"][0]["delta"]["tool_calls"].is_null())
            .expect("应有 tool 起始帧");
        assert_eq!(start["choices"][0]["delta"]["tool_calls"][0]["index"], 0);
        assert_eq!(
            start["choices"][0]["delta"]["tool_calls"][0]["id"],
            "call_1"
        );
        let args: String = events
            .iter()
            .filter_map(|e| {
                e["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect();
        assert_eq!(args, "{\"a\":1}", "增量拼接，且 .done 不重复追加");
        let last = events.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(last["usage"]["prompt_tokens"], 4);
        assert_eq!(last["usage"]["prompt_tokens_details"]["cached_tokens"], 3);
        assert!(has_done(&bytes));
    }

    /// response.incomplete（max_output_tokens 截断）→ finish_reason length。
    #[tokio::test]
    async fn responses_incomplete_maps_to_length() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"response.created","response":{"id":"resp_3","model":"m"}}"#,
            "\n",
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"input_tokens":9,"output_tokens":9}}}"#,
            "\n"
        );
        let out = reg.chunk("responses", 42, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(
            events.last().unwrap()["choices"][0]["finish_reason"],
            "length"
        );
        assert!(has_done(&bytes));
    }

    /// 终态事件不带 response 对象时也必须先发 finish 再发 [DONE]（原实现
    /// 先发 [DONE]，finish 被 finish() 追加到 DONE 之后 → 客户端已停读）。
    #[tokio::test]
    async fn responses_done_never_precedes_finish() {
        let reg = ProtocolRegistry::new();
        let out = reg
            .chunk("responses", 43, b"{\"type\":\"response.completed\"}\n")
            .await
            .unwrap();
        let text = String::from_utf8(STANDARD.decode(out["data"].as_str().unwrap()).unwrap())
            .expect("output utf8");
        let finish_at = text
            .find("\"finish_reason\":\"stop\"")
            .expect("应补 finish 帧");
        let done_at = text.find("data: [DONE]").expect("应发 [DONE]");
        assert!(finish_at < done_at, "[DONE] 必须在 finish 之后: {text}");
        assert_eq!(reg.finish(43).await.unwrap(), json!({}));
    }

    /// 上游不给 item id / call_id 时，多个 function_call 不能全落到 index 0
    /// （客户端按 index 合并参数会把两份 JSON 拼坏）。
    #[tokio::test]
    async fn responses_unkeyed_function_calls_get_distinct_indices() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","name":"a"}}"#,
            "\n",
            r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#,
            "\n",
            r#"{"type":"response.output_item.added","item":{"type":"function_call","name":"b"}}"#,
            "\n",
            r#"{"type":"response.function_call_arguments.delta","delta":"{\"x\":1}"}"#,
            "\n"
        );
        let out = reg.chunk("responses", 44, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(
            events[0]["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        assert_eq!(
            events[2]["choices"][0]["delta"]["tool_calls"][0]["index"],
            1
        );
        assert_eq!(
            events[3]["choices"][0]["delta"]["tool_calls"][0]["index"],
            1
        );
    }

    /// 上游只发 output_item.done（无参数增量）时用其完整 arguments 兜底。
    #[tokio::test]
    async fn responses_full_arguments_in_done_used_without_deltas() {
        let reg = ProtocolRegistry::new();
        let input = concat!(
            r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc_9","call_id":"call_9","name":"f"}}"#,
            "\n",
            r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"fc_9","call_id":"call_9","name":"f","arguments":"{\"k\":2}"}}"#,
            "\n"
        );
        let out = reg.chunk("responses", 45, input.as_bytes()).await.unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let events = parse_out(&bytes);
        assert_eq!(events.len(), 2, "起始帧 + 兜底参数帧: {events:?}");
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["index"],
            0
        );
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"],
            "{\"k\":2}"
        );
    }

    /// 并行工具调用的多个 tool 结果合并成一条 user 消息里的多个
    /// tool_result 块（Anthropic 不接受连续同角色轮次）。
    #[test]
    fn request_conversion_anthropic_merges_parallel_tool_results() {
        let body = json!({
            "model": "claude-x",
            "messages": [
                {"role": "user", "content": "q"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "a", "arguments": "{}"}},
                    {"id": "call_2", "type": "function", "function": {"name": "b", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "r1"},
                {"role": "tool", "tool_call_id": "call_2", "content": "r2"}
            ]
        });
        let out = ProtocolRegistry::request("anthropic", &body).unwrap();
        let msgs = out["body"]["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "两个 tool 结果应合并成一条 user: {msgs:?}");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "call_1");
        assert_eq!(msgs[2]["content"][1]["tool_use_id"], "call_2");
    }

    /// 中途 system 指令降级为 user 轮次（Anthropic 只有顶层 system）；
    /// 首条为 assistant 时补占位 user（Anthropic 要求首条 user）。
    #[test]
    fn request_conversion_anthropic_mid_system_and_leading_assistant() {
        let body = json!({
            "model": "m",
            "messages": [
                {"role": "assistant", "content": "first"},
                {"role": "user", "content": "u"},
                {"role": "system", "content": "later instruction"}
            ]
        });
        let out = ProtocolRegistry::request("anthropic", &body).unwrap();
        let b = &out["body"];
        assert!(b.get("system").is_none(), "中途 system 不应提升为顶层: {b}");
        let msgs = b["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user", "首条必须补占位 user");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "later instruction");
    }

    /// user 消息里的 base64 图片块不能丢（Anthropic 原生桥支持该转换）。
    #[test]
    fn request_conversion_anthropic_image_data_uri_kept() {
        let body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "see"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                ]
            }]
        });
        let out = ProtocolRegistry::request("anthropic", &body).unwrap();
        let content = &out["body"]["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAAA");
    }

    /// Gemini 的 functionResponse.name 必须与 functionCall 对齐：OpenAI 的
    /// tool 消息只有 tool_call_id，名字要从 assistant 轮解出来。
    #[test]
    fn request_conversion_gemini_tool_response_name_and_json_result() {
        let body = json!({
            "model": "gemini-x",
            "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"BJ\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "{\"temp\":21}"}
            ]
        });
        let out = ProtocolRegistry::request("gemini", &body).unwrap();
        let contents = out["body"]["contents"].as_array().unwrap();
        let resp = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(
            resp["name"], "get_weather",
            "函数名必须按 id 解出: {contents:?}"
        );
        assert_eq!(resp["response"]["temp"], 21);
        assert_eq!(
            contents[1]["parts"][0]["functionCall"]["name"],
            "get_weather"
        );
    }

    /// 显式 null 的参数（OpenAI SDK 序列化常见）不能原样透传给上游。
    #[test]
    fn request_conversion_drops_explicit_nulls() {
        let body = json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": null,
            "temperature": null,
            "top_p": null,
            "stop": null
        });
        let a = ProtocolRegistry::request("anthropic", &body).unwrap();
        assert_eq!(a["body"]["max_tokens"], 4096);
        assert!(a["body"].get("temperature").is_none());
        assert!(a["body"].get("top_p").is_none());
        assert!(a["body"].get("stop_sequences").is_none());

        let r = ProtocolRegistry::request("responses", &body).unwrap();
        assert!(r["body"].get("max_output_tokens").is_none());
        assert!(r["body"].get("temperature").is_none());
        assert!(r["body"].get("top_p").is_none());

        let g = ProtocolRegistry::request("gemini", &body).unwrap();
        assert!(g["body"].get("generationConfig").is_none());
    }
}
