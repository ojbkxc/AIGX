//! 订阅套餐定义

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 套餐价格配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingConfig {
    /// 月价格（元）
    pub monthly: f64,
    /// 年价格（元）
    pub yearly: f64,
    /// 是否启用
    pub enabled: bool,
}

impl Default for PricingConfig {
    fn default() -> Self {
        Self {
            monthly: 0.0,
            yearly: 0.0,
            enabled: true,
        }
    }
}

/// 订阅套餐
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// 套餐 ID
    pub id: String,
    /// 套餐名称
    pub name: String,
    /// 套餐描述
    pub description: String,
    /// 每月配额（token）
    pub quota: i64,
    /// 价格配置
    pub pricing: PricingConfig,
    /// 最大并发请求数
    pub max_concurrency: i32,
    /// 是否支持流式
    pub supports_streaming: bool,
    /// 可用模型列表
    pub models: Vec<String>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// 套餐 Schema（用于 API 请求/响应）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanSchema {
    pub name: String,
    pub description: Option<String>,
    pub quota: i64,
    pub pricing: Option<PricingConfig>,
    pub max_concurrency: Option<i32>,
    pub supports_streaming: Option<bool>,
    pub models: Option<Vec<String>>,
}

impl Plan {
    pub fn new(schema: PlanSchema) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: schema.name,
            description: schema.description.unwrap_or_default(),
            quota: schema.quota,
            pricing: schema.pricing.unwrap_or_default(),
            max_concurrency: schema.max_concurrency.unwrap_or(5),
            supports_streaming: schema.supports_streaming.unwrap_or(true),
            models: schema.models.unwrap_or_default(),
            created_at: now,
            updated_at: now,
        }
    }
}
