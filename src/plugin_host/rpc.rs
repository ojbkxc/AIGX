//! 插件宿主 RPC 内核 —— 与 Agent2API `provider-protocol` 的线格式对齐。
//!
//! ## 线格式（逐行 JSON，双向对称）
//!
//! 插件是独立二进制，通过 **stdin/stdout** 与网关通信。每一帧一行 JSON：
//!
//! ```json
//! {"id":7,"method":"chat","params":{…}}          // 请求
//! {"id":7,"result":{…}}                          // 应答（成功）
//! {"id":7,"error":{"message":"…","code":"…"}}    // 应答（失败）
//! {"id":7,"event":"head","status":200,…}         // 流式中间帧
//! {"id":0,"event":"log","level":"info","message":"…"}  // 通知（无应答）
//! ```
//!
//! ## 协议常量
//!
//! 这些数字是**双方共同遵守的约定**——网关按 [`MAX_LINE_BYTES`] 判定
//! 「子进程坏了」，插件也按同一个数决定「这一帧要不要拆」。两边各写一份
//! 的下场是一边以为能发、另一边判定超限并杀掉进程。
//!
//! ## id 分配规则（双向对称）
//!
//! 网关发请求用**奇数** id（1,3,5…），插件回调网关用**偶数** id（2,4,6…），
//! `id=0` 保留给通知/日志（无应答）。这样两个方向的 id 空间不冲突，
//! 不需要跨方向协调 id 分配。

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

/// 协议版本。握手时双方比对，不一致一律拒绝加载。
///
/// 与上游 `provider-protocol::PROTOCOL_VERSION` 对齐。升级条件：删字段/
/// 改字段语义/把可选改必填/改方法行为契约。新增可选字段或新方法不需要升。
pub const PROTOCOL_VERSION: u32 = 1;

/// 一行的字节上限（含行尾换行）。超限判致命——读循环直接断开。
///
/// 上游从 16 MiB 提到 128 MiB：无状态家的 `chat` 参数装完整请求体，
/// 带 base64 图片时会线性增长。这里对齐上游取值。
pub const MAX_LINE_BYTES: usize = 128 << 20;

/// 单个流式 chunk 的载荷上限（解码后）。插件发 chunk 前必须自行切分。
pub const MAX_CHUNK_BYTES: usize = 64 << 10;

/// 一次流式调用的初始信用窗口（字节）。背压用。
pub const DEFAULT_WINDOW_BYTES: usize = 512 << 10;

/// 一帧的计费开销（字节）。帧的 JSON 包装、base64 膨胀、字段名都占内存。
pub const FRAME_OVERHEAD_BYTES: usize = 2048;

/// 读缓冲区大小（同时用作测试里 duplex 通道的容量）。
const READ_BUF_SIZE: usize = 1 << 16;

/// RPC 错误（线上形状 + Rust 侧）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl RpcError {
    fn new(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
        }
    }
}

/// 帧种类（由 `Frame::kind()` 判定）。
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum FrameKind {
    /// 请求（带 `method`，id ≠ 0）
    Request,
    /// 应答（带 `result` 或 `error`）
    Response,
    /// 流式中间帧（带 `event`）
    Event,
    /// 通知（带 `method`，id = 0，无应答）
    Notification,
    /// 无法识别
    Unknown,
}

/// 一个线上帧。宽结构 + 大量 `Option`——未知/缺失字段自然落到 `None`，
/// 旧实现读新帧不炸。这是刻意留的兼容余量。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Frame {
    /// 请求/应答关联 id。`0` = 通知（无应答）。
    #[serde(default)]
    pub id: i64,
    /// 请求方法名（带 `method` 即请求/通知）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// 请求参数（仅请求有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    /// 成功应答的载荷（仅应答有；JSON `null` 也是合法结果）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present_or_null"
    )]
    pub result: Option<Value>,
    /// 失败应答（仅应答有）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    /// 流式中间帧的事件名（`head`/`chunk`/`end`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// 流式 `head` 帧：上游状态码。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// 流式 `head` 帧：上游响应头（小写键）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
    /// 流式 `head` 帧：这份响应用哪套协议。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// 流式 `chunk` 帧：base64 编码的载荷。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// 通知载荷：日志级别（`log` 事件用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    /// 通知载荷：日志正文（`log` 事件用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Frame {
    /// 判定帧的种类。
    pub fn kind(&self) -> FrameKind {
        if self.method.is_some() {
            if self.id == 0 {
                FrameKind::Notification
            } else {
                FrameKind::Request
            }
        } else if self.result.is_some() || self.error.is_some() {
            FrameKind::Response
        } else if self.event.is_some() {
            FrameKind::Event
        } else {
            FrameKind::Unknown
        }
    }

    /// 是否是一个请求（带 method 且 id ≠ 0）。
    pub fn is_request(&self) -> bool {
        self.kind() == FrameKind::Request
    }

    /// 是否是一个应答。
    pub fn is_response(&self) -> bool {
        self.kind() == FrameKind::Response
    }

    /// 是否是一个通知（id = 0，带 method）。
    pub fn is_notification(&self) -> bool {
        self.kind() == FrameKind::Notification
    }

    /// 是否是一个流式事件帧。
    pub fn is_event(&self) -> bool {
        self.kind() == FrameKind::Event
    }
}

/// `Option<Value>` 的自定义反序列化：让「字段出现过」（哪怕值是 null）
/// 本身成为判据。
///
/// 默认的 `Option<Value>` 把「字段缺失」与「字段是 JSON null」都读成
/// `None`，于是 `{"id":7,"result":null}`（成功但没有返回值——流式调用
/// 收尾的形态）会被判成 `Unknown`，上层永远等不到收尾。
fn present_or_null<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Value::deserialize(deserializer)?;
    Ok(Some(value))
}

/// 把一帧编码成一行 JSON（不含换行）。
pub fn encode_frame(frame: &Frame) -> Result<String, RpcError> {
    serde_json::to_string(frame).map_err(|e| RpcError::new(format!("encode: {e}"), "encode"))
}

/// 把一行 JSON 解码成一帧。
pub fn decode_frame(line: &str) -> Result<Frame, RpcError> {
    serde_json::from_str(line).map_err(|e| RpcError::new(format!("decode: {e}"), "decode"))
}

/// 插件→网关的回调处理器。
///
/// 插件通过这个方法族反向调用网关（读账号、写台账、打日志、发通知）。
/// 网关侧的实现见 `callbacks.rs`。
pub trait CallbackHandler: Send + Sync {
    /// 处理一个插件回调。返回值作为回调的应答。
    ///
    /// 这是同步方法——回调的语义是「插件问网关一个问题，网关答一嘴」，
    /// 不涉及长耗时。需要落盘/读库的回调由实现方自己处理。
    fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}

/// 无回调能力的处理器（M1 骨架用，M3 换成真正的 `callbacks.rs`）。
#[derive(Default)]
pub struct NoopCallbacks;

impl CallbackHandler for NoopCallbacks {
    fn handle(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        Err(RpcError::new(
            format!("no callback handler for '{method}'"),
            "no_handler",
        ))
    }
}

/// 内部共享状态。
struct Shared {
    /// 出站写句柄（锁内写一行 JSON）。
    writer: Mutex<BoxWriter>,
    /// 在途请求的应答通道。
    pending: RwLock<HashMap<i64, oneshot::Sender<Frame>>>,
    /// 回调处理器。
    callbacks: Arc<dyn CallbackHandler>,
    /// 我方分配的下一个奇数 id。
    next_id: AtomicI64,
}

/// 出站写句柄。
pub type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;
/// 入站读句柄。
pub type BoxReader = Box<dyn AsyncRead + Send + Unpin>;

/// 一个 RPC 通道（网关侧视角）。
///
/// 持有出站写句柄 + 在途请求表 + 读循环任务句柄。读循环在后台任务里
/// 持续读插件的 stdout，把应答路由到 `pending`，把回调分发给 `callbacks`。
pub struct RpcChannel {
    shared: Arc<Shared>,
    /// 读循环任务句柄。用 `Arc` 包住以便 `shutdown(&self)` 直接 abort
    /// （`JoinHandle::abort` 取 `&self`，重复调用无害）。
    shutdown: Option<Arc<JoinHandle<()>>>,
}

impl RpcChannel {
    /// 建一个通道并启动读循环。
    ///
    /// `reader`/`writer` 是插件的 stdout/stdin（或测试用的 duplex）。
    /// `callbacks` 处理插件→网关的回调。
    pub fn new(reader: BoxReader, writer: BoxWriter, callbacks: Arc<dyn CallbackHandler>) -> Self {
        let shared = Arc::new(Shared {
            writer: Mutex::new(writer),
            pending: RwLock::new(HashMap::new()),
            callbacks,
            next_id: AtomicI64::new(1),
        });
        let buf_reader = BufReader::new(reader);
        let handle = tokio::spawn(read_loop(buf_reader, shared.clone()));
        Self {
            shared,
            shutdown: Some(Arc::new(handle)),
        }
    }

    /// 发一个请求并等应答。
    ///
    /// 分配奇数 id，写请求帧，在 `pending` 里注册应答通道，等插件回应答。
    /// 超时（由调用方的 select 控制）或通道关闭则报错。
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.shared.next_id.fetch_add(2, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.shared.pending.write().await;
            pending.insert(id, tx);
        }
        let frame = Frame {
            id,
            method: Some(method.to_string()),
            params: Some(params),
            ..Default::default()
        };
        if let Err(e) = self.shared.write_frame(&frame).await {
            let mut pending = self.shared.pending.write().await;
            pending.remove(&id);
            return Err(e);
        }
        match rx.await {
            Ok(frame) => {
                if let Some(err) = frame.error {
                    return Err(err);
                }
                Ok(frame.result.unwrap_or(Value::Null))
            }
            Err(_) => Err(RpcError::new("channel closed before response", "closed")),
        }
    }

    /// 发一个通知（不期待应答）。
    ///
    /// `id=0`，插件收到后不回帧。用于日志、进度等单向消息。
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RpcError> {
        let frame = Frame {
            id: 0,
            method: Some(method.to_string()),
            params: Some(params),
            ..Default::default()
        };
        self.shared.write_frame(&frame).await
    }

    /// 关闭通道：abort 读循环任务。
    pub fn shutdown(&self) {
        if let Some(h) = &self.shutdown {
            h.abort();
        }
    }

    /// 当前在途请求数（诊断用）。
    pub async fn pending_count(&self) -> usize {
        self.shared.pending.read().await.len()
    }
}

impl Shared {
    /// 写一帧到插件 stdin。
    async fn write_frame(&self, frame: &Frame) -> Result<(), RpcError> {
        let line = encode_frame(frame)?;
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| RpcError::new(format!("write: {e}"), "write"))?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|e| RpcError::new(format!("write: {e}"), "write"))?;
        writer
            .flush()
            .await
            .map_err(|e| RpcError::new(format!("flush: {e}"), "flush"))?;
        Ok(())
    }
}

/// 读一行（有界）。
///
/// `AsyncBufReadExt::read_until` 会一直读到换行或 EOF——对端不换行时会
/// 无界分配内存。这里手写有界版本：按 `fill_buf`/`consume` 分块收集，
/// 超过 `MAX_LINE_BYTES` 即报错，读循环据此断开。
///
/// 返回 `Ok(None)` = EOF（对端关闭）；`Ok(Some(n))` = 读到 n 字节（含换行）。
async fn read_bounded_line(
    reader: &mut BufReader<BoxReader>,
    buf: &mut Vec<u8>,
) -> std::io::Result<Option<usize>> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                None
            } else {
                Some(buf.len())
            });
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            return Ok(Some(buf.len()));
        }
        buf.extend_from_slice(available);
        let len = available.len();
        reader.consume(len);
        if buf.len() > MAX_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "rpc line exceeds MAX_LINE_BYTES",
            ));
        }
    }
}

/// 读循环：持续从插件 stdout 读行，路由帧。
///
/// 路由规则：
/// - 带 `method` 且 `id ≠ 0` → 插件→网关请求 → 调 `callbacks.handle()`，回写应答
/// - 带 `method` 且 `id = 0` → 通知 → 记日志（M1 不回帧）
/// - 带 `result`/`error` → 网关在途请求的应答 → 发回 `pending` 的 oneshot
/// - 带 `event` → 流式帧 → M1 只记日志（M3 走 `conversation` 时用）
///
/// EOF（插件退出）→ 断掉所有在途请求。
async fn read_loop(mut reader: BufReader<BoxReader>, shared: Arc<Shared>) {
    let mut buf: Vec<u8> = Vec::with_capacity(READ_BUF_SIZE);
    loop {
        let n = match read_bounded_line(&mut reader, &mut buf).await {
            Ok(Some(n)) => n,
            Ok(None) => break, // EOF：插件退出
            Err(e) => {
                tracing::warn!("plugin RPC read error: {e}");
                break;
            }
        };
        let trimmed = String::from_utf8_lossy(&buf[..n]);
        let trimmed = trimmed.trim_end_matches(['\n', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        let frame = match decode_frame(trimmed) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!("plugin RPC invalid frame: {e}");
                continue;
            }
        };
        match frame.kind() {
            FrameKind::Request => {
                // 插件→网关请求（回调）
                let method = frame.method.unwrap_or_default();
                let params = frame.params.unwrap_or(Value::Null);
                let result = shared.callbacks.handle(&method, params);
                let response = match result {
                    Ok(value) => Frame {
                        id: frame.id,
                        result: Some(value),
                        ..Default::default()
                    },
                    Err(err) => Frame {
                        id: frame.id,
                        error: Some(err),
                        ..Default::default()
                    },
                };
                if let Err(e) = shared.write_frame(&response).await {
                    tracing::warn!("plugin RPC callback response write failed: {e}");
                }
            }
            FrameKind::Notification => {
                // 通知：M1 只记日志
                if frame.event.as_deref() == Some("log") {
                    tracing::info!("[plugin] {}", frame.message.as_deref().unwrap_or(""));
                } else if let Some(m) = frame.method.as_deref() {
                    tracing::debug!("[plugin notify] {m}");
                }
            }
            FrameKind::Response => {
                // 网关在途请求的应答
                let id = frame.id;
                if let Some(tx) = shared.pending.write().await.remove(&id) {
                    let _ = tx.send(frame);
                }
            }
            FrameKind::Event => {
                // 流式帧：M1 只记日志
                if frame.event.as_deref() == Some("chunk") {
                    tracing::debug!(
                        "[plugin stream] chunk {} bytes",
                        frame.data.as_ref().map(|d| d.len()).unwrap_or(0)
                    );
                } else if let Some(ev) = frame.event.as_deref() {
                    tracing::debug!("[plugin event] {ev}");
                }
            }
            FrameKind::Unknown => {
                tracing::warn!("plugin RPC unknown frame kind: {frame:?}");
            }
        }
    }
    // EOF：所有在途请求失败
    {
        let failed: Vec<_> = shared
            .pending
            .write()
            .await
            .drain()
            .map(|(_, tx)| {
                let _ = tx.send(Frame {
                    error: Some(RpcError::new("plugin process exited", "eof")),
                    ..Default::default()
                });
            })
            .collect();
        if !failed.is_empty() {
            tracing::warn!(
                "plugin RPC channel closed, {} pending requests failed",
                failed.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 简单回声回调：收到 `ping` 回 `pong`，其余报「未实现」。
    struct EchoCallbacks;

    impl CallbackHandler for EchoCallbacks {
        fn handle(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
            if method == "ping" {
                Ok(Value::String("pong".into()))
            } else {
                Err(RpcError::new(
                    format!("echo: unsupported '{method}'"),
                    "unsupported",
                ))
            }
        }
    }

    /// 建一对 duplex 通道：client 侧用来测 call，plugin 侧是对方的写入端。
    fn duplex_pair() -> (BoxReader, BoxWriter, BoxReader, BoxWriter) {
        // client→plugin 方向
        let (client_out, plugin_in) = tokio::io::duplex(READ_BUF_SIZE);
        // plugin→client 方向
        let (plugin_out, client_in) = tokio::io::duplex(READ_BUF_SIZE);
        (
            Box::new(client_in),
            Box::new(client_out),
            Box::new(plugin_in),
            Box::new(plugin_out),
        )
    }

    /// 从 mock 插件侧读一帧（测试用）。
    async fn read_one_frame<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> Frame {
        let mut buf = Vec::new();
        reader
            .read_until(b'\n', &mut buf)
            .await
            .expect("read frame");
        decode_frame(String::from_utf8_lossy(&buf).trim()).expect("decode frame")
    }

    #[test]
    fn frame_encode_decode_roundtrip() {
        let frame = Frame {
            id: 7,
            method: Some("chat".into()),
            params: Some(serde_json::json!({"model": "gpt-4"})),
            ..Default::default()
        };
        let line = encode_frame(&frame).unwrap();
        let decoded = decode_frame(&line).unwrap();
        assert_eq!(decoded.id, 7);
        assert_eq!(decoded.method.as_deref(), Some("chat"));
        assert_eq!(decoded.params.as_ref().unwrap()["model"], "gpt-4");
        assert!(decoded.is_request());
    }

    #[test]
    fn frame_result_null_is_some() {
        // `{"id":7,"result":null}` 必须读成 `Some(Value::Null)`，不能是 `None`
        let line = r#"{"id":7,"result":null}"#;
        let frame = decode_frame(line).unwrap();
        assert!(frame.result.is_some(), "result:null 应读成 Some");
        assert_eq!(frame.result.as_ref().unwrap(), &Value::Null);
        assert!(frame.is_response());
    }

    #[test]
    fn frame_missing_result_is_none() {
        let line = r#"{"id":7,"method":"ping"}"#;
        let frame = decode_frame(line).unwrap();
        assert!(frame.result.is_none(), "缺 result 字段应读成 None");
        assert!(frame.is_request());
    }

    #[test]
    fn frame_event_and_notification_kinds() {
        // 流式 head 帧
        let head = decode_frame(r#"{"id":3,"event":"head","status":200,"headers":{"content-type":"text/event-stream"}}"#).unwrap();
        assert!(head.is_event());
        assert_eq!(head.status, Some(200));
        // 通知
        let notif =
            decode_frame(r#"{"id":0,"event":"log","level":"info","message":"hello"}"#).unwrap();
        assert_eq!(notif.kind(), FrameKind::Event); // event 帧也算 Event 类
        assert_eq!(notif.message.as_deref(), Some("hello"));
        // 带 method 且 id=0 的通知
        let notify =
            decode_frame(r#"{"id":0,"method":"log","params":{"level":"info","message":"hi"}}"#)
                .unwrap();
        assert!(notify.is_notification());
    }

    #[test]
    fn frame_unknown_is_unknown() {
        let frame = decode_frame(r#"{"id":5}"#).unwrap();
        assert_eq!(frame.kind(), FrameKind::Unknown);
    }

    #[test]
    fn frame_error_deserialize() {
        let frame = decode_frame(r#"{"id":7,"error":{"message":"boom","code":"test"}}"#).unwrap();
        assert!(frame.is_response());
        assert_eq!(frame.error.as_ref().unwrap().message, "boom");
        assert_eq!(frame.error.as_ref().unwrap().code.as_deref(), Some("test"));
    }

    #[tokio::test]
    async fn call_and_response_via_duplex() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();

        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 模拟插件端：读 client 发来的请求，回一个应答
        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            assert_eq!(frame.method.as_deref(), Some("ping"));
            // 回应答
            let response = Frame {
                id: frame.id,
                result: Some(serde_json::json!("pong")),
                ..Default::default()
            };
            let mut w = plugin_write;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        });

        let result = client.call("ping", Value::Null).await;
        assert_eq!(result.unwrap(), Value::String("pong".into()));
        plugin_task.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn call_error_response() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            let response = Frame {
                id: frame.id,
                error: Some(RpcError::new("remote boom", "remote_error")),
                ..Default::default()
            };
            let mut w = plugin_write;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
        });

        let result = client.call("chat", Value::Null).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().message, "remote boom");
        plugin_task.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn callback_from_plugin() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(EchoCallbacks));

        // 插件端发一个回调请求（id=2，偶数）
        let plugin_task = tokio::spawn(async move {
            let mut w = plugin_write;
            let frame = Frame {
                id: 2,
                method: Some("ping".into()),
                params: Some(Value::Null),
                ..Default::default()
            };
            w.write_all(serde_json::to_string(&frame).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
            // 读回调应答
            let mut reader = BufReader::new(plugin_read);
            let resp = read_one_frame(&mut reader).await;
            assert_eq!(resp.id, 2);
            assert_eq!(resp.result.unwrap(), Value::String("pong".into()));
        });

        plugin_task.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn notification_does_not_expect_response() {
        let (client_read, client_write, plugin_read, _plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 插件端读通知帧
        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            assert_eq!(frame.id, 0);
            assert_eq!(frame.method.as_deref(), Some("log"));
        });

        client
            .notify("log", serde_json::json!({"message": "hi"}))
            .await
            .unwrap();
        plugin_task.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn eof_fails_pending() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 插件端直接关闭（drop）
        drop(plugin_read);
        drop(plugin_write);

        // client 发请求，插件已退出 → 应答失败
        let result = client.call("ping", Value::Null).await;
        assert!(result.is_err(), "插件退出后请求应失败");
        client.shutdown();
    }

    #[tokio::test]
    async fn calls_use_odd_incrementing_ids() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 插件端：读 3 个请求，断言 id 都是奇数且递增，各回一个应答
        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let mut ids: Vec<i64> = Vec::new();
            for i in 0..3 {
                let mut buf = Vec::new();
                reader.read_until(b'\n', &mut buf).await.unwrap();
                let frame: Frame =
                    serde_json::from_str(String::from_utf8_lossy(&buf).trim()).unwrap();
                ids.push(frame.id);
                let response = Frame {
                    id: frame.id,
                    result: Some(serde_json::json!(i)),
                    ..Default::default()
                };
                plugin_write
                    .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                    .await
                    .unwrap();
                plugin_write.write_all(b"\n").await.unwrap();
            }
            plugin_write.flush().await.unwrap();
            ids
        });

        let r1 = client.call("a", Value::Null).await.unwrap();
        let r2 = client.call("b", Value::Null).await.unwrap();
        let r3 = client.call("c", Value::Null).await.unwrap();
        assert_eq!(
            (r1, r2, r3),
            (
                serde_json::json!(0),
                serde_json::json!(1),
                serde_json::json!(2)
            )
        );
        let ids = plugin_task.await.unwrap();
        assert!(
            ids.iter().all(|id| id % 2 == 1),
            "client id 应为奇数: {ids:?}"
        );
        assert!(ids[0] < ids[1] && ids[1] < ids[2], "id 应递增: {ids:?}");
        client.shutdown();
    }

    #[test]
    fn present_or_null_helper() {
        // 记录一个语言事实：默认 `Option<Value>` 把 JSON null 读成 None，
        // 这正是 `Frame.result` 需要自定义反序列化的原因。
        let v: Option<Value> = serde_json::from_str::<Option<Value>>("null").unwrap();
        assert!(
            v.is_none(),
            "默认 Option 把 null 读成 None（这正是要修复的）"
        );
    }

    #[test]
    fn frame_skip_serializing_none_fields() {
        let frame = Frame {
            id: 1,
            method: Some("ping".into()),
            ..Default::default()
        };
        let line = encode_frame(&frame).unwrap();
        // 不应包含 result/error/event 等 None 字段
        assert!(!line.contains("result"), "None 字段应跳过");
        assert!(!line.contains("error"), "None 字段应跳过");
        assert!(!line.contains("event"), "None 字段应跳过");
    }
}
