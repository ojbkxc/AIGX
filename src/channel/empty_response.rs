//! 空响应检测 — 追踪 per-channel 连续空响应（HTTP 200 但零 token）。
//!
//! 参照 burncloud `crates/router/src/lib.rs` 的 `EmptyResponseCounter`（第 42-133 行）：
//! - 滑动计数：连续空响应累加，成功响应清零
//! - 超过阈值（默认 3 次）返回 `true`，调用方据此触发渠道故障处理
//! - 接近阈值时打 warn 日志，便于运维提前介入
//!
//! 集成点：proxy 响应处理在统计 token 后，若 `completion_tokens == 0` 调用
//! `record_empty`；非零则调用 `reset`。`record_empty` 返回 `true` 时，
//! 调用方应把该渠道置入冷却或记入断路器失败（`FailureType::EmptyResponse`）。

use std::collections::HashMap;
use std::sync::RwLock;

/// 默认阈值：连续 3 次空响应判定为故障。
const DEFAULT_EMPTY_RESPONSE_THRESHOLD: u32 = 3;

/// per-channel 连续空响应计数器。
///
/// 线程安全：`std::sync::RwLock<HashMap>`，与 burncloud 实现一致。
/// 读写频率低（每次请求结束一次），RwLock 足够。
pub struct EmptyResponseCounter {
    counters: RwLock<HashMap<String, u32>>,
    threshold: u32,
}

impl Default for EmptyResponseCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl EmptyResponseCounter {
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(HashMap::new()),
            threshold: DEFAULT_EMPTY_RESPONSE_THRESHOLD,
        }
    }

    /// 用自定义阈值构造。
    pub fn with_threshold(threshold: u32) -> Self {
        Self {
            counters: RwLock::new(HashMap::new()),
            threshold: threshold.max(1),
        }
    }

    /// 计入一次空响应。返回 `true` 表示连续空响应已超阈值（应触发故障处理）。
    ///
    /// 接近阈值（threshold - 1）时打 warn 日志；超阈值时打更高级别 warn。
    pub fn record_empty(&self, channel_id: &str) -> bool {
        let mut counters = self
            .counters
            .write()
            .expect("EmptyResponseCounter lock poisoned");
        let count = counters.entry(channel_id.to_string()).or_insert(0);
        *count += 1;
        let exceeded = *count >= self.threshold;

        if *count == self.threshold.saturating_sub(1) {
            tracing::warn!(
                channel_id = channel_id,
                count = *count,
                threshold = self.threshold,
                "空响应计数接近阈值，建议关注该渠道"
            );
        } else if exceeded {
            tracing::warn!(
                channel_id = channel_id,
                count = *count,
                threshold = self.threshold,
                "连续空响应超阈值，标记渠道故障"
            );
        } else {
            tracing::debug!(
                channel_id = channel_id,
                count = *count,
                threshold = self.threshold,
                "记入空响应，未达阈值"
            );
        }
        exceeded
    }

    /// 成功（非空）响应后清零计数。
    pub fn reset(&self, channel_id: &str) {
        let mut counters = self
            .counters
            .write()
            .expect("EmptyResponseCounter lock poisoned");
        if let Some(count) = counters.get_mut(channel_id) {
            if *count > 0 {
                tracing::debug!(
                    channel_id = channel_id,
                    previous_count = *count,
                    "成功响应后清零空响应计数"
                );
                *count = 0;
            }
        }
    }

    /// 获取当前计数（监控/管理面用）。
    pub fn get_count(&self, channel_id: &str) -> u32 {
        let counters = self
            .counters
            .read()
            .expect("EmptyResponseCounter lock poisoned");
        counters.get(channel_id).copied().unwrap_or(0)
    }

    /// 管理员强制重置（返回 previous count）。
    pub fn force_reset(&self, channel_id: &str) -> u32 {
        let mut counters = self
            .counters
            .write()
            .expect("EmptyResponseCounter lock poisoned");
        let previous = counters.remove(channel_id).unwrap_or(0);
        if previous > 0 {
            tracing::info!(
                channel_id = channel_id,
                previous_count = previous,
                "管理员强制重置空响应计数"
            );
        }
        previous
    }

    /// 所有非零计数的渠道（监控用）。
    pub fn get_all_counts(&self) -> Vec<(String, u32)> {
        let counters = self
            .counters
            .read()
            .expect("EmptyResponseCounter lock poisoned");
        counters
            .iter()
            .filter(|(_, &c)| c > 0)
            .map(|(k, &v)| (k.clone(), v))
            .collect()
    }

    /// 当前阈值。
    pub fn threshold(&self) -> u32 {
        self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn below_threshold_returns_false() {
        let c = EmptyResponseCounter::with_threshold(3);
        assert!(!c.record_empty("ch1"));
        assert!(!c.record_empty("ch1"));
    }

    #[test]
    fn at_threshold_returns_true() {
        let c = EmptyResponseCounter::with_threshold(3);
        c.record_empty("ch1");
        c.record_empty("ch1");
        assert!(c.record_empty("ch1"));
    }

    #[test]
    fn reset_clears_count() {
        let c = EmptyResponseCounter::with_threshold(3);
        c.record_empty("ch1");
        c.record_empty("ch1");
        c.reset("ch1");
        assert_eq!(c.get_count("ch1"), 0);
        // 重置后再次累加
        assert!(!c.record_empty("ch1"));
    }

    #[test]
    fn separate_channels_tracked_independently() {
        let c = EmptyResponseCounter::with_threshold(2);
        assert!(!c.record_empty("ch1"));
        assert!(!c.record_empty("ch2")); // ch2 第一次，未超
                                         // ch2 第二次超阈值
        assert!(c.record_empty("ch2"));
        assert_eq!(c.get_count("ch1"), 1);
        assert_eq!(c.get_count("ch2"), 2);
    }

    #[test]
    fn force_reset_returns_previous() {
        let c = EmptyResponseCounter::with_threshold(3);
        c.record_empty("ch1");
        c.record_empty("ch1");
        assert_eq!(c.force_reset("ch1"), 2);
        assert_eq!(c.get_count("ch1"), 0);
    }

    #[test]
    fn get_all_counts_filters_zero() {
        let c = EmptyResponseCounter::with_threshold(5);
        c.record_empty("ch1");
        c.record_empty("ch2");
        c.reset("ch1");
        let all = c.get_all_counts();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "ch2");
    }
}
