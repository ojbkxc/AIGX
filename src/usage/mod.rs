use chrono::{Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::account::AccountPool;
use crate::config::UsageConfig;
use crate::storage::FileStore;

/// Token 统计
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenStats {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub requests: u64,
    pub tok_per_sec_sum: u64,
    pub tok_per_sec_count: u64,
}

impl TokenStats {
    /// 计算平均每秒 token 数
    pub fn avg_tok_per_sec(&self) -> u64 {
        self.tok_per_sec_sum.checked_div(self.tok_per_sec_count).unwrap_or(0)
        } else {
            0
        }
    }

    /// 总 token 数
    pub fn total(&self) -> u64 {
        self.input + self.output
    }
}

/// 用量追踪器
pub struct UsageTracker {
    store: Arc<FileStore>,
    #[allow(dead_code)]
    account_pool: Arc<AccountPool>,
}

impl UsageTracker {
    pub fn new(store: Arc<FileStore>, account_pool: Arc<AccountPool>) -> Self {
        Self { store, account_pool }
    }

    /// 获取今日 token 统计键名
    fn daily_key() -> String {
        let now = Utc::now();
        format!("token_daily_{:04}{:02}{:02}", now.year(), now.month(), now.day())
    }

    /// 获取月度 token 统计键名
    fn monthly_key() -> String {
        let now = Utc::now();
        format!("token_monthly_{:04}{:02}", now.year(), now.month())
    }

    /// 累加 token 统计
    pub fn accumulate(
        &self,
        input: u64,
        output: u64,
        reasoning: u64,
        cache_read: u64,
        cache_write: u64,
        duration_sec: f64,
    ) {
        // 今日统计
        let daily_key = Self::daily_key();
        let mut daily: TokenStats = self
            .store
            .get::<TokenStats>(&daily_key)
            .ok()
            .flatten()
            .unwrap_or_default();

        daily.input += input;
        daily.output += output;
        daily.reasoning += reasoning;
        daily.cache_read += cache_read;
        daily.cache_write += cache_write;
        daily.requests += 1;
        if duration_sec > 0.0 && output > 0 {
            daily.tok_per_sec_sum += (output as f64 / duration_sec).round() as u64;
            daily.tok_per_sec_count += 1;
        }

        if let Err(e) = self.store.put(&daily_key, &daily) {
            tracing::error!("Failed to persist daily token stats: {e}");
        }

        // 月度统计
        let monthly_key = Self::monthly_key();
        let mut monthly: TokenStats = self
            .store
            .get::<TokenStats>(&monthly_key)
            .ok()
            .flatten()
            .unwrap_or_default();

        monthly.input += input;
        monthly.output += output;
        monthly.reasoning += reasoning;
        monthly.requests += 1;

        if let Err(e) = self.store.put(&monthly_key, &monthly) {
            tracing::error!("Failed to persist monthly token stats: {e}");
        }
    }

    /// 获取今日统计
    pub fn today_stats(&self) -> TokenStats {
        let key = Self::daily_key();
        self.store.get::<TokenStats>(&key).ok().flatten().unwrap_or_default()
    }

    /// 获取月度统计
    pub fn monthly_stats(&self) -> TokenStats {
        let key = Self::monthly_key();
        self.store.get::<TokenStats>(&key).ok().flatten().unwrap_or_default()
    }

    /// 检查是否超过限额，返回警告信息
    #[allow(dead_code)]
    pub fn check_limits(&self, config: &UsageConfig) -> Option<String> {
        let daily = self.today_stats();
        let monthly = self.monthly_stats();

        let daily_total = daily.total();
        let monthly_total = monthly.total();

        let mut warnings = Vec::new();

        if config.daily_limit > 0 && daily_total >= config.daily_limit {
            return Some(format!(
                "Daily limit exceeded: {} / {}",
                fmt_tok(daily_total),
                fmt_limit(config.daily_limit)
            ));
        }

        if config.daily_limit > 0 && config.threshold > 0.0 {
            let daily_ratio = daily_total as f64 / config.daily_limit as f64;
            if daily_ratio >= config.threshold {
                warnings.push(format!(
                    "Daily usage at {:.1}% ({} / {})",
                    daily_ratio * 100.0,
                    fmt_tok(daily_total),
                    fmt_limit(config.daily_limit)
                ));
            }
        }

        if config.monthly_limit > 0 && monthly_total >= config.monthly_limit {
            return Some(format!(
                "Monthly limit exceeded: {} / {}",
                fmt_tok(monthly_total),
                fmt_limit(config.monthly_limit)
            ));
        }

        if config.monthly_limit > 0 && config.threshold > 0.0 {
            let monthly_ratio = monthly_total as f64 / config.monthly_limit as f64;
            if monthly_ratio >= config.threshold {
                warnings.push(format!(
                    "Monthly usage at {:.1}% ({} / {})",
                    monthly_ratio * 100.0,
                    fmt_tok(monthly_total),
                    fmt_limit(config.monthly_limit)
                ));
            }
        }

        if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("; "))
        }
    }

    /// 近 7 日消耗趋势数据
    pub fn weekly_trend(&self) -> Vec<serde_json::Value> {
        use chrono::Duration;
        let mut trend = Vec::new();
        for i in (0..7).rev() {
            let day = Utc::now() - Duration::days(i);
            let key = format!("token_daily_{:04}{:02}{:02}", day.year(), day.month(), day.day());
            let stats: TokenStats = self.store.get::<TokenStats>(&key).ok().flatten().unwrap_or_default();
            trend.push(serde_json::json!({
                "label": day.format("%m/%d").to_string(),
                "value": stats.total(),
            }));
        }
        trend
    }

    /// 模型用量统计（简化：返回空列表）
    pub fn model_usage(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }
}

/// 格式化 token 数为可读形式
#[allow(dead_code)]
pub fn fmt_tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// 格式化限额为可读形式
#[allow(dead_code)]
pub fn fmt_limit(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}