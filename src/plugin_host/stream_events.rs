//! 流式事件接收器 —— 对齐 Agent2API v3.5.0-beta.2 `plugin_host::stream_events`。
//!
//! beta.2 将 `StreamCall.events` 从裸 `UnboundedReceiver<StreamEvent>` 改为
//! [`StreamEvents`]，以便在 Wasm 流上挂一个内存分配守卫（`Allocation`），
//! 确保流的生命周期内有一块预留内存不被回收。
//!
//! AIGX 的 `StreamCall` 内部维护了更丰富的状态（`head`/`consumed`/`credited`/
//! `finished`），不直接暴露 `UnboundedReceiver`；本模块提供类型别名与 `From`
//! 转换，使 AIGX 的 `StreamCall` 在对外接口上与 beta.2 兼容。

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::sync::mpsc::UnboundedReceiver;

use super::rpc::StreamEvent;

/// 流式事件接收器。
///
/// 对齐上游 `StreamEvents`：封装 `UnboundedReceiver<StreamEvent>`，
/// 可选挂一个内存分配守卫。AIGX 不使用 wasmtime 的 `Allocation`，
/// 但保留相同接口形状以便未来接入。
pub(crate) struct StreamEvents {
    inner: UnboundedReceiver<StreamEvent>,
}

impl StreamEvents {
    /// 从裸接收器构造（脚本插件路径，无内存守卫）。
    pub(super) fn new(inner: UnboundedReceiver<StreamEvent>) -> Self {
        Self { inner }
    }

    /// 异步接收下一个事件。
    pub(crate) async fn recv(&mut self) -> Option<StreamEvent> {
        self.inner.recv().await
    }

    /// 轮询接收（用于 `Stream` trait 实现）。
    pub(crate) fn poll_recv(&mut self, context: &mut Context<'_>) -> Poll<Option<StreamEvent>> {
        Pin::new(&mut self.inner).poll_recv(context)
    }
}

impl From<UnboundedReceiver<StreamEvent>> for StreamEvents {
    fn from(inner: UnboundedReceiver<StreamEvent>) -> Self {
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn recv_returns_events_in_order() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut events: StreamEvents = rx.into();
        tx.send(StreamEvent::Log {
            message: "a".into(),
        })
        .unwrap();
        tx.send(StreamEvent::Log {
            message: "b".into(),
        })
        .unwrap();
        drop(tx);
        assert!(events.recv().await.is_some());
        assert!(events.recv().await.is_some());
        assert!(events.recv().await.is_none());
    }

    #[tokio::test]
    async fn from_unbounded_receiver() {
        let (_tx, rx) = mpsc::unbounded_channel::<StreamEvent>();
        let _events: StreamEvents = rx.into();
    }
}
