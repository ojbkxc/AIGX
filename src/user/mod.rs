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

pub mod checkin;

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
    /// 已预留但未结算的配额（P1：配额预留/结算两段式）
    #[serde(default)]
    pub reserved_quota: i64,
    /// 最近一次预留的时间戳（G2：超时解冻依据，None=无未结算预留）
    #[serde(default)]
    pub reserved_at: Option<i64>,
    /// 状态: active / disabled
    #[serde(default = "default_status")]
    pub status: String,
    /// 用户分组（计费倍率与模型权限依据，参照 new-api user.group）
    #[serde(default = "default_group")]
    pub group: String,
    /// TOTP 密钥（base32，空=未设置 2FA）。
    ///
    /// 明文存储：位于管理后台文件权限范围内（与密码哈希同级机密度），
    /// 后续可增加静态加密（见 ROADMAP）。
    #[serde(default)]
    pub totp_secret: String,
    /// 是否已启用 TOTP 二次验证（secret 已绑定且经过一次有效验证）
    #[serde(default)]
    pub totp_enabled: bool,
    /// TOTP 一次性恢复码的 SHA-256 哈希（G3：备换设备时使用）。
    ///
    /// 只存哈希不存明文：恢复码明文仅在生成时展示一次，落盘即
    /// 不可逆。存哈希意味着「备份/恢复场景下用户看到自己的 secret
    /// 明文」的泄露面不会扩展到恢复码。空数组 = 未生成恢复码
    /// （兼容旧数据与未启用 TOTP 的用户）。
    #[serde(default)]
    pub totp_recovery_codes: Vec<String>,
    /// 邀请码（4 位随机字符，对齐 new-api AffCode；空=未生成）
    #[serde(default)]
    pub aff_code: String,
    /// 已成功邀请人数
    #[serde(default)]
    pub aff_count: i64,
    /// 邀请奖励余额（可划转到可用配额）
    #[serde(default)]
    pub aff_quota: i64,
    /// 累计邀请获得配额（历史总量，划转不减）
    #[serde(default)]
    pub aff_history_quota: i64,
    /// 邀请人 ID（注册时携带邀请码写入，空=自然注册）
    #[serde(default)]
    pub inviter_id: String,
    #[serde(default)]
    pub created_at: i64,
}

fn default_status() -> String {
    "active".to_string()
}

fn default_group() -> String {
    "default".to_string()
}

impl User {
    /// 剩余可用配额（扣除已用 + 已预留）
    pub fn remaining(&self) -> i64 {
        (self.quota - self.used_quota - self.reserved_quota).max(0)
    }

    /// 是否为管理员
    pub fn is_admin(&self) -> bool {
        matches!(self.role, Role::Admin)
    }

    /// 显示名称：优先用 username，否则用 email
    pub fn display_name(&self) -> &str {
        if self.username.is_empty() {
            &self.email
        } else {
            &self.username
        }
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
        for user in by_id.values() {
            if user.email.is_empty() && !user.username.is_empty() {
                by_email
                    .entry(user.username.clone())
                    .or_insert_with(|| user.id.clone());
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
            reserved_quota: 0,
            reserved_at: None,
            status: "active".into(),
            group: "default".into(),
            totp_secret: String::new(),
            totp_enabled: false,
            totp_recovery_codes: Vec::new(),
            aff_code: String::new(),
            aff_count: 0,
            aff_quota: 0,
            aff_history_quota: 0,
            inviter_id: String::new(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_email
            .write()
            .insert(user.email.clone(), user.id.clone());
        Ok(user)
    }

    /// 创建用户（带邮箱和用户名）
    pub fn create_with_username(
        &self,
        email: &str,
        username: &str,
        password: &str,
        role: Role,
        quota: i64,
    ) -> Result<User> {
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
            reserved_quota: 0,
            reserved_at: None,
            status: "active".into(),
            group: "default".into(),
            totp_secret: String::new(),
            totp_enabled: false,
            totp_recovery_codes: Vec::new(),
            aff_code: String::new(),
            aff_count: 0,
            aff_quota: 0,
            aff_history_quota: 0,
            inviter_id: String::new(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_email
            .write()
            .insert(user.email.clone(), user.id.clone());
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

    /// 原子增加配额（充值入账）。
    ///
    /// B02 修复：原 `add_quota` 走非原子的 `update`（读-克隆-改-写回），
    /// 与原子的 `try_charge` 并发时会产生丢失更新（后写覆盖前写）。
    /// 本方法与 `try_charge` 一样在写锁内完成读-改-写并持久化，
    /// 保证与扣费路径并发安全。返回更新后的用户快照。
    pub fn add_quota_atomic(&self, id: &str, delta: i64) -> Result<User> {
        let mut by_id = self.by_id.write();
        let user = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("user not found"))?;
        user.quota += delta;
        let snapshot = user.clone();
        drop(by_id);
        self.persist(&snapshot)?;
        Ok(snapshot)
    }

    /// 增加配额（充值入账，委托原子实现）
    pub fn add_quota(&self, id: &str, amount: i64) -> Result<()> {
        self.add_quota_atomic(id, amount)?;
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
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!("Failed to persist user {} try_charge: {}", id, e);
        }
        true
    }

    /// 预留配额（P1：两段式计费第一步）。
    ///
    /// 在请求发起前调用，将 `amount` 从可用余额中「冻结」到 `reserved_quota`。
    /// 请求完成后由 `settle_quota` 把实际消费从 reserved 转入 used_quota，
    /// 多预留的部分由 `release_quota` 归还。
    ///
    /// 返回 true 表示预留成功，false 表示余额不足。
    pub fn reserve_quota(&self, id: &str, amount: i64) -> bool {
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
        if user.reserved_quota == 0 {
            user.reserved_at = Some(chrono::Utc::now().timestamp());
        }
        user.reserved_quota += amount;
        let snapshot = user.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!("Failed to persist user {} reserve_quota: {}", id, e);
        }
        true
    }

    /// 结算预留配额（P1：两段式计费第二步）。
    ///
    /// `reserved` 是之前预留的总额，`actual` 是实际消费额（≤ reserved）。
    /// 将 actual 转入 used_quota，剩余 (reserved - actual) 归还到可用余额。
    ///
    /// 若 actual > reserved（罕见：估算严重不足），超出部分直接从余额扣。
    pub fn settle_quota(&self, id: &str, reserved: i64, actual: i64) {
        if reserved <= 0 {
            return;
        }
        let actual = actual.max(0);
        let mut by_id = self.by_id.write();
        let user = match by_id.get_mut(id) {
            Some(u) => u,
            None => return,
        };
        // 释放预留（不超过已预留量）
        let release = reserved.min(user.reserved_quota);
        if release >= user.reserved_quota {
            user.reserved_at = None;
        }
        user.reserved_quota -= release;
        // 实际消费转入 used_quota
        user.used_quota += actual;
        let snapshot = user.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!("Failed to persist user {} settle_quota: {}", id, e);
        }
    }

    /// 释放预留配额（P1：两段式计费的取消/回滚路径）。
    ///
    /// 与 `settle_quota` 的区别：settle 是「结算」——解冻预留并把实际消费
    /// 转入 used_quota；release 是「取消」——只把预留解冻归还余额，
    /// 不产生任何消费。请求失败、预留回滚、预留超时回收都走这里。
    ///
    /// 为什么必须独立成方法：settle 语义下 `actual=0` 看似等价于 release，
    /// 但 settle 内部对并发场景有 `reserved.min(user.reserved_quota)` 的
    /// 钳制，同一用户多请求并发时可能解冻到他人刚建立的预留（2026-09-08
    /// 二轮审查 G1）。release 按「本次预留的实际持有量」精确归还，语义
    /// 可审计且不依赖钳制。
    pub fn release_quota(&self, id: &str, amount: i64) {
        if amount <= 0 {
            return;
        }
        let mut by_id = self.by_id.write();
        let user = match by_id.get_mut(id) {
            Some(u) => u,
            None => return,
        };
        // 最多归还当前预留量：预留可能已被其他路径结算（防御性上限）
        let release = amount.min(user.reserved_quota);
        if release >= user.reserved_quota {
            user.reserved_at = None;
        }
        user.reserved_quota -= release;
        let snapshot = user.clone();
        drop(by_id);
        if let Err(e) = self.persist(&snapshot) {
            tracing::error!("Failed to persist user {} release_quota: {}", id, e);
        }
    }

    /// 回收超时未结算的预留（G2：预留 TTL 解冻）。
    ///
    /// 进程崩溃 / 流式请求挂死会让预留永久冻结；周期任务按 `reserved_at`
    /// 判断超时（默认 30 分钟，远超单请求上限），把整笔预留归还余额。
    /// 返回解冻的账号数。
    pub fn release_stale_reservations(&self, ttl_secs: i64) -> u64 {
        let cutoff = chrono::Utc::now().timestamp() - ttl_secs;
        let mut count = 0u64;
        let mut by_id = self.by_id.write();
        for user in by_id.values_mut() {
            let stale = user.reserved_at.map(|t| t < cutoff).unwrap_or(false);
            if stale && user.reserved_quota > 0 {
                user.reserved_quota = 0;
                user.reserved_at = None;
                let snapshot = user.clone();
                if let Err(e) = self.persist(&snapshot) {
                    tracing::error!(
                        "Failed to persist stale reservation release {}: {}",
                        user.id,
                        e
                    );
                }
                count += 1;
            }
        }
        count
    }
    /// 通过邀请码查找用户（对齐 new-api GetUserIdByAffCode）
    pub fn get_by_aff_code(&self, code: &str) -> Option<User> {
        let code = code.trim();
        if code.is_empty() {
            return None;
        }
        self.by_id
            .read()
            .values()
            .find(|u| u.aff_code == code)
            .cloned()
    }

    /// 确保用户拥有邀请码（无则生成 4 位随机码，对齐 new-api GenAffCode）。
    /// 冲突时重试（62^4 = 14M 空间，用户量级内碰撞概率极低）。
    pub fn ensure_aff_code(&self, id: &str) -> Result<String> {
        {
            let by_id = self.by_id.read();
            if let Some(u) = by_id.get(id) {
                if !u.aff_code.is_empty() {
                    return Ok(u.aff_code.clone());
                }
            }
        }
        self.update(id, |u| {
            if u.aff_code.is_empty() {
                u.aff_code = gen_aff_code();
            }
        })?;
        Ok(self
            .by_id
            .read()
            .get(id)
            .map(|u| u.aff_code.clone())
            .unwrap_or_default())
    }

    /// 记录一次成功邀请：邀请人 aff_count+1、双方奖励入账。
    ///
    /// 在注册事务内调用（注册携带有效邀请码时）；奖励值为 0 时跳过入账
    /// 但仍计数。返回 (邀请人奖励, 新用户奖励) 实际入账值。
    pub fn record_aff(
        &self,
        inviter_id: &str,
        invitee_id: &str,
        quota_for_inviter: i64,
        quota_for_invitee: i64,
    ) -> Result<(i64, i64)> {
        // 邀请人：计数 + 奖励进 aff 余额（需划转才可消费，防刷）
        self.update(inviter_id, |u| {
            u.aff_count += 1;
            if quota_for_inviter > 0 {
                u.aff_quota += quota_for_inviter;
                u.aff_history_quota += quota_for_inviter;
            }
        })?;
        // 新用户：奖励直接进可用配额（注册即用）
        if quota_for_invitee > 0 {
            self.add_quota(invitee_id, quota_for_invitee)?;
        }
        Ok((quota_for_inviter.max(0), quota_for_invitee.max(0)))
    }

    /// 邀请奖励划转：aff_quota → 可用配额（对齐 new-api TransferAffQuotaToQuota）。
    /// 返回实际划转额度（余额不足时全部划转）。
    pub fn transfer_aff_quota(&self, id: &str) -> Result<i64> {
        let mut by_id = self.by_id.write();
        let user = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("user not found"))?;
        let amount = user.aff_quota;
        if amount <= 0 {
            return Ok(0);
        }
        user.aff_quota = 0;
        user.quota += amount;
        let snapshot = user.clone();
        drop(by_id);
        self.persist(&snapshot)?;
        Ok(amount)
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

/// 生成 4 位随机邀请码（对齐 new-api GenAffCode：字母+数字）
pub fn gen_aff_code() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    (0..4)
        .map(|_| CHARSET[rand::thread_rng().gen_range(0..CHARSET.len())] as char)
        .collect()
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

    /// G3：旧 JSON 用户数据无 `totp_recovery_codes` 字段时，
    /// serde default 将其解析为空数组（向后兼容，不破坏老库）。
    #[test]
    fn user_json_without_recovery_codes_parses() {
        let legacy = r#"{
            "id": "u1",
            "email": "legacy@test.com",
            "username": "",
            "password": "hash",
            "role": "user",
            "quota": 0,
            "used_quota": 0,
            "reserved_quota": 0,
            "status": "active",
            "group": "default",
            "totp_secret": "SECRET",
            "totp_enabled": true,
            "created_at": 0
        }"#;
        let user: User = serde_json::from_str(legacy).unwrap();
        assert!(user.totp_recovery_codes.is_empty());
        assert!(user.totp_enabled);
    }

    // ── 预留/结算/释放语义测试（P1 两段式，2026-09-08 G1/G5 修复）──

    /// 预留 → 结算：reserved 解冻，used 增加实际消费，余额正确。
    #[test]
    fn reserve_then_settle() {
        let s = store();
        let u = s.create("rs1@test.com", "pw", Role::User, 1000).unwrap();
        assert!(s.reserve_quota(&u.id, 300));
        assert_eq!(s.get_by_id(&u.id).unwrap().reserved_quota, 300);
        s.settle_quota(&u.id, 300, 180);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 180);
        assert_eq!(after.remaining(), 820);
    }

    /// 预留 → 释放：reserved 解冻，used 不变（无消费归还）。
    #[test]
    fn reserve_then_release() {
        let s = store();
        let u = s.create("rs2@test.com", "pw", Role::User, 1000).unwrap();
        assert!(s.reserve_quota(&u.id, 300));
        s.release_quota(&u.id, 300);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 0);
        assert_eq!(after.remaining(), 1000);
    }

    /// 释放量超过当前预留时只归还当前量（防御性上限）。
    #[test]
    fn release_clamped() {
        let s = store();
        let u = s.create("rs3@test.com", "pw", Role::User, 1000).unwrap();
        assert!(s.reserve_quota(&u.id, 50));
        s.release_quota(&u.id, 500);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 0);
    }

    /// G2：预留带时间戳，超时未结算的预留被周期任务解冻。
    #[test]
    fn stale_reservation_released_after_ttl() {
        let s = store();
        let u = s.create("stale@test.com", "pw", Role::User, 1000).unwrap();
        assert!(s.reserve_quota(&u.id, 300));
        assert!(s.get_by_id(&u.id).unwrap().reserved_at.is_some());
        // 手动把预留时间拨到 1 小时前（模拟挂死请求）
        {
            let mut by_id = s.by_id.write();
            by_id.get_mut(&u.id).unwrap().reserved_at = Some(chrono::Utc::now().timestamp() - 3600);
        }
        // TTL 30 分钟：1 小时前的预留应被回收
        assert_eq!(s.release_stale_reservations(30 * 60), 1);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert!(after.reserved_at.is_none());
        assert_eq!(after.remaining(), 1000);
    }

    /// G2：未超时的预留不受影响。
    #[test]
    fn fresh_reservation_not_released() {
        let s = store();
        let u = s.create("fresh@test.com", "pw", Role::User, 1000).unwrap();
        assert!(s.reserve_quota(&u.id, 300));
        assert_eq!(s.release_stale_reservations(30 * 60), 0);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_quota, 300);
        assert!(after.reserved_at.is_some());
    }

    #[test]
    fn get_by_email() {
        let s = store();
        s.create("test@example.com", "pw", Role::User, 0).unwrap();
        assert!(s.get_by_email("test@example.com").is_some());
        assert!(s.get_by_email("nonexist@example.com").is_none());
    }
}
