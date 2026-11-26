//! 统一错误处理策略（对齐 Agent2API v3.5.0-beta.2 `config/error_policy*` 与
//! `core/upstream/error_policy*`；beta.8 补齐插件申报层）。
//!
//! 这里是 **配置面 + 决策面**：
//! - 配置结构 [`ErrorPolicyConfig`]（持久化键语义 = `routingErrorPolicy`）：
//!   `ErrorSettings` 全字段、9 条全局 [`ErrorRule`]、提供商例外 `overrides`；
//! - 默认值 [`default_rules`] 与参考树逐条一致（限流 429 / 额度 429 / 登录 401 /
//!   余额 402 / 连接 5xx / 能力 501 / 请求 400·413·422 / 内容 / 其它兜底）；
//! - 校验 [`ErrorPolicyConfig::validate`]：区间钳制 + 9 条全局规则唯一 + 例外 ≤100；
//! - 旧字段迁移：缺 `[error_policy]` 段时按既有 `[usage]` 重试字段投影成 legacy 快照；
//! - 决策：[`decide`] 把 状态码/业务码 → [`ErrorKind`] → 命中规则 → `action`/`cooldown`，
//!   供 `bridge/account.rs` 的自重试/换号与冷却链路消费；
//! - **beta.8 插件层**：插件在 `init` 握手里申报的默认错误规则与识别映射
//!   （[`PluginErrorRule`] / [`PluginErrorClassification`]，由
//!   `plugin_host::error_rules` 解析）参与三层解析
//!   （用户例外 > 插件默认 > 全局），见 [`ErrorPolicyConfig::resolve_with`]。
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
    /// 不进入硬冷却，只把该账号/提供商/模型「降权」排到候选队尾
    /// （对齐 v3.5.1 `CooldownMode::Deprioritize`）。窗口到点自动恢复，
    /// 成功转发一次也提前恢复。
    Deprioritize,
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

/// 「换账号上限」的上界（对齐 Agent2API v3.5.1 `MAX_SWITCHES` = 100）。
///
/// 与转发层单请求尝试上限（[`crate::config::MAX_SWITCH_ATTEMPTS`]）同源：
/// 配置上界比转发层小的话，管理员填了也用不上。
pub const MAX_SWITCHES: u64 = 100;

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
            switches: crate::config::DEFAULT_RETRY_SWITCH_COUNT as u64,
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

/// 9 条全局默认规则（与 v3.5.1 `default_rules`(v2) 逐条一致）。
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
            Fixed,
            300,
            true,
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
            Deprioritize,
            1800,
            false,
            CooldownScope::Auto,
        ),
        rule(
            BadRequest,
            "请求有误",
            &[400, 413, 422],
            SwitchProvider,
            Deprioritize,
            1800,
            false,
            CooldownScope::Auto,
        ),
        rule(
            Content,
            "内容被拒绝",
            &[],
            SwitchProvider,
            Deprioritize,
            1800,
            false,
            CooldownScope::Provider,
        ),
        rule(
            Unknown,
            "其他错误",
            &[],
            SwitchAccount,
            Deprioritize,
            1800,
            false,
            CooldownScope::Auto,
        ),
    ]
}

/// v1 时代的 9 条默认规则（对齐 v3.5.1 `v1_default_rules`，仅供 v1→v2 迁移判据）。
///
/// 判据：某条全局规则若与这里逐字段相等，说明用户没改过它，可以安全升级到
/// [`default_rules`]（v2）；改过的规则保持原样，避免覆盖用户配置。
pub fn v1_default_rules() -> Vec<ErrorRule> {
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

/// v1→v2 内存迁移（对齐 v3.5.1 `config/error_policy.rs::migrate_untouched_v1_defaults`）。
///
/// 只升级「逐字段仍等于 v1 默认值」的全局规则；用户改过的不动。幂等：
/// 升级后规则已等于 v2，不再满足「等于 v1」的判据。
pub fn migrate_untouched_v1_defaults(settings: &mut ErrorPolicyConfig) {
    let v1 = v1_default_rules();
    let v2 = default_rules();
    for rule in &mut settings.rules {
        if let (Some(old), Some(next)) = (
            v1.iter().find(|item| item.kind == rule.kind),
            v2.iter().find(|item| item.kind == rule.kind),
        ) {
            if *rule == *old {
                *rule = next.clone();
            }
        }
    }
}

// ── 插件申报的错误规则（对齐参考树 `PluginErrorRule` / `DeclaredErrorPolicy`）──

/// 规则来源：`override`（用户例外）> `plugin`（插件申报默认）> `global`（全局）。
/// 对齐参考树 `RuleSource`（snake_case 序列化，路由记录用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSource {
    Override,
    Plugin,
    Global,
}

/// 插件在 `init` 握手里申报的一条错误规则（对齐参考树 `PluginErrorRule`）。
///
/// `rule.id` 是网关侧规范化 id（`plugin:{provider}:{本地 id}`）；
/// `plugin_rule_id` 保留插件本地 id（用户界面显示 / 例外对齐用）；`reason`
/// 是插件给的说明文字。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginErrorRule {
    #[serde(flatten)]
    pub rule: ErrorRule,
    pub plugin_rule_id: String,
    #[serde(default)]
    pub reason: String,
}

/// 插件申报的「错误识别映射」：状态码/业务码 → 错误大类（含作用域与
/// hard-stop 建议）。对齐参考树 `PluginErrorClassification`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginErrorClassification {
    pub kind: ErrorKind,
    pub statuses: Vec<u16>,
    pub codes: Vec<String>,
    pub scope: Option<CooldownScope>,
    pub hard_stop: bool,
}

/// 一个 provider 的插件申报快照：规则 + 识别映射。
/// 对齐参考树 `DeclaredErrorPolicy`（`Default` = 两边都空）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeclaredErrorPolicy {
    pub rules: Vec<PluginErrorRule>,
    pub classifications: Vec<PluginErrorClassification>,
}

impl ErrorPolicyConfig {
    /// 校验（区间 + 唯一性 + 语义约束），与参考树 `validate` 同口径。
    pub fn validate(&self) -> Result<(), String> {
        let bounds = [
            (self.resends, 0, 10, "原账号重发次数"),
            (self.switches, 0, MAX_SWITCHES, "换账号上限"),
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
            // v3.5.1 取消了「BadRequest/Content 必须 ReturnError」的约束：
            // v2 默认规则把它们改成换提供商 + 降权（不直接拒回），约束若还在，
            // 默认配置自己就校验不过。
            if rule.kind == ErrorKind::Unknown && global && !rule.enabled {
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
        self.resolve_with(&[], kind, status, code, provider, model)
            .0
    }

    /// 三层解析（对齐参考树 `resolve_with`）：
    /// 用户例外（override）→ 插件申报默认（plugin）→ 全局同类 → 兜底 Unknown。
    ///
    /// 插件层取**特异性最高**的命中（model > code > statuses 三元评分），
    /// 且被同 id 用户例外屏蔽的插件规则不参与（用户改过的优先）。
    /// 返回命中的规则与来源（路由记录 / 「插件默认」标签用）。
    pub fn resolve_with<'a>(
        &'a self,
        plugin_rules: &'a [PluginErrorRule],
        kind: ErrorKind,
        status: u16,
        code: &str,
        provider: &str,
        model: &str,
    ) -> (&'a ErrorRule, RuleSource) {
        if let Some(rule) = self.overrides.iter().find(|rule| {
            rule.kind == kind && rule_matches_condition(rule, status, code, provider, model)
        }) {
            return (rule, RuleSource::Override);
        }
        let overridden = |id: &str| self.overrides.iter().any(|over| over.id == id);
        let mut best: Option<&PluginErrorRule> = None;
        let mut best_score = (false, false, false);
        for item in plugin_rules {
            let rule = &item.rule;
            if rule.kind != kind || overridden(&rule.id) || overridden(&item.plugin_rule_id) {
                continue;
            }
            if !rule_matches_condition(rule, status, code, provider, model) {
                continue;
            }
            let score = (
                !rule.model.is_empty(),
                !rule.code.is_empty(),
                !rule.statuses.is_empty(),
            );
            if best.is_none() || score > best_score {
                best = Some(item);
                best_score = score;
            }
        }
        if let Some(item) = best {
            return (&item.rule, RuleSource::Plugin);
        }
        let global = self
            .rules
            .iter()
            .find(|rule| rule.enabled && rule.kind == kind)
            .or_else(|| {
                self.rules
                    .iter()
                    .find(|rule| rule.kind == ErrorKind::Unknown)
            })
            .unwrap_or(&self.rules[0]);
        (global, RuleSource::Global)
    }

    /// 未识别错误（Unknown / 400·403·409 的 BadRequest）从例外与插件规则里
    /// 反推错误大类（对齐参考树 `infer_exception_kind`）：只看「带 code 或
    /// statuses 条件」的规则，返回其 kind。
    pub fn infer_exception_kind(
        &self,
        plugin_rules: &[PluginErrorRule],
        status: u16,
        code: &str,
        provider: &str,
        model: &str,
    ) -> Option<ErrorKind> {
        if let Some(rule) = self.overrides.iter().find(|rule| {
            rule.provider == provider
                && (!rule.code.is_empty() || !rule.statuses.is_empty())
                && rule_matches_condition(rule, status, code, provider, model)
        }) {
            return Some(rule.kind);
        }
        let overridden = |id: &str| self.overrides.iter().any(|over| over.id == id);
        let mut best: Option<&PluginErrorRule> = None;
        let mut best_score = (false, false, false);
        for item in plugin_rules {
            let rule = &item.rule;
            if rule.provider != provider
                || (rule.code.is_empty() && rule.statuses.is_empty())
                || overridden(&rule.id)
                || overridden(&item.plugin_rule_id)
                || !rule_matches_condition(rule, status, code, provider, model)
            {
                continue;
            }
            let score = (
                !rule.model.is_empty(),
                !rule.code.is_empty(),
                !rule.statuses.is_empty(),
            );
            if best.is_none() || score > best_score {
                best = Some(item);
                best_score = score;
            }
        }
        best.map(|item| item.rule.kind)
    }
}

/// 条件命中判定（对齐参考树 `matches_condition`；`enabled` 在 override 层
/// 由调用方逐层短路，插件层在此统一判）。
fn rule_matches_condition(
    rule: &ErrorRule,
    status: u16,
    code: &str,
    provider: &str,
    model: &str,
) -> bool {
    rule.enabled
        && (rule.provider == "*" || rule.provider == provider)
        && (rule.model.is_empty() || rule.model == model)
        && (rule.code.is_empty() || rule.code == code)
        && (rule.statuses.is_empty() || rule.statuses.contains(&status))
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

/// 是否挂了显式 `[error_policy]` 段。数据面据此决定用统一策略还是既有
/// 轮换冷却（未配置时保持旧行为，不凭空改变线上冷却）。
pub fn configured() -> bool {
    snapshot().configured
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
    if let Some(mut raw) = config.error_policy.clone() {
        if raw.validate().is_ok() {
            // 已持久化的 v1 默认值在加载时升级到 v2（只迁移用户没改过的规则）。
            migrate_untouched_v1_defaults(&mut raw);
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
        || usage.retry_switch_count != crate::config::DEFAULT_RETRY_SWITCH_COUNT
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

/// 错误决策（对齐参考树 `ErrorDecision`；`rule_source` 为 beta.8 新增的
/// 规则来源标记，路由记录里展示「插件默认」用）。
#[derive(Clone, Debug)]
pub struct ErrorDecision {
    pub rule: ErrorRule,
    pub rule_source: RuleSource,
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
    /// 该决策走「降权沉底」而非硬冷却（对齐 v3.5.1 `ErrorDecision::deprioritizes`）。
    pub fn deprioritizes(&self) -> bool {
        !self.hard_stop && self.rule.cooldown == CooldownMode::Deprioritize
    }
    pub fn cooldown(&self) -> bool {
        // Deprioritize 不阻塞选路，只降权，因此不算「冷却」。
        !self.hard_stop
            && !matches!(
                self.rule.cooldown,
                CooldownMode::None | CooldownMode::Deprioritize
            )
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
    let snapshot = snapshot();
    let declared = crate::plugin_host::error_rules::for_provider(provider);
    decide_with_declared(&snapshot, &declared, provider, model, status, code)
}

/// 显式指定快照做决策（单测 / 预演用）。
pub fn decide_with(
    snapshot: &ErrorPolicySnapshot,
    provider: &str,
    model: &str,
    status: u16,
    code: &str,
) -> ErrorDecision {
    let declared = crate::plugin_host::error_rules::for_provider(provider);
    decide_with_declared(snapshot, &declared, provider, model, status, code)
}

/// 显式指定快照 + 插件申报做决策（三层解析 + 插件识别映射；
/// 对齐参考树 `decide_with_declared`，AIGX 的证据层无上游 body 候选码
/// 集合，只取单一 code）。
pub fn decide_with_declared(
    snapshot: &ErrorPolicySnapshot,
    declared: &DeclaredErrorPolicy,
    provider: &str,
    model: &str,
    status: u16,
    code: &str,
) -> ErrorDecision {
    let settings = &snapshot.settings;
    // 1. 插件识别映射优先于状态码推断（上游同样的顺序）。
    let classified = declared
        .classifications
        .iter()
        .find(|entry| {
            entry.statuses.contains(&status)
                || (!code.is_empty() && entry.codes.iter().any(|item| item == code))
        })
        .cloned();
    let mut kind = match &classified {
        Some(entry) => entry.kind,
        None => kind_from_status(status, code),
    };
    // 2. Unknown / 400·403·409 的 BadRequest 用例外与插件规则反推大类。
    if kind == ErrorKind::Unknown
        || (matches!(status, 400 | 403 | 409) && kind == ErrorKind::BadRequest)
    {
        if let Some(inferred) =
            settings.infer_exception_kind(&declared.rules, status, code, provider, model)
        {
            kind = inferred;
        }
    }
    // 3. 三层解析。
    let (resolved, rule_source) =
        settings.resolve_with(&declared.rules, kind, status, code, provider, model);
    let rule = resolved.clone();
    // v3.5.1 起不再按 kind 一刀切拒回：BadRequest/Content 走 v2 默认规则
    // （换提供商 + 降权）。硬停只由状态码/业务码与插件硬停建议决定。
    let stopped = hard_stop(status, code)
        // 插件识别映射的 hard_stop 建议仅对非例外来源生效（用户改过的不被覆盖）。
        || (rule_source != RuleSource::Override && classified.as_ref().is_some_and(|entry| entry.hard_stop));
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
    // Auto 作用域且无上游建议时采纳插件识别映射的 scope 建议。
    if scope == CooldownScope::Auto {
        if let Some(entry) = classified.as_ref() {
            if let Some(suggested) = entry.scope {
                if suggested != CooldownScope::Auto {
                    scope = suggested;
                }
            }
        }
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
    let exception = match rule_source {
        RuleSource::Override => true,
        RuleSource::Plugin => false,
        RuleSource::Global => settings.overrides.iter().any(|item| item.id == rule.id),
    };
    ErrorDecision {
        rule,
        rule_source,
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

/// 统一策略下的硬冷却时长（秒），对齐 v3.5.1 `account_store::policy_cooldowns::cooldown_entry`：
/// - `exponential`：`fallback_seconds × 2^(strikes-1)`，封顶 `fallback_cap_seconds`
/// - 否则：`fallback_seconds`
/// - 不短于 `floor_seconds`（限流/额度类的最短冷却，见 [`cooldown_floor_seconds`]）
/// - 上游给了 `Retry-After` 时：`Fixed` 取 `max(上游, fallback)`，其余取 `max(上游, floor)`
///
/// `strikes` 用账号的连续失败次数（AIGX 侧即 `consecutive_errors`），与上游
/// 「同一冷却键 1800s 内连续失败则 strike+1」等效。
pub fn policy_cooldown_seconds(
    rule: &ErrorRule,
    floor_seconds: u64,
    strikes: u64,
    upstream_retry_after: Option<u64>,
) -> u64 {
    let strikes = strikes.max(1);
    let delay = if rule.exponential {
        rule.fallback_seconds
            .saturating_mul(1u64 << (strikes - 1).min(30))
            .min(rule.fallback_cap_seconds)
    } else {
        rule.fallback_seconds
    };
    let fallback = delay.max(floor_seconds);
    let secs = match (upstream_retry_after, rule.cooldown) {
        (Some(at), CooldownMode::Fixed) => at.max(fallback),
        (Some(at), _) => at.max(floor_seconds),
        (None, _) => fallback,
    };
    secs.max(1)
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

    /// v3.5.1：换账号上限默认 5 → 32，上界 10 → 100（上游 `MAX_SWITCHES`）。
    ///
    /// 两个数字必须和配置面同源：默认值跟 `DEFAULT_RETRY_SWITCH_COUNT` 走，
    /// 上界不能比转发层尝试上限小（否则填了也用不上）。
    #[test]
    fn switch_limit_follows_v351_defaults_and_bound() {
        let defaults = ErrorPolicyConfig::default();
        assert_eq!(defaults.switches, 32);
        assert_eq!(
            defaults.switches,
            u64::from(crate::config::DEFAULT_RETRY_SWITCH_COUNT)
        );
        assert!(defaults.validate().is_ok());

        assert_eq!(MAX_SWITCHES, 100);
        // clippy::field_reassign_with_default：Default 之后逐字段赋值会被判警告，
        // 直接在构造器里给（值语义不变）
        let at_max = ErrorPolicyConfig {
            switches: MAX_SWITCHES,
            ..ErrorPolicyConfig::default()
        };
        assert!(at_max.validate().is_ok(), "上界本身必须可保存");
        let over_max = ErrorPolicyConfig {
            switches: MAX_SWITCHES + 1,
            ..at_max.clone()
        };
        assert!(over_max.validate().is_err(), "超上界必须拒绝");

        // 转发层的尝试次数上界要比配置上界多容纳「首发那次」
        assert_eq!(
            crate::config::MAX_SWITCH_ATTEMPTS,
            crate::config::RETRY_MAX_SWITCH_COUNT + 1
        );
        assert!(crate::config::MAX_SWITCH_ATTEMPTS as u64 > MAX_SWITCHES);
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

    /// v3.5.1 v2：请求有误不再直接拒回，改为换提供商 + 降权（不硬冷却）。
    #[test]
    fn decide_bad_request_switches_provider_and_deprioritizes() {
        let snap = ErrorPolicySnapshot::default();
        let d = decide_with(&snap, "p", "m", 400, "");
        assert!(!d.hard_stop, "v2 起 400 不再无条件硬停");
        assert_eq!(d.action, ErrorAction::SwitchProvider);
        assert!(d.switches());
        assert!(d.deprioritizes());
        assert!(!d.cooldown(), "降权不是硬冷却");
    }

    /// 九条默认规则逐条对照 v3.5.1 `default_rules`(v2)。
    #[test]
    fn default_rules_match_v351_v2() {
        let find = |kind: ErrorKind| {
            default_rules()
                .into_iter()
                .find(|r| r.kind == kind)
                .expect("九条规则齐全")
        };
        for (kind, action, cooldown, secs, exp, scope) in [
            (
                ErrorKind::RateLimit,
                ErrorAction::SwitchAccount,
                CooldownMode::Upstream,
                60,
                false,
                CooldownScope::Auto,
            ),
            (
                ErrorKind::Quota,
                ErrorAction::SwitchAccount,
                CooldownMode::Upstream,
                600,
                true,
                CooldownScope::Auto,
            ),
            (
                ErrorKind::Auth,
                ErrorAction::RefreshThenSwitch,
                CooldownMode::Fixed,
                300,
                true,
                CooldownScope::Account,
            ),
            (
                ErrorKind::Balance,
                ErrorAction::SwitchAccount,
                CooldownMode::Upstream,
                600,
                false,
                CooldownScope::Account,
            ),
            (
                ErrorKind::Transient,
                ErrorAction::RetryThenSwitch,
                CooldownMode::Protection,
                60,
                false,
                CooldownScope::Auto,
            ),
            (
                ErrorKind::Unsupported,
                ErrorAction::SwitchProvider,
                CooldownMode::Deprioritize,
                1800,
                false,
                CooldownScope::Auto,
            ),
            (
                ErrorKind::BadRequest,
                ErrorAction::SwitchProvider,
                CooldownMode::Deprioritize,
                1800,
                false,
                CooldownScope::Auto,
            ),
            (
                ErrorKind::Content,
                ErrorAction::SwitchProvider,
                CooldownMode::Deprioritize,
                1800,
                false,
                CooldownScope::Provider,
            ),
            (
                ErrorKind::Unknown,
                ErrorAction::SwitchAccount,
                CooldownMode::Deprioritize,
                1800,
                false,
                CooldownScope::Auto,
            ),
        ] {
            let r = find(kind);
            assert_eq!(r.action, action, "{} 动作", kind.id());
            assert_eq!(r.cooldown, cooldown, "{} 冷却模式", kind.id());
            assert_eq!(r.fallback_seconds, secs, "{} 冷却时长", kind.id());
            assert_eq!(r.exponential, exp, "{} 指数", kind.id());
            assert_eq!(r.scope, scope, "{} 作用域", kind.id());
        }
    }

    /// v1 默认规则在显式配置加载时升级到 v2；迁移幂等。
    #[test]
    fn v1_defaults_migrate_to_v2_idempotently() {
        let mut config = AppConfig::default();
        let mut settings = ErrorPolicyConfig::default();
        settings.rules = v1_default_rules();
        // 用户手改过一条（限流）——迁移不得覆盖。
        settings.rules[0].fallback_seconds = 45;
        config.error_policy = Some(settings);

        let snap = build_snapshot(&config);
        assert!(snap.configured);
        let auth = snap
            .settings
            .rules
            .iter()
            .find(|r| r.kind == ErrorKind::Auth)
            .unwrap();
        assert_eq!(auth.cooldown, CooldownMode::Fixed, "未改过的 v1 规则升级");
        assert_eq!(auth.fallback_seconds, 300);
        let rate = snap
            .settings
            .rules
            .iter()
            .find(|r| r.kind == ErrorKind::RateLimit)
            .unwrap();
        assert_eq!(rate.fallback_seconds, 45, "用户改过的不被覆盖");

        // 幂等：再迁一次结果不变。
        let mut again = snap.settings.clone();
        migrate_untouched_v1_defaults(&mut again);
        assert_eq!(again.rules, snap.settings.rules);
    }

    /// 冷却时长：floor 抬底、指数翻倍、封顶、Fixed 优先取较大值。
    #[test]
    fn policy_cooldown_seconds_honors_floor_cap_and_exponential() {
        let mut rule = ErrorRule {
            id: "x".into(),
            name: "x".into(),
            kind: ErrorKind::Quota,
            statuses: vec![],
            code: String::new(),
            provider: "*".into(),
            model: String::new(),
            enabled: true,
            action: ErrorAction::SwitchAccount,
            cooldown: CooldownMode::Upstream,
            fallback_seconds: 60,
            exponential: true,
            fallback_cap_seconds: 600,
            scope: CooldownScope::Auto,
        };
        // 第 1 次：60；第 3 次：60×4=240；第 99 次：封顶 600
        assert_eq!(policy_cooldown_seconds(&rule, 0, 1, None), 60);
        assert_eq!(policy_cooldown_seconds(&rule, 0, 3, None), 240);
        assert_eq!(policy_cooldown_seconds(&rule, 0, 99, None), 600);
        // floor 抬底
        assert_eq!(policy_cooldown_seconds(&rule, 300, 1, None), 300);
        // 非指数：固定 fallback，floor 仍抬底
        rule.exponential = false;
        assert_eq!(policy_cooldown_seconds(&rule, 0, 9, None), 60);
        assert_eq!(policy_cooldown_seconds(&rule, 120, 9, None), 120);
        // 上游 Retry-After：Fixed 取 max(上游, fallback)，其余取 max(上游, floor)
        rule.cooldown = CooldownMode::Fixed;
        assert_eq!(policy_cooldown_seconds(&rule, 0, 1, Some(900)), 900);
        assert_eq!(policy_cooldown_seconds(&rule, 0, 1, Some(10)), 60);
        rule.cooldown = CooldownMode::Upstream;
        assert_eq!(policy_cooldown_seconds(&rule, 30, 1, Some(10)), 30);
    }

    /// 能力不支持 501：v2 走换提供商 + 降权，作用域升到 Provider。
    #[test]
    fn decide_unsupported_deprioritizes_provider() {
        let snap = ErrorPolicySnapshot::default();
        let d = decide_with(&snap, "p", "m", 501, "");
        assert!(d.deprioritizes());
        assert!(!d.cooldown());
        assert_eq!(d.scope, CooldownScope::Provider);
        assert_eq!(d.action, ErrorAction::SwitchProvider);
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

    // ── beta.8 插件申报层：三层解析 / 识别映射 / hard-stop ────────────────

    fn plugin_rule(id: &str, kind: ErrorKind, action: ErrorAction) -> PluginErrorRule {
        PluginErrorRule {
            rule: ErrorRule {
                id: format!("plugin:acme:{id}"),
                name: format!("插件规则 {id}"),
                kind,
                statuses: vec![429],
                code: String::new(),
                provider: "acme".into(),
                model: String::new(),
                enabled: true,
                action,
                cooldown: CooldownMode::Upstream,
                fallback_seconds: 300,
                exponential: false,
                fallback_cap_seconds: 1800,
                scope: CooldownScope::Auto,
            },
            plugin_rule_id: id.into(),
            reason: "插件申报".into(),
        }
    }

    #[test]
    fn resolve_layer_order_override_plugin_global() {
        let cfg = ErrorPolicyConfig::default();
        let plugins = vec![plugin_rule(
            "rl",
            ErrorKind::RateLimit,
            ErrorAction::ReturnError,
        )];

        // 插件层命中（provider=acme 且 429）
        let (hit, source) = cfg.resolve_with(&plugins, ErrorKind::RateLimit, 429, "", "acme", "m");
        assert_eq!(hit.id, "plugin:acme:rl");
        assert_eq!(source, RuleSource::Plugin);

        // 其它 provider 不命中插件层 → 全局
        let (global, source) =
            cfg.resolve_with(&plugins, ErrorKind::RateLimit, 429, "", "other", "m");
        assert_eq!(global.id, "rate_limit");
        assert_eq!(source, RuleSource::Global);

        // 用户例外（同 id 覆盖插件规则）最优先
        let mut with_override = cfg.clone();
        with_override.overrides.push(ErrorRule {
            id: "plugin:acme:rl".into(),
            name: "我改过的".into(),
            kind: ErrorKind::RateLimit,
            statuses: vec![429],
            code: String::new(),
            provider: "acme".into(),
            model: String::new(),
            enabled: true,
            action: ErrorAction::SwitchProvider,
            cooldown: CooldownMode::None,
            fallback_seconds: 60,
            exponential: false,
            fallback_cap_seconds: 1800,
            scope: CooldownScope::Provider,
        });
        let (hit, source) =
            with_override.resolve_with(&plugins, ErrorKind::RateLimit, 429, "", "acme", "m");
        assert_eq!(hit.id, "plugin:acme:rl");
        assert_eq!(
            hit.action,
            ErrorAction::SwitchProvider,
            "用户改写后的动作生效"
        );
        assert_eq!(source, RuleSource::Override);
    }

    #[test]
    fn plugin_specificity_model_beats_statuses() {
        let cfg = ErrorPolicyConfig::default();
        let mut narrow = plugin_rule("narrow", ErrorKind::RateLimit, ErrorAction::RetryThenSwitch);
        narrow.rule.model = "acme-pro".into();
        let broad = plugin_rule("broad", ErrorKind::RateLimit, ErrorAction::ReturnError);
        // narrow 在前也会被 broad 先命中？不——特异性评分 model > code > statuses，
        // 窄者胜，与顺序无关。
        let plugins = vec![broad, narrow];
        let (hit, _) =
            cfg.resolve_with(&plugins, ErrorKind::RateLimit, 429, "", "acme", "acme-pro");
        assert_eq!(
            hit.id, "plugin:acme:narrow",
            "带 model 条件的插件规则特异性更高"
        );
    }

    #[test]
    fn declared_classifications_reclassify_and_hard_stop() {
        let snap = ErrorPolicySnapshot::default();
        // 插件说 418 + code=tea_time 是限流 → AIGX 静态推断 418 = Unknown，
        // 识别映射应把 kind 改成 RateLimit。
        let declared = DeclaredErrorPolicy {
            rules: vec![plugin_rule(
                "rl",
                ErrorKind::RateLimit,
                ErrorAction::SwitchAccount,
            )],
            classifications: vec![PluginErrorClassification {
                kind: ErrorKind::RateLimit,
                statuses: vec![418],
                codes: vec![],
                scope: Some(CooldownScope::Model),
                hard_stop: false,
            }],
        };
        let d = decide_with_declared(&snap, &declared, "acme", "m", 418, "");
        assert_eq!(
            d.kind,
            ErrorKind::RateLimit,
            "识别映射改写 Unknown → RateLimit"
        );
        assert_eq!(d.action, ErrorAction::SwitchAccount, "三层解析命中插件规则");
        assert!(!d.hard_stop);

        // hard_stop 建议：非例外来源采纳
        let stopping = DeclaredErrorPolicy {
            rules: vec![],
            classifications: vec![PluginErrorClassification {
                kind: ErrorKind::Unknown,
                statuses: vec![599],
                codes: vec![],
                scope: None,
                hard_stop: true,
            }],
        };
        let d2 = decide_with_declared(&snap, &stopping, "acme", "m", 599, "");
        assert!(d2.hard_stop, "插件 hard_stop 建议生效");
        assert_eq!(d2.action, ErrorAction::ReturnError);
    }

    #[test]
    fn infer_exception_kind_from_plugin_conditions() {
        let cfg = ErrorPolicyConfig::default();
        let mut conditioned =
            plugin_rule("secret-code", ErrorKind::Quota, ErrorAction::SwitchAccount);
        conditioned.rule.statuses.clear();
        conditioned.rule.code = "acme_daily_cap".into();
        // 未知状态码 500 + 业务码 → 插件条件规则反推出 Quota
        let inferred = cfg.infer_exception_kind(&[conditioned], 500, "acme_daily_cap", "acme", "m");
        assert_eq!(inferred, Some(ErrorKind::Quota));
    }

    #[test]
    fn plugin_hard_stop_ignored_when_user_overrode() {
        let mut snap = ErrorPolicySnapshot::default();
        // 用户对同一插件规则 id 建了例外 → 插件的 hard_stop 不再压过用户选择。
        snap.settings.overrides.push(ErrorRule {
            id: "plugin:acme:rl".into(),
            name: "用户例外".into(),
            kind: ErrorKind::RateLimit,
            statuses: vec![418],
            code: String::new(),
            provider: "acme".into(),
            model: String::new(),
            enabled: true,
            action: ErrorAction::SwitchAccount,
            cooldown: CooldownMode::None,
            fallback_seconds: 60,
            exponential: false,
            fallback_cap_seconds: 1800,
            scope: CooldownScope::Auto,
        });
        let declared = DeclaredErrorPolicy {
            rules: vec![],
            classifications: vec![PluginErrorClassification {
                kind: ErrorKind::RateLimit,
                statuses: vec![418],
                codes: vec![],
                scope: None,
                hard_stop: true,
            }],
        };
        let d = decide_with_declared(&snap, &declared, "acme", "m", 418, "");
        assert_eq!(
            d.action,
            ErrorAction::SwitchAccount,
            "用户例外动作生效（插件 hard_stop 不覆盖）"
        );
        assert!(!d.hard_stop);
    }
}
