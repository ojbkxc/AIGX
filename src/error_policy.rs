//! 统一错误处理策略（对齐 Agent2API v3.5.0-beta.2 `config/error_policy*` 与
//! `core/upstream/error_policy*`）。
//!
//! 这里是 **配置面 + 决策面**：
//! - 配置结构 [`ErrorPolicyConfig`]（持久化键语义 = `routingErrorPolicy`）：
//!   `ErrorSettings` 全字段、9 条全局 [`ErrorRule`]、提供商例外 `overrides`；
//! - 默认值 [`default_rules`] 与参考树逐条一致（限流 429 / 额度 429 / 登录 401 /
//!   余额 402 / 连接 5xx / 能力 501 / 请求 400·413·422 / 内容 / 其它兜底）；
//! - 校验 [`ErrorPolicyConfig::validate`]：区间钳制 + 9 条全局规则唯一 + 例外 ≤100；
//! - 旧字段迁移：缺 `[error_policy]` 段时按既有 `[usage]` 重试字段投影成 legacy 快照；
//! - 决策：[`decide`] 把 状态码/业务码 → [`ErrorKind`] → 命中规则 → `action`/`cooldown`，
//!   供 `bridge/account.rs` 的自重试/换号与冷却链路消费。
//!
//! AIGX 的 `error_translate` 只负责把上游错误渲染成客户端可见的 OpenAI 错误体；
//! 本模块补的是**重试 / 换号 / 冷却 / 停用**的决策层，两者职责不重叠。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::AppConfig;

/// 持久化/线表键名（与参考树一致，前端按此键渲染）。
pub const KEY_ERROR_POLICY: &str = "routingErrorPolicy";

// ── 枚举 ─────────────────────────────────────────────────────────────

/// 错误大类（对齐参考树 `ErrorKind`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    RateLimit,
    Quota,
    Auth,
    Balance,
    Transient,
    Unsupported,
    BadRequest,
    Content,
    Unknown,
}

impl ErrorKind {
    pub fn id(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::Quota => "quota",
            Self::Auth => "auth",
            Self::Balance => "balance",
            Self::Transient => "transient",
            Self::Unsupported => "unsupported",
            Self::BadRequest => "bad_request",
            Self::Content => "content",
            Self::Unknown => "unknown",
        }
    }
}

/// 命中错误后的动作。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorAction {
    SwitchAccount,
    RetryThenSwitch,
    RefreshThenSwitch,
    SwitchProvider,
    ReturnError,
}

/// 冷却模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CooldownMode {
    Upstream,
    Fixed,
    Protection,
    None,
}

/// 冷却作用域。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CooldownScope {
    Auto,
    Model,
    Account,
    Provider,
}

// ── 规则与设置 ───────────────────────────────────────────────────────

/// 单条错误规则（对齐参考树 `ErrorRule`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorRule {
    pub id: String,
    pub name: String,
    pub kind: ErrorKind,
    #[serde(default)]
    pub statuses: Vec<u16>,
    #[serde(default)]
    pub code: String,
    #[serde(default = "all_providers")]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub action: ErrorAction,
    pub cooldown: CooldownMode,
    pub fallback_seconds: u64,
    #[serde(default)]
    pub exponential: bool,
    pub fallback_cap_seconds: u64,
    pub scope: CooldownScope,
}

fn all_providers() -> String {
    "*".into()
}
fn enabled() -> bool {
    true
}

/// 统一错误处理设置（对齐参考树 `ErrorSettings`）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorPolicyConfig {
    pub resends: u64,
    pub switches: u64,
    pub interval_seconds: u64,
    pub wait_budget_seconds: u64,
    pub cooldown_floor_seconds: u64,
    pub backoff: bool,
    pub backoff_seconds: u64,
    pub backoff_cap_seconds: u64,
    pub sink: bool,
    pub sink_seconds: u64,
    pub breaker: bool,
    pub breaker_failures: u64,
    pub breaker_seconds: u64,
    pub breaker_successes: u64,
    pub rules: Vec<ErrorRule>,
    pub overrides: Vec<ErrorRule>,
}

impl Default for ErrorPolicyConfig {
    fn default() -> Self {
        Self {
            resends: 3,
            switches: 5,
            interval_seconds: 5,
            wait_budget_seconds: 30,
            cooldown_floor_seconds: 30,
            backoff: true,
            backoff_seconds: 30,
            backoff_cap_seconds: 300,
            sink: true,
            sink_seconds: 1800,
            breaker: false,
            breaker_failures: 5,
            breaker_seconds: 60,
            breaker_successes: 2,
            rules: default_rules(),
            overrides: Vec::new(),
        }
    }
}

/// 9 条全局默认规则（与参考树 `default_rules` 逐条一致）。
pub fn default_rules() -> Vec<ErrorRule> {
    use CooldownMode::*;
    use ErrorAction::*;
    use ErrorKind::*;
    let rule = |kind: ErrorKind,
                name: &str,
                statuses: &[u16],
                action,
                cooldown,
                fallback_seconds,
                exponential,
                scope| ErrorRule {
        id: kind.id().into(),
        name: name.into(),
        kind,
        statuses: statuses.to_vec(),
        code: String::new(),
        provider: "*".into(),
        model: String::new(),
        enabled: true,
        action,
        cooldown,
        fallback_seconds,
        exponential,
        fallback_cap_seconds: 1800,
        scope,
    };
    vec![
        rule(
            RateLimit,
            "请求限流",
            &[429],
            SwitchAccount,
            Upstream,
            60,
            false,
            CooldownScope::Auto,
        ),
        rule(
            Quota,
            "额度耗尽",
            &[429],
            SwitchAccount,
            Upstream,
            600,
            true,
            CooldownScope::Auto,
        ),
        rule(
            Auth,
            "登录失效",
            &[401],
            RefreshThenSwitch,
            None,
            60,
            false,
            CooldownScope::Account,
        ),
        rule(
            Balance,
            "余额不足",
            &[402],
            SwitchAccount,
            Upstream,
            600,
            false,
            CooldownScope::Account,
        ),
        rule(
            Transient,
            "连接或服务异常",
            &[408, 500, 502, 503, 504],
            RetryThenSwitch,
            Protection,
            60,
            false,
            CooldownScope::Auto,
        ),
        rule(
            Unsupported,
            "能力不支持",
            &[501],
            SwitchProvider,
            None,
            60,
            false,
            CooldownScope::Auto,
        ),
        rule(
            BadRequest,
            "请求有误",
            &[400, 413, 422],
            ReturnError,
            None,
            60,
            false,
            CooldownScope::Model,
        ),
        rule(
            Content,
            "内容被拒绝",
            &[],
            ReturnError,
            None,
            60,
            false,
            CooldownScope::Model,
        ),
        rule(
            Unknown,
            "其他错误",
            &[],
            ReturnError,
            None,
            60,
            false,
            CooldownScope::Auto,
        ),
    ]
}

impl ErrorPolicyConfig {
    /// 校验（区间 + 唯一性 + 语义约束），与参考树 `validate` 同口径。
    pub fn validate(&self) -> Result<(), String> {
        let bounds = [
            (self.resends, 0, 10, "原账号重发次数"),
            (self.switches, 0, 10, "换账号上限"),
            (self.interval_seconds, 0, 300, "重发间隔"),
            (self.wait_budget_seconds, 0, 300, "累计等待上限"),
            (self.cooldown_floor_seconds, 1, 60, "最短冷却"),
            (self.backoff_seconds, 1, 3600, "首次失败休息"),
            (
                self.backoff_cap_seconds,
                self.backoff_seconds,
                3600,
                "最长失败休息",
            ),
            (self.sink_seconds, 30, 3600, "降低优先级时长"),
            (self.breaker_failures, 1, 20, "连续失败阈值"),
            (self.breaker_seconds, 10, 600, "连续失败保护时长"),
            (self.breaker_successes, 1, 10, "恢复成功次数"),
        ];
        for (value, min, max, name) in bounds {
            if !(min..=max).contains(&value) {
                return Err(format!("{name}必须是 {min}–{max} 的整数"));
            }
        }
        if self.rules.len() != 9 || self.overrides.len() > 100 {
            return Err("全局错误规则必须完整，提供商例外最多 100 条".into());
        }
        let mut ids = std::collections::HashSet::new();
        let mut kinds = std::collections::HashSet::new();
        for (rule, global) in self
            .rules
            .iter()
            .map(|rule| (rule, true))
            .chain(self.overrides.iter().map(|rule| (rule, false)))
        {
            for (text, max, name) in [
                (&rule.id, 80, "规则标识"),
                (&rule.name, 100, "规则名称"),
                (&rule.provider, 256, "提供商"),
                (&rule.model, 256, "模型"),
                (&rule.code, 256, "业务码"),
            ] {
                if text.len() > max || text.chars().any(char::is_control) || text.trim() != text {
                    return Err(format!("{name}包含空白、控制字符或超过长度上限"));
                }
            }
            if rule.id.is_empty()
                || rule.name.is_empty()
                || rule.provider.is_empty()
                || !ids.insert(&rule.id)
            {
                return Err("规则标识、名称或提供商为空，或规则标识重复".into());
            }
            if global
                && (rule.id != rule.kind.id()
                    || !rule.enabled
                    || !kinds.insert(rule.kind.id())
                    || rule.provider != "*"
                    || !rule.model.is_empty()
                    || !rule.code.is_empty())
            {
                return Err(
                    "全局规则按错误类型唯一，提供商、模型和业务码条件请使用例外规则".into(),
                );
            }
            if rule.statuses.len() > 50
                || rule
                    .statuses
                    .iter()
                    .any(|status| !(400..600).contains(status))
            {
                return Err("错误状态码必须在 400–599 范围，最多 50 项".into());
            }
            if !(1..=86400).contains(&rule.fallback_seconds)
                || !(rule.fallback_seconds..=86400).contains(&rule.fallback_cap_seconds)
            {
                return Err("冷却时间必须在 1–86400 秒范围，上限不能短于首次时间".into());
            }
            if rule.kind != ErrorKind::Auth && rule.action == ErrorAction::RefreshThenSwitch {
                return Err("仅登录失效规则可以使用凭证续期".into());
            }
            if matches!(rule.kind, ErrorKind::BadRequest | ErrorKind::Content)
                && (rule.action != ErrorAction::ReturnError || rule.cooldown != CooldownMode::None)
            {
                return Err("已识别的请求错误和内容拒绝不能设置为重复发送、换号或账号冷却".into());
            }
            if rule.kind == ErrorKind::Unknown && !rule.enabled {
                return Err("其他错误的兜底规则不能关闭".into());
            }
        }
        Ok(())
    }

    /// 按错误类型 + 条件解析命中的规则：例外优先 → 全局同类 → 兜底 Unknown。
    pub fn resolve(
        &self,
        kind: ErrorKind,
        status: u16,
        code: &str,
        provider: &str,
        model: &str,
    ) -> &ErrorRule {
        self.overrides
            .iter()
            .find(|rule| {
                rule.enabled
                    && rule.kind == kind
                    && (rule.provider == "*" || rule.provider == provider)
                    && (rule.model.is_empty() || rule.model == model)
                    && (rule.code.is_empty() || rule.code == code)
                    && (rule.statuses.is_empty() || rule.statuses.contains(&status))
            })
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|rule| rule.enabled && rule.kind == kind)
            })
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|rule| rule.kind == ErrorKind::Unknown)
            })
            .unwrap_or(&self.rules[0])
    }
}

// ── 快照 ─────────────────────────────────────────────────────────────

/// 进程级错误策略快照。
#[derive(Clone, Debug, Default)]
pub struct ErrorPolicySnapshot {
    pub settings: ErrorPolicyConfig,
    /// `true` = 来自显式 `[error_policy]` 段；`false` = 默认/旧字段投影。
    pub configured: bool,
    /// `true` = 来自旧 `[usage]` 重试字段的等价投影。
    pub legacy: bool,
}

static SNAPSHOT: parking_lot::RwLock<Option<Arc<ErrorPolicySnapshot>>> =
    parking_lot::RwLock::new(None);

/// 安装/刷新进程级快照（启动与保存后调用）。
pub fn install_snapshot(snapshot: Arc<ErrorPolicySnapshot>) {
    *SNAPSHOT.write() = Some(snapshot);
}

/// 读取进程级快照（未安装时返回默认）。
pub fn snapshot() -> Arc<ErrorPolicySnapshot> {
    SNAPSHOT
        .read()
        .clone()
        .unwrap_or_else(|| Arc::new(ErrorPolicySnapshot::default()))
}

/// 快照内容摘要（保存冲突检测 / 变更判据）。
pub fn revision(snapshot: &ErrorPolicySnapshot) -> String {
    format!(
        "{:x}",
        Sha256::digest(
            json!({
                "settings": snapshot.settings,
                "configured": snapshot.configured,
                "legacy": snapshot.legacy,
            })
            .to_string()
            .as_bytes()
        )
    )
}

/// 由整份配置构造快照：
/// - `[error_policy]` 段存在且校验通过 → `configured = true`；
/// - 否则若旧 `[usage]` 重试字段偏离默认 → 投影成 legacy 快照（含 `retry_on_status`
///   对应类型的一条 `SwitchAccount` 例外规则）；
/// - 都不满足 → 默认快照（`configured = false, legacy = false`）。
pub fn build_snapshot(config: &AppConfig) -> ErrorPolicySnapshot {
    if let Some(raw) = config.error_policy.clone() {
        if raw.validate().is_ok() {
            return ErrorPolicySnapshot {
                settings: raw,
                configured: true,
                legacy: false,
            };
        }
    }
    let usage = &config.usage;
    // 与 `UsageConfig::default` 逐项对比：只有偏离默认才视为「运维显式改过旧
    // 重试字段」，据此做 legacy 投影（默认配置不标 legacy）。
    let legacy = usage.retry_self_count != 1
        || usage.retry_switch_count != 5
        || usage.retry_interval_ms != 0
        || usage.retry_on_status != vec![402u16];
    let mut settings = ErrorPolicyConfig::default();
    if legacy {
        settings.resends = usage.retry_self_count as u64;
        settings.switches = usage.retry_switch_count as u64;
        settings.interval_seconds = usage.retry_interval_ms / 1000;
        // 旧「直接换号状态码」名单投影成一条条 SwitchAccount 例外，与参考树同构。
        for status in usage
            .retry_on_status
            .iter()
            .copied()
            .filter(|status| (400..600).contains(status))
        {
            let kind = kind_from_status(status, "");
            if matches!(kind, ErrorKind::BadRequest | ErrorKind::Content) {
                continue;
            }
            if let Some(mut rule) = settings.rules.iter().find(|r| r.kind == kind).cloned() {
                rule.id = format!("legacy-{status}");
                rule.name = format!("原直接换号设置 {status}");
                rule.statuses = vec![status];
                rule.action = ErrorAction::SwitchAccount;
                if kind == ErrorKind::Unsupported {
                    rule.scope = CooldownScope::Model;
                }
                settings.overrides.push(rule);
            }
        }
    }
    ErrorPolicySnapshot {
        settings,
        configured: false,
        legacy,
    }
}

// ── 证据归类与决策 ───────────────────────────────────────────────────

/// 由上游状态码 + 业务码推断错误大类（对齐参考树 `error_policy_evidence::kind`
/// 的通用部分；provider 专属业务码映射按需在证据层扩展）。
pub fn kind_from_status(status: u16, code: &str) -> ErrorKind {
    if matches!(
        code,
        "insufficient_balance"
            | "balance_insufficient"
            | "billing_hard_limit_reached"
            | "payment_required"
            | "credit_balance_exhausted"
            | "credit_balance_too_low"
    ) || status == 402
    {
        return ErrorKind::Balance;
    }
    if matches!(
        code,
        "quota_exhausted"
            | "quota_exceeded"
            | "insufficient_quota"
            | "usage_limit_reached"
            | "monthly_request_count"
            | "quota_limit_reached"
            | "organization_spend_limit_exceeded"
            | "project_spend_limit_exceeded"
            | "organization_usage_limit_exceeded"
    ) {
        return ErrorKind::Quota;
    }
    if matches!(
        code,
        "account_model_unavailable"
            | "account_model_permissions_unknown"
            | "model_not_available_for_account"
            | "model_not_available"
            | "model_not_found"
            | "unsupported"
            | "no_method"
            | "not_implemented"
            | "route_not_supported"
    ) || status == 501
    {
        return ErrorKind::Unsupported;
    }
    if matches!(
        code,
        "content_policy_violation"
            | "content_filter"
            | "content_blocked"
            | "content_policy_rejected"
            | "prohibited_content"
            | "refusal"
            | "safety"
    ) {
        return ErrorKind::Content;
    }
    if matches!(
        code,
        "context_length_exceeded"
            | "payload_too_large"
            | "invalid_request_error"
            | "invalid_request"
            | "context_window_exceeded"
            | "model_context_window_exceeded"
    ) || matches!(status, 400 | 413 | 422)
    {
        return ErrorKind::BadRequest;
    }
    if status == 429 {
        return ErrorKind::RateLimit;
    }
    if matches!(status, 408 | 500 | 502 | 503 | 504) {
        return ErrorKind::Transient;
    }
    if status == 401 {
        return ErrorKind::Auth;
    }
    ErrorKind::Unknown
}

/// 硬停判据：这些状态/业务码无论策略如何都不应重试或换号。
pub fn hard_stop(status: u16, code: &str) -> bool {
    matches!(status, 403 | 409 | 428 | 499)
        || matches!(
            code,
            "permission_denied"
                | "access_denied"
                | "permission_error"
                | "plugin_busy"
                | "gateway_busy"
                | "account_busy"
                | "plugin_resource_limit"
                | "plugin_trap"
                | "forbidden"
                | "aborted"
                | "cancelled"
        )
}

/// 错误决策（对齐参考树 `ErrorDecision`）。
#[derive(Clone, Debug)]
pub struct ErrorDecision {
    pub rule: ErrorRule,
    pub kind: ErrorKind,
    pub action: ErrorAction,
    pub scope: CooldownScope,
    pub hard_stop: bool,
    pub exception: bool,
    pub interval_ms: u64,
    pub floor_ms: u64,
    pub model: String,
}

impl ErrorDecision {
    pub fn retries(&self) -> bool {
        !self.hard_stop && self.action == ErrorAction::RetryThenSwitch
    }
    pub fn refreshes(&self) -> bool {
        !self.hard_stop && self.action == ErrorAction::RefreshThenSwitch
    }
    pub fn switches(&self) -> bool {
        !self.hard_stop && self.action != ErrorAction::ReturnError
    }
    pub fn skips_provider(&self) -> bool {
        self.scope == CooldownScope::Provider || self.action == ErrorAction::SwitchProvider
    }
    pub fn cooldown(&self) -> bool {
        self.rule.cooldown != CooldownMode::None && !self.hard_stop
    }
    /// 若该决策要冷却，返回最终生效规则（Protection 模式改写为 backoff 参数）。
    pub fn cooldown_rule(&self, settings: &ErrorPolicyConfig) -> Option<ErrorRule> {
        if !self.cooldown() {
            return None;
        }
        let mut rule = self.rule.clone();
        if rule.cooldown == CooldownMode::Protection {
            if !settings.backoff {
                return None;
            }
            rule.fallback_seconds = settings.backoff_seconds;
            rule.fallback_cap_seconds = settings.backoff_cap_seconds;
            rule.exponential = true;
        }
        Some(rule)
    }
    /// 重试前应等待的毫秒数（≤ 重发间隔 / 上游建议 / 冷却剩余三者取大）。
    pub fn retry_delay(&self, suggested: u64) -> u64 {
        self.interval_ms.max(suggested)
    }
}

/// 用当前进程快照对一次上游失败做决策。
pub fn decide(provider: &str, model: &str, status: u16, code: &str) -> ErrorDecision {
    decide_with(&snapshot(), provider, model, status, code)
}

/// 显式指定快照做决策（单测 / 预演用）。
pub fn decide_with(
    snapshot: &ErrorPolicySnapshot,
    provider: &str,
    model: &str,
    status: u16,
    code: &str,
) -> ErrorDecision {
    let settings = &snapshot.settings;
    let kind = kind_from_status(status, code);
    let rule = settings
        .resolve(kind, status, code, provider, model)
        .clone();
    let stopped =
        hard_stop(status, code) || matches!(kind, ErrorKind::BadRequest | ErrorKind::Content);
    let action = if stopped {
        ErrorAction::ReturnError
    } else {
        rule.action
    };
    // 账号/模型维度的 Unsupported 自动升到 Provider 作用域（对齐参考树）。
    let mut scope = rule.scope;
    if kind == ErrorKind::Unsupported && scope == CooldownScope::Auto {
        scope = CooldownScope::Provider;
    }
    if matches!(kind, ErrorKind::Auth | ErrorKind::Balance)
        && scope == CooldownScope::Auto
        && scope != CooldownScope::Provider
    {
        scope = CooldownScope::Account;
    }
    let action = if kind != ErrorKind::Auth && action == ErrorAction::RefreshThenSwitch {
        ErrorAction::ReturnError
    } else {
        action
    };
    let exception = settings.overrides.iter().any(|item| item.id == rule.id);
    ErrorDecision {
        rule,
        kind,
        action,
        scope,
        hard_stop: stopped,
        exception,
        interval_ms: settings.interval_seconds.saturating_mul(1000),
        floor_ms: if matches!(kind, ErrorKind::RateLimit | ErrorKind::Quota) {
            settings.cooldown_floor_seconds.saturating_mul(1000)
        } else {
            0
        },
        model: model.to_string(),
    }
}

/// 自重试闸门：`Some(false)` = 直接换号（不原地重发）；`Some(true)` = 可原地重发；
/// `None` = 尚未配置统一策略，调用方沿用既有判定。
pub fn self_retry_allowed(status: u16, code: &str) -> Option<bool> {
    let snapshot = snapshot();
    if !snapshot.configured {
        return None;
    }
    let decision = decide_with(&snapshot, "", "", status, code);
    Some(!decision.hard_stop && decision.action == ErrorAction::RetryThenSwitch)
}

/// 换号/换渠道是否应立即进行（`Some(true)` = 可继续尝试其它账号/渠道）。
pub fn should_switch(status: u16, code: &str) -> Option<bool> {
    let snapshot = snapshot();
    if !snapshot.configured {
        return None;
    }
    let decision = decide_with(&snapshot, "", "", status, code);
    Some(decision.switches())
}

/// 冷却下限（秒）：限流/额度类错误的最短冷却；未配置返回 `None`。
pub fn cooldown_floor_seconds(status: u16, code: &str) -> Option<u64> {
    let snapshot = snapshot();
    if !snapshot.configured {
        return None;
    }
    let decision = decide_with(&snapshot, "", "", status, code);
    decision
        .cooldown_rule(&snapshot.settings)
        .map(|rule| rule.fallback_seconds)
        .filter(|secs| *secs > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_have_nine_global_rules_and_validate() {
        let cfg = ErrorPolicyConfig::default();
        assert_eq!(cfg.rules.len(), 9);
        assert!(cfg.validate().is_ok());
        // 全局规则按类型唯一且 id = kind.id()
        for rule in &cfg.rules {
            assert_eq!(rule.id, rule.kind.id());
            assert_eq!(rule.provider, "*");
        }
    }

    #[test]
    fn resolve_prefers_override_then_global() {
        let mut cfg = ErrorPolicyConfig::default();
        let mut over = cfg
            .rules
            .iter()
            .find(|r| r.kind == ErrorKind::RateLimit)
            .cloned()
            .unwrap();
        over.id = "acme-429".into();
        over.name = "Acme 限流".into();
        over.provider = "acme".into();
        over.action = ErrorAction::ReturnError;
        cfg.overrides.push(over);

        let hit = cfg.resolve(ErrorKind::RateLimit, 429, "", "acme", "m");
        assert_eq!(hit.id, "acme-429");
        // 其它 provider 回落全局限流规则
        let global = cfg.resolve(ErrorKind::RateLimit, 429, "", "other", "m");
        assert_eq!(global.id, "rate_limit");
    }

    #[test]
    fn validate_rejects_incomplete_global_rules() {
        let mut cfg = ErrorPolicyConfig::default();
        cfg.rules.pop();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn kind_classification_covers_common_statuses() {
        assert_eq!(kind_from_status(429, ""), ErrorKind::RateLimit);
        assert_eq!(kind_from_status(402, ""), ErrorKind::Balance);
        assert_eq!(kind_from_status(401, ""), ErrorKind::Auth);
        assert_eq!(kind_from_status(503, ""), ErrorKind::Transient);
        assert_eq!(kind_from_status(400, ""), ErrorKind::BadRequest);
        assert_eq!(kind_from_status(418, ""), ErrorKind::Unknown);
        assert_eq!(
            kind_from_status(200, "insufficient_quota"),
            ErrorKind::Quota
        );
        assert_eq!(kind_from_status(200, "content_filter"), ErrorKind::Content);
    }

    #[test]
    fn decide_marks_bad_request_as_hard_stop_return_error() {
        let snap = ErrorPolicySnapshot::default();
        let d = decide_with(&snap, "p", "m", 400, "");
        assert!(d.hard_stop);
        assert_eq!(d.action, ErrorAction::ReturnError);
        assert!(!d.switches());
    }

    #[test]
    fn decide_rate_limit_switches_account_and_cools_down() {
        let snap = ErrorPolicySnapshot::default();
        let d = decide_with(&snap, "p", "m", 429, "");
        assert_eq!(d.action, ErrorAction::SwitchAccount);
        assert!(d.switches());
        assert!(!d.hard_stop);
        assert!(d.cooldown());
    }

    #[test]
    fn legacy_projection_from_retry_on_status() {
        let mut config = AppConfig::default();
        {
            let usage = &mut config.usage;
            usage.retry_on_status = vec![402, 429];
            usage.retry_self_count = 2;
        }
        let snap = build_snapshot(&config);
        assert!(!snap.configured);
        assert!(snap.legacy);
        assert_eq!(snap.settings.resends, 2);
        // 402 → Balance 例外一条，429 → RateLimit 例外一条
        assert!(snap.settings.overrides.iter().any(|r| r.id == "legacy-402"));
        assert!(snap.settings.overrides.iter().any(|r| r.id == "legacy-429"));
    }

    #[test]
    fn explicit_section_wins_and_sets_configured() {
        let config = AppConfig {
            error_policy: Some(ErrorPolicyConfig::default()),
            ..AppConfig::default()
        };
        let snap = build_snapshot(&config);
        assert!(snap.configured);
        assert!(!snap.legacy);
    }
}
