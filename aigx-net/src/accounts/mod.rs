//! 账号池管理模块
//!
//! 提供多账号凭据管理、状态跟踪和负载均衡功能
//!
//! 功能：
//! - 多账号凭据管理
//! - 账号状态跟踪（空闲/忙碌/错误/无效）
//! - 智能负载均衡（轮询/权重/延迟）
//! - 错误处理和降级策略
//! - 账号恢复机制

pub mod account;
pub mod account_pool;
pub mod account_guard;
pub use account::*;
pub use account_pool::*;
pub use account_guard::*;

use std::time::SystemTime;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 账号状态枚举
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountState {
    Idle = 0,      // 空闲可用
    Busy = 1,      // 正在使用中
    Error = 2,     // 当前请求失败
    Invalid = 3,   // 无效凭据，需要重新登录
}

impl AccountState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Idle,
            1 => Self::Busy,
            2 => Self::Error,
            3 => Self::Invalid,
            _ => Self::Idle,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Error => "error",
            Self::Invalid => "invalid",
        }
    }

    pub fn is_available(self) -> bool {
        self == Self::Idle
    }

    pub fn is_error(self) -> bool {
        self == Self::Error
    }

    pub fn is_invalid(self) -> bool {
        self == Self::Invalid
    }
}

/// 账号配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    /// 账号标识（邮箱或手机号）
    pub id: String,
    /// 密码
    pub password: String,
    /// 用户名（可选）
    pub username: Option<String>,
    /// 代理地址（可选）
    pub proxy_url: Option<String>,
    /// 优先级（权重）
    pub priority: u8,
    /// 重试次数
    pub max_retries: u16,
}

/// 账号状态信息
#[derive(Debug, Clone, Serialize)]
pub struct AccountStatus {
    pub id: String,
    pub state: AccountState,
    pub last_used_ms: i64,
    pub error_count: u8,
    pub consecutive_errors: u16,
    pub priority: u8,
    pub last_error_time_ms: Option<i64>,
}

/// 账号错误计数器
pub struct AccountErrorTracker {
    error_count: u8,
    consecutive_errors: u16,
    last_error_time: Option<i64>,
}

impl AccountErrorTracker {
    pub fn new() -> Self {
        Self {
            error_count: 0,
            consecutive_errors: 0,
            last_error_time: None,
        }
    }

    pub fn record_error(&mut self) {
        self.error_count += 1;
        self.consecutive_errors += 1;
        self.last_error_time = Some(SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64);
    }

    pub fn reset_error(&mut self) {
        self.consecutive_errors = 0;
    }

    pub fn needs_re elevate(&self) -> bool {
        self.consecutive_errors >= 3 // 连续3次错误需要重新验证
    }
}