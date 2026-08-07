//! 兑换码/充值码系统 — 批量生成、兑换入账。
//!
//! 参照 new-api model/redemption.go 的 Redemption 模型：
//! Key（兑换码）/Status（1=未使用,2=已使用,3=禁用）/Name/Quota/CreatedTime/RedeemedTime/
//! UsedUserId/ExpiredTime。
//!
//! 持久化使用 FileStore KV：key 前缀 `redemption:{id}`，并维护 `redemption:code:{code}` → id 索引。

use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

// ── Redemption ─────────────────────────────────────────────────────

/// 兑换码状态。
pub const STATUS_UNUSED: i32 = 1;
pub const STATUS_USED: i32 = 2;
pub const STATUS_DISABLED: i32 = 3;

/// 兑换码记录。
///
/// 参照 new-api Redemption。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Redemption {
    /// 唯一 ID（uuid）
    pub id: String,
    /// 兑换码（用户输入）
    pub code: String,
    /// 名称（备注）
    #[serde(default)]
    pub name: String,
    /// 面额（配额单位）
    #[serde(default = "default_quota")]
    pub quota: i64,
    /// 状态：1=未使用, 2=已使用, 3=禁用
    #[serde(default = "default_status")]
    pub status: i32,
    /// 使用者用户 ID
    #[serde(default)]
    pub used_by: Option<String>,
    /// 兑换时间
    #[serde(default)]
    pub used_at: Option<i64>,
    /// 创建时间
    #[serde(default)]
    pub created_at: i64,
    /// 过期时间（0=永不过期）
    #[serde(default)]
    pub expires_at: i64,
}

fn default_quota() -> i64 {
    100
}

fn default_status() -> i32 {
    STATUS_UNUSED
}

impl Redemption {
    pub fn new(code: impl Into<String>, quota: i64, name: impl Into<String>, expires_at: i64) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            code: code.into(),
            name: name.into(),
            quota,
            status: STATUS_UNUSED,
            used_by: None,
            used_at: None,
            created_at: chrono::Utc::now().timestamp(),
            expires_at,
        }
    }

    /// 是否可兑换（未使用 + 未禁用 + 未过期）
    pub fn is_redeemable(&self) -> bool {
        if self.status != STATUS_UNUSED {
            return false;
        }
        if self.expires_at > 0 && chrono::Utc::now().timestamp() >= self.expires_at {
            return false;
        }
        true
    }

    /// 是否已过期
    pub fn is_expired(&self) -> bool {
        self.expires_at > 0 && chrono::Utc::now().timestamp() >= self.expires_at
    }

    /// 状态文本
    pub fn status_text(&self) -> &'static str {
        match self.status {
            STATUS_UNUSED => if self.is_expired() { "expired" } else { "unused" },
            STATUS_USED => "used",
            STATUS_DISABLED => "disabled",
            _ => "unknown",
        }
    }
}

// ── RedemptionStore ────────────────────────────────────────────────

/// 兑换码存储。
///
/// key 格式：
/// - `redemption:{id}` — 兑换码记录
/// - `redemption_code:{code}` — code → id 索引（用于按 code 快速查找）
pub struct RedemptionStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, Redemption>>,
    by_code: RwLock<HashMap<String, String>>, // code -> id
}

impl RedemptionStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
            by_code: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.store.list("redemption:")?;
        let mut by_id = self.by_id.write();
        let mut by_code = self.by_code.write();
        by_id.clear();
        by_code.clear();
        for key in &keys {
            if let Some(r) = self.store.get::<Redemption>(key)? {
                by_id.insert(r.id.clone(), r.clone());
                by_code.insert(r.code.clone(), r.id.clone());
            }
        }
        Ok(())
    }

    fn persist(&self, r: &Redemption) -> anyhow::Result<()> {
        self.store.put(&format!("redemption:{}", r.id), r)?;
        self.store.put(&format!("redemption_code:{}", r.code), &r.id)?;
        Ok(())
    }

    /// 生成随机兑换码（16 位大写字母数字，去除易混淆字符 0/O/1/I）
    pub fn generate_code() -> String {
        const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let mut rng = rand::thread_rng();
        (0..16)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect()
    }

    /// 批量生成兑换码。
    ///
    /// 返回生成的 Redemption 列表。若 quota <= 0 则报错。
    pub fn batch_generate(
        &self,
        count: usize,
        quota: i64,
        name: &str,
        expires_at: i64,
    ) -> anyhow::Result<Vec<Redemption>> {
        if count == 0 {
            anyhow::bail!("count must be positive");
        }
        if quota <= 0 {
            anyhow::bail!("quota must be positive");
        }
        let mut generated = Vec::with_capacity(count);
        let mut by_id = self.by_id.write();
        let mut by_code = self.by_code.write();
        for _ in 0..count {
            // 生成唯一 code（重试避免冲突）
            let mut attempts = 0;
            let code = loop {
                let c = Self::generate_code();
                if !by_code.contains_key(&c) {
                    break c;
                }
                attempts += 1;
                if attempts > 100 {
                    anyhow::bail!("failed to generate unique code after 100 attempts");
                }
            };
            let r = Redemption::new(code, quota, name, expires_at);
            self.persist(&r)?;
            by_id.insert(r.id.clone(), r.clone());
            by_code.insert(r.code.clone(), r.id.clone());
            generated.push(r);
        }
        Ok(generated)
    }

    /// 列出所有兑换码（按创建时间倒序）
    pub fn list(&self) -> Vec<Redemption> {
        let mut all: Vec<Redemption> = self.by_id.read().values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all
    }

    /// 分页查询
    pub fn list_paged(&self, page: usize, size: usize) -> (Vec<Redemption>, usize) {
        let all = self.list();
        let total = all.len();
        let page = page.max(1);
        let size = size.max(1);
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            all[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }

    /// 按 code 查找
    pub fn get_by_code(&self, code: &str) -> Option<Redemption> {
        let id = self.by_code.read().get(code)?.clone();
        self.by_id.read().get(&id).cloned()
    }

    /// 按 id 查找
    pub fn get(&self, id: &str) -> Option<Redemption> {
        self.by_id.read().get(id).cloned()
    }

    /// 删除未使用的兑换码。
    ///
    /// 已使用的兑换码不允许删除（保留审计轨迹）。
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        let r = self
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("redemption not found"))?;
        if r.status == STATUS_USED {
            anyhow::bail!("cannot delete used redemption");
        }
        self.store.delete(&format!("redemption:{}", r.id))?;
        self.store.delete(&format!("redemption_code:{}", r.code))?;
        self.by_id.write().remove(id);
        self.by_code.write().remove(&r.code);
        Ok(())
    }

    /// 兑换码兑换。
    ///
    /// 校验有效性（未使用/未过期），标记已使用，返回面额（quota）。
    /// 调用方负责将 quota 加到用户余额（user_store.add_quota）。
    pub fn redeem(&self, code: &str, user_id: &str) -> anyhow::Result<i64> {
        let mut by_id = self.by_id.write();
        let r = by_id
            .get_mut(
                self.by_code
                    .read()
                    .get(code)
                    .ok_or_else(|| anyhow::anyhow!("invalid redemption code"))?,
            )
            .ok_or_else(|| anyhow::anyhow!("redemption not found"))?;
        if r.status != STATUS_UNUSED {
            anyhow::bail!("redemption code already used or disabled");
        }
        if r.expires_at > 0 && chrono::Utc::now().timestamp() >= r.expires_at {
            anyhow::bail!("redemption code expired");
        }
        let quota = r.quota;
        r.status = STATUS_USED;
        r.used_by = Some(user_id.to_string());
        r.used_at = Some(chrono::Utc::now().timestamp());
        let snapshot = r.clone();
        drop(by_id);
        if let Err(e) = self.store.put(&format!("redemption:{}", snapshot.id), &snapshot) {
            tracing::error!("Failed to persist redemption {} redeem: {}", snapshot.id, e);
        }
        Ok(quota)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> RedemptionStore {
        RedemptionStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn batch_generate_and_list() {
        let s = store();
        let codes = s.batch_generate(5, 100, "test", 0).unwrap();
        assert_eq!(codes.len(), 5);
        assert_eq!(s.list().len(), 5);
        // codes 应唯一
        let mut seen = std::collections::HashSet::new();
        for c in &codes {
            assert!(seen.insert(&c.code));
        }
    }

    #[test]
    fn redeem_success() {
        let s = store();
        let codes = s.batch_generate(1, 200, "test", 0).unwrap();
        let code = &codes[0].code;
        let quota = s.redeem(code, "user-1").unwrap();
        assert_eq!(quota, 200);
        let r = s.get_by_code(code).unwrap();
        assert_eq!(r.status, STATUS_USED);
        assert_eq!(r.used_by.as_deref(), Some("user-1"));
    }

    #[test]
    fn redeem_twice_fails() {
        let s = store();
        let codes = s.batch_generate(1, 100, "test", 0).unwrap();
        let code = codes[0].code.clone();
        s.redeem(&code, "user-1").unwrap();
        assert!(s.redeem(&code, "user-2").is_err());
    }

    #[test]
    fn redeem_expired_fails() {
        let s = store();
        let past = chrono::Utc::now().timestamp() - 100;
        let codes = s.batch_generate(1, 100, "test", past).unwrap();
        assert!(s.redeem(&codes[0].code, "user-1").is_err());
    }

    #[test]
    fn delete_unused_only() {
        let s = store();
        let codes = s.batch_generate(2, 100, "test", 0).unwrap();
        s.redeem(&codes[0].code, "u1").unwrap();
        // 已使用不能删
        assert!(s.delete(&codes[0].id).is_err());
        // 未使用可以删
        assert!(s.delete(&codes[1].id).is_ok());
        assert_eq!(s.list().len(), 1);
    }
}