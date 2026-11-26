// Ported from Agent2API v3.5.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 协议转换（`protocol.convert.v1`）—— 只做转换、不发网络请求。
//!
//! 对齐上游 `plugin_host/wasm/protocol.rs`（v3.5.1）：四个动作
//! `protocol.request / open / push / close`，句柄与输入缓冲**归当前 Wasm 调用所有**。
//!
//! ## 与上游的实现差异
//!
//! 上游把转换状态机放在脚本宿主那份**全局** `script/protocol.rs` 里（`open_stream`
//! 用 `wasm-{id}` 字符串键、Gemini 还会把 `provider/account/identity/session`
//! 摘要成 `wasm:` 会话键以免跨账号串签名）。AIGX 的等价物是
//! [`ProtocolRegistry`]（`script::protocol`，同一份 Anthropic / Responses / Gemini
//! 增量状态机）。本模块直接复用它，但：
//!
//! - **id 由宿主生成**（`wasm-{net_id}` 或注册表 uuid），不再接受插件自带 id；
//!   于是「插件自报会话串被当全局键」的隐患天然消失，无需再做来源摘要。
//! - `chat` 协议在 AIGX 注册表里没有独立状态机（脚本侧用不到），这里用**本地
//!   直通**实现：合法 SSE 行原样返回，与上游 `Machine::Chat` 同语义。
//! - Gemini 的 `session` 参数仍按上游校验形状，但不参与键派生（见上）。
//!
//! 输入校验（逐字节上限、事件必须是 JSON 对象、收尾必须空数据）逐条保留。

use std::sync::{Arc, OnceLock};

use base64::Engine;
use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

/// 单次调用可同时打开的转换器（与上游 `MAX_CONVERTERS` 一致）。
pub(crate) const MAX_CONVERTERS: usize = 8;
/// 单个输入分片上限（与上游 `MAX_CHUNK_BYTES` 一致）。
pub(crate) const MAX_CHUNK_BYTES: usize = 64 * 1024;
/// 单次请求 / 单次结果的 JSON 上限（与上游 `MAX_OUTPUT_BYTES` 一致）。
pub(crate) const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// 单个转换流累计输入上限（与上游 `MAX_STREAM_BYTES` 一致）。
pub(crate) const MAX_STREAM_BYTES: usize = 64 * 1024 * 1024;
/// 单个 SSE 事件上限（与上游 `MAX_EVENT_BYTES` 一致）。
const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// 进程级转换器注册表（Wasm 宿主与脚本宿主各持一份转换器表；这里为 Wasm
/// 单独建一份，避免把两种宿主的 `id` 键混进同一张表）。
fn registry() -> &'static Arc<crate::plugin_host::script::protocol::ProtocolRegistry> {
    static REGISTRY: OnceLock<Arc<crate::plugin_host::script::protocol::ProtocolRegistry>> =
        OnceLock::new();
    REGISTRY.get_or_init(crate::plugin_host::script::protocol::ProtocolRegistry::new)
}

fn bad_params(message: &str) -> WireError {
    WireError::with_code(message, "bad_params")
}

fn limited(message: &str) -> WireError {
    WireError::with_code(message, "plugin_resource_limit")
}

/// `RpcError` → `WireError`：保留上游的 `code`（缺省 `internal`）。
fn wire(error: crate::plugin_host::rpc::RpcError) -> WireError {
    WireError::with_code(
        error.message,
        error.code.unwrap_or_else(|| "internal".into()),
    )
}

fn fields(params: &Value, required: &[&str], optional: &[&str]) -> Result<(), WireError> {
    let object = params
        .as_object()
        .ok_or_else(|| bad_params("协议转换参数必须是对象"))?;
    if required.iter().any(|key| !object.contains_key(*key))
        || object
            .keys()
            .any(|key| !required.contains(&key.as_str()) && !optional.contains(&key.as_str()))
    {
        return Err(bad_params("协议转换缺少参数或包含未声明字段"));
    }
    Ok(())
}

fn text<'a>(params: &'a Value, key: &str, max: usize) -> Result<&'a str, WireError> {
    params[key]
        .as_str()
        .filter(|value| value.len() <= max && !value.chars().any(char::is_control))
        .ok_or_else(|| bad_params("协议转换文本字段无效或超过长度限制"))
}

fn protocol(params: &Value, streaming: bool) -> Result<&str, WireError> {
    let value = text(params, "protocol", 32)?;
    if !matches!(value, "chat" | "anthropic" | "responses") && !(streaming && value == "gemini") {
        return Err(WireError::with_code("不支持指定的协议转换", "unsupported"));
    }
    Ok(value)
}

fn json_size(value: &Value) -> Result<(), WireError> {
    let bytes = serde_json::to_vec(value).map_err(|_| bad_params("协议转换内容无法序列化"))?;
    if bytes.len() > MAX_OUTPUT_BYTES {
        return Err(limited("协议转换请求或结果超过 4 MiB 限制"));
    }
    Ok(())
}

/// 一个转换器的**本地**校验状态（真实的增量转换状态在 [`registry`] 里）。
///
/// - `delegated == true`：非 chat，实际转换交给 `ProtocolRegistry`；
/// - `delegated == false`：chat 直通，本结构自己维护 SSE 行校验。
pub(crate) struct Conversion {
    pub(crate) id: String,
    line: Vec<u8>,
    event_bytes: usize,
    total_bytes: usize,
    delegated: bool,
}

impl Conversion {
    fn check_line(&self) -> Result<(), WireError> {
        let line = std::str::from_utf8(&self.line)
            .map_err(|_| bad_params("协议转换输入不是有效 UTF-8"))?;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(data) = line.strip_prefix("data:").map(str::trim) {
            if !data.is_empty() && data != "[DONE]" {
                let value: Value = serde_json::from_str(data)
                    .map_err(|_| bad_params("协议转换收到无效的 JSON 事件"))?;
                if !value.is_object() {
                    return Err(bad_params("协议转换事件必须是 JSON 对象"));
                }
            }
        }
        Ok(())
    }

    fn observe(&mut self, bytes: &[u8], finish: bool) -> Result<(), WireError> {
        self.total_bytes = self.total_bytes.saturating_add(bytes.len());
        if self.total_bytes > MAX_STREAM_BYTES {
            return Err(limited("单个协议转换流输入超过 64 MiB 限制"));
        }
        for &byte in bytes {
            self.event_bytes = self.event_bytes.saturating_add(1);
            if self.event_bytes > MAX_EVENT_BYTES {
                return Err(limited("协议转换单个事件超过 1 MiB 限制"));
            }
            if byte == b'\n' {
                self.check_line()?;
                if self.line.is_empty() || self.line == b"\r" {
                    self.event_bytes = 0;
                }
                self.line.clear();
            } else {
                self.line.push(byte);
            }
        }
        if finish && !self.line.is_empty() {
            self.check_line()?;
            self.line.clear();
        }
        Ok(())
    }
}

impl Drop for Conversion {
    fn drop(&mut self) {
        // chat 直通没有外部状态；非 chat 必须把注册表里的转换器一并摘除。
        if !self.delegated {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let id = self.id.clone();
        handle.spawn(async move {
            registry().close_named(&id).await;
        });
    }
}

impl Context {
    /// 分派 `protocol.request / open / push / close`。
    ///
    /// 与插件其余能力一样，先查 `protocol.convert.v1` 白名单；未授权一律
    /// `forbidden`，不泄露转换器是否存在。
    pub(super) async fn protocol(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        self.permission("protocol.convert.v1")?;
        match method {
            "protocol.request" => {
                fields(params, &["protocol", "body", "model"], &[])?;
                let protocol = protocol(params, false)?;
                text(params, "model", 512)?;
                let body = &params["body"];
                if !body.is_object()
                    || ["messages", "tools"].iter().any(|key| {
                        body.get(*key).is_some_and(|value| {
                            value
                                .as_array()
                                .is_none_or(|items| items.iter().any(|item| !item.is_object()))
                        })
                    })
                {
                    return Err(bad_params(
                        "协议转换请求体必须是对象，消息和工具必须是对象数组",
                    ));
                }
                json_size(body)?;
                let result =
                    crate::plugin_host::script::protocol::ProtocolRegistry::request(protocol, body)
                        .map_err(wire)
                        .map_err(|_| bad_params("请求协议转换失败，请检查消息与工具格式"))?;
                if !result.is_object() {
                    return Err(super::wire::error("请求协议转换未返回对象"));
                }
                json_size(&result)?;
                Ok(result)
            }
            "protocol.open" => {
                fields(params, &["protocol", "model"], &["session"])?;
                let protocol = protocol(params, true)?;
                text(params, "model", 512)?;
                // `session` 只按上游校验形状；AIGX 的转换器键是宿主生成的，
                // 不拿插件自报串当跨账号全局键（见模块头注）。
                if params.get("session").is_some() {
                    text(params, "session", 1024)?;
                }
                if self.converters.len() >= MAX_CONVERTERS {
                    return Err(limited("单次调用协议转换器超过 8 个"));
                }
                let (id, delegated) = if protocol == "chat" {
                    // chat 直通：本地状态即可，无需注册表。
                    (format!("wasm-{}", super::net_id()), false)
                } else {
                    let value = registry().open_named(protocol).await.map_err(wire)?;
                    let id = value["id"]
                        .as_str()
                        .filter(|id| !id.is_empty() && id.len() <= 80)
                        .ok_or_else(|| super::wire::error("协议转换句柄无效"))?
                        .to_string();
                    (id, true)
                };
                self.converters.insert(
                    id.clone(),
                    Conversion {
                        id: id.clone(),
                        line: Vec::new(),
                        event_bytes: 0,
                        total_bytes: 0,
                        delegated,
                    },
                );
                Ok(json!({ "id": id }))
            }
            "protocol.push" | "protocol.close" => {
                let id = text(params, "id", 80)?.to_string();
                // 先移出本地登记；后续任何校验/转换错误由 Drop 释放，仅成功且
                // 未 finish 才归还（与上游同语义）。
                let mut converter = self.converters.remove(&id).ok_or_else(|| {
                    WireError::with_code("协议转换器不属于当前调用或已结束", "forbidden")
                })?;
                if method == "protocol.close" {
                    fields(params, &["id"], &[])?;
                    return Ok(Value::Null);
                }
                fields(params, &["id", "data", "finish"], &[])?;
                let finish = params["finish"]
                    .as_bool()
                    .ok_or_else(|| bad_params("协议转换 finish 必须是布尔值"))?;
                let encoded = params["data"]
                    .as_str()
                    .ok_or_else(|| bad_params("协议转换 data 必须是 base64 文本"))?;
                if encoded.len() > MAX_CHUNK_BYTES.div_ceil(3) * 4 {
                    return Err(limited("协议转换输入分片超过 64 KiB"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| bad_params("协议转换输入不是有效 base64"))?;
                if bytes.len() > MAX_CHUNK_BYTES {
                    return Err(limited("协议转换输入分片超过 64 KiB"));
                }
                if finish && !bytes.is_empty() {
                    return Err(bad_params("协议转换收尾必须传空数据，请先提交最后一片内容"));
                }
                converter.observe(&bytes, finish)?;
                let result = if converter.delegated {
                    registry()
                        .push_named(&id, &bytes, finish)
                        .await
                        .map_err(wire)?
                } else {
                    // chat 直通：合法 SSE 原样返回。
                    json!({
                        "data": base64::engine::general_purpose::STANDARD.encode(&bytes)
                    })
                };
                let data = result["data"]
                    .as_str()
                    .ok_or_else(|| super::wire::error("协议转换结果缺少字节内容"))?;
                if data.len() > MAX_OUTPUT_BYTES.div_ceil(3) * 4 {
                    return Err(limited("协议转换结果超过 4 MiB 限制"));
                }
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| super::wire::error("协议转换结果编码无效"))?;
                if decoded.len() > MAX_OUTPUT_BYTES {
                    return Err(limited("协议转换结果超过 4 MiB 限制"));
                }
                if !finish {
                    self.converters.insert(id, converter);
                }
                Ok(result)
            }
            _ => Err(WireError::with_code(
                "不支持指定的协议转换操作",
                WireError::NO_METHOD,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_names_are_restricted() {
        let ok = json!({ "protocol": "anthropic" });
        assert_eq!(protocol(&ok, false).unwrap(), "anthropic");
        // gemini 只在流式（open/push）路径允许。
        let gemini = json!({ "protocol": "gemini" });
        assert!(protocol(&gemini, false).is_err());
        assert_eq!(protocol(&gemini, true).unwrap(), "gemini");
        assert!(protocol(&json!({ "protocol": "weird" }), true).is_err());
    }

    #[test]
    fn observe_validates_event_json_and_limits() {
        let mut conversion = Conversion {
            id: "wasm-test".into(),
            line: Vec::new(),
            event_bytes: 0,
            total_bytes: 0,
            delegated: false,
        };
        assert!(conversion
            .observe(b"data: {\"choices\":[]}\n\n", false)
            .is_ok());
        assert!(conversion.observe(b"data: not-json\n", false).is_err());
        // 单事件超 1 MiB 直接拒绝。
        let huge = vec![b'x'; MAX_EVENT_BYTES + 16];
        assert!(conversion.observe(&huge, false).is_err());
    }

    #[test]
    fn request_rejects_non_object_messages() {
        let bad = json!({
            "protocol": "anthropic",
            "model": "m",
            "body": { "messages": ["not-an-object"] }
        });
        // 直接测字段形状（不构造完整 Context）。
        let body = &bad["body"];
        let invalid = ["messages", "tools"].iter().any(|key| {
            body.get(*key).is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|items| items.iter().any(|item| !item.is_object()))
            })
        });
        assert!(invalid);
    }
}
