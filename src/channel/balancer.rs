//! 渠道负载均衡 — RoundRobin + 加权轮询。
//!
//! 参照 burncloud `crates/router/src/balancer/mod.rs` 的 `RoundRobinBalancer`：
//! - per-group 原子计数器，`fetch_add` 后对组大小取模
//! - 线程安全：`DashMap<String, AtomicUsize>`
//!
//! AIGX 既有 `ChannelStore::select_for_model` 已按 priority 分组 + weight 加权随机
//! 抽取。本模块在其之上提供确定性 RoundRobin（同组内严格轮询），适合：
//! - 测试场景需要可复现的渠道选取顺序
//! - 同优先级同权重渠道希望均匀分布（而非随机）
//!
//! 加权 RoundRobin：把每个渠道按 weight 展开成 weight 个槽位，按槽位轮询。
//! 例如权重 `[2, 1]` 展开为 `[A, A, B]`，轮询序列 `A, A, B, A, A, B, ...`。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

/// RoundRobin 负载均衡器 — per-group 原子计数器。
///
/// 线程安全：DashMap + AtomicUsize，无锁热路径。
pub struct RoundRobinBalancer {
    counters: Arc<DashMap<String, AtomicUsize>>,
}

impl Default for RoundRobinBalancer {
    fn default() -> Self {
        Self::new()
    }
}

impl RoundRobinBalancer {
    pub fn new() -> Self {
        Self {
            counters: Arc::new(DashMap::new()),
        }
    }

    /// 取组内下一个索引（`group_size` 取模）。
    ///
    /// `group_size == 0` 时返回 0（调用方应自行判空）。
    pub fn next_index(&self, group_id: &str, group_size: usize) -> usize {
        if group_size == 0 {
            return 0;
        }
        let counter = self
            .counters
            .entry(group_id.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        let current = counter.fetch_add(1, Ordering::Relaxed);
        current % group_size
    }

    /// 从候选切片中选下一个（RoundRobin）。
    ///
    /// 返回 `Some(&T)` 或 `None`（候选为空时）。
    pub fn next<'a, T>(&self, group_id: &str, candidates: &'a [T]) -> Option<&'a T> {
        if candidates.is_empty() {
            return None;
        }
        let idx = self.next_index(group_id, candidates.len());
        candidates.get(idx)
    }

    /// 加权 RoundRobin：按权重展开槽位后轮询。
    ///
    /// - `candidates`：`(item, weight)` 列表
    /// - 返回 `Some(&T)` 或 `None`（候选为空或全 0 权重时）
    ///
    /// 实现方式：维护 per-group 计数器，对总权重取模得到槽位，再映射回候选。
    /// 权重为 0 的候选被跳过（视为不可用）。
    pub fn next_weighted<'a, T>(
        &self,
        group_id: &str,
        candidates: &'a [(T, u32)],
    ) -> Option<&'a T> {
        // 过滤掉 0 权重候选，保留原索引
        let effective: Vec<(usize, u32)> = candidates
            .iter()
            .enumerate()
            .filter(|(_, (_, w))| *w > 0)
            .map(|(idx, (_, w))| (idx, *w))
            .collect();
        if effective.is_empty() {
            return None;
        }
        let total_weight: u32 = effective.iter().map(|(_, w)| *w).sum();
        if total_weight == 0 {
            return None;
        }

        let counter = self
            .counters
            .entry(group_id.to_string())
            .or_insert_with(|| AtomicUsize::new(0));
        let slot = counter.fetch_add(1, Ordering::Relaxed) as u32 % total_weight;

        // 在展开的槽位序列中找到对应候选
        let mut acc = 0u32;
        for (idx, w) in &effective {
            acc += *w;
            if slot < acc {
                return candidates.get(*idx).map(|(item, _)| item);
            }
        }
        // 理论不可达（slot < total_weight 保证落在某个区间）
        let (idx, _) = effective.last().unwrap();
        candidates.get(*idx).map(|(item, _)| item)
    }

    /// 重置指定组的计数器（管理面/测试用）。
    pub fn reset_group(&self, group_id: &str) {
        if let Some(c) = self.counters.get(group_id) {
            c.store(0, Ordering::Relaxed);
        }
    }

    /// 清除所有组的状态（测试用）。
    pub fn clear(&self) {
        self.counters.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_robin_wraps_around() {
        let b = RoundRobinBalancer::new();
        assert_eq!(b.next_index("g1", 3), 0);
        assert_eq!(b.next_index("g1", 3), 1);
        assert_eq!(b.next_index("g1", 3), 2);
        assert_eq!(b.next_index("g1", 3), 0);
    }

    #[test]
    fn separate_groups_independent() {
        let b = RoundRobinBalancer::new();
        assert_eq!(b.next_index("g1", 2), 0);
        assert_eq!(b.next_index("g2", 2), 0);
        assert_eq!(b.next_index("g1", 2), 1);
        assert_eq!(b.next_index("g2", 2), 1);
    }

    #[test]
    fn next_returns_none_on_empty() {
        let b = RoundRobinBalancer::new();
        let cands: Vec<i32> = vec![];
        assert!(b.next("g", &cands).is_none());
    }

    #[test]
    fn next_iterates_candidates() {
        let b = RoundRobinBalancer::new();
        let cands = vec!["a", "b", "c"];
        assert_eq!(b.next("g", &cands), Some(&"a"));
        assert_eq!(b.next("g", &cands), Some(&"b"));
        assert_eq!(b.next("g", &cands), Some(&"c"));
        assert_eq!(b.next("g", &cands), Some(&"a"));
    }

    #[test]
    fn weighted_round_robin_respects_weights() {
        let b = RoundRobinBalancer::new();
        // 权重 A=2, B=1 → 槽位 [A, A, B]
        let cands: Vec<(&str, u32)> = vec![("A", 2), ("B", 1)];
        let mut seq = Vec::new();
        for _ in 0..6 {
            seq.push(*b.next_weighted("g", &cands).unwrap());
        }
        assert_eq!(seq, vec!["A", "A", "B", "A", "A", "B"]);
    }

    #[test]
    fn weighted_skips_zero_weight() {
        let b = RoundRobinBalancer::new();
        let cands: Vec<(&str, u32)> = vec![("A", 0), ("B", 1)];
        // A 权重 0 被跳过，只选 B
        assert_eq!(b.next_weighted("g", &cands), Some(&"B"));
        assert_eq!(b.next_weighted("g", &cands), Some(&"B"));
    }

    #[test]
    fn weighted_returns_none_on_all_zero() {
        let b = RoundRobinBalancer::new();
        let cands: Vec<(&str, u32)> = vec![("A", 0), ("B", 0)];
        assert!(b.next_weighted("g", &cands).is_none());
    }

    #[test]
    fn weighted_returns_none_on_empty() {
        let b = RoundRobinBalancer::new();
        let cands: Vec<(&str, u32)> = vec![];
        assert!(b.next_weighted("g", &cands).is_none());
    }

    #[test]
    fn reset_group_restarts_counter() {
        let b = RoundRobinBalancer::new();
        assert_eq!(b.next_index("g", 3), 0);
        assert_eq!(b.next_index("g", 3), 1);
        b.reset_group("g");
        assert_eq!(b.next_index("g", 3), 0);
    }
}
