//! 模型 Key 的进程内权威（对齐 Agent2API v3.5.0-beta.2 `config/model_key_source.rs`）。
//!
//! A2A 用它在「配置 JSON 是 API Key 定义」的模型下，为每次真实鉴权结果签发一个
//! 不可伪造的 [`Source`]：代次（generation）+ 修订（revision）+ 撤销令牌。任何一次
//! Key 定义变更都先**整体撤销旧代**，新代只在真实发布成功后建立；撤销后旧 Source
//! 无论内容是否相同都不复活。
//!
//! AIGX 的 Key 落在 `ApiKeyStore`（DB/FileStore）里，由 `verify_api_key_full` 校验。
//! 本模块作为**额外的撤销闸**：发布一份权威定义后，数据面鉴权会在 Key 校验通过
//! 之后再核对「它是否仍是当前代的有效 Key」——运维在别处删除/停用 Key 时，只要
//! 触发一次 [`publish_keys`]，旧 Key 立即在这道闸上失效，而不必等缓存过期。
//!
//! 设计约束（与参考树一致）：
//! - [`Source`] 不可 `Clone/Debug/Default/serde`，只能由真实鉴权成功路径签发；
//! - [`authenticate_key`] 不接匿名 / 环境 Key / 管理会话配对，只认已发布定义；
//! - 撤销与发布都在短临界区内完成，不解析、不记录日志、不做 IO。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

/// 一条已发布的 Key 定义（映射自 `ApiKeyStore` 的 `ApiKey`）。
#[derive(Clone, Debug)]
pub struct KeyDefinition {
    pub id: String,
    pub name: String,
    /// 明文 Key（与 `ApiKey::key` 对应）。
    pub key: String,
    /// 是否启用；停用的 Key 不参与鉴权。
    pub enabled: bool,
    /// Provider 白名单；空 = 不限。
    pub allowed_providers: Vec<String>,
}

/// 鉴权作用域（对齐参考树 `KeyScope`）：全量或限定到若干提供商。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyScope {
    All,
    Providers(Vec<String>),
}

impl KeyScope {
    /// 是否允许访问某提供商（`All` 恒真）。
    pub fn allows(&self, provider: &str) -> bool {
        match self {
            Self::All => true,
            Self::Providers(list) => list.is_empty() || list.iter().any(|p| p == provider),
        }
    }
}

/// 仅供日志/展示的身份（不含 Key 明文）。
#[derive(Clone, Debug)]
pub struct KeyIdentity {
    pub id: String,
    pub name: String,
}

struct Record {
    revision: u64,
    revoked: AtomicBool,
}

struct Authority {
    generation: u64,
    revision: u64,
    revoked: AtomicBool,
    records: Vec<Arc<Record>>,
}

struct Definition {
    keys: Vec<KeyDefinition>,
}

impl Definition {
    fn from_keys(keys: Vec<KeyDefinition>) -> Self {
        Self { keys }
    }

    fn same(&self, other: &Self) -> bool {
        self.keys.len() == other.keys.len()
            && self.keys.iter().zip(other.keys.iter()).all(|(a, b)| {
                a.id == b.id
                    && a.key == b.key
                    && a.enabled == b.enabled
                    && a.allowed_providers == b.allowed_providers
            })
    }

    fn scope_for(&self, index: usize) -> Option<KeyScope> {
        let def = self.keys.get(index)?;
        if def.allowed_providers.is_empty() {
            Some(KeyScope::All)
        } else {
            Some(KeyScope::Providers(def.allowed_providers.clone()))
        }
    }
}

struct Published {
    authority: Arc<Authority>,
    definition: Definition,
}

struct State {
    generation: u64,
    revision: u64,
    current: Option<Published>,
    replacement: bool,
    failed: bool,
}

static AUTHORITY: Mutex<State> = Mutex::new(State {
    generation: 0,
    revision: 0,
    current: None,
    replacement: false,
    failed: false,
});

/// 曾出现在任意已发布代中的 Key 明文集合（用于区分「从未发布的新 Key」与
/// 「发布后被删除/停用的 Key」）。撤销闸只对后者 fail-closed，避免新 Key
/// 在下次 refresh 之前被误拒。
static EVER_SEEN: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

fn record_seen(keys: &[KeyDefinition]) {
    if let Ok(mut guard) = EVER_SEEN.lock() {
        let set = guard.get_or_insert_with(std::collections::HashSet::new);
        for def in keys {
            set.insert(def.key.clone());
        }
    }
}

/// 由真实鉴权成功路径签发的来源凭据；字段封闭且不可克隆/序列化。
pub struct Source {
    authority: Arc<Authority>,
    record: Arc<Record>,
    generation: u64,
    revision: u64,
    identity: KeyIdentity,
}

impl Source {
    /// 纯对象关系比较，不取门、不判存活。
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authority, &other.authority)
            && Arc::ptr_eq(&self.record, &other.record)
            && self.generation == other.generation
            && self.revision == other.revision
    }

    /// 当前是否仍是有效代（短同步核对）。
    pub fn is_current(&self) -> bool {
        let Ok(state) = AUTHORITY.try_lock() else {
            return false;
        };
        matches_current(
            &state,
            &self.authority,
            &self.record,
            self.generation,
            self.revision,
        )
    }

    /// 仅显示事实；不含 Key 明文。
    pub fn identity(&self) -> KeyIdentity {
        self.identity.clone()
    }
}

fn matches_current(
    state: &State,
    authority: &Arc<Authority>,
    record: &Arc<Record>,
    generation: u64,
    revision: u64,
) -> bool {
    if state.failed
        || state.replacement
        || authority.revoked.load(Ordering::Acquire)
        || record.revoked.load(Ordering::Acquire)
        || authority.generation != generation
        || authority.revision != revision
        || record.revision != revision
    {
        return false;
    }
    state.current.as_ref().is_some_and(|current| {
        Arc::ptr_eq(&current.authority, authority)
            && current
                .authority
                .records
                .iter()
                .any(|original| Arc::ptr_eq(original, record))
    })
}

/// 唯一签发入口：Key 校验成功后调用。
///
/// 返回 `None` = 未发布 / 已撤销 / 不匹配（调用方据此拒绝）。参考树返回
/// `(KeyScope, Source)`；这里把作用域挂在 [`Authenticated`] 上。
pub fn authenticate_key(presented: &str) -> Option<Authenticated> {
    if presented.is_empty() {
        return None;
    }
    let (authority, definition, index) = {
        let state = AUTHORITY.try_lock().ok()?;
        if state.failed || state.replacement {
            return None;
        }
        let current = state.current.as_ref()?;
        if current.authority.revoked.load(Ordering::Acquire) {
            return None;
        }
        let index = current
            .definition
            .keys
            .iter()
            .position(|def| def.enabled && def.key == presented)?;
        (
            current.authority.clone(),
            current.definition.keys.clone(),
            index,
        )
    };
    // 短门已释放；纯比较不触达 IO。
    let record = authority.records.get(index)?.clone();
    let scope = Definition::from_keys(definition.clone()).scope_for(index)?;
    let (id, name) = definition
        .get(index)
        .map(|d| (d.id.clone(), d.name.clone()))
        .unwrap_or_default();
    let state = AUTHORITY.try_lock().ok()?;
    if !matches_current(
        &state,
        &authority,
        &record,
        authority.generation,
        record.revision,
    ) {
        return None;
    }
    Some(Authenticated {
        scope,
        source: Source {
            generation: authority.generation,
            revision: record.revision,
            authority,
            record,
            identity: KeyIdentity { id, name },
        },
    })
}

/// 鉴权结果：作用域 + 来源凭据。
pub struct Authenticated {
    scope: KeyScope,
    source: Source,
}

impl Authenticated {
    pub fn scope(&self) -> &KeyScope {
        &self.scope
    }
    pub fn source(&self) -> &Source {
        &self.source
    }
}

/// 数据面闸门：未发布（未初始化）时放行。
///
/// 已发布时：
/// - Key 仍是当前代有效 Key → 放行；
/// - Key 曾在历史代出现过但现在不在（被删除/停用）→ 拒绝（撤销）；
/// - Key 从未在任何代出现过（运维新加、尚未 refresh）→ 放行，避免误拒。
pub fn check_current(presented: &str) -> bool {
    if !is_initialized() {
        return true;
    }
    if authenticate_key(presented).is_some() {
        return true;
    }
    if presented.is_empty() {
        return false;
    }
    // 曾在历史代出现过 → 视为已撤销。
    let seen = EVER_SEEN
        .lock()
        .map(|guard| guard.as_ref().is_some_and(|set| set.contains(presented)))
        .unwrap_or(false);
    !seen
}

/// 是否已经发布过一代（闸门是否生效）。
pub fn is_initialized() -> bool {
    AUTHORITY
        .lock()
        .map(|state| state.current.is_some() && !state.failed && !state.replacement)
        .unwrap_or(false)
}

fn revoke_locked(state: &mut State) -> Option<Arc<Authority>> {
    let original = state.current.take()?.authority;
    original.revoked.store(true, Ordering::Release);
    for record in &original.records {
        record.revoked.store(true, Ordering::Release);
    }
    Some(original)
}

fn revoke(replacement: bool, failed: bool) -> bool {
    let mut state = match AUTHORITY.lock() {
        Ok(state) => state,
        Err(error) => {
            let mut state = error.into_inner();
            state.failed = true;
            let _ = revoke_locked(&mut state);
            return false;
        }
    };
    state.replacement |= replacement;
    state.failed |= failed;
    let _ = revoke_locked(&mut state);
    !state.failed
}

/// 固定恢复前入口：真实替换前调用，撤销旧代且不复活。
pub fn pre_replace() -> bool {
    revoke(true, false)
}

/// 进程重载前调用。
pub fn before_reload() {
    let _ = pre_replace();
}

/// 失败关闭：进程内永久拒绝，直到下次成功发布。
pub fn fail_closed() {
    let _ = revoke(false, true);
}

/// Key 定义变化时先撤销旧代（无关配置改动不碰权威）。
pub fn before_change(previous: &[KeyDefinition], next: &[KeyDefinition]) {
    if !Definition::from_keys(previous.to_vec()).same(&Definition::from_keys(next.to_vec())) {
        let _ = revoke(false, false);
    }
}

/// 发布新代：撤销旧代 → 建立新代记录。返回是否成功。
pub fn publish_keys(keys: Vec<KeyDefinition>, reloaded: bool) -> bool {
    let seen_keys = keys.clone();
    let definition = Definition::from_keys(keys);
    let mut state = match AUTHORITY.lock() {
        Ok(state) => state,
        Err(error) => {
            let mut state = error.into_inner();
            state.failed = true;
            let _ = revoke_locked(&mut state);
            return false;
        }
    };
    if state.failed {
        return false;
    }
    if !reloaded
        && state
            .current
            .as_ref()
            .is_some_and(|current| current.definition.same(&definition))
    {
        return true;
    }
    let next_generation = match state.generation.checked_add(1) {
        Some(generation) => generation,
        None => {
            state.failed = true;
            let _ = revoke_locked(&mut state);
            return false;
        }
    };
    let next_revision = match state.revision.checked_add(1) {
        Some(revision) => revision,
        None => {
            state.failed = true;
            let _ = revoke_locked(&mut state);
            return false;
        }
    };
    let _ = revoke_locked(&mut state);
    state.generation = next_generation;
    state.revision = next_revision;
    state.replacement = false;
    let records: Vec<Arc<Record>> = (0..definition.keys.len())
        .map(|_| {
            Arc::new(Record {
                revision: next_revision,
                revoked: AtomicBool::new(false),
            })
        })
        .collect();
    state.current = Some(Published {
        authority: Arc::new(Authority {
            generation: next_generation,
            revision: next_revision,
            revoked: AtomicBool::new(false),
            records,
        }),
        definition,
    });
    drop(state);
    record_seen(&seen_keys);
    true
}

/// 管理面只读状态。
pub fn status() -> Value {
    let state = match AUTHORITY.lock() {
        Ok(state) => state,
        Err(error) => error.into_inner(),
    };
    json!({
        "initialized": state.current.is_some() && !state.failed && !state.replacement,
        "failed": state.failed,
        "replacing": state.replacement,
        "generation": state.generation,
        "revision": state.revision,
        "publishedKeys": state.current.as_ref().map(|c| c.definition.keys.len()).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: &str, plain: &str) -> KeyDefinition {
        KeyDefinition {
            id: id.into(),
            name: id.into(),
            key: plain.into(),
            enabled: true,
            allowed_providers: vec![],
        }
    }

    /// 单测串行覆盖整个生命周期：进程级权威是全局单例，绝不能用多个独立
    /// `#[test]` 并行改写（会顺序依赖假红）。这里一个测试内按序演练。
    #[test]
    fn lifecycle_contract_is_sequential() {
        // 1. 发布 a，闸门生效
        assert!(publish_keys(vec![key("a", "sk-a")], true));
        assert!(is_initialized());
        assert!(check_current("sk-a"));
        // 从未发布过的 Key：放行（避免新 Key 在 refresh 前被误拒）
        assert!(check_current("sk-never-seen"));

        // 2. 发布 a + b，旧代 Source 在替换后失效
        assert!(publish_keys(vec![key("a", "sk-a"), key("b", "sk-b")], true));
        let auth = authenticate_key("sk-a").expect("current key authenticates");
        assert!(auth.source().is_current());

        // 3. 发布仅 b：a 被撤销（曾出现过）
        assert!(publish_keys(vec![key("b", "sk-b")], true));
        assert!(!auth.source().is_current(), "旧代 Source 必须失效");
        assert!(!check_current("sk-a"), "移除的 Key 必须被撤销");
        assert!(check_current("sk-b"));

        // 4. 停用 b：不再签发
        let mut disabled = key("b", "sk-b");
        disabled.enabled = false;
        assert!(publish_keys(vec![disabled], true));
        assert!(!check_current("sk-b"), "停用的 Key 必须被撤销");

        // 5. 作用域：provider 白名单
        let mut scoped = key("c", "sk-c");
        scoped.allowed_providers = vec!["acme".into()];
        assert!(publish_keys(vec![scoped], true));
        let auth = authenticate_key("sk-c").unwrap();
        assert!(auth.scope().allows("acme"));
        assert!(!auth.scope().allows("other"));
    }
}
