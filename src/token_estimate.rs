//! Token 精确估算 — 借鉴 aisix-proxy/src/token_estimate.rs
//!
//! 当上游响应不携带 usage 块时（流式或非流式），回退到本地 token 计数。
//! 估算填补**仅用于遥测**：客户端可见的响应体不会被重写；
//! 上游返回值始终优先（逐字段 or 语义）。
//!
//! 编码选择镜像 tiktoken 的模型映射：
//! - `gpt-4o`/`o1` → `o200k_base`
//! - `gpt-4`/`gpt-3.5` → `cl100k_base`
//! - 未知/非 OpenAI 模型回退到 `cl100k_base`

use tiktoken_rs::tokenizer::{get_tokenizer, Tokenizer};
use tiktoken_rs::CoreBPE;

use crate::bridge::ChatFormat;

/// 输出文本累积上限（~1 MiB），超过后估算变为下界
pub const OUTPUT_ACCUMULATION_CAP: usize = 1 << 20;

/// 追加到估算缓冲区，硬上限为 OUTPUT_ACCUMULATION_CAP
pub fn push_capped(buf: &mut String, s: &str) {
    let remaining = OUTPUT_ACCUMULATION_CAP.saturating_sub(buf.len());
    if remaining == 0 || s.is_empty() {
        return;
    }
    if s.len() <= remaining {
        buf.push_str(s);
        return;
    }
    let mut cut = remaining;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    buf.push_str(&s[..cut]);
}

const TOKENS_PER_MESSAGE: u32 = 3;
const REPLY_PRIMING: u32 = 3;

/// 编码切片大小。tokenizer 的正则引擎在长连续段上超线性，切片线性化成本。
const ENCODE_SLICE_BYTES: usize = 64 * 1024;

/// 精确计数字节预算上限。超出尾部按 ~4 bytes/token 推断。
const EXACT_COUNT_BUDGET: usize = 1 << 20;

/// 推断 token 数（~4 bytes/token 经验法则）
fn approx_tokens(bytes: usize) -> u32 {
    (bytes / 4).min(u32::MAX as usize) as u32
}

fn clamp(n: usize) -> u32 {
    n.min(u32::MAX as usize) as u32
}

/// 选择模型的 BPE 编码器
fn bpe_for(model: &str) -> &'static CoreBPE {
    match get_tokenizer(model) {
        Some(Tokenizer::O200kHarmony) => tiktoken_rs::o200k_harmony_singleton(),
        Some(Tokenizer::O200kBase) => tiktoken_rs::o200k_base_singleton(),
        Some(Tokenizer::P50kBase) => tiktoken_rs::p50k_base_singleton(),
        Some(Tokenizer::P50kEdit) => tiktoken_rs::p50k_edit_singleton(),
        Some(Tokenizer::R50kBase | Tokenizer::Gpt2) => tiktoken_rs::r50k_base_singleton(),
        Some(Tokenizer::Cl100kBase) | None => tiktoken_rs::cl100k_base_singleton(),
    }
}

/// 防 panic、成本有界的 token 计数。
/// 分片编码，捕获 tokenizer panic，超出精确预算的尾部推断。
fn enc(bpe: &CoreBPE, text: &str) -> u32 {
    let mut n: u32 = 0;
    let mut rest = text;
    let mut budget = EXACT_COUNT_BUDGET;
    while !rest.is_empty() {
        if budget == 0 {
            return n.saturating_add(approx_tokens(rest.len()));
        }
        let mut cut = ENCODE_SLICE_BYTES.min(rest.len());
        while !rest.is_char_boundary(cut) {
            cut += 1;
        }
        let (head, tail) = rest.split_at(cut);
        let counted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bpe.encode_ordinary(head).len()
        }))
        .map(clamp)
        .unwrap_or_else(|_| approx_tokens(head.len()));
        n = n.saturating_add(counted);
        budget = budget.saturating_sub(cut);
        rest = tail;
    }
    n
}

/// 计数文本为纯文本（无消息开销）— 完成侧的规则
pub fn count_text(model: &str, text: &str) -> u32 {
    enc(bpe_for(model), text)
}

/// 估算聊天请求的 prompt token 数
pub fn count_chat_prompt(model: &str, req: &ChatFormat) -> u32 {
    let bpe = bpe_for(model);
    let mut n: u32 = 0;
    for m in &req.messages {
        let role = match m.role {
            crate::bridge::Role::System => "system",
            crate::bridge::Role::User => "user",
            crate::bridge::Role::Assistant => "assistant",
            crate::bridge::Role::Tool => "tool",
        };
        n = n.saturating_add(TOKENS_PER_MESSAGE);
        n = n.saturating_add(enc(bpe, role));
        if let Some(text) = m.content.as_deref() {
            n = n.saturating_add(enc(bpe, text));
        }
    }
    n.saturating_add(REPLY_PRIMING)
}

/// 逐字段 or 填充：上游值非零时优先，零值用本地计数替换。
/// `output_text: None` 禁用完成侧（无交付内容或未累积）。
pub fn fill_missing(
    model: &str,
    upstream_prompt: u32,
    upstream_completion: u32,
    output_text: Option<&str>,
) -> (u32, u32, bool) {
    let prompt = upstream_prompt;
    let mut completion = upstream_completion;
    let mut estimated = false;

    if upstream_prompt == 0 {
        // 需要请求体才能估算 prompt，这里只能返回 0
        // 调用方应使用 count_chat_prompt 预先估算
    }
    if upstream_completion == 0 {
        if let Some(text) = output_text {
            let n = count_text(model, text);
            if n > 0 {
                completion = n;
                estimated = true;
            }
        }
    }
    (prompt, completion, estimated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatMessage, Role};

    #[test]
    fn count_text_plain() {
        assert_eq!(count_text("gpt-4", ""), 0);
        // cl100k_base: "Hello" + " world" = 2 tokens
        assert_eq!(count_text("gpt-4", "Hello world"), 2);
        assert_eq!(count_text("some-proxy-model", "Hello world"), 2);
    }

    #[test]
    fn adversarial_whitespace_run_does_not_panic() {
        let evil = " ".repeat(1 << 20) + "x";
        let n = count_text("gpt-4", &evil);
        assert!(n > 0, "a 1 MiB run must still produce a count, got {n}");
    }

    #[test]
    fn push_capped_is_a_hard_cap() {
        let mut buf = String::new();
        push_capped(&mut buf, &"a".repeat(OUTPUT_ACCUMULATION_CAP - 1));
        push_capped(&mut buf, "汉汉汉");
        assert!(buf.len() <= OUTPUT_ACCUMULATION_CAP);
        assert!(buf.is_char_boundary(buf.len()));
    }

    #[test]
    fn chat_prompt_counts_cookbook_overhead() {
        let req = ChatFormat {
            model: "gpt-4".into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("Hello".into()),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
        };
        // 3 (per-message) + 1 ("user") + 1 ("Hello") + 3 (reply priming) = 8
        assert_eq!(count_chat_prompt("gpt-4", &req), 8);
    }

    #[test]
    fn encoding_selection_falls_back_to_cl100k() {
        // Unknown / non-OpenAI models fall back to cl100k_base.
        assert_eq!(count_text("claude-sonnet-4-5", "Hello world"), 2);
        assert_eq!(count_text("", "Hello world"), 2);
    }
}