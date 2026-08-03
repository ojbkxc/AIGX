use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminConfig {
    pub password: String,
    #[serde(default)]
    pub session_secret: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            password: String::new(),
            session_secret: String::new(),
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

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            admin: AdminConfig::default(),
            usage: UsageConfig::default(),
            epay: EpayConfig::default(),
            server_address: String::new(),
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