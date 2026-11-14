//! 流式拒绝检测（破甲控制台「异源递进」的流式侧补全）。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! SSE 的元信息（`message_start` / `message_delta`）与正文增量是分开到达的，
//! **HTTP 200 建立流不等于模型没有拒答**。非流式可以拿到整段文本后
//! `looks_refusal` 判定再换源；流式此前只按「建流失败/上游报错」换源，
//! 「建流成功但内容是拒答」的场景不会触发异源递进。
//!
//! ── 这里做什么 ──────────────────────────────────────────────
//! 首事件到达之后、把流交给客户端之前，**先读取少量前缀 chunk**做一次
//! 拒答判定：
//! - 命中拒绝 → 丢弃本轮流，按可重试失败处理（熔断记账 → 换下一候选 /
//!   异源模型），客户端还没收到任何字节，语义上等价于「上游拒绝了」；
//! - 未命中 → 把已读的前缀按原顺序 chain 回，下游逐字拿到完整流。
//!
//! ── 边界（刻意的取舍）───────────────────────────────────────
//! - 只看**前缀**（[`PEEK_CHUNKS`] 个 chunk 或 [`PEEK_BYTES`] 字节，先到者为准）。
//!   拒答句几乎总在开头；流后段才出现的疑似拒绝不算，避免长回答被误伤。
//! - 只看 `content` / `reasoning`，不看 tool_calls：工具调用的参数里可能天然
//!   含「I can't help with」这类待处理文本，误判会打断合法工具流程。
//! - 前缀判定用 `looks_refusal`，与失败分类同口径（同一套词表）。
//! - 判定纯读、零分配改写；未开启时不构造（调用方门控）。

use futures::StreamExt;

use crate::bridge::{BridgeError, ChatChunk, ChatChunkStream};

/// 前缀最多读取的 chunk 数。
const PEEK_CHUNKS: usize = 8;
/// 前缀最多累积的字符数（按 `chars()` 计，与词表匹配同单位）。
const PEEK_CHARS: usize = 512;

/// 拒答前缀探测结果。
pub enum Peek {
    /// 前缀命中拒答：流已被消费掉，本轮按失败处理。
    Refused,
    /// 未命中：返回「已读前缀 + 余下流」的重组流，逐字等价于原流。
    Pass(ChatChunkStream),
}

/// 把片段追加进探测缓冲，超出 [`PEEK_CHARS`] 的部分按字符边界截断。
fn push_prefix(buf: &mut String, filled: &mut usize, s: &str) {
    let remaining = PEEK_CHARS.saturating_sub(*filled);
    if remaining == 0 || s.is_empty() {
        return;
    }
    let take = s.chars().take(remaining).count();
    let end = s
        .char_indices()
        .nth(take)
        .map(|(idx, _)| idx)
        .unwrap_or(s.len());
    buf.push_str(&s[..end]);
    *filled += take;
}

/// 读取前缀判定是否拒答；未命中则把前缀 chain 回。
///
/// `stream` 必须是已经过首事件守卫（保证首事件已到、非空挂起）的流。
/// 中途出错时：错误原样交还下游（`Pass` 里带回错误元素），不在探测阶段吞掉——
/// 探测只判断「拒绝」，其余失败语义仍由原有消费路径处理。
pub async fn peek_for_refusal(mut stream: ChatChunkStream) -> Peek {
    let mut prefix: Vec<Result<ChatChunk, BridgeError>> = Vec::with_capacity(PEEK_CHUNKS);
    let mut text = String::new();
    let mut filled = 0usize;

    while prefix.len() < PEEK_CHUNKS && filled < PEEK_CHARS {
        match stream.next().await {
            Some(item) => {
                match &item {
                    Ok(chunk) => {
                        if let Some(content) = &chunk.delta.content {
                            push_prefix(&mut text, &mut filled, content);
                        }
                        if let Some(reasoning) = &chunk.delta.reasoning {
                            push_prefix(&mut text, &mut filled, reasoning);
                        }
                    }
                    // 前缀内出错：停止探测，把错误连同已读前缀交还下游。
                    Err(_) => {
                        prefix.push(item);
                        break;
                    }
                }
                prefix.push(item);
            }
            // 流在前缀内自然结束：没有下一项，前缀即全流。
            None => break,
        }
    }

    if !text.is_empty() && crate::purge::looks_refusal(&text) {
        return Peek::Refused;
    }

    let head = futures::stream::iter(prefix);
    Peek::Pass(Box::pin(head.chain(stream)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatChunk, ChatDelta};
    use futures::StreamExt;

    fn chunk(text: &str) -> Result<ChatChunk, BridgeError> {
        Ok(ChatChunk {
            id: "c1".into(),
            model: "m1".into(),
            delta: ChatDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                reasoning: None,
            },
            finish_reason: None,
            usage: None,
        })
    }

    fn stream_of(items: Vec<Result<ChatChunk, BridgeError>>) -> ChatChunkStream {
        Box::pin(futures::stream::iter(items))
    }

    #[tokio::test]
    async fn refusal_in_prefix_is_detected() {
        let s = stream_of(vec![
            chunk("I'm sorry, but I can't"),
            chunk(" help with that request."),
        ]);
        assert!(matches!(peek_for_refusal(s).await, Peek::Refused));
    }

    #[tokio::test]
    async fn normal_prefix_passes_through_verbatim() {
        let items = vec![chunk("Hello"), chunk(", world"), chunk("!")];
        let s = stream_of(items);
        match peek_for_refusal(s).await {
            Peek::Pass(stream) => {
                let got: Vec<String> = stream
                    .map(|r| r.unwrap().delta.content.unwrap_or_default())
                    .collect()
                    .await;
                assert_eq!(got, vec!["Hello", ", world", "!"], "前缀必须按原顺序回放");
            }
            Peek::Refused => panic!("正常内容不应判定为拒绝"),
        }
    }

    #[tokio::test]
    async fn refusal_after_prefix_window_is_not_detected() {
        // 前缀窗口外（第 9 个 chunk 起）出现的拒答不算：避免长回答被误伤。
        let mut items: Vec<Result<ChatChunk, BridgeError>> =
            (0..PEEK_CHUNKS).map(|_| chunk("ok ")).collect();
        items.push(chunk("I'm sorry, but I can't"));
        match peek_for_refusal(stream_of(items)).await {
            Peek::Pass(stream) => assert!(stream.count().await >= PEEK_CHUNKS),
            Peek::Refused => panic!("窗口外命中不应触发拒绝"),
        }
    }

    #[tokio::test]
    async fn empty_stream_passes_through() {
        match peek_for_refusal(stream_of(vec![])).await {
            Peek::Pass(stream) => assert_eq!(stream.count().await, 0),
            Peek::Refused => panic!("空流不应判定为拒绝"),
        }
    }

    #[tokio::test]
    async fn error_in_prefix_is_forwarded_not_swallowed() {
        let items: Vec<Result<ChatChunk, BridgeError>> =
            vec![chunk("hi"), Err(BridgeError::StreamAborted), chunk("after")];
        match peek_for_refusal(stream_of(items)).await {
            Peek::Pass(stream) => {
                let got: Vec<Result<ChatChunk, BridgeError>> = stream.collect().await;
                assert_eq!(got.len(), 3, "错误元素必须原样交还下游");
                assert!(got[1].is_err());
            }
            Peek::Refused => panic!("错误不应被当成拒绝"),
        }
    }
}
