// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 行为探针 —— 端到端验证「沙箱能装载 / 能调用 / 能关停」。
//!
//! 用 [`super::fixture`] 手写的最小 Wasm 模块真跑 wasmtime：装载 → init
//! 握手 → 调用 → 关停。**这是唯一能证明沙箱可用的证据**，纯逻辑测（manifest
//! 解析、public_view 校验等）不覆盖这条链。
//!
//! 夹具的能力边界（如实）：它对**任何**方法都返回同一段 `init` 应答，所以
//! 只能验证「装载 + 握手 + 一次调用 + 关停」，不能模拟真实 `conversation`
//! SSE 或 `tasks.list` 数组（那需要在 wasm 里按方法名分支，超出最小夹具）。

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{json, Value};

    use super::super::access::{WasmAccountAccess, WasmEgress};
    use super::super::fixture::minimal_wasm_module;
    use super::super::wire::WireError;
    use super::super::{host_for_provider, WasmOptions};
    use crate::account_session::model::AccountSession;
    use crate::account_session::AccountSessionStore;
    use crate::plugin_host::manager::PluginHostManager;
    use crate::plugin_host::process::HostEnv;
    use crate::plugin_host::registry::PackRegistry;
    use crate::plugin_host::wasm::access::WasmStreamSink;
    use crate::storage::FileStore;

    /// 本模块所有会 `start()` 的用例共享一把串行锁。
    ///
    /// `install_accounts` 是进程级「首次注入保留」单例（`mod.rs` 语义），
    /// 并行测试下「谁先 start 谁占坑」会让依赖真实账号面的用例顺序敏感。
    /// 串行化后，每个需要特定账号面的用例先 `reset_accounts()` 再 `start()`，
    /// 结果与执行顺序无关。
    fn wasm_test_lock() -> &'static tokio::sync::Mutex<()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        &LOCK
    }

    /// 复位进程级账号访问面（仅测试；生产语义「首次注入保留」不变）。
    fn reset_accounts() {
        if let Ok(mut slot) = super::super::accounts().lock() {
            *slot = None;
        }
    }

    /// 最小注入面：所有能力都返回「未就绪」。
    ///
    /// 探针只跑 `init` 握手与一次无能力的调用 —— 不需要账号/出站。
    struct NullAccess;

    #[async_trait::async_trait]
    impl WasmAccountAccess for NullAccess {
        fn provider_allowed(&self, _provider: &str) -> bool {
            false
        }
        fn account_provider(&self, _account: &str) -> Option<String> {
            None
        }
        async fn store_get(&self, _provider: &str, _account: &str) -> Result<Value, WireError> {
            Err(WireError::internal("no accounts in probe"))
        }
        async fn store_update_with_public(
            &self,
            _account: &str,
            _expect: Value,
            _secret: Value,
            _public: Value,
        ) -> Result<Value, WireError> {
            Err(WireError::internal("no accounts in probe"))
        }
        async fn ledger_get(&self, _account: &str, _kind: &str) -> Result<Value, WireError> {
            Ok(Value::Null)
        }
        async fn ledger_put(
            &self,
            _account: &str,
            _kind: &str,
            _value: Value,
        ) -> Result<(), WireError> {
            Ok(())
        }
        async fn settings_get(&self, _provider: &str, _key: &str) -> Result<Value, WireError> {
            Ok(Value::Null)
        }
        async fn plugin_state(
            &self,
            _provider: &str,
            _account: &str,
            _key: &str,
        ) -> Result<Value, WireError> {
            Ok(json!({ "revision": 0, "schema": 1, "value": null }))
        }
        async fn plugin_state_compare_exchange(
            &self,
            _provider: &str,
            _account: &str,
            _key: &str,
            _revision: u64,
            _schema: u32,
            _value: Value,
        ) -> Result<Value, WireError> {
            Ok(json!({ "updated": false, "revision": 0 }))
        }
        fn accounts_for_provider(&self, _provider: &str) -> Vec<Value> {
            Vec::new()
        }
    }

    /// 出站面：探针不发出站请求，全部返回「不支持」。
    struct NullEgress;

    #[async_trait::async_trait]
    impl WasmEgress for NullEgress {
        async fn request(&self, _params: Value) -> Result<Value, WireError> {
            Err(WireError::with_code("probe", "unsupported"))
        }
        async fn read(&self, _session: &str) -> Result<Value, WireError> {
            Err(WireError::with_code("probe", "unsupported"))
        }
        async fn close(&self, _session: &str) -> Result<Value, WireError> {
            Ok(Value::Null)
        }
        async fn ws_open(&self, _params: Value) -> Result<Value, WireError> {
            Err(WireError::with_code("probe", "unsupported"))
        }
        async fn ws_read(&self, _params: Value) -> Result<Value, WireError> {
            Err(WireError::with_code("probe", "unsupported"))
        }
        async fn ws_send(&self, _params: Value) -> Result<Value, WireError> {
            Err(WireError::with_code("probe", "unsupported"))
        }
        async fn ws_close(&self, _params: Value) -> Result<Value, WireError> {
            Ok(Value::Null)
        }
        fn resolve_proxy(&self, _params: &Value) -> Result<Value, WireError> {
            Ok(Value::Null)
        }
    }

    /// 落一个可装载的 wasm 包（`pack.json` + `wasm.json` + `.wasm`），返回包目录。
    fn write_probe_pack(dir: &std::path::Path, provider: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("pack.json"),
            json!({
                "id": "probe-pack",
                "version": "0.0.1",
                "protocol": crate::plugin_host::rpc::PROTOCOL_VERSION,
                "entry": { super::super::TARGET: "probe.wasm" },
                "providers": [{ "id": provider, "label": "Probe" }],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("wasm.json"),
            json!({ "abi": super::super::ABI, "requires": [], "origins": [] }).to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("probe.wasm"),
            minimal_wasm_module(provider, "Probe"),
        )
        .unwrap();
    }

    /// **探针 1**：装载 → `host_for_provider` 可见 → 关停后不可见。
    #[tokio::test]
    async fn probe_1_load_call_and_shutdown() {
        let _guard = wasm_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let provider = "probe-provider-1";
        write_probe_pack(tmp.path(), provider);
        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        let options = WasmOptions::from_pack(
            tmp.path(),
            &pack,
            Arc::new(NullAccess),
            Arc::new(NullEgress),
        )
        .expect("from_pack")
        .expect("is wasm pack");
        let host = super::super::start(options).await.expect("start");

        // 装载后：注册表可见 + provider 身份与清单一致（start 内部已校验）
        assert_eq!(host.providers()[0].id, provider);
        assert!(
            host_for_provider(provider).is_some(),
            "start 后 host_for_provider 必须可见"
        );
        assert!(super::super::owns(provider));

        // 夹具对任何方法返回同一段 init 应答 —— 这足以证明「实例化 + 调用 +
        // 宿主 ABI 三个函数都工作」（`a2a_call` 的返回值被正确解包）。
        let reply = host
            .call(
                "tasks.list",
                json!({ "provider": provider }),
                Duration::from_secs(5),
            )
            .await
            .expect("call");
        assert_eq!(reply["providers"][0]["id"], provider);

        // 关停后：注册表与清单都摘除
        super::super::stop(provider).await;
        assert!(
            host_for_provider(provider).is_none(),
            "stop 后 host_for_provider 必须 None"
        );
        assert!(!super::super::owns(provider), "stop 后 owns 必须 false");
    }

    /// **探针 1 的反面对照**：清单校验失败的包不会被装载。
    ///
    /// 与探针 1 配对，证明「装载成功」不是因为校验形同虚设。
    #[tokio::test]
    async fn probe_1_negative_invalid_manifest_rejected() {
        let _guard = wasm_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        write_probe_pack(tmp.path(), "probe-provider-2");
        // ABI 不匹配
        std::fs::write(
            tmp.path().join("wasm.json"),
            json!({ "abi": 999, "requires": [], "origins": [] }).to_string(),
        )
        .unwrap();
        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        let result = WasmOptions::from_pack(
            tmp.path(),
            &pack,
            Arc::new(NullAccess),
            Arc::new(NullEgress),
        );
        assert!(result.is_err(), "ABI 不匹配必须拒绝: {:?}", result.err());
        assert!(host_for_provider("probe-provider-2").is_none());
    }

    /// 宿主注册表在 wasm 加载失败时不留残留（`start` 中途失败的清理语义）。
    #[tokio::test]
    async fn probe_1_failed_start_leaves_no_registration() {
        let _guard = wasm_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let provider = "probe-provider-3";
        write_probe_pack(tmp.path(), provider);
        // 把 .wasm 改成非 wasm 字节：编译失败
        std::fs::write(tmp.path().join("probe.wasm"), b"\0asm\x01\0\0\0garbage").unwrap();
        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        let options = WasmOptions::from_pack(
            tmp.path(),
            &pack,
            Arc::new(NullAccess),
            Arc::new(NullEgress),
        )
        .expect("from_pack ok")
        .expect("is wasm");
        assert!(
            super::super::start(options).await.is_err(),
            "坏模块必须失败"
        );
        assert!(host_for_provider(provider).is_none(), "失败不留注册");
        assert!(!super::super::owns(provider), "失败不留清单");
    }

    /// **探针 2（数据面路由）**：wasm provider 的 bridge 能产出 `Wasm` 后端，
    /// 且 `open_stream` 之后事件流能被消费。
    ///
    /// `resolve.rs` 的分支本身依赖完整 `AppState`（63 字段，只在 main.rs 构造
    /// 一次，测试里没有构造器）；所以这里钉的是**分支所依赖的那一环**：
    /// `plugin_host::wasm::host_for_provider` → `PluginBridge::new_wasm` →
    /// `ConversationStream::Wasm`。`resolve.rs` 里那 6 行是纯 match 分支，
    /// 由 `cargo check` 保证形状正确（无法在单测里跑：见报告「无法验证项」）。
    #[tokio::test]
    async fn probe_2_bridge_routes_to_wasm_backend() {
        let _guard = wasm_test_lock().lock().await;
        use std::time::Duration;

        use crate::bridge::plugin::{ConversationStream, PluginBridge};
        use crate::bridge::{ChatFormat, ChatMessage};

        let tmp = tempfile::tempdir().unwrap();
        let provider = "probe-provider-bridge";
        write_probe_pack(tmp.path(), provider);
        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        let options = WasmOptions::from_pack(
            tmp.path(),
            &pack,
            Arc::new(NullAccess),
            Arc::new(NullEgress),
        )
        .unwrap()
        .unwrap();
        let host = super::super::start(options).await.expect("start");

        // 与 resolve.rs 的分支同形：拿到 host 就构造 bridge
        let bridge = PluginBridge::new_wasm(host.clone(), provider.to_string());
        let req = ChatFormat {
            model: "probe-model".into(),
            messages: vec![ChatMessage::assistant("hi")],
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        // 夹具对 conversation 返回的是 init 应答（不是 SSE），但桥的关键
        // **路由形态**可验：`ConversationStream::Wasm` 被构造出来，且能读出
        // 至少一个事件（夹具的 JSON 字节会作为 Chunk 下发）。
        let mut stream = bridge
            .open_conversation_for_test(&req)
            .await
            .expect("open_conversation");
        assert!(
            matches!(stream, ConversationStream::Wasm { .. }),
            "wasm provider 必须产出 Wasm 分支"
        );
        let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("首个事件不能超时");
        assert!(first.is_some(), "夹具应答应作为事件下发: {first:?}");

        super::super::stop(provider).await;
    }

    /// **探针 3（流错误传播）**：沙箱以 `WireError` 终结时，消费方必须拿到
    /// `Some(Err(..))` 而不是 `None`（这正是 T3 里的静默吞错缺陷）。
    #[tokio::test]
    async fn probe_3_handshake_error_is_not_silent() {
        let _guard = wasm_test_lock().lock().await;
        let tmp = tempfile::tempdir().unwrap();
        let provider = "probe-provider-error";
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("pack.json"),
            json!({
                "id": "probe-pack",
                "entry": { super::super::TARGET: "probe.wasm" },
                "providers": [{ "id": provider, "label": "Probe" }],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("wasm.json"),
            json!({ "abi": super::super::ABI, "requires": [], "origins": [] }).to_string(),
        )
        .unwrap();
        // 应答是 error 信封：`unwrap_reply` 会把它解码成 WireError
        std::fs::write(
            tmp.path().join("probe.wasm"),
            super::super::fixture::module_with_response(
                &json!({ "error": { "message": "上游 401", "code": "auth" } }).to_string(),
            ),
        )
        .unwrap();

        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        let options = WasmOptions::from_pack(
            tmp.path(),
            &pack,
            Arc::new(NullAccess),
            Arc::new(NullEgress),
        )
        .unwrap()
        .unwrap();
        // 握手拿到的应答是 error 信封 -> start 必须报错且**带上插件自报的文案**，
        // 不能只说「握手失败」（那就是又一层吞错）。
        let error = match super::super::start(options).await {
            Ok(_) => panic!("error 信封的握手必须失败"),
            Err(error) => error,
        };
        assert!(
            error.contains("401"),
            "错误文案必须透出插件自报的 message，实际: {error}"
        );
        assert!(host_for_provider(provider).is_none(), "失败不留注册");
    }

    /// **探针 3（直测）**：`ChannelSink::fail` → `take_error` 语义。
    #[test]
    fn probe_3_sink_error_surfaces() {
        use crate::plugin_host::wasm::ChannelSink;

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = ChannelSink::new(tx);
        assert!(sink.take_error().is_none(), "未见错误前必须 None");
        sink.fail(super::super::wire::WireError::with_code("上游 401", "auth"));
        let error = sink.take_error().expect("错误必须可取");
        assert_eq!(error.message, "上游 401");
        assert_eq!(error.code.as_deref(), Some("auth"));
        assert!(sink.take_error().is_none(), "错误只交付一次");
    }

    /// **探针 5（任务调度）**：`tasks::load` 解析声明进表 → `run_with` 真跑
    /// 「账号枚举 + 租约 + 宿主 `tasks.run` 调用 + 结果解析」整条链。
    ///
    /// ## 能验到哪一步（如实）
    ///
    /// - `tasks::load` 走真实的宿主调用（`tasks.list`），夹具按「请求字节
    ///   长度」分派到声明应答 —— 声明解析、校验、入表全真跑
    /// - `run_with` 传入注入的账号面（1 个账号），因此账号枚举、`task_state`
    ///   租约（`claim`/`finish`）、`host.call("tasks.run")`、结果解包全真跑
    /// - `task_state` 的存储由本用例**自己** `install` 到一个临时 FileStore
    ///   （进程级 `OnceLock`，只会被第一个安装者占住；本用例只读自己写入的
    ///   键，无跨用例干扰）
    ///
    /// ## 不能验到的（见报告「无法验证项」）
    ///
    /// - 生产入口 `tasks::run`（走 `active_accounts()` 的进程级注入面）——
    ///   该注入面由 `wasm::start(options)` 登记，本用例的 `NullAccess` 也走
    ///   同一注册路径，但「生产真的登记了正确实现」只能靠 T9 接线后目视核对
    /// - 30 秒调度循环的到点判定（`interval()` 已有纯逻辑单测；循环本身只
    ///   由 `spawn_with` 的可停止性单测覆盖）
    #[tokio::test]
    async fn probe_5_tasks_run_through_host() {
        let _guard = wasm_test_lock().lock().await;
        use crate::plugin_host::wasm::tasks;
        use crate::scheduled_tasks::task_state;
        use crate::storage::FileStore;

        struct OneAccount {
            provider: String,
        }

        #[async_trait::async_trait]
        impl WasmAccountAccess for OneAccount {
            fn provider_allowed(&self, provider: &str) -> bool {
                provider == self.provider
            }
            fn account_provider(&self, _account: &str) -> Option<String> {
                Some(self.provider.clone())
            }
            async fn store_get(&self, _provider: &str, _account: &str) -> Result<Value, WireError> {
                Ok(json!({ "id": "acct-1", "identity": "acct-1" }))
            }
            async fn store_update_with_public(
                &self,
                _account: &str,
                _expect: Value,
                _secret: Value,
                _public: Value,
            ) -> Result<Value, WireError> {
                Ok(json!({ "updated": false }))
            }
            async fn ledger_get(&self, _account: &str, _kind: &str) -> Result<Value, WireError> {
                Ok(Value::Null)
            }
            async fn ledger_put(
                &self,
                _account: &str,
                _kind: &str,
                _value: Value,
            ) -> Result<(), WireError> {
                Ok(())
            }
            async fn settings_get(&self, _provider: &str, _key: &str) -> Result<Value, WireError> {
                Ok(Value::Null)
            }
            async fn plugin_state(
                &self,
                _provider: &str,
                _account: &str,
                _key: &str,
            ) -> Result<Value, WireError> {
                Ok(json!({ "revision": 0, "schema": 1, "value": null }))
            }
            async fn plugin_state_compare_exchange(
                &self,
                _provider: &str,
                _account: &str,
                _key: &str,
                _revision: u64,
                _schema: u32,
                _value: Value,
            ) -> Result<Value, WireError> {
                Ok(json!({ "updated": false, "revision": 0 }))
            }
            fn accounts_for_provider(&self, provider: &str) -> Vec<Value> {
                if provider == self.provider {
                    vec![json!({ "id": "acct-1", "identity": "acct-1" })]
                } else {
                    Vec::new()
                }
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let provider = "probe-provider-tasks";
        std::fs::create_dir_all(tmp.path()).unwrap();
        std::fs::write(
            tmp.path().join("pack.json"),
            json!({
                "id": "probe-pack-tasks",
                "entry": { super::super::TARGET: "probe.wasm" },
                "providers": [{ "id": provider, "label": "Probe" }],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("wasm.json"),
            json!({
                "abi": super::super::ABI,
                // 任务要读账号（`account.get`），故一并声明账号能力
                "requires": ["tasks.v1", "accounts.cas.v1"],
                "origins": []
            })
            .to_string(),
        )
        .unwrap();
        let init_reply = json!({
            "result": { "providers": [{ "id": provider, "label": "Probe" }] }
        })
        .to_string();

        // —— ① `tasks::load`：声明解析（真走宿主 `tasks.list`）——
        // 宿主只认 `{"result": ...}` 信封（`errors::decode`）——裸数组会被
        // 判成无效协议消息，这是本探针第一次跑挂的原因。
        let declared = json!({
            "result": [
                { "id": "growth", "label": "Growth", "enabled": true, "interval_ms": 60000 },
                { "id": "daily", "label": "Daily", "enabled": true, "times": ["09:00"] }
            ]
        })
        .to_string();
        // 请求字节长度与宿主同源：`serde_json::to_vec({"method","params"})`
        let tasks_list_len = serde_json::to_vec(&json!({
            "method": "tasks.list",
            "params": { "provider": provider }
        }))
        .unwrap()
        .len();
        // `tasks.run` 的实际编码长度：`run_with` 的 params 恰好是
        // {provider, account, task, manual, scheduled_at}；`growth` 是
        // interval 型任务，`interval()` 对它返回 planned_at = 0。宿主
        // `runtime.rs` 只删 `proxy`/`proxy_trace` 等键（本请求没有），
        // 故与下面的算式逐字节一致。
        let run_len = serde_json::to_vec(&json!({
            "method": "tasks.run",
            "params": {
                "provider": provider,
                "account": "acct-1",
                "task": "growth",
                "manual": true,
                "scheduled_at": 0
            }
        }))
        .unwrap()
        .len();
        assert_ne!(run_len, tasks_list_len, "两条分派键必须不同");
        let run_reply = json!({
            "result": {
                "success": true,
                "message": "ran acct-1",
                "credential_revision": 0,
                "value": null
            }
        })
        .to_string();
        // 单宿主三路分派：init → `init_reply`；`tasks.list` → 声明；
        // `tasks.run` → 任务应答。**不换宿主** —— `stop()` 会 `forget(provider)`
        // 把任务声明摘掉，换宿主就得重新 load，反而绕远路。
        std::fs::write(
            tmp.path().join("probe.wasm"),
            super::super::fixture::module_with_two_len_dispatch(
                tasks_list_len,
                &declared,
                run_len,
                &run_reply,
                &init_reply,
            ),
        )
        .unwrap();
        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path().join("pack.json")).unwrap())
                .unwrap();
        // 账号面必须给 OneAccount：`run_with` 传的非空 account 会让宿主先
        // 调 `account.get`（`Context::new` 的归属校验），`NullAccess` 在那里
        // 就会以「no accounts in probe」失败。
        let accounts = Arc::new(OneAccount {
            provider: provider.to_string(),
        });
        let options =
            WasmOptions::from_pack(tmp.path(), &pack, accounts.clone(), Arc::new(NullEgress))
                .unwrap()
                .unwrap();
        let _host = super::super::start(options).await.expect("start");
        tasks::load(provider).await.expect("tasks::load");
        let snapshot = tasks::snapshot();
        assert!(
            snapshot.as_array().unwrap().iter().any(|entry| {
                entry["provider"] == provider
                    && entry["task"]["id"] == "growth"
                    && entry["task"]["enabled"] == true
            }),
            "声明必须进快照: {snapshot}"
        );
        // 同一声明表下 `configure`（管理面写配置路径）也必须可用
        tasks::configure(provider, "growth", &json!({ "enabled": true })).expect("configure");

        // —— ② `run_with`：执行（真走租约 + 宿主 `tasks.run`）——
        // 租约状态落临时 FileStore；`install` 是进程级 OnceLock，已被别的
        // 用例占住也无妨 —— 本用例只读自己写的键。
        task_state::install(Arc::new(FileStore::new(tmp.path().join("state"))));
        let outcome = tasks::run_with(accounts, provider, "growth", true).await;
        super::super::stop(provider).await;
        tasks::forget(provider);
        let outcome = outcome.expect("任务执行链必须跑通（含 task_state 租约）");
        let results = outcome["results"].as_array().expect("results");
        assert_eq!(results.len(), 1, "{outcome}");
        assert_eq!(results[0]["account"], "acct-1");
        assert_eq!(results[0]["success"], true, "{outcome}");
        assert_eq!(results[0]["message"], "ran acct-1");
    }

    /// **探针 4（配置持久化）**：`StoreSettings` 落真 FileStore，跨实例可读回。
    #[test]
    fn probe_4_task_settings_persist_across_instances() {
        use crate::plugin_host::wasm::tasks::{StoreSettings, WasmTaskSettings};
        use crate::storage::FileStore;

        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        let writer = StoreSettings::new(store.clone());
        writer
            .put(
                "p",
                "daily",
                &json!({ "enabled": true, "times": ["09:00"] }),
            )
            .expect("put");

        // 新建同 store 的实例读回（证明落盘，不是内存兜底）
        let reader = StoreSettings::new(store.clone());
        let value = reader.get("p", "daily");
        assert_eq!(value["enabled"], true, "{value}");
        assert_eq!(value["times"][0], "09:00");

        // 合并语义：后写补丁不丢旧键
        reader
            .put("p", "daily", &json!({ "enabled": false }))
            .unwrap();
        let merged = StoreSettings::new(store).get("p", "daily");
        assert_eq!(merged["enabled"], false);
        assert_eq!(merged["times"][0], "09:00", "旧键必须保留");

        // legacy 白名单：**反向对照**——只断言 `growthSchedule` 为 Null 是恒真的
        // （键不存在也返回 Null，无法区分「白名单拒绝」与「键不存在」）。
        // 先真写一个 `growthSchedule` 证明白名单放行它，再写一个非白名单键
        // 证明它即使存在也读不到。
        let legacy_store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        legacy_store
            .put("growthSchedule", &json!({ "enabled": true }))
            .unwrap();
        legacy_store
            .put("someOtherKey", &json!({ "leak": true }))
            .unwrap();
        let legacy_reader = StoreSettings::new(legacy_store);
        assert_eq!(
            legacy_reader.legacy("growthSchedule")["enabled"],
            true,
            "白名单键必须真读到"
        );
        assert_eq!(
            legacy_reader.legacy("someOtherKey"),
            Value::Null,
            "非白名单键即使存在也必须读不到"
        );
    }

    // ────────────── 真实 wasm 包端到端（§六.10 / §六·六.1/.2） ──────────────

    /// 夹具包的 id（固定值，便于断言）。
    const STREAM_PACK_ID: &str = "wasm-stream-probe";

    /// 两段 OpenAI SSE 正文（`stream.chunk` 的 data，夹具负责 base64）。
    const STREAM_CHUNK_1: &[u8] = b"data: {\"id\":\"chatcmpl-wasm\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"probe-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hel\"},\"finish_reason\":null}]}\n\n";
    const STREAM_CHUNK_2: &[u8] = b"data: {\"id\":\"chatcmpl-wasm\",\"object\":\"chat.completion.chunk\",\"created\":1700000000,\"model\":\"probe-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

    /// 打包一个**真流式** Wasm 插件包（`pack.json` + `wasm.json` + `plugin.wasm`）。
    ///
    /// `plugin.wasm` 由 [`fixture::streaming_wasm_module`] 生成 —— 它真导入
    /// `agent2api.call` 并在 conversation 分支里主动下发 head + 每段 chunk
    /// （宿主侧零 mock）。
    fn stream_pack_zip(provider: &str, chunks: &[&[u8]]) -> Vec<u8> {
        let pack = json!({
            "id": STREAM_PACK_ID,
            "name": "Wasm Stream Probe",
            "version": "0.0.1",
            "protocol": crate::plugin_host::rpc::PROTOCOL_VERSION,
            "entry": { super::super::TARGET: "plugin.wasm" },
            "providers": [{ "id": provider, "label": "Wasm Stream Probe" }],
        });
        let wasm = json!({
            "abi": super::super::ABI,
            "requires": ["chat.stream.v1", "accounts.cas.v1", "native.helpers.v1"],
            "helpers": ["keychain"],
            "origins": [],
            "memory_mib": 16,
            "concurrency": 2,
        });
        let module =
            super::super::fixture::streaming_wasm_module(provider, "Wasm Stream Probe", chunks);
        let mut out = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
            let options: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, data) in [
                ("pack.json", pack.to_string().into_bytes()),
                ("wasm.json", wasm.to_string().into_bytes()),
                ("plugin.wasm", module),
            ] {
                writer.start_file(name, options).unwrap();
                writer.write_all(&data).unwrap();
            }
            writer.finish().unwrap();
        }
        out
    }

    /// 起一个真实 `PluginHostManager`（临时注册表 / 插件根 / 账号池 / 存储）。
    fn probe_manager(
        tmp: &std::path::Path,
        store: Arc<FileStore>,
    ) -> (Arc<PluginHostManager>, Arc<PackRegistry>) {
        let registry = Arc::new(PackRegistry::new(store.clone()));
        registry.load().unwrap();
        let manager = Arc::new(PluginHostManager::new(
            registry.clone(),
            tmp.join("plugins"),
            crate::test_http_client(),
            HostEnv {
                data_dir: tmp.to_string_lossy().into_owned(),
                host_version: "test".into(),
                target: crate::plugin_host::host_target_triple(),
                proxy: String::new(),
                debug_traffic: false,
                loopback_port: 0,
            },
            Arc::new(AccountSessionStore::new(store.clone())),
            store,
        ));
        (manager, registry)
    }

    /// 假的 helper 数据源：把一个「原生包」摆成候选，验证 snapshot / grant
    /// 的聚合与指纹钉死（真实 wasm 宿主参与 available 判定）。
    struct FakeHelpers {
        provider: String,
        caller_pack: String,
        caller_version: String,
        caller_digest: String,
        grants: std::sync::Mutex<Value>,
    }

    #[async_trait::async_trait]
    impl super::super::native::WasmHelperSource for FakeHelpers {
        fn packs(&self) -> Vec<super::super::native::PackInfo> {
            vec![
                super::super::native::PackInfo {
                    id: self.caller_pack.clone(),
                    version: self.caller_version.clone(),
                    sha256: self.caller_digest.clone(),
                    enabled: true,
                    providers: vec![super::super::native::PackProviderInfo {
                        id: self.provider.clone(),
                        loaded: true,
                    }],
                },
                super::super::native::PackInfo {
                    id: "native-pack".into(),
                    version: "1.0.0".into(),
                    sha256: "digest-native".into(),
                    enabled: true,
                    providers: vec![super::super::native::PackProviderInfo {
                        id: "native-prov".into(),
                        loaded: true,
                    }],
                },
            ]
        }

        fn provider_pack(&self, provider: &str) -> Option<String> {
            (provider == self.provider).then_some(self.caller_pack.clone())
        }

        fn serves(&self, provider: &str) -> bool {
            provider == self.provider
        }

        fn grants(&self) -> Value {
            self.grants
                .lock()
                .map(|grants| grants.clone())
                .unwrap_or_else(|error| error.into_inner().clone())
        }

        fn set_grants(&self, grants: Value) -> Result<(), String> {
            if !grants.is_object() {
                return Err("原生辅助授权记录无效".into());
            }
            *self.grants.lock().unwrap_or_else(|e| e.into_inner()) = grants;
            Ok(())
        }

        async fn call_pack(
            &self,
            pack_id: &str,
            method: &str,
            _params: Value,
            _timeout: Duration,
        ) -> Result<Value, String> {
            if pack_id == "native-pack" && method == "native.helpers" {
                return Ok(json!([{ "id": "keychain", "operations": ["read", "write"] }]));
            }
            Err("no such helper".into())
        }
    }

    /// **探针 6**：真实 zip 包 → `manager.install_bytes` → `enable` → init
    /// 回 provider → `PluginBridge` 收到 head + ≥2 chunk；随后验证
    /// `native.snapshot` / `grant` 的聚合（helper 指纹钉死）。
    ///
    /// **零 mock**：wasmtime 真编译/实例化；head 与 chunk 由 wasm 经
    /// `agent2api.call` 主动发起。
    #[tokio::test]
    async fn probe_6_real_pack_install_enable_stream_and_helpers() {
        let _guard = wasm_test_lock().lock().await;
        use crate::bridge::plugin::{ConversationStream, PluginBridge};
        use crate::bridge::{ChatFormat, ChatMessage};
        use crate::plugin_host::rpc::StreamEvent;
        use crate::plugin_host::wasm::native;

        let provider = "probe-stream-provider";
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(FileStore::new(tmp.path().join("data")));
        let (manager, _registry) = probe_manager(tmp.path(), store.clone());

        let zip = stream_pack_zip(provider, &[STREAM_CHUNK_1, STREAM_CHUNK_2]);
        let meta = manager.install_bytes(&zip, None).expect("install_bytes");
        assert_eq!(meta.id, STREAM_PACK_ID);
        assert!(!meta.enabled, "新装包默认停用");
        // 入口二进制摘要 = helper 指纹的数据源（install.rs 安装时现算）
        let digest = meta.sha256.clone().expect("入口摘要必须落库");
        assert_eq!(
            digest,
            crate::plugin_host::install::sha256_hex(&super::super::fixture::streaming_wasm_module(
                provider,
                "Wasm Stream Probe",
                &[STREAM_CHUNK_1, STREAM_CHUNK_2],
            )),
            "sha256 必须是入口二进制摘要"
        );

        // enable → init 握手回 provider（wasm 真跑）
        let providers = manager.enable(&meta.id).await.expect("enable");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, provider);
        let host = host_for_provider(provider).expect("wasm 宿主必须登记");
        assert!(host.ready(), "init 成功后必须 ready");

        // PluginBridge：head + ≥2 chunk（wasm 主动调用 stream.head/chunk）
        let bridge = PluginBridge::new_wasm(host.clone(), provider.to_string());
        let req = ChatFormat {
            model: "probe-model".into(),
            messages: vec![ChatMessage::assistant("hi")],
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let mut stream = bridge
            .open_conversation_for_test(&req)
            .await
            .expect("open_conversation");
        assert!(
            matches!(stream, ConversationStream::Wasm { .. }),
            "wasm provider 必须产出 Wasm 分支"
        );
        let mut head = false;
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        loop {
            match stream.next().await {
                Some(Ok(StreamEvent::Head { status, .. })) => {
                    assert_eq!(status, 200);
                    head = true;
                }
                Some(Ok(StreamEvent::Chunk(bytes))) => chunks.push(bytes),
                Some(Ok(StreamEvent::Log { .. })) => {}
                Some(Ok(StreamEvent::End)) => {}
                Some(Err(error)) => panic!("wasm 流错误: {error}"),
                None => break,
            }
        }
        assert!(head, "必须收到 head");
        assert!(
            chunks.len() >= 2,
            "PluginBridge 必须收到 ≥2 个 chunk，实际 {}",
            chunks.len()
        );
        let body = String::from_utf8_lossy(&chunks.concat()).to_string();
        assert!(body.contains("\"content\":\"hel\""), "{body}");
        assert!(body.contains("\"content\":\"lo\""), "{body}");

        // ── native.snapshot / grant：真实宿主 + 假数据源 ──
        let _source_guard = native::source_test_lock().lock().await;
        native::reset_source_for_test();
        native::install_source(Arc::new(FakeHelpers {
            provider: provider.to_string(),
            caller_pack: STREAM_PACK_ID.to_string(),
            caller_version: meta.version.clone(),
            caller_digest: digest.clone(),
            grants: std::sync::Mutex::new(json!({})),
        }));
        let snapshot = native::snapshot(None).await;
        let requested = snapshot["requested"].as_array().expect("requested");
        let entry = requested
            .iter()
            .find(|item| item["provider"] == provider && item["id"] == "keychain")
            .expect("requested 必须含已申报的 helper");
        assert_eq!(entry["available"], true, "活宿主 + serves → available");
        assert_eq!(entry["callerDigest"], digest);
        let candidates = snapshot["candidates"].as_array().expect("candidates");
        let candidate = candidates
            .iter()
            .find(|item| item["pack"] == "native-pack" && item["id"] == "keychain")
            .expect("候选必须来自非 wasm 的启用包");
        assert_eq!(candidate["provider"], "native-prov");
        assert_eq!(candidate["digest"], "digest-native");
        assert_eq!(candidate["operations"], json!(["read", "write"]));

        // grant：选中候选后落授权（含 callerPack / callerDigest 钉死）
        let granted = native::grant(provider, "keychain", candidate, None)
            .await
            .expect("grant");
        assert_eq!(granted["granted"], true);
        let scoped = native::snapshot(None).await;
        assert_eq!(
            scoped["grants"][provider]["keychain"]["callerPack"],
            STREAM_PACK_ID
        );
        assert_eq!(
            scoped["grants"][provider]["keychain"]["callerDigest"],
            digest
        );
        native::reset_source_for_test();

        // 停用：宿主摘除、路由不再可见（反向对照）
        manager.disable(&meta.id).await.expect("disable");
        assert!(host_for_provider(provider).is_none(), "停用后宿主必须摘除");
    }

    /// **探针 7**：`tasks::run` 生产入口（`active_accounts()` 单例）→ 真实
    /// `AccountSessionStore::upsert` 种子账号 → 账号选择走到真实账号。
    ///
    /// 进程级单例防污染：本模块所有 `start()` 用例由 [`wasm_test_lock`]
    /// 串行，进入用例先 [`reset_accounts`]（生产「首次注入保留」语义不变）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)] // 与 account_access 测试同款例外：current-thread 运行时
    async fn probe_7_tasks_run_uses_real_account_store() {
        let _guard = wasm_test_lock().lock().await;
        let _secret = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use crate::plugin_host::wasm::tasks;
        use crate::plugin_host::wasm::AccountSessionAccess;
        use crate::scheduled_tasks::task_state;

        let provider = "probe-real-account-prov";
        let tmp = tempfile::tempdir().unwrap();
        crate::account_session::secret::set_data_dir(tmp.path());
        let store = Arc::new(FileStore::new(tmp.path().join("data")));
        let sessions = Arc::new(AccountSessionStore::new(store.clone()));

        // 真实账号池：种子一条本 provider 账号（+ 一条异家账号做反向对照）
        let mut seeded = AccountSession::new("ch-wasm", "acct-real");
        seeded.id = "acct-real".into();
        seeded.provider = provider.to_string();
        seeded.credential.secret = "sk-real-secret".into();
        let seeded = sessions.upsert(seeded).expect("upsert 真实账号");
        let mut other = AccountSession::new("ch-other", "acct-other");
        other.id = "acct-other".into();
        other.provider = "probe-other-prov".into();
        other.credential.secret = "sk-other".into();
        sessions.upsert(other).expect("upsert 异家账号");
        assert_eq!(sessions.len(), 2);

        // 两路长度分派夹具：tasks.list 声明 / tasks.run 应答 / init 兜底
        let dir = tmp.path().join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.json"),
            json!({
                "id": "probe-real-account-pack",
                "entry": { super::super::TARGET: "probe.wasm" },
                "providers": [{ "id": provider, "label": "Probe" }],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("wasm.json"),
            json!({
                "abi": super::super::ABI,
                "requires": ["tasks.v1", "accounts.cas.v1"],
                "origins": []
            })
            .to_string(),
        )
        .unwrap();
        let init_reply =
            json!({ "result": { "providers": [{ "id": provider, "label": "Probe" }] } })
                .to_string();
        let declared = json!({ "result": [
            { "id": "growth", "label": "Growth", "enabled": true, "interval_ms": 60000 }
        ] })
        .to_string();
        let tasks_list_len = serde_json::to_vec(&json!({
            "method": "tasks.list",
            "params": { "provider": provider }
        }))
        .unwrap()
        .len();
        let run_reply = json!({ "result": {
            "success": true, "message": format!("ran {seeded_id}", seeded_id = seeded.id)
        } })
        .to_string();
        let run_len = serde_json::to_vec(&json!({
            "method": "tasks.run",
            "params": {
                "provider": provider,
                "account": seeded.id,
                "task": "growth",
                "manual": true,
                "scheduled_at": 0
            }
        }))
        .unwrap()
        .len();
        assert_ne!(run_len, tasks_list_len);
        std::fs::write(
            dir.join("probe.wasm"),
            super::super::fixture::module_with_two_len_dispatch(
                tasks_list_len,
                &declared,
                run_len,
                &run_reply,
                &init_reply,
            ),
        )
        .unwrap();

        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("pack.json")).unwrap()).unwrap();
        let access =
            AccountSessionAccess::new("probe-real-account-pack", sessions.clone(), store.clone());
        let options = WasmOptions::from_pack(&dir, &pack, access, Arc::new(NullEgress))
            .unwrap()
            .unwrap();
        reset_accounts();
        let _host = super::super::start(options).await.expect("start");
        tasks::load(provider).await.expect("tasks::load");
        task_state::install(Arc::new(FileStore::new(tmp.path().join("state"))));

        // 生产入口：走 active_accounts() 单例（start 时登记的真实账号面）
        let outcome = tasks::run(provider, "growth", true)
            .await
            .expect("tasks::run 必须跑通真实账号选择");
        let results = outcome["results"].as_array().expect("results");
        assert_eq!(results.len(), 1, "只能选中本 provider 的账号: {outcome}");
        assert_eq!(results[0]["account"], seeded.id.as_str());
        assert_eq!(results[0]["success"], true, "{outcome}");
        assert!(
            results[0]["message"]
                .as_str()
                .unwrap_or("")
                .contains(&seeded.id),
            "插件应答必须来自真实执行: {outcome}"
        );

        super::super::stop(provider).await;
        tasks::forget(provider);
    }

    /// 账号访问面计数器包装（委托真实 `AccountSessionAccess`）。
    struct CountingAccess {
        inner: Arc<dyn WasmAccountAccess>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl WasmAccountAccess for CountingAccess {
        fn provider_allowed(&self, provider: &str) -> bool {
            self.inner.provider_allowed(provider)
        }
        fn account_provider(&self, account: &str) -> Option<String> {
            self.inner.account_provider(account)
        }
        async fn store_get(&self, provider: &str, account: &str) -> Result<Value, WireError> {
            self.inner.store_get(provider, account).await
        }
        async fn store_update_with_public(
            &self,
            account: &str,
            expect: Value,
            secret: Value,
            public: Value,
        ) -> Result<Value, WireError> {
            self.inner
                .store_update_with_public(account, expect, secret, public)
                .await
        }
        async fn ledger_get(&self, account: &str, kind: &str) -> Result<Value, WireError> {
            self.inner.ledger_get(account, kind).await
        }
        async fn ledger_put(
            &self,
            account: &str,
            kind: &str,
            value: Value,
        ) -> Result<(), WireError> {
            self.inner.ledger_put(account, kind, value).await
        }
        async fn settings_get(&self, provider: &str, key: &str) -> Result<Value, WireError> {
            self.inner.settings_get(provider, key).await
        }
        async fn plugin_state(
            &self,
            provider: &str,
            account: &str,
            key: &str,
        ) -> Result<Value, WireError> {
            self.inner.plugin_state(provider, account, key).await
        }
        async fn plugin_state_compare_exchange(
            &self,
            provider: &str,
            account: &str,
            key: &str,
            revision: u64,
            schema: u32,
            value: Value,
        ) -> Result<Value, WireError> {
            self.inner
                .plugin_state_compare_exchange(provider, account, key, revision, schema, value)
                .await
        }
        fn accounts_for_provider(&self, provider: &str) -> Vec<Value> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.inner.accounts_for_provider(provider)
        }
    }

    /// **探针 8**：30s tick —— `spawn_with` + `start_paused` + `advance(30s)`
    /// 驱动**第二轮**真实执行（`task_state::last_run_at` 前进）。
    ///
    /// 为什么不靠暂停时钟本身：`task_state` / `interval()` 用的是墙钟
    /// （`chrono::Utc::now`），`advance` 只推动 tokio 定时器。因此流程是：
    /// 第一拍执行 → **真实**等过 `min_gap_ms=1000` → `schedule_now` 把下一拍
    /// 排到「现在」→ `advance(30s)` 唤醒 tick → 第二轮真正执行。
    #[tokio::test(start_paused = true)]
    #[allow(clippy::await_holding_lock)] // 同 probe_7：current-thread 运行时例外
    async fn probe_8_scheduler_second_round_after_advance() {
        let _guard = wasm_test_lock().lock().await;
        let _secret = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        use crate::plugin_host::wasm::tasks;
        use crate::scheduled_tasks::task_state;

        let provider = "probe-tick-prov";
        let tmp = tempfile::tempdir().unwrap();
        crate::account_session::secret::set_data_dir(tmp.path());
        let store = Arc::new(FileStore::new(tmp.path().join("data")));
        let sessions = Arc::new(AccountSessionStore::new(store.clone()));
        let mut seeded = AccountSession::new("ch-tick", "acct-tick");
        seeded.id = "acct-tick".into();
        seeded.provider = provider.to_string();
        seeded.credential.secret = "sk-tick".into();
        sessions.upsert(seeded).expect("upsert");

        let dir = tmp.path().join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("pack.json"),
            json!({
                "id": "probe-tick-pack",
                "entry": { super::super::TARGET: "probe.wasm" },
                "providers": [{ "id": provider, "label": "Probe" }],
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("wasm.json"),
            json!({
                "abi": super::super::ABI,
                "requires": ["tasks.v1", "accounts.cas.v1"],
                "origins": []
            })
            .to_string(),
        )
        .unwrap();
        let init_reply =
            json!({ "result": { "providers": [{ "id": provider, "label": "Probe" }] } })
                .to_string();
        let declared = json!({ "result": [
            { "id": "growth", "label": "Growth", "enabled": true, "interval_ms": 60000 }
        ] })
        .to_string();
        let tasks_list_len = serde_json::to_vec(&json!({
            "method": "tasks.list",
            "params": { "provider": provider }
        }))
        .unwrap()
        .len();
        let run_reply = json!({ "result": { "success": true, "message": "ran tick" } }).to_string();
        let run_len = serde_json::to_vec(&json!({
            "method": "tasks.run",
            "params": {
                "provider": provider,
                "account": "acct-tick",
                "task": "growth",
                "manual": false,
                "scheduled_at": 0
            }
        }))
        .unwrap()
        .len();
        assert_ne!(run_len, tasks_list_len);
        std::fs::write(
            dir.join("probe.wasm"),
            super::super::fixture::module_with_two_len_dispatch(
                tasks_list_len,
                &declared,
                run_len,
                &run_reply,
                &init_reply,
            ),
        )
        .unwrap();

        let pack: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("pack.json")).unwrap()).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let inner = crate::plugin_host::wasm::AccountSessionAccess::new(
            "probe-tick-pack",
            sessions.clone(),
            store.clone(),
        );
        let access: Arc<dyn WasmAccountAccess> = Arc::new(CountingAccess {
            inner,
            calls: calls.clone(),
        });
        let options = WasmOptions::from_pack(&dir, &pack, access, Arc::new(NullEgress))
            .unwrap()
            .unwrap();
        reset_accounts();
        let _host = super::super::start(options).await.expect("start");
        tasks::load(provider).await.expect("tasks::load");
        task_state::install(Arc::new(FileStore::new(tmp.path().join("state"))));
        let key = format!("pluginTask:{provider}:growth:acct-tick");

        let token = crate::plugin_host::wasm::Cancel::new();
        tasks::spawn_with(token.clone());

        // 第一轮：循环第一拍不打盹，spawn 后立即执行
        let first = {
            let mut last = 0;
            for _ in 0..20_000 {
                last = task_state::read(&key)
                    .map(|state| state.last_run_at)
                    .unwrap_or(0);
                if last > 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
            assert!(last > 0, "第一轮必须执行（last_run_at）");
            last
        };
        assert!(calls.load(Ordering::Relaxed) >= 1, "第一轮必须枚举真实账号");

        // 真实墙钟等过 claim 的 min_gap_ms=1000（暂停时钟不推动 chrono）
        tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(1100)))
            .await
            .unwrap();
        // 把下一拍排到「现在」并推进 30s，唤醒 tick 进入第二轮
        task_state::schedule_now(&key).expect("schedule_now");
        tokio::time::advance(Duration::from_secs(30)).await;

        let mut second = 0;
        for _ in 0..2_000 {
            second = task_state::read(&key)
                .map(|state| state.last_run_at)
                .unwrap_or(0);
            if second > first {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            second > first,
            "advance(30s) 后第二轮必须真正执行（{first} → {second}）"
        );
        assert!(
            calls.load(Ordering::Relaxed) >= 2,
            "第二轮必须再次枚举真实账号"
        );

        token.cancel();
        tokio::task::yield_now().await;
        super::super::stop(provider).await;
        tasks::forget(provider);
    }
}
