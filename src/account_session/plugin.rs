// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件账号合同 —— 不透明凭证 / 公开投影 / 身份归并 / 状态 CAS。
//!
//! 对齐 A2A `core/account_store/{plugin_accounts,plugin_contract,plugin_identity,
//! plugin_public,plugin_state}.rs` 的可观察契约：
//!
//! - **凭证是插件自己的不透明 JSON**（`plugin_secret`），网关不解析结构；
//! - **公开视图**由 `plugin_public` + 白名单派生标量组成（`hasAccessToken` /
//!   `canRefresh` / `expiresAt`），凭证一个字节都不进公开出口；
//! - **身份归并**：`sha256(provider + 稳定身份字段)` 派生主键，重复添加
//!   幂等（同一身份更新原行，不产生副本）；
//! - **状态 CAS**：插件私有状态按 `{key: {revision, schema, value}}` 比较-
//!   交换，总量 ≤ 2 MiB、键数 ≤ 128（与 A2A 同口径）。
//!
//! ## 与 AIGX 原生账号的关系
//!
//! 原生账号（凭证在 `credential`、加密落盘）完全不受影响：本模块的所有
//! 写路径只在提供 `plugin_secret` 时启用。插件 provider 的账号自动挂到
//! 一个**隐藏的 `ChannelType::Plugin` 渠道**（每个 provider 一个），数据面
//! 路由由既有插件渠道承担，账号池只负责凭证与调度元数据。
//!
//! ## 隐藏渠道为什么要单独登记
//!
//! 渠道模型（`crate::channel::Channel`）是共享结构，本波不改字段；隐藏
//! 渠道 id 落在账号池自己的 KV（`acctses:hidden_plugin_channels`），
//! `GET /api/channels` 列表据此给行打 `hidden: true`（见共享 patch 报告）。

use std::sync::OnceLock;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::channel::{Channel, ChannelStore, ChannelType};

use super::model::{
    AccountSession, PLUGIN_STATE_MAX_BYTES, PLUGIN_STATE_MAX_KEYS, PLUGIN_STATE_MAX_KEY_LEN,
};
use super::store::AccountSessionStore;

/// 隐藏插件渠道 id 的 KV 键。
const HIDDEN_CHANNELS_KEY: &str = "acctses:hidden_plugin_channels";

/// 身份字段候选键（按优先级从高到低；大小写敏感——插件协议是驼峰）。
pub const IDENTITY_KEYS: &[&str] = &[
    "uid",
    "userId",
    "user_id",
    "accountId",
    "account_id",
    "sub",
    "email",
    "login",
    "username",
    "user",
    "nickname",
    "name",
    "id",
];

/// 公开投影的私密键黑名单（A2A `plugin_public::is_private_field` 同集）。
///
/// 这些键即使插件误写进 `plugin_public` 也不下发：公开投影是管理面列表
/// 接口的一部分，局域网内浏览器可读。
pub fn is_private_field(key: &str) -> bool {
    matches!(
        key,
        "pluginSecret"
            | "pluginPublic"
            | "pluginIdentity"
            | "pluginIdentityContract"
            | "pluginState"
            | "accessToken"
            | "refreshToken"
            | "access_token"
            | "refresh_token"
            | "apiKey"
            | "api_key"
            | "password"
            | "balanceToken"
            | "workbuddyWebSession"
            | "access_key_id"
            | "secret_access_key"
            | "security_token"
            | "oauth_context"
            | "cookie"
            | "cookies"
            | "session"
            | "sessionToken"
    )
}

// ── 身份归并 ────────────────────────────────────────────────────────

/// 从一组 JSON 对象里收集稳定身份字段（`BTreeMap` 天然按键排序）。
fn collect_identity_fields(fields: &Map<String, Value>, out: &mut Map<String, Value>) {
    for key in IDENTITY_KEYS {
        if out.contains_key(*key) {
            continue;
        }
        let Some(value) = fields.get(*key) else {
            continue;
        };
        let usable = match value {
            Value::String(text) => !text.trim().is_empty(),
            Value::Number(_) | Value::Bool(_) => true,
            _ => false,
        };
        if usable {
            out.insert((*key).to_string(), value.clone());
        }
    }
    // 凭证常把主要字段装在 `auth` 子对象里（OpenCode 系插件形态）
    if let Some(Value::Object(auth)) = fields.get("auth") {
        collect_identity_fields(auth, out);
    }
}

/// 身份种子：`provider + 稳定身份字段` 的可哈希串。
///
/// 优先用身份字段集合（uid / email / accountId…，provider 内的业务身份）；
/// 拿不到任何字段时退回插件自报的 `identity` 提示（A2A `pluginIdentity`）。
/// 两者都没有 → `None`（新账号主键退回随机 uuid——没有稳定身份就不该合并）。
pub fn identity_seed(
    provider: &str,
    secret: Option<&Value>,
    public: Option<&Value>,
    credential_fields: &std::collections::HashMap<String, String>,
    identity_hint: &str,
) -> Option<String> {
    let _ = provider;
    let mut fields = Map::new();
    // public 优先：A2A 的 account_view 已做过归一（secret 里可能是用户随手填的别名）
    for source in [public, secret] {
        if let Some(Value::Object(object)) = source {
            collect_identity_fields(object, &mut fields);
        }
    }
    for key in IDENTITY_KEYS {
        if fields.contains_key(*key) {
            continue;
        }
        if let Some(value) = credential_fields.get(*key) {
            if !value.trim().is_empty() {
                fields.insert((*key).to_string(), Value::String(value.clone()));
            }
        }
    }
    if !fields.is_empty() {
        // serde_json 的 Map 默认是 BTreeMap（无 preserve_order feature）→ 有序
        return Some(format!("fields:{}", Value::Object(fields)));
    }
    let hint = identity_hint.trim();
    if hint.is_empty() {
        None
    } else {
        Some(format!("hint:{hint}"))
    }
}

/// `sha256(provider + 0 + seed)` 的十六进制摘要。
pub fn identity_digest(provider: &str, seed: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 稳定主键：`{provider}-account-{sha256(provider + seed)}`（A2A 同形）。
pub fn derive_account_id(provider: &str, seed: &str) -> String {
    format!("{provider}-account-{}", identity_digest(provider, seed))
}

/// 既有账号的身份种子（缺插件字段时回落到 `credential.fields`）。
fn session_identity_seed(session: &AccountSession) -> Option<String> {
    identity_seed(
        &session.provider,
        session.plugin_secret.as_ref(),
        session.plugin_public.as_ref(),
        &session.credential.fields,
        &session.identity_key,
    )
}

// ── 公开投影 ────────────────────────────────────────────────────────

const EXPIRY_KEYS: &[&str] = &[
    "accessTokenExpiresAt",
    "expiresAt",
    "expires_at",
    "expires",
    "tokenExpiresAt",
    "expiresAtMs",
    "tokenExpireAt",
    "expiration",
];

fn token_text(fields: Option<&Map<String, Value>>, keys: &[&str]) -> bool {
    let Some(fields) = fields else { return false };
    let mut values = keys
        .iter()
        .filter_map(|key| {
            fields
                .get("auth")
                .and_then(|auth| auth.get(*key))
                .or_else(|| fields.get(*key))
        })
        .peekable();
    if values.peek().is_some() {
        return values.any(|value| value.as_str().is_some_and(|token| !token.trim().is_empty()));
    }
    false
}

fn expiry_millis(value: &Value) -> Option<i64> {
    let raw = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => {
            let text = text.trim();
            if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
                return (time.timestamp_millis() > 0).then(|| time.timestamp_millis());
            }
            text.parse::<f64>().ok()
        }
        _ => None,
    }?;
    if !raw.is_finite() || raw <= 0.0 {
        return None;
    }
    // 小于 1e11 视为秒 → 毫秒（A2A `expiry_millis` 同判据）
    let millis = if raw < 1e11 { raw * 1000.0 } else { raw };
    if millis >= i64::MAX as f64 {
        return None;
    }
    (millis >= 1.0).then_some(millis as i64)
}

/// 插件账号的公开投影（A2A `to_plugin_public_account` 的 AIGX 版）。
///
/// 组成：插件自报展示字段（过滤私密键）+ 网关事实（id/provider/name/
/// 权重/优先级/时间戳）+ 凭证派生标量（exists/canRefresh/expiresAt）。
/// **凭证绝不下发**。
pub fn plugin_account_view(session: &AccountSession) -> Value {
    let mut public = Map::new();
    if let Some(Value::Object(extra)) = &session.plugin_public {
        for (key, value) in extra {
            if !is_private_field(key) {
                public.insert(key.clone(), value.clone());
            }
        }
    }
    let secret = session.plugin_secret.as_ref().and_then(Value::as_object);
    let credential_fields: Map<String, Value> = session
        .credential
        .fields
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect();
    let credential_fields = Some(credential_fields);
    let mut has_access = token_text(
        secret,
        &[
            "accessToken",
            "access_token",
            "access",
            "token",
            "apiKey",
            "api_key",
            "key",
        ],
    ) || token_text(
        credential_fields.as_ref(),
        &[
            "accessToken",
            "access_token",
            "token",
            "apiKey",
            "api_key",
            "key",
        ],
    );
    // 插件持有本机引用 / 不透明凭证时，公开视图的既有标记仍可声明「存在」
    if !has_access
        && (session.credential.kind == super::model::CREDENTIAL_KIND_SESSION
            || session.credential.kind == super::model::CREDENTIAL_KIND_NONE)
    {
        has_access = !session.credential.secret.trim().is_empty()
            || public
                .get("hasAccessToken")
                .and_then(Value::as_bool)
                .unwrap_or(false);
    }
    let has_refresh = !session.refresh_secret.is_empty()
        || token_text(secret, &["refreshToken", "refresh_token", "refresh"])
        || token_text(
            credential_fields.as_ref(),
            &["refreshToken", "refresh_token"],
        );
    let can_refresh = public
        .get("canRefresh")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || has_refresh;
    public.insert("hasAccessToken".to_string(), Value::Bool(has_access));
    public.insert("hasRefreshToken".to_string(), Value::Bool(has_refresh));
    public.insert("canRefresh".to_string(), Value::Bool(can_refresh));

    // 到期时刻取「公开快照 / 凭证 / 账号级 expires_at」的最新值（陈旧快照
    // 不能压住新到期时刻，A2A 用 max 收口；账号级是秒，换算成毫秒）。
    let account_expiry = session
        .expires_at
        .filter(|at| *at > 0)
        .map(|at| at.saturating_mul(1000));
    let expiry = EXPIRY_KEYS
        .iter()
        .flat_map(|key| [public.get(*key), secret.and_then(|fields| fields.get(*key))])
        .flatten()
        .filter_map(expiry_millis)
        .chain(account_expiry)
        .max();
    if let Some(expiry) = expiry {
        public.insert("expiresAt".to_string(), Value::from(expiry));
    }

    // 网关事实覆盖插件展示字段（身份与运营属性不可被插件改写）
    public.insert("id".to_string(), Value::String(session.id.clone()));
    public.insert(
        "provider".to_string(),
        Value::String(session.provider.clone()),
    );
    public.insert("name".to_string(), Value::String(session.name.clone()));
    public.insert(
        "identity".to_string(),
        Value::String(if session.identity_key.is_empty() {
            session.id.clone()
        } else {
            session.identity_key.clone()
        }),
    );
    public.insert(
        "source".to_string(),
        Value::String(if session.is_plugin_account() {
            "plugin".to_string()
        } else {
            "builtin".to_string()
        }),
    );
    public.insert("enabled".to_string(), Value::Bool(session.is_enabled()));
    public.insert("hidden".to_string(), Value::Bool(session.hidden));
    public.insert(
        "needsRelogin".to_string(),
        Value::Bool(session.needs_relogin()),
    );
    public.insert("priority".to_string(), Value::from(session.priority));
    public.insert("weight".to_string(), Value::from(session.weight));
    public.insert("sortOrder".to_string(), Value::from(session.sort_order));
    public.insert(
        "maxConcurrent".to_string(),
        Value::from(session.max_concurrency),
    );
    public.insert(
        "allowRouteSkip".to_string(),
        Value::Bool(session.allow_route_skip),
    );
    public.insert(
        "rateLimits".to_string(),
        Value::Object(session.rate_limits.clone()),
    );
    public.insert("models".to_string(), json!(session.models));
    // A2A 协议侧时间戳是毫秒
    public.insert(
        "addedAt".to_string(),
        Value::from(session.created_at.max(0) * 1000),
    );
    public.insert(
        "updatedAt".to_string(),
        Value::from(session.updated_at.max(0) * 1000),
    );
    public.insert(
        "proxy".to_string(),
        match session.proxy.as_deref() {
            Some(proxy) if !proxy.trim().is_empty() => {
                json!({ "state": "static", "config": { "source": "direct", "url": proxy } })
            }
            _ => json!({ "state": "unset" }),
        },
    );
    Value::Object(public)
}

// ── 写路径：upsert / CAS ────────────────────────────────────────────

/// `plugin_upsert_account_with_source_and_proxy` 的入参。
#[derive(Debug, Clone)]
pub struct PluginUpsertInput<'a> {
    /// 承载渠道 id（插件账号用隐藏插件渠道；空串 = 沿用旧行的渠道）。
    pub channel_id: &'a str,
    pub provider: &'a str,
    /// 插件自报的稳定身份提示（A2A `pluginIdentity`；可空）。
    pub identity: &'a str,
    pub name: &'a str,
    pub secret: &'a Value,
    pub public: &'a Value,
    /// 账户来源：`plugin` / `builtin`（A2A 只允许这两个值）。
    pub source: &'a str,
    pub proxy: Option<&'a str>,
    pub hidden: bool,
}

/// upsert 结果。
#[derive(Debug, Clone)]
pub struct PluginUpserted {
    pub session: AccountSession,
    pub created: bool,
}

impl AccountSessionStore {
    /// 该 provider 的插件/原生账号（按创建序）。
    pub fn plugin_accounts_for_provider(&self, provider: &str) -> Vec<AccountSession> {
        let provider = super::registry::normalize_kind(provider);
        self.all()
            .into_iter()
            .filter(|session| session.provider.eq_ignore_ascii_case(&provider))
            .collect()
    }

    /// 读一条账号的插件形态（A2A `plugin_account`）：
    /// `{id, provider, identity, secret, public, maxConcurrentConfigured, proxy}`。
    ///
    /// 无 `plugin_secret` 的原生账号按「摊平 credential」返回 secret，方便
    /// 插件在迁移过渡期读到既有凭证（与 A2A `flatten_builtin_secret` 同因）。
    pub fn plugin_account(&self, id: &str) -> Option<Value> {
        let session = self.get(id)?;
        let secret = match &session.plugin_secret {
            Some(value) if !value.is_null() => value.clone(),
            _ => {
                let mut flattened = Map::new();
                if !session.credential.secret_plain().is_empty() {
                    flattened.insert(
                        "token".to_string(),
                        Value::String(session.credential.secret_plain()),
                    );
                }
                for key in session.credential.fields.keys() {
                    flattened.insert(
                        key.clone(),
                        Value::String(session.credential.field_plain(key)),
                    );
                }
                Value::Object(flattened)
            }
        };
        Some(json!({
            "id": session.id,
            "provider": session.provider,
            "identity": if session.identity_key.is_empty() { session.id.clone() } else { session.identity_key.clone() },
            "secret": secret,
            "public": plugin_account_view(&session),
            "maxConcurrentConfigured": session.max_concurrency,
            "proxy": session.proxy.as_deref().map(|proxy| json!({ "state": "static", "config": { "url": proxy } }))
                .unwrap_or_else(|| json!({ "state": "unset" })),
        }))
    }

    /// 按 provider 建/更新插件账号（A2A `plugin_upsert_account_with_source_and_proxy`）。
    ///
    /// 身份归并（幂等核心）：
    /// 1. `identity_hint` 命中既有 `identity_key`；
    /// 2. 身份字段摘要（uid/email/accountId…）相等；
    /// 3. 派生主键已存在。
    ///
    /// 多条匹配视为冲突（**未自动合并或覆盖**）；新主键按
    /// [`derive_account_id`] 派生，无稳定身份时退回随机 uuid。
    pub fn plugin_upsert_account_with_source_and_proxy(
        &self,
        input: PluginUpsertInput<'_>,
    ) -> Result<PluginUpserted, String> {
        if input.provider.trim().is_empty() {
            return Err("插件账号缺少提供商".to_string());
        }
        if !matches!(input.source, "plugin" | "builtin") {
            return Err("标准账号来源必须是 plugin 或 builtin".to_string());
        }
        let provider = super::registry::normalize_kind(input.provider);
        let identity = input.identity.trim();
        if identity.len() > 512 || identity.contains('\0') {
            return Err("插件必须提供非空且稳定的账号身份".to_string());
        }
        if !input.secret.is_object() {
            return Err("插件凭证必须是不透明 JSON 对象".to_string());
        }
        // 新建账号必须带承载渠道：选号（acquire）按渠道过滤，渠道为空的账号
        // 永远选不到。API/登录桥在调用前已解析隐藏插件渠道；这里再卡一道，
        // 防止别的调用方漏填后静默产出「看得见但转不了」的账号。
        if input.channel_id.trim().is_empty() {
            return Err("插件账号缺少承载渠道（隐藏插件渠道未创建）".to_string());
        }
        let seed = identity_seed(
            &provider,
            Some(input.secret),
            Some(input.public),
            &Default::default(),
            identity,
        );
        let digest = seed.as_deref().map(|seed| identity_digest(&provider, seed));
        let candidate_id = seed
            .as_deref()
            .map(|seed| derive_account_id(&provider, seed));

        let outcome = self.plugin_tx(
            |sessions, now| -> (Result<PluginUpserted, String>, Vec<AccountSession>) {
                // ① 身份归并：同 provider 且（提示相等 / 摘要相等 / 主键相等）
                let mut matches: Vec<usize> = Vec::new();
                for (index, session) in sessions.iter().enumerate() {
                    if !session.provider.eq_ignore_ascii_case(&provider) {
                        continue;
                    }
                    let hint_match = !identity.is_empty()
                        && (session.identity_key == identity || session.id == identity);
                    let digest_match = digest.as_deref().is_some_and(|digest| {
                        identity_digest(
                            &provider,
                            &session_identity_seed(session).unwrap_or_default(),
                        )
                        .as_str()
                            == digest
                            && session_identity_seed(session).is_some()
                    });
                    let id_match = candidate_id
                        .as_deref()
                        .is_some_and(|candidate| session.id == candidate);
                    if hint_match || digest_match || id_match {
                        matches.push(index);
                    }
                }
                if matches.len() > 1 {
                    return (
                        Err("多条旧账号匹配同一插件身份，未自动合并或覆盖".to_string()),
                        Vec::new(),
                    );
                }
                if let Some(index) = matches.pop() {
                    let session = &mut sessions[index];
                    session.plugin_secret = Some(input.secret.clone());
                    session.plugin_public = Some(input.public.clone());
                    if !identity.is_empty() {
                        session.identity_key = identity.to_string();
                    }
                    if !input.channel_id.trim().is_empty() {
                        session.channel_id = input.channel_id.trim().to_string();
                    }
                    if !input.name.trim().is_empty() && session.name.trim().is_empty() {
                        session.name = input.name.trim().to_string();
                    }
                    if let Some(proxy) = input.proxy {
                        session.proxy = if proxy.trim().is_empty() {
                            None
                        } else {
                            Some(proxy.trim().to_string())
                        };
                    }
                    // 重新登录拿到新凭证后，同次清掉旧凭证的重登标记（A2A 同）
                    session.needs_relogin = false;
                    if session.status == super::model::STATUS_NEEDS_RELOGIN {
                        session.status = "enabled".to_string();
                    }
                    session.updated_at = now;
                    let saved = session.clone();
                    (
                        Ok(PluginUpserted {
                            session: saved.clone(),
                            created: false,
                        }),
                        vec![saved],
                    )
                } else {
                    let id = candidate_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    if let Some(occupied) = sessions.iter().find(|session| session.id == id) {
                        if !occupied.provider.eq_ignore_ascii_case(&provider) {
                            return (
                                Err("账号身份与已有主键冲突，未覆盖原账号".to_string()),
                                Vec::new(),
                            );
                        }
                        // 同 provider 同主键：归并进既有行（候选 id 与实际行一致）
                        let mut saved = occupied.clone();
                        saved.plugin_secret = Some(input.secret.clone());
                        saved.plugin_public = Some(input.public.clone());
                        if !identity.is_empty() {
                            saved.identity_key = identity.to_string();
                        }
                        saved.updated_at = now;
                        if let Some(slot) = sessions.iter_mut().find(|session| session.id == id) {
                            *slot = saved.clone();
                        }
                        return (
                            Ok(PluginUpserted {
                                session: saved.clone(),
                                created: false,
                            }),
                            vec![saved],
                        );
                    }
                    let channel_id = input.channel_id.trim().to_string();
                    let mut session =
                        AccountSession::new(channel_id, input.name.trim().to_string());
                    session.id = id;
                    session.provider = provider.clone();
                    session.plugin_secret = Some(input.secret.clone());
                    session.plugin_public = Some(input.public.clone());
                    session.identity_key = identity.to_string();
                    session.hidden = input.hidden;
                    session.allow_route_skip = true;
                    session.created_at = now;
                    session.updated_at = now;
                    if let Some(proxy) = input.proxy {
                        session.proxy = Some(proxy.trim().to_string()).filter(|p| !p.is_empty());
                    }
                    sessions.push(session.clone());
                    // 与 upsert 同口径：按创建时间升序保持稳定全序
                    sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
                    (
                        Ok(PluginUpserted {
                            session: session.clone(),
                            created: true,
                        }),
                        vec![session],
                    )
                }
            },
        );
        outcome
    }

    /// 比较-再写插件凭证（A2A `plugin_update_account`）。
    ///
    /// `expect` 是插件读到的秘密值；当前值仍等于它才写入，否则返回
    /// `Ok(false)`（并发刷新竞态，不是错误）。`public` 为 `Some(object)`
    /// 时同步替换公开快照，并把 `needsRelogin` 等标记镜像到账号级。
    pub fn plugin_update_secret_cas(
        &self,
        id: &str,
        expect: &Value,
        secret: &Value,
        public: Option<&Value>,
    ) -> Result<bool, String> {
        self.plugin_tx(
            |sessions, now| -> (Result<bool, String>, Vec<AccountSession>) {
                let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
                    return (Err(format!("账号 {id} 不存在")), Vec::new());
                };
                let current = session.plugin_secret.clone().unwrap_or(Value::Null);
                if &current != expect {
                    return (Ok(false), Vec::new());
                }
                session.plugin_secret = Some(secret.clone());
                if let Some(Value::Object(public)) = public {
                    session.plugin_public = Some(Value::Object(public.clone()));
                    if let Some(flag) = public.get("needsRelogin").and_then(Value::as_bool) {
                        session.needs_relogin = flag;
                    }
                    if let Some(reason) = public.get("needsReloginReason").and_then(Value::as_str) {
                        session.last_error = Some(reason.to_string());
                    }
                }
                session.updated_at = now;
                (Ok(true), vec![session.clone()])
            },
        )
    }

    /// 读插件状态槽（A2A `plugin_state`）：缺失返回 `{revision:0,schema:1,value:null}`。
    pub fn plugin_state_get(&self, provider: &str, id: &str, key: &str) -> Result<Value, String> {
        let session = self.get(id).ok_or("账号不存在")?;
        if !session.provider.eq_ignore_ascii_case(provider) {
            return Err("账号不属于当前提供商".to_string());
        }
        Ok(session
            .plugin_state
            .as_ref()
            .and_then(|state| state.get(key))
            .cloned()
            .unwrap_or_else(|| json!({ "revision": 0, "schema": 1, "value": null })))
    }

    /// 插件状态比较-交换（A2A `plugin_state_compare_exchange`）。
    ///
    /// 返回 `{updated, revision}`：`updated=false` = 期望版本已变（**不是
    /// 错误**，调用方按并发冲突处理）。校验：键 ≤160 字符、schema ≥1、
    /// 值序列化 ≤2 MiB、状态键数 ≤128、总量 ≤2 MiB。
    pub fn plugin_state_cas(
        &self,
        provider: &str,
        id: &str,
        key: &str,
        expected_revision: u64,
        schema: u32,
        value: Value,
    ) -> Result<Value, String> {
        if key.is_empty()
            || key.len() > PLUGIN_STATE_MAX_KEY_LEN
            || schema == 0
            || value.to_string().len() > 256 * 1024
        {
            return Err("插件状态名称、版本或大小无效".to_string());
        }
        self.plugin_tx(
            |sessions, now| -> (Result<Value, String>, Vec<AccountSession>) {
                let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
                    return (Err("账号不存在".to_string()), Vec::new());
                };
                if !session.provider.eq_ignore_ascii_case(provider) {
                    return (Err("账号归属已变化".to_string()), Vec::new());
                }
                let mut state = session
                    .plugin_state
                    .as_ref()
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let revision = state
                    .get(key)
                    .and_then(|entry| entry.get("revision"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if revision != expected_revision {
                    return (
                        Ok(json!({ "updated": false, "revision": revision })),
                        Vec::new(),
                    );
                }
                if !state.contains_key(key) && state.len() >= PLUGIN_STATE_MAX_KEYS {
                    return (Err("账号插件状态条数超过上限".to_string()), Vec::new());
                }
                let revision = match revision.checked_add(1) {
                    Some(revision) => revision,
                    None => return (Err("插件状态版本溢出".to_string()), Vec::new()),
                };
                state.insert(
                    key.to_string(),
                    json!({ "revision": revision, "schema": schema, "value": value }),
                );
                let serialized = match serde_json::to_vec(&state) {
                    Ok(serialized) => serialized,
                    Err(e) => return (Err(format!("插件状态序列化失败: {e}")), Vec::new()),
                };
                if serialized.len() > PLUGIN_STATE_MAX_BYTES {
                    return (Err("账号插件状态总量超过 2 MiB".to_string()), Vec::new());
                }
                session.plugin_state = Some(Value::Object(state));
                session.updated_at = now;
                (
                    Ok(json!({ "updated": true, "revision": revision })),
                    vec![session.clone()],
                )
            },
        )
    }

    /// 按 provider 找持有某状态键的账号（A2A `plugin_states_for_provider`）。
    pub fn plugin_states_for_provider(&self, provider: &str, key: &str) -> Vec<(String, Value)> {
        self.plugin_accounts_for_provider(provider)
            .into_iter()
            .filter(|session| {
                session.is_enabled() && !session.plugin_secret.as_ref().is_none_or(Value::is_null)
            })
            .filter_map(|session| {
                session
                    .plugin_state
                    .as_ref()
                    .and_then(|state| state.get(key))
                    .map(|value| (session.id.clone(), value.clone()))
            })
            .collect()
    }

    /// 当前被标记为隐藏的插件渠道 id（`GET /api/channels` 打标用）。
    pub fn hidden_plugin_channel_ids(&self) -> Vec<String> {
        self.store()
            .get::<Vec<String>>(HIDDEN_CHANNELS_KEY)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// 幂等解析/创建 **隐藏插件承载渠道**（每个 provider 一个）。
    ///
    /// 名称 = provider（A2A 同口径）；`channel_type = plugin`、
    /// `plugin_provider = provider`；不写凭证（凭证在账号行）。渠道 id 登记
    /// 进账号池的隐藏名单，`GET /api/channels` 据此给行打 `hidden: true`。
    pub fn ensure_hidden_plugin_channel(
        &self,
        channels: &ChannelStore,
        provider: &str,
    ) -> Result<Channel, String> {
        static CREATE_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
        let provider = super::registry::normalize_kind(provider);
        let _guard = CREATE_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(found) = channels.list().into_iter().find(|channel| {
            channel.channel_type == ChannelType::Plugin
                && channel.plugin_provider.eq_ignore_ascii_case(&provider)
        }) {
            self.mark_hidden_channel(&found.id);
            return Ok(found);
        }
        let channel = Channel {
            id: String::new(),
            name: provider.clone(),
            channel_type: ChannelType::Plugin,
            base_url: String::new(),
            backup_base_urls: Vec::new(),
            api_key: String::new(),
            priority: 0,
            weight: 1,
            status: "enabled".to_string(),
            models: Vec::new(),
            model_mapping: std::collections::HashMap::new(),
            cost_pricing: std::collections::HashMap::new(),
            account_id: String::new(),
            plugin_provider: provider.clone(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            response_time: None,
            test_time: None,
            balance: None,
            credit: None,
            quirks: Default::default(),
            balance_scheme: String::new(),
            balance_unit: String::new(),
            balance_query_interval_secs: None,
            balance_last_query_at: None,
            balance_last_error: None,
            created_at: 0,
            updated_at: 0,
        };
        let created = channels
            .add(channel)
            .map_err(|error| format!("创建插件承载渠道失败：{error}"))?;
        self.mark_hidden_channel(&created.id);
        Ok(created)
    }

    /// 把渠道 id 记进隐藏名单（重复调用幂等）。
    fn mark_hidden_channel(&self, channel_id: &str) {
        let mut ids = self.hidden_plugin_channel_ids();
        if ids.iter().any(|id| id == channel_id) {
            return;
        }
        ids.push(channel_id.to_string());
        if let Err(error) = self.store().put::<Vec<String>>(HIDDEN_CHANNELS_KEY, &ids) {
            tracing::warn!("保存隐藏插件渠道名单失败: {error}");
        }
    }
}

/// 合并原生与插件 provider 目录行（按 `kind` 去重，**插件优先**）。
///
/// 归属判据与 `plugin_host::registry::is_plugin_owned` 同口径（2026-11 修订：
/// 插件优先，原生实现只在插件未装/停用/加载失败时兜底）：
///
/// - **插件声明了原生家**（如 workbuddy）：只出一行，`source = "plugin"`、
///   `native_fallback = true`；原生条目的展示元数据（label / category /
///   auth / chat_path / default_base_url…）在插件行缺省时补上——运维仍能
///   看到「这家有原生实现，插件接管后原生降为兜底」。
/// - **插件独占家**（如 kiro）：`source = "plugin"`、`native_supported = false`。
/// - 同一 kind 被多个包声明：保留第一份（安装顺序稳定）。
///
/// 行顺序沿用原生目录顺序，插件独占家追加在尾部（界面分组不抖动）。
pub fn merge_provider_rows(native: Vec<Value>, plugins: Vec<Value>) -> Vec<Value> {
    // 插件行按 kind 索引（第一份胜出）
    let mut plugin_by_kind: std::collections::HashMap<String, Value> =
        std::collections::HashMap::new();
    for row in plugins {
        let Some(kind) = row.get("kind").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        if kind.trim().is_empty() {
            continue;
        }
        plugin_by_kind.entry(kind).or_insert(row);
    }
    let mut out: Vec<Value> = Vec::with_capacity(native.len() + plugin_by_kind.len());
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in native {
        let Some(kind) = row.get("kind").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        match plugin_by_kind.remove(&kind) {
            Some(mut plugin) => {
                enrich_from_native(&mut plugin, &row);
                emitted.insert(kind);
                out.push(plugin);
            }
            None => out.push(row),
        }
    }
    // 插件独占家：按 kind 排序追加（HashMap 迭代无序，输出必须稳定）
    let mut rest: Vec<(String, Value)> = plugin_by_kind.into_iter().collect();
    rest.sort_by(|a, b| a.0.cmp(&b.0));
    for (kind, mut row) in rest {
        if emitted.contains(&kind) {
            continue;
        }
        if let Some(object) = row.as_object_mut() {
            object.insert("native_supported".to_string(), Value::Bool(false));
            object.insert("native_fallback".to_string(), Value::Bool(false));
        }
        out.push(row);
    }
    out
}

/// 插件行接管原生家时：原生展示元数据补进插件行的空位。
///
/// 只补**空位**（插件自报优先），且只补展示/描述类字段，不碰归属字段
/// （`source` / `pack_id` / `login` 由插件行决定）；`native_supported` 沿用
/// 原生读数，另加 `native_fallback: true` 说明「原生实现降为兜底」。
fn enrich_from_native(plugin: &mut Value, native: &Value) {
    let Some(plugin) = plugin.as_object_mut() else {
        return;
    };
    for key in [
        "label",
        "protocol",
        "default_base_url",
        "category",
        "auth",
        "credential_hint",
        "chat_path",
        "openai_compatible",
        "requires_base_url",
        "native_supported",
    ] {
        let missing = plugin
            .get(key)
            .map(|value| {
                value.is_null()
                    || value.as_str().is_some_and(|text| text.trim().is_empty())
                    || value.as_array().is_some_and(Vec::is_empty)
            })
            .unwrap_or(true);
        if !missing {
            continue;
        }
        if let Some(value) = native.get(key) {
            if !value.is_null() {
                plugin.insert(key.to_string(), value.clone());
            }
        }
    }
    // 插件行的 label 若仍是 kind 原文，用原生展示名替换（运维可读）
    if let (Some(Value::String(label)), Some(Value::String(native_label))) =
        (plugin.get("label").cloned(), native.get("label").cloned())
    {
        if label == plugin.get("kind").and_then(Value::as_str).unwrap_or("") {
            plugin.insert("label".to_string(), Value::String(native_label));
        }
    }
    plugin.insert("native_fallback".to_string(), Value::Bool(true));
}

/// 判断某 provider 是否由插件接管（原生包或脚本包）。
///
/// 数据面路由的权威判据在 `plugin_host`/`script_host`；本函数只给管理面
/// 「这个 provider 该不该走插件账号流程」一个同步答案。
pub fn provider_is_plugin_owned(state: &crate::api::openai::AppState, provider: &str) -> bool {
    state.plugin_host.registry().is_plugin_owned(provider)
        || state.script_host.route_for_provider(provider).is_some()
        || state
            .script_host
            .routed_providers()
            .iter()
            .any(|id| id == provider)
}

/// 插件 provider 的承载渠道：隐藏 `ChannelType::Plugin`（每 provider 一个）。
pub fn plugin_channel_for(
    state: &crate::api::openai::AppState,
    provider: &str,
) -> Result<Channel, String> {
    state
        .account_sessions
        .ensure_hidden_plugin_channel(&state.channel_store, provider)
}

// ── 快照辅助（列表响应用）──────────────────────────────────────────

/// 逐 provider 的首选账号 id（启用 + 未过期，按 priority 降序 / sort_order
/// 升序 / created_at 升序选第一个，与列表排序同口径）。
pub fn current_account_ids(sessions: &[AccountSession]) -> Value {
    let now = chrono::Utc::now().timestamp();
    let mut heads: std::collections::BTreeMap<&str, &AccountSession> =
        std::collections::BTreeMap::new();
    for session in sessions {
        if !session.is_enabled() || session.expires_at.is_some_and(|at| at > 0 && at <= now) {
            continue;
        }
        match heads.get(session.provider.as_str()) {
            None => {
                heads.insert(session.provider.as_str(), session);
            }
            Some(current) => {
                let candidate = (
                    std::cmp::Reverse(session.priority),
                    session.sort_order,
                    session.created_at,
                    session.id.as_str(),
                );
                let incumbent = (
                    std::cmp::Reverse(current.priority),
                    current.sort_order,
                    current.created_at,
                    current.id.as_str(),
                );
                if candidate < incumbent {
                    heads.insert(session.provider.as_str(), session);
                }
            }
        }
    }
    Value::Object(
        heads
            .into_iter()
            .map(|(provider, session)| (provider.to_string(), Value::String(session.id.clone())))
            .collect(),
    )
}

/// provider 摘要（`{id,label,count}`）：列表分组标题与筛选器直接用。
pub fn provider_summary(sessions: &[AccountSession]) -> Value {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for session in sessions {
        *counts.entry(session.provider.as_str()).or_insert(0) += 1;
    }
    let rows: Vec<Value> = counts
        .into_iter()
        .map(|(provider, count)| {
            let label = super::registry::find(provider)
                .map(|spec| spec.label)
                .unwrap_or(provider);
            json!({ "id": provider, "label": label, "count": count })
        })
        .collect();
    Value::Array(rows)
}

/// 隐藏账号摘要：`{total, byProvider: {provider: n}, accounts: [公开投影]}`。
pub fn hidden_accounts_summary(sessions: &[AccountSession]) -> Value {
    let hidden: Vec<&AccountSession> = sessions.iter().filter(|session| session.hidden).collect();
    let mut by_provider = Map::new();
    for session in &hidden {
        let next = by_provider
            .get(&session.provider)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        by_provider.insert(session.provider.clone(), Value::from(next));
    }
    json!({
        "total": hidden.len(),
        "byProvider": Value::Object(by_provider),
        "accounts": hidden.iter().map(|session| plugin_account_view(session)).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::FileStore;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    fn temp_store() -> AccountSessionStore {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-plugin-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 凭证加密用全局 data_dir：测试里把「插件合同」路径与加密路径隔离，
        // 插件 secret 是明文 JSON、不经 Credential::encoded。
        AccountSessionStore::new(Arc::new(FileStore::new(dir)))
    }

    fn input<'a>(provider: &'a str, identity: &'a str, secret: &'a Value) -> PluginUpsertInput<'a> {
        PluginUpsertInput {
            channel_id: "ch-plugin",
            provider,
            identity,
            name: "acct",
            secret,
            public: &Value::Null,
            source: "plugin",
            proxy: None,
            hidden: false,
        }
    }

    /// 同一身份重复 upsert 命中同一行（幂等），不同身份生成不同主键。
    #[test]
    fn identity_merge_is_idempotent() {
        let store = temp_store();
        let secret = json!({ "accessToken": "tok-1", "uid": "u-100" });
        let first = store
            .plugin_upsert_account_with_source_and_proxy(input("kiro", "kiro:u-100", &secret))
            .unwrap();
        assert!(first.created);
        let id = first.session.id.clone();
        assert!(
            id.starts_with("kiro-account-"),
            "主键按 provider 隔离：{id}"
        );
        assert_eq!(
            first.session.channel_id, "ch-plugin",
            "创建时必须带承载渠道（否则选号按渠道过滤找不到）"
        );

        // 同身份 + 改 token：更新原行，不产生副本
        let second = store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "kiro:u-100",
                &json!({ "accessToken": "tok-2", "uid": "u-100" }),
            ))
            .unwrap();
        assert!(!second.created);
        assert_eq!(second.session.id, id);
        assert_eq!(store.plugin_accounts_for_provider("kiro").len(), 1);
        assert_eq!(
            store.get(&id).unwrap().plugin_secret.unwrap()["accessToken"],
            "tok-2"
        );
    }

    /// 身份字段（uid）相同、提示串不同 → 仍归并到同一行。
    #[test]
    fn identity_fields_beat_hint() {
        let store = temp_store();
        let a = store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "first-hint",
                &json!({ "uid": "u-1", "accessToken": "a" }),
            ))
            .unwrap();
        let b = store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "second-hint",
                &json!({ "uid": "u-1", "accessToken": "b" }),
            ))
            .unwrap();
        assert_eq!(a.session.id, b.session.id);
        assert_eq!(store.plugin_accounts_for_provider("kiro").len(), 1);
    }

    /// 不同身份字段 → 不同主键（不会误并）。
    #[test]
    fn different_identities_stay_isolated() {
        let store = temp_store();
        store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "",
                &json!({ "uid": "u-1" }),
            ))
            .unwrap();
        store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "",
                &json!({ "uid": "u-2" }),
            ))
            .unwrap();
        assert_eq!(store.plugin_accounts_for_provider("kiro").len(), 2);
    }

    /// 凭证 CAS：期望值匹配才写；旧快照返回 false（不是错误）。
    #[test]
    fn secret_cas_rejects_stale_expectation() {
        let store = temp_store();
        let saved = store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "hint",
                &json!({ "accessToken": "old" }),
            ))
            .unwrap();
        let id = saved.session.id;
        // 旧快照写失败（Ok(false)，不是 Err）
        assert!(!store
            .plugin_update_secret_cas(
                &id,
                &json!({ "accessToken": "stale" }),
                &json!({ "accessToken": "new" }),
                None,
            )
            .unwrap());
        // 匹配的期望值写入成功
        assert!(store
            .plugin_update_secret_cas(
                &id,
                &json!({ "accessToken": "old" }),
                &json!({ "accessToken": "new" }),
                None,
            )
            .unwrap());
        assert_eq!(
            store.get(&id).unwrap().plugin_secret.unwrap()["accessToken"],
            "new"
        );
    }

    /// 状态 CAS：版本匹配递增；版本过期返回 updated=false。
    #[test]
    fn plugin_state_cas_bumps_revision() {
        let store = temp_store();
        let saved = store
            .plugin_upsert_account_with_source_and_proxy(input(
                "kiro",
                "hint",
                &json!({ "uid": "u" }),
            ))
            .unwrap();
        let id = saved.session.id;
        let first = store
            .plugin_state_cas("kiro", &id, "models", 0, 1, json!({ "list": ["m1"] }))
            .unwrap();
        assert_eq!(first["updated"], true);
        assert_eq!(first["revision"], 1);
        let stale = store
            .plugin_state_cas("kiro", &id, "models", 0, 1, json!({ "list": ["m2"] }))
            .unwrap();
        assert_eq!(stale["updated"], false);
        let second = store
            .plugin_state_cas("kiro", &id, "models", 1, 1, json!({ "list": ["m2"] }))
            .unwrap();
        assert_eq!(second["revision"], 2);
        assert_eq!(
            store.plugin_state_get("kiro", &id, "models").unwrap()["value"]["list"][0],
            "m2"
        );
    }

    /// 公开投影：私密键被滤掉、派生标量正确、凭证不下发。
    #[test]
    fn public_view_filters_private_keys() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "kiro".to_string();
        session.plugin_secret = Some(json!({ "accessToken": "secret-token", "refreshToken": "r" }));
        session.plugin_public = Some(json!({
            "uid": "u-1",
            "accessToken": "leak",
            "password": "leak",
            "edition": "intl"
        }));
        let view = plugin_account_view(&session);
        assert_eq!(view["hasAccessToken"], true);
        assert_eq!(view["hasRefreshToken"], true);
        assert_eq!(view["canRefresh"], true);
        assert_eq!(view["uid"], "u-1");
        assert!(view.get("accessToken").is_none(), "凭证键必须被过滤");
        assert!(view.get("password").is_none(), "密码键必须被过滤");
        let text = view.to_string();
        assert!(!text.contains("secret-token"));
        assert!(!text.contains("leak"));
    }

    /// 到期时刻按毫秒归一（秒值 ×1000）。
    #[test]
    fn public_view_normalizes_expiry_to_millis() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "kiro".to_string();
        session.plugin_secret = Some(json!({ "expiresAt": 1_800_000_000i64 }));
        session.expires_at = Some(1_800_000_000);
        let view = plugin_account_view(&session);
        assert_eq!(view["expiresAt"], 1_800_000_000_000i64);
    }

    /// provider 目录合并：**插件优先**——插件声明了原生家时只出一行
    /// `source=plugin`，原生展示元数据被补进空位并标 `native_fallback`。
    #[test]
    fn provider_directory_merge_is_plugin_first() {
        let native = vec![
            json!({
                "kind": "workbuddy", "source": "native", "label": "WorkBuddy",
                "category": "agent", "default_base_url": "https://wb.example",
                "native_supported": true, "auth": "bearer"
            }),
            json!({ "kind": "openai", "source": "native", "label": "OpenAI" }),
        ];
        let plugins = vec![
            json!({
                "kind": "workbuddy", "source": "plugin", "label": "workbuddy",
                "pack_id": "magpie", "native_supported": true
            }),
            json!({ "kind": "kiro", "source": "plugin", "label": "Kiro" }),
            json!({ "kind": "kiro", "source": "plugin", "label": "dup" }),
            json!({ "kind": "astudio", "source": "plugin", "label": "AStudio" }),
        ];
        let merged = merge_provider_rows(native, plugins);
        let kinds: Vec<&str> = merged
            .iter()
            .map(|row| row["kind"].as_str().unwrap())
            .collect();
        // workbuddy 只出现一次（插件行），openai 保持原生，插件独占家按 kind 排序
        assert_eq!(kinds, vec!["workbuddy", "openai", "astudio", "kiro"]);
        let workbuddy = &merged[0];
        assert_eq!(workbuddy["source"], "plugin", "插件优先接管原生家");
        assert_eq!(workbuddy["pack_id"], "magpie");
        assert_eq!(workbuddy["native_fallback"], true);
        // 插件 label 恰等于 kind 原文 → 用原生展示名替换（运维可读）；
        // 插件自报的**非占位** label 优先保留（见下方 enrich 用例）
        assert_eq!(workbuddy["label"], "WorkBuddy");
        assert_eq!(workbuddy["category"], "agent");
        assert_eq!(workbuddy["default_base_url"], "https://wb.example");
        assert_eq!(workbuddy["auth"], "bearer");
        // 插件独占家：native_supported=false（不冒充原生）
        let kiro = merged.iter().find(|row| row["kind"] == "kiro").unwrap();
        assert_eq!(kiro["native_supported"], false);
        assert_eq!(kiro["native_fallback"], false);
        assert_eq!(kiro["label"], "Kiro");
    }
}
