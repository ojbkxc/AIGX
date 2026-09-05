//! 账号访问守卫
//!
//! 管理账号的租借与释放：业务层持有 Guard 期间账号标记为占用，
//! Guard 离开作用域（Drop）时自动归还账号池。

use super::account_pool::AccountPool;
use super::account::Account;
use std::sync::{Arc, RwLock};

/// 账号租用守卫
///
/// 通过 Drop 语义保证账号一定被释放回池中
pub struct AccountGuard {
    account: Arc<Account>,
    pool: Arc<AccountPool>,
    released: RwLock<bool>,
}

impl AccountGuard {
    /// 创建守卫（从池中租出账号）
    pub fn new(account: Arc<Account>, pool: Arc<AccountPool>) -> Self {
        Self {
            account,
            pool,
            released: RwLock::new(false),
        }
    }

    /// 账号 ID
    pub fn id(&self) -> &str {
        &self.account.id
    }

    /// 是否已释放
    pub fn is_released(&self) -> bool {
        *self.released.read().unwrap()
    }

    /// 账号快照
    pub fn account(&self) -> &Account {
        &self.account
    }

    /// 主动释放（幂等）
    pub fn release(&self) {
        let mut released = self.released.write().unwrap();
        if *released {
            return;
        }
        *released = true;
        self.pool.reset_account(&self.account.id);
    }
}

impl Drop for AccountGuard {
    fn drop(&mut self) {
        self.release();
    }
}
