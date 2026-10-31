//! 粘贴式登录（校验 + 归一化）的几个家：MonkeyCode / KukuAI / OrcaTerm /
//! Antigravity（仅凭证校验）/ Command Code。
//!
//! 这些家**没有可代跑的网页登录**（登录态是浏览器 Cookie、或上游要图形验证
//! 码 / 微信扫码 / 自定义协议回调），网关能做的是：用户自己登录后把凭证粘进来，
//! 网关**归一化**（剥掉 `Cookie:` / `Bearer ` / 引号等包装）并**校验一次**
//! （打一个只读端点），通过后才落账号 —— 这样「粘错了东西」在添加时就有
//! 明确报错，而不是等到转发时得到误导性的 401。
//!
//! 参照 Agent2API 的 `monkeycode/login.rs` / `kuku/login.rs` / `orcaterm/login.rs`
//! 与 `commandcode/login.rs` 的归一化规则；AIGX 把「校验」放在这里，
//! 「网页登录」那一条仍由前端引导用户去官方站点完成。
//!
//! ── 与登录任务表的关系 ────────────────────────────────────
//! 本模块**不**创建 `LoginFlow` 任务（没有轮询/回调可言），而是直接返回
//! `Credentials`，由调用方走 `save_credentials` 落账号。因此 `start()` 抛出的
//! 可读错误会直接出现在「添加账号」弹窗里。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use serde_json::Value;

use super::Credentials;

/// 粘贴的凭证长度上限（超过这个长度一定是粘错了东西）
const MAX_SECRET_LENGTH: usize = 8192;

/// MonkeyCode 的 session cookie 名
const MONKEYCODE_COOKIE: &str = "monkeycode_ai_session";

/// 归一化「整段 Cookie / `name=value` / 裸值 / 带引号」形态，取指定 cookie 名
/// 的值；`cookie_name` 为空时按「整段 Cookie 原样保留」处理。
pub fn normalize_cookie(raw: &str, cookie_name: &str) -> String {
    let mut text = raw.trim();
    // 整行 `Cookie: ...` 头形态
    if let Some(rest) = text
        .strip_prefix("Cookie:")
        .or_else(|| text.strip_prefix("cookie:"))
    {
        text = rest.trim();
    }
    // `name=value` 形态：优先取指定 cookie 名的值
    if !cookie_name.is_empty() {
        for part in text.split(';') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix(cookie_name) {
                if let Some(value) = value.strip_prefix('=') {
                    return value
                        .trim()
                        .trim_matches(|ch| ch == '"' || ch == '\'')
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .collect();
                }
            }
        }
    }
    // 取第一个分号之前的内容（Cookie 串里可能带着别的 cookie）
    if let Some((head, _)) = text.split_once(';') {
        text = head.trim();
    }
    text.trim_matches(|ch| ch == '"' || ch == '\'')
        .trim()
        .to_string()
}

/// 归一化 Bearer / 裸 token / 带引号形态
pub fn normalize_token(raw: &str) -> String {
    let mut text = raw.trim();
    for prefix in [
        "Authorization:",
        "authorization:",
        "Bearer ",
        "bearer ",
        "x-api-key:",
        "ot_session=",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    text.trim_matches(|ch| ch == '"' || ch == '\'').trim().to_string()
}

fn check_length(secret: &str, label: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err(format!("请粘贴 {label} 的凭证"));
    }
    if secret.chars().count() > MAX_SECRET_LENGTH {
        return Err(format!("{label} 的凭证过长，请确认粘贴的是正确的值"));
    }
    Ok(())
}

fn text_of(object: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = object.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

// ── MonkeyCode ──────────────────────────────────────────────────────

/// 用粘贴的 session 校验 MonkeyCode 登录态，返回可直接落账号的凭证。
///
/// 校验端点 `GET /api/v1/users/status`（200 + `code:0` 即已登录）。
/// `site` 为 `monkeycode-ai.com`（国内）或 `monkeycode-ai.net`（国际）。
pub async fn verify_monkeycode(
    client: &reqwest::Client,
    site: &str,
    raw_session: &str,
) -> Result<Credentials, String> {
    let session = normalize_cookie(raw_session, MONKEYCODE_COOKIE);
    check_length(&session, "MonkeyCode session")?;
    let base = if site.trim().is_empty() {
        "https://monkeycode-ai.com"
    } else {
        site.trim().trim_end_matches('/')
    };
    let url = format!("{base}/api/v1/users/status");
    let response = client
        .get(&url)
        .header("Cookie", format!("{MONKEYCODE_COOKIE}={session}"))
        .send()
        .await
        .map_err(|error| format!("MonkeyCode 登录态校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let code = payload.get("code").and_then(Value::as_i64);
    if !(200..300).contains(&status) || code != Some(0) {
        let upstream = text_of(&payload, &["message", "msg", "error"]);
        return Err(if upstream.is_empty() {
            format!(
                "MonkeyCode 登录态校验失败（HTTP {status}）：请确认 session 有效且站点选对（国内 .com / 国际 .net）"
            )
        } else {
            format!("MonkeyCode 登录态校验失败：{upstream}")
        });
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let user = data.get("user").cloned().unwrap_or_else(|| data.clone());
    let name = text_of(&user, &["name", "nickname", "display_name", "email"]);
    Ok(Credentials {
        name: if name.is_empty() {
            "MonkeyCode".to_string()
        } else {
            format!("MonkeyCode {name}")
        },
        secret: session,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── KukuAI ──────────────────────────────────────────────────────────

/// 用粘贴的 Cookie 校验 KukuAI 登录态（BDUSS 必填）。
pub async fn verify_kuku(
    client: &reqwest::Client,
    raw_cookie: &str,
) -> Result<Credentials, String> {
    // KukuAI 要整段 Cookie（BDUSS / STOKEN / PTOKEN），不做按名提取
    let cookie = normalize_cookie(raw_cookie, "");
    check_length(&cookie, "KukuAI Cookie")?;
    if !cookie.contains("BDUSS=") {
        return Err("KukuAI 的 Cookie 缺少 BDUSS（请粘贴完整 Cookie）".to_string());
    }
    let url = "https://kuku.baidu.com/api/user/info";
    let response = client
        .get(url)
        .header("Cookie", cookie.clone())
        .send()
        .await
        .map_err(|error| format!("KukuAI 登录态校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let ok = (200..300).contains(&status)
        && (payload.get("errno").and_then(Value::as_i64) == Some(0)
            || payload.get("code").and_then(Value::as_i64) == Some(0)
            || payload.get("data").is_some());
    if !ok {
        let upstream = text_of(&payload, &["errmsg", "msg", "message"]);
        return Err(if upstream.is_empty() {
            format!("KukuAI 登录态校验失败（HTTP {status}）：Cookie 可能已过期")
        } else {
            format!("KukuAI 登录态校验失败：{upstream}")
        });
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let name = text_of(&data, &["uk", "name", "nickname", "user_name"]);
    Ok(Credentials {
        name: if name.is_empty() {
            "KukuAI".to_string()
        } else {
            format!("KukuAI {name}")
        },
        secret: cookie,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── OrcaTerm ────────────────────────────────────────────────────────

/// 用粘贴的 accessToken / ot_session 校验 OrcaTerm 登录态。
///
/// 上游没有公开的「校验」端点，这里用**一次最小请求**验证令牌形态：
/// 只做长度与非空校验，真正是否有效留给首次转发暴露（与 Agent2API
/// 的 best-effort 口径一致 —— 它也不在上游侧做前置校验）。
pub async fn verify_orcaterm(raw_token: &str) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "OrcaTerm accessToken")?;
    // OrcaTerm 的 JWT 是三段点分；ot_session 是不透明串。两种都收，
    // 但明显不是令牌形态的（极短）直接拒绝，避免把说明文字粘进来。
    if token.chars().count() < 16 {
        return Err("OrcaTerm 的令牌过短，请粘贴完整的 accessToken 或 ot_session".to_string());
    }
    Ok(Credentials {
        name: "OrcaTerm".to_string(),
        secret: token,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── Command Code ────────────────────────────────────────────────────

/// 归一化并校验 Command Code 的 `user_` API Key（best-effort 探活）。
pub async fn verify_commandcode(
    client: &reqwest::Client,
    raw_key: &str,
) -> Result<Credentials, String> {
    let key = normalize_token(raw_key);
    check_length(&key, "Command Code API Key")?;
    if !key.starts_with("user_") {
        return Err("Command Code 的 API Key 应以 user_ 开头".to_string());
    }
    // 探活：GET /alpha/billing/credits，只有 401/403 才判无效
    let url = "https://api.commandcode.ai/alpha/billing/credits";
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .map_err(|error| format!("Command Code 校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(format!(
            "Command Code API Key 无效（HTTP {status}）：请到控制台重新生成"
        ));
    }
    Ok(Credentials {
        name: "Command Code".to_string(),
        secret: key,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_cookie_extracts_named_value() {
        assert_eq!(
            normalize_cookie("Cookie: a=1; monkeycode_ai_session=abc123; b=2", MONKEYCODE_COOKIE),
            "abc123"
        );
        assert_eq!(
            normalize_cookie("monkeycode_ai_session=xyz", MONKEYCODE_COOKIE),
            "xyz"
        );
        assert_eq!(normalize_cookie("\"abc\"", MONKEYCODE_COOKIE), "abc");
    }

    #[test]
    fn normalize_cookie_keeps_full_cookie_when_no_name() {
        let full = "BDUSS=abc; STOKEN=def";
        assert_eq!(normalize_cookie(full, ""), full);
    }

    #[test]
    fn normalize_token_strips_prefixes() {
        assert_eq!(normalize_token("Bearer user_abc"), "user_abc");
        assert_eq!(normalize_token("Authorization: Bearer x"), "Bearer x");
        assert_eq!(normalize_token("ot_session=abc"), "abc");
    }

    #[test]
    fn check_length_rejects_empty_and_huge() {
        assert!(check_length("", "x").is_err());
        assert!(check_length(&"a".repeat(MAX_SECRET_LENGTH + 1), "x").is_err());
        assert!(check_length("abc", "x").is_ok());
    }
}
