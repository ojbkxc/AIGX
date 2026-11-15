//! 云同步的**设置与状态**（KV 键 `sync:config` / `sync:status` / `sync:key_versions`）。
//!
//! ── 为什么单独占 KV 键，而不是塞进 AppConfig ──────────────────
//! 设置里有远端凭据；而且它**不能进同步快照**（否则远端凭据会被传上云、换机
//! 恢复时还会被旧凭据覆盖）。快照的键筛选见 [`super::snapshot::is_synced_key`]，
//! 三个 `sync:` 键在那一处被显式排除。
//!
//! ── 凭据存放 ────────────────────────────────────────────────
//! WebDAV 密码 / S3 Secret 经 `account_session::secret::seal_secret` 加密为
//! `enc1:` 信封（AES-256-GCM，密钥是本机数据目录的 `account.key`）。读取设置
//! 只回 `has_password` / `has_secret_key`。保存请求里密码字段**空串 = 沿用旧值**，
//! 非空 = 换新——前端不必知道旧值，也不会把旧值回传。
//!
//! ── 三个键各自的职责 ────────────────────────────────────────
//!   · `sync:config`    用户配置（开关 / 间隔 / 凭据密文）
//!   · `sync:status`    运行状态（上次成功 / 上次错误 / 内容哈希 / 连续失败数）
//!   · `sync:key_versions` 逐键版本表（键 → 内容哈希 + 首次观察到的变化时刻），
//!     为拉取合并提供「按条目 updated_at 新者胜」的比较基准

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::account_session::secret;
use crate::storage::FileStore;

/// 云同步配置在 KV 里的键。
pub const KV_KEY_CONFIG: &str = "sync:config";
/// 云同步状态在 KV 里的键。
pub const KV_KEY_STATUS: &str = "sync:status";
/// 逐键版本表在 KV 里的键。
pub const KV_KEY_VERSIONS: &str = "sync:key_versions";

/// 服务商标识：WebDAV。
pub const PROVIDER_WEBDAV: &str = "webdav";
/// 服务商标识：S3 兼容存储。
pub const PROVIDER_S3: &str = "s3";

/// 默认远端目录名（不会被别的程序占用）。
pub const DEFAULT_PATH: &str = "aigx-sync";
/// 默认自动同步间隔（秒）。
pub const DEFAULT_INTERVAL_SECS: i64 = 3600;
/// 默认最小同步间隔（秒）：两次成功同步之间的硬下限。
pub const DEFAULT_MIN_INTERVAL_SECS: i64 = 60;
/// 自动同步间隔下限（秒）。
pub const MIN_INTERVAL_FLOOR_SECS: i64 = 10;
/// 自动同步间隔上限（秒，一天）。
pub const MAX_INTERVAL_SECS: i64 = 86_400;
/// 最小同步间隔上限（秒，一小时）。
pub const MAX_MIN_INTERVAL_SECS: i64 = 3_600;
/// 默认 User-Agent。
pub const DEFAULT_USER_AGENT: &str = concat!("AIGX-CloudSync/", env!("CARGO_PKG_VERSION"));

/// WebDAV 设置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebDavConfig {
    /// 基地址（只认 http / https）。
    pub url: String,
    /// 用户名（空 = 匿名，不带认证）。
    pub username: String,
    /// `enc1:` 信封；空串 = 未设置。
    pub password: String,
}

/// S3 兼容存储设置。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Config {
    /// 自定义端点（空 = AWS 官方；`minio.local:9000` 默认按 https）。
    pub endpoint: String,
    /// 存储桶。
    pub bucket: String,
    /// 区域（Cloudflare R2 填 auto）。
    pub region: String,
    /// Access Key ID（非密文）。
    pub access_key: String,
    /// `enc1:` 信封；空串 = 未设置。
    pub secret_key: String,
    /// 对象键前缀（可选，和公共 `path` 拼接）。
    pub prefix: String,
}

/// 云同步运行状态（服务端维护，界面只读）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncStatus {
    /// 上次尝试时刻（毫秒）。
    pub last_attempt_at: i64,
    /// 上次成功时刻（毫秒）。
    pub last_success_at: i64,
    /// 上次成功的动作：`upload` / `pull`。
    pub last_action: String,
    /// 上次成功上传（或拉取后落定）的本机内容哈希。
    pub last_hash: String,
    /// 上次失败原因（成功后清空）。
    pub last_error: String,
    /// 连续失败次数（成功后清零；调度退避的依据）。
    pub consecutive_failures: u32,
    /// 失败退避到期时刻（毫秒）。
    pub retry_at: i64,
    /// 上次成功快照的字节数。
    pub last_bytes: u64,
}

/// 逐键版本信息。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyVersion {
    /// 值的内容哈希（sha256 十六进制）。
    pub hash: String,
    /// 首次观察到该值（或该变化）的时刻（毫秒）。
    pub updated_at: i64,
}

/// 整份云同步设置。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncConfig {
    /// 总开关（关着时自动同步与手动按钮都会被拒）。
    pub enabled: bool,
    /// 当前后端：`webdav` / `s3`。
    pub backend: String,
    /// 自动同步间隔（秒）。
    pub interval_secs: i64,
    /// 最小同步间隔（秒）：两次成功同步之间的硬下限。
    pub min_interval_secs: i64,
    /// 自定义 User-Agent（空 = 默认）。
    pub user_agent: String,
    /// 远端目录（WebDAV 相对路径 / S3 键路径的公共部分）。
    pub path: String,
    /// WebDAV 配置。
    pub webdav: WebDavConfig,
    /// S3 配置。
    pub s3: S3Config,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: PROVIDER_WEBDAV.to_string(),
            interval_secs: DEFAULT_INTERVAL_SECS,
            min_interval_secs: DEFAULT_MIN_INTERVAL_SECS,
            user_agent: String::new(),
            path: DEFAULT_PATH.to_string(),
            webdav: WebDavConfig::default(),
            s3: S3Config::default(),
        }
    }
}

impl SyncConfig {
    /// 收口空白字段并补默认值。
    pub fn normalize(&mut self) {
        self.backend = self.backend.trim().to_ascii_lowercase();
        if self.backend != PROVIDER_S3 {
            self.backend = PROVIDER_WEBDAV.to_string();
        }
        self.user_agent = self.user_agent.trim().to_string();
        self.path = self.path.trim().to_string();
        if self.path.is_empty() {
            self.path = DEFAULT_PATH.to_string();
        }
        if self.interval_secs <= 0 {
            self.interval_secs = DEFAULT_INTERVAL_SECS;
        }
        if self.min_interval_secs < 0 {
            self.min_interval_secs = 0;
        }
        self.webdav.url = self.webdav.url.trim().to_string();
        self.webdav.username = self.webdav.username.trim().to_string();
        self.s3.endpoint = self.s3.endpoint.trim().to_string();
        self.s3.bucket = self.s3.bucket.trim().to_string();
        self.s3.region = self.s3.region.trim().to_string();
        self.s3.access_key = self.s3.access_key.trim().to_string();
        self.s3.prefix = self.s3.prefix.trim().to_string();
    }

    /// 保存前的校验：只校验**当前后端**的必填项。
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_INTERVAL_FLOOR_SECS..=MAX_INTERVAL_SECS).contains(&self.interval_secs) {
            return Err(format!(
                "自动同步间隔必须在 {MIN_INTERVAL_FLOOR_SECS}–{MAX_INTERVAL_SECS} 秒之间"
            ));
        }
        if !(0..=MAX_MIN_INTERVAL_SECS).contains(&self.min_interval_secs) {
            return Err(format!(
                "最小同步间隔必须在 0–{MAX_MIN_INTERVAL_SECS} 秒之间"
            ));
        }
        if path_segments(&self.path)?.is_empty() {
            return Err("同步路径不能为空".to_string());
        }
        match self.backend.as_str() {
            PROVIDER_S3 => {
                if self.s3.bucket.is_empty() {
                    return Err("S3 存储桶不能为空".to_string());
                }
                if self.s3.region.is_empty() {
                    return Err("S3 区域不能为空（Cloudflare R2 填 auto）".to_string());
                }
                if self.s3.access_key.is_empty() {
                    return Err("S3 Access Key 不能为空".to_string());
                }
                if self.s3.secret_key.is_empty() {
                    return Err("S3 Secret Key 不能为空".to_string());
                }
                if !self.s3.prefix.is_empty() {
                    path_segments(&self.s3.prefix)?;
                }
            }
            _ => {
                let url = self.webdav.url.trim();
                if url.is_empty() {
                    return Err("WebDAV 地址不能为空".to_string());
                }
                match url::Url::parse(url) {
                    Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
                    Ok(parsed) => {
                        return Err(format!(
                            "WebDAV 只支持 http/https 地址（当前是 {}）",
                            parsed.scheme()
                        ))
                    }
                    Err(error) => return Err(format!("WebDAV 地址无效: {error}")),
                }
            }
        }
        Ok(())
    }

    /// 界面形态：密码 / Secret 只给「有没有」，绝不回显。
    pub fn public_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "backend": self.backend,
            "interval_secs": self.interval_secs,
            "min_interval_secs": self.min_interval_secs,
            "user_agent": self.user_agent,
            "path": self.path,
            "remote_path": self.remote_display(),
            "webdav": {
                "url": self.webdav.url,
                "username": self.webdav.username,
                "has_password": !self.webdav.password.is_empty(),
            },
            "s3": {
                "endpoint": self.s3.endpoint,
                "bucket": self.s3.bucket,
                "region": self.s3.region,
                "access_key": self.s3.access_key,
                "prefix": self.s3.prefix,
                "has_secret_key": !self.s3.secret_key.is_empty(),
            },
        })
    }

    /// 远端路径的展示形态。
    pub fn remote_display(&self) -> String {
        let path_segments = path_segments(&self.path).unwrap_or_default();
        match self.backend.as_str() {
            PROVIDER_S3 => {
                let mut segments = path_segments(&self.s3.prefix).unwrap_or_default();
                segments.extend(path_segments);
                if self.s3.bucket.is_empty() {
                    segments.join("/")
                } else {
                    format!("{}/{}", self.s3.bucket, segments.join("/"))
                }
            }
            _ => format!("/{}", path_segments.join("/")),
        }
    }

    /// 解出 WebDAV 密码明文（未设置 → 空串）。
    pub fn webdav_password_plain(&self) -> Result<String, String> {
        open_credential(&self.webdav.password, "WebDAV 密码")
    }

    /// 解出 S3 Secret 明文（未设置 → 空串）。
    pub fn s3_secret_plain(&self) -> Result<String, String> {
        open_credential(&self.s3.secret_key, "S3 Secret Key")
    }
}

/// 把用户输入的远端路径切成路径段（逐段过滤空段与 `.`，拒绝 `..` 与反斜杠）。
pub fn path_segments(raw: &str) -> Result<Vec<String>, String> {
    let mut segments = Vec::new();
    for part in raw.trim().trim_matches('/').split('/') {
        let part = part.trim();
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return Err("同步路径不能包含 .. 段".to_string());
        }
        if part.contains('\\') {
            return Err("同步路径请用 / 分隔".to_string());
        }
        segments.push(part.to_string());
    }
    Ok(segments)
}

// ─── 凭据加解密 ───────────────────────────────────────────────

/// 加密一个凭据明文（`enc1:` 信封）。
fn seal_credential(plain: &str) -> Result<String, String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    secret::seal_secret(plain, &secret::data_dir())
}

/// 解出一个凭据明文。
///
/// 存储值非空但解出空串 = 解密失败（`account.key` 被换过 / 数据被改过），
/// 必须报错而不是当成「没设密码」——后者会让请求静默地不带认证。
fn open_credential(stored: &str, label: &str) -> Result<String, String> {
    if stored.is_empty() {
        return Ok(String::new());
    }
    let plain = secret::open_secret(stored, &secret::data_dir());
    if plain.is_empty() {
        return Err(format!(
            "{label}解密失败（本机 account.key 可能已更换），请重新填写"
        ));
    }
    Ok(plain)
}

// ─── 保存请求 ─────────────────────────────────────────────────

/// WebDAV 保存请求（`None` = 不改）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct WebDavInput {
    pub url: Option<String>,
    pub username: Option<String>,
    /// 空串 = 沿用旧值。
    pub password: Option<String>,
}

/// S3 保存请求（`None` = 不改）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct S3Input {
    pub endpoint: Option<String>,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub access_key: Option<String>,
    /// 空串 = 沿用旧值。
    pub secret_key: Option<String>,
    pub prefix: Option<String>,
}

/// 保存 / 测试连接的请求形态（整份表单，只传要改的字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SaveInput {
    pub enabled: Option<bool>,
    pub backend: Option<String>,
    pub interval_secs: Option<i64>,
    pub min_interval_secs: Option<i64>,
    pub user_agent: Option<String>,
    pub path: Option<String>,
    pub webdav: Option<WebDavInput>,
    pub s3: Option<S3Input>,
}

/// 应用一次保存请求（**不落库**）：密码 / Secret 空串 = 沿用旧值。
pub fn apply_input(mut config: SyncConfig, input: SaveInput) -> Result<SyncConfig, String> {
    if let Some(enabled) = input.enabled {
        config.enabled = enabled;
    }
    if let Some(backend) = input.backend {
        config.backend = backend;
    }
    if let Some(interval) = input.interval_secs {
        config.interval_secs = interval;
    }
    if let Some(interval) = input.min_interval_secs {
        config.min_interval_secs = interval;
    }
    if let Some(user_agent) = input.user_agent {
        config.user_agent = user_agent;
    }
    if let Some(path) = input.path {
        config.path = path;
    }
    if let Some(webdav) = input.webdav {
        if let Some(url) = webdav.url {
            config.webdav.url = url;
        }
        if let Some(username) = webdav.username {
            config.webdav.username = username;
        }
        if let Some(password) = webdav.password {
            if !password.is_empty() {
                config.webdav.password = seal_credential(&password)?;
            }
        }
    }
    if let Some(s3) = input.s3 {
        if let Some(endpoint) = s3.endpoint {
            config.s3.endpoint = endpoint;
        }
        if let Some(bucket) = s3.bucket {
            config.s3.bucket = bucket;
        }
        if let Some(region) = s3.region {
            config.s3.region = region;
        }
        if let Some(access_key) = s3.access_key {
            config.s3.access_key = access_key;
        }
        if let Some(secret_key) = s3.secret_key {
            if !secret_key.is_empty() {
                config.s3.secret_key = seal_credential(&secret_key)?;
            }
        }
        if let Some(prefix) = s3.prefix {
            config.s3.prefix = prefix;
        }
    }
    config.normalize();
    config.validate()?;
    Ok(config)
}

// ─── KV 读写 ─────────────────────────────────────────────────

/// 读设置（读不出来 = 还没配过，回落默认值）。
pub fn load(store: &FileStore) -> SyncConfig {
    let mut config = store
        .get::<SyncConfig>(KV_KEY_CONFIG)
        .ok()
        .flatten()
        .unwrap_or_default();
    config.normalize();
    config
}

/// 读设置并要求「已启用」。
pub fn require_enabled(store: &FileStore) -> Result<SyncConfig, String> {
    let config = load(store);
    if !config.enabled {
        return Err("云同步未启用，请先在设置里打开开关".to_string());
    }
    config.validate()?;
    Ok(config)
}

/// 写设置。
pub fn save(store: &FileStore, config: &SyncConfig) -> Result<(), String> {
    store
        .put(KV_KEY_CONFIG, config)
        .map_err(|error| format!("保存云同步设置失败: {error}"))
}

/// 读状态。
pub fn load_status(store: &FileStore) -> SyncStatus {
    store
        .get::<SyncStatus>(KV_KEY_STATUS)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 写状态。
pub fn save_status(store: &FileStore, status: &SyncStatus) -> Result<(), String> {
    store
        .put(KV_KEY_STATUS, status)
        .map_err(|error| format!("保存云同步状态失败: {error}"))
}

/// 读逐键版本表。
pub fn load_versions(store: &FileStore) -> BTreeMap<String, KeyVersion> {
    store
        .get::<BTreeMap<String, KeyVersion>>(KV_KEY_VERSIONS)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 写逐键版本表。
pub fn save_versions(
    store: &FileStore,
    versions: &BTreeMap<String, KeyVersion>,
) -> Result<(), String> {
    store
        .put(KV_KEY_VERSIONS, versions)
        .map_err(|error| format!("保存云同步版本表失败: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_rejects_parent_dir() {
        assert!(path_segments("a/../b").is_err());
        assert_eq!(
            path_segments("/a//b/").unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(path_segments(" . / x ").unwrap(), vec!["x".to_string()]);
    }

    #[test]
    fn defaults_are_stable_and_valid() {
        let config = SyncConfig::default();
        assert_eq!(config.backend, PROVIDER_WEBDAV);
        assert_eq!(config.path, DEFAULT_PATH);
        assert_eq!(config.interval_secs, DEFAULT_INTERVAL_SECS);
        // 默认配置缺 WebDAV 地址 → 校验失败（保存前必须填）
        assert!(config.validate().is_err());
    }

    #[test]
    fn normalize_restores_blank_path_and_backend() {
        let mut config = SyncConfig {
            backend: "S3 ".to_string(),
            path: "  ".to_string(),
            interval_secs: 0,
            min_interval_secs: -5,
            ..SyncConfig::default()
        };
        config.normalize();
        assert_eq!(config.backend, PROVIDER_S3);
        assert_eq!(config.path, DEFAULT_PATH);
        assert_eq!(config.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(config.min_interval_secs, 0);
    }

    #[test]
    fn validate_only_checks_active_backend() {
        let config = SyncConfig {
            backend: PROVIDER_WEBDAV.to_string(),
            webdav: WebDavConfig {
                url: "https://dav.example.com/dav".to_string(),
                username: "u".to_string(),
                password: "enc1:whatever".to_string(),
            },
            ..SyncConfig::default()
        };
        // S3 那半边全空也不该拦住 WebDAV 的保存
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_bad_intervals_and_schemes() {
        let mut config = SyncConfig {
            webdav: WebDavConfig {
                url: "ftp://example.com".to_string(),
                ..WebDavConfig::default()
            },
            ..SyncConfig::default()
        };
        assert!(config.validate().is_err());
        config.webdav.url = "https://dav.example.com/dav".to_string();
        config.interval_secs = 1;
        assert!(config.validate().is_err());
        config.interval_secs = MAX_INTERVAL_SECS + 1;
        assert!(config.validate().is_err());
        config.interval_secs = DEFAULT_INTERVAL_SECS;
        config.min_interval_secs = MAX_MIN_INTERVAL_SECS + 1;
        assert!(config.validate().is_err());
    }

    #[test]
    fn public_json_never_exposes_secrets() {
        let config = SyncConfig {
            webdav: WebDavConfig {
                url: "https://dav.example.com/dav".to_string(),
                username: "u".to_string(),
                password: "enc1:SUPERSECRET".to_string(),
            },
            s3: S3Config {
                secret_key: "enc1:ALSOSECRET".to_string(),
                ..S3Config::default()
            },
            ..SyncConfig::default()
        };
        let text = config.public_json().to_string();
        assert!(!text.contains("SUPERSECRET"));
        assert!(!text.contains("ALSOSECRET"));
        let value = config.public_json();
        assert_eq!(value["webdav"]["has_password"], json!(true));
        assert_eq!(value["s3"]["has_secret_key"], json!(true));
    }

    #[test]
    fn remote_display_combines_prefix_and_path() {
        let config = SyncConfig {
            path: "aigx-sync/default".to_string(),
            backend: PROVIDER_S3.to_string(),
            s3: S3Config {
                bucket: "mybucket".to_string(),
                prefix: "backups".to_string(),
                ..S3Config::default()
            },
            ..SyncConfig::default()
        };
        assert_eq!(
            config.remote_display(),
            "mybucket/backups/aigx-sync/default"
        );

        let webdav = SyncConfig {
            path: "/aigx-sync/".to_string(),
            ..SyncConfig::default()
        };
        assert_eq!(webdav.remote_display(), "/aigx-sync");
    }

    #[test]
    fn apply_input_keeps_old_secret_on_blank() {
        // 不依赖真实 account.key 的路径：空串沿用旧值（不调用加解密）
        let config = SyncConfig {
            webdav: WebDavConfig {
                url: "https://dav.example.com/dav".to_string(),
                username: "u".to_string(),
                password: "enc1:old-envelope".to_string(),
            },
            ..SyncConfig::default()
        };
        let input = SaveInput {
            webdav: Some(WebDavInput {
                password: Some(String::new()),
                ..WebDavInput::default()
            }),
            ..SaveInput::default()
        };
        let applied = apply_input(config, input).unwrap();
        assert_eq!(applied.webdav.password, "enc1:old-envelope");
    }
}
