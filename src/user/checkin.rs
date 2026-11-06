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
    20
}

fn default_max_quota() -> i64 {
    20
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

/// 签到失败原因（供调用方区分重试语义与 HTTP 状态码）
#[derive(Debug)]
pub enum CheckinError {
    /// 今日已签到（含并发双击败者——入账已冲回，重试无意义）
    AlreadyChecked,
    /// 入账失败：未落签到键，调用方可安全重试
    AwardFailed(String),
    /// KV 落键失败：入账已冲回，调用方可安全重试
    StoreFailed(String),
}

impl std::fmt::Display for CheckinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckinError::AlreadyChecked => write!(f, "今日已签到"),
            CheckinError::AwardFailed(m) => write!(f, "签到入账失败: {m}"),
            CheckinError::StoreFailed(m) => write!(f, "签到落库失败: {m}"),
        }
    }
}

impl std::error::Error for CheckinError {}

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

/// 入账冲回：CAS 败者/落键失败时把已入账的奖励原额收回。
/// 失败仅记 error（罕见——同一存储刚刚成功过），不掩盖原始错误。
fn try_refund(user_id: &str, amount: i64, award: &mut impl FnMut(i64) -> Result<()>) {
    if let Err(e) = award(-amount) {
        tracing::error!("Checkin refund failed for {user_id} ({amount} quota): {e}");
    }
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

    /// 执行签到：同日重复签到返回 [`CheckinError::AlreadyChecked`]。
    ///
    /// 入账顺序：**先 `award` 入账、成功后再 CAS 落签到键**。
    /// 这样入账失败（用户存储异常）时不写签到键，用户当日可重试，
    /// 不会出现「钱没到账但显示已签到」的死键（回归：原先先落键后入账，
    /// 入账失败不回滚记录，当日再点直接被拒）。
    ///
    /// 幂等由 `put_if_absent`（SQLite ON CONFLICT DO NOTHING）保证：
    /// 并发双击时只有一个请求写入成功；败者已先行入账，因此 CAS 失败
    /// 时会**原额冲回**（`award(-amount)`）再拒绝——净效果仍是
    /// 「一次签到一笔奖励」。落键失败（KV 异常）时同样冲回。
    pub fn checkin(
        &self,
        user_id: &str,
        setting: &CheckinSetting,
        mut award: impl FnMut(i64) -> Result<()>,
    ) -> Result<Checkin, CheckinError> {
        let date = today();
        let key = Self::key(user_id, &date);
        // 快速路径：已签到直接拒绝，不再入账（避免无谓的冲回往返）
        if self
            .has_checked_today(user_id)
            .map_err(|e| CheckinError::StoreFailed(e.to_string()))?
        {
            return Err(CheckinError::AlreadyChecked);
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
        // 1) 先入账：失败则不落签到键，调用方可安全重试
        if let Err(e) = award(rec.quota_awarded) {
            return Err(CheckinError::AwardFailed(e.to_string()));
        }
        // 2) 再 CAS 落签到键（并发双击只有一个写入成功）
        match self
            .store
            .put_if_absent(&key, &rec)
            .map_err(|e| CheckinError::StoreFailed(e.to_string()))
        {
            Ok(true) => Ok(rec),
            Ok(false) => {
                // 并发先到：本请求已入账，原额冲回后按"已签到"拒绝
                try_refund(user_id, rec.quota_awarded, &mut award);
                Err(CheckinError::AlreadyChecked)
            }
            Err(e) => {
                // 落键失败（事务保证未写入）：冲回已入账的奖励，避免重试双发
                try_refund(user_id, rec.quota_awarded, &mut award);
                Err(e)
            }
        }
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

    /// 全部签到记录（不限月份），按时间倒序（最新优先）。
    ///
    /// 用于用户侧积分流水端点：与 API 消耗明细合并成一条时间线后分页。
    /// `list_month` 只给当月，跨月流水会漏；这里按 user_id 前缀扫全部键。
    pub fn list_all(&self, user_id: &str) -> Result<Vec<Checkin>> {
        let prefix = format!("checkin:{user_id}:");
        let mut list = Vec::new();
        for key in self.store.list(&prefix)? {
            if let Some(rec) = self.store.get::<Checkin>(&key)? {
                list.push(rec);
            }
        }
        list.sort_by_key(|a| std::cmp::Reverse(a.created_at));
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
        let rec = s.checkin("u1", &setting, |_| Ok(())).unwrap();
        assert!(rec.quota_awarded >= 100 && rec.quota_awarded <= 200);
        // 同日重复签到被拒
        assert!(s.checkin("u1", &setting, |_| Ok(())).is_err());
        assert!(s.has_checked_today("u1").unwrap());
    }

    #[test]
    fn month_list_only_self() {
        let (s, setting) = setup();
        s.checkin("u1", &setting, |_| Ok(())).unwrap();
        s.checkin("u2", &setting, |_| Ok(())).unwrap();
        let list = s.list_month("u1", None).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].user_id, "u1");
    }

    /// 回归：入账失败不落签到键，当日可安全重试。
    /// （原先先落键后入账：入账失败留下「已签到」死键，当日再点直接被拒。）
    #[test]
    fn award_failure_leaves_key_absent_for_retry() {
        let (s, setting) = setup();
        let err = s
            .checkin("u1", &setting, |_| anyhow::bail!("db down"))
            .unwrap_err();
        assert!(matches!(err, CheckinError::AwardFailed(_)));
        assert!(!s.has_checked_today("u1").unwrap(), "入账失败不得落签到键");
        // 当日重试成功
        let rec = s.checkin("u1", &setting, |_| Ok(())).unwrap();
        assert!(s.has_checked_today("u1").unwrap());
        assert_eq!(rec.user_id, "u1");
    }

    /// 并发双击：两个请求同时签到，净入账恰为一笔奖励（败者冲回），
    /// 且只有一条签到记录。
    #[test]
    fn concurrent_double_click_awards_once() {
        let fs = Arc::new(FileStore::new(TempDir::new().unwrap().path().to_path_buf()));
        let s1 = CheckinStore::new(fs.clone());
        let s2 = CheckinStore::new(fs.clone());
        let probe = CheckinStore::new(fs); // 线程用掉 s1/s2 后仍可查询
        let setting = CheckinSetting {
            enabled: true,
            min_quota: 100,
            max_quota: 200,
        };
        let awards: Arc<std::sync::Mutex<Vec<i64>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut handles = Vec::new();
        for s in [s1, s2] {
            let awards = awards.clone();
            let setting = setting.clone();
            handles.push(std::thread::spawn(move || {
                s.checkin("u1", &setting, |amt| {
                    awards.lock().unwrap().push(amt);
                    Ok(())
                })
            }));
        }
        let results: Vec<Result<Checkin, CheckinError>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();
        // 恰好一个成功、一个被拒
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        // 净入账 == 成功那笔的奖励（败者 +b 入账后 -b 冲回）
        let net: i64 = awards.lock().unwrap().iter().sum();
        let winner = results
            .iter()
            .find_map(|r| r.as_ref().ok().cloned())
            .unwrap();
        assert_eq!(net, winner.quota_awarded);
        // 只有一条签到记录
        let list = probe.list_month("u1", None).unwrap();
        assert_eq!(list.len(), 1);
    }
}
