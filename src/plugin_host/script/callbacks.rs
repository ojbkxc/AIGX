//! 脚本宿主的回调组合器 —— 把「脚本→网关」的回调分发给三张表：
//!
//! | 方法族 | 落点 |
//! |---|---|
//! | `store.*` / `settings.*` / `log` / `notify` | 既有 [`PluginCallbacks`]（账号池 + FileStore，pack 前缀隔离） |
//! | `script.transport.*` | [`Transport`]（HTTP egress 代理，流式会话） |
//! | `script.protocol.*` | [`ProtocolRegistry`]（上游 SSE → OpenAI SSE 增量转换） |
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

use std::pin::Pin;
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;

use crate::account_session::AccountSessionStore;
use crate::plugin_host::callbacks::PluginCallbacks;
use crate::plugin_host::rpc::{CallbackHandler, RpcError};
use crate::storage::FileStore;

use super::protocol::ProtocolRegistry;
use super::transport::Transport;

/// 脚本宿主共用的回调命名空间（见模块注释）。
pub const PACK_NS: &str = "script";

/// 脚本宿主回调分发器。
pub struct ScriptCallbacks {
    transport: Arc<Transport>,
    protocol: Arc<ProtocolRegistry>,
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
            inner: PluginCallbacks::new(PACK_NS.to_string(), sessions, store),
        })
    }

    async fn handle_script(&self, method: &str, params: Value) -> Option<Result<Value, RpcError>> {
        Some(match method {
            "script.transport.request" => self.transport.request(&params).await,
            "script.transport.read" => {
                let session = params.get("session").and_then(Value::as_str).unwrap_or("");
                self.transport.read(session).await
            }
            "script.transport.close" => {
                let session = params.get("session").and_then(Value::as_str).unwrap_or("");
                self.transport.close(session).await
            }
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
                    Err(e) => Err(RpcError {
                        message: format!("protocol.chunk: invalid base64: {e}"),
                        code: Some("bad_request".into()),
                    }),
                }
            }
            "script.protocol.finish" => {
                let stream = params.get("stream").and_then(Value::as_i64).unwrap_or(0);
                self.protocol.finish(stream).await
            }
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
            // 绝不能 clone——`script.transport.request` 的 `body_b64` 可达
            // 上百 MB（插件转发整个请求体），一次深拷贝就是整包内存翻倍；
            // 原生回调（store/settings/…）也不该白拷贝一遍。
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

    /// 原生回调携带大字段（生产里 transport 会转发整包 ~160MB 的请求体）
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
}
