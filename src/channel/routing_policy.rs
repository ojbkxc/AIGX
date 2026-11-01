//! 路由策略配置 — 对照 Agent2API「设置 → 路由」（RoutingPane / `api/routing_api.rs`）。
//!
//! AIGX 的选路组件已齐（priority+weight 分组、RoundRobin、多因子调度、亲和粘性、
//! 断路器、AIMD），但全部**硬编码**：策略固定走「加权随机 + 多因子重排」，
//! 粘性 TTL 固定 5min/30min，断路器固定 5 次/30s。本模块把这些旋钮收敛成
//! 一份可持久化、可热更新的 `RoutingPolicy`（FileStore 键 `routing_policy`），
//! 并提供按策略分派的选路入口，供 `resolve_bridges` 替换硬编码路径。
//!
//! 与 A2A 的对照：
//! - `strategy`：A2A order/rotate/provider → AIGX weighted（加权随机，现状默认）/
//!   round_robin（确定性轮询）/ ranked（多因子 health×cost×rpm）
//! - `sticky`：A2A auto/session/turn/off → AIGX 通过亲和 TTL 表达：
//!   auto=5min/30min、session=6h/6h、off=关闭亲和
//! - `failover_backoff` / `sink`：AIGX 已有冷却/断路器/健康归档承担，不做开关
//! - 熔断参数：A2A breaker threshold/timeout/success_threshold → AIGX 阈值/冷却
//!   （AIGX 断路器 HalfOpen 成功即恢复，无独立 success_threshold，不强造）
//! - 调度权重：A2A 无此项；AIGX 把闲置的 `set_scheduler_config()` 接上

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::channel::scheduler::SchedulerPolicyConfig;
use crate::storage::FileStore;

/// 持久化键。
pub const ROUTING_POLICY_KEY: &str = "routing_policy";

/// 选路策略。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// 加权随机 + 同优先级分组（`select_for_model` 现状，默认）
    #[default]
    Weighted,
    /// 确定性轮询（`select_for_model_round_robin`，均匀分布）
    RoundRobin,
    /// 多因子调度（health × cost × rpm 几何加权，`select_for_model_ranked`）
    Ranked,
}

impl RouteStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
            Self::Ranked => "ranked",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "weighted" => Some(Self::Weighted),
            "round_robin" => Some(Self::RoundRobin),
            "ranked" => Some(Self::Ranked),
            _ => None,
        }
    }
}

/// 会话粘性档位（决定亲和缓存 TTL；对照 A2A routingSticky 四档）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickyMode {
    /// 5 分钟粘性 / 30 分钟硬过期（A2A auto，多数场景默认）
    #[default]
    Auto,
    /// 6 小时粘性（长会话吃满上游上下文缓存；A2A session）
    Session,
    /// 每轮重选：与 Off 同为不粘（A2A turn；AIGX 无独立会话记录面，
    /// 语义上与 Off 等效，保留该档以对齐 API 契约）
    Turn,
    /// 关闭亲和，每请求独立选路（A2A off）
    Off,
}

impl StickyMode {
    /// 返回 `(sticky_ttl_secs, hard_ttl_secs)`；Turn/Off 返回 None（不粘）。
    pub fn ttls(&self) -> Option<(u64, u64)> {
        match self {
            Self::Auto => Some((5 * 60, 30 * 60)),
            Self::Session => Some((6 * 3600, 6 * 3600)),
            Self::Turn | Self::Off => None,
        }
    }

    /// 是否参与亲和（读取与写入两侧统一判据）。
    pub fn sticky_enabled(&self) -> bool {
        self.ttls().is_some()
    }

    /// 数据面便捷判据：从策略快照取「本请求是否写/读亲和」。
    pub fn sticky_enabled_from(policy: &RoutingPolicy) -> bool {
        policy.sticky.sticky_enabled()
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Session => "session",
            Self::Turn => "turn",
            Self::Off => "off",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "session" => Some(Self::Session),
            "turn" => Some(Self::Turn),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// 断路器参数（对照 A2A routingBreaker*；A2A 默认关 / 5 次 / 30s / 半开成功 2 次）。
///
/// **不可 derive(Default)**：`#[serde(default = "...")]` 只作用于**反序列化**；
/// derive 出来的 `Default::default()` 给的是各字段的类型零值
/// （threshold=0 / cooldown_secs=0 / success_threshold=0），而非这里的业务默认
/// （5 / 30 / 2）。阈值 0 意味着断路器在**首次失败**就打开 —— 那等于把整条
/// 渠道链路停掉。手写 impl 见下。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BreakerPolicy {
    /// 断路器开关（false = 不熔断，只保留冷却/AIMD）
    #[serde(default = "default_breaker_enabled")]
    pub enabled: bool,
    /// 连续失败多少次打开断路器
    #[serde(default = "default_breaker_threshold")]
    pub threshold: u32,
    /// 打开后冷却秒数（HalfOpen 试探前）
    #[serde(default = "default_breaker_cooldown")]
    pub cooldown_secs: u64,
    /// HalfOpen 连续成功多少次才真正关闭（A2A routingBreakerSuccessThreshold；
    /// 1 = 单次试探成功即恢复现状行为）
    #[serde(default = "default_breaker_success_threshold")]
    pub success_threshold: u32,
}

fn default_breaker_enabled() -> bool {
    false
}
fn default_breaker_threshold() -> u32 {
    5
}
fn default_breaker_cooldown() -> u64 {
    30
}
fn default_breaker_success_threshold() -> u32 {
    2
}

/// 手写（非 derive）：默认值走上面那组 `default_breaker_*`，与反序列化口径一致。
impl Default for BreakerPolicy {
    fn default() -> Self {
        Self {
            enabled: default_breaker_enabled(),
            threshold: default_breaker_threshold(),
            cooldown_secs: default_breaker_cooldown(),
            success_threshold: default_breaker_success_threshold(),
        }
    }
}

/// 路由策略整份配置。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoutingPolicy {
    /// 选路策略
    #[serde(default)]
    pub strategy: RouteStrategy,
    /// 会话粘性档位
    #[serde(default)]
    pub sticky: StickyMode,
    /// 断路器参数
    #[serde(default)]
    pub breaker: BreakerPolicy,
    /// 多因子调度权重（strategy=ranked 时生效；健康/成本/RPM）
    #[serde(default)]
    pub scheduler_weights: SchedulerPolicyConfig,
}

/// 路由策略运行时持有者：进程级快照，保存即生效。
///
/// 数据面每请求 `snapshot()`（Arc clone），读写均无锁竞争。
pub struct RoutingPolicyStore {
    current: parking_lot::RwLock<Arc<RoutingPolicy>>,
    store: Option<Arc<FileStore>>,
}

impl RoutingPolicyStore {
    /// 无持久化构建（测试用；配置不落盘）。
    pub fn in_memory(policy: RoutingPolicy) -> Self {
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: None,
        }
    }

    /// 带持久化构建（启动时调用，自动加载已存配置）。
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let policy = store
            .get::<RoutingPolicy>(ROUTING_POLICY_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            current: parking_lot::RwLock::new(Arc::new(policy)),
            store: Some(store),
        }
    }

    /// 当前生效配置快照（每请求调用，Arc clone 廉价）。
    pub fn snapshot(&self) -> Arc<RoutingPolicy> {
        self.current.read().clone()
    }

    /// 更新并持久化（返回生效值）。
    pub fn update(&self, policy: RoutingPolicy) -> anyhow::Result<RoutingPolicy> {
        if let Some(store) = &self.store {
            store.put(ROUTING_POLICY_KEY, &policy)?;
        }
        *self.current.write() = Arc::new(policy.clone());
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_a2a_semantics() {
        let p = RoutingPolicy::default();
        assert_eq!(p.strategy, RouteStrategy::Weighted);
        assert_eq!(p.sticky, StickyMode::Auto);
        // A2A 断路器默认关（routingBreakerEnabled=false），AIGX 对齐
        assert!(!p.breaker.enabled);
        assert_eq!(p.breaker.threshold, 5);
        assert_eq!(p.breaker.cooldown_secs, 30);
    }

    #[test]
    fn sticky_ttls() {
        assert_eq!(
            StickyMode::Auto.ttls(),
            Some((5 * 60, 30 * 60)),
            "auto = 5min 粘性 / 30min 硬过期"
        );
        assert_eq!(StickyMode::Session.ttls(), Some((6 * 3600, 6 * 3600)));
        assert_eq!(StickyMode::Off.ttls(), None);
    }

    #[test]
    fn serde_roundtrip_strategy_names() {
        // 蛇形枚举名 = API 契约（前端 / api 均用 snake_case）
        let json = serde_json::to_value(RouteStrategy::RoundRobin).unwrap();
        assert_eq!(json, serde_json::json!("round_robin"));
        let s: RouteStrategy = serde_json::from_value(json).unwrap();
        assert_eq!(s, RouteStrategy::RoundRobin);
        assert_eq!(
            RouteStrategy::from_str_opt("ranked"),
            Some(RouteStrategy::Ranked)
        );
        assert_eq!(RouteStrategy::from_str_opt("bogus"), None);
    }

    #[test]
    fn policy_roundtrip_defaults() {
        // 旧数据（缺字段）反序列化回默认值
        let v: RoutingPolicy =
            serde_json::from_str(r#"{"strategy":"ranked","sticky":"session"}"#).unwrap();
        assert_eq!(v.strategy, RouteStrategy::Ranked);
        assert_eq!(v.sticky, StickyMode::Session);
        assert_eq!(v.breaker, BreakerPolicy::default());
        assert_eq!(v.scheduler_weights, SchedulerPolicyConfig::default());
    }

    #[test]
    fn store_update_hot_swap() {
        let store = RoutingPolicyStore::in_memory(RoutingPolicy::default());
        let mut p = store.snapshot();
        assert_eq!(p.strategy, RouteStrategy::Weighted);
        let next = RoutingPolicy {
            strategy: RouteStrategy::RoundRobin,
            ..RoutingPolicy::default()
        };
        store.update(next).unwrap();
        p = store.snapshot();
        assert_eq!(p.strategy, RouteStrategy::RoundRobin);
    }

    #[test]
    fn turn_mode_is_non_sticky() {
        // turn：不粘（TTL None）、sticky_enabled false；序列化为 "turn"
        assert_eq!(StickyMode::Turn.ttls(), None);
        assert!(!StickyMode::Turn.sticky_enabled());
        assert_eq!(StickyMode::from_str_opt("turn"), Some(StickyMode::Turn));
        let json = serde_json::to_value(StickyMode::Turn).unwrap();
        assert_eq!(json, serde_json::json!("turn"));
        // 旧档位不受影响
        assert!(StickyMode::Auto.sticky_enabled());
        assert!(StickyMode::Session.sticky_enabled());
        assert!(!StickyMode::Off.sticky_enabled());
    }

    #[test]
    fn breaker_policy_success_threshold_default() {
        // A2A 对齐：默认关 / 阈值 5 / 冷却 30s / 半开成功 2 次
        let d = BreakerPolicy::default();
        assert!(!d.enabled);
        assert_eq!(d.threshold, 5);
        assert_eq!(d.cooldown_secs, 30);
        assert_eq!(d.success_threshold, 2);
        // 旧数据（无 success_threshold 字段）回落默认 2
        let v: BreakerPolicy = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(v.enabled);
        assert_eq!(v.success_threshold, 2);
    }

    #[test]
    fn sticky_enabled_from_policy() {
        let mut p = RoutingPolicy::default();
        assert!(StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Turn;
        assert!(!StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Off;
        assert!(!StickyMode::sticky_enabled_from(&p));
        p.sticky = StickyMode::Session;
        assert!(StickyMode::sticky_enabled_from(&p));
    }
}
