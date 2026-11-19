//! 本机启动确认（对齐 Agent2API v3.5.0-beta.2
//! `core/account_store/desktop_launch.rs` 的语义）。
//!
//! 参考树用 `LaunchOrigin` 把「一次本机启动准备」绑定到具体账号行的
//! `rowid + added_at + secret` 三元组：准备期间账号被改动（并发写入触发了
//! 重排）或来源被恢复/切换，`check` 都会失败，要求重新发起操作。写回时也
//! 复核同一份凭据，并回显写后的值。
//!
//! AIGX 无 SQL 行号，适配为「账号 id + provider + 凭证明文 + updated_at」：
//! - [`AccountSessionStore`] 是唯一事实来源；
//! - [`LaunchOrigin::check`] 复核快照是否仍一致；
//! - [`LaunchOrigin::update`] 走 `update_credential_cas`（旧值比较-再写）；
//! - [`LaunchOrigin::accept`] 交回插件准备结果对应的凭据。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use std::sync::Mutex;

use crate::account_session::model::Credential;
use crate::account_session::store::{AccountSessionStore, CasOutcome};
use serde_json::Value;

/// 一次本机启动的原始快照。
#[derive(Clone, Debug)]
struct Snapshot {
    id: String,
    provider: String,
    secret: String,
    updated_at: i64,
}

/// 启动确认令牌：Drop 时清空暂存（不保留明文）。
pub struct LaunchOrigin {
    store: std::sync::Arc<AccountSessionStore>,
    initial: Snapshot,
    accepted: Mutex<Snapshot>,
}

impl LaunchOrigin {
    /// 读取当前账号行建立启动快照；账号不存在/provider 不符即失败。
    pub fn open(
        store: std::sync::Arc<AccountSessionStore>,
        provider: &str,
        id: &str,
    ) -> Result<Self, String> {
        if provider.is_empty() || id.is_empty() {
            return Err("启动确认缺少有效的账号归属".into());
        }
        let session = store.get(id).ok_or("账号不存在")?;
        if !provider.is_empty() && session.provider != provider {
            return Err("账号不属于当前提供商".into());
        }
        let snapshot = Snapshot {
            id: id.to_string(),
            provider: session.provider.clone(),
            secret: session.credential.secret_plain(),
            updated_at: session.updated_at,
        };
        Ok(Self {
            store,
            initial: snapshot.clone(),
            accepted: Mutex::new(snapshot),
        })
    }

    /// 初始快照（不含 updated_at 之外的内部字段）。
    pub fn initial(&self) -> Value {
        serde_json::json!({
            "identity": self.initial.id,
            "provider": self.initial.provider,
        })
    }

    /// 复核：账号仍是同一 provider、凭证与 updated_at 未变。
    pub fn check(&self) -> Result<(), String> {
        let session = self.store.get(&self.initial.id).ok_or("账号不存在")?;
        if session.provider != self.initial.provider
            || session.updated_at != self.initial.updated_at
            || session.credential.secret_plain() != self.initial.secret
        {
            return Err("账号在启动准备期间已变化，请重新发起操作".into());
        }
        Ok(())
    }

    /// 校验一份外部读到的账号记录是否与本次启动一致。
    pub fn validate_record(&self, id: &str, provider: &str, secret: &str) -> Result<(), String> {
        if id != self.initial.id || provider != self.initial.provider {
            return Err("账号读取来源与本次启动不符".into());
        }
        let accepted = self.accepted.lock().map_err(|_| "账号启动凭证来源不可用")?;
        if secret != accepted.secret {
            return Err("账号在启动准备期间已变化，请重新发起操作".into());
        }
        drop(accepted);
        self.check()
    }

    /// 写回：旧值比较-再写，成功后刷新 accepted 快照。
    pub fn update(&self, expect_secret: &str, next: Credential) -> Result<Value, String> {
        let mut accepted = self.accepted.lock().map_err(|_| "账号启动凭证来源不可用")?;
        if accepted.secret != expect_secret {
            return Err("账号写回不是本次启动读取的凭证".into());
        }
        self.check()?;
        let next_secret = next.secret_plain();
        match self
            .store
            .update_credential_cas(&self.initial.id, expect_secret, next)
        {
            CasOutcome::Updated => {
                accepted.secret = next_secret;
                accepted.updated_at = self
                    .store
                    .get(&self.initial.id)
                    .map(|s| s.updated_at)
                    .unwrap_or(accepted.updated_at);
                Ok(serde_json::json!({ "updated": true }))
            }
            CasOutcome::Stale => Err("账号凭证已被并发刷新，本次写回被拒绝".into()),
            CasOutcome::Missing => Err("账号不存在".into()),
        }
    }

    /// 接受插件准备结果：必须与本次启动凭证一致且仍有效。
    pub fn accept(&self, secret: &str) -> Result<String, String> {
        let accepted = self.accepted.lock().map_err(|_| "账号启动凭证来源不可用")?;
        if accepted.secret != secret {
            return Err("插件准备结果不是本次账号凭证".into());
        }
        drop(accepted);
        self.check()?;
        Ok(self
            .accepted
            .lock()
            .map(|s| s.secret.clone())
            .unwrap_or_default())
    }
}

impl Drop for LaunchOrigin {
    fn drop(&mut self) {
        // 清除暂存明文（best-effort）。
        if let Ok(mut accepted) = self.accepted.lock() {
            accepted.secret.clear();
        }
    }
}
