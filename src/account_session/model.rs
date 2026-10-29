//! 账号会话数据模型 —— 渠道内的账号级调度单元。
//!
//! 一个 `AccountSession` 代表「渠道下的一把上游凭证」：上游地址、凭证、
//! 出口代理、优先级/权重/并发上限，以及运行态统计与按上游真名的冷却表。
//!
//! 设计要点（参照 Agent2API 的踩坑结论）：
//! - 凭证不进 `Channel` 结构，独立 KV（`acctses:{channel_id}:{id}`），
//!   避免渠道调度克隆与日志泄漏凭证；
//! - 冷却键必须是**上游真名**（映射后的模型名），否则映射渠道的冷却永远
//!   命中不到同一模型；
//! - `in_flight` 是运行态并发计数，不落盘。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 凭证形态：粘贴型 API Key（鉴权头由 provider 协议决定）。
pub const CREDENTIAL_KIND_API_KEY: &str = "api_key";

/// 粘贴型凭证。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credential {
    /// 凭证形态（`api_key` / `none`）。
    #[serde(default = "default_credential_kind")]
    pub kind: String,
    /// 密钥；落盘前以 `enc:` + base64 混淆（与 `Channel::api_key` 同口径）。
    #[serde(default)]
    pub secret: String,
}

impl Default for Credential {
    fn default() -> Self {
        Self {
            kind: default_credential_kind(),
            secret: String::new(),
        }
    }
}

fn default_credential_kind() -> String {
    CREDENTIAL_KIND_API_KEY.to_string()
}

impl Credential {
    /// 明文密钥（兼容带 `enc:` 前缀的存储值）。
    pub fn secret_plain(&self) -> String {
        crate::channel::Channel::decode_api_key_value(&self.secret)
    }

    /// 落盘前的编码副本（已编码则原样返回）。
    pub fn encoded(&self) -> Self {
        let mut next = self.clone();
        if !next.secret.is_empty() && !next.secret.starts_with("enc:") {
            next.secret = crate::channel::Channel::encode_api_key(&next.secret);
        }
        if next.kind.is_empty() {
            next.kind = default_credential_kind();
        }
        next
    }
}

/// 渠道下的单个上游账号。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSession {
    pub id: String,
    /// 归属渠道（`ChannelType::Account` 渠道的 id）。
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub name: String,
    /// 上游协议类型（`registry::ProviderSpec::kind`）。
    #[serde(default = "default_provider")]
    pub provider: String,
    /// 账号级上游地址；空 = 用 provider 目录里的默认地址。
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub credential: Credential,
    /// 账号级出口代理（`http://` / `https://` / `socks5://`）；空 = 直连。
    #[serde(default)]
    pub proxy: Option<String>,
    /// 配置态状态：`enabled` / `disabled` / `expired`。
    #[serde(default = "default_status")]
    pub status: String,
    /// 优先级（数值越大越优先，同优先级按权重随机）。
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_weight")]
    pub weight: u32,
    /// 单账号并发上限（0 = 不限制）。
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    /// 账号级模型白名单（空 = 不限，跟随渠道）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 上游真名 → 冷却到期 unix 秒。
    #[serde(default)]
    pub cooldowns: HashMap<String, i64>,
    #[serde(default)]
    pub last_used_at: Option<i64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub error_count: u64,
    /// 累计成功请求延迟（毫秒），与 `success_count` 求均值。
    #[serde(default)]
    pub total_latency_ms: u64,
    /// 凭证过期时间（unix 秒；None / 0 = 不过期）。
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// 运行态并发占用（不落盘）。
    #[serde(skip)]
    pub in_flight: u32,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_provider() -> String {
    crate::account_session::registry::DEFAULT_PROVIDER_KIND.to_string()
}

fn default_status() -> String {
    "enabled".to_string()
}

fn default_weight() -> u32 {
    1
}

fn default_max_concurrency() -> u32 {
    3
}

impl AccountSession {
    /// 新建账号（字段默认值集中在此，批量导入/管理接口共用）。
    pub fn new(channel_id: impl Into<String>, name: impl Into<String>) -> Self {
        let now = chrono::Utc::now().timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            channel_id: channel_id.into(),
            name: name.into(),
            provider: default_provider(),
            base_url: String::new(),
            credential: Credential::default(),
            proxy: None,
            status: default_status(),
            priority: 0,
            weight: default_weight(),
            max_concurrency: default_max_concurrency(),
            models: Vec::new(),
            cooldowns: HashMap::new(),
            last_used_at: None,
            last_error: None,
            success_count: 0,
            error_count: 0,
            total_latency_ms: 0,
            expires_at: None,
            in_flight: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// 配置态是否启用。
    pub fn is_enabled(&self) -> bool {
        self.status == "enabled"
    }

    /// 凭证是否已过期（`expires_at` 为 0 / None 视为不过期）。
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|at| at > 0 && at <= now)
    }

    /// 对外展示的运行态（`expired` / `disabled` / `enabled`）。
    pub fn runtime_status(&self, now: i64) -> &'static str {
        if self.is_expired(now) {
            "expired"
        } else if !self.is_enabled() {
            "disabled"
        } else {
            "enabled"
        }
    }

    /// 是否支持指定模型（`models` 为空 = 全部支持）。
    pub fn supports_model(&self, model: &str) -> bool {
        self.models.is_empty() || self.models.iter().any(|m| m == model)
    }

    /// 该上游模型的冷却到期时间（unix 秒）。
    pub fn cooling_until(&self, model: &str) -> Option<i64> {
        self.cooldowns
            .get(model)
            .copied()
            .filter(|until| *until > 0)
    }

    /// 该上游模型是否处于冷却期。
    pub fn is_cooling(&self, model: &str, now: i64) -> bool {
        self.cooling_until(model).is_some_and(|until| until > now)
    }

    /// 平均成功延迟（毫秒；无成功样本时为 0）。
    pub fn avg_latency_ms(&self) -> u64 {
        if self.success_count == 0 {
            0
        } else {
            self.total_latency_ms / self.success_count
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_roundtrip_keeps_plaintext_recoverable() {
        let cred = Credential {
            kind: CREDENTIAL_KIND_API_KEY.to_string(),
            secret: "sk-test-1234567890".to_string(),
        };
        let encoded = cred.encoded();
        assert!(encoded.secret.starts_with("enc:"));
        assert_eq!(encoded.secret_plain(), "sk-test-1234567890");
    }

    #[test]
    fn cooling_is_per_upstream_model() {
        let mut session = AccountSession::new("ch-1", "acct-a");
        session.cooldowns.insert("upstream-x".to_string(), 100);
        assert!(session.is_cooling("upstream-x", 50));
        assert!(!session.is_cooling("other-model", 50));
        assert!(!session.is_cooling("upstream-x", 120));
    }

    #[test]
    fn expired_credential_reports_expired_status() {
        let mut session = AccountSession::new("ch-1", "acct-a");
        session.expires_at = Some(10);
        assert_eq!(session.runtime_status(100), "expired");
        session.expires_at = None;
        assert_eq!(session.runtime_status(100), "enabled");
    }
}
