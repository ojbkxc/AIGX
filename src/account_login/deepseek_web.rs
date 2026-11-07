//! DeepSeek 网页版（`chat.deepseek.com`）的**粘贴式登录**校验。
//!
//! 上游没有可代跑的网页登录（登录态在浏览器 localStorage 的 `userToken`），
//! 网关能做的是：用户自己登录后把 userToken 粘进来，网关**归一化**（剥掉
//! `Bearer` / `userToken=` / JSON 包装）并**校验形态**（长度 + 拒绝 `sk-`
//! 开头的收费 API Key），通过后落账号。
//!
//! 校验口径（对齐上游 `add-provider-deepseek.ts` 与 `deepseek_login.rs` 的
//! 壳侧采集规则）：userToken 来自 `localStorage.userToken` 的 JSON `.value`
//! 字段，最长 16 KB、不含换行；**不接受 `sk-` 前缀的付费 API Key**。
//! 不做上游探活：deepseek-web 没有「无副作用」的只读校验端点
//! （建会话会真实占资源），与 OrcaTerm 的 best-effort 同口径 —— 真实
//! 有效性留给首次转发暴露。
//!
//! ── 与登录任务表的关系 ────────────────────────────────────
//! 本模块**不**创建 `LoginFlow` 任务（没有轮询/回调可言），直接返回
//! `Credentials`，由调用方走 `save_credentials` 落账号。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use serde_json::Value;

use super::Credentials;

/// 粘贴的凭证长度上限（上游壳侧采集的上限就是 16 KB）。
const MAX_SECRET_LENGTH: usize = 16 * 1024;

/// 归一化并校验 DeepSeek 网页版登录态。
///
/// 接受三种形态（与上游壳侧采集一致）：
/// 1. 裸 userToken（`localStorage.userToken` 解出的 `.value`）；
/// 2. 整段 JSON（`{"value": "…"}` 或 `{"userToken": {"value": "…"}}`）；
/// 3. 整段 Cookie（`userToken=…; …`，按名提取；其余键原样保留作可选 Cookie）。
pub async fn verify_deepseek_web(raw: &str) -> Result<Credentials, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("请粘贴 DeepSeek 网页版的 userToken".to_string());
    }
    if text.chars().count() > MAX_SECRET_LENGTH {
        return Err("DeepSeek 网页版的凭证过长，请确认粘贴的是 userToken（≤16KB）".to_string());
    }
    // JSON 包装形态：取 .value（或 userToken.value）
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        let token = value
            .get("value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        if let Some(token) = token {
            return finish(token, None);
        }
        let token = value
            .pointer("/userToken/value")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string);
        if let Some(token) = token {
            return finish(token, None);
        }
    }
    // Cookie / 包装前缀形态
    let mut cookie: Option<String> = None;
    let mut token = String::new();
    let mut body = text;
    for prefix in ["Cookie:", "cookie:"] {
        if let Some(rest) = body.strip_prefix(prefix) {
            body = rest.trim();
        }
    }
    if body.contains(';') || body.contains('=') {
        // Cookie 形态：userToken 键优先取值；整段保留作可选 Cookie 头
        cookie = Some(body.to_string());
        for part in body.split(';') {
            let part = part.trim();
            if let Some(value) = part
                .strip_prefix("userToken")
                .and_then(|v| v.strip_prefix('='))
            {
                token = value.trim().trim_matches('"').to_string();
                break;
            }
        }
        if token.is_empty() {
            // 没有 userToken 键的整段 Cookie：首个非空值当 token（尽力而为）
            token = body.to_string();
            cookie = None;
        }
    } else {
        for prefix in ["Bearer ", "bearer ", "accessToken="] {
            if let Some(rest) = body.strip_prefix(prefix) {
                body = rest.trim();
            }
        }
        token = body.trim_matches('"').to_string();
    }
    finish(token, cookie)
}

fn finish(token: String, cookie: Option<String>) -> Result<Credentials, String> {
    if token.is_empty() {
        return Err(
            "请粘贴 DeepSeek 网页版的 userToken（localStorage.userToken 的 .value）".to_string(),
        );
    }
    if token.starts_with("sk-") {
        return Err(
            "这是 DeepSeek 的付费 API Key（sk- 开头）：网页版不接受，请粘贴 userToken（或用 \
             DeepSeek 渠道直连 API）"
                .to_string(),
        );
    }
    if token.chars().count() < 20 {
        return Err("DeepSeek 网页版的令牌过短，请粘贴完整的 userToken".to_string());
    }
    let mut extra_headers = HashMap::new();
    if let Some(cookie) = cookie {
        extra_headers.insert("cookie".to_string(), cookie);
    }
    Ok(Credentials {
        name: "DeepSeek Web".to_string(),
        secret: token,
        base_url: None,
        extra_headers,
        expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_bare_token() {
        let creds = verify_deepseek_web("a".repeat(40).as_str())
            .await
            .expect("裸 token");
        assert_eq!(creds.name, "DeepSeek Web");
        assert_eq!(creds.secret, "a".repeat(40));
        assert!(!creds.extra_headers.contains_key("cookie"));
    }

    #[tokio::test]
    async fn accepts_json_value_wrapper() {
        let raw = r#"{"value":"token-value-1234567890abc"}"#;
        let creds = verify_deepseek_web(raw).await.expect("JSON 包装");
        assert_eq!(creds.secret, "token-value-1234567890abc");
    }

    #[tokio::test]
    async fn cookie_form_extracts_user_token_and_keeps_cookie() {
        let raw = "Cookie: other=1; userToken=token-value-1234567890; more=2";
        let creds = verify_deepseek_web(raw).await.expect("Cookie 形态");
        assert_eq!(creds.secret, "token-value-1234567890");
        assert_eq!(
            creds.extra_headers.get("cookie").map(String::as_str),
            Some("other=1; userToken=token-value-1234567890; more=2")
        );
    }

    #[tokio::test]
    async fn rejects_paid_api_key_prefix() {
        let error = verify_deepseek_web(&format!("sk-{}", "a".repeat(40)))
            .await
            .unwrap_err();
        assert!(error.contains("sk-"), "实际：{error}");
    }

    #[tokio::test]
    async fn rejects_empty_and_short() {
        assert!(verify_deepseek_web("").await.is_err());
        assert!(verify_deepseek_web("short").await.is_err());
    }
}
