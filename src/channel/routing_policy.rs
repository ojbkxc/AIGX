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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// 加权随机 + 同优先级分组（`select_for_model` 现状，默认）
    Weighted,
    /// 确定性轮询（`select_for_model_round_robin`，均匀分布）
    RoundRobin,
    /// 多因子调度（health × cost × rpm 几何加权，`select_for_model_ranked`）
    Ranked,
}

impl Default for RouteStrategy {
    fn default() -> Self {
        Self::Weighted
    }
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

/// 会话粘性档位（决定亲和缓存 TTL；对照 A2A routingSticky）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StickyMode {
    /// 5 分钟粘性 / 30 分钟硬过期（A2A auto，多数场景默认）
    Auto,
    /// 6 小时粘性（长会话吃满上游上下文缓存；A2A session）
    Session,
    /// 关闭亲和，每请求独立选路（A2A off）
    Off,
}

impl Default for StickyMode {
    fn default() -> Self {
        Self::Auto
    }
}

impl StickyMode {
    /// 返回 `(sticky_ttl_secs, hard_ttl_secs)`；Off 返回 None。
    pub fn ttls(&self) -> Option<(u64, u64)> {
        match self {
            Self::Auto => Some((5 * 60, 30 * 60)),
            Self::Session => Some((6 * 3600, 6 * 3600)),
            Self::Off => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Session => "session",
            Self::Off => "off",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "session" => Some(Self::Session),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

/// 断路器参数（对照 A2A routingBreaker*；A2A 默认 5 次 / 60s）。
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

impl Default for BreakerPolicy {
    fn default() -> Self {
        Self {
            enabled: default_breaker_enabled(),
            threshold: default_breaker_threshold(),
            cooldown_secs: default_breaker_cooldown(),
        }
    }
}

/// 路由策略整份配置。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            strategy: RouteStrategy::default(),
            sticky: StickyMode::default(),
            breaker: BreakerPolicy::default(),
            scheduler_weights: SchedulerPolicyConfig::default(),
        }
    }
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
        let mut next = RoutingPolicy::default();
        next.strategy = RouteStrategy::RoundRobin;
        store.update(next).unwrap();
        p = store.snapshot();
        assert_eq!(p.strategy, RouteStrategy::RoundRobin);
    }
}
