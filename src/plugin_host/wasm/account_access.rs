// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! [`WasmAccountAccess`] 的 AIGX 实现 —— 把沙箱的宿主能力面转发到账号池与
//! 既有插件回调。
//!
//! ## 为什么需要这一层
//!
//! wasm 沙箱只认 [`WasmAccountAccess`] 这个 trait（不直接依赖
//! `account_session`，见 `wasm/access.rs` 的模块注释）。本文件是**唯一的
//! 桥**：持 `Arc<AccountSessionStore>` + 一个 [`PluginCallbacks`]（settings /
//! ledger 的 KV 前缀隔离由它负责），把 10 个 trait 方法逐个转发到既有实现。
//!
//! ## 转发落点（逐条可查）
//!
//! | trait 方法 | 落点 |
//! |---|---|
//! | `provider_allowed` | `host_for_provider(provider).is_some()`（已装载 ∧ 活着的 wasm 宿主） |
//! | `account_provider` | `AccountSessionStore::get` → `provider` |
//! | `store_get` | `AccountSessionStore::plugin_account`（空 account 取该 provider 队首） |
//! | `store_update_with_public` | `AccountSessionStore::plugin_update_secret_cas` |
//! | `ledger_get` / `ledger_put` | `PluginCallbacks::handle("store.ledger_get/put")` |
//! | `settings_get` | `PluginCallbacks::handle("settings.get")` |
//! | `plugin_state` | `AccountSessionStore::plugin_state_get` |
//! | `plugin_state_compare_exchange` | `AccountSessionStore::plugin_state_cas` |
//! | `accounts_for_provider` | `plugin_accounts_for_provider` + `plugin_account` |

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::account_session::AccountSessionStore;
use crate::plugin_host::callbacks::PluginCallbacks;
use crate::plugin_host::rpc::CallbackHandler;
use crate::storage::FileStore;

use super::access::WasmAccountAccess;
use super::wire::WireError;

/// 账号访问面的 AIGX 实现。
///
/// `pack_id` 是 `PluginCallbacks` 的 KV 前缀（`plugin:{ns}:settings:{provider}:{key}`
/// 与 `plugin:{ns}:ledger:{account}:{kind}`）。上层按包 id 传入即可；同一
/// 包内多个 wasm provider 共享同一前缀（与原生包同口径）。
pub struct AccountSessionAccess {
    sessions: Arc<AccountSessionStore>,
    callbacks: PluginCallbacks,
}

impl AccountSessionAccess {
    pub fn new(
        pack_id: impl Into<String>,
        sessions: Arc<AccountSessionStore>,
        store: Arc<FileStore>,
    ) -> Arc<Self> {
        let pack_id = pack_id.into();
        Arc::new(Self {
            callbacks: PluginCallbacks::new(pack_id, sessions.clone(), store),
            sessions,
        })
    }

    /// 转发一个同步回调并统一错误形状。
    fn callback(&self, method: &str, params: Value) -> Result<Value, WireError> {
        self.callbacks
            .handle(method, params)
            .map_err(|error| WireError {
                message: error.message,
                code: error.code,
            })
    }

    /// 依据 provider 找该 provider 的队首账号 id（`store.get` 空 account 语义）。
    fn first_account(&self, provider: &str) -> Option<String> {
        self.sessions
            .plugin_accounts_for_provider(provider)
            .into_iter()
            .next()
            .map(|session| session.id)
    }
}

#[async_trait]
impl WasmAccountAccess for AccountSessionAccess {
    fn provider_allowed(&self, provider: &str) -> bool {
        // 「允许」= 该 provider 确实由已装载且未在关闭的 wasm 宿主接管。
        // 只看清单表会漏掉停用中的宿主（见 `wasm::owns` 的三条件判据）。
        super::owns(provider)
    }

    fn account_provider(&self, account: &str) -> Option<String> {
        self.sessions.get(account).map(|session| session.provider)
    }

    async fn store_get(&self, provider: &str, account: &str) -> Result<Value, WireError> {
        // 空 account = 取该 provider 的队首（与上游 `resolve_account` 同语义）。
        let id = if account.is_empty() {
            self.first_account(provider)
        } else {
            Some(account.to_string())
        };
        let Some(id) = id else {
            return Err(WireError::with_code(
                format!("no account for provider '{provider}'"),
                "not_found",
            ));
        };
        self.sessions
            .plugin_account(&id)
            .ok_or_else(|| WireError::with_code(format!("account '{id}' not found"), "not_found"))
    }

    async fn store_update_with_public(
        &self,
        account: &str,
        expect: Value,
        secret: Value,
        public: Value,
    ) -> Result<Value, WireError> {
        // `plugin_update_secret_cas` 的 `expect` 是插件先前读到的 `plugin_secret`
        // 原样 JSON（与 `plugin_account().secret` 同源），版本不符返回 Ok(false)。
        let updated = self
            .sessions
            .plugin_update_secret_cas(account, &expect, &secret, Some(&public))
            .map_err(super::wire::error)?;
        Ok(json!({ "updated": updated }))
    }

    async fn ledger_get(&self, account: &str, kind: &str) -> Result<Value, WireError> {
        self.callback(
            "store.ledger_get",
            json!({ "account": account, "kind": kind }),
        )
    }

    async fn ledger_put(&self, account: &str, kind: &str, value: Value) -> Result<(), WireError> {
        self.callback(
            "store.ledger_put",
            json!({ "account": account, "kind": kind, "value": value }),
        )?;
        Ok(())
    }

    async fn settings_get(&self, provider: &str, key: &str) -> Result<Value, WireError> {
        self.callback("settings.get", json!({ "provider": provider, "key": key }))
    }

    async fn plugin_state(
        &self,
        provider: &str,
        account: &str,
        key: &str,
    ) -> Result<Value, WireError> {
        self.sessions
            .plugin_state_get(provider, account, key)
            .map_err(super::wire::error)
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
        self.sessions
            .plugin_state_cas(provider, account, key, revision, schema, value)
            .map_err(super::wire::error)
    }

    fn accounts_for_provider(&self, provider: &str) -> Vec<Value> {
        self.sessions
            .plugin_accounts_for_provider(provider)
            .into_iter()
            .filter_map(|session| self.sessions.plugin_account(&session.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::model::AccountSession;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 与 `plugin_host::callbacks` 的测试同手法：拿跨模块共享锁设独立 data_dir，
    /// 防并行测试互相换密钥。
    ///
    /// 返回守卫（不吞进闭包）：这些用例体内要 `.await`，闭包版本装不下异步块。
    /// 守卫生存期 = 整个测试；`#[tokio::test]` 默认 current-thread 运行时，
    /// 持 `std` 锁跨 await 无真实阻塞风险（与 `checkin::runtime_tests` 同款例外）。
    #[must_use = "守卫必须活到测试结束；丢弃会立刻释放全局锁，隔离失效"]
    fn with_secret_dir(tag: &str) -> std::sync::MutexGuard<'static, ()> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-wasm-access-{tag}-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        guard
    }

    fn harness(pack: &str) -> (Arc<AccountSessionStore>, Arc<AccountSessionAccess>) {
        let store = Arc::new(FileStore::new(
            tempfile::TempDir::new().unwrap().path().to_path_buf(),
        ));
        let sessions = Arc::new(AccountSessionStore::new(store.clone()));
        let access = AccountSessionAccess::new(pack, sessions.clone(), store);
        (sessions, access)
    }

    fn account(id: &str, provider: &str, secret: &str) -> AccountSession {
        let mut session = AccountSession::new("ch1", id);
        session.id = id.to_string();
        session.provider = provider.to_string();
        session.credential.secret = secret.to_string();
        session
    }

    /// `store_get` 空 account 时取该 provider 队首（与上游 `resolve_account` 同语义）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn store_get_empty_account_takes_first() {
        let _guard = with_secret_dir("store-get");
        let (sessions, access) = harness("p");
        let _ = sessions.upsert(account("a1", "kiro", "sk-a1"));
        let _ = sessions.upsert(account("a2", "kiro", "sk-a2"));
        let plain = access.store_get("kiro", "").await;
        assert!(plain.is_ok(), "{plain:?}");
        let direct = access.store_get("kiro", "a2").await;
        assert_eq!(direct.unwrap()["id"], "a2");
        // 不存在的 provider -> not_found
        let missing = access.store_get("nope", "").await;
        assert_eq!(missing.unwrap_err().code.as_deref(), Some("not_found"));
    }

    /// `plugin_state` 与 `plugin_state_compare_exchange` 走账号池的 CAS 语义。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn plugin_state_roundtrip() {
        let _guard = with_secret_dir("state");
        let (sessions, access) = harness("p");
        let _ = sessions.upsert(account("a1", "kiro", "sk-a1"));
        let missing = access.plugin_state("kiro", "a1", "k").await.unwrap();
        assert_eq!(missing["revision"], 0);
        let writes = access
            .plugin_state_compare_exchange("kiro", "a1", "k", 0, 1, json!({ "v": 1 }))
            .await
            .unwrap();
        assert_eq!(writes["updated"], true);
        let read = access.plugin_state("kiro", "a1", "k").await.unwrap();
        assert_eq!(read["value"]["v"], 1);
    }

    /// `ledger_*` / `settings_get` 走 `PluginCallbacks`（pack 前缀隔离）。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn ledger_and_settings_use_pack_namespace() {
        let _guard = with_secret_dir("ledger");
        let (_sessions, access) = harness("pack-x");
        access
            .ledger_put("a1", "growth", json!({ "n": 1 }))
            .await
            .unwrap();
        let read = access.ledger_get("a1", "growth").await.unwrap();
        assert_eq!(read["n"], 1);
        // 缺失返回 null（不是错误）
        let absent = access.settings_get("kiro", "absent").await.unwrap();
        assert_eq!(absent, Value::Null);
    }

    /// `account_provider` / `accounts_for_provider` 的归属判定。
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn account_lookups_are_provider_scoped() {
        let _guard = with_secret_dir("scope");
        let (sessions, access) = harness("p");
        let _ = sessions.upsert(account("a1", "kiro", "sk-a1"));
        let _ = sessions.upsert(account("b1", "other", "sk-b1"));
        assert_eq!(access.account_provider("a1").as_deref(), Some("kiro"));
        assert_eq!(access.account_provider("missing"), None);
        let list = access.accounts_for_provider("kiro");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["id"], "a1");
        // 未装载的 provider 一律不允许
        assert!(!access.provider_allowed("kiro"));
    }
}
