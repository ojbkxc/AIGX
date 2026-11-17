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
//! password}`）与 `trace`（调试抓包）。AIGX 用 reqwest `Proxy::all` 为每条
//! 不同代理建独立 client（http/https/socks5/socks5h 全部支持，走 reqwest
//! 的 socks feature），直连仍用网关全局 client；本机环回目标按上游口径
//! 绕过代理。`trace` 暂不实现（AIGX 无 debug_traffic 的插件抓包通道）——
//! 这些差异集中在适配层，不污染 transport 的既有测试。

use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};

use serde_json::{json, Value};

use crate::plugin_host::rpc::RpcError;

use super::transport::Transport;

/// 目标主机是本机环回（localhost / 127.0.0.0/8 / ::1）时不走账号代理。
///
/// 与上游 `script/transport.rs::proxy_for` 同口径：插件的本机浏览器控制与
/// 回调不能发往账号的远程代理。这是**语义决策**而非静默忽略——代理参数在
/// 其余所有目标上都必须生效或明确报错。
pub(crate) fn is_local_host(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => {
            host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        }
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|address| address.is_loopback())
        }
        None => false,
    }
}

/// 代理协议（wire 形状的 `protocol` 字段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProxyProtocol {
    Http,
    Https,
    Socks5,
    Socks5h,
}

impl ProxyProtocol {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw {
            "http" => Some(Self::Http),
            "https" => Some(Self::Https),
            "socks5" => Some(Self::Socks5),
            "socks5h" => Some(Self::Socks5h),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks5 => "socks5",
            Self::Socks5h => "socks5h",
        }
    }
}

/// 一条解析后的代理端点（`{protocol,host,port,username,password}` 的强类型
/// 形态；`script.proxy.resolve` 的 wire 形状与字符串 URL 都归一到它）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxyEndpoint {
    pub protocol: ProxyProtocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl ProxyEndpoint {
    /// 代理 URL（reqwest `Proxy::all` / 日志用；凭据按 URL 规则转义）。
    pub(crate) fn url(&self) -> Result<String, String> {
        let host = if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let mut url = url::Url::parse(&format!(
            "{}://{host}:{}",
            self.protocol.as_str(),
            self.port
        ))
        .map_err(|e| format!("invalid proxy address: {e}"))?;
        if !self.username.is_empty() {
            url.set_username(&self.username)
                .map_err(|_| "invalid proxy username".to_string())?;
        }
        if !self.password.is_empty() {
            url.set_password(Some(&self.password))
                .map_err(|_| "invalid proxy password".to_string())?;
        }
        Ok(url.to_string())
    }
}

/// 解析 `params.proxy`（null/缺省 = 直连；对象或 URL 字符串 = 代理）。
///
/// 本机环回目标直接返回 `None`（见 [`is_local_host`]）。**协议不支持一律
/// 报错，绝不静默直连**——账号配了代理却从错误出口发流量是安全问题。
pub(crate) fn proxy_endpoint(
    params: &Value,
    target: &url::Url,
) -> Result<Option<ProxyEndpoint>, String> {
    if is_local_host(target) {
        return Ok(None);
    }
    match params.get("proxy") {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => parse_proxy_value(raw).map(Some),
    }
}

fn parse_proxy_value(raw: &Value) -> Result<ProxyEndpoint, String> {
    if let Some(text) = raw.as_str() {
        let url =
            url::Url::parse(text.trim()).map_err(|_| format!("invalid proxy url '{text}'"))?;
        return parse_proxy_url(&url);
    }
    let object = raw
        .as_object()
        .ok_or_else(|| "proxy must be an object or a proxy url string".to_string())?;
    let protocol_raw = object.get("protocol").and_then(Value::as_str).unwrap_or("");
    let protocol = ProxyProtocol::parse(protocol_raw).ok_or_else(|| {
        format!("unsupported proxy protocol '{protocol_raw}' (http/https/socks5/socks5h)")
    })?;
    let host = object
        .get("host")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "proxy host is required".to_string())?
        .to_string();
    let port = object
        .get("port")
        .and_then(Value::as_u64)
        .ok_or_else(|| "proxy port is required".to_string())?;
    if !(1..=u16::MAX as u64).contains(&port) {
        return Err(format!("invalid proxy port {port}"));
    }
    Ok(ProxyEndpoint {
        protocol,
        host,
        port: port as u16,
        username: object
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        password: object
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}

fn parse_proxy_url(url: &url::Url) -> Result<ProxyEndpoint, String> {
    let protocol = ProxyProtocol::parse(url.scheme()).ok_or_else(|| {
        format!(
            "unsupported proxy protocol '{}' (http/https/socks5/socks5h)",
            url.scheme()
        )
    })?;
    let host = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "proxy host is required".to_string())?
        .to_string();
    let port = url.port_or_known_default().unwrap_or(
        if matches!(protocol, ProxyProtocol::Socks5 | ProxyProtocol::Socks5h) {
            1080
        } else {
            80
        },
    );
    Ok(ProxyEndpoint {
        protocol,
        host,
        port,
        username: url.username().to_string(),
        password: url.password().unwrap_or("").to_string(),
    })
}

/// 按代理端点缓存 `Transport`（每个不同代理一份 reqwest client）。
///
/// 缓存上限 [`MAX_PROXY_TRANSPORTS`]：超限整体清空——在途会话各自持有
/// `Arc<Transport>`，清缓存不会打断它们，只是下次重新建（代理池条目数
/// 通常远小于上限）。
pub(crate) struct ProxyTransports {
    inner: StdMutex<HashMap<String, Arc<Transport>>>,
}

const MAX_PROXY_TRANSPORTS: usize = 64;

impl ProxyTransports {
    pub(crate) fn new() -> Self {
        Self {
            inner: StdMutex::new(HashMap::new()),
        }
    }

    /// 取（或建）该代理的 Transport。
    pub(crate) fn get_or_create(
        &self,
        endpoint: &ProxyEndpoint,
    ) -> Result<Arc<Transport>, RpcError> {
        let key = endpoint.url().map_err(bad_request)?;
        if let Ok(map) = self.inner.lock() {
            if let Some(transport) = map.get(&key) {
                return Ok(transport.clone());
            }
        }
        let proxy = reqwest::Proxy::all(key.clone())
            .map_err(|e| bad_request(format!("script.http.open: invalid proxy: {e}")))?;
        let client = reqwest::Client::builder()
            .proxy(proxy)
            .build()
            .map_err(|e| internal(format!("script.http.open: build proxied client: {e}")))?;
        let transport = Transport::new(client);
        if let Ok(mut map) = self.inner.lock() {
            if map.len() >= MAX_PROXY_TRANSPORTS {
                map.clear();
            }
            map.insert(key, transport.clone());
        }
        Ok(transport)
    }
}

impl Default for ProxyTransports {
    fn default() -> Self {
        Self::new()
    }
}

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
/// 这里维护 `客户端 id → (Transport, session)` 的映射，close 时双向清理。
///
/// **代理**：`params.proxy`（`script.proxy.resolve` 的 wire 形状）走
/// reqwest `Proxy::all` 的独立 client（http/https/socks5/socks5h），
/// 与直连的全局 client 隔离；本机环回目标按上游口径直连。
pub(crate) async fn open(
    transports: &ProxyTransports,
    default_transport: &Arc<Transport>,
    sessions: &SessionMap,
    params: &Value,
) -> Result<Value, RpcError> {
    let (id, open) = open_params(params)?;
    let target = open
        .get("url")
        .and_then(Value::as_str)
        .and_then(|url| url::Url::parse(url).ok());
    let endpoint = match target {
        Some(target) => proxy_endpoint(params, &target).map_err(bad_request)?,
        None => None, // 非法 URL 交给 Transport 报 bad_request（错误文案同旧路径）
    };
    let transport = match endpoint {
        None => default_transport.clone(),
        Some(endpoint) => transports.get_or_create(&endpoint)?,
    };
    let created = transport.request(&open).await?;
    let session = created
        .get("session")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError {
            message: "script.http.open: transport did not return a session".into(),
            code: Some("internal".into()),
        })?
        .to_string();
    sessions.insert(id.clone(), transport, session);
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
pub(crate) async fn read(sessions: &SessionMap, params: &Value) -> Result<Value, RpcError> {
    let id = params
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request("script.http.read: id is required"))?;
    let (transport, session) = sessions
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
pub(crate) async fn close(sessions: &SessionMap, params: &Value) -> Result<Value, RpcError> {
    if let Some(id) = params.get("id").and_then(Value::as_str) {
        if let Some((transport, session)) = sessions.remove(id) {
            let _ = transport.close(&session).await;
        }
    }
    Ok(Value::Null)
}

/// 一条活跃的 HTTP 会话：客户端 id → 承载它的 Transport（直连或按代理）
/// + Transport 内部的 session 键。
struct HttpSession {
    transport: Arc<Transport>,
    session: String,
}

/// 客户端 id → Transport session 的映射表。
///
/// 用 `std::sync::Mutex`（不是 tokio）：映射操作是纯内存 O(1)，不跨 await
/// 持锁——`open` 里先 await transport 再插入，`read/close` 先取快照再 await。
pub(crate) struct SessionMap {
    inner: StdMutex<HashMap<String, HttpSession>>,
}

impl SessionMap {
    pub(crate) fn new() -> Self {
        Self {
            inner: StdMutex::new(HashMap::new()),
        }
    }

    fn insert(&self, id: String, transport: Arc<Transport>, session: String) {
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id, HttpSession { transport, session });
        }
    }

    fn get(&self, id: &str) -> Option<(Arc<Transport>, String)> {
        let map = self.inner.lock().ok()?;
        let entry = map.get(id)?;
        Some((entry.transport.clone(), entry.session.clone()))
    }

    fn remove(&self, id: &str) -> Option<(Arc<Transport>, String)> {
        let mut map = self.inner.lock().ok()?;
        let entry = map.remove(id)?;
        Some((entry.transport, entry.session))
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

fn internal(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("internal".into()),
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
        let transport = Transport::new(crate::test_http_client());
        map.insert("a".into(), transport, "s1".into());
        let (bound, session) = map.get("a").expect("应命中");
        assert!(Arc::ptr_eq(&bound, &map.get("a").unwrap().0));
        assert_eq!(session, "s1");
        assert_eq!(map.len(), 1);
        let (_, session) = map.remove("a").expect("应可取回");
        assert_eq!(session, "s1");
        assert!(map.get("a").is_none());
        assert_eq!(map.len(), 0);
    }

    #[tokio::test]
    async fn read_close_unknown_session_shape() {
        let map = SessionMap::new();
        let err = read(&map, &json!({"id": "nope"})).await.unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
        // close 幂等：未知 id 也返回 null
        assert_eq!(
            close(&map, &json!({"id": "nope"})).await.unwrap(),
            Value::Null
        );
        // 缺 id：bad_request
        assert_eq!(
            read(&map, &json!({})).await.unwrap_err().code.as_deref(),
            Some("bad_request")
        );
    }

    // ── 代理解析（wire 形状 / URL / 环回让位 / 拒绝路径）────────────

    fn target(url: &str) -> url::Url {
        url::Url::parse(url).unwrap()
    }

    #[test]
    fn proxy_endpoint_parses_wire_object() {
        let endpoint = proxy_endpoint(
            &json!({"proxy": {
                "protocol": "http", "host": "127.0.0.1", "port": 8080,
                "username": "u", "password": "p"
            }}),
            &target("https://example.invalid/x"),
        )
        .unwrap()
        .expect("应解析出代理");
        assert_eq!(endpoint.protocol, ProxyProtocol::Http);
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 8080);
        assert_eq!(endpoint.username, "u");
        assert_eq!(endpoint.password, "p");
        // reqwest/socks 共用的 URL 形态（凭据已转义）
        assert_eq!(endpoint.url().unwrap(), "http://u:p@127.0.0.1:8080/");
    }

    #[test]
    fn proxy_endpoint_accepts_url_string_and_socks_defaults() {
        let endpoint = proxy_endpoint(
            &json!({"proxy": "socks5h://h.example:1080"}),
            &target("https://example.invalid/x"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(endpoint.protocol, ProxyProtocol::Socks5h);
        assert_eq!(endpoint.port, 1080);
        // socks 无端口 → 1080 缺省
        let endpoint = proxy_endpoint(
            &json!({"proxy": "socks5://h.example"}),
            &target("https://example.invalid/x"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(endpoint.port, 1080);
    }

    #[test]
    fn proxy_endpoint_null_and_loopback_bypass() {
        // null/缺省 = 直连
        assert!(
            proxy_endpoint(&json!({}), &target("https://example.invalid/x"))
                .unwrap()
                .is_none()
        );
        assert!(proxy_endpoint(
            &json!({"proxy": null}),
            &target("https://example.invalid/x")
        )
        .unwrap()
        .is_none());
        // 本机环回目标：即使配了代理也直连（上游 model 同口径）
        for url in [
            "https://localhost/x",
            "http://127.0.0.1:9000/x",
            "http://[::1]:9000/x",
        ] {
            assert!(
                proxy_endpoint(
                    &json!({"proxy": {"protocol": "http", "host": "p", "port": 1}}),
                    &target(url)
                )
                .unwrap()
                .is_none(),
                "{url} 应绕过代理"
            );
        }
    }

    #[test]
    fn proxy_endpoint_rejects_unsupported_and_malformed() {
        // 不支持的协议：明确报错（绝不静默直连）
        let err = proxy_endpoint(
            &json!({"proxy": {"protocol": "ftp", "host": "h", "port": 21}}),
            &target("https://example.invalid/x"),
        )
        .unwrap_err();
        assert!(err.contains("unsupported proxy protocol"), "got: {err}");
        // 缺 host / 非法端口
        assert!(proxy_endpoint(
            &json!({"proxy": {"protocol": "http", "port": 8080}}),
            &target("https://example.invalid/x")
        )
        .is_err());
        assert!(proxy_endpoint(
            &json!({"proxy": {"protocol": "http", "host": "h", "port": 0}}),
            &target("https://example.invalid/x")
        )
        .is_err());
        // 形状不对
        assert!(
            proxy_endpoint(&json!({"proxy": 7}), &target("https://example.invalid/x")).is_err()
        );
    }

    #[test]
    fn proxy_transports_are_keyed_by_endpoint() {
        // reqwest `rustls-no-provider` 需要进程级 crypto provider（生产由
        // main.rs 安装；测试进程用同一个 Once 安装点）。
        crate::install_rustls_provider_for_tests();
        let pool = ProxyTransports::new();
        let a = ProxyEndpoint {
            protocol: ProxyProtocol::Http,
            host: "127.0.0.1".into(),
            port: 8080,
            username: String::new(),
            password: String::new(),
        };
        let b = ProxyEndpoint {
            port: 8081,
            ..a.clone()
        };
        let ta = pool.get_or_create(&a).unwrap();
        let ta2 = pool.get_or_create(&a).unwrap();
        let tb = pool.get_or_create(&b).unwrap();
        assert!(Arc::ptr_eq(&ta, &ta2), "同代理必须复用 Transport");
        assert!(!Arc::ptr_eq(&ta, &tb), "不同代理必须隔离");
    }
}
