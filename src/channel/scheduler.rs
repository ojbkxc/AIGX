//! CombinedScheduler — 多因子几何均值打分的组合调度器。
//!
//! 参照 burncloud `crates/router/src/scheduler/combined.rs`：
//!
//! Score = admin_weight × (health_norm^w_h × cost_norm^w_r × rpm_norm^w_r)
//!
//! - 0.5 偏移 min-max 归一化：小候选集下避免极端失真
//!   （归一化值域 [0.5, 1.0]，任何两渠道分差最多 2 倍）
//! - 退化维度（min == max）权重清零并重新归一
//! - 全维度退化 → 等权（1/3, 1/3, 1/3）
//!
//! 因子来源（AIGX 映射）：
//! - health：`health_manager::ChannelStateTracker` 的 per-channel 错误率换算
//! - cost：`pricing_store` 输入+输出单价倒数（1/price，越便宜分越高）
//! - rpm：AIMD `current_limit`（Learning=1.0，Cooldown=0.1）

use std::collections::HashMap;

/// 单候选的调度因子。
#[derive(Debug, Clone, Copy)]
pub struct CandidateFactors {
    /// 健康分 [0, 1]
    pub health: f64,
    /// 成本分（1/USD 价格，越便宜越大）
    pub cost: f64,
    /// RPM 因子（Learning 期固定 1.0，Cooldown 0.1）
    pub rpm: f64,
}

/// Learning 期的 RPM 因子（容量未知，中性）。
pub const RPM_FACTOR_LEARNING: f64 = 1.0;
/// Cooldown 期的 RPM 因子（严重惩罚）。
pub const RPM_FACTOR_COOLDOWN: f64 = 0.1;

/// 组合调度权重配置。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchedulerPolicyConfig {
    #[serde(default = "default_health_weight")]
    pub health_weight: f64,
    #[serde(default = "default_cost_weight")]
    pub cost_weight: f64,
    #[serde(default = "default_rpm_weight")]
    pub rpm_weight: f64,
}

fn default_health_weight() -> f64 {
    0.4
}
fn default_cost_weight() -> f64 {
    0.4
}
fn default_rpm_weight() -> f64 {
    0.2
}

impl Default for SchedulerPolicyConfig {
    fn default() -> Self {
        Self {
            health_weight: default_health_weight(),
            cost_weight: default_cost_weight(),
            rpm_weight: default_rpm_weight(),
        }
    }
}

impl SchedulerPolicyConfig {
    /// 权重合法：非负、有限、至少一个为正。
    pub fn validate(&self) -> bool {
        let weights = [self.health_weight, self.cost_weight, self.rpm_weight];
        weights
            .iter()
            .all(|w| *w >= 0.0 && w.is_finite() && !w.is_nan())
            && weights.iter().any(|w| *w > 0.0)
    }
}

/// 避免除零的小量。
const EPS: f64 = 1e-6;

/// 组合调度器：健康 × 成本 × RPM 几何均值打分。
pub struct CombinedScheduler {
    config: SchedulerPolicyConfig,
}

impl CombinedScheduler {
    pub fn new(config: SchedulerPolicyConfig) -> Self {
        Self { config }
    }

    /// 打分：返回 channel_id → score（降序即优先级）。
    ///
    /// `candidates`：`(channel_id, admin_weight, factors)` 列表。
    pub fn score(&self, candidates: &[(String, u32, CandidateFactors)]) -> HashMap<String, f64> {
        let n = candidates.len();
        if n == 0 {
            return HashMap::new();
        }

        // 单遍：收集因子 + 跟踪 min/max 供归一化
        let mut raw: Vec<CandidateFactors> = Vec::with_capacity(n);
        let (mut h_min, mut h_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut c_min, mut c_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut r_min, mut r_max) = (f64::INFINITY, f64::NEG_INFINITY);

        for (_, _, f) in candidates {
            let health = f.health.max(0.0);
            let health = if health.is_nan() { 1.0 } else { health };
            h_min = h_min.min(health);
            h_max = h_max.max(health);

            let cost = if f.cost.is_finite() && f.cost > 0.0 {
                f.cost
            } else {
                1.0
            };
            c_min = c_min.min(cost);
            c_max = c_max.max(cost);

            let rpm = if f.rpm.is_nan() {
                RPM_FACTOR_LEARNING
            } else {
                f.rpm
            };
            r_min = r_min.min(rpm);
            r_max = r_max.max(rpm);

            raw.push(CandidateFactors { health, cost, rpm });
        }

        let h_degen = (h_max - h_min).abs() < EPS;
        let c_degen = (c_max - c_min).abs() < EPS;
        let r_degen = (r_max - r_min).abs() < EPS;

        let (w_h, w_c, w_r) = self.effective_weights(h_degen, c_degen, r_degen);

        let h_range = if h_degen { 0.0 } else { h_max - h_min };
        let c_range = if c_degen { 0.0 } else { c_max - c_min };
        let r_range = if r_degen { 0.0 } else { r_max - r_min };

        let mut scores = HashMap::with_capacity(n);
        for ((ch_id, admin_w), f) in candidates.iter().zip(raw.iter()) {
            let h = if h_degen {
                0.75
            } else {
                (0.5 + 0.5 * (f.health - h_min) / h_range).clamp(0.5, 1.0)
            };
            let c = if c_degen {
                0.75
            } else {
                (0.5 + 0.5 * (f.cost - c_min) / c_range).clamp(0.5, 1.0)
            };
            let r = if r_degen {
                0.75
            } else {
                (0.5 + 0.5 * (f.rpm - r_min) / r_range).clamp(0.5, 1.0)
            };

            let quality = h.powf(w_h) * c.powf(w_c) * r.powf(w_r);
            let final_score = (*admin_w).max(1) as f64 * quality;

            let score = if final_score.is_finite() && final_score > 0.0 {
                final_score
            } else {
                0.0
            };
            scores.insert(ch_id.clone(), score);
        }
        scores
    }

    /// 按分数排序的渠道 ID（降序，调度优先级顺序）。
    pub fn rank(&self, candidates: &[(String, u32, CandidateFactors)]) -> Vec<String> {
        let scores = self.score(candidates);
        let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.into_iter().map(|(id, _)| id).collect()
    }

    /// 退化维度权重清零后重新归一。
    fn effective_weights(&self, h_degen: bool, c_degen: bool, r_degen: bool) -> (f64, f64, f64) {
        let mut w_h = if h_degen {
            0.0
        } else {
            self.config.health_weight
        };
        let mut w_c = if c_degen {
            0.0
        } else {
            self.config.cost_weight
        };
        let mut w_r = if r_degen { 0.0 } else { self.config.rpm_weight };

        let total = w_h + w_c + w_r;
        if total <= 0.0 {
            return (1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0);
        }

        w_h /= total;
        w_c /= total;
        w_r /= total;

        (w_h, w_c, w_r)
    }
}

impl Default for CombinedScheduler {
    fn default() -> Self {
        Self::new(SchedulerPolicyConfig::default())
    }
}

// ── 阶段3：调度器 trait + Passthrough + 上下文构建 ──────────────────────
//
// 参照 burncloud `crates/router/src/scheduler/mod.rs`：
// - `ChannelScheduler` trait：无状态、panic-safe 评分策略
// - `PassthroughScheduler`：仅按 admin weight（默认，向后兼容）
// - `rank_candidates`：用 `catch_unwind` 包裹评分，panic/错误回退 passthrough
// - `build_context`：从 `ChannelStateTracker` + `PricingStore` 预计算因子
//
// 与既有 `CombinedScheduler::score`（直接签名）共存：trait 方法名 `score_candidates`
// 避免与既有 `score` 冲突；`impl ChannelScheduler for CombinedScheduler` 做适配。

use std::panic::{catch_unwind, AssertUnwindSafe};

use super::Channel;
use crate::pricing::PricingStore;

/// 每次调度决策的只读上下文（候选维度快照）。
#[derive(Debug, Clone, Default)]
pub struct SchedulingContext {
    /// per-channel 预计算因子（key = channel_id）
    pub factors: HashMap<String, CandidateFactors>,
}

/// 每请求元数据（Server → Router 流入）。
///
/// 携带 L3 亲和性键（user_id / session_id）。评分器不读此结构 —
/// 亲和性在 `ChannelStore::select_for_model_with_affinity` 阶段已处理。
#[derive(Debug, Clone, Default)]
pub struct SchedulingRequest {
    /// 用户 ID（亲和性键）
    pub user_id: Option<String>,
    /// 会话/对话 ID（优先于 user_id 作亲和性键）
    pub session_id: Option<String>,
}

impl SchedulingRequest {
    /// 取亲和性键 — session_id 优先，否则 user_id。
    pub fn affinity_key(&self) -> Option<&str> {
        self.session_id.as_deref().or(self.user_id.as_deref())
    }
}

/// 调度错误。
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("调度失败: {0}")]
    #[allow(dead_code)]
    Internal(String),
}

/// 渠道调度策略 trait。
///
/// 实现须无状态且 panic-safe。`rank_candidates` 用 `catch_unwind` 包裹 `score_candidates`。
pub trait ChannelScheduler: Send + Sync {
    /// 策略名。
    fn name(&self) -> &'static str;

    /// 对候选渠道评分，返回 `channel_id → score` 映射。
    ///
    /// 方法名 `score_candidates` 避免与既有 `CombinedScheduler::score` 冲突。
    fn score_candidates(
        &self,
        candidates: &[(Channel, u32)],
        ctx: &SchedulingContext,
    ) -> Result<HashMap<String, f64>, ScheduleError>;
}

/// Passthrough 调度器 — 仅按管理员 weight 评分（向后兼容默认）。
pub struct PassthroughScheduler;

impl ChannelScheduler for PassthroughScheduler {
    fn name(&self) -> &'static str {
        "passthrough"
    }

    fn score_candidates(
        &self,
        candidates: &[(Channel, u32)],
        _ctx: &SchedulingContext,
    ) -> Result<HashMap<String, f64>, ScheduleError> {
        Ok(candidates
            .iter()
            .map(|(ch, w)| (ch.id.clone(), *w as f64))
            .collect())
    }
}

/// 为既有 `CombinedScheduler` 实现 trait 适配。
///
/// 把 `(Channel, u32)` + `SchedulingContext.factors` 转为既有 `score` 的
/// `(String, u32, CandidateFactors)` 三元组调用。
impl ChannelScheduler for CombinedScheduler {
    fn name(&self) -> &'static str {
        "combined"
    }

    fn score_candidates(
        &self,
        candidates: &[(Channel, u32)],
        ctx: &SchedulingContext,
    ) -> Result<HashMap<String, f64>, ScheduleError> {
        let default_factors = CandidateFactors {
            health: 1.0,
            cost: 1.0,
            rpm: RPM_FACTOR_LEARNING,
        };
        let triples: Vec<(String, u32, CandidateFactors)> = candidates
            .iter()
            .map(|(ch, w)| {
                let f = ctx.factors.get(&ch.id).copied().unwrap_or(default_factors);
                (ch.id.clone(), *w, f)
            })
            .collect();
        Ok(self.score(&triples))
    }
}

/// 调度器类型（per-group 配置）。
#[derive(Debug, Clone)]
pub enum SchedulerKind {
    Passthrough,
    Combined { config: SchedulerPolicyConfig },
}

/// group 名 → 调度器类型映射。
pub type SchedulerPolicyMap = HashMap<String, SchedulerKind>;

/// 从环境变量加载调度策略配置。
///
/// 读取 `SCHEDULER_POLICIES`（JSON），格式：
/// ```json
/// {
///   "vip": { "type": "combined", "health_weight": 0.4, "cost_weight": 0.4, "rpm_weight": 0.2 },
///   "default": { "type": "passthrough" }
/// }
/// ```
///
/// 缺失或解析失败 → 空 map（全走 passthrough）。
pub fn load_scheduler_config() -> SchedulerPolicyMap {
    let json_str = match std::env::var("SCHEDULER_POLICIES") {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let raw: HashMap<String, serde_json::Value> = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("解析 SCHEDULER_POLICIES 失败: {e}");
            return HashMap::new();
        }
    };

    let mut policies = HashMap::new();
    for (group, val) in raw {
        let kind = match serde_json::from_value::<SchedulerPolicyEntry>(val) {
            Ok(entry) => match entry.scheduler_type.as_str() {
                "combined" => {
                    let config = SchedulerPolicyConfig {
                        health_weight: entry.health_weight.unwrap_or_else(default_health_weight),
                        cost_weight: entry.cost_weight.unwrap_or_else(default_cost_weight),
                        rpm_weight: entry.rpm_weight.unwrap_or_else(default_rpm_weight),
                    };
                    if !config.validate() {
                        tracing::warn!("group '{}' 调度权重非法，回退 passthrough", group);
                        SchedulerKind::Passthrough
                    } else {
                        SchedulerKind::Combined { config }
                    }
                }
                _ => SchedulerKind::Passthrough,
            },
            Err(e) => {
                tracing::warn!("解析 group '{}' 调度配置失败: {e}", group);
                SchedulerKind::Passthrough
            }
        };
        policies.insert(group.to_lowercase(), kind);
    }

    tracing::info!("加载了 {} 条调度策略", policies.len());
    policies
}

#[derive(serde::Deserialize)]
struct SchedulerPolicyEntry {
    #[serde(rename = "type", default = "default_type")]
    scheduler_type: String,
    #[serde(default)]
    health_weight: Option<f64>,
    #[serde(default)]
    cost_weight: Option<f64>,
    #[serde(default)]
    rpm_weight: Option<f64>,
}

fn default_type() -> String {
    "passthrough".to_string()
}

/// 按调度评分排序候选，返回排序后的 `(Channel, weight)` 对。
///
/// 用 `catch_unwind` 包裹 `score_candidates` 做 panic 保护。
/// panic 或错误时回退到 passthrough 排序。
pub fn rank_candidates(
    candidates: Vec<(Channel, u32)>,
    ctx: &SchedulingContext,
    scheduler: &dyn ChannelScheduler,
) -> Vec<(Channel, u32)> {
    if candidates.len() <= 1 {
        return candidates;
    }

    let scores = match catch_unwind(AssertUnwindSafe(|| {
        scheduler.score_candidates(&candidates, ctx)
    })) {
        Ok(Ok(map)) => map,
        Ok(Err(e)) => {
            tracing::warn!(
                "调度器 '{}' 返回错误: {e}，回退 passthrough",
                scheduler.name()
            );
            return rank_passthrough(candidates);
        }
        Err(payload) => {
            tracing::error!(
                "调度器 '{}' panic: {}，回退 passthrough",
                scheduler.name(),
                payload.downcast_ref::<&str>().unwrap_or(&"unknown panic")
            );
            return rank_passthrough(candidates);
        }
    };

    let mut candidates = candidates;
    candidates.sort_by(|a, b| {
        let sa = scores.get(&a.0.id).copied().unwrap_or(0.0);
        let sb = scores.get(&b.0.id).copied().unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    candidates
}

/// Passthrough 排序 — 按 admin weight 降序。
pub fn rank_passthrough(mut candidates: Vec<(Channel, u32)>) -> Vec<(Channel, u32)> {
    if candidates.len() <= 1 {
        return candidates;
    }
    candidates.sort_by_key(|b| std::cmp::Reverse(b.1));
    candidates
}

/// 从 `ChannelStateTracker` + `PricingStore` 构建调度上下文。
///
/// - health：从 `health_tracker.get_health(channel_id)` 推算（1 - error_rate），
///   认证/余额故障进一步惩罚
/// - cost：从 `pricing_store.get_price(model)` 反算（1/price，免费或无定价=1.0 最佳）
/// - rpm：由 `rpm_overrides` 传入（来自 AIMD 控制器快照）；缺省 `RPM_FACTOR_LEARNING`
pub fn build_context(
    model: &str,
    candidates: &[(Channel, u32)],
    health_tracker: &super::health_manager::ChannelStateTracker,
    pricing_store: &PricingStore,
    rpm_overrides: Option<&HashMap<String, f64>>,
) -> SchedulingContext {
    let mut factors = HashMap::with_capacity(candidates.len());
    let price_usd = pricing_store
        .get_price(model)
        .map(|p| p.input_price + p.output_price);

    for (ch, _) in candidates {
        let health = if let Some(summary) = health_tracker.get_health(&ch.id) {
            let h = 1.0 - summary.overall_error_rate;
            let h = if !summary.auth_ok { h * 0.1 } else { h };
            let h = if summary.balance_status == super::health_manager::BalanceStatus::Exhausted {
                h * 0.1
            } else {
                h
            };
            h.clamp(0.0, 1.0)
        } else {
            1.0
        };

        let cost = match price_usd {
            Some(p) if p > 0.0 => 1.0 / p,
            _ => 1.0,
        };

        let rpm = rpm_overrides
            .and_then(|m| m.get(&ch.id).copied())
            .unwrap_or(RPM_FACTOR_LEARNING);

        factors.insert(ch.id.clone(), CandidateFactors { health, cost, rpm });
    }

    SchedulingContext { factors }
}

/// 把 AIMD 状态转为 rpm 因子（供 `build_context` 用）。
pub fn aimd_state_to_rpm_factor(
    state: &crate::ratelimit::aimd_limiter::RateLimitState,
    current_limit: u32,
) -> f64 {
    use crate::ratelimit::aimd_limiter::RateLimitState;
    match state {
        RateLimitState::Stable => current_limit as f64,
        RateLimitState::Learning => RPM_FACTOR_LEARNING,
        RateLimitState::Cooldown => RPM_FACTOR_COOLDOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, w: u32, h: f64, c: f64, r: f64) -> (String, u32, CandidateFactors) {
        (
            id.to_string(),
            w,
            CandidateFactors {
                health: h,
                cost: c,
                rpm: r,
            },
        )
    }

    #[test]
    fn prefers_healthier_channel() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 1.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 0.9, 1.0, 10.0),
            cand("c2", 10, 0.5, 1.0, 10.0),
        ]);
        assert!(scores["c1"] > scores["c2"], "更健康的渠道应得分更高");
    }

    #[test]
    fn prefers_cheaper_channel() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 1.0,
            rpm_weight: 0.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 1.0, 0.07, 10.0),  // 更便宜（1/14.3）
            cand("c2", 10, 1.0, 0.002, 10.0), // 更贵（1/500）
        ]);
        assert!(scores["c1"] > scores["c2"], "更便宜的渠道应得分更高");
    }

    #[test]
    fn prefers_higher_rpm() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 1.0, 1.0, 100.0),
            cand("c2", 10, 1.0, 1.0, 10.0),
        ]);
        assert!(scores["c1"] > scores["c2"], "RPM 更高的渠道应得分更高");
    }

    #[test]
    fn degenerate_dimensions_get_equal_scores() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        // 两渠道 rpm 相同 → 退化 → 等分
        let scores = s.score(&[
            cand("c1", 10, 1.0, 1.0, 10.0),
            cand("c2", 10, 1.0, 1.0, 10.0),
        ]);
        assert!((scores["c1"] - scores["c2"]).abs() < 1e-9);
    }

    #[test]
    fn cooldown_channel_penalized() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 1.0, 1.0, 100.0),
            cand("c2", 10, 1.0, 1.0, 0.1), // Cooldown
        ]);
        assert!(scores["c1"] > scores["c2"], "Cooldown 渠道应被惩罚");
    }

    #[test]
    fn rank_orders_desc() {
        let s = CombinedScheduler::default();
        let ranked = s.rank(&[
            cand("bad", 10, 0.2, 0.002, 0.1),
            cand("good", 10, 0.9, 0.07, 100.0),
        ]);
        assert_eq!(ranked[0], "good");
        assert_eq!(ranked[1], "bad");
    }

    #[test]
    fn empty_candidates_returns_empty() {
        let s = CombinedScheduler::default();
        assert!(s.score(&[]).is_empty());
        assert!(s.rank(&[]).is_empty());
    }

    #[test]
    fn admin_weight_multiplies_score() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 1.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 0.9, 1.0, 10.0),
            cand("c2", 10, 0.9, 1.0, 10.0),
        ]);
        assert!((scores["c1"] - scores["c2"]).abs() < 1e-9);

        // c2 权重翻倍 → 分数翻倍
        let scores = s.score(&[
            cand("c1", 10, 0.9, 1.0, 10.0),
            cand("c2", 20, 0.9, 1.0, 10.0),
        ]);
        assert!((scores["c2"] / scores["c1"] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn policy_config_validation() {
        assert!(SchedulerPolicyConfig::default().validate());
        assert!(!SchedulerPolicyConfig {
            health_weight: -1.0,
            cost_weight: 0.4,
            rpm_weight: 0.2,
        }
        .validate());
        assert!(!SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
        }
        .validate());
    }
}

#[cfg(test)]
mod scheduler_trait_tests {
    use super::*;
    use crate::channel::ChannelType;

    /// 测试用渠道构造。
    fn make_channel(id: &str, weight: u32) -> (Channel, u32) {
        (
            Channel {
                id: id.to_string(),
                name: format!("ch-{id}"),
                channel_type: ChannelType::OpenaiCompatible,
                base_url: format!("https://ch{id}.example.com"),
                api_key: String::new(),
                priority: 0,
                weight,
                status: "enabled".to_string(),
                models: vec![],
                account_id: String::new(),
                last_error: None,
                last_used_at: None,
                discovered_models: vec![],
                created_at: 0,
                updated_at: 0,
            },
            weight,
        )
    }

    fn make_ctx(factors: Vec<(&str, f64, f64, f64)>) -> SchedulingContext {
        SchedulingContext {
            factors: factors
                .into_iter()
                .map(|(id, h, c, r)| {
                    (
                        id.to_string(),
                        CandidateFactors {
                            health: h,
                            cost: c,
                            rpm: r,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn passthrough_scores_by_admin_weight() {
        let c = vec![make_channel("1", 5), make_channel("2", 10)];
        let ctx = SchedulingContext::default();
        let s = PassthroughScheduler;
        let scores = s.score_candidates(&c, &ctx).unwrap();
        assert_eq!(scores["1"], 5.0);
        assert_eq!(scores["2"], 10.0);
    }

    #[test]
    fn combined_trait_prefers_healthier() {
        let c1 = make_channel("1", 10);
        let c2 = make_channel("2", 10);
        let ctx = make_ctx(vec![("1", 0.9, 1.0, 10.0), ("2", 0.5, 1.0, 10.0)]);
        let scheduler = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 1.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
        });
        let scores = scheduler.score_candidates(&[c1, c2], &ctx).unwrap();
        assert!(scores["1"] > scores["2"], "更健康的渠道评分应更高");
    }

    #[test]
    fn combined_trait_prefers_cheaper() {
        let c1 = make_channel("1", 10);
        let c2 = make_channel("2", 10);
        let ctx = make_ctx(vec![("1", 1.0, 0.07, 10.0), ("2", 1.0, 0.002, 10.0)]);
        let scheduler = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 1.0,
            rpm_weight: 0.0,
        });
        let scores = scheduler.score_candidates(&[c1, c2], &ctx).unwrap();
        assert!(scores["1"] > scores["2"], "更便宜的渠道评分应更高");
    }

    #[test]
    fn combined_trait_prefers_higher_rpm() {
        let c1 = make_channel("1", 10);
        let c2 = make_channel("2", 10);
        let ctx = make_ctx(vec![("1", 1.0, 1.0, 100.0), ("2", 1.0, 1.0, 10.0)]);
        let scheduler = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        let scores = scheduler.score_candidates(&[c1, c2], &ctx).unwrap();
        assert!(scores["1"] > scores["2"], "RPM 更高的渠道评分应更高");
    }

    #[test]
    fn rank_candidates_single_short_circuits() {
        let c = vec![make_channel("1", 10)];
        let ctx = SchedulingContext::default();
        let result = rank_candidates(c, &ctx, &PassthroughScheduler);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0.id, "1");
    }

    #[test]
    fn rank_passthrough_orders_by_weight_desc() {
        let c = vec![make_channel("1", 5), make_channel("2", 10)];
        let result = rank_passthrough(c);
        assert_eq!(result[0].0.id, "2");
        assert_eq!(result[1].0.id, "1");
    }

    #[test]
    fn rank_candidates_error_falls_back_to_passthrough() {
        struct FailingScheduler;
        impl ChannelScheduler for FailingScheduler {
            fn name(&self) -> &'static str {
                "failing"
            }
            fn score_candidates(
                &self,
                _candidates: &[(Channel, u32)],
                _ctx: &SchedulingContext,
            ) -> Result<HashMap<String, f64>, ScheduleError> {
                Err(ScheduleError::Internal("intentional".into()))
            }
        }
        let c = vec![make_channel("1", 5), make_channel("2", 10)];
        let ctx = SchedulingContext::default();
        let result = rank_candidates(c, &ctx, &FailingScheduler);
        assert_eq!(result[0].0.id, "2");
    }

    #[test]
    fn rank_candidates_panic_falls_back_to_passthrough() {
        struct PanickingScheduler;
        impl ChannelScheduler for PanickingScheduler {
            fn name(&self) -> &'static str {
                "panicking"
            }
            fn score_candidates(
                &self,
                _candidates: &[(Channel, u32)],
                _ctx: &SchedulingContext,
            ) -> Result<HashMap<String, f64>, ScheduleError> {
                panic!("intentional");
            }
        }
        let c = vec![
            make_channel("1", 3),
            make_channel("2", 7),
            make_channel("3", 1),
        ];
        let ctx = SchedulingContext::default();
        let result = rank_candidates(c, &ctx, &PanickingScheduler);
        assert_eq!(result[0].0.id, "2");
        assert_eq!(result[1].0.id, "1");
        assert_eq!(result[2].0.id, "3");
    }

    #[test]
    fn cold_start_equal_scores() {
        let c1 = make_channel("1", 10);
        let c2 = make_channel("2", 10);
        let ctx = SchedulingContext::default();
        let scheduler = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        let scores = scheduler.score_candidates(&[c1, c2], &ctx).unwrap();
        assert!(
            (scores["1"] - scores["2"]).abs() < 0.01,
            "冷启动渠道应得等分"
        );
    }

    #[test]
    fn cooldown_channel_penalized_via_trait() {
        let c1 = make_channel("1", 10);
        let c2 = make_channel("2", 10);
        let ctx = make_ctx(vec![("1", 1.0, 1.0, 100.0), ("2", 1.0, 1.0, 0.1)]);
        let scheduler = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 1.0,
        });
        let scores = scheduler.score_candidates(&[c1, c2], &ctx).unwrap();
        assert!(scores["1"] > scores["2"], "Cooldown 渠道应被惩罚");
    }

    #[test]
    fn scheduling_request_affinity_key_prefers_session() {
        let req = SchedulingRequest {
            user_id: Some("u1".to_string()),
            session_id: Some("s1".to_string()),
        };
        assert_eq!(req.affinity_key(), Some("s1"));
        let req2 = SchedulingRequest {
            user_id: Some("u1".to_string()),
            session_id: None,
        };
        assert_eq!(req2.affinity_key(), Some("u1"));
    }

    #[test]
    fn aimd_state_to_rpm_factor_mapping() {
        use crate::ratelimit::aimd_limiter::RateLimitState;
        assert_eq!(
            aimd_state_to_rpm_factor(&RateLimitState::Stable, 100),
            100.0
        );
        assert_eq!(
            aimd_state_to_rpm_factor(&RateLimitState::Learning, 50),
            RPM_FACTOR_LEARNING
        );
        assert_eq!(
            aimd_state_to_rpm_factor(&RateLimitState::Cooldown, 50),
            RPM_FACTOR_COOLDOWN
        );
    }

    #[test]
    fn load_scheduler_config_missing_env_returns_empty() {
        std::env::remove_var("SCHEDULER_POLICIES");
        let policies = load_scheduler_config();
        assert!(policies.is_empty());
    }

    #[test]
    fn load_scheduler_config_parses_combined() {
        std::env::set_var(
            "SCHEDULER_POLICIES",
            r#"{"vip":{"type":"combined","health_weight":0.5,"cost_weight":0.3,"rpm_weight":0.2}}"#,
        );
        let policies = load_scheduler_config();
        assert!(matches!(
            policies.get("vip"),
            Some(SchedulerKind::Combined { .. })
        ));
        std::env::remove_var("SCHEDULER_POLICIES");
    }

    #[test]
    fn load_scheduler_config_invalid_json_returns_empty() {
        std::env::set_var("SCHEDULER_POLICIES", "not json");
        let policies = load_scheduler_config();
        assert!(policies.is_empty());
        std::env::remove_var("SCHEDULER_POLICIES");
    }

    #[test]
    fn build_context_with_pricing_and_health() {
        use crate::storage::FileStore;
        use tempfile::TempDir;

        let store =
            std::sync::Arc::new(FileStore::new(TempDir::new().unwrap().path().to_path_buf()));
        let pricing = PricingStore::new(store);
        pricing
            .upsert_price(crate::pricing::ModelPrice::new("gpt-4", 0.03, 0.06))
            .unwrap();

        let health = super::super::health_manager::ChannelStateTracker::new();
        health.record_error(
            "1",
            Some("gpt-4"),
            &super::super::circuit_breaker::FailureType::ServerError,
            "boom",
        );

        let candidates = vec![make_channel("1", 10), make_channel("2", 10)];
        let ctx = build_context("gpt-4", &candidates, &health, &pricing, None);

        let h1 = ctx.factors["1"].health;
        let h2 = ctx.factors["2"].health;
        assert!(h1 < 1.0, "有错误记录的渠道 health 应 < 1");
        assert_eq!(h2, 1.0, "未知渠道 health 应 = 1");
        assert!((ctx.factors["1"].cost - ctx.factors["2"].cost).abs() < 1e-9);
        assert_eq!(ctx.factors["1"].rpm, RPM_FACTOR_LEARNING);
    }

    #[test]
    fn build_context_with_rpm_overrides() {
        use crate::storage::FileStore;
        use tempfile::TempDir;

        let store =
            std::sync::Arc::new(FileStore::new(TempDir::new().unwrap().path().to_path_buf()));
        let pricing = PricingStore::new(store);
        let health = super::super::health_manager::ChannelStateTracker::new();

        let candidates = vec![make_channel("1", 10), make_channel("2", 10)];
        let mut overrides = HashMap::new();
        overrides.insert("1".to_string(), 50.0);
        let ctx = build_context("gpt-4", &candidates, &health, &pricing, Some(&overrides));
        assert_eq!(ctx.factors["1"].rpm, 50.0);
        assert_eq!(ctx.factors["2"].rpm, RPM_FACTOR_LEARNING); // 无 override
    }
}
