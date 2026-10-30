//! 账号会话池 —— 渠道内的第二级调度（优先级 / 权重 / 冷却 / 并发）。
//!
//! 与 `ChannelStore` 同级协作：渠道级（priority/weight/熔断/亲和）不变，
//! 本模块只在**选定渠道之后**决定用哪一把账号凭证。
//!
//! 关键约束：
//! - 冷却键是**上游真名**（`AccountBridge` 传入的 `req.model` 已是映射后的名字）；
//! - 并发用租约（`AccountLease`）归还，流式请求随流生命周期释放；
//! - 凭证比较-再写（`update_credential_cas`）防止并发的旧快照覆盖新 token。

use dashmap::DashMap;
use parking_lot::RwLock;
use rand::Rng;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::model::{AccountSession, Credential};
use super::rotate::{self, RotationPolicy};
use crate::bridge::BridgeError;
use crate::channel::circuit_breaker::FailureType;
use crate::egress::EgressCache;
use crate::storage::FileStore;

/// KV 键前缀（`acctses:{channel_id}:{account_id}`）。
const KEY_PREFIX: &str = "acctses:";
/// 轮换策略的落盘键（管理台「账号路由」保存后重启仍生效）。
const POLICY_KEY: &str = "account_rotation_policy";
/// 运行态统计落盘节流窗口（秒）——热路径不每次请求都写存储。
const PERSIST_INTERVAL_SECS: i64 = 30;

/// 并发槽位租约：`drop` 时归还账号的并发计数。
pub struct AccountLease {
    inner: Arc<Inner>,
    session: AccountSession,
}

impl AccountLease {
    /// 账号 id。
    pub fn id(&self) -> &str {
        &self.session.id
    }

    /// 账号快照（含明文可解的凭证与代理配置）。
    pub fn session(&self) -> &AccountSession {
        &self.session
    }
}

impl Drop for AccountLease {
    fn drop(&mut self) {
        let mut sessions = self.inner.sessions.write();
        if let Some(s) = sessions.iter_mut().find(|s| s.id == self.session.id) {
            s.in_flight = s.in_flight.saturating_sub(1);
        }
    }
}

/// 凭证比较-再写的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CasOutcome {
    /// 旧快照匹配，已写入新凭证。
    Updated,
    /// 旧快照不匹配（凭证已被并发刷新），本次写入被拒绝。
    Stale,
    /// 账号不存在。
    Missing,
}

struct Inner {
    store: Arc<FileStore>,
    sessions: RwLock<Vec<AccountSession>>,
    egress: Arc<EgressCache>,
    policy: RwLock<RotationPolicy>,
    /// 换号重试间隔（毫秒，0 = 立即换号）；由「请求重试」设置实时驱动。
    retry_interval_ms: AtomicU64,
    /// 命中即直接换号（不给指数退避）的上游状态码；由「请求重试」设置实时驱动。
    retry_on_status: RwLock<Vec<u16>>,
    /// 落盘节流：account id → 上次持久化 unix 秒。
    persisted_at: DashMap<String, i64>,
}

/// 账号会话池（内部 `Arc`，`clone` 廉价——可安全塞进 `AppState`/Bridge）。
#[derive(Clone)]
pub struct AccountSessionStore {
    inner: Arc<Inner>,
}

impl AccountSessionStore {
    /// 打开账号池（内部自建出口 Client 池）。
    pub fn new(store: Arc<FileStore>) -> Self {
        Self::with_egress(store, Arc::new(EgressCache::new()))
    }

    /// 打开账号池并复用外部出口 Client 池（测试/多实例共享时使用）。
    pub fn with_egress(store: Arc<FileStore>, egress: Arc<EgressCache>) -> Self {
        let pool = Self {
            inner: Arc::new(Inner {
                store,
                sessions: RwLock::new(Vec::new()),
                egress,
                policy: RwLock::new(RotationPolicy::default()),
                retry_interval_ms: AtomicU64::new(0),
                retry_on_status: RwLock::new(vec![402]),
                persisted_at: DashMap::new(),
            }),
        };
        if let Err(e) = pool.load() {
            tracing::error!("Failed to load account sessions: {e}");
        }
        pool.load_policy();
        pool
    }

    /// 加载落盘的轮换策略（缺失/损坏时保持默认值）。
    fn load_policy(&self) {
        match self.inner.store.get::<RotationPolicy>(POLICY_KEY) {
            Ok(Some(policy)) => {
                *self.inner.policy.write() = policy.clamped();
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("load account rotation policy: {e}"),
        }
    }

    /// 从存储加载全部账号（按创建时间升序）。
    pub fn load(&self) -> anyhow::Result<()> {
        let keys = self.inner.store.list(KEY_PREFIX)?;
        let mut sessions = Vec::with_capacity(keys.len());
        for key in &keys {
            if let Some(session) = self.inner.store.get::<AccountSession>(key)? {
                sessions.push(session);
            }
        }
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        *self.inner.sessions.write() = sessions;
        Ok(())
    }

    /// KV 键。
    fn key_of(channel_id: &str, id: &str) -> String {
        format!("{KEY_PREFIX}{channel_id}:{id}")
    }

    /// 立即落盘。
    fn persist(&self, session: &AccountSession) -> anyhow::Result<()> {
        let key = Self::key_of(&session.channel_id, &session.id);
        self.inner.store.put(&key, session)?;
        self.inner
            .persisted_at
            .insert(session.id.clone(), chrono::Utc::now().timestamp());
        Ok(())
    }

    /// 节流落盘（运行态统计高频变化，避免每请求写存储）。
    fn persist_throttled(&self, session: &AccountSession) {
        let now = chrono::Utc::now().timestamp();
        let due = self
            .inner
            .persisted_at
            .get(&session.id)
            .map(|at| now - *at >= PERSIST_INTERVAL_SECS)
            .unwrap_or(true);
        if !due {
            return;
        }
        if let Err(e) = self.persist(session) {
            tracing::warn!("persist account session {}: {e}", session.id);
        }
    }

    /// 全部账号快照（管理面列表）。
    pub fn all(&self) -> Vec<AccountSession> {
        self.inner.sessions.read().clone()
    }

    /// 指定渠道的账号快照。
    pub fn for_channel(&self, channel_id: &str) -> Vec<AccountSession> {
        self.inner
            .sessions
            .read()
            .iter()
            .filter(|s| s.channel_id == channel_id)
            .cloned()
            .collect()
    }

    /// 按 id 取账号（id 全局唯一）。
    pub fn get(&self, id: &str) -> Option<AccountSession> {
        self.inner
            .sessions
            .read()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// 账号数量。
    pub fn len(&self) -> usize {
        self.inner.sessions.read().len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 新增或更新账号（写入即落盘）。
    ///
    /// 更新时保留运行态并发计数（`in_flight` 不落盘也不被管理面覆盖）。
    pub fn upsert(&self, session: AccountSession) -> anyhow::Result<AccountSession> {
        let now = chrono::Utc::now().timestamp();
        let mut next = session;
        if next.id.is_empty() {
            next.id = uuid::Uuid::new_v4().to_string();
        }
        if next.created_at == 0 {
            next.created_at = now;
        }
        next.updated_at = now;
        let credential = next.credential.encoded();
        next.credential = credential;
        if next.status.is_empty() {
            next.status = "enabled".to_string();
        }
        if next.weight == 0 {
            next.weight = 1;
        }
        if next.provider.trim().is_empty() {
            next.provider = super::registry::DEFAULT_PROVIDER_KIND.to_string();
        } else {
            next.provider = super::registry::normalize_kind(&next.provider);
        }
        self.persist(&next)?;
        {
            let mut sessions = self.inner.sessions.write();
            match sessions.iter_mut().find(|s| s.id == next.id) {
                Some(slot) => {
                    let in_flight = slot.in_flight;
                    *slot = next.clone();
                    slot.in_flight = in_flight;
                }
                None => sessions.push(next.clone()),
            }
            sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
        }
        Ok(next)
    }

    /// 删除账号（返回被删记录）。
    pub fn remove(&self, id: &str) -> anyhow::Result<Option<AccountSession>> {
        let removed = {
            let mut sessions = self.inner.sessions.write();
            match sessions.iter().position(|s| s.id == id) {
                Some(pos) => sessions.remove(pos),
                None => return Ok(None),
            }
        };
        self.inner
            .store
            .delete(&Self::key_of(&removed.channel_id, &removed.id))?;
        self.inner.persisted_at.remove(&removed.id);
        Ok(Some(removed))
    }

    /// 删除某渠道下全部账号（渠道删除时级联清理）。
    pub fn remove_for_channel(&self, channel_id: &str) -> anyhow::Result<usize> {
        let removed: Vec<AccountSession> = {
            let mut sessions = self.inner.sessions.write();
            let (matched, kept): (Vec<AccountSession>, Vec<AccountSession>) =
                sessions.drain(..).partition(|s| s.channel_id == channel_id);
            *sessions = kept;
            matched
        };
        for session in &removed {
            self.inner
                .store
                .delete(&Self::key_of(&session.channel_id, &session.id))?;
            self.inner.persisted_at.remove(&session.id);
        }
        Ok(removed.len())
    }

    /// 轮换参数。
    pub fn policy(&self) -> RotationPolicy {
        *self.inner.policy.read()
    }

    /// 更新轮换参数（归一化后立即生效并落盘）。
    pub fn set_policy(&self, policy: RotationPolicy) -> anyhow::Result<RotationPolicy> {
        let next = policy.clamped();
        *self.inner.policy.write() = next;
        self.inner.store.put(POLICY_KEY, &next)?;
        Ok(next)
    }

    /// 换号重试间隔（毫秒，0 = 立即换号）。
    pub fn retry_interval_ms(&self) -> u64 {
        self.inner.retry_interval_ms.load(Ordering::Relaxed)
    }

    /// 设置换号重试间隔（上限 10s，设置页保存时实时生效）。
    pub fn set_retry_interval_ms(&self, ms: u64) {
        self.inner
            .retry_interval_ms
            .store(ms.min(10_000), Ordering::Relaxed);
    }

    /// 直接换号状态码名单（命中时不给指数退避冷却）。
    pub fn retry_on_status(&self) -> Vec<u16> {
        self.inner.retry_on_status.read().clone()
    }

    /// 更新直接换号状态码名单（去重、只保留 100–599，最多 50 个）。
    pub fn set_retry_on_status(&self, codes: Vec<u16>) {
        let mut cleaned: Vec<u16> = Vec::new();
        for code in codes {
            if (100..=599).contains(&code) && !cleaned.contains(&code) {
                cleaned.push(code);
            }
            if cleaned.len() >= 50 {
                break;
            }
        }
        *self.inner.retry_on_status.write() = cleaned;
    }

    /// 出口 Client 池。
    pub fn egress(&self) -> &Arc<EgressCache> {
        &self.inner.egress
    }

    /// 取该账号的出口 Client（按账号代理缓存，非法代理返回配置错误）。
    pub fn client_for(
        &self,
        session: &AccountSession,
    ) -> Result<Arc<reqwest::Client>, BridgeError> {
        self.inner
            .egress
            .client(session.proxy.as_deref())
            .map_err(BridgeError::Config)
    }

    /// 租用一把可用账号（优先级降序 → 组内权重随机 → 空闲最久优先）。
    ///
    /// `alias` 是同一模型的「用户可见名」，用于兼容账号白名单按渠道 models
    /// 配置的场景；冷却键始终是 `model`（上游真名）。
    ///
    /// 返回 `None` 表示该渠道下当前没有可用账号（全部冷却/并发占满/禁用）。
    pub fn acquire(
        &self,
        channel_id: &str,
        model: &str,
        alias: Option<&str>,
    ) -> Option<AccountLease> {
        let now = chrono::Utc::now().timestamp();
        let mut sessions = self.inner.sessions.write();
        let mut candidates: Vec<usize> = Vec::new();
        for (index, session) in sessions.iter().enumerate() {
            if session.channel_id != channel_id || !session.is_enabled() {
                continue;
            }
            if session.is_expired(now) || !session.supports_model_any(model, alias) {
                continue;
            }
            if session.is_cooling(model, now) {
                continue;
            }
            if session.max_concurrency > 0 && session.in_flight >= session.max_concurrency {
                continue;
            }
            candidates.push(index);
        }
        if candidates.is_empty() {
            return None;
        }
        // 最高优先级组内：权重随机（同组内再让空闲最久的账号权重更高）
        let max_priority = candidates
            .iter()
            .map(|index| sessions[*index].priority)
            .max()
            .unwrap_or(0);
        let group: Vec<usize> = candidates
            .into_iter()
            .filter(|index| sessions[*index].priority == max_priority)
            .collect();
        let pick = pick_weighted(&sessions, &group, now);
        let next_in_flight = sessions[pick].in_flight.saturating_add(1);
        sessions[pick].in_flight = next_in_flight;
        sessions[pick].last_used_at = Some(now);
        let session = sessions[pick].clone();
        drop(sessions);
        self.persist_throttled(&session);
        Some(AccountLease {
            inner: self.inner.clone(),
            session,
        })
    }

    /// 记入一次成功（清该模型冷却、累计延迟）。
    pub fn record_success(&self, id: &str, model: &str, latency_ms: u64) {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return;
            };
            session.success_count = session.success_count.saturating_add(1);
            session.total_latency_ms = session.total_latency_ms.saturating_add(latency_ms);
            session.consecutive_errors = 0;
            session.cooldowns.remove(model);
            session.last_error = None;
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        self.persist_throttled(&snapshot);
    }

    /// 记入一次失败（按失败类型冷却该账号的该上游模型），返回冷却秒数。
    ///
    /// 失败分类复用渠道断路器的 `classify_bridge_error` 结果。
    pub fn record_failure(
        &self,
        id: &str,
        model: &str,
        failure: &FailureType,
        message: &str,
        status: Option<u16>,
    ) -> u64 {
        let now = chrono::Utc::now().timestamp();
        let retry_after = match failure {
            FailureType::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        };
        let policy = self.policy();
        let direct_switch = status
            .is_some_and(|code| self.inner.retry_on_status.read().contains(&code));
        let (snapshot, secs) = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return 0;
            };
            session.error_count = session.error_count.saturating_add(1);
            session.consecutive_errors = session.consecutive_errors.saturating_add(1);
            let secs = if direct_switch {
                // 命中「直接换号」名单：只给基础冷却，不走指数退避
                policy.base_cooldown_secs.max(1)
            } else {
                rotate::cooldown_secs(&policy, failure, session.consecutive_errors, retry_after)
            };
            if secs > 0 {
                session
                    .cooldowns
                    .insert(model.to_string(), now + secs as i64);
            }
            session.last_error = Some(message.to_string());
            session.updated_at = now;
            (session.clone(), secs)
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist account session {} after failure: {e}", snapshot.id);
        }
        secs
    }

    /// 清除该账号的全部冷却与最近错误（手动恢复）。返回是否命中账号。
    pub fn reset_cooldowns(&self, id: &str) -> bool {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return false;
            };
            session.cooldowns.clear();
            session.consecutive_errors = 0;
            session.last_error = None;
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist account session {} after reset: {e}", snapshot.id);
        }
        true
    }

    /// 设置配置态状态（`enabled` / `disabled`）。返回是否命中账号。
    pub fn set_status(&self, id: &str, status: &str) -> bool {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return false;
            };
            session.status = status.to_string();
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist account session {} status: {e}", snapshot.id);
        }
        true
    }

    /// 凭证比较-再写：旧快照一致才写入新凭证（防并发旧快照覆盖新 token）。
    pub fn update_credential_cas(
        &self,
        id: &str,
        expected_plain: &str,
        next: Credential,
    ) -> CasOutcome {
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return CasOutcome::Missing;
            };
            if session.credential.secret_plain() != expected_plain {
                return CasOutcome::Stale;
            }
            session.credential = next.encoded();
            session.error_count = 0;
            session.last_error = None;
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist account session {} credential: {e}", snapshot.id);
        }
        CasOutcome::Updated
    }

    /// 周期维护：清理过期冷却、同步凭证过期状态。返回本轮变更条数。
    pub fn sweep(&self, now: i64) -> u64 {
        let mut touched: Vec<AccountSession> = Vec::new();
        {
            let mut sessions = self.inner.sessions.write();
            for session in sessions.iter_mut() {
                let mut dirty = false;
                let before = session.cooldowns.len();
                session.cooldowns.retain(|_, until| *until > now);
                if before != session.cooldowns.len() {
                    dirty = true;
                }
                if session.is_expired(now) && session.status == "enabled" {
                    session.status = "expired".to_string();
                    session.last_error = Some("credential expired".to_string());
                    dirty = true;
                } else if session.status == "expired" && !session.is_expired(now) {
                    session.status = "enabled".to_string();
                    session.last_error = None;
                    dirty = true;
                }
                if dirty {
                    session.updated_at = now;
                    touched.push(session.clone());
                }
            }
        }
        for session in &touched {
            if let Err(e) = self.persist(session) {
                tracing::warn!("persist account session {} during sweep: {e}", session.id);
            }
        }
        touched.len() as u64
    }
}

/// 组内加权随机：空闲最久的账号获得额外权重（避免热点账号被持续打满）。
fn pick_weighted(sessions: &[AccountSession], group: &[usize], now: i64) -> usize {
    if group.len() == 1 {
        return group[0];
    }
    let weights: Vec<u64> = group
        .iter()
        .map(|index| {
            let session = &sessions[*index];
            // 空闲越久权重越高：base(weight) + 空闲分钟数（封顶 60）
            let idle_bonus = session
                .last_used_at
                .map(|at| (now - at).clamp(0, 3600) / 60)
                .unwrap_or(60);
            let base_weight = u64::from(session.weight.max(1));
            base_weight.saturating_add(idle_bonus.max(0) as u64)
        })
        .collect();
    let total: u64 = weights.iter().sum();
    let mut cursor = rand::thread_rng().gen_range(0..total.max(1));
    for (offset, index) in group.iter().enumerate() {
        let weight = weights[offset].max(1);
        if cursor < weight {
            return *index;
        }
        cursor -= weight;
    }
    group[0]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pool() -> AccountSessionStore {
        AccountSessionStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    fn session(channel_id: &str, name: &str, priority: i64) -> AccountSession {
        let mut s = AccountSession::new(channel_id, name);
        s.priority = priority;
        s.credential.secret = format!("sk-{name}");
        s
    }

    #[test]
    fn upsert_persists_and_encodes_secret() {
        let pool = pool();
        let created = pool.upsert(session("ch", "acct", 0)).unwrap();
        assert!(created.credential.secret.starts_with("enc:"));
        let loaded = pool.get(&created.id).unwrap();
        assert_eq!(loaded.credential.secret_plain(), "sk-acct");
        assert_eq!(
            loaded.provider,
            super::super::registry::DEFAULT_PROVIDER_KIND
        );
    }

    #[test]
    fn acquire_prefers_higher_priority() {
        let pool = pool();
        pool.upsert(session("ch", "low", 1)).unwrap();
        pool.upsert(session("ch", "high", 9)).unwrap();
        let lease = pool.acquire("ch", "m", None).expect("应有可用账号");
        assert_eq!(lease.session().name, "high");
    }

    #[test]
    fn acquire_stops_at_concurrency_limit_and_releases_on_drop() {
        let pool = pool();
        let mut only = session("ch", "only", 0);
        only.max_concurrency = 1;
        pool.upsert(only).unwrap();
        let lease = pool.acquire("ch", "m", None).expect("首个租约");
        assert!(pool.acquire("ch", "m", None).is_none());
        drop(lease);
        assert!(pool.acquire("ch", "m", None).is_some());
    }

    #[test]
    fn failure_cools_down_only_the_upstream_model() {
        let pool = pool();
        let account = pool.upsert(session("ch", "acct", 0)).unwrap();
        let failure = FailureType::ServerError;
        let secs = pool.record_failure(&account.id, "upstream-x", &failure, "boom", None);
        assert!(secs >= 60);
        assert!(pool.acquire("ch", "upstream-x", None).is_none());
        assert!(pool.acquire("ch", "other-model", None).is_some());
        assert!(pool.reset_cooldowns(&account.id));
        assert!(pool.acquire("ch", "upstream-x", None).is_some());
    }

    #[test]
    fn retry_on_status_uses_base_cooldown_only() {
        let pool = pool();
        let account = pool.upsert(session("ch", "acct", 0)).unwrap();
        // 把基础冷却调小，便于断言「命中名单后不再指数退避」
        let mut policy = pool.policy();
        policy.base_cooldown_secs = 30;
        policy.max_cooldown_secs = 1_800;
        pool.set_policy(policy).unwrap();

        let failure = FailureType::PaymentRequired;
        // 未命中名单：PaymentRequired 走上限冷却
        let uncapped = pool.record_failure(&account.id, "m1", &failure, "x", None);
        assert_eq!(uncapped, 1_800);
        // 命中名单（默认 402）：只给基础冷却
        pool.set_retry_on_status(vec![402]);
        let capped = pool.record_failure(&account.id, "m2", &failure, "x", Some(402));
        assert_eq!(capped, 30);
    }

    #[test]
    fn credential_cas_rejects_stale_snapshot() {
        let pool = pool();
        let account = pool.upsert(session("ch", "acct", 0)).unwrap();
        let next = Credential {
            kind: "api_key".to_string(),
            secret: "sk-new".to_string(),
        };
        assert_eq!(
            pool.update_credential_cas(&account.id, "sk-wrong", next.clone()),
            CasOutcome::Stale
        );
        assert_eq!(
            pool.update_credential_cas(&account.id, "sk-acct", next),
            CasOutcome::Updated
        );
        assert_eq!(
            pool.get(&account.id).unwrap().credential.secret_plain(),
            "sk-new"
        );
    }

    #[test]
    fn sweep_prunes_cooldowns_and_flips_expired_status() {
        let pool = pool();
        let mut account = session("ch", "acct", 0);
        account.cooldowns.insert("m".to_string(), 10);
        account.expires_at = Some(10);
        pool.upsert(account).unwrap();
        assert_eq!(pool.sweep(100), 1);
        let loaded = pool.all();
        assert_eq!(loaded[0].status, "expired");
        assert!(loaded[0].cooldowns.is_empty());
    }

    #[test]
    fn remove_for_channel_cascades() {
        let pool = pool();
        pool.upsert(session("ch-a", "a", 0)).unwrap();
        pool.upsert(session("ch-b", "b", 0)).unwrap();
        assert_eq!(pool.remove_for_channel("ch-a").unwrap(), 1);
        assert_eq!(pool.len(), 1);
    }
}
