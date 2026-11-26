//! 内存缓存模块 — 基于 dashmap 的 AsyncCache + MemoryCache
//!
//! 提供：
//! - TTL 过期（Time-to-Live）
//! - 容量限制（最大条目数）
//! - 线程安全的异步访问
//!
//! `AsyncCache` 模拟 `moka::future::Cache` 的常用 API（builder 链式 + async get/insert/get_with），
//! 作为 moka 在离线环境不可得时的轻量替代。`MemoryCache` 是对 `AsyncCache` 的薄包装，
//! 保持原有公开 API 不变。
//!
//! 典型用途：
//! - 模型列表缓存（减少 Cloudflare API 调用）
//! - API Key 验证结果缓存
//! - 会话状态缓存
//! - per-IP 速率限制计数

use dashmap::DashMap;
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 满容量插入时抽样清理过期项的条数上限（O(1) 有界，替代旧的全表 retain）。
const EXPIRED_SWEEP_SAMPLE: usize = 16;

// ── AsyncCacheBuilder ───────────────────────────────────────────────

/// 异步缓存 builder（模拟 `moka::future::Cache::builder`）。
///
/// 链式调用 `max_capacity` / `time_to_live` 后用 `build` 构建缓存。
pub struct AsyncCacheBuilder {
    max_capacity: u64,
    ttl: Duration,
}

impl Default for AsyncCacheBuilder {
    fn default() -> Self {
        Self {
            max_capacity: 0,
            ttl: Duration::MAX,
        }
    }
}

impl AsyncCacheBuilder {
    /// 设置最大条目数。`0` 表示无限制（与 moka 语义一致）。
    #[must_use]
    pub fn max_capacity(mut self, capacity: u64) -> Self {
        self.max_capacity = capacity;
        self
    }

    /// 设置条目存活时间（TTL）。
    #[must_use]
    pub fn time_to_live(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// 构建缓存。
    pub fn build<K, V>(self) -> AsyncCache<K, V>
    where
        K: Hash + Eq + Send + Sync + 'static,
        V: Clone + Send + Sync + 'static,
    {
        AsyncCache {
            inner: DashMap::new(),
            ttl: self.ttl,
            max_capacity: self.max_capacity,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }
}

// ── AsyncCache ──────────────────────────────────────────────────────

/// 基于 `dashmap::DashMap` 的异步缓存，模拟 `moka::future::Cache` 的常用 API。
///
/// 每个条目存储 `(value, inserted_at)`，`get` 时懒惰检查 TTL。
/// 容量驱逐策略：`insert` 时若超 `max_capacity`，先清过期项，仍超则移除任意一项
///（dashmap 无序，近似驱逐，非严格 LRU）。
pub struct AsyncCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    inner: DashMap<K, (V, Instant)>,
    ttl: Duration,
    max_capacity: u64,
    /// 命中次数（用于缓存统计面板的命中率）
    hits: AtomicU64,
    /// 未命中次数（含过期/不存在）
    misses: AtomicU64,
}

impl<K, V> AsyncCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// 创建 builder。
    pub fn builder() -> AsyncCacheBuilder {
        AsyncCacheBuilder::default()
    }

    /// 判断条目是否过期。`ttl == Duration::MAX` 视为永不过期。
    fn is_expired(inserted_at: Instant, ttl: Duration) -> bool {
        ttl != Duration::MAX && inserted_at.elapsed() >= ttl
    }

    /// 获取缓存值。过期则返回 `None` 并移除该条目。
    pub async fn get(&self, key: &K) -> Option<V> {
        if let Some(entry) = self.inner.get(key) {
            let (value, inserted_at) = entry.value();
            if Self::is_expired(*inserted_at, self.ttl) {
                drop(entry);
                self.inner.remove(key);
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            self.hits.fetch_add(1, Ordering::Relaxed);
            Some(value.clone())
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
            None
        }
    }

    /// 插入缓存值。超 `max_capacity` 时先做**有界抽样**清过期项，仍超则
    /// O(1) 移除任意一项（近似驱逐，非严格 LRU）。
    pub async fn insert(&self, key: K, value: V)
    where
        K: Clone,
    {
        // 仅当 key 不存在且已达容量上限时才驱逐，避免覆盖已有 key 时误删
        if self.max_capacity > 0
            && !self.inner.contains_key(&key)
            && self.inner.len() >= self.max_capacity as usize
        {
            self.sweep_expired_sample();
            if self.inner.len() >= self.max_capacity as usize {
                self.evict_one_arbitrary();
            }
        }
        self.inner.insert(key, (value, Instant::now()));
    }

    /// 有界抽样清理过期项（最多扫 `EXPIRED_SWEEP_SAMPLE` 条，O(1) 有界）。
    ///
    /// 旧实现 `retain` 全表清（O(n) 且逐个分片加写锁），满容量时每次 insert
    /// 全扫会成为热点。这里只抽样一小批；未扫到的过期项仍由 `get` 的懒惰
    /// 检查兜底 —— TTL 语义不变（到期值绝不返回）。
    fn sweep_expired_sample(&self)
    where
        K: Clone,
    {
        if self.ttl == Duration::MAX {
            return;
        }
        let now = Instant::now();
        let ttl = self.ttl;
        let mut victims: Vec<K> = Vec::new();
        for entry in self.inner.iter().take(EXPIRED_SWEEP_SAMPLE) {
            if now.duration_since(entry.value().1) >= ttl {
                victims.push(entry.key().clone());
            }
        }
        // 先放开迭代器（Ref 持分片读锁）再 remove，避免同分片重入死锁。
        for key in &victims {
            self.inner.remove(key);
        }
    }

    /// 移除任意一项：取迭代器首条，O(1) 有界（受分片数上界）。
    /// 拿 key 后立刻松开 `Ref` 再 remove，避免同分片重入死锁。
    fn evict_one_arbitrary(&self)
    where
        K: Clone,
    {
        let victim = self.inner.iter().next().map(|e| e.key().clone());
        if let Some(key) = victim {
            self.inner.remove(&key);
        }
    }

    /// 获取或插入。若 key 不存在或已过期，执行 `init` future 生成值并缓存。
    ///
    /// 注意：与 moka 不同，并发重复调用时 `init` 可能被多个 future 同时执行
    ///（moka 会去重只执行一次）。缓存场景下此差异无害。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub async fn get_with(&self, key: K, init: impl std::future::Future<Output = V>) -> V
    where
        K: Clone,
    {
        // 先查未过期
        if let Some(entry) = self.inner.get(&key) {
            let (value, inserted_at) = entry.value();
            if !Self::is_expired(*inserted_at, self.ttl) {
                return value.clone();
            }
        }
        // 已过期或不存在，执行 init 并插入
        let value = init.await;
        self.insert(key, value.clone()).await;
        value
    }

    /// 删除缓存值。
    pub async fn remove(&self, key: &K) {
        self.inner.remove(key);
    }

    /// 清空缓存。
    pub fn invalidate_all(&self) {
        self.inner.clear();
    }

    /// 返回缓存条目数（近似值，含可能已过期但尚未懒惰清理的项）。
    pub fn entry_count(&self) -> u64 {
        self.inner.len() as u64
    }

    /// 命中 / 未命中计数（缓存统计面板用）。
    pub fn hit_miss(&self) -> (u64, u64) {
        (
            self.hits.load(Ordering::Relaxed),
            self.misses.load(Ordering::Relaxed),
        )
    }

    /// 命中率（0.0~1.0；无任何访问时为 0）。
    pub fn hit_rate(&self) -> f64 {
        let (hits, misses) = self.hit_miss();
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }

    /// 最大容量（0 = 无限制）。
    pub fn max_capacity(&self) -> u64 {
        self.max_capacity
    }

    /// TTL 秒数（Duration::MAX 时为 0，表示永不过期）。
    pub fn ttl_secs(&self) -> u64 {
        if self.ttl == Duration::MAX {
            0
        } else {
            self.ttl.as_secs()
        }
    }
}

impl<K, V> std::fmt::Debug for AsyncCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static + std::fmt::Debug,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncCache")
            .field("entry_count", &self.entry_count())
            .field("ttl", &self.ttl)
            .field("max_capacity", &self.max_capacity)
            .finish()
    }
}

// ── MemoryCache：对 AsyncCache 的薄包装，保持原有公开 API 不变 ──────

/// 内存缓存包装器
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct MemoryCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    inner: AsyncCache<K, V>,
}

#[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
impl<K, V> MemoryCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static + std::fmt::Debug,
    V: Clone + Send + Sync + 'static,
{
    /// 创建新缓存
    ///
    /// # 参数
    /// - `max_capacity`: 最大条目数
    /// - `ttl`: 条目存活时间
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: AsyncCache::<K, V>::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    /// 获取或插入。如果 key 不存在，调用 init 函数生成值并缓存。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub async fn get_or_insert(&self, key: K, init: impl std::future::Future<Output = V>) -> V
    where
        K: Clone,
    {
        self.inner.get_with(key, init).await
    }

    /// 获取缓存值
    pub async fn get(&self, key: &K) -> Option<V> {
        self.inner.get(key).await
    }

    /// 插入缓存值
    pub async fn insert(&self, key: K, value: V)
    where
        K: Clone,
    {
        self.inner.insert(key, value).await;
    }

    /// 删除缓存值
    pub async fn remove(&self, key: &K) {
        self.inner.remove(key).await;
    }

    /// 清空缓存
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }

    /// 返回缓存条目数（近似值）
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }

    /// 命中率（0.0~1.0）
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn hit_rate(&self) -> f64 {
        self.inner.hit_rate()
    }
}

impl<K, V> std::fmt::Debug for MemoryCache<K, V>
where
    K: Hash + Eq + Send + Sync + 'static + std::fmt::Debug,
    V: Clone + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryCache")
            .field("entry_count", &self.entry_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_get_or_insert_caches_value() {
        let cache = MemoryCache::new(100, Duration::from_secs(60));
        let call_count = Arc::new(AtomicU32::new(0));

        let cnt = call_count.clone();
        let v1 = cache
            .get_or_insert("key1", async move {
                cnt.fetch_add(1, Ordering::SeqCst);
                "value1".to_string()
            })
            .await;
        assert_eq!(v1, "value1");

        let v2 = cache.get(&"key1").await;
        assert_eq!(v2, Some("value1".to_string()));
        // init 函数只调用一次
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_remove() {
        let cache = MemoryCache::new(100, Duration::from_secs(60));
        cache.insert("key1", "value1".to_string()).await;
        assert!(cache.get(&"key1").await.is_some());

        cache.remove(&"key1").await;
        assert!(cache.get(&"key1").await.is_none());
    }

    #[tokio::test]
    async fn test_async_cache_builder_and_ttl() {
        // 验证 AsyncCache builder + TTL 过期
        let cache: AsyncCache<String, u32> = AsyncCache::<String, u32>::builder()
            .max_capacity(10)
            .time_to_live(Duration::from_millis(50))
            .build();

        cache.insert("k".to_string(), 42).await;
        assert_eq!(cache.get(&"k".to_string()).await, Some(42));

        // 等待过期
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(cache.get(&"k".to_string()).await, None);
    }

    #[tokio::test]
    async fn test_async_cache_capacity_eviction() {
        // max_capacity=2，插入 3 个不同 key 应触发驱逐
        let cache: AsyncCache<u32, u32> = AsyncCache::<u32, u32>::builder()
            .max_capacity(2)
            .time_to_live(Duration::MAX)
            .build();

        cache.insert(1, 10).await;
        cache.insert(2, 20).await;
        cache.insert(3, 30).await;
        // 容量上限 2，条目数不应超过 2（驱逐后）
        assert!(cache.entry_count() <= 2);
    }

    #[tokio::test]
    async fn test_async_cache_invalidate_all() {
        let cache: AsyncCache<u32, u32> = AsyncCache::<u32, u32>::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(60))
            .build();

        cache.insert(1, 10).await;
        cache.insert(2, 20).await;
        assert_eq!(cache.entry_count(), 2);

        cache.invalidate_all();
        assert_eq!(cache.entry_count(), 0);
    }
}
