//! 用户系统 — 多用户与配额账本。
//!
//! 仿 new-api 的用户与额度模型：每个用户拥有可消费配额 (quota) 与已用配额
//! (used_quota)。API Key 可绑定到用户，调用推理时按 token 估算扣费。
//! 管理员通过 /api/users 进行用户管理，并通过易支付充值订单入账。
//!
//! 登录方式：统一使用邮箱(email)作为唯一标识，username 仅作展示昵称。

use anyhow::Result;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 用户角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    #[default]
    User,
}

// Default impl removed — derived via #[derive(Default)]

/// 用户记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    /// 邮箱（唯一标识，用于登录）
    pub email: String,
    /// 用户名（展示昵称，可空）
    #[serde(default)]
    pub username: String,
    /// 密码哈希 (argon2)
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

    /// 显示名称：优先用 username，否则用 email
    pub fn display_name(&self) -> &str {
        if self.username.is_empty() { &self.email } else { &self.username }
    }
}

/// 用户存储
pub struct UserStore {
    store: Arc<FileStore>,
    by_id: RwLock<HashMap<String, User>>,
    /// email -> id 索引
    by_email: RwLock<HashMap<String, String>>,
}

impl UserStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        let s = Self {
            store,
            by_id: RwLock::new(HashMap::new()),
            by_email: RwLock::new(HashMap::new()),
        };
        let _ = s.load();
        s
    }

    pub fn load(&self) -> Result<()> {
        let keys = self.store.list("user:")?;
        let mut by_id = self.by_id.write();
        let mut by_email = self.by_email.write();
        by_id.clear();
        by_email.clear();
        for key in &keys {
            if let Some(u) = self.store.get::<User>(key)? {
                by_id.insert(u.id.clone(), u.clone());
                if !u.email.is_empty() {
                    by_email.insert(u.email.clone(), u.id.clone());
                }
            }
        }
        // 向后兼容旧数据：无 email 字段的用户以 username 作为 email
        for (_, user) in by_id.iter() {
            if user.email.is_empty() && !user.username.is_empty() {
                by_email.entry(user.username.clone()).or_insert_with(|| user.id.clone());
            }
        }
        Ok(())
    }

    fn persist(&self, user: &User) -> Result<()> {
        self.store.put(&format!("user:{}", user.id), user)?;
        Ok(())
    }

    /// 创建用户（email 必填，username 可选）
    pub fn create(&self, email: &str, password: &str, role: Role, quota: i64) -> Result<User> {
        if email.is_empty() {
            anyhow::bail!("email cannot be empty");
        }
        if !is_valid_email(email) {
            anyhow::bail!("invalid email format");
        }
        if self.by_email.read().contains_key(email) {
            anyhow::bail!("email already exists");
        }
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            email: email.to_string(),
            username: String::new(),
            password: hash_password(password),
            role,
            quota,
            used_quota: 0,
            status: "active".into(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_email.write().insert(user.email.clone(), user.id.clone());
        Ok(user)
    }

    /// 创建用户（带邮箱和用户名）
    pub fn create_with_username(&self, email: &str, username: &str, password: &str, role: Role, quota: i64) -> Result<User> {
        if email.is_empty() {
            anyhow::bail!("email cannot be empty");
        }
        if !is_valid_email(email) {
            anyhow::bail!("invalid email format");
        }
        if self.by_email.read().contains_key(email) {
            anyhow::bail!("email already exists");
        }
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            email: email.to_string(),
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
        self.by_email.write().insert(user.email.clone(), user.id.clone());
        Ok(user)
    }

    pub fn list(&self) -> Vec<User> {
        let mut users: Vec<User> = self.by_id.read().values().cloned().collect();
        users.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        users
    }

    pub fn get_by_id(&self, id: &str) -> Option<User> {
        self.by_id.read().get(id).cloned()
    }

    /// 通过邮箱查找用户
    pub fn get_by_email(&self, email: &str) -> Option<User> {
        let id = self.by_email.read().get(email)?.clone();
        self.by_id.read().get(&id).cloned()
    }

    /// 兼容旧接口：通过 username 查找（优先 email 索引）
    pub fn get_by_username(&self, name: &str) -> Option<User> {
        if let Some(u) = self.get_by_email(name) {
            return Some(u);
        }
        // 回退：遍历查找 username 匹配
        for user in self.by_id.read().values() {
            if user.username == name {
                return Some(user.clone());
            }
        }
        None
    }

    /// 校验密码并返回用户（通过 email 登录）
    pub fn authenticate(&self, email: &str, password: &str) -> Option<User> {
        let user = self.get_by_email(email).or_else(|| {
            // 兼容旧模式：允许用 username 登录（无 email 字段的用户）
            self.get_by_username(email)
        })?;
        if user.status != "active" {
            return None;
        }
        if verify_password(password, &user.password) {
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
        let old_email = user.email.clone();
        mutator(&mut user);
        if user.email != old_email {
            if user.email.is_empty() {
                anyhow::bail!("email cannot be empty");
            }
            if !is_valid_email(&user.email) {
                anyhow::bail!("invalid email format");
            }
            if self.by_email.read().contains_key(&user.email)
                && self.by_email.read().get(&user.email) != Some(&id.to_string())
            {
                anyhow::bail!("email already exists");
            }
        }
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        let mut by_email = self.by_email.write();
        if !old_email.is_empty() {
            by_email.remove(&old_email);
        }
        if !user.email.is_empty() {
            by_email.insert(user.email.clone(), user.id.clone());
        }
        Ok(user)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let user = self.get_by_id(id);
        if let Some(u) = user {
            self.store.delete(&format!("user:{id}"))?;
            self.by_id.write().remove(id);
            if !u.email.is_empty() {
                self.by_email.write().remove(&u.email);
            }
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

/// 验证邮箱格式
fn is_valid_email(email: &str) -> bool {
    let email = email.trim();
    if email.is_empty() || email.len() > 254 {
        return false;
    }
    let parts: Vec<&str> = email.splitn(2, '@').collect();
    if parts.len() != 2 {
        return false;
    }
    let (local, domain) = (parts[0], parts[1]);
    if local.is_empty() || local.len() > 64 || domain.is_empty() || domain.len() > 255 {
        return false;
    }
    if !domain.contains('.') {
        return false;
    }
    true
}

/// Argon2 密码哈希
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hash")
        .to_string()
}

/// 验证密码
pub fn verify_password(password: &str, hash: &str) -> bool {
    // 兼容旧 SHA256 格式
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(password.as_bytes());
        let expected = hex::encode(hasher.finalize());
        return expected == hash;
    }
    // argon2 验证
    PasswordHash::new(hash)
        .and_then(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed))
        .is_ok()
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
        let u = s.create("alice@test.com", "pw", Role::User, 1000).unwrap();
        assert_eq!(u.quota, 1000);
        assert!(s.authenticate("alice@test.com", "pw").is_some());
        assert!(s.authenticate("alice@test.com", "bad").is_none());
    }

    #[test]
    fn charge_quota() {
        let s = store();
        let u = s.create("bob@test.com", "pw", Role::User, 100).unwrap();
        assert!(s.try_charge(&u.id, 30));
        assert_eq!(s.get_by_id(&u.id).unwrap().used_quota, 30);
        assert!(s.try_charge(&u.id, 70));
        assert!(!s.try_charge(&u.id, 1));
    }

    #[test]
    fn duplicate_email() {
        let s = store();
        s.create("dup@test.com", "pw", Role::User, 0).unwrap();
        assert!(s.create("dup@test.com", "pw", Role::User, 0).is_err());
    }

    #[test]
    fn invalid_email() {
        let s = store();
        assert!(s.create("", "pw", Role::User, 0).is_err());
        assert!(s.create("notanemail", "pw", Role::User, 0).is_err());
    }

    #[test]
    fn get_by_email() {
        let s = store();
        s.create("test@example.com", "pw", Role::User, 0).unwrap();
        assert!(s.get_by_email("test@example.com").is_some());
        assert!(s.get_by_email("nonexist@example.com").is_none());
    }
}
