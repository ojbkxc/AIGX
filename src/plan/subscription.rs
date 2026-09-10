//! 订阅系统 — 时长订阅与独立配额池（对齐 new-api model/subscription.go）。
//!
//! 与「按量套餐发 key」（plan 模板 → 独立 key）并存：订阅是绑定到用户的
//! 时长权益（SubscriptionPlan 快照），拥有独立配额池（amount_total /
//! amount_used），到期自动失效，可选周期重置与用户分组升降级。
//!
//! KV 布局：`usersub:{id}` — 用户订阅实例。
//! 计费接入：`api::openai` 的 reserve/settle/charge 路径优先扣订阅池，
//! 池不足时按 `allow_wallet_overflow` 决定是否回退用户钱包。

use anyhow::Result;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;
use crate::user::UserStore;

use super::Plan;

fn default_status() -> String {
    "active".into()
}

fn default_source() -> String {
    "balance".into()
}

fn default_true() -> bool {
    true
}

/// 用户订阅实例（对齐 new-api UserSubscription）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSubscription {
    /// 唯一 ID（uuid）
    pub id: String,
    pub user_id: String,
    pub plan_id: String,
    /// 订阅总配额（0 = 不限）
    #[serde(default)]
    pub amount_total: i64,
    /// 已消耗配额
    #[serde(default)]
    pub amount_used: i64,
    pub start_time: i64,
    /// 到期时间（unix 秒）；status=active 且 end_time <= now 视为已过期
    pub end_time: i64,
    /// active / expired / cancelled
    #[serde(default = "default_status")]
    pub status: String,
    /// balance（余额购买）/ admin（管理员绑定）
    #[serde(default = "default_source")]
    pub source: String,
    /// 最近一次配额重置时间（0 = 从未重置）
    #[serde(default)]
    pub last_reset_time: i64,
    /// 下次配额重置时间（0 = 不重置）
    #[serde(default)]
    pub next_reset_time: i64,
    /// 购买后升级到的用户分组（快照自 plan；空 = 不变）
    #[serde(default)]
    pub upgrade_group: String,
    /// 购买前的用户分组（升级回退依据）
    #[serde(default)]
    pub prev_user_group: String,
    /// 到期后回退到的分组（快照自 plan；空 = 回退 prev_user_group）
    #[serde(default)]
    pub downgrade_group: String,
    /// 订阅池耗尽后是否允许回退用户钱包（快照自 plan）
    #[serde(default = "default_true")]
    pub allow_wallet_overflow: bool,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

impl UserSubscription {
    /// 动态活跃判断（status 与到期时间双重条件，cron 落库前的过渡态安全）
    pub fn is_active_at(&self, now: i64) -> bool {
        self.status == "active" && self.end_time > now
    }
}

/// 计算订阅到期时间（对齐 new-api calcPlanEndTime）。
///
/// year/month 走日历进位，day/hour/custom 走固定秒数。
pub fn calc_plan_end(start: i64, plan: &Plan) -> Result<i64> {
    let unit = plan.duration_unit.as_str();
    if plan.duration_value <= 0 && unit != "custom" {
        anyhow::bail!("duration_value must be > 0");
    }
    let base = chrono::DateTime::from_timestamp(start, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid start timestamp"))?;
    let months = plan.duration_value.max(0) as u32;
    let end = match unit {
        "year" => base
            .checked_add_months(chrono::Months::new(months.saturating_mul(12)))
            .ok_or_else(|| anyhow::anyhow!("year overflow"))?,
        "month" => base
            .checked_add_months(chrono::Months::new(months))
            .ok_or_else(|| anyhow::anyhow!("month overflow"))?,
        "day" => base
            .checked_add_signed(chrono::Duration::days(plan.duration_value))
            .ok_or_else(|| anyhow::anyhow!("day overflow"))?,
        "hour" => base
            .checked_add_signed(chrono::Duration::hours(plan.duration_value))
            .ok_or_else(|| anyhow::anyhow!("hour overflow"))?,
        "custom" => {
            if plan.custom_seconds <= 0 {
                anyhow::bail!("custom_seconds must be > 0");
            }
            base.checked_add_signed(chrono::Duration::seconds(plan.custom_seconds))
                .ok_or_else(|| anyhow::anyhow!("custom duration overflow"))?
        }
        other => anyhow::bail!("invalid duration_unit: {other}"),
    };
    Ok(end.timestamp())
}

/// 计算下次配额重置时间（对齐 new-api calcNextResetTime）。
///
/// 周期对齐本地时区自然日/周一/月初。返回 0 = 不重置
/// （never / 周期超出订阅期）。直接在 naive 日期域推算，避免
/// TimeZone trait 引入。
pub fn calc_next_reset(base_ts: i64, plan: &Plan, end_ts: i64) -> i64 {
    use chrono::{Datelike, TimeZone};
    let period = plan.quota_reset_period.as_str();
    if period == "never" {
        return 0;
    }
    let base = match chrono::DateTime::from_timestamp(base_ts, 0) {
        Some(t) => t,
        None => return 0,
    };
    let local_date = base.with_timezone(&chrono::Local).date_naive();
    // naive 日期域推算目标自然日，再经 Local 00:00 转回时间戳
    let target_date = match period {
        "daily" => local_date.checked_add_days(chrono::Days::new(1)),
        "weekly" => {
            // 对齐下周一 00:00（weekday 从周一开始计数；周日=7 天）
            let days_until = (7 - base.weekday().num_days_from_monday()) as u64;
            local_date.checked_add_days(chrono::Days::new(days_until))
        }
        "monthly" => chrono::NaiveDate::from_ymd_opt(local_date.year(), local_date.month(), 1)
            .and_then(|d| d.checked_add_months(chrono::Months::new(1))),
        "custom" => {
            if plan.quota_reset_custom_seconds <= 0 {
                return 0;
            }
            return match chrono::DateTime::from_timestamp(
                base_ts + plan.quota_reset_custom_seconds,
                0,
            ) {
                Some(t) => {
                    let n_ts = t.timestamp();
                    if end_ts > 0 && n_ts > end_ts {
                        0
                    } else {
                        n_ts
                    }
                }
                None => 0,
            };
        }
        _ => None,
    };
    match target_date.and_then(|d| {
        chrono::Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .single()
    }) {
        Some(n) => {
            let n_ts = n.timestamp();
            // 周期点超出订阅期 → 不重置
            if end_ts > 0 && n_ts > end_ts {
                0
            } else {
                n_ts
            }
        }
        None => 0,
    }
}

/// 用户订阅存储。
///
/// key 格式：`usersub:{id}`。内存 HashMap 索引 + FileStore 持久化，
/// 扣减/预留/结算均在写锁内完成读-改-写（与 UserStore 原子口径一致）。
pub struct SubscriptionStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, UserSubscription>>,
}

impl SubscriptionStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("usersub:")?;
        let mut by_id = self.by_id.write();
        by_id.clear();
        for key in &keys {
            if let Some(sub) = self.store.get::<UserSubscription>(key)? {
                by_id.insert(sub.id.clone(), sub);
            }
        }
        Ok(())
    }

    fn persist(&self, sub: &UserSubscription) -> Result<()> {
        self.store.put(&format!("usersub:{}", sub.id), sub)?;
        Ok(())
    }

    /// 插入新订阅（id 由调用方生成）
    pub fn create(&self, sub: UserSubscription) -> Result<UserSubscription> {
        self.persist(&sub)?;
        self.by_id.write().insert(sub.id.clone(), sub.clone());
        Ok(sub)
    }

    pub fn get(&self, id: &str) -> Option<UserSubscription> {
        self.by_id.read().get(id).cloned()
    }

    /// 用户的全部订阅（含过期/取消，按 end_time 倒序）
    pub fn list_by_user(&self, user_id: &str) -> Vec<UserSubscription> {
        let mut subs: Vec<UserSubscription> = self
            .by_id
            .read()
            .values()
            .filter(|s| s.user_id == user_id)
            .cloned()
            .collect();
        subs.sort_by(|a, b| b.end_time.cmp(&a.end_time).then(b.id.cmp(&a.id)));
        subs
    }

    /// 用户的活跃订阅（动态判断过期，end_time 升序——优先消耗快到期的）
    pub fn find_active(&self, user_id: &str, now: i64) -> Vec<UserSubscription> {
        let mut subs: Vec<UserSubscription> = self
            .by_id
            .read()
            .values()
            .filter(|s| s.is_active_at(now) && s.user_id == user_id)
            .cloned()
            .collect();
        subs.sort_by(|a, b| a.end_time.cmp(&b.end_time).then(a.id.cmp(&b.id)));
        subs
    }

    /// 是否任一活跃订阅禁止钱包兜底（对齐 new-api
    /// UserActiveSubscriptionsAllowWalletOverflow：一条 false 即整体禁止）。
    /// 无活跃订阅时返回 true（钱包本来就是唯一支付方式）。
    pub fn wallet_overflow_allowed(&self, user_id: &str, now: i64) -> bool {
        !self
            .by_id
            .read()
            .values()
            .any(|s| s.is_active_at(now) && s.user_id == user_id && !s.allow_wallet_overflow)
    }

    /// 用户对某套餐的购买总数（不限状态，对齐 new-api CountUserSubscriptionsByPlan）
    pub fn count_by_plan(&self, user_id: &str, plan_id: &str) -> i64 {
        self.by_id
            .read()
            .values()
            .filter(|s| s.user_id == user_id && s.plan_id == plan_id)
            .count() as i64
    }

    /// 直接扣减订阅池（无预留路径）。池不足返回 false。
    pub fn try_charge(&self, id: &str, amount: i64) -> bool {
        if amount <= 0 {
            return true;
        }
        let now = chrono::Utc::now().timestamp();
        let mut by_id = self.by_id.write();
        let sub = match by_id.get_mut(id) {
            Some(s) => s,
            None => return false,
        };
        // 写锁内重验活跃（防 find_active 快照与扣减之间的 TOCTOU）
        if !sub.is_active_at(now) {
            return false;
        }
        if sub.amount_total > 0 && sub.amount_used + amount > sub.amount_total {
            return false;
        }
        sub.amount_used += amount;
        sub.updated_at = now;
        let snapshot = sub.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!(
                "Failed to persist subscription {} charge: {}",
                snapshot.id,
                e
            );
        }
        true
    }

    /// 预留订阅池额度（两段式第一步）。池不足返回 false。
    pub fn try_reserve(&self, id: &str, amount: i64) -> bool {
        self.try_charge(id, amount)
    }

    /// 结算订阅池预留（两段式第二步）：释放预留并将实际消费入账。
    /// actual 超出 reserved 的部分照常入账（钳制到 total，与钱包 settle 的
    /// 宽容口径一致，不因估算误差报错）。
    pub fn settle(&self, id: &str, reserved: i64, actual: i64) {
        if reserved <= 0 {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let mut by_id = self.by_id.write();
        let sub = match by_id.get_mut(id) {
            Some(s) => s,
            None => return,
        };
        let release = reserved.min(sub.amount_used);
        sub.amount_used = sub.amount_used - release + actual.max(0);
        if sub.amount_total > 0 && sub.amount_used > sub.amount_total {
            sub.amount_used = sub.amount_total;
        }
        sub.updated_at = now;
        let snapshot = sub.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!(
                "Failed to persist subscription {} settle: {}",
                snapshot.id,
                e
            );
        }
    }

    /// 释放订阅池预留（请求取消/失败路径），归还到池可用额度。
    pub fn release(&self, id: &str, amount: i64) {
        if amount <= 0 {
            return;
        }
        let now = chrono::Utc::now().timestamp();
        let mut by_id = self.by_id.write();
        let sub = match by_id.get_mut(id) {
            Some(s) => s,
            None => return,
        };
        sub.amount_used -= amount.min(sub.amount_used);
        sub.updated_at = now;
        let snapshot = sub.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!(
                "Failed to persist subscription {} release: {}",
                snapshot.id,
                e
            );
        }
    }

    /// 扫描到期订阅：active 且 end_time <= now → expired 落库。
    /// 返回刚过期的列表（调用方负责分组回退）。
    pub fn expire_due(&self, now: i64) -> Vec<UserSubscription> {
        let mut expired = Vec::new();
        let mut by_id = self.by_id.write();
        for sub in by_id.values_mut() {
            if sub.status == "active" && sub.end_time > 0 && sub.end_time <= now {
                sub.status = "expired".into();
                sub.updated_at = now;
                expired.push(sub.clone());
            }
        }
        let snapshots = expired.clone();
        drop(by_id);
        for sub in &snapshots {
            if let Err(e) = self.persist(sub) {
                tracing::error!("Failed to persist subscription {} expiry: {}", sub.id, e);
            }
        }
        expired
    }

    /// 扫描到期的周期重置：active 且 next_reset_time 落在 now 及以前。
    /// 实际的重置执行（查 plan 算新周期点）由调用方完成——本方法只给候选。
    pub fn list_reset_due(&self, now: i64) -> Vec<UserSubscription> {
        self.by_id
            .read()
            .values()
            .filter(|s| {
                s.status == "active"
                    && s.next_reset_time > 0
                    && s.next_reset_time <= now
                    && s.end_time > now
            })
            .cloned()
            .collect()
    }

    /// 执行周期重置：清空已用配额并写入新的重置周期锚点。
    pub fn apply_reset(&self, id: &str, last_reset: i64, next_reset: i64) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let mut by_id = self.by_id.write();
        let sub = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("subscription not found"))?;
        sub.amount_used = 0;
        sub.last_reset_time = last_reset;
        sub.next_reset_time = next_reset;
        sub.updated_at = now;
        let snapshot = sub.clone();
        drop(by_id);
        self.persist(&snapshot)
    }

    /// 取消订阅（管理员操作）：status=cancelled，end_time 截断到 now。
    pub fn cancel(&self, id: &str, now: i64) -> Result<UserSubscription> {
        let mut by_id = self.by_id.write();
        let sub = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("subscription not found"))?;
        sub.status = "cancelled".into();
        sub.end_time = now;
        sub.updated_at = now;
        let snapshot = sub.clone();
        drop(by_id);
        self.persist(&snapshot)?;
        Ok(snapshot)
    }
}

/// 订阅结束（到期/取消）后的用户分组回退（对齐 new-api
/// downgradeUserGroupForSubscriptionTx）。
///
/// 规则：
/// 1. 显式 downgrade_group 优先；否则仅在当前分组仍是升级分组时回退到
///    购买前分组（prev_user_group）。
/// 2. 用户还有其他活跃的升级订阅时不回退（保持最高权益）。
///
/// 返回回退到的分组名（空 = 无回退）。
pub fn downgrade_user_group(
    user_store: &UserStore,
    sub_store: &SubscriptionStore,
    sub: &UserSubscription,
    now: i64,
) -> Result<String> {
    let downgrade = sub.downgrade_group.trim().to_string();
    let upgrade = sub.upgrade_group.trim().to_string();
    if downgrade.is_empty() && upgrade.is_empty() {
        return Ok(String::new());
    }
    // 其他活跃升级订阅存在 → 保持现分组
    let has_other_active_upgrade = sub_store
        .find_active(&sub.user_id, now)
        .iter()
        .any(|s| s.id != sub.id && !s.upgrade_group.trim().is_empty());
    if has_other_active_upgrade {
        return Ok(String::new());
    }
    let current = match user_store.get_by_id(&sub.user_id) {
        Some(u) => u.group.clone(),
        None => return Ok(String::new()),
    };
    let target = if !downgrade.is_empty() {
        downgrade
    } else {
        // 无显式回退目标：仅当订阅确实升级过分组才回退（当前分组必须仍是升级分组）
        if upgrade.is_empty() || current != upgrade {
            return Ok(String::new());
        }
        sub.prev_user_group.trim().to_string()
    };
    if target.is_empty() || target == current {
        return Ok(String::new());
    }
    user_store.update(&sub.user_id, |u| u.group = target.clone())?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Datelike;
    use tempfile::TempDir;

    fn sub_store() -> SubscriptionStore {
        SubscriptionStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn user_store() -> UserStore {
        UserStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn plan(unit: &str, value: i64, custom: i64) -> Plan {
        Plan {
            id: "p1".into(),
            name: "月度会员".into(),
            price: 9.9,
            quota: 0,
            duration_days: 0,
            group: "vip".into(),
            allowed_models: None,
            description: String::new(),
            enabled: true,
            issued_count: 0,
            created_at: 0,
            updated_at: 0,
            plan_type: "subscription".into(),
            duration_unit: unit.into(),
            duration_value: value,
            custom_seconds: custom,
            total_amount: 1000,
            quota_reset_period: "never".into(),
            quota_reset_custom_seconds: 0,
            allow_balance_pay: true,
            allow_wallet_overflow: true,
            max_purchase_per_user: 0,
            upgrade_group: String::new(),
            downgrade_group: String::new(),
            sort_order: 0,
        }
    }

    fn active_sub(store: &SubscriptionStore, total: i64, end_offset: i64) -> UserSubscription {
        let now = chrono::Utc::now().timestamp();
        store
            .create(UserSubscription {
                id: uuid::Uuid::new_v4().to_string(),
                user_id: "u1".into(),
                plan_id: "p1".into(),
                amount_total: total,
                amount_used: 0,
                start_time: now,
                end_time: now + end_offset,
                status: "active".into(),
                source: "balance".into(),
                last_reset_time: 0,
                next_reset_time: 0,
                upgrade_group: String::new(),
                prev_user_group: String::new(),
                downgrade_group: String::new(),
                allow_wallet_overflow: true,
                created_at: now,
                updated_at: now,
            })
            .unwrap()
    }

    #[test]
    fn calc_plan_end_units() {
        let start = chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .timestamp();
        // 1 天
        assert_eq!(
            calc_plan_end(start, &plan("day", 1, 0)).unwrap(),
            start + 86_400
        );
        // 2 小时
        assert_eq!(
            calc_plan_end(start, &plan("hour", 2, 0)).unwrap(),
            start + 7_200
        );
        // custom 90 秒
        assert_eq!(
            calc_plan_end(start, &plan("custom", 0, 90)).unwrap(),
            start + 90
        );
        // 1 年 = 日历进位
        let end = calc_plan_end(start, &plan("year", 1, 0)).unwrap();
        let (y1, y2) = (
            chrono::DateTime::from_timestamp(start, 0)
                .unwrap()
                .date_naive()
                .year(),
            chrono::DateTime::from_timestamp(end, 0)
                .unwrap()
                .date_naive()
                .year(),
        );
        assert_eq!(y2, y1 + 1);
        // 1 月
        let end = calc_plan_end(start, &plan("month", 1, 0)).unwrap();
        assert!(end > start && end <= start + 32 * 86_400);
        // 非法输入
        assert!(calc_plan_end(start, &plan("month", 0, 0)).is_err());
        assert!(calc_plan_end(start, &plan("custom", 0, 0)).is_err());
        assert!(calc_plan_end(start, &plan("week", 1, 0)).is_err());
    }

    #[test]
    fn calc_next_reset_periods() {
        let start = chrono::DateTime::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .timestamp();
        // never → 0
        assert_eq!(calc_next_reset(start, &plan("month", 1, 0), start + 10), 0);
        // never 之外必然给出未来时间点
        let mut daily = plan("month", 1, 0);
        daily.quota_reset_period = "daily".into();
        let next = calc_next_reset(start, &daily, start + 10 * 86_400);
        assert!(next > start && next <= start + 86_400);
        // 周期点超出订阅期 → 0
        let short = calc_next_reset(start, &daily, start + 100);
        assert_eq!(short, 0);
        // custom 周期
        let mut custom = plan("month", 1, 0);
        custom.quota_reset_period = "custom".into();
        custom.quota_reset_custom_seconds = 3600;
        assert_eq!(
            calc_next_reset(start, &custom, start + 10_000),
            start + 3600
        );
    }

    #[test]
    fn charge_reserve_settle_release() {
        let s = sub_store();
        let sub = active_sub(&s, 1000, 3600);
        // 直接扣
        assert!(s.try_charge(&sub.id, 300));
        assert_eq!(s.get(&sub.id).unwrap().amount_used, 300);
        // 超池拒绝
        assert!(!s.try_charge(&sub.id, 800));
        // 预留 → 结算（实际少于预留）
        assert!(s.try_reserve(&sub.id, 200));
        s.settle(&sub.id, 200, 150);
        assert_eq!(s.get(&sub.id).unwrap().amount_used, 450);
        // 预留 → 释放（无消费归还）
        assert!(s.try_reserve(&sub.id, 100));
        s.release(&sub.id, 100);
        assert_eq!(s.get(&sub.id).unwrap().amount_used, 450);
    }

    #[test]
    fn unlimited_pool_allows_any_amount() {
        let s = sub_store();
        let sub = active_sub(&s, 0, 3600); // total=0 不限
        assert!(s.try_charge(&sub.id, 1_000_000));
        // 过期后拒绝扣减
        let mut expired = s.get(&sub.id).unwrap();
        expired.end_time = chrono::Utc::now().timestamp() - 1;
        s.create(expired).unwrap();
        assert!(!s.try_charge(&sub.id, 1));
    }

    #[test]
    fn find_active_orders_by_end_time() {
        let s = sub_store();
        let now = chrono::Utc::now().timestamp();
        let early = active_sub(&s, 0, 600);
        let late = active_sub(&s, 0, 3600);
        let list = s.find_active("u1", now);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, early.id, "最早到期的排前面");
        // 无关用户不可见
        assert!(s.find_active("u2", now).is_empty());
        // 已过期的不出现
        let mut e = late.clone();
        e.status = "expired".into();
        s.create(e).unwrap();
        assert_eq!(s.find_active("u1", now).len(), 1);
    }

    #[test]
    fn expire_due_marks_and_returns() {
        let s = sub_store();
        let now = chrono::Utc::now().timestamp();
        let mut sub = active_sub(&s, 100, 3600);
        sub.end_time = now - 1;
        s.create(sub).unwrap();
        let expired = s.expire_due(now);
        assert_eq!(expired.len(), 1);
        assert_eq!(s.get(&expired[0].id).unwrap().status, "expired");
        // 再次扫描无新增
        assert!(s.expire_due(now).is_empty());
    }

    #[test]
    fn reset_due_and_apply() {
        let s = sub_store();
        let mut sub = active_sub(&s, 500, 10 * 86_400);
        sub.next_reset_time = chrono::Utc::now().timestamp() - 10;
        let sub_id = sub.id.clone();
        s.create(sub).unwrap();
        s.try_charge(&sub_id, 400);
        let due = s.list_reset_due(chrono::Utc::now().timestamp());
        assert_eq!(due.len(), 1);
        s.apply_reset(
            &due[0].id,
            due[0].next_reset_time,
            due[0].next_reset_time + 86_400,
        )
        .unwrap();
        let after = s.get(&sub_id).unwrap();
        assert_eq!(after.amount_used, 0);
        assert!(after.next_reset_time > due[0].next_reset_time);
    }

    #[test]
    fn count_by_plan_for_purchase_limit() {
        let s = sub_store();
        active_sub(&s, 0, 3600);
        active_sub(&s, 0, 3600);
        assert_eq!(s.count_by_plan("u1", "p1"), 2);
        assert_eq!(s.count_by_plan("u1", "p2"), 0);
    }

    #[test]
    fn wallet_overflow_blocked_by_any_strict_sub() {
        let s = sub_store();
        let now = chrono::Utc::now().timestamp();
        assert!(s.wallet_overflow_allowed("u1", now), "无订阅时允许钱包");
        let mut strict = active_sub(&s, 100, 3600);
        strict.allow_wallet_overflow = false;
        s.create(strict).unwrap();
        assert!(!s.wallet_overflow_allowed("u1", now));
        // 过期后恢复允许
        let mut e = s.list_by_user("u1")[0].clone();
        e.end_time = now - 1;
        s.create(e).unwrap();
        assert!(s.wallet_overflow_allowed("u1", now));
    }

    #[test]
    fn downgrade_reverts_to_prev_group() {
        let us = user_store();
        let ss = sub_store();
        let now = chrono::Utc::now().timestamp();
        let user = us
            .create("dg@test.com", "pw", crate::user::Role::User, 100)
            .unwrap();
        us.update(&user.id, |u| u.group = "vip".into()).unwrap();
        let mut sub = active_sub(&ss, 0, 3600);
        sub.user_id = user.id.clone();
        sub.upgrade_group = "vip".into();
        sub.prev_user_group = "default".into();
        sub.end_time = now - 1;
        sub.status = "active".into();
        let created = ss.create(sub).unwrap();
        let target = downgrade_user_group(&us, &ss, &created, now).unwrap();
        assert_eq!(target, "default");
        assert_eq!(us.get_by_id(&user.id).unwrap().group, "default");
    }

    #[test]
    fn downgrade_kept_when_other_active_upgrade_exists() {
        let us = user_store();
        let ss = sub_store();
        let now = chrono::Utc::now().timestamp();
        let user = us
            .create("dg2@test.com", "pw", crate::user::Role::User, 100)
            .unwrap();
        us.update(&user.id, |u| u.group = "vip".into()).unwrap();
        // 过期的订阅 A
        let mut a = active_sub(&ss, 0, 3600);
        a.user_id = user.id.clone();
        a.upgrade_group = "vip".into();
        a.prev_user_group = "default".into();
        a.end_time = now - 1;
        let created_a = ss.create(a).unwrap();
        // 仍活跃的订阅 B（也升级到 vip）
        let mut b = active_sub(&ss, 0, 3600);
        b.user_id = user.id.clone();
        b.upgrade_group = "vip".into();
        b.prev_user_group = "default".into();
        ss.create(b).unwrap();
        let target = downgrade_user_group(&us, &ss, &created_a, now).unwrap();
        assert_eq!(target, "", "有其他活跃升级订阅时不回退");
        assert_eq!(us.get_by_id(&user.id).unwrap().group, "vip");
    }

    #[test]
    fn downgrade_explicit_target_wins() {
        let us = user_store();
        let ss = sub_store();
        let now = chrono::Utc::now().timestamp();
        let user = us
            .create("dg3@test.com", "pw", crate::user::Role::User, 100)
            .unwrap();
        us.update(&user.id, |u| u.group = "vip".into()).unwrap();
        let mut sub = active_sub(&ss, 0, 3600);
        sub.user_id = user.id.clone();
        sub.upgrade_group = "vip".into();
        sub.prev_user_group = "default".into();
        sub.downgrade_group = "basic".into();
        let created = ss.create(sub).unwrap();
        let target = downgrade_user_group(&us, &ss, &created, now).unwrap();
        assert_eq!(target, "basic");
        assert_eq!(us.get_by_id(&user.id).unwrap().group, "basic");
    }
}
