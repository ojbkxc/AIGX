//! 渠道亲和性路由 — session+model 粘性路由 + HRW 一致性哈希。
//!
//! 参照 burncloud `crates/router/src/affinity.rs`：
//! - 同一 `(session_id, model)` 优先路由到上次成功的渠道（保留上游 KV 缓存）
//! - 双 TTL：「粘性 TTL」内直接返回缓存渠道；「硬 TTL」过期强制清除
//! - 缓存未命中时，调用方用 `pick_hrw` 在候选渠道中按 HRW 一致性哈希选取
//! - 失败时调用 `evict` 清除亲和性，下次请求重新走 HRW
//!
//! HRW（Highest Random Weight）算法：score = hash(key, channel_id) × weight × health，
//! 选 score 最大的渠道。相比一致性哈希更适合候选集小且频繁变更的场景。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// 默认粘性 TTL（5 分钟）：窗口内直接返回缓存渠道。
pub const DEFAULT_STICKY_TTL: Duration = Duration::from_secs(5 * 60);
/// 默认硬 TTL（30 分钟）：过期强制清除条目。
pub const DEFAULT_HARD_TTL: Duration = Duration::from_secs(30 * 60);

/// 复合缓存键 `(session_id, model)` — 同 session 不同模型独立路由。
type CacheKey = (String, String);

/// 缓存条目：渠道 ID + 创建时间（用于 TTL 判断）。
#[derive(Debug, Clone)]
struct CacheEntry {
    channel_id: String,
    created_at: Instant,
}

/// 亲和性缓存 — DashMap 实现，双 TTL。
///
/// 线程安全：所有方法通过 DashMap 的内部分片锁保护。
pub struct AffinityCache {
    entries: DashMap<CacheKey, CacheEntry>,
    sticky_ttl: Duration,
    hard_ttl: Duration,
}

impl Default for AffinityCache {
    fn default() -> Self {
        Self::with_ttls(DEFAULT_STICKY_TTL, DEFAULT_HARD_TTL)
    }
}

impl AffinityCache {
    /// 用指定 TTL 构造（`sticky_ttl` 必须 ≤ `hard_ttl`，否则 panic）。
    pub fn with_ttls(sticky_ttl: Duration, hard_ttl: Duration) -> Self {
        assert!(
            sticky_ttl <= hard_ttl,
            "sticky_ttl 必须 <= hard_ttl，得到 sticky={:?} hard={:?}",
            sticky_ttl,
            hard_ttl
        );
        Self {
            entries: DashMap::new(),
            sticky_ttl,
            hard_ttl,
        }
    }

    /// 查询 `(session_id, model)` 的亲和渠道。
    ///
    /// - `Some(id)`：粘性窗口内有缓存条目
    /// - `None`：无条目，或已过硬 TTL（条目被清除），或过粘性 TTL 但未过硬 TTL
    ///   （调用方应重新走 HRW；若 HRW 选回同一渠道，调用 `insert` 续期）
    pub fn lookup(&self, session_id: &str, model: &str) -> Option<String> {
        let compound = (session_id.to_string(), model.to_string());
        let entry = self.entries.get(&compound)?;
        let age = entry.created_at.elapsed();
        if age > self.hard_ttl {
            drop(entry);
            self.entries.remove(&compound);
            return None;
        }
        if age > self.sticky_ttl {
            return None;
        }
        Some(entry.channel_id.clone())
    }

    /// 写入或刷新亲和条目 `(session_id, model) → channel_id`。
    ///
    /// 请求成功后调用，建立/续期亲和性。
    pub fn insert(&self, session_id: &str, model: &str, channel_id: &str) {
        let compound = (session_id.to_string(), model.to_string());
        self.entries.insert(
            compound,
            CacheEntry {
                channel_id: channel_id.to_string(),
                created_at: Instant::now(),
            },
        );
    }

    /// 清除 `(session_id, model)` 的亲和条目。
    ///
    /// 渠道故障时调用，避免下次还粘到病渠道。
    pub fn evict(&self, session_id: &str, model: &str) {
        let compound = (session_id.to_string(), model.to_string());
        self.entries.remove(&compound);
    }

    /// 近似条目数（DashMap len 在并发下是近似值）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空（近似）。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 用 HRW（Rendezvous Hashing）为 `session_id` 在候选渠道中选一个。
///
/// `score(channel_i) = hash(session_id, channel_i.id) × weight_i × health_i`
///
/// - `candidates`：`(&Channel id, weight)` 列表（已过滤过的候选池）
/// - `health_of`：返回渠道健康分 `[0, 1]`；`0.0` 的渠道被跳过（病渠道不能赢 HRW）
///
/// 仅当候选池为空或所有候选健康分都为 0 时返回 `None`。
///
/// 注：此处用 `(&str, u32)` 而非 burncloud 的 `(&Channel, i32)`，避免本模块依赖
/// `Channel` 结构，保持解耦。调用方负责把 `Channel` 投影成 `(id, weight)`。
pub fn pick_hrw<F>(session_id: &str, candidates: &[(&str, u32)], health_of: F) -> Option<String>
where
    F: Fn(&str) -> f64,
{
    let mut best: Option<(String, f64)> = None;
    for (ch_id, weight) in candidates {
        let health = health_of(ch_id);
        if health <= 0.0 {
            continue;
        }
        let h = mix_hash(session_id, ch_id);
        // u64 哈希映射到 (0, 1] 均匀浮点，再乘权重与健康分
        let r = (h as f64 + 1.0) / (u64::MAX as f64 + 1.0);
        let score = r * (*weight as f64).max(1.0) * health;
        match &best {
            Some((_, best_score)) if *best_score >= score => {}
            _ => best = Some((ch_id.to_string(), score)),
        }
    }
    best.map(|(id, _)| id)
}

/// 混合哈希 `(key, channel_id)` — 用标准库 DefaultHasher（足够 HRW 用）。
fn mix_hash(key: &str, channel_id: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    channel_id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrw_is_deterministic() {
        let cands: Vec<(&str, u32)> = vec![("c1", 1), ("c2", 1), ("c3", 1)];
        let p1 = pick_hrw("user-A", &cands, |_| 1.0);
        let p2 = pick_hrw("user-A", &cands, |_| 1.0);
        assert_eq!(p1, p2);
        assert!(p1.is_some());
    }

    #[test]
    fn hrw_skips_dead_candidates() {
        let cands: Vec<(&str, u32)> = vec![("c1", 100), ("c2", 1)];
        // c1 权重 100 但已死 → 必须选 c2
        let pick = pick_hrw("user-A", &cands, |id| if id == "c1" { 0.0 } else { 1.0 });
        assert_eq!(pick.as_deref(), Some("c2"));
    }

    #[test]
    fn hrw_returns_none_on_empty() {
        let pick = pick_hrw("user-A", &[], |_| 1.0);
        assert_eq!(pick, None);
    }

    #[test]
    fn hrw_returns_none_when_all_dead() {
        let cands: Vec<(&str, u32)> = vec![("c1", 1), ("c2", 1)];
        let pick = pick_hrw("user-A", &cands, |_| 0.0);
        assert_eq!(pick, None);
    }

    #[test]
    fn cache_round_trip() {
        let cache = AffinityCache::default();
        cache.insert("sess-1", "glm-5.1", "ch-42");
        assert_eq!(cache.lookup("sess-1", "glm-5.1").as_deref(), Some("ch-42"));
    }

    #[test]
    fn cache_separates_per_model() {
        let cache = AffinityCache::default();
        cache.insert("sess-1", "glm-5.1", "ch-1");
        cache.insert("sess-1", "claude", "ch-2");
        assert_eq!(cache.lookup("sess-1", "glm-5.1").as_deref(), Some("ch-1"));
        assert_eq!(cache.lookup("sess-1", "claude").as_deref(), Some("ch-2"));
    }

    #[test]
    fn cache_evict_removes_entry() {
        let cache = AffinityCache::default();
        cache.insert("sess-1", "glm-5.1", "ch-7");
        cache.evict("sess-1", "glm-5.1");
        assert_eq!(cache.lookup("sess-1", "glm-5.1"), None);
    }

    #[test]
    fn sticky_ttl_returns_none_after_expiry() {
        let cache = AffinityCache::with_ttls(Duration::from_millis(20), Duration::from_secs(60));
        cache.insert("u", "m", "ch-9");
        assert_eq!(cache.lookup("u", "m").as_deref(), Some("ch-9"));
        std::thread::sleep(Duration::from_millis(40));
        // 过粘性但未过硬 → 返回 None（调用方重走 HRW）
        assert_eq!(cache.lookup("u", "m"), None);
    }

    #[test]
    fn hard_ttl_removes_entry() {
        let cache = AffinityCache::with_ttls(Duration::from_millis(10), Duration::from_millis(30));
        cache.insert("u", "m", "ch-9");
        std::thread::sleep(Duration::from_millis(50));
        let _ = cache.lookup("u", "m");
        assert_eq!(cache.len(), 0, "硬 TTL 应物理删除条目");
    }

    #[test]
    #[should_panic(expected = "sticky_ttl 必须 <= hard_ttl")]
    fn ttls_must_be_ordered() {
        let _ = AffinityCache::with_ttls(Duration::from_secs(60), Duration::from_secs(30));
    }
}
