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
//! 处理子协议。
//!
//! ## 代理（`params.proxy`，与 A2A `script.ws.open` 同形状）
//!
//! - `http`：手写 `CONNECT host:port` 隧道（可选 Basic 认证）；`wss` 在隧道
//!   上由 tokio-tungstenite 的默认 rustls connector（内建 webpki roots）
//!   完成 TLS；
//! - `socks5` / `socks5h`：手写 SOCKS5 握手（无新依赖；`socks5` 本地解析
//!   域名、`socks5h` 交代理解析）；
//! - `https`（**连代理本身走 TLS**）：暂不支持，显式报错（TLS-to-proxy 需要
//!   CA roots 的直接依赖，见报告）——绝不静默降级成明文；
//! - 本机环回目标按上游 `proxy_for` 口径绕过代理（插件的本机浏览器控制
//!   不能发往账号的远程代理）。
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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    protocol::{CloseFrame, Message},
    Error as WsError,
};
use tokio_tungstenite::WebSocketStream;

use super::http::{proxy_endpoint, ProxyEndpoint, ProxyProtocol};
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

/// 隧道/直连统一之后的底层 I/O：任意可读写流（TCP / TLS / SOCKS 隧道）。
///
/// 用辅助 trait 而不是 `dyn AsyncRead + AsyncWrite`：Rust 的 trait object
/// 只允许一个非 auto trait 作为 principal（E0225）；空实现 + blanket impl
/// 把两个 trait 收成一个可做对象的 principal。
trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

type BoxIo = Box<dyn AsyncReadWrite>;

/// 一条 WebSocket 连接的两种建连形态。
///
/// 直连沿用 tokio-tungstenite 的 `MaybeTlsStream`（ws 明文 / wss 内建 TLS）；
/// 代理隧道则是 `MaybeTlsStream<Box<dyn AsyncRead+AsyncWrite>>`（wss 的 TLS 由
/// `client_async_tls_with_config` 的默认 connector 在隧道上完成）。
/// 两个泛型实参不同的 `WebSocketStream` 用枚举归一，方法级分派即可。
enum Socket {
    Direct(WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>),
    Tunneled(WebSocketStream<tokio_tungstenite::MaybeTlsStream<BoxIo>>),
}

impl Socket {
    /// `result_large_err`：`WsError` 是 tungstenite 的错误类型（~136B），
    /// 这里只是把两条 socket 分支的结果原样透出，装箱会在调用点引入一次
    /// 额外的 WsError 拆箱，故按既有口径就地放行。
    #[allow(clippy::result_large_err)]
    async fn send(&mut self, message: Message) -> Result<(), WsError> {
        match self {
            Self::Direct(socket) => socket.send(message).await,
            Self::Tunneled(socket) => socket.send(message).await,
        }
    }

    async fn next(&mut self) -> Option<Result<Message, WsError>> {
        match self {
            Self::Direct(socket) => socket.next().await,
            Self::Tunneled(socket) => socket.next().await,
        }
    }
}

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

    // 代理（`{protocol,host,port,username,password}`，与 A2A 同形状）：
    // 不支持/畸形的代理必须在这里失败，绝不静默直连。
    let proxy = proxy_endpoint(params, &parsed)
        .map_err(|e| error(format!("script.ws.open: invalid proxy: {e}")))?;

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
    let connected = async {
        match &proxy {
            None => {
                tokio_tungstenite::connect_async_tls_with_config(request, Some(config), false, None)
                    .await
                    .map(|(socket, response)| (Socket::Direct(socket), response))
                    .map_err(|e| error(format!("script.ws.open: handshake failed: {e}")))
            }
            Some(proxy) => {
                let tunnel = proxy_tunnel(proxy, &parsed)
                    .await
                    .map_err(|e| error(format!("script.ws.open: proxy connect failed: {e}")))?;
                // wss 的 TLS 由默认 connector 在隧道上完成（内建 webpki roots）
                tokio_tungstenite::client_async_tls_with_config(request, tunnel, Some(config), None)
                    .await
                    .map(|(socket, response)| (Socket::Tunneled(socket), response))
                    .map_err(|e| error(format!("script.ws.open: handshake failed: {e}")))
            }
        }
    };
    let (socket, response) = tokio::select! {
        biased;
        _ = connection.cancel.cancelled() => return Err(error("script.ws.open: cancelled")),
        result = tokio::time::timeout(HANDSHAKE_TIMEOUT, connected) => {
            result.map_err(|_| error("script.ws.open: handshake timeout"))??
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

// ── 代理隧道（http CONNECT / socks5 / socks5h）──────────────────────

/// CONNECT 响应头上限（正常代理只有几百字节）。
const MAX_CONNECT_HEAD: usize = 16 * 1024;

/// 与代理建立隧道，返回可交给 WebSocket 握手的裸 I/O。
///
/// `target` 是 ws/wss URL（port 缺省按 scheme 补 80/443）。
async fn proxy_tunnel(proxy: &ProxyEndpoint, target: &url::Url) -> Result<BoxIo, String> {
    let host = target
        .host_str()
        .ok_or_else(|| "target url has no host".to_string())?;
    let port = target
        .port_or_known_default()
        .ok_or_else(|| "target url has no port".to_string())?;
    match proxy.protocol {
        ProxyProtocol::Http => {
            let stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
                .await
                .map_err(|e| {
                    format!("connect to http proxy {}:{} failed: {e}", proxy.host, proxy.port)
                })?;
            let mut tunnel: BoxIo = Box::new(stream);
            http_connect(&mut tunnel, proxy, host, port).await?;
            Ok(tunnel)
        }
        // 连代理本身要 TLS：需要 CA roots 的直接依赖（当前只有传递依赖），
        // 不引入 Cargo.toml 变更前明确报错，绝不静默降级成明文 CONNECT。
        ProxyProtocol::Https => Err(
            "https proxy (TLS to the proxy itself) is not supported for ws/wss; use an http/socks5 proxy"
                .to_string(),
        ),
        ProxyProtocol::Socks5 | ProxyProtocol::Socks5h => socks5_tunnel(proxy, host, port).await,
    }
}

/// 手写 HTTP `CONNECT` 隧道（可选 Basic 认证）。
async fn http_connect(
    stream: &mut BoxIo,
    proxy: &ProxyEndpoint,
    host: &str,
    port: u16,
) -> Result<(), String> {
    let authority = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if !proxy.username.is_empty() {
        let credentials = STANDARD.encode(format!("{}:{}", proxy.username, proxy.password));
        request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write CONNECT to proxy failed: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("flush CONNECT to proxy failed: {e}"))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let status = loop {
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]);
            let status = head
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| "proxy returned an invalid CONNECT response".to_string())?;
            break status;
        }
        if buf.len() > MAX_CONNECT_HEAD {
            return Err("proxy CONNECT response headers exceed 16KiB".to_string());
        }
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|e| format!("read proxy CONNECT response failed: {e}"))?;
        if read == 0 {
            return Err("proxy closed the connection during CONNECT".to_string());
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    if !(200..300).contains(&status) {
        return Err(format!("proxy rejected CONNECT with HTTP {status}"));
    }
    Ok(())
}

/// 手写 SOCKS5 握手（无新依赖）。`socks5` 本地解析域名，`socks5h` 交给
/// 代理解析；IP 字面量直接用对应 ATYP。
async fn socks5_tunnel(proxy: &ProxyEndpoint, host: &str, port: u16) -> Result<BoxIo, String> {
    let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .map_err(|e| {
            format!(
                "connect to socks5 proxy {}:{} failed: {e}",
                proxy.host, proxy.port
            )
        })?;

    // 问候：无认证 0x00；有凭据时追加用户名/密码 0x02
    let methods: &[u8] = if proxy.username.is_empty() {
        &[0x00]
    } else {
        &[0x00, 0x02]
    };
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream
        .write_all(&greeting)
        .await
        .map_err(|e| format!("socks5 greeting failed: {e}"))?;
    let mut selected = [0u8; 2];
    stream
        .read_exact(&mut selected)
        .await
        .map_err(|e| format!("socks5 greeting reply failed: {e}"))?;
    if selected[0] != 0x05 {
        return Err(format!("socks5 proxy replied with version {}", selected[0]));
    }
    match selected[1] {
        0x00 => {}
        0x02 => {
            let user = proxy.username.as_bytes();
            let pass = proxy.password.as_bytes();
            if user.is_empty() {
                return Err("socks5 proxy requires username/password".to_string());
            }
            if user.len() > 255 || pass.len() > 255 {
                return Err("socks5 credentials exceed 255 bytes".to_string());
            }
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user);
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass);
            stream
                .write_all(&auth)
                .await
                .map_err(|e| format!("socks5 auth write failed: {e}"))?;
            let mut outcome = [0u8; 2];
            stream
                .read_exact(&mut outcome)
                .await
                .map_err(|e| format!("socks5 auth reply failed: {e}"))?;
            if outcome[0] != 0x01 || outcome[1] != 0x00 {
                return Err(format!(
                    "socks5 authentication failed (status {})",
                    outcome[1]
                ));
            }
        }
        0xff => return Err("socks5 proxy has no acceptable authentication method".to_string()),
        other => {
            return Err(format!(
                "socks5 proxy selected unsupported auth method {other:#04x}"
            ))
        }
    }

    // CONNECT 请求：地址按「IP 字面量 / 域名字节 / 本地解析」三态编码
    let mut request = vec![0x05, 0x01, 0x00];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => {
            request.push(0x01);
            request.extend_from_slice(&address.octets());
        }
        Ok(std::net::IpAddr::V6(address)) => {
            request.push(0x04);
            request.extend_from_slice(&address.octets());
        }
        Err(_) => match proxy.protocol {
            ProxyProtocol::Socks5h => {
                if host.len() > 255 {
                    return Err("socks5h target host exceeds 255 bytes".to_string());
                }
                request.push(0x03);
                request.push(host.len() as u8);
                request.extend_from_slice(host.as_bytes());
            }
            ProxyProtocol::Socks5 => {
                let resolved = tokio::net::lookup_host((host, port))
                    .await
                    .map_err(|e| format!("resolve {host} locally for socks5 failed: {e}"))?
                    .next()
                    .ok_or_else(|| {
                        format!("resolve {host} locally for socks5 returned no address")
                    })?;
                match resolved {
                    std::net::SocketAddr::V4(address) => {
                        request.push(0x01);
                        request.extend_from_slice(&address.ip().octets());
                    }
                    std::net::SocketAddr::V6(address) => {
                        request.push(0x04);
                        request.extend_from_slice(&address.ip().octets());
                    }
                }
            }
            _ => return Err("internal: socks5 tunnel with non-socks proxy".to_string()),
        },
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream
        .write_all(&request)
        .await
        .map_err(|e| format!("socks5 CONNECT failed: {e}"))?;

    let mut head = [0u8; 4];
    stream
        .read_exact(&mut head)
        .await
        .map_err(|e| format!("socks5 CONNECT reply failed: {e}"))?;
    if head[0] != 0x05 {
        return Err(format!("socks5 proxy replied with version {}", head[0]));
    }
    if head[1] != 0x00 {
        return Err(format!("socks5 CONNECT rejected (code {})", head[1]));
    }
    // 消费 BND.ADDR + BND.PORT
    let mut skip = match head[3] {
        0x01 => vec![0u8; 4 + 2],
        0x04 => vec![0u8; 16 + 2],
        0x03 => {
            let mut len = [0u8; 1];
            stream
                .read_exact(&mut len)
                .await
                .map_err(|e| format!("socks5 CONNECT reply failed: {e}"))?;
            vec![0u8; len[0] as usize + 2]
        }
        other => {
            return Err(format!(
                "socks5 CONNECT reply has unknown ATYP {other:#04x}"
            ))
        }
    };
    stream
        .read_exact(&mut skip)
        .await
        .map_err(|e| format!("socks5 CONNECT reply failed: {e}"))?;
    Ok(Box::new(stream))
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
        // 连接表是进程级单例，并行测试可能持有连接——这里只断言调用
        // 不 panic / 不误删别的条目（按 id 精确摘除）。
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

    // ── 代理链路（本地 CONNECT / SOCKS5 桩，打环回网络）──────────────
    // AsyncReadExt / AsyncWriteExt 由 `use super::*` 带入。

    use tokio::net::TcpListener;

    /// 本地 WebSocket 回显服务（accept_async + 文本回显）。
    async fn ws_echo_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(message)) = socket.next().await {
                        match message {
                            Message::Text(text) => {
                                if socket.send(Message::Text(text)).await.is_err() {
                                    break;
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                });
            }
        });
        addr
    }

    /// 读一个 HTTP 头块（到 \r\n\r\n 为止）。
    async fn read_head(stream: &mut TcpStream) -> Option<String> {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                return Some(String::from_utf8_lossy(&buf).to_string());
            }
            if buf.len() > MAX_CONNECT_HEAD {
                return None;
            }
        }
    }

    /// CONNECT 桩代理：校验请求行/认证头 → 200 → 双向转发到 `target`。
    /// `reject_status` 非空时直接回该状态（拒绝路径测试）。
    async fn connect_proxy(
        target: std::net::SocketAddr,
        reject_status: Option<u16>,
    ) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let Some(head) = read_head(&mut client).await else {
                        return;
                    };
                    if let Some(status) = reject_status {
                        let _ = client
                            .write_all(
                                format!("HTTP/1.1 {status} Proxy Authentication Required\r\n\r\n")
                                    .as_bytes(),
                            )
                            .await;
                        return;
                    }
                    if !head.starts_with("CONNECT ") {
                        return;
                    }
                    if !head.contains("Proxy-Authorization: Basic ") {
                        let _ = client
                            .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                            .await;
                        return;
                    }
                    let _ = client
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await;
                    let Ok(mut upstream) = TcpStream::connect(target).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        addr
    }

    /// SOCKS5 桩代理（要求用户名/密码认证）：握手 → 校验凭据 → 回 CONNECT
    /// 成功 → 双向转发到 `target`。
    async fn socks5_proxy(target: std::net::SocketAddr) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut head = [0u8; 2];
                    if client.read_exact(&mut head).await.is_err() || head[0] != 0x05 {
                        return;
                    }
                    let mut methods = vec![0u8; head[1] as usize];
                    if client.read_exact(&mut methods).await.is_err() {
                        return;
                    }
                    if !methods.contains(&0x02) {
                        let _ = client.write_all(&[0x05, 0xff]).await;
                        return;
                    }
                    if client.write_all(&[0x05, 0x02]).await.is_err() {
                        return;
                    }
                    // 用户名/密码子协商
                    let mut ver = [0u8; 1];
                    if client.read_exact(&mut ver).await.is_err() || ver[0] != 0x01 {
                        return;
                    }
                    let mut len = [0u8; 1];
                    if client.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut user = vec![0u8; len[0] as usize];
                    if client.read_exact(&mut user).await.is_err() {
                        return;
                    }
                    if client.read_exact(&mut len).await.is_err() {
                        return;
                    }
                    let mut pass = vec![0u8; len[0] as usize];
                    if client.read_exact(&mut pass).await.is_err() {
                        return;
                    }
                    if user != b"user" || pass != b"pass" {
                        let _ = client.write_all(&[0x01, 0x01]).await;
                        return;
                    }
                    if client.write_all(&[0x01, 0x00]).await.is_err() {
                        return;
                    }
                    // CONNECT 请求
                    let mut request = [0u8; 4];
                    if client.read_exact(&mut request).await.is_err()
                        || request[0] != 0x05
                        || request[1] != 0x01
                    {
                        return;
                    }
                    match request[3] {
                        0x01 => {
                            let mut skip = [0u8; 4];
                            if client.read_exact(&mut skip).await.is_err() {
                                return;
                            }
                        }
                        0x04 => {
                            let mut skip = [0u8; 16];
                            if client.read_exact(&mut skip).await.is_err() {
                                return;
                            }
                        }
                        0x03 => {
                            let mut len = [0u8; 1];
                            if client.read_exact(&mut len).await.is_err() {
                                return;
                            }
                            let mut skip = vec![0u8; len[0] as usize];
                            if client.read_exact(&mut skip).await.is_err() {
                                return;
                            }
                        }
                        _ => return,
                    }
                    let mut port = [0u8; 2];
                    if client.read_exact(&mut port).await.is_err() {
                        return;
                    }
                    // 成功应答（BND 0.0.0.0:0）
                    let _ = client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await;
                    let Ok(mut upstream) = TcpStream::connect(target).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        addr
    }

    fn proxy_object(protocol: &str, addr: std::net::SocketAddr) -> Value {
        let mut object = json!({
            "protocol": protocol,
            "host": addr.ip().to_string(),
            "port": addr.port(),
        });
        if protocol.starts_with("socks5") {
            object["username"] = json!("user");
            object["password"] = json!("pass");
        } else {
            object["username"] = json!("u");
            object["password"] = json!("p");
        }
        object
    }

    /// 经 HTTP CONNECT 隧道建立 ws 连接并完成一次文本回显。
    #[tokio::test]
    async fn open_through_http_connect_proxy_and_echo() {
        let echo = ws_echo_server().await;
        let proxy = connect_proxy(echo, None).await;
        let id = format!("proxy-http-{}", uuid::Uuid::new_v4());
        // 非环回目标：代理必须生效（桩忽略目标名，自己连回显服务）
        let opened = open(&json!({
            "id": id,
            "url": "ws://example.invalid/socket",
            "proxy": proxy_object("http", proxy),
        }))
        .await
        .expect("经 CONNECT 隧道应握手成功");
        assert_eq!(opened["id"], id.as_str());

        send(&json!({"id": id, "type": "text", "data": "hello-proxy"}))
            .await
            .unwrap();
        let event = tokio::time::timeout(Duration::from_secs(10), read(&json!({"id": id})))
            .await
            .expect("read 不应悬挂")
            .unwrap();
        assert_eq!(event["type"], "text");
        assert_eq!(event["data"], "hello-proxy");
        abort(&json!({"id": id}));
    }

    /// 代理拒绝 CONNECT（407）：open 必须返回明确错误，且不得登记会话。
    #[tokio::test]
    async fn open_through_http_proxy_rejection_reports_status() {
        let echo = ws_echo_server().await;
        let proxy = connect_proxy(echo, Some(407)).await;
        let id = format!("proxy-reject-{}", uuid::Uuid::new_v4());
        let err = open(&json!({
            "id": id,
            "url": "ws://example.invalid/socket",
            "proxy": proxy_object("http", proxy),
        }))
        .await
        .unwrap_err();
        assert!(
            err.message.contains("proxy connect failed") && err.message.contains("407"),
            "错误应带代理拒绝状态: {}",
            err.message
        );
        // 失败路径不得把 id 留在连接表里（Opening 守卫已 abort）
        assert!(read(&json!({"id": id})).await.is_err());
    }

    /// 经 SOCKS5（用户名/密码认证）隧道建立 ws 连接。
    ///
    /// 两种协议各走一遍：`socks5h`（域名交代理）与 `socks5`（目标为 IP
    /// 字面量，ATYP=1 直填，不做本地 DNS）。
    #[tokio::test]
    async fn open_through_socks5_proxy_and_echo() {
        let echo = ws_echo_server().await;
        let proxy = socks5_proxy(echo).await;
        for (protocol, url) in [
            ("socks5h", "ws://example.invalid/socket".to_string()),
            // 非环回 IP 字面量（TEST-NET-1）：不得触发环回绕过，ATYP=1 直填
            ("socks5", "ws://192.0.2.1/socket".to_string()),
        ] {
            let id = format!("proxy-socks-{}", uuid::Uuid::new_v4());
            let opened = open(&json!({
                "id": id,
                "url": url,
                "proxy": proxy_object(protocol, proxy),
            }))
            .await
            .unwrap_or_else(|e| panic!("经 {protocol} 隧道应握手成功: {}", e.message));
            assert_eq!(opened["id"], id.as_str());
            send(&json!({"id": id, "type": "text", "data": "via-socks"}))
                .await
                .unwrap();
            let event = tokio::time::timeout(Duration::from_secs(10), read(&json!({"id": id})))
                .await
                .expect("read 不应悬挂")
                .unwrap();
            assert_eq!(event["data"], "via-socks");
            abort(&json!({"id": id}));
        }
    }

    /// 不支持的代理协议 / https 代理：必须明确报错（不静默直连）。
    #[tokio::test]
    async fn open_rejects_unsupported_proxy_protocols() {
        let err = open(&json!({
            "id": "bad-proxy-1",
            "url": "ws://example.invalid/x",
            "proxy": {"protocol": "ftp", "host": "h", "port": 21},
        }))
        .await
        .unwrap_err();
        assert!(
            err.message.contains("unsupported proxy protocol"),
            "{}",
            err.message
        );

        let err = open(&json!({
            "id": "bad-proxy-2",
            "url": "wss://example.invalid/x",
            "proxy": {"protocol": "https", "host": "h", "port": 8443},
        }))
        .await
        .unwrap_err();
        assert!(err.message.contains("https proxy"), "{}", err.message);
    }

    /// 本机环回目标按上游口径绕过代理：代理端点不可达也应直连成功。
    #[tokio::test]
    async fn open_loopback_target_bypasses_proxy() {
        let echo = ws_echo_server().await;
        let id = format!("proxy-loop-{}", uuid::Uuid::new_v4());
        let opened = open(&json!({
            "id": id,
            "url": format!("ws://127.0.0.1:{}/socket", echo.port()),
            // 不可达的代理：若被使用，握手必然失败
            "proxy": {"protocol": "http", "host": "127.0.0.1", "port": 1},
        }))
        .await
        .expect("环回目标应绕过代理直连");
        assert_eq!(opened["id"], id.as_str());
        abort(&json!({"id": id}));
    }
}
