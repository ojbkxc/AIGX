// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 账号排序规则（服务端）—— `GET/PUT /api/account-sessions/sorting`。
//!
//! 对齐 A2A `core/account_order.rs` + `api/accounts_sort.rs` 的可观察口径：
//!
//! - 规则**按 provider 分组**保存（每家的业务语义不同：有的按积分到期、
//!   有的按余额、有的只看加入时间）；
//! - 返回 `configured`（管理员配的，原样）与 `rules`（生效的，缺省补齐）；
//! - 同时返回每家账号的**实际先试次序**（`account_order`）和排序读数
//!   （`credit_expires_at` / `balances`），界面不用自己再算一套。
//!
//! ## credit 规则 = AIGX 的 `Expires`
//!
//! AIGX 的 `AccountSortRule` 没有单独的 credit 变体：积分到期的数据源就是
//! 余额快照 `wallets[].expireAt`（见 `session_json.credit_expires_at`），
//! 与凭证到期 `expires_at` 属于同一类「何时失效」读数。管理面因此接受
//! A2A 的 `credit` 写法并归一为 `expires`——**存储与比较只有一套语义**，
//! 不新增一个永远与 `Expires` 同值的枚举变体。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::channel::routing_policy::{AccountSortRule, SortDir};

use super::model::AccountSession;
use super::store::AccountSessionStore;

/// 排序规则的 KB 键（与轮换策略同库）。
pub const SORT_SETTINGS_KEY: &str = "acctses:account_sort_rules";

/// 每个 provider 最多几条规则（与路由档案同上限口径）。
pub const MAX_SORT_RULES: usize = 2;

/// 落盘设置：`{rules: {provider: [...]}, updated_at}`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccountSortSettings {
    #[serde(default)]
    pub rules: BTreeMap<String, Vec<AccountSortRule>>,
    #[serde(default)]
    pub updated_at: i64,
}

impl AccountSortSettings {
    /// 从账号池的 KV 读取（损坏/缺失 = 空设置，不影响启动）。
    pub fn load(store: &AccountSessionStore) -> Self {
        store
            .store()
            .get::<AccountSortSettings>(SORT_SETTINGS_KEY)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn save(&mut self, store: &AccountSessionStore) -> Result<(), String> {
        self.updated_at = chrono::Utc::now().timestamp();
        store
            .store()
            .put(SORT_SETTINGS_KEY, self)
            .map_err(|e| format!("保存账号排序规则失败: {e}"))
    }
}

/// A2A 口径的规则输入：`credit` 归一为 AIGX 的 `expires`。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum SortRuleInput {
    /// A2A 名字：按积分/套餐最早到期（→ `Expires`）
    Credit {
        dir: SortDir,
    },
    Expires {
        dir: SortDir,
    },
    Balance {
        dir: SortDir,
    },
    Added {
        dir: SortDir,
    },
    /// 配额重置（AIGX 扩展；无持久化读数时恒等，不影响其他规则）
    QuotaReset {
        dir: SortDir,
    },
}

impl From<SortRuleInput> for AccountSortRule {
    fn from(input: SortRuleInput) -> Self {
        match input {
            SortRuleInput::Credit { dir } | SortRuleInput::Expires { dir } => {
                AccountSortRule::Expires { dir }
            }
            SortRuleInput::Balance { dir } => AccountSortRule::Balance { dir },
            SortRuleInput::Added { dir } => AccountSortRule::Added { dir },
            SortRuleInput::QuotaReset { dir } => AccountSortRule::QuotaReset { dir },
        }
    }
}

/// 校验一批规则：条数上限。
///
/// 空数组**不报错**：`effective_rules` 把它与「没配」同等对待（回落默认），
/// 而「显式清空/恢复默认」的正式写法是 PUT 时传 `rules: null`（handler 会
/// 直接把该 provider 的条目移除）。两条路殊途同归，空数组不必额外拒绝。
pub fn validate_rules(rules: &[AccountSortRule]) -> Result<(), String> {
    if rules.len() > MAX_SORT_RULES {
        return Err(format!("账号排序规则最多 {MAX_SORT_RULES} 条"));
    }
    Ok(())
}

/// 生效规则：管理员配的优先；没配（或空数组）回落 A2A 的默认
/// credit 规则（`expires` 升序 = 最早到期先试）。
///
/// 只补默认**整组**、不逐条补：管理员显式配了 `[balance]` 就是「只按
/// 余额排」——悄悄再塞一条 credit 会改变他看到的顺序（与 A2A 的
/// `on:false` 语义一致：没开的维度不参与）。
pub fn effective_rules(provider: &str, settings: &AccountSortSettings) -> Vec<AccountSortRule> {
    match settings.rules.get(provider) {
        Some(configured) if !configured.is_empty() => configured.clone(),
        _ => vec![AccountSortRule::Expires { dir: SortDir::Asc }],
    }
}

/// 把规则序列化回 JSON（管理面回显）。
fn rules_json(rules: &[AccountSortRule]) -> Value {
    serde_json::to_value(rules).unwrap_or(Value::Array(Vec::new()))
}

/// 单条账号的排序读数：积分最早到期（秒）与可用余额。
fn credit_expires_of(session: &AccountSession) -> Option<i64> {
    let balance = session.last_balance.as_ref()?;
    let wallets = balance.get("wallets")?.as_array()?;
    let mut earliest: Option<i64> = None;
    for wallet in wallets {
        let raw = wallet
            .get("expireAt")
            .or_else(|| wallet.get("expire_at"))
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|text| text.parse::<i64>().ok()))
            });
        let Some(at) = raw else { continue };
        let at = if at > 10_000_000_000 { at / 1000 } else { at };
        if at > 0 {
            earliest = Some(earliest.map_or(at, |current| current.min(at)));
        }
    }
    earliest
}

fn balance_available_of(session: &AccountSession) -> Option<f64> {
    session
        .last_balance
        .as_ref()
        .and_then(|balance| balance.get("available"))
        .and_then(Value::as_f64)
}

/// 按 provider 分组并对每组排序（与选号阶段共用 `compare_sessions_by_rules`）。
pub struct AccountSorting {
    pub configured: AccountSortSettings,
    pub providers: Vec<String>,
    pub effective: BTreeMap<String, Vec<AccountSortRule>>,
    pub account_order: BTreeMap<String, Vec<String>>,
    pub credit_expires_at: BTreeMap<String, Option<i64>>,
    pub balances: BTreeMap<String, Option<f64>>,
}

impl AccountSorting {
    pub fn build(sessions: &[AccountSession], settings: AccountSortSettings) -> Self {
        let mut providers: Vec<String> = sessions
            .iter()
            .map(|session| session.provider.clone())
            .collect();
        providers.sort();
        providers.dedup();
        let mut effective = BTreeMap::new();
        let mut account_order = BTreeMap::new();
        let mut credit_expires_at = BTreeMap::new();
        let mut balances = BTreeMap::new();
        for provider in &providers {
            let rules = effective_rules(provider, &settings);
            let mut members: Vec<&AccountSession> = sessions
                .iter()
                .filter(|session| &session.provider == provider)
                .collect();
            members.sort_by(|a, b| {
                AccountSessionStore::compare_sessions_by_rules(a, b, &rules)
                    .then(b.priority.cmp(&a.priority))
                    .then(a.sort_order.cmp(&b.sort_order))
                    .then(a.created_at.cmp(&b.created_at))
                    .then(a.id.cmp(&b.id))
            });
            for session in &members {
                credit_expires_at.insert(session.id.clone(), credit_expires_of(session));
                balances.insert(session.id.clone(), balance_available_of(session));
            }
            account_order.insert(
                provider.clone(),
                members.iter().map(|session| session.id.clone()).collect(),
            );
            effective.insert(provider.clone(), rules);
        }
        Self {
            configured: settings,
            providers,
            effective,
            account_order,
            credit_expires_at,
            balances,
        }
    }

    pub fn to_json(&self, now: i64) -> Value {
        let rules = self
            .effective
            .iter()
            .map(|(provider, rules)| (provider.clone(), rules_json(rules)))
            .collect::<Map<String, Value>>();
        let configured = self
            .configured
            .rules
            .iter()
            .map(|(provider, rules)| (provider.clone(), rules_json(rules)))
            .collect::<Map<String, Value>>();
        let credit = self
            .credit_expires_at
            .iter()
            .map(|(id, at)| (id.clone(), json!(at)))
            .collect::<Map<String, Value>>();
        let balances = self
            .balances
            .iter()
            .map(|(id, available)| (id.clone(), json!(available)))
            .collect::<Map<String, Value>>();
        json!({
            "updated_at": now.max(self.configured.updated_at),
            "configured": Value::Object(configured),
            "rules": Value::Object(rules),
            "providers": self.providers,
            "account_order": self.account_order,
            "credit_expires_at": Value::Object(credit),
            "balances": Value::Object(balances),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(
        provider: &str,
        id: &str,
        created_at: i64,
        expires_at: Option<i64>,
    ) -> AccountSession {
        let mut session = AccountSession::new("ch", id);
        session.id = id.to_string();
        session.provider = provider.to_string();
        session.created_at = created_at;
        session.expires_at = expires_at;
        session
    }

    #[test]
    fn default_credit_rule_sorts_earliest_expiry_first() {
        let sessions = vec![
            session("kiro", "late", 2, Some(200)),
            session("kiro", "early", 1, Some(100)),
            session("kiro", "none", 3, None),
        ];
        let settings = AccountSortSettings::default();
        let sorting = AccountSorting::build(&sessions, settings);
        // 缺省生效规则 = expires asc（credit）
        assert_eq!(sorting.account_order["kiro"], vec!["early", "late", "none"]);
    }

    #[test]
    fn configured_balance_rule_replaces_defaults() {
        let mut balance_low = AccountSession::new("ch", "low");
        balance_low.id = "low".into();
        balance_low.provider = "kiro".into();
        balance_low.last_balance = Some(json!({ "available": 1.0 }));
        let mut balance_high = balance_low.clone();
        balance_high.id = "high".into();
        balance_high.last_balance = Some(json!({ "available": 9.0 }));
        let mut settings = AccountSortSettings::default();
        settings.rules.insert(
            "kiro".to_string(),
            vec![AccountSortRule::Balance { dir: SortDir::Desc }],
        );
        let sorting = AccountSorting::build(&[balance_low, balance_high], settings);
        assert_eq!(sorting.account_order["kiro"], vec!["high", "low"]);
    }

    #[test]
    fn credit_alias_maps_to_expires() {
        let input: SortRuleInput =
            serde_json::from_value(json!({ "on": "credit", "dir": "asc" })).unwrap();
        assert!(matches!(
            AccountSortRule::from(input),
            AccountSortRule::Expires { dir: SortDir::Asc }
        ));
    }
}
