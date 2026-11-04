//! 出站指纹脱敏（对照 Agent2API `core/sanitize.rs`：剥离上游内容审核黑名单指纹）。
//!
//! ── 背景 ────────────────────────────────────────────────────
//! 客户端（Claude Code 类 CLI）在 system prompt 注入若干固定模板句，上游内容
//! 审核按**逐字精确匹配**拦截（非语义审核），一字改动即可绕过。策略：
//! 表头键值型指纹整段剥离；承载语义的模板句最小改写（换/插一个词，语义不变）。
//!
//! ── 规则集（静态数组驱动，无词表端点）───────────────────────
//! 与 Agent2API 同一取舍：规则是硬编码的，用户没有可维护的词表；开关
//! （`usage.sanitize_fingerprints`，默认开）在转发层逐请求读取，本模块
//! 无状态、无锁、无 IO。
//!
//! ── 与参考实现的差异（workaround 留痕）──────────────────────
//! 参考仓库的 `SANITIZE_REWRITES` 源码本身已被污染：「改写前」串（from）被
//! 它自己的净化规则改写成了「改写后」串（to），表头键名同样被 "header→hdr"
//! 缩写层污染。本文件按参考注释与幸存的 to 串反推原始规则——from 为客户端
//! 真实注入的指纹原文，to 为绕过上游逐字匹配的最小改写：
//! - `official CLI for Claude` → `official CLI tool for Claude`（插词，任务锚点）；
//! - `Default branch (you will usually use this for PRs)` →
//!   `Default branch (you will usually use this for PRs)`（换词；预检特征
//!   "Main branch (" 对应客户端注入形态）；
//! - Codex 身份句 `Codex CLI` → `Codex CLI tool`（插词，与身份句同型）；
//! - 反馈句 `To give feedback` → `To provide feedback`（参考注释明说 give→provide）；
//! - 裸数字 `11128`（上游反探测：请求体含其内部错误码即整单拦截）→ 插一个
//!   连字符改成 `11-128`（参考注释"改写保留可读性与指代"；零宽类字符实测
//!   被上游归一化剔除）；
//! - 表头键名 `x-anthropic-billing-header`：键值形态整段删除（以分号为分段
//!   边界）；残留的裸键名缩写为 `x-anthropic-billing-hdr`（参考注释"最小
//!   缩写 header→hdr"）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use regex::Regex;

use crate::bridge::ChatMessage;

/// 命中统计（P2-11）：进入净化的请求数 + 改写应用次数。
static STATS_HITS: AtomicU64 = AtomicU64::new(0);
static STATS_REWRITES: AtomicU64 = AtomicU64::new(0);

/// 特征预检：任一命中才进入净化（`str::contains` 快速路径，
/// 普通请求全不中 → 原样返回，零分配）。匹配大小写敏感（对应参考的
/// `strings.Contains` 口径）。
const SANITIZE_FEATURES: &[&str] = &[
    "x-anthropic-billing-header", // 表头键值段键名（完整拼写；hdr 是缩写后形态）
    "cc_entrypoint=",             // 尾随裸键值（截断前缀即可命中；可无表头键名）
    "You are Claude Code",        // 身份句（截断前缀即可命中）
    "Main branch (",              // 注入指令句（截断前缀即可命中）
    "You are a coding agent running in the Codex CLI", // Codex instructions 首段
    "github.com/anthropics/",     // 反馈句里的 Anthropic 仓库链接
    "11128",                      // 上游反探测：裸数字错误码
];

/// 改写层：全模板句逐字替换（每句只改一个词，语义不变）。
/// 匹配是大小写敏感的字面替换（对应 Go `strings.ReplaceAll`，与预检同口径）。
const SANITIZE_REWRITES: &[(&str, &str)] = &[
    // 身份句：真实模板无 tool 一词（上游黑名单按原句逐字拦截），插入后破坏匹配。
    (
        "You are Claude Code, Anthropic's official CLI for Claude",
        "You are Claude Code, Anthropic's official CLI tool for Claude",
    ),
    // git 上下文注入句：客户端注入 "Main branch (you will usually use this
    // for PRs)"，换词后语义不变（预检特征 "Main branch (" 是入站形态）。
    (
        "Main branch (you will usually use this for PRs)",
        "Default branch (you will usually use this for PRs)",
    ),
    // Codex instructions 首段：身份句同型插词。
    (
        "You are a coding agent running in the Codex CLI, a terminal-based coding assistant.",
        "You are a coding agent running in the Codex CLI tool, a terminal-based coding assistant.",
    ),
    (
        // 反馈句：整句带 Anthropic 仓库链接，上游按整句拦截（只留链接或只留
        // 半边均不拦，实测需整句同时出现）。give→provide 一词之差即可绕过。
        "To give feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
        "To provide feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
    ),
    (
        // 上游反探测：请求体里出现裸数字 11128 就整单拦截（与该数字的上下文
        // 无关："code=11128" / 裸 "11128" / "错误码 11128" 全部命中）。
        // 11128 正是本类拦截自身的错误码，上游据此识别"在讨论/回显其内部
        // 错误码"的请求。代价：用户对话中任何 11128 都会被改写——但这串数字
        // 出现在请求里本身就是拦截条件，不改写必然失败。插一个连字符保留
        // 可读性与指代（零宽类字符无效——实测上游会归一化剔除）。
        //
        // 注意：from 是**纯数字**，不是 `11-128`。参考实现源码的 from 串曾
        // 被其自身规则改写成 to 串，核对时以参考注释明确记载的原文 `11128`
        // 为准，不要把改写后的形态当成指纹原文。
        "11128",
        "11-128",
    ),
];

// ─── 正则（惰性编译一次）────────────────────────────────────
//
// `\s` 写成显式的 `[\t\n\f\r ]`、`\b` 用 `(?-u:...)` 关掉 Unicode 模式：
// 参考实现（Go 移植）里这两个类都是**纯 ASCII** 语义，与 Go 逐字对齐，
// 否则「上游实测能绕过」这条结论就不再成立。

fn hdr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 剥离层：表头键名即触发（与值无关），键值整段删除。
        // 值以分号为分段边界：`[^;\n]*` 而不是 `[^\n]*`，否则同一行分号后的
        // 其它键值也会被一并吞掉（过度删除）。
        Regex::new(r"(?i)x-anthropic-billing-header[\t\n\r ]*:[^;\n]*;?[\t\n\f\r ]*")
            .expect("header 剥离正则必须可编译")
    })
}

fn kv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 剥离层：`cc_*=值` 键值段整段删除（尾随裸键值由调用方循环清理）。
        Regex::new(r"(?i)(?-u:\b)cc_[a-z0-9_]+=[^;\n]*;?[\t\n\f\r ]*")
            .expect("cc_ 键值正则必须可编译")
    })
}

fn bare_hdr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 兜底层：裸键名（无冒号无值）同样是指纹——实测 assistant 消息里
        // 反引号引用裸键名即触发 11-128，而剥离层要求冒号、对裸串无效。
        // 键值形态被整段删除后，残留的裸键名做最小缩写（header→hdr）：
        // 破坏逐字匹配、语义不变、保留可读性。大小写不敏感，覆盖变体。
        Regex::new(r"(?i)x-anthropic-billing-header").expect("裸 header 正则必须可编译")
    })
}

/// 裸键名缩写目标（参考注释"最小缩写 header→hdr"）。
const BARE_HDR_REPLACEMENT: &str = "x-anthropic-billing-hdr";

/// 单段文本净化。预检不中 → 原串返回（零分配）。
///
/// 顺序照抄参考实现：改写层 → 表头剥离层 → cc_ 键值循环清理 →
/// 裸键名缩写兜底。**顺序不能换**：改写在前是因为它要求原串完整（表头
/// 剥离会动到同一段文本的其它位置）；cc_ 循环在裸键名缩写之前，是因为
/// 缩写会把键名改掉、再清 cc_ 就认不出来了。
/// 与参考的差异：结尾不做 `trim()`（最小改写原则，不触碰未命中段的首尾空白）。
pub fn sanitize_text(text: &str) -> String {
    if !has_fingerprint(text) {
        return text.to_string();
    }
    // P2-11 命中统计：进入净化即计一次
    STATS_HITS.fetch_add(1, Ordering::Relaxed);
    let mut out = text.to_string();
    for (from, to) in SANITIZE_REWRITES {
        if out.contains(from) {
            tracing::debug!("指纹脱敏改写命中: {from:?}");
            STATS_REWRITES.fetch_add(1, Ordering::Relaxed);
            out = out.replace(from, to);
        }
    }
    let hdr = hdr_re();
    if hdr.is_match(&out) {
        tracing::debug!("指纹脱敏剥离 x-anthropic-billing-hdr 键值段");
        out = hdr.replace_all(&out, "").into_owned();
    }
    if out.contains("cc_") {
        let kv = kv_re();
        let mut prev = String::new();
        // 清尾随裸 kv：一次替换可能因相邻键值的新边界又暴露出下一个，
        // 循环到不动为止
        while prev != out {
            prev = out.clone();
            out = kv.replace_all(&out, "").into_owned();
        }
    }
    // 兜底：键值形态已在上面整段删除，这里只剩裸键名（引用/示例文本形态）。
    if bare_hdr_re().is_match(&out) {
        tracing::debug!("指纹脱敏缩写裸键名 x-anthropic-billing-hdr");
        out = bare_hdr_re()
            .replace_all(&out, BARE_HDR_REPLACEMENT)
            .into_owned();
    }
    out
}

/// 特征预检：先走 `contains` 快速路径（零分配）。`cc_entrypoint=` 是独立的
/// 裸键值特征（可能没有表头键名，必须单列否则整条净化被跳过）；表头键名还有
/// 大小写变体与裸键名形态，`contains` 大小写敏感会漏，必须再用 `(?i)` 正则兜底。
fn has_fingerprint(text: &str) -> bool {
    SANITIZE_FEATURES
        .iter()
        .any(|feature| text.contains(feature))
        || bare_hdr_re().is_match(text)
}

/// 净化一段多模态 content：字符串，或数组（只动带 `text` 字符串字段的分片）。
/// 与参考一致，**不判 `type`**：任何带 `text` 字符串的分片都过一遍。
fn sanitize_content(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let next = sanitize_text(text);
            if next != *text {
                *text = next;
            }
        }
        serde_json::Value::Array(parts) => {
            for part in parts.iter_mut() {
                let Some(object) = part.as_object_mut() else {
                    continue;
                };
                if let Some(serde_json::Value::String(text)) = object.get_mut("text") {
                    let next = sanitize_text(text);
                    if next != *text {
                        *text = next;
                    }
                }
            }
        }
        _ => {}
    }
}

/// 净化整个请求体（serde_json 形态，供 Responses/Completions 等直接持有
/// `Value` 的转发路径使用；`strip(body)` 签名对齐任务契约）。
///
/// 覆盖 `messages[]` 的 content / reasoning_content / tool_calls 参数
/// （arguments 是字符串化的 JSON，按文本净化即可）——与参考实现的作用面
/// 一致：顶层 `system`、`input` 等不在规则集的作用面。
pub fn strip(body: &mut serde_json::Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        // content 与 tool_calls 各自独立判断：content 可为 null（工具调用轮），
        // 不能因 content 缺失跳过整条消息。
        if let Some(content) = object.get_mut("content") {
            sanitize_content(content);
        }
        // reasoning_content（思维链回填字段）实测同样携带指纹，同等净化。
        if let Some(serde_json::Value::String(reasoning)) = object.get_mut("reasoning_content") {
            let next = sanitize_text(reasoning);
            if next != *reasoning {
                *reasoning = next;
            }
        }
        if let Some(call_list) = object.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
            for call in call_list.iter_mut() {
                if let Some(serde_json::Value::String(arguments)) = call
                    .get_mut("function")
                    .and_then(|f| f.get_mut("arguments"))
                {
                    let next = sanitize_text(arguments);
                    if next != *arguments {
                        *arguments = next;
                    }
                }
            }
        }
    }
}

/// 净化归一化消息列表（`ChatFormat` 无 serde 派生，桥接路径直接遍历）。
///
/// 覆盖 content / 多模态 content_blocks 分片的 text / reasoning /
/// tool_calls 参数——与 [`strip`] 的作用面对齐。openai `chat_completions`
/// 与 anthropic `messages` 两个主力端点共用本入口。
pub fn strip_chat_messages(messages: &mut [ChatMessage]) {
    for message in messages.iter_mut() {
        if let Some(content) = message.content.as_deref() {
            let next = sanitize_text(content);
            if next != content {
                message.content = Some(next);
            }
        }
        if let Some(blocks) = message.content_blocks.as_deref_mut() {
            for block in blocks.iter_mut() {
                if let Some(serde_json::Value::String(text)) = block.get_mut("text") {
                    let next = sanitize_text(text);
                    if next != *text {
                        *text = next;
                    }
                }
            }
        }
        if let Some(reasoning) = message.reasoning.as_deref() {
            let next = sanitize_text(reasoning);
            if next != reasoning {
                message.reasoning = Some(next);
            }
        }
        if let Some(calls) = message.tool_calls.as_deref_mut() {
            for call in calls.iter_mut() {
                let next = sanitize_text(&call.arguments);
                if next != call.arguments {
                    call.arguments = next;
                }
            }
        }
    }
}

/// 命中统计快照（P2-11）：进入净化的请求数 + 改写应用次数。
pub fn snapshot() -> serde_json::Value {
    serde_json::json!({
        "hits": STATS_HITS.load(Ordering::Relaxed),
        "rewrites": STATS_REWRITES.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatMessage, Role, ToolCall};

    /// 构造一条带文本的桥接消息，减少测试样板。
    fn msg(role: Role, content: Option<&str>) -> ChatMessage {
        ChatMessage {
            role,
            content: content.map(str::to_string),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    #[test]
    fn no_fingerprint_returns_input() {
        // Given 一段不含任何指纹特征的普通文本
        let text = "hello world, nothing to sanitize";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 一个字节都不改（预检零分配快速路径）
        assert_eq!(out, text);
    }

    #[test]
    fn numeric_error_code_11128_rewritten() {
        // Given 上游反探测的裸错误码原文
        let text = "upstream returned code 11128";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 必须破坏逐字匹配，而不是把真正的指纹漏放出去
        assert_eq!(out, "upstream returned code 11-128");
        assert!(!out.contains("11128"));
    }

    #[test]
    fn hyphenated_version_range_untouched() {
        // Given 用户文本里本来就是带连字符的版本区间（不是指纹）
        let text = "supported versions: 11-128";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 预检不命中，原样返回（避免误伤正常对话）
        assert_eq!(out, text);
    }

    #[test]
    fn standalone_cc_entrypoint_kv_removed() {
        // Given 只有 cc_ 键值、没有表头键名的一段文本（旧预检会漏）
        let text = "cc_entrypoint=cli;";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 整段删除，而不是因为预检不中而原样漏出
        assert_eq!(out, "");
    }

    #[test]
    fn header_kv_segment_removed_keeps_tail() {
        // Given 表头键值段后还有其它非指纹内容
        let text = "x-anthropic-billing-header: cc_version=2.1.0; keep this";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只删到分号为止，后面的内容不能被吞掉
        assert_eq!(out, "keep this");
    }

    #[test]
    fn bare_header_key_abbreviated() {
        // Given assistant 历史里反引号引用的裸键名（无冒号无值）
        let text = "refer to `x-anthropic-billing-header` in docs";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 最小缩写，破坏逐字匹配且保留可读性
        assert_eq!(out, "refer to `x-anthropic-billing-hdr` in docs");
    }

    #[test]
    fn mixed_case_bare_header_abbreviated() {
        // Given 大小写变体（contains 快速路径不命中，靠 (?i) 正则兜底）
        let text = "X-Anthropic-Billing-Header";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 仍然缩写为统一形态
        assert_eq!(out, "x-anthropic-billing-hdr");
    }

    #[test]
    fn claude_identity_sentence_rewritten() {
        // Given 客户端注入的身份句（带句号收尾）
        let text = "You are Claude Code, Anthropic's official CLI for Claude.";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只插一个词，原有句尾标点保留
        assert_eq!(
            out,
            "You are Claude Code, Anthropic's official CLI tool for Claude."
        );
    }

    #[test]
    fn multimodal_text_block_sanitized() {
        // Given content 为多模态数组，只有 text 分片带指纹
        let mut value = serde_json::json!([
            {"type": "text", "text": "cc_entrypoint=cli;"},
            {"type": "image_url", "image_url": {"url": "http://example.com/a.png"}}
        ]);
        // When 走 content 净化
        sanitize_content(&mut value);
        // Then 只改带 text 的分片，非文本分片原样保留
        assert_eq!(value[0]["text"], "");
        assert_eq!(value[1]["image_url"]["url"], "http://example.com/a.png");
    }

    #[test]
    fn tool_call_arguments_sanitized() {
        // Given 工具调用消息的 content 为 null、arguments 是字符串化 JSON
        let mut body = serde_json::json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "function": {
                        "name": "Read",
                        "arguments": "{\"cmd\":\"echo cc_entrypoint=cli;\"}"
                    }
                }]
            }]
        });
        // When 走请求体净化
        strip(&mut body);
        // Then arguments 里的键值段被剥离（不能因 content 缺失跳过整条消息）
        let arguments = body["messages"][0]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments 应为字符串");
        assert!(!arguments.contains("cc_entrypoint="));
    }

    #[test]
    fn reasoning_content_sanitized() {
        // Given assistant 消息的 reasoning_content 携带指纹
        let mut body = serde_json::json!({
            "messages": [{
                "role": "assistant",
                "content": "ok",
                "reasoning_content": "code 11128"
            }]
        });
        // When 走请求体净化
        strip(&mut body);
        // Then 与 content 同等净化
        assert_eq!(body["messages"][0]["reasoning_content"], "code 11-128");
    }

    #[test]
    fn chat_message_fields_sanitized() {
        // Given content / content_blocks / reasoning / tool_calls 各带一处指纹
        let mut message = msg(Role::Assistant, Some("plain"));
        message.content_blocks = Some(vec![serde_json::json!({
            "type": "text",
            "text": "cc_entrypoint=cli;"
        })]);
        message.reasoning = Some("code 11128".to_string());
        message.tool_calls = Some(vec![ToolCall {
            id: "c1".to_string(),
            function_name: "Read".to_string(),
            arguments: "{\"p\":\"cc_entrypoint=cli;\"}".to_string(),
        }]);
        let mut messages = [message];
        // When 走桥接消息净化
        strip_chat_messages(&mut messages);
        // Then 四类字段全部生效
        assert_eq!(messages[0].content.as_deref(), Some("plain"));
        assert_eq!(messages[0].content_blocks.as_ref().unwrap()[0]["text"], "");
        assert_eq!(messages[0].reasoning.as_deref(), Some("code 11-128"));
        let tool_arguments = &messages[0].tool_calls.as_ref().unwrap()[0].arguments;
        assert!(!tool_arguments.contains("cc_entrypoint="));
    }

    #[test]
    fn leading_trailing_whitespace_preserved() {
        // Given 带首尾空白的文本（AIGX 有意区别于参考实现的末尾 trim）
        let text = "  cc_entrypoint=cli;x  ";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只删键值段，不触碰未命中段的首尾空白
        assert_eq!(out, "  x  ");
    }
}
