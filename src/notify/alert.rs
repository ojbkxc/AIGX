//! 告警规则引擎——参照 burncloud crates/service/crates/alert 的
//! AlertRuleEvaluator（rules.rs/types.rs）移植适配 AIGX 单 crate 结构。
//!
//! 设计要点（与 burncloud 对齐）：
//! - `AlertType` 按「判别式 + 实体 ID」分组（get_alert_key），
//!   同类型告警共享静默期，避免告警风暴；
//! - `AlertRuleEvaluator::evaluate(type, current)` 阈值触发 + 静默期判定；
//! - 触发的 Alert 经 NotifyService 分发（Telegram/Email，见 notify/mod.rs）。
//!
//! 差异（AIGX 简化）：
//! - AlertType 不携带实时数值字段（burncloud 在枚举里带 latency_ms 等瞬时值），
//!   统一在 message 里渲染，避免 evaluate 时构造重复实例的样板代码；
//! - 通知渠道复用现有 NotifyService（Telegram + SMTP），
//!   不重复实现 Slack/Email 通道（slack/webhook 见 notify_channels 部分）。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;

// ── 类型定义 ─────────────────────────────────────────────────────────

/// 告警级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertLevel {
    Info,
    Warning,
    Critical,
}

impl AlertLevel {
    /// 小写字符串形式（供通知渠道/Webhook 载荷用）。
    pub fn as_str(&self) -> &'static str {
        match self {
            AlertLevel::Info => "info",
            AlertLevel::Warning => "warning",
            AlertLevel::Critical => "critical",
        }
    }
}

impl std::fmt::Display for AlertLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertLevel::Info => write!(f, "Info"),
            AlertLevel::Warning => write!(f, "Warning"),
            AlertLevel::Critical => write!(f, "Critical"),
        }
    }
}

/// 告警类型（kind + 实体 ID 决定分组 key）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AlertKind {
    /// 渠道连续失败次数
    ChannelFailure { channel_id: String },
    /// 渠道平均延迟（ms）
    ChannelHighLatency { channel_id: String },
    /// 渠道剩余额度百分比（current=100-percent，超阈值触发）
    ChannelQuotaLow { channel_id: String },
    /// 内存使用率（%）
    MemoryHigh,
    /// 请求队列积压
    QueueBacklog,
    /// 用户额度耗尽
    UserQuotaExhausted { user_id: String },
    /// 异常流量（req/min）
    AbnormalTraffic,
    /// 成本环比增幅（%）
    CostAnomaly,
    /// 账号登录态已被上游连续拒绝，需人工重新登录（#35 C1）。
    ///
    /// 只给**不可自动续期**的家（loomy / deepseek_web / kuku 这类
    /// `!supports_refresh`）——可续期家走 `mark_credential_rejected` 的刷新链，
    /// 不该出现在「等人来登」的面板上。
    AccountReloginRequired { account_id: String },
    /// 账号登录态即将到期（固定寿命上游，提前 3 天提醒；#35 C2）。
    AccountReloginSoon { account_id: String },
}

impl std::fmt::Display for AlertKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AlertKind::ChannelFailure { channel_id } => {
                write!(f, "ChannelFailure({channel_id})")
            }
            AlertKind::ChannelHighLatency { channel_id } => {
                write!(f, "ChannelHighLatency({channel_id})")
            }
            AlertKind::ChannelQuotaLow { channel_id } => {
                write!(f, "ChannelQuotaLow({channel_id})")
            }
            AlertKind::MemoryHigh => write!(f, "MemoryHigh"),
            AlertKind::QueueBacklog => write!(f, "QueueBacklog"),
            AlertKind::UserQuotaExhausted { user_id } => {
                write!(f, "UserQuotaExhausted({user_id})")
            }
            AlertKind::AbnormalTraffic => write!(f, "AbnormalTraffic"),
            AlertKind::CostAnomaly => write!(f, "CostAnomaly"),
            AlertKind::AccountReloginRequired { account_id } => {
                write!(f, "AccountReloginRequired({account_id})")
            }
            AlertKind::AccountReloginSoon { account_id } => {
                write!(f, "AccountReloginSoon({account_id})")
            }
        }
    }
}

/// 告警状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertStatus {
    Active,
    Resolved,
}

/// 一条已触发的告警
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub kind: AlertKind,
    pub level: AlertLevel,
    pub status: AlertStatus,
    pub message: String,
    pub triggered_at: i64,
    pub resolved_at: Option<i64>,
    pub trigger_count: u32,
}

/// 告警规则：kind（判别式匹配）+ 阈值 + 静默期
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertRule {
    pub name: String,
    pub kind: AlertKind,
    /// 触发阈值（current >= threshold 时触发）
    pub threshold: u64,
    /// 静默期秒数（同 key 告警在窗口内不重复通知）
    #[serde(default = "default_silence")]
    pub silence_period_secs: u64,
    pub level: AlertLevel,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_silence() -> u64 {
    300
}

fn default_true() -> bool {
    true
}

impl AlertRule {
    /// 系统默认规则集（burncloud 同款阈值基线）
    pub fn defaults() -> Vec<AlertRule> {
        vec![
            AlertRule {
                name: "channel_failure".into(),
                kind: AlertKind::ChannelFailure {
                    channel_id: "*".into(),
                },
                threshold: 10,
                silence_period_secs: 600,
                level: AlertLevel::Critical,
                enabled: true,
            },
            AlertRule {
                name: "channel_high_latency".into(),
                kind: AlertKind::ChannelHighLatency {
                    channel_id: "*".into(),
                },
                threshold: 30_000,
                silence_period_secs: 600,
                level: AlertLevel::Warning,
                enabled: true,
            },
            AlertRule {
                name: "memory_high".into(),
                kind: AlertKind::MemoryHigh,
                threshold: 85,
                silence_period_secs: 300,
                level: AlertLevel::Warning,
                enabled: true,
            },
            AlertRule {
                name: "user_quota_exhausted".into(),
                kind: AlertKind::UserQuotaExhausted {
                    user_id: "*".into(),
                },
                threshold: 1,
                silence_period_secs: 300,
                level: AlertLevel::Info,
                enabled: true,
            },
        ]
    }
}

// ── 规则评估器 ───────────────────────────────────────────────────────

/// 告警历史环形缓冲上限（超出丢弃最旧）。
pub const ALERT_HISTORY_LIMIT: usize = 500;

/// 持久化 key：FileStore 中的规则集 JSON。
const RULES_STORE_KEY: &str = "alert_rules";
/// 持久化 key：FileStore 中的历史记录 JSON。
const HISTORY_STORE_KEY: &str = "alert_history";

/// 告警规则评估器（线程安全版本——burncloud 为 &mut self，AIGX 部署在
/// 后台巡检任务里用 Mutex 包裹）
pub struct AlertRuleEvaluator {
    /// 活跃告警（静默期跟踪 + 触发计数）
    active_alerts: HashMap<String, Alert>,
    /// 已配置规则
    rules: Vec<AlertRule>,
    /// 触发历史（环形，最新在前）
    history: Vec<Alert>,
}

impl AlertRuleEvaluator {
    pub fn new(rules: Vec<AlertRule>) -> Self {
        Self {
            active_alerts: HashMap::new(),
            rules,
            history: Vec::new(),
        }
    }

    /// 从 FileStore 加载规则集（无记录时用默认规则并回写）。
    pub fn load_or_default(store: &crate::storage::FileStore) -> Self {
        let rules: Vec<AlertRule> = match store.get::<Vec<AlertRule>>(RULES_STORE_KEY) {
            Ok(Some(rules)) if !rules.is_empty() => rules,
            _ => {
                let defaults = AlertRule::defaults();
                if let Err(e) = store.put(RULES_STORE_KEY, &defaults) {
                    tracing::warn!("persist default alert rules failed: {e}");
                }
                defaults
            }
        };
        let mut ev = Self::new(rules);
        // 历史记录加载（失败不阻塞启动）
        match store.get::<Vec<Alert>>(HISTORY_STORE_KEY) {
            Ok(Some(h)) => ev.history = h,
            Ok(None) => {}
            Err(e) => tracing::warn!("load alert history failed: {e}"),
        }
        ev
    }

    /// 把当前规则集持久化到 FileStore。
    pub fn persist_rules(&self, store: &crate::storage::FileStore) {
        if let Err(e) = store.put(RULES_STORE_KEY, &self.rules) {
            tracing::warn!("persist alert rules failed: {e}");
        }
    }

    /// 把历史记录持久化到 FileStore。
    pub fn persist_history(&self, store: &crate::storage::FileStore) {
        if let Err(e) = store.put(HISTORY_STORE_KEY, &self.history) {
            tracing::warn!("persist alert history failed: {e}");
        }
    }

    /// 触发历史（最新在前，环形上限 500）。
    pub fn history(&self) -> &[Alert] {
        &self.history
    }

    pub fn add_rule(&mut self, rule: AlertRule) {
        tracing::info!(rule = %rule.name, "alert rule added");
        self.rules.push(rule);
    }

    pub fn rules(&self) -> &[AlertRule] {
        &self.rules
    }

    pub fn set_rules(&mut self, rules: Vec<AlertRule>) {
        self.rules = rules;
    }

    /// 分组 key：kind 判别式 + 实体 ID（通配规则 "*" 匹配任意实体）
    fn rule_matches(rule: &AlertRule, kind: &AlertKind) -> bool {
        match (&rule.kind, kind) {
            (
                AlertKind::ChannelFailure { channel_id: a },
                AlertKind::ChannelFailure { channel_id: b },
            ) => a == b || a == "*",
            (
                AlertKind::ChannelHighLatency { channel_id: a },
                AlertKind::ChannelHighLatency { channel_id: b },
            ) => a == b || a == "*",
            (
                AlertKind::ChannelQuotaLow { channel_id: a },
                AlertKind::ChannelQuotaLow { channel_id: b },
            ) => a == b || a == "*",
            (
                AlertKind::UserQuotaExhausted { user_id: a },
                AlertKind::UserQuotaExhausted { user_id: b },
            ) => a == b || a == "*",
            (AlertKind::MemoryHigh, AlertKind::MemoryHigh) => true,
            (AlertKind::QueueBacklog, AlertKind::QueueBacklog) => true,
            (AlertKind::AbnormalTraffic, AlertKind::AbnormalTraffic) => true,
            (AlertKind::CostAnomaly, AlertKind::CostAnomaly) => true,
            _ => false,
        }
    }

    /// 评估：current >= threshold 且不在静默期 → 触发告警
    pub fn evaluate(&mut self, kind: &AlertKind, current_value: u64) -> Option<Alert> {
        let rule = self
            .rules
            .iter()
            .find(|r| r.enabled && Self::rule_matches(r, kind))?;

        if current_value < rule.threshold {
            return None;
        }

        let alert_key = kind.to_string();
        // 静默期检查
        if let Some(existing) = self.active_alerts.get(&alert_key) {
            let elapsed = Utc::now().timestamp() - existing.triggered_at;
            if elapsed < rule.silence_period_secs as i64 {
                return None;
            }
        }

        let alert = Alert {
            id: Uuid::new_v4().to_string(),
            kind: kind.clone(),
            level: rule.level,
            status: AlertStatus::Active,
            message: Self::generate_message(kind, current_value, rule.threshold),
            triggered_at: Utc::now().timestamp(),
            resolved_at: None,
            trigger_count: self
                .active_alerts
                .get(&alert_key)
                .map(|a| a.trigger_count + 1)
                .unwrap_or(1),
        };
        self.active_alerts.insert(alert_key, alert.clone());
        // 记入历史环形缓冲（最新在前，超限丢最旧）
        self.history.insert(0, alert.clone());
        self.history.truncate(ALERT_HISTORY_LIMIT);
        Some(alert)
    }

    /// 事件型告警：不经阈值规则，由触发方直接给文案（#35 登录态提醒链）。
    ///
    /// 与 [`Self::evaluate`] 的三点区别：
    /// 1. **不查 `rules`**——登录态提醒不是「指标越限」，不该要求管理员先建
    ///    规则才生效（默认规则集里没有这两类 kind；若仍走 `evaluate`，#35 的
    ///    提醒会在出厂状态下静默失效）；
    /// 2. `message` 由调用方给定（只有触发方知道账号名等上下文）；
    /// 3. 只写**站内**告警面（`active_alerts` + `history`），**不经 NotifyService
    ///    分发**——lead 约束「C2 提醒走站内通知，别动 Telegram/Email/Slack/
    ///    Webhook 那 4 个渠道」，所以这里刻意不调 notify 链。
    ///
    /// 去重：同 key 在 `silence_secs` 窗口内重复调用返回 `None`（不叠加、不
    /// 刷历史）。跨进程的持久化去重位在 `AccountSession::relogin_notified_at`
    /// （#35 C2），这里的静默只是「一轮风暴内不刷屏」的兜底。
    pub fn raise_event(
        &mut self,
        kind: &AlertKind,
        level: AlertLevel,
        message: String,
        silence_secs: u64,
    ) -> Option<Alert> {
        let alert_key = kind.to_string();
        let now = Utc::now().timestamp();
        if let Some(existing) = self.active_alerts.get(&alert_key) {
            if now - existing.triggered_at < silence_secs as i64 {
                return None;
            }
        }
        let alert = Alert {
            id: Uuid::new_v4().to_string(),
            kind: kind.clone(),
            level,
            status: AlertStatus::Active,
            message,
            triggered_at: now,
            resolved_at: None,
            trigger_count: self
                .active_alerts
                .get(&alert_key)
                .map(|a| a.trigger_count + 1)
                .unwrap_or(1),
        };
        self.active_alerts.insert(alert_key, alert.clone());
        self.history.insert(0, alert.clone());
        self.history.truncate(ALERT_HISTORY_LIMIT);
        Some(alert)
    }

    /// 评估并自动恢复：值回到阈值以下时解决该活跃告警（08-prod P2-5）。
    ///
    /// 此前巡检只触发不恢复——渠道延迟降回正常（或渠道不再被调用、拿不到
    /// 新样本）后告警仍永久 active，成了噪音面板。这里在 `evaluate` 的
    /// 「低于阈值 → None」分支上顺带 resolve：巡检下一轮看到值正常就关闭。
    /// 返回 Recovered = 本次**恢复**的告警（与触发对称，调用方可分发
    /// 「已恢复」通知）。
    pub fn evaluate_with_recovery(&mut self, kind: &AlertKind, current_value: u64) -> Outcome {
        let below = self
            .rules
            .iter()
            .find(|r| r.enabled && Self::rule_matches(r, kind))
            .is_none_or(|rule| current_value < rule.threshold);
        if below {
            // 值正常（或无规则）：活跃告警自动恢复
            if let Some(alert) = self.resolve(kind) {
                return Outcome::Recovered(alert);
            }
            return Outcome::None;
        }
        match self.evaluate(kind, current_value) {
            Some(alert) => Outcome::Triggered(alert),
            None => Outcome::None,
        }
    }

    /// 标记告警解决（同步记入历史）
    pub fn resolve(&mut self, kind: &AlertKind) -> Option<Alert> {
        let alert_key = kind.to_string();
        self.resolve_by_key(&alert_key)
    }

    /// 当前活跃告警（拷贝快照）
    pub fn active_alerts(&self) -> Vec<Alert> {
        self.active_alerts.values().cloned().collect()
    }

    /// 按告警 id 标记解决（同步记入历史）。
    /// 活跃告警的 map key 是 kind 的 Display 串，前端只持有告警 id，
    /// 因此这里先按 id 反查 kind 再 resolve。
    pub fn resolve_by_id(&mut self, id: &str) -> Option<Alert> {
        let key = self
            .active_alerts
            .iter()
            .find(|(_, a)| a.id == id)
            .map(|(k, _)| k.clone())?;
        self.resolve_by_key(&key)
    }

    /// resolve 的共用实现：按 map key 移除活跃告警、置 Resolved 并记入历史
    fn resolve_by_key(&mut self, key: &str) -> Option<Alert> {
        if let Some(mut alert) = self.active_alerts.remove(key) {
            alert.status = AlertStatus::Resolved;
            alert.resolved_at = Some(Utc::now().timestamp());
            self.history.insert(0, alert.clone());
            if self.history.len() > ALERT_HISTORY_LIMIT {
                self.history.truncate(ALERT_HISTORY_LIMIT);
            }
            Some(alert)
        } else {
            None
        }
    }

    /// 告警消息（脱敏——不带密钥/上游 URL 等敏感数据）
    fn generate_message(kind: &AlertKind, current: u64, threshold: u64) -> String {
        match kind {
            AlertKind::ChannelFailure { channel_id } => {
                format!("渠道 {channel_id} 失败 {current} 次（阈值 {threshold}）")
            }
            AlertKind::ChannelHighLatency { channel_id } => {
                format!("渠道 {channel_id} 延迟 {current}ms（阈值 {threshold}ms）")
            }
            AlertKind::ChannelQuotaLow { channel_id } => {
                format!("渠道 {channel_id} 剩余额度不足（阈值 {threshold}%）")
            }
            AlertKind::MemoryHigh => format!("内存使用率 {current}%（阈值 {threshold}%）"),
            AlertKind::QueueBacklog => format!("请求队列积压 {current}（阈值 {threshold}）"),
            AlertKind::UserQuotaExhausted { user_id } => {
                format!("用户 {user_id} 额度已耗尽（阈值 {threshold}）")
            }
            AlertKind::AbnormalTraffic => {
                format!("异常流量 {current} req/min（阈值 {threshold}）")
            }
            AlertKind::CostAnomaly => format!("成本异常增长 {current}%（阈值 {threshold}%）"),
            // 事件型告警（#35）：不走阈值规则，文案由触发方经
            // [`AlertRuleEvaluator::raise_event`] 传入；这里只是兜底
            // （万一有人给这两个 kind 配了阈值规则也不会漏臂）。
            AlertKind::AccountReloginRequired { account_id } => {
                format!("账号 {account_id} 登录态已被上游拒绝 {current} 次，需重新登录")
            }
            AlertKind::AccountReloginSoon { account_id } => {
                format!("账号 {account_id} 登录态即将到期，请提前重新登录")
            }
        }
    }

    /// 静默期时长（测试用）
    #[allow(dead_code)]
    pub fn silence_of(&self, kind: &AlertKind) -> Option<Duration> {
        let rule = self
            .rules
            .iter()
            .find(|r| r.enabled && Self::rule_matches(r, kind))?;
        Some(Duration::from_secs(rule.silence_period_secs))
    }
}

/// [`AlertRuleEvaluator::evaluate_with_recovery`] 的三态结果。
#[derive(Debug, Clone)]
pub enum Outcome {
    /// 触发了新告警（或静默期外的重复触发）
    Triggered(Alert),
    /// 值回常态，活跃告警被自动解决
    Recovered(Alert),
    /// 无事发生（低于阈值且无活跃告警 / 规则禁用）
    None,
}

// ── 测试 ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rule_evaluation_below_threshold() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::ChannelFailure {
            channel_id: "ch1".into(),
        };
        assert!(ev.evaluate(&k, 3).is_none());
    }

    #[test]
    fn test_rule_evaluation_at_threshold() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::ChannelFailure {
            channel_id: "ch1".into(),
        };
        let alert = ev.evaluate(&k, 10).expect("should trigger");
        assert_eq!(alert.level, AlertLevel::Critical);
        assert_eq!(alert.status, AlertStatus::Active);
        assert_eq!(alert.trigger_count, 1);
        assert!(alert.message.contains("ch1"));
    }

    #[test]
    fn test_silence_period() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::ChannelFailure {
            channel_id: "ch1".into(),
        };
        // 第一次触发
        assert!(ev.evaluate(&k, 10).is_some());
        // 静默期内第二次（数值更高也压制）
        assert!(ev.evaluate(&k, 20).is_none(), "should be silenced");
    }

    #[test]
    fn test_resolve_alert() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::MemoryHigh;
        ev.evaluate(&k, 90);
        let resolved = ev.resolve(&k).expect("should resolve");
        assert_eq!(resolved.status, AlertStatus::Resolved);
        assert!(resolved.resolved_at.is_some());
        assert!(ev.active_alerts().is_empty());
    }

    #[test]
    fn evaluate_with_recovery_auto_resolves_when_back_to_normal() {
        // 08-prod P2-5：高延迟告警触发后，值降回阈值以下 → 下一轮巡检自动恢复
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::ChannelHighLatency {
            channel_id: "ch1".into(),
        };
        // 超阈值 → 触发
        match ev.evaluate_with_recovery(&k, 55_000) {
            Outcome::Triggered(alert) => assert_eq!(alert.status, AlertStatus::Active),
            other => panic!("应触发，实际 {other:?}"),
        }
        assert_eq!(ev.active_alerts().len(), 1);
        // 降回正常 → 自动恢复
        match ev.evaluate_with_recovery(&k, 900) {
            Outcome::Recovered(alert) => {
                assert_eq!(alert.status, AlertStatus::Resolved);
                assert!(alert.resolved_at.is_some());
            }
            other => panic!("应恢复，实际 {other:?}"),
        }
        assert!(ev.active_alerts().is_empty());
        // 再查一次：无事发生（无活跃告警可恢复）
        assert!(matches!(ev.evaluate_with_recovery(&k, 900), Outcome::None));
    }

    #[test]
    fn test_wildcard_rule_matches_any_channel() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::ChannelHighLatency {
            channel_id: "any-channel-id".into(),
        };
        let alert = ev.evaluate(&k, 60_000).expect("wildcard should match");
        assert!(alert.message.contains("any-channel-id"));
    }

    #[test]
    fn test_disabled_rule_not_triggered() {
        let rule = AlertRule {
            name: "disabled".into(),
            kind: AlertKind::MemoryHigh,
            threshold: 50,
            silence_period_secs: 60,
            level: AlertLevel::Warning,
            enabled: false,
        };
        let mut ev = AlertRuleEvaluator::new(vec![rule]);
        assert!(ev.evaluate(&AlertKind::MemoryHigh, 90).is_none());
    }

    #[test]
    fn history_records_triggers_and_resolves() {
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        let k = AlertKind::MemoryHigh;
        ev.evaluate(&k, 90);
        ev.evaluate(&k, 95); // 静默期内，不重复记历史
        assert_eq!(ev.history().len(), 1);
        ev.resolve(&k);
        // 触发 + 解决 = 2 条历史
        assert_eq!(ev.history().len(), 2);
        assert_eq!(ev.history()[0].status, AlertStatus::Resolved);
    }

    #[test]
    fn history_ring_buffer_truncates() {
        let rule = AlertRule {
            name: "always".into(),
            kind: AlertKind::QueueBacklog,
            threshold: 1,
            silence_period_secs: 0,
            level: AlertLevel::Info,
            enabled: true,
        };
        let mut ev = AlertRuleEvaluator::new(vec![rule]);
        // 静默期 0：每次都触发
        for _ in 0..(ALERT_HISTORY_LIMIT + 50) {
            ev.evaluate(&AlertKind::QueueBacklog, 5);
        }
        assert_eq!(ev.history().len(), ALERT_HISTORY_LIMIT);
    }

    #[test]
    fn raise_event_triggers_without_any_matching_rule() {
        // #35：默认规则集里没有登录态这两类 kind——事件型提醒必须照样能落地
        // （否则出厂状态下「不可续期家凭据被连续拒绝」的提醒静默失效）。
        let mut ev = AlertRuleEvaluator::new(AlertRule::defaults());
        assert!(ev.rules().iter().all(|r| !matches!(
            r.kind,
            AlertKind::AccountReloginRequired { .. } | AlertKind::AccountReloginSoon { .. }
        )));
        let k = AlertKind::AccountReloginRequired {
            account_id: "loomy-1".into(),
        };
        let alert = ev
            .raise_event(&k, AlertLevel::Warning, "账号 loomy-1 需重新登录".into(), 0)
            .expect("无规则也应触发");
        assert_eq!(alert.level, AlertLevel::Warning);
        assert_eq!(alert.status, AlertStatus::Active);
        assert_eq!(alert.trigger_count, 1);
        assert_eq!(ev.active_alerts().len(), 1);
        assert_eq!(ev.history().len(), 1);
    }

    #[test]
    fn raise_event_silences_duplicates_within_window() {
        // 静默窗口内重复 raise → None（不刷历史、不叠加计数）；窗口外再次触发
        // → trigger_count 递增（沿用 evaluate 的计数口径）。
        let mut ev = AlertRuleEvaluator::new(vec![]);
        let k = AlertKind::AccountReloginSoon {
            account_id: "loomy-1".into(),
        };
        assert!(ev
            .raise_event(&k, AlertLevel::Warning, "a".into(), 3600)
            .is_some());
        assert!(
            ev.raise_event(&k, AlertLevel::Warning, "b".into(), 3600)
                .is_none(),
            "静默期内应压制"
        );
        assert_eq!(ev.active_alerts().len(), 1);
        assert_eq!(ev.history().len(), 1);
        // 静默期 0 = 每轮都可再触发
        let again = ev
            .raise_event(&k, AlertLevel::Warning, "c".into(), 0)
            .expect("静默期外应再触发");
        assert_eq!(again.trigger_count, 2);
    }

    #[test]
    fn raise_event_resolved_allows_new_instance() {
        // 「已处理（解决）」后再到期提醒应能重新落地：resolve 移出 active，
        // 下一次 raise 的 key 不再命中静默判断。
        let mut ev = AlertRuleEvaluator::new(vec![]);
        let k = AlertKind::AccountReloginSoon {
            account_id: "loomy-2".into(),
        };
        ev.raise_event(&k, AlertLevel::Warning, "a".into(), 86_400)
            .unwrap();
        assert!(ev
            .raise_event(&k, AlertLevel::Warning, "b".into(), 86_400)
            .is_none());
        assert!(ev.resolve(&k).is_some());
        let after = ev
            .raise_event(&k, AlertLevel::Warning, "c".into(), 86_400)
            .expect("已解决后应能再提醒");
        assert_eq!(after.trigger_count, 1);
    }

    #[test]
    fn rules_roundtrip_via_store() {
        let dir = std::env::temp_dir().join(format!("aigx-alert-test-{}", uuid::Uuid::new_v4()));
        let store = crate::storage::FileStore::new(dir.clone());
        let ev = AlertRuleEvaluator::load_or_default(&store);
        let n = ev.rules().len();
        ev.persist_rules(&store);

        // 重新加载：规则数量一致
        let ev2 = AlertRuleEvaluator::load_or_default(&store);
        assert_eq!(ev2.rules().len(), n);

        // 修改规则集并持久化 → 再加载应保留修改
        let mut rules = ev2.rules().to_vec();
        rules[0].threshold = 12345;
        let ev3 = AlertRuleEvaluator::new(rules);
        ev3.persist_rules(&store);
        let ev4 = AlertRuleEvaluator::load_or_default(&store);
        assert_eq!(ev4.rules()[0].threshold, 12345);

        let _ = std::fs::remove_dir_all(dir);
    }
}
