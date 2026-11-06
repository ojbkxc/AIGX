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

/// 内置默认管理员邮箱（首次启动自动创建，用户要求不可更改/删除/禁用）。
/// main.rs 的 ensure_default_admin 与 admin users.rs 的锁定逻辑共用此常量。
pub const DEFAULT_ADMIN_EMAIL: &str = "admin@gmail.com";

/// 用户角色
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    #[default]
    User,
}

// Default impl removed — derived via #[derive(Default)]

/// 单笔配额预留记录（P0-2）。
///
/// 替代原先的裸 `reserved_quota + reserved_at` 单值：同一用户并发请求各自
/// 持有一个 token，结算/释放按 token 精确摘除，超时回收只清过期条目，
/// 不会误伤仍在进行中的预留。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservationEntry {
    /// 预留 token（对应 `reserve_quota` 的返回值；0 保留表示空预留）
    pub token: u64,
    /// 预留金额
    pub amount: i64,
    /// 预留时间戳（超时回收依据）
    pub at: i64,
}

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
    /// 逐笔未结算预留（P0-2：替代裸 reserved_quota，支持并发请求各自精确回收）
    #[serde(default)]
    pub reservations: Vec<ReservationEntry>,
    /// 预留 token 分配器（单调递增；0 保留表示「空预留」）
    #[serde(default)]
    pub next_reservation_token: u64,
    /// 结算击穿限额的累计溢出额（P0-3；仅记账展示，remaining 仍以 0 为下限）
    #[serde(default)]
    pub overdraft_quota: i64,
    /// 版本号（P1-4：异步落盘防乱序，每次修改自增）
    #[serde(default)]
    pub version: u64,
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
    /// 是否必须在下次登录后修改密码（首次启动的内置管理员置 true）。
    ///
    /// 内置管理员用固定的弱口令 `123456` 初始化；该标记让前端在登录成功后
    /// 强制跳转改密页（不阻断登录本身 —— 否则用户拿不到初始密码就无法进入）。
    /// 用户改过一次密码后置 false。旧数据缺该字段时为 false（不影响存量用户）。
    #[serde(default)]
    pub must_change_password: bool,
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
    /// 管理员备注（用户管理后台添加，对齐 v2board 用户备注）
    #[serde(default)]
    pub remark: String,
    /// GitHub 账号身份（上游返回的用户 id 字符串；空 = 未绑定）。
    ///
    /// OAuth 登录/绑定按「显式身份优先」匹配：只要某 GitHub 账号绑到某用户，
    /// 该账号再走 OAuth 只会登进这个用户，即使其邮箱与他人相同 —— 杜绝
    /// 「换个相同邮箱就能登进别人账号」的接管面。邮箱匹配仅在身份未绑定时
    /// 作为兜底（命中后自动把身份写入，升级为显式绑定）。
    #[serde(default)]
    pub oauth_github: String,
    /// Google 账号身份（`sub` claim，稳定唯一；空 = 未绑定）
    #[serde(default)]
    pub oauth_google: String,
    /// LinuxDO 账号身份（上游用户 id 字符串；空 = 未绑定）
    #[serde(default)]
    pub oauth_linuxdo: String,
    /// 是否已设置真实登录密码。
    ///
    /// OAuth 首次自动建号时随机密码不可用，置 false；密码注册 / 改密 / 重置 /
    /// 成功密码登录后置 true（自愈）。**旧数据缺该字段按 false 处理**——让存量
    /// OAuth 账号在身份字段还是空时仍走邮箱兜底登录（首次登录即补写身份，
    /// 升级为显式绑定）；而任何用密码成功登录过的账号都会被翻成 true，从而
    /// 关闭「他人用相同邮箱注册密码账号后、凭 OAuth 登进该账号」的接管面。
    /// 另用于解绑前的防锁死校验（确认还有其它登录方式）。
    #[serde(default)]
    pub password_set: bool,
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
    /// 剩余可用配额（扣除已用 + 已预留，非负）
    pub fn remaining(&self) -> i64 {
        (self.quota - self.used_quota - self.reserved_total()).max(0)
    }

    /// 当前全部未结算预留之和（P0-2：逐笔预留的聚合视图）。
    pub fn reserved_total(&self) -> i64 {
        self.reservations.iter().map(|r| r.amount).sum()
    }

    /// 自增版本号（P1-4：每次修改调用，异步落盘据此丢弃旧快照）。
    pub fn bump_version(&mut self) {
        self.version = self.version.wrapping_add(1);
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

    /// 锁外异步落盘：快照已持有完整副本，丢给 blocking 线程池写存储，
    /// 消除「写守卫内做同步 IO」的固有风险。失败仅记日志。
    ///
    /// P1-4：落盘前读回当前存储值，若其 version 不低于本快照则丢弃本次写入，
    /// 避免旧快照后落盘把新值覆盖回退（重启后配额回血）。多一次读，可接受。
    fn persist_async(&self, user: &User) {
        let key = format!("user:{}", user.id);
        if let Ok(Some(current)) = self.store.get::<User>(&key) {
            if current.version >= user.version {
                return;
            }
        }
        self.store.put_async(key, user.clone());
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
            reservations: Vec::new(),
            next_reservation_token: 0,
            overdraft_quota: 0,
            version: 0,
            status: "active".into(),
            group: "default".into(),
            totp_secret: String::new(),
            totp_enabled: false,
            must_change_password: false,
            totp_recovery_codes: Vec::new(),
            aff_code: String::new(),
            aff_count: 0,
            aff_quota: 0,
            aff_history_quota: 0,
            inviter_id: String::new(),
            remark: String::new(),
            oauth_github: String::new(),
            oauth_google: String::new(),
            oauth_linuxdo: String::new(),
            password_set: true,
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
            reservations: Vec::new(),
            next_reservation_token: 0,
            overdraft_quota: 0,
            version: 0,
            status: "active".into(),
            group: "default".into(),
            totp_secret: String::new(),
            totp_enabled: false,
            must_change_password: false,
            totp_recovery_codes: Vec::new(),
            aff_code: String::new(),
            aff_count: 0,
            aff_quota: 0,
            aff_history_quota: 0,
            inviter_id: String::new(),
            remark: String::new(),
            oauth_github: String::new(),
            oauth_google: String::new(),
            oauth_linuxdo: String::new(),
            password_set: true,
            created_at: chrono::Utc::now().timestamp(),
        };
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_email
            .write()
            .insert(user.email.clone(), user.id.clone());
        Ok(user)
    }

    /// 为 OAuth 首次登录创建用户：随机密码不可用（password_set=false），
    /// 并写入对应 provider 的稳定身份 id。OAuth 用户不走密码登录。
    pub fn create_oauth_user(
        &self,
        email: &str,
        username: &str,
        provider: &str,
        identity: &str,
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
        let mut user = User {
            id: uuid::Uuid::new_v4().to_string(),
            email: email.to_string(),
            username: username.to_string(),
            // 随机密码：OAuth 用户不用密码登录；password_set=false 让解绑保护生效
            password: hash_password(&uuid::Uuid::new_v4().to_string()),
            role: Role::User,
            quota,
            used_quota: 0,
            reservations: Vec::new(),
            next_reservation_token: 0,
            overdraft_quota: 0,
            version: 0,
            status: "active".into(),
            group: "default".into(),
            totp_secret: String::new(),
            totp_enabled: false,
            must_change_password: false,
            totp_recovery_codes: Vec::new(),
            aff_code: String::new(),
            aff_count: 0,
            aff_quota: 0,
            aff_history_quota: 0,
            inviter_id: String::new(),
            remark: String::new(),
            oauth_github: String::new(),
            oauth_google: String::new(),
            oauth_linuxdo: String::new(),
            password_set: false,
            created_at: chrono::Utc::now().timestamp(),
        };
        match provider {
            "github" => user.oauth_github = identity.to_string(),
            "google" => user.oauth_google = identity.to_string(),
            "linuxdo" => user.oauth_linuxdo = identity.to_string(),
            _ => anyhow::bail!("unknown oauth provider {provider}"),
        }
        self.persist(&user)?;
        self.by_id.write().insert(user.id.clone(), user.clone());
        self.by_email
            .write()
            .insert(user.email.clone(), user.id.clone());
        Ok(user)
    }

    /// 按 provider 稳定身份查找用户（显式绑定优先）。返回 None 表示该身份未绑定任何人。
    pub fn find_by_oauth(&self, provider: &str, identity: &str) -> Option<User> {
        if identity.is_empty() {
            return None;
        }
        self.list().into_iter().find(|u| match provider {
            "github" => u.oauth_github == identity,
            "google" => u.oauth_google == identity,
            "linuxdo" => u.oauth_linuxdo == identity,
            _ => false,
        })
    }

    /// 某 provider 身份是否已被「别的用户」占用（绑定冲突检测）。
    /// `self_email` 允许为空（新绑定场景），只要有任何用户的该身份槽位
    /// 等于 identity 且 email 不同即视为冲突。
    pub fn oauth_taken_by_other(&self, provider: &str, identity: &str, self_email: &str) -> bool {
        if identity.is_empty() {
            return false;
        }
        self.list().iter().any(|u| {
            let same_identity = match provider {
                "github" => u.oauth_github == identity,
                "google" => u.oauth_google == identity,
                "linuxdo" => u.oauth_linuxdo == identity,
                _ => false,
            };
            same_identity && u.email != self_email
        })
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
        // 写锁内完成读-改-写：原实现「读锁克隆→改→写锁覆盖回」与并发的
        // try_charge/add_quota_atomic（同样在写锁内改 quota）会产生丢失
        // 更新——update 后写覆盖掉并发的扣费/充值。此处持写锁后再执行
        // mutator，保证与所有配额变更路径串行化。
        let mut by_id = self.by_id.write();
        let user = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("user not found"))?;
        let old_email = user.email.clone();
        mutator(user);
        user.bump_version();
        // 邮箱变更校验（写锁内做，读到的 by_email 与写入之间无窗口）
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
        let snapshot = user.clone();
        drop(by_id);
        self.persist(&snapshot)?;
        let mut by_email = self.by_email.write();
        if !old_email.is_empty() {
            by_email.remove(&old_email);
        }
        if !snapshot.email.is_empty() {
            by_email.insert(snapshot.email.clone(), snapshot.id.clone());
        }
        Ok(snapshot)
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
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        // 锁外异步落盘（P1-4：version 单调校验防乱序覆盖——旧注释担心的
        // 「先放锁再写盘、后写覆盖前写」由 persist_async 的 version 比较兜住：
        // 落盘前读回当前存储，version 不低于本快照则丢弃，杜绝旧值覆盖新值）。
        self.persist_async(&snapshot);
        Ok(snapshot)
    }

    /// 增加配额（充值入账，委托原子实现）
    pub fn add_quota(&self, id: &str, amount: i64) -> Result<()> {
        self.add_quota_atomic(id, amount)?;
        Ok(())
    }

    /// 原子扣减余额（余额购买订阅等场景）。
    ///
    /// 与 `try_charge` 同构：写锁内校验 remaining 后再改 quota，
    /// 负余额直接拒绝。取代原先「独立读预检 + add_quota(-x)」的两步
    /// 非原子做法——预检与扣减之间的窗口内并发请求可双重消费，
    /// 把余额刷成负数。
    pub fn try_decrease_quota(&self, id: &str, amount: i64) -> Result<()> {
        if amount <= 0 {
            return Ok(());
        }
        let mut by_id = self.by_id.write();
        let user = by_id
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("user not found"))?;
        if user.remaining() < amount {
            anyhow::bail!("insufficient quota");
        }
        user.quota -= amount;
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        // 锁外异步落盘（P1-4：version 单调校验防乱序覆盖）
        self.persist_async(&snapshot);
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
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        // 锁外异步落盘；失败只记日志不清零内存态——计费已经在内存里扣了，
        // 回滚会让用户白嫖，宁可下次写盘纠正。
        self.persist_async(&snapshot);
        true
    }

    /// 预留配额（P0-2：逐笔预留，返回 token）。
    ///
    /// 在请求发起前调用，把 `amount` 冻结为一笔带 token 的预留记录。
    /// 请求完成后由 `settle_quota(uid, token, actual)` 结算或
    /// `release_quota(uid, token)` 取消；超时回收只清过期条目，
    /// 并发请求各自持有 token、互不误伤。
    ///
    /// 返回 `Some(token)` 表示预留成功（token=0 表示 amount<=0 的空预留）；
    /// `None` 表示余额不足。
    pub fn reserve_quota(&self, id: &str, amount: i64) -> Option<u64> {
        if amount <= 0 {
            return Some(0);
        }
        let mut by_id = self.by_id.write();
        let user = by_id.get_mut(id)?;
        if user.remaining() < amount {
            return None;
        }
        let token = user.next_reservation_token.wrapping_add(1);
        user.next_reservation_token = token;
        user.reservations.push(ReservationEntry {
            token,
            amount,
            at: chrono::Utc::now().timestamp(),
        });
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        // 锁外异步落盘（并发安全：后续修改会再次落盘最新值）
        self.persist_async(&snapshot);
        Some(token)
    }

    /// 结算预留配额（P0-2：按 token 精确摘除；P0-3：超额记 overdraft）。
    ///
    /// 摘除该 token 的预留并把 `actual` 计入 `used_quota`。若结算后
    /// `used_quota` 超过 `quota`，本次新增的超出部分累计进 `overdraft_quota`
    /// —— 债务照记不白送，同时 `remaining()` 以 0 为下限不会为负。
    pub fn settle_quota(&self, id: &str, token: u64, actual: i64) {
        if token == 0 {
            return;
        }
        let actual = actual.max(0);
        let mut by_id = self.by_id.write();
        let user = match by_id.get_mut(id) {
            Some(u) => u,
            None => return,
        };
        // 按 token 精确摘除本次预留（不存在则视为已结算，防御性 no-op）
        if let Some(pos) = user.reservations.iter().position(|r| r.token == token) {
            user.reservations.remove(pos);
        }
        // 记账：实际消费转入 used_quota
        let before_over = (user.used_quota - user.quota).max(0);
        user.used_quota += actual;
        let over = (user.used_quota - user.quota).max(0) - before_over;
        if over > 0 {
            user.overdraft_quota += over;
            tracing::warn!(
                user = %user.id,
                overdraft_delta = over,
                used_quota = user.used_quota,
                quota = user.quota,
                "settle_quota exceeded user quota; recorded as overdraft"
            );
        }
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        self.persist_async(&snapshot);
    }

    /// 释放预留配额（P0-2：按 token 精确归还，不产生任何消费）。
    ///
    /// 请求失败、预留回滚、预留超时回收都走这里。按 token 摘除对应预留，
    /// 不依赖任何「当前预留量」钳制，并发请求互不干扰。
    pub fn release_quota(&self, id: &str, token: u64) {
        if token == 0 {
            return;
        }
        let mut by_id = self.by_id.write();
        let user = match by_id.get_mut(id) {
            Some(u) => u,
            None => return,
        };
        let Some(pos) = user.reservations.iter().position(|r| r.token == token) else {
            return;
        };
        user.reservations.remove(pos);
        user.bump_version();
        let snapshot = user.clone();
        drop(by_id);
        self.persist_async(&snapshot);
    }

    /// 回收超时未结算的预留（P0-2：只清 `at < cutoff` 的逐笔条目）。
    ///
    /// 进程崩溃 / 流式请求挂死会让预留永久冻结；周期任务按每笔预留的时间戳
    /// 判断超时（默认 30 分钟，远超单请求上限），只归还过期那几笔，
    /// 仍在进行中的并发预留不受影响。返回被回收的账号数。
    pub fn release_stale_reservations(&self, ttl_secs: i64) -> u64 {
        let cutoff = chrono::Utc::now().timestamp() - ttl_secs;
        let mut count = 0u64;
        let mut dirty: Vec<User> = Vec::new();
        {
            let mut by_id = self.by_id.write();
            for user in by_id.values_mut() {
                let before = user.reservations.len();
                user.reservations.retain(|r| r.at >= cutoff);
                if user.reservations.len() != before {
                    user.bump_version();
                    dirty.push(user.clone());
                    count += 1;
                }
            }
        }
        // 锁外异步落盘（P1-4：version 单调校验防乱序覆盖；put_async 内部记错误日志）
        for snapshot in &dirty {
            self.persist_async(snapshot);
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
        user.bump_version();
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
        // 旧数据无新增字段：reservations/version/overdraft 走 serde default
        assert!(user.reservations.is_empty());
        assert_eq!(user.version, 0);
        assert_eq!(user.overdraft_quota, 0);
    }

    // ── 预留/结算/释放语义测试（P0-2 逐笔 token 化，2026-11-04）──

    /// 预留 → 结算：按 token 精确摘除预留，used 增加实际消费，余额正确。
    #[test]
    fn reserve_then_settle() {
        let s = store();
        let u = s.create("rs1@test.com", "pw", Role::User, 1000).unwrap();
        let tok = s.reserve_quota(&u.id, 300).unwrap();
        assert_eq!(s.get_by_id(&u.id).unwrap().reserved_total(), 300);
        s.settle_quota(&u.id, tok, 180);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 0);
        assert_eq!(after.used_quota, 180);
        assert_eq!(after.remaining(), 820);
    }

    /// 缓存命中：预留 1000、命中成本 300 → 预留归零、used == 300（P0-1 止损）。
    #[test]
    fn cache_hit_settles_reservation() {
        let s = store();
        let u = s.create("cache1@test.com", "pw", Role::User, 5000).unwrap();
        let tok = s.reserve_quota(&u.id, 1000).unwrap();
        // 缓存命中按实际成本 300 结算（预留多出的 700 归还）
        s.settle_quota(&u.id, tok, 300);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 0);
        assert_eq!(after.used_quota, 300);
        assert_eq!(after.remaining(), 4700);
    }

    /// 缓存命中成本为 0 → 预留全退、无消费（未配 cache_price）。
    #[test]
    fn cache_hit_zero_cost_releases_all() {
        let s = store();
        let u = s.create("cache2@test.com", "pw", Role::User, 5000).unwrap();
        let tok = s.reserve_quota(&u.id, 1000).unwrap();
        s.release_quota(&u.id, tok);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 0);
        assert_eq!(after.used_quota, 0);
        assert_eq!(after.remaining(), 5000);
    }

    /// 预留 → 释放：预留解冻，used 不变（无消费归还）。
    #[test]
    fn reserve_then_release() {
        let s = store();
        let u = s.create("rs2@test.com", "pw", Role::User, 1000).unwrap();
        let tok = s.reserve_quota(&u.id, 300).unwrap();
        s.release_quota(&u.id, tok);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 0);
        assert_eq!(after.used_quota, 0);
        assert_eq!(after.remaining(), 1000);
    }

    /// 释放未知 token 是 no-op（不误伤其他预留）。
    #[test]
    fn release_unknown_token_noop() {
        let s = store();
        let u = s.create("rs3@test.com", "pw", Role::User, 1000).unwrap();
        let tok = s.reserve_quota(&u.id, 50).unwrap();
        s.release_quota(&u.id, tok + 999);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 50);
        assert_eq!(after.used_quota, 0);
    }

    /// P0-2（关键）：并发两笔预留，清扫只清过期那条，另一条仍在。
    #[test]
    fn stale_sweep_keeps_fresh_reservation() {
        let s = store();
        let u = s.create("conc@test.com", "pw", Role::User, 1000).unwrap();
        let stale_tok = s.reserve_quota(&u.id, 300).unwrap();
        let fresh_tok = s.reserve_quota(&u.id, 200).unwrap();
        // 只把第一笔拨到 1 小时前（模拟挂死请求）
        {
            let mut by_id = s.by_id.write();
            let entry = by_id
                .get_mut(&u.id)
                .unwrap()
                .reservations
                .iter_mut()
                .find(|r| r.token == stale_tok)
                .unwrap();
            entry.at = chrono::Utc::now().timestamp() - 3600;
        }
        assert_eq!(s.release_stale_reservations(30 * 60), 1);
        let after = s.get_by_id(&u.id).unwrap();
        // 过期那笔被清、在进行中的那笔保留
        assert_eq!(after.reserved_total(), 200);
        assert!(after.reservations.iter().any(|r| r.token == fresh_tok));
        assert!(!after.reservations.iter().any(|r| r.token == stale_tok));
    }

    /// P0-3：结算超额 → used 记全额、overdraft 记超出部分、remaining 不为负。
    #[test]
    fn settle_overdraft_records_excess() {
        let s = store();
        let u = s.create("over@test.com", "pw", Role::User, 1000).unwrap();
        let tok = s.reserve_quota(&u.id, 1000).unwrap();
        // 实际消费 1500（估算严重不足）
        s.settle_quota(&u.id, tok, 1500);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 0);
        assert_eq!(after.used_quota, 1500);
        assert_eq!(after.overdraft_quota, 500);
        assert_eq!(after.remaining(), 0); // 不为负
    }

    /// G2：未超时的预留不受影响。
    #[test]
    fn fresh_reservation_not_released() {
        let s = store();
        let u = s.create("fresh@test.com", "pw", Role::User, 1000).unwrap();
        s.reserve_quota(&u.id, 300).unwrap();
        assert_eq!(s.release_stale_reservations(30 * 60), 0);
        let after = s.get_by_id(&u.id).unwrap();
        assert_eq!(after.reserved_total(), 300);
    }

    /// P1-4：旧快照后写被丢弃（版本号防乱序覆盖）。
    #[test]
    fn stale_snapshot_write_discarded() {
        let s = store();
        let u = s.create("ver@test.com", "pw", Role::User, 1000).unwrap();
        // 制造一次较新写入（update 同步落盘 + bump version）
        s.update(&u.id, |user| user.username = "newer".to_string())
            .unwrap();
        let newer = s.get_by_id(&u.id).unwrap();
        // 构造一份更旧的快照（version 更小、内容不同）
        let mut older = newer.clone();
        older.username = "stale".to_string();
        older.version = newer.version.saturating_sub(1);
        s.persist_async(&older);
        // 落盘仍是较新那份
        let on_disk: User = s.store.get(&format!("user:{}", u.id)).unwrap().unwrap();
        assert_eq!(on_disk.username, "newer");
        assert_eq!(on_disk.version, newer.version);
    }

    #[test]
    fn get_by_email() {
        let s = store();
        s.create("test@example.com", "pw", Role::User, 0).unwrap();
        assert!(s.get_by_email("test@example.com").is_some());
        assert!(s.get_by_email("nonexist@example.com").is_none());
    }
}
