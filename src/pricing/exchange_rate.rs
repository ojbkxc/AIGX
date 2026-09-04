//! 汇率服务 — 多币种汇率转换（USD / CNY / EUR 等）。
//!
//! 参照 burncloud `crates/router/src/exchange_rate.rs`：
//! - 内存缓存 `DashMap<(Currency, Currency), CachedRate>`
//! - `convert(amount, from, to)`：直接汇率 → 反向汇率回退 → 报错
//! - 后台周期同步任务（每小时检查，超 24h 标记陈旧）
//!
//! 适配 AIGX 单 crate + FileStore：持久化用 `FileStore`（key=`exchange_rate:{from}:{to}`），
//! 不依赖 burncloud_database::Database / sqlx。

use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use crate::storage::FileStore;

/// 后台同步检查间隔（1 小时）。
const SYNC_CHECK_INTERVAL_SECS: u64 = 3600;
/// 汇率陈旧阈值（24 小时）。
const STALE_THRESHOLD_HOURS: i64 = 24;

/// 支持的币种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Usd,
    Cny,
    Eur,
    Jpy,
    Gbp,
}

impl Currency {
    /// 转三字母代码（ISO 4217）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Usd => "USD",
            Self::Cny => "CNY",
            Self::Eur => "EUR",
            Self::Jpy => "JPY",
            Self::Gbp => "GBP",
        }
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.code())
    }
}

impl FromStr for Currency {
    type Err = CurrencyParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "USD" => Ok(Self::Usd),
            "CNY" => Ok(Self::Cny),
            "EUR" => Ok(Self::Eur),
            "JPY" => Ok(Self::Jpy),
            "GBP" => Ok(Self::Gbp),
            _ => Err(CurrencyParseError(s.to_string())),
        }
    }
}

/// 币种解析错误。
#[derive(Debug, thiserror::Error)]
#[error("unknown currency: {0}")]
pub struct CurrencyParseError(pub String);

/// 缓存汇率条目（带时间戳）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedRate {
    /// 汇率（f64，from → to 的乘数）
    pub rate: f64,
    pub updated_at: DateTime<Utc>,
}

impl CachedRate {
    /// 从 f64 构造（当前时间）。
    pub fn from_rate(rate: f64) -> Self {
        Self {
            rate,
            updated_at: Utc::now(),
        }
    }
}

/// 持久化用记录（与 `CachedRate` 同形，单独类型便于演进 schema）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRate {
    from: String,
    to: String,
    rate: f64,
    updated_at: DateTime<Utc>,
}

/// 汇率服务 — 管理多币种汇率与转换。
pub struct ExchangeRateService {
    /// 内存缓存：(from, to) → rate
    rates: DashMap<(Currency, Currency), CachedRate>,
    store: Option<Arc<FileStore>>,
}

const EXCHANGE_RATE_KEY: &str = "exchange_rates";

impl ExchangeRateService {
    /// 构造纯内存服务（无持久化）。
    pub fn new() -> Self {
        Self {
            rates: DashMap::new(),
            store: None,
        }
    }

    /// 带 FileStore 持久化构造（启动时自动加载）。
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let s = Self {
            rates: DashMap::new(),
            store: Some(store),
        };
        let _ = s.load_from_store();
        s
    }

    /// 转换金额。
    ///
    /// - 同币种 → 原值
    /// - 直接汇率 → amount × rate
    /// - 反向汇率 → amount / reverse_rate（若 > 0）
    /// - 都没有 → Err
    pub fn convert(&self, amount: f64, from: Currency, to: Currency) -> anyhow::Result<f64> {
        if from == to {
            return Ok(amount);
        }
        if let Some(rate) = self.get_rate(from, to) {
            return Ok(amount * rate);
        }
        // 反向汇率回退
        if let Some(reverse_rate) = self.get_rate(to, from) {
            if reverse_rate > 0.0 {
                return Ok(amount / reverse_rate);
            }
            return Err(anyhow::anyhow!("反向汇率无效（<=0）: {} -> {}", to, from));
        }
        Err(anyhow::anyhow!("未配置汇率: {} -> {}", from, to))
    }

    /// 取直接汇率（同币种返回 1.0）。
    pub fn get_rate(&self, from: Currency, to: Currency) -> Option<f64> {
        if from == to {
            return Some(1.0);
        }
        self.rates.get(&(from, to)).map(|r| r.rate)
    }

    /// 设置汇率（仅内存）。
    pub fn set_rate(&self, from: Currency, to: Currency, rate: f64) {
        self.rates.insert((from, to), CachedRate::from_rate(rate));
    }

    /// 设置汇率并持久化。
    pub fn set_rate_persisted(
        &self,
        from: Currency,
        to: Currency,
        rate: f64,
    ) -> anyhow::Result<()> {
        self.set_rate(from, to, rate);
        self.persist_all()
    }

    /// 从 FileStore 加载全部汇率到缓存。
    pub fn load_from_store(&self) -> anyhow::Result<usize> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(0),
        };
        let all: Vec<PersistedRate> = store
            .get::<Vec<PersistedRate>>(EXCHANGE_RATE_KEY)?
            .unwrap_or_default();
        let mut count = 0;
        for r in all {
            if let (Ok(from), Ok(to)) = (Currency::from_str(&r.from), Currency::from_str(&r.to)) {
                self.rates.insert(
                    (from, to),
                    CachedRate {
                        rate: r.rate,
                        updated_at: r.updated_at,
                    },
                );
                count += 1;
            }
        }
        tracing::info!("从存储加载了 {count} 条汇率");
        Ok(count)
    }

    /// 把全部缓存汇率持久化到 FileStore。
    pub fn persist_all(&self) -> anyhow::Result<()> {
        let store = match &self.store {
            Some(s) => s,
            None => return Ok(()),
        };
        let all: Vec<PersistedRate> = self
            .rates
            .iter()
            .map(|e| {
                let key = e.key();
                PersistedRate {
                    from: key.0.code().to_string(),
                    to: key.1.code().to_string(),
                    rate: e.rate,
                    updated_at: e.updated_at,
                }
            })
            .collect();
        store.put(EXCHANGE_RATE_KEY, &all)?;
        Ok(())
    }

    /// 列出全部缓存汇率。
    pub fn list_rates(&self) -> Vec<(Currency, Currency, f64, DateTime<Utc>)> {
        self.rates
            .iter()
            .map(|e| (e.key().0, e.key().1, e.rate, e.updated_at))
            .collect()
    }

    /// 清空缓存。
    pub fn clear_cache(&self) {
        self.rates.clear();
    }

    /// 取指定汇率最近更新时间。
    pub fn get_last_updated(&self, from: Currency, to: Currency) -> Option<DateTime<Utc>> {
        self.rates.get(&(from, to)).map(|r| r.updated_at)
    }

    /// 是否有汇率已陈旧（超 24h）。
    pub fn has_stale_rates(&self) -> bool {
        let now = Utc::now();
        self.rates.iter().any(|e| {
            let age = now.signed_duration_since(e.updated_at);
            age.num_hours() >= STALE_THRESHOLD_HOURS
        })
    }

    /// 启动后台周期同步任务（每小时检查）。
    ///
    /// 周期从存储重载汇率；若陈旧则记录日志（外部 API 刷新由调用方按需触发）。
    pub fn start_sync_task(self: Arc<Self>) {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(SYNC_CHECK_INTERVAL_SECS));
            loop {
                interval.tick().await;
                if let Err(e) = self.load_from_store() {
                    tracing::warn!("从存储加载汇率失败: {e}");
                }
                if self.has_stale_rates() {
                    tracing::info!(
                        "汇率已陈旧（>{}h），建议通过管理 API 或外部 API 刷新",
                        STALE_THRESHOLD_HOURS
                    );
                }
            }
        });
    }
}

impl Default for ExchangeRateService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn service() -> ExchangeRateService {
        ExchangeRateService::new()
    }

    fn persisted_service() -> ExchangeRateService {
        ExchangeRateService::with_store(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn convert_same_currency() {
        let s = service();
        let amount = s.convert(100.0, Currency::Usd, Currency::Usd).unwrap();
        assert!((amount - 100.0).abs() < 1e-9);
    }

    #[test]
    fn set_and_get_rate() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        assert_eq!(s.get_rate(Currency::Usd, Currency::Cny), Some(7.2));
        // 反向未设置
        assert_eq!(s.get_rate(Currency::Cny, Currency::Usd), None);
    }

    #[test]
    fn convert_with_direct_rate() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        let amount = s.convert(100.0, Currency::Usd, Currency::Cny).unwrap();
        assert!((amount - 720.0).abs() < 1e-9);
    }

    #[test]
    fn convert_with_reverse_rate_fallback() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        // 反向转换：720 CNY → 100 USD（用反向汇率 1/7.2）
        let amount = s.convert(720.0, Currency::Cny, Currency::Usd).unwrap();
        assert!((amount - 100.0).abs() < 1e-9);
    }

    #[test]
    fn convert_missing_rate_errors() {
        let s = service();
        assert!(s.convert(100.0, Currency::Usd, Currency::Eur).is_err());
    }

    #[test]
    fn list_rates_returns_all() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        s.set_rate(Currency::Eur, Currency::Usd, 1.08);
        assert_eq!(s.list_rates().len(), 2);
    }

    #[test]
    fn clear_cache_removes_all() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        assert!(s.get_rate(Currency::Usd, Currency::Cny).is_some());
        s.clear_cache();
        assert!(s.get_rate(Currency::Usd, Currency::Cny).is_none());
    }

    #[test]
    fn get_last_updated_tracks_set() {
        let s = service();
        assert!(s.get_last_updated(Currency::Usd, Currency::Cny).is_none());
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        assert!(s.get_last_updated(Currency::Usd, Currency::Cny).is_some());
    }

    #[test]
    fn multiple_currencies_convert_independently() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        s.set_rate(Currency::Usd, Currency::Eur, 0.93);
        s.set_rate(Currency::Eur, Currency::Cny, 7.75);

        assert!((s.convert(100.0, Currency::Usd, Currency::Cny).unwrap() - 720.0).abs() < 1e-9);
        assert!((s.convert(100.0, Currency::Usd, Currency::Eur).unwrap() - 93.0).abs() < 1e-9);
        assert!((s.convert(100.0, Currency::Eur, Currency::Cny).unwrap() - 775.0).abs() < 1e-9);
    }

    #[test]
    fn zero_amount_conversion() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        let amount = s.convert(0.0, Currency::Usd, Currency::Cny).unwrap();
        assert!((amount - 0.0).abs() < 1e-9);
    }

    #[test]
    fn currency_from_str_parses_case_insensitive() {
        assert_eq!(Currency::from_str("usd").unwrap(), Currency::Usd);
        assert_eq!(Currency::from_str("CNY").unwrap(), Currency::Cny);
        assert_eq!(Currency::from_str("eur").unwrap(), Currency::Eur);
        assert!(Currency::from_str("XXX").is_err());
    }

    #[test]
    fn currency_code_round_trip() {
        for c in [
            Currency::Usd,
            Currency::Cny,
            Currency::Eur,
            Currency::Jpy,
            Currency::Gbp,
        ] {
            let parsed = Currency::from_str(c.code()).unwrap();
            assert_eq!(parsed, c);
        }
    }

    #[test]
    fn persisted_service_loads_on_construct() {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
        // 第一次构造 + 设置 + 持久化
        {
            let s = ExchangeRateService::with_store(store.clone());
            s.set_rate(Currency::Usd, Currency::Cny, 7.2);
            s.persist_all().unwrap();
        }
        // 第二次构造应自动加载
        let s2 = ExchangeRateService::with_store(store);
        assert_eq!(s2.get_rate(Currency::Usd, Currency::Cny), Some(7.2));
    }

    #[test]
    fn set_rate_persisted_round_trips() {
        let s = persisted_service();
        s.set_rate_persisted(Currency::Usd, Currency::Eur, 0.93)
            .unwrap();
        assert_eq!(s.get_rate(Currency::Usd, Currency::Eur), Some(0.93));
        // 重新加载应仍存在
        s.load_from_store().unwrap();
        assert_eq!(s.get_rate(Currency::Usd, Currency::Eur), Some(0.93));
    }

    #[test]
    fn has_stale_rates_detects_old_entries() {
        let s = service();
        // 手动插入一条陈旧汇率
        s.rates.insert(
            (Currency::Usd, Currency::Cny),
            CachedRate {
                rate: 7.2,
                updated_at: Utc::now() - chrono::Duration::hours(48),
            },
        );
        assert!(s.has_stale_rates());
    }

    #[test]
    fn has_stale_rates_false_for_fresh() {
        let s = service();
        s.set_rate(Currency::Usd, Currency::Cny, 7.2);
        assert!(!s.has_stale_rates());
    }
}
