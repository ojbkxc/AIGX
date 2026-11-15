//! 云同步的出网细节：**重定向逐跳策略**与 URL 脱敏。
//!
//! ── 为什么不用 reqwest 的自动重定向 ──────────────────────────
//! reqwest 内建的重定向跟随时，`Authorization` 头的行为依赖版本与策略：跨源
//! 跳转时是否剥离凭据、是否保留方法，都没有可由调用方审计的显式决策点。云同步
//! 的凭据（WebDAV Basic / S3 SigV4）指向用户的私有存储，一旦被 30x 带到第三方
//! 主机就等于泄露。因此所有请求都建在 `Policy::none()` 的 Client 上，由本模块
//! 的纯函数逐跳决策：
//!   · 只有**同源**（scheme + host + port 全部一致）的下一跳才携带凭据；
//!   · 跨源下一跳丢弃 `Authorization`（WebDAV 继续匿名请求；S3 直接拒绝跟随）；
//!   · 只跟随标准重定向语义允许的方法（303 → GET；301/302 仅 GET/HEAD；
//!     307/308 保留方法与请求体）；
//!   · 跳数上限 [`MAX_REDIRECT_HOPS`]。
//!
//! 这些函数全部是纯逻辑，单测不需要发起真实网络。
//!
//! ── 为什么错误文案必须脱敏 ──────────────────────────────────
//! 用户可能把 `https://user:pass@host/...` 填进地址栏；底层错误（reqwest 的
//! source 链）常把完整 URL 带出来。写进日志或 API 响应前统一经
//! [`redact_url`] / [`redact_text`]：只保留 scheme + host + port + path。

use reqwest::{Method, StatusCode};
use url::Url;

/// 允许跟随的最大重定向跳数。
pub(crate) const MAX_REDIRECT_HOPS: usize = 5;

/// 两个 URL 是否同源（scheme + host + port 全部一致）。
///
/// 端口按「显式端口，缺省时用协议默认端口」比较：`https://a` 与 `https://a:443`
/// 视为同源。host 比较不区分大小写。
pub(crate) fn same_origin(a: &Url, b: &Url) -> bool {
    if a.scheme() != b.scheme() {
        return false;
    }
    let host_a = a.host_str().unwrap_or_default();
    let host_b = b.host_str().unwrap_or_default();
    if !host_a.eq_ignore_ascii_case(host_b) {
        return false;
    }
    a.port_or_known_default() == b.port_or_known_default()
}

/// 下一跳是否允许携带凭据。**唯一决策点**：只有与配置源同源才允许。
pub(crate) fn credentials_allowed(configured: &Url, next: &Url) -> bool {
    same_origin(configured, next)
}

/// 解析 `Location`（相对地址按当前 URL 解析），并拒绝非 http(s) 协议。
pub(crate) fn resolve_location(current: &Url, location: &str) -> Result<Url, String> {
    let next = current
        .join(location.trim())
        .map_err(|error| format!("重定向地址无效: {error}"))?;
    match next.scheme() {
        "http" | "https" => Ok(next),
        other => Err(format!("重定向到不支持的协议: {other}")),
    }
}

/// 按 RFC 语义决定重定向后的请求方法；`None` = 该状态码 + 方法组合不允许跟随。
pub(crate) fn redirect_method(status: StatusCode, method: &Method) -> Option<Method> {
    match status {
        StatusCode::SEE_OTHER => Some(Method::GET),
        StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND => {
            if *method == Method::GET || *method == Method::HEAD {
                Some(method.clone())
            } else {
                None
            }
        }
        StatusCode::TEMPORARY_REDIRECT | StatusCode::PERMANENT_REDIRECT => Some(method.clone()),
        _ => None,
    }
}

/// URL 脱敏：只保留 scheme + host + port + path（去掉 userinfo 与 query）。
pub(crate) fn redact_url(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(parsed) => {
            let mut out = format!("{}://", parsed.scheme());
            if let Some(host) = parsed.host_str() {
                out.push_str(host);
            }
            if let Some(port) = parsed.port() {
                out.push(':');
                out.push_str(&port.to_string());
            }
            out.push_str(parsed.path());
            out
        }
        Err(_) => "（无效地址）".to_string(),
    }
}

/// 一段自由文本的脱敏：扫描所有含 `://` 的片段，把 URL 本体替换为脱敏形态。
///
/// 不引入正则依赖：错误详情里的 URL 可能被括号 / 引号 / 中文标点包着
///（`(https://u:p@host/x?y=1):`），所以先从 `://` 向左回溯到 scheme 起点，
/// 再向右取到标点 / 空白为止。
pub(crate) fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let Some(idx) = rest.find("://") else {
            out.push_str(rest);
            break;
        };
        // scheme 起点：从 `://` 向左回溯 `[A-Za-z0-9+.-]`
        let head = &rest[..idx];
        let scheme_start = head
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            .last()
            .map(|(i, _)| i)
            .unwrap_or(idx);
        out.push_str(&rest[..scheme_start]);
        let candidate = &rest[scheme_start..];
        let end = candidate
            .char_indices()
            .find(|(_, c)| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '"' | '\'' | '<' | '>' | '(' | ')' | '[' | ']' | '，' | '。' | '；' | '：'
                    )
            })
            .map(|(i, _)| i)
            .unwrap_or(candidate.len());
        out.push_str(&redact_url(&candidate[..end]));
        rest = &candidate[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn same_origin_respects_scheme_host_port() {
        let base = url("https://dav.example.com/dav");
        // 同源：大小写 / 默认端口等价
        assert!(same_origin(&base, &url("https://DAV.example.com/other")));
        assert!(same_origin(&base, &url("https://dav.example.com:443/x")));
        // 跨源：协议 / 主机 / 端口任一不同
        assert!(!same_origin(&base, &url("http://dav.example.com/dav")));
        assert!(!same_origin(&base, &url("https://evil.example.com/dav")));
        assert!(!same_origin(
            &base,
            &url("https://dav.example.com:8443/dav")
        ));
    }

    #[test]
    fn credentials_only_sent_to_configured_origin() {
        let configured = url("https://dav.example.com/remote.php/dav/files/demo");
        // 同源跳转（例如服务端把目录补成带斜杠的规范形态）：保留凭据
        assert!(credentials_allowed(
            &configured,
            &url("https://dav.example.com/remote.php/dav/files/demo/")
        ));
        // 跨源跳转（防盗链 CDN / 登录门户 / 攻击者控制的 30x）：凭据必须丢弃
        assert!(!credentials_allowed(
            &configured,
            &url("https://cdn.example.net/redirected")
        ));
        // 协议降级（https → http）同样视为跨源
        assert!(!credentials_allowed(
            &configured,
            &url("http://dav.example.com/remote.php/dav/files/demo/")
        ));
    }

    #[test]
    fn resolve_location_handles_relative_and_rejects_other_schemes() {
        let current = url("https://dav.example.com/dav/a");
        assert_eq!(
            resolve_location(&current, "/dav/b").unwrap().as_str(),
            "https://dav.example.com/dav/b"
        );
        assert_eq!(
            resolve_location(&current, "c/").unwrap().as_str(),
            "https://dav.example.com/dav/c/"
        );
        assert!(resolve_location(&current, "ftp://example.com/x").is_err());
    }

    #[test]
    fn redirect_method_follows_standard_semantics() {
        assert_eq!(
            redirect_method(StatusCode::SEE_OTHER, &Method::PUT),
            Some(Method::GET)
        );
        // 301/302 只对 GET/HEAD 自动跟随；PUT 的重定向不静默降级
        assert_eq!(redirect_method(StatusCode::FOUND, &Method::PUT), None);
        assert_eq!(
            redirect_method(StatusCode::FOUND, &Method::GET),
            Some(Method::GET)
        );
        // 307/308 保留方法与请求体
        assert_eq!(
            redirect_method(StatusCode::TEMPORARY_REDIRECT, &Method::PUT),
            Some(Method::PUT)
        );
        // PROPFIND 不在 http crate 的关联常量里（Method::from_bytes 构造，
        // 见 webdav.rs propfind()），308 语义=保留方法 → 返回同一名值
        let propfind = Method::from_bytes(b"PROPFIND").expect("PROPFIND 是合法 HTTP 方法");
        assert_eq!(
            redirect_method(StatusCode::PERMANENT_REDIRECT, &propfind),
            Some(propfind.clone())
        );
        assert_eq!(redirect_method(StatusCode::CREATED, &Method::PUT), None);
    }

    #[test]
    fn redact_url_strips_credentials_and_query() {
        let redacted = redact_url("https://alice:secret@example.com:8443/dav?token=abc");
        assert_eq!(redacted, "https://example.com:8443/dav");
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("abc"));
    }

    #[test]
    fn redact_text_rewrites_embedded_urls() {
        let text = "error sending request for url (https://u:p@host.example/dav?x=1): timeout";
        let redacted = redact_text(text);
        assert!(!redacted.contains("u:p"));
        assert!(!redacted.contains("x=1"));
        assert!(redacted.contains("https://host.example/dav"));
    }
}
