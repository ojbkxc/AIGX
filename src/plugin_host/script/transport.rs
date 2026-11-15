//! 脚本宿主的 HTTP egress 代理 —— `script.transport.*` 回调的网关侧实现。
//!
//! JS 宿主（`host/provider.mjs` 的默认 conversation 管线）经这三个回调发
//! 上游 HTTP：出口统一走网关（复用全局 reqwest client 的代理/连接池/超时
//! 配置），宿主进程自己不开直连。**WS egress 推迟**：`script.websocket.*`
//! 本期不实现，会话面只有 HTTP。
//!
//! ## 回调契约（与 host/provider.mjs 逐字对齐）
//!
//! - `script.transport.request` `{method,url,headers,body_b64?,stream?}` →
//!   `{status,headers,session?}`：`stream=true` 不读 body，响应挂会话表返回
//!   `session`（uuid）；否则读全量 body 一次返回（`chunk_b64`，session 缺省）
//! - `script.transport.read` `{session}` → `{chunk_b64?,done}`：阻塞到下一块
//!   或 EOF；session 缺失/未知 → 错误码 `not_found`。**`done` 帧不带载荷**
//!   ——EOF 后剩余尾巴按普通 chunk 帧先吐完（分片规则同正文），最后一个
//!   空块才带 `done:true`，宿主侧「见 done 即终结」的判定不用让步
//! - `script.transport.close` `{session}` → `{}`：幂等（不存在也 `{}`）
//!
//! ## 有界性（第三方内容经网关出网，三条都不可省）
//!
//! - **体上限 120MB**：请求体与响应**累计**计数（会话创建即计入请求体），
//!   超限断会话并报错——宿主侧 chunk 每块 ≤64KiB 且 wire 层有 128MiB 行
//!   上限，网关侧再把源头封死
//! - **分片 1KB–256KB**：read 每块 ≤256KiB（reqwest 的 chunk 是网络分片
//!   大小，不可控，必须本地重切）；网络给的块 <1KiB 时先攒（攒到 ≥1KiB
//!   或 EOF 才吐）——避免宿主为几字节发一整帧（chunk 帧计费含 2048 帧
//!   开销，碎块会让 credit 窗口被帧开销吃光）
//! - **空闲 30min 回收**：后台 reaper（60s tick）清 last_read 超 30min 的
//!   会话；read 自身等块也有 30min 超时（超时=会话结束并回收，慢上游不给
//!   宿主留僵尸会话）。reaper 只 try_lock：锁被 read 持有=会话活跃，跳过
//!
//! ## 并发
//!
//! 会话表 `RwLock<HashMap>` 索引 + 每会话独立 `Mutex`（reqwest 0.12 的
//! `Response::chunk()` 要 `&mut self`）——**read 不阻塞其它会话**，锁的
//! 粒度是单会话的单次 chunk 读取。表锁与 inner 锁**从不同时持有**（各
//! 路径都是先取快照/再锁内层），无锁序死锁面。头键 reqwest 全程规范化
//! 小写，与宿主侧 wire 帧的小写头约定一致。

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::plugin_host::rpc::RpcError;

/// 请求体 + 响应的累计字节上限（120MB）。
const MAX_TOTAL_BYTES: usize = 120 * 1024 * 1024;

/// read 单块下限：网络块小于此值先攒（攒到 ≥ 此值或 EOF 才吐）。
const MIN_SLICE_BYTES: usize = 1024;

/// read 单块上限。
const MAX_SLICE_BYTES: usize = 256 * 1024;

/// 会话空闲回收阈值（30min）。
const IDLE_SECS: i64 = 30 * 60;

/// read 等下一块的超时（= 空闲阈值：超时按会话结束处置）。
const READ_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// reaper 巡检周期（60s）。
const REAPER_TICK: Duration = Duration::from_secs(60);

/// 一个流式响应会话。
struct Session {
    /// reqwest 的 `chunk()` 要 `&mut`——整把锁包住响应与读取状态。
    inner: Mutex<SessionInner>,
}

struct SessionInner {
    /// 未读完的上游响应（drop 即断连）。
    response: reqwest::Response,
    /// 攒块缓冲（<1KiB 的网络块在这里等齐）。
    buf: Vec<u8>,
    /// 请求体 + 响应累计字节（120MB 上限口径）。
    total_bytes: usize,
    /// EOF 已到：此后 read 进入 flush 模式（吐尾巴直到空 → done）。
    done: bool,
    /// 最后一次读到数据的时刻（unix 秒，reaper 的回收判据）。
    last_read: i64,
}

/// 脚本宿主的 HTTP egress 代理。
pub struct Transport {
    /// 全局共享 client（main.rs 注入：代理/超时/连接池随网关主配置）。
    http: reqwest::Client,
    sessions: RwLock<HashMap<String, Arc<Session>>>,
}

impl Transport {
    /// 建代理并启动 reaper。
    ///
    /// reaper 持 `Weak`：`Transport` 被弃用后巡检任务自动退出，不构成
    /// Arc 循环引用；每个实例一个 reaper（本类型按单例使用）。
    ///
    /// **无 runtime 时不 panic**：`main.rs` 的构造点在 tokio 运行时内，
    /// 但单测/工具路径可能不在——此时跳过 reaper（会话靠 read 自身 30min
    /// 超时回收，语义不变）。
    pub fn new(http: reqwest::Client) -> Arc<Self> {
        let this = Arc::new(Self {
            http,
            sessions: RwLock::new(HashMap::new()),
        });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let weak = Arc::downgrade(&this);
            handle.spawn(async move {
                let mut tick = tokio::time::interval(REAPER_TICK);
                // interval 第一次 tick 立即返回，跳过（刚建的会话不值得巡检）
                tick.tick().await;
                loop {
                    tick.tick().await;
                    let Some(transport) = weak.upgrade() else {
                        return; // Transport 已弃用：巡检收工
                    };
                    transport.reap_idle().await;
                }
            });
        }
        this
    }

    /// `script.transport.request`：发出站请求。
    ///
    /// `stream=true` 不读 body（响应挂会话表，返回 session）；否则读全量
    /// body（≤120MB）以 `chunk_b64` 一次返回。参数校验失败一律 `bad_request`，
    /// 网络失败 `transport`——两者宿主侧都按 pre-head 错误通道处理。
    pub async fn request(&self, params: &Value) -> Result<Value, RpcError> {
        let obj = params
            .as_object()
            .ok_or_else(|| bad_request("params must be an object"))?;

        let url = obj
            .get("url")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| bad_request("url is required"))?;
        let parsed =
            reqwest::Url::parse(url).map_err(|e| bad_request(format!("invalid url: {e}")))?;
        // 只放行 http/https：脚本内容不可信，file/ftp/data 一律拒绝
        if parsed.scheme() != "http" && parsed.scheme() != "https" {
            return Err(bad_request(format!(
                "url scheme must be http/https, got '{}'",
                parsed.scheme()
            )));
        }

        let method = obj
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("GET")
            .to_uppercase();
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|e| bad_request(format!("invalid method: {e}")))?;

        // 请求体先解码计数：120MB 口径含请求体
        let body: Option<Vec<u8>> = match obj.get("body_b64").and_then(Value::as_str) {
            None => None,
            Some("") => None,
            Some(s) => {
                let bytes = STANDARD
                    .decode(s)
                    .map_err(|e| bad_request(format!("body_b64 is not valid base64: {e}")))?;
                if bytes.len() > MAX_TOTAL_BYTES {
                    return Err(bad_request("request body exceeds 120MB"));
                }
                Some(bytes)
            }
        };

        let mut req = self.http.request(method, parsed);
        if let Some(map) = obj.get("headers").and_then(Value::as_object) {
            for (k, v) in map {
                // 头值只收 string/number：宿主侧已小写化，这里再防一次坏值
                let val = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err(bad_request(format!("header '{k}' value must be a string"))),
                };
                let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| bad_request(format!("invalid header name '{k}': {e}")))?;
                let value = reqwest::header::HeaderValue::from_str(&val)
                    .map_err(|e| bad_request(format!("invalid header value for '{k}': {e}")))?;
                req = req.header(name, value);
            }
        }
        if let Some(b) = &body {
            req = req.body(b.clone());
        }

        let resp = req
            .send()
            .await
            .map_err(|e| rpc_error("transport", format!("upstream request failed: {e}")))?;

        let status = resp.status().as_u16();
        let headers = response_headers(&resp);

        if !obj.get("stream").and_then(Value::as_bool).unwrap_or(false) {
            // 非流式：全量读 body（累计计数 ≤120MB），一次回
            let mut resp = resp;
            let mut total = body.as_ref().map_or(0, |b| b.len());
            let mut buf: Vec<u8> = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| rpc_error("transport", format!("upstream read failed: {e}")))?
            {
                total += chunk.len();
                if total > MAX_TOTAL_BYTES {
                    return Err(rpc_error("transport", "response exceeds 120MB"));
                }
                buf.extend_from_slice(&chunk);
            }
            return Ok(json!({
                "status": status,
                "headers": headers,
                "chunk_b64": STANDARD.encode(&buf),
            }));
        }

        // 流式：content-length 可预判的超大响应直接拒，不占会话表
        if let Some(len) = resp.content_length() {
            let with_body = body.as_ref().map_or(0, |b| b.len());
            if with_body.saturating_add(len as usize) > MAX_TOTAL_BYTES {
                return Err(rpc_error(
                    "transport",
                    "response exceeds 120MB (content-length)",
                ));
            }
        }

        let id = Uuid::new_v4().to_string();
        let session = Arc::new(Session {
            inner: Mutex::new(SessionInner {
                response: resp,
                buf: Vec::new(),
                // 请求体计入累计口径：会话创建即带上
                total_bytes: body.as_ref().map_or(0, |b| b.len()),
                done: false,
                last_read: now_unix(),
            }),
        });
        self.sessions.write().await.insert(id.clone(), session);
        Ok(json!({ "status": status, "headers": headers, "session": id }))
    }

    /// `script.transport.read`：取下一块（阻塞到有块 / EOF / 超时）。
    ///
    /// 状态机：正常 → 攒缓冲够 1KiB 就吐（不读网络）→ 网络块攒进缓冲 →
    /// EOF 进 flush 模式（尾巴按普通块吐完，最后一帧才 `done:true`）。
    /// 网络错误 / 30min 无块 / 累计超限 → 会话终结并从表摘除。
    pub async fn read(&self, session: &str) -> Result<Value, RpcError> {
        let sess = {
            let sessions = self.sessions.read().await;
            sessions.get(session).cloned()
        }
        .ok_or_else(|| rpc_error("not_found", format!("unknown session '{session}'")))?;

        let mut inner = sess.inner.lock().await;

        // EOF 后：flush 模式——尾巴按普通块吐完（分片规则同正文），空了才正式终结
        if inner.done {
            if let Some(tail) = next_slice(&mut inner.buf, true) {
                return Ok(chunk_frame(&tail));
            }
            drop(inner);
            self.sessions.write().await.remove(session);
            return Ok(json!({ "done": true }));
        }

        // 攒缓冲已够一块（上次 read 攒下的）：直接吐，不读网络
        if let Some(slice) = next_slice(&mut inner.buf, false) {
            return Ok(chunk_frame(&slice));
        }

        loop {
            match tokio::time::timeout(READ_TIMEOUT, inner.response.chunk()).await {
                // 30min 无块：会话终结并回收（慢上游不留僵尸会话）
                Err(_) => {
                    inner.done = true;
                    drop(inner);
                    self.sessions.write().await.remove(session);
                    return Err(rpc_error(
                        "timeout",
                        "session read timeout (30min without data)",
                    ));
                }
                Ok(Err(e)) => {
                    inner.done = true;
                    drop(inner);
                    self.sessions.write().await.remove(session);
                    return Err(rpc_error("transport", format!("upstream read failed: {e}")));
                }
                Ok(Ok(None)) => {
                    // EOF：进 flush 模式；有尾巴先吐普通块，空则当场终结
                    inner.done = true;
                    if let Some(tail) = next_slice(&mut inner.buf, true) {
                        return Ok(chunk_frame(&tail));
                    }
                    drop(inner);
                    self.sessions.write().await.remove(session);
                    return Ok(json!({ "done": true }));
                }
                Ok(Ok(Some(bytes))) => {
                    inner.total_bytes += bytes.len();
                    if inner.total_bytes > MAX_TOTAL_BYTES {
                        inner.done = true;
                        drop(inner);
                        self.sessions.write().await.remove(session);
                        return Err(rpc_error("transport", "response exceeds 120MB"));
                    }
                    inner.buf.extend_from_slice(&bytes);
                    inner.last_read = now_unix();
                    if let Some(slice) = next_slice(&mut inner.buf, false) {
                        return Ok(chunk_frame(&slice));
                    }
                    // 不足 1KiB：继续攒
                }
            }
        }
    }

    /// `script.transport.close`：关会话（幂等）。
    ///
    /// 从表摘除 + drop response 断上游连接；进行中的 read 手里持有 Arc
    /// 会自然跑到 EOF/错误，宿主下次 read 拿 `not_found`。
    pub async fn close(&self, session: &str) -> Result<Value, RpcError> {
        self.sessions.write().await.remove(session);
        Ok(json!({}))
    }

    /// 清掉空闲超阈值的会话（reaper 调用）。
    ///
    /// 只 `try_lock`：锁被 read 持有 = 会话正在等块（活跃），跳过——
    /// reaper 永不误杀进行中的读取。
    async fn reap_idle(&self) {
        let candidates: Vec<(String, Arc<Session>)> = {
            let sessions = self.sessions.read().await;
            sessions
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        };
        let now = now_unix();
        for (id, sess) in candidates {
            let Ok(mut inner) = sess.inner.try_lock() else {
                continue; // read 正持有：活跃
            };
            if now - inner.last_read >= IDLE_SECS {
                inner.done = true;
                drop(inner);
                self.sessions.write().await.remove(&id);
            }
        }
    }
}

/// 从攒缓冲取下一块。
///
/// - 空缓冲：`None`
/// - 非 EOF 且 <1KiB：`None`（继续攒——碎块等齐再吐，省帧开销）
/// - 其余：吐 ≤256KiB 的一段（超长切分，剩余留缓冲，下次 read 再吐）
///
/// EOF 时放宽下限：尾巴再小也吐（数据不能丢）。
pub(crate) fn next_slice(buf: &mut Vec<u8>, eof: bool) -> Option<Vec<u8>> {
    if buf.is_empty() {
        return None;
    }
    if !eof && buf.len() < MIN_SLICE_BYTES {
        return None;
    }
    let take = buf.len().min(MAX_SLICE_BYTES);
    Some(buf.drain(..take).collect())
}

/// 一个 chunk 帧的应答载荷（done 由调用方在终结分支单独给，永不并载）。
fn chunk_frame(bytes: &[u8]) -> Value {
    json!({ "chunk_b64": STANDARD.encode(bytes), "done": false })
}

/// 响应头 → JSON object（同名头逗号拼接保真；键 reqwest 已规范化小写）。
fn response_headers(resp: &reqwest::Response) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in resp.headers() {
        let v = String::from_utf8_lossy(value.as_bytes()).into_owned();
        out.entry(name.as_str().to_string())
            .and_modify(|e| {
                e.push_str(", ");
                e.push_str(&v);
            })
            .or_insert(v);
    }
    out
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn rpc_error(code: &str, message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some(code.to_string()),
    }
}

fn bad_request(message: impl Into<String>) -> RpcError {
    rpc_error("bad_request", message)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 分片纯函数（不打网络）────────────────────────────────────

    #[test]
    fn next_slice_empty_is_none() {
        let mut buf = Vec::new();
        assert!(next_slice(&mut buf, false).is_none());
        assert!(next_slice(&mut buf, true).is_none());
    }

    #[test]
    fn next_slice_buffers_under_1kib() {
        // <1KiB：非 EOF 不吐（攒），EOF 放宽下限吐尾巴
        let mut buf: Vec<u8> = vec![b'x'; MIN_SLICE_BYTES - 1];
        assert!(
            next_slice(&mut buf, false).is_none(),
            "非 EOF 的碎块应继续攒"
        );
        assert_eq!(buf.len(), MIN_SLICE_BYTES - 1, "攒模式不应消费缓冲");
        let tail = next_slice(&mut buf, true).expect("EOF 尾巴再小也要吐");
        assert_eq!(tail.len(), MIN_SLICE_BYTES - 1);
        assert!(buf.is_empty());
    }

    #[test]
    fn next_slice_returns_1kib_to_256kib_whole() {
        // 1KiB 与 256KiB 边界值都整段吐出
        for size in [MIN_SLICE_BYTES, MAX_SLICE_BYTES] {
            let mut buf: Vec<u8> = vec![b'x'; size];
            let out = next_slice(&mut buf, false).expect("满块应吐出");
            assert_eq!(out.len(), size);
            assert!(buf.is_empty());
        }
    }

    #[test]
    fn next_slice_splits_over_256kib() {
        // 300KiB → 先吐 256KiB，缓冲剩 44KiB，下次再吐
        let mut buf: Vec<u8> = vec![b'x'; MAX_SLICE_BYTES + 44 * 1024];
        let first = next_slice(&mut buf, false).expect("超长应切第一段");
        assert_eq!(first.len(), MAX_SLICE_BYTES);
        assert_eq!(buf.len(), 44 * 1024, "剩余应留在缓冲");
        let second = next_slice(&mut buf, false).expect("剩余 ≥1KiB 应再吐");
        assert_eq!(second.len(), 44 * 1024);
        assert!(buf.is_empty());
    }

    // ── 参数校验（校验发生在发请求前，不打网络）─────────────────

    fn transport() -> Arc<Transport> {
        Transport::new(reqwest::Client::new())
    }

    #[tokio::test]
    async fn request_rejects_missing_url() {
        let t = transport();
        let err = t.request(&Value::Null).await.unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
        let err = t
            .request(&serde_json::json!({"method": "GET"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
        let err = t
            .request(&serde_json::json!({"url": ""}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
    }

    #[tokio::test]
    async fn request_rejects_non_http_scheme() {
        let t = transport();
        for url in [
            "ftp://example.invalid/x",
            "file:///etc/passwd",
            "data:text/plain,hi",
        ] {
            let err = t
                .request(&serde_json::json!({"url": url}))
                .await
                .unwrap_err();
            assert_eq!(
                err.code.as_deref(),
                Some("bad_request"),
                "scheme {url} 应被拒"
            );
        }
    }

    #[tokio::test]
    async fn request_rejects_bad_method_and_body() {
        let t = transport();
        // 非法 method（含 NUL 字节，Method::from_bytes 拒）
        let err = t
            .request(
                &serde_json::json!({"url": "https://example.invalid/x", "method": "PO\u{0}ST"}),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
        // 非法 base64 body
        let err = t
            .request(&serde_json::json!({"url": "https://example.invalid/x", "body_b64": "!!!"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
    }

    #[tokio::test]
    async fn request_rejects_bad_header_name() {
        let t = transport();
        let err = t
            .request(&serde_json::json!({
                "url": "https://example.invalid/x",
                "headers": {"bad name\n": "v"}
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
    }

    // ── 会话面（不碰网络，只验表行为）────────────────────────────

    #[tokio::test]
    async fn read_unknown_session_is_not_found() {
        let t = transport();
        let err = t.read("no-such-session").await.unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn close_is_idempotent() {
        let t = transport();
        let first = t.close("never-existed").await.unwrap();
        assert_eq!(first, serde_json::json!({}));
        let second = t.close("never-existed").await.unwrap();
        assert_eq!(second, serde_json::json!({}));
    }
}
