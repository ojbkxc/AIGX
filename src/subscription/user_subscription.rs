//! 用户订阅记录

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::plan::Plan;

/// 订阅状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionStatus {
    /// 活跃
    Active,
    /// 已过期
    Expired,
    /// 已取消
    Cancelled,
    /// 暂停
    Paused,
    /// 试用期
    Trial,
}

impl std::fmt::Display for SubscriptionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Expired => write!(f, "expired"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Paused => write!(f, "paused"),
            Self::Trial => write!(f, "trial"),
        }
    }
}

/// 用户订阅
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSubscription {
    /// 订阅 ID
    pub id: String,
    /// 用户 ID
    pub user_id: String,
    /// 关联套餐
    pub plan_id: String,
    /// 套餐快照
    pub plan_name: String,
    /// 配额
    pub quota: i64,
    /// 已用配额
    pub used_quota: i64,
    /// 订阅状态
    pub status: SubscriptionStatus,
    /// 开始时间
    pub start_at: DateTime<Utc>,
    /// 结束时间
    pub end_at: DateTime<Utc>,
    /// 是否自动续费
    pub auto_renew: bool,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

/// 订阅 Schema（用于 API 请求/响应）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSubscriptionSchema {
    pub user_id: String,
    pub plan_id: String,
    pub auto_renew: Option<bool>,
}

impl UserSubscription {
    pub fn new(user_id: &str, plan: &Plan, auto_renew: bool) -> Self {
        let now = Utc::now();
        let end_at = now + Duration::days(30);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: user_id.to_string(),
            plan_id: plan.id.clone(),
            plan_name: plan.name.clone(),
            quota: plan.quota,
            used_quota: 0,
            status: SubscriptionStatus::Active,
            start_at: now,
            end_at,
            auto_renew,
            created_at: now,
            updated_at: now,
        }
    }

    /// 检查是否过期
    pub fn is_expired(&self) -> bool {
        self.status == SubscriptionStatus::Expired || Utc::now() > self.end_at
    }

    /// 检查是否活跃
    pub fn is_active(&self) -> bool {
        self.status == SubscriptionStatus::Active && !self.is_expired()
    }

    /// 剩余配额
    pub fn remaining_quota(&self) -> i64 {
        (self.quota - self.used_quota).max(0)
    }
}
