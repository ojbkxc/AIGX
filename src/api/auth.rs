use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

use crate::storage::FileStore;

/// 管理会话
#[derive(Debug, Clone)]
pub struct Session {
    pub token: String,
    #[allow(dead_code)]
    pub email: String,
    #[allow(dead_code)]
    pub created_at: i64,
    pub expires_at: i64,
}

/// API 密钥
///
/// 参照 new-api token.go 的 Token 模型：name/group/allowed_models/expires_at/quota_limit/
/// used_quota/ip_limit/status。所有新字段用 `#[serde(default)]` 确保旧数据可加载。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub key: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub is_active: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
    /// 所属用户 ID（None=管理员级令牌）
    #[serde(default)]
    pub user_id: Option<String>,
    /// 分组（计费倍率依据，default "default"）
    #[serde(default = "default_group")]
    pub group: String,
    /// 模型白名单（None=不限）
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// 过期时间（unix timestamp，None=永不过期）
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// 额度上限（None=不限）
    #[serde(default)]
    pub quota_limit: Option<i64>,
    /// 已用额度
    #[serde(default)]
    pub used_quota: i64,
    /// IP 白名单（None=不限）
    #[serde(default)]
    pub ip_limit: Option<Vec<String>>,
    /// 状态：active / disabled（与 is_active 并存，向后兼容）
    #[serde(default = "default_active_status")]
    pub status: String,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_true() -> bool {
    true
}

fn default_group() -> String {
    "default".to_string()
}

fn default_active_status() -> String {
    "active".to_string()
}

impl ApiKey {
    /// 是否启用（兼容 is_active 与 status 双字段）
    pub fn is_enabled(&self) -> bool {
        self.is_active && self.status == "active"
    }

    /// 是否过期
    pub fn is_expired(&self) -> bool {
        match self.expires_at {
            Some(exp) => chrono::Utc::now().timestamp() >= exp,
            None => false,
        }
    }

    /// 是否允许使用指定模型
    pub fn allows_model(&self, model: &str) -> bool {
        match &self.allowed_models {
            None => true,
            Some(list) => list.iter().any(|m| m == model),
        }
    }

    /// 是否允许来自指定 IP
    pub fn allows_ip(&self, ip: &str) -> bool {
        match &self.ip_limit {
            None => true,
            Some(list) => list.iter().any(|allowed| allowed == ip),
        }
    }

    /// 是否超出额度上限
    pub fn is_quota_exhausted(&self) -> bool {
        match self.quota_limit {
            Some(limit) => self.used_quota >= limit,
            None => false,
        }
    }
}

/// API Key 鉴权失败原因（B22：结构化错误）。
///
/// 调用方按变体映射 HTTP 状态码，取代原先对错误消息文本的
/// `contains(...)` 匹配（消息措辞变化会静默改变 API 行为）。
#[derive(Debug, Clone, thiserror::Error)]
pub enum ApiKeyError {
    #[error("Invalid API key")]
    Invalid,
    #[error("API key is disabled")]
    Disabled,
    #[error("API key has expired")]
    Expired,
    #[error("Model '{0}' is not allowed for this API key")]
    ModelNotAllowed(String),
    #[error("API key quota exhausted")]
    QuotaExhausted,
    #[error("IP '{0}' is not allowed for this API key")]
    IpNotAllowed(String),
}

/// API 密钥存储
pub struct ApiKeyStore {
    store: Arc<FileStore>,
    keys: Arc<RwLock<HashMap<String, ApiKey>>>,
    key_hash_map: Arc<RwLock<HashMap<String, String>>>, // hash -> id
}

impl ApiKeyStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            keys: Arc::new(RwLock::new(HashMap::new())),
            key_hash_map: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 从存储加载密钥
    pub fn load(&self) -> Result<(), anyhow::Error> {
        let keys = self.store.list("apikey_")?;
        let mut map = self.keys.write();
        let mut hash_map = self.key_hash_map.write();

        for key in &keys {
            if let Some(api_key) = self.store.get::<ApiKey>(key)? {
                let id = key.strip_prefix("apikey_").unwrap_or(key).to_string();
                let hash = hash_api_key(&api_key.key);
                hash_map.insert(hash, id.clone());
                map.insert(id, api_key);
            }
        }

        tracing::info!("Loaded {} API keys", map.len());
        Ok(())
    }

    /// 验证 API Key
    pub fn validate(&self, key: &str) -> Option<ApiKey> {
        let actual_key = if let Some(stripped) = key.strip_prefix("sk-") {
            stripped
        } else {
            key
        };

        let hash = hash_api_key(actual_key);
        let hash_map = self.key_hash_map.read();

        if let Some(id) = hash_map.get(&hash) {
            let keys = self.keys.read();
            if let Some(api_key) = keys.get(id) {
                if api_key.is_active {
                    return Some(api_key.clone());
                }
            }
        }

        None
    }

    /// 生成新密钥（简化版，默认分组、无限制）
    pub fn generate(&self, name: &str) -> Result<ApiKey, anyhow::Error> {
        self.generate_with_options(CreateApiKeyOptions {
            name: name.to_string(),
            user_id: None,
            group: "default".to_string(),
            allowed_models: None,
            expires_at: None,
            quota_limit: None,
            ip_limit: None,
        })
    }

    /// 生成新密钥（带完整选项）
    pub fn generate_with_options(&self, opts: CreateApiKeyOptions) -> Result<ApiKey, anyhow::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let key = format!("sk-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
        let now = chrono::Utc::now().timestamp();

        let api_key = ApiKey {
            id: id.clone(),
            key,
            name: opts.name,
            is_active: true,
            created_at: now,
            last_used_at: None,
            user_id: opts.user_id,
            group: opts.group,
            allowed_models: opts.allowed_models,
            expires_at: opts.expires_at,
            quota_limit: opts.quota_limit,
            used_quota: 0,
            ip_limit: opts.ip_limit,
            status: "active".to_string(),
            updated_at: now,
        };

        let hash = hash_api_key(
            api_key
                .key
                .strip_prefix("sk-")
                .unwrap_or(&api_key.key),
        );

        self.store
            .put(&format!("apikey_{id}"), &api_key)?;
        self.keys.write().insert(id.clone(), api_key.clone());
        self.key_hash_map.write().insert(hash, id);

        Ok(api_key)
    }

    /// 更新密钥（读取-修改-写入）
    pub fn update(&self, id: &str, mutator: impl FnOnce(&mut ApiKey)) -> Result<ApiKey, anyhow::Error> {
        let mut api_key = self
            .keys
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("api key not found"))?;
        let old_hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));
        mutator(&mut api_key);
        api_key.updated_at = chrono::Utc::now().timestamp();
        api_key.is_active = api_key.status == "active";
        let new_hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));

        self.store.put(&format!("apikey_{id}"), &api_key)?;
        self.keys.write().insert(id.to_string(), api_key.clone());
        let mut hm = self.key_hash_map.write();
        if old_hash != new_hash {
            hm.remove(&old_hash);
        }
        hm.insert(new_hash, id.to_string());
        Ok(api_key)
    }

    /// 列出某用户的所有密钥
    pub fn list_by_user(&self, user_id: &str) -> Vec<ApiKey> {
        self.keys
            .read()
            .values()
            .filter(|k| k.user_id.as_deref() == Some(user_id))
            .cloned()
            .collect()
    }

    /// 校验 API Key 并执行全部鉴权检查。
    ///
    /// 参照 new-api token.go：状态、过期、模型白名单、额度上限、IP 限制。
    ///
    /// B22：返回结构化错误 `ApiKeyError`——原先返回裸 String，调用方只能靠
    /// `msg.contains(...)` 字符串匹配判断 HTTP 状态码（401/403），错误消息
    /// 一旦调整措辞就会静默改变 API 行为。
    pub fn validate_request(
        &self,
        key: &str,
        model: &str,
        ip: Option<&str>,
    ) -> Result<ApiKey, ApiKeyError> {
        let api_key = self.validate(key).ok_or(ApiKeyError::Invalid)?;
        if !api_key.is_enabled() {
            return Err(ApiKeyError::Disabled);
        }
        if api_key.is_expired() {
            return Err(ApiKeyError::Expired);
        }
        if !api_key.allows_model(model) {
            return Err(ApiKeyError::ModelNotAllowed(model.to_string()));
        }
        if api_key.is_quota_exhausted() {
            return Err(ApiKeyError::QuotaExhausted);
        }
        if let Some(ip) = ip {
            if !api_key.allows_ip(ip) {
                return Err(ApiKeyError::IpNotAllowed(ip.to_string()));
            }
        }
        Ok(api_key)
    }

    /// 扣减令牌已用额度（配额不足返回 false）
    pub fn charge_quota(&self, id: &str, amount: i64) -> bool {
        if amount <= 0 {
            return true;
        }
        let mut keys = self.keys.write();
        let api_key = match keys.get_mut(id) {
            Some(k) => k,
            None => return false,
        };
        if let Some(limit) = api_key.quota_limit {
            if api_key.used_quota + amount > limit {
                return false;
            }
        }
        api_key.used_quota += amount;
        api_key.last_used_at = Some(chrono::Utc::now().timestamp());
        let snapshot = api_key.clone();
        drop(keys);
        if let Err(e) = self.store.put(&format!("apikey_{id}"), &snapshot) {
            tracing::error!("Failed to persist apikey {} charge_quota: {}", id, e);
        }
        true
    }

    /// 重置已用额度
    pub fn reset_used_quota(&self, id: &str) -> bool {
        let mut keys = self.keys.write();
        if let Some(api_key) = keys.get_mut(id) {
            api_key.used_quota = 0;
            api_key.updated_at = chrono::Utc::now().timestamp();
            let snapshot = api_key.clone();
            drop(keys);
            if let Err(e) = self.store.put(&format!("apikey_{id}"), &snapshot) {
                tracing::error!("Failed to persist apikey {} reset_used_quota: {}", id, e);
            }
            true
        } else {
            false
        }
    }

    /// 删除密钥
    pub fn delete(&self, id: &str) -> Result<(), anyhow::Error> {
        let keys = self.keys.read();
        if let Some(api_key) = keys.get(id) {
            let hash = hash_api_key(
                api_key
                    .key
                    .strip_prefix("sk-")
                    .unwrap_or(&api_key.key),
            );
            self.key_hash_map.write().remove(&hash);
        }
        drop(keys);

        self.store.delete(&format!("apikey_{id}"))?;
        self.keys.write().remove(id);
        Ok(())
    }

    /// 列出所有密钥
    pub fn list(&self) -> Vec<ApiKey> {
        self.keys.read().values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.keys.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.read().is_empty()
    }
}

/// 创建 API Key 的选项
#[derive(Debug, Clone)]
pub struct CreateApiKeyOptions {
    pub name: String,
    pub user_id: Option<String>,
    pub group: String,
    pub allowed_models: Option<Vec<String>>,
    pub expires_at: Option<i64>,
    pub quota_limit: Option<i64>,
    pub ip_limit: Option<Vec<String>>,
}


/// 会话存储 - 使用 HMAC 签名方式，无需共享内存状态
pub struct SessionStore {
    secret: String,
    expiry_hours: i64,
}

impl SessionStore {
    pub fn new(session_secret: &str, expiry_hours: i64) -> Self {
        Self {
            secret: session_secret.to_string(),
            expiry_hours,
        }
    }

    /// 创建会话并返回签名的 token
    pub fn create_session(&self, email: &str) -> Session {
        let now = chrono::Utc::now().timestamp();
        let expires_at = now + self.expiry_hours * 3600;
        let token = self.sign_token(email, expires_at);

        Session {
            token,
            email: email.to_string(),
            created_at: now,
            expires_at,
        }
    }

    /// 验证签名的 token 并返回会话信息
    pub fn validate_session(&self, token: &str) -> Option<Session> {
        let parts: Vec<&str> = token.splitn(3, '.').collect();
        if parts.len() != 3 {
            return None;
        }

        let (email_b64, expires_str, _sig) = (parts[0], parts[1], parts[2]);

        // 解码 email（base64 URL_SAFE_NO_PAD，避免 email 中的 '.' 干扰 splitn）
        let email_bytes = URL_SAFE_NO_PAD.decode(email_b64).ok()?;
        let email_str = String::from_utf8(email_bytes).ok()?;

        // 验证签名
        let expected_sig = self.compute_signature(&email_str, expires_str);
        if _sig != expected_sig {
            return None;
        }

        // 解析过期时间
        let expires_at: i64 = expires_str.parse().ok()?;
        let now = chrono::Utc::now().timestamp();
        if expires_at <= now {
            return None;
        }

        let now_ts = chrono::Utc::now().timestamp();
        Some(Session {
            token: token.to_string(),
            email: email_str,
            created_at: now_ts,
            expires_at,
        })
    }

    /// 生成签名 token：base64(email).expires_at.hmac_hex
    ///
    /// email 经 base64 URL_SAFE_NO_PAD 编码，避免 email 中的 '.'（如 user@example.com）
    /// 干扰 token 的 `splitn(3, '.')` 解析。
    fn sign_token(&self, email: &str, expires_at: i64) -> String {
        let sig = self.compute_signature(email, &expires_at.to_string());
        let email_b64 = URL_SAFE_NO_PAD.encode(email.as_bytes());
        format!("{}.{}.{}", email_b64, expires_at, sig)
    }

    fn compute_signature(&self, email: &str, expires: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes())
            .expect("HMAC key");
        mac.update(email.as_bytes());
        mac.update(b".");
        mac.update(expires.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

/// 计算 API Key 的哈希
fn hash_api_key(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hex::encode(hasher.finalize())
}