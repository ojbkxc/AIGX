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
    /// 当前进行中事件的 `data:` 负载
    current_data: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推送一个字节块。返回此 feed 解锁的所有完整事件。
    /// 部分消息保留在缓冲区中，直到后续调用提供剩余部分。
    pub fn feed<'a>(&mut self, bytes: impl Into<Cow<'a, [u8]>>) -> Vec<SseEvent> {
        let bytes = bytes.into();
        self.byte_buf.extend_from_slice(&bytes);
        self.decode_buffered_bytes();

        let mut events = Vec::new();
        while let Some(idx) = self.buffer.find("\n\n") {
            let message: String = self.buffer.drain(..idx + 2).collect();
            self.decode_message(&message, &mut events);
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
        if remaining.is_empty() && self.current_data.is_empty() {
            return None;
        }
        // 将剩余文本作为 `data:` 行处理
        if !remaining.is_empty() {
            self.decode_message(&remaining, &mut Vec::new());
        }
        if !self.current_data.is_empty() {
            let data = std::mem::take(&mut self.current_data);
            return Some(SseEvent::Data(data));
        }
        None
    }

    /// 将 `byte_buf` 中的字节解码为 UTF-8 字符串追加到 `buffer`
    fn decode_buffered_bytes(&mut self) {
        let bytes = std::mem::take(&mut self.byte_buf);
        let s = String::from_utf8_lossy(&bytes);
        self.buffer.push_str(&s);
        // 保留可能被截断的多字节字符的尾部字节
        if let Some(&last_byte) = bytes.last() {
            if last_byte >> 6 == 0b10 {
                // 上一个字节是多字节序列的延续部分，但可能不完整
                // 保留它以等待下一个块
                self.byte_buf.push(last_byte);
            }
        }
    }

    /// 解析一条 SSE 消息，提取 `data:` 行
    fn decode_message(&mut self, message: &str, events: &mut Vec<SseEvent>) {
        for line in message.lines() {
            if let Some(payload) = line.strip_prefix("data:") {
                let payload = payload.trim();
                if payload == "[DONE]" {
                    // 刷新当前累积的 data
                    if !self.current_data.is_empty() {
                        events.push(SseEvent::Data(std::mem::take(&mut self.current_data)));
                    }
                    events.push(SseEvent::Done);
                } else {
                    if !self.current_data.is_empty() {
                        self.current_data.push('\n');
                    }
                    self.current_data.push_str(payload);
                }
            }
            // 忽略其他 SSE 字段（event:, id:, retry:）
        }
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