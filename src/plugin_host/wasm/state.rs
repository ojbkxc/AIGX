// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 插件状态的三档作用域 —— `login`（临时）/ `conversation`（临时，按账号+身份
//! +会话隔离）/ `account`（持久，走宿主 CAS 存储）。
//!
//! 与上游逐字一致：临时档用进程内表 + TTL；持久档走 [`WasmAccountAccess`] 的
//! `plugin_state*`（AIGX 侧由 integrator 接到账号存储的 CAS 语义上）。
//!
//! 容量口径（上游常量）：
//! - 单条 value ≤256 KiB，总表 ≤64 MiB，单 namespace ≤32/64 条
//! - provider 维度 ≤1024 条 / ≤16 MiB

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

#[derive(Clone)]
struct Entry {
    revision: u64,
    schema: u32,
    expires: i64,
    value: Value,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct Transient {
    entries: HashMap<String, Entry>,
    sequence: u64,
}

fn conversations() -> &'static Mutex<Transient> {
    static STATE: OnceLock<Mutex<Transient>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Transient::default()))
}

pub(crate) fn forget_provider(provider: &str) {
    if let Ok(mut state) = conversations().lock() {
        state
            .entries
            .retain(|key, _| !key.starts_with(&format!("{provider}\0")));
    }
}

impl Transient {
    pub fn remove_namespace(&mut self, namespace: &str) {
        let prefix = format!("{namespace}\0");
        self.entries.retain(|key, _| !key.starts_with(&prefix));
    }

    fn invoke(
        &mut self,
        method: &str,
        namespace: &str,
        key: &str,
        params: &Value,
        max_ttl: i64,
        limit: usize,
    ) -> Result<Value, WireError> {
        let now = super::now_ms();
        self.entries.retain(|_, entry| entry.expires > now);
        let id = format!("{namespace}\0{key}");
        let previous = self.entries.get(&id).cloned();
        let schema = params["schema"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|schema| *schema > 0)
            .ok_or_else(|| WireError::with_code("状态必须声明格式版本", "bad_params"))?;
        if method == "state.get" {
            return Ok(previous
                .map(|entry| {
                    json!({ "revision": entry.revision, "schema": entry.schema,
                        "value": entry.value, "compatible": entry.schema == schema,
                        "expires_at": entry.expires })
                })
                .unwrap_or_else(
                    || json!({ "revision": 0, "schema": schema, "value": null, "compatible": true }),
                ));
        }
        let expected = params["revision"]
            .as_u64()
            .ok_or_else(|| super::wire::error("状态写入必须携带读取版本"))?;
        let revision = previous.as_ref().map(|entry| entry.revision).unwrap_or(0);
        if revision != expected {
            return Ok(json!({ "updated": false, "revision": revision }));
        }
        let ttl = params["ttl_ms"]
            .as_i64()
            .unwrap_or(max_ttl)
            .clamp(1000, max_ttl);
        let value = params["value"].clone();
        let bytes = value.to_string().len();
        let total: usize = self.entries.values().map(|entry| entry.bytes).sum();
        let namespaced = self
            .entries
            .keys()
            .filter(|entry| entry.starts_with(&format!("{namespace}\0")))
            .count();
        if let Some((provider, _)) = namespace.split_once('\0') {
            let prefix = format!("{provider}\0");
            let provider_entries: Vec<_> = self
                .entries
                .iter()
                .filter(|(key, _)| key.starts_with(&prefix))
                .collect();
            let provider_bytes: usize = provider_entries.iter().map(|(_, entry)| entry.bytes).sum();
            if provider_entries.len() >= 1024 && previous.is_none()
                || provider_bytes
                    .saturating_sub(previous.as_ref().map(|entry| entry.bytes).unwrap_or(0))
                    + bytes
                    > 16 * 1024 * 1024
            {
                return Err(WireError::with_code(
                    "提供商会话状态超过容量限制",
                    "plugin_resource_limit",
                ));
            }
        }
        if bytes > 256 * 1024
            || total.saturating_sub(previous.as_ref().map(|entry| entry.bytes).unwrap_or(0)) + bytes
                > 64 * 1024 * 1024
            || previous.is_none() && (namespaced >= limit || self.entries.len() >= 4096)
        {
            return Err(WireError::with_code(
                "插件临时状态超过容量限制",
                "plugin_resource_limit",
            ));
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| super::wire::error("状态版本已耗尽"))?;
        let revision = self.sequence;
        self.entries.insert(
            id,
            Entry {
                revision,
                schema,
                expires: now + ttl,
                value,
                bytes,
            },
        );
        Ok(json!({ "updated": true, "revision": revision }))
    }
}

impl Context {
    pub(super) async fn state(&mut self, method: &str, params: &Value) -> Result<Value, WireError> {
        self.permission("state.v1")?;
        let key = params["key"]
            .as_str()
            .filter(|key| !key.is_empty() && key.len() <= 128 && !key.contains('\0'))
            .ok_or_else(|| super::wire::error("插件状态名称无效"))?;
        if params.get("conversation").is_some_and(|value| {
            value
                .as_str()
                .is_none_or(|key| key.is_empty() || key.len() > 256 || key.contains('\0'))
        }) {
            return Err(super::wire::error("会话状态标识无效"));
        }
        let provider = self.shared.manifest.provider.clone();
        let schema = params["schema"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .filter(|schema| *schema > 0)
            .ok_or_else(|| super::wire::error("插件状态格式版本无效"))?;
        match params["scope"].as_str() {
            Some("login") => {
                if self.flow.is_empty() {
                    return Err(super::wire::error("当前调用未绑定登录流程"));
                }
                let namespace = format!("{}:{}", provider, self.flow);
                self.shared
                    .login_state
                    .lock()
                    .map_err(|_| super::wire::error("登录状态不可用"))?
                    .invoke(method, &namespace, key, params, 900_000, 64)
            }
            Some("conversation") => {
                let account = self.account()?;
                let session = params["conversation"]
                    .as_str()
                    .filter(|key| !key.is_empty() && key.len() <= 256 && !key.contains('\0'))
                    .map(|key| format!("plugin:{key}"))
                    .unwrap_or_else(|| self.session.clone());
                if session.is_empty() {
                    return Err(super::wire::error("当前请求缺少会话标识"));
                }
                use sha2::{Digest, Sha256};
                let identity = format!("{:x}", Sha256::digest(self.identity.as_bytes()));
                let namespace = format!("{provider}\0{account}\0{identity}\0{session}");
                conversations()
                    .lock()
                    .map_err(|_| super::wire::error("会话状态不可用"))?
                    .invoke(method, &namespace, key, params, 86_400_000, 32)
            }
            Some("account") => {
                let account = self.account()?.to_string();
                let key = format!("plugin:{key}");
                let params = params.clone();
                let get = method == "state.get";
                let accounts = self.shared.accounts.clone();
                if get {
                    let mut state = accounts.plugin_state(&provider, &account, &key).await?;
                    state["compatible"] =
                        json!(state["revision"] == 0 || state["schema"] == schema);
                    Ok(state)
                } else {
                    let revision = params["revision"]
                        .as_u64()
                        .ok_or_else(|| super::wire::error("状态写入缺少读取版本"))?;
                    accounts
                        .plugin_state_compare_exchange(
                            &provider,
                            &account,
                            &key,
                            revision,
                            schema,
                            params["value"].clone(),
                        )
                        .await
                }
            }
            _ => Err(super::wire::error(
                "状态范围必须是 login、conversation 或 account",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invoke(state: &mut Transient, method: &str, params: Value) -> Result<Value, WireError> {
        state.invoke(method, "p\0a", "k", &params, 60_000, 32)
    }

    #[test]
    fn get_missing_returns_revision_zero() {
        let mut state = Transient::default();
        let value = invoke(&mut state, "state.get", json!({ "schema": 1 })).unwrap();
        assert_eq!(value["revision"], 0);
        assert_eq!(value["compatible"], true);
    }

    #[test]
    fn compare_exchange_flow() {
        let mut state = Transient::default();
        let written = invoke(
            &mut state,
            "state.compare_exchange",
            json!({ "schema": 1, "revision": 0, "value": { "a": 1 } }),
        )
        .unwrap();
        assert_eq!(written["updated"], true);
        let revision = written["revision"].as_u64().unwrap();
        // 旧版本再写 -> 冲突
        let conflict = invoke(
            &mut state,
            "state.compare_exchange",
            json!({ "schema": 1, "revision": 0, "value": { "a": 2 } }),
        )
        .unwrap();
        assert_eq!(conflict["updated"], false);
        // 新版本可写
        let ok = invoke(
            &mut state,
            "state.compare_exchange",
            json!({ "schema": 1, "revision": revision, "value": { "a": 3 } }),
        )
        .unwrap();
        assert_eq!(ok["updated"], true);
        let read = invoke(&mut state, "state.get", json!({ "schema": 1 })).unwrap();
        assert_eq!(read["value"]["a"], 3);
    }

    #[test]
    fn schema_mismatch_marks_incompatible() {
        let mut state = Transient::default();
        invoke(
            &mut state,
            "state.compare_exchange",
            json!({ "schema": 1, "revision": 0, "value": 1 }),
        )
        .unwrap();
        let read = invoke(&mut state, "state.get", json!({ "schema": 2 })).unwrap();
        assert_eq!(read["compatible"], false);
    }

    #[test]
    fn missing_schema_is_bad_params() {
        let mut state = Transient::default();
        let error = invoke(&mut state, "state.get", json!({})).unwrap_err();
        assert_eq!(error.code.as_deref(), Some("bad_params"));
    }

    #[test]
    fn remove_namespace_drops_entries() {
        let mut state = Transient::default();
        state
            .invoke(
                "state.compare_exchange",
                "p:flow1",
                "k",
                &json!({ "schema": 1, "revision": 0, "value": 1 }),
                900_000,
                64,
            )
            .unwrap();
        state.remove_namespace("p:flow1");
        let read = state
            .invoke(
                "state.get",
                "p:flow1",
                "k",
                &json!({ "schema": 1 }),
                900_000,
                64,
            )
            .unwrap();
        assert_eq!(read["revision"], 0);
    }
}
