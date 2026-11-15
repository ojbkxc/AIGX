//! 内容拦截降级状态机（对照 Agent2API `core/degrade.rs`）。
//!
//! ── 它解决什么 ───────────────────────────────────────────────
//! 输入护栏（关键词/正则黑名单）命中请求体消息文本时，默认策略**不是**
//! 拒绝请求，而是：
//! ```text
//!   ① 本次请求改用最小中性提示词（同账号、原地重发一次，不罚账号不换账号）；
//!   ② 同时触发降级：接下来一段时间内，请求直接携带中性提示词出门，
//!      不再先撞一次规则再补救。
//! ```
//!
//! 降级期到**次日 00:00（UTC+8）**结束——与参考项目同一重置口径：拦截
//! 窗口通常与「当天」绑定（上游风控按天统计），次日零点是一次天然重置点；
//! 「无限期降级」会让用户在问题早已消失后仍拿不到自己的 system 提示词。
//!
//! ── 触发不续期 ───────────────────────────────────────────────
//! 降级期内再次触发**不延长**截止时间（保持最早那个零点）：否则一条持续
//! 命中的客户端会把降级期无限推后，等于永久降级（参考项目明确语义）。
//!
//! ── 状态放内存、重启清零 ─────────────────────────────────────
//! 它是运行期状态而不是配置：进程重启等于一次人工介入，把降级带过去没有
//! 意义；落盘还会引入「什么时候该清」的第二套判据，得不偿失。
//!
//! ── 手动解除 ─────────────────────────────────────────────────
//! 用户可能当场把提示词改好了（或调整了词表），此时还要等到零点纯属折磨。
//! 管理面 `POST /api/guardrail/degrade/clear` 显式清零（设置页「解除降级」）。

use std::sync::atomic::{AtomicI64, Ordering};

/// UTC+8 的固定偏移（毫秒）。
///
/// 用固定偏移而不是时区库：中国标准时间没有夏令时，1970 年后恒为 +08:00
/// —— 参考项目同样用固定偏移（不依赖宿主机时区配置）。
const CST_OFFSET_MS: i64 = 8 * 3600 * 1000;

/// 一天的毫秒数。
const DAY_MS: i64 = 86_400_000;

/// 降级状态机。
///
/// 独立成类型（而不是裸 static）是为了单测可以各自持有一份实例，
/// 不与其他测试共享全局状态、也不受测试并行执行影响。
#[derive(Debug)]
pub struct DegradeState {
    /// 降级截止时刻（UTC 毫秒）；`0` = 未触发。
    until_ms: AtomicI64,
}

impl Default for DegradeState {
    fn default() -> Self {
        Self::new()
    }
}

impl DegradeState {
    pub const fn new() -> Self {
        Self {
            until_ms: AtomicI64::new(0),
        }
    }

    /// 降级截止时刻；已过期视为 `0`（调用方不必自己比较时间）。
    pub fn until_at(&self, now_ms: i64) -> i64 {
        let until = self.until_ms.load(Ordering::SeqCst);
        if until > 0 && now_ms < until {
            until
        } else {
            0
        }
    }

    /// 当前是否处于降级期。
    pub fn active_at(&self, now_ms: i64) -> bool {
        self.until_at(now_ms) > 0
    }

    /// 触发降级（返回本次生效的截止时刻）。
    ///
    /// 已在降级期内**不续期**：直接返回原来的截止时刻。
    pub fn trigger_at(&self, now_ms: i64) -> i64 {
        let current = self.until_ms.load(Ordering::SeqCst);
        if current > now_ms {
            return current;
        }
        let next = next_midnight_cst_ms(now_ms);
        self.until_ms.store(next, Ordering::SeqCst);
        next
    }

    /// 解除降级（截止时刻清零）。
    pub fn clear(&self) {
        self.until_ms.store(0, Ordering::SeqCst);
    }
}

/// 进程级降级状态（数据面与管理面共用）。
static DEGRADE: DegradeState = DegradeState::new();

/// 当前时刻（UTC 毫秒）。
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 当前是否处于降级期（数据面每请求读一次）。
pub fn active() -> bool {
    DEGRADE.active_at(now_ms())
}

/// 降级截止时刻（UTC 毫秒）；`0` = 当前不在降级期。
pub fn until_ms() -> i64 {
    DEGRADE.until_at(now_ms())
}

/// 触发降级（已在内不续期），返回截止时刻（UTC 毫秒）。
pub fn trigger() -> i64 {
    DEGRADE.trigger_at(now_ms())
}

/// 解除降级（管理面手动出口）。
pub fn clear() {
    DEGRADE.clear();
}

/// 降级截止时刻的本地化展示（UTC+8）。
///
/// `0` 或无法格式化时给空串——这只是界面文案，绝不能因为它把接口带崩
/// （release 是 panic=abort）。
pub fn until_text() -> String {
    let until = until_ms();
    if until <= 0 {
        return String::new();
    }
    let Some(utc) = chrono::DateTime::from_timestamp_millis(until) else {
        return String::new();
    };
    let Some(offset) = chrono::FixedOffset::east_opt(8 * 3600) else {
        return String::new();
    };
    utc.with_timezone(&offset)
        .format("%Y/%m/%d %H:%M")
        .to_string()
}

/// `now` 之后最近的 UTC+8 零点（纯函数，便于核对边界）。
///
/// 边界语义：
///   - `23:59` → 次日 `00:00`（几分钟后）；
///   - 恰好 `00:00` → **次日** `00:00`（刚过零点，下个零点是明天）。
///
/// 算法：把时刻平移到 CST 视角（+8h），当天零点就是 `div_euclid(DAY) * DAY`，
/// 再加一天即为「最近的未来零点」；`div_euclid` 保证 1970 年之前的负数时刻
/// 也落在正确的整天边界上（负数除法在 Rust 里默认向零取整，直接 `/` 会算错）。
fn next_midnight_cst_ms(now_ms: i64) -> i64 {
    let shifted = now_ms + CST_OFFSET_MS;
    let start_of_today = shifted.div_euclid(DAY_MS) * DAY_MS;
    start_of_today + DAY_MS - CST_OFFSET_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-01-01 00:00:00 UTC+8 == 2025-12-31 16:00:00 UTC。
    const MIDNIGHT_CST: i64 = 1_767_196_800_000;

    #[test]
    fn next_midnight_before_midnight_is_today() {
        // 23:59:59.999（UTC+8）→ 最近零点就是今天的 00:00
        assert_eq!(next_midnight_cst_ms(MIDNIGHT_CST - 1), MIDNIGHT_CST);
        assert_eq!(next_midnight_cst_ms(MIDNIGHT_CST - 60_000), MIDNIGHT_CST);
    }

    #[test]
    fn next_midnight_at_midnight_is_tomorrow() {
        // 恰好 00:00 → 下个零点是明天（不是「现在」）
        assert_eq!(next_midnight_cst_ms(MIDNIGHT_CST), MIDNIGHT_CST + DAY_MS);
        assert_eq!(
            next_midnight_cst_ms(MIDNIGHT_CST + 1),
            MIDNIGHT_CST + DAY_MS
        );
    }

    #[test]
    fn trigger_sets_until_next_midnight() {
        let state = DegradeState::new();
        let now = MIDNIGHT_CST - 3_600_000; // 23:00 UTC+8
        let until = state.trigger_at(now);
        assert_eq!(until, MIDNIGHT_CST);
        assert!(state.active_at(now));
        assert!(state.active_at(MIDNIGHT_CST - 1));
    }

    #[test]
    fn degrade_auto_recovers_at_next_midnight() {
        let state = DegradeState::new();
        let now = MIDNIGHT_CST - 1_000;
        state.trigger_at(now);
        // 下一秒仍是降级期
        assert!(state.active_at(MIDNIGHT_CST - 1));
        // 到达零点的瞬间自动恢复（不续期、不需要任何清理动作）
        assert!(!state.active_at(MIDNIGHT_CST));
        assert_eq!(state.until_at(MIDNIGHT_CST), 0);
        // 过期后再触发，从现在重新算到次日零点
        assert_eq!(state.trigger_at(MIDNIGHT_CST + 1), MIDNIGHT_CST + DAY_MS);
    }

    #[test]
    fn retrigger_does_not_extend_deadline() {
        let state = DegradeState::new();
        let first = state.trigger_at(MIDNIGHT_CST - 10_000);
        let second = state.trigger_at(MIDNIGHT_CST - 5_000);
        assert_eq!(first, second, "降级期内触发不得续期");
        assert_eq!(state.until_at(MIDNIGHT_CST - 1), first);
    }

    #[test]
    fn clear_releases_immediately() {
        let state = DegradeState::new();
        state.trigger_at(MIDNIGHT_CST - 10_000);
        assert!(state.active_at(MIDNIGHT_CST - 9_999));
        state.clear();
        assert!(!state.active_at(MIDNIGHT_CST - 9_999));
        assert_eq!(state.until_at(MIDNIGHT_CST - 9_999), 0);
    }
}
