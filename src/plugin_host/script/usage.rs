// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/usage.rs
//!
//! `script.usage` —— 脚本插件自报的用量（`environment.mjs` 的
//! `magpie.usage(id, value)` 经 RPC 发来）。
//!
//! A2A 把它写进 `RequestStats`（插件逐请求上报 token/耗时/错误）。AIGX 的
//! 请求日志模型是 [`crate::log::RequestLog`]（`LogStore::record_request`
//! 入异步批量写旁路），本模块做等价投影：把插件上报的字段拼成一条
//! `RequestLog` 入队。
//!
//! ## 形状校验（第三方内容，逐字段有界）
//!
//! `model` ≤200 / `kind` 为 `[a-z0-9_-]` ≤40 / 各计数钳到 `[0,1e12]` /
//! `status` ≤999 / `error` ≤500 字符——与 A2A `record` 同口径。任何一项
//! 不合规直接 `bad_params`，不入日志。
//!
//! ## 为什么用 OnceLock 而不是构造注入
//!
//! `script.usage` 是**宿主级**回调（不区分哪个 provider 实例），而 `LogStore`
//! 在 main.rs 里只构造一次。用 OnceLock 让 `main.rs` 调 [`install`] 一次即可，
//! 不必把它穿进 `ScriptHostManager` 的构造链（少一处共享文件改动）。

use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::log::{LogStore, RequestLog};
use crate::plugin_host::rpc::RpcError;

static LOG_STORE: OnceLock<Arc<LogStore>> = OnceLock::new();

/// 注入请求日志存储（main.rs 启动时调一次；重复调用忽略）。
pub fn install(store: Arc<LogStore>) {
    let _ = LOG_STORE.set(store);
}

fn bad_params(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("bad_params".into()),
    }
}

/// `script.usage`：记一条插件用量。
///
/// 返回 `true`（与 A2A `Ok(Value::Bool(true))` 同形——宿主侧不解析具体值，
/// 只判是否成功）。
pub(crate) fn record(params: &Value) -> Result<Value, RpcError> {
    let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or("");
    let model = text("model");
    let kind = text("kind");
    if model.is_empty()
        || model.len() > 200
        || kind.is_empty()
        || kind.len() > 40
        || !kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_ -".contains(&b))
    {
        return Err(bad_params("script.usage: invalid model or kind"));
    }
    let Some(store) = LOG_STORE.get() else {
        return Err(RpcError {
            message: "script.usage: request log store not ready".into(),
            code: Some("internal".into()),
        });
    };
    let number = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .clamp(0, 1_000_000_000_000)
    };
    let mut log = RequestLog::new();
    log.model = model.to_string();
    log.origin_model = Some(model.to_string());
    log.channel_name = Some(text("provider").to_string()).filter(|s| !s.is_empty());
    log.latency_ms = number("ms") as u64;
    log.status_code = number("status").clamp(0, 999) as u16;
    log.input_tokens = (number("input") + number("cacheRead") + number("cacheWrite")) as u64;
    log.output_tokens = number("output") as u64;
    log.error_msg = params
        .get("error")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(500).collect());
    // RequestLog 没有自由字段：账号/台账类型只进 tracing（管理面要看细节
    // 时查日志），不塞进请求日志的固定列——避免污染既有报表维度。
    tracing::debug!(
        "[Script] plugin usage provider={} account={} kind={kind} cacheRead={} cacheWrite={}",
        text("provider"),
        text("account"),
        number("cacheRead"),
        number("cacheWrite"),
    );
    store.record_request(log);
    Ok(Value::Bool(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn with_store<R>(f: impl FnOnce() -> R) -> R {
        // 全局 OnceLock 只能设一次：本测试模块内共享一份临时 store。
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let dir = tempfile::TempDir::new().unwrap();
            let store = Arc::new(LogStore::new(Arc::new(crate::storage::FileStore::new(
                dir.path().to_path_buf(),
            ))));
            // 泄漏临时目录：测试进程退出即清（避免 keep() 之外的 drop 竞态）
            std::mem::forget(dir);
            install(store);
        });
        f()
    }

    #[test]
    fn record_accepts_valid_payload() {
        with_store(|| {
            let out = record(&json!({
                "provider": "kiro",
                "account": "acc-1",
                "kind": "chat",
                "model": "claude-x",
                "input": 10,
                "output": 20,
                "cacheRead": 5,
                "cacheWrite": 2,
                "status": 200,
                "ms": 1234,
            }))
            .unwrap();
            assert_eq!(out, Value::Bool(true));
        });
    }

    #[test]
    fn record_rejects_bad_model_or_kind() {
        with_store(|| {
            assert_eq!(
                record(&json!({"kind": "chat"}))
                    .unwrap_err()
                    .code
                    .as_deref(),
                Some("bad_params")
            );
            assert_eq!(
                record(&json!({"model": "m", "kind": "BAD KIND!"}))
                    .unwrap_err()
                    .code
                    .as_deref(),
                Some("bad_params")
            );
            let long = "x".repeat(201);
            assert!(record(&json!({"model": long, "kind": "chat"})).is_err());
        });
    }
}
