//! 内存缓存模块 — 借鉴 aisix-cache/src/memory.rs
//!
//! 基于 moka 的内存缓存，提供：
//! - TTL 过期（Time-to-Live）
//! - 容量限制（最大条目数）
//! - 线程安全的异步访问
//!
//! 典型用途：
//! - 模型列表缓存（减少 Cloudflare API 调用）
//! - API Key 验证结果缓存
//! - 会话状态缓存

use std::time::Duration;

/// 内存缓存包装器
pub struct MemoryCache<K, V>
where
    K: moka::cache::Key + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    inner: moka::future::Cache<K, V>,
}

impl<K, V> MemoryCache<K, V>
where
    K: moka::cache::Key + Send + Sync + 'static + std::fmt::Debug,
    V: Clone + Send + Sync + 'static,
{
    /// 创建新缓存
    ///
    /// # 参数
    /// - `max_capacity`: 最大条目数
    /// - `ttl`: 条目存活时间
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: moka::future::Cache::builder()
                .max_capacity(max_capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    /// 获取或插入。如果 key 不存在，调用 init 函数生成值并缓存。
    pub async fn get_or_insert(&self, key: K, init: impl std::future::Future<Output = V>) -> V {
        self.inner
            .get_with(key, init)
            .await
    }

    /// 获取缓存值
    pub async fn get(&self, key: &K) -> Option<V> {
        self.inner.get(key).await
    }

    /// 插入缓存值
    pub async fn insert(&self, key: K, value: V) {
        self.inner.insert(key, value).await;
    }

    /// 删除缓存值
    pub async fn remove(&self, key: &K) {
        self.inner.remove(key).await;
    }

    /// 清空缓存
    pub fn invalidate_all(&self) {
        self.inner.invalidate_all();
    }

    /// 返回缓存条目数（近似值）
    pub fn entry_count(&self) -> u64 {
        self.inner.entry_count()
    }
}

impl<K, V> std::fmt::Debug for MemoryCache<K, V>
where
    K: moka::cache::Key + Send + Sync + 'static + std::fmt::Debug,
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
}