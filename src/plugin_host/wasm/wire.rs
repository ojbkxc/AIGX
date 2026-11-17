// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
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
}
