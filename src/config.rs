use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;

use crate::notify::NotifyConfig;
use crate::payment::EpayConfig;

// ── Default value functions ──────────────────────────────────────────

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_data_dir() -> String {
    "~/.aigx".to_string()
}

fn default_daily_limit() -> u64 {
    10000
}

fn default_monthly_limit() -> u64 {
    100_000
}

// ── Config structs ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminConfig {
    /// 会话签名密钥。为空时首次启动由 `ensure_session_secret` 生成并持久化。
    #[serde(default)]
    pub session_secret: String,
    /// 会话有效期（小时），默认 24。
    #[serde(default = "default_session_ttl")]
    pub session_ttl_hours: i64,
}

fn default_session_ttl() -> i64 {
    24
}

/// 数据库配置 — 多数据库后端支持。
///
/// 渐进式迁移策略：
/// - `url` 为空（默认）：使用现有 FileStore（rusqlite bundled SQLite），零配置零依赖
/// - `url` 有值：启用 SeaORM 连接，支持 PostgreSQL/MySQL
///
/// 支持的 URL 格式：
/// - `postgres://user:pass@localhost:5432/aigx` — PostgreSQL
/// - `mysql://user:pass@localhost:3306/aigx` — MySQL
///
/// 注意：启用 SeaORM 后端需要编译时启用对应 feature：
/// ```text
/// cargo build --no-default-features --features "sea-orm,postgres"
/// cargo build --no-default-features --features "sea-orm,mysql"
/// ```
///
/// SQLite 场景由默认的 FileStore/rusqlite 后端覆盖，无需 SeaORM。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// 数据库连接 URL。留空则使用默认 FileStore（rusqlite）。
    ///
    /// 示例：
    /// - `sqlite://./data/aigx.db`
    /// - `postgres://user:pass@localhost:5432/aigx`
    /// - `mysql://user:pass@localhost:3306/aigx`
    #[serde(default)]
    pub url: String,
    /// 连接池最大连接数（默认 10）
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_max_connections() -> u32 {
    10
}

impl DatabaseConfig {
    /// 是否启用 SeaORM 后端（url 非空时启用）。
    pub fn is_enabled(&self) -> bool {
        !self.url.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageConfig {
    #[serde(default = "default_daily_limit")]
    pub daily_limit: u64,
    #[serde(default = "default_monthly_limit")]
    pub monthly_limit: u64,
    #[serde(default)]
    pub threshold: f64,
    #[serde(default = "default_api_timeout")]
    pub api_timeout_secs: u64,
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

fn default_api_timeout() -> u64 {
    120
}

fn default_max_retries() -> u32 {
    2
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub usage: UsageConfig,
    /// 易支付配置
    #[serde(default)]
    pub epay: EpayConfig,
    /// 站点对外访问地址，用于构造回调 URL
    #[serde(default)]
    pub server_address: String,
    /// 通知系统配置（Telegram + SMTP）
    #[serde(default)]
    pub notify: NotifyConfig,
    /// 数据库配置（多数据库后端支持）
    ///
    /// 留空则使用默认 FileStore（rusqlite），填 URL 则启用 SeaORM。
    #[serde(default)]
    pub database: DatabaseConfig,
    /// CORS 允许的来源列表。
    ///
    /// 生产环境应显式配置允许的前端来源（如 `["https://admin.example.com"]`）。
    /// 留空时默认允许 localhost 开发来源（见 main.rs build_cors_layer）。
    #[serde(default)]
    pub cors_origins: Vec<String>,
}

// ── Default implementations ──────────────────────────────────────────

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            data_dir: default_data_dir(),
        }
    }
}


impl Default for UsageConfig {
    fn default() -> Self {
        Self {
            daily_limit: default_daily_limit(),
            monthly_limit: default_monthly_limit(),
            threshold: 0.0,
            api_timeout_secs: default_api_timeout(),
            max_retries: default_max_retries(),
        }
    }
}


// ── ConfigManager ────────────────────────────────────────────────────

pub struct ConfigManager {
    config: RwLock<AppConfig>,
    path: PathBuf,
}

impl ConfigManager {
    /// 创建 ConfigManager。如果未指定路径，默认使用 ~/.aigx/config.toml。
    pub async fn new(path: Option<PathBuf>) -> Self {
        let path = path.unwrap_or_else(default_config_path);
        // 确保父目录存在
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        let config = RwLock::new(AppConfig::default());
        let manager = Self { config, path };

        // 如果配置文件已存在则加载，否则创建默认配置
        if manager.path.exists() {
            let _ = manager.load().await;
        } else {
            let _ = manager.save().await;
        }

        manager
    }

    /// 从磁盘加载配置
    pub async fn load(&self) -> AppConfig {
        let content = tokio::fs::read_to_string(&self.path)
            .await
            .unwrap_or_default();
        let config: AppConfig = toml::from_str(&content).unwrap_or_default();
        let cfg = config.clone();
        *self.config.write().await = config;
        cfg
    }

    /// 保存配置到磁盘
    pub async fn save(&self) -> anyhow::Result<()> {
        let config = self.config.read().await;
        let content = toml::to_string_pretty(&*config)?;
        tokio::fs::write(&self.path, content).await?;
        Ok(())
    }

    /// 更新配置并持久化
    pub async fn update(&self, config: AppConfig) -> anyhow::Result<()> {
        *self.config.write().await = config;
        self.save().await?;
        Ok(())
    }

    /// 获取当前配置的只读副本
    pub async fn get(&self) -> AppConfig {
        self.config.read().await.clone()
    }

    /// 获取配置路径
    #[allow(dead_code)]
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

/// 返回默认的配置文件路径：~/.aigx/config.toml
fn default_config_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    home.join(".aigx").join("config.toml")
}

/// 展开 ~ 为 home 目录
pub fn expand_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        let home = dirs::home_dir().unwrap_or_default();
        home.join(rest)
    } else if path == "~" {
        dirs::home_dir().unwrap_or_default()
    } else {
        PathBuf::from(path)
    }
}