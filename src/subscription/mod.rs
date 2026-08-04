//! 订阅管理模块
//!
//! 功能：
//! - 订阅套餐管理（价格、时长、配额）
//! - 用户订阅记录管理
//! - 订阅购买和续费
//! - 自动续费检查

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc, Duration};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub mod plan;
pub mod user_subscription;
pub mod service;

pub use plan::{Plan, PlanSchema, PricingConfig};
pub use user_subscription::{UserSubscription, UserSubscriptionSchema, SubscriptionStatus};
pub use service::{SubscriptionService, SubscriptionManager};

/// 订阅事件回调
pub type SubscriptionCallback = Box<dyn Fn(&UserSubscription) -> Result<()> + Send + Sync>;

/// 订阅事件类型
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SubscriptionEvent {
    Created,
    Renewed,
    Expired,
    Cancelled,
    Upgraded,
    Downgraded,
}