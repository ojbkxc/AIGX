//! 签到系统 — 每日随机配额奖励（对齐 new-api model/checkin.go）。
//!
//! KV 布局：`checkin:{user_id}:{YYYY-MM-DD}`（键本身即唯一索引，
//! 天然保证「同用户同日只签一次」）。settings 控制开关与奖励区间。

use anyhow::Result;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::storage::FileStore;

/// 签到设置（对齐 new-api CheckinSetting）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckinSetting {
    /// 是否启用签到功能
    #[serde(default)]
    pub enabled: bool,
    /// 签到最小奖励配额
    #[serde(default = "default_min_quota")]
    pub min_quota: i64,
    /// 签到最大奖励配额
    #[serde(default = "default_max_quota")]
    pub max_quota: i64,
}

fn default_min_quota() -> i64 {
    1000
}

fn default_max_quota() -> i64 {
    5000
}

impl Default for CheckinSetting {
    fn default() -> Self {
        Self {
            enabled: false,
            min_quota: default_min_quota(),
            max_quota: default_max_quota(),
        }
    }
}

/// 签到记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkin {
    pub user_id: String,
    /// 签到日期（YYYY-MM-DD）
    pub checkin_date: String,
    pub quota_awarded: i64,
    pub created_at: i64,
}

/// 签到存储
pub struct CheckinStore {
    store: Arc<FileStore>,
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn month_of(date: &str) -> String {
    // "YYYY-MM-DD" → "YYYY-MM"
    date.get(..7).unwrap_or(date).to_string()
}

impl CheckinStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }

    fn key(user_id: &str, date: &str) -> String {
        format!("checkin:{user_id}:{date}")
    }

    /// 今日是否已签到
    pub fn has_checked_today(&self, user_id: &str) -> Result<bool> {
        let key = Self::key(user_id, &today());
        Ok(self.store.get::<Checkin>(&key)?.is_some())
    }

    /// 执行签到：同日重复签到返回 Err("今日已签到")。
    /// 成功时写入记录（键含日期 = 天然幂等）并返回奖励配额。
    pub fn checkin(&self, user_id: &str, setting: &CheckinSetting) -> Result<Checkin> {
        let date = today();
        let key = Self::key(user_id, &date);
        if self.store.get::<Checkin>(&key)?.is_some() {
            anyhow::bail!("今日已签到");
        }
        // 随机奖励 [min, max]
        let awarded = if setting.max_quota > setting.min_quota {
            rand::thread_rng().gen_range(setting.min_quota..=setting.max_quota)
        } else {
            setting.min_quota
        };
        let rec = Checkin {
            user_id: user_id.to_string(),
            checkin_date: date,
            quota_awarded: awarded,
            created_at: chrono::Utc::now().timestamp(),
        };
        // 先写记录再加配额：写失败时不入账；写成功后 add_quota 也有持久化，
        // 极端场景（写记录成功、入账失败）下次补试也不会重复奖励（键已存在）。
        self.store.put(&key, &rec)?;
        Ok(rec)
    }

    /// 指定月份的签到记录（默认当月），按日期升序
    pub fn list_month(&self, user_id: &str, month: Option<&str>) -> Result<Vec<Checkin>> {
        let month = month
            .map(|m| m.to_string())
            .unwrap_or_else(|| month_of(&today()));
        let prefix = format!("checkin:{user_id}:");
        let mut list = Vec::new();
        for key in self.store.list(&prefix)? {
            if let Some(rec) = self.store.get::<Checkin>(&key)? {
                if month_of(&rec.checkin_date) == month {
                    list.push(rec);
                }
            }
        }
        list.sort_by(|a, b| a.checkin_date.cmp(&b.checkin_date));
        Ok(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup() -> (CheckinStore, CheckinSetting) {
        let store = CheckinStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )));
        let setting = CheckinSetting {
            enabled: true,
            min_quota: 100,
            max_quota: 200,
        };
        (store, setting)
    }

    #[test]
    fn checkin_once_per_day() {
        let (s, setting) = setup();
        let rec = s.checkin("u1", &setting).unwrap();
        assert!(rec.quota_awarded >= 100 && rec.quota_awarded <= 200);
        // 同日重复签到被拒
        assert!(s.checkin("u1", &setting).is_err());
        assert!(s.has_checked_today("u1").unwrap());
    }

    #[test]
    fn month_list_only_self() {
        let (s, setting) = setup();
        s.checkin("u1", &setting).unwrap();
        s.checkin("u2", &setting).unwrap();
        let list = s.list_month("u1", None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].user_id, "u1");
    }
}
