//! 关键词 + 正则表达式黑名单 — 借鉴 aisix-guardrails/src/keyword.rs
//!
//! 两种模式可混合使用：
//! - `Literal(s)`: 大小写不敏感子串匹配
//! - `Regex(re)`: 构造时编译一次；大小写敏感由调用方控制（在正则中使用 `(?i)`）

use async_trait::async_trait;
use regex::Regex;

use super::{Guardrail, GuardrailVerdict};
use crate::bridge::ChatFormat;
use crate::bridge::ChatResponse;

#[derive(Debug, Clone)]
pub enum KeywordRule {
    Literal(String),
    Regex(Regex),
}

impl KeywordRule {
    pub fn literal(s: impl Into<String>) -> Self {
        Self::Literal(s.into())
    }

    pub fn regex(pattern: &str) -> Result<Self, regex::Error> {
        Regex::new(pattern).map(Self::Regex)
    }

    fn matches(&self, haystack: &str) -> bool {
        match self {
            KeywordRule::Literal(needle) => {
                let h = haystack.to_lowercase();
                let n = needle.to_lowercase();
                !needle.is_empty() && h.contains(&n)
            }
            KeywordRule::Regex(re) => re.is_match(haystack),
        }
    }

    fn description(&self) -> String {
        match self {
            KeywordRule::Literal(s) => format!("literal {s:?}"),
            KeywordRule::Regex(r) => format!("regex /{}/", r.as_str()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeywordBlocklist {
    rules: Vec<KeywordRule>,
    /// 是否对输入消息启用检查
    pub check_input_enabled: bool,
    /// 是否对输出内容启用检查
    pub check_output_enabled: bool,
}

impl KeywordBlocklist {
    pub fn new(rules: Vec<KeywordRule>) -> Self {
        Self {
            rules,
            check_input_enabled: true,
            check_output_enabled: true,
        }
    }

    pub fn input_only(rules: Vec<KeywordRule>) -> Self {
        Self {
            rules,
            check_input_enabled: true,
            check_output_enabled: false,
        }
    }

    pub fn output_only(rules: Vec<KeywordRule>) -> Self {
        Self {
            rules,
            check_input_enabled: false,
            check_output_enabled: true,
        }
    }

    fn first_match(&self, text: &str) -> Option<&KeywordRule> {
        self.rules.iter().find(|r| r.matches(text))
    }
}

#[async_trait]
impl Guardrail for KeywordBlocklist {
    fn name(&self) -> &'static str {
        "keyword_blocklist"
    }

    /// 只有实际检查输出的黑名单才需要流式输出缓冲
    fn runs_on_output(&self) -> bool {
        self.check_output_enabled
    }

    async fn check_input(&self, req: &ChatFormat) -> GuardrailVerdict {
        if !self.check_input_enabled {
            return GuardrailVerdict::Allow;
        }
        // 拼接所有消息内容 — 分别检查每条消息不会更高效，
        // 因为规则不跨消息。
        let combined: String = req
            .messages
            .iter()
            .map(|m| m.content_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        match self.first_match(&combined) {
            Some(rule) => {
                GuardrailVerdict::block(format!("input blocked by {}", rule.description()))
            }
            None => GuardrailVerdict::Allow,
        }
    }

    async fn check_output(&self, resp: &ChatResponse) -> GuardrailVerdict {
        if !self.check_output_enabled {
            return GuardrailVerdict::Allow;
        }
        let text = resp.message.content_str();
        match self.first_match(text) {
            Some(rule) => {
                GuardrailVerdict::block(format!("output blocked by {}", rule.description()))
            }
            None => GuardrailVerdict::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatMessage, FinishReason, Role, UsageStats};

    fn req(messages: &[(&str, &str)]) -> ChatFormat {
        let msgs = messages
            .iter()
            .map(|(role, content)| {
                let role = match *role {
                    "system" => Role::System,
                    "user" => Role::User,
                    _ => Role::Assistant,
                };
                ChatMessage {
                    role,
                    content: Some(content.to_string()),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                }
            })
            .collect();
        ChatFormat {
            model: "test".into(),
            messages: msgs,
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        }
    }

    fn resp(content: &str) -> ChatResponse {
        ChatResponse {
            id: "r".into(),
            model: "m".into(),
            message: ChatMessage::assistant(content),
            finish_reason: FinishReason::Stop,
            usage: UsageStats::new(0, 0),
        }
    }

    #[tokio::test]
    async fn literal_match_is_case_insensitive() {
        let g = KeywordBlocklist::new(vec![KeywordRule::literal("Forbidden")]);
        let v = g
            .check_input(&req(&[("user", "say the FORBIDDEN word")]))
            .await;
        assert!(v.is_block());
    }

    #[tokio::test]
    async fn empty_literal_pattern_never_matches() {
        let g = KeywordBlocklist::new(vec![KeywordRule::literal("")]);
        let v = g.check_input(&req(&[("user", "anything")])).await;
        assert_eq!(v, GuardrailVerdict::Allow);
    }

    #[tokio::test]
    async fn regex_pattern_matches() {
        let g = KeywordBlocklist::new(vec![
            KeywordRule::regex(r"\bssn:\s*\d{3}-\d{2}-\d{4}").unwrap()
        ]);
        let v = g
            .check_input(&req(&[("user", "the user's ssn: 123-45-6789 is")]))
            .await;
        assert!(v.is_block());
    }

    #[tokio::test]
    async fn no_match_returns_allow() {
        let g = KeywordBlocklist::new(vec![KeywordRule::literal("nothing here")]);
        let v = g.check_input(&req(&[("user", "hello world")])).await;
        assert_eq!(v, GuardrailVerdict::Allow);
    }

    #[tokio::test]
    async fn output_check_runs_against_response_content() {
        let g = KeywordBlocklist::new(vec![KeywordRule::literal("dangerous")]);
        let v = g.check_output(&resp("here is a dangerous answer")).await;
        assert!(v.is_block());
    }

    #[tokio::test]
    async fn input_only_skips_output_checks() {
        let g = KeywordBlocklist::input_only(vec![KeywordRule::literal("zeta")]);
        let v = g.check_output(&resp("zeta zeta zeta")).await;
        assert_eq!(v, GuardrailVerdict::Allow);
    }

    #[tokio::test]
    async fn output_only_skips_input_checks() {
        let g = KeywordBlocklist::output_only(vec![KeywordRule::literal("zeta")]);
        let v = g.check_input(&req(&[("user", "zeta zeta")])).await;
        assert_eq!(v, GuardrailVerdict::Allow);
    }

    #[test]
    fn invalid_regex_is_a_clean_error_not_a_panic() {
        assert!(KeywordRule::regex("[unclosed").is_err());
    }
}
