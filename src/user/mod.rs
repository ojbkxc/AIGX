//! 用户系统 — 多用户与配额账本。
//!
//! 仿 new-api 的用户与额度模型：每个用户拥有可消费配额 (quota) 与已用配额
//! (used_quota)。API Key 可绑定到用户，调用推理时按 token 估算扣费。
//! 管理员通过 /api/users 进行用户管理，并通过易支付充值订单入账。

use anyhow::Result;
use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 用户角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

impl Default for Role {
    fn default() -> Self {
        Role::User
    }
}

/// 用户记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub username: String,
    /// 密码哈希 (SHA256 hex)
    pub password: String,
    #[serde(default)]
    pub role: Role,
    /// 总配额（充值 + 赠送）
    #[serde(default)]
    pub quota: i64,
    /// 已用配额
    #[serde(default)]
    pub used_quota: i64,
    /// 状态: active / disabled
    #[serde(default = "default_status")]
    pub status: String,
    #[serde(default)]
    pub created_at: i64,
}

fn default_status() -> String {
    "active".to_string()
}

impl User {
    /// 剩余可用配额
    pub fn remaining(&self) -> i64 {
        (self.quota - self.used_quota).max(0)
    }

    /// 是否为管理员
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }
}

/// 用户存储
pub struct UserStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, User>>,
    by_name: RwLock<HashMap<String, String>>, // username -> id
}

impl UserStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
            by_name: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("user:")?;
        let mut by_id = self.by_id.write();
        let mut by_name = self.by_name.write();
        by_id.clear();
        by_name.clear();
        for key in &keys {
            if let Some(u) = self.store.get::<User>(key)? {
                by_id.insert(u.id.clone(), u.clone());
                by_name.insert(u.username.clone(), u.id);
            }
        }
        Ok(())
    }

    fn persist(&self, user: &User) -> Result<()> {
        self.store.put(&format!("user:{}", user.id), user)?;
        Ok(())
    }

    /// 创建用户
    pub fn create(&self, username: &str, password: &str, role: Role, quota: i64) -> Result<User> {
        if username.is_empty() {
            anyhow::bail!("username cannot be empty");
        }
        if self.by_name.read().contains_key(username) {
            anyhow::bail!("username already exists");
        }
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            username: username.to_string(),
            password: hash_password(password),
            role,
            quota,
            used_quota: 0,
            status: "active".into(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_name.write().insert(user.username.clone(), user.id);
        Ok(user)
    }

    pub fn list(&self) -> Vec<User> {
        let mut users: Vec<User> = self.by_id.read().values().cloned().collect();
        users.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        users
    }

    pub fn get_by_id(&self, id: &str) -> Option<User> {
        self.by_id.read().get(id).cloned()
    }

    pub fn get_by_username(&self, name: &str) -> Option<User> {
        let id = self.by_name.read().get(name)?.clone();
        self.by_id.read().get(&id).cloned()
    }

    /// 校验密码并返回用户
    pub fn authenticate(&self, username: &str, password: &str) -> Option<User> {
        let user = self.get_by_username(username)?;
        if user.status != "active" {
            return None;
        }
        if hash_password(password) == user.password {
            Some(user)
        } else {
            None
        }
    }

    pub fn update(&self, id: &str, mutator: impl FnOnce(&mut User)) -> Result<User> {
        let mut user = self
            .by_id
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("user not found"))?;
        let old_username = user.username.clone();
        mutator(&mut user);
        if user.username != old_username
            && self.by_name.read().contains_key(&user.username)
            && self.by_name.read().get(&user.username) != Some(&id.to_string())
        {
            anyhow::bail!("username already exists");
        }
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        let mut by_name = self.by_name.write();
        by_name.remove(&old_username);
        by_name.insert(user.username.clone(), user.id);
        Ok(user)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let user = self.get_by_id(id);
        if let Some(u) = user {
            self.store.delete(&format!("user:{id}"))?;
            self.by_id.write().remove(id);
            self.by_name.write().remove(&u.username);
        }
        Ok(())
    }

    /// 增加配额（充值入账）
    pub fn add_quota(&self, id: &str, amount: i64) -> Result<()> {
        self.update(id, |u| {
            u.quota += amount;
        })?;
        Ok(())
    }

    /// 扣除已用配额，返回是否成功（余额不足则不扣）
    pub fn try_charge(&self, id: &str, amount: i64) -> bool {
        if amount <= 0 {
            return true;
        }
        let mut by_id = self.by_id.write();
        let user = match by_id.get_mut(id) {
            Some(u) => u,
            None => return false,
        };
        if user.remaining() < amount {
            return false;
        }
        user.used_quota += amount;
        let snapshot = user.clone();
        drop(by_id);
        let _ = self.persist(&snapshot);
        true
    }

    /// 生成随机默认密码 (8 位)
    pub fn random_password() -> String {
        let mut rng = rand::thread_rng();
        (0..8)
            .map(|_| {
                let n = rng.gen_range(0..36);
                if n < 10 {
                    (b'0' + n) as char
                } else {
                    (b'a' + n - 10) as char
                }
            })
            .collect()
    }
}

/// SHA256 密码哈希
pub fn hash_password(password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hex::encode(hasher.finalize())
}

/// 生成随机 trade_no
pub fn new_trade_no(prefix: &str, user_id: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let rand: String = (0..6)
        .map(|_| {
            let n = rand::thread_rng().gen_range(0..36);
            if n < 10 {
                (b'0' + n) as char
            } else {
                (b'a' + n - 10) as char
            }
        })
        .collect();
    format!("{prefix}{user_id}NO{rand}{now}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> UserStore {
        UserStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn create_and_auth() {
        let s = store();
        let u = s.create("alice", "pw", Role::User, 1000).unwrap();
        assert_eq!(u.quota, 1000);
        assert!(s.authenticate("alice", "pw").is_some());
        assert!(s.authenticate("alice", "bad").is_none());
    }

    #[test]
    fn charge_quota() {
        let s = store();
        let u = s.create("bob", "pw", Role::User, 100).unwrap();
        assert!(s.try_charge(&u.id, 30));
        assert_eq!(s.get_by_id(&u.id).unwrap().used_quota, 30);
        assert!(s.try_charge(&u.id, 100)); // remaining = 70, ok
        assert!(!s.try_charge(&u.id, 1)); // remaining = 0
    }

    #[test]
    fn duplicate_username() {
        let s = store();
        s.create("dup", "pw", Role::User, 0).unwrap();
        assert!(s.create("dup", "pw", Role::User, 0).is_err());
    }
}
