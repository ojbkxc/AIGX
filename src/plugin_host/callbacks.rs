//! 插件 → 网关的回调实现（M3 回调层）。
//!
//! 插件运行期通过 RPC 反向调用网关：读账号凭证、条件更新凭证、读写
//! pack 私有的 settings/ledger、打日志、发通知。本模块把线上的方法名
//! （与上游 provider-protocol 的 `methods::plugin::*` 对齐）接到 AIGX 的
//! [`AccountSessionStore`] 与 [`FileStore`] 上。
//!
//! ## 形状与语义
//!
//! - `store.get` `{provider, account}` → **完整插件账号记录**
//!   `{id, provider, identity, secret, public, proxy, …}`（A2A SDK 的
//!   `StoreAccount` 逐字段反序列化，缺 `id`/`provider` 会直接失败）；
//!   `account` 为空取该 provider 的队首（入库顺序第一个匹配项），非空按 id 直取
//! - `store.list` `{provider}` → 上述记录的数组
//! - `store.update` `{account, expect, secret[, public]}` → `{updated}`；
//!   `expect` 是插件先前读到的 **plugin_secret**，当前值仍等于它才写入
//!   （防并发刷新互相覆盖）；`public` 有则同步替换公开快照
//! - `store.mark_limited` `{account, model, reset_at?}` → 记一次 429 冷却
//! - `settings.get/set` `{provider, key[, value]}`、`store.ledger_get/put`
//!   `{account, kind[, value]}` → 落到 [`FileStore`]，键带
//!   `plugin:{pack_id}:` 前缀——插件只能读写自己名下的键
//! - `log` / `notify` → `tracing::info!`
//!
//! ## 硬约束
//!
//! - **任何回调都不把凭证明文写日志**。`log`/`notify` 只记插件自报的文案，
//!   不 dump 整个 params（params 里可能夹带账号数据）
//! - 不写 provider 伪装逻辑（既有立场）

use std::sync::Arc;

use serde_json::{json, Value};

use crate::account_session::AccountSessionStore;
use crate::channel::circuit_breaker::{FailureType, RateLimitScope};
use crate::plugin_host::rpc::{CallbackHandler, RpcError};
use crate::storage::FileStore;

/// 一个插件包的回调处理器（`pack_id` 隔离键空间）。
pub struct PluginCallbacks {
    pack_id: String,
    sessions: Arc<AccountSessionStore>,
    store: Arc<FileStore>,
}

impl PluginCallbacks {
    pub fn new(pack_id: String, sessions: Arc<AccountSessionStore>, store: Arc<FileStore>) -> Self {
        Self {
            pack_id,
            sessions,
            store,
        }
    }

    /// settings 的 KV 键（pack 私有命名空间）。
    fn settings_key(&self, provider: &str, key: &str) -> String {
        format!("plugin:{}:settings:{}:{}", self.pack_id, provider, key)
    }

    /// ledger 的 KV 键（按账号 + 台账类型命名）。
    fn ledger_key(&self, account: &str, kind: &str) -> String {
        format!("plugin:{}:ledger:{}:{}", self.pack_id, account, kind)
    }

    /// `store.get`：读完整插件账号记录（A2A SDK 的 `StoreAccount`）。
    ///
    /// ── 为什么不能只回 `{secret, public}` ─────────────────────────
    /// 插件 SDK 用 `serde_json::from_value::<StoreAccount>` 解这份应答，而
    /// `StoreAccount` 的 `id` / `provider` / `secret` / `public` 都是**必填**。
    /// 少了 `id`/`provider` 会报「返回形状不对」，插件随后把账号当成不可用，
    /// 数据面表现为所有对话 500 —— 这正是「装得上但跑不起来」的来源之一。
    /// [`AccountSessionStore::plugin_account`] 已是 A2A 同形的投影，直接复用。
    fn store_get(&self, provider: &str, account: &str) -> Result<Value, RpcError> {
        let id = if account.is_empty() {
            self.sessions
                .plugin_accounts_for_provider(provider)
                .into_iter()
                .next()
                .map(|session| session.id)
        } else {
            Some(account.to_string())
        };
        let Some(id) = id else {
            return Err(not_found(format!("no account for provider '{provider}'")));
        };
        let Some(session) = self.sessions.get(&id) else {
            return Err(not_found("account not found"));
        };
        // 插件只能读自己 provider 的账号（与上游 `authorize_callback_account`
        // 的归属核对同口径；都不允许凭参数跨家读取）。
        if !provider.is_empty() && !session.provider.eq_ignore_ascii_case(provider) {
            return Err(forbidden(
                "account does not belong to the requested provider",
            ));
        }
        self.sessions
            .plugin_account(&id)
            .ok_or_else(|| not_found("account not found"))
    }

    /// `store.list`：该 provider 的全部账号（同 `StoreAccount` 形状）。
    fn store_list(&self, provider: &str) -> Result<Value, RpcError> {
        let list: Vec<Value> = self
            .sessions
            .plugin_accounts_for_provider(provider)
            .into_iter()
            .filter(|session| session.provider.eq_ignore_ascii_case(provider))
            .filter_map(|session| self.sessions.plugin_account(&session.id))
            .collect();
        Ok(Value::Array(list))
    }

    /// `store.update`：比较-再写（A2A `plugin_update_secret_cas`）。
    ///
    /// `expect`/`secret` 都是插件读到的 **plugin_secret** 值（JSON），不是
    /// 网关内部 `credential.secret` 的明文。`public` 有则同步替换公开快照。
    /// 账号不存在 / 期望值已变都返回 `{updated:false}` —— 这不是错误，是
    /// 并发下的正常竞态结果。
    fn store_update(
        &self,
        account: &str,
        expect: Option<&Value>,
        secret: Option<&Value>,
        public: Option<&Value>,
    ) -> Result<Value, RpcError> {
        if account.is_empty() {
            return Err(bad_request("store.update: account is required"));
        }
        let Some(secret) = secret.filter(|value| !value.is_null()) else {
            // 空 secret 写入等于清空凭证：几乎必然是插件 bug，直接拒绝
            return Err(bad_request("store.update: secret must not be empty"));
        };
        if secret.as_str().is_some_and(|text| text.trim().is_empty()) {
            return Err(bad_request("store.update: secret must not be empty"));
        }
        if self.sessions.get(account).is_none() {
            return Ok(json!({ "updated": false }));
        }
        let expect = expect.cloned().unwrap_or(Value::Null);
        let public = public.filter(|value| value.is_object());
        let updated = self
            .sessions
            .plugin_update_secret_cas(account, &expect, secret, public)
            .map_err(|error| internal(format!("store.update: {error}")))?;
        Ok(json!({ "updated": updated }))
    }

    /// `store.mark_limited`：插件在上游撞到 429 后上报（A2A 同名回调）。
    ///
    /// 不实现这条会让 SDK 的 `store_mark_limited` 收到 `unsupported`，插件若
    /// 把回调失败当致命错误，一次限流就会把整轮请求打断。这里落成账号冷却：
    /// `model` 非空 = 模型级，空 = 账号级；`reset_at` 是毫秒时间戳。
    fn store_mark_limited(
        &self,
        account: &str,
        model: &str,
        reset_at: Option<i64>,
    ) -> Result<Value, RpcError> {
        if account.is_empty() {
            return Err(bad_request("store.mark_limited: account is required"));
        }
        let Some(session) = self.sessions.get(account) else {
            return Err(not_found("account not found"));
        };
        let retry_after = reset_at.map(|reset_at| {
            let remaining_ms = reset_at.saturating_sub(now_ms()).max(0);
            (remaining_ms / 1000).max(1) as u64
        });
        let (scope, model) = if model.is_empty() {
            (RateLimitScope::Account, model)
        } else {
            (RateLimitScope::Model, model)
        };
        self.sessions.record_failure(
            &session.id,
            model,
            &FailureType::RateLimited { scope, retry_after },
            "插件上报限额",
            Some(429),
        );
        Ok(Value::Null)
    }

    /// `settings.get`：读 pack 私有设置；缺失返回 `null`。
    ///
    /// 两个**网关专用键**（A2A `PLUGIN_SETTING_GATEWAY_PROMPT*` 同口径）：
    /// - `gatewayPrompt`：这一家要不要装网关自带提示词（键缺失 = true）；
    /// - `gatewayPromptText`：三段正文覆盖（只含用户改过的段，缺段为 null，
    ///   未配置整家返回 null）。
    fn settings_get(&self, provider: &str, key: &str) -> Result<Value, RpcError> {
        if key.is_empty() {
            return Err(bad_request("settings.get: key is required"));
        }
        // 网关保留键：插件问的是「本 provider 的模型目录快照」，不是自己的
        // 设置（= 上游 `rpc_cache::SETTING_MODEL_CATALOG`）。原生包此前没有
        // 这条拦截，插件拿到的永远是 `null`，模型列表因此一直是空的。
        if key == super::model_catalog::SETTING_MODEL_CATALOG {
            return Ok(super::model_catalog::snapshot(provider));
        }
        if key == "gatewayPrompt" {
            let enabled = crate::prompt::snapshot()
                .and_then(|cfg| cfg.gateway.get(provider).copied())
                .unwrap_or(true);
            return Ok(Value::Bool(enabled));
        }
        if key == "gatewayPromptText" {
            let Some(blocks) =
                crate::prompt::snapshot().and_then(|cfg| cfg.gateway_text.get(provider).cloned())
            else {
                return Ok(Value::Null);
            };
            return serde_json::to_value(blocks).map_err(|e| RpcError {
                message: format!("settings.get gatewayPromptText: {e}"),
                code: Some("internal".into()),
            });
        }
        match self.store.get::<Value>(&self.settings_key(provider, key)) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Ok(Value::Null),
            Err(e) => Err(storage_error("settings.get", e)),
        }
    }

    /// `settings.set`：写 pack 私有设置。
    fn settings_set(
        &self,
        provider: &str,
        key: &str,
        value: Option<&Value>,
    ) -> Result<Value, RpcError> {
        if key.is_empty() {
            return Err(bad_request("settings.set: key is required"));
        }
        let value = value.cloned().unwrap_or(Value::Null);
        self.store
            .put(&self.settings_key(provider, key), &value)
            .map_err(|e| storage_error("settings.set", e))?;
        Ok(Value::Null)
    }

    /// `store.ledger_get`：读台账；缺失返回 `null`。
    fn ledger_get(&self, account: &str, kind: &str) -> Result<Value, RpcError> {
        if account.is_empty() || kind.is_empty() {
            return Err(bad_request(
                "store.ledger_get: account and kind are required",
            ));
        }
        match self.store.get::<Value>(&self.ledger_key(account, kind)) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Ok(Value::Null),
            Err(e) => Err(storage_error("store.ledger_get", e)),
        }
    }

    /// `store.ledger_put`：写台账。
    fn ledger_put(
        &self,
        account: &str,
        kind: &str,
        value: Option<&Value>,
    ) -> Result<Value, RpcError> {
        if account.is_empty() || kind.is_empty() {
            return Err(bad_request(
                "store.ledger_put: account and kind are required",
            ));
        }
        let value = value.cloned().unwrap_or(Value::Null);
        self.store
            .put(&self.ledger_key(account, kind), &value)
            .map_err(|e| storage_error("store.ledger_put", e))?;
        Ok(Value::Null)
    }
}

impl CallbackHandler for PluginCallbacks {
    fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        };
        match method {
            "store.get" => self.store_get(&text("provider"), &text("account")),
            "store.list" => self.store_list(&text("provider")),
            "store.update" => self.store_update(
                &text("account"),
                params.get("expect"),
                params.get("secret"),
                params.get("public"),
            ),
            "store.mark_limited" => {
                let reset_at = params.get("reset_at").and_then(Value::as_i64);
                self.store_mark_limited(&text("account"), &text("model"), reset_at)
            }
            "settings.get" => self.settings_get(&text("provider"), &text("key")),
            "settings.set" => {
                self.settings_set(&text("provider"), &text("key"), params.get("value"))
            }
            // 两种拼写都收：SDK 常量是 `store.ledger_get`，脚本宿主历史上
            // 用的是 `store.ledger.get`（上游 callbacks.rs 同款兼容）。
            "store.ledger_get" | "store.ledger.get" => {
                self.ledger_get(&text("account"), &text("kind"))
            }
            "store.ledger_put" | "store.ledger.put" => {
                self.ledger_put(&text("account"), &text("kind"), params.get("value"))
            }
            // 传输能力探测（上游 callbacks.rs 固定回 multipart:true）。
            "transport.capabilities" => Ok(json!({ "multipart": true })),
            "log" => {
                let level = text("level");
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                tracing::info!("[plugin {} log:{}] {}", self.pack_id, level, message);
                Ok(Value::Null)
            }
            "notify" => {
                let kind = text("kind");
                // SDK 侧通知形状是 `{kind, payload}`（不是 `message`）。
                // `models_changed` 的 payload 带 provider/account，记下来便于
                // 排查「插件刷新了模型但网关目录没跟上」；真正的目录同步在
                // 管理面 `models.refresh` 链路（本回调不持有宿主句柄）。
                let detail = params
                    .get("payload")
                    .map(|payload| payload.to_string())
                    .or_else(|| {
                        params
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .unwrap_or_default();
                // 只记插件自报的文案，不 dump 整个 params（可能夹带账号数据）
                tracing::info!("[plugin {} notify] {kind}: {detail}", self.pack_id);
                Ok(Value::Null)
            }
            other => Err(RpcError {
                message: format!("unsupported callback: {other}"),
                code: Some("unsupported".into()),
            }),
        }
    }
}

/// 当前毫秒时间戳（与 `wasm::now_ms` 同口径）。
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
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

fn forbidden(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("forbidden".into()),
    }
}

fn internal(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("internal".into()),
    }
}

fn storage_error(method: &str, e: anyhow::Error) -> RpcError {
    RpcError {
        message: format!("{method}: {e}"),
        code: Some("storage".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::model::AccountSession;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    /// 编码路径走全局 data_dir——拿跨模块共享锁设独立临时目录，防并行
    /// 测试互相换密钥（与 `account_session::store::tests` 同手法）。
    fn with_secret_dir<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let _guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-callbacks-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        f()
    }

    /// 建一对 (sessions, callbacks)；FileStore 目录与 store.rs 测试同手法。
    fn harness(pack_id: &str) -> (AccountSessionStore, PluginCallbacks, Arc<FileStore>) {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let sessions = AccountSessionStore::new(store.clone());
        let callbacks = PluginCallbacks::new(
            pack_id.to_string(),
            Arc::new(sessions.clone()),
            store.clone(),
        );
        (sessions, callbacks, store)
    }

    /// 造一个带明文凭证的账号（明文入，落盘前由 store 加密）。
    ///
    /// `created_at` 显式给定——队首判据是 `(created_at, id)`，同秒入库的
    /// 两条会以随机 UUID 决胜负，测试要确定性就得自己填序。
    fn account(
        channel_id: &str,
        name: &str,
        provider: &str,
        secret: &str,
        created_at: i64,
    ) -> AccountSession {
        let mut s = AccountSession::new(channel_id, name);
        s.provider = provider.to_string();
        s.created_at = created_at;
        s.credential.secret = secret.to_string();
        s
    }

    #[test]
    fn store_get_returns_store_account_shape() {
        with_secret_dir("store-get", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let created = sessions
                .upsert(account("ch1", "alice", "kiro", "sk-alice-token", 1))
                .unwrap();

            let got = callbacks
                .handle(
                    "store.get",
                    json!({"provider": "kiro", "account": created.id}),
                )
                .unwrap();
            // A2A SDK 的 `StoreAccount`：id/provider/secret/public 都是必填。
            assert_eq!(got["id"], created.id);
            assert_eq!(got["provider"], "kiro");
            assert_eq!(got["secret"]["token"], "sk-alice-token");
            assert_eq!(got["public"]["id"], created.id);
            assert_eq!(got["public"]["provider"], "kiro");
            assert_eq!(got["public"]["name"], "alice");
            // 明文只出现在返回给插件的 secret 里：日志侧不允许出现
            //（本测试只校验返回形状；防泄漏靠 log/notify 只记文案）
        });
    }

    #[test]
    fn store_get_rejects_cross_provider_read() {
        with_secret_dir("store-cross", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let created = sessions
                .upsert(account("ch1", "alice", "kiro", "sk-alice", 1))
                .unwrap();
            let err = callbacks
                .handle(
                    "store.get",
                    json!({"provider": "astudio", "account": created.id}),
                )
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("forbidden"));
        });
    }

    #[test]
    fn store_get_empty_account_takes_queue_head() {
        with_secret_dir("store-head", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let first = sessions
                .upsert(account("ch1", "alice", "kiro", "sk-alice", 1))
                .unwrap();
            let _second = sessions
                .upsert(account("ch1", "bob", "kiro", "sk-bob", 2))
                .unwrap();

            // 空 account → 队首（先入库的 alice）
            let got = callbacks
                .handle("store.get", json!({"provider": "kiro"}))
                .unwrap();
            assert_eq!(got["secret"]["token"], "sk-alice");
            assert_eq!(got["id"], first.id);

            // 只匹配该 provider：别的家的账号不进这个队列
            let other = sessions
                .upsert(account("ch2", "carol", "astudio", "sk-carol", 3))
                .unwrap();
            let got = callbacks
                .handle("store.get", json!({"provider": "kiro"}))
                .unwrap();
            assert_eq!(got["id"], first.id);
            assert_ne!(got["id"], other.id);

            // 不存在的 provider → not_found
            let err = callbacks
                .handle("store.get", json!({"provider": "nope"}))
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("not_found"));
        });
    }

    #[test]
    fn store_list_returns_all_accounts_of_provider() {
        with_secret_dir("store-list", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let a = sessions
                .upsert(account("ch1", "alice", "kiro", "sk-a", 1))
                .unwrap();
            let b = sessions
                .upsert(account("ch1", "bob", "kiro", "sk-b", 2))
                .unwrap();
            let _c = sessions
                .upsert(account("ch2", "carol", "astudio", "sk-c", 3))
                .unwrap();

            let got = callbacks
                .handle("store.list", json!({"provider": "kiro"}))
                .unwrap();
            let list = got.as_array().unwrap();
            assert_eq!(list.len(), 2);
            let ids: Vec<&str> = list.iter().map(|v| v["id"].as_str().unwrap()).collect();
            assert!(ids.contains(&a.id.as_str()) && ids.contains(&b.id.as_str()));
            for v in list {
                // 同 StoreAccount 形状：secret 是对象、provider 必填
                assert!(v["secret"].is_object());
                assert_eq!(v["provider"], "kiro");
                assert_eq!(v["public"]["provider"], "kiro");
            }
        });
    }

    /// 插件账号：`plugin_secret` 才是插件读写的凭证（`credential.*` 是网关
    /// 内部投影）。CAS 测试必须用它，才能复现 SDK 的 expect/secret 语义。
    fn plugin_account(
        sessions: &AccountSessionStore,
        id_hint: &str,
        provider: &str,
        secret: Value,
        created_at: i64,
    ) -> AccountSession {
        let mut s = account("ch1", id_hint, provider, "", created_at);
        s.plugin_secret = Some(secret);
        sessions.upsert(s).unwrap()
    }

    #[test]
    fn store_update_is_compare_then_write_on_plugin_secret() {
        with_secret_dir("store-update", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let created = plugin_account(&sessions, "alice", "kiro", json!("sk-old"), 1);

            // 期望值相符 → 写入成功
            let got = callbacks
                .handle(
                    "store.update",
                    json!({"account": created.id, "expect": "sk-old", "secret": "sk-new"}),
                )
                .unwrap();
            assert_eq!(got["updated"], true);
            assert_eq!(
                sessions.get(&created.id).unwrap().plugin_secret,
                Some(json!("sk-new"))
            );

            // 期望值已变（陈旧快照）→ updated:false，不覆盖
            let got = callbacks
                .handle(
                    "store.update",
                    json!({"account": created.id, "expect": "sk-old", "secret": "sk-stale"}),
                )
                .unwrap();
            assert_eq!(got["updated"], false);
            assert_eq!(
                sessions.get(&created.id).unwrap().plugin_secret,
                Some(json!("sk-new"))
            );

            // public 有则同步替换公开快照
            let got = callbacks
                .handle(
                    "store.update",
                    json!({
                        "account": created.id,
                        "expect": "sk-new",
                        "secret": "sk-newer",
                        "public": {"name": "Alice", "needsRelogin": false},
                    }),
                )
                .unwrap();
            assert_eq!(got["updated"], true);
            let session = sessions.get(&created.id).unwrap();
            assert_eq!(session.plugin_public.as_ref().unwrap()["name"], "Alice");

            // 账号不存在 → updated:false（不是错误）
            let got = callbacks
                .handle(
                    "store.update",
                    json!({"account": "missing", "expect": "x", "secret": "y"}),
                )
                .unwrap();
            assert_eq!(got["updated"], false);

            // 空 secret 拒绝写入（防清空凭证）
            let err = callbacks
                .handle(
                    "store.update",
                    json!({"account": created.id, "expect": "sk-newer", "secret": ""}),
                )
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("bad_request"));
        });
    }

    #[test]
    fn store_mark_limited_marks_model_cooldown() {
        with_secret_dir("store-limited", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let created = plugin_account(&sessions, "alice", "kiro", json!("sk"), 1);
            let reset_at = now_ms() + 120_000;

            let got = callbacks
                .handle(
                    "store.mark_limited",
                    json!({"account": created.id, "model": "m1", "reset_at": reset_at}),
                )
                .unwrap();
            assert_eq!(got, Value::Null);
            let session = sessions.get(&created.id).unwrap();
            assert!(
                session.cooldowns.contains_key("m1"),
                "模型级限流应写入 cooldowns: {:?}",
                session.cooldowns
            );

            // 缺 account → bad_request；账号不存在 → not_found
            let err = callbacks
                .handle("store.mark_limited", json!({"model": "m1"}))
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("bad_request"));
            let err = callbacks
                .handle(
                    "store.mark_limited",
                    json!({"account": "missing", "model": "m1"}),
                )
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("not_found"));
        });
    }

    #[test]
    fn settings_roundtrip_isolated_by_pack() {
        with_secret_dir("settings", || {
            let (_sessions, callbacks, store) = harness("kiro-pack");
            callbacks
                .handle(
                    "settings.set",
                    json!({"provider": "kiro", "key": "timeout", "value": 30}),
                )
                .unwrap();

            let got = callbacks
                .handle(
                    "settings.get",
                    json!({"provider": "kiro", "key": "timeout"}),
                )
                .unwrap();
            assert_eq!(got, json!(30));

            // 缺失 → null
            let got = callbacks
                .handle("settings.get", json!({"provider": "kiro", "key": "absent"}))
                .unwrap();
            assert_eq!(got, Value::Null);

            // 键前缀隔离：别的 pack 看不到（另起一个 callbacks 实例同库）
            let other = PluginCallbacks::new(
                "other-pack".to_string(),
                Arc::new(_sessions.clone()),
                store.clone(),
            );
            let got = other
                .handle(
                    "settings.get",
                    json!({"provider": "kiro", "key": "timeout"}),
                )
                .unwrap();
            assert_eq!(got, Value::Null);

            // 网关保留键 `modelCatalog` 必须回目录快照，而不是插件的设置值
            // （原生包此前拿到 null，模型列表因此一直为空）。
            let provider = format!("unit-callbacks-catalog-{}", std::process::id());
            crate::plugin_host::model_catalog::set_remote(
                &provider,
                vec![json!({ "id": "m-alpha" })],
                42,
            );
            let got = callbacks
                .handle(
                    "settings.get",
                    json!({"provider": provider, "key": "modelCatalog"}),
                )
                .unwrap();
            assert_eq!(got["models"][0]["id"], "m-alpha");
            assert_eq!(got["meta"]["refreshed_at"], 42);
            crate::plugin_host::model_catalog::remove(&provider);
        });
    }

    #[test]
    fn ledger_accepts_dotted_aliases() {
        with_secret_dir("ledger-dotted", || {
            let (_sessions, callbacks, _store) = harness("kiro-pack");
            callbacks
                .handle(
                    "store.ledger.put",
                    json!({"account": "acc-1", "kind": "balance", "value": {"credits": 9}}),
                )
                .unwrap();
            let got = callbacks
                .handle(
                    "store.ledger.get",
                    json!({"account": "acc-1", "kind": "balance"}),
                )
                .unwrap();
            assert_eq!(got["credits"], 9);
        });
    }

    #[test]
    fn transport_capabilities_reports_multipart() {
        let callbacks = PluginCallbacks::new(
            "kiro-pack".to_string(),
            Arc::new(AccountSessionStore::new(Arc::new(FileStore::new(
                tempfile::TempDir::new().unwrap().keep(),
            )))),
            Arc::new(FileStore::new(tempfile::TempDir::new().unwrap().keep())),
        );
        let got = callbacks
            .handle("transport.capabilities", Value::Null)
            .unwrap();
        assert_eq!(got["multipart"], true);
    }

    #[test]
    fn ledger_roundtrip() {
        with_secret_dir("ledger", || {
            let (_sessions, callbacks, _store) = harness("kiro-pack");
            callbacks
                .handle(
                    "store.ledger_put",
                    json!({"account": "acc-1", "kind": "balance", "value": {"credits": 7}}),
                )
                .unwrap();
            let got = callbacks
                .handle(
                    "store.ledger_get",
                    json!({"account": "acc-1", "kind": "balance"}),
                )
                .unwrap();
            assert_eq!(got["credits"], 7);

            let got = callbacks
                .handle(
                    "store.ledger_get",
                    json!({"account": "acc-1", "kind": "absent"}),
                )
                .unwrap();
            assert_eq!(got, Value::Null);

            // 缺 account/kind → bad_request
            let err = callbacks
                .handle("store.ledger_get", json!({"account": "", "kind": "x"}))
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("bad_request"));
        });
    }

    #[test]
    fn log_and_notify_return_null() {
        // 只验形状：不读日志（读日志要装 subscriber，单测里不值得）
        let callbacks = PluginCallbacks::new(
            "kiro-pack".to_string(),
            Arc::new(AccountSessionStore::new(Arc::new(FileStore::new(
                tempfile::TempDir::new().unwrap().keep(),
            )))),
            Arc::new(FileStore::new(tempfile::TempDir::new().unwrap().keep())),
        );
        assert_eq!(
            callbacks
                .handle("log", json!({"level": "info", "message": "hi"}))
                .unwrap(),
            Value::Null
        );
        assert_eq!(
            callbacks
                .handle("notify", json!({"kind": "progress", "message": "10%"}))
                .unwrap(),
            Value::Null
        );
    }

    #[test]
    fn unsupported_method_rejected() {
        let callbacks = PluginCallbacks::new(
            "kiro-pack".to_string(),
            Arc::new(AccountSessionStore::new(Arc::new(FileStore::new(
                tempfile::TempDir::new().unwrap().path().to_path_buf(),
            )))),
            Arc::new(FileStore::new(
                tempfile::TempDir::new().unwrap().path().to_path_buf(),
            )),
        );
        let err = callbacks.handle("whatever", Value::Null).unwrap_err();
        assert_eq!(err.message, "unsupported callback: whatever");
        assert_eq!(err.code.as_deref(), Some("unsupported"));
    }
}
