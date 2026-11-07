//! 账号限制器条件组（Phase1-A2 契约，对齐上游 `core/limiter` types.rs）。
//!
//! 上游模型：一个账号可挂**若干条件组**，每组 = 动作（skip/disable）+ 目标
//! （models/account）+ 1~32 条**条件**；条件内部用 `join` 把相邻条件组成
//! 「与」分支（or 开新分支），组命中 = 任一与分支全命中（与优先于或）。
//! 条件的读数来源两类：
//! - **余额类**（remaining / remaining_percent / used / used_percent）：
//!   读账号余额快照（`last_balance`），`source_id` = "account"（主余额）或
//!   钱包行 id/displayName；
//! - **本地计数类**（tokens / requests）：按窗口聚合本进程内的消耗台账——
//!   **计数器基础设施在 A2 接入**，接入前这类条件恒 unknown（不命中），
//!   原因写明，绝不误伤。
//!
//! **兼容存量**：`AccountSession.limiter_groups` 非空时用条件组；为空时把
//! 存量扁平 `limiters`（balance/token 规则）按上游 `groups_from_rules` 同一
//! 映射降维成单条件组（`groups_from_flat`），双读语义与上游
//! `effective_groups_in` 一致。
//!
//! 性能影响：求值发生在**选号过滤阶段**（每候选账号一次，与
//! `should_skip_by_balance` 同层）；纯内存读 JSON/Vec，无锁无 IO；组数有界
//! （≤64×32 条件）。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 条件间的连接方式（作用于「本条件与前一条件」之间；首条恒 Or = 开新分支）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterJoin {
    And,
    #[default]
    Or,
}

/// 命中后的动作：skip = 选号跳过（可按模型）；disable = 整号禁用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterAction {
    #[default]
    Skip,
    Disable,
}

/// 拦截目标：models = 仅拦截条件里声明的模型；account = 整号拦截。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterTarget {
    #[default]
    Models,
    Account,
}

/// 读数指标。余额类读 `last_balance` 快照；本地类读消耗计数器（A2 接入）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterMetric {
    Remaining,
    RemainingPercent,
    Used,
    UsedPercent,
    Tokens,
    Requests,
}

impl LimiterMetric {
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Tokens | Self::Requests)
    }

    pub fn is_percent(&self) -> bool {
        matches!(self, Self::RemainingPercent | Self::UsedPercent)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterComparison {
    Lt,
    #[default]
    Lte,
    Gte,
}

/// 本地计数窗口的重置方式（余额类不携带）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimiterReset {
    #[default]
    Fixed,
    Rolling,
    Daily,
    Weekly,
    Monthly,
    Total,
}

/// 本地计数窗口（对齐上游 `Window`：固定/滚动/自然日/周/月/累计）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimiterWindow {
    #[serde(default)]
    pub reset: LimiterReset,
    /// fixed/rolling 窗口长度（秒）；自然窗口不携带
    #[serde(default)]
    pub seconds: i64,
    /// fixed 窗口的显式起止（unix 秒；0 = 未指定）
    #[serde(default)]
    pub starts_at: i64,
    #[serde(default)]
    pub ends_at: i64,
    /// 自然窗口的时区偏移（分钟；None = 本地时区）
    #[serde(default)]
    pub utc_offset_minutes: Option<i32>,
}

/// 单条条件：一个来源 × 一组模型 × 一个指标 × 一个阈值（+ 可选窗口）。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimiterCondition {
    /// 与前一条件的连接方式（首条恒 Or，写入侧归一）
    #[serde(default)]
    pub join: LimiterJoin,
    /// 额度来源 id："account" / 空 = 账号主余额；其余 = 钱包行 id/displayName
    #[serde(default)]
    pub source_id: String,
    /// 本条件涉及的模型（空 = 不限定模型维度）
    #[serde(default)]
    pub models: Vec<String>,
    pub metric: LimiterMetric,
    pub comparison: LimiterComparison,
    /// 阈值（非负有限；百分比 0~100；本地类须为正整数）
    pub threshold: f64,
    #[serde(default)]
    pub window: Option<LimiterWindow>,
}

/// 条件组：动作 + 目标 + 条件列表。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimiterGroup {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub action: LimiterAction,
    #[serde(default)]
    pub target: LimiterTarget,
    pub conditions: Vec<LimiterCondition>,
}

fn default_true() -> bool {
    true
}

/// 组数与每组条件数的硬上限（对齐上游 normalize_groups 的 64×32）。
pub const MAX_GROUPS: usize = 64;
pub const MAX_CONDITIONS: usize = 32;
/// 条件内模型数上限。
pub const MAX_CONDITION_MODELS: usize = 128;

impl LimiterGroup {
    /// 写入前校验（对齐上游 normalize_groups 的约束；错误为中文文案）。
    pub fn validate(&self) -> Result<(), String> {
        if self.conditions.is_empty() || self.conditions.len() > MAX_CONDITIONS {
            return Err("每个条件组必须有 1~32 条条件".to_string());
        }
        for (index, condition) in self.conditions.iter_mut().enumerate() {
            if !condition.threshold.is_finite() || condition.threshold < 0.0 {
                return Err("限制阈值必须是非负有限数字".to_string());
            }
            if condition.metric.is_local()
                && (condition.threshold <= 0.0
                    || condition.threshold.fract() != 0.0
                    || condition.threshold > 9_007_199_254_740_991.0)
            {
                return Err("本地消耗上限必须是大于 0 的安全整数".to_string());
            }
            if condition.metric.is_percent() && condition.threshold > 100.0 {
                return Err("百分比阈值必须在 0~100 之间".to_string());
            }
            condition.source_id = condition.source_id.trim().to_string();
            if condition.source_id.len() > 512 || condition.models.len() > MAX_CONDITION_MODELS {
                return Err("额度来源或模型范围过大".to_string());
            }
            condition.models = condition
                .models
                .iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            if condition.models.iter().any(|m| m.len() > 256) {
                return Err("模型标识过长".to_string());
            }
            condition.models.sort();
            condition.models.dedup();
            if index == 0 {
                condition.join = LimiterJoin::Or;
            }
        }
        Ok(())
    }
}

/// 批量校验（对齐上游：最多 64 组）。
pub fn validate_groups(groups: &mut [LimiterGroup]) -> Result<(), String> {
    if groups.len() > MAX_GROUPS {
        return Err(format!("最多添加 {MAX_GROUPS} 个限制条件组"));
    }
    for group in groups {
        group.validate()?;
    }
    Ok(())
}

// ── 求值 ────────────────────────────────────────────────────────────

/// 一条条件的读数（求值中间态；reason 供回放/排查）。
struct Reading {
    hit: bool,
    known: bool,
    models: Vec<String>,
    reason: String,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            hit: false,
            known: false,
            models: Vec::new(),
            reason: String::new(),
        }
    }
}

/// 求值结论：skip（按模型/整号）与 disable 是否命中。
#[derive(Debug, Default, PartialEq)]
pub struct LimiterVerdict {
    /// skip 命中的模型（小写归一；空 = 未命中 skip 或整号 skip）
    pub skip_models: Vec<String>,
    /// 整号级命中（skip 且 target=account，或 disable 命中）
    pub account_skip: bool,
    pub disable: bool,
    /// unknown 条件的原因（回放用；unknown 恒不命中，宁放勿误伤）
    pub unknown_reasons: Vec<String>,
}

impl LimiterVerdict {
    /// 该 (账号, 模型) 是否应被跳过。
    pub fn blocks(&self, model: &str) -> bool {
        if self.account_skip {
            return true;
        }
        let model = model.to_ascii_lowercase();
        self.skip_models.iter().any(|m| *m == model)
    }
}

/// 从余额快照读一个余额类读数（source_id → available/钱包行）。
///
/// `last_balance` 形态：`{available: number?, wallets: [{id?/displayName,
/// balance, total?, used?}]}`。读不出 = unknown（不命中，宁放勿误伤）。
fn read_balance_metric(balance: Option<&Value>, condition: &LimiterCondition) -> Reading {
    let Some(balance) = balance else {
        return Reading {
            reason: "暂无余额快照".to_string(),
            ..Default::default()
        };
    };
    let value = if condition.source_id.is_empty() || condition.source_id == "account" {
        balance.get("available").and_then(Value::as_f64)
    } else {
        // 钱包行：id 或 displayName 匹配（各 provider 的行键不齐，两者都试）
        balance
            .get("wallets")
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| {
                        row.get("id").and_then(Value::as_str) == Some(condition.source_id.as_str())
                            || row.get("displayName").and_then(Value::as_str)
                                == Some(condition.source_id.as_str())
                    })
                    .and_then(|row| match condition.metric {
                        LimiterMetric::Remaining | LimiterMetric::RemainingPercent => {
                            row.get("balance").and_then(Value::as_f64)
                        }
                        LimiterMetric::Used | LimiterMetric::UsedPercent => {
                            row.get("used").and_then(Value::as_f64)
                        }
                        LimiterMetric::Tokens | LimiterMetric::Requests => None,
                    })
            })
    };
    let percent = |v: f64| -> Option<f64> {
        match condition.metric {
            LimiterMetric::RemainingPercent => balance
                .get("available")
                .and_then(Value::as_f64)
                .zip(balance.get("total").and_then(Value::as_f64))
                .map(|(a, t)| if t > 0.0 { a / t * 100.0 } else { 0.0 })
                .or(Some(v)),
            LimiterMetric::UsedPercent => Some(100.0 - v),
            _ => Some(v),
        }
    };
    match value.map(percent).flatten() {
        Some(v) if v.is_finite() => Reading {
            hit: compare(v, condition),
            known: true,
            models: condition.models.clone(),
            reason: String::new(),
        },
        _ => Reading {
            reason: "暂无可判定的额度读数".to_string(),
            ..Default::default()
        },
    }
}

fn compare(value: f64, condition: &LimiterCondition) -> bool {
    match condition.comparison {
        LimiterComparison::Lt => value < condition.threshold,
        LimiterComparison::Lte => value <= condition.threshold,
        LimiterComparison::Gte => value >= condition.threshold,
    }
}

/// 本地计数类读数（tokens/requests）：**计数器在 A2 接入**，接入前恒
/// unknown（不命中 + 原因明示），宁放勿误伤。
fn read_local_metric(condition: &LimiterCondition) -> Reading {
    Reading {
        reason: "本地消耗计数器尚未接入（A2）".to_string(),
        ..Default::default()
    }
}

fn read_condition(balance: Option<&Value>, condition: &LimiterCondition) -> Reading {
    if condition.metric.is_local() {
        read_local_metric(condition)
    } else {
        read_balance_metric(balance, condition)
    }
}

/// 条件组求值（对齐上游 evaluate::state 的分支语义）：
/// 相邻 And 归并成同一「与分支」，Or 开新分支；分支全命中才算命中，
/// 组命中 = 任一分支命中。返回该组的拦截范围（整号或模型集）。
fn group_state(
    group: &LimiterGroup,
    balance: Option<&Value>,
) -> (bool, bool, Vec<String>, Vec<String>) {
    let readings: Vec<Reading> = group
        .conditions
        .iter()
        .map(|c| read_condition(balance, c))
        .collect();
    let mut unknown_reasons: Vec<String> = readings
        .iter()
        .filter(|r| !r.known && !r.reason.is_empty())
        .map(|r| r.reason.clone())
        .collect();
    unknown_reasons.dedup();
    // 与优先于或：只有成立的合取分支才贡献拦截范围
    let mut branches: Vec<Vec<usize>> = Vec::new();
    for (index, condition) in group.conditions.iter().enumerate() {
        if index == 0 || condition.join == LimiterJoin::Or {
            branches.push(Vec::new());
        }
        if let Some(branch) = branches.last_mut() {
            branch.push(index);
        }
    }
    let mut hit = false;
    let mut account_hit = false;
    let mut models: Vec<String> = Vec::new();
    if group.enabled {
        for branch in branches {
            if !branch.iter().all(|i| readings[*i].hit) {
                continue;
            }
            hit = true;
            // 整号命中：target=account；或分支内所有条件都不限定模型
            if group.target == LimiterTarget::Account {
                account_hit = true;
            }
            let mut branch_models: Vec<String> = Vec::new();
            for i in &branch {
                for m in &readings[*i].models {
                    let lower = m.to_ascii_lowercase();
                    if !branch_models.iter().any(|x| *x == lower) {
                        branch_models.push(lower);
                    }
                }
            }
            if branch_models.is_empty() {
                account_hit = true;
            }
            for m in branch_models {
                if !models.contains(&m) {
                    models.push(m);
                }
            }
        }
    }
    (hit, account_hit, models, unknown_reasons)
}

/// 求值一组条件组 → 拦截结论。
pub fn evaluate_groups(groups: &[LimiterGroup], balance: Option<&Value>) -> LimiterVerdict {
    let mut verdict = LimiterVerdict::default();
    for group in groups {
        let (hit, account_hit, models, unknown) = group_state(group, balance);
        verdict.unknown_reasons.extend(unknown);
        if !hit {
            continue;
        }
        match group.action {
            LimiterAction::Skip => {
                if account_hit {
                    verdict.account_skip = true;
                } else {
                    verdict.skip_models.extend(models);
                }
            }
            LimiterAction::Disable => {
                verdict.disable = true;
            }
        }
    }
    verdict.skip_models.sort();
    verdict.skip_models.dedup();
    verdict.unknown_reasons.dedup();
    verdict
}

// ── 存量扁平规则降维（双读兼容）────────────────────────────────────

/// 把存量扁平 `AccountLimiterRule` 降维成单条件组（与上游
/// `groups_from_rules` 同一映射）：balance 规则 → remaining<阈值（整号），
/// token 规则 → tokens>=阈值（整号，fixed/daily 窗口）。
pub fn groups_from_flat(
    rules: &[crate::account_session::model::AccountLimiterRule],
) -> Vec<LimiterGroup> {
    rules
        .iter()
        .map(|rule| {
            let token = rule.kind == "token";
            LimiterGroup {
                enabled: rule.enabled,
                action: if rule.action == "disable" {
                    LimiterAction::Disable
                } else {
                    LimiterAction::Skip
                },
                target: LimiterTarget::Account,
                conditions: vec![LimiterCondition {
                    join: LimiterJoin::Or,
                    source_id: String::new(),
                    models: Vec::new(),
                    metric: if token {
                        LimiterMetric::Tokens
                    } else {
                        LimiterMetric::Remaining
                    },
                    comparison: if token {
                        LimiterComparison::Gte
                    } else {
                        LimiterComparison::Lt
                    },
                    threshold: rule.threshold,
                    window: token.then(|| LimiterWindow {
                        reset: if rule.reset.as_deref() == Some("daily") {
                            LimiterReset::Daily
                        } else {
                            LimiterReset::Fixed
                        },
                        seconds: rule.period.unwrap_or(0),
                        ..LimiterWindow::default()
                    }),
                }],
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn balance(available: f64) -> Value {
        json!({ "available": available, "wallets": [
            { "id": "w1", "displayName": "主包", "balance": available, "total": 100.0 }
        ] })
    }

    fn cond(metric: LimiterMetric, cmp: LimiterComparison, threshold: f64) -> LimiterCondition {
        LimiterCondition {
            join: LimiterJoin::Or,
            source_id: "account".to_string(),
            models: Vec::new(),
            metric,
            comparison: cmp,
            threshold,
            window: None,
        }
    }

    #[test]
    fn balance_skip_hits_below_threshold() {
        let groups = vec![LimiterGroup {
            enabled: true,
            action: LimiterAction::Skip,
            target: LimiterTarget::Account,
            conditions: vec![cond(LimiterMetric::Remaining, LimiterComparison::Lt, 5.0)],
        }];
        let v = evaluate_groups(&groups, Some(&balance(3.0)));
        assert!(v.account_skip);
        assert!(v.blocks("any-model"));
        // 阈值之上不放过去
        let v = evaluate_groups(&groups, Some(&balance(9.0)));
        assert!(!v.blocks("any-model"));
        // 无快照：unknown 不命中（宁放勿误伤）
        let v = evaluate_groups(&groups, None);
        assert!(!v.blocks("any-model"));
        assert!(!v.unknown_reasons.is_empty());
    }

    #[test]
    fn and_branch_requires_all_conditions() {
        // 与分支：余额低 且 命中模型 gpt-4 —— 两个条件都要成立
        let mut low = cond(LimiterMetric::Remaining, LimiterComparison::Lt, 5.0);
        low.join = LimiterJoin::Or;
        let mut model_cond = cond(LimiterMetric::Remaining, LimiterComparison::Lt, 5.0);
        model_cond.join = LimiterJoin::And;
        model_cond.models = vec!["gpt-4".to_string()];
        // 模型限定在余额快照上无键可读 → unknown；这里改用钱包行验证
        let groups = vec![LimiterGroup {
            enabled: true,
            action: LimiterAction::Skip,
            target: LimiterTarget::Models,
            conditions: vec![low, model_cond],
        }];
        // 只有第一条成立（第二条 unknown）→ 分支不全中 → 不拦
        let v = evaluate_groups(&groups, Some(&balance(3.0)));
        assert!(!v.blocks("gpt-4"));
    }

    #[test]
    fn wallet_row_metric_reads_display_name() {
        let mut c = cond(LimiterMetric::Remaining, LimiterComparison::Lt, 5.0);
        c.source_id = "主包".to_string();
        let groups = vec![LimiterGroup {
            enabled: true,
            action: LimiterAction::Skip,
            target: LimiterTarget::Account,
            conditions: vec![c],
        }];
        let v = evaluate_groups(&groups, Some(&balance(3.0)));
        assert!(v.account_skip);
    }

    #[test]
    fn local_metrics_unknown_until_counter_wired() {
        let groups = vec![LimiterGroup {
            enabled: true,
            action: LimiterAction::Disable,
            target: LimiterTarget::Account,
            conditions: vec![cond(LimiterMetric::Tokens, LimiterComparison::Gte, 1000.0)],
        }];
        let v = evaluate_groups(&groups, Some(&balance(3.0)));
        assert!(!v.disable);
        assert!(v.unknown_reasons.iter().any(|r| r.contains("A2")));
    }

    #[test]
    fn flat_rules_degrade_to_groups() {
        let rules = vec![crate::account_session::model::AccountLimiterRule {
            kind: "balance".to_string(),
            action: "skip".to_string(),
            threshold: 5.0,
            reset: None,
            period: None,
            enabled: true,
        }];
        let groups = groups_from_flat(&rules);
        let v = evaluate_groups(&groups, Some(&balance(3.0)));
        assert!(v.account_skip);
    }

    #[test]
    fn validate_bounds() {
        let mut g = LimiterGroup {
            enabled: true,
            action: LimiterAction::Skip,
            target: LimiterTarget::Account,
            conditions: vec![cond(
                LimiterMetric::RemainingPercent,
                LimiterComparison::Lt,
                101.0,
            )],
        };
        assert!(g.validate().is_err());
        g.conditions[0].threshold = 10.0;
        assert!(g.validate().is_ok());
        assert_eq!(g.conditions[0].join, LimiterJoin::Or);
    }
}
