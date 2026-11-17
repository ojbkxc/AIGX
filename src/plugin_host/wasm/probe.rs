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
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::{json, Value};

    use super::super::access::{WasmAccountAccess, WasmEgress, WasmStreamSink};
    use super::super::fixture::minimal_wasm_module;
    use super::super::wire::WireError;
    use super::super::{host_for_provider, WasmOptions};

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
}
