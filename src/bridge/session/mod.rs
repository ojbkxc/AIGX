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
pub mod catpaw;
pub mod codearts;
pub mod commandcode;
pub mod kuku;
pub mod monkeycode;
pub mod orcaterm;
pub mod qoder;
pub mod qoder_cosy;
pub mod trae;

/// 按 provider kind 构造私有协议 Bridge；不支持的 kind 返回 None。
///
/// `headers` 是 provider 默认头 + 账号级额外头的合并结果（`{credential}`
/// 已替换）；需要私有身份的供应商（如 Trae 的 `X-Uid`）靠它拿值。
pub fn build(
    kind: &str,
    base: &str,
    credential: &str,
    headers: Vec<(String, String)>,
    client: reqwest::Client,
) -> Option<Arc<dyn Bridge>> {
    match kind {
        "orcaterm" => Some(Arc::new(orcaterm::OrcaTermBridge::new(
            base, credential, client,
        ))),
        "antigravity" => Some(Arc::new(antigravity::AntigravityBridge::new(
            base, credential, client,
        ))),
        "accio" => Some(Arc::new(accio::AccioBridge::new(base, credential, client))),
        "commandcode" => Some(Arc::new(commandcode::CommandCodeBridge::new(
            base, credential, client,
        ))),
        "kuku" => Some(Arc::new(kuku::KukuBridge::new(base, credential, client))),
        "qoder" => Some(Arc::new(qoder::QoderBridge::new(base, credential, client))),
        "codearts" => Some(Arc::new(codearts::CodeArtsBridge::new(
            base, credential, client,
        ))),
        "catpaw" => Some(Arc::new(catpaw::CatPawBridge::new(
            base, credential, client,
        ))),
        "monkeycode" => Some(Arc::new(monkeycode::MonkeyCodeBridge::new(
            base, credential, client,
        ))),
        "trae" => Some(Arc::new(trae::TraeBridge::new(
            base, credential, headers, client,
        ))),
        _ => None,
    }
}

/// 去掉用户从抓包整段复制时带的前缀（大小写不敏感），并去掉首尾空白。
///
/// 例如 `Authorization: Bearer eyJ…` / `x-passport-token=…` / `Cookie: a=b`。
pub fn normalize_credential<'a>(raw: &'a str, prefixes: &[&str]) -> &'a str {
    let mut value = raw.trim();
    loop {
        let mut matched = false;
        for prefix in prefixes {
            if value.len() < prefix.len() {
                continue;
            }
            if value[..prefix.len()].eq_ignore_ascii_case(prefix) {
                value = value[prefix.len()..].trim_start();
                matched = true;
                break;
            }
        }
        if !matched {
            return value;
        }
    }
}

/// 是否为已实现的私有协议供应商。
pub fn supports(kind: &str) -> bool {
    matches!(
        kind,
        "orcaterm"
            | "antigravity"
            | "accio"
            | "commandcode"
            | "kuku"
            | "trae"
            | "qoder"
            | "codearts"
            | "catpaw"
            | "monkeycode"
    )
}
