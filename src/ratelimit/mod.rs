//! 限流模块 — 借鉴 aisix-ratelimit
//!
//! 提供两阶段 RPM/TPM/并发限流器。
//! 请求中间件在分发前调用 `Limiter::pre_commit()`；
//! 返回的 `Reservation` 在上游响应完成后通过 `Reservation::commit_tokens()` 完成。
//!
//! 限制配置来自 `RateLimit` 结构。RPM/RPD 预先检查并递增，
//! 使突发流量快速失败；TPM/TPD 预先只检查，在 commit 时递增，
//! 因为 token 消耗只有在上游响应返回后才能确定。
//!
//! ## 多维度限流（功能 3 扩展）
//!
//! `RateLimiter` 包装 `Limiter`，按 per-key / per-model / per-user / per-ip
//! 四个维度分别检查。配置由 `RateLimitConfig` 描述，可通过 admin API 修改。
//! 配置持久化在 FileStore（key=`ratelimit_config`）。

pub mod clock;
pub mod error;
pub mod limiter;
pub mod store;
pub mod window;

pub use error::RateLimitError;
pub use limiter::{Limiter, Reservation};
pub use store::RateLimit;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::storage::FileStore;

// ── RateLimitConfig ────────────────────────────────────────────────

/// 多维度限流配置。
///
/// 参照 aisix 的 RateLimit + new-api middleware/rate-limit.go 的多维度设计。
/// 每个字段为 None 表示该维度不限流。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitConfig {
    /// 每 key 每分钟请求数
    #[serde(default)]
    pub per_key_rpm: Option<u64>,
    /// 每 key 每分钟 token 数
    #[serde(default)]
    pub per_key_tpm: Option<u64>,
    /// 每模型每分钟请求数（全局）
    #[serde(default)]
    pub per_model_rpm: Option<u64>,
    /// 每用户每分钟请求数
    #[serde(default)]
    pub per_user_rpm: Option<u64>,
    /// 每用户每分钟 token 数
    #[serde(default)]
    pub per_user_tpm: Option<u64>,
    /// 每 IP 每分钟请求数
    #[serde(default)]
    pub per_ip_rpm: Option<u64>,
    /// 全局每分钟请求数
    #[serde(default)]
    pub global_rpm: Option<u64>,
    /// 全局每分钟 token 数
    #[serde(default)]
    pub global_tpm: Option<u64>,
    /// 是否启用
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    false
}

impl RateLimitConfig {
    /// 是否有任何维度配置了限流
    pub fn has_any_limit(&self) -> bool {
        self.enabled
            && (self.per_key_rpm.is_some()
                || self.per_key_tpm.is_some()
                || self.per_model_rpm.is_some()
                || self.per_user_rpm.is_some()
                || self.per_user_tpm.is_some()
                || self.per_ip_rpm.is_some()
                || self.global_rpm.is_some()
                || self.global_tpm.is_some())
    }

    /// 转为 per-key RateLimit
    fn key_limits(&self) -> RateLimit {
        RateLimit {
            rps: None,
            rpm: self.per_key_rpm,
            rph: None,
            rpd: None,
            tpm: self.per_key_tpm,
            tpd: None,
            concurrency: None,
        }
    }

    /// 转为 per-model RateLimit
    fn model_limits(&self) -> RateLimit {
        RateLimit {
            rps: None,
            rpm: self.per_model_rpm,
            rph: None,
            rpd: None,
            tpm: None,
            tpd: None,
            concurrency: None,
        }
    }

    /// 转为 per-user RateLimit
    fn user_limits(&self) -> RateLimit {
        RateLimit {
            rps: None,
            rpm: self.per_user_rpm,
            rph: None,
            rpd: None,
            tpm: self.per_user_tpm,
            tpd: None,
            concurrency: None,
        }
    }

    /// 转为 per-ip RateLimit
    fn ip_limits(&self) -> RateLimit {
        RateLimit {
            rps: None,
            rpm: self.per_ip_rpm,
            rph: None,
            rpd: None,
            tpm: None,
            tpd: None,
            concurrency: None,
        }
    }

    /// 转为 global RateLimit
    fn global_limits(&self) -> RateLimit {
        RateLimit {
            rps: None,
            rpm: self.global_rpm,
            rph: None,
            rpd: None,
            tpm: self.global_tpm,
            tpd: None,
            concurrency: None,
        }
    }
}

// ── RateLimiter ────────────────────────────────────────────────────

/// 多维度限流器。
///
/// 包装底层 `Limiter`，按 key/model/user/ip 四维度分别 pre_commit。
/// 配置可热更新（update_config）。
pub struct RateLimiter {
    limiter: Limiter,
    config: RwLock<RateLimitConfig>,
    store: Option<Arc<FileStore>>,
}

const RATELIMIT_CONFIG_KEY: &str = "ratelimit_config";

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            limiter: Limiter::new(),
            config: RwLock::new(RateLimitConfig::default()),
            store: None,
        }
    }

    /// 带持久化存储构建（启动时调用，自动加载配置）
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let config = store
            .get::<RateLimitConfig>(RATELIMIT_CONFIG_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        Self {
            limiter: Limiter::new(),
            config: RwLock::new(config),
            store: Some(store),
        }
    }

    /// 获取当前配置快照
    pub fn config(&self) -> RateLimitConfig {
        self.config.read().clone()
    }

    /// 更新配置并持久化
    pub fn update_config(&self, cfg: RateLimitConfig) -> anyhow::Result<RateLimitConfig> {
        if let Some(store) = &self.store {
            store.put(RATELIMIT_CONFIG_KEY, &cfg)?;
        }
        *self.config.write() = cfg.clone();
        Ok(cfg)
    }

    /// 检查所有维度是否允许通过。
    ///
    /// 参数：
    /// - key：API Key ID
    /// - model：模型名
    /// - user：用户 ID（可能为 None）
    /// - ip：客户端 IP（可能为 None）
    ///
    /// 返回 Ok(ReservationBundle) 表示通过，调用方需在事后调用 `commit_tokens(tokens)` 完成事后记账；
    /// 返回 Err(RateLimitError) 表示超限，应返回 429。
    pub async fn check(
        &self,
        key: &str,
        model: &str,
        user: Option<&str>,
        ip: Option<&str>,
    ) -> Result<ReservationBundle, RateLimitError> {
        let cfg = self.config.read().clone();
        if !cfg.has_any_limit() {
            return Ok(ReservationBundle::empty());
        }

        let mut reservations = Vec::new();

        // per-key
        let kl = cfg.key_limits();
        if kl.rpm.is_some() || kl.tpm.is_some() {
            reservations.push(self.limiter.pre_commit(&format!("key:{key}"), &kl).await?);
        }

        // per-model
        let ml = cfg.model_limits();
        if ml.rpm.is_some() {
            reservations.push(self.limiter.pre_commit(&format!("model:{model}"), &ml).await?);
        }

        // per-user
        if let Some(u) = user {
            let ul = cfg.user_limits();
            if ul.rpm.is_some() || ul.tpm.is_some() {
                reservations.push(self.limiter.pre_commit(&format!("user:{u}"), &ul).await?);
            }
        }

        // per-ip
        if let Some(ip) = ip {
            let il = cfg.ip_limits();
            if il.rpm.is_some() {
                reservations.push(self.limiter.pre_commit(&format!("ip:{ip}"), &il).await?);
            }
        }

        // global
        let gl = cfg.global_limits();
        if gl.rpm.is_some() || gl.tpm.is_some() {
            reservations.push(self.limiter.pre_commit("global", &gl).await?);
        }

        Ok(ReservationBundle { reservations })
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// 多维度预留句柄集合。commit_tokens 时将 tokens 加到所有维度。
///
/// B05：Reservation 本身 Clone，此处派生 Clone 以便流式计费守卫
///（兜底计费路径）与后缀事件（正常结束路径）各持一份独立提交。
#[derive(Clone)]
pub struct ReservationBundle {
    reservations: Vec<Reservation>,
}

impl ReservationBundle {
    /// 空预留（限流未启用时返回）
    pub fn empty() -> Self {
        Self { reservations: Vec::new() }
    }

    /// 事后记账：将 tokens 加到所有维度的 tpm 计数器。
    pub async fn commit_tokens(self, tokens: u64) {
        for r in self.reservations {
            r.commit_tokens(tokens).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_config_allows_all() {
        let limiter = RateLimiter::new();
        let bundle = limiter.check("k1", "gpt-4", Some("u1"), Some("1.2.3.4")).await.unwrap();
        bundle.commit_tokens(100).await;
    }

    #[tokio::test]
    async fn per_key_rpm_blocks() {
        let limiter = RateLimiter::new();
        let cfg = RateLimitConfig {
            enabled: true,
            per_key_rpm: Some(2),
            ..Default::default()
        };
        limiter.update_config(cfg).unwrap();

        let _r1 = limiter.check("k1", "m", None, None).await.unwrap();
        let _r2 = limiter.check("k1", "m", None, None).await.unwrap();
        assert!(limiter.check("k1", "m", None, None).await.is_err());
    }

    #[tokio::test]
    async fn disabled_config_allows_all() {
        let limiter = RateLimiter::new();
        let cfg = RateLimitConfig {
            enabled: false,
            per_key_rpm: Some(1),
            ..Default::default()
        };
        limiter.update_config(cfg).unwrap();
        // enabled=false 时不限流
        let _ = limiter.check("k1", "m", None, None).await.unwrap();
        let _ = limiter.check("k1", "m", None, None).await.unwrap();
    }
}
