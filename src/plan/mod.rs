//! 套餐系统 — 按量套餐模板 + 按套餐发放 API Key + 时长订阅（SubscriptionPlan）。
//!
//! 业务模式（参照 cf-ai-gw「key 即套餐」+ new-api SubscriptionPlan 模板化）：
//! 卖家在后台定义套餐模板（名称/售价/额度/有效期天数/分组/模型白名单），
//! 买家付款后卖家按模板一键创建 API Key 交付——key 自带 quota_limit 与
//! expires_at，即「次数/额度 + 有效期」的套餐凭证，无需额外交付实体。
//!
//! plan_type 二分（#85 套餐订阅化，对齐 new-api SubscriptionPlan）：
//! - `once`（默认）：按量模板，走发 key 交付路径；
//! - `subscription`：时长订阅，用户余额购买后生成 UserSubscription
//!   （见 subscription.rs），自带独立配额池/到期时间/分组升降级。
//!
//! 持久化使用 FileStore KV：key 前缀 `plan:{id}`。
//! Token（ApiKey）已有 quota_limit/used_quota/expires_at 字段完整覆盖
//! cf-ai-gw 的 maxCalls/usedCalls/expiresAt 模型，无需 schema 变更。

pub mod subscription;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 套餐模板。
///
/// - `quota`：额度上限（配额单位，写入 key 的 quota_limit；0=不限）
/// - `duration_days`：有效期天数（0=永不过期），发 key 时换算为 expires_at
/// - `price`：售价（展示用，单位元；实际收款由卖家线下/支付渠道完成）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    /// 唯一 ID（uuid）
    pub id: String,
    /// 套餐名称（如「入门包 100 万 tokens」）
    pub name: String,
    /// 售价（元，展示用）
    #[serde(default)]
    pub price: f64,
    /// 额度上限（配额单位；0 = 不限额度）
    #[serde(default)]
    pub quota: i64,
    /// 有效期天数（0 = 永不过期）
    #[serde(default = "default_duration")]
    pub duration_days: i64,
    /// 计费分组（写入 key 的 group，default "default"）
    #[serde(default = "default_group")]
    pub group: String,
    /// 模型白名单（None/空 = 不限）
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// 描述（展示用）
    #[serde(default)]
    pub description: String,
    /// 是否上架（false = 停售，仅历史记录可见）
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 已按此套餐发放的 key 数（发 key 时递增，供卖家对账）
    #[serde(default)]
    pub issued_count: i64,
    // ── 订阅化扩展（#85，对齐 new-api SubscriptionPlan）──────────────────
    /// 套餐类型：once（按量发 key，默认）/ subscription（时长订阅）
    #[serde(default)]
    pub plan_type: String,
    /// 订阅时长单位：year / month / day / hour / custom
    #[serde(default = "default_duration_unit")]
    pub duration_unit: String,
    /// 订阅时长数值（custom 时忽略；必须 > 0）
    #[serde(default = "default_duration_value")]
    pub duration_value: i64,
    /// custom 时长的秒数
    #[serde(default)]
    pub custom_seconds: i64,
    /// 订阅总配额（0 = 不限）
    #[serde(default)]
    pub total_amount: i64,
    /// 订阅配额周期重置：never / daily / weekly / monthly / custom
    #[serde(default = "default_reset_period")]
    pub quota_reset_period: String,
    /// custom 重置周期的秒数
    #[serde(default)]
    pub quota_reset_custom_seconds: i64,
    /// 是否允许余额购买订阅
    #[serde(default = "default_allow_balance_pay")]
    pub allow_balance_pay: bool,
    /// 订阅池耗尽后是否允许回退用户钱包
    #[serde(default = "default_allow_wallet_overflow")]
    pub allow_wallet_overflow: bool,
    /// 每用户最多购买次数（0 = 不限）
    #[serde(default)]
    pub max_purchase_per_user: i64,
    /// 购买后升级到的用户分组（空 = 不变）
    #[serde(default)]
    pub upgrade_group: String,
    /// 到期后回退到的分组（空 = 回退购买前分组）
    #[serde(default)]
    pub downgrade_group: String,
    /// 展示排序（升序）
    #[serde(default)]
    pub sort_order: i64,
    /// 创建时间
    pub created_at: i64,
    /// 更新时间
    #[serde(default)]
    pub updated_at: i64,
}

fn default_duration_unit() -> String {
    "month".to_string()
}

fn default_duration_value() -> i64 {
    1
}

fn default_reset_period() -> String {
    "never".to_string()
}

fn default_allow_balance_pay() -> bool {
    true
}

fn default_allow_wallet_overflow() -> bool {
    true
}

impl Plan {
    /// 是否为时长订阅套餐
    pub fn is_subscription(&self) -> bool {
        self.plan_type == "subscription"
    }
}

fn default_duration() -> i64 {
    30
}

fn default_group() -> String {
    "default".to_string()
}

fn default_enabled() -> bool {
    true
}

impl Plan {
    /// 发 key 时换算过期时间：now + duration_days 天；0 = None（永不过期）
    pub fn compute_expires_at(&self) -> Option<i64> {
        if self.duration_days <= 0 {
            return None;
        }
        Some(chrono::Utc::now().timestamp() + self.duration_days.saturating_mul(86_400))
    }
}

/// 套餐存储。
///
/// key 格式：`plan:{id}` — 套餐模板记录
pub struct PlanStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, Plan>>,
}

impl PlanStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("plan:")?;
        let mut by_id = self.by_id.write();
        by_id.clear();
        for key in &keys {
            if let Some(p) = self.store.get::<Plan>(key)? {
                by_id.insert(p.id.clone(), p);
            }
        }
        Ok(())
    }

    fn persist(&self, p: &Plan) -> anyhow::Result<()> {
        self.store.put(&format!("plan:{}", p.id), p)?;
        Ok(())
    }

    /// 创建或更新套餐（按 id upsert）。
    pub fn upsert(&self, mut p: Plan) -> anyhow::Result<Plan> {
        let now = chrono::Utc::now().timestamp();
        if p.id.is_empty() {
            p.id = uuid::Uuid::new_v4().to_string();
            p.created_at = now;
        } else if let Some(old) = self.by_id.read().get(&p.id) {
            // 保留服务端维护字段：发放计数不可被表单覆盖
            p.issued_count = old.issued_count;
            p.created_at = old.created_at;
        }
        p.updated_at = now;
        self.persist(&p)?;
        self.by_id.write().insert(p.id.clone(), p.clone());
        Ok(p)
    }

    /// 删除套餐（模板删除不影响已发放的 key）
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        if !self.by_id.read().contains_key(id) {
            anyhow::bail!("plan not found");
        }
        self.store.delete(&format!("plan:{id}"))?;
        self.by_id.write().remove(id);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Plan> {
        self.by_id.read().get(id).cloned()
    }

    /// 列出所有套餐（按创建时间倒序）
    pub fn list(&self) -> Vec<Plan> {
        let mut all: Vec<Plan> = self.by_id.read().values().cloned().collect();
        all.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        all
    }

    /// 上架的订阅套餐（用户侧购买页，sort_order 升序 + 创建时间倒序）
    pub fn list_subscription_enabled(&self) -> Vec<Plan> {
        let mut all: Vec<Plan> = self
            .by_id
            .read()
            .values()
            .filter(|p| p.enabled && p.is_subscription())
            .cloned()
            .collect();
        all.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(b.created_at.cmp(&a.created_at))
        });
        all
    }

    /// 按 id 解析出创建 key 的完整选项（发 key 入口的唯一路径）。
    ///
    /// 套餐不存在或已停售返回 None。
    pub fn build_key_options(
        &self,
        plan_id: &str,
        key_name: &str,
    ) -> Option<crate::api::auth::CreateApiKeyOptions> {
        let p = self.get(plan_id)?;
        if !p.enabled {
            return None;
        }
        Some(crate::api::auth::CreateApiKeyOptions {
            name: key_name.to_string(),
            // 套餐 key 不挂用户（cf-ai-gw 模式：key 自带额度，独立交付），
            // 不受用户余额约束，只受自身 quota_limit + expires_at 约束
            user_id: None,
            group: if p.group.is_empty() {
                "default".to_string()
            } else {
                p.group.clone()
            },
            allowed_models: p.allowed_models.clone(),
            expires_at: p.compute_expires_at(),
            quota_limit: if p.quota > 0 { Some(p.quota) } else { None },
            ip_limit: None,
        })
    }

    /// 记一次发放（issued_count += 1）
    pub fn record_issue(&self, plan_id: &str) {
        let mut by_id = self.by_id.write();
        if let Some(p) = by_id.get_mut(plan_id) {
            p.issued_count += 1;
            let snapshot = p.clone();
            drop(by_id);
            if let Err(e) = self.persist(&snapshot) {
                tracing::error!("Failed to persist plan {} issue count: {}", snapshot.id, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> PlanStore {
        PlanStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn sample_plan(name: &str, quota: i64, days: i64) -> Plan {
        Plan {
            id: String::new(),
            name: name.to_string(),
            price: 9.9,
            quota,
            duration_days: days,
            group: "default".to_string(),
            allowed_models: None,
            description: String::new(),
            enabled: true,
            issued_count: 0,
            plan_type: String::new(),
            duration_unit: String::new(),
            duration_value: 0,
            custom_seconds: 0,
            total_amount: 0,
            quota_reset_period: String::new(),
            quota_reset_custom_seconds: 0,
            allow_balance_pay: true,
            allow_wallet_overflow: true,
            max_purchase_per_user: 0,
            upgrade_group: String::new(),
            downgrade_group: String::new(),
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn upsert_assigns_id_and_persists() {
        let s = store();
        let p = s.upsert(sample_plan("入门包", 100, 30)).unwrap();
        assert!(!p.id.is_empty());
        assert_eq!(s.list().len(), 1);
        // 重新加载持久化数据
        let s2 = PlanStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )));
        assert!(s2.list().is_empty(), "tempdir 不同实例应独立");
    }

    #[test]
    fn build_key_options_translates_plan() {
        let s = store();
        let p = s.upsert(sample_plan("标准包", 5000, 30)).unwrap();
        let opts = s.build_key_options(&p.id, "买家-张三").unwrap();
        assert_eq!(opts.name, "买家-张三");
        assert_eq!(opts.quota_limit, Some(5000));
        assert!(opts.expires_at.unwrap() > chrono::Utc::now().timestamp());
        assert_eq!(opts.group, "default");
        assert!(opts.user_id.is_none());
    }

    #[test]
    fn build_key_options_unlimited_quota_and_never_expires() {
        let s = store();
        let mut p = sample_plan("不限包", 0, 0);
        p.quota = 0;
        p.duration_days = 0;
        let saved = s.upsert(p).unwrap();
        let opts = s.build_key_options(&saved.id, "x").unwrap();
        assert!(opts.quota_limit.is_none());
        assert!(opts.expires_at.is_none());
    }

    #[test]
    fn build_key_options_rejects_disabled_plan() {
        let s = store();
        let mut p = sample_plan("停售包", 100, 30);
        p.enabled = false;
        let saved = s.upsert(p).unwrap();
        assert!(s.build_key_options(&saved.id, "x").is_none());
    }

    #[test]
    fn issue_count_survives_upsert_and_increments() {
        let s = store();
        let p = s.upsert(sample_plan("计数包", 100, 30)).unwrap();
        s.record_issue(&p.id);
        s.record_issue(&p.id);
        // 编辑套餐（表单不含 issued_count）：保留计数
        let mut edited = s.get(&p.id).unwrap();
        edited.name = "计数包 v2".to_string();
        let saved = s.upsert(edited).unwrap();
        assert_eq!(saved.issued_count, 2);
        s.record_issue(&saved.id);
        assert_eq!(s.get(&saved.id).unwrap().issued_count, 3);
    }

    #[test]
    fn delete_plan() {
        let s = store();
        let p = s.upsert(sample_plan("待删包", 1, 1)).unwrap();
        assert!(s.delete(&p.id).is_ok());
        assert!(s.get(&p.id).is_none());
        assert!(s.delete(&p.id).is_err());
    }

    #[test]
    fn subscription_plan_filter_and_defaults() {
        let s = store();
        let mut sub = sample_plan("月度会员", 0, 0);
        sub.plan_type = "subscription".into();
        sub.sort_order = 1;
        let mut sub2 = sample_plan("年度会员", 0, 0);
        sub2.plan_type = "subscription".into();
        sub2.sort_order = 0;
        // 默认 once 模式的套餐（plan_type 为空）
        s.upsert(sample_plan("按量包", 100, 30)).unwrap();
        s.upsert(sub).unwrap();
        s.upsert(sub2).unwrap();
        let subs = s.list_subscription_enabled();
        assert_eq!(subs.len(), 2, "仅订阅套餐且已上架的入选");
        assert_eq!(subs[0].name, "年度会员", "sort_order 升序");
        assert_eq!(subs[1].name, "月度会员");
    }
}
