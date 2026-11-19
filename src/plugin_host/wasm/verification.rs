// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 宿主签发的「验证租约」—— 一次领取等待跨调用持有的有效期票据。
//!
//! ## 对齐来源
//!
//! 对应上游 `plugin_host::wasm::verification` 的租约面
//! （`verification.lease.open|check|close`）与 `verification/lease/transaction`
//! 的「与状态 CAS 同锁提交」语义。AIGX 的 Wasm 宿主没有上游那套「宿主自有
//! 账号验证调用短通道」，因此这里落地**租约原语 + 事务提交**两条通用的、
//! 自洽的能力：插件可为一个当前账号开一个带有效期的租约，跨调用轮询状态，
//! 并在一次 `state.compare_exchange` 成功时把租约作为一次性票据消费掉
//! （CAS 失败则不消费，留给下一次重试）。
//!
//! ## 边界（诚实）
//!
//! - 只覆盖租约与事务提交；`captcha.push` / `claim.verification.*` 这类需要
//!   宿主自有账号验证管线的短通道**不在此模块**，契约里单列为未服务。
//! - 租约严格绑定 provider × account × identity（与上游同口径），一旦账号
//!   归属或身份变化，旧租约在 check 时即判无效。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

/// 租约主能力（与上游同名；短通道 `captcha.push` / `claim.verification.*`
/// 未在本模块实现，故不作为独立常量导出）。
/// 领取租约所需能力（与上游同名）。
pub(crate) const CLAIM_CAPABILITY: &str = "verification.claim.v1";
/// 单次租约最长等待（与上游 `MAX_WAIT_MS` 一致：50s）。
pub(crate) const MAX_WAIT_MS: u64 = 50_000;
/// 单次交互消息上限（与上游 `MAX_MESSAGE` 一致）。
pub(crate) const MAX_MESSAGE: usize = 256 * 1024;
/// 全局在途租约上限（与上游 `MAX_PENDING` 一致）。
const MAX_PENDING: usize = 128;

struct Entry {
    provider: String,
    account: String,
    identity: String,
    label: Option<String>,
    deadline_ms: i64,
    expires_at: i64,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, Entry>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Registry::default()))
}

fn now_ms() -> i64 {
    super::now_ms()
}

fn busy(message: &str) -> WireError {
    WireError::with_code(message, "verification_busy")
}

fn forbidden(message: &str) -> WireError {
    WireError::with_code(message, "forbidden")
}

fn bad_params(message: &str) -> WireError {
    WireError::with_code(message, "bad_params")
}

impl Registry {
    fn prune(&mut self) {
        let now = now_ms();
        self.entries
            .retain(|_, entry| entry.deadline_ms > now && entry.expires_at > now);
    }
}

/// 摘除某 provider 的全部租约（停用 / 卸载时调用）。
pub(crate) fn forget_provider(provider: &str) {
    if let Ok(mut registry) = registry().lock() {
        registry
            .entries
            .retain(|_, entry| entry.provider != provider);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Open {
    #[serde(default)]
    ttl_ms: u64,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    request_id: String,
}

fn random_lease_id() -> Result<String, WireError> {
    let mut nonce = [0u8; 32];
    getrandom::getrandom(&mut nonce)
        .map_err(|_| WireError::with_code("验证租约随机源不可用", "internal"))?;
    Ok(format!(
        "vclaim_{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

impl Context {
    /// 分派 `verification.lease.open|check|close`。
    ///
    /// 全部要求 `verification.claim.v1`；租约绑定当前调用账号与身份。
    pub(super) async fn verification_lease(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        self.permission(CLAIM_CAPABILITY)?;
        if serde_json::to_vec(params)
            .map_err(|_| bad_params("租约参数不可编码"))?
            .len()
            > MAX_MESSAGE
        {
            return Err(WireError::with_code(
                "验证租约请求超过大小限制",
                "plugin_resource_limit",
            ));
        }
        match method {
            "verification.lease.open" => self.lease_open(params),
            "verification.lease.check" => self.lease_check(params),
            "verification.lease.close" => self.lease_close(params),
            _ => Err(forbidden("未知的验证租约操作")),
        }
    }

    fn lease_open(&self, params: &Value) -> Result<Value, WireError> {
        let input: Open =
            serde_json::from_value(params.clone()).map_err(|_| bad_params("验证租约参数无效"))?;
        if input.ttl_ms == 0 {
            return Err(bad_params("验证租约有效期必须大于 0"));
        }
        if let Some(label) = &input.label {
            if label.len() > 256 || label.chars().any(char::is_control) {
                return Err(bad_params("验证租约标签无效"));
            }
        }
        let provider = self.shared.manifest.provider.clone();
        let account = self.account()?.to_string();
        let identity = self.identity.clone();
        let budget = input.ttl_ms.min(MAX_WAIT_MS);
        let deadline_ms = now_ms().saturating_add(budget as i64);
        let id = random_lease_id()?;
        {
            let mut registry = registry()
                .lock()
                .map_err(|_| WireError::with_code("验证租约表不可用", "internal"))?;
            registry.prune();
            // 一个账号同一时刻只允许一个在途租约（与上游同口径）。
            if registry
                .entries
                .values()
                .any(|entry| entry.provider == provider && entry.account == account)
            {
                return Err(busy("当前账号已有领取等待验证，或等待队列已满"));
            }
            if registry.entries.len() >= MAX_PENDING {
                return Err(busy("验证租约等待队列已满"));
            }
            registry.entries.insert(
                id.clone(),
                Entry {
                    provider,
                    account,
                    identity,
                    label: input.label,
                    deadline_ms,
                    expires_at: deadline_ms,
                },
            );
        }
        Ok(json!({
            "requestId": id,
            "expiresAt": deadline_ms,
            "remainingMs": budget,
        }))
    }

    fn lease_check(&self, params: &Value) -> Result<Value, WireError> {
        let input: Request =
            serde_json::from_value(params.clone()).map_err(|_| bad_params("验证租约标识无效"))?;
        if input.request_id.is_empty() || input.request_id.len() > 128 {
            return Err(bad_params("验证租约标识无效"));
        }
        self.account()?;
        let provider = self.shared.manifest.provider.clone();
        let account = self.account()?.to_string();
        let mut registry = registry()
            .lock()
            .map_err(|_| WireError::with_code("验证租约表不可用", "internal"))?;
        registry.prune();
        let Some(entry) = registry.entries.get(&input.request_id) else {
            return Ok(json!({ "valid": false, "status": "invalid" }));
        };
        if entry.provider != provider || entry.account != account || entry.identity != self.identity
        {
            return Err(forbidden("验证租约不属于当前调用或账号"));
        }
        let remaining = entry.deadline_ms.saturating_sub(now_ms()).max(0) as u64;
        Ok(json!({
            "valid": true,
            "status": "waiting",
            "label": entry.label,
            "expiresAt": entry.expires_at,
            "remainingMs": remaining,
        }))
    }

    fn lease_close(&self, params: &Value) -> Result<Value, WireError> {
        let input: Request =
            serde_json::from_value(params.clone()).map_err(|_| bad_params("验证租约标识无效"))?;
        if input.request_id.is_empty() || input.request_id.len() > 128 {
            return Err(bad_params("验证租约标识无效"));
        }
        self.account()?;
        let provider = self.shared.manifest.provider.clone();
        let account = self.account()?.to_string();
        let mut registry = registry()
            .lock()
            .map_err(|_| WireError::with_code("验证租约表不可用", "internal"))?;
        registry.prune();
        let owned = registry
            .entries
            .get(&input.request_id)
            .is_some_and(|entry| {
                entry.provider == provider
                    && entry.account == account
                    && entry.identity == self.identity
            });
        if !owned {
            // 不属于当前调用：不泄露存在性，按未关闭返回（与上游一致）。
            if registry.entries.contains_key(&input.request_id) {
                return Err(forbidden("验证租约不属于当前调用或账号"));
            }
            return Ok(json!({ "closed": false }));
        }
        registry.entries.remove(&input.request_id);
        Ok(json!({ "closed": true }))
    }
}

/// 与状态 CAS 同锁提交：一次绑定了租约的 `state.compare_exchange` 成功后，
/// 租约作为一次性票据被消费；CAS 失败（`updated=false`）则不消费，留给
/// 下一次重试（对齐上游「检查失败或版本冲突均不能占用/消费租约」）。
///
/// 若 `updated == true` 但租约不属于当前调用/账号，直接忽略（不误删他人租约）。
pub(super) fn on_state_commit(context: &Context, lease_id: &str, updated: bool) {
    if !updated {
        return;
    }
    let provider = context.shared.manifest.provider.clone();
    let account = context.account.clone();
    let identity = context.identity.clone();
    if let Ok(mut registry) = registry().lock() {
        let owned = registry.entries.get(lease_id).is_some_and(|entry| {
            entry.provider == provider && entry.account == account && entry.identity == identity
        });
        if owned {
            registry.entries.remove(lease_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_prune_drops_expired() {
        let mut registry = Registry::default();
        registry.entries.insert(
            "x".into(),
            Entry {
                provider: "p".into(),
                account: "a".into(),
                identity: "i".into(),
                label: None,
                deadline_ms: now_ms() - 1,
                expires_at: now_ms() - 1,
            },
        );
        registry.prune();
        assert!(registry.entries.is_empty());
    }

    #[test]
    fn registry_keeps_live_entries() {
        let mut registry = Registry::default();
        registry.entries.insert(
            "y".into(),
            Entry {
                provider: "p".into(),
                account: "a".into(),
                identity: "i".into(),
                label: Some("captcha".into()),
                deadline_ms: now_ms() + 30_000,
                expires_at: now_ms() + 30_000,
            },
        );
        registry.prune();
        assert_eq!(registry.entries.len(), 1);
    }

    #[test]
    fn random_id_has_prefix_and_uniqueness() {
        let a = random_lease_id().unwrap();
        let b = random_lease_id().unwrap();
        assert!(a.starts_with("vclaim_"));
        assert_ne!(a, b);
    }
}
