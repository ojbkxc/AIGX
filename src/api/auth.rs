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
    pub session_id: String,
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
    /// 已预留但未结算的额度（P1：配额预留/结算两段式）
    #[serde(default)]
    pub reserved_quota: i64,
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

    /// 是否允许使用指定模型。
    ///
    /// `None` 与 `Some(vec![])` 均表示不限（对齐 new-api：空列表 = 全部放行）。
    pub fn allows_model(&self, model: &str) -> bool {
        match &self.allowed_models {
            None => true,
            Some(list) => list.is_empty() || list.iter().any(|m| m == model),
        }
    }

    /// 是否允许来自指定 IP
    pub fn allows_ip(&self, ip: &str) -> bool {
        match &self.ip_limit {
            None => true,
            Some(list) => list.iter().any(|allowed| allowed == ip),
        }
    }

    /// 是否超出额度上限（含已预留）
    pub fn is_quota_exhausted(&self) -> bool {
        match self.quota_limit {
            Some(limit) => self.used_quota + self.reserved_quota >= limit,
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
    #[error("User quota exhausted, please top up")]
    UserQuotaExhausted,
    #[error("IP '{0}' is not allowed for this API key")]
    IpNotAllowed(String),
}

/// API 密钥存储
pub struct ApiKeyStore {
    store: Arc<FileStore>,
    keys: Arc<RwLock<HashMap<String, ApiKey>>>,
    key_hash_map: Arc<RwLock<HashMap<String, String>>>, // hash -> id
    /// 用户存储（余额预检用；None 时跳过用户级检查）
    user_store: Option<std::sync::Arc<crate::user::UserStore>>,
}

impl ApiKeyStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            keys: Arc::new(RwLock::new(HashMap::new())),
            key_hash_map: Arc::new(RwLock::new(HashMap::new())),
            user_store: None,
        }
    }

    /// 注入用户存储（启动时由 main 调用，供余额预检）。
    pub fn with_user_store(&mut self, user_store: std::sync::Arc<crate::user::UserStore>) {
        self.user_store = Some(user_store);
    }

    /// 从存储加载密钥
    pub fn load(&self) -> Result<(), anyhow::Error> {
        let keys = self.store.list("apikey_")?;
        let mut map = self.keys.write();
        let mut hash_map = self.key_hash_map.write();

        for key in &keys {
            if let Some(api_key) = self.store.get::<ApiKey>(key)? {
                let id = key.strip_prefix("apikey_").unwrap_or(key).to_string();
                // 与 validate()/generate_with_options() 保持同一口径：
                // 对去掉 "sk-" 前缀的 key 计算哈希，否则重启加载后鉴权全部失配
                let hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));
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
    pub fn generate_with_options(
        &self,
        opts: CreateApiKeyOptions,
    ) -> Result<ApiKey, anyhow::Error> {
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
            allowed_models: normalize_model_list(opts.allowed_models),
            expires_at: opts.expires_at,
            quota_limit: opts.quota_limit,
            used_quota: 0,
            ip_limit: opts.ip_limit,
            reserved_quota: 0,
            status: "active".to_string(),
            updated_at: now,
        };

        let hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));

        self.store.put(&format!("apikey_{id}"), &api_key)?;
        self.keys.write().insert(id.clone(), api_key.clone());
        self.key_hash_map.write().insert(hash, id);

        Ok(api_key)
    }

    /// 更新密钥（读取-修改-写入）
    pub fn update(
        &self,
        id: &str,
        mutator: impl FnOnce(&mut ApiKey),
    ) -> Result<ApiKey, anyhow::Error> {
        let mut api_key = self
            .keys
            .read()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("api key not found"))?;
        let old_hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));
        mutator(&mut api_key);
        api_key.allowed_models = normalize_model_list(api_key.allowed_models.take());
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
        // 绑定用户余额预检：计费在响应完成后才发生（try_charge 后置），
        // 不预检则余额耗尽的用户仍可无限消费，上游成本由站方承担。
        if let Some(uid) = &api_key.user_id {
            if let Some(store) = &self.user_store {
                if let Some(user) = store.get_by_id(uid) {
                    if user.remaining() <= 0 {
                        return Err(ApiKeyError::UserQuotaExhausted);
                    }
                }
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

    /// 预留令牌额度（P1：两段式计费第一步）。
    ///
    /// 将 `amount` 冻结到 `reserved_quota`，请求完成后由 `settle_quota` 结算。
    pub fn reserve_quota(&self, id: &str, amount: i64) -> bool {
        if amount <= 0 {
            return true;
        }
        let mut keys = self.keys.write();
        let api_key = match keys.get_mut(id) {
            Some(k) => k,
            None => return false,
        };
        if let Some(limit) = api_key.quota_limit {
            if api_key.used_quota + api_key.reserved_quota + amount > limit {
                return false;
            }
        }
        api_key.reserved_quota += amount;
        let snapshot = api_key.clone();
        drop(keys);
        if let Err(e) = self.store.put(&format!("apikey_{id}"), &snapshot) {
            tracing::error!("Failed to persist apikey {} reserve_quota: {}", id, e);
        }
        true
    }

    /// 结算令牌预留额度（P1：两段式计费第二步）。
    ///
    /// `reserved` 是预留总额，`actual` 是实际消费。释放预留，转入实际消费。
    pub fn settle_quota(&self, id: &str, reserved: i64, actual: i64) {
        if reserved <= 0 {
            return;
        }
        let actual = actual.max(0);
        let mut keys = self.keys.write();
        let api_key = match keys.get_mut(id) {
            Some(k) => k,
            None => return,
        };
        let release = reserved.min(api_key.reserved_quota);
        api_key.reserved_quota -= release;
        api_key.used_quota += actual;
        api_key.last_used_at = Some(chrono::Utc::now().timestamp());
        let snapshot = api_key.clone();
        drop(keys);
        if let Err(e) = self.store.put(&format!("apikey_{id}"), &snapshot) {
            tracing::error!("Failed to persist apikey {} settle_quota: {}", id, e);
        }
    }

    /// 释放令牌预留额度（P1：两段式计费的取消/回滚路径）。
    ///
    /// 与 `settle_quota` 的区别见 `UserStore::release_quota`：这里只把
    /// `reserved_quota` 解冻归还，不产生 used_quota 消费。请求失败、
    /// 预留回滚、预留超时回收统一走此方法（2026-09-08 二轮审查 G1/G5）。
    pub fn release_quota(&self, id: &str, amount: i64) {
        if amount <= 0 {
            return;
        }
        let mut keys = self.keys.write();
        let api_key = match keys.get_mut(id) {
            Some(k) => k,
            None => return,
        };
        // 防御性上限：最多归还当前预留量
        let release = amount.min(api_key.reserved_quota);
        api_key.reserved_quota -= release;
        let snapshot = api_key.clone();
        drop(keys);
        if let Err(e) = self.store.put(&format!("apikey_{id}"), &snapshot) {
            tracing::error!("Failed to persist apikey {} release_quota: {}", id, e);
        }
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
            let hash = hash_api_key(api_key.key.strip_prefix("sk-").unwrap_or(&api_key.key));
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

/// 空白名单归一化为 `None`：`Some(vec![])` 与 `None` 语义相同（均不限），
/// 统一存储形态，避免历史数据与判断逻辑出现双语义。
pub(crate) fn normalize_model_list(list: Option<Vec<String>>) -> Option<Vec<String>> {
    match list {
        Some(v) if v.is_empty() => None,
        other => other,
    }
}

// ── 会话注册表（P1：会话撤销）──────────────────────────────────────────
//
// SessionStore 的 token 是无状态 HMAC 签名——签名与有效期通过即合法，
// 无法服务端单点作废。会话注册表补上这一层：
//
// - 登录时 create_session 注册（jti → email，并维护 email → 活跃 jti 集合）
// - 验证时 validate_session 之后查 revocation：jti 已撤销 → 拒绝
// - logout / 改密 / 管理端踢人时 revoke，旧 token 立即失效
//
// 撤销记录 TTL 与会话 TTL 对齐：会话本身过期后撤销记录也无意义，
// 过期自动清扫避免注册表无限膨胀。重启后注册表为空——已签发且
// 未撤销的 token 仍有效（无状态签名的固有语义，重启不做全量失效），
// 撤销中的会话在重启后恢复有效是可接受的权衡（需撤销+重启同时发生）。

/// 会话注册表 — 撤销检查 + 用户维度活跃会话计数。
///
/// 线程安全（内部 dashmap）；TTL 由后台清扫或懒惰清理负责。
pub struct SessionRegistry {
    /// 已撤销的 jti → 撤销时间戳（TTL 后清扫，与会话有效期对齐）
    revoked: dashmap::DashMap<String, i64>,
    /// email → 活跃 jti 集合（登录注册，撤销/过期移除）
    active: dashmap::DashMap<String, dashmap::DashSet<String>>,
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            revoked: dashmap::DashMap::new(),
            active: dashmap::DashMap::new(),
        }
    }

    /// 注册新会话（登录成功时调用）。
    pub fn register(&self, email: &str, session_id: &str) {
        self.active
            .entry(email.to_string())
            .or_default()
            .insert(session_id.to_string());
    }

    /// 是否已撤销。
    pub fn is_revoked(&self, session_id: &str) -> bool {
        // 旧版三段式 token 无 session_id（空串）：无撤销语义，不视为撤销
        !session_id.is_empty() && self.revoked.contains_key(session_id)
    }

    /// 撤销单个会话（logout / 单点踢出）。返回是否确实撤销。
    pub fn revoke(&self, email: &str, session_id: &str) -> bool {
        if session_id.is_empty() {
            return false;
        }
        let removed_active = self
            .active
            .get(email)
            .map(|set| set.remove(session_id).is_some())
            .unwrap_or(false);
        let newly_revoked = self
            .revoked
            .insert(session_id.to_string(), chrono::Utc::now().timestamp())
            .is_none();
        removed_active || newly_revoked
    }

    /// 撤销某用户全部活跃会话（改密 / 管理端全踢）。返回撤销数。
    pub fn revoke_all(&self, email: &str) -> usize {
        let Some((_, sessions)) = self.active.remove(email) else {
            return 0;
        };
        let now = chrono::Utc::now().timestamp();
        let count = sessions.len();
        for jti in sessions.iter() {
            self.revoked.insert(jti.key().clone(), now);
        }
        count
    }

    /// 某用户当前活跃会话数。
    pub fn active_count(&self, email: &str) -> usize {
        self.active.get(email).map(|s| s.len()).unwrap_or(0)
    }

    /// 撤销时间早于 `before`（unix 秒）的撤销记录清扫。
    ///
    /// 用于让注册表与会话 TTL 对齐：会话自然过期后撤销记录
    /// 保留已无意义。由定时任务或懒惰触发调用。
    pub fn sweep_expired_revocations(&self, before: i64) -> usize {
        let old_len = self.revoked.len();
        self.revoked.retain(|_, ts| *ts >= before);
        old_len - self.revoked.len()
    }

    /// 注册表规模（测试/监控用）。
    pub fn revoked_len(&self) -> usize {
        self.revoked.len()
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
        let session_id = uuid::Uuid::new_v4().to_string();
        let token = self.sign_token(email, &session_id, expires_at);

        Session {
            token,
            session_id,
            email: email.to_string(),
            created_at: now,
            expires_at,
        }
    }

    /// 验证签名的 token 并返回会话信息
    ///
    /// 兼容两种 token 形态：
    /// - 新版四段式：base64(email).session_id.expires_at.hmac_hex
    /// - 旧版三段式：base64(email).expires_at.hmac_hex（升级前签发，
    ///   无 session_id；仍验证签名与有效期，session_id 置空串）
    pub fn validate_session(&self, token: &str) -> Option<Session> {
        let parts: Vec<&str> = token.splitn(4, '.').collect();
        let (email_b64, session_id, expires_str, _sig) = match parts.len() {
            4 => (parts[0], parts[1].to_string(), parts[2], parts[3]),
            3 => (parts[0], String::new(), parts[1], parts[2]),
            _ => return None,
        };

        // 解码 email（base64 URL_SAFE_NO_PAD，避免 email 中的 '.' 干扰 splitn）
        let email_bytes = URL_SAFE_NO_PAD.decode(email_b64).ok()?;
        let email_str = String::from_utf8(email_bytes).ok()?;

        // 验证签名（按 token 形态选择对应签名输入）
        let expected_sig = if parts.len() == 4 {
            self.compute_signature_triple(&email_str, &session_id, expires_str)
        } else {
            self.compute_signature(&email_str, expires_str)
        };
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
            session_id,
            email: email_str,
            created_at: now_ts,
            expires_at,
        })
    }

    /// 生成签名 token：base64(email).session_id.expires_at.hmac_hex
    ///
    /// email 经 base64 URL_SAFE_NO_PAD 编码，避免 email 中的 '.'（如 user@example.com）
    /// 干扰 token 的 `splitn(4, '.')` 解析；session_id（jti）参与签名，
    /// 使每个会话的 token 互不相同，撤销时可按 session_id 精确吊销。
    fn sign_token(&self, email: &str, session_id: &str, expires_at: i64) -> String {
        let sig = self.compute_signature_triple(email, session_id, &expires_at.to_string());
        let email_b64 = URL_SAFE_NO_PAD.encode(email.as_bytes());
        format!("{}.{}.{}.{}", email_b64, session_id, expires_at, sig)
    }

    fn compute_signature(&self, email: &str, expires: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes()).expect("HMAC key");
        mac.update(email.as_bytes());
        mac.update(b".");
        mac.update(expires.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// 三段输入（email.session_id.expires）的签名 —— 新版四段式 token 用。
    fn compute_signature_triple(&self, email: &str, session_id: &str, expires: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(self.secret.as_bytes()).expect("HMAC key");
        mac.update(email.as_bytes());
        mac.update(b".");
        mac.update(session_id.as_bytes());
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
#[cfg(test)]
mod session_registry_tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn register_and_revoke_single() {
        let reg = SessionRegistry::new();
        reg.register("a@x.com", "jti-1");
        reg.register("a@x.com", "jti-2");
        assert_eq!(reg.active_count("a@x.com"), 2);
        assert!(!reg.is_revoked("jti-1"));

        assert!(reg.revoke("a@x.com", "jti-1"));
        assert!(reg.is_revoked("jti-1"));
        assert!(!reg.is_revoked("jti-2"));
        assert_eq!(reg.active_count("a@x.com"), 1);

        // 重复撤销同一 jti：revoked 标记幂等，active 已移除 → false
        assert!(!reg.revoke("a@x.com", "jti-1"));
    }

    // ── 预留/结算/释放语义测试（P1 两段式，2026-09-08 G1/G5 修复）──

    fn key_store() -> ApiKeyStore {
        ApiKeyStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    /// 预留 → 结算：reserved 解冻，used 增加实际消费。
    #[test]
    fn apikey_reserve_then_settle() {
        let s = key_store();
        let k = s.generate("t1").unwrap();
        assert!(s.reserve_quota(&k.id, 100));
        assert_eq!(s.validate(&k.key).unwrap().reserved_quota, 100);
        s.settle_quota(&k.id, 100, 60);
        let after = s.validate(&k.key).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 60);
    }

    /// 预留 → 释放：reserved 解冻，used 不变（无消费）。
    #[test]
    fn apikey_reserve_then_release() {
        let s = key_store();
        let k = s.generate("t2").unwrap();
        assert!(s.reserve_quota(&k.id, 100));
        s.release_quota(&k.id, 100);
        let after = s.validate(&k.key).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 0);
    }

    /// 释放量超过当前预留时只归还当前量（防御性上限，不产生负值）。
    #[test]
    fn apikey_release_clamped() {
        let s = key_store();
        let k = s.generate("t3").unwrap();
        assert!(s.reserve_quota(&k.id, 50));
        s.release_quota(&k.id, 500);
        let after = s.validate(&k.key).unwrap();
        assert_eq!(after.reserved_quota, 0);
        assert_eq!(after.used_quota, 0);
    }
    #[test]
    fn revoke_all_sessions() {
        let reg = SessionRegistry::new();
        reg.register("b@x.com", "jti-a");
        reg.register("b@x.com", "jti-b");
        reg.register("c@x.com", "jti-c");

        assert_eq!(reg.revoke_all("b@x.com"), 2);
        assert!(reg.is_revoked("jti-a"));
        assert!(reg.is_revoked("jti-b"));
        assert!(!reg.is_revoked("jti-c"));
        assert_eq!(reg.active_count("b@x.com"), 0);

        // 再撤销空用户：0
        assert_eq!(reg.revoke_all("b@x.com"), 0);
    }

    #[test]
    fn legacy_empty_session_id_not_revocable() {
        let reg = SessionRegistry::new();
        // 旧版三段式 token 的 session_id 为空串：不参与撤销语义
        assert!(!reg.is_revoked(""));
        assert!(!reg.revoke("a@x.com", ""));
    }

    #[test]
    fn sweep_expired_revocations_removes_old_only() {
        let reg = SessionRegistry::new();
        reg.revoke("a@x.com", "jti-old");
        reg.revoke("a@x.com", "jti-new");
        // 手动把 jti-old 的撤销时间拨到过去
        *reg.revoked.get_mut("jti-old").unwrap() = 1000;
        assert_eq!(reg.sweep_expired_revocations(2000), 1);
        assert!(!reg.is_revoked("jti-old"));
        assert!(reg.is_revoked("jti-new"));
        assert_eq!(reg.revoked_len(), 1);
    }

    /// 生成时 Some([]) 归一化为 None（语义：不限）。
    #[test]
    fn generate_normalizes_empty_allowed_models() {
        let s = key_store();
        let opts = CreateApiKeyOptions {
            name: "n1".to_string(),
            user_id: None,
            group: "default".to_string(),
            allowed_models: Some(vec![]),
            expires_at: None,
            quota_limit: None,
            ip_limit: None,
        };
        let k = s.generate_with_options(opts).unwrap();
        assert!(k.allowed_models.is_none());
        assert!(s.validate(&k.key).unwrap().allows_model("any-model"));
    }

    /// update 后空列表归一化为 None（防写入双语义）。
    #[test]
    fn update_normalizes_empty_allowed_models() {
        let s = key_store();
        let k = s.generate("n2").unwrap();
        s.update(&k.id, |key| {
            key.allowed_models = Some(vec![]);
        })
        .unwrap();
        let after = s.validate(&k.key).unwrap();
        assert!(after.allowed_models.is_none());
        assert!(after.allows_model("any-model"));
    }
}
