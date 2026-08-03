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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApiKey {
    pub id: String,
    pub key: String,
    pub name: String,
    pub is_active: bool,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
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

    /// 生成新密钥
    pub fn generate(&self, name: &str) -> Result<ApiKey, anyhow::Error> {
        let id = uuid::Uuid::new_v4().to_string();
        let key = format!("sk-{}", uuid::Uuid::new_v4().to_string().replace('-', ""));
        let now = chrono::Utc::now().timestamp();

        let api_key = ApiKey {
            id: id.clone(),
            key,
            name: name.to_string(),
            is_active: true,
            created_at: now,
            last_used_at: None,
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

        let (email_str, expires_str, _sig) = (parts[0], parts[1], parts[2]);

        // 验证签名
        let expected_sig = self.compute_signature(email_str, expires_str);
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
            email: email_str.to_string(),
            created_at: now_ts,
            expires_at,
        })
    }

    /// 生成签名 token：email.expires_at.hmac_hex
    fn sign_token(&self, email: &str, expires_at: i64) -> String {
        let sig = self.compute_signature(email, &expires_at.to_string());
        format!("{}.{}.{}", email, expires_at, sig)
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