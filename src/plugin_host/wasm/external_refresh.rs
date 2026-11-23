// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Kiro 外部 IdP 续期（`credential.kiro.external.refresh`）。
//!
//! ## 为什么这条要单独存在
//!
//! 上游 Kiro 账号有三条登录态（`social` / `idc` / `external-idp`）。前两条的续期
//! 走 AWS 自家端点，AIGX 原生 `account_session/refresh.rs` 已覆盖；`external-idp`
//! 的令牌端点是**插件注册时写进凭据里的**（`tokenURL` / `clientId` / `refresh`），
//! 只有插件知道该往哪儿换，原生续期无从推断——所以这条能力必须由插件调用宿主完成。
//!
//! ## 与上游的等价点 / 差异
//!
//! - 端点校验同口径：必须 https:443、无 userinfo / query / fragment、主机含点且不是
//!   IP 字面量（防「拿凭据去换任意内网地址」）。
//! - **一次性 refresh_token 防重复消耗**：换证前先按 `stamp = sha256(端点:refresh)`
//!   在账号级 `plugin_state` 里 CAS 占位；若占位仍是同一个 stamp，说明上次换完
//!   插件没落盘，直接失败（`kiro_external_refresh_unconfirmed`），绝不再烧一次。
//!   占位由插件续期成功后的账号写回间接清掉——宿主不替插件清，避免「写成功一半」。
//! - 换证前后的账号记录指纹必须一致（期间被别的路径改过就失败），与上游 `current()` 同义。
//! - 网络出口复用注入的 [`WasmEgress`]（等价上游 `FrozenHttpClient`）：不新增直连、
//!   不绕过代理、总量 ≤ 128 KiB、整段受 `call_deadline` 约束。
//!
//! 上游用 `reqwest::RequestBuilder::form(...)`；AIGX 的 egress 只吃 `body_b64`，
//! 因此这里自己做 `application/x-www-form-urlencoded` 编码（按 RFC 3986 保留
//! 非保留字符，其余百分号编码）。

use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use base64::Engine as _;

use super::context::Context;
use super::wire::WireError;

/// 单次换证响应的硬上限（令牌 JSON 实际只有几 KB）。
const MAX_RESPONSE_BYTES: usize = 128 * 1024;
/// 整段换证的时限上界（外层还有 `call_deadline` 兜底）。
const REFRESH_TIMEOUT: Duration = Duration::from_secs(30);
/// CAS 占位键（账号级）。
const STATE_KEY: &str = "host.kiro-external-refresh.v1";

fn unavailable() -> WireError {
    WireError::with_code(
        "Kiro 外部身份续期不可用：凭据缺少端点/客户端或上次结果未确认，请重新登录",
        "kiro_external_refresh_unavailable",
    )
}

fn unconfirmed() -> WireError {
    WireError::with_code(
        "上次外部授权续期结果未确认，请重新登录；不会重复消耗原凭据",
        "kiro_external_refresh_unconfirmed",
    )
}

fn changed() -> WireError {
    WireError::with_code(
        "本机账号或登录文件已经变化，请重新登录官方客户端",
        "kiro_external_refresh_changed",
    )
}

/// 取第一个非空字符串字段（上游 `text` 同口径）。
fn text(value: &Value, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| value[*key].as_str().filter(|item| !item.is_empty()))
        .unwrap_or_default()
        .to_string()
}

/// 凭据可能是历史形态的 JSON 字符串（旧导出），统一解成对象再读。
fn secret_of(record: &Value) -> Value {
    match &record["secret"] {
        Value::String(raw) if raw.len() <= 256 * 1024 => serde_json::from_str(raw)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| record["secret"].clone()),
        other => other.clone(),
    }
}

/// 端点校验（与上游同口径）：https:443、无凭据/查询/片段、主机必须是域名。
fn endpoint(secret: &Value) -> Result<String, WireError> {
    let raw = text(
        secret,
        &["tokenURL", "tokenUrl", "tokenEndpoint", "token_endpoint"],
    );
    let url = url::Url::parse(&raw).map_err(|_| unavailable())?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url
            .host_str()
            .is_none_or(|host| host.parse::<std::net::IpAddr>().is_ok() || !host.contains('.'))
    {
        return Err(unavailable());
    }
    Ok(url.to_string())
}

/// `application/x-www-form-urlencoded` 编码（RFC 3986：非保留字符原样，其余 %XX）。
fn form_encode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (index, (name, value)) in pairs.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        for (part_index, part) in [name, value].iter().enumerate() {
            if part_index > 0 {
                out.push('=');
            }
            for byte in part.as_bytes() {
                let keep =
                    byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'.' | b'_' | b'~');
                if keep {
                    out.push(*byte as char);
                } else {
                    out.push_str(&format!("%{byte:02X}"));
                }
            }
        }
    }
    out
}

fn stamp_of(endpoint: &str, refresh: &str) -> String {
    format!("{:x}", Sha256::digest(format!("{endpoint}:{refresh}")))
}

/// 用注入的 egress 发一次 POST，读完正文（≤ [`MAX_RESPONSE_BYTES`]）后关闭会话。
async fn post_json(
    context: &Context,
    url: &str,
    form: String,
    deadline: tokio::time::Instant,
) -> Result<(u16, Value), WireError> {
    let opened = tokio::time::timeout_at(
        deadline,
        context.shared.egress.request(json!({
            "url": url,
            "method": "POST",
            "headers": {
                "content-type": "application/x-www-form-urlencoded",
                "accept": "application/json",
            },
            "body_b64": base64::engine::general_purpose::STANDARD.encode(form.as_bytes()),
        })),
    )
    .await
    .map_err(|_| unavailable())??;
    let session = opened["session"]
        .as_str()
        .filter(|session| !session.is_empty())
        .ok_or_else(unavailable)?
        .to_string();
    let status = opened["status"].as_u64().unwrap_or(0) as u16;

    let mut body = Vec::new();
    let result = async {
        loop {
            let frame = tokio::time::timeout_at(deadline, context.shared.egress.read(&session))
                .await
                .map_err(|_| unavailable())??;
            if frame["done"].as_bool().unwrap_or(false) {
                break;
            }
            let chunk = frame["chunk_b64"].as_str().unwrap_or("");
            if chunk.is_empty() {
                return Err(unavailable());
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(chunk)
                .map_err(|_| unavailable())?;
            if body.len().saturating_add(bytes.len()) > MAX_RESPONSE_BYTES {
                return Err(unavailable());
            }
            body.extend_from_slice(&bytes);
        }
        Ok::<(), WireError>(())
    }
    .await;
    // 无论成败都关会话：失败的半截连接不能留在宿主会话表里。
    let _ = context.shared.egress.close(&session).await;
    result?;

    let value: Value = serde_json::from_slice(&body).map_err(|_| unavailable())?;
    Ok((status, value))
}

/// `credential.kiro.external.refresh` 入口：零参数，返回新的 access/refresh/到期。
pub(super) async fn invoke(context: &mut Context, params: &Value) -> Result<Value, WireError> {
    context.permission("credential.kiro-external-refresh.v1")?;
    let provider = context.shared.manifest.provider.clone();
    if provider != "kiro" || params.as_object().is_none_or(|fields| !fields.is_empty()) {
        return Err(unavailable());
    }
    let account = context.account()?.to_string();
    let record = context
        .shared
        .accounts
        .store_get(&provider, &account)
        .await?;
    let secret = secret_of(&record);
    if text(&secret, &["method"]) != "external-idp" {
        return Err(unavailable());
    }
    let endpoint = endpoint(&secret)?;
    // 与上游 `protocols.rs` 一致：端点仍要过清单 origin 白名单。
    // 少了这一步，凭据里写什么域就能往什么域发请求（token 泄漏面）。
    context.shared.manifest.check_url(&endpoint, false)?;
    let client_id = text(&secret, &["clientId", "client_id"]);
    let refresh = text(&secret, &["refresh", "refreshToken", "refresh_token"]);
    if client_id.is_empty() || refresh.is_empty() {
        return Err(unavailable());
    }

    // ── 一次性凭据防重：先占位，再换证 ──────────────────────────────
    let stamp = stamp_of(&endpoint, &refresh);
    let state = context
        .shared
        .accounts
        .plugin_state(&provider, &account, STATE_KEY)
        .await?;
    if state["value"]["stamp"].as_str() == Some(stamp.as_str()) {
        return Err(unconfirmed());
    }
    let revision = state["revision"].as_u64().ok_or_else(unavailable)?;
    let reserved = context
        .shared
        .accounts
        .plugin_state_compare_exchange(
            &provider,
            &account,
            STATE_KEY,
            revision,
            1,
            json!({ "stamp": stamp }),
        )
        .await?;
    if reserved["updated"].as_bool() != Some(true) {
        return Err(changed());
    }

    let deadline = context
        .call_deadline
        .min(tokio::time::Instant::now() + REFRESH_TIMEOUT);
    let form = form_encode(&[
        ("grant_type", "refresh_token"),
        ("client_id", client_id.as_str()),
        ("refresh_token", refresh.as_str()),
    ]);
    let (status, value) = post_json(context, &endpoint, form, deadline).await?;
    if !(200..300).contains(&status) || value.get("error").is_some_and(|error| !error.is_null()) {
        return Err(unavailable());
    }
    let access = text(&value, &["access_token"]);
    if access.is_empty() || access.len() > 8192 || access.chars().any(char::is_control) {
        return Err(unavailable());
    }
    let next_refresh = text(&value, &["refresh_token"]);
    let expires = value["expires_in"]
        .as_i64()
        .filter(|value| *value > 0)
        .and_then(|value| value.checked_mul(1000))
        .and_then(|value| super::now_ms().checked_add(value));

    // ── 换证期间账号被改过就作废结果（同上游 `current()`）────────────
    let after = context
        .shared
        .accounts
        .store_get(&provider, &account)
        .await?;
    if secret_of(&after) != secret {
        return Err(changed());
    }

    Ok(json!({
        "accessToken": access,
        "refreshToken": if next_refresh.is_empty() { refresh } else { next_refresh },
        "expiresAt": expires,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_requires_https_443_without_extras() {
        let ok = json!({"tokenURL": "https://idp.example.com/oauth/token"});
        assert_eq!(
            endpoint(&ok).ok().as_deref(),
            Some("https://idp.example.com/oauth/token")
        );
        for bad in [
            json!({"tokenURL": "http://idp.example.com/token"}),
            json!({"tokenURL": "https://idp.example.com:8443/token"}),
            json!({"tokenURL": "https://user:pass@idp.example.com/token"}),
            json!({"tokenURL": "https://idp.example.com/token?x=1"}),
            json!({"tokenURL": "https://idp.example.com/token#f"}),
            json!({"tokenURL": "https://127.0.0.1/token"}),
            json!({"tokenURL": "https://localhost/token"}),
            json!({"tokenURL": "nonsense"}),
            json!({}),
        ] {
            assert!(endpoint(&bad).is_err(), "必须拒绝: {bad}");
        }
    }

    #[test]
    fn form_encoding_matches_rfc3986() {
        let encoded = form_encode(&[
            ("grant_type", "refresh_token"),
            ("client_id", "a b+c/d"),
            ("refresh_token", "t~-._9"),
        ]);
        assert_eq!(
            encoded,
            "grant_type=refresh_token&client_id=a%20b%2Bc%2Fd&refresh_token=t~-._9"
        );
    }

    #[test]
    fn stamp_changes_with_endpoint_or_secret() {
        let a = stamp_of("https://a.example.com/t", "r1");
        assert_eq!(a, stamp_of("https://a.example.com/t", "r1"));
        assert_ne!(a, stamp_of("https://b.example.com/t", "r1"));
        assert_ne!(a, stamp_of("https://a.example.com/t", "r2"));
    }

    #[test]
    fn secret_string_is_decoded_like_record_object() {
        let record = json!({"secret": "{\"method\":\"external-idp\",\"refresh\":\"r\"}"});
        let secret = secret_of(&record);
        assert_eq!(secret["method"], json!("external-idp"));
        // 非 JSON 字符串原样返回，让上层的 method 判定失败（不 panic）。
        let raw = json!({"secret": "not-json"});
        assert_eq!(secret_of(&raw), json!("not-json"));
    }
}
