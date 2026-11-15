//! WebDAV 传输原语：PROPFIND / MKCOL / PUT / GET。
//!
//! 只做「把一个 URL 上的字节读出来 / 写进去」这一层，不知道快照与 manifest。
//!
//! ── 出网与重定向 ────────────────────────────────────────────
//! 所有请求都建在 `redirect(Policy::none())` 的 Client 上，重定向由本模块逐跳
//! 处理：只有与**配置源同源**的下一跳才携带 Basic 凭据（见
//! [`super::http::credentials_allowed`]），跨源跳转自动降级为匿名请求；跳数与
//! 方法语义由 [`super::http`] 的纯函数裁决。这样即使服务端被劫持、返回指向
//! 第三方主机的 30x，凭据也不会离开用户配置的那台存储。
//!
//! ── 自定义 User-Agent ───────────────────────────────────────
//! 少数 WebDAV 服务把凭据绑到「应用类型」上，并按 User-Agent 校验；空串 =
//! 用 reqwest 默认值（显式传空串会变成「不发 UA」，反而更糟）。
//!
//! ── 超时 ────────────────────────────────────────────────────
//! 建独立 Client（不复用共享 http_client）：普通请求 30 秒、大文件传输 300 秒，
//! 这是一次性长任务的口径，与数据面共享 Client 的 SSE 超时语义不同。

use std::time::Duration;

use base64::Engine as _;
use reqwest::{Method, Response, StatusCode};
use url::Url;

use super::http;

/// 普通请求（PROPFIND / MKCOL）的超时。
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// 大文件传输（PUT / GET 快照）的超时。
const TRANSFER_TIMEOUT_SECS: u64 = 300;

/// Basic Auth 的 `(用户名, 密码)`；`None` = 匿名。
pub(crate) type WebDavAuth = Option<(String, Option<String>)>;

fn method_propfind() -> Method {
    Method::from_bytes(b"PROPFIND").expect("PROPFIND 是合法 HTTP 方法")
}

fn method_mkcol() -> Method {
    Method::from_bytes(b"MKCOL").expect("MKCOL 是合法 HTTP 方法")
}

/// 建一个禁用自动重定向的 Client。
pub(crate) fn client(user_agent: &str) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none());
    let user_agent = user_agent.trim();
    builder = builder.user_agent(if user_agent.is_empty() {
        super::config::DEFAULT_USER_AGENT.to_string()
    } else {
        user_agent.to_string()
    });
    builder.build().map_err(|error| {
        format!(
            "创建 HTTP 客户端失败: {}",
            http::redact_text(&error.to_string())
        )
    })
}

/// 解析并校验 WebDAV 基地址（只认 http / https）。
pub(crate) fn parse_base_url(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("WebDAV 地址不能为空".to_string());
    }
    let url = Url::parse(trimmed).map_err(|error| format!("WebDAV 地址无效: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!("WebDAV 只支持 http/https 地址（当前是 {other}）")),
    }
}

/// 把基地址与路径段拼成完整 URL（每段单独百分号编码，含空格与中文）。
///
/// `trailing_slash` 用于 MKCOL：目录 URL 以 `/` 结尾是 WebDAV 的惯例，部分
/// 服务端对不带斜杠的 MKCOL 会回 301。
pub(crate) fn build_remote_url(
    base_url: &str,
    segments: &[String],
    trailing_slash: bool,
) -> Result<Url, String> {
    let mut url = parse_base_url(base_url)?;
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|_| "WebDAV 地址格式不支持追加路径".to_string())?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
        if trailing_slash {
            path.push("");
        }
    }
    Ok(url)
}

/// 用户名 / 密码 → Basic Auth（用户名为空 = 匿名）。
pub(crate) fn auth_from_credentials(username: &str, password: &str) -> WebDavAuth {
    let user = username.trim();
    if user.is_empty() {
        return None;
    }
    Some((user.to_string(), Some(password.to_string())))
}

fn basic_header(auth: &WebDavAuth) -> Option<String> {
    auth.as_ref().map(|(user, password)| {
        let token = base64::engine::general_purpose::STANDARD.encode(format!(
            "{user}:{}",
            password.as_deref().unwrap_or_default()
        ));
        format!("Basic {token}")
    })
}

/// 发一个请求并逐跳跟随重定向（调用方已保证 Client 是 `Policy::none()`）。
///
/// 返回最终响应与最终 URL（错误文案用）。凭据只在与 `origin` 同源的跳上附带。
async fn send(
    client: &reqwest::Client,
    method: Method,
    url: &Url,
    auth: &WebDavAuth,
    headers: &[(&str, String)],
    body: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<(Response, Url), String> {
    let origin = url.clone();
    let auth_header = basic_header(auth);
    let mut current = url.clone();
    let mut current_method = method;

    for hop in 0..=http::MAX_REDIRECT_HOPS {
        let mut request = client
            .request(current_method.clone(), current.clone())
            .timeout(timeout);
        for (name, value) in headers {
            request = request.header(*name, value.clone());
        }
        if let Some(bytes) = &body {
            request = request.body(bytes.clone());
        }
        if let Some(value) = &auth_header {
            // 跨源跳转不携带凭据（唯一决策点）
            if http::credentials_allowed(&origin, &current) {
                request = request.header(reqwest::header::AUTHORIZATION, value.clone());
            }
        }
        let response = request
            .send()
            .await
            .map_err(|error| transport_error(&current_method, &current, &error))?;
        let status = response.status();
        if !status.is_redirection() {
            return Ok((response, current));
        }
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
        else {
            // 3xx 却没有 Location：交给调用方按状态码报错
            return Ok((response, current));
        };
        if hop == http::MAX_REDIRECT_HOPS {
            return Err(format!(
                "WebDAV 重定向次数超过 {} 次: {}",
                http::MAX_REDIRECT_HOPS,
                http::redact_url(current.as_str())
            ));
        }
        let next = http::resolve_location(&current, &location)?;
        current_method = http::redirect_method(status, &current_method).ok_or_else(|| {
            format!(
                "WebDAV 服务器返回 {status} 重定向，但 {} 方法不允许按标准语义跟随",
                current_method
            )
        })?;
        current = next;
    }
    Err("WebDAV 重定向处理异常".to_string())
}

/// 错误文案里**不能带凭据与查询串**。
fn transport_error(op: &Method, url: &Url, error: &reqwest::Error) -> String {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_redirect() {
        "重定向失败"
    } else if error.is_builder() {
        "请求构造失败"
    } else if error.is_connect() {
        "连接失败（目标地址 / 端口 / 证书，或需要代理）"
    } else if error.is_body() {
        "响应传输失败"
    } else {
        "网络请求失败"
    };
    format!(
        "WebDAV {op} 失败（{reason}）: {}",
        http::redact_url(url.as_str())
    )
}

fn is_jianguoyun(url: &Url) -> bool {
    url.host_str()
        .map(|host| {
            let host = host.to_ascii_lowercase();
            host.contains("jianguoyun.com") || host.contains("nutstore")
        })
        .unwrap_or(false)
}

fn status_error(op: &str, status: StatusCode, url: &Url) -> String {
    let safe_url = http::redact_url(url.as_str());
    let mut message = format!("WebDAV {op} 失败: {status} ({safe_url})");
    let jianguoyun = is_jianguoyun(url);
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        if jianguoyun {
            message.push_str("。坚果云请使用「第三方应用密码」，并确认地址指向 /dav/ 下的目录。");
        } else {
            message.push_str("。请检查 WebDAV 用户名、密码与目录读写权限。");
        }
    } else if status == StatusCode::CONFLICT && op == "MKCOL" {
        if jianguoyun {
            message.push_str("。坚果云不允许自动创建顶层文件夹，请先在网页端手动创建后再试。");
        } else {
            message.push_str("。请确认上级目录存在。");
        }
    } else if status == StatusCode::NOT_FOUND {
        message.push_str("。请确认 WebDAV 地址与同步路径是否正确。");
    }
    message
}

/// 测试连接：对基地址发 `PROPFIND Depth: 0`。
///
/// 用 PROPFIND 而不是 HEAD：很多 WebDAV 服务对目录的 HEAD 返回 405，而
/// PROPFIND 能同时验证「地址对」与「这确实是 WebDAV 而不是静态站点」。
pub(crate) async fn test_connection(
    base_url: &str,
    auth: &WebDavAuth,
    user_agent: &str,
) -> Result<(), String> {
    let url = parse_base_url(base_url)?;
    let headers = [("Depth", "0".to_string())];
    let (response, final_url) = send(
        &client(user_agent)?,
        method_propfind(),
        &url,
        auth,
        &headers,
        None,
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    )
    .await?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(status_error("PROPFIND", response.status(), &final_url))
}

/// 逐级创建远端目录（MKCOL 必须一级一级来）。
///
/// 乐观创建 + 歧义回退：直接 MKCOL 比「先 PROPFIND 再决定建不建」少一半往返；
/// 已存在时服务端回 405 / 409 / 3xx，再补一次 PROPFIND 确认，确认存在就算成功。
pub(crate) async fn ensure_directories(
    base_url: &str,
    segments: &[String],
    auth: &WebDavAuth,
    user_agent: &str,
) -> Result<(), String> {
    if segments.is_empty() {
        return Ok(());
    }
    let client = client(user_agent)?;
    for depth in 1..=segments.len() {
        let url = build_remote_url(base_url, &segments[..depth], true)?;
        let (response, final_url) = send(
            &client,
            method_mkcol(),
            &url,
            auth,
            &[],
            None,
            Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        )
        .await?;
        let status = response.status();
        if status.is_success() {
            continue;
        }
        if status == StatusCode::METHOD_NOT_ALLOWED
            || status == StatusCode::CONFLICT
            || status.is_redirection()
        {
            if propfind_exists(&client, &final_url, auth).await? {
                continue;
            }
        }
        return Err(status_error("MKCOL", status, &final_url));
    }
    Ok(())
}

async fn propfind_exists(
    client: &reqwest::Client,
    url: &Url,
    auth: &WebDavAuth,
) -> Result<bool, String> {
    let headers = [("Depth", "0".to_string())];
    let response = send(
        client,
        method_propfind(),
        url,
        auth,
        &headers,
        None,
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    )
    .await;
    match response {
        Ok((response, _)) => Ok(response.status().is_success()),
        // 探测失败按「不存在」处理：调用方会给出 MKCOL 的原始错误
        Err(_) => Ok(false),
    }
}

/// PUT 一个文件。
pub(crate) async fn put_bytes(
    url: &Url,
    auth: &WebDavAuth,
    bytes: Vec<u8>,
    content_type: &str,
    user_agent: &str,
) -> Result<(), String> {
    let headers = [("Content-Type", content_type.to_string())];
    let (response, final_url) = send(
        &client(user_agent)?,
        Method::PUT,
        url,
        auth,
        &headers,
        Some(bytes),
        Duration::from_secs(TRANSFER_TIMEOUT_SECS),
    )
    .await?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(status_error("PUT", response.status(), &final_url))
}

/// GET 一个文件；404 返回 `None`（「远端还没有这份数据」是正常状态）。
///
/// `max_bytes` 是硬上限：先看 Content-Length，再在流式读取时逐块累计 ——
/// 只看头不够（服务端可以不发长度，或发一个假的），所以两道都做。
pub(crate) async fn get_bytes(
    url: &Url,
    auth: &WebDavAuth,
    max_bytes: usize,
    user_agent: &str,
) -> Result<Option<Vec<u8>>, String> {
    use futures::StreamExt as _;

    let (response, final_url) = send(
        &client(user_agent)?,
        Method::GET,
        url,
        auth,
        &[],
        None,
        Duration::from_secs(TRANSFER_TIMEOUT_SECS),
    )
    .await?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !status.is_success() {
        return Err(status_error("GET", status, &final_url));
    }
    if let Some(length) = response.content_length() {
        if length > max_bytes as u64 {
            return Err(format!(
                "远端文件超过上限（{length} 字节 > {max_bytes} 字节）: {}",
                http::redact_url(final_url.as_str())
            ));
        }
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            format!(
                "读取 WebDAV 响应失败: {}",
                http::redact_text(&error.to_string())
            )
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(format!("远端文件超过上限（{max_bytes} 字节）"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_remote_url_encodes_segments() {
        let url = build_remote_url(
            "https://dav.example.com/remote.php/dav/files/demo/",
            &["aigx sync".to_string(), "default".to_string()],
            false,
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://dav.example.com/remote.php/dav/files/demo/aigx%20sync/default"
        );
        assert!(!url.as_str().contains("//aigx"), "不应出现双斜杠");
    }

    #[test]
    fn build_remote_url_can_mark_directory() {
        let url = build_remote_url(
            "https://dav.example.com/dav",
            &["aigx-sync".to_string()],
            true,
        )
        .unwrap();
        assert!(url.as_str().ends_with("/aigx-sync/"));
    }

    #[test]
    fn parse_base_url_rejects_other_schemes() {
        assert!(parse_base_url("ftp://example.com/dav").is_err());
        assert!(parse_base_url("  ").is_err());
        assert!(parse_base_url("https://dav.jianguoyun.com/dav/").is_ok());
    }

    #[test]
    fn auth_from_credentials_rejects_blank_username() {
        assert!(auth_from_credentials("  ", "pass").is_none());
        assert_eq!(
            auth_from_credentials(" user ", "pass"),
            Some(("user".to_string(), Some("pass".to_string())))
        );
    }

    #[test]
    fn basic_header_encodes_credentials() {
        let auth = Some(("user".to_string(), Some("pass".to_string())));
        assert_eq!(
            basic_header(&auth).unwrap(),
            "Basic dXNlcjpwYXNz".to_string()
        );
        assert!(basic_header(&None).is_none());
    }
}
