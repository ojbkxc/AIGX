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
//! 对齐 Agent2API v3.5.0-beta.3 `core/debug_traffic/redact.rs`：写入与读取
//! 各脱敏一次（防御纵深），除请求头外还覆盖
//! - **URL**：userinfo、`token`/`api_key`/`secret`/`code`/`signature`/`state`
//!   等查询参数、`/copy|token|apikey/` 这类密钥路径段、片段（fragment）；
//! - **JSON 正文**：`authorization`/`password`/`*token`/`*secret`/`*key`/
//!   `credential`/`jwt` 等字段（含嵌套与「name+value」形态的凭据记录）；
//! - **响应正文**：HTML/JS 类响应整体隐藏（无法可靠逐字段扫描），SSE 的
//!   `data:` 行按 JSON 脱敏。
//!
//! ── 单条详情（`?id=`）──────────────────────────────────────
//! 列表接口只给摘要；详情按请求日志 id 取（[`get`] + [`detail_payload`]），
//! 与上游「列表不返回 raw、详情接口才返回」同一取舍（见 `api/admin/debug.rs`）。
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

/// 一条上游原始报文记录（请求侧 + 错误路径的响应侧）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrafficEntry {
    /// 关联键：请求日志 id（同一请求在两边同 id）。
    ///
    /// 详情端点 `GET /api/debug/traffic?id=` 按它查询；旧持久化记录没有该字段，
    /// 启动恢复时由 [`load_persisted`] 用 `request_id` 补齐。
    #[serde(default)]
    pub id: String,
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
        let shown = if header_sensitive(&lower) {
            REDACTED
        } else {
            value.as_ref()
        };
        object.insert(lower, Value::String(shown.to_string()));
    }
    object
}

// ─── 脱敏（对齐上游 core/debug_traffic/redact.rs）────────────────

/// 字段名归一化：小写、只保留字母数字（`X-Api-Key` → `xapikey`）。
fn normalized_name(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

/// 正文/查询参数里的凭据字段名（覆盖上游名单的核心集合）。
///
/// 取「宁可多脱」口径：多脱只损失排障信息，少脱是凭据事故。名单与上游
/// `redact.rs` 的 `sensitive()` 保持同形（按归一化后的名字匹配）。
fn sensitive_name(name: &str) -> bool {
    matches!(
        normalized_name(name).as_str(),
        "authorization"
            | "auth"
            | "proxyauthorization"
            | "cookie"
            | "cookies"
            | "cookieheader"
            | "sessionid"
            | "setcookie"
            | "sgcookie"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "authtoken"
            | "oauthtoken"
            | "securitytoken"
            | "sessiontoken"
            | "sessionkey"
            | "devicetoken"
            | "sid"
            | "personaltoken"
            | "personalaccesstoken"
            | "pat"
            | "apikey"
            | "accesskey"
            | "accesskeyid"
            | "secret"
            | "secretkey"
            | "apisecret"
            | "clientsecret"
            | "modelappkey"
            | "modelappsecret"
            | "password"
            | "passwd"
            | "credential"
            | "credentials"
            | "dpop"
            | "proof"
            | "jwt"
            | "privatekey"
            | "publickeyjwk"
            | "privatekeyjwk"
            | "jwk"
            | "codechallenge"
            | "codeverifier"
            | "authorizationcode"
            | "smscode"
            | "verificationcode"
            | "captchaverifyparam"
            | "authcode"
            | "state"
            | "signature"
            | "sig"
            | "xsignature"
            | "xamzcredential"
            | "xamzsignature"
            | "xamzsecuritytoken"
            | "secretaccesskey"
            | "accesskeysecret"
    )
}

/// 请求头是否凭据类：既有名单 + 用户配置的额外名单 + `*-token/-secret/-key`
/// 后缀规则（复用渠道层的同一判据，避免两套名单漂移）。
fn header_sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SENSITIVE_HEADERS.contains(&lower.as_str())
        || crate::channel::is_sensitive_header(&lower)
        || sensitive_name(&lower)
}

fn query_sensitive(name: &str) -> bool {
    sensitive_name(name) || matches!(normalized_name(name).as_str(), "code")
}

fn nested_url(name: &str) -> bool {
    matches!(
        normalized_name(name).as_str(),
        "url"
            | "redirect"
            | "redirecturi"
            | "redirecturl"
            | "returnurl"
            | "callbackurl"
            | "authcallbackurl"
            | "oauthurl"
            | "authurl"
            | "authorizeurl"
            | "navigateuri"
    )
}

/// URL 脱敏：userinfo / 片段 / 密钥路径段 / 凭据查询参数。
///
/// 解析失败时整体替换为 [`REDACTED`]（相对、畸形或额外编码的地址无法可靠
/// 检查时隐藏，而非照原文放行 —— 与上游同一取舍）。
pub fn redact_url(raw: &str) -> String {
    redact_url_at(raw, 0)
}

fn redact_url_at(raw: &str, depth: usize) -> String {
    if raw.is_empty() {
        return String::new();
    }
    if depth > 4 {
        return REDACTED.to_string();
    }
    let Ok(mut parsed) = url::Url::parse(raw) else {
        return REDACTED.to_string();
    };
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    if parsed.fragment().is_some() {
        parsed.set_fragment(Some(REDACTED));
    }
    let mut previous = String::new();
    let mut changed = false;
    let segments: Vec<String> = parsed
        .path()
        .split('/')
        .map(|segment| {
            let hide = matches!(
                normalized_name(&previous).as_str(),
                "copy" | "token" | "apikey" | "secretkey"
            );
            previous = segment.to_string();
            if hide && !segment.is_empty() {
                changed = true;
                REDACTED.into()
            } else {
                segment.into()
            }
        })
        .collect();
    if changed {
        parsed.set_path(&segments.join("/"));
    }
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| {
            let shown = if query_sensitive(&key) {
                REDACTED.to_string()
            } else if nested_url(&key) {
                redact_url_at(&value, depth + 1)
            } else {
                value.into_owned()
            };
            (key.into_owned(), shown)
        })
        .collect();
    if parsed.query().is_some() {
        parsed.query_pairs_mut().clear().extend_pairs(pairs);
    }
    parsed.to_string()
}

/// JSON 值脱敏（深拷贝后按字段名与容器形态处理）。
pub fn redact_json(source: &Value) -> Value {
    let mut output = source.clone();
    redact_json_in_place(&mut output, 0);
    output
}

fn redact_json_in_place(value: &mut Value, depth: usize) {
    if depth > 64 {
        *value = Value::String(REDACTED.into());
        return;
    }
    match value {
        Value::Object(fields) => {
            // `{name: "authorization", value: "..."}` 与 cookie 记录按容器整体隐藏
            let named_secret = fields
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(sensitive_name);
            let cookie_record = fields.get("name").is_some_and(Value::is_string)
                && fields.get("value").is_some_and(Value::is_string)
                && [
                    "domain",
                    "path",
                    "httpOnly",
                    "secure",
                    "expires",
                    "expirationDate",
                ]
                .iter()
                .any(|key| fields.contains_key(*key));
            if (named_secret || cookie_record) && fields.contains_key("value") {
                fields.insert("value".to_string(), Value::String(REDACTED.into()));
            }
            for (key, value) in fields {
                if sensitive_name(key) || normalized_name(key) == "code" && value.is_string() {
                    *value = Value::String(REDACTED.into());
                } else if nested_url(key) {
                    if let Some(raw) = value.as_str() {
                        *value = Value::String(redact_url(raw));
                    } else {
                        redact_json_in_place(value, depth + 1);
                    }
                } else {
                    redact_json_in_place(value, depth + 1);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json_in_place(item, depth + 1);
            }
        }
        Value::String(raw) => {
            // 正文里可能嵌着「JSON 字符串」形态的凭据，尝试再解析一层
            let trimmed = raw.trim();
            if (trimmed.starts_with('{') || trimmed.starts_with('[')) && !trimmed.is_empty() {
                if let Ok(mut nested) = serde_json::from_str::<Value>(trimmed) {
                    redact_json_in_place(&mut nested, depth + 1);
                    *raw = nested.to_string();
                }
            }
        }
        _ => {}
    }
}

fn content_type(headers: &Map<String, Value>) -> Option<String> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
        .and_then(|(_, value)| value.as_str())
        .map(str::to_string)
}

fn markup_type(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|kind| {
        matches!(
            kind.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
                .as_str(),
            "text/html"
                | "application/xhtml+xml"
                | "text/javascript"
                | "application/javascript"
                | "text/ecmascript"
                | "application/ecmascript"
        )
    })
}

/// 截断/畸形 JSON 的逐字段兜底脱敏。
///
/// 完整解析失败（通常是超过 [`MAX_BODY_BYTES`] 被截断）时不再整段隐藏 ——
/// 否则大请求体就完全失去排障价值。扫描 `"key": value` 形态：
/// - 敏感 key 的字符串值 → `"[redacted]"`；
/// - 敏感 key 的非字符串值（数字/对象/数组…）→ `"[redacted]"` 并从该处起
///   停止输出（无法廉价确认嵌套结构里没有凭据；宁可少展示，不泄露）。
fn redact_json_text_fallback(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::with_capacity(raw.len());
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '"' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // 读一个字符串 token（保留原始切片，避免重写转义）
        let start = i;
        i += 1;
        let mut key = String::new();
        let mut closed = false;
        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    if i + 1 < chars.len() {
                        key.push(chars[i + 1]);
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                '"' => {
                    closed = true;
                    i += 1;
                    break;
                }
                ch => {
                    key.push(ch);
                    i += 1;
                }
            }
        }
        if !closed {
            // 截断发生在 key 中间：此时还没碰到敏感值，保留原文
            out.extend(chars[start..].iter());
            break;
        }
        // key 的定义：跳过空白后紧跟 ':'
        let mut j = i;
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j >= chars.len() || chars[j] != ':' || !sensitive_name(&key) {
            out.extend(chars[start..i].iter());
            continue;
        }
        out.extend(chars[start..i].iter());
        out.extend(chars[i..=j].iter());
        i = j + 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i < chars.len() && chars[i] == '"' {
            // 字符串值：吃到闭合引号；截断在半截字符串时停止输出
            let mut k = i + 1;
            let mut value_closed = false;
            while k < chars.len() {
                match chars[k] {
                    '\\' => k += 2,
                    '"' => {
                        value_closed = true;
                        k += 1;
                        break;
                    }
                    _ => k += 1,
                }
            }
            out.push_str("\"[redacted]\"");
            if !value_closed {
                return out;
            }
            i = k;
        } else {
            out.push_str("\"[redacted]\"");
            out.push_str("\n[rest omitted: sensitive value]");
            return out;
        }
    }
    out
}

/// 文本正文脱敏：JSON / 表单 / SSE 逐行处理；HTML/JS 整体隐藏。
///
/// 纯文本（例如非 JSON 的 SSE 增量）原样保留 —— 它不是结构化的凭据载体，
/// 整段隐藏会把排障信息一起丢掉。
pub fn redact_text(raw: &str, content_type: Option<&str>) -> String {
    let prefix = raw.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    if markup_type(content_type) || prefix.starts_with('<') {
        return REDACTED.to_string();
    }
    if let Ok(parsed) = serde_json::from_str::<Value>(raw) {
        return redact_json(&parsed).to_string();
    }
    if content_type.is_some_and(|kind| {
        kind.split(';').next().is_some_and(|kind| {
            kind.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
    }) {
        let mut encoded = url::form_urlencoded::Serializer::new(String::new());
        for (key, value) in url::form_urlencoded::parse(raw.as_bytes()) {
            let shown = if query_sensitive(&key) {
                REDACTED.to_string()
            } else if nested_url(&key) {
                redact_url(&value)
            } else {
                value.into_owned()
            };
            encoded.append_pair(&key, &shown);
        }
        return encoded.finish();
    }
    let json_response = content_type.is_some_and(|kind| {
        let kind = kind
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        kind == "application/json" || kind.ends_with("+json")
    });
    if json_response {
        // 声称是 JSON 却解析失败：多半是超过 MAX_BODY_BYTES 被截断。逐字段扫描
        // 仍能把凭据替换掉；若在敏感值中间截断，从该处起整体隐藏（不泄露半截）。
        return redact_json_text_fallback(raw);
    }
    let mut output = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        let (body, ending) = if let Some(body) = line.strip_suffix("\r\n") {
            (body, "\r\n")
        } else if let Some(body) = line.strip_suffix('\n') {
            (body, "\n")
        } else {
            (line, "")
        };
        if let Some(data) = body.strip_prefix("data:") {
            let leading = &data[..data.len() - data.trim_start().len()];
            match serde_json::from_str::<Value>(data.trim()) {
                Ok(parsed) => {
                    output.push_str("data:");
                    output.push_str(leading);
                    output.push_str(&redact_json(&parsed).to_string());
                }
                Err(_) => output.push_str(body),
            }
        } else if let Ok(parsed) = serde_json::from_str::<Value>(body) {
            output.push_str(&redact_json(&parsed).to_string());
        } else {
            output.push_str(body);
        }
        output.push_str(ending);
    }
    output
}

fn redact_header_map(headers: &mut Map<String, Value>) {
    for (name, value) in headers {
        if header_sensitive(name) {
            *value = Value::String(REDACTED.into());
        } else if matches!(name.to_ascii_lowercase().as_str(), "location" | "referer") {
            if let Some(raw) = value.as_str() {
                *value = Value::String(redact_url(raw));
            }
        }
    }
}

/// 整条记录脱敏（写入前与读取前各跑一次，保证历史数据也被覆盖）。
pub fn redact_entry(entry: &mut TrafficEntry) {
    entry.url = redact_url(&entry.url);
    redact_header_map(&mut entry.headers);
    let request_type = content_type(&entry.headers);
    if let Some(body) = &mut entry.body {
        *body = redact_text(body, request_type.as_deref());
    }
    if let Some(headers) = &mut entry.response_headers {
        redact_header_map(headers);
    }
    if let Some(body) = &mut entry.response_body {
        let response_type = entry.response_headers.as_ref().and_then(content_type);
        *body = redact_text(body, response_type.as_deref());
    }
}

/// 请求日志「详情」的响应形态（字段名对齐 Agent2API `detail_payload`）。
///
/// 请求侧与响应侧都以 camelCase 给出；`status` 保留 `null`（而不是 0）——
/// 前端据此区分「没收到响应」与「响应码 0」。
pub fn detail_payload(entry: &TrafficEntry) -> Value {
    let mut entry = entry.clone();
    if entry.id.is_empty() {
        entry.id = entry.request_id.clone();
    }
    redact_entry(&mut entry);
    serde_json::json!({
        "id": entry.id,
        "requestId": entry.request_id,
        "ts": entry.ts,
        "method": entry.method,
        "url": entry.url,
        "model": entry.model,
        "accountId": entry.account_id,
        "requestHeaders": entry.headers,
        "requestBody": entry.body,
        "status": entry.response_status,
        "responseHeaders": entry.response_headers,
        "responseBody": entry.response_body,
        "truncated": entry.truncated,
    })
}

/// 按请求日志 id 取一条（同 id 取最新）。库/缓冲都不可用时 `None`。
pub fn get(id: &str) -> Option<TrafficEntry> {
    let id = id.trim();
    if id.is_empty() {
        return None;
    }
    let buffer = BUFFER.lock();
    buffer
        .iter()
        .rev()
        .find(|entry| entry.id == id || entry.request_id == id)
        .cloned()
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
            id: ctx.request_id.clone(),
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
fn push(mut entry: TrafficEntry) {
    // 写入前脱敏：URL 查询凭据与 JSON 正文里的凭据字段不能明文落盘/进缓冲
    redact_entry(&mut entry);
    // parking_lot 锁不会中毒；环形缓冲只是调试视图，不阻塞数据面语义。
    let mut buffer = BUFFER.lock();
    while buffer.len() >= MAX_ENTRIES {
        buffer.pop_front();
        DROPPED.fetch_add(1, Ordering::Relaxed);
    }
    buffer.push_back(entry.clone());

    // 持久化到 FileStore（best-effort，失败不阻断数据面）。
    // 键含请求 id：同一毫秒内的多条报文不再互相覆盖（旧键 `debug_traffic:<ts>`
    // 仍能被 list 前缀命中，恢复路径兼容）。
    if let Some(store) = STORE.get() {
        let key = format!("debug_traffic:{}:{}", entry.ts, entry.id);
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
    // 首字节延迟（TTFB）：始终捕获，不依赖 debug 开关
    if let (Some(start), Some(mut slot)) = (
        ctx.request_start_ms.lock().ok().and_then(|g| *g),
        ctx.first_response_ms.lock().ok(),
    ) {
        if slot.is_none() {
            let now = chrono::Utc::now().timestamp_millis() as u64;
            *slot = Some(now.saturating_sub(start));
        }
    }
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
        let response_type = entry.response_headers.as_ref().and_then(content_type);
        entry.response_body = Some(redact_text(&text, response_type.as_deref()));
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
        if let Ok(Some(mut entry)) = store.get::<TrafficEntry>(&key) {
            // 旧版本持久化记录没有 `id`：用 request_id 补齐（详情 `?id=` 可用）
            if entry.id.is_empty() {
                entry.id = entry.request_id.clone();
            }
            // 历史记录可能来自「仅头脱敏」的旧版本：恢复时再过一遍全量脱敏
            redact_entry(&mut entry);
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

    #[test]
    fn redacts_url_credentials_but_keeps_routing_facts() {
        let raw = "https://user:pass@api.example.com/v1/chat/completions\
                   ?token=secret&api_key=k1&signature=sig&model=gpt-4o&stream=true";
        let redacted = redact_url(raw);
        assert!(redacted.contains("token=%5Bredacted%5D") || redacted.contains("token=[redacted]"));
        assert!(
            redacted.contains("api_key=%5Bredacted%5D") || redacted.contains("api_key=[redacted]")
        );
        assert!(
            redacted.contains("signature=%5Bredacted%5D")
                || redacted.contains("signature=[redacted]")
        );
        // 正向对照：路径、模型与流式参数必须保留（否则详情失去排障价值）
        assert!(redacted.contains("/v1/chat/completions"));
        assert!(redacted.contains("model=gpt-4o"));
        assert!(redacted.contains("stream=true"));
        assert!(!redacted.contains("user:pass@"));
    }

    #[test]
    fn redacts_key_path_segments() {
        let redacted = redact_url("https://api.example.com/copy/abc123/files/token/xyz");
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("xyz"));
    }

    #[test]
    fn redacts_json_body_credentials_and_keeps_content() {
        let body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "你好"}],
            "api_key": "sk-leak",
            "nested": {"authorization": "Bearer leak", "temperature": 0.2},
            // 容器键本身不敏感（"cookies" 是敏感键，会整段隐藏；见下方单独断言）
            "jar": [{"name": "sessionid", "value": "leak", "domain": "x"}]
        });
        let redacted = redact_json(&body);
        assert_eq!(redacted["api_key"], REDACTED);
        assert_eq!(redacted["nested"]["authorization"], REDACTED);
        // name+value 形态的凭据记录：value 脱敏、键名保留
        assert_eq!(redacted["jar"][0]["value"], REDACTED);
        assert_eq!(redacted["jar"][0]["name"], "sessionid");
        // 正向对照：业务字段保持不变
        assert_eq!(redacted["model"], "gpt-4o");
        assert_eq!(redacted["messages"][0]["content"], "你好");
        assert_eq!(redacted["nested"]["temperature"], 0.2);
        // 敏感容器键（cookies）按整段隐藏（无法保证数组内每个元素都被识别）
        let wholesale = redact_json(&serde_json::json!({"cookies": [{"a": 1}]}));
        assert_eq!(wholesale["cookies"], REDACTED);
    }

    #[test]
    fn redact_text_handles_sse_and_markup() {
        // SSE 的 data 行按 JSON 脱敏；普通增量文本保留
        let sse = "data: {\"token\":\"leak\",\"delta\":\"hi\"}\n\ndata: [DONE]\n";
        let out = redact_text(sse, Some("text/event-stream"));
        assert!(out.contains("\"token\":\"[redacted]\""));
        assert!(out.contains("\"delta\":\"hi\""));
        // HTML/JS 响应整体隐藏（无法可靠逐字段扫描）
        assert_eq!(
            redact_text("<html><script>token</script></html>", Some("text/html")),
            REDACTED
        );
        // 声称 JSON 但完全不是 JSON：无凭据字段可脱，原样保留（截断正文兜底）
        assert_eq!(
            redact_text("not-json", Some("application/json")),
            "not-json"
        );
    }

    #[test]
    fn truncated_json_fallback_still_redacts_credentials() {
        // 超长正文被截断（没有闭合括号）时走逐字段扫描，而不是整段隐藏
        let truncated =
            "{\"api_key\":\"leak\",\"model\":\"gpt-4o\",\"messages\":[{\"role\":\"user\"";
        let out = redact_text(truncated, Some("application/json"));
        assert!(!out.contains("leak"));
        assert!(out.contains("\"api_key\":\"[redacted]\""));
        // 正向对照：业务字段保留
        assert!(out.contains("gpt-4o"));
        // 敏感非字符串值：从该处起整体隐藏，不把值带出去
        let numeric = redact_text(
            "{\"password\":123456,\"model\":\"gpt-4o\"}",
            Some("application/json"),
        );
        assert!(!numeric.contains("123456"));
    }

    #[test]
    fn redact_entry_masks_both_sides_and_detail_payload_keeps_shape() {
        let mut entry = TrafficEntry {
            id: "req-1".into(),
            ts: 1,
            request_id: "req-1".into(),
            model: "gpt-4o".into(),
            account_id: Some("acc-1".into()),
            method: "POST".into(),
            url: "https://api.example.com/v1/chat?token=leak".into(),
            headers: redact_headers(vec![
                ("Authorization", "Bearer leak"),
                ("Content-Type", "application/json"),
            ]),
            body: Some("{\"api_key\":\"leak\",\"model\":\"gpt-4o\"}".into()),
            truncated: false,
            response_status: Some(200),
            response_headers: Some(redact_headers(vec![("Set-Cookie", "sessionid=leak")])),
            response_body: Some("{\"result\":\"ok\",\"access_token\":\"leak\"}".into()),
        };
        redact_entry(&mut entry);
        assert!(!entry.url.contains("leak"));
        assert_eq!(entry.headers["authorization"], REDACTED);
        assert!(entry.body.as_deref().unwrap().contains("[redacted]"));
        assert!(!entry.body.as_deref().unwrap().contains("\"leak\""));
        assert_eq!(
            entry.response_headers.as_ref().unwrap()["set-cookie"],
            REDACTED
        );
        assert!(!entry.response_body.as_deref().unwrap().contains("leak"));

        let detail = detail_payload(&entry);
        assert_eq!(detail["id"], "req-1");
        assert_eq!(detail["requestId"], "req-1");
        assert_eq!(detail["status"], 200);
        assert_eq!(detail["model"], "gpt-4o");
        assert_eq!(detail["requestHeaders"]["content-type"], "application/json");
        // 正向对照：业务结果保留
        assert!(detail["responseBody"]
            .as_str()
            .unwrap()
            .contains("\"result\":\"ok\""));
    }
}
