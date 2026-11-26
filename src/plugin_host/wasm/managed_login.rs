// Ported from Agent2API v3.5.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 托管登录（`login.managed.v1`）—— 宿主签发的登录会话 + 五个受控动作。
//!
//! 对齐上游 `plugin_host/wasm/managed.rs`（五个动作）与 `managed_policy.rs`
//! （静态授权表）。上游把动作挂在一个**宿主自有的登录任务**（`login::managed::Call`）
//! 上：`context` 读任务上下文、`input.take` 一次性取走真人提交材料、
//! `step.publish` 与宿主期望的步骤做 CAS、`loopback.open/close` 申请/关闭
//! 本机回调监听。插件永远拿不到「凭空开一个托管登录」的能力。
//!
//! ## AIGX 侧的实现边界（诚实）
//!
//! AIGX 的 Wasm 宿主没有上游那套 `LoginTaskHandle` / `Call` 栈，因此这里
//! 落地一个**自洽的会话登记表**：
//!
//! - [`issue`] 由宿主（将来的登录任务接线点）签发一个绑定 `provider × account ×
//!   identity × flow × methodIndex` 的会话，返回 `flow`；
//! - 插件随后在**同一 flow** 的调用里访问五个动作；会话不在登记表里即
//!   `forbidden`（与上游「当前调用没有托管登录权限」同口径）；
//! - `context` / `input.take` / `step.publish` 的语义与上游逐条一致；
//! - `loopback.open` 目前**只做声明校验**，本机构建没有回调监听端口，
//!   如实返回 `unsupported`（`login.loopback.v1` 不列为已服务能力）。
//!
//! 因此本模块整体属于「新文件待接线」：dispatch 已接好，真正的登录任务
//! 签发（`issue`）与回调监听由主控后续接线。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

/// 主能力（与上游同名）。
pub(crate) const CAPABILITY: &str = "login.managed.v1";
/// 回调监听能力（`login.loopback.*` 的额外闸门）。
pub(crate) const LOOPBACK_CAPABILITY: &str = "login.loopback.v1";
/// 单次会话最长有效期（与上游登录任务 15 分钟同量级）。
const MAX_TTL_MS: i64 = 900_000;
/// 在途会话上限。
const MAX_SESSIONS: usize = 16;
/// 单个动作消息上限（与上游 `MAX_MESSAGE` 一致）。
const MAX_MESSAGE: usize = 256 * 1024;

fn now_ms() -> i64 {
    super::now_ms()
}

fn unavailable() -> WireError {
    WireError::with_code(
        "托管登录执行体已关闭、停用或换代，请重新发起",
        "plugin_unavailable",
    )
}

fn forbidden(message: &str) -> WireError {
    WireError::with_code(message, "forbidden")
}

fn bad_params(message: &str) -> WireError {
    WireError::with_code(message, "bad_params")
}

fn conflict(message: &str) -> WireError {
    WireError::with_code(message, "conflict")
}

// ── 静态授权表（逐条对齐上游 `managed_policy.rs`）────────────────────

/// 回调类型（只用来决定「哪些来源必须被显式批准」）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum CallbackKind {
    GoogleCode,
    AutoclawCode,
    CatpawPassport,
    KiroPortal,
    KiroAws,
    TraeCode,
}

const KIRO_AWS_ORIGINS: &[&str] = &[
    "https://oidc.us-east-1.amazonaws.com",
    "https://oidc.us-east-2.amazonaws.com",
    "https://oidc.eu-central-1.amazonaws.com",
];

const KIRO_PORTS: &[u16] = &[
    3128, 4649, 6588, 8008, 9091, 49153, 50153, 51153, 52153, 53153,
];

#[derive(Clone, Copy)]
#[allow(dead_code)] // 待接线：`loopback.open` 接线后 host/path/ports 才会被读取
struct Profile {
    id: &'static str,
    host: &'static str,
    path: &'static str,
    ports: &'static [u16],
    origins: &'static [&'static str],
    callback: CallbackKind,
}

fn approved(provider: &str, method: u64, id: &str) -> Option<Profile> {
    match (provider, method, id) {
        ("autoclaw-intl", 0, "autoclaw-intl.zai.v1") => Some(Profile {
            id: "autoclaw-intl.zai.v1",
            host: "localhost",
            path: "/auth/callback-zai",
            ports: &[18432, 19654, 19723, 53699],
            origins: &[],
            callback: CallbackKind::AutoclawCode,
        }),
        ("autoclaw-intl", 1, "autoclaw-intl.google.v1") => Some(Profile {
            id: "autoclaw-intl.google.v1",
            host: "localhost",
            path: "/auth/callback-google",
            ports: &[18432, 19654, 19723, 53699],
            origins: &["https://accounts.google.com"],
            callback: CallbackKind::GoogleCode,
        }),
        ("catpaw", 0, "catpaw.passport.v1") => Some(Profile {
            id: "catpaw.passport.v1",
            host: "127.0.0.1",
            path: "/catpaw/callback",
            ports: &[0],
            origins: &["https://catpaw.meituan.com"],
            callback: CallbackKind::CatpawPassport,
        }),
        ("kiro", 0, "kiro.portal.v1") => Some(Profile {
            id: "kiro.portal.v1",
            host: "localhost",
            path: "/signin/callback",
            ports: KIRO_PORTS,
            origins: &["https://app.kiro.dev"],
            callback: CallbackKind::KiroPortal,
        }),
        ("kiro", 0, "kiro.aws.v1") => Some(Profile {
            id: "kiro.aws.v1",
            host: "127.0.0.1",
            path: "/oauth/callback",
            ports: KIRO_PORTS,
            origins: KIRO_AWS_ORIGINS,
            callback: CallbackKind::KiroAws,
        }),
        ("trae", 0, "trae.pkce.v1") => Some(Profile {
            id: "trae.pkce.v1",
            host: "127.0.0.1",
            path: "/authorize",
            ports: &[0],
            origins: &["https://www.trae.cn"],
            callback: CallbackKind::TraeCode,
        }),
        _ => None,
    }
}

/// `wasm.json` 的 `managed_login` 声明（逐字对齐上游）。
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub methods: Vec<u64>,
    #[serde(default)]
    pub loopback_profiles: Vec<String>,
    #[serde(default)]
    pub authorization_origins: Vec<String>,
}

impl Declaration {
    /// 校验声明形状（安装 / 会话签发时调用）。
    #[allow(dead_code)] // 待接线：会话签发侧在安装/发起登录时调用
    pub(crate) fn validate(&self, provider: &str) -> Result<(), String> {
        if self.methods.is_empty()
            || self.methods.len() > 16
            || self
                .methods
                .iter()
                .any(|index| *index > 9_007_199_254_740_991)
            || self
                .methods
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.methods.len()
            || self.loopback_profiles.len() > 8
            || self.authorization_origins.len()
                > if provider == "kiro" {
                    KIRO_AWS_ORIGINS.len() + 1
                } else {
                    8
                }
        {
            return Err("托管登录权限声明无效".into());
        }
        let mut approved_origins = std::collections::HashSet::new();
        let mut profiles = std::collections::HashSet::new();
        for id in &self.loopback_profiles {
            if !profiles.insert(id) {
                return Err("托管登录回调配置重复".into());
            }
            let profile = self
                .methods
                .iter()
                .find_map(|index| approved(provider, *index, id))
                .ok_or_else(|| "托管登录回调配置尚未获主程序批准".to_string())?;
            approved_origins.extend(profile.origins.iter().copied());
        }
        let mut origins = std::collections::HashSet::new();
        for origin in &self.authorization_origins {
            let parsed = url::Url::parse(origin).map_err(|_| "托管授权页来源格式无效")?;
            if parsed.scheme() != "https"
                || parsed.port_or_known_default() != Some(443)
                || parsed.path() != "/"
                || parsed.query().is_some()
                || parsed.fragment().is_some()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.origin().ascii_serialization() != *origin
                || !approved_origins.contains(origin.as_str())
                || !origins.insert(origin)
            {
                return Err("托管授权页必须使用主程序批准的精确 HTTPS 来源".into());
            }
        }
        Ok(())
    }

    /// 取某方法可用的回调配置（必须在 `methods` 与 `loopback_profiles` 内，
    /// 且其必要来源已全部被显式批准）。
    fn profile(&self, provider: &str, method: u64, id: &str) -> Option<Profile> {
        if !self.methods.contains(&method) || !self.loopback_profiles.iter().any(|item| item == id)
        {
            return None;
        }
        let profile = approved(provider, method, id)?;
        let required = if profile.callback == CallbackKind::KiroAws {
            &KIRO_AWS_ORIGINS[..2]
        } else {
            profile.origins
        };
        required
            .iter()
            .all(|origin| {
                self.authorization_origins
                    .iter()
                    .any(|item| item.as_str() == *origin)
            })
            .then_some(profile)
    }

    /// 本方法在 `authorizationOrigins` 里应公布的来源（去重）。
    fn origins(&self, provider: &str, method: u64) -> Vec<String> {
        let mut origins: Vec<String> = Vec::new();
        for id in &self.loopback_profiles {
            if let Some(profile) = self.profile(provider, method, id) {
                for origin in profile.origins {
                    if self
                        .authorization_origins
                        .iter()
                        .any(|item| item.as_str() == *origin)
                        && !origins.iter().any(|item| item.as_str() == *origin)
                    {
                        origins.push((*origin).to_string());
                    }
                }
            }
        }
        origins
    }
}

/// 读取并解析某 provider 的 `managed_login` 声明（缺失/非法 → `None`）。
fn declaration(provider: &str) -> Option<Declaration> {
    let manifest = super::manifest_for(provider)?;
    serde_json::from_value(manifest.managed_login?).ok()
}

// ── 会话登记表 ─────────────────────────────────────────────────────

struct Session {
    provider: String,
    account: String,
    identity: String,
    method_index: u64,
    /// 宿主期望的交互步骤 id（`login.step.publish` 的 CAS 依据）。
    expected: String,
    /// 登录启动时的 options（`login.context` 可回读）。
    options: Value,
    expires_at: i64,
    input: Option<Value>,
    taken: bool,
    published: bool,
    step: Option<Value>,
    loopback: Option<Value>,
}

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prune(map: &mut HashMap<String, Session>) {
    let now = now_ms();
    map.retain(|_, session| session.expires_at > now);
}

fn random_flow() -> Result<String, WireError> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce)
        .map_err(|_| WireError::with_code("托管登录随机源不可用", "internal"))?;
    Ok(format!(
        "managed_{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

/// 宿主签发一个托管登录会话，返回 `flow`。
///
/// **待接线**：真正的登录任务（管理面发起 / 浏览器授权回调）由主控接线后
/// 调用本函数；在那之前没有任何路径能创建会话，五个动作因此一律 `forbidden`。
#[allow(dead_code)] // 待接线：登录任务签发点尚未接线
pub(crate) fn issue(
    provider: &str,
    account: &str,
    identity: &str,
    method_index: u64,
    expected: &str,
    options: Value,
    ttl_ms: i64,
) -> Result<String, WireError> {
    if provider.is_empty() || expected.is_empty() || expected.len() > 128 {
        return Err(bad_params("托管登录会话参数无效"));
    }
    let budget = ttl_ms.clamp(1_000, MAX_TTL_MS);
    let flow = random_flow()?;
    let mut map = sessions()
        .lock()
        .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
    prune(&mut map);
    // 同一账号发起新登录即顶替旧会话（旧 flow 立即失效）。
    if !account.is_empty() {
        map.retain(|_, session| !(session.provider == provider && session.account == account));
    }
    if map.len() >= MAX_SESSIONS {
        return Err(WireError::with_code(
            "托管登录会话数量已达上限",
            "plugin_busy",
        ));
    }
    map.insert(
        flow.clone(),
        Session {
            provider: provider.to_string(),
            account: account.to_string(),
            identity: identity.to_string(),
            method_index,
            expected: expected.to_string(),
            options,
            expires_at: now_ms().saturating_add(budget),
            input: None,
            taken: false,
            published: false,
            step: None,
            loopback: None,
        },
    );
    Ok(flow)
}

/// 宿主向会话投递一次性输入材料（`login.input.take` 的消费对象）。
#[allow(dead_code)] // 待接线：管理面提交真人材料时调用
pub(crate) fn submit_input(flow: &str, value: Value) -> Result<(), WireError> {
    let mut map = sessions()
        .lock()
        .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
    prune(&mut map);
    let session = map.get_mut(flow).ok_or_else(|| unavailable())?;
    if session.taken {
        return Err(conflict("当前登录输入已被读取，不能重复使用"));
    }
    session.input = Some(value);
    Ok(())
}

/// 摘除某 provider 的全部会话（停用 / 卸载时调用）。
pub(crate) fn forget_provider(provider: &str) {
    if let Ok(mut map) = sessions().lock() {
        map.retain(|_, session| session.provider != provider);
    }
}

/// 摘除单个会话（流程结束 / 取消）。
#[allow(dead_code)] // 待接线：登录任务取消/结束时调用
pub(crate) fn revoke_flow(flow: &str) {
    if let Ok(mut map) = sessions().lock() {
        map.remove(flow);
    }
}

/// 绑定校验：provider 必须一致；会话若绑定账号/身份则必须匹配；不得过期。
fn check_binding(session: &Session, context: &Context, provider: &str) -> Result<(), WireError> {
    if session.provider != provider {
        return Err(forbidden("托管登录会话不属于当前插件"));
    }
    if !session.account.is_empty() && session.account != context.account {
        return Err(forbidden("托管登录会话不属于当前账号"));
    }
    if !session.identity.is_empty()
        && !context.identity.is_empty()
        && session.identity != context.identity
    {
        return Err(forbidden("托管登录会话身份已变化"));
    }
    if session.expires_at <= now_ms() {
        return Err(unavailable());
    }
    Ok(())
}

/// 把 `step.publish` 的入参投影成可公开的步骤快照（剥离凭据原值）。
fn public_step(params: &Value) -> Value {
    let mut value = params.clone();
    if let Some(account) = value.get_mut("account").and_then(Value::as_object_mut) {
        account.remove("secret");
    }
    if let Some(object) = value.as_object_mut() {
        object.remove("proxy");
    }
    value
}

/// `step.publish` 里若带账号，按公开视图 + 身份规则校验（本地重实现，
/// 与 `host::validate_identity` 同口径）。
fn validate_published_account(context: &Context, params: &Value) -> Result<(), WireError> {
    let account = &params["account"];
    if account.is_null() {
        return Ok(());
    }
    if !account.is_object() {
        return Err(bad_params("登录步骤的账号必须是对象"));
    }
    super::public_view::validate(&account["public"], &account["secret"])?;
    if context.shared.manifest.allows("accounts.identity.v1")
        && account["identity"].as_str().is_none_or(|identity| {
            identity.trim().is_empty() || identity.len() > 512 || identity.contains('\0')
        })
    {
        return Err(super::wire::error("插件必须返回稳定、非空的账号身份"));
    }
    Ok(())
}

impl Context {
    /// 分派 `login.context / input.take / step.publish / loopback.open / close`。
    pub(super) async fn managed_login(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        self.permission(CAPABILITY)?;
        let provider = self.shared.manifest.provider.clone();
        let flow = self.flow.clone();
        if flow.is_empty() {
            return Err(forbidden("当前调用没有托管登录权限"));
        }
        if serde_json::to_vec(params)
            .map_err(|_| bad_params("托管登录参数不可编码"))?
            .len()
            > MAX_MESSAGE
        {
            return Err(WireError::with_code(
                "托管登录消息超过大小限制",
                "plugin_resource_limit",
            ));
        }
        match method {
            "login.context" | "login.input.take" => {
                if params.as_object().is_none_or(|fields| !fields.is_empty()) {
                    return Err(bad_params("托管登录上下文查询不接受参数"));
                }
                let mut map = sessions()
                    .lock()
                    .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
                prune(&mut map);
                let session = map
                    .get_mut(&flow)
                    .ok_or_else(|| forbidden("当前调用没有托管登录权限"))?;
                check_binding(session, self, &provider)?;
                if method == "login.input.take" {
                    if session.taken {
                        return Err(conflict("当前登录输入已被读取，不能重复使用"));
                    }
                    let input = session
                        .input
                        .take()
                        .ok_or_else(|| conflict("当前调用没有真人提交材料"))?;
                    session.taken = true;
                    return Ok(input);
                }
                let remaining = (session.expires_at - now_ms()).max(0) as u64;
                let origins = declaration(&provider)
                    .map(|declaration| declaration.origins(&provider, session.method_index))
                    .unwrap_or_default();
                Ok(json!({
                    "provider": session.provider,
                    "flow": flow,
                    "methodIndex": session.method_index,
                    "interaction": session.expected,
                    "options": session.options,
                    "expiresAt": session.expires_at,
                    "remainingMs": remaining,
                    "authorizationOrigins": origins,
                }))
            }
            "login.step.publish" => {
                let expected = params["expected"]
                    .as_str()
                    .filter(|value| !value.is_empty() && value.len() <= 128)
                    .ok_or_else(|| bad_params("托管登录步骤缺少合法 expected 标识"))?;
                let mut map = sessions()
                    .lock()
                    .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
                prune(&mut map);
                let session = map
                    .get_mut(&flow)
                    .ok_or_else(|| forbidden("当前调用没有托管登录权限"))?;
                check_binding(session, self, &provider)?;
                if session.published || expected != session.expected.as_str() {
                    return Err(conflict("登录步骤已变化或不允许重复发布"));
                }
                validate_published_account(self, params)?;
                let public = public_step(params);
                session.step = Some(public);
                session.published = true;
                Ok(json!({
                    "published": true,
                    "expected": session.expected,
                    "step": session.step,
                }))
            }
            "login.loopback.open" => {
                self.permission(LOOPBACK_CAPABILITY)?;
                let profile = params["profile"]
                    .as_str()
                    .filter(|value| !value.is_empty() && value.len() <= 128)
                    .ok_or_else(|| bad_params("回调监听缺少 profile 标识"))?;
                let mut map = sessions()
                    .lock()
                    .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
                prune(&mut map);
                let session = map
                    .get_mut(&flow)
                    .ok_or_else(|| forbidden("当前调用没有托管登录权限"))?;
                check_binding(session, self, &provider)?;
                let declaration = declaration(&provider)
                    .ok_or_else(|| forbidden("插件未声明托管登录回调配置"))?;
                declaration
                    .profile(&provider, session.method_index, profile)
                    .ok_or_else(|| forbidden("托管登录回调配置尚未获主程序批准"))?;
                if session.loopback.is_some() {
                    return Err(conflict("当前登录步骤已打开回调监听"));
                }
                // 本机构建没有托管登录回调监听端口（待接线），如实报不支持，
                // 不返回一个打不通的端口。
                Err(WireError::with_code(
                    "本机构建暂未提供托管登录回调监听，请在管理面使用浏览器授权流程",
                    "unsupported",
                ))
            }
            "login.loopback.close" => {
                self.permission(LOOPBACK_CAPABILITY)?;
                let mut map = sessions()
                    .lock()
                    .map_err(|_| WireError::with_code("托管登录会话表不可用", "internal"))?;
                prune(&mut map);
                let session = map
                    .get_mut(&flow)
                    .ok_or_else(|| forbidden("当前调用没有托管登录权限"))?;
                check_binding(session, self, &provider)?;
                let closed = session.loopback.take().is_some();
                Ok(json!({ "closed": closed }))
            }
            _ => Err(WireError::with_code(
                "未知托管登录操作",
                WireError::NO_METHOD,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_rejects_unapproved_profile() {
        let declaration = Declaration {
            methods: vec![0],
            loopback_profiles: vec!["trae.pkce.v1".into()],
            authorization_origins: vec!["https://www.trae.cn".into()],
        };
        // trae 只在 method 0 被批准，这里声明一致 → 通过。
        assert!(declaration.validate("trae").is_ok());
        // 换 provider 后该 profile 不在批准表里。
        assert!(declaration.validate("kiro").is_err());
    }

    #[test]
    fn declaration_requires_exactly_approved_origins() {
        let declaration = Declaration {
            methods: vec![1],
            loopback_profiles: vec!["autoclaw-intl.google.v1".into()],
            authorization_origins: vec!["https://accounts.google.com".into()],
        };
        assert!(declaration.validate("autoclaw-intl").is_ok());
        // 非批准来源（不同主机）必须被拒。
        let bad = Declaration {
            authorization_origins: vec!["https://example.com".into()],
            ..declaration
        };
        assert!(bad.validate("autoclaw-intl").is_err());
    }

    #[test]
    fn issue_then_take_input_is_one_shot() {
        let flow = issue(
            "unit-managed",
            "acc",
            "id",
            0,
            "step-1",
            json!({ "mode": "browser" }),
            60_000,
        )
        .unwrap();
        submit_input(&flow, json!({ "username": "u" })).unwrap();
        {
            let mut map = sessions().lock().unwrap();
            let session = map.get_mut(&flow).unwrap();
            assert!(!session.taken);
            let input = session.input.take().unwrap();
            session.taken = true;
            assert_eq!(input["username"], "u");
            assert!(session.input.is_none());
        }
        revoke_flow(&flow);
        assert!(!sessions().lock().unwrap().contains_key(&flow));
    }

    #[test]
    fn forget_provider_only_drops_its_sessions() {
        let a = issue("unit-a", "acc-a", "", 0, "s", Value::Null, 60_000).unwrap();
        let b = issue("unit-b", "acc-b", "", 0, "s", Value::Null, 60_000).unwrap();
        forget_provider("unit-a");
        let map = sessions().lock().unwrap();
        assert!(!map.contains_key(&a));
        assert!(map.contains_key(&b));
        drop(map);
        revoke_flow(&b);
    }

    #[test]
    fn public_step_strips_secret() {
        let params = json!({
            "expected": "s",
            "account": { "public": { "email": "x" }, "secret": { "token": "t" } }
        });
        let public = public_step(&params);
        assert!(public["account"].get("secret").is_none());
        assert_eq!(public["account"]["public"]["email"], "x");
    }
}
