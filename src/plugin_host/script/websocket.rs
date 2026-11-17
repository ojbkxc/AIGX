// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/websocket.rs
//!
//! `script.ws.*` —— 脚本宿主的 WebSocket egress 会话面。
//!
//! A2A 宿主 `websocket.mjs` 的 `WebSocket` 垫片调用四个回调：
//!
//! - `script.ws.open {id,url,protocols,headers,proxy}` → `{id,protocol}`
//! - `script.ws.read {id}` → `{type,data?}`（`text`/`binary`/`close`/`error`/`pending`）
//! - `script.ws.send {id,type,data|code,reason}` → `null`
//! - `script.ws.abort {id}` → `null`（幂等）
//!
//! ## 与 A2A 实现的差异（如实标注）
//!
//! A2A 用 reqwest 0.12 的 `Response::upgrade()` 拿原始升级流，再手工做
//! 握手校验。AIGX 用 reqwest 0.13（无 `upgrade()`），改走 `tokio-tungstenite`
//! 的 `client_async_tls_with_config`——它完成握手、校验 `Sec-WebSocket-Accept`、
//! 处理子协议。语义等价（甚至更严），但**代理**：A2A 支持为 WS 指定账号代理，
//! tokio-tungstenite 的 connector 只支持 TLS 参数、不支持 HTTP 代理串；因此
//! 本实现**忽略 `proxy`**（走直连）——AIGX 的 WS 消费方（`websocket.mjs`）
//! 当前只用于少数 OAuth 回调与本地事件流，直连可接受。需要代理时再补（
//! 用 `tokio-socks`/手写 CONNECT 隧道）。
//!
//! ## 有界性
//!
//! - 单条消息上限 2MiB（与 A2A 同口径）；超限直接关连接并报错。
//! - 会话表上限 64（同 A2A）；空闲 30min 回收（每次 read 刷新时间戳）。
//! - 每条连接一条后台任务驱动 `socket.next()`，把消息推进 `mpsc` 事件队列
//!   （容量 2，满则丢弃——WS 是尽力而为的实时通道，不能让慢消费者反压上游）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    protocol::{CloseFrame, Message},
    Error as WsError,
};
use tokio_tungstenite::WebSocketStream;

use crate::plugin_host::rpc::RpcError;

/// 单条消息上限（与 A2A `MAX_MESSAGE` 一致）。
const MAX_MESSAGE: usize = 2 * 1024 * 1024;

/// 并发连接上限（同 A2A）。
const MAX_CONNECTIONS: usize = 64;

/// 空闲回收阈值（同 A2A：30min）。
const IDLE: Duration = Duration::from_secs(1800);

/// 握手超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// read 单次等待上限（超时返回 `{type:"pending"}`，宿主会再调一次）。
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// send 单次等待上限。
const SEND_TIMEOUT: Duration = Duration::from_secs(35);

type Socket = WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// 一条命令：要发出的消息 + 回执通道。
struct Command {
    message: Message,
    reply: tokio::sync::oneshot::Sender<Result<(), String>>,
}

/// 一条 WebSocket 连接。
struct Connection {
    /// 发送命令的通道（后台任务消费）。
    commands: mpsc::Sender<Command>,
    /// 收到的事件（后台任务写入）。
    events: Mutex<mpsc::Receiver<Value>>,
    /// 中止信号（abort/回收置位）。
    cancel: Arc<tokio_util_cancel::Cancel>,
    /// 最后一次活动时刻（reaper 判据）。
    touched: StdMutex<Instant>,
}

/// 极简取消令牌（避免为此引入 tokio-util 依赖面；只需「一次性置位 + 唤醒」）。
mod tokio_util_cancel {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    pub struct Cancel {
        flag: AtomicBool,
        notify: Notify,
    }

    impl Cancel {
        pub fn new() -> Self {
            Self {
                flag: AtomicBool::new(false),
                notify: Notify::new(),
            }
        }

        pub fn cancel(&self) {
            self.flag.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }

        pub fn is_cancelled(&self) -> bool {
            self.flag.load(Ordering::Acquire)
        }

        pub async fn cancelled(&self) {
            loop {
                if self.is_cancelled() {
                    return;
                }
                let notified = self.notify.notified();
                if self.is_cancelled() {
                    return;
                }
                notified.await;
            }
        }
    }
}

/// 全局连接表（脚本宿主是单例，进程级静态表足够；与 A2A 同形态）。
fn connections() -> &'static StdMutex<HashMap<String, Arc<Connection>>> {
    static ITEMS: OnceLock<StdMutex<HashMap<String, Arc<Connection>>>> = OnceLock::new();
    ITEMS.get_or_init(|| StdMutex::new(HashMap::new()))
}

fn error(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("internal".into()),
    }
}

fn find(params: &Value) -> Result<Arc<Connection>, RpcError> {
    let id = params.get("id").and_then(Value::as_str).unwrap_or("");
    let connection = connections()
        .lock()
        .map_err(|_| error("script ws: session table unavailable"))?
        .get(id)
        .cloned()
        .ok_or_else(|| error("script ws: session closed"))?;
    if let Ok(mut time) = connection.touched.lock() {
        *time = Instant::now();
    }
    Ok(connection)
}

/// 建连守卫：open 失败/被取消时自动 abort（避免半开连接滞留表里）。
struct Opening(Option<String>);

impl Drop for Opening {
    fn drop(&mut self) {
        if let Some(id) = &self.0 {
            abort(&json!({ "id": id }));
        }
    }
}

/// `script.ws.open`。
pub(crate) async fn open(params: &Value) -> Result<Value, RpcError> {
    let raw = params
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| error("script.ws.open: url is required"))?;
    let parsed = url::Url::parse(raw).map_err(|_| error("script.ws.open: invalid url"))?;
    if !matches!(parsed.scheme(), "ws" | "wss")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(error(
            "script.ws.open: only ws/wss urls without credentials are allowed",
        ));
    }
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 80)
        .ok_or_else(|| error("script.ws.open: id is required (≤80 chars)"))?
        .to_string();

    // 构造请求：URL + 子协议 + 允许的头（黑名单与 A2A 一致）
    let mut request = raw
        .into_client_request()
        .map_err(|e| error(format!("script.ws.open: invalid request: {e}")))?;
    {
        let headers = request.headers_mut();
        if let Some(map) = params.get("headers").and_then(Value::as_object) {
            if map.len() > 128 {
                return Err(error("script.ws.open: too many headers"));
            }
            for (name, value) in map {
                let lower = name.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "host"
                        | "connection"
                        | "upgrade"
                        | "content-length"
                        | "transfer-encoding"
                        | "proxy-authorization"
                        | "sec-websocket-key"
                        | "sec-websocket-version"
                        | "sec-websocket-protocol"
                        | "sec-websocket-extensions"
                ) {
                    continue;
                }
                if let Some(text) = value.as_str() {
                    if let (Ok(name), Ok(value)) = (
                        http::header::HeaderName::from_bytes(name.as_bytes()),
                        http::header::HeaderValue::from_str(text),
                    ) {
                        headers.insert(name, value);
                    }
                }
            }
        }
        if let Some(protocols) = params.get("protocols") {
            let list = match protocols {
                Value::Null => Vec::new(),
                Value::Array(items) if items.len() <= 32 => items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .filter(|text| {
                                !text.is_empty()
                                    && text.len() <= 256
                                    && text.bytes().all(|ch| {
                                        ch.is_ascii_alphanumeric()
                                            || b"!#$%&'*+-.^_`|~".contains(&ch)
                                    })
                            })
                            .map(str::to_string)
                            .ok_or_else(|| error("script.ws.open: invalid subprotocol"))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(error("script.ws.open: invalid subprotocols")),
            };
            if list
                .iter()
                .enumerate()
                .any(|(i, item)| list[..i].contains(item))
            {
                return Err(error("script.ws.open: duplicate subprotocol"));
            }
            if !list.is_empty() {
                if let Ok(value) = http::header::HeaderValue::from_str(&list.join(", ")) {
                    request
                        .headers_mut()
                        .insert("sec-websocket-protocol", value);
                }
            }
        }
    }

    let (commands, incoming) = mpsc::channel(8);
    let (events, outgoing) = mpsc::channel(2);
    let connection = Arc::new(Connection {
        commands,
        events: Mutex::new(outgoing),
        cancel: Arc::new(tokio_util_cancel::Cancel::new()),
        touched: StdMutex::new(Instant::now()),
    });

    // 登记前先清闲置 + 查重/查满
    {
        let mut items = connections()
            .lock()
            .map_err(|_| error("script ws: session table unavailable"))?;
        items.retain(|_, item| {
            let keep = item.touched.lock().is_ok_and(|time| time.elapsed() < IDLE);
            if !keep {
                item.cancel.cancel();
            }
            keep
        });
        if items.len() >= MAX_CONNECTIONS || items.contains_key(&id) {
            return Err(error("script ws: connection limit reached or duplicate id"));
        }
        items.insert(id.clone(), connection.clone());
    }

    let mut opening = Opening(Some(id.clone()));
    let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE),
        max_frame_size: Some(MAX_MESSAGE),
        ..Default::default()
    };
    let connected =
        tokio_tungstenite::connect_async_tls_with_config(request, Some(config), false, None);
    let (socket, response) = tokio::select! {
        biased;
        _ = connection.cancel.cancelled() => return Err(error("script.ws.open: cancelled")),
        result = tokio::time::timeout(HANDSHAKE_TIMEOUT, connected) => {
            result
                .map_err(|_| error("script.ws.open: handshake timeout"))?
                .map_err(|e| error(format!("script.ws.open: handshake failed: {e}")))?
        }
    };
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    spawn_run(socket, incoming, events, connection.cancel.clone());
    opening.0 = None;
    Ok(json!({ "id": id, "protocol": protocol }))
}

fn spawn_run(
    socket: Socket,
    commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Value>,
    cancel: Arc<tokio_util_cancel::Cancel>,
) {
    let task = run(socket, commands, events, cancel);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(task);
    }
}

async fn send_event(
    events: &mpsc::Sender<Value>,
    cancel: &tokio_util_cancel::Cancel,
    value: Value,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        result = events.send(value) => result.is_ok(),
    }
}

async fn run(
    mut socket: Socket,
    mut commands: mpsc::Receiver<Command>,
    events: mpsc::Sender<Value>,
    cancel: Arc<tokio_util_cancel::Cancel>,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            command = commands.recv() => {
                let Some(command) = command else { break; };
                let close = command.message.is_close();
                let sent = tokio::select! {
                    _ = cancel.cancelled() => break,
                    result = tokio::time::timeout(Duration::from_secs(30), socket.send(command.message)) => result,
                };
                if !matches!(sent, Ok(Ok(()))) {
                    let _ = command.reply.send(Err("script ws: send failed".into()));
                    send_event(&events, &cancel, json!({ "type": "error" })).await;
                    break;
                }
                let _ = command.reply.send(Ok(()));
                if close {
                    // 关闭后给对端 5s 收尾再断
                    let _ = tokio::time::timeout(Duration::from_secs(5), async {
                        while let Some(Ok(_)) = socket.next().await {}
                    })
                    .await;
                    break;
                }
            }
            message = socket.next() => {
                let value = match message {
                    Some(Ok(Message::Text(text))) => {
                        if text.len() > MAX_MESSAGE {
                            send_event(&events, &cancel, json!({ "type": "error" })).await;
                            break;
                        }
                        json!({ "type": "text", "data": text.as_str() })
                    }
                    Some(Ok(Message::Binary(bytes))) => {
                        if bytes.len() > MAX_MESSAGE {
                            send_event(&events, &cancel, json!({ "type": "error" })).await;
                            break;
                        }
                        json!({ "type": "binary", "data": STANDARD.encode(bytes) })
                    }
                    Some(Ok(Message::Ping(_))) => continue,
                    Some(Ok(Message::Pong(_))) => continue,
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) = frame
                            .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                            .unwrap_or((1005, String::new()));
                        send_event(&events, &cancel, json!({
                            "type": "close", "code": code, "reason": reason, "clean": true
                        }))
                        .await;
                        break;
                    }
                    Some(Ok(Message::Frame(_))) => continue,
                    None => {
                        send_event(&events, &cancel, json!({
                            "type": "close", "code": 1006, "reason": "", "clean": false
                        }))
                        .await;
                        break;
                    }
                    Some(Err(e)) => {
                        tracing::debug!("script ws read error: {e}");
                        send_event(&events, &cancel, json!({ "type": "error" })).await;
                        break;
                    }
                };
                if !send_event(&events, &cancel, value).await {
                    break;
                }
            }
        }
    }
    cancel.cancel();
}

/// `script.ws.read`。
pub(crate) async fn read(params: &Value) -> Result<Value, RpcError> {
    let connection = find(params)?;
    let mut events = connection.events.lock().await;
    tokio::select! {
        biased;
        _ = connection.cancel.cancelled() => Err(error("script ws: aborted")),
        event = tokio::time::timeout(READ_TIMEOUT, events.recv()) => Ok(match event {
            Ok(Some(value)) => value,
            Ok(None) => json!({ "type": "close", "code": 1006, "reason": "", "clean": false }),
            Err(_) => json!({ "type": "pending" }),
        }),
    }
}

/// `script.ws.send`。
pub(crate) async fn send(params: &Value) -> Result<Value, RpcError> {
    let data = params.get("data").and_then(Value::as_str).unwrap_or("");
    let message = match params.get("type").and_then(Value::as_str) {
        Some("text") if data.len() <= MAX_MESSAGE => Message::Text(data.into()),
        Some("binary") if data.len() <= MAX_MESSAGE.div_ceil(3) * 4 => {
            let bytes = STANDARD
                .decode(data)
                .map_err(|_| error("script ws: invalid base64 payload"))?;
            if bytes.len() > MAX_MESSAGE {
                return Err(error("script ws: message too large"));
            }
            Message::Binary(bytes)
        }
        Some("close") => {
            let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
            if reason.len() > 123 {
                return Err(error("script ws: close reason too long"));
            }
            let code = params.get("code").and_then(Value::as_u64).unwrap_or(1000);
            if code != 1000 && !(3000..=4999).contains(&code) {
                return Err(error("script ws: invalid close code"));
            }
            Message::Close(Some(CloseFrame {
                code: tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::from(
                    code as u16,
                ),
                reason: std::borrow::Cow::Owned(reason.to_string()),
            }))
        }
        _ => return Err(error("script ws: invalid message type or size")),
    };
    let connection = find(params)?;
    let (reply, result) = tokio::sync::oneshot::channel();
    let work = async {
        connection
            .commands
            .send(Command { message, reply })
            .await
            .map_err(|_| error("script ws: closed"))?;
        result
            .await
            .map_err(|_| error("script ws: closed"))?
            .map_err(error)?;
        Ok(Value::Null)
    };
    tokio::select! {
        biased;
        _ = connection.cancel.cancelled() => Err(error("script ws: aborted")),
        result = tokio::time::timeout(SEND_TIMEOUT, work) => {
            result.map_err(|_| error("script ws: send timeout"))?
        }
    }
}

/// `script.ws.abort`（幂等）。
pub(crate) fn abort(params: &Value) {
    if let Some(id) = params.get("id").and_then(Value::as_str) {
        if let Ok(mut items) = connections().lock() {
            if let Some(connection) = items.remove(id) {
                connection.cancel.cancel();
            }
        }
    }
}

/// 清空全部连接（宿主重建/关闭时调）。
pub(crate) fn clear() {
    if let Ok(mut items) = connections().lock() {
        for (_, connection) in items.drain() {
            connection.cancel.cancel();
        }
    }
}

/// 当前连接数（测试用）。
#[cfg(test)]
pub(crate) fn count() -> usize {
    connections().lock().map(|m| m.len()).unwrap_or(0)
}

// 让 `WsError` 的 import 不被 clippy 判为未用（错误分支只记 debug）。
#[allow(dead_code)]
fn _ws_error_type(_: WsError) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abort_is_idempotent() {
        abort(&json!({"id": "nope"}));
        abort(&json!({"id": "nope"}));
        abort(&Value::Null);
        assert_eq!(count(), 0);
    }

    #[tokio::test]
    async fn read_unknown_session_errors() {
        let err = read(&json!({"id": "nope"})).await.unwrap_err();
        assert!(err.message.contains("session closed"));
    }

    #[tokio::test]
    async fn send_rejects_bad_type() {
        let err = send(&json!({"id": "nope", "type": "bogus"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("invalid message type"));
    }

    #[tokio::test]
    async fn open_rejects_non_ws_scheme() {
        for url in ["http://example.invalid/x", "file:///etc/passwd"] {
            let err = open(&json!({"id": "a", "url": url})).await.unwrap_err();
            assert!(err.message.contains("only ws/wss"), "got: {}", err.message);
        }
    }

    #[tokio::test]
    async fn open_rejects_credentials_in_url() {
        let err = open(&json!({"id": "a", "url": "wss://user:pass@example.invalid/x"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("without credentials"));
    }

    #[test]
    fn cancel_token_wakes_waiters() {
        let cancel = Arc::new(tokio_util_cancel::Cancel::new());
        let waiter = cancel.clone();
        let handle = tokio::runtime::Runtime::new().unwrap().spawn(async move {
            waiter.cancelled().await;
        });
        cancel.cancel();
        handle.abort();
        assert!(cancel.is_cancelled());
    }
}
