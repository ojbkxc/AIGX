//! 调试模式 · **上游原始报文**抓取（对齐 Agent2API `core/debug_traffic.rs`）。
//!
//! ── 与 `log_body` 的分工 ─────────────────────────────────────
//! `log_body`（`src/log/mod.rs` 的 `LogDebugSnapshot`）抓的是**下游**侧：
//! 客户端发来的请求体 + 网关最终下发的响应体，挂在请求日志上（写库）。
//! 本模块抓的是**上游**侧：AIGX 实际发往各家的 URL / 请求头 / 请求体
//! ——即「按别名映射改写过 model、叠加过渠道静态头之后，最终出网的那一份」。
//! 两者互补：前者回答「用户要什么」，后者回答「我们到底发出去了什么」。
//!
//! ── 持久化（P1-5 落库）───────────────────────────────────────
//! 1.0.32 起，每条报文同时写入 FileStore（`debug_traffic:<ts>` 键），
//! 进程内环形缓冲保留用于快速读取。重启后从 FileStore 恢复最近
//! MAX_ENTRIES 条，清空时同步删除 FileStore 中的报文。
//!
//! ── 脱敏是硬要求（不可关闭）──────────────────────────────────
//! 请求头里的 `authorization` / `cookie` / `x-api-key` / 各家自定义 token
//! 一律替换成 `[redacted]`（见 [`redact_headers`]）。报文会展示在管理页上，
//! 不脱敏等于把各家账号的 token 明文摊开。
//!
//! ── 开关与开销 ───────────────────────────────────────────────
//! 由 [`crate::config::debug_traffic_enabled`] 的原子快照控制（设置页
//! 「调试模式」）。关闭时 [`traced`] 直接透传构造器，不做 `try_clone` /
//! `build`，零开销。

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::bridge::BridgeContext;

/// 环形缓冲最多保留的条数（够看「最近这批请求」，与上游 MAX_ENTRIES 同量级）。
pub const MAX_ENTRIES: usize = 200;

/// 单条请求体的截断上限（字节）。请求侧通常远小于它；真超了的多半是
/// 异常长上下文，截断比整条丢弃有用。
pub const MAX_BODY_BYTES: usize = 64 * 1024;

/// 凭据类请求头：命中即替换成 [`REDACTED`]（大小写不敏感）。
///
/// 名单对齐上游 `SENSITIVE_HEADERS`：除了通用凭据头，还补了账号型渠道
/// 各家适配器会带的自定义 token 头（宁可多脱几个，少脱是事故）。
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "cookie2",
    "set-cookie",
    "x-api-key",
    "api-key",
    "x-auth-token",
    "x-access-token",
    "x-refresh-token",
    "x-ide-token",
    "x-ide-auth",
    "x-workbuddy-token",
    "x-raccoon-token",
    "x-cline-token",
    "x-qoder-token",
    "x-catpaw-cookie",
    "x-autoclaw-token",
];

/// 脱敏占位符（与上游同字面量，便于对照）。
pub const REDACTED: &str = "[redacted]";

/// 一条上游原始报文记录（只有请求侧）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrafficEntry {
    /// 记录时刻（毫秒 Unix 时间戳）
    pub ts: i64,
    /// 关联键：与请求日志同值的 request id（便于对照日志详情）
    #[serde(default)]
    pub request_id: String,
    /// 本次请求的上游模型真名（别名映射之后）
    #[serde(default)]
    pub model: String,
    /// 实际承载的账号 id（账号型渠道才有）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// HTTP 方法
    pub method: String,
    /// 上游 URL（含路径与查询串）
    pub url: String,
    /// 发给上游的请求头（**已脱敏**）
    #[serde(default)]
    pub headers: Map<String, Value>,
    /// 发给上游的请求体（截断至 [`MAX_BODY_BYTES`]）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// 请求体是否被截断
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// 上游响应状态码（P1-7：响应侧抓取）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_status: Option<u16>,
    /// 上游响应头（**已脱敏**，P1-7）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_headers: Option<Map<String, Value>>,
    /// 上游响应体（仅错误路径抓取，截断至 [`MAX_BODY_BYTES`]，P1-7）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_body: Option<String>,
}

/// 进程级环形缓冲。`Mutex<VecDeque<_>>`：写入是每请求一次（开关开启时），
/// 读取是管理页按需拉取——不是热路径，无需无锁结构。
static BUFFER: Mutex<VecDeque<TrafficEntry>> = Mutex::new(VecDeque::new());

/// 累计丢弃条数（超出 [`MAX_ENTRIES`] 被挤掉的总数），仅供概览展示。
static DROPPED: AtomicU64 = AtomicU64::new(0);

/// FileStore 句柄（启动时注入，用于报文持久化）。
static STORE: OnceLock<std::sync::Arc<crate::storage::FileStore>> = OnceLock::new();

/// 注入 FileStore（main 启动时调用一次）。
pub fn set_store(store: std::sync::Arc<crate::storage::FileStore>) {
    let _ = STORE.set(store);
}

/// 头集合脱敏：凭据类字段换成 [`REDACTED`]，其余原样；键统一小写。
///
/// 接受任意 `(name, value)` 迭代器（`&str` / `String` 都行），避免在调用点
/// 为了凑类型而额外分配。
pub fn redact_headers<I, N, V>(headers: I) -> Map<String, Value>
where
    I: IntoIterator<Item = (N, V)>,
    N: AsRef<str>,
    V: AsRef<str>,
{
    let mut object = Map::new();
    for (name, value) in headers {
        let lower = name.as_ref().to_ascii_lowercase();
        let shown = if SENSITIVE_HEADERS.contains(&lower.as_str())
            || crate::config::is_extra_sensitive_header(&lower)
        {
            REDACTED
        } else {
            value.as_ref()
        };
        object.insert(lower, Value::String(shown.to_string()));
    }
    object
}

/// 字节安全截断（不切断 UTF-8 多字节序列），返回 (文本, 是否截断)。
fn truncate_bytes(bytes: &[u8], limit: usize) -> (String, bool) {
    if bytes.len() <= limit {
        return (String::from_utf8_lossy(bytes).to_string(), false);
    }
    let mut end = limit;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    (String::from_utf8_lossy(&bytes[..end]).to_string(), true)
}

impl TrafficEntry {
    /// 从已 build 的 reqwest 请求 + 桥接上下文构造一条记录。
    fn from_request(req: &reqwest::Request, ctx: &BridgeContext) -> Self {
        let url = req.url().to_string();
        let method = req.method().to_string();
        let headers = redact_headers(
            req.headers()
                .iter()
                .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or("<binary>"))),
        );
        let (body, truncated) = match req.body().and_then(|b| b.as_bytes()) {
            Some(bytes) => {
                let (text, cut) = truncate_bytes(bytes, MAX_BODY_BYTES);
                (if text.is_empty() { None } else { Some(text) }, cut)
            }
            None => (None, false),
        };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            ts,
            request_id: ctx.request_id.clone(),
            model: ctx.model.clone(),
            account_id: ctx.account_id.clone(),
            method,
            url,
            headers,
            body,
            truncated,
            response_status: None,
            response_headers: None,
            response_body: None,
        }
    }
}

/// 记录一条（内部接口，供 [`traced`] 调用）。
fn push(entry: TrafficEntry) {
    // parking_lot 锁不会中毒；环形缓冲只是调试视图，不阻塞数据面语义。
    let mut buffer = BUFFER.lock();
    while buffer.len() >= MAX_ENTRIES {
        buffer.pop_front();
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    buffer.push_back(entry.clone());

    // 持久化到 FileStore（best-effort，失败不阻断数据面）
    if let Some(store) = STORE.get() {
        let key = format!("debug_traffic:{}", entry.ts);
        let _ = store.put(&key, &entry);
    }
}

/// 抓取构造器里已定稿的请求（URL/头/体），并把**原构造器**透传回去。
///
/// 调用点形如 `debug_traffic::traced(builder.json(&body), ctx).send()`：
/// 开关关闭时直接返回入参（零开销）；开启时先 `try_clone` + `build` 取一份
/// 快照，原构造器原样交还给 `.send()`。
///
/// `try_clone` 对「body 是流」的构造器返回 `None`（例如直接喂
/// `Body::wrap_stream`）——此时静默跳过，不影响请求本身。本仓库的桥接层
/// 一律 `.json(&body)`，所以正常路径都能抓到。
pub fn traced(req: reqwest::RequestBuilder, ctx: &BridgeContext) -> reqwest::RequestBuilder {
    if !crate::config::debug_traffic_enabled() {
        return req;
    }
    if let Some(built) = req.try_clone().and_then(|r| r.build().ok()) {
        push(TrafficEntry::from_request(&built, ctx));
    }
    req
}

/// 抓取上游响应的状态码与响应头（P1-7 响应侧报文）。
///
/// 在 `.send().await` 成功后、消费响应体之前调用：
/// ```ignore
/// let resp = debug_traffic::traced(builder, ctx).send().await?;
/// debug_traffic::traced_response(&resp, ctx);
/// ```
/// 只读取 status + headers（零 body 消费），不影响后续 `resp.json()` /
/// `resp.bytes_stream()` 的读取。开关关闭时零开销。
pub fn traced_response(resp: &reqwest::Response, ctx: &BridgeContext) {
    if !crate::config::debug_traffic_enabled() {
        return;
    }
    let status = resp.status().as_u16();
    let headers = redact_headers(
        resp.headers()
            .iter()
            .map(|(k, v)| (k.as_str(), v.to_str().unwrap_or("<binary>"))),
    );
    let mut buffer = BUFFER.lock();
    // 找到刚 push 的那条（request_id 匹配），补上响应侧字段
    if let Some(entry) = buffer
        .iter_mut()
        .rev()
        .find(|e| e.request_id == ctx.request_id)
    {
        entry.response_status = Some(status);
        entry.response_headers = Some(headers);
    }
}

/// 抓取上游错误响应体（P1-7）：在错误路径消费 body 之前调用。
///
/// 错误响应的 body 通常很短（JSON 错误信息），读取后不影响后续解析。
/// 调用点形如：
/// ```ignore
/// if !resp.status().is_success() {
///     let body_text = resp.text().await.unwrap_or_default();
///     debug_traffic::traced_response_body(&body_text, ctx);
///     return Err(...);
/// }
/// ```
pub fn traced_response_body(body: &str, ctx: &BridgeContext) {
    if !crate::config::debug_traffic_enabled() {
        return;
    }
    let (text, truncated) = truncate_bytes(body.as_bytes(), MAX_BODY_BYTES);
    let mut buffer = BUFFER.lock();
    if let Some(entry) = buffer
        .iter_mut()
        .rev()
        .find(|e| e.request_id == ctx.request_id)
    {
        entry.response_body = Some(text);
        entry.truncated = entry.truncated || truncated;
    }
}

/// 读取当前缓冲（最新在后）与概览计数。
pub fn snapshot() -> Value {
    let b = BUFFER.lock();
    let entries = b.iter().cloned().collect::<Vec<_>>();
    let dropped = DROPPED.load(Ordering::Relaxed);
    let (entries, dropped) = (entries, dropped);
    serde_json::json!({
        "enabled": crate::config::debug_traffic_enabled(),
        "maxEntries": MAX_ENTRIES,
        "maxBodyBytes": MAX_BODY_BYTES,
        "dropped": dropped,
        // 最新在前，方便页面直接按序渲染
        "entries": entries.into_iter().rev().collect::<Vec<_>>(),
    })
}

/// 清空缓冲（返回清掉的条数）。同步删除 FileStore 中的持久化报文。
pub fn clear() -> usize {
    let mut b = BUFFER.lock();
    let n = b.len();
    b.clear();
    // 删除 FileStore 中的报文
    if let Some(store) = STORE.get() {
        if let Ok(keys) = store.list("debug_traffic:") {
            for key in keys {
                let _ = store.delete(&key);
            }
        }
    }
    n
}

/// 启动时从 FileStore 恢复最近报文到内存缓冲。
///
/// 读取所有 `debug_traffic:*` 键，按 ts 排序后取最近 MAX_ENTRIES 条载入。
pub fn load_persisted() {
    let Some(store) = STORE.get() else {
        return;
    };
    let Ok(keys) = store.list("debug_traffic:") else {
        return;
    };
    let mut entries: Vec<TrafficEntry> = Vec::new();
    for key in keys {
        if let Ok(Some(entry)) = store.get::<TrafficEntry>(&key) {
            entries.push(entry);
        }
    }
    // 按 ts 降序排列，取最近 MAX_ENTRIES 条
    entries.sort_by_key(|a| std::cmp::Reverse(a.ts));
    entries.truncate(MAX_ENTRIES);
    let mut buffer = BUFFER.lock();
    for entry in entries {
        buffer.push_back(entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credential_headers_case_insensitively() {
        let m = redact_headers(vec![
            ("Authorization", "Bearer secret"),
            ("X-Api-Key", "sk-123"),
            ("content-type", "application/json"),
        ]);
        assert_eq!(m["authorization"].as_str(), Some(REDACTED));
        assert_eq!(m["x-api-key"].as_str(), Some(REDACTED));
        assert_eq!(m["content-type"].as_str(), Some("application/json"));
    }

    #[test]
    fn truncation_is_char_boundary_safe() {
        // 每个汉字 3 字节；限制 4 字节时只能保留 1 个汉字（3 字节）
        let bytes = "中文字".as_bytes();
        let (text, cut) = truncate_bytes(bytes, 4);
        assert!(cut);
        assert_eq!(text, "中");
    }
}
