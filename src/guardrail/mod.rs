//! 护栏模块 — 借鉴 aisix-guardrails
//!
//! 提供关键词 + 正则表达式黑名单护栏，可应用于输入消息和输出内容。
//! 两种模式可混合使用：
//! - `Literal(s)`: 大小写不敏感子串匹配
//! - `Regex(re)`: 构造时编译一次的正则表达式
//!
//! 应用于输入消息（拼接所有消息内容）和输出内容。
//! 无论触发哪一侧，裁决都携带匹配的规则文本在 `reason` 中，
//! 方便运营人员从日志中调试。

use async_trait::async_trait;

use crate::bridge::{ChatFormat, ChatResponse};

pub mod keyword;

/// 护栏裁决结果
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardrailVerdict {
    /// 放行
    Allow,
    /// 拦截，附带原因
    Block { reason: String },
}

impl GuardrailVerdict {
    pub fn block(reason: impl Into<String>) -> Self {
        Self::Block {
            reason: reason.into(),
        }
    }

    pub fn is_block(&self) -> bool {
        matches!(self, Self::Block { .. })
    }
}

/// 护栏 trait。每个护栏实现负责：
/// - `check_input`: 检查请求消息内容
/// - `check_output`: 检查响应内容
#[async_trait]
pub trait Guardrail: Send + Sync + 'static {
    /// 护栏名称，用于日志和指标
    fn name(&self) -> &'static str;

    /// 是否检查输出内容。输入检查总是执行；输出检查仅当返回 true 时执行。
    /// 只检查输入的护栏返回 false 可避免不必要的流式输出缓冲。
    fn runs_on_output(&self) -> bool {
        true
    }

    /// 检查输入消息
    async fn check_input(&self, req: &ChatFormat) -> GuardrailVerdict;

    /// 检查输出响应
    async fn check_output(&self, resp: &ChatResponse) -> GuardrailVerdict {
        let _ = resp;
        GuardrailVerdict::Allow
    }
}
