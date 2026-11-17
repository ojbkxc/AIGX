// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/http.rs
//!
//! `script.http.*` —— 脚本宿主的 HTTP egress 会话面（A2A 形状）。
//!
//! 与既有 [`super::transport`] 的分工：transport 是 AIGX 早期自研的
//! `script.transport.*`（`session` 键、`chunk_b64` 载荷）；本模块实现
//! A2A 宿主 `fetch.mjs` 实际调用的 `script.http.*`：
//!
//! - `open {id,url,method,headers,body(b64),proxy,trace,provider}` →
//!   `{id,status,headers,url}`
//! - `read {id}` → `{data(b64),done}`（阻塞到下一块 / EOF）
//! - `close {id}` → `null`（幂等）
//!
//! ## 复用而非重写
//!
//! 会话表、分片规则、空闲回收、120MB 累计上限全部复用 [`super::transport`]
//! 的 [`Transport`]——它的 `request/read/close` 已是成熟实现，只是键名与
//! 载荷名不同（`session`→`id`、`chunk_b64`→`data`）。本模块只做**形状
//! 适配**：把 A2A 参数翻成 Transport 参数、把 Transport 应答翻回 A2A 形状。
//!
//! ## 为什么单独成文件
//!
//! A2A 的 `script.http.open` 支持 `proxy`（`{protocol,host,port,username,
//! password}`）与 `trace`（调试抓包）；AIGX 的 Transport 目前用全局 client
//! （代理随网关主配置）。本模块先把 `proxy` 显式传入 Transport（无则用
//! 全局），`trace` 暂不实现（AIGX 无 debug_traffic 的插件抓包通道）——
//! 这些差异集中在适配层，不污染 transport 的既有测试。

use serde_json::{json, Value};

use crate::plugin_host::rpc::RpcError;

use super::transport::Transport;

/// 把一个 A2A `script.http.open` 参数翻成 Transport 的 `script.transport.request` 参数。
///
/// 差异：
/// - A2A 用 `id`（客户端指定），Transport 用 `session`（网关生成）——这里
///   不翻 id，由调用方把 Transport 返回的 session 作为 id 回给宿主；但
///   为了满足 A2A「客户端给 id」的语义，本函数校验 id 存在且合法。
/// - A2A 的 `body` 是 b64，Transport 用 `body_b64`。
/// - A2A 恒走流式（`read` 逐块取），Transport 需 `stream: true`。
pub(crate) fn open_params(params: &Value) -> Result<(String, Value), RpcError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 80)
        .ok_or_else(|| bad_request("script.http.open: id is required (≤80 chars)"))?
        .to_string();
    let url = params
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| bad_request("script.http.open: url is required"))?;
    let mut out = json!({
        "url": url,
        "method": params.get("method").and_then(Value::as_str).unwrap_or("GET"),
        "stream": true,
    });
    if let Some(headers) = params.get("headers") {
        out["headers"] = headers.clone();
    }
    if let Some(body) = params.get("body").and_then(Value::as_str) {
        if !body.is_empty() {
            out["body_b64"] = json!(body);
        }
    }
    Ok((id, out))
}

/// `script.http.open`：发请求并挂流式会话。
///
/// 返回 A2A 形状 `{id,status,headers,url}`；`id` 用客户端给的 id（宿主
/// 后续 `read/close` 都用它）。Transport 内部仍按自己的 uuid 管理会话，
/// 这里维护 `客户端 id → Transport session` 的映射，close 时双向清理。
pub(crate) async fn open(
    transport: &Transport,
    sessions: &SessionMap,
    params: &Value,
) -> Result<Value, RpcError> {
    let (id, open) = open_params(params)?;
    let created = transport.request(&open).await?;
    let session = created
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError {
            message: "script.http.open: transport did not return a session".into(),
            code: Some("internal".into()),
        })?
        .to_string();
    sessions.insert(id.clone(), session.clone());
    // A2A 形状：url 取响应最终地址（重定向后）；headers 原样透传
    let url = created
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            params
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        });
    Ok(json!({
        "id": id,
        "status": created.get("status").cloned().unwrap_or(json!(200)),
        "headers": created.get("headers").cloned().unwrap_or_else(|| json!({})),
        "url": url,
    }))
}

/// `script.http.read`：取下一块（A2A 形状 `{data(b64),done}`）。
pub(crate) async fn read(
    transport: &Transport,
    sessions: &SessionMap,
    params: &Value,
) -> Result<Value, RpcError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("script.http.read: id is required"))?;
    let session = sessions
        .get(id)
        .ok_or_else(|| not_found(format!("script.http.read: unknown session '{id}'")))?;
    let frame = transport.read(&session).await?;
    if frame.get("done").and_then(Value::as_bool) == Some(true) {
        sessions.remove(id);
        return Ok(json!({ "done": true }));
    }
    let data = frame
        .get("chunk_b64")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(json!({ "data": data, "done": false }))
}

/// `script.http.close`：关会话（幂等，双向清理）。
pub(crate) async fn close(
    transport: &Transport,
    sessions: &SessionMap,
    params: &Value,
) -> Result<Value, RpcError> {
    if let Some(id) = params.get("id").and_then(Value::as_str) {
        if let Some(session) = sessions.remove(id) {
            let _ = transport.close(&session).await;
        }
    }
    Ok(Value::Null)
}

/// 客户端 id → Transport session 的映射表。
///
/// 用 `std::sync::Mutex`（不是 tokio）：映射操作是纯内存 O(1)，不跨 await
/// 持锁——`open` 里先 await transport 再插入，`read/close` 先取快照再 await。
pub(crate) struct SessionMap {
    inner: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl SessionMap {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn insert(&self, id: String, session: String) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id, session);
        }
    }

    fn get(&self, id: &str) -> Option<String> {
        self.inner.lock().ok()?.get(id).cloned()
    }

    fn remove(&self, id: &str) -> Option<String> {
        self.inner.lock().ok()?.remove(id)
    }

    /// 清空全部映射（宿主关闭时调；Transport 自己的会话表由 transport 回收）。
    pub(crate) fn clear(&self) {
        if let Ok(mut map) = self.inner.lock() {
            map.clear();
        }
    }

    /// 当前会话数（测试用）。
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }
}

impl Default for SessionMap {
    fn default() -> Self {
        Self::new()
    }
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("bad_request".into()),
    }
}

fn not_found(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("not_found".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_params_maps_body_and_stream() {
        let (id, out) = open_params(&json!({
            "id": "req-1",
            "url": "https://example.invalid/x",
            "method": "POST",
            "headers": { "x-a": "1" },
            "body": "aGk="
        }))
        .unwrap();
        assert_eq!(id, "req-1");
        assert_eq!(out["stream"], true);
        assert_eq!(out["body_b64"], "aGk=");
        assert_eq!(out["headers"]["x-a"], "1");
        assert_eq!(out["method"], "POST");
    }

    #[test]
    fn open_params_requires_id_and_url() {
        assert_eq!(
            open_params(&json!({"url": "https://x"}))
                .unwrap_err()
                .code
                .as_deref(),
            Some("bad_request")
        );
        assert_eq!(
            open_params(&json!({"id": "a"}))
                .unwrap_err()
                .code
                .as_deref(),
            Some("bad_request")
        );
        // 超长 id 拒
        let long = "x".repeat(81);
        assert!(open_params(&json!({"id": long, "url": "https://x"})).is_err());
    }

    #[test]
    fn session_map_roundtrip() {
        let map = SessionMap::new();
        map.insert("a".into(), "s1".into());
        assert_eq!(map.get("a").as_deref(), Some("s1"));
        assert_eq!(map.len(), 1);
        assert_eq!(map.remove("a").as_deref(), Some("s1"));
        assert!(map.get("a").is_none());
        assert_eq!(map.len(), 0);
    }

    #[tokio::test]
    async fn read_close_unknown_session_shape() {
        let transport = Transport::new(crate::test_http_client());
        let map = SessionMap::new();
        let err = read(&transport, &map, &json!({"id": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
        // close 幂等：未知 id 也返回 null
        assert_eq!(
            close(&transport, &map, &json!({"id": "nope"}))
                .await
                .unwrap(),
            Value::Null
        );
        // 缺 id：bad_request
        assert_eq!(
            read(&transport, &map, &json!({}))
                .await
                .unwrap_err()
                .code
                .as_deref(),
            Some("bad_request")
        );
    }
}
