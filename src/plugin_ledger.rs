//! 插件台账与持久状态的**严格读取**（对齐 Agent2API v3.5.0-beta.2
//! `core/account_store/plugin_ledger.rs` + `plugin_state/strict.rs`）。
//!
//! 核心口径：**读取失败不是空台账**。缺失（Missing）、损坏（Corrupt）、
//! 有效（Valid）必须三态区分——不能把损坏记录当作缺失继而覆盖，也不能因为
//! 读不到就返回空对象让调用方以为「没有数据」。
//!
//! 参考树基于 SQLite 账号行；AIGX 这里提供**与存储无关**的 `serde_json::Value`
//! 版本，账号行/插件公共字段用同一套判据。字段布局与参考树一致：
//! - 台账：`pluginLedger.<kind>`（并对 `onboarding` / `onboardingSettled` 做历史别名回落）；
//! - 持久状态：`pluginState.<key> = { revision, schema, value }`。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use std::io::Write;

use serde_json::{json, Map, Value};

/// 严格读取三态。
#[derive(Clone, Debug, PartialEq)]
pub enum StrictRead {
    /// 记录/条目不存在（调用方据此返回「缺失」，**不可**当作空值覆盖）。
    Missing,
    /// 记录存在但格式损坏。
    Corrupt(String),
    /// 有效值。
    Valid(Value),
}

impl StrictRead {
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Self::Corrupt(_))
    }
}

/// 台账取值：`pluginLedger.<kind>`，并保留参考树的历史别名回落顺序。
///
/// 显式 `null` 与缺键不同：显式 null 直接作为值返回，**不**继续回落。
pub fn ledger_value(record: &Value, kind: &str) -> Value {
    let primary = record.pointer(&format!("/pluginLedger/{kind}"));
    if let Some(value) = primary {
        return value.clone();
    }
    let alias = match kind {
        "onboarding" => record
            .pointer("/pluginLedger/onboardingSettled")
            .or_else(|| record.get("onboardingSettled")),
        "onboardingSettled" => record
            .pointer("/pluginLedger/onboarding")
            .or_else(|| record.get(kind)),
        "onboardingGrants" => record.get(kind),
        _ => None,
    };
    alias.cloned().unwrap_or(Value::Null)
}

/// 严格读取台账：缺账号/账号归属变化/格式无效一律返回错误，绝不返回空台账。
pub fn plugin_ledger_strict(
    provider: &str,
    id: &str,
    kind: &str,
    record: Option<&Value>,
) -> Result<Value, String> {
    if provider.is_empty() || id.is_empty() || kind.is_empty() || kind.len() > 80 {
        return Err("插件台账读取缺少有效的账号归属或台账名称".into());
    }
    let record = record.ok_or("插件台账所属账号不存在，未返回空台账")?;
    if record.get("id").and_then(Value::as_str) != Some(id) {
        return Err("插件台账所属账号已变化，未读取台账".into());
    }
    if record.get("provider").and_then(Value::as_str) != Some(provider) {
        return Err("插件台账所属提供商已变化，未读取台账".into());
    }
    Ok(ledger_value(record, kind))
}

// ── 持久状态（严格三态 + 大小预算 + CAS）─────────────────────────────

mod strict {
    use super::*;

    const MAX_ENTRY_BYTES: usize = 256 * 1024;
    const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
    const MAX_ENTRIES: usize = 128;

    fn invalid() -> String {
        "账号插件状态格式无效，未将损坏记录当作缺失或覆盖".to_string()
    }

    fn target(provider: &str, id: &str, key: &str) -> Result<(), String> {
        if provider.is_empty()
            || provider.len() > 128
            || id.is_empty()
            || id.len() > 512
            || key.is_empty()
            || key.len() > 160
            || provider
                .chars()
                .chain(id.chars())
                .chain(key.chars())
                .any(char::is_control)
        {
            return Err("插件状态缺少有效的账号归属或状态名称".into());
        }
        Ok(())
    }

    fn container(record: &Value) -> Result<Option<&Map<String, Value>>, String> {
        match record.get("pluginState") {
            None => Ok(None),
            Some(Value::Object(state)) => {
                if state.len() > MAX_ENTRIES || !within_limit(state, MAX_STATE_BYTES) {
                    return Err(invalid());
                }
                Ok(Some(state))
            }
            Some(_) => Err(invalid()),
        }
    }

    fn entry(value: &Value) -> Result<(u64, u32), String> {
        let fields = value.as_object().ok_or_else(invalid)?;
        if fields.len() != 3 || !fields.contains_key("value") {
            return Err(invalid());
        }
        let revision = fields
            .get("revision")
            .and_then(Value::as_u64)
            .filter(|revision| *revision > 0)
            .ok_or_else(invalid)?;
        let schema = fields
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|schema| u32::try_from(schema).ok())
            .filter(|schema| *schema > 0)
            .ok_or_else(invalid)?;
        if !within_limit(&fields["value"], MAX_ENTRY_BYTES) {
            return Err(invalid());
        }
        Ok((revision, schema))
    }

    /// 严格读取单个状态条目：缺失 → [`StrictRead::Missing`]；损坏 → `Corrupt`。
    pub fn plugin_state_strict(
        provider: &str,
        id: &str,
        key: &str,
        record: Option<&Value>,
    ) -> Result<StrictRead, String> {
        target(provider, id, key)?;
        let record = record.ok_or("插件状态所属账号不存在，未返回空状态")?;
        if record.get("id").and_then(Value::as_str) != Some(id)
            || record.get("provider").and_then(Value::as_str) != Some(provider)
        {
            return Err("插件状态所属账号或提供商已变化".into());
        }
        let state = match container(record) {
            Ok(state) => state,
            Err(error) => return Ok(StrictRead::Corrupt(error)),
        };
        let Some(value) = state.and_then(|state| state.get(key)) else {
            return Ok(StrictRead::Missing);
        };
        if let Err(error) = entry(value) {
            return Ok(StrictRead::Corrupt(error));
        }
        let mut result = value.clone();
        result["present"] = json!(true);
        Ok(StrictRead::Valid(result))
    }

    /// 比较-再写（在给定账号记录上就地修改 `pluginState`）。
    ///
    /// 返回 `{updated, revision}`；`expected` 不匹配返回 `updated=false`。
    pub fn compare_exchange(
        record: &mut Value,
        key: &str,
        expected: u64,
        schema: u32,
        value: Value,
    ) -> Result<Value, String> {
        if schema == 0 || !within_limit(&value, MAX_ENTRY_BYTES) {
            return Err("插件状态版本或大小无效".into());
        }
        if !record.is_object() {
            return Err(invalid());
        }
        let current = container(record)?.cloned().unwrap_or_default();
        let revision = current
            .get(key)
            .map(entry)
            .transpose()?
            .map(|(revision, _)| revision)
            .unwrap_or(0);
        if revision != expected {
            return Ok(json!({ "updated": false, "revision": revision }));
        }
        let mut state = current;
        if !state.contains_key(key) && state.len() >= MAX_ENTRIES {
            return Err("账号插件状态条数超过上限".into());
        }
        let revision = revision.checked_add(1).ok_or("账号插件状态版本已耗尽")?;
        state.insert(
            key.to_string(),
            json!({ "revision": revision, "schema": schema, "value": value }),
        );
        if !within_limit(&state, MAX_STATE_BYTES) {
            return Err("账号插件状态总量超过 2 MiB".into());
        }
        record["pluginState"] = Value::Object(state);
        Ok(json!({ "updated": true, "revision": revision }))
    }

    struct Budget {
        used: usize,
        limit: usize,
    }

    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.used = self
                .used
                .checked_add(bytes.len())
                .filter(|used| *used <= self.limit)
                .ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "state limit")
                })?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn within_limit(value: &impl serde::Serialize, limit: usize) -> bool {
        serde_json::to_writer(Budget { used: 0, limit }, value).is_ok()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn account() -> Value {
            json!({ "id": "acc-1", "provider": "acme", "pluginState": {} })
        }

        #[test]
        fn missing_is_distinct_from_corrupt() {
            let record = account();
            assert_eq!(
                plugin_state_strict("acme", "acc-1", "theme", Some(&record)).unwrap(),
                StrictRead::Missing
            );
            // 损坏：pluginState 不是对象
            let bad = json!({ "id": "acc-1", "provider": "acme", "pluginState": 42 });
            assert!(plugin_state_strict("acme", "acc-1", "theme", Some(&bad))
                .unwrap()
                .is_corrupt());
        }

        #[test]
        fn compare_exchange_revision_guard() {
            let mut record = account();
            let out = compare_exchange(&mut record, "theme", 0, 1, json!("dark")).unwrap();
            assert_eq!(out["updated"], json!(true));
            assert_eq!(out["revision"], json!(1));
            let stale = compare_exchange(&mut record, "theme", 0, 1, json!("light")).unwrap();
            assert_eq!(stale["updated"], json!(false));
            let read = plugin_state_strict("acme", "acc-1", "theme", Some(&record)).unwrap();
            assert!(matches!(read, StrictRead::Valid(_)));
        }
    }
}

#[allow(unused_imports)]
pub use strict::{compare_exchange as plugin_state_compare_exchange, plugin_state_strict};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ledger_alias_fallback_and_null_kept() {
        let record = json!({ "onboardingSettled": "yes" });
        assert_eq!(ledger_value(&record, "onboarding"), json!("yes"));
        // 显式 null 不回落
        let record = json!({ "pluginLedger": { "onboarding": null }, "onboardingSettled": "yes" });
        assert_eq!(ledger_value(&record, "onboarding"), Value::Null);
    }

    #[test]
    fn strict_ledger_refuses_missing_account() {
        assert!(plugin_ledger_strict("acme", "acc-1", "onboarding", None).is_err());
        let record =
            json!({ "id": "acc-1", "provider": "acme", "pluginLedger": { "onboarding": true } });
        assert_eq!(
            plugin_ledger_strict("acme", "acc-1", "onboarding", Some(&record)).unwrap(),
            json!(true)
        );
        // provider 不符 → 错误
        assert!(plugin_ledger_strict("other", "acc-1", "onboarding", Some(&record)).is_err());
    }
}
