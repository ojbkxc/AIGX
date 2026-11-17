// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 请求正文的组装 —— 从二进制资源拼 `bytes` / `multipart`。
//!
//! 与上游逐字一致：三种正文形式（`body` / `body_resource` / `multipart`）
//! 互斥；`multipart` 文本字段累计 ≤1 MiB、文件累计 ≤96 MiB、字段名受限。

use base64::Engine;
use serde_json::Value;

use super::context::Context;
use super::wire::WireError;

/// 出站请求体的三种形态之一（AIGX 的 `script::transport` 只收 base64，
/// 因此 multipart 在这里拼成字节，由调用方 base64 编码）。
pub(crate) enum Body {
    Bytes(Vec<u8>),
    Multipart(Vec<(String, MultipartPart)>),
}

pub(crate) struct MultipartPart {
    pub filename: Option<String>,
    pub content_type: String,
    pub bytes: Vec<u8>,
    /// 该字段是文本还是二进制（上游用于诊断展示；AIGX 侧拼字节时按
    /// `content_type` 判定，此字段暂无读取方）。
    #[allow(dead_code)]
    pub is_text: bool,
}

/// 编码后的正文：base64 + 需要补的 Content-Type。
pub(crate) struct EncodedBody {
    pub body_b64: String,
    pub content_type: Option<String>,
}

/// 把 [`Body`] 编码成宿主 egress 能吃的 base64。
///
/// `Multipart` 在这里拼成 RFC 7578 字节流（边界随机），并把 `Content-Type`
/// 一并返回 —— 脚本宿主的 `transport` 只认 `body_b64` + 普通 headers。
pub(crate) fn encode_body(body: Body) -> Result<EncodedBody, WireError> {
    match body {
        Body::Bytes(bytes) => Ok(EncodedBody {
            body_b64: base64::engine::general_purpose::STANDARD.encode(bytes),
            content_type: None,
        }),
        Body::Multipart(parts) => {
            let mut nonce = [0u8; 12];
            getrandom::getrandom(&mut nonce).map_err(|_| super::wire::error("无法生成上传边界"))?;
            let boundary = format!(
                "----aigx{}",
                nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
            );
            let mut out = Vec::new();
            for (field, part) in parts {
                out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                match part.filename {
                    Some(filename) => out.extend_from_slice(
                        format!(
                            "Content-Disposition: form-data; name=\"{field}\"; filename=\"{filename}\"\r\nContent-Type: {}\r\n\r\n",
                            part.content_type
                        )
                        .as_bytes(),
                    ),
                    None => out.extend_from_slice(
                        format!("Content-Disposition: form-data; name=\"{field}\"\r\n\r\n").as_bytes(),
                    ),
                }
                out.extend_from_slice(&part.bytes);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            Ok(EncodedBody {
                body_b64: base64::engine::general_purpose::STANDARD.encode(out),
                content_type: Some(format!("multipart/form-data; boundary={boundary}")),
            })
        }
    }
}

fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-[]".contains(&byte))
}

/// 解析 `http.open` 的正文声明。
pub(crate) fn body(context: &Context, params: &Value) -> Result<Option<Body>, WireError> {
    let resource = params.get("body_resource").filter(|value| !value.is_null());
    let multipart = params.get("multipart").filter(|value| !value.is_null());
    if resource.is_some() as u8
        + multipart.is_some() as u8
        + params.get("body").is_some_and(|value| !value.is_null()) as u8
        > 1
    {
        return Err(super::wire::error("请求正文不能同时使用多种传输形式"));
    }
    if let Some(resource) = resource {
        context.permission("resources.v1")?;
        return Ok(Some(Body::Bytes(
            context
                .blobs
                .raw(resource.as_str().ok_or_else(super::errors::protocol)?)?,
        )));
    }
    let Some(multipart) = multipart else {
        return Ok(None);
    };
    context.permission("resources.v1")?;
    Ok(Some(Body::Multipart(form(&context.blobs, multipart)?)))
}

fn form(
    resources: &super::resources::Resources,
    multipart: &Value,
) -> Result<Vec<(String, MultipartPart)>, WireError> {
    let fields = multipart["fields"]
        .as_object()
        .filter(|fields| fields.len() <= 64)
        .ok_or_else(super::errors::protocol)?;
    let files = multipart["files"]
        .as_array()
        .filter(|files| files.len() <= 16)
        .ok_or_else(super::errors::protocol)?;
    let mut total = 0;
    let mut parts = Vec::new();
    for (key, value) in fields {
        if !name(key) || !value.is_string() {
            return Err(super::errors::protocol());
        }
        let text = value.as_str().unwrap_or("");
        total += text.len();
        if total > 1024 * 1024 {
            return Err(super::wire::error("上传文本字段过大"));
        }
        parts.push((
            key.clone(),
            MultipartPart {
                filename: None,
                content_type: "text/plain".into(),
                bytes: text.as_bytes().to_vec(),
                is_text: true,
            },
        ));
    }
    for file in files {
        let field = file["field"].as_str().unwrap_or("");
        let filename = file["filename"].as_str().unwrap_or("");
        if !name(field) || !name(filename) {
            return Err(super::errors::protocol());
        }
        let bytes = resources.raw(
            file["resource"]
                .as_str()
                .ok_or_else(super::errors::protocol)?,
        )?;
        total += bytes.len();
        if total > 96 * 1024 * 1024 {
            return Err(super::wire::error("上传总量超过资源限制"));
        }
        parts.push((
            field.to_string(),
            MultipartPart {
                filename: Some(filename.to_string()),
                content_type: file["content_type"]
                    .as_str()
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                bytes,
                is_text: false,
            },
        ));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_name_charset() {
        assert!(name("file"));
        assert!(name("a.b-c_d[0]"));
        assert!(!name(""));
        assert!(!name("has space"));
        assert!(!name(&"x".repeat(257)));
    }
}
