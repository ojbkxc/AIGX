//! 价格同步 — 定期从上游拉取最新模型定价并同步到本地 `PricingStore`。
//!
//! 参照 burncloud `crates/router/src/price_sync.rs` 的多源优先级设计：
//! 1. 本地覆盖配置（`pricing.override.json`，最高优先级）
//! 2. 远程定价仓库（GitHub 主 + Gitee 镜像回退）
//!    - 启动快路径：本地已有定价且非强制 → 跳过远程
//!    - 周期同步（forced=true）：始终拉取远程
//!
//! 适配 AIGX 单 crate + FileStore 架构：远程 JSON 解析为 `RemotePricingConfig`，
//! 逐条 upsert 到 `PricingStore`（不引入 burncloud 的多币种/分层定价复杂度）。

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::ModelPrice;
use super::PricingStore;

/// HTTP 客户端超时（秒）。
const HTTP_CLIENT_TIMEOUT_SECS: u64 = 30;
/// 默认远程同步间隔（秒，24 小时）。
pub const DEFAULT_REMOTE_SYNC_INTERVAL_SECS: u64 = 86400;

/// AIGX 官方定价仓库（GitHub raw）。
pub const AIGX_PRICES_URL: &str =
    "https://raw.githubusercontent.com/AIGX/pricing_data/main/pricing.json";
/// Gitee 镜像（CN 环境回退）。
pub const AIGX_PRICES_URL_GITEE: &str = "https://gitee.com/AIGX/pricing_data/raw/main/pricing.json";

/// 同步结果统计。
#[derive(Debug, Clone, Default)]
pub struct SyncResult {
    /// 已同步模型数
    pub models_synced: usize,
    /// 失败模型数
    pub errors: usize,
    /// 数据来源标识
    pub source: String,
}

/// 价格同步配置。
#[derive(Debug, Clone)]
pub struct PriceSyncConfig {
    /// 本地覆盖配置文件路径
    pub override_config_path: PathBuf,
    /// 远程定价仓库主 URL（通常 GitHub）
    pub remote_url: String,
    /// 主 URL 失败时的回退 URL（如 Gitee 镜像）
    pub remote_url_fallback: Option<String>,
    /// 是否启用远程同步
    pub remote_sync_enabled: bool,
    /// 远程同步间隔（秒）
    pub remote_sync_interval_secs: u64,
}

impl Default for PriceSyncConfig {
    fn default() -> Self {
        Self {
            override_config_path: PathBuf::from("conf/pricing.override.json"),
            remote_url: AIGX_PRICES_URL.to_string(),
            remote_url_fallback: Some(AIGX_PRICES_URL_GITEE.to_string()),
            remote_sync_enabled: true,
            remote_sync_interval_secs: DEFAULT_REMOTE_SYNC_INTERVAL_SECS,
        }
    }
}

/// 远程定价 JSON 单条目格式。
///
/// 期望远程 `pricing.json` 形如：
/// ```json
/// {
///   "models": [
///     {"model_name": "gpt-4", "input_price": 0.03, "output_price": 0.06, "price_type": "token"}
///   ]
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RemotePricingConfig {
    #[serde(default)]
    pub models: Vec<RemoteModelPrice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteModelPrice {
    pub model_name: String,
    #[serde(default)]
    pub input_price: f64,
    #[serde(default)]
    pub output_price: f64,
    #[serde(default = "default_price_type")]
    pub price_type: String,
    #[serde(default)]
    pub cache_price: Option<f64>,
}

fn default_price_type() -> String {
    "token".to_string()
}

impl RemoteModelPrice {
    /// 转为本地 `ModelPrice`（不带时间戳，由 `PricingStore::upsert_price` 补齐）。
    fn to_model_price(&self) -> ModelPrice {
        ModelPrice {
            model_name: self.model_name.clone(),
            input_price: self.input_price,
            output_price: self.output_price,
            cache_price: self.cache_price,
            price_type: self.price_type.clone(),
            created_at: 0,
            updated_at: 0,
        }
    }
}

/// 价格同步服务 — 多源优先级同步到 `PricingStore`。
pub struct PriceSyncService {
    store: Arc<PricingStore>,
    http_client: Client,
    config: PriceSyncConfig,
    /// 上次成功远程同步时间
    last_remote_sync: Option<DateTime<Utc>>,
}

impl PriceSyncService {
    /// 用默认配置构造。
    pub fn new(store: Arc<PricingStore>) -> Self {
        Self::with_config(store, PriceSyncConfig::default())
    }

    /// 用自定义配置构造。
    pub fn with_config(store: Arc<PricingStore>, config: PriceSyncConfig) -> Self {
        let http_client = Client::builder()
            .timeout(std::time::Duration::from_secs(HTTP_CLIENT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|e| {
                tracing::warn!("按超时构建 HTTP 客户端失败，回退默认: {e}");
                Client::new()
            });
        Self {
            store,
            http_client,
            config,
            last_remote_sync: None,
        }
    }

    /// 按优先级同步所有源。
    ///
    /// - `forced=false`（启动）：本地已有定价 → 跳过远程
    /// - `forced=true`（周期/手动）：始终拉取远程
    ///
    /// 远程失败时：本地有定价 → 优雅降级返回 Ok；本地空 → 重试 3 次（5s/15s/30s）后返回 Err。
    pub async fn sync_all(&mut self, forced: bool) -> anyhow::Result<SyncResult> {
        // 1. 本地覆盖（最高优先级，始终检查）
        if let Some(config) = self.load_local_override()? {
            tracing::info!("应用本地覆盖定价配置...");
            return self.apply_remote_config(&config, "local_override").await;
        }

        // 2. 启动快路径：本地已有定价且非强制 → 跳过远程
        if !forced {
            let local_count = self.store.list_prices().len();
            if local_count > 0 {
                tracing::info!(
                    models = local_count,
                    "本地已有定价，跳过远程同步（启动快路径）"
                );
                return Ok(SyncResult {
                    source: "local_cache".to_string(),
                    ..Default::default()
                });
            }
        }

        // 3. 远程拉取 + 冷启动重试
        if !self.config.remote_sync_enabled {
            tracing::info!("远程同步已禁用");
            return Ok(SyncResult {
                source: "disabled".to_string(),
                ..Default::default()
            });
        }

        const RETRY_DELAYS_SECS: &[u64] = &[5, 15, 30];
        let mut last_err: Option<anyhow::Error> = None;
        for (attempt, &delay) in RETRY_DELAYS_SECS.iter().enumerate() {
            match self.sync_remote_prices().await {
                Ok(result) => {
                    self.last_remote_sync = Some(Utc::now());
                    return Ok(result);
                }
                Err(e) => {
                    if attempt < RETRY_DELAYS_SECS.len() - 1 {
                        tracing::warn!(
                            attempt = attempt + 1,
                            delay_secs = delay,
                            error = %e,
                            "远程定价同步失败，重试中..."
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    }
                    last_err = Some(e);
                }
            }
        }

        let err = last_err.ok_or_else(|| anyhow::anyhow!("重试后 last_err 必然被设置"))?;
        let local_count = self.store.list_prices().len();
        if local_count > 0 {
            tracing::warn!(
                error = %err,
                models = local_count,
                "远程同步全部重试失败，沿用本地已有定价"
            );
            return Ok(SyncResult {
                source: "local_fallback".to_string(),
                ..Default::default()
            });
        }

        tracing::error!(
            error = %err,
            "FATAL: 远程定价不可达且本地无定价，请检查网络或预置本地定价"
        );
        Err(err)
    }

    /// 加载本地覆盖配置文件。
    fn load_local_override(&self) -> anyhow::Result<Option<RemotePricingConfig>> {
        let path = &self.config.override_config_path;
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(path)?;
        let config: RemotePricingConfig = serde_json::from_str(&content)?;
        Ok(Some(config))
    }

    /// 把远程配置逐条 upsert 到 `PricingStore`。
    pub async fn apply_remote_config(
        &self,
        config: &RemotePricingConfig,
        source: &str,
    ) -> anyhow::Result<SyncResult> {
        let mut result = SyncResult {
            source: source.to_string(),
            ..Default::default()
        };

        for remote in &config.models {
            if remote.model_name.is_empty() {
                result.errors += 1;
                continue;
            }
            let price = remote.to_model_price();
            match self.store.upsert_price(price) {
                Ok(_) => result.models_synced += 1,
                Err(e) => {
                    tracing::error!("upsert 定价失败 (model={}): {}", remote.model_name, e);
                    result.errors += 1;
                }
            }
        }

        tracing::info!(
            "从 {} 同步了 {} 个模型定价（{} 个错误）",
            source,
            result.models_synced,
            result.errors
        );
        Ok(result)
    }

    /// 从远程仓库同步（含 Gitee 回退）。
    ///
    /// 包含模型数量骤降保护：新数据模型数 < 旧数据 50% 时告警。
    async fn sync_remote_prices(&self) -> anyhow::Result<SyncResult> {
        let response = self.fetch_remote_config().await?;
        let config: RemotePricingConfig = serde_json::from_str(&response)
            .map_err(|e| anyhow::anyhow!("解析远程定价 JSON 失败: {e}"))?;

        // 模型数量骤降保护
        let prev_count = self.store.list_prices().len();
        let new_count = config.models.len();
        if prev_count > 0 && new_count * 2 < prev_count {
            tracing::warn!(
                prev_models = prev_count,
                new_models = new_count,
                "远程定价模型数比本地少 >50% — 可能数据异常"
            );
        }

        self.apply_remote_config(&config, "remote").await
    }

    /// 拉取远程定价，主 URL 失败时回退镜像。
    async fn fetch_remote_config(&self) -> anyhow::Result<String> {
        match self
            .http_client
            .get(&self.config.remote_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(response) => Ok(response.text().await?),
            Err(e) => {
                if let Some(fallback_url) = &self.config.remote_url_fallback {
                    tracing::warn!(
                        primary_url = %self.config.remote_url,
                        error = %e,
                        "主 URL 失败，尝试回退镜像"
                    );
                    let response = self
                        .http_client
                        .get(fallback_url)
                        .send()
                        .await?
                        .error_for_status()?;
                    Ok(response.text().await?)
                } else {
                    Err(e.into())
                }
            }
        }
    }

    /// 上次远程同步时间。
    pub fn last_remote_sync(&self) -> Option<DateTime<Utc>> {
        self.last_remote_sync
    }

    /// 启动后台周期同步任务（每小时检查一次）。
    pub fn start_periodic_sync(self: Arc<Self>, interval_secs: u64) {
        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                // 取可变借用：通过 Arc::get_mut 仅当唯一引用时可用；
                // 这里服务封装为 Arc 共享，采用内部同步逻辑（无 &mut 需求）。
                // 简化：直接调用 sync_remote_prices 的等价路径——
                // 由于 last_remote_sync 字段非关键，此处仅触发拉取并 apply。
                if let Err(e) = Self::periodic_tick(&self).await {
                    tracing::warn!("周期定价同步失败: {e}");
                }
            }
        });
    }

    /// 周期同步单次执行（内部辅助，不修改 last_remote_sync）。
    async fn periodic_tick(self: &Arc<Self>) -> anyhow::Result<()> {
        let response = self.fetch_remote_config().await?;
        let config: RemotePricingConfig = serde_json::from_str(&response)
            .map_err(|e| anyhow::anyhow!("解析远程定价 JSON 失败: {e}"))?;
        self.apply_remote_config(&config, "periodic").await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileStore;
    use tempfile::TempDir;

    fn pricing_store() -> Arc<PricingStore> {
        Arc::new(PricingStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        ))))
    }

    #[test]
    fn remote_model_price_to_model_price_preserves_fields() {
        let remote = RemoteModelPrice {
            model_name: "gpt-4".to_string(),
            input_price: 0.03,
            output_price: 0.06,
            price_type: "token".to_string(),
            cache_price: Some(0.015),
        };
        let local = remote.to_model_price();
        assert_eq!(local.model_name, "gpt-4");
        assert!((local.input_price - 0.03).abs() < 1e-9);
        assert!((local.output_price - 0.06).abs() < 1e-9);
        assert_eq!(local.price_type, "token");
        assert_eq!(local.cache_price, Some(0.015));
        // 时间戳由 upsert 补齐
        assert_eq!(local.created_at, 0);
        assert_eq!(local.updated_at, 0);
    }

    #[tokio::test]
    async fn apply_remote_config_upserts_to_store() {
        let store = pricing_store();
        let svc = PriceSyncService::new(store.clone());
        let config = RemotePricingConfig {
            models: vec![
                RemoteModelPrice {
                    model_name: "gpt-4".to_string(),
                    input_price: 0.03,
                    output_price: 0.06,
                    price_type: "token".to_string(),
                    cache_price: None,
                },
                RemoteModelPrice {
                    model_name: "dall-e".to_string(),
                    input_price: 0.04,
                    output_price: 0.0,
                    price_type: "count".to_string(),
                    cache_price: None,
                },
            ],
        };
        let result = svc.apply_remote_config(&config, "test").await.unwrap();
        assert_eq!(result.models_synced, 2);
        assert_eq!(result.errors, 0);
        assert_eq!(result.source, "test");

        // 验证写入 store
        let p = store.get_price("gpt-4").unwrap();
        assert!((p.input_price - 0.03).abs() < 1e-9);
        let p2 = store.get_price("dall-e").unwrap();
        assert_eq!(p2.price_type, "count");
    }

    #[tokio::test]
    async fn apply_remote_config_skips_empty_model_name() {
        let store = pricing_store();
        let svc = PriceSyncService::new(store);
        let config = RemotePricingConfig {
            models: vec![RemoteModelPrice {
                model_name: "".to_string(),
                input_price: 0.0,
                output_price: 0.0,
                price_type: "token".to_string(),
                cache_price: None,
            }],
        };
        let result = svc.apply_remote_config(&config, "test").await.unwrap();
        assert_eq!(result.models_synced, 0);
        assert_eq!(result.errors, 1);
    }

    #[tokio::test]
    async fn sync_all_startup_fast_path_skips_remote() {
        let store = pricing_store();
        // 预置一条定价
        store
            .upsert_price(ModelPrice::new("gpt-4", 0.03, 0.06))
            .unwrap();
        let mut svc = PriceSyncService::new(store);
        // forced=false 且本地有定价 → 走快路径，不触网
        let result = svc.sync_all(false).await.unwrap();
        assert_eq!(result.source, "local_cache");
    }

    #[tokio::test]
    async fn sync_all_local_override_takes_priority() {
        let store = pricing_store();
        let mut config = PriceSyncConfig::default();
        // 写一个临时覆盖文件
        let dir = TempDir::new().unwrap();
        let override_path = dir.path().join("override.json");
        std::fs::write(
            &override_path,
            r#"{"models":[{"model_name":"override-model","input_price":0.5,"output_price":1.0,"price_type":"token"}]}"#,
        )
        .unwrap();
        config.override_config_path = override_path;
        // 禁用远程避免触网
        config.remote_sync_enabled = false;

        let mut svc = PriceSyncService::with_config(store.clone(), config);
        let result = svc.sync_all(true).await.unwrap();
        assert_eq!(result.source, "local_override");
        assert_eq!(result.models_synced, 1);
        let p = store.get_price("override-model").unwrap();
        assert!((p.input_price - 0.5).abs() < 1e-9);
    }

    #[test]
    fn remote_pricing_config_parses_json() {
        let json = r#"{
            "models": [
                {"model_name": "a", "input_price": 0.01, "output_price": 0.02},
                {"model_name": "b", "input_price": 0.0, "output_price": 0.0, "price_type": "count"}
            ]
        }"#;
        let config: RemotePricingConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.models.len(), 2);
        assert_eq!(config.models[0].price_type, "token"); // 默认
        assert_eq!(config.models[1].price_type, "count");
    }

    #[test]
    fn price_sync_config_default_urls() {
        let cfg = PriceSyncConfig::default();
        assert_eq!(cfg.remote_url, AIGX_PRICES_URL);
        assert_eq!(
            cfg.remote_url_fallback.as_deref(),
            Some(AIGX_PRICES_URL_GITEE)
        );
        assert!(cfg.remote_sync_enabled);
        assert_eq!(
            cfg.remote_sync_interval_secs,
            DEFAULT_REMOTE_SYNC_INTERVAL_SECS
        );
    }
}
