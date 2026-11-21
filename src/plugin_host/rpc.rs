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

#![allow(dead_code)] // 原生插件 RPC 辅助与线表（仅测试/对表用）
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
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

/// 流式调用等首帧 `head` 的超时。插件必须在 30s 内回 head，否则
/// [`RpcChannel::call_stream`] 直接失败——网关不能挂着一条不知死活的流。
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

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

/// 一次性的取消令牌（对齐 A2A beta.4 的 `CancellationToken` 语义）。
///
/// ── 为什么自带而不是用 `tokio_util::sync::CancellationToken` ──────────
/// 项目既有约定是**不为此新增依赖**（见 `log/inflight.rs` 的同名注释）；
/// `tokio` 已是直接依赖，`Notify` 足够表达「一次性置位 + 唤醒全部等待者」。
///
/// ── 为什么 `cancelled()` 要先 `enable()` 再复查 ──────────────────────
/// `Notify::notified()` 只有在**首次被 poll 时**才登记 waiter。若先查
/// `is_cancelled()`、再 `notified().await`，`cancel()` 恰好落在「查完」
/// 与「登记」之间时，`notify_waiters()` 没有 waiter 可唤醒、标志位却已置
/// true —— 等待者会永久挂起。`Notified::enable()` 显式完成登记，之后再
/// 复查一次标志位，即可堵住这条丢失唤醒的窗口。
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationInner>,
}

#[derive(Default)]
struct CancellationInner {
    cancelled: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// 触发取消（幂等，可多线程调用）。
    pub fn cancel(&self) {
        self.inner.cancelled.store(true, Ordering::SeqCst);
        self.inner.notify.notify_waiters();
    }

    /// 是否已取消（非阻塞）。
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// 等待取消；可放进 `tokio::select!`。
    pub async fn cancelled(&self) {
        loop {
            if self.is_cancelled() {
                return;
            }
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            // 先登记 waiter，再复查：消除 cancel 落在检查与登记之间的丢失唤醒。
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
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

/// 补齐 `conversation` / `chat` 的**必填线字段**。
///
/// 上游 `provider-protocol` 的 `ConversationRequest` / `ChatRequest` 直接把
/// `params` 反序列化成结构体，而 `headers` 与 `stream` **没有** serde 默认值
/// —— 缺任何一个，插件 SDK 都会回「参数无法解析」，表现为所有对话 500。
/// 网关自己的调用方可能只填 `{provider, account, body}`（插件渠道没有客户端
/// 头上下文），所以由 RPC 内核统一补默认值：
///
/// - `headers`: `{}`（空表 = 没有可透传的客户端头）
/// - `stream`: `true`（`conversation` 恒强制；`chat` 缺省时补 true）
/// - `account`: `""`（未指定账号 = 插件用自己已授权的默认账号）
/// - `session`: `""`（`conversation` 专用，少一个会话标识不影响首轮）
/// - `wire_model`: `body.model`（插件拼 URL / 选信封时要用的发送名）
/// - `reasoning`: `""`（`chat` 专用）
///
/// 调用方已经给了的值一律保留。
fn fill_wire_defaults(method: &str, params: Value) -> Value {
    let conversation = method == "conversation";
    if !conversation && method != "chat" {
        return params;
    }
    let Value::Object(mut map) = params else {
        return params;
    };
    let object_or_default = |map: &mut serde_json::Map<String, Value>, key: &str| match map.get(key)
    {
        Some(Value::Object(_)) => {}
        _ => {
            map.insert(key.to_string(), Value::Object(Default::default()));
        }
    };
    object_or_default(&mut map, "headers");
    if conversation {
        // 网关只实现流式路径：插件永远推 chat SSE 帧，非流式由网关自己聚合
        // （与上游 `forward_conversation` 恒发 `stream:true` 同口径）。
        map.insert("stream".to_string(), Value::Bool(true));
    } else {
        match map.get("stream") {
            Some(Value::Bool(_)) => {}
            _ => {
                map.insert("stream".to_string(), Value::Bool(true));
            }
        }
    }
    match map.get("account") {
        Some(Value::String(_)) => {}
        _ => {
            map.insert("account".to_string(), Value::String(String::new()));
        }
    }
    if conversation {
        match map.get("session") {
            Some(Value::String(_)) => {}
            _ => {
                map.insert("session".to_string(), Value::String(String::new()));
            }
        }
    } else {
        match map.get("reasoning") {
            Some(Value::String(_)) => {}
            _ => {
                map.insert("reasoning".to_string(), Value::String(String::new()));
            }
        }
    }
    if !matches!(map.get("wire_model"), Some(Value::String(_))) {
        let model = map
            .get("body")
            .and_then(|body| body.get("model"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        map.insert("wire_model".to_string(), Value::String(model));
    }
    Value::Object(map)
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

    /// 异步回调（脚本宿主的 egress / 协议转换需要真正 await I/O）。
    ///
    /// 默认实现委托给 [`Self::handle`]——原生插件的回调全部走同步语义，
    /// 行为零变化。脚本宿主的 `ScriptCallbacks` 覆写它实现
    /// `script.transport.*` / `script.protocol.*` 的流式阻塞语义。
    fn handle_async<'a>(
        &'a self,
        method: &'a str,
        params: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, RpcError>> + Send + 'a>>
    {
        let result = self.handle(method, params);
        Box::pin(async move { result })
    }
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

/// 流式调用的事件（`head` → `chunk`×N → `End`，出错则 `Err` 终结）。
#[derive(Clone, Debug)]
pub enum StreamEvent {
    /// 上游响应头（流的第一帧）。
    Head {
        status: u16,
        headers: BTreeMap<String, String>,
        /// 这份响应用哪套上游协议（插件自报，网关不校验）。
        upstream: Option<String>,
    },
    /// 一段上游字节（`chunk` 帧的 base64 已解码）。
    Chunk(Vec<u8>),
    /// 流正常收尾（`end` 事件帧或 `{"id":X,"result":null}` 应答帧）。
    End,
    /// 插件日志输出（测试用；生产侧由 RPC 层直接截获）。
    Log { message: String },
}

/// 一条流式调用在网关侧的句柄（[`RpcChannel::call_stream`] 的产出）。
///
/// `head` 已由 call_stream 等好（30s 超时），消费方用 [`StreamCall::next`]
/// 逐个取 `Chunk` / `End`。背压在这里闭环：每消费一个 chunk 按插件侧**同
/// 口径**计费（载荷 + [`FRAME_OVERHEAD_BYTES`]，差一个常量窗口就会缓慢
/// 泄漏），未归还量攒满 `DEFAULT_WINDOW_BYTES / 2` 就向插件发一帧
/// `credit`——客户端读得慢就不归还，插件发送窗口耗尽后自然停住。
///
/// Drop 时把流从登记表摘除（消费方中途放弃不再泄漏表项）。
pub struct StreamCall {
    /// 本次调用的 RPC id（插件按 `params.id` 指认被补充的流）。
    id: i64,
    /// 共享状态：写 credit 帧 + 摘除登记。
    shared: Arc<Shared>,
    /// 事件接收端（读循环按 id 路由进来的原始帧）。
    rx: mpsc::UnboundedReceiver<Frame>,
    /// call_stream 已等出的 head，第一次 `next()` 先交出它。
    head: Option<StreamEvent>,
    /// 已消费字节（载荷 + 帧开销，与插件侧计费口径一致）。
    consumed: usize,
    /// 已归还额度。
    credited: usize,
    /// 流已终态（end / 出错 / 截断）——此后 `next()` 一律返回 `None`。
    finished: bool,
}

impl StreamCall {
    /// 取下一个流事件。
    ///
    /// - `Some(Ok(..))` —— 一个事件（head / chunk / end）
    /// - `Some(Err(..))` —— 插件中途报错或流被截断（进程退出、通道关闭）
    /// - `None` —— 流已终态，不会再有事件
    pub async fn next(&mut self) -> Option<Result<StreamEvent, RpcError>> {
        if self.finished {
            return None;
        }
        if let Some(head) = self.head.take() {
            return Some(Ok(head));
        }
        loop {
            let frame = match self.rx.recv().await {
                Some(frame) => frame,
                None => {
                    // 读循环退出且没等到收尾帧：把截断当错误上报，不能让
                    // 消费方把不完整的流当成正常结束（与上游同处置）
                    self.mark_finished();
                    return Some(Err(RpcError::new(
                        "stream interrupted before end frame",
                        "eof",
                    )));
                }
            };
            match frame.event.as_deref() {
                Some("head") => {
                    // 首帧 head 已由 call_stream 取出缓冲；循环里再见的 head
                    // 都是插件重复发的协议违规——丢弃，多余的控制帧不该
                    // 毁掉正文（与上游消费侧同处置）
                }
                Some("chunk") => {
                    let data = frame.data.unwrap_or_default();
                    let bytes = match STANDARD.decode(data.as_bytes()) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            self.mark_finished();
                            // base64 坏了 = 后续字节边界不可信，与坏帧同等
                            // 处置：流以错误终结
                            return Some(Err(RpcError::new(
                                format!("chunk base64 decode failed: {e}"),
                                "decode",
                            )));
                        }
                    };
                    if bytes.len() > MAX_CHUNK_BYTES {
                        // 超限仅告警不中断：切分是插件侧的约定，网关不替它
                        // 执行，但要让运维看得见
                        tracing::warn!(
                            "plugin stream {} chunk {} bytes > MAX_CHUNK_BYTES ({MAX_CHUNK_BYTES})",
                            self.id,
                            bytes.len()
                        );
                    }
                    self.charge_and_credit(bytes.len()).await;
                    return Some(Ok(StreamEvent::Chunk(bytes)));
                }
                Some("end") => {
                    self.mark_finished();
                    return Some(Ok(StreamEvent::End));
                }
                // 未知事件名：忽略（向前兼容，旧网关对新事件不报错）
                Some(_) => {}
                None => {
                    // 应答形状的收尾帧（`{"id":X,"result":null}` / error）。
                    // 收尾优先认 error：先 `error` 后 `result` 的判定与
                    // `Frame::kind` 同序
                    self.mark_finished();
                    return match frame.error {
                        Some(err) => Some(Err(err)),
                        None => Some(Ok(StreamEvent::End)),
                    };
                }
            }
        }
    }

    /// 本次调用的 RPC id。
    pub fn id(&self) -> i64 {
        self.id
    }

    /// 计费 + 攒满半个窗口就归还额度。
    ///
    /// 归还量 `n` 是**实际未归还的消费量**，不是固定的半个窗口：一块
    /// chunk 跨过阈值时多出的部分一并归还，否则窗口随流缓慢泄漏（插件
    /// 最终在客户端正常读取的情况下停摆，背压形同虚设）。
    async fn charge_and_credit(&mut self, payload_len: usize) {
        self.consumed += payload_len + FRAME_OVERHEAD_BYTES;
        let uncredited = self.consumed - self.credited;
        if uncredited < DEFAULT_WINDOW_BYTES / 2 {
            return;
        }
        let frame = Frame {
            // credit 的外层 id 必须是**新分配的请求 id**，不能复用流 id：
            // 插件 SDK 对 credit 应答 `{"id":<请求id>,"result":null}`
            // （provider-sdk serve.rs 的 `host.reply(id, …)`），复用流 id 会让
            // 这份应答被读循环当成流的收尾帧投进流通道，把活流提前判死。
            // 流的归属由 `params.id` 指认（与上游 Host::credit 同形状）。
            id: self.shared.next_id.fetch_add(2, Ordering::Relaxed),
            method: Some("credit".to_string()),
            params: Some(serde_json::json!({ "id": self.id, "n": uncredited })),
            ..Default::default()
        };
        if let Err(e) = self.shared.write_frame(&frame).await {
            tracing::warn!("plugin stream {} credit write failed: {e}", self.id);
            return;
        }
        self.credited += uncredited;
    }

    /// 终态标记 + 从登记表摘除自己（读循环不再往这条通道投递）。
    fn mark_finished(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let shared = self.shared.clone();
            let id = self.id;
            handle.spawn(async move {
                shared.streams.write().await.remove(&id);
            });
        }
    }
}

impl Drop for StreamCall {
    fn drop(&mut self) {
        // 消费方中途放弃（没读到收尾就丢句柄）：摘除登记，避免表项泄漏
        self.mark_finished();
    }
}

/// 内部共享状态。
struct Shared {
    /// 出站写句柄（锁内写一行 JSON）。
    writer: Mutex<BoxWriter>,
    /// 在途请求的应答通道。
    pending: RwLock<HashMap<i64, oneshot::Sender<Frame>>>,
    /// 在途流式调用的帧通道（head/chunk/end 按调用 id 路由进来）。
    ///
    /// 通道是**无界**的：有界通道满了要么丢帧（正文损坏）要么把读循环
    /// 卡死（殃及其它在途请求）。内存有界性由 credit 窗口保证——插件
    /// 发满一个窗口就停，等网关归还额度；网关侧最多积压约一个窗口的字节。
    streams: RwLock<HashMap<i64, mpsc::UnboundedSender<Frame>>>,
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
            streams: RwLock::new(HashMap::new()),
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
        self.call_with_cancel(method, params, None).await
    }

    /// 与 [`Self::call`] 同一件事，但可被 [`CancellationToken`] 中止。
    ///
    /// 取消语义（对齐 A2A beta.4 `call_coded_with_cancel`）：令牌置位时
    /// **立刻放弃等待**、从 `pending` 摘除这条在途登记，并返回 `aborted`
    /// 错误。之所以要显式摘除：取消后调用方已经不再等这条 `oneshot`，若不
    /// 摘除，插件迟到的应答会命中一个没人接收的发送端，登记表也会随每次
    /// 登录取消累积 —— 即「取消时子进程泄漏 / 竞态」的根因。
    ///
    /// 注意：这里只中止**网关侧等待**，不向插件补发 abort 帧。登录路径的
    /// `login.cancel` 会另外通知插件终止流程；本条只保证网关不悬挂等待。
    pub async fn call_with_cancel(
        &self,
        method: &str,
        params: Value,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, RpcError> {
        let params = fill_wire_defaults(method, params);
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
        let wait = async {
            match rx.await {
                Ok(frame) => {
                    if let Some(err) = frame.error {
                        return Err(err);
                    }
                    Ok(frame.result.unwrap_or(Value::Null))
                }
                Err(_) => Err(RpcError::new("channel closed before response", "closed")),
            }
        };
        match cancel {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        // 摘除在途登记：迟到的应答由读循环按未知 id 丢弃。
                        self.shared.pending.write().await.remove(&id);
                        Err(RpcError::new("登录已取消", "aborted"))
                    }
                    result = wait => result,
                }
            }
            None => wait.await,
        }
    }

    /// 发起一次流式调用（`conversation` 形态：head → chunk×N → 收尾）。
    ///
    /// 分配奇数 id、把流登记进 [`Shared::streams`]、写出请求帧，然后等首帧
    /// `head`（[`HEAD_TIMEOUT`] 30s）。head 到手后返回 [`StreamCall`]，
    /// 消费方用 [`StreamCall::next`] 逐帧取。
    ///
    /// 插件拒绝（error 应答）、head 超时、首帧不是 head、写出失败——都在
    /// 这里直接报错，不返回半条流。
    pub async fn call_stream(&self, method: &str, params: Value) -> Result<StreamCall, RpcError> {
        let params = fill_wire_defaults(method, params);
        let id = self.shared.next_id.fetch_add(2, Ordering::Relaxed);
        let (tx, mut rx) = mpsc::unbounded_channel();
        {
            let mut streams = self.shared.streams.write().await;
            streams.insert(id, tx);
        }
        let frame = Frame {
            id,
            method: Some(method.to_string()),
            params: Some(params),
            ..Default::default()
        };
        if let Err(e) = self.shared.write_frame(&frame).await {
            self.shared.streams.write().await.remove(&id);
            return Err(e);
        }
        let first = match tokio::time::timeout(HEAD_TIMEOUT, rx.recv()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                // 读循环退出（进程没了）且没等到 head
                self.shared.streams.write().await.remove(&id);
                return Err(RpcError::new("stream closed before head frame", "eof"));
            }
            Err(_) => {
                self.shared.streams.write().await.remove(&id);
                return Err(RpcError::new("stream head frame timeout (30s)", "timeout"));
            }
        };
        match first.event.as_deref() {
            // 正常：head 到手，装进 StreamCall 让第一次 next() 先交出它
            Some("head") => Ok(StreamCall {
                id,
                shared: self.shared.clone(),
                rx,
                head: Some(StreamEvent::Head {
                    status: first.status.unwrap_or(200),
                    headers: first.headers.unwrap_or_default(),
                    upstream: first.upstream,
                }),
                consumed: 0,
                credited: 0,
                finished: false,
            }),
            // 插件拒绝（error 应答）：透传插件的错误
            None if first.error.is_some() => {
                self.shared.streams.write().await.remove(&id);
                Err(first.error.unwrap())
            }
            // 其余（chunk 先来 / end 先来 / result:null 收尾）都是协议违规：
            // 宁可失败，也不能让消费方拿一条没有 head 的流
            other => {
                self.shared.streams.write().await.remove(&id);
                Err(RpcError::new(
                    format!("stream did not start with head frame (got {other:?})"),
                    "protocol",
                ))
            }
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

    /// 关闭通道：abort 读循环任务，并把全部在途请求/流**明确失败**。
    ///
    /// `abort` 会直接丢弃读循环 future，末尾的 EOF 清理（失败 pending /
    /// 摘流）**不会执行**；只 abort 的话，等应答的 oneshot 与流通道的发送端
    /// 还活在 `Shared` 里，接收端永远等不到信号——热切换排空超时、网关关停
    /// 走到这条路径时，在途 `call` / `StreamCall::next` 会悬挂到进程退出。
    /// 这里补一次 fail_all，让它们以 `closed` / `eof` 错误及时终结。
    pub fn shutdown(&self) {
        if let Some(h) = &self.shutdown {
            h.abort();
        }
        // 只有运行时上下文里才有人在等（`RpcChannel::new` 本身就要求
        // runtime）；无 runtime 的构造路径没有等待者，跳过即可。
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let shared = self.shared.clone();
            handle.spawn(async move {
                fail_all(&shared, "plugin channel shut down", "closed").await;
            });
        }
    }

    /// 当前在途请求数（诊断用）。
    pub async fn pending_count(&self) -> usize {
        self.shared.pending.read().await.len()
    }

    /// 当前活跃流数（热切换前的在途排空判定用）。
    pub async fn active_streams(&self) -> usize {
        self.shared.streams.read().await.len()
    }
}

impl Shared {
    /// 写一帧到插件 stdin。
    ///
    /// ── 为什么在这里挡超限而不是让插件去挡 ──────────────────────
    /// 插件 SDK 的 `read_frame` 遇到超过 [`MAX_LINE_BYTES`] 的行会判
    /// `InvalidData` **并断开读循环**（整个宿主随之失效）。网关如果在写端
    /// 不拦，一条超限的 `conversation` 就能把插件进程打死，后续所有请求
    /// 全部失败。这里提前给出可分类的 `plugin_request_too_large`（与 Wasm
    /// 宿主同码），编排层按 413 透传，不换号重试。
    async fn write_frame(&self, frame: &Frame) -> Result<(), RpcError> {
        let line = encode_frame(frame)?;
        if let Some(error) = oversize_error(line.len()) {
            return Err(error);
        }
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

/// 一帧（含行尾）超过 [`MAX_LINE_BYTES`] 时的错误；未超限返回 `None`。
///
/// 错误码用 `plugin_request_too_large`：与 Wasm 宿主、`bridge/plugin.rs`
/// 的 413 映射同码（对齐 A2A beta.3）。
fn oversize_error(line_len: usize) -> Option<RpcError> {
    if line_len < MAX_LINE_BYTES {
        return None;
    }
    Some(RpcError::new(
        format!(
            "本次请求约 {:.1} MiB，超过插件单帧上限 {:.1} MiB",
            line_len as f64 / (1024.0 * 1024.0),
            MAX_LINE_BYTES as f64 / (1024.0 * 1024.0),
        ),
        "plugin_request_too_large",
    ))
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
            if buf.len() > MAX_LINE_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "rpc line exceeds MAX_LINE_BYTES",
                ));
            }
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
                // 插件→网关请求（回调）。**每个回调独立 spawn**：脚本宿主的
                // `script.transport.read` 会阻塞到上游下一块数据（可能是
                // 几十秒），就地 await 会把整条读循环卡住——同一宿主的其它
                // provider 流（甚至网关自己发出的请求应答）全部排队。回调
                // 应答按 id 关联，乱序安全（插件 SDK 也是按 id 匹配）。
                let method = frame.method.unwrap_or_default();
                let params = frame.params.unwrap_or(Value::Null);
                let id = frame.id;
                let callbacks = shared.callbacks.clone();
                let shared = shared.clone();
                tokio::spawn(async move {
                    let result = callbacks.handle_async(&method, params).await;
                    let response = match result {
                        Ok(value) => Frame {
                            id,
                            result: Some(value),
                            ..Default::default()
                        },
                        Err(err) => Frame {
                            id,
                            error: Some(err),
                            ..Default::default()
                        },
                    };
                    if let Err(e) = shared.write_frame(&response).await {
                        tracing::warn!("plugin RPC callback response write failed: {e}");
                    }
                });
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
                } else {
                    // 不在 pending：可能是流式调用的收尾帧（`{"id":X,
                    // "result":null}` / error）——按 id 投进流通道保持帧序
                    //（与上游 settle 先查流式表同构）；也不是流（如 credit
                    // 应答）就丢弃——网关对 credit 不等应答
                    let tx = shared.streams.read().await.get(&id).cloned();
                    if let Some(tx) = tx {
                        let _ = tx.send(frame);
                    } else {
                        tracing::debug!("plugin RPC unmatched response for id={id}");
                    }
                }
            }
            FrameKind::Event => {
                // 流式帧：按 id 路由到对应流
                let id = frame.id;
                let tx = shared.streams.read().await.get(&id).cloned();
                if let Some(tx) = tx {
                    let _ = tx.send(frame);
                } else if frame.event.as_deref() == Some("log") {
                    // id=0 的日志通知（无主 → 直接记日志）
                    tracing::info!("[plugin] {}", frame.message.as_deref().unwrap_or(""));
                } else {
                    tracing::debug!(
                        "[plugin event] unclaimed id={id} event={}",
                        frame.event.as_deref().unwrap_or("")
                    );
                }
            }
            FrameKind::Unknown => {
                tracing::warn!("plugin RPC unknown frame kind: {frame:?}");
            }
        }
    }
    // EOF：所有在途请求失败；所有活流摘除——接收端收到 None 后由
    // `StreamCall::next` 把截断上报为错误（不当正常结束）
    fail_all(&shared, "plugin process exited", "eof").await;
}

/// 把全部在途请求/流明确失败（幂等；两个入口共用）。
///
/// - 读循环 EOF：插件进程退出 → 错误码 `eof`；
/// - [`RpcChannel::shutdown`]：网关主动关通道（abort 跳过了 EOF 清理）→ `closed`。
///
/// pending 逐个投错误帧（`call` 收到即返回 Err）；streams 直接清空
///（发送端 drop → 接收端 `recv()` 得到 None → `StreamCall::next` 按截断报错）。
async fn fail_all(shared: &Shared, message: &str, code: &str) {
    let failed: Vec<oneshot::Sender<Frame>> = shared
        .pending
        .write()
        .await
        .drain()
        .map(|(_, tx)| tx)
        .collect();
    let pending_count = failed.len();
    for tx in failed {
        let _ = tx.send(Frame {
            error: Some(RpcError::new(message, code)),
            ..Default::default()
        });
    }
    let dropped = shared.streams.write().await.drain().count();
    if pending_count > 0 {
        tracing::warn!("plugin RPC channel closed, {pending_count} pending requests failed");
    }
    if dropped > 0 {
        tracing::warn!("plugin RPC channel closed, {dropped} active streams dropped");
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

    /// 取消令牌置位时，在途的 `call_with_cancel` 必须立刻返回 `aborted`
    /// 并从 `pending` 摘除登记（否则插件迟到的应答会命中无人接收的发送端，
    /// 登记表随每次登录取消累积）。
    #[tokio::test]
    async fn call_with_cancel_aborts_in_flight_and_clears_pending() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 插件端只把请求读走、**永不回应答**——模拟卡住的登录轮询。
        let _plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            assert_eq!(frame.method.as_deref(), Some("login.poll"));
            // 故意不回应答；把写端留在作用域里直到任务被丢弃（保持通道存活，
            // 否则 EOF 会先让 pending 以 "closed" 结束，测不到取消路径）。
            let _keep_write = plugin_write;
            std::future::pending::<()>().await;
        });

        let token = CancellationToken::new();
        let canceller = token.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            canceller.cancel();
        });

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            client.call_with_cancel("login.poll", Value::Null, Some(&token)),
        )
        .await
        .expect("取消必须在超时前生效")
        .expect_err("取消后必须返回错误");
        assert_eq!(result.code.as_deref(), Some("aborted"));
        assert_eq!(result.message, "登录已取消");
        // 在途登记已被摘除。
        assert_eq!(client.pending_count().await, 0);
        client.shutdown();
    }

    /// 未取消时，`call_with_cancel(Some(token))` 行为与普通 `call` 一致。
    #[tokio::test]
    async fn call_with_cancel_returns_response_when_not_cancelled() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let plugin_task = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            let response = Frame {
                id: frame.id,
                result: Some(serde_json::json!({ "state": "pending" })),
                ..Default::default()
            };
            let mut w = plugin_write;
            w.write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            w.write_all(b"\n").await.unwrap();
            w.flush().await.unwrap();
        });

        let token = CancellationToken::new();
        let value = client
            .call_with_cancel("login.poll", Value::Null, Some(&token))
            .await
            .expect("未取消应拿到应答");
        assert_eq!(value["state"], "pending");
        plugin_task.await.unwrap();
        assert_eq!(client.pending_count().await, 0);
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

    // ── 线字段默认值（Community SDK 的必填 ConversationRequest）────────

    /// 只带 `{provider, account, body}` 的 conversation 参数必须被补齐
    /// `headers` / `stream` / `session` / `wire_model`，否则插件 SDK 的
    /// `serde_json::from_value::<ConversationRequest>` 会因缺字段失败。
    #[test]
    fn wire_defaults_fill_conversation_required_fields() {
        let params = serde_json::json!({
            "provider": "kiro",
            "account": "",
            "body": { "model": "wire-model-x" }
        });
        let filled = fill_wire_defaults("conversation", params);
        assert!(filled["headers"].is_object(), "headers 必填：{filled}");
        assert_eq!(filled["stream"], true, "stream 必填：{filled}");
        assert_eq!(filled["session"], "", "session 应有默认串：{filled}");
        assert_eq!(filled["wire_model"], "wire-model-x");
        // 插件 SDK 的严格反序列化：这一份必须能解出全部必填字段。
        assert_eq!(filled["provider"], "kiro");
        assert_eq!(filled["account"], "");
        assert!(filled["body"].is_object());
    }

    #[test]
    fn wire_defaults_preserve_caller_values_and_skip_other_methods() {
        let params = serde_json::json!({
            "provider": "kiro",
            "account": "acc-1",
            "body": { "model": "m" },
            "headers": { "x-session-id": "s1" },
            "stream": false,
            "session": "s1",
            "wire_model": "m2"
        });
        let filled = fill_wire_defaults("conversation", params.clone());
        assert_eq!(
            filled["stream"], true,
            "conversation 恒为流式（上游同口径）"
        );
        assert_eq!(filled["headers"]["x-session-id"], "s1");
        assert_eq!(filled["session"], "s1");
        assert_eq!(filled["wire_model"], "m2");
        assert_eq!(filled["account"], "acc-1", "显式账号应保留");

        // 其它方法原样透传（不偷偷改动 admin/login 参数）。
        let login = serde_json::json!({ "provider": "kiro", "flow": "f1" });
        assert_eq!(fill_wire_defaults("login.poll", login.clone()), login);
    }

    #[test]
    fn wire_defaults_fill_chat_reasoning() {
        let params = serde_json::json!({ "provider": "kiro", "body": { "model": "m" } });
        let filled = fill_wire_defaults("chat", params);
        assert_eq!(filled["stream"], true);
        assert_eq!(filled["reasoning"], "");
        assert_eq!(filled["wire_model"], "m");
        assert!(filled["headers"].is_object());
    }

    #[test]
    fn oversize_error_matches_plugin_request_too_large_code() {
        assert!(
            oversize_error(MAX_LINE_BYTES - 1).is_none(),
            "含行尾恰好到上限不算超"
        );
        let error = oversize_error(MAX_LINE_BYTES).expect("再加行尾就应拒绝");
        assert_eq!(error.code.as_deref(), Some("plugin_request_too_large"));
        assert!(error.message.contains("MiB"), "文案应带尺寸: {error}");
    }

    // ── 流式调用（call_stream / StreamCall）──────────────────────────
    /// 组一帧并写出（mock 插件侧用）。
    async fn write_frame_line<W: AsyncWrite + Unpin>(w: &mut W, frame: &Frame) {
        w.write_all(encode_frame(frame).unwrap().as_bytes())
            .await
            .unwrap();
        w.write_all(b"\n").await.unwrap();
        w.flush().await.unwrap();
    }

    #[tokio::test]
    async fn stream_head_chunk_end_via_duplex() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            assert_eq!(req.method.as_deref(), Some("conversation"));
            let id = req.id;
            let head = Frame {
                id,
                event: Some("head".into()),
                status: Some(200),
                headers: Some(BTreeMap::from([(
                    "content-type".to_string(),
                    "text/event-stream".to_string(),
                )])),
                ..Default::default()
            };
            let chunk1 = Frame {
                id,
                event: Some("chunk".into()),
                data: Some(STANDARD.encode("hello ")),
                ..Default::default()
            };
            let chunk2 = Frame {
                id,
                event: Some("chunk".into()),
                data: Some(STANDARD.encode("world")),
                ..Default::default()
            };
            let end = Frame {
                id,
                event: Some("end".into()),
                ..Default::default()
            };
            for f in [&head, &chunk1, &chunk2, &end] {
                write_frame_line(&mut plugin_write, f).await;
            }
        });

        let mut call = client
            .call_stream("conversation", Value::Null)
            .await
            .unwrap();
        match call.next().await.unwrap().unwrap() {
            StreamEvent::Head {
                status,
                headers,
                upstream,
            } => {
                assert_eq!(status, 200);
                assert_eq!(
                    headers.get("content-type").map(String::as_str),
                    Some("text/event-stream")
                );
                assert_eq!(upstream, None);
            }
            other => panic!("expected head, got {other:?}"),
        }
        match call.next().await.unwrap().unwrap() {
            StreamEvent::Chunk(bytes) => assert_eq!(bytes.as_slice(), b"hello ".as_slice()),
            other => panic!("expected chunk, got {other:?}"),
        }
        match call.next().await.unwrap().unwrap() {
            StreamEvent::Chunk(bytes) => assert_eq!(bytes.as_slice(), b"world".as_slice()),
            other => panic!("expected chunk, got {other:?}"),
        }
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::End
        ));
        // 收尾之后 next() 恒为 None
        assert!(call.next().await.is_none(), "终态后不应再有事件");
        drop(call);
        mock.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn stream_ends_with_result_null_response() {
        // 上游形态：插件不发 `event:"end"`，而是用 `{"id":X,"result":null}`
        // 收尾——同样要把流判成正常结束
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            let id = req.id;
            let head = Frame {
                id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            let chunk = Frame {
                id,
                event: Some("chunk".into()),
                data: Some(STANDARD.encode("payload")),
                ..Default::default()
            };
            let close = Frame {
                id,
                result: Some(Value::Null),
                ..Default::default()
            };
            for f in [&head, &chunk, &close] {
                write_frame_line(&mut plugin_write, f).await;
            }
        });

        let mut call = client
            .call_stream("conversation", Value::Null)
            .await
            .unwrap();
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Chunk(_)
        ));
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::End
        ));
        drop(call);
        mock.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn stream_plugin_error_response_propagates() {
        // 插件直接拒了这次调用（error 应答）：call_stream 报错，不返回半条流
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            let close = Frame {
                id: req.id,
                error: Some(RpcError::new("no such model", "model")),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &close).await;
        });

        let err = client
            .call_stream("conversation", Value::Null)
            .await
            .err()
            .expect("插件 error 应答时 call_stream 应失败");
        assert_eq!(err.message, "no such model");
        assert_eq!(err.code.as_deref(), Some("model"));
        mock.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn stream_interrupted_on_eof() {
        // 插件发完 head 就退出：流被截断，next() 必须报错而不是静默结束
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            let head = Frame {
                id: req.id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            let mut w = plugin_write;
            write_frame_line(&mut w, &head).await;
            // 直接退出（两个端口都丢）→ 网关读循环 EOF
            drop(w);
            drop(reader);
        });

        let mut call = client
            .call_stream("conversation", Value::Null)
            .await
            .unwrap();
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        let err = call.next().await.unwrap().unwrap_err();
        assert_eq!(err.code.as_deref(), Some("eof"), "截断应报 eof: {err:?}");
        drop(call);
        client.shutdown();
    }

    #[tokio::test]
    async fn stream_unknown_event_frames_are_ignored() {
        // 未知事件名（向前兼容）：不影响正文流
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            let id = req.id;
            let head = Frame {
                id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            let progress = Frame {
                id,
                event: Some("progress".into()),
                message: Some("42%".into()),
                ..Default::default()
            };
            let chunk = Frame {
                id,
                event: Some("chunk".into()),
                data: Some(STANDARD.encode("data")),
                ..Default::default()
            };
            let end = Frame {
                id,
                event: Some("end".into()),
                ..Default::default()
            };
            for f in [&head, &progress, &chunk, &end] {
                write_frame_line(&mut plugin_write, f).await;
            }
        });

        let mut call = client
            .call_stream("conversation", Value::Null)
            .await
            .unwrap();
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        match call.next().await.unwrap().unwrap() {
            StreamEvent::Chunk(bytes) => assert_eq!(bytes.as_slice(), b"data".as_slice()),
            other => panic!("expected chunk after unknown event, got {other:?}"),
        }
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::End
        ));
        drop(call);
        mock.await.unwrap();
        client.shutdown();
    }

    /// 网关只发 `{provider, account, body}` 时，写出的 conversation 请求必须
    /// 已带齐 SDK 的必填字段（headers/stream/session/wire_model）。
    #[tokio::test]
    async fn conversation_call_stream_fills_required_params() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            assert_eq!(req.method.as_deref(), Some("conversation"));
            let params = req.params.as_ref().expect("params 必须有");
            assert!(params["headers"].is_object(), "headers 缺失: {params}");
            assert_eq!(params["stream"], true, "stream 缺失: {params}");
            assert_eq!(params["session"], "");
            assert_eq!(params["wire_model"], "wire-x");
            let head = Frame {
                id: req.id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &head).await;
            let end = Frame {
                id: req.id,
                event: Some("end".into()),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &end).await;
        });

        let mut call = client
            .call_stream(
                "conversation",
                serde_json::json!({
                    "provider": "kiro",
                    "account": "",
                    "body": { "model": "wire-x", "messages": [] }
                }),
            )
            .await
            .expect("call_stream 应成功");
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::End
        ));
        drop(call);
        mock.await.unwrap();
        client.shutdown();
    }

    #[tokio::test]
    async fn stream_credit_frames_after_window() {
        // 背压：消费超过半个窗口（DEFAULT_WINDOW_BYTES/2 = 256 KiB）就该
        // 收到 credit 帧；计费口径 = 载荷 + FRAME_OVERHEAD_BYTES（与插件侧
        // 逐字一致）。10 × 64 KiB：第 4、8 块各触发一次归还（270 336 字节/次）。
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        const CHUNK_PAYLOAD: usize = 64 << 10;
        const CHUNKS: usize = 10;

        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            assert_eq!(req.method.as_deref(), Some("conversation"));
            let id = req.id;
            let head = Frame {
                id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &head).await;
            let data = STANDARD.encode(vec![b'x'; CHUNK_PAYLOAD]);
            for _ in 0..CHUNKS {
                let chunk = Frame {
                    id,
                    event: Some("chunk".into()),
                    data: Some(data.clone()),
                    ..Default::default()
                };
                write_frame_line(&mut plugin_write, &chunk).await;
            }
            let end = Frame {
                id,
                event: Some("end".into()),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &end).await;
            // 读 credit 直到 EOF（主任务收完流、关掉通道后这里自然退出）
            let mut credits = Vec::new();
            loop {
                let mut buf = Vec::new();
                match reader.read_until(b'\n', &mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let frame: Frame =
                    decode_frame(String::from_utf8_lossy(&buf).trim()).expect("decode");
                if frame.method.as_deref() == Some("credit") {
                    credits.push(frame);
                }
            }
            (id, credits)
        });

        let mut call = client
            .call_stream("conversation", Value::Null)
            .await
            .unwrap();
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        let mut total = 0usize;
        loop {
            match call.next().await.unwrap().unwrap() {
                StreamEvent::Chunk(bytes) => total += bytes.len(),
                StreamEvent::End => break,
                other => panic!("unexpected event {other:?}"),
            }
        }
        assert_eq!(total, CHUNK_PAYLOAD * CHUNKS);
        drop(call);
        client.shutdown();
        drop(client);

        let (id, credits) = mock.await.unwrap();
        let charge = CHUNK_PAYLOAD + FRAME_OVERHEAD_BYTES;
        let expected = (charge * CHUNKS) / (DEFAULT_WINDOW_BYTES / 2);
        assert_eq!(credits.len(), expected, "credit 帧数应与窗口计费一致");
        for credit in &credits {
            assert_eq!(credit.method.as_deref(), Some("credit"));
            let params = credit.params.as_ref().unwrap();
            assert_eq!(params["id"], id, "params.id 应指认被补充的流");
            let n = params["n"].as_u64().unwrap() as usize;
            assert!(n >= DEFAULT_WINDOW_BYTES / 2, "每次归还应攒满半个窗口: {n}");
            assert!(n <= charge * CHUNKS, "归还量不应超过总消费");
        }
    }

    // ── shutdown：abort 读循环后，在途请求/流必须明确失败（不能悬挂）────

    /// `call()` 等在应答上时调 shutdown：oneshot 发送端还躺在 `pending` 表
    /// 里，只 abort 读循环的话接收端永远等不到信号。修复后必须立刻以
    /// `closed` 错误终结。
    #[tokio::test]
    async fn shutdown_fails_pending_call_instead_of_hanging() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));
        // 插件侧：读走请求但不回帧（进程活着、卡住）
        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let _ = read_one_frame(&mut reader).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(plugin_write);
        });
        let call = client.call("ping", Value::Null);
        let shutdown = async {
            // 让 call 先登记 pending 并进入等待
            tokio::time::sleep(Duration::from_millis(50)).await;
            client.shutdown();
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(call, shutdown)
        })
        .await
        .expect("shutdown 后在途 call 必须失败，不能悬挂");
        let err = result.expect_err("在途 call 应以错误终结");
        assert_eq!(err.code.as_deref(), Some("closed"));
        mock.abort();
    }

    /// 已拿到 head、正在消费的流：shutdown 后 `next()` 必须以截断错误终结
    ///（流通道发送端被摘除 → recv None → eof），而不是永远等下去。
    #[tokio::test]
    async fn shutdown_interrupts_inflight_stream_instead_of_hanging() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let client = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));
        let mock = tokio::spawn(async move {
            let mut reader = BufReader::new(plugin_read);
            let req = read_one_frame(&mut reader).await;
            let head = Frame {
                id: req.id,
                event: Some("head".into()),
                status: Some(200),
                ..Default::default()
            };
            write_frame_line(&mut plugin_write, &head).await;
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let mut call = tokio::time::timeout(
            Duration::from_secs(2),
            client.call_stream("conversation", Value::Null),
        )
        .await
        .expect("head 应已到达")
        .expect("call_stream 应成功");
        assert!(matches!(
            call.next().await.unwrap().unwrap(),
            StreamEvent::Head { .. }
        ));
        client.shutdown();
        let next = tokio::time::timeout(Duration::from_secs(2), call.next())
            .await
            .expect("shutdown 后 next 必须及时终结，不能悬挂")
            .expect("应有一个终态事件");
        let err = next.expect_err("截断应以错误上报");
        assert_eq!(err.code.as_deref(), Some("eof"));
        mock.abort();
    }
}
