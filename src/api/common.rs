//! API 公共辅助函数（H8 抽取自 `openai.rs` / `anthropic.rs`）。
//!
//! 提供 API Key 与客户端 IP 提取等共享工具，消除 openai/anthropic 模块间的重复实现。
//!
//! ## 关于 `extract_api_key` 的两个变体
//!
//! OpenAI 与 Anthropic 协议对 API Key 头的优先级不同：
//! - OpenAI 习惯 `Authorization: Bearer <key>`，故 `extract_api_key_bearer_first` 先查 Bearer。
//! - Anthropic 标准 `x-api-key`，故 `extract_api_key_xapi_first` 先查 x-api-key。
//!
//! 当两个头同时存在时优先级会影响返回哪个 key，故保留两个变体以保持原行为不变。
//!
//! ## 关于 `verify_api_key_full`
//!
//! 该函数在 openai.rs / anthropic.rs 中逻辑相同，但错误响应格式不同
//! （OpenAI `error_response` vs Anthropic `anthropic_error`），且依赖各自模块的
//! `AppState` 引用与错误构造器。强行抽取需引入泛型错误 trait，收益有限且易破坏
//! API 兼容，故保留在各自模块内。本模块只抽取无争议的纯工具函数。

use axum::http::HeaderMap;

/// 从请求中提取 API Key（OpenAI 优先顺序：`Authorization: Bearer` 优先，再 `x-api-key`）。
///
/// 抽取自 `openai.rs`，签名与行为保持不变。
pub fn extract_api_key_bearer_first(headers: &HeaderMap) -> Option<String> {
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(key_str) = key.to_str() {
            return Some(key_str.to_string());
        }
    }
    None
}

/// 从请求中提取 API Key（Anthropic 优先顺序：`x-api-key` 优先，再 `Authorization: Bearer`）。
///
/// 抽取自 `anthropic.rs`，签名与行为保持不变。
pub fn extract_api_key_xapi_first(headers: &HeaderMap) -> Option<String> {
    // x-api-key header (Anthropic standard)
    if let Some(key) = headers.get("x-api-key") {
        if let Ok(key_str) = key.to_str() {
            return Some(key_str.to_string());
        }
    }
    // Authorization: Bearer sk-xxx (OpenAI compatible)
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(key) = auth_str.strip_prefix("Bearer ") {
                return Some(key.to_string());
            }
        }
    }
    None
}

/// 从请求头提取客户端 IP（取 `X-Forwarded-For` 首段或 `X-Real-IP`）。
///
/// `openai.rs` 与 `anthropic.rs` 原实现完全一致，合并于此消除重复。
pub fn extract_client_ip(headers: &HeaderMap) -> Option<String> {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = xff.split(',').next() {
            let ip = first.trim();
            if !ip.is_empty() {
                return Some(ip.to_string());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
