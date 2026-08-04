//! 订阅管理服务

use anyhow::Result;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

use super::plan::Plan;
use super::user_subscription::{SubscriptionStatus, UserSubscription};

/// 订阅管理服务
#[derive(Debug, Clone)]
pub struct SubscriptionService {
    /// 套餐列表
    plans: HashMap<String, Plan>,
    /// 用户订阅
    subscriptions: HashMap<String, Vec<UserSubscription>>,
}

/// 订阅管理器（线程安全版本）
#[derive(Debug, Clone)]
pub struct SubscriptionManager {
    inner: Arc<RwLock<SubscriptionService>>,
}

impl SubscriptionService {
    pub fn new() -> Self {
        Self {
            plans: HashMap::new(),
            subscriptions: HashMap::new(),
        }
    }

    /// 注册套餐
    pub fn register_plan(&mut self, plan: Plan) {
        self.plans.insert(plan.id.clone(), plan);
    }

    /// 获取套餐
    pub fn get_plan(&self, id: &str) -> Option<&Plan> {
        self.plans.get(id)
    }

    /// 获取所有套餐
    pub fn list_plans(&self) -> Vec<&Plan> {
        self.plans.values().collect()
    }

    /// 创建订阅
    pub fn create_subscription(
        &mut self,
        user_id: &str,
        plan_id: &str,
        auto_renew: bool,
    ) -> Result<UserSubscription> {
        let plan = self
            .plans
            .get(plan_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Plan not found: {}", plan_id))?;

        let sub = UserSubscription::new(user_id, &plan, auto_renew);
        self.subscriptions
            .entry(user_id.to_string())
            .or_default()
            .push(sub.clone());

        Ok(sub)
    }

    /// 获取用户订阅
    pub fn get_user_subscriptions(&self, user_id: &str) -> Vec<&UserSubscription> {
        self.subscriptions
            .get(user_id)
            .map(|subs| subs.iter().collect())
            .unwrap_or_default()
    }

    /// 检查用户活跃订阅
    pub fn has_active_subscription(&self, user_id: &str) -> bool {
        self.get_user_subscriptions(user_id)
            .iter()
            .any(|s| s.is_active())
    }

    /// 取消订阅
    pub fn cancel_subscription(&mut self, user_id: &str, subscription_id: &str) -> Result<()> {
        let subs = self
            .subscriptions
            .get_mut(user_id)
            .ok_or_else(|| anyhow::anyhow!("No subscriptions found"))?;

        let sub = subs
            .iter_mut()
            .find(|s| s.id == subscription_id)
            .ok_or_else(|| anyhow::anyhow!("Subscription not found"))?;

        sub.status = SubscriptionStatus::Cancelled;
        sub.updated_at = chrono::Utc::now();
        Ok(())
    }
}

impl Default for SubscriptionService {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(SubscriptionService::new())),
        }
    }

    pub fn register_plan(&self, plan: Plan) -> Result<()> {
        self.inner.write().unwrap().register_plan(plan);
        Ok(())
    }

    pub fn get_plan(&self, id: &str) -> Option<Plan> {
        self.inner.read().unwrap().get_plan(id).cloned()
    }

    pub fn list_plans(&self) -> Vec<Plan> {
        self.inner.read().unwrap().list_plans().into_iter().cloned().collect()
    }

    pub fn create_subscription(
        &self,
        user_id: &str,
        plan_id: &str,
        auto_renew: bool,
    ) -> Result<UserSubscription> {
        self.inner.write().unwrap().create_subscription(user_id, plan_id, auto_renew)
    }

    pub fn get_user_subscriptions(&self, user_id: &str) -> Vec<UserSubscription> {
        self.inner.read().unwrap().get_user_subscriptions(user_id).into_iter().cloned().collect()
    }

    pub fn has_active_subscription(&self, user_id: &str) -> bool {
        self.inner.read().unwrap().has_active_subscription(user_id)
    }

    pub fn cancel_subscription(&self, user_id: &str, subscription_id: &str) -> Result<()> {
        self.inner.write().unwrap().cancel_subscription(user_id, subscription_id)
    }
}

impl Default for SubscriptionManager {
    fn default() -> Self {
        Self::new()
    }
}
