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
/// 凭证形态：登录态（Cookie / JWT + 专用请求头，如 Cline / Qoder / Trae）。
pub const CREDENTIAL_KIND_SESSION: &str = "session";
/// 凭证形态：免鉴权（bridge 不发送鉴权头，用于 IP 白名单型上游）。
pub const CREDENTIAL_KIND_NONE: &str = "none";

/// `cooldowns` 里的整号冷却哨兵键。
///
/// 账号级限流（429 + account/organization 措辞）、认证失败、余额耗尽
/// 影响的是**整个账号**而不是某个模型。用 `"*"` 占位避免逐模型写一遍
/// （漏掉未请求过的模型会让它们仍被调度）。`"*"` 不是合法的上游模型名
/// （模型名只允许字母数字与 `- _ . / :`），不会与真实模型冲突。
pub const ACCOUNT_WIDE_COOLDOWN: &str = "*";

/// 粘贴型凭证。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credential {
    /// 凭证形态（`api_key` / `none`）。
    #[serde(default = "default_credential_kind")]
    pub kind: String,
    /// 主密钥；落盘前加密（见 [`crate::account_session::secret`]）。
    ///
    /// 大多数家只��这一个字段（单 token / cookie / key）。
    #[serde(default)]
    pub secret: String,
    /// 额外凭证字段（多层凭证）。
    ///
    /// 有些家要的不止一个值 —— Agent2API 的账号记录就是**扁平 JSON**：
    /// Trae 要 `accessToken` + `refreshToken` + `deviceId` + `machineId` + `uid`，
    /// CodeArts 要 `access_key_id` + `secret_access_key` + `security_token`，
    /// OfficeAce 要**两层**（网关凭据 `model_app_key`/`model_app_secret` +
    /// 控制面临时 AK/SK/STS）。单 secret 字段承载不了这些。
    ///
    /// 这里放**除主密钥外**的具名字段；主密钥仍走 `secret`（多数家与
    /// Agent2API 的 `accessToken`/`token`/`cookie` 对应）。
    ///
    /// 值同样加密落盘（与 `secret` 同信封）。
    #[serde(default)]
    pub fields: std::collections::HashMap<String, String>,
}

impl Default for Credential {
    fn default() -> Self {
        Self {
            kind: default_credential_kind(),
            secret: String::new(),
            fields: std::collections::HashMap::new(),
        }
    }
}

fn default_credential_kind() -> String {
    CREDENTIAL_KIND_API_KEY.to_string()
}

impl Credential {
    /// 明文密钥。
    ///
    /// 认三种存储形态（见 [`crate::account_session::secret::open_secret`]）：
    /// `enc1:`（AES-256-GCM 真加密）、`enc:`（旧的可逆 base64 混淆，存量）、
    /// 明文（尚未加密的存量）。
    pub fn secret_plain(&self) -> String {
        crate::account_session::secret::open_secret(
            &self.secret,
            &crate::account_session::secret::data_dir(),
        )
    }

    /// 落盘前的加密副本。
    ///
    /// 新写入一律用 `enc1:`（AES-256-GCM，密钥在库外的 `account.key`）；
    /// 已是信封的原样返回（幂等）。加密失败时**不退化成明文** —— 那样等于
    /// 让「加密」变成摆设。
    pub fn encoded(&self) -> Self {
        let mut next = self.clone();
        let dir = crate::account_session::secret::data_dir();
        next.secret = crate::account_session::secret::seal_or_keep(&next.secret, &dir);
        for value in next.fields.values_mut() {
            if !value.is_empty() && !value.starts_with("enc1:") {
                *value = crate::account_session::secret::seal_or_keep(value, &dir);
            }
        }
        if next.kind.is_empty() {
            next.kind = default_credential_kind();
        }
        next
    }

    /// 取一个具名凭证字段的明文。
    ///
    /// `name` 为空时返回主密钥（`secret`）—— 调用方可以只问「这个家的
    /// 凭证」而不用关心它落在 `secret` 还是 `fields`。
    pub fn field_plain(&self, name: &str) -> String {
        if name.is_empty() {
            return self.secret_plain();
        }
        self.fields
            .get(name)
            .map(|v| {
                crate::account_session::secret::open_secret(
                    v,
                    &crate::account_session::secret::data_dir(),
                )
            })
            .unwrap_or_default()
    }

    /// 设一个具名凭证字段（明文入，明文存 —— 落盘前由 `encoded` 处理）。
    pub fn set_field(&mut self, name: &str, value: &str) {
        if name.is_empty() {
            self.secret = value.to_string();
        } else {
            self.fields.insert(name.to_string(), value.to_string());
        }
    }

    /// 这个家需要哪些具名字段（不含主密钥）。
    ///
    /// 移植 Agent2API 的账号结构时按它填 —— 少填的字段在调用处会拿到空串，
    /// 上游会明确报「缺什么」，而不是静默用空值发请求。
    pub fn field_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.fields.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        names
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
    /// 账号级对话路径覆盖（如 `/api/chat`）；空 = 默认 `<base>/chat/completions`。
    ///
    /// 登录态型供应商的私有接口常常不是标准 OpenAI 路径，用它来定「请求形态」。
    #[serde(default)]
    pub chat_path: Option<String>,
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
    /// 账号级额外请求头（登录态型供应商用：逐家补齐专用头，如 `x-token` / `origin`）。
    ///
    /// 与 provider 目录里的默认头合并，同名时以账号级为准；
    /// 值里的 `{credential}` 会被替换成凭证明文。
    #[serde(default)]
    pub extra_headers: HashMap<String, String>,
    /// 续期令牌（refreshToken / Coding Plan JWT / deviceId 等）。
    ///
    /// 与 `credential.secret` 同口径落盘（`enc1:` AES-256-GCM，见
    /// [`crate::account_session::secret`]；存量 `enc:` 读兼容）。AIGX 目前
    /// **不自动续期**（各家续期接口不同、失败形态各异），这个字段先存下来，
    /// 供后续接入各家 refresh 链路与手工导出使用；空 = 该家没有续期凭证。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub refresh_secret: String,
    /// 上游真名 → 冷却到期 unix 秒。
    ///
    /// 特殊键 [`ACCOUNT_WIDE_COOLDOWN`]（`"*"`）表示**整号冷却**：账号级
    /// 限流 / 认证失败 / 余额耗尽会让该账号所有模型都不可用，此时逐模型
    /// 写一遍既浪费存储又漏掉未请求过的模型。前端展示时 `*` 渲染成
    /// 「全部模型」。
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
    /// 连续失败次数（成功即清零；用于指数退避，区别于累计 `error_count`）。
    #[serde(default)]
    pub consecutive_errors: u64,
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
            chat_path: None,
            credential: Credential::default(),
            proxy: None,
            status: default_status(),
            priority: 0,
            weight: default_weight(),
            max_concurrency: default_max_concurrency(),
            models: Vec::new(),
            extra_headers: HashMap::new(),
            refresh_secret: String::new(),
            cooldowns: HashMap::new(),
            last_used_at: None,
            last_error: None,
            success_count: 0,
            error_count: 0,
            consecutive_errors: 0,
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

    /// 是否支持指定模型（兼容「用户可见名 / 上游真名」两种配置口径）。
    ///
    /// 数据面传入的 `model` 是映射后的上游真名，而管理面白名单通常按渠道
    /// `models`（用户可见名）填写；两者任一命中即视为支持。
    pub fn supports_model_any(&self, model: &str, alias: Option<&str>) -> bool {
        if self.supports_model(model) {
            return true;
        }
        alias.is_some_and(|other| self.models.iter().any(|m| m == other))
    }

    /// 该上游模型的冷却到期时间（unix 秒）。
    ///
    /// 整号冷却优先返回：账号级限流期间问任何模型都该得到那个到期时间。
    pub fn cooling_until(&self, model: &str) -> Option<i64> {
        self.cooldowns
            .get(ACCOUNT_WIDE_COOLDOWN)
            .or_else(|| self.cooldowns.get(model))
            .copied()
            .filter(|until| *until > 0)
    }

    /// 该上游模型是否处于冷却期。
    pub fn is_cooling(&self, model: &str, now: i64) -> bool {
        self.cooling_until(model).is_some_and(|until| until > now)
    }

    /// 平均成功延迟（毫秒；无成功样本时为 0）。
    pub fn avg_latency_ms(&self) -> u64 {
        self.total_latency_ms
            .checked_div(self.success_count)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 凭证加解密用**全局**的 data_dir（密钥文件所在目录）—— 并行测试会
    /// 互相换钥匙，所以这里用跨模块共享锁（`secret::test_lock`）把
    /// 「设目录 + 跑断言」串起来（自有锁只管得住本模块，管不住 `channel`
    /// 等其他模块同样操作全局目录的测试）。
    fn with_secret_dir<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let _guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-cred-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        f()
    }

    #[test]
    fn credential_roundtrip_keeps_plaintext_recoverable() {
        with_secret_dir("roundtrip", || {
            let cred = Credential {
                kind: CREDENTIAL_KIND_API_KEY.to_string(),
                secret: "sk-test-1234567890".to_string(),
                fields: std::collections::HashMap::new(),
            };
            let encoded = cred.encoded();
            // 新写入用 AES-256-GCM 真加密（不是可逆的 base64 混淆）
            assert!(encoded.secret.starts_with("enc1:"));
            assert!(!encoded.secret.contains("sk-test"));
            assert_eq!(encoded.secret_plain(), "sk-test-1234567890");
        });
    }

    #[test]
    fn credential_multi_fields_roundtrip() {
        with_secret_dir("multi", || {
            let mut cred = Credential {
                kind: CREDENTIAL_KIND_SESSION.to_string(),
                secret: "access-token-value".to_string(),
                fields: std::collections::HashMap::new(),
            };
            cred.set_field("refreshToken", "refresh-value");
            cred.set_field("deviceId", "device-value");
            let encoded = cred.encoded();
            // 每个字段各自加密
            assert!(encoded.fields["refreshToken"].starts_with("enc1:"));
            assert!(!encoded.fields["deviceId"].contains("device-value"));
            assert_eq!(encoded.secret_plain(), "access-token-value");
            assert_eq!(encoded.field_plain("refreshToken"), "refresh-value");
            assert_eq!(encoded.field_plain("deviceId"), "device-value");
            assert_eq!(encoded.field_names(), vec!["deviceId", "refreshToken"]);
        });
    }

    #[test]
    fn legacy_enc_credential_still_readable() {
        with_secret_dir("legacy", || {
            // 存量数据是 base64 混淆的 enc: 前缀 —— 必须还能读
            let legacy = crate::channel::Channel::encode_api_key("sk-legacy-value");
            let cred = Credential {
                kind: CREDENTIAL_KIND_API_KEY.to_string(),
                secret: legacy,
                fields: std::collections::HashMap::new(),
            };
            assert_eq!(cred.secret_plain(), "sk-legacy-value");
        });
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
