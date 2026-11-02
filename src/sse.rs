//! Server-Sent Events (SSE) 行解码器，用于流式 Bridge。
//!
//! 参考 aisix 的 `SseDecoder` 设计。所有主流 AI 提供商（OpenAI、Anthropic、
//! Gemini、DeepSeek）都以 OpenAI 风格的 SSE 格式输出补全结果：
//!
//! ```text
//! data: {"choices":[…]}
//! data: {"choices":[…]}
//! data: [DONE]
//! ```
//!
//! 解码器采用 feed 驱动方式：调用方推送原始 HTTP Body 块，拉取类型化的
//! [`SseEvent`]。状态跨块边界保持，处理部分消息跨越 chunk 边界的情况。

use std::borrow::Cow;

use serde_json::Value;

/// SSE 事件
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// `data:` 负载（`data: ` 与事件终结符之间的全部内容）
    Data(String),
    /// OpenAI 风格的结束标记 `[DONE]`
    Done,
}

/// SSE 行解码器
#[derive(Debug, Default)]
pub struct SseDecoder {
    /// 尚未解码为完整 UTF-8 序列的原始字节
    byte_buf: Vec<u8>,
    /// 已解码的文本，等待 `\n\n` 事件终结符
    buffer: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推送一个字节块。返回此 feed 解锁的所有完整事件。
    /// 部分消息保留在缓冲区中，直到后续调用提供剩余部分。
    pub fn feed<'a>(&mut self, bytes: impl Into<Cow<'a, [u8]>>) -> Vec<SseEvent> {
        let bytes: Cow<'a, [u8]> = bytes.into();
        self.byte_buf.extend_from_slice(&bytes);
        self.decode_buffered_bytes();

        let mut events = Vec::new();
        // SSE 规范允许事件以 \n\n、\r\n\r\n 或 \r\r 分隔。
        // 部分上游/代理会输出 CRLF 行尾，只匹配 \n\n 会把两条事件
        // 粘成一条导致 JSON 解析失败，此处归一化处理。
        while let Some(idx) = find_event_end(&self.buffer) {
            let message: String = self.buffer.drain(..idx).collect();
            decode_message(&message, &mut events);
        }
        events
    }

    /// 刷新所有缓冲的尾部字节作为最终事件。
    /// 在 HTTP Body 结束后调用；如果没有缓冲内容则返回 `None`。
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.byte_buf.is_empty() {
            self.decode_buffered_bytes();
        }
        let remaining = std::mem::take(&mut self.buffer);
        if remaining.is_empty() {
            return None;
        }
        let mut events = Vec::new();
        decode_message(&remaining, &mut events);
        events.pop()
    }

    /// 将 `byte_buf` 中的字节解码为 UTF-8 字符串追加到 `buffer`。
    /// 保留可能被截断的多字节序列的尾部字节，等待下一块补全。
    fn decode_buffered_bytes(&mut self) {
        let mut bytes = std::mem::take(&mut self.byte_buf);
        // 找到最后一个完整 UTF-8 字符的边界
        let mut valid_len = bytes.len();
        while valid_len > 0 {
            // 尝试从 valid_len 处回退到上一个完整字符的结尾
            match std::str::from_utf8(&bytes[..valid_len]) {
                Ok(_) => break,
                Err(e) => {
                    valid_len = e.valid_up_to();
                    if valid_len == 0 {
                        // 第一个字节就是非法/不完整序列起始，保留所有字节等待
                        break;
                    }
                }
            }
        }
        if valid_len < bytes.len() {
            // 保留未完成部分
            let rest = bytes.split_off(valid_len);
            self.buffer
                .push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            self.byte_buf = rest;
        } else {
            self.buffer
                .push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            self.byte_buf.clear();
        }
    }
}

/// 在缓冲区中查找 SSE 事件终结符（`\n\n` / `\r\n\r\n` / `\r\r`）。
///
/// 返回从缓冲区开头到终结符（含）的字节数。未找到返回 `None`。
fn find_event_end(buffer: &str) -> Option<usize> {
    find_event_end_bytes(buffer.as_bytes())
}

/// 字节版事件终结符查找（供字节缓冲的流式 bridge 复用）。
pub(crate) fn find_event_end_bytes(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 1 < bytes.len() {
        match (bytes[i], bytes[i + 1]) {
            (b'\r', b'\r') => return Some(i + 2),
            (b'\n', b'\n') => return Some(i + 2),
            (b'\r', b'\n')
                if i + 3 < bytes.len() && bytes[i + 2] == b'\r' && bytes[i + 3] == b'\n' =>
            {
                return Some(i + 4)
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// 解析一条 SSE 消息，提取 `data:` 行。
/// 一条消息可能包含多个 `data:` 行，按 SSE 规范以 `\n` 连接为单个事件负载。
fn decode_message(message: &str, events: &mut Vec<SseEvent>) {
    let mut data_parts: Vec<String> = Vec::new();
    let mut saw_done = false;
    for line in message.lines() {
        if let Some(payload) = line.strip_prefix("data:") {
            let payload = payload.strip_prefix(' ').unwrap_or(payload);
            if payload == "[DONE]" {
                saw_done = true;
            } else {
                data_parts.push(payload.to_string());
            }
        }
        // 忽略其他 SSE 字段（event:, id:, retry:）
    }
    if !data_parts.is_empty() {
        events.push(SseEvent::Data(data_parts.join("\n")));
    }
    if saw_done {
        events.push(SseEvent::Done);
    }
}

// ────────────────────────────────────────────────────────────────

// Wrapper stream that injects ": keepalive\n\n" SSE comment events
// during idle periods. Used for passthrough SSE streams (Responses API)
// that cannot use axum Sse::keep_alive. Prevents CDN/proxy timeout.

pub struct KeepaliveStream<S> {
    inner: std::pin::Pin<Box<S>>,
    interval: tokio::time::Interval,
    done: bool,
}

impl<S> KeepaliveStream<S> {
    pub fn new(inner: S, interval: std::time::Duration) -> Self {
        let mut i = tokio::time::interval(interval);
        // First tick fires immediately ? we want to wait first
        i.reset();
        Self {
            inner: Box::pin(inner),
            interval: i,
            done: false,
        }
    }
}

impl<S: futures::Stream<Item = Result<bytes::Bytes, E>>, E: std::fmt::Debug> futures::Stream
    for KeepaliveStream<S>
{
    type Item = Result<bytes::Bytes, E>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if this.done {
            return std::task::Poll::Ready(None);
        }

        // Try inner stream first using poll_select-like approach:
        // We poll inner, and if pending, we check the timer.
        match this.inner.as_mut().poll_next(cx) {
            std::task::Poll::Ready(Some(item)) => {
                // Reset interval on data
                this.interval.reset();
                std::task::Poll::Ready(Some(item))
            }
            std::task::Poll::Ready(None) => {
                this.done = true;
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => {
                // Check interval timer
                match this.interval.poll_tick(cx) {
                    std::task::Poll::Ready(_) => std::task::Poll::Ready(Some(Ok(
                        bytes::Bytes::from_static(b": keepalive\n\n"),
                    ))),
                    std::task::Poll::Pending => std::task::Poll::Pending,
                }
            }
        }
    }
}

// ────────────────────────────────────────────────────────────────
// 「换行保活帧」识别（对齐 Agent2API `core/upstream/sse.rs`）。
//
// 部分上游会在正文分片之间插「content 只由换行组成」的帧给自己保活；客户端按
// markdown 渲染就成了「一行一个词」。开关开启时在下发前丢掉这类帧；关掉时
// 完全不调用本模块，桥接层零开销。

/// JS 真值语义（仅用于终止信号判定）。
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// 这一帧是否携带客户端只能从它拿到的终止 / 统计 / 错误信号。
///
/// 三项任一为真都**不能**当保活帧丢掉：`finish_reason`（收尾）、`usage`
/// （用量）、`error`（错误）。
fn carries_terminal_signal(chunk: &Value) -> bool {
    if chunk.get("error").is_some_and(truthy) {
        return true;
    }
    if chunk.get("usage").is_some_and(truthy) {
        return true;
    }
    chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("finish_reason"))
        .is_some_and(truthy)
}

/// 是否是「换行保活帧」。全部满足才算：
/// - 没有终止信号（见 [`carries_terminal_signal`]）；
/// - `choices[0].delta.content` 非空且只由 `\n` / `\r` 组成；
/// - 同一 delta 里没有别的载荷：`reasoning_content` / `tool_calls` /
///   `function_call` 都是假值。
///
/// `delta.role` 不参与判定：有的上游每帧都带 `role:"assistant"`，算进去就
/// 一帧都丢不掉。
pub fn is_newline_keepalive(chunk: &Value) -> bool {
    if carries_terminal_signal(chunk) {
        return false;
    }
    let Some(delta) = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("delta"))
    else {
        return false;
    };
    let Some(content) = delta.get("content").and_then(Value::as_str) else {
        return false;
    };
    if content.is_empty() || !content.chars().all(|c| c == '\n' || c == '\r') {
        return false;
    }
    if delta.get("reasoning_content").is_some_and(truthy)
        || delta.get("tool_calls").is_some_and(truthy)
        || delta.get("function_call").is_some_and(truthy)
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_data_event() {
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: hello\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("hello".into()));
    }

    #[test]
    fn done_event() {
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: [DONE]\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Done);
    }

    #[test]
    fn multiple_events_in_one_feed() {
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: first\n\ndata: second\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], SseEvent::Data("first".into()));
        assert_eq!(events[1], SseEvent::Data("second".into()));
    }

    #[test]
    fn partial_message_across_feeds() {
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: hel");
        assert!(events.is_empty());
        let events = decoder.feed(b"lo\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("hello".into()));
    }

    #[test]
    fn data_with_done() {
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: progress\n\ndata: [DONE]\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], SseEvent::Data("progress".into()));
        assert_eq!(events[1], SseEvent::Done);
    }

    #[test]
    fn crlf_event_separator() {
        // 部分上游/代理输出 CRLF 行尾的 SSE
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: first\r\n\r\ndata: second\r\n\r\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0], SseEvent::Data("first".into()));
        assert_eq!(events[1], SseEvent::Data("second".into()));
    }

    #[test]
    fn mixed_lf_and_crlf_lines() {
        // 消息内 \r\n 行尾 + \r\n\r\n 终结符
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: line1\r\ndata: line2\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("line1\nline2".into()));
    }

    #[test]
    fn partial_crlf_across_feeds() {
        // \r\n\r\n 终结符跨 chunk 边界（先到 \r\n\r，再到 \n）
        let mut decoder = SseDecoder::new();
        let events = decoder.feed(b"data: hello\r\n\r");
        assert!(events.is_empty());
        let events = decoder.feed(b"\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], SseEvent::Data("hello".into()));
    }

    /// 保活帧必须是 SSE **注释行**（以 `:` 开头），不能是 `event: ping`。
    ///
    /// OpenAI / Anthropic 的官方流里都没有 `ping` 事件，硬塞会让严格按事件
    /// 类型解析的 SDK 收到未知事件；注释行按 SSE 规范必须被忽略，但同样能
    /// 刷新 TCP/CDN 的空闲计时器 —— 这正是保活的目的。
    #[test]
    fn keepalive_frame_is_an_sse_comment() {
        let frame = b": keepalive\n\n";
        assert_eq!(
            frame.first(),
            Some(&b':'),
            "保活帧必须以冒号开头（SSE 注释行）"
        );
        assert!(!frame.starts_with(b"event:"), "不能伪装成事件帧");

        // 注释行不产生任何事件：解码器应当忽略它
        let mut decoder = SseDecoder::new();
        assert!(decoder.feed(frame).is_empty(), "注释行不应被解析成数据事件");
    }

    #[test]
    fn newline_only_chunk_is_keepalive() {
        let chunk = serde_json::json!({
            "choices": [{"delta": {"role": "assistant", "content": "\n\n"}}]
        });
        assert!(is_newline_keepalive(&chunk));
    }

    #[test]
    fn chunk_with_payload_is_not_keepalive() {
        // 有正文
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "hello\n"}}]
        })));
        // 换行 + 思维链
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "\n", "reasoning_content": "思考"}}]
        })));
        // 换行 + 工具调用
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "\n", "tool_calls": [{"id": "c1"}]}}]
        })));
    }

    #[test]
    fn terminal_frames_are_not_keepalive() {
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "\n"}, "finish_reason": "stop"}]
        })));
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "\n"}}], "usage": {"total_tokens": 1}
        })));
        assert!(!is_newline_keepalive(&serde_json::json!({
            "choices": [{"delta": {"content": "\n"}}], "error": {"message": "boom"}
        })));
        // 没有 choices / delta
        assert!(!is_newline_keepalive(&serde_json::json!({"usage": null})));
    }
}
