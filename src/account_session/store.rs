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
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::model::{AccountSession, Credential};
use super::rotate::{self, RotationPolicy};
use crate::bridge::BridgeError;
use crate::channel::circuit_breaker::FailureType;
use crate::egress::EgressCache;
use crate::model::capability::CapabilityMask;
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

/// 选号阶段各过滤器跳过的账号数（无候选时的原因化，502 文案用）。
///
/// 生产实证（08-prod P1-1）：502「exhausted all accounts」时账号
/// `error_count=0` —— 按设计，选号层过滤掉的账号不尝试、不计错。
/// 这份计数把「为什么一个都没选上」带回错误文案。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkipReasons {
    /// 渠道下没有任何账号（连被过滤的都没有）
    pub no_accounts: bool,
    /// 状态禁用
    pub disabled: usize,
    /// provider 被整家关停（模型管理规则 / 插件面闸门）
    pub provider_off: usize,
    /// 凭证已过期
    pub expired: usize,
    /// 模型不在账号白名单（models 为空 = 全支持，不计）
    pub model_mismatch: usize,
    /// 模型能力位不满足请求需求（#40 §1：图/工具/视频）。**只计有证据的拒否**
    /// —— 能力标签缺失（老账号目录快照）按放行处理，不计入本档。
    pub capability: usize,
    /// 模型 / 账号级冷却中
    pub cooling: usize,
    /// 余额低于限制器阈值
    pub balance_low: usize,
    /// 限制条件组命中（Phase1-A2：skip 按模型/整号 与 disable 均记此档）
    pub limited: usize,
    /// 并发占满（max_concurrency）
    pub concurrency: usize,
}

impl SkipReasons {
    /// 拼成一句人话（只列非零项；全空 = 「没有可用账号」）。
    pub fn summary(&self) -> String {
        if self.no_accounts {
            return "渠道下没有账号".to_string();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.disabled > 0 {
            parts.push(format!("{} 个已禁用", self.disabled));
        }
        if self.provider_off > 0 {
            parts.push(format!("{} 个 provider 被关停", self.provider_off));
        }
        if self.expired > 0 {
            parts.push(format!("{} 个凭证过期", self.expired));
        }
        if self.model_mismatch > 0 {
            parts.push(format!("{} 个不在模型白名单", self.model_mismatch));
        }
        if self.capability > 0 {
            parts.push(format!("{} 个不支持请求能力", self.capability));
        }
        if self.cooling > 0 {
            parts.push(format!("{} 个冷却中", self.cooling));
        }
        if self.balance_low > 0 {
            parts.push(format!("{} 个余额低于阈值", self.balance_low));
        }
        if self.limited > 0 {
            parts.push(format!("{} 个被限制规则拦截", self.limited));
        }
        if self.concurrency > 0 {
            parts.push(format!("{} 个并发占满", self.concurrency));
        }
        if parts.is_empty() {
            "没有可用账号".to_string()
        } else {
            parts.join("、")
        }
    }
}

struct Inner {
    store: Arc<FileStore>,
    sessions: RwLock<Vec<AccountSession>>,
    /// 账号目录能力索引（#40 §1）：启动时按账号构建，目录同步时增量刷新。
    /// 选号热路径按模型名查能力位，**零 DB 读**（见 `catalog::AccountCapabilityIndex`）。
    /// 备份/外部直写 KV 恢复后需重启才生效 —— 与 `ModelMetadataRegistry` 同口径。
    capabilities: RwLock<HashMap<String, Arc<super::catalog::AccountCapabilityIndex>>>,
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

/// 条件组限制裁决门（A2）：对单个账号求值，返回 `Some(原因码)` 表示跳过该账号。
///
/// 单独立名而非内联写 `Option<&dyn Fn(..) -> ..>`：调用点与签名两处都要用，
/// 内联写法触发 `clippy::type_complexity`。
pub type LimiterGate<'a> = &'a dyn Fn(&AccountSession) -> Option<&'static str>;

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
                capabilities: RwLock::new(HashMap::new()),
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
        // #40 §1：按账号构建能力索引（每账号一次 KV 读，仅发生在启动/重载）。
        // 无目录缓存的账号不进表 —— 查询侧把「不在表里」当无证据放行。
        let mut capabilities: HashMap<String, Arc<super::catalog::AccountCapabilityIndex>> =
            HashMap::with_capacity(sessions.len());
        for session in &sessions {
            let key = super::catalog::cache_key(&session.provider, &session.id);
            if let Ok(Some(snapshot)) = self
                .inner
                .store
                .get::<super::catalog::CatalogSnapshot>(&key)
            {
                capabilities.insert(
                    session.id.clone(),
                    Arc::new(super::catalog::AccountCapabilityIndex::from_snapshot(
                        &snapshot,
                    )),
                );
            }
        }
        *self.inner.sessions.write() = sessions;
        *self.inner.capabilities.write() = capabilities;
        Ok(())
    }

    /// 刷新某账号的能力索引（#40 §1）。
    ///
    /// **唯一调用方**是 `catalog::save_cache`（目录缓存的唯一写入口），
    /// 故不存在第二处需要失效的地方。
    pub(crate) fn set_catalog_index(
        &self,
        account_id: &str,
        index: Arc<super::catalog::AccountCapabilityIndex>,
    ) {
        self.inner
            .capabilities
            .write()
            .insert(account_id.to_string(), index);
    }

    /// 渠道级能力粗筛（#40 §1 的第一级）。
    ///
    /// 返回 `false` = **有证据**表明该渠道下没有任何账号能满足 `required`
    /// （调用方据此在候选构造阶段裁掉整条渠道）；返回 `true` = 可能有
    /// （含「无证据」的放行，见 `AccountCapabilityIndex` 的三态口径）。
    ///
    /// 只读两把锁、零分配：闭包用在渠道候选循环里。
    pub fn channel_may_serve(&self, channel_id: &str, required: CapabilityMask) -> bool {
        if required.is_empty() {
            return true;
        }
        // 锁序统一 sessions → capabilities（与 acquire 路径一致）
        let sessions = self.inner.sessions.read();
        let capabilities = self.inner.capabilities.read();
        let mut evidence = false;
        for session in sessions.iter().filter(|s| s.channel_id == channel_id) {
            let Some(index) = capabilities.get(&session.id) else {
                continue;
            };
            if !index.has_evidence() {
                continue;
            }
            evidence = true;
            if index.any().covers(required) {
                return true;
            }
        }
        !evidence
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

    /// 底层 KV 存储（账号池目录缓存等旁路数据也落在同一库里）。
    pub fn store(&self) -> &Arc<FileStore> {
        &self.inner.store
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
        // 续期令牌同口径编码（已编码的 enc:/enc1: 值原样保留，明文升级 enc1:）
        if !next.refresh_secret.is_empty()
            && !next.refresh_secret.starts_with("enc:")
            && !next.refresh_secret.starts_with("enc1:")
        {
            next.refresh_secret = crate::account_session::secret::seal_or_keep(
                &next.refresh_secret,
                &crate::account_session::secret::data_dir(),
            );
        }
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
    ///
    /// 账号 `proxy` 字段的 `pool:` / `rotate` 编码在这里解析成实际代理 URL
    ///（见 `crate::proxy_pool::resolve_proxy`）：池条目被删/被禁用时返回
    /// 配置错误，**不静默直连**——否则账号流量会从错误的出口 IP 发出。
    /// 轮询游标按账号 id 前进（逐请求换出口）。
    pub fn client_for(
        &self,
        session: &AccountSession,
    ) -> Result<Arc<reqwest::Client>, BridgeError> {
        let resolved = crate::proxy_pool::resolve_proxy(session.proxy.as_deref(), &session.id)
            .map_err(BridgeError::Config)?;
        self.inner
            .egress
            .client(resolved.as_deref())
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
        self.acquire_with(channel_id, model, alias, |_| true)
    }

    /// 带 provider 过滤的选号。
    ///
    /// `allow` 收到账号的 provider，返回 false 的账号直接跳过。数据面用它
    /// 落实统一模型管理里「按 (provider, model) 关停」的规则 —— 账号型渠道
    /// 的 provider 在渠道上不存在，只能到这一层判断。
    pub fn acquire_with<F>(
        &self,
        channel_id: &str,
        model: &str,
        alias: Option<&str>,
        allow: F,
    ) -> Option<AccountLease>
    where
        F: Fn(&str) -> bool,
    {
        self.acquire_with_reasons(channel_id, model, alias, allow).0
    }

    /// [`Self::acquire_with`] 的可观测版：无候选时返回各过滤器的跳过计数
    ///（502「exhausted」的原因化，08-prod P1-1：生产出现过 502 exhausted
    /// 但账号 error_count=0 —— 选号层过滤掉的账号按设计不计错误，用户
    /// 看不到「为什么一个都没选上」）。
    ///
    /// 有候选时计数字段无意义（不累计）。
    pub fn acquire_with_reasons<F>(
        &self,
        channel_id: &str,
        model: &str,
        alias: Option<&str>,
        allow: F,
    ) -> (Option<AccountLease>, SkipReasons)
    where
        F: Fn(&str) -> bool,
    {
        self.acquire_with_reasons_detail(
            channel_id,
            model,
            alias,
            allow,
            None,
            None,
            CapabilityMask::EMPTY,
        )
    }

    /// [`Self::acquire_with_reasons`] 的回放+门控+能力过滤版（Phase1-B/A2、#40 §1）：
    /// `detail` 非 None 时逐账号记录跳过原因（`route_replay` 开关驱动，
    /// 请求本地缓冲，仅此一个消费者）；`gate` 非 None 时逐账号做**条件组
    /// 限制裁决**（返回 Some(原因码) = 跳过该账号，disable/skip 均记
    /// `limited` 档）；`needs` 非空时按账号目录能力位做**精确过滤**
    /// （上游名已定，见 `catalog::AccountCapabilityIndex`）。
    ///
    /// 三者皆缺省（`None` / `CapabilityMask::EMPTY`）时判定路径与原方法
    /// 完全一致，零额外分配 —— 这是「默认关 = 与今天逐字节等价」的形式。
    pub fn acquire_with_reasons_detail<F>(
        &self,
        channel_id: &str,
        model: &str,
        alias: Option<&str>,
        allow: F,
        gate: Option<LimiterGate<'_>>,
        mut detail: Option<&mut crate::log::AccountSkipCapture>,
        needs: CapabilityMask,
    ) -> (Option<AccountLease>, SkipReasons)
    where
        F: Fn(&str) -> bool,
    {
        let now = chrono::Utc::now().timestamp();
        let mut sessions = self.inner.sessions.write();
        // 能力索引只在有需求时上锁（空需求 = 老路径，连锁都不碰）。
        // 锁序统一 sessions → capabilities，与 `channel_may_serve` 一致。
        let capabilities = if needs.is_empty() {
            None
        } else {
            Some(self.inner.capabilities.read())
        };
        let mut candidates: Vec<usize> = Vec::new();
        let mut reasons = SkipReasons::default();
        let mut in_channel = 0usize;
        for (index, session) in sessions.iter().enumerate() {
            if session.channel_id != channel_id {
                continue;
            }
            in_channel += 1;
            if !session.is_enabled() {
                reasons.disabled += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "disabled"));
                }
                continue;
            }
            if !allow(&session.provider) {
                reasons.provider_off += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "provider_not_allowed"));
                }
                continue;
            }
            // 限制条件组裁决（Phase1-A2）：skip/disable 命中均跳过该账号；
            // 余额类即时生效，本地计数类在计数器接入前恒不命中（宁放勿误伤）
            if let Some(gate) = gate {
                if let Some(reason) = gate(session) {
                    reasons.limited += 1;
                    if let Some(cap) = detail.as_mut() {
                        cap.push(self.skip_detail(session, reason));
                    }
                    continue;
                }
            }
            if session.is_expired(now) {
                reasons.expired += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "expired"));
                }
                continue;
            }
            if !session.supports_model_any(model, alias) {
                reasons.model_mismatch += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "model_mismatch"));
                }
                continue;
            }
            // 能力精筛（#40 §1）：上游名已定，按本账号目录里该模型的能力位判定。
            // 只有**有证据的拒否**才裁人；无目录/老快照/名字未收录一律放行。
            // （写成嵌套 if-let 会触发 clippy::collapsible_if，故先取索引再判。）
            let capability_index = capabilities.as_ref().and_then(|caps| caps.get(&session.id));
            let blocked = capability_index
                .is_some_and(|index| index.verdict(model, alias, needs) == Some(false));
            if blocked {
                reasons.capability += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "capability_mismatch"));
                }
                continue;
            }
            if session.is_cooling(model, now) {
                reasons.cooling += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "cooling"));
                }
                continue;
            }
            // 余额限制器：available 低于阈值时跳过（P1-6 选路消费）
            if session.should_skip_by_balance() {
                reasons.balance_low += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "balance_low"));
                }
                continue;
            }
            if session.max_concurrency > 0 && session.in_flight >= session.max_concurrency {
                reasons.concurrency += 1;
                if let Some(cap) = detail.as_mut() {
                    cap.push(self.skip_detail(session, "concurrency"));
                }
                continue;
            }
            candidates.push(index);
        }
        if in_channel == 0 {
            reasons.no_accounts = true;
        }
        if candidates.is_empty() {
            return (None, reasons);
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
        (
            Some(AccountLease {
                inner: self.inner.clone(),
                session,
            }),
            reasons,
        )
    }

    /// 逐账号跳过明细行的构造（Phase1-B/#26；仅 route_replay 开启时被
    /// 调用；行不带 channel_id——由 [`crate::log::AccountSkipGroup`] 分组承载）。
    fn skip_detail(
        &self,
        session: &AccountSession,
        reason: &'static str,
    ) -> crate::log::AccountSkip {
        crate::log::AccountSkip {
            account_id: session.id.clone(),
            account_name: session.name.clone(),
            reason: reason.to_string(),
        }
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
            // 清该模型冷却；整号冷却也一并清掉 —— 账号既然成功响应，
            // 之前的账号级限流/认证失败窗口已经过去，继续挂着只会白白
            // 让账号退出调度。
            session.cooldowns.remove(model);
            session
                .cooldowns
                .remove(crate::account_session::model::ACCOUNT_WIDE_COOLDOWN);
            session.last_error = None;
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        self.persist_throttled(&snapshot);
    }

    /// 记入一次失败（按失败类型冷却），返回冷却秒数。
    ///
    /// 失败分类复用渠道断路器的 `classify_bridge_error` 结果。
    ///
    /// **冷却粒度**由失败类型决定（此前一律按模型冷却，账号级限流因此
    /// 只冷掉撞上的那一个模型，其余模型继续撞墙 —— 上游惩罚窗口越撞越长）：
    /// - `RateLimited { scope }` → `Account`/`Unknown` 冷整号，`Model` 冷该模型
    /// - `AuthFailed` / `PaymentRequired` → 冷整号（凭证/余额问题是账号级的）
    /// - 其余（5xx / 超时 / 连接失败 / 模型不存在）→ 只冷该模型
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
        // 这次失败该冷「整号」还是「这一个模型」
        let account_wide = match failure {
            FailureType::RateLimited { scope, .. } => scope.cools_whole_account(),
            FailureType::AuthFailed | FailureType::PaymentRequired => true,
            _ => false,
        };
        let policy = self.policy();
        let direct_switch =
            status.is_some_and(|code| self.inner.retry_on_status.read().contains(&code));
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
                let key = if account_wide {
                    crate::account_session::model::ACCOUNT_WIDE_COOLDOWN
                } else {
                    model
                };
                session.cooldowns.insert(key.to_string(), now + secs as i64);
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

    /// 按模型清冷却：`Some(model)` 只删该条（`"*"` = 整号哨兵），`None` 全清。
    /// 返回实际删除的条目数；账号不存在返回 `None`。
    ///
    /// 语义对齐 Agent2API 的 rate-limits/clear —— 可逆操作：清掉后若上游
    /// 仍在限流，下一次失败会重新标记，因此前端无需二次确认。
    pub fn clear_cooldown(&self, id: &str, model: Option<&str>) -> Option<usize> {
        let (snapshot, cleared) = {
            let mut sessions = self.inner.sessions.write();
            let session = sessions.iter_mut().find(|s| s.id == id)?;
            let cleared = match model {
                Some(key) => usize::from(session.cooldowns.remove(key).is_some()),
                None => {
                    let n = session.cooldowns.len();
                    session.cooldowns.clear();
                    if n > 0 {
                        // 与 reset_cooldowns 同口径：全清视为一次手动恢复
                        session.consecutive_errors = 0;
                        session.last_error = None;
                    }
                    n
                }
            };
            session.updated_at = chrono::Utc::now().timestamp();
            (session.clone(), cleared)
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!(
                "persist account session {} after cooldown clear: {e}",
                snapshot.id
            );
        }
        Some(cleared)
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

    /// token 三件套比较-再写：access / refresh / 过期时间一次落盘。
    ///
    /// 刷新会轮换 refresh_token —— 与 access 分两次写，中间崩掉会把账号
    /// 弄丢（旧 rt 已作废、新 rt 没存上），所以必须同一把 CAS 一起写。
    /// `next_refresh` 传明文（内部按 upsert 同口径做 `enc1:` 加密）。
    pub fn update_tokens_cas(
        &self,
        id: &str,
        expected_plain: &str,
        next: Credential,
        next_refresh: &str,
        next_expires_at: Option<i64>,
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
            session.refresh_secret = if next_refresh.trim().is_empty() {
                String::new()
            } else if next_refresh.starts_with("enc:") || next_refresh.starts_with("enc1:") {
                next_refresh.to_string()
            } else {
                crate::account_session::secret::seal_or_keep(
                    next_refresh,
                    &crate::account_session::secret::data_dir(),
                )
            };
            session.expires_at = next_expires_at;
            session.error_count = 0;
            session.last_error = None;
            session.updated_at = chrono::Utc::now().timestamp();
            session.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist account session {} tokens: {e}", snapshot.id);
        }
        CasOutcome::Updated
    }

    /// 上游明确拒了这枚凭据（401/403）：把 `expires_at` 拉到现在。
    ///
    /// 两个后果，都是我们要的：
    ///   1. `needs_refresh` 立刻为真 → 下一轮巡检（≤10 分钟）会去换证；
    ///   2. `is_expired` 立刻为真 → `sweep` 把它标成 `expired` 移出轮换，
    ///      不再拿一枚已死的令牌去撞上游（换证成功后 `update_tokens_cas` 会
    ///      写回新的 `expires_at`，`sweep` 随即恢复 `enabled`）。
    ///
    /// 只对**支持续期**的 provider 生效：不可续期的家（如纯 Key 类）被标记后
    /// 除重新登录/粘贴外无法自愈，那属于另一套语义，不在这里顺手改掉。
    ///
    /// 返回是否真的标了（provider 不支持续期 / 账号不存在时为 false）。
    pub fn mark_credential_rejected(&self, id: &str, provider: &str) -> bool {
        if !super::refresh::supports_refresh(provider) {
            return false;
        }
        let now = chrono::Utc::now().timestamp();
        let snapshot = {
            let mut sessions = self.inner.sessions.write();
            let Some(session) = sessions.iter_mut().find(|s| s.id == id) else {
                return false;
            };
            // 已经是「过期/待刷」状态就不重复写盘（避免 401 风暴下的无谓持久化）
            if session.expires_at.is_some_and(|at| at > 0 && at <= now) {
                return false;
            }
            session.expires_at = Some(now);
            session.status = "expired".to_string();
            session.last_error = Some("upstream rejected credential (401/403)".to_string());
            session.updated_at = now;
            session.clone()
        };
        if let Err(e) = self.persist(&snapshot) {
            tracing::warn!("persist rejected credential {}: {e}", snapshot.id);
        }
        true
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

    /// 编码路径走全局 data_dir（密钥文件所在目录）—— 拿跨模块共享锁设
    /// 独立临时目录，防并行测试互相换密钥（见 `secret::test_lock`）。
    fn with_secret_dir<R>(tag: &str, f: impl FnOnce() -> R) -> R {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let _guard = crate::account_session::secret::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aigx-store-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        crate::account_session::secret::set_data_dir(&dir);
        f()
    }

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
        with_secret_dir("upsert", || {
            let pool = pool();
            let created = pool.upsert(session("ch", "acct", 0)).unwrap();
            // 新写入用 enc1:（AES-256-GCM，见 crate::account_session::secret）。
            // 旧的 enc: base64 混淆前缀只在存量数据上还能看到。
            assert!(created.credential.secret.starts_with("enc1:"));
            let loaded = pool.get(&created.id).unwrap();
            assert_eq!(loaded.credential.secret_plain(), "sk-acct");
            assert_eq!(
                loaded.provider,
                super::super::registry::DEFAULT_PROVIDER_KIND
            );
        });
    }

    #[test]
    fn skip_reasons_summary_only_lists_nonzero() {
        let none = SkipReasons::default();
        assert_eq!(none.summary(), "没有可用账号");
        let empty_channel = SkipReasons {
            no_accounts: true,
            ..Default::default()
        };
        assert_eq!(empty_channel.summary(), "渠道下没有账号");
        let mixed = SkipReasons {
            cooling: 2,
            balance_low: 1,
            ..Default::default()
        };
        assert_eq!(mixed.summary(), "2 个冷却中、1 个余额低于阈值");
        // 顺序稳定（禁用 → provider → 过期 → 白名单 → 能力 → 冷却 → 余额 → 限制 → 并发）
        let full = SkipReasons {
            no_accounts: false,
            disabled: 1,
            provider_off: 1,
            expired: 1,
            model_mismatch: 1,
            capability: 1,
            cooling: 1,
            balance_low: 1,
            limited: 1,
            concurrency: 1,
        };
        assert_eq!(
            full.summary(),
            "1 个已禁用、1 个 provider 被关停、1 个凭证过期、1 个不在模型白名单、1 个不支持请求能力、1 个冷却中、1 个余额低于阈值、1 个被限制规则拦截、1 个并发占满"
        );
    }

    #[test]
    fn acquire_with_reasons_counts_skip_causes() {
        with_secret_dir("acquire-reasons", || {
            let pool = pool();
            // 禁用账号 + 冷却账号 + 可用账号
            let mut disabled = session("ch", "off", 0);
            disabled.status = "disabled".to_string();
            pool.upsert(disabled).unwrap();
            let cooling = pool.upsert(session("ch", "cool", 0)).unwrap();
            pool.upsert(session("ch", "ok", 0)).unwrap();
            // 给 cool 号挂上模型级冷却
            let failure = FailureType::ServerError;
            pool.record_failure(&cooling.id, "m", &failure, "boom", None);
            // 有可用账号：租得到，reasons 不再有意义
            let (lease, _) = pool.acquire_with_reasons("ch", "m", None, |_| true);
            assert!(lease.is_some());
            // 全部被挡：策略门（provider 过滤）先于瞬态态（冷却）计数——
            // 关停/白名单是根因，冷却是伴随态，502 文案要给可行动的原因。
            // off 号也被过滤，所以 provider_off=2、cooling=0。
            let (lease, reasons) = pool.acquire_with_reasons("ch", "m", None, |_| false);
            assert!(lease.is_none());
            assert_eq!(reasons.provider_off, 2);
            assert_eq!(reasons.cooling, 0);
            assert!(reasons.summary().contains("provider"));
            // 没有账号的渠道
            let (lease, reasons) = pool.acquire_with_reasons("other", "m", None, |_| true);
            assert!(lease.is_none());
            assert!(reasons.no_accounts);
        });
    }

    /// Phase1-B：逐账号跳过明细（route_replay 回放）——计数与明细一一对应。
    #[test]
    fn acquire_reasons_detail_records_per_account() {
        with_secret_dir("acquire-detail", || {
            let pool = pool();
            let mut disabled = session("ch", "off", 0);
            disabled.status = "disabled".to_string();
            pool.upsert(disabled).unwrap();
            let cooling = pool.upsert(session("ch", "cool", 0)).unwrap();
            pool.upsert(session("ch", "ok", 0)).unwrap();
            let failure = FailureType::ServerError;
            pool.record_failure(&cooling.id, "m", &failure, "boom", None);
            // 全挡（allow=false）：off/disabled、cool/provider_not_allowed、
            // ok/provider_not_allowed —— 三条明细
            let mut capture = crate::log::AccountSkipCapture::new("ch");
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| false,
                None,
                Some(&mut capture),
                CapabilityMask::EMPTY,
            );
            assert!(lease.is_none());
            assert_eq!(reasons.disabled, 1);
            assert_eq!(reasons.provider_off, 2);
            assert_eq!(capture.rows.len(), 3);
            // 顺序无关断言（取证：AccountSessionStore::upsert 尾部对池做
            // `sort_by(created_at, id)`——本测试三个账号同一秒创建，排序
            // 落到 id（UUID）字典序，测试运行间不稳定；迭代顺序不是插入
            // 序，断言按名归类比对，不能假设 [off, cool, ok]）。
            let mut by_name: Vec<(String, String)> = capture
                .rows
                .iter()
                .map(|r| (r.account_name.clone(), r.reason.clone()))
                .collect();
            by_name.sort();
            assert_eq!(
                by_name,
                vec![
                    ("cool".to_string(), "provider_not_allowed".to_string()),
                    ("off".to_string(), "disabled".to_string()),
                    ("ok".to_string(), "provider_not_allowed".to_string()),
                ]
            );
            // detail=None（开关关闭）：行为与 acquire_with_reasons 完全一致
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| false,
                None,
                None,
                CapabilityMask::EMPTY,
            );
            assert!(lease.is_none());
            assert_eq!(reasons.provider_off, 2);
        });
    }

    /// #26：明细行数有界（ROUTE_REPLAY_MAX_SKIPS），超出转 truncated 不静默丢。
    #[test]
    fn acquire_detail_is_bounded() {
        with_secret_dir("acquire-detail-bounded", || {
            let pool = pool();
            // 70 个全部禁用的账号 → 70 行明细，只留前 64 条
            for i in 0..70 {
                let mut s = session("ch", &format!("acct-{i}"), 0);
                s.status = "disabled".to_string();
                pool.upsert(s).unwrap();
            }
            let mut capture = crate::log::AccountSkipCapture::new("ch");
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| true,
                None,
                Some(&mut capture),
                CapabilityMask::EMPTY,
            );
            assert!(lease.is_none());
            assert_eq!(reasons.disabled, 70);
            assert_eq!(capture.rows.len(), crate::log::ROUTE_REPLAY_MAX_SKIPS);
            assert_eq!(capture.truncated, 70 - crate::log::ROUTE_REPLAY_MAX_SKIPS);
        });
    }

    #[test]
    fn acquire_prefers_higher_priority() {
        with_secret_dir("acquire-prio", || {
            let pool = pool();
            pool.upsert(session("ch", "low", 1)).unwrap();
            pool.upsert(session("ch", "high", 9)).unwrap();
            let lease = pool.acquire("ch", "m", None).expect("应有可用账号");
            assert_eq!(lease.session().name, "high");
        });
    }

    #[test]
    fn acquire_stops_at_concurrency_limit_and_releases_on_drop() {
        with_secret_dir("acquire-lease", || {
            let pool = pool();
            let mut only = session("ch", "only", 0);
            only.max_concurrency = 1;
            pool.upsert(only).unwrap();
            let lease = pool.acquire("ch", "m", None).expect("首个租约");
            assert!(pool.acquire("ch", "m", None).is_none());
            drop(lease);
            assert!(pool.acquire("ch", "m", None).is_some());
        });
    }

    #[test]
    fn failure_cools_down_only_the_upstream_model() {
        with_secret_dir("cooldown", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::ServerError;
            let secs = pool.record_failure(&account.id, "upstream-x", &failure, "boom", None);
            assert!(secs >= 60);
            assert!(pool.acquire("ch", "upstream-x", None).is_none());
            assert!(pool.acquire("ch", "other-model", None).is_some());
            assert!(pool.reset_cooldowns(&account.id));
            assert!(pool.acquire("ch", "upstream-x", None).is_some());
        });
    }

    #[test]
    fn clear_cooldown_removes_single_model_entry() {
        with_secret_dir("clear-cooldown", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::ServerError;
            pool.record_failure(&account.id, "model-a", &failure, "boom-a", None);
            pool.record_failure(&account.id, "model-b", &failure, "boom-b", None);
            // 只清 model-a：b 仍在冷却
            assert_eq!(pool.clear_cooldown(&account.id, Some("model-a")), Some(1));
            assert!(pool.acquire("ch", "model-a", None).is_some());
            assert!(pool.acquire("ch", "model-b", None).is_none());
            // 不存在的键：命中账号但没删掉任何东西
            assert_eq!(pool.clear_cooldown(&account.id, Some("ghost")), Some(0));
            // 整号清：剩余全没了，last_error 一并清
            assert_eq!(pool.clear_cooldown(&account.id, None), Some(1));
            let loaded = pool.get(&account.id).unwrap();
            assert!(loaded.cooldowns.is_empty());
            assert!(loaded.last_error.is_none());
            // 账号不存在：None
            assert_eq!(pool.clear_cooldown("no-such-id", None), None);
        });
    }

    /// 造一份目录快照（#40 §1 能力索引测试用）。
    fn catalog_snapshot(entries: &[(&str, &[&str])]) -> super::super::catalog::CatalogSnapshot {
        super::super::catalog::CatalogSnapshot {
            models: entries
                .iter()
                .map(|(id, caps)| super::super::catalog::CatalogEntry {
                    id: (*id).to_string(),
                    upstream: None,
                    context_length: None,
                    capabilities: caps.iter().map(|c| c.to_string()).collect(),
                })
                .collect(),
            fetched_at: 1,
            source: "remote".to_string(),
        }
    }

    /// 造一份能力索引。
    fn index_of(entries: &[(&str, &[&str])]) -> super::super::catalog::AccountCapabilityIndex {
        super::super::catalog::AccountCapabilityIndex::from_snapshot(&catalog_snapshot(entries))
    }

    /// #40 §1 默认等价：`needs` 为空时能力索引完全不参与判定。
    #[test]
    fn acquire_without_needs_ignores_capability_index() {
        with_secret_dir("needs-off", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            // 目录明确说这个模型不支持视觉 —— 但本次请求没有能力需求
            pool.set_catalog_index(&account.id, Arc::new(index_of(&[("m", &["tool"])])));
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| true,
                None,
                None,
                CapabilityMask::EMPTY,
            );
            assert!(lease.is_some(), "无需求时不得因能力位裁人");
            assert_eq!(reasons.capability, 0);
            // 匿名入口（acquire）同样不受影响
            assert!(pool.acquire("ch", "m", None).is_some());
        });
    }

    /// #40 §1 精筛：只有**有证据的拒否**才裁人；无证据（老快照 / 名字未收录）放行。
    #[test]
    fn acquire_needs_rejects_only_evidenced_mismatch() {
        with_secret_dir("needs-on", || {
            let pool = pool();
            // a：目录说 m 只支持工具（明确不支持视觉）→ 应被裁
            let a = pool.upsert(session("ch", "a", 5)).unwrap();
            pool.set_catalog_index(&a.id, Arc::new(index_of(&[("m", &["tool"])])));
            // b：老快照（条目在但没有能力位）→ 无证据，放行
            let b = pool.upsert(session("ch", "b", 0)).unwrap();
            pool.set_catalog_index(&b.id, Arc::new(index_of(&[("m", &[])])));
            let mut capture = crate::log::AccountSkipCapture::new("ch");
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| true,
                None,
                Some(&mut capture),
                CapabilityMask::VISION,
            );
            let lease = lease.expect("无证据的账号必须仍可被选中");
            assert_eq!(lease.id(), b.id.as_str(), "被裁的是有证据的 a");
            assert_eq!(reasons.capability, 1);
            assert_eq!(
                capture.rows,
                vec![("a".to_string(), "capability_mismatch".to_string())]
            );
        });
    }

    /// #40 §1 精筛：证据说支持 → 放行；别名命中能力位同样算数。
    #[test]
    fn acquire_needs_passes_when_capability_evidenced() {
        with_secret_dir("needs-pass", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            pool.set_catalog_index(
                &account.id,
                Arc::new(index_of(&[("m", &["tools", "reasoning", "video"])])),
            );
            // tools（复数）与 tool 同义
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| true,
                None,
                None,
                CapabilityMask::TOOL.union(CapabilityMask::VIDEO),
            );
            assert!(lease.is_some());
            assert_eq!(reasons.capability, 0);
            // 要视觉：已知不支持 → 裁
            let (lease, reasons) = pool.acquire_with_reasons_detail(
                "ch",
                "m",
                None,
                |_| true,
                None,
                None,
                CapabilityMask::VISION,
            );
            assert!(lease.is_none());
            assert_eq!(reasons.capability, 1);
        });
    }

    /// #40 §1 粗筛：渠道级只看「有无证据」与「并集是否覆盖」。
    #[test]
    fn channel_may_serve_uses_catalog_evidence() {
        with_secret_dir("coarse", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            // 未同步过目录 → 无证据 → 放行
            assert!(pool.channel_may_serve("ch", CapabilityMask::VISION));
            pool.set_catalog_index(&account.id, Arc::new(index_of(&[("m", &["tool"])])));
            // 有证据且并集不含视觉 → 整条渠道可裁
            assert!(!pool.channel_may_serve("ch", CapabilityMask::VISION));
            assert!(pool.channel_may_serve("ch", CapabilityMask::TOOL));
            // 别的渠道的账号不参与本渠道判定
            assert!(pool.channel_may_serve("other", CapabilityMask::VISION));
            // 无需求恒真
            assert!(pool.channel_may_serve("ch", CapabilityMask::EMPTY));
        });
    }

    /// #40 §1：索引在启动加载时按目录缓存重建（重启后不丢）。
    #[test]
    fn load_builds_capability_index_from_catalog_cache() {
        with_secret_dir("needs-load", || {
            let pool = pool();
            let created = pool.upsert(session("ch", "acct", 0)).unwrap();
            // 直接写目录缓存（= 目录同步流程的落盘形态），再 load 一次（= 重启）
            pool.store()
                .put(
                    &super::super::catalog::cache_key(&created.provider, &created.id),
                    &catalog_snapshot(&[("m", &["tool"])]),
                )
                .unwrap();
            pool.load().unwrap();
            assert!(!pool.channel_may_serve("ch", CapabilityMask::VISION));
            assert!(pool.channel_may_serve("ch", CapabilityMask::TOOL));
        });
    }

    #[test]
    fn clear_cooldown_can_remove_account_wide_sentinel() {
        with_secret_dir("clear-cooldown-sentinel", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::RateLimited {
                scope: crate::channel::circuit_breaker::RateLimitScope::Account,
                retry_after: Some(90),
            };
            pool.record_failure(&account.id, "model-a", &failure, "429", Some(429));
            assert!(pool.acquire("ch", "model-a", None).is_none());
            // 哨兵键 "*" 单独可清，清完全号恢复可用
            assert_eq!(
                pool.clear_cooldown(
                    &account.id,
                    Some(crate::account_session::model::ACCOUNT_WIDE_COOLDOWN)
                ),
                Some(1)
            );
            assert!(pool.acquire("ch", "model-a", None).is_some());
        });
    }

    #[test]
    fn account_scope_rate_limit_cools_whole_account() {
        with_secret_dir("cooldown-account", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::RateLimited {
                scope: crate::channel::circuit_breaker::RateLimitScope::Account,
                retry_after: Some(90),
            };
            let secs = pool.record_failure(
                &account.id,
                "upstream-x",
                &failure,
                "rate limited",
                Some(429),
            );
            assert_eq!(secs, 90);
            // 账号级限流：**任何**模型都不可用，不只是撞上的那个
            assert!(pool.acquire("ch", "upstream-x", None).is_none());
            assert!(
                pool.acquire("ch", "other-model", None).is_none(),
                "账号级限流必须冷却整号，不能只冷一个模型"
            );
            // 展示口径：整号冷却写在哨兵键上，前端渲染成「全部模型」
            let loaded = pool.get(&account.id).unwrap();
            let until = loaded
                .cooldowns
                .get(crate::account_session::model::ACCOUNT_WIDE_COOLDOWN)
                .copied()
                .expect("整号冷却应写进哨兵键");
            let now = chrono::Utc::now().timestamp();
            assert!(
                until > now && until <= now + 91,
                "哨兵键到期时间应落在 90 秒窗口内（实际差 {} 秒）",
                until - now
            );
        });
    }

    #[test]
    fn model_scope_rate_limit_cools_only_that_model() {
        with_secret_dir("cooldown-model", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::RateLimited {
                scope: crate::channel::circuit_breaker::RateLimitScope::Model,
                retry_after: None,
            };
            pool.record_failure(
                &account.id,
                "upstream-x",
                &failure,
                "rate limited",
                Some(429),
            );
            assert!(pool.acquire("ch", "upstream-x", None).is_none());
            assert!(
                pool.acquire("ch", "other-model", None).is_some(),
                "模型级限流只冷该模型，其余模型应照常可用"
            );
        });
    }

    #[test]
    fn unknown_scope_is_treated_as_account_wide() {
        with_secret_dir("cooldown-unknown", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            // 判不出作用域时按账号级保守处理（见 RateLimitScope::cools_whole_account）
            let failure = FailureType::RateLimited {
                scope: crate::channel::circuit_breaker::RateLimitScope::Unknown,
                retry_after: None,
            };
            pool.record_failure(
                &account.id,
                "upstream-x",
                &failure,
                "rate limited",
                Some(429),
            );
            assert!(pool.acquire("ch", "other-model", None).is_none());
        });
    }

    #[test]
    fn auth_failure_cools_whole_account() {
        with_secret_dir("cooldown-auth", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            pool.record_failure(
                &account.id,
                "upstream-x",
                &FailureType::AuthFailed,
                "401",
                Some(401),
            );
            // 凭证坏了，换模型也救不回来
            assert!(pool.acquire("ch", "other-model", None).is_none());
        });
    }

    #[test]
    fn rejected_credential_is_flagged_for_refresh() {
        with_secret_dir("rejected-cred", || {
            let pool = pool();
            let mut account = session("ch", "acct", 0);
            account.provider = "codearts".to_string();
            account.refresh_secret = "refresh-token".to_string();
            // 远端到期还早：光看 expires_at 判不出「凭据已被上游拒」
            account.expires_at = Some(chrono::Utc::now().timestamp() + 3_600);
            let account = pool.upsert(account).unwrap();
            assert!(
                !crate::account_session::refresh::needs_refresh(
                    &account,
                    chrono::Utc::now().timestamp()
                ),
                "离到期还有一小时，本来不该刷"
            );

            assert!(pool.mark_credential_rejected(&account.id, "codearts"));
            let after = pool.get(&account.id).unwrap();
            let now = chrono::Utc::now().timestamp();
            assert!(after.is_expired(now), "被上游拒后要移出轮换");
            assert_eq!("expired", after.status);
            assert!(
                crate::account_session::refresh::needs_refresh(&after, now),
                "打完标必须排进续期队列 —— 否则账号就死在那了"
            );

            // 重复标记不重复写盘（第二次返回 false）
            assert!(!pool.mark_credential_rejected(&account.id, "codearts"));

            // 不可续期的 provider 不参与这套自愈
            let plain = pool.upsert(session("ch", "plain", 0)).unwrap();
            assert!(!pool.mark_credential_rejected(&plain.id, "openai"));
            assert_eq!("enabled", pool.get(&plain.id).unwrap().status);
        });
    }

    #[test]
    fn success_clears_account_wide_cooldown() {
        with_secret_dir("cooldown-recover", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let failure = FailureType::RateLimited {
                scope: crate::channel::circuit_breaker::RateLimitScope::Account,
                retry_after: Some(30),
            };
            pool.record_failure(
                &account.id,
                "upstream-x",
                &failure,
                "rate limited",
                Some(429),
            );
            assert!(pool.acquire("ch", "other-model", None).is_none());
            // 账号成功响应一次 = 限流窗口已过，整号冷却该清掉
            pool.record_success(&account.id, "upstream-x", 100);
            assert!(pool.acquire("ch", "other-model", None).is_some());
        });
    }

    #[test]
    fn retry_on_status_uses_base_cooldown_only() {
        with_secret_dir("retry-status", || {
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
        });
    }

    #[test]
    fn credential_cas_rejects_stale_snapshot() {
        with_secret_dir("cas", || {
            let pool = pool();
            let account = pool.upsert(session("ch", "acct", 0)).unwrap();
            let next = Credential {
                kind: "api_key".to_string(),
                secret: "sk-new".to_string(),
                fields: std::collections::HashMap::new(),
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
        });
    }

    #[test]
    fn sweep_prunes_cooldowns_and_flips_expired_status() {
        with_secret_dir("sweep", || {
            let pool = pool();
            let mut account = session("ch", "acct", 0);
            account.cooldowns.insert("m".to_string(), 10);
            account.expires_at = Some(10);
            pool.upsert(account).unwrap();
            assert_eq!(pool.sweep(100), 1);
            let loaded = pool.all();
            assert_eq!(loaded[0].status, "expired");
            assert!(loaded[0].cooldowns.is_empty());
        });
    }

    #[test]
    fn remove_for_channel_cascades() {
        with_secret_dir("remove", || {
            let pool = pool();
            pool.upsert(session("ch-a", "a", 0)).unwrap();
            pool.upsert(session("ch-b", "b", 0)).unwrap();
            assert_eq!(pool.remove_for_channel("ch-a").unwrap(), 1);
            assert_eq!(pool.len(), 1);
        });
    }
}
