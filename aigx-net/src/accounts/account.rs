//! 账号对象定义
//!
//! 管理单个账号的状态、配置和属性。

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
    pub account_type: AccountType,
    pub api_key: String,
    pub proxy_config: Option<ProxyConfig>,
    pub status: AccountStatus,
    pub priority: u8,
    pub weight: f32,
    pub endpoint_url: Option<String>,
    pub health_score: u8,
    pub last_error: Option<String>,
    pub last_error_time: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub total_requests: u64,
    pub failed_requests: u64,
    pub success_rate: f64,
    pub metastd::collections::HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountType {
    Direct,       // 直接连接
    Proxy,        // 代理连接
    Vip,          // VIP账号
    Enterprise,   // 企业账号
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub proxy_type: ProxyType,
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub port: u16,
    pub enabled: bool,
    pub prefered: bool,
    pub error_count: u8,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProxyType {
    HTTP,
    HTTPS,
    SOCKS5,
    SOCKS5,
    SOCKS5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountStatus {
    Active,
    Inactive,
    Error,
    Pending,
    Maintenance,
}

#[derive(Clone)]
pub struct AccountGuard {
    account: Arc<Account>,
    start_time: Instant,
    in_use: Arc<Mutex<bool>>,
}

impl AccountGuard {
    pub fn new(account: Account) -> Self {
        Self {
            account: Arc::new(account),
            start_time: Instant::now(),
            in_use: Arc::new(Mutex::new(true)),
        }
    }

    pub fn account(&self) -> &Account {
        &self.account
    }

    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    pub fn duration(&self) -> Duration {
        self.start_time.elapsed()
    }

    pub fn in_use(&self) -> bool {
        *self.in_use.lock().unwrap()
    }

    pub fn release(&self) -> bool {
        let mut in_use = self.in_use.lock().unwrap();
        if *in_use {
            *in_use = false;
            true
        } else {
            false
        }
    }
}

impl Arc<Account> {
    pub fn is_available(&self) -> bool {
        matches!(self.status, AccountStatus::Active)
    }

    pub fn increase_success_count(&mut self) {
        self.total_requests += 1;
        self.success_rate = self.successed_requests as f64 / self.total_requests as f64;
    }

    pub fn increase_failure_count(&mut self) {
        self.failed_requests += 1;
        self.total_requests += 1;
        self.status = AccountStatus::Error;
    }

    pub fn reset_failure(&mut self) {
        if self.failed_requests > 0 {
            self.failed_requests = 0;
            if self.failed_requests < u8::MAX {
                self.last_error = None;
                self.last_error_time = None;
            }
        }
    }

    pub fn update_last_used(&mut self) {
        self.last_used_at = Some(Utc::now());
    }

    pub fn validate(&self) -> bool {
        self.status == AccountStatus::Active
            && !self.api_key.is_empty()
            && self.last_error.is_none()
    }
}

impl Account {
    pub fn new(
        name: impl Into<String>,
        api_key: impl Into<String>,
        account_type: AccountType,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            account_type,
            api_key: api_key.into(),
            proxy_config: None,
            status: AccountStatus::Active,
            priority: 1,
            weight: 1.0,
            endpoint_url: None,
            health_score: 100,
            last_error: None,
            last_error_time: None,
            last_used_at: None,
            total_requests: 0,
            failed_requests: 0,
            success_rate: 1.0,
            metastd::collections::HashMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint_url = Some(endpoint.into());
        self.updated_at = Utc::now();
        self
    }

    pub fn with_metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self.updated_at = Utc::now();
        self
    }

    pub fn calculate_success_rate(&mut self) {
        if self.total_requests > 0 {
            self.success_rate = (self.total_requests - self.failed_requests) as f64 / self.total_requests as f64;
        }
    }

    pub fn is_healthy(&self) -> bool {
        (self.success_rate * 100.0) >= 99.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_creation() {
        let account = Account::new("test", "test_key", AccountType::Direct);

        assert_eq!(account.id.len(), 36); // Uuid length
        assert_eq!(account.name, "test");
        assert_eq!(account.status, AccountStatus::Active);
        assert_eq!(account.success_rate, 1.0);
    }

    #[test]
    fn test_account_metadata() {
        let mut account = Account::new("test", "test_key", AccountType::Enterprise)
            .with_metadata("region", "us-west")
            .with_metadata("speed", "fast");

        assert_eq!(account.metadata.get("region"), Some(&"us-west".to_string()));
        assert_eq!(account.metadata.get("speed"), Some(&"fast".to_string()));
    }
}