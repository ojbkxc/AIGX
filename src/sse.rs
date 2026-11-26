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
    /// 上次扫描到的最远字节位置；无完整事件时保存，避免跨块从头重扫。
    scan_pos: usize,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推送一个字节块。返回此 feed 解锁的所有完整事件。
    /// 部分消息保留在缓冲区中，直到后续调用提供剩余部分。
    pub fn feed<'a>(&mut self, bytes: impl Into<Cow<'a, [u8]>>) -> Vec<SseEvent> {
        let mut events = Vec::new();
        self.feed_into(bytes, &mut events);
        events
    }

    /// 与 [`feed`] 相同，但把事件追加进调用方复用的 `events`（免每次新建 Vec）。
    /// 内部只对每个事件分配一次 payload，不再产生中间 message String，
    /// 也不再「每事件 drain 一次 + 从 0 重扫」（一包多事件 → 单次正向扫描 + 单次 drain）。
    pub fn feed_into<'a>(&mut self, bytes: impl Into<Cow<'a, [u8]>>, events: &mut Vec<SseEvent>) {
        let bytes: Cow<'a, [u8]> = bytes.into();
        if !bytes.is_empty() {
            self.byte_buf.extend_from_slice(&bytes);
            self.decode_buffered_bytes();
        }

        // SSE 规范允许事件以 \n\n、\r\n\r\n 或 \r\r 分隔。
        // 部分上游/代理会输出 CRLF 行尾，只匹配 \n\n 会把两条事件
        // 粘成一条导致 JSON 解析失败，此处归一化处理。
        //
        // 首轮从上次扫描位置回退 3 字节续扫（覆盖跨块终结符边界——最长终结符
        // \r\n\r\n 为 4 字节，最多只有 3 个字节落在上一块）；找到第一个终结符后，
        // 后续从该边界精确续扫（不回退，避免把已消费的空事件重扫成死循环）。
        let mut consumed = 0usize;
        let mut search_from = if self.scan_pos == 0 {
            0
        } else {
            self.scan_pos.saturating_sub(3)
        };
        while let Some(end) = find_event_end_bytes_from(self.buffer.as_bytes(), search_from) {
            // 借用切片解码（与旧 `drain(..idx).collect()` 逐字等价，都含终结符），
            // 单次事件只分配一次 payload。
            decode_message(&self.buffer[consumed..end], events);
            consumed = end;
            search_from = end;
        }
        if consumed > 0 {
            // 单次前缀移除（摊还 O(剩余)），避免逐事件 drain 搬移。
            self.buffer.drain(..consumed);
        }
        // 记下「已扫描到缓冲区末尾」的位置，供下一块回退续扫。
        self.scan_pos = self.buffer.len();
    }

    /// 刷新所有缓冲的尾部字节作为最终事件。
    /// 在 HTTP Body 结束后调用；如果没有缓冲内容则返回 `None`。
    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.byte_buf.is_empty() {
            self.decode_buffered_bytes();
        }
        let remaining = std::mem::take(&mut self.buffer);
        self.scan_pos = 0;
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

/// 从 `from` 起**精确**查找事件终结符（`\n\n` / `\r\n\r\n` / `\r\r`）。
///
/// 返回自缓冲区开头的绝对字节数（含终结符），未找到返回 `None`。
/// 需覆盖跨块边界时，调用方自行把 `from` 回退 3 字节传入。
pub(crate) fn find_event_end_bytes_from(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
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
    // 借用切片收集，免去逐行 `to_string()`；单行时只分配一次 payload。
    let mut data_parts: Vec<&str> = Vec::new();
    let mut saw_done = false;
    for line in message.lines() {
        if let Some(payload) = line.strip_prefix("data:") {
            let payload = payload.strip_prefix(' ').unwrap_or(payload);
            if payload == "[DONE]" {
                saw_done = true;
            } else {
                data_parts.push(payload);
            }
        }
        // 忽略其他 SSE 字段（event:, id:, retry:）
    }
    if let Some(first) = data_parts.first().copied() {
        // 绝大多数事件只有一行 data：直接 to_string 一次即可（旧路径 join 的二次拼接）；
        // 多行（罕见）才按规范 \n 连接。
        if data_parts.len() == 1 {
            events.push(SseEvent::Data(first.to_string()));
        } else {
            events.push(SseEvent::Data(data_parts.join("\n")));
        }
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
    fn byte_at_a_time_uses_scan_continuation() {
        // 逐字节喂入：既验证跨块终结符（\n\n 的第二个 \n 落在新块），
        // 也验证 scan_pos 回退续扫既不漏事件、也不重复产出。
        let mut decoder = SseDecoder::new();
        let mut events = Vec::new();
        for b in b"data: one\n\ndata: two\n\n" {
            decoder.feed_into(std::slice::from_ref(b), &mut events);
        }
        assert_eq!(
            events,
            vec![SseEvent::Data("one".into()), SseEvent::Data("two".into())]
        );
    }

    #[test]
    fn feed_into_appends_to_caller_vec() {
        let mut decoder = SseDecoder::new();
        let mut events = vec![SseEvent::Data("pre".into())];
        decoder.feed_into(b"data: hello\n\n", &mut events);
        assert_eq!(
            events,
            vec![SseEvent::Data("pre".into()), SseEvent::Data("hello".into())]
        );
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
