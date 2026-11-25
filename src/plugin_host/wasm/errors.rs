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
            reason: None,
            retry_after: None,
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

/// 把 wasmtime 的错误链折成一行可读文本（按字符边界截断到 512 字节）。
///
/// `wasmtime::Error` 是 anyhow 风格错误链：`Display` 只给最外层，真实原因
/// 常在内层（`caused by`）。`{:#}` 是 anyhow 的「一行链式」格式，最外层与
/// 根因都在同一行；非 anyhow 类型会忽略该 flag，退化成普通 `Display`。
fn describe(error: &wasmtime::Error) -> String {
    let mut text = format!("{error:#}");
    if text.len() > 512 {
        let mut cut = 512;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push('…');
    }
    text
}

pub(crate) fn interrupted(error: &wasmtime::Error) -> WireError {
    let (code, message) = match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel | wasmtime::Trap::StackOverflow) => (
            "plugin_resource_limit",
            "Wasm 插件超过计算或栈限制".to_string(),
        ),
        Some(_) => ("plugin_trap", "Wasm 插件执行中断".to_string()),
        // 非 Trap 的 wasmtime 错误：宿主函数返回 Err（消息越界 / 不是 JSON /
        // 回应过大）、实例化与导出类型不匹配、内存写入越界、结果反序列化失败
        // 都落在这一支。原先这里折成一句「超过资源限制或返回了无效消息」，
        // **真实原因被整段丢掉** —— e2e 在 CI 上挂 180s 之后只能拿到这句话，
        // 排障只能猜。现在把原因链带出来（截断到 512 字节）。
        None => (
            "plugin_protocol_error",
            format!("Wasm 插件调用失败：{}", describe(error)),
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

    /// 非 Trap 的 wasmtime 错误必须**带上原因**，不能只剩一句笼统文案。
    ///
    /// 这条分支覆盖宿主函数返回 Err / 实例化失败 / 内存越界 / 结果反序列化
    /// 失败等一大类原因，彼此的处置完全不同。此前它们全被压成
    /// 「Wasm 插件超过资源限制或返回了无效消息」—— e2e 在 CI 上挂 180s 之后
    /// 拿到的就是这句话，真实原因一个字都没有。
    #[test]
    fn non_trap_error_carries_its_own_reason() {
        let error = wasmtime::Error::msg("memory minimum size of 3 pages exceeds memory limits");
        let wire = interrupted(&error);
        assert_eq!(wire.code.as_deref(), Some("plugin_protocol_error"));
        assert!(
            wire.message.contains("memory minimum size"),
            "原因必须透出，实际: {}",
            wire.message
        );
    }

    /// 原因过长时按字符边界截断，不能切出半个 UTF-8（也不许整句丢掉）。
    #[test]
    fn long_reason_is_truncated_on_char_boundary() {
        let error = wasmtime::Error::msg("超".repeat(400));
        let wire = interrupted(&error);
        assert!(
            wire.message.starts_with("Wasm 插件调用失败：超"),
            "{}",
            wire.message
        );
        assert!(wire.message.ends_with('…'), "长原因必须带截断标记");
        assert!(wire.message.len() < 1200, "{}", wire.message.len());
    }
}
