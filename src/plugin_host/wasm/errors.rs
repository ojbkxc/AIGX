// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件应答解码 —— `{"result": ...}` 解包、`{"error": {...}}` 还原成 [`WireError`]。
//!
//! 与上游逐字一致（只把 `proto::WireError` 换成本地 [`WireError`]）：
//! 插件返回的 error 对象可以带 `status`（上游 HTTP 状态）、`upstream_code`、
//! `reset_at`、`credential_scope`，网关把它们原样编码进 `provider_http:<json>`
//! 错误码，交给上层的错误分类器识别。

use serde::Deserialize;
use serde_json::Value;

use super::wire::WireError;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct Failure {
    message: String,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    upstream_code: Option<i64>,
    #[serde(default)]
    reset_at: Option<i64>,
    #[serde(default)]
    credential_scope: Option<String>,
}

pub(crate) fn decode(mut value: Value) -> Result<Value, WireError> {
    if let Some(error) = value.get("error") {
        let error: Failure = serde_json::from_value(error.clone()).map_err(|_| protocol())?;
        if error.message.len() > 16 * 1024
            || error.code.as_ref().is_some_and(|code| code.len() > 4096)
        {
            return Err(protocol());
        }
        if let Some(status) = error.status {
            if !(400..600).contains(&status) {
                return Err(protocol());
            }
            return Err(WireError::with_code(
                error.message,
                format!(
                    "provider_http:{}",
                    serde_json::json!({
                        "status": status, "code": error.code, "upstreamCode": error.upstream_code,
                        "resetAt": error.reset_at, "credentialScope": error.credential_scope,
                    })
                ),
            ));
        }
        return Err(WireError {
            message: error.message,
            code: error.code,
        });
    }
    value
        .as_object_mut()
        .and_then(|object| object.remove("result"))
        .ok_or_else(protocol)
}

pub(crate) fn protocol() -> WireError {
    WireError::with_code("Wasm 插件返回了无效的协议消息", "plugin_protocol_error")
}

pub(crate) fn interrupted(error: &wasmtime::Error) -> WireError {
    let (code, message) = match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::StackOverflow) => {
            ("plugin_resource_limit", "Wasm 插件超过计算或栈限制")
        }
        Some(_) => ("plugin_trap", "Wasm 插件执行中断"),
        None => (
            "plugin_protocol_error",
            "Wasm 插件超过资源限制或返回了无效消息",
        ),
    };
    WireError::with_code(message, code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwraps_result_envelope() {
        let value = decode(json!({"result": {"ok": true}})).unwrap();
        assert_eq!(value, json!({"ok": true}));
    }

    #[test]
    fn missing_result_is_protocol_error() {
        let error = decode(json!({"nope": 1})).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("plugin_protocol_error"));
    }

    #[test]
    fn error_with_status_becomes_provider_http() {
        let error = decode(json!({"error": {"message": "quota", "status": 429}})).unwrap_err();
        let code = error.code.unwrap_or_default();
        assert!(code.starts_with("provider_http:"), "{code}");
        assert!(code.contains("\"status\":429"));
    }

    #[test]
    fn out_of_range_status_rejected() {
        let error = decode(json!({"error": {"message": "x", "status": 200}})).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("plugin_protocol_error"));
    }

    #[test]
    fn plain_error_keeps_code() {
        let error = decode(json!({"error": {"message": "no", "code": "forbidden"}})).unwrap_err();
        assert_eq!(error.message, "no");
        assert_eq!(error.code.as_deref(), Some("forbidden"));
    }
}
