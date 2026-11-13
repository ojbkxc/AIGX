//! 插件 → 网关的回调实现（M3 回调层）。
//!
//! 插件运行期通过 RPC 反向调用网关：读账号凭证、条件更新凭证、读写
//! pack 私有的 settings/ledger、打日志、发通知。本模块把线上的方法名
//! （与上游 provider-protocol 的 `methods::plugin::*` 对齐）接到 AIGX 的
//! [`AccountSessionStore`] 与 [`FileStore`] 上。
//!
//! ## 形状与语义
//!
//! - `store.get` `{provider, account}` → `{secret, public}`；
//!   `account` 为空取该 provider 的队首（入库顺序第一个匹配项），非空按 id 直取
//! - `store.list` `{provider}` → `[{id, secret, public}]`
//! - `store.update` `{account, expect, secret}` → `{updated}`；
//!   `expect` 是插件先前读到的 secret 明文，当前值仍等于它才写入
//!   （防并发刷新互相覆盖）
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

use crate::account_session::model::{AccountSession, Credential};
use crate::account_session::store::CasOutcome;
use crate::account_session::AccountSessionStore;
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

    /// `store.get`：读账号凭证。
    fn store_get(&self, provider: &str, account: &str) -> Result<Value, RpcError> {
        let Some(session) = self.resolve_account(provider, account) else {
            return Err(RpcError {
                message: format!("no account for provider '{provider}'"),
                code: Some("not_found".into()),
            });
        };
        Ok(json!({
            "secret": session.credential.secret_plain(),
            "public": public_view(&session),
        }))
    }

    /// `store.list`：该 provider 的全部账号。
    fn store_list(&self, provider: &str) -> Result<Value, RpcError> {
        let list: Vec<Value> = self
            .sessions
            .all()
            .into_iter()
            .filter(|s| s.provider.eq_ignore_ascii_case(provider))
            .map(|s| {
                json!({
                    "id": s.id,
                    "secret": s.credential.secret_plain(),
                    "public": public_view(&s),
                })
            })
            .collect();
        Ok(Value::Array(list))
    }

    /// `store.update`：比较-再写。
    ///
    /// 账号不存在 / 期望值已变（另一个刷新先落盘了）都返回 `{updated:false}`
    /// —— 这不是错误，是并发下的正常竞态结果。
    fn store_update(
        &self,
        account: &str,
        expect: Option<&Value>,
        secret: Option<&Value>,
    ) -> Result<Value, RpcError> {
        if account.is_empty() {
            return Err(bad_request("store.update: account is required"));
        }
        let secret_plain = secret.and_then(Value::as_str).unwrap_or("");
        if secret_plain.is_empty() {
            // 空 secret 写入等于清空凭证：几乎必然是插件 bug，直接拒绝
            return Err(bad_request("store.update: secret must not be empty"));
        }
        let expect_plain = expect.and_then(Value::as_str).unwrap_or("");
        let Some(existing) = self.sessions.get(account) else {
            return Ok(json!({ "updated": false }));
        };
        // 只换主密钥：kind / 具名字段原样保留（凭证刷新不碰其它字段）
        let mut next: Credential = existing.credential.clone();
        next.secret = secret_plain.to_string();
        let updated = matches!(
            self.sessions
                .update_credential_cas(account, expect_plain, next),
            CasOutcome::Updated
        );
        Ok(json!({ "updated": updated }))
    }

    /// 按 provider（+可选账号 id）定位账号。空 `account` 取该 provider 的
    /// 队首——入库顺序（created_at, id）的第一个匹配项。
    fn resolve_account(&self, provider: &str, account: &str) -> Option<AccountSession> {
        if !account.is_empty() {
            return self.sessions.get(account);
        }
        if provider.is_empty() {
            return None;
        }
        self.sessions
            .all()
            .into_iter()
            .find(|s| s.provider.eq_ignore_ascii_case(provider))
    }

    /// `settings.get`：读 pack 私有设置；缺失返回 `null`。
    fn settings_get(&self, provider: &str, key: &str) -> Result<Value, RpcError> {
        if key.is_empty() {
            return Err(bad_request("settings.get: key is required"));
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
            "store.update" => {
                self.store_update(&text("account"), params.get("expect"), params.get("secret"))
            }
            "settings.get" => self.settings_get(&text("provider"), &text("key")),
            "settings.set" => {
                self.settings_set(&text("provider"), &text("key"), params.get("value"))
            }
            "store.ledger_get" => self.ledger_get(&text("account"), &text("kind")),
            "store.ledger_put" => {
                self.ledger_put(&text("account"), &text("kind"), params.get("value"))
            }
            "log" => {
                let level = text("level");
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                tracing::info!("[plugin {} log:{}] {}", self.pack_id, level, message);
                Ok(Value::Null)
            }
            "notify" => {
                let kind = text("kind");
                let message = params.get("message").and_then(Value::as_str).unwrap_or("");
                // 只记插件自报的文案，不 dump 整个 params（可能夹带账号数据）
                tracing::info!("[plugin {} notify] {kind}: {message}", self.pack_id);
                Ok(Value::Null)
            }
            other => Err(RpcError {
                message: format!("unsupported callback: {other}"),
                code: Some("unsupported".into()),
            }),
        }
    }
}

/// 账号的非保密视图（回给插件做决策用；**绝不带凭证值**）。
fn public_view(session: &AccountSession) -> Value {
    json!({
        "id": session.id,
        "provider": session.provider,
        "name": session.name,
        "models": session.models,
        "base_url": session.base_url,
        "chat_path": session.chat_path,
        "status": session.status,
        "proxy": session.proxy,
        // 具名字段只给名字不给值：插件据此决定要刷新哪些字段
        "fields": session.credential.field_names(),
    })
}

fn bad_request(message: impl Into<String>) -> RpcError {
    RpcError {
        message: message.into(),
        code: Some("bad_request".into()),
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
    fn store_get_returns_secret_and_public() {
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
            assert_eq!(got["secret"], "sk-alice-token");
            assert_eq!(got["public"]["id"], created.id);
            assert_eq!(got["public"]["provider"], "kiro");
            assert_eq!(got["public"]["name"], "alice");
            // 明文只出现在返回给插件的 secret 里：日志侧不允许出现
            //（本测试只校验返回形状；防泄漏靠 log/notify 只记文案）
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
            assert_eq!(got["secret"], "sk-alice");
            assert_eq!(got["public"]["id"], first.id);

            // 只匹配该 provider：别的家的账号不进这个队列
            let other = sessions
                .upsert(account("ch2", "carol", "astudio", "sk-carol", 3))
                .unwrap();
            let got = callbacks
                .handle("store.get", json!({"provider": "kiro"}))
                .unwrap();
            assert_eq!(got["public"]["id"], first.id);
            assert_ne!(got["public"]["id"], other.id);

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
                assert!(!v["secret"].as_str().unwrap().is_empty());
                assert_eq!(v["public"]["provider"], "kiro");
            }
        });
    }

    #[test]
    fn store_update_is_compare_then_write() {
        with_secret_dir("store-update", || {
            let (sessions, callbacks, _store) = harness("kiro-pack");
            let created = sessions
                .upsert(account("ch1", "alice", "kiro", "sk-old", 1))
                .unwrap();

            // 期望值相符 → 写入成功
            let got = callbacks
                .handle(
                    "store.update",
                    json!({"account": created.id, "expect": "sk-old", "secret": "sk-new"}),
                )
                .unwrap();
            assert_eq!(got["updated"], true);
            assert_eq!(
                sessions.get(&created.id).unwrap().credential.secret_plain(),
                "sk-new"
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
                sessions.get(&created.id).unwrap().credential.secret_plain(),
                "sk-new"
            );

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
                    json!({"account": created.id, "expect": "sk-new", "secret": ""}),
                )
                .unwrap_err();
            assert_eq!(err.code.as_deref(), Some("bad_request"));
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
        });
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
