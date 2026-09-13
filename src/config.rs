use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;

use crate::notify::NotifyConfig;
use crate::oauth::github::GithubOauthConfig;
use crate::oauth::google::GoogleOauthConfig;
use crate::oauth::linuxdo::LinuxDoOauthConfig;
use crate::payment::stripe::StripeConfig;
use crate::payment::EpayConfig;

/// 默认 cf-ai-gw Worker 地址（AI Binding 方式调用 Cloudflare Workers AI）。
///
/// AIGX 不再直接调用 Cloudflare REST API（`api.cloudflare.com`），而是通过
/// cf-ai-gw Worker 桥接——Worker 内部使用 **AI Binding**（`env.AI.run()`）调用
/// Workers AI。本地址即 cf-ai-gw Worker 的部署地址。
pub const DEFAULT_CF_BINDING_URL: &str = "http://127.0.0.1:8787";

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

/// 注册赠送配额默认 0（与 Epay price 口径一致：1 元 = price 单位配额）。
fn default_register_quota() -> i64 {
    0
}

/// 注册开关默认开启（向后兼容：升级不停用既有注册入口）。
fn default_register_enabled() -> bool {
    true
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

fn default_cf_binding_url() -> String {
    DEFAULT_CF_BINDING_URL.to_string()
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

/// AI 运维 Agent 配置（`[agent]` 段）——自举式运维工作台。
///
/// Agent 的"大脑"走 AIGX 自己的渠道（进程内直调 bridge，复用渠道调度/
/// 熔断/亲和，但不经 HTTP 端口与计费），模型与渠道均可配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// 是否启用 AI 运维 Agent（默认关闭，避免未配置模型时误启）。
    #[serde(default)]
    pub enabled: bool,
    /// Agent 推理模型名（走 AIGX 渠道，缺省用全局映射解析）。
    #[serde(default)]
    pub model: String,
    /// 锁定渠道 ID：非空时只走该渠道（排障用）；空 = 走全局调度。
    #[serde(default)]
    pub channel: String,
    /// 单次任务最大推理轮数（防死循环）。
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// 高危工具审批超时（秒），超时未响应视为拒绝。
    #[serde(default = "default_approval_timeout")]
    pub approval_timeout_secs: u64,
    /// 对话历史最大保留条数（上下文压缩：超出时只保留最近 N 条）。
    #[serde(default = "default_max_history")]
    pub max_history: usize,
}

fn default_max_turns() -> usize {
    12
}

fn default_approval_timeout() -> u64 {
    300
}

fn default_max_history() -> usize {
    20
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            model: String::new(),
            channel: String::new(),
            max_turns: default_max_turns(),
            approval_timeout_secs: default_approval_timeout(),
            max_history: default_max_history(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageConfig {
    #[serde(default = "default_daily_limit")]
    pub daily_limit: u64,
    #[serde(default = "default_monthly_limit")]
    pub monthly_limit: u64,
    /// 新用户注册赠送配额（与 monthly_limit 分离，避免"月度限额"被当作
    /// 充值配额赠送——原实现二者混用导致计费口径错乱）。
    #[serde(default = "default_register_quota")]
    pub register_quota: i64,
    /// 是否开放自助注册（对齐 new-api RegisterEnabled，false 时注册端点 403）
    #[serde(default = "default_register_enabled")]
    pub register_enabled: bool,
    /// 每个邀请人的奖励配额（对齐 new-api QuotaForInviter，0=关闭邀请奖励）
    #[serde(default)]
    pub quota_for_inviter: i64,
    /// 每个受邀新用户的奖励配额（对齐 new-api QuotaForInvitee，0=无奖励）
    #[serde(default)]
    pub quota_for_invitee: i64,
    /// 签到设置（对齐 new-api CheckinSetting：开关 + 奖励区间）
    #[serde(default)]
    pub checkin: crate::user::checkin::CheckinSetting,
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
    /// Stripe 支付配置
    #[serde(default)]
    pub stripe: StripeConfig,
    /// GitHub OAuth configuration
    #[serde(default)]
    pub github_oauth: GithubOauthConfig,
    /// Google OAuth configuration
    #[serde(default)]
    pub google_oauth: GoogleOauthConfig,
    /// LinuxDO OAuth configuration
    #[serde(default)]
    pub linuxdo_oauth: LinuxDoOauthConfig,
    /// 站点对外访问地址，用于构造回调 URL
    #[serde(default)]
    pub server_address: String,
    /// 通知系统配置（Telegram + SMTP）
    #[serde(default)]
    pub notify: NotifyConfig,
    /// cf-ai-gw Worker 部署地址（AI Binding 桥接）。
    ///
    /// AIGX 通过该地址调用 cf-ai-gw Worker，Worker 内部使用 **AI Binding**
    /// （`env.AI.run()`）调用 Cloudflare Workers AI，而非 REST API。
    /// 默认 `http://127.0.0.1:8787`（wrangler dev 本地调试）。
    #[serde(default = "default_cf_binding_url")]
    pub cf_binding_url: String,
    /// 数据库配置（多数据库后端支持）
    ///
    /// 留空则使用默认 FileStore（rusqlite），填 URL 则启用 SeaORM。
    #[serde(default)]
    pub database: DatabaseConfig,
    /// CORS 允许的来源列表（对外暴露时务必显式配置）。
    ///
    /// 生产环境应显式配置允许的前端来源（如 `["https://admin.example.com"]`）。
    /// 留空时优先使用 `server_address` 作为唯一允许来源；两者皆空则不发 CORS 头
    /// （跨源请求被浏览器拦截，同源不受影响），见 main.rs build_cors_layer。
    #[serde(default)]
    pub cors_origins: Vec<String>,
    /// 是否信任反向代理的 `X-Forwarded-For` / `X-Real-IP` 头。
    ///
    /// - `false`（默认）：忽略这两个头，IP 记为 "unknown"（直连部署无代理，
    ///   客户端可伪造 XFF 头绕过 IP 白名单/黑名单或伪造日志归属）。
    /// - `true`：仅在确认部署于可信反代（Nginx/CF 等）之后开启，此时
    ///   由代理负责剥离外部传入的 XFF 并追加真实 IP。
    #[serde(default)]
    pub trust_proxy_headers: bool,
    /// AI 运维 Agent 配置（`[agent]` 段，自举式运维工作台）。
    #[serde(default)]
    pub agent: AgentConfig,
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
            register_quota: default_register_quota(),
            register_enabled: default_register_enabled(),
            quota_for_inviter: 0,
            quota_for_invitee: 0,
            checkin: Default::default(),
            threshold: 0.0,
            api_timeout_secs: default_api_timeout(),
            max_retries: default_max_retries(),
        }
    }
}

// ── ConfigManager ────────────────────────────────────────────────────

/// 是否信任代理头的进程级快照（由 main 启动时写入）。
///
/// `extract_client_ip` 是同步函数（18 个调用点都无法 await），而配置
/// 在运行期可经管理端修改——这里取启动时快照即可：改这个开关需要
/// 同时调整反代部署方式，重启进程应用是合理语义。
static TRUST_PROXY_HEADERS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// 读取 trust_proxy_headers 快照（供 `extract_client_ip` 同步访问）
pub fn trust_proxy_headers() -> bool {
    TRUST_PROXY_HEADERS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 启动时由 main 写入快照（见 main.rs）
pub fn set_trust_proxy_headers(v: bool) {
    TRUST_PROXY_HEADERS.store(v, std::sync::atomic::Ordering::Relaxed);
}

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
        let config = apply_env_overrides(config);
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

/// 环境变量覆盖配置（容器部署用）。
///
/// 格式：`AIGX_<SECTION>__<FIELD>`，双下划线分隔 section 与 field，field 用小写。
/// 例如 `AIGX_SERVER__HOST=0.0.0.0` 覆盖 `config.server.host`，
/// `AIGX_SERVER__PORT=9527` 覆盖 `config.server.port`。
/// 仅支持标量覆盖（String / 整数），在 config.toml 加载后应用。
fn apply_env_overrides(mut config: AppConfig) -> AppConfig {
    if let Ok(v) = std::env::var("AIGX_SERVER__HOST") {
        config.server.host = v;
    }
    if let Ok(v) = std::env::var("AIGX_SERVER__PORT") {
        if let Ok(port) = v.parse() {
            config.server.port = port;
        }
    }
    if let Ok(v) = std::env::var("AIGX_SERVER__DATA_DIR") {
        config.server.data_dir = v;
    }
    if let Ok(v) = std::env::var("AIGX_CF_BINDING_URL") {
        config.cf_binding_url = v;
    }
    if let Ok(v) = std::env::var("AIGX_SERVER_ADDRESS") {
        config.server_address = v;
    }
    config
}
