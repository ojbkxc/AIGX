//! 渠道健康档案（C1）——每渠道最近 30 天的成功率 / P95 延迟 / 熔断次数。
//!
//! 与 `health_manager`（实时内存状态、调度用）互补：本模块负责
//! **跨进程存活**的健康趋势——内存累加器按天聚合，周期 flush 到
//! FileStore（key `health_archive:{channel_id}:{day}`），只保留 30 天。
//!
//! ## 设计要点
//! - **热路径零落盘**：`note_*` 只写 DashMap 内存累加器；cron 每 5 分钟
//!   flush 一次（丢失上限 = 一个采样周期，运营趋势可接受）。
//! - **延迟 P95 近似**：存 13 桶对数直方图（16ms 起步 ×2 递增），查询时
//!   线性插值估算 P95——比只存均值更能反映尾延迟，又不为每个请求留全样本。
//! - **熔断次数**：由调用方（`ChannelStore::record_channel_failure`）对比
//!   断路器状态变化后 `note_trip`，语义 = 「进入 open 的次数」。
//! - **重启恢复**：首次访问某渠道时从 FileStore 恢复当日快照继续累加。
//!
//! 百年工程红线：不引 histogram/prometheus-client 等 crate；对数桶 +
//! 线性插值是够用且可替换的保守选择（若未来需要真 P95，替换本模块即可，
//! 不污染调度热路径）。

use chrono::Datelike;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::storage::FileStore;

/// 延迟直方图桶数：13 桶覆盖 0ms → 无限（16ms 起步，指数 ×2）。
const BUCKET_COUNT: usize = 13;
/// 桶下界（毫秒）。桶 i 覆盖 `[LO[i], LO[i+1])`，最后一桶 `[16384, ∞)`。
const BUCKET_LO: [u64; BUCKET_COUNT] = [
    0,
    16,
    32,
    64,
    128,
    256,
    512,
    1024,
    2048,
    4096,
    8192,
    16384,
    u64::MAX,
];

/// 档案保留天数。
const RETENTION_DAYS: i64 = 30;

/// 归档的 key 前缀。
const ARCHIVE_PREFIX: &str = "health_archive:";

/// 渠道健康档案（C1）。
///
/// 线程安全：`live` 用 DashMap 分片锁，热路径每次请求一个 entry 操作。
pub struct HealthArchive {
    /// channel_id → 当日累加器（含延迟直方图）。
    live: DashMap<String, DayAccumulator>,
    /// 底层 KV（FileStore，零配置）。
    store: Arc<FileStore>,
}

/// 当日累加器（内存形态）。`day` 用 `num_days_from_ce`，跨日即归零重开。
#[derive(Debug, Clone)]
struct DayAccumulator {
    day: i64,
    success: u64,
    failure: u64,
    trips: u64,
    /// 延迟对数直方图（成功请求）。
    buckets: [u64; BUCKET_COUNT],
    last_error: Option<String>,
}

impl DayAccumulator {
    fn new(day: i64) -> Self {
        Self {
            day,
            success: 0,
            failure: 0,
            trips: 0,
            buckets: [0; BUCKET_COUNT],
            last_error: None,
        }
    }

    fn from_snapshot(s: DaySnapshot) -> Self {
        Self {
            day: s.day,
            success: s.success,
            failure: s.failure,
            trips: s.trips,
            buckets: s.buckets,
            last_error: s.last_error,
        }
    }
}

/// 当日快照（落盘 / 查询形态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaySnapshot {
    pub day: i64,
    pub success: u64,
    pub failure: u64,
    pub trips: u64,
    pub buckets: [u64; BUCKET_COUNT],
    pub last_error: Option<String>,
}

impl DaySnapshot {
    /// 当日成功率（无请求时返回 0）。
    pub fn success_rate(&self) -> f64 {
        let total = self.success + self.failure;
        if total == 0 {
            0.0
        } else {
            self.success as f64 / total as f64
        }
    }

    /// P95 延迟估算（毫秒）：对数桶内线性插值。
    ///
    /// 桶内均匀假设是近似（文档已注明），零样本返回 0。
    pub fn p95_ms(&self) -> u64 {
        let total: u64 = self.buckets.iter().sum();
        if total == 0 {
            return 0;
        }
        let rank = ((total as f64) * 0.95).ceil() as u64;
        let mut seen: u64 = 0;
        for (i, count) in self.buckets.iter().enumerate() {
            seen += count;
            if seen >= rank {
                let rank_in_bucket = rank - (seen - count);
                let lo = BUCKET_LO[i];
                let hi = if i + 1 < BUCKET_COUNT {
                    BUCKET_LO[i + 1]
                } else {
                    // 最后一桶 [16384, ∞)：返回下界（上界未定义）。
                    return lo;
                };
                let width = hi - lo;
                return lo + (rank_in_bucket as f64 / *count as f64 * width as f64) as u64;
            }
        }
        0
    }
}

fn today() -> i64 {
    chrono::Utc::now().date_naive().num_days_from_ce() as i64
}

fn archive_key(channel_id: &str, day: i64) -> String {
    format!("{ARCHIVE_PREFIX}{channel_id}:{day}")
}

impl HealthArchive {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            live: DashMap::new(),
            store,
        }
    }

    /// 取当日累加器（无则尝试从落盘快照恢复；恢复失败按零值开始）。
    fn live_entry(
        &self,
        channel_id: &str,
    ) -> dashmap::mapref::one::RefMut<'_, String, DayAccumulator> {
        let day = today();
        // 先尝试已有 entry（跨日滚动）
        let mut entry = self.live.entry(channel_id.to_string()).or_insert_with(|| {
            // 恢复当日已落盘快照（重启续跑）
            let restored = self
                .store
                .get::<DaySnapshot>(&archive_key(channel_id, day))
                .ok()
                .flatten()
                .map(DayAccumulator::from_snapshot);
            match restored {
                Some(acc) if acc.day == day => acc,
                _ => DayAccumulator::new(day),
            }
        });
        if entry.day != day {
            // 跨日：先 flush 旧日快照（尽力而为），再开新累加器。
            let old = entry.clone();
            let _ = self.store.put(
                &archive_key(channel_id, old.day),
                &DaySnapshot {
                    day: old.day,
                    success: old.success,
                    failure: old.failure,
                    trips: old.trips,
                    buckets: old.buckets,
                    last_error: old.last_error.clone(),
                },
            );
            *entry = DayAccumulator::new(day);
        }
        entry
    }

    /// 记入一次成功请求（延迟进直方图）。
    pub fn note_success(&self, channel_id: &str, latency_ms: u64) {
        let mut entry = self.live_entry(channel_id);
        entry.success = entry.success.saturating_add(1);
        let bucket = bucket_for(latency_ms);
        entry.buckets[bucket] = entry.buckets[bucket].saturating_add(1);
    }

    /// 记入一次失败请求。
    pub fn note_failure(&self, channel_id: &str, error_message: &str) {
        let mut entry = self.live_entry(channel_id);
        entry.failure = entry.failure.saturating_add(1);
        entry.last_error = Some(error_message.to_string());
    }

    /// 记入一次断路器进入 open（由调用方在状态迁移时调用）。
    pub fn note_trip(&self, channel_id: &str) {
        let mut entry = self.live_entry(channel_id);
        entry.trips = entry.trips.saturating_add(1);
    }

    /// 只补延迟样本（流式完成路径用，不重复计成功次数）。
    ///
    /// 流式请求在「建流成功」时已 `note_success`（清零断路器），
    /// 但那时只有建流时延；完整流结束后用真实总时延补一份延迟样本。
    pub fn note_latency(&self, channel_id: &str, latency_ms: u64) {
        let mut entry = self.live_entry(channel_id);
        let bucket = bucket_for(latency_ms);
        entry.buckets[bucket] = entry.buckets[bucket].saturating_add(1);
    }

    /// 把所有内存累加器 flush 到 FileStore。返回写入的渠道数。
    pub fn flush(&self) -> u64 {
        let mut written = 0u64;
        for r in self.live.iter() {
            let (channel_id, acc) = (r.key(), r.value());
            let snapshot = DaySnapshot {
                day: acc.day,
                success: acc.success,
                failure: acc.failure,
                trips: acc.trips,
                buckets: acc.buckets,
                last_error: acc.last_error.clone(),
            };
            if self
                .store
                .put(&archive_key(channel_id, snapshot.day), &snapshot)
                .is_ok()
            {
                written += 1;
            }
        }
        written
    }

    /// 清理超过 30 天保留期的档案。返回删除的条数。
    pub fn prune(&self) -> u64 {
        let cutoff = today() - RETENTION_DAYS;
        let mut removed = 0u64;
        let keys = match self.store.list(ARCHIVE_PREFIX) {
            Ok(k) => k,
            Err(_) => return 0,
        };
        for key in keys {
            // key 形态: health_archive:{channel_id}:{day}
            let day: i64 = match key.rsplit(':').next().and_then(|d| d.parse().ok()) {
                Some(d) => d,
                None => continue,
            };
            if day < cutoff {
                if self.store.delete(&key).is_ok() {
                    removed += 1;
                }
            }
        }
        removed
    }

    /// 查询某渠道最近 `days` 天的健康档案（含当日内存态，升序）。
    pub fn query(&self, channel_id: &str, days: u32) -> Vec<DaySnapshot> {
        let today = today();
        let mut out = Vec::new();
        for day in (today - days as i64 + 1)..=today {
            let snapshot = if let Some(acc) = self.live.get(channel_id) {
                if acc.day == day {
                    Some(DaySnapshot {
                        day: acc.day,
                        success: acc.success,
                        failure: acc.failure,
                        trips: acc.trips,
                        buckets: acc.buckets,
                        last_error: acc.last_error.clone(),
                    })
                } else {
                    self.store
                        .get::<DaySnapshot>(&archive_key(channel_id, day))
                        .ok()
                        .flatten()
                }
            } else {
                self.store
                    .get::<DaySnapshot>(&archive_key(channel_id, day))
                    .ok()
                    .flatten()
            };
            if let Some(s) = snapshot {
                out.push(s);
            }
        }
        out
    }
}

/// 延迟 → 直方图桶索引。
fn bucket_for(latency_ms: u64) -> usize {
    // 桶 i 覆盖 [LO[i], LO[i+1])；最后一桶 [16384, ∞)。
    for i in 0..BUCKET_COUNT - 1 {
        if latency_ms < BUCKET_LO[i + 1] {
            return i;
        }
    }
    BUCKET_COUNT - 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> HealthArchive {
        HealthArchive::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn bucket_boundaries() {
        assert_eq!(bucket_for(0), 0);
        assert_eq!(bucket_for(15), 0);
        assert_eq!(bucket_for(16), 1);
        assert_eq!(bucket_for(16383), 11);
        assert_eq!(bucket_for(16384), 12);
        assert_eq!(bucket_for(u64::MAX), 12);
    }

    #[test]
    fn p95_interpolation_basic() {
        // 100 个样本：50 个 10ms（桶0），50 个 100ms（桶3 [64,128)）。
        let mut s = DaySnapshot {
            day: 1,
            success: 100,
            failure: 0,
            trips: 0,
            buckets: [0; BUCKET_COUNT],
            last_error: None,
        };
        s.buckets[0] = 50;
        s.buckets[3] = 50;
        // rank=95 落在桶3，桶内第 45/50 → 64 + 45/50*64 = 121.6 → 121
        assert_eq!(s.p95_ms(), 121);
    }

    #[test]
    fn p95_zero_samples_is_zero() {
        let s = DaySnapshot {
            day: 1,
            success: 0,
            failure: 0,
            trips: 0,
            buckets: [0; BUCKET_COUNT],
            last_error: None,
        };
        assert_eq!(s.p95_ms(), 0);
        assert_eq!(s.success_rate(), 0.0);
    }

    #[test]
    fn note_success_failure_and_query() {
        let a = store();
        a.note_success("c1", 50);
        a.note_success("c1", 10);
        a.note_failure("c1", "boom");
        a.note_trip("c1");
        let snap = a.query("c1", 1);
        assert_eq!(snap.len(), 1);
        let s = &snap[0];
        assert_eq!(s.success, 2);
        assert_eq!(s.failure, 1);
        assert_eq!(s.trips, 1);
        assert_eq!(s.last_error.as_deref(), Some("boom"));
        assert!((s.success_rate() - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn flush_and_restart_recovers_today() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        {
            let a = HealthArchive::new(Arc::new(FileStore::new(path.clone())));
            a.note_success("c1", 20);
            assert_eq!(a.flush(), 1);
        }
        // 模拟进程重启：新实例应从落盘恢复当日快照继续累加
        let a2 = HealthArchive::new(Arc::new(FileStore::new(path)));
        a2.note_success("c1", 30);
        let snap = a2.query("c1", 1);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].success, 2, "restart must continue accumulating");
    }

    #[test]
    fn prune_removes_stale_days_only() {
        let a = store();
        let old_day = today() - 40;
        let _ = a.store.put(
            &archive_key("c1", old_day),
            &DaySnapshot {
                day: old_day,
                success: 1,
                failure: 0,
                trips: 0,
                buckets: [0; BUCKET_COUNT],
                last_error: None,
            },
        );
        a.note_success("c1", 10);
        assert_eq!(a.prune(), 1);
        assert!(a.query("c1", 30).iter().all(|s| s.day >= today() - 30 + 1));
    }
}
