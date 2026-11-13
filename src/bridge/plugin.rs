//! 插件渠道 Bridge —— 请求由插件子进程构造并发出。
//!
//! ## 定位
//!
//! `ChannelType::Plugin` 的渠道指向「插件声明的 provider」：网关不做
//! 上游协议转换、不构造出站请求、**不实现任何上游伪装头/签名**——
//! 请求体经 `conversation` 流式 RPC 交给插件进程，由插件自己构造并发出，
//! 上游响应以 OpenAI SSE 原始字节流的形式经 chunk 帧回传，网关只负责
//! 逐行解析成 [`ChatChunk`]（流式）或聚合成 [`ChatResponse`]（非流式）。
//!
//! ## conversation 参数形状（参照上游 rpc.rs 的 forward_conversation）
//!
//! ```json
//! { "provider": "<插件 provider id>", "account": "", "body": { …OpenAI chat 请求体… } }
//! ```
//!
//! body 里的 `stream` 恒为 `true`：上游设计是「插件只写一条流式路径」
//! （非流式时网关本地聚合），因此本桥的 `chat` 同样走流式 RPC 再聚合，
//! 与上游 `aggregate_frame_stream` 同设计——插件作者不必维护两条路径。
//!
//! ## 其余能力
//!
//! embed / rerank / complete / 图片 / Responses 走 [`Bridge`] trait 的
//! 默认实现（`BridgeError::Config`），插件渠道当前只声明 chat 能力。

use async_trait::async_trait;
use futures::stream;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::Arc;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UpstreamErrorView,
    UpstreamWire, UsageStats,
};
use crate::plugin_host::{PluginProcess, RpcError, StreamCall, StreamEvent};

/// call_stream 等 head 的超时（与 rpc.rs 的 HEAD_TIMEOUT 一致）。
const HEAD_WAIT_MS: u64 = 30_000;

/// 插件接管的 provider 的 Bridge。
///
/// 每份候选持有一个 `Arc<PluginProcess>`（共享插件进程）+ provider id。
/// 进程句柄由 `PluginHostManager::process_for_provider` 给出——只有
/// 「插件声明了该 provider 且进程在跑」才会构造成功，因此这里拿到的
/// 进程必然活着（崩溃退避窗口内 resolve 层就拿不到它，根本到不了这里）。
pub struct PluginBridge {
    process: Arc<PluginProcess>,
    provider: String,
}

impl PluginBridge {
    /// 构造。
    pub fn new(process: Arc<PluginProcess>, provider: String) -> Self {
        Self { process, provider }
    }

    /// 发起一次 conversation 流式调用。
    ///
    /// body 恒带 `stream: true`（见模块头说明）；`account` 传空串——
    /// 插件渠道的账号语义由插件侧 provider 自己管理（插件契约里
    /// account 是账号库主键，渠道级桥没有账号上下文）。
    async fn open_conversation(&self, req: &ChatFormat) -> Result<StreamCall, BridgeError> {
        let params = json!({
            "provider": self.provider,
            "account": "",
            "body": build_openai_body(req),
        });
        self.process
            .rpc
            .call_stream("conversation", params)
            .await
            .map_err(|e| map_rpc_error(&e))
    }
}

/// 插件侧 RPC 失败 → BridgeError。
///
/// `timeout`（等 head 30s 未到）映射为可重试的 Timeout；`eof`（进程
/// 退出导致流截断）映射为 StreamAborted；其余（插件拒绝、写失败）按
/// 传输层失败处理——failover 都会换下一渠道。
fn map_rpc_error(err: &RpcError) -> BridgeError {
    match err.code.as_deref() {
        Some("timeout") => BridgeError::Timeout {
            elapsed_ms: HEAD_WAIT_MS,
            cause: err.message.clone(),
        },
        Some("eof") => BridgeError::StreamAborted,
        _ => BridgeError::Transport(format!("插件调用失败: {}", err.message)),
    }
}

/// 取 head 并校验状态。
///
/// call_stream 已保证首帧是 head（协议违规/插件拒绝都在 call_stream 里
/// 报错了），这里只需取状态码。非 2xx：插件仍会推错误体字节（上游
/// finish_stream 的同款约定），先收齐再按上游错误形状解析，不让客户端
/// 拿到一段 SSE 当错误。插件违规先发正文时按 200 继续（少一帧头不把
/// 整条流作废，正文字节随 `pending` 交回调用方）。
async fn take_head(call: &mut StreamCall) -> Result<(u16, Option<Vec<u8>>), BridgeError> {
    match call.next().await {
        Some(Ok(StreamEvent::Head { status, .. })) => {
            if (200..300).contains(&status) {
                return Ok((status, None));
            }
            let body = drain_stream(call).await?;
            Err(error_body_to_bridge(status, &body))
        }
        Some(Ok(StreamEvent::Chunk(bytes))) => Ok((200, Some(bytes))),
        Some(Ok(StreamEvent::End)) => Ok((200, None)),
        Some(Err(e)) => Err(map_rpc_error(&e)),
        None => Err(BridgeError::Transport("插件流在响应头前结束".into())),
    }
}

/// 收齐流剩余字节（失败状态的错误体用）。上限 1 MiB，超限后继续丢弃
/// 直到流走完——不提前丢句柄，否则插件侧发送窗口悬挂到自己超时。
async fn drain_stream(call: &mut StreamCall) -> Result<Vec<u8>, BridgeError> {
    const CAP: usize = 1 << 20;
    let mut out = Vec::new();
    loop {
        match call.next().await {
            Some(Ok(StreamEvent::Chunk(bytes))) => {
                if out.len() < CAP {
                    let take = (CAP - out.len()).min(bytes.len());
                    out.extend_from_slice(&bytes[..take]);
                }
            }
            Some(Ok(StreamEvent::Head { .. })) => {}
            Some(Ok(StreamEvent::End)) | None => return Ok(out),
            Some(Err(e)) => return Err(map_rpc_error(&e)),
        }
    }
}

/// 失败状态 + 错误体字节 → BridgeError。
///
/// 插件把上游错误体原样推成字节流：按 OpenAI 形状
/// `{error:{message,type,code,param}}` 解析，解析不出就试 Anthropic
/// 形状 `{type:"error",error:{...}}`，都失败则把原文当文案。
fn error_body_to_bridge(status: u16, body: &[u8]) -> BridgeError {
    let text = String::from_utf8_lossy(body);
    let parsed = parse_error_body(&text);
    let message = parsed
        .as_ref()
        .and_then(|p| p.message.clone())
        .unwrap_or_else(|| text.trim().to_string());
    BridgeError::UpstreamStatus {
        status,
        message,
        parsed: parsed.map(Box::new),
        wire: UpstreamWire::OpenAI,
        retry_after: None,
    }
}

fn parse_error_body(text: &str) -> Option<UpstreamErrorView> {
    let v: Value = serde_json::from_str(text).ok()?;
    let err = v.get("error")?;
    // Anthropic 形状的外层信封（{type:"error",error:{...}}）：
    // 字段语义与 OpenAI 一致（message/type），仅无 code/param。
    if v.get("type").and_then(Value::as_str) == Some("error") {
        return Some(UpstreamErrorView {
            kind: err.get("type").and_then(Value::as_str).map(String::from),
            message: err.get("message").and_then(Value::as_str).map(String::from),
            code: None,
            param: None,
        });
    }
    Some(UpstreamErrorView {
        kind: err.get("type").and_then(Value::as_str).map(String::from),
        message: err.get("message").and_then(Value::as_str).map(String::from),
        code: err.get("code").and_then(Value::as_str).map(String::from),
        param: err.get("param").and_then(Value::as_str).map(String::from),
    })
}

/// ChatFormat → OpenAI chat 请求体。
///
/// 字段口径与 openai bridge 的 `build_body` 一致（messages 的
/// content_blocks / tool_calls / tool_call_id / reasoning 都保留）；
/// `stream` 恒为 true（见模块头说明）。
fn build_openai_body(req: &ChatFormat) -> Value {
    let messages: Vec<Value> = req
        .messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            // 优先转发 content_blocks（vision/多模态 typed-block 数组），
            // 否则用 content（含 None → null，保留 assistant with
            // tool_calls 的 content:null 历史回放形状）。
            let mut msg = match &m.content_blocks {
                Some(blocks) => json!({
                    "role": role,
                    "content": Value::Array(blocks.clone()),
                }),
                None => json!({
                    "role": role,
                    "content": m.content.clone(),
                }),
            };
            if let Some(tid) = &m.tool_call_id {
                msg["tool_call_id"] = Value::String(tid.clone());
            }
            if let Some(tool_calls) = &m.tool_calls {
                msg["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.function_name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect(),
                );
            }
            msg
        })
        .collect();

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
    });
    if let Some(tools) = &req.tools {
        body["tools"] = json!(tools);
    }
    if let Some(tool_choice) = &req.tool_choice {
        body["tool_choice"] = json!(tool_choice);
    }
    if let Some(effort) = &req.reasoning_effort {
        body["reasoning_effort"] = json!(effort);
    }
    if let Some(web) = &req.web_search_options {
        body["web_search_options"] = json!(web);
    }
    if let Some(mt) = req.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    if let Some(t) = req.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = req.top_p {
        body["top_p"] = json!(p);
    }
    if let Some(stop) = &req.stop {
        body["stop"] = json!(stop);
    }
    if let Some(extra) = &req.extra {
        if let Some(obj) = extra.as_object() {
            for (k, v) in obj {
                body[k] = v.clone();
            }
        }
    }
    body
}

/// 一行 SSE 的解析结果。
#[derive(Debug)]
enum LineOutcome {
    /// 一行 → 一个 ChatChunk
    Chunk(ChatChunk),
    /// `[DONE]` 终结标记（流到此为止）
    Done,
    /// 空行 / 注释行 / event: id: retry: 等 → 忽略
    Skip,
}

/// 插件 chunk 帧是 OpenAI SSE 的**原始字节**（可能被任意切分），
/// 因此先按换行攒行、再逐行解析。支持的行形态：
///
/// - `data: {...}`（SSE 标准行；兼容 `data:` 后无空格）
/// - 纯 JSON 行（无 `data:` 前缀，部分插件实现直接推 JSON）
/// - 空行 / 注释行（`:` 开头）/ `event:` 等其它 SSE 行 → 忽略
/// - `data: [DONE]` → 终结标记（此后的行一律忽略）
#[derive(Default)]
pub struct SseLineParser {
    /// 未成行的字节尾巴
    buf: Vec<u8>,
    /// 已解析、待下发的 chunk（feed 可能一次产出多行）
    queue: VecDeque<ChatChunk>,
    /// 已见 `[DONE]`：流终结
    done: bool,
}

impl SseLineParser {
    /// 喂入一段原始字节，把其中已完成的行解析进内部队列。
    pub fn feed(&mut self, bytes: &[u8], id: &str, model: &str) {
        self.buf.extend_from_slice(bytes);
        self.parse_lines(id, model);
    }

    /// 流结束时冲刷缓冲里未换行的尾巴（最后一帧可能没有尾随空行）。
    pub fn finish(&mut self, id: &str, model: &str) {
        if self.done {
            return;
        }
        // feed 已消费完所有带换行的行，缓冲里剩下的就是最后一行
        let tail = std::mem::take(&mut self.buf);
        let text = String::from_utf8_lossy(&tail);
        self.parse_line(text.trim_end_matches('\r'), id, model);
    }

    /// 取出下一个已完成 chunk（无则 `None`）。
    pub fn drain_one(&mut self) -> Option<ChatChunk> {
        self.queue.pop_front()
    }

    fn parse_lines(&mut self, id: &str, model: &str) {
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line = String::from_utf8_lossy(&self.buf[..pos]).to_string();
            self.buf.drain(..=pos);
            self.parse_line(&line, id, model);
        }
    }

    fn parse_line(&mut self, line: &str, id: &str, model: &str) {
        if self.done {
            return;
        }
        match parse_sse_line(line, id, model) {
            LineOutcome::Chunk(c) => self.queue.push_back(c),
            LineOutcome::Done => {
                self.done = true;
                self.queue.push_back(done_chunk(id, model));
            }
            LineOutcome::Skip => {}
        }
    }
}

/// 解析一行 SSE → [`LineOutcome`]。
fn parse_sse_line(line: &str, id: &str, model: &str) -> LineOutcome {
    // 兼容 \r\n 行尾；空行 / SSE 注释行（`:` 开头）直接忽略
    let trimmed = line.trim_end_matches('\r').trim();
    if trimmed.is_empty() || trimmed.starts_with(':') {
        return LineOutcome::Skip;
    }
    // 取 data: 负载（兼容 `data:` 后无空格，与 openai bridge 同口径）；
    // 无前缀则整行当负载（纯 JSON 形态）
    let payload = match trimmed.strip_prefix("data:").map(str::trim_start) {
        Some(p) => p.trim(),
        None => trimmed,
    };
    if payload.is_empty() {
        return LineOutcome::Skip;
    }
    if payload == "[DONE]" {
        return LineOutcome::Done;
    }
    match serde_json::from_str::<Value>(payload) {
        Ok(v) => LineOutcome::Chunk(chunk_from_json(&v, id, model)),
        // 非 JSON 负载（保活注释的变体等）忽略，不伤正文
        Err(_) => LineOutcome::Skip,
    }
}

/// OpenAI chunk JSON → ChatChunk（与 openai bridge 的 parse_sse_event 同口径）。
fn chunk_from_json(v: &Value, fallback_id: &str, fallback_model: &str) -> ChatChunk {
    let choice = v.get("choices").and_then(|c| c.get(0));
    let delta = choice.and_then(|c| c.get("delta"));
    let content = delta
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .map(String::from);
    let tool_calls = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .map(|t| ToolCallDelta {
                    index: t.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    id: t.get("id").and_then(Value::as_str).map(String::from),
                    function_name: t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    arguments: t
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .map(String::from),
                    signature: None,
                })
                .collect()
        });
    // 流式推理增量：DeepSeek 系用 reasoning_content，OpenAI o 系列用 reasoning
    let reasoning = delta
        .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
        .and_then(Value::as_str)
        .map(String::from);
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(map_finish_reason);
    // stream_options.include_usage 终帧：usage 出现在顶层
    let usage = v.get("usage").map(parse_usage_from_value);
    ChatChunk {
        id: v
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(fallback_id)
            .to_string(),
        model: v
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model)
            .to_string(),
        delta: ChatDelta {
            content,
            tool_calls,
            reasoning,
        },
        finish_reason: finish,
        usage,
    }
}

/// `[DONE]` 终结帧：空 delta + finish=stop（与 openai bridge 同形状）。
fn done_chunk(id: &str, model: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
    }
}

/// finish_reason 字符串 → 归一化（与 openai bridge 的 parse_finish_reason 同口径：
/// 先过长度别名表，再 content_filter / tool_calls / stop）。
fn map_finish_reason(s: &str) -> FinishReason {
    if super::is_length_finish(s) {
        return FinishReason::Length;
    }
    match s {
        "content_filter" => FinishReason::ContentFilter,
        "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

/// 流式 usage 帧（`stream_options.include_usage` 终帧）→ UsageStats。
///
/// 缓存命中兜底链与 openai bridge 一致（prompt_cache_hit_tokens /
/// prompt_tokens_details.cached_tokens / cached_tokens），并收敛到
/// [0, prompt_tokens]（上游字段自相矛盾时不破恒等式）。
fn parse_usage_from_value(u: &Value) -> UsageStats {
    let prompt = u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0);
    let completion = u
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total = u
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(prompt + completion);
    let cached = u
        .get("prompt_cache_hit_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            u.get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
        })
        .or_else(|| u.get("cached_tokens").and_then(Value::as_u64))
        .unwrap_or(0);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cached_prompt_tokens: cached.clamp(0, prompt),
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 把流式 ChatChunk 聚合成非流式 ChatResponse。
///
/// - content / reasoning：拼接
/// - tool_calls：跨帧按 index 合并（id/name 取首个出现、arguments 拼接，
///   合并与参数修复复用 tool_repair 的 accumulate_tool_call_arguments，
///   与 openai bridge 非流式路径同口径）
/// - finish_reason / usage：取最后一个携带它们的帧；都没有则 Stop
fn aggregate_chunks(chunks: &[ChatChunk], fallback_id: &str, fallback_model: &str) -> ChatResponse {
    let (id, model) = match chunks.first() {
        Some(c) => (c.id.clone(), c.model.clone()),
        None => (fallback_id.to_string(), fallback_model.to_string()),
    };
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut finish_reason = None;
    let mut usage = None;
    let mut all_tool_deltas: Vec<ToolCallDelta> = Vec::new();
    for chunk in chunks {
        if let Some(c) = &chunk.delta.content {
            content.push_str(c);
        }
        if let Some(r) = &chunk.delta.reasoning {
            reasoning.push_str(r);
        }
        if let Some(tcs) = &chunk.delta.tool_calls {
            all_tool_deltas.extend(tcs.iter().cloned());
        }
        if chunk.finish_reason.is_some() {
            finish_reason = chunk.finish_reason;
        }
        if chunk.usage.is_some() {
            usage = chunk.usage;
        }
    }
    let merged = super::tool_repair::accumulate_tool_call_arguments(&all_tool_deltas);
    let mut msg = ChatMessage::assistant(content);
    if !reasoning.is_empty() {
        msg.reasoning = Some(reasoning);
    }
    msg.tool_calls = if merged.is_empty() {
        None
    } else {
        Some(
            merged
                .into_iter()
                .map(|(_index, id, name, arguments)| ToolCall {
                    id: id.unwrap_or_default(),
                    function_name: name.unwrap_or_default(),
                    arguments,
                })
                .collect(),
        )
    };
    ChatResponse {
        id,
        model,
        message: msg,
        finish_reason: finish_reason.unwrap_or(FinishReason::Stop),
        usage: usage.unwrap_or_default(),
    }
}

#[async_trait]
impl Bridge for PluginBridge {
    fn name(&self) -> &'static str {
        "plugin"
    }

    /// 非流式聊天：插件恒按流式推，本桥把帧聚合成完整响应
    /// （与上游 aggregate_frame_stream 同设计）。
    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut call = self.open_conversation(req).await?;
        let (_status, pending) = take_head(&mut call).await?;

        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let mut parser = SseLineParser::default();
        if let Some(bytes) = pending {
            parser.feed(&bytes, &id, &model);
        }
        let mut chunks = Vec::new();
        loop {
            match call.next().await {
                Some(Ok(StreamEvent::Chunk(bytes))) => parser.feed(&bytes, &id, &model),
                Some(Ok(StreamEvent::Head { .. })) => {}
                Some(Ok(StreamEvent::End)) => {
                    parser.finish(&id, &model);
                    break;
                }
                Some(Err(e)) => return Err(map_rpc_error(&e)),
                None => {
                    parser.finish(&id, &model);
                    break;
                }
            }
        }
        while let Some(chunk) = parser.drain_one() {
            chunks.push(chunk);
        }
        Ok(aggregate_chunks(&chunks, &id, &model))
    }

    /// 流式聊天：conversation RPC 的字节流逐行解析为 ChatChunk。
    ///
    /// head 已在 call_stream 里等好（30s 超时）；非 2xx 的 head 由
    /// take_head 收齐错误体后报错，2xx 则把 chunk 帧的 OpenAI SSE
    /// 原始字节解析成 ChatChunk 下发。
    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let mut call = self.open_conversation(req).await?;
        let (_status, pending) = take_head(&mut call).await?;

        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let mut parser = SseLineParser::default();
        if let Some(bytes) = pending {
            parser.feed(&bytes, &id, &model);
        }
        let chunk_stream = stream::unfold(
            (call, parser, id, model, false),
            |(mut call, mut parser, id, model, mut ended)| async move {
                loop {
                    if let Some(chunk) = parser.drain_one() {
                        return Some((Ok(chunk), (call, parser, id, model, ended)));
                    }
                    if ended {
                        return None;
                    }
                    match call.next().await {
                        Some(Ok(StreamEvent::Chunk(bytes))) => parser.feed(&bytes, &id, &model),
                        // 重复 head（协议违规）：丢弃，不毁正文
                        Some(Ok(StreamEvent::Head { .. })) => {}
                        Some(Ok(StreamEvent::End)) => {
                            ended = true;
                            parser.finish(&id, &model);
                        }
                        Some(Err(e)) => {
                            ended = true;
                            return Some((
                                Err(map_rpc_error(&e)),
                                (call, parser, id, model, ended),
                            ));
                        }
                        None => ended = true,
                    }
                }
            },
        );
        Ok(Box::pin(chunk_stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_line(line: &str) -> LineOutcome {
        parse_sse_line(line, "fb-id", "fb-model")
    }

    /// `data: {...}` SSE 行 → ChatChunk（delta.content / id / model）。
    #[test]
    fn data_line_parses_delta_content() {
        let chunk = match parse_line(
            r#"data: {"id":"chatcmpl-9","model":"gpt-x","choices":[{"index":0,"delta":{"content":"你好"},"finish_reason":null}]}"#,
        ) {
            LineOutcome::Chunk(c) => c,
            other => panic!("应解析为 chunk: {other:?}"),
        };
        assert_eq!(chunk.id, "chatcmpl-9");
        assert_eq!(chunk.model, "gpt-x");
        assert_eq!(chunk.delta.content.as_deref(), Some("你好"));
        assert_eq!(chunk.finish_reason, None);
    }

    /// `data:` 后无空格（部分 SSE 实现剥掉空格）也要兼容。
    #[test]
    fn data_line_without_space_after_colon() {
        let chunk = match parse_line(r#"data:{"choices":[{"delta":{"content":"x"}}]}"#) {
            LineOutcome::Chunk(c) => c,
            other => panic!("data: 后无空格应兼容: {other:?}"),
        };
        assert_eq!(chunk.delta.content.as_deref(), Some("x"));
        assert_eq!(chunk.id, "fb-id", "无 id 字段 → 回退传入的 id");
    }

    /// 纯 JSON 行（无 `data:` 前缀）直接解析。
    #[test]
    fn plain_json_line_parses() {
        let chunk = match parse_line(r#"{"choices":[{"delta":{"content":"纯JSON"}}]}"#) {
            LineOutcome::Chunk(c) => c,
            other => panic!("无 data: 前缀的纯 JSON 行应解析: {other:?}"),
        };
        assert_eq!(chunk.delta.content.as_deref(), Some("纯JSON"));
    }

    /// 空行 / 注释行 / 其它 SSE 行 / 非 JSON 负载一律忽略。
    #[test]
    fn empty_and_comment_lines_skipped() {
        assert!(matches!(parse_line(""), LineOutcome::Skip));
        assert!(matches!(parse_line("\r"), LineOutcome::Skip));
        assert!(matches!(parse_line("data:"), LineOutcome::Skip));
        assert!(matches!(parse_line(": keepalive"), LineOutcome::Skip));
        assert!(matches!(parse_line("event: ping"), LineOutcome::Skip));
        assert!(matches!(parse_line("id: chatcmpl-1"), LineOutcome::Skip));
        assert!(matches!(parse_line("not json at all"), LineOutcome::Skip));
    }

    /// `data: [DONE]` → 终结标记。
    #[test]
    fn done_marker_terminates() {
        assert!(matches!(parse_line("data: [DONE]"), LineOutcome::Done));
    }

    /// finish_reason + 流式 usage 帧的解析。
    #[test]
    fn finish_reason_and_usage_parsed() {
        let chunk = match parse_line(
            r#"data: {"choices":[{"delta":{},"finish_reason":"length"}],"usage":{"prompt_tokens":3,"completion_tokens":5,"total_tokens":8}}"#,
        ) {
            LineOutcome::Chunk(c) => c,
            other => panic!("应解析: {other:?}"),
        };
        assert_eq!(chunk.finish_reason, Some(FinishReason::Length));
        let usage = chunk.usage.expect("usage 帧应带 usage");
        assert_eq!(usage.prompt_tokens, 3);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 8);
    }

    /// 流式 tool_calls 增量的解析（id / name / 首段 arguments）。
    #[test]
    fn tool_call_delta_parsed() {
        let chunk = match parse_line(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"f","arguments":"{\"a\":"}}]}}]}"#,
        ) {
            LineOutcome::Chunk(c) => c,
            other => panic!("应解析: {other:?}"),
        };
        let tcs = chunk.delta.tool_calls.expect("tool_calls 增量");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].index, 0);
        assert_eq!(tcs[0].id.as_deref(), Some("call_1"));
        assert_eq!(tcs[0].function_name.as_deref(), Some("f"));
        assert_eq!(tcs[0].arguments.as_deref(), Some("{\"a\":"));
    }

    /// 跨帧的半行先缓冲，拼接完整后再产出。
    #[test]
    fn parser_buffers_split_lines() {
        let mut p = SseLineParser::default();
        assert!(p.drain_one().is_none());
        p.feed(
            b"data: {\"id\":\"c1\",\"choices\":[{\"delta\":{\"content\":",
            "fb",
            "fb",
        );
        assert!(p.drain_one().is_none(), "跨帧的半行不应产出");
        p.feed(b"\"split\"}}]}\n", "fb", "fb");
        let chunk = p.drain_one().expect("拼接完成后应产出");
        assert_eq!(chunk.delta.content.as_deref(), Some("split"));
    }

    /// 一次 feed 里多行 → 按行序全部产出。
    #[test]
    fn parser_multiple_lines_one_feed() {
        let mut p = SseLineParser::default();
        p.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}\n",
            "fb",
            "fb",
        );
        let a = p.drain_one().unwrap();
        let b = p.drain_one().unwrap();
        assert_eq!(a.delta.content.as_deref(), Some("a"));
        assert_eq!(b.delta.content.as_deref(), Some("b"));
        assert!(p.drain_one().is_none());
    }

    /// `[DONE]` 之后的行一律忽略。
    #[test]
    fn parser_done_ignores_following_lines() {
        let mut p = SseLineParser::default();
        p.feed(
            b"data: [DONE]\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n",
            "fb",
            "fb",
        );
        let done = p.drain_one().unwrap();
        assert_eq!(done.finish_reason, Some(FinishReason::Stop));
        assert!(p.drain_one().is_none(), "[DONE] 后的行应忽略");
    }

    /// finish 冲刷没有尾随换行的终帧。
    #[test]
    fn parser_finish_flushes_unterminated_tail() {
        let mut p = SseLineParser::default();
        p.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"tail\"}}]}",
            "fb",
            "fb",
        );
        assert!(p.drain_one().is_none());
        p.finish("fb", "fb");
        let chunk = p.drain_one().expect("finish 应冲刷无尾随换行的终帧");
        assert_eq!(chunk.delta.content.as_deref(), Some("tail"));
    }

    /// 空缓冲的 finish 是 no-op。
    #[test]
    fn parser_finish_empty_is_noop() {
        let mut p = SseLineParser::default();
        p.finish("fb", "fb");
        assert!(p.drain_one().is_none());
    }

    /// 聚合：content/reasoning 拼接、tool_calls 跨帧按 index 合并 + 参数修复、
    /// finish_reason 取最后携带的帧。
    #[test]
    fn aggregate_merges_content_reasoning_toolcalls() {
        let mk = |content: Option<&str>,
                  reasoning: Option<&str>,
                  tool_calls: Option<Vec<ToolCallDelta>>,
                  finish: Option<FinishReason>| {
            ChatChunk {
                id: "chatcmpl-1".into(),
                model: "m".into(),
                delta: ChatDelta {
                    content: content.map(String::from),
                    tool_calls,
                    reasoning: reasoning.map(String::from),
                },
                finish_reason: finish,
                usage: None,
            }
        };
        let chunks = vec![
            mk(Some("Hel"), Some("think1"), None, None),
            mk(
                Some("lo"),
                Some("think2"),
                Some(vec![ToolCallDelta {
                    index: 0,
                    id: Some("call_1".into()),
                    function_name: Some("get_weather".into()),
                    arguments: Some("{\"loc\":".into()),
                    signature: None,
                }]),
                None,
            ),
            mk(
                Some("!"),
                None,
                Some(vec![ToolCallDelta {
                    index: 0,
                    id: None,
                    function_name: None,
                    arguments: Some("\"beijing\"}".into()),
                    signature: None,
                }]),
                Some(FinishReason::ToolCalls),
            ),
        ];
        let resp = aggregate_chunks(&chunks, "fb-id", "fb-model");
        assert_eq!(resp.id, "chatcmpl-1");
        assert_eq!(resp.message.content_str(), "Hello!");
        assert_eq!(resp.message.reasoning.as_deref(), Some("think1think2"));
        let calls = resp.message.tool_calls.as_ref().expect("tool_calls 应合并");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function_name, "get_weather");
        assert_eq!(calls[0].arguments, "{\"loc\":\"beijing\"}");
        assert_eq!(resp.finish_reason, FinishReason::ToolCalls);
    }

    /// 空流聚合：空正文 + Stop（与上游聚合器的空响应同语义）。
    #[test]
    fn aggregate_empty_stream() {
        let resp = aggregate_chunks(&[], "fb-id", "fb-model");
        assert_eq!(resp.id, "fb-id");
        assert_eq!(resp.message.content_str(), "");
        assert_eq!(resp.finish_reason, FinishReason::Stop);
        // UsageStats 未派生 PartialEq，逐字段断言
        assert_eq!(resp.usage.prompt_tokens, 0);
        assert_eq!(resp.usage.completion_tokens, 0);
        assert_eq!(resp.usage.total_tokens, 0);
    }
}
