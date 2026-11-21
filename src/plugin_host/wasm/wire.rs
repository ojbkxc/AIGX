// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Wasm 插件线格式错误 —— 对齐上游 `provider-protocol` 的 `WireError` 形状。
//!
//! AIGX 没有上游的 `agent2api-provider-protocol` crate（它是 AGPL 且只在上游
//! 工作区里），Wasm 沙箱只需要其中**两个类型**：`WireError` 与
//! `ProviderInfo`（见 [`crate::plugin_host::wasm::host::ProviderInfo`]）。
//! 这里按上游字段名/序列化形状独立实现，保证插件看到的 JSON 完全一致：
//!
//! ```json
//! {"error": {"message": "...", "code": "forbidden"}}
//! ```
//!
//! ## beta.2 新增错误码
//!
//! - `plugin_busy` → 503（插件正在处理其他操作，等待超时）
//! - `plugin_resource_limit` → 503（Wasm 计算额度/栈/内存超限）
//! - `plugin_request_too_large` → 413（beta.3：正文/图片超过插件可处理尺寸，
//!   编排层应停止重试）
//! - `account_origin_authorization_required` → 403（账号来源需授权）
//! - `account_origin_proxy_unsupported` → 409（账号来源不支持代理）

//! ## beta.4 忙拒绝元数据（reason / retry_after）
//!
//! beta.4 的容量/锁忙拒绝在服务端以 `GatewayError` 承载 `reason`（拒绝原因码，
//! 如 `account_lock` / `concurrency_limit`）与 `retry_after`（建议重试秒数），
//! 并在把插件的 `plugin_busy` 文案还原时用 `classify_busy_message` 按文案反查。
//! AIGX 的 wasm 层没有 `GatewayError`（HTTP 错误层在 `plugin_host` 之外），也
//! 没有 beta.4 的 `runtime_capacity` 子系统，故本层把这两项作为 `WireError` 的
//! **可选**结构化字段**直接透传**：仅在确有原因时填，缺省（`None`）时序列化
//! 省略键，插件看到的 `{message, code}` 与上游逐字节一致（向后兼容）。

use serde::{Deserialize, Serialize};

/// 插件调用的错误应答（上游 `WireError` 同形，额外承载可选的忙拒绝元数据）。
///
/// 线格式（未填元数据时）：`{"error": {"message": "...", "code": "..."}}`；
/// 忙拒绝带原因时追加 `"reason"` / `"retry_after"`（均缺省省略）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// 拒绝原因码（beta.4 `GatewayError::with_reason` / `classify_busy_message`）。
    /// 缺省省略，保持与上游 `{message, code}` 的向后兼容。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 建议重试秒数（beta.4 `with_retry_after_seconds`）；`0` 视为不给、
    /// 缺省省略。仅在有 `reason` 时才有意义。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

impl WireError {
    /// 上游 `WireError::INTERNAL`。
    pub const INTERNAL: &'static str = "internal";
    /// 上游 `WireError::ABORTED`。
    pub const ABORTED: &'static str = "aborted";
    /// 上游 `WireError::NO_METHOD`。
    pub const NO_METHOD: &'static str = "no_method";

    /// beta.2：插件忙（正在处理其他操作，等待超时）。
    #[allow(dead_code)] // beta.2 错误码形状：待 bridge/adapter 接线后移除
    pub const PLUGIN_BUSY: &'static str = "plugin_busy";
    /// beta.2：插件资源超限（Wasm 计算额度/栈/内存/表/共享内存超限）。
    #[allow(dead_code)] // beta.2 错误码形状：待 bridge/adapter 接线后移除
    pub const PLUGIN_RESOURCE_LIMIT: &'static str = "plugin_resource_limit";
    /// beta.2：账号来源需独立授权。
    #[allow(dead_code)] // beta.2 错误码形状：待 account_origin 接线后移除
    pub const ACCOUNT_ORIGIN_AUTH_REQUIRED: &'static str = "account_origin_authorization_required";
    /// beta.2：账号来源不支持代理。
    #[allow(dead_code)] // beta.2 错误码形状：待 account_origin 接线后移除
    pub const ACCOUNT_ORIGIN_PROXY_UNSUPPORTED: &'static str = "account_origin_proxy_unsupported";

    pub fn with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
            reason: None,
            retry_after: None,
        }
    }

    /// 挂上拒绝原因码（beta.4 `GatewayError::with_reason` 的线格式对端）。
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// 挂上建议重试秒数（`0` 视为不给、保持省略，对齐 beta.4 响应侧口径）。
    pub fn with_retry_after(mut self, seconds: u64) -> Self {
        self.retry_after = Some(seconds).filter(|value| *value > 0);
        self
    }

    /// 构造一个 `plugin_busy` 忙拒绝，带结构化原因与建议重试秒数。
    ///
    /// beta.4 在 wasm 调用内只用 `with_code(message, "plugin_busy")`（原因靠文案
    /// 反查）；AIGX 直接把原因/重试一并带出，便于上层编排按 `reason` 决定重试。
    pub fn busy(message: impl Into<String>, reason: impl Into<String>, retry_after: u64) -> Self {
        Self::with_code(message, Self::PLUGIN_BUSY)
            .with_reason(reason)
            .with_retry_after(retry_after)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::with_code(message, Self::INTERNAL)
    }
}

impl From<String> for WireError {
    fn from(message: String) -> Self {
        Self::internal(message)
    }
}

impl From<&str> for WireError {
    fn from(message: &str) -> Self {
        Self::internal(message)
    }
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.code {
            Some(code) => write!(f, "{} ({code})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for WireError {}

/// 模块内最常用的「内部错误」构造（与上游 `super::error()` 同义）。
pub(crate) fn error(message: impl Into<String>) -> WireError {
    WireError::internal(message)
}

/// 错误码 → HTTP 状态码映射（对齐 beta.2 `rpc::rpc_error` 的 status 分支）。
///
/// 上游在 `rpc.rs` 里做这个映射；AIGX 的 `plugin_host` 不直接返回
/// `GatewayError`，故提供此函数供上层（bridge / adapter）调用。
///
/// 返回 `None` 表示「未识别的 code，用默认 502」。
///
/// 当前无内部调用方（供 bridge/adapter 接线的 beta.2 形状 API）。
#[allow(dead_code)]
pub fn http_status_for_code(code: &str) -> Option<u16> {
    match code {
        WireError::ABORTED => Some(499),
        "timeout" => Some(504),
        "plugin_unavailable" => Some(503),
        WireError::PLUGIN_BUSY => Some(503),
        WireError::PLUGIN_RESOURCE_LIMIT => Some(503),
        // beta.3：正文/图片超过插件能处理的尺寸：换个账号或重试都不会变好。
        // 413 让编排层直接停下，并把可读的尺寸说明交给调用方。
        "plugin_request_too_large" => Some(413),
        "forbidden" | WireError::ACCOUNT_ORIGIN_AUTH_REQUIRED => Some(403),
        WireError::ACCOUNT_ORIGIN_PROXY_UNSUPPORTED => Some(409),
        "bad_params" => Some(400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_shape_matches_upstream() {
        let value = serde_json::to_value(WireError::with_code("no access", "forbidden")).unwrap();
        assert_eq!(value["message"], "no access");
        assert_eq!(value["code"], "forbidden");
        // 无 code 时省略键（上游 skip_serializing_if 同口径）
        let value = serde_json::to_value(WireError::internal("boom")).unwrap();
        assert_eq!(value["code"], "internal");
        let bare = serde_json::to_value(WireError {
            message: "x".into(),
            code: None,
            reason: None,
            retry_after: None,
        })
        .unwrap();
        assert!(bare.get("code").is_none());
        // 缺省元数据不出现，线格式与上游 {message, code} 一致
        assert!(bare.get("reason").is_none());
        assert!(bare.get("retry_after").is_none());
    }

    #[test]
    fn busy_carries_reason_and_retry() {
        let value = serde_json::to_value(WireError::busy(
            "当前账号还有其他操作，等待账号锁超时，请稍后重试",
            "account_lock",
            1,
        ))
        .unwrap();
        assert_eq!(value["code"], WireError::PLUGIN_BUSY);
        assert_eq!(value["reason"], "account_lock");
        assert_eq!(value["retry_after"], 1);
        // 0 秒视为不给：不出现 retry_after，但 reason 仍在
        let zero = serde_json::to_value(
            WireError::with_code("busy", WireError::PLUGIN_BUSY)
                .with_reason("account_lock")
                .with_retry_after(0),
        )
        .unwrap();
        assert_eq!(zero["reason"], "account_lock");
        assert!(zero.get("retry_after").is_none());
    }

    #[test]
    fn from_string_is_internal() {
        let err: WireError = "bad".into();
        assert_eq!(err.code.as_deref(), Some(WireError::INTERNAL));
    }

    #[test]
    fn http_status_for_beta2_codes() {
        assert_eq!(http_status_for_code(WireError::PLUGIN_BUSY), Some(503));
        assert_eq!(
            http_status_for_code(WireError::PLUGIN_RESOURCE_LIMIT),
            Some(503)
        );
        assert_eq!(
            http_status_for_code(WireError::ACCOUNT_ORIGIN_AUTH_REQUIRED),
            Some(403)
        );
        assert_eq!(
            http_status_for_code(WireError::ACCOUNT_ORIGIN_PROXY_UNSUPPORTED),
            Some(409)
        );
        assert_eq!(http_status_for_code("forbidden"), Some(403));
        assert_eq!(http_status_for_code("bad_params"), Some(400));
        assert_eq!(http_status_for_code("unknown"), None);
    }

    #[test]
    fn http_status_for_beta3_codes() {
        // 正文/图片超限：413 让编排层停止重试（对齐上游 rpc_error）。
        assert_eq!(http_status_for_code("plugin_request_too_large"), Some(413));
    }
}
