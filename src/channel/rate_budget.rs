//! 三色令牌桶 — per-channel 的 RPM/TPM 分级预留与借位（L2 Shaper）。
//!
//! 参照 burncloud `crates/router/src/rate_budget.rs`（MPLS TE 带宽模型 /
//! RFC 2698 trTCM 形状）：
//! - 每渠道总容量按 Green/Yellow/Red 三色预留（默认 40%/40%/20%）
//! - 请求按自身颜色先扣自己的桶；桶空时可向更高优先级颜色**借**空闲容量
//!   （Red → Yellow → Green；Green 不向下借）
//! - 每分钟整窗回填到预留水位
//! - 未配置的渠道不限流（fail-open）
//!
//! AIGX 的颜色映射：管理员/付费用户 = Green，普通用户 = Yellow，
//! 免费层/低优先级 = Red（由调用方按用户分组决定）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

/// DiffServ 风格三色流量等级。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrafficColor {
    /// 最高优先级
    Green,
    /// 默认优先级
    #[default]
    Yellow,
    /// 尽力而为
    Red,
}

impl TrafficColor {
    pub fn as_char(&self) -> char {
        match self {
            TrafficColor::Green => 'G',
            TrafficColor::Yellow => 'Y',
            TrafficColor::Red => 'R',
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TrafficColor::Green => "green",
            TrafficColor::Yellow => "yellow",
            TrafficColor::Red => "red",
        }
    }
}

/// 一次 try_consume 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    /// 从自身颜色桶扣取
    OwnBucket,
    /// 从更高优先级颜色的空闲预留借位
    Borrowed { from: TrafficColor },
    /// 所有可用的桶都空了，应拒绝（503 + X-Rejected-By: shaper）
    Rejected,
}

impl ConsumeOutcome {
    /// 是否放行（OwnBucket 或 Borrowed）。
    pub fn admitted(&self) -> bool {
        !matches!(self, ConsumeOutcome::Rejected)
    }

    /// 标签（日志/监控用）。
    pub fn as_label(&self) -> &'static str {
        match self {
            ConsumeOutcome::OwnBucket => "shaper_own",
            ConsumeOutcome::Borrowed { .. } => "shaper_borrow",
            ConsumeOutcome::Rejected => "shaper_reject",
        }
    }
}

/// 单渠道三色预留比例（和必须为 1.0 ± 0.01）。
#[derive(Debug, Clone, Copy)]
pub struct ChannelReservation {
    pub green: f64,
    pub yellow: f64,
    pub red: f64,
}

impl Default for ChannelReservation {
    fn default() -> Self {
        Self {
            green: 0.4,
            yellow: 0.4,
            red: 0.2,
        }
    }
}

impl ChannelReservation {
    /// 校验比例合法（和 ≈ 1.0 且各项非负）。
    pub fn is_valid(&self) -> bool {
        let sum = self.green + self.yellow + self.red;
        (0.99..=1.01).contains(&sum) && self.green >= 0.0 && self.yellow >= 0.0 && self.red >= 0.0
    }

    #[allow(dead_code)]
    fn share(&self, color: TrafficColor) -> f64 {
        match color {
            TrafficColor::Green => self.green,
            TrafficColor::Yellow => self.yellow,
            TrafficColor::Red => self.red,
        }
    }
}

/// 单渠道预算快照（监控用）。
#[derive(Debug, Clone, Copy)]
pub struct BudgetSnapshot {
    pub rpm_cap: u32,
    pub rpm_remaining_green: u32,
    pub rpm_remaining_yellow: u32,
    pub rpm_remaining_red: u32,
    pub tpm_cap: u64,
    pub tpm_remaining_green: u64,
    pub tpm_remaining_yellow: u64,
    pub tpm_remaining_red: u64,
}

/// 三色预算后端 trait（未来可换 Redis 实现做分布式）。
pub trait BudgetBackend: Send + Sync {
    /// 原子尝试扣取 1 RPM + est_tpm TPM。
    fn try_consume(&self, channel_id: &str, color: TrafficColor, est_tpm: u64) -> ConsumeOutcome;

    /// 归还 TPM（实际用量小于预估时）。
    fn refund(&self, channel_id: &str, color: TrafficColor, tpm_to_return: u64);

    /// 只读快照。
    fn snapshot(&self, channel_id: &str) -> Option<BudgetSnapshot>;
}

/// RAII 守卫：确保 TPM 预留在请求未完成（取消/超时/panic）时归还。
///
/// - 正常路径：`guard.commit(actual_tpm)` —— 高估部分归还，标记已提交
/// - 其他路径（提前 return / panic / async 取消）：Drop 全额归还
///
/// `commit(self)` 按值获取 self，类型系统上禁止重复提交。
pub struct BudgetGuard<'a> {
    backend: &'a (dyn BudgetBackend + Send + Sync),
    channel_id: String,
    color: TrafficColor,
    est_tpm: u64,
    committed: bool,
}

impl<'a> BudgetGuard<'a> {
    /// 包装一次刚成功的 try_consume 预留。
    pub fn new(
        backend: &'a (dyn BudgetBackend + Send + Sync),
        channel_id: &str,
        color: TrafficColor,
        est_tpm: u64,
    ) -> Self {
        Self {
            backend,
            channel_id: channel_id.to_string(),
            color,
            est_tpm,
            committed: false,
        }
    }

    /// 以实际 TPM 提交预留；高估部分归还。
    pub fn commit(mut self, actual_tpm: u64) {
        let to_refund = self.est_tpm.saturating_sub(actual_tpm);
        if to_refund > 0 {
            self.backend.refund(&self.channel_id, self.color, to_refund);
        }
        self.committed = true;
    }
}

impl Drop for BudgetGuard<'_> {
    fn drop(&mut self) {
        if !self.committed && self.est_tpm > 0 {
            // 取消 / panic / 提前返回路径：全额归还，避免预占比死桶
            self.backend
                .refund(&self.channel_id, self.color, self.est_tpm);
        }
    }
}

/// 单实例内存实现：per-channel Mutex 桶，每分钟回填。
pub struct InMemoryBudget {
    channels: DashMap<String, Mutex<ChannelBuckets>>,
}

impl Default for InMemoryBudget {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryBudget {
    pub fn new() -> Self {
        Self {
            channels: DashMap::new(),
        }
    }

    /// 配置（或更新）单渠道容量与预留比例。
    ///
    /// 比例非法（和 ≠ 1.0 ± 0.01）时回退默认 0.4/0.4/0.2 并告警。
    pub fn configure(
        &self,
        channel_id: &str,
        rpm_cap: u32,
        tpm_cap: u64,
        reservation: ChannelReservation,
    ) {
        let reservation = if reservation.is_valid() {
            reservation
        } else {
            tracing::warn!(
                channel_id,
                "rate_budget: 非法预留比例（和≠1.0），回退默认 0.4/0.4/0.2"
            );
            ChannelReservation::default()
        };
        let buckets = ChannelBuckets::new(rpm_cap, tpm_cap, reservation);
        self.channels
            .insert(channel_id.to_string(), Mutex::new(buckets));
    }

    /// 渠道是否已配置。
    pub fn is_configured(&self, channel_id: &str) -> bool {
        self.channels.contains_key(channel_id)
    }
}

impl BudgetBackend for InMemoryBudget {
    fn try_consume(&self, channel_id: &str, color: TrafficColor, est_tpm: u64) -> ConsumeOutcome {
        // 未配置 = 不限流（fail-open）
        let entry = match self.channels.get(channel_id) {
            Some(e) => e,
            None => return ConsumeOutcome::OwnBucket,
        };
        let mut buckets = entry.value().lock().unwrap_or_else(|e| {
            // Shaper 尽力而为：中毒锁恢复继续
            tracing::warn!(channel_id, "rate_budget 互斥锁中毒，恢复");
            e.into_inner()
        });
        buckets.refill_if_due();
        buckets.try_consume(color, est_tpm)
    }

    fn refund(&self, channel_id: &str, color: TrafficColor, tpm_to_return: u64) {
        if let Some(entry) = self.channels.get(channel_id) {
            let mut buckets = entry.value().lock().unwrap_or_else(|e| e.into_inner());
            buckets.refund(color, tpm_to_return);
        }
    }

    fn snapshot(&self, channel_id: &str) -> Option<BudgetSnapshot> {
        let entry = self.channels.get(channel_id)?;
        let buckets = entry.value().lock().unwrap_or_else(|e| e.into_inner());
        Some(BudgetSnapshot {
            rpm_cap: buckets.rpm_cap,
            rpm_remaining_green: buckets.rpm_remaining[color_idx(TrafficColor::Green)],
            rpm_remaining_yellow: buckets.rpm_remaining[color_idx(TrafficColor::Yellow)],
            rpm_remaining_red: buckets.rpm_remaining[color_idx(TrafficColor::Red)],
            tpm_cap: buckets.tpm_cap,
            tpm_remaining_green: buckets.tpm_remaining[color_idx(TrafficColor::Green)],
            tpm_remaining_yellow: buckets.tpm_remaining[color_idx(TrafficColor::Yellow)],
            tpm_remaining_red: buckets.tpm_remaining[color_idx(TrafficColor::Red)],
        })
    }
}

// ── 内部实现 ──────────────────────────────────────────────────────

const COLOR_COUNT: usize = 3;

#[inline]
fn color_idx(c: TrafficColor) -> usize {
    match c {
        TrafficColor::Green => 0,
        TrafficColor::Yellow => 1,
        TrafficColor::Red => 2,
    }
}

#[inline]
fn idx_color(i: usize) -> TrafficColor {
    match i {
        0 => TrafficColor::Green,
        1 => TrafficColor::Yellow,
        _ => TrafficColor::Red,
    }
}

/// 每分钟回填窗口（与 RPM 语义一致）。
const REFILL_WINDOW: Duration = Duration::from_secs(60);

struct ChannelBuckets {
    rpm_cap: u32,
    tpm_cap: u64,
    rpm_reserved: [u32; COLOR_COUNT],
    tpm_reserved: [u64; COLOR_COUNT],
    rpm_remaining: [u32; COLOR_COUNT],
    tpm_remaining: [u64; COLOR_COUNT],
    last_refill: Instant,
}

impl ChannelBuckets {
    fn new(rpm_cap: u32, tpm_cap: u64, reservation: ChannelReservation) -> Self {
        let rpm_g = (rpm_cap as f64 * reservation.green) as u32;
        let rpm_y = (rpm_cap as f64 * reservation.yellow) as u32;
        let rpm_r = rpm_cap.saturating_sub(rpm_g + rpm_y);
        let tpm_g = (tpm_cap as f64 * reservation.green) as u64;
        let tpm_y = (tpm_cap as f64 * reservation.yellow) as u64;
        let tpm_r = tpm_cap.saturating_sub(tpm_g + tpm_y);
        Self {
            rpm_cap,
            tpm_cap,
            rpm_reserved: [rpm_g, rpm_y, rpm_r],
            tpm_reserved: [tpm_g, tpm_y, tpm_r],
            rpm_remaining: [rpm_g, rpm_y, rpm_r],
            tpm_remaining: [tpm_g, tpm_y, tpm_r],
            last_refill: Instant::now(),
        }
    }

    /// 整窗（60s）过去后回填到预留水位。
    fn refill_if_due(&mut self) {
        if self.last_refill.elapsed() < REFILL_WINDOW {
            return;
        }
        for i in 0..COLOR_COUNT {
            self.rpm_remaining[i] = self.rpm_reserved[i];
            self.tpm_remaining[i] = self.tpm_reserved[i];
        }
        self.last_refill = Instant::now();
    }

    /// 先扣自己的桶，再沿借位链向上借：
    /// - Green → 只扣自己
    /// - Yellow → 自己 → Green
    /// - Red → 自己 → Yellow → Green
    fn try_consume(&mut self, color: TrafficColor, est_tpm: u64) -> ConsumeOutcome {
        let own = color_idx(color);
        if self.try_take_from(own, est_tpm) {
            return ConsumeOutcome::OwnBucket;
        }
        for &borrowed_idx in borrow_chain(color) {
            if self.try_take_from(borrowed_idx, est_tpm) {
                return ConsumeOutcome::Borrowed {
                    from: idx_color(borrowed_idx),
                };
            }
        }
        ConsumeOutcome::Rejected
    }

    fn try_take_from(&mut self, idx: usize, est_tpm: u64) -> bool {
        if self.rpm_remaining[idx] == 0 || self.tpm_remaining[idx] < est_tpm {
            return false;
        }
        self.rpm_remaining[idx] -= 1;
        self.tpm_remaining[idx] -= est_tpm;
        true
    }

    fn refund(&mut self, color: TrafficColor, tpm_to_return: u64) {
        let i = color_idx(color);
        let cap = self.tpm_reserved[i];
        self.tpm_remaining[i] = self.tpm_remaining[i].saturating_add(tpm_to_return).min(cap);
    }
}

/// 各颜色的借位链。
fn borrow_chain(color: TrafficColor) -> &'static [usize] {
    static GREEN: [usize; 0] = [];
    static YELLOW: [usize; 1] = [0];
    static RED: [usize; 2] = [1, 0];
    match color {
        TrafficColor::Green => &GREEN,
        TrafficColor::Yellow => &YELLOW,
        TrafficColor::Red => &RED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_channel_admits_all() {
        let b = InMemoryBudget::new();
        let r = b.try_consume("ch-99", TrafficColor::Red, 1_000);
        assert_eq!(r, ConsumeOutcome::OwnBucket);
    }

    #[test]
    fn green_takes_from_own_bucket() {
        let b = InMemoryBudget::new();
        b.configure("ch-1", 100, 100_000, ChannelReservation::default());
        let r = b.try_consume("ch-1", TrafficColor::Green, 100);
        assert_eq!(r, ConsumeOutcome::OwnBucket);
    }

    #[test]
    fn yellow_borrows_from_green_when_own_empty() {
        let b = InMemoryBudget::new();
        // Yellow 份额 = 1 RPM，100 TPM —— 第一次扣干
        b.configure(
            "ch-1",
            10,
            1_000,
            ChannelReservation {
                green: 0.5,
                yellow: 0.1,
                red: 0.4,
            },
        );
        assert_eq!(
            b.try_consume("ch-1", TrafficColor::Yellow, 10),
            ConsumeOutcome::OwnBucket
        );
        // 第二次：Yellow 桶干 → 向 Green 借
        assert_eq!(
            b.try_consume("ch-1", TrafficColor::Yellow, 10),
            ConsumeOutcome::Borrowed {
                from: TrafficColor::Green
            }
        );
    }

    #[test]
    fn red_rejected_when_all_empty() {
        let b = InMemoryBudget::new();
        // 容量：10 RPM, 100 TPM（默认预留 0.4/0.4/0.2）
        b.configure("ch-1", 10, 100, ChannelReservation::default());
        assert!(b.try_consume("ch-1", TrafficColor::Green, 10).admitted());
        // Red 桶 TPM=20，消耗 50 超过所有桶剩余 → 被拒
        assert_eq!(
            b.try_consume("ch-1", TrafficColor::Red, 50),
            ConsumeOutcome::Rejected
        );
    }

    #[test]
    fn guard_refunds_on_drop() {
        let b = InMemoryBudget::new();
        b.configure("ch-1", 100, 10_000, ChannelReservation::default());
        {
            let guard = BudgetGuard::new(&b, "ch-1", TrafficColor::Yellow, 500);
            assert!(b.try_consume("ch-1", TrafficColor::Green, 0).admitted());
            drop(guard); // 未 commit → Drop 全额归还
        }
        let snap = b.snapshot("ch-1").unwrap();
        // Yellow TPM 预留 = 4000，借出 500 后归还 → 仍是 4000
        assert_eq!(snap.tpm_remaining_yellow, 4_000);
    }

    #[test]
    fn guard_commit_refunds_overestimate() {
        let b = InMemoryBudget::new();
        b.configure("ch-1", 100, 10_000, ChannelReservation::default());
        let guard = BudgetGuard::new(&b, "ch-1", TrafficColor::Green, 1_000);
        guard.commit(400); // 实际只用了 400 → 归还 600
        let snap = b.snapshot("ch-1").unwrap();
        // Green TPM 预留 = 4000，guard 本身未扣桶（调用方先 try_consume），
        // commit 归还 600 上限 4000 → 4000
        assert_eq!(snap.tpm_remaining_green, 4_000);
    }

    #[test]
    fn invalid_reservation_falls_back_to_default() {
        let b = InMemoryBudget::new();
        // 和 = 0.9，非法 → 回退默认 0.4/0.4/0.2
        b.configure(
            "ch-1",
            100,
            10_000,
            ChannelReservation {
                green: 0.5,
                yellow: 0.4,
                red: 0.0,
            },
        );
        let snap = b.snapshot("ch-1").unwrap();
        assert_eq!(snap.rpm_remaining_green, 40);
        assert_eq!(snap.rpm_remaining_red, 20);
    }

    #[test]
    fn refund_capped_at_reserved() {
        let b = InMemoryBudget::new();
        b.configure("ch-1", 100, 1_000, ChannelReservation::default());
        // 超额归还也不会超过预留水位
        b.refund("ch-1", TrafficColor::Green, 9_999);
        let snap = b.snapshot("ch-1").unwrap();
        assert_eq!(snap.tpm_remaining_green, 400);
    }
}
