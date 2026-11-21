// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 注入面的**开箱即用**实现 —— 让 T9 接线只需两行构造，而不是照 trait 手写 16 个方法。
//!
//! ## 为什么放在 wasm 目录里
//!
//! 这三个适配器只依赖 AIGX 已有的公共设施（`script::transport::Transport`、
//! `proxy_pool::resolve_proxy`、`plugin_host::rpc::StreamEvent`），不依赖
//! `account_session` 的账号模型 —— 后者才是「wasm 不直接依赖」那条约束要挡的
//! 东西（账号记录的形状由 integrator 决定）。把可复用的部分固化在这里，
//! 剩下的只有真正需要 integrator 决策的 [`WasmAccountAccess`]。
//!
//! ## WebSocket 为什么是 `unsupported`
//!
//! AIGX 的脚本宿主同样**不做** WebSocket egress（见 `plugin_host::script`
//! 模块注释：「WebSocket egress 本期不做，等有包真正需要时再补」）。这里如实
//! 返回 `unsupported` 错误码，而不是假装成功 —— 声明了 `websocket.v1` 的
//! Wasm 插件会拿到明确的失败，而不是静默无数据。

use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use super::access::{WasmEgress, WasmStreamSink};
use super::wire::WireError;

/// 基于脚本宿主 egress 代理的 [`WasmEgress`] 实现。
///
/// 语义与 `script.transport.*` 完全一致（同一份 `Transport` 实例即可共享
/// 连接池、并发会话闸与 reaper）。
pub struct TransportEgress {
    transport: Arc<crate::plugin_host::script::transport::Transport>,
}

impl TransportEgress {
    pub fn new(transport: Arc<crate::plugin_host::script::transport::Transport>) -> Arc<Self> {
        Arc::new(Self { transport })
    }
}

fn map(error: crate::plugin_host::rpc::RpcError) -> WireError {
    WireError {
        message: error.message,
        code: error.code,
        reason: None,
        retry_after: None,
    }
}

fn unsupported(what: &str) -> WireError {
    WireError::with_code(format!("当前网关的 Wasm 沙箱不支持 {what}"), "unsupported")
}

#[async_trait::async_trait]
impl WasmEgress for TransportEgress {
    async fn request(&self, params: Value) -> Result<Value, WireError> {
        // 沙箱已按清单校验过 URL origin；Transport 自己还会再挡一次非 http(s)。
        self.transport.request(&params).await.map_err(map)
    }

    async fn read(&self, session: &str) -> Result<Value, WireError> {
        self.transport.read(session).await.map_err(map)
    }

    async fn close(&self, session: &str) -> Result<Value, WireError> {
        self.transport.close(session).await.map_err(map)
    }

    async fn ws_open(&self, _params: Value) -> Result<Value, WireError> {
        Err(unsupported("WebSocket 出站"))
    }

    async fn ws_read(&self, _params: Value) -> Result<Value, WireError> {
        Err(unsupported("WebSocket 出站"))
    }

    async fn ws_send(&self, _params: Value) -> Result<Value, WireError> {
        Err(unsupported("WebSocket 出站"))
    }

    async fn ws_close(&self, _params: Value) -> Result<Value, WireError> {
        // 关闭是幂等的清理动作，不报错（否则插件侧 Drop 路径会刷错误日志）
        Ok(Value::Null)
    }

    fn resolve_proxy(&self, params: &Value) -> Result<Value, WireError> {
        // 与上游 `script::resolve_proxy` 同输入：`{config: {url: "..."}}`。
        let raw = params["config"]["url"]
            .as_str()
            .map(str::trim)
            .filter(|p| !p.is_empty());
        match crate::proxy_pool::resolve_proxy(raw, "") {
            Ok(Some(url)) => Ok(Value::String(url)),
            Ok(None) => Ok(Value::Null),
            Err(reason) => Err(WireError::with_code(reason, "proxy_unavailable")),
        }
    }
}

/// 把沙箱的流式事件转成网关既有的 [`StreamEvent`] 通道。
///
/// integrator 的数据面拿到 `StreamCall` 的消费端后，直接把这条 `mpsc` 接上
/// 即可（`head` → `StreamEvent::Head`、`chunk` → `StreamEvent::Chunk`）。
///
/// 错误经 [`Self::take_error`] 交付：`rx` 关闭后消费方必须检查一次，否则
/// 「插件失败」与「插件正常结束空回复」不可区分（failover 会失效）。
pub struct ChannelSink {
    events: UnboundedSender<crate::plugin_host::rpc::StreamEvent>,
    error: Mutex<Option<WireError>>,
}

impl ChannelSink {
    pub fn new(events: UnboundedSender<crate::plugin_host::rpc::StreamEvent>) -> Arc<Self> {
        Arc::new(Self {
            events,
            error: Mutex::new(None),
        })
    }

    /// 取走终结错误（只交付一次；第二次返回 `None`）。
    ///
    /// **必须在 `rx` 关闭后调用**：流正常结束（或消费方主动放弃）时返回
    /// `None`，只有沙箱以错误终结时才返回 `Some`。
    pub fn take_error(&self) -> Option<WireError> {
        self.error.lock().ok()?.take()
    }
}

impl WasmStreamSink for ChannelSink {
    fn head(&self, status: u16, headers: Vec<(String, String)>) -> Result<(), WireError> {
        self.events
            .send(crate::plugin_host::rpc::StreamEvent::Head {
                status,
                headers: headers.into_iter().collect(),
                upstream: None,
            })
            .map_err(|_| WireError::with_code("下游请求已结束", WireError::ABORTED))
    }

    fn chunk(&self, bytes: Vec<u8>) -> Result<(), WireError> {
        self.events
            .send(crate::plugin_host::rpc::StreamEvent::Chunk(bytes))
            .map_err(|_| WireError::with_code("下游请求已结束", WireError::ABORTED))
    }

    fn fail(&self, error: WireError) {
        if let Ok(mut slot) = self.error.lock() {
            // 首个错误为准（后续 fail 只可能是连锁清理）
            if slot.is_none() {
                *slot = Some(error);
            }
        }
    }

    fn end(&self) {
        // 显式终点：消费者以 `StreamEvent::End` 判定收尾，而不是等通道关闭
        // （发送端的生命周期由宿主持有，等关闭会互相等待）。
        let _ = self.events.send(crate::plugin_host::rpc::StreamEvent::End);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::rpc::StreamEvent;
    use serde_json::json;

    #[test]
    fn channel_sink_forwards_head_and_chunks() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ChannelSink::new(tx);
        sink.head(
            200,
            vec![("content-type".into(), "text/event-stream".into())],
        )
        .unwrap();
        sink.chunk(b"data: hi\n\n".to_vec()).unwrap();
        match rx.try_recv().unwrap() {
            StreamEvent::Head {
                status, headers, ..
            } => {
                assert_eq!(status, 200);
                assert_eq!(
                    headers.get("content-type").map(String::as_str),
                    Some("text/event-stream")
                );
            }
            other => panic!("expected head, got {other:?}"),
        }
        match rx.try_recv().unwrap() {
            StreamEvent::Chunk(bytes) => assert_eq!(bytes, b"data: hi\n\n"),
            other => panic!("expected chunk, got {other:?}"),
        }
    }

    #[test]
    fn channel_sink_reports_closed_downstream() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        drop(rx);
        let sink = ChannelSink::new(tx);
        let error = sink.chunk(vec![1]).unwrap_err();
        assert_eq!(error.code.as_deref(), Some(WireError::ABORTED));
    }

    #[test]
    fn websocket_is_explicitly_unsupported() {
        // 用 lib.rs 的测试专用入口：`rustls-no-provider` 下裸 `Client::new()`
        // 会因未安装全局 crypto provider 而 panic（测试进程不经过 main()）。
        let egress = TransportEgress::new(crate::plugin_host::script::transport::Transport::new(
            crate::test_http_client(),
        ));
        let error = futures_block(egress.ws_open(json!({}))).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("unsupported"));
    }

    /// 同步跑一个立即完成的 future（这两个 ws 方法不 await 任何东西）。
    fn futures_block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }
}
