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
        while let Some(idx) = self.buffer.find("\n\n") {
            let message: String = self.buffer.drain(..idx + 2).collect();
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
            self.buffer.push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            self.byte_buf = rest;
        } else {
            self.buffer.push_str(std::str::from_utf8(&bytes).unwrap_or(""));
            self.byte_buf.clear();
        }
    }
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
}