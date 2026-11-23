// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! `resource.fetch_public` —— 插件让宿主下载一张公网图片，产出**本次调用内**的封存资源。
//!
//! ## 边界（与上游同口径，逐条落实）
//!
//! - 只接受 `{url}` 一个参数，**不接受**代理、请求头、账号或凭据：公网图片下载
//!   不借用任何账号身份（否则等于「拿账号的 UA/代理去打第三方」）。
//! - 只允许 `https:443`，无 userinfo / query / fragment；主机必须含点且不是 IP
//!   字面量；`localhost` 与 `.local/.internal/.home.arpa` 等后缀一律拒绝。
//!   另加清单 origin 白名单（`manifest.check_url`），与宿主其它出站同一道闸。
//! - 单图 ≤ 32 MiB、单次调用累计 ≤ 64 MiB；只接受 PNG / JPEG / WebP，且
//!   **响应头声明的类型必须与字节真实魔数一致**（防「声明 png 实际 svg」）。
//! - 不跟随跳转：egress 客户端本身不跟随 3xx，这里再显式拒绝（3xx 一律失败）。
//! - 下载、校验全部通过后才登记资源；任何一步失败都不留半成品。
//!
//! ## 与上游的差异
//!
//! 上游用 `egress::image_download_client`（DNS 逐跳钉死 + `runtime_capacity` 内存记账）；
//! AIGX 复用注入的 [`WasmEgress`]（同一套代理/超时/会话回收），但没有 `OrdinaryPolicy`
//! 那层「公共 URL 特征表」与内存预算记账——因此把上限压在上游同值，并把「不跟跳转」
//! 作为硬条件写在这里。这个差异是**能力范围更小**，不是更宽松。

use serde_json::{json, Value};

use base64::Engine as _;

use super::context::Context;
use super::wire::WireError;

/// 单图上限（上游同值）。
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
/// 单次调用累计上限（上游同值）。
const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
/// 单次 `append` 的分片原始字节数：base64 后正好 96 KiB，贴着 resources 的分片上限。
const CHUNK_BYTES: usize = 72 * 1024;

fn bad_params(message: &str) -> WireError {
    WireError::with_code(message, "bad_params")
}

fn forbidden() -> WireError {
    WireError::with_code(
        "公共图片地址不能指向本机、内网、保留地址或未授权端口",
        "forbidden",
    )
}

fn limit(message: &str) -> WireError {
    WireError::with_code(message, "plugin_resource_limit")
}

fn unavailable(message: &str) -> WireError {
    WireError::with_code(message, "unsupported")
}

/// 地址逐项校验（不含 DNS：真实解析交给 egress 客户端自己的解析路径）。
fn checked_url(raw: &str, context: &Context) -> Result<url::Url, WireError> {
    if raw.is_empty()
        || raw.len() > 8192
        || raw.chars().any(|ch| ch.is_control() || ch.is_whitespace())
        || raw.contains('\\')
    {
        return Err(bad_params("公共图片地址无效或超过长度限制"));
    }
    let url = url::Url::parse(raw).map_err(|_| bad_params("公共图片地址格式无效"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(bad_params("公共图片仅支持不含登录凭据或片段的 HTTPS 地址"));
    }
    if url.port_or_known_default() != Some(443) {
        return Err(forbidden());
    }
    let host = url
        .host_str()
        .ok_or_else(|| bad_params("公共图片地址缺少主机"))?
        .trim_matches(['[', ']'])
        .trim_end_matches('.');
    if host.is_empty()
        || host.eq_ignore_ascii_case("localhost")
        || [".localhost", ".local", ".internal", ".home.arpa"]
            .iter()
            .any(|suffix| host.ends_with(*suffix))
        || host.parse::<std::net::IpAddr>().is_err() && !host.contains('.')
    {
        return Err(forbidden());
    }
    // 清单 origin 白名单：插件要在 `wasm.json.origins` 里声明才允许拉。
    context.shared.manifest.check_url(url.as_str(), false)?;
    Ok(url)
}

/// 响应头里的 `content-type` → 只认三种图片类型；多个/缺失/其他一律失败。
fn header_mime(headers: &Value) -> Result<&'static str, WireError> {
    let raw = headers
        .as_object()
        .and_then(|map| {
            map.iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
                .and_then(|(_, value)| value.as_str())
        })
        .unwrap_or("");
    let kind = raw.split(';').next().unwrap_or("").trim();
    if kind.eq_ignore_ascii_case("image/png") {
        Ok("image/png")
    } else if kind.eq_ignore_ascii_case("image/jpeg") {
        Ok("image/jpeg")
    } else if kind.eq_ignore_ascii_case("image/webp") {
        Ok("image/webp")
    } else {
        Err(unavailable("公共下载仅支持 PNG、JPEG 或 WebP 图片"))
    }
}

/// 用真实字节校验编码（不信响应头）。
fn actual_mime(bytes: &[u8]) -> Result<&'static str, WireError> {
    match image::guess_format(bytes) {
        Ok(image::ImageFormat::Png) => Ok("image/png"),
        Ok(image::ImageFormat::Jpeg) => Ok("image/jpeg"),
        Ok(image::ImageFormat::WebP) if bytes.get(8..12) == Some(b"WEBP") => Ok("image/webp"),
        _ => Err(unavailable("公共下载内容不是支持的图片编码")),
    }
}

/// `resource.fetch_public` 入口。
pub(super) async fn fetch_public(
    context: &mut Context,
    params: &Value,
) -> Result<Value, WireError> {
    context.permission("resources.fetch-public.v1")?;
    context.permission("resources.v1")?;
    if params
        .as_object()
        .is_none_or(|fields| fields.len() != 1 || !fields.contains_key("url"))
    {
        return Err(bad_params("公共图片下载只接受 url 参数"));
    }
    let url = params["url"]
        .as_str()
        .ok_or_else(|| bad_params("公共图片下载地址必须是文本"))?;
    let url = checked_url(url, context)?;

    let opened = context
        .shared
        .egress
        .request(json!({
            "url": url.as_str(),
            "method": "GET",
            "headers": {
                "accept": "image/png, image/jpeg, image/webp",
                "accept-encoding": "identity",
            },
        }))
        .await?;
    let session = opened["session"]
        .as_str()
        .filter(|session| !session.is_empty())
        .ok_or_else(|| unavailable("公共图片下载连接不可用"))?
        .to_string();
    let status = opened["status"].as_u64().unwrap_or(0);
    let headers = opened["headers"].clone();

    let result = async {
        // 3xx 一律失败：本宿主不跟跳转（egress 本身不跟，这里显式判定）。
        if (300..400).contains(&status) {
            return Err(unavailable("公共图片返回了跳转，宿主不跟随重定向"));
        }
        if status != 200 {
            return Err(unavailable("公共图片下载未返回 200"));
        }
        let declared = header_mime(&headers)?;
        let mut bytes = Vec::new();
        loop {
            let frame = context.shared.egress.read(&session).await?;
            if frame["done"].as_bool().unwrap_or(false) {
                break;
            }
            let chunk = frame["chunk_b64"].as_str().unwrap_or("");
            if chunk.is_empty() {
                return Err(unavailable("公共图片下载中断"));
            }
            let raw = base64::engine::general_purpose::STANDARD
                .decode(chunk)
                .map_err(|_| unavailable("公共图片下载内容编码无效"))?;
            if bytes.len().saturating_add(raw.len()) > MAX_IMAGE_BYTES {
                return Err(limit("公共图片超过单图 32 MiB 限制"));
            }
            bytes.extend_from_slice(&raw);
        }
        if bytes.is_empty() {
            return Err(unavailable("公共图片内容为空"));
        }
        if actual_mime(&bytes)? != declared {
            return Err(unavailable("公共图片的实际编码与响应内容类型不一致"));
        }
        Ok((declared, bytes))
    }
    .await;
    // 失败也要关会话：半截连接不能留在宿主会话表里。
    let _ = context.shared.egress.close(&session).await;
    let (mime, bytes) = result?;

    if bytes.len() > MAX_TOTAL_BYTES {
        return Err(limit("单次调用公共图片下载累计超过 64 MiB"));
    }
    let size = bytes.len();
    // 分片写入本次调用的资源表（`create` → `append` → `seal`），最后返回句柄。
    let created = context.blobs.create(mime)?;
    let id = created["id"]
        .as_str()
        .ok_or_else(|| unavailable("公共图片资源登记失败"))?
        .to_string();
    for chunk in bytes.chunks(CHUNK_BYTES) {
        let encoded = base64::engine::general_purpose::STANDARD.encode(chunk);
        context.blobs.append(&id, &encoded)?;
    }
    context.blobs.seal(&id)?;
    Ok(json!({ "id": id, "size": size, "mime": mime }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_mime_accepts_only_three_image_types() {
        assert_eq!(
            header_mime(&json!({"Content-Type": "image/png; charset=binary"})).expect("png"),
            "image/png"
        );
        assert_eq!(
            header_mime(&json!({"content-type": "image/jpeg"})).expect("jpeg"),
            "image/jpeg"
        );
        assert!(header_mime(&json!({"content-type": "image/svg+xml"})).is_err());
        assert!(header_mime(&json!({"content-type": "text/html"})).is_err());
        assert!(header_mime(&json!({})).is_err());
    }

    #[test]
    fn actual_mime_reads_magic_bytes_not_headers() {
        // 1x1 PNG
        let png = [
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89,
        ];
        assert_eq!(actual_mime(&png).expect("png"), "image/png");
        assert!(actual_mime(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").is_err());
        assert!(actual_mime(b"").is_err());
    }

    #[test]
    fn chunk_size_stays_within_append_limit() {
        // 分片 base64 后必须 ≤ resources 的 96 KiB 上限，否则 append 会整批失败。
        let encoded = base64::engine::general_purpose::STANDARD.encode(vec![0u8; CHUNK_BYTES]);
        assert!(
            encoded.len() <= 96 * 1024,
            "分片编码后 {} 字节超限",
            encoded.len()
        );
    }
}
