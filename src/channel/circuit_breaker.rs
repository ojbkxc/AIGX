//! 渠道断路器 — per-channel 故障自动熔断。
//!
//! 参照 burncloud `crates/router/src/circuit_breaker.rs` 的设计：
//! - 每个渠道维护一份 `UpstreamState`（失败计数 + 最近失败时间 + 失败类型 + 限流到期时间）
//! - 失败次数超过阈值（默认 5）则打开断路器
//! - 冷却期（默认 30 秒）过后进入 HalfOpen，放一个试探请求
//! - AuthFailed / PaymentRequired 视为永久故障，强制长期冷却（30 分钟）
//! - RateLimited 记入限流到期窗口，期间拒绝请求
//!
//! 与 AIGX 既有 `ChannelStore::mark_cooldown` 互补：mark_cooldown 是粗粒度
//! 冷却（任意错误 60s），断路器是细粒度按失败类型分级处理。集成时二者并存，
//! 断路器在 `select_for_model` 过滤阶段生效，mark_cooldown 仍由调用方按需触发。

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// 限流作用域，参照 burncloud `RateLimitScope`。
///
/// 用于区分账号级限流（影响该渠道所有模型）与模型级限流（仅影响特定模型）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RateLimitScope {
    /// 账号级限流（影响渠道下所有模型）
    Account,
    /// 模型级限流（仅影响特定模型）
    Model,
    /// 作用域未知（按账号级保守处理）
    #[default]
    Unknown,
}

/// 渠道失败类型，参照 burncloud `FailureType`。
///
/// 不同失败类型在断路器中触发不同行为：
/// - `AuthFailed` / `PaymentRequired`：视为永久故障，强制 30 分钟冷却
/// - `RateLimited`：记录限流到期时间，期间拒绝请求
/// - 其他类型：累加失败计数，达到阈值后打开断路器
#[derive(Debug, Clone)]
pub enum FailureType {
    /// 认证失败（401）
    AuthFailed,
    /// 余额不足 / 配额耗尽（402）
    PaymentRequired,
    /// 被上游限流（429）
    RateLimited {
        /// 限流作用域
        scope: RateLimitScope,
        /// 距限流重置的秒数（无则用默认 60s）
        retry_after: Option<u64>,
    },
    /// 上游不提供该模型（404）
    ModelNotFound,
    /// 上游服务端错误（5xx）
    ServerError,
    /// 请求超时
    Timeout,
    /// 连接失败（DNS / TCP / TLS）
    ConnectionError,
    /// 空响应（HTTP 200 但零 token）
    EmptyResponse,
}

/// 单个渠道的断路器状态。
#[derive(Debug)]
struct UpstreamState {
    /// 连续失败计数（成功时清零）
    failure_count: AtomicU32,
    /// 最近一次失败时间（用于冷却期判断）
    last_failure_time: Option<Instant>,
    /// 最近一次失败类型
    failure_type: Option<FailureType>,
    /// 限流到期时间（被 429 时设置）
    rate_limit_until: Option<Instant>,
    /// T5：HalfOpen 试探租约到期时间（单飞保护）。
    ///
    /// `None` = 无在飞试探；`Some(t)` = 已放行一个试探，t 过期前其余请求
    /// 拒绝（防并发多请求同时试探冲击上游）。t 过期后允许重新夺取——
    /// 兜底"租约持有者未被实际调度"场景（候选过滤阶段抢到租约但
    /// failover 循环未尝试该渠道）。
    probe_lease_until: Option<Instant>,
}

impl Default for UpstreamState {
    fn default() -> Self {
        Self {
            failure_count: AtomicU32::new(0),
            last_failure_time: None,
            failure_type: None,
            rate_limit_until: None,
            probe_lease_until: None,
        }
    }
}

/// 无 `retry_after` 头时的默认限流时长（秒）。
pub(crate) const DEFAULT_RATE_LIMIT_RETRY_SECS: u64 = 60;
/// `retry_after` 上限（秒）。防上游返回异常大值导致 `Instant` 加法溢出 panic。
pub(crate) const MAX_RATE_LIMIT_RETRY_SECS: u64 = 3600;
/// AuthFailed / PaymentRequired 的强制冷却时长（30 分钟）。
const PERMANENT_FAILURE_COOLDOWN_SECS: u64 = 1800;
/// T5：HalfOpen 试探租约时长（60s）。
///
/// 兜底租约持有者不汇报结果的场景（候选过滤抢到租约但循环未实际尝试该
/// 渠道、或试探结果丢失）——60s 后自动解锁，渠道不会永久卡死。试探请求
/// 本身的生命周期：流式有 30s 首事件守卫（`bridge::first_event_or_timeout`），
/// 渠道测试/prober 有 15s/30s 超时，均远短于 60s，正常试探不会双飞。
/// （rust-tunnel 的 STALE_PROBE_GRACE 取 300s 对应其无 deadline 的读超时，
/// AIGX 试探路径均有更短 deadline，取 60s 即可。）
const PROBE_LEASE_TTL: Duration = Duration::from_secs(60);

impl FailureType {
    /// 机器可读的失败类型名（诊断端点快照用，与 `{:?}` Debug 输出解耦）。
    pub fn as_str(&self) -> &'static str {
        match self {
            FailureType::AuthFailed => "auth_failed",
            FailureType::PaymentRequired => "payment_required",
            FailureType::RateLimited { .. } => "rate_limited",
            FailureType::ModelNotFound => "model_not_found",
            FailureType::ServerError => "server_error",
            FailureType::Timeout => "timeout",
            FailureType::ConnectionError => "connection_error",
            FailureType::EmptyResponse => "empty_response",
        }
    }
}

/// 单个渠道断路器只读快照（`GET /api/diagnostics/breakers` 响应条目）。
///
/// 只反映当前状态机事实，不携带也不改变任何控制语义——写操作（reset）
/// 属管理端点，不在只读诊断范围。
#[derive(Debug, Clone, Serialize)]
pub struct BreakerSnapshot {
    pub channel_id: String,
    /// `"closed"` / `"open"` / `"halfopen"`
    pub state: &'static str,
    /// 连续失败计数（成功时清零）
    pub failure_count: u32,
    /// 最近一次失败类型（机器可读名；无失败记录为 None）
    pub failure_type: Option<&'static str>,
    /// Open 状态剩余冷却秒数（Closed/HalfOpen 为 0）
    pub cooldown_remaining_secs: u64,
    /// 限流窗口剩余秒数（未被限流为 0）
    pub rate_limit_remaining_secs: u64,
    /// HalfOpen 试探租约是否在飞（单飞保护期间为 true）
    pub probe_in_flight: bool,
}

/// 渠道断路器 — per-channel 状态机（Closed / Open / HalfOpen）。
///
/// 线程安全：内部用 `DashMap` 存储 per-channel 状态，失败计数用 `AtomicU32`。
/// 热路径 `allow_request` 只读，不创建条目，健康渠道零开销。
pub struct CircuitBreaker {
    /// channel_id → 状态
    states: DashMap<String, UpstreamState>,
    /// 打开断路器的失败次数阈值
    failure_threshold: u32,
    /// 打开后的冷却时长
    cooldown_duration: Duration,
}

impl CircuitBreaker {
    /// 构造断路器。
    ///
    /// - `failure_threshold`：连续失败多少次打开断路器（burncloud 默认 5）
    /// - `cooldown_seconds`：打开后冷却秒数（burncloud 默认 30）
    pub fn new(failure_threshold: u32, cooldown_seconds: u64) -> Self {
        Self {
            states: DashMap::new(),
            failure_threshold,
            cooldown_duration: Duration::from_secs(cooldown_seconds),
        }
    }

    /// 默认配置：阈值 5，冷却 30s（与 burncloud 对齐）。
    pub fn with_defaults() -> Self {
        Self::new(5, 30)
    }

    /// 判断指定渠道当前是否允许请求。
    ///
    /// - 渠道无状态（从未失败）→ 允许
    /// - 限流未到期 → 拒绝
    /// - 失败计数 < 阈值 → 允许（Closed）
    /// - 失败计数 ≥ 阈值且冷却未过 → 拒绝（Open）
    /// - 失败计数 ≥ 阈值且冷却已过 → HalfOpen 试探（T5 单飞保护：
    ///   仅第一个抢到租约的请求放行，其余拒绝；租约 60s 过期后可重新夺取）
    pub fn allow_request(&self, channel_id: &str) -> bool {
        let entry = match self.states.get(channel_id) {
            Some(e) => e,
            None => return true, // 无状态 = 从未失败 = 放行
        };
        if Self::is_half_open(&entry, self.failure_threshold, self.cooldown_duration) {
            // HalfOpen：先释放只读锁再抢租约（DashMap 同 key 读写锁嵌套会死锁）
            drop(entry);
            return self.acquire_probe_lease(channel_id);
        }
        Self::state_allows(&entry, self.failure_threshold, self.cooldown_duration)
    }

    /// 判断渠道是否处于 HalfOpen（失败计数达阈值且冷却已过，待试探）。
    ///
    /// 限流窗口内视为 open（非 HalfOpen）；无失败时间（理论不可达，防御）非 HalfOpen。
    fn is_half_open(
        entry: &UpstreamState,
        failure_threshold: u32,
        cooldown_duration: Duration,
    ) -> bool {
        if let Some(rate_limit_until) = entry.rate_limit_until {
            if rate_limit_until > Instant::now() {
                return false;
            }
        }
        if entry.failure_count.load(Ordering::Relaxed) < failure_threshold {
            return false; // Closed
        }
        matches!(entry.last_failure_time, Some(last) if last.elapsed() >= cooldown_duration)
    }

    /// T5：抢 HalfOpen 试探租约（单飞保护）。
    ///
    /// 第一个到达的请求抢到租约放行试探，租约期内其余请求拒绝；租约
    /// 过期（持有者未被实际调度/结果丢失）后允许重新夺取，不永久卡死。
    fn acquire_probe_lease(&self, channel_id: &str) -> bool {
        let Some(mut entry) = self.states.get_mut(channel_id) else {
            return true; // 竞态下条目消失（states 无删除路径，防御放行）
        };
        let now = Instant::now();
        match entry.probe_lease_until {
            Some(t) if t > now => false, // 已有在飞试探
            _ => {
                entry.probe_lease_until = Some(now + PROBE_LEASE_TTL);
                true // 抢到试探权
            }
        }
    }

    /// 基于 entry 值判定是否放行（不查 map，避免在 iter 持锁期间嵌套 get 死锁）。
    fn state_allows(
        entry: &UpstreamState,
        failure_threshold: u32,
        cooldown_duration: Duration,
    ) -> bool {
        // 限流未到期 → 拒绝
        if let Some(rate_limit_until) = entry.rate_limit_until {
            if rate_limit_until > Instant::now() {
                return false;
            }
        }

        let current_failures = entry.failure_count.load(Ordering::Relaxed);
        if current_failures < failure_threshold {
            return true; // Closed
        }

        // Open：检查冷却是否已过
        if let Some(last_failure) = entry.last_failure_time {
            if last_failure.elapsed() >= cooldown_duration {
                return true; // HalfOpen 放一个试探
            }
        }

        false // Open
    }

    /// 记入一次成功 — 清零失败计数与限流状态。
    pub fn record_success(&self, channel_id: &str) {
        if let Some(mut entry) = self.states.get_mut(channel_id) {
            entry.failure_count.store(0, Ordering::Relaxed);
            entry.last_failure_time = None;
            entry.failure_type = None;
            entry.rate_limit_until = None;
            entry.probe_lease_until = None; // T5：试探结束，释放单飞租约
        }
    }

    /// 计入一次失败（按类型分级处理）。
    ///
    /// - `AuthFailed` / `PaymentRequired`：失败计数 ×10 + 30 分钟冷却
    /// - `RateLimited`：设置限流到期时间 + 失败计数 +1
    /// - 其他：失败计数 +1
    pub fn record_failure(&self, channel_id: &str, failure_type: FailureType) {
        let mut entry = self.states.entry(channel_id.to_string()).or_default();
        entry.failure_type = Some(failure_type.clone());
        entry.last_failure_time = Some(Instant::now());
        // T5：试探结束（无论成败）清租约——失败回 Open 后旧租约
        // 不能挡住下一轮 HalfOpen 试探
        entry.probe_lease_until = None;

        match &failure_type {
            FailureType::AuthFailed | FailureType::PaymentRequired => {
                // 永久性故障：放大计数 + 长冷却，避免反复重试坏密钥/空余额渠道
                entry
                    .failure_count
                    .store(self.failure_threshold.saturating_mul(10), Ordering::Relaxed);
                entry.last_failure_time = Some(Instant::now());
                entry.rate_limit_until =
                    Some(Instant::now() + Duration::from_secs(PERMANENT_FAILURE_COOLDOWN_SECS));
                tracing::warn!(
                    "CircuitBreaker: 渠道 {} 认证/余额故障 ({:?}) — 断路器打开 30 分钟",
                    channel_id,
                    failure_type
                );
            }
            FailureType::RateLimited { retry_after, .. } => {
                let duration = retry_after
                    .as_ref()
                    .map(|r| Duration::from_secs((*r).min(MAX_RATE_LIMIT_RETRY_SECS)))
                    .unwrap_or_else(|| Duration::from_secs(DEFAULT_RATE_LIMIT_RETRY_SECS));
                entry.rate_limit_until = Some(Instant::now() + duration);
                let new_count = entry.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if new_count >= self.failure_threshold {
                    tracing::warn!(
                        "CircuitBreaker: 渠道 {} 因限流打开断路器 (失败计数 {})",
                        channel_id,
                        new_count
                    );
                }
            }
            _ => {
                let new_count = entry.failure_count.fetch_add(1, Ordering::Relaxed) + 1;
                if new_count >= self.failure_threshold {
                    tracing::warn!(
                        "CircuitBreaker: 渠道 {} 打开断路器 (失败计数 {})",
                        channel_id,
                        new_count
                    );
                }
            }
        }
    }

    /// 紧急熔断：强制所有已知渠道进入 Open 状态。
    ///
    /// 返回被熔断的渠道 ID 列表。用于全局故障（如所有上游不可达）场景。
    pub fn trip_all(&self) -> Vec<String> {
        let mut tripped = Vec::new();
        for mut entry in self.states.iter_mut() {
            entry
                .failure_count
                .store(self.failure_threshold, Ordering::Relaxed);
            entry.last_failure_time = Some(Instant::now());
            entry.failure_type = Some(FailureType::ServerError);
            entry.rate_limit_until = None;
            entry.probe_lease_until = None; // T5：熔断重开，旧租约作废
            tripped.push(entry.key().clone());
        }
        tracing::warn!(
            "CircuitBreaker: 紧急全量熔断 — {} 个渠道强制 Open",
            tripped.len()
        );
        tripped
    }

    /// 返回全部渠道的断路器状态（机器可读枚举值，供巡检/前端/监控使用）。
    ///
    /// 返回值：`"open"` / `"halfopen"` / `"closed"`。
    /// 注意：只有出现失败记录的渠道才会出现在 map 中（从未失败的渠道无状态）。
    pub fn get_status_map(&self) -> std::collections::HashMap<String, String> {
        let mut map = std::collections::HashMap::new();
        for r in self.states.iter() {
            // 直接基于 entry 值计算，不嵌套 get（避免 DashMap 迭代中同 key 二次加锁死锁）
            map.insert(
                r.key().clone(),
                Self::state_of(r.value(), self.failure_threshold, self.cooldown_duration)
                    .to_string(),
            );
        }
        map
    }

    /// 全量只读快照 — 逐渠道返回状态机完整事实（管理面诊断端点用）。
    ///
    /// 只遍历 `states` 读取，不写任何字段、不创建条目（从未失败的渠道
    /// 无状态条目，不出现在结果中——调用方如需"未见=Closed"的完整清单，
    /// 自行与 ChannelStore 渠道表对齐）。三态计算复用 `state_of`，
    /// 剩余时长基于 `Instant` 差值推导，与 `get_status_human` 同口径。
    pub fn snapshot_all(&self) -> Vec<BreakerSnapshot> {
        let now = Instant::now();
        let mut out: Vec<BreakerSnapshot> = self
            .states
            .iter()
            .map(|r| {
                let entry = r.value();
                let state = Self::state_of(entry, self.failure_threshold, self.cooldown_duration);
                BreakerSnapshot {
                    channel_id: r.key().clone(),
                    state,
                    failure_count: entry.failure_count.load(Ordering::Relaxed),
                    failure_type: entry.failure_type.as_ref().map(|f| f.as_str()),
                    cooldown_remaining_secs: Self::open_cooldown_remaining(
                        entry,
                        state,
                        self.cooldown_duration,
                    ),
                    rate_limit_remaining_secs: Self::rate_limit_remaining(entry, now),
                    probe_in_flight: entry.probe_lease_until.is_some_and(|t| t > now),
                }
            })
            .collect();
        out.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        out
    }

    /// Open 状态剩余冷却秒数（其余状态为 0）。
    fn open_cooldown_remaining(entry: &UpstreamState, state: &str, cooldown: Duration) -> u64 {
        if state != "open" {
            return 0;
        }
        entry
            .last_failure_time
            .and_then(|last| cooldown.checked_sub(last.elapsed()))
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 限流窗口剩余秒数（无记录或已过期为 0）。
    fn rate_limit_remaining(entry: &UpstreamState, now: Instant) -> u64 {
        entry
            .rate_limit_until
            .and_then(|until| until.checked_duration_since(now))
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// 查询单个渠道的断路器状态（机器可读枚举值）。
    ///
    /// 返回 `"open"` / `"halfopen"` / `"closed"`。
    /// 无状态（从未失败）的渠道视为 `"closed"`。
    pub fn get_state(&self, channel_id: &str) -> &'static str {
        let entry = match self.states.get(channel_id) {
            Some(e) => e,
            None => return "closed",
        };
        Self::state_of(&entry, self.failure_threshold, self.cooldown_duration)
    }

    /// 基于 entry 值计算状态（不查 map）。
    fn state_of(
        entry: &UpstreamState,
        failure_threshold: u32,
        cooldown_duration: Duration,
    ) -> &'static str {
        // 限流未到期视为 open（拒绝请求）
        if let Some(rate_limit_until) = entry.rate_limit_until {
            if rate_limit_until > Instant::now() {
                return "open";
            }
        }
        let count = entry.failure_count.load(Ordering::Relaxed);
        if count < failure_threshold {
            return "closed";
        }
        if let Some(last_failure) = entry.last_failure_time {
            if last_failure.elapsed() < cooldown_duration {
                "open"
            } else {
                "halfopen"
            }
        } else {
            "open"
        }
    }

    /// 返回人类可读的状态描述（含剩余冷却秒数，供日志/调试）。
    pub fn get_status_human(&self, channel_id: &str) -> String {
        match self.get_state(channel_id) {
            "open" => {
                let entry = self.states.get(channel_id);
                let remaining = entry
                    .and_then(|e| e.last_failure_time)
                    .map(|last| {
                        self.cooldown_duration
                            .checked_sub(last.elapsed())
                            .map(|d| d.as_secs())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0);
                format!("Open (剩余 {}s)", remaining)
            }
            "halfopen" => "HalfOpen (试探中)".to_string(),
            _ => "Closed (健康)".to_string(),
        }
    }

    /// 手动重置指定渠道的断路器（管理面用）。
    pub fn reset(&self, channel_id: &str) {
        if let Some(mut entry) = self.states.get_mut(channel_id) {
            entry.failure_count.store(0, Ordering::Relaxed);
            entry.last_failure_time = None;
            entry.failure_type = None;
            entry.rate_limit_until = None;
            entry.probe_lease_until = None; // T5：手动重置，租约一并清除
        }
    }

    /// 当前正在被断路器阻断的渠道数量（监控用）。
    pub fn open_count(&self) -> usize {
        self.states
            .iter()
            .filter(|r| {
                !Self::state_allows(r.value(), self.failure_threshold, self.cooldown_duration)
            })
            .count()
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_channel_is_allowed() {
        let cb = CircuitBreaker::with_defaults();
        assert!(cb.allow_request("unknown"));
    }

    #[test]
    fn threshold_opens_circuit() {
        let cb = CircuitBreaker::new(3, 30);
        for _ in 0..3 {
            cb.record_failure("ch1", FailureType::ServerError);
        }
        assert!(!cb.allow_request("ch1"));
    }

    #[test]
    fn success_resets_circuit() {
        let cb = CircuitBreaker::new(3, 30);
        cb.record_failure("ch1", FailureType::ServerError);
        cb.record_failure("ch1", FailureType::ServerError);
        cb.record_success("ch1");
        assert!(cb.allow_request("ch1"));
    }

    #[test]
    fn auth_failed_trips_immediately() {
        let cb = CircuitBreaker::new(5, 30);
        cb.record_failure("ch1", FailureType::AuthFailed);
        assert!(!cb.allow_request("ch1"), "AuthFailed 应立即打开断路器");
    }

    #[test]
    fn rate_limited_blocks_until_expiry() {
        let cb = CircuitBreaker::new(5, 30);
        cb.record_failure(
            "ch1",
            FailureType::RateLimited {
                scope: RateLimitScope::Model,
                retry_after: Some(60),
            },
        );
        assert!(!cb.allow_request("ch1"), "限流期间应拒绝");
    }

    #[test]
    fn cooldown_allows_probe() {
        let cb = CircuitBreaker::new(1, 0); // 冷却 0s → 立即 HalfOpen
        cb.record_failure("ch1", FailureType::ServerError);
        // 冷却 0s，elapsed >= 0 总成立 → 放试探
        assert!(cb.allow_request("ch1"));
    }

    #[test]
    fn get_state_reports_machine_readable() {
        let cb = CircuitBreaker::new(3, 30);
        // 未失败渠道：closed
        assert_eq!(cb.get_state("ch1"), "closed");
        // 失败达到阈值：open
        for _ in 0..3 {
            cb.record_failure("ch1", FailureType::ServerError);
        }
        assert_eq!(cb.get_state("ch1"), "open");
        // 成功重置：closed
        cb.record_success("ch1");
        assert_eq!(cb.get_state("ch1"), "closed");
    }

    #[test]
    fn get_status_map_values_are_machine_readable() {
        let cb = CircuitBreaker::new(2, 30);
        cb.record_failure("ch1", FailureType::ServerError);
        cb.record_failure("ch1", FailureType::ServerError);
        let map = cb.get_status_map();
        // map 只含出现过失败的渠道
        assert_eq!(map.get("ch1").map(|s| s.as_str()), Some("open"));
        // 从未失败的渠道不出现在 map
        assert!(!map.contains_key("ch2"));
    }

    // ── T5：HalfOpen 试探单飞保护 ──────────────────────────────────────

    /// 单飞：第一个请求抢到试探租约，并发其余请求拒绝；试探成功后恢复。
    #[test]
    fn half_open_probe_single_flight() {
        let cb = CircuitBreaker::new(1, 0); // 阈值 1，冷却 0s → 立即 HalfOpen
        cb.record_failure("ch1", FailureType::ServerError);
        assert!(cb.allow_request("ch1"), "第一个请求应抢到试探权");
        assert!(!cb.allow_request("ch1"), "并发试探应被单飞拒绝");
        // 试探成功 → Closed，租约释放，不再单飞
        cb.record_success("ch1");
        assert!(cb.allow_request("ch1"), "成功后渠道恢复");
        assert!(cb.allow_request("ch1"), "Closed 状态不受租约影响");
    }

    /// 试探失败：回 Open 并释放租约，冷却后可再次试探。
    #[test]
    fn half_open_probe_failure_releases_lease() {
        let cb = CircuitBreaker::new(1, 0);
        cb.record_failure("ch1", FailureType::ServerError);
        assert!(cb.allow_request("ch1"));
        assert!(!cb.allow_request("ch1"));
        // 试探失败 → record_failure 清租约（last_failure_time 重置回 Open）
        cb.record_failure("ch1", FailureType::ServerError);
        // 冷却 0s → 立即又 HalfOpen，应能再次试探
        assert!(cb.allow_request("ch1"), "试探失败释放租约后应能再次试探");
    }

    /// 租约过期回收：持有者未汇报结果（未被实际调度）不会永久卡死渠道。
    #[test]
    fn half_open_probe_lease_expires_and_reclaimed() {
        let cb = CircuitBreaker::new(1, 0);
        cb.record_failure("ch1", FailureType::ServerError);
        assert!(cb.allow_request("ch1"), "抢到租约");
        assert!(!cb.allow_request("ch1"), "租约期内拒绝");
        // 直接回写租约时间戳为过去，模拟 60s 租约到期（避免测试真实等待）
        if let Some(mut entry) = cb.states.get_mut("ch1") {
            entry.probe_lease_until = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(cb.allow_request("ch1"), "租约过期应允许重新夺取试探权");
        assert!(!cb.allow_request("ch1"), "重新夺取后仍保持单飞");
    }

    // ── 只读快照 snapshot_all（诊断端点） ─────────────────────────────

    /// 三态 + 租约态映射：closed / open / halfopen / probe_in_flight。
    #[test]
    fn snapshot_all_maps_states_and_lease() {
        let cb = CircuitBreaker::new(2, 60);
        // closed-ch：1 次失败 < 阈值 2 → closed
        cb.record_failure("closed-ch", FailureType::Timeout);
        // open-ch：2 次失败 → open（冷却 60s 内）
        cb.record_failure("open-ch", FailureType::ServerError);
        cb.record_failure("open-ch", FailureType::ServerError);
        // halfopen-ch：2 次失败后把最近失败时间回写为 61s 前 → 冷却已过
        cb.record_failure("halfopen-ch", FailureType::ServerError);
        cb.record_failure("halfopen-ch", FailureType::ServerError);
        if let Some(mut entry) = cb.states.get_mut("halfopen-ch") {
            entry.last_failure_time = Some(Instant::now() - Duration::from_secs(61));
        }

        let snaps = cb.snapshot_all();
        assert_eq!(snaps.len(), 3, "从未失败的渠道无条目，其余三条都在");

        let by_id = |id: &str| {
            snaps
                .iter()
                .find(|s| s.channel_id == id)
                .unwrap_or_else(|| panic!("missing snapshot for {id}"))
        };

        let closed = by_id("closed-ch");
        assert_eq!(closed.state, "closed");
        assert_eq!(closed.failure_count, 1);
        assert_eq!(closed.failure_type, Some("timeout"));
        assert_eq!(closed.cooldown_remaining_secs, 0);
        assert!(!closed.probe_in_flight);

        let open = by_id("open-ch");
        assert_eq!(open.state, "open");
        assert_eq!(open.failure_count, 2);
        assert_eq!(open.failure_type, Some("server_error"));
        assert_eq!(open.cooldown_remaining_secs, 60, "刚失败：剩余冷却≈60s");
        assert!(!open.probe_in_flight);

        let half = by_id("halfopen-ch");
        assert_eq!(half.state, "halfopen");
        assert_eq!(half.cooldown_remaining_secs, 0, "冷却已过：无剩余冷却");

        // 租约态：halfopen 渠道抢到试探租约 → probe_in_flight = true
        assert!(cb.allow_request("halfopen-ch"), "halfopen 抢到试探权");
        let leased = cb
            .snapshot_all()
            .into_iter()
            .find(|s| s.channel_id == "halfopen-ch")
            .unwrap();
        assert!(leased.probe_in_flight, "在飞租约应在快照可见");
    }

    /// 限流窗口剩余秒数：RateLimited 记录 retry_after 后可见，过期归零。
    #[test]
    fn snapshot_all_reports_rate_limit_remaining() {
        let cb = CircuitBreaker::new(5, 30);
        cb.record_failure(
            "rl-ch",
            FailureType::RateLimited {
                scope: RateLimitScope::Account,
                retry_after: Some(120),
            },
        );
        let snap = cb.snapshot_all().into_iter().next().unwrap();
        assert_eq!(snap.failure_type, Some("rate_limited"));
        assert!(
            snap.rate_limit_remaining_secs > 0 && snap.rate_limit_remaining_secs <= 120,
            "限流剩余应在 (0, 120] 内，实测 {}",
            snap.rate_limit_remaining_secs
        );
        // AuthFailed：30 分钟强制冷却 → 限流窗口=冷却窗口
        cb.record_failure("auth-ch", FailureType::AuthFailed);
        let snap = cb
            .snapshot_all()
            .into_iter()
            .find(|s| s.channel_id == "auth-ch")
            .unwrap();
        assert_eq!(snap.failure_type, Some("auth_failed"));
        assert!(snap.rate_limit_remaining_secs > 0, "认证故障强制 30 分钟冷却");
    }

    /// 快照只读：连续两次调用结果一致，不改变任何状态机行为。
    #[test]
    fn snapshot_all_is_read_only() {
        let cb = CircuitBreaker::new(1, 30);
        cb.record_failure("ch1", FailureType::ServerError);
        let before = cb.allow_request("ch1");
        let s1 = cb.snapshot_all();
        let s2 = cb.snapshot_all();
        assert_eq!(s1.len(), s2.len());
        assert_eq!(s1[0].state, s2[0].state);
        assert_eq!(s1[0].failure_count, s2[0].failure_count);
        assert_eq!(before, cb.allow_request("ch1"), "快照不应影响放行判定");
    }
}
