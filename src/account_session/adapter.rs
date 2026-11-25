//! 账号 → 上游 Bridge 的适配层。
//!
//! 请求构造不重复实现：按 provider 协议族复用既有原生 Bridge
//! （`bridge::openai` / `bridge::anthropic` / `bridge::gemini`）。
//! 本层只负责解析生效地址、解密凭证、取出口 Client。

use std::sync::Arc;

use super::model::{AccountSession, CREDENTIAL_KIND_NONE};
use super::registry::{self, Protocol};
use super::store::AccountSessionStore;
use crate::bridge::{Bridge, BridgeError};

/// Trae 的「网页/控制台」主机 —— **不是** 对话与目录所在的站。
///
/// `api.trae.cn` 只承载 OAuth（`GetLoginGuidance` / `ExchangeToken`）与网页控制台；
/// `/api/agent/v3/llm_utils_chat`（对话）、`/api/ide/v1/get_detail_param`（目录）、
/// `/trae/api/v2/ug/checkin_credits/*`（签到）都只在 Agent 站上存在。
const TRAE_AGENT_HOST: &str = "https://trae-api-cn.mchost.guru";

/// 把 Trae 账号的「网页站」地址归一到 Agent 站。
///
/// **生产实测（2026-11-25）**：7 个 `trae-solo-*` 账号的 `base_url` 被填成
/// `https://api.trae.cn`，于是每一次对话都打到那个站的 `/api/agent/v3/llm_utils_chat`，
/// 由字节 TLB 直接回 `404 <html><center>TLB</center>`；而同一份凭据把地址换成
/// Agent 站后立刻正常出 token（两条链路都实测过）。同一账号的目录/签到也因此
/// 一直在打错的站。
///
/// 只认**官方网页站**这几个精确值（大小写不敏感、带不带尾部 `/` 都算），
/// 自建中转/别的域名一概不动 —— 归一不是"猜一个地址"，而是"把已知打错的那个
/// 站换回上游真正承载这些接口的站"。
fn canonical_trae_agent_base(raw: &str) -> Option<String> {
    let host = raw.trim().trim_end_matches('/');
    let lowered = host.to_ascii_lowercase();
    match lowered.as_str() {
        "https://api.trae.cn"
        | "http://api.trae.cn"
        | "https://www.trae.cn"
        | "http://www.trae.cn"
        | "https://api.trae.com.cn"
        | "http://api.trae.com.cn" => Some(TRAE_AGENT_HOST.to_string()),
        _ => None,
    }
}

/// 生效的上游地址：账号级地址优先，空则用 provider 目录默认值。
pub fn effective_base_url(session: &AccountSession) -> Result<String, BridgeError> {
    let raw = session.base_url.trim();
    if !raw.is_empty() {
        // Trae：把网页站归一成 Agent 站（见 [`canonical_trae_agent_base`]）。
        if session.provider == "trae" {
            if let Some(base) = canonical_trae_agent_base(raw) {
                return Ok(base);
            }
        }
        return Ok(raw.to_string());
    }
    let spec = registry::find(&session.provider).ok_or_else(|| {
        BridgeError::Config(format!(
            "unknown account provider '{}' for account {}",
            session.provider, session.id
        ))
    })?;
    if spec.default_base_url.is_empty() {
        return Err(BridgeError::Config(format!(
            "account provider '{}' requires an explicit base_url",
            spec.kind
        )));
    }
    Ok(spec.default_base_url.to_string())
}

/// OfficeAce 的 Basic 鉴权：`Basic base64(modelAppKey:modelAppSecret)`。
///
/// 注册表里的 `authorization: Basic {credential}` 只能填主密钥
/// （导入后是 `modelAppKey`），**缺 `modelAppSecret` 一半** → 上游 400。
/// 聊天与模型目录都要按上游契约重拼（目录侧 `catalog.rs` 复用本函数）。
/// 任一半缺失返回 `None`（不伪造空 secret 的 Basic）。
pub(crate) fn officeace_basic_authorization(session: &AccountSession) -> Option<String> {
    use base64::Engine;
    let key = session.credential.secret_plain();
    let secret = session.credential.field_plain("modelAppSecret");
    let key = key.trim();
    let secret = secret.trim();
    if key.is_empty() || secret.is_empty() {
        return None;
    }
    let raw = format!("{key}:{secret}");
    Some(format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
    ))
}

/// 合并请求头：provider 默认头 + Cline 伪装头覆盖表 + 账号级额外头
/// （同名以账号级为准；覆盖表只对 `cline*` 渠道生效，空串 = 不发这个头）。
fn session_headers(
    session: &AccountSession,
    meta: &registry::SessionMeta,
) -> Vec<(String, String)> {
    let merged: Vec<(String, String)> = meta
        .headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    // 先叠全局覆盖表（用户可改的伪装头），再叠账号级额外头：后者更具体，优先级更高。
    let mut merged = crate::config::apply_cline_header_overrides(&session.provider, merged);
    for (name, value) in &session.extra_headers {
        let lower = name.to_ascii_lowercase();
        merged.retain(|(existing, _)| existing.to_ascii_lowercase() != lower);
        merged.push((name.clone(), value.clone()));
    }
    // OfficeAce：Authorization 必须是 `Basic base64(modelAppKey:modelAppSecret)`；
    // 注册表模板 `Basic {credential}` 只含 modelAppKey 一半 → 上游 400。
    if session.provider == "officeace" {
        if let Some(basic) = officeace_basic_authorization(session) {
            merged.retain(|(name, _)| !name.eq_ignore_ascii_case("authorization"));
            merged.push(("Authorization".to_string(), basic));
        }
    }
    merged
}

/// 生效的对话路径：账号级路径优先，空则回落 provider 默认（再空由 Bridge 补 `/chat/completions`）。
fn session_chat_path(session: &AccountSession, meta: &registry::SessionMeta) -> Option<String> {
    session
        .chat_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let path = meta.chat_path.trim();
            (!path.is_empty()).then(|| path.to_string())
        })
}

/// 按账号凭证构造上游 Bridge（按账号代理选出口 Client）。
pub fn build_bridge(
    session: &AccountSession,
    sessions: &AccountSessionStore,
) -> Result<Arc<dyn Bridge>, BridgeError> {
    build_bridge_with_client(session, sessions, None)
}

/// 与 [`build_bridge`] 相同，但可用 `client_override` 强制出口 Client
/// （模型测试临时指定「直连 / 代理池 / 指定节点」时用；`None` = 沿用账号代理）。
pub fn build_bridge_with_client(
    session: &AccountSession,
    sessions: &AccountSessionStore,
    client_override: Option<Arc<reqwest::Client>>,
) -> Result<Arc<dyn Bridge>, BridgeError> {
    let spec = registry::find(&session.provider).ok_or_else(|| {
        BridgeError::Config(format!("unknown account provider '{}'", session.provider))
    })?;
    let base = effective_base_url(session)?;
    // `kind = none` = 免鉴权上游（IP 白名单等），此时不发送任何鉴权头
    let secret = if session.credential.kind == CREDENTIAL_KIND_NONE {
        String::new()
    } else {
        session.credential.secret_plain()
    };
    let client = match client_override {
        Some(client) => client,
        None => sessions.client_for(session)?,
    };
    let bridge: Arc<dyn Bridge> = match spec.protocol {
        Protocol::Openai => {
            // 登录态型供应商：Cookie / JWT + 专用请求头（不走 Bearer API Key）
            if let Some(meta) = registry::session_meta(spec.kind) {
                // 私有协议供应商（请求体/响应帧非 OpenAI 方言）走专用 Bridge
                if !meta.openai_compatible {
                    // 用户常从抓包整段复制，先剥掉 `Authorization:` / `Bearer` 这类前缀，
                    // 避免和 provider 默认头重复拼接。
                    let credential = crate::bridge::session::normalize_credential(
                        &secret,
                        &["authorization:", "cloud-ide-jwt", "bearer"],
                    );
                    let mut headers = session_headers(session, meta);
                    for (_, value) in headers.iter_mut() {
                        *value = value.replace("{credential}", credential);
                    }
                    // 私有协议的身份不只在 token 里：Trae 的凭证还带
                    // `uid` / `deviceId` / `machineId` / `variant`（见 `import.rs` 里
                    // A2A 记录的真实形状）。前三者对应 `x-uid` / `x-device-id` /
                    // `x-machine-id` 三个身份头，`variant` 决定 SOLO 还是 IDE 两套 UA。
                    // 早先这里只传 token，这些头一个都发不出去 —— 上游因此把请求
                    // 判成「非法客户端」（`11-128 request illegal`）或「参数不对」
                    // （`[4001] the param is invalid`）。
                    // 账号里显式配了同名头的以账号为准，不覆盖。
                    if spec.kind == "trae" {
                        for (field, header) in [
                            ("uid", "x-uid"),
                            ("deviceId", "x-device-id"),
                            ("machineId", "x-machine-id"),
                            ("variant", "x-aigx-trae-variant"),
                        ] {
                            let value = session.credential.field_plain(field);
                            if value.trim().is_empty() {
                                continue;
                            }
                            if !headers
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case(header))
                            {
                                headers.push((header.to_string(), value));
                            }
                        }
                    }
                    if let Some(bridge) = crate::bridge::session::build(
                        spec.kind,
                        &base,
                        credential,
                        headers,
                        client.as_ref().clone(),
                    ) {
                        return Ok(bridge);
                    }
                }
                let auth_mode = match meta.auth {
                    registry::SessionAuth::Bearer => crate::bridge::openai::AuthMode::Bearer,
                    registry::SessionAuth::Cookie => crate::bridge::openai::AuthMode::Cookie,
                    registry::SessionAuth::Headers => crate::bridge::openai::AuthMode::Headers,
                };
                // 账号级对话路径优先，空则回落到 provider 默认路径
                let chat_path = session_chat_path(session, meta);
                Arc::new(
                    crate::bridge::openai::OpenaiCompatibleBridge::with_session_auth(
                        &base,
                        &secret,
                        client.as_ref().clone(),
                        auth_mode,
                        session_headers(session, meta),
                        chat_path,
                    )
                    // 只收流式的家（workbuddy）：非流式入口改发流式再聚合
                    .with_force_stream_aggregate(registry::requires_stream(spec.kind)),
                )
            } else {
                crate::bridge::openai::make_bridge(&base, &secret, &client)
            }
        }
        Protocol::Anthropic => crate::bridge::anthropic::make_bridge(&base, &secret, &client),
        Protocol::Gemini => crate::bridge::gemini::make_bridge(&base, &secret, &client),
    };
    Ok(bridge)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::model::AccountSession;

    #[test]
    fn explicit_base_url_wins() {
        let mut session = AccountSession::new("ch", "acct");
        session.base_url = "https://example.com/v1".to_string();
        assert_eq!(
            effective_base_url(&session).unwrap(),
            "https://example.com/v1"
        );
    }

    #[test]
    fn provider_default_applies_when_base_url_empty() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "deepseek".to_string();
        assert_eq!(
            effective_base_url(&session).unwrap(),
            "https://api.deepseek.com"
        );
    }

    #[test]
    fn openai_compatible_requires_explicit_base_url() {
        let session = AccountSession::new("ch", "acct");
        assert!(effective_base_url(&session).is_err());
    }

    /// Trae 账号把网页站写成上游地址时，生效地址必须是 Agent 站。
    ///
    /// 生产实测：7 个 `trae-solo-*` 账号 `base_url = https://api.trae.cn`，
    /// 对话固定回 TLB 404（同一凭据换 Agent 站立刻正常）。
    #[test]
    fn trae_web_host_is_normalized_to_the_agent_host() {
        for raw in [
            "https://api.trae.cn",
            "https://api.trae.cn/",
            "HTTPS://API.TRAE.CN",
            "https://www.trae.cn",
            "https://api.trae.com.cn",
        ] {
            let mut session = AccountSession::new("ch", "acct");
            session.provider = "trae".to_string();
            session.base_url = raw.to_string();
            assert_eq!(
                effective_base_url(&session).unwrap(),
                "https://trae-api-cn.mchost.guru",
                "raw={raw}"
            );
        }
    }

    /// 归一只认官方网页站的精确值：自建中转 / 别的域名必须原样透传。
    ///
    /// 反向对照 —— 若把归一写成"含 trae.cn 就换"，中转网关会被悄悄改地址，
    /// 那比 404 更难查。
    #[test]
    fn trae_self_hosted_and_other_base_urls_are_left_alone() {
        for raw in [
            "https://my-trae-proxy.example.com",
            "https://api.trae.cn:8443",
            "https://api.trae.cn/v1",
            "https://trae-api-cn.mchost.guru",
        ] {
            let mut session = AccountSession::new("ch", "acct");
            session.provider = "trae".to_string();
            session.base_url = raw.to_string();
            assert_eq!(effective_base_url(&session).unwrap(), raw, "raw={raw}");
        }
    }

    /// 非 trae 家即使用同一个网页站地址也不改（归一挂在 provider 上，不是挂在 URL 上）。
    #[test]
    fn other_providers_keep_their_own_base_url() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "deepseek".to_string();
        session.base_url = "https://api.trae.cn".to_string();
        assert_eq!(effective_base_url(&session).unwrap(), "https://api.trae.cn");
    }

    #[test]
    fn session_chat_path_falls_back_to_provider_default() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "workbuddy".to_string();
        let meta = registry::session_meta("workbuddy").unwrap();
        assert_eq!(
            session_chat_path(&session, meta).as_deref(),
            Some("/v2/chat/completions")
        );

        session.chat_path = Some("/custom/path".to_string());
        assert_eq!(
            session_chat_path(&session, meta).as_deref(),
            Some("/custom/path")
        );

        let zcode = registry::session_meta("zcode").unwrap();
        session.provider = "zcode".to_string();
        session.chat_path = None;
        assert!(session_chat_path(&session, zcode).is_none());
    }

    #[test]
    fn account_headers_override_provider_defaults_case_insensitively() {
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "qoder".to_string();
        session
            .extra_headers
            .insert("User-Agent".to_string(), "custom/1.0".to_string());
        session
            .extra_headers
            .insert("x-token".to_string(), "{credential}".to_string());
        let meta = registry::session_meta("qoder").unwrap();
        let merged = session_headers(&session, meta);
        let ua: Vec<&(String, String)> = merged
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
            .collect();
        assert_eq!(ua.len(), 1, "同名请求头应只保留账号级一条");
        assert_eq!(ua[0].1, "custom/1.0");
    }

    #[test]
    fn admin_overrides_apply_to_cline_and_only_cline() {
        // 快照为空时：Cline 默认头原样下发
        crate::config::set_cline_header_overrides(Vec::new());
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "cline-free".to_string();
        let cline_meta = registry::session_meta("cline-free").unwrap();
        let before = session_headers(&session, cline_meta);
        assert!(before
            .iter()
            .any(|(name, value)| name == "user-agent" && value == "Cline/3.0.62"));

        // 写入覆盖表后：Cline 拿到覆盖值
        crate::config::set_cline_header_overrides(vec![
            ("User-Agent".to_string(), "Cline/9.9.9".to_string()),
            ("x-title".to_string(), String::new()),
        ]);
        let after = session_headers(&session, cline_meta);
        assert!(
            after
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("user-agent")
                    && value == "Cline/9.9.9")
        );
        assert!(!after.iter().any(|(name, _)| name == "x-title"));

        // 非 cline 渠道不受覆盖表影响
        let mut other = AccountSession::new("ch", "acct2");
        other.provider = "qoder".to_string();
        let qoder_meta = registry::session_meta("qoder").unwrap();
        let untouched = session_headers(&other, qoder_meta);
        assert!(!untouched.iter().any(|(_, value)| value == "Cline/9.9.9"));

        // 恢复空表，避免影响其它用例
        crate::config::set_cline_header_overrides(Vec::new());
    }

    #[test]
    fn officeace_chat_authorization_is_basic_key_secret() {
        // chat 与目录同源：`Authorization: Basic base64(modelAppKey:modelAppSecret)`，
        // 不是 registry 模板里只含 secret 一半的 `Basic {credential}`（那会 400）。
        use base64::Engine;
        let mut session = AccountSession::new("ch", "acct");
        session.provider = "officeace".to_string();
        session.credential.secret = "APPKEY".to_string();
        session.credential.set_field("modelAppSecret", "APPSECRET");
        let meta = registry::session_meta("officeace").unwrap();
        let headers = session_headers(&session, meta);
        let auth: Vec<&String> = headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
            .map(|(_, value)| value)
            .collect();
        assert_eq!(auth.len(), 1, "只保留一条 Authorization");
        let want = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(b"APPKEY:APPSECRET")
        );
        assert_eq!(auth[0], &want);
        // 缺 modelAppSecret 时不伪造（保留模板，让上游如实报错）
        session.credential.set_field("modelAppSecret", "");
        assert!(officeace_basic_authorization(&session).is_none());
    }
}
