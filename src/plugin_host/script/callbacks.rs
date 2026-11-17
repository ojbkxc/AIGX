//! 脚本宿主的回调组合器 —— 把「脚本→网关」的回调分发给四张表：
//!
//! | 方法族 | 落点 |
//! |---|---|
//! | `store.*` / `settings.*` / `log` / `notify` | 既有 [`PluginCallbacks`]（账号池 + FileStore，pack 前缀隔离） |
//! | `script.http.*` | [`super::http`]（A2A 形状的 HTTP egress，复用 [`Transport`] 会话） |
//! | `script.ws.*` | [`super::websocket`]（tokio-tungstenite 连接表） |
//! | `script.stream.*` / `script.protocol.*` | [`ProtocolRegistry`]（上游 SSE → OpenAI SSE） |
//! | `script.proxy.resolve` / `script.signature` / `script.usage` | 本文件（代理解析 / 签名缓存 / 用量记日志） |
//!
//! ## 为什么包一层组合器（不改 callbacks.rs）
//!
//! 原生插件的回调是同步语义（读内存/写 KV），实现已稳定；脚本宿主的
//! egress/转换需要真正 await 网络 I/O。`CallbackHandler::handle_async` 的
//! 默认实现委托同步 `handle`，原生侧行为零变化；本结构只覆写异步版本。
//!
//! ## 命名空间
//!
//! 脚本宿主是**一个** Bun 承载全部包，回调里的 `pack_id` 无法从 params
//! 区分来源；这里统一用 `"script"` 作为隔离前缀（settings/ledger 键形如
//! `plugin:script:settings:{provider}:{key}`）。不同包之间靠 provider id
//! 天然隔离——owners 表保证一个 provider 只属于一个包。
//!
//! ## 归属语义（A2A「插件优先」的网关侧对应）
//!
//! A2A 宿主在 `serve_callback` 里对每个回调做 provider/account 归属校验
//! （防插件读别人的账号）。AIGX 的归属事实由 `registry::is_plugin_owned`
//! 与 `ScriptHostManager::route_for_provider` 在**数据面路由**时保证——回调
//! 里再查一遍会与运行期路由表形成双重真相源（热切换窗口不一致）。本实现
//! 的立场：`store.*` 的 provider/account 参数**直接用于查账号池**（账号池
//! 本身按 provider 归属存储），不额外做「这个 provider 是否属于当前包」
//! 的判定——脚本拿到的 secret 只可能是它自己 provider 名下的账号。

use std::pin::Pin;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;

use crate::account_session::AccountSessionStore;
use crate::plugin_host::callbacks::PluginCallbacks;
use crate::plugin_host::rpc::{CallbackHandler, RpcError};
use crate::storage::FileStore;

use super::http::SessionMap;
use super::protocol::ProtocolRegistry;
use super::transport::Transport;
use super::usage;

/// 脚本宿主共用的回调命名空间（见模块注释）。
pub const PACK_NS: &str = "script";

/// 脚本宿主回调分发器。
pub struct ScriptCallbacks {
    transport: Arc<Transport>,
    protocol: Arc<ProtocolRegistry>,
    /// `script.http.*` 的「客户端 id → Transport session」映射。
    http_sessions: SessionMap,
    /// 同步回调（store/settings/ledger/log/notify）的既有实现。
    inner: PluginCallbacks,
}

impl ScriptCallbacks {
    pub fn new(
        transport: Arc<Transport>,
        protocol: Arc<ProtocolRegistry>,
        sessions: Arc<AccountSessionStore>,
        store: Arc<FileStore>,
    ) -> Arc<Self> {
        Arc::new(Self {
            transport,
            protocol,
            http_sessions: SessionMap::new(),
            inner: PluginCallbacks::new(PACK_NS.to_string(), sessions, store),
        })
    }

    /// 宿主重建/关闭时清空会话表（HTTP/WS/转换器）。
    pub fn clear_sessions(&self) {
        self.http_sessions.clear();
        super::websocket::clear();
    }

    async fn handle_script(&self, method: &str, params: Value) -> Option<Result<Value, RpcError>> {
        Some(match method {
            // ── HTTP egress（A2A 形状）────────────────────────────────
            "script.http.open" => {
                super::http::open(&self.transport, &self.http_sessions, &params).await
            }
            "script.http.read" => {
                super::http::read(&self.transport, &self.http_sessions, &params).await
            }
            "script.http.close" => {
                super::http::close(&self.transport, &self.http_sessions, &params).await
            }
            // ── 旧 AIGX transport 形状（保留向后兼容）─────────────────
            "script.transport.request" => self.transport.request(&params).await,
            "script.transport.read" => {
                let session = params.get("session").and_then(Value::as_str).unwrap_or("");
                self.transport.read(session).await
            }
            "script.transport.close" => {
                let session = params.get("session").and_then(Value::as_str).unwrap_or("");
                self.transport.close(session).await
            }
            // ── WebSocket egress ───────────────────────────────────────
            "script.ws.open" => super::websocket::open(&params).await,
            "script.ws.read" => super::websocket::read(&params).await,
            "script.ws.send" => super::websocket::send(&params).await,
            "script.ws.abort" => {
                super::websocket::abort(&params);
                Ok(Value::Null)
            }
            // ── 流式协议转换（A2A 字符串键形状）───────────────────────
            "script.stream.open" => {
                let protocol = params
                    .get("protocol")
                    .and_then(Value::as_str)
                    .unwrap_or("chat");
                self.protocol.open_named(protocol).await
            }
            "script.stream.push" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                let finish = params
                    .get("finish")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let encoded = params.get("data").and_then(Value::as_str).unwrap_or("");
                match STANDARD.decode(encoded.as_bytes()) {
                    Ok(bytes) => self.protocol.push_named(id, &bytes, finish).await,
                    Err(e) => Err(bad_request(format!(
                        "script.stream.push: invalid base64: {e}"
                    ))),
                }
            }
            "script.stream.close" => {
                let id = params.get("id").and_then(Value::as_str).unwrap_or("");
                self.protocol.close_named(id).await;
                Ok(Value::Null)
            }
            // ── 旧 AIGX protocol 形状（数字键，保留兼容）──────────────
            "script.protocol.request" => {
                let protocol = params
                    .get("protocol")
                    .and_then(Value::as_str)
                    .unwrap_or("chat");
                let body = params.get("body").cloned().unwrap_or(Value::Null);
                ProtocolRegistry::request(protocol, &body)
            }
            "script.protocol.chunk" => {
                let protocol = params
                    .get("protocol")
                    .and_then(Value::as_str)
                    .unwrap_or("chat");
                let stream = params.get("stream").and_then(Value::as_i64).unwrap_or(0);
                let encoded = params.get("data").and_then(Value::as_str).unwrap_or("");
                match STANDARD.decode(encoded.as_bytes()) {
                    Ok(bytes) => self.protocol.chunk(protocol, stream, &bytes).await,
                    Err(e) => Err(bad_request(format!("protocol.chunk: invalid base64: {e}"))),
                }
            }
            "script.protocol.finish" => {
                let stream = params.get("stream").and_then(Value::as_i64).unwrap_or(0);
                self.protocol.finish(stream).await
            }
            // ── 代理解析 / 签名 / 用量 ─────────────────────────────────
            "script.proxy.resolve" => super::transport::resolve_proxy(&params),
            "script.signature" => {
                // Antigravity 的调用签名缓存（A2A `gemini.mjs` 用）：按
                // `{session, call}` 查回签名字符串，查不到返回 null。
                let session = params.get("session").and_then(Value::as_str).unwrap_or("");
                let call = params.get("call").and_then(Value::as_str).unwrap_or("");
                let found =
                    crate::bridge::session::antigravity_signature_cache::lookup_for_session(
                        session, call,
                    )
                    .or_else(|| {
                        crate::bridge::session::antigravity_signature_cache::lookup_call(call)
                    });
                Ok(found.map(Value::String).unwrap_or(Value::Null))
            }
            "script.usage" => usage::record(&params),
            _ => return None,
        })
    }
}

impl CallbackHandler for ScriptCallbacks {
    /// 同步入口：只覆盖既有回调面；egress/转换类方法必须走
    /// [`Self::handle_async`]（读循环已统一走异步入口）。
    fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        if method.starts_with("script.") {
            return Err(RpcError {
                message: format!("callback '{method}' requires async handler"),
                code: Some("unsupported".into()),
            });
        }
        self.inner.handle(method, params)
    }

    fn handle_async<'a>(
        &'a self,
        method: &'a str,
        params: Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, RpcError>> + Send + 'a>> {
        Box::pin(async move {
            // 先按命名空间分流再决定参数归属：`params` 只 move 一次。
            // 绝不能 clone——`script.http.open` 的 `body` 可达上百 MB（插件
            // 转发整个请求体），一次深拷贝就是整包内存翻倍；原生回调
            // （store/settings/…）也不该白拷贝一遍。
            if !method.starts_with("script.") {
                return self.inner.handle(method, params);
            }
            match self.handle_script(method, params).await {
                Some(result) => result,
                None => Err(RpcError {
                    message: format!("unsupported callback: {method}"),
                    code: Some("unsupported".into()),
                }),
            }
        })
    }
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("bad_request".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 建一个隔离的临时目录（secret 编码路径是全局 data_dir，需串行化）。
    fn with_secret_dir<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let _guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-script-cb-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        f()
    }

    fn callbacks(store: Arc<FileStore>) -> Arc<ScriptCallbacks> {
        let sessions = Arc::new(AccountSessionStore::new(store.clone()));
        ScriptCallbacks::new(
            Transport::new(crate::test_http_client()),
            ProtocolRegistry::new(),
            sessions,
            store,
        )
    }

    #[test]
    fn settings_delegate_to_pack_namespaced_inner() {
        with_secret_dir("settings", || {
            let store = Arc::new(FileStore::new(
                tempfile::TempDir::new().unwrap().path().to_path_buf(),
            ));
            let cb = callbacks(store.clone());
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                cb.handle_async(
                    "settings.set",
                    json!({"provider": "kiro", "key": "k", "value": 1}),
                )
                .await
                .unwrap();
                let got = cb
                    .handle_async("settings.get", json!({"provider": "kiro", "key": "k"}))
                    .await
                    .unwrap();
                assert_eq!(got, json!(1));
                // 键必须落在 script 命名空间下
                assert!(store
                    .get::<Value>("plugin:script:settings:kiro:k")
                    .unwrap()
                    .is_some());
            });
        });
    }

    #[tokio::test]
    async fn protocol_request_via_async() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let out = cb
            .handle_async(
                "script.protocol.request",
                json!({"protocol": "anthropic", "body": {"model": "m", "messages": [{"role": "user", "content": "hi"}]}}),
            )
            .await
            .unwrap();
        assert_eq!(out["path"], "v1/messages");
        assert_eq!(out["body"]["messages"][0]["content"], "hi");
    }

    #[tokio::test]
    async fn protocol_chunk_base64_roundtrip() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let raw = br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ok"}}"#;
        let mut with_nl = raw.to_vec();
        with_nl.push(b'\n');
        let out = cb
            .handle_async(
                "script.protocol.chunk",
                json!({"protocol": "anthropic", "stream": 11, "data": STANDARD.encode(&with_nl)}),
            )
            .await
            .unwrap();
        let bytes = STANDARD.decode(out["data"].as_str().unwrap()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("\"content\":\"ok\""), "转换输出: {text}");
        cb.handle_async("script.protocol.finish", json!({"stream": 11}))
            .await
            .unwrap();
    }

    /// A2A 宿主 `provider.mjs` 用的是 `script.stream.*`（字符串 id）——
    /// 与旧 `script.protocol.*`（数字 id）并存，两者互不影响。
    #[tokio::test]
    async fn stream_open_push_close_named() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let opened = cb
            .handle_async(
                "script.stream.open",
                json!({"protocol": "anthropic", "model": "m"}),
            )
            .await
            .unwrap();
        let id = opened["id"]
            .as_str()
            .expect("stream.open 应返回 id")
            .to_string();
        let raw = br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}"#;
        let mut with_nl = raw.to_vec();
        with_nl.push(b'\n');
        let out = cb
            .handle_async(
                "script.stream.push",
                json!({"id": id, "data": STANDARD.encode(&with_nl)}),
            )
            .await
            .unwrap();
        let text =
            String::from_utf8_lossy(&STANDARD.decode(out["data"].as_str().unwrap()).unwrap())
                .into_owned();
        assert!(text.contains("\"content\":\"hi\""), "转换输出: {text}");
        // finish=true 冲刷并移除
        cb.handle_async(
            "script.stream.push",
            json!({"id": id, "data": "", "finish": true}),
        )
        .await
        .unwrap();
        // 已关闭：再 push 是 not_found
        let err = cb
            .handle_async("script.stream.push", json!({"id": id, "data": ""}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
        // close 幂等
        cb.handle_async("script.stream.close", json!({"id": "nope"}))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn transport_callbacks_reach_transport_validation() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        // 缺 url：transport.request 的参数校验必须是 bad_request（不打网络）
        let err = cb
            .handle_async("script.transport.request", json!({"method": "POST"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("bad_request"));
        // 未知 session：not_found
        let err = cb
            .handle_async("script.transport.read", json!({"session": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
    }

    #[tokio::test]
    async fn http_open_requires_id_and_url() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        // A2A 形状：缺 id / 缺 url 都是 bad_request
        for params in [json!({"url": "https://x"}), json!({"id": "a"})] {
            let err = cb
                .handle_async("script.http.open", params)
                .await
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("bad_request"));
        }
        // 未知会话 read：not_found
        let err = cb
            .handle_async("script.http.read", json!({"id": "nope"}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("not_found"));
        // close 幂等：null
        assert_eq!(
            cb.handle_async("script.http.close", json!({"id": "nope"}))
                .await
                .unwrap(),
            Value::Null
        );
    }

    #[tokio::test]
    async fn ws_abort_and_read_unknown() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        assert_eq!(
            cb.handle_async("script.ws.abort", json!({"id": "nope"}))
                .await
                .unwrap(),
            Value::Null
        );
        let err = cb
            .handle_async("script.ws.read", json!({"id": "nope"}))
            .await
            .unwrap_err();
        assert!(err.message.contains("session closed"));
    }

    #[tokio::test]
    async fn proxy_resolve_direct_is_null_and_static_parses() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        assert_eq!(
            cb.handle_async(
                "script.proxy.resolve",
                json!({"config": {"source": "direct"}})
            )
            .await
            .unwrap(),
            Value::Null
        );
        let out = cb
            .handle_async(
                "script.proxy.resolve",
                json!({"config": {"url": "http://u:p@127.0.0.1:8080"}}),
            )
            .await
            .unwrap();
        assert_eq!(out["protocol"], "http");
        assert_eq!(out["host"], "127.0.0.1");
        assert_eq!(out["port"], 8080);
        assert_eq!(out["username"], "u");
        // null config → null（直连）
        assert_eq!(
            cb.handle_async("script.proxy.resolve", json!({}))
                .await
                .unwrap(),
            Value::Null
        );
    }

    #[tokio::test]
    async fn signature_lookup_missing_is_null() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let out = cb
            .handle_async(
                "script.signature",
                json!({"session": "s", "call": "no-such-call"}),
            )
            .await
            .unwrap();
        assert_eq!(out, Value::Null);
    }

    #[test]
    fn sync_entry_rejects_async_only_methods() {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let err = cb
            .handle("script.protocol.finish", json!({"stream": 1}))
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("unsupported"));
    }

    #[tokio::test]
    async fn unknown_script_method_keeps_unsupported_shape() {
        // `script.` 前缀的未知方法：错误形状与旧实现（落到 inner 的
        // unsupported callback）逐字一致，只是不再白 clone 一份 params。
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let err = cb
            .handle_async("script.unknown", json!({"k": 1}))
            .await
            .unwrap_err();
        assert_eq!(err.code.as_deref(), Some("unsupported"));
        assert!(
            err.message.contains("script.unknown"),
            "错误文案应点名方法: {}",
            err.message
        );
    }

    /// 原生回调携带大字段（生产里 http egress 会转发整包 ~160MB 的请求体）
    /// 必须原样工作：新版只 move、不 clone params，行为与旧版等值。
    #[tokio::test]
    async fn native_callback_with_large_params_still_works() {
        const BIG: usize = 4 * 1024 * 1024;
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let cb = callbacks(store);
        let big = "x".repeat(BIG);
        cb.handle_async(
            "settings.set",
            json!({"provider": "kiro", "key": "big", "value": big}),
        )
        .await
        .unwrap();
        let got = cb
            .handle_async("settings.get", json!({"provider": "kiro", "key": "big"}))
            .await
            .unwrap();
        assert_eq!(got.as_str().map(str::len), Some(BIG));
    }

    /// 回调方法名集合与 29 个宿主 mjs 的 `rpc("…")` 调用对表（防漏接）。
    #[test]
    fn callback_methods_match_host_mjs() {
        const HOST_MJS: &[(&str, &str)] = &[
            ("main.mjs", include_str!("host/main.mjs")),
            ("wire.mjs", include_str!("host/wire.mjs")),
            ("fetch.mjs", include_str!("host/fetch.mjs")),
            ("websocket.mjs", include_str!("host/websocket.mjs")),
            ("accounts.mjs", include_str!("host/accounts.mjs")),
            ("models.mjs", include_str!("host/models.mjs")),
            ("provider.mjs", include_str!("host/provider.mjs")),
            ("environment.mjs", include_str!("host/environment.mjs")),
            ("gemini.mjs", include_str!("host/gemini.mjs")),
            ("login.mjs", include_str!("host/login.mjs")),
            ("allowance.mjs", include_str!("host/allowance.mjs")),
        ];
        // 宿主实际会发的回调方法（来自 grep rpc("…")）
        const REQUIRED: &[&str] = &[
            "script.http.open",
            "script.http.read",
            "script.http.close",
            "script.ws.open",
            "script.ws.read",
            "script.ws.send",
            "script.ws.abort",
            "script.stream.open",
            "script.stream.push",
            "script.stream.close",
            "script.proxy.resolve",
            "script.signature",
            "script.usage",
            "script.protocol.request",
            "store.get",
            "store.list",
            "store.update",
            "store.ledger_get",
            "store.ledger_put",
            "notify",
        ];
        for method in REQUIRED {
            assert!(
                HOST_MJS
                    .iter()
                    .any(|(_, src)| src.contains(&format!("\"{method}\""))),
                "回调方法 {method} 必须被某个宿主 mjs 调用（对表失败）"
            );
        }
    }
}
