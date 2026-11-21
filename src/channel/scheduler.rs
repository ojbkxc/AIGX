//! CombinedScheduler — 多因子几何均值打分的组合调度器。
//!
//! 参照 burncloud `crates/router/src/scheduler/combined.rs`：
//!
//! Score = admin_weight × (health_norm^w_h × cost_norm^w_c × rpm_norm^w_r × latency_norm^w_l)
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
//! - latency：`ChannelHealthSummary::overall_avg_latency_ms` 的 EMA（越短越
//!   高）；无实测延迟的渠道取候选集内的**最大延迟**（保守，不奖励未知）
//!
//! ── latency / load 因子的对齐口径（2026-11-21）──────────────
//! 上游 Agent2API 3.5.0-beta.3 **没有**多因子调度器（其路由是账号池 +
//! routing profile），因此 ranked 是 AIGX 的扩展，没有逐字对齐对象。为避免
//! 改变既有选路，latency 权重默认 **0.0（关闭）**：关闭时该维度权重被清零，
//! health×cost×rpm 的归一化权重与旧实现逐位一致（见单测
//! `latency_factor_off_is_behavior_equal`）。
//!
//! **load 因子不单独引入**：AIGX 唯一可得的「负载/容量」信号就是 AIMD 的
//! `current_limit`，它已经是上面的 rpm 因子。再加一个 load 维度会对同一信号
//! 二次计权，反而扭曲排序；故 load 由 rpm 覆盖，不造假第二个来源。

#![allow(dead_code)] // 见 main.rs 债务清单：rank_candidates/build_context 已接线；load_scheduler_config/PassthroughScheduler 为预留/测试符号
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
    /// 延迟（毫秒，越小越好）；`0.0` = 未知。仅在 `latency_weight > 0` 时参与
    /// 打分；未知值在打分时取候选集最大延迟（保守）。
    pub latency_ms: f64,
}

/// Learning 期的 RPM 因子（容量未知，中性）。
pub const RPM_FACTOR_LEARNING: f64 = 1.0;
/// Cooldown 期的 RPM 因子（严重惩罚）。
pub const RPM_FACTOR_COOLDOWN: f64 = 0.1;

/// 组合调度权重配置。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SchedulerPolicyConfig {
    #[serde(default = "default_health_weight")]
    pub health_weight: f64,
    #[serde(default = "default_cost_weight")]
    pub cost_weight: f64,
    #[serde(default = "default_rpm_weight")]
    pub rpm_weight: f64,
    /// 延迟权重。默认 **0.0 = 关闭**（不改变既有 health×cost×rpm 排序结果）。
    /// 仅当管理员显式设 >0 时才把实测延迟纳入打分。
    #[serde(default = "default_latency_weight")]
    pub latency_weight: f64,
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
fn default_latency_weight() -> f64 {
    0.0
}

impl Default for SchedulerPolicyConfig {
    fn default() -> Self {
        Self {
            health_weight: default_health_weight(),
            cost_weight: default_cost_weight(),
            rpm_weight: default_rpm_weight(),
            latency_weight: default_latency_weight(),
        }
    }
}

impl SchedulerPolicyConfig {
    /// 权重合法：非负、有限、至少一个为正。
    pub fn validate(&self) -> bool {
        let weights = [
            self.health_weight,
            self.cost_weight,
            self.rpm_weight,
            self.latency_weight,
        ];
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
        // 延迟先做一次预扫：只有「有限且 >0」的实测值才算数；未知渠道在打分
        // 时取候选集最大延迟（保守 —— 不因缺数据而奖励）。全未知 → 维度退化。
        let (mut lat_max_known, mut lat_any_known) = (f64::NEG_INFINITY, false);
        for (_, _, f) in candidates {
            if f.latency_ms.is_finite() && f.latency_ms > 0.0 {
                lat_any_known = true;
                lat_max_known = lat_max_known.max(f.latency_ms);
            }
        }
        let mut raw: Vec<CandidateFactors> = Vec::with_capacity(n);
        let (mut h_min, mut h_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut c_min, mut c_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut r_min, mut r_max) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut l_min, mut l_max) = (f64::INFINITY, f64::NEG_INFINITY);

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

            let latency = if f.latency_ms.is_finite() && f.latency_ms > 0.0 {
                f.latency_ms
            } else if lat_any_known {
                lat_max_known
            } else {
                1.0
            };
            l_min = l_min.min(latency);
            l_max = l_max.max(latency);

            raw.push(CandidateFactors {
                health,
                cost,
                rpm,
                latency_ms: latency,
            });
        }

        let h_degen = (h_max - h_min).abs() < EPS;
        let c_degen = (c_max - c_min).abs() < EPS;
        let r_degen = (r_max - r_min).abs() < EPS;
        let l_degen = (l_max - l_min).abs() < EPS;

        let (w_h, w_c, w_r, w_l) = self.effective_weights(h_degen, c_degen, r_degen, l_degen);

        let h_range = if h_degen { 0.0 } else { h_max - h_min };
        let c_range = if c_degen { 0.0 } else { c_max - c_min };
        let r_range = if r_degen { 0.0 } else { r_max - r_min };
        let l_range = if l_degen { 0.0 } else { l_max - l_min };

        let mut scores = HashMap::with_capacity(n);
        for ((ch_id, admin_w, _), f) in candidates.iter().zip(raw.iter()) {
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
            // 延迟越低越好：归一化时反向（值域仍 [0.5, 1.0]）。
            let l = if l_degen {
                0.75
            } else {
                (0.5 + 0.5 * (l_max - f.latency_ms) / l_range).clamp(0.5, 1.0)
            };

            let quality = h.powf(w_h) * c.powf(w_c) * r.powf(w_r) * l.powf(w_l);
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
    ///
    /// 全维度退化（`total <= 0`）时回退到**旧的** health/cost/rpm 等权
    /// （1/3, 1/3, 1/3, 0）—— 与引入延迟维度前的逐位行为一致；延迟维度在
    /// 默认权重 0.0 下永远不参与归一（`w_l` 恒为 0）。
    fn effective_weights(
        &self,
        h_degen: bool,
        c_degen: bool,
        r_degen: bool,
        l_degen: bool,
    ) -> (f64, f64, f64, f64) {
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
        let mut w_l = if l_degen {
            0.0
        } else {
            self.config.latency_weight
        };

        let total = w_h + w_c + w_r + w_l;
        if total <= 0.0 {
            return (1.0 / 3.0, 1.0 / 3.0, 1.0 / 3.0, 0.0);
        }

        w_h /= total;
        w_c /= total;
        w_r /= total;
        w_l /= total;

        (w_h, w_c, w_r, w_l)
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
            latency_ms: 0.0,
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
                        latency_weight: default_latency_weight(),
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
/// - latency：健康摘要的 `overall_avg_latency_ms`（成功请求延迟 EMA，毫秒）；
///   无健康记录时回落渠道最近一次连通性测试的 `response_time`；都没有 → 0.0
///   （未知，打分时取候选集最大延迟）。仅在 `latency_weight > 0` 时影响排序。
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
        let (health, latency_ms) = if let Some(summary) = health_tracker.get_health(&ch.id) {
            let h = 1.0 - summary.overall_error_rate;
            let h = if !summary.auth_ok { h * 0.1 } else { h };
            let h = if summary.balance_status == super::health_manager::BalanceStatus::Exhausted {
                h * 0.1
            } else {
                h
            };
            // 健康摘要里的延迟是成功请求的 EMA；没有观测时回落连通性测试值。
            let latency = if summary.overall_avg_latency_ms > 0.0 {
                summary.overall_avg_latency_ms
            } else {
                ch.response_time.map(|ms| ms as f64).unwrap_or(0.0)
            };
            (h.clamp(0.0, 1.0), latency)
        } else {
            // 无健康记录：health 视为最好；延迟用连通性测试快照（可能也没有）。
            (1.0, ch.response_time.map(|ms| ms as f64).unwrap_or(0.0))
        };

        let cost = match price_usd {
            Some(p) if p > 0.0 => 1.0 / p,
            _ => 1.0,
        };

        let rpm = rpm_overrides
            .and_then(|m| m.get(&ch.id).copied())
            .unwrap_or(RPM_FACTOR_LEARNING);

        factors.insert(
            ch.id.clone(),
            CandidateFactors {
                health,
                cost,
                rpm,
                latency_ms,
            },
        );
    }

    SchedulingContext { factors }
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
                latency_ms: 0.0,
            },
        )
    }

    #[test]
    fn prefers_healthier_channel() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 1.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
        });
        let scores = s.score(&[
            cand("c1", 10, 1.0, 1.0, 100.0),
            cand("c2", 10, 1.0, 1.0, 0.1), // Cooldown
        ]);
        assert!(scores["c1"] > scores["c2"], "Cooldown 渠道应被惩罚");
    }

    /// 构造带延迟的候选（延迟单位毫秒，0.0 = 未知）。
    fn cand_lat(
        id: &str,
        w: u32,
        h: f64,
        c: f64,
        r: f64,
        latency_ms: f64,
    ) -> (String, u32, CandidateFactors) {
        (
            id.to_string(),
            w,
            CandidateFactors {
                health: h,
                cost: c,
                rpm: r,
                latency_ms,
            },
        )
    }

    /// **行为等值闸（硬要求）**：`latency_weight` 默认 0.0 时，加入（甚至差异
    /// 巨大的）延迟数据不得改变任何候选的分数与排名 —— 与引入延迟前逐位一致。
    #[test]
    fn latency_factor_off_is_behavior_equal() {
        let s = CombinedScheduler::default();
        assert_eq!(
            s.config.latency_weight, 0.0,
            "默认必须关闭，否则会改变既有选路"
        );
        // 同一健康/成本/RPM 集，一份不带延迟、一份延迟差异极大
        let base = [
            cand("a", 10, 0.9, 1.0, 10.0),
            cand("b", 10, 0.5, 1.0, 10.0),
            cand("c", 10, 0.7, 1.0, 10.0),
        ];
        let with_latency = [
            cand_lat("a", 10, 0.9, 1.0, 10.0, 5000.0),
            cand_lat("b", 10, 0.5, 1.0, 10.0, 10.0),
            cand_lat("c", 10, 0.7, 1.0, 10.0, 300.0),
        ];
        let s_base = s.score(&base);
        let s_lat = s.score(&with_latency);
        for id in ["a", "b", "c"] {
            assert!(
                (s_base[id] - s_lat[id]).abs() < 1e-12,
                "{id} 分数必须与未加延迟时一致：{} vs {}",
                s_base[id],
                s_lat[id]
            );
        }
        assert_eq!(s.rank(&base), s.rank(&with_latency), "排名也必须一致");
    }

    /// 正向对照：显式开启延迟因子后，延迟低的候选应胜出（健康/成本/RPM 全退化）。
    #[test]
    fn latency_factor_on_prefers_lower_latency() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
            latency_weight: 1.0,
        });
        let scores = s.score(&[
            cand_lat("slow", 10, 1.0, 1.0, 10.0, 2000.0),
            cand_lat("fast", 10, 1.0, 1.0, 10.0, 50.0),
        ]);
        assert!(scores["fast"] > scores["slow"], "低延迟应得高分");
        assert_eq!(
            s.rank(&[
                cand_lat("slow", 10, 1.0, 1.0, 10.0, 2000.0),
                cand_lat("fast", 10, 1.0, 1.0, 10.0, 50.0),
            ])[0],
            "fast"
        );
    }

    /// 未知延迟不得被奖励：未知值按候选集**最大延迟**计，短延迟仍胜出。
    #[test]
    fn unknown_latency_not_rewarded() {
        let s = CombinedScheduler::new(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
            latency_weight: 1.0,
        });
        // 三候选：快(40ms)、慢(200ms)、未知。未知按最大延迟 200 计 → 与慢者同分，
        // 都低于快者（若未知被当成 0ms，它会与快者并列甚至胜出）。
        let scores = s.score(&[
            cand_lat("known_fast", 10, 1.0, 1.0, 10.0, 40.0),
            cand_lat("known_slow", 10, 1.0, 1.0, 10.0, 200.0),
            cand_lat("unknown", 10, 1.0, 1.0, 10.0, 0.0),
        ]);
        assert!(
            scores["known_fast"] > scores["unknown"],
            "未知延迟按最大延迟计，不得优于实测低延迟"
        );
        assert!(
            (scores["unknown"] - scores["known_slow"]).abs() < 1e-12,
            "未知延迟等价于候选集最大延迟"
        );
        // 全未知 → 维度退化 → 等分
        let all_unknown = s.score(&[
            cand_lat("x", 10, 1.0, 1.0, 10.0, 0.0),
            cand_lat("y", 10, 1.0, 1.0, 10.0, 0.0),
        ]);
        assert!((all_unknown["x"] - all_unknown["y"]).abs() < 1e-12);
    }

    #[test]
    fn latency_weight_validation() {
        assert!(SchedulerPolicyConfig::default().validate());
        assert!(!SchedulerPolicyConfig {
            health_weight: 0.4,
            cost_weight: 0.4,
            rpm_weight: 0.2,
            latency_weight: -1.0,
        }
        .validate());
        assert!(SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
            latency_weight: 1.0,
        }
        .validate());
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
        }
        .validate());
        assert!(!SchedulerPolicyConfig {
            health_weight: 0.0,
            cost_weight: 0.0,
            rpm_weight: 0.0,
            latency_weight: 0.0,
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
                backup_base_urls: Vec::new(),
                api_key: String::new(),
                priority: 0,
                weight,
                status: "enabled".to_string(),
                models: vec![],
                account_id: String::new(),
                plugin_provider: String::new(),
                model_mapping: std::collections::HashMap::new(),
                cost_pricing: std::collections::HashMap::new(),
                last_error: None,
                last_used_at: None,
                discovered_models: vec![],
                response_time: None,
                test_time: None,
                balance: None,
                credit: None,
                quirks: Default::default(),
                balance_scheme: String::new(),
                balance_unit: String::new(),
                balance_query_interval_secs: None,
                balance_last_query_at: None,
                balance_last_error: None,
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
                            latency_ms: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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
            latency_weight: 0.0,
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

    /// 环境变量测试互斥锁：并行测试下 set_var/remove_var 竞态
    /// （missing_env 测试偶发读到其他测试残留的 SCHEDULER_POLICIES）
    static ENV_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn load_scheduler_config_missing_env_returns_empty() {
        let _guard = ENV_TEST_LOCK.lock();
        std::env::remove_var("SCHEDULER_POLICIES");
        let policies = load_scheduler_config();
        assert!(policies.is_empty());
    }

    #[test]
    fn load_scheduler_config_parses_combined() {
        let _guard = ENV_TEST_LOCK.lock();
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
        let _guard = ENV_TEST_LOCK.lock();
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
