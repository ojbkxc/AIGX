//! HTTP 200 假答复门控：**200 也可能是一次内容审核拒绝**。
//!
//! ── 为什么需要它 ────────────────────────────────────────────
//! 部分上游把「内容审核拒绝」包装成正常答复：HTTP 200、流正常收尾，但正文
//! 是一句固定的拒绝话术（如「作为一个人工智能语言模型，我还没学习如何回答
//! 这个问题…」）。只看 HTTP 状态就成了一次**假成功**：编排层不会换候选，
//! 客户端拿到的「答案」是一句套话。
//!
//! ── 判据完全可配置、默认空 = 不启用 ─────────────────────────
//! 词表来自 [`GuardrailSettings::fake_reply_rules`]（字面子串或正则），
//! 默认空表 ⇒ 本模块不参与任何请求（数据面零行为变化）。
//!
//! ── 流式：只看前缀 ──────────────────────────────────────────
//! 拒绝话术几乎总在开头；在把流交给客户端之前先读少量前缀 chunk 做判定：
//! - 命中：丢弃本轮流，调用方按「可轮换失败」处理（记录失败尝试、换下一
//!   候选），客户端尚未收到任何字节；
//! - 未命中：把已读前缀按原顺序 chain 回，下游逐字拿到完整流。
//!
//! 与破甲「异源递进」的 `peek_for_refusal` 同构，但判据独立
//! （本模块走配置词表，不依赖 purge 配置）。

use futures::StreamExt;

use super::keyword::KeywordRule;
use crate::bridge::ChatChunkStream;

/// 前缀最多读取的 chunk 数。
const PEEK_CHUNKS: usize = 8;
/// 前缀最多累积的字符数（与破甲流式探测同口径）。
const PEEK_CHARS: usize = 512;

/// 配置词表编译后的假答复匹配器（构造时编译一次）。
#[derive(Debug, Clone, Default)]
pub struct FakeReplyMatcher {
    rules: Vec<KeywordRule>,
}

impl FakeReplyMatcher {
    pub fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn new(rules: Vec<KeywordRule>) -> Self {
        Self { rules }
    }

    /// 是否启用（空表恒不启用）。
    pub fn is_enabled(&self) -> bool {
        !self.rules.is_empty()
    }

    /// 正文命中第一条规则时返回规则描述。
    pub fn hit(&self, text: &str) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        self.rules
            .iter()
            .find(|rule| rule.matches(text))
            .map(|rule| rule.description())
    }
}

/// 前缀探测结果。
pub enum FakeReplyPeek {
    /// 前缀命中假答复：流已被消费掉，本轮按可轮换失败处理。
    Refused { pattern: String },
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

/// 读取前缀判定是否命中假答复规则；未命中则把前缀 chain 回。
///
/// `stream` 必须是已经过首事件守卫的流（首事件已到）。前缀内出错时错误
/// 原样交还下游（`Pass` 里带回错误元素），探测阶段不吞错误。
pub async fn peek_for_fake_reply(
    mut stream: ChatChunkStream,
    matcher: &FakeReplyMatcher,
) -> FakeReplyPeek {
    let mut prefix: Vec<Result<crate::bridge::ChatChunk, crate::bridge::BridgeError>> =
        Vec::with_capacity(PEEK_CHUNKS);
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

    if let Some(pattern) = matcher.hit(&text) {
        return FakeReplyPeek::Refused { pattern };
    }

    let head = futures::stream::iter(prefix);
    FakeReplyPeek::Pass(Box::pin(head.chain(stream)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{BridgeError, ChatChunk, ChatDelta};
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

    fn matcher() -> FakeReplyMatcher {
        FakeReplyMatcher::new(vec![
            KeywordRule::literal("作为一个人工智能语言模型"),
            KeywordRule::regex("(?i)as an ai language model").unwrap(),
        ])
    }

    #[test]
    fn empty_matcher_is_disabled() {
        let m = FakeReplyMatcher::empty();
        assert!(!m.is_enabled());
        assert!(m.hit("作为一个人工智能语言模型").is_none());
    }

    #[test]
    fn literal_and_regex_rules_match() {
        let m = matcher();
        assert!(m
            .hit("作为一个人工智能语言模型，我还没学习如何回答这个问题")
            .is_some());
        assert!(m.hit("As an AI language model, I cannot answer").is_some());
        assert!(m.hit("这是正常答案").is_none());
    }

    /// 200 假答复触发换号，且假答复候选不记渠道成功。
    ///
    /// 用一个最小候选循环模拟数据面语义：命中 → 记录一次失败尝试、继续；
    /// 只有未命中的候选才计入「渠道成功」。
    #[test]
    fn fake_reply_rotates_and_never_marks_channel_success() {
        let m = matcher();
        let responses = [
            (
                "channel-a",
                "作为一个人工智能语言模型，我还没学习如何回答这个问题",
            ),
            ("channel-b", "这是正常答案"),
        ];
        let mut attempted = Vec::new();
        let mut successes = Vec::new();
        for (channel, text) in responses {
            attempted.push(channel);
            if m.hit(text).is_some() {
                // 可轮换失败：记录一次失败尝试，换下一候选，不记成功
                continue;
            }
            successes.push(channel);
            break;
        }
        assert_eq!(
            attempted,
            vec!["channel-a", "channel-b"],
            "必须换下一个候选"
        );
        assert_eq!(successes, vec!["channel-b"], "假答复候选不得记渠道成功");
    }

    #[tokio::test]
    async fn fake_reply_in_prefix_is_detected() {
        let m = matcher();
        match peek_for_fake_reply(
            stream_of(vec![
                chunk("作为一个人工智能语言模型，"),
                chunk("我还没学习如何回答这个问题。"),
            ]),
            &m,
        )
        .await
        {
            FakeReplyPeek::Refused { pattern } => assert!(pattern.contains("literal")),
            FakeReplyPeek::Pass(_) => panic!("前缀命中假答复必须判定为 Refused"),
        }
    }

    #[tokio::test]
    async fn normal_prefix_passes_through_verbatim() {
        let m = matcher();
        let items = vec![chunk("Hello"), chunk(", world"), chunk("!")];
        match peek_for_fake_reply(stream_of(items), &m).await {
            FakeReplyPeek::Pass(stream) => {
                let got: Vec<String> = stream
                    .map(|r| r.unwrap().delta.content.unwrap_or_default())
                    .collect()
                    .await;
                assert_eq!(got, vec!["Hello", ", world", "!"], "前缀必须原顺序回放");
            }
            FakeReplyPeek::Refused { .. } => panic!("正常内容不应判定为假答复"),
        }
    }

    #[tokio::test]
    async fn error_in_prefix_is_forwarded_not_swallowed() {
        let m = matcher();
        let items: Vec<Result<ChatChunk, BridgeError>> =
            vec![chunk("hi"), Err(BridgeError::StreamAborted), chunk("after")];
        match peek_for_fake_reply(stream_of(items), &m).await {
            FakeReplyPeek::Pass(stream) => {
                let got: Vec<Result<ChatChunk, BridgeError>> = stream.collect().await;
                assert_eq!(got.len(), 3, "错误元素必须原样交还下游");
                assert!(got[1].is_err());
            }
            FakeReplyPeek::Refused { .. } => panic!("错误不应被当成假答复"),
        }
    }
}
