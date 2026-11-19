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
//! - `account_origin_authorization_required` → 403（账号来源需授权）
//! - `account_origin_proxy_unsupported` → 409（账号来源不支持代理）

use serde::{Deserialize, Serialize};

/// 插件调用的错误应答（上游 `WireError` 同形）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WireError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
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
        }
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
        })
        .unwrap();
        assert!(bare.get("code").is_none());
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
}
