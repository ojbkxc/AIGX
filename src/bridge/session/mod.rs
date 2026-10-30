//! 登录态供应商的**私有协议** Bridge 目录。
//!
//! `registry::SessionMeta` 里 `openai_compatible = false` 的那些上游，
//! 请求体与响应帧都不是 OpenAI 方言，不能用 `OpenAiCompatibleBridge`
//! 转发；每种协议族在这里各占一个模块，实现统一的 [`Bridge`] trait。
//!
//! 已接入：OrcaTerm。其余家按同一形态逐步补齐（每家的协议细节抄自
//! Agent2API 对应 provider 的 adapter）。

use std::sync::Arc;

use crate::bridge::Bridge;

pub mod accio;
pub mod antigravity;
pub mod orcaterm;

/// 按 provider kind 构造私有协议 Bridge；不支持的 kind 返回 None。
pub fn build(
    kind: &str,
    base: &str,
    credential: &str,
    client: reqwest::Client,
) -> Option<Arc<dyn Bridge>> {
    match kind {
        "orcaterm" => Some(Arc::new(orcaterm::OrcaTermBridge::new(
            base, credential, client,
        ))),
        "antigravity" => Some(Arc::new(antigravity::AntigravityBridge::new(
            base, credential, client,
        ))),
        "accio" => Some(Arc::new(accio::AccioBridge::new(
            base, credential, client,
        ))),
        _ => None,
    }
}

/// 是否为已实现的私有协议供应商。
pub fn supports(kind: &str) -> bool {
    matches!(kind, "orcaterm" | "antigravity" | "accio")
}
