//! S3 兼容存储的传输原语：PUT / GET / HEAD + AWS Signature V4。
//!
//! ── 为什么手写 SigV4 ────────────────────────────────────────
//! 只有三个动作（PUT / GET / HEAD），引 `aws-sdk-s3` 会带进整套 SDK 与它自己的
//! 运行时；而仓库已有 `hmac` / `sha2` / `hex`（凭证加密与 ALTCHA 在用），签名
//! 本身是几十行纯函数，不新增依赖。
//!
//! ── URL 风格 ────────────────────────────────────────────────
//!   · AWS 官方（endpoint 为空或含 `amazonaws.com`）→ 虚拟主机风格
//!     `https://{bucket}.s3.{region}.amazonaws.com/{key}`
//!   · 自定义端点（MinIO / R2 / OSS / COS）→ path-style
//!     `{scheme}://{host}/{bucket}/{key}`
//! 端点不带 scheme 时默认 https（本地 MinIO 想走明文请显式写 `http://`）。
//!
//! ── 重定向与凭据 ────────────────────────────────────────────
//! SigV4 的 `Authorization` 绑定 host 与路径，跨源跳转的签名必然失效，且把
//! Access Key ID 与签名送给第三方主机本身就违反「凭据只发往配置的源」。因此：
//!   · 同源 307/308（以及 GET/HEAD 的 301/302）→ 对新 URL **重新签名**后跟随；
//!   · **跨源 3xx 一律拒绝**，错误文案说明原因（不会发任何凭据过去）；
//!   · 303 不跟随（S3 API 不存在这种语义，出现即视为异常）。
//!
//! ── 签名只覆盖需要的头 ──────────────────────────────────────
//! 参与签名的是 `host` / `x-amz-content-sha256` / `x-amz-date`（+ 可选
//! `content-type`）。不把 `content-length` 纳入：reqwest 在最后一步补它，
//! 签名时拿不到最终值；S3 允许只签部分头，只要 `SignedHeaders` 与
//! `Authorization` 一致即可。自定义 User-Agent 不参与签名，可以安全覆盖。

use std::time::Duration;

use reqwest::{Method, Response, StatusCode};
use url::Url;

use super::http;

/// 普通请求（HEAD bucket）的超时。
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// 大文件传输（PUT / GET）的超时。
const TRANSFER_TIMEOUT_SECS: u64 = 300;

/// S3 凭据（解密后的明文）。
pub(crate) struct S3Credentials {
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    pub bucket: String,
    /// 自定义端点；空 = AWS 官方。
    pub endpoint: String,
    /// 自定义 User-Agent；空 = reqwest 默认。
    pub user_agent: String,
}

/// 建一个禁用自动重定向的 Client。
fn client(credentials: &S3Credentials) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none());
    let user_agent = credentials.user_agent.trim();
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

fn is_aws_endpoint(endpoint: &str) -> bool {
    let trimmed = endpoint.trim();
    trimmed.is_empty() || trimmed.contains("amazonaws.com")
}

/// `(scheme, host[:port])`。
fn split_scheme_host(endpoint: &str) -> (&str, &str) {
    let trimmed = endpoint.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = trimmed.strip_prefix("https://") {
        ("https", rest)
    } else {
        ("https", trimmed)
    }
}

/// SigV4 的百分号编码（只保留 RFC 3986 unreserved）。
pub(crate) fn uri_encode(input: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b'/' if !encode_slash => out.push('/'),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// 对象键 → URL 路径（逐段编码，保留段间 `/`）。
fn encode_key(key: &str) -> String {
    key.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| uri_encode(segment, true))
        .collect::<Vec<_>>()
        .join("/")
}

/// 对象 URL（见模块头的两种风格）。
pub(crate) fn object_url(credentials: &S3Credentials, key: &str) -> Result<Url, String> {
    let encoded = encode_key(key);
    let raw = if is_aws_endpoint(&credentials.endpoint) {
        format!(
            "https://{}.s3.{}.amazonaws.com/{}",
            credentials.bucket, credentials.region, encoded
        )
    } else {
        let (scheme, host) = split_scheme_host(&credentials.endpoint);
        format!(
            "{scheme}://{host}/{}/{}",
            uri_encode(&credentials.bucket, true),
            encoded
        )
    };
    Url::parse(&raw).map_err(|error| format!("S3 地址无效: {error}"))
}

/// 桶级 URL（测试连接用）。
fn bucket_url(credentials: &S3Credentials) -> Result<Url, String> {
    let raw = if is_aws_endpoint(&credentials.endpoint) {
        format!(
            "https://{}.s3.{}.amazonaws.com/",
            credentials.bucket, credentials.region
        )
    } else {
        let (scheme, host) = split_scheme_host(&credentials.endpoint);
        format!(
            "{scheme}://{host}/{}/",
            uri_encode(&credentials.bucket, true)
        )
    };
    Url::parse(&raw).map_err(|error| format!("S3 地址无效: {error}"))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// 给一个请求签名，返回需要附上的头。
///
/// `now` 显式传入：签名是纯函数，单测可以用固定时刻断言确定性。
pub(crate) fn sign(
    method: &Method,
    url: &Url,
    body: &[u8],
    content_type: Option<&str>,
    credentials: &S3Credentials,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(String, String)> {
    let timestamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let datestamp = now.format("%Y%m%d").to_string();
    let body_hash = super::sha256_hex(body);

    let host = match url.port() {
        Some(port) => format!("{}:{}", url.host_str().unwrap_or_default(), port),
        None => url.host_str().unwrap_or_default().to_string(),
    };

    let mut headers: Vec<(String, String)> = vec![
        ("host".to_string(), host),
        ("x-amz-content-sha256".to_string(), body_hash.clone()),
        ("x-amz-date".to_string(), timestamp.clone()),
    ];
    if let Some(value) = content_type {
        headers.push(("content-type".to_string(), value.to_string()));
    }
    headers.sort_by(|a, b| a.0.cmp(&b.0));

    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        .join(";");

    let canonical_uri = {
        let path = url.path();
        if path.is_empty() {
            "/"
        } else {
            path
        }
    };
    let mut query_pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    query_pairs.sort();
    let canonical_query = query_pairs
        .iter()
        .map(|(key, value)| format!("{}={}", uri_encode(key, true), uri_encode(value, true)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical_request = format!(
        "{}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{body_hash}",
        method.as_str()
    );

    let scope = format!("{datestamp}/{}/s3/aws4_request", credentials.region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{timestamp}\n{scope}\n{}",
        super::sha256_hex(canonical_request.as_bytes())
    );

    let k_date = hmac_sha256(
        format!("AWS4{}", credentials.secret_key).as_bytes(),
        datestamp.as_bytes(),
    );
    let k_region = hmac_sha256(&k_date, credentials.region.as_bytes());
    let k_service = hmac_sha256(&k_region, b"s3");
    let k_signing = hmac_sha256(&k_service, b"aws4_request");
    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
        credentials.access_key
    );

    let mut result = headers;
    result.push(("authorization".to_string(), authorization));
    result
}

fn transport_error(op: &str, error: &reqwest::Error) -> String {
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_builder() {
        "请求构造失败"
    } else if error.is_connect() {
        "连接失败（端点 / 端口 / 证书，或需要代理）"
    } else if error.is_body() {
        "响应传输失败"
    } else {
        "网络请求失败"
    };
    format!("S3 {op}失败（{reason}）")
}

fn status_error(op: &str, status: StatusCode, url: &Url) -> String {
    let safe_url = http::redact_url(url.as_str());
    let mut message = format!("S3 {op} 失败: {status} ({safe_url})");
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        message.push_str(
            "。请检查 Access Key / Secret Key 与该密钥是否具备此桶的读写权限；若确认无误仍被拒，部分服务按 User-Agent 校验客户端类型，可填写自定义 User-Agent（如 rclone/v1.67.0）。",
        );
    } else if status == StatusCode::NOT_FOUND {
        message.push_str("。请检查存储桶名称、区域（Region）与端点（Endpoint）。");
    } else if status == StatusCode::BAD_REQUEST {
        message.push_str(
            "。请检查区域（Region）是否正确 —— 部分服务（如 Cloudflare R2）要求填 auto。",
        );
    }
    message
}

/// 发一个签名请求并逐跳跟随**同源**重定向（每跳重新签名）。
async fn send_signed(
    client: &reqwest::Client,
    method: Method,
    url: &Url,
    body: Vec<u8>,
    content_type: Option<&str>,
    credentials: &S3Credentials,
    timeout: Duration,
) -> Result<(Response, Url), String> {
    let body = bytes::Bytes::from(body);
    let mut current = url.clone();
    let mut current_method = method;

    for hop in 0..=http::MAX_REDIRECT_HOPS {
        let headers = sign(
            &current_method,
            &current,
            &body,
            content_type,
            credentials,
            chrono::Utc::now(),
        );
        let mut request = client
            .request(current_method.clone(), current.clone())
            .timeout(timeout)
            .body(body.clone());
        if let Some(value) = content_type {
            request = request.header(reqwest::header::CONTENT_TYPE, value);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| transport_error(current_method.as_str(), &error))?;
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
            return Ok((response, current));
        };
        if hop == http::MAX_REDIRECT_HOPS {
            return Err(format!(
                "S3 重定向次数超过 {} 次: {}",
                http::MAX_REDIRECT_HOPS,
                http::redact_url(current.as_str())
            ));
        }
        let next = http::resolve_location(&current, &location)?;
        // SigV4 凭据绝不发给配置源之外的主机（跨源签名没有意义，且泄露 Access Key ID）
        if !http::credentials_allowed(&current, &next) {
            return Err(format!(
                "S3 端点重定向到其他源（{}），已拒绝跟随以避免签名凭据外泄；请把 Endpoint 直接配置为最终地址",
                http::redact_url(next.as_str())
            ));
        }
        let next_method = http::redirect_method(status, &current_method).ok_or_else(|| {
            format!(
                "S3 端点返回 {status} 重定向，但 {} 方法不允许按标准语义跟随",
                current_method
            )
        })?;
        if next_method == Method::GET && current_method != Method::GET {
            return Err("S3 端点把写请求重定向为 GET，已拒绝（可能签错区域）".to_string());
        }
        current_method = next_method;
        current = next;
    }
    Err("S3 重定向处理异常".to_string())
}

/// 测试连接：对桶发 HEAD。
pub(crate) async fn test_connection(credentials: &S3Credentials) -> Result<(), String> {
    let url = bucket_url(credentials)?;
    let (response, final_url) = send_signed(
        &client(credentials)?,
        Method::HEAD,
        &url,
        Vec::new(),
        None,
        credentials,
        Duration::from_secs(DEFAULT_TIMEOUT_SECS),
    )
    .await?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(status_error("HEAD bucket", response.status(), &final_url))
}

/// PUT 一个对象。
pub(crate) async fn put_object(
    credentials: &S3Credentials,
    key: &str,
    bytes: Vec<u8>,
    content_type: &str,
) -> Result<(), String> {
    let url = object_url(credentials, key)?;
    let (response, final_url) = send_signed(
        &client(credentials)?,
        Method::PUT,
        &url,
        bytes,
        Some(content_type),
        credentials,
        Duration::from_secs(TRANSFER_TIMEOUT_SECS),
    )
    .await?;
    if response.status().is_success() {
        return Ok(());
    }
    Err(status_error("PUT", response.status(), &final_url))
}

/// GET 一个对象；404 返回 `None`。
pub(crate) async fn get_object(
    credentials: &S3Credentials,
    key: &str,
    max_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    use futures::StreamExt as _;

    let url = object_url(credentials, key)?;
    let (response, final_url) = send_signed(
        &client(credentials)?,
        Method::GET,
        &url,
        Vec::new(),
        None,
        credentials,
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
                "远端对象超过上限（{length} 字节 > {max_bytes} 字节）: {}",
                http::redact_url(final_url.as_str())
            ));
        }
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            format!(
                "读取 S3 响应失败: {}",
                http::redact_text(&error.to_string())
            )
        })?;
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(format!("远端对象超过上限（{max_bytes} 字节）"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials() -> S3Credentials {
        S3Credentials {
            access_key: "AKIDEXAMPLE".to_string(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_string(),
            region: "us-east-1".to_string(),
            bucket: "examplebucket".to_string(),
            endpoint: String::new(),
            user_agent: String::new(),
        }
    }

    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn aws_url_uses_virtual_host_style() {
        let url = object_url(&credentials(), "a/b.json").unwrap();
        assert_eq!(
            url.as_str(),
            "https://examplebucket.s3.us-east-1.amazonaws.com/a/b.json"
        );
    }

    #[test]
    fn custom_endpoint_uses_path_style() {
        let mut creds = credentials();
        creds.endpoint = "http://minio.local:9000".to_string();
        let url = object_url(&creds, "a/b.json").unwrap();
        assert_eq!(
            url.as_str(),
            "http://minio.local:9000/examplebucket/a/b.json"
        );
    }

    #[test]
    fn bare_endpoint_defaults_to_https() {
        let mut creds = credentials();
        creds.endpoint = "minio.local:9000".to_string();
        let url = object_url(&creds, "x.json").unwrap();
        assert_eq!(
            url.as_str(),
            "https://minio.local:9000/examplebucket/x.json"
        );
    }

    #[test]
    fn object_key_segments_are_encoded() {
        let url = object_url(&credentials(), "aigx sync/默认/snapshot.json").unwrap();
        assert!(url.as_str().contains("aigx%20sync"));
        assert!(url.as_str().contains("%E9%BB%98%E8%AE%A4"));
        // 段间斜杠保留
        assert!(url
            .as_str()
            .ends_with("/aigx%20sync/%E9%BB%98%E8%AE%A4/snapshot.json"));
    }

    #[test]
    fn uri_encode_follows_sigv4_rules() {
        assert_eq!(uri_encode("a b/c", false), "a%20b/c");
        assert_eq!(uri_encode("a b/c", true), "a%20b%2Fc");
        assert_eq!(uri_encode("~_-.", true), "~_-.");
    }

    #[test]
    fn sign_is_deterministic_for_fixed_time() {
        let url = Url::parse("https://examplebucket.s3.us-east-1.amazonaws.com/a.json").unwrap();
        let first = sign(
            &Method::PUT,
            &url,
            b"{}",
            Some("application/json"),
            &credentials(),
            now(),
        );
        let second = sign(
            &Method::PUT,
            &url,
            b"{}",
            Some("application/json"),
            &credentials(),
            now(),
        );
        assert_eq!(first, second, "同一输入（含固定时刻）必须得到同一签名");
        let changed = sign(
            &Method::PUT,
            &url,
            b"{\"x\":1}",
            Some("application/json"),
            &credentials(),
            now(),
        );
        assert_ne!(first, changed, "请求体变化必须改变签名");
    }

    #[test]
    fn sign_produces_expected_authorization_shape() {
        let url = Url::parse("https://examplebucket.s3.us-east-1.amazonaws.com/a.json").unwrap();
        let headers = sign(
            &Method::PUT,
            &url,
            b"{}",
            Some("application/json"),
            &credentials(),
            now(),
        );
        let authorization = headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.clone())
            .expect("authorization header");
        assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/"));
        assert!(authorization.contains("/us-east-1/s3/aws4_request"));
        assert!(authorization
            .contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"));
        // x-amz-date 与固定时刻一致
        assert!(headers
            .iter()
            .any(|(name, value)| name == "x-amz-date" && value == "20130524T000000Z"));
    }

    #[test]
    fn sign_binds_host_and_path() {
        let a = Url::parse("https://examplebucket.s3.us-east-1.amazonaws.com/a.json").unwrap();
        let b = Url::parse("https://evil.example.com/a.json").unwrap();
        let headers_a = sign(&Method::GET, &a, b"", None, &credentials(), now());
        let headers_b = sign(&Method::GET, &b, b"", None, &credentials(), now());
        let signature = |headers: &[(String, String)]| {
            headers
                .iter()
                .find(|(name, _)| name == "authorization")
                .map(|(_, value)| value.clone())
                .unwrap()
        };
        assert_ne!(
            signature(&headers_a),
            signature(&headers_b),
            "签名必须绑定 host（跨源签名无法复用）"
        );
    }
}
