// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/icons/formats.rs
//!
//! 图标引用与图片字节的**纯格式判定**（无 IO、无网络）。
//!
//! 只接受两类引用：
//! - `data:image/...`（内嵌，≤ 1.5MiB，可 base64 或百分号编码）；
//! - `https://` URL（无凭据/无 fragment，≤8KiB）。
//!
//! 落盘文件名形如 `{sha256}.{ext}`，`read` 侧据此校验内容未被篡改。

use base64::Engine;

/// 单张图片字节上限。
pub const MAX_BYTES: usize = 1024 * 1024;

/// 单个引用（含 data URL）的字符上限。
pub const MAX_REFERENCE: usize = 3 << 19;

/// 归一一个「自有」图标引用（data URL 或 https URL）。
pub fn own(value: &str) -> Option<String> {
    let value = value.trim();
    if value.len() > MAX_REFERENCE {
        return None;
    }
    if value
        .get(..11)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/"))
    {
        return Some(value.to_string());
    }
    let url = url::Url::parse(value).ok()?;
    (value.len() <= 8192
        && url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then(|| url.to_string())
}

/// Magpie 市场图标引用 → 可抓取的 URL 候选。
///
/// registry.json 的 `icon` 有三种形态（2026-11-20 实测 126 条）：完整
/// `https://…`（31 条，走 [`own`] 原样接受）、内嵌 `data:`（3 条）、以及
/// **裸名字**（26 条，如 `githubcopilot` / `kiro-color`）——后者是 Magpie
/// 自带的图标资源名，相对上游 GUI 的 `internal/gui/assets/icons/` 目录。
///
/// 裸名字若原样透会给前端（`<img src="githubcopilot">` 解析成管理面自身
/// 的相对路径 → 404），这正是「商店图标不显示」的根因之一。这里把裸名字
/// 展开成 svg/png 两个候选（同名资源可能只存在其一），按序由前端回退。
///
/// 名字字符集钉死为字母数字与 `-_`（防把奇怪字符串拼进 URL）；`generic`
/// 是上游的占位默认图标，没有意义，丢弃。
pub fn market(value: &str) -> Vec<String> {
    if let Some(value) = own(value) {
        return vec![value];
    }
    let name = value.trim();
    if name.is_empty()
        || name.len() > 80
        || name == "generic"
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Vec::new();
    }
    ["svg", "png"]
        .into_iter()
        .map(|ext| {
            format!(
                "https://raw.githubusercontent.com/yetone/magpie/main/internal/gui/assets/icons/{name}.{ext}"
            )
        })
        .collect()
}

/// 百分号解码（data URL 的非 base64 分支）。
fn percent_decode(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(value.len().min(MAX_BYTES));
    let mut bytes = value.as_bytes().iter().copied();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        };
        output.push(byte);
        if output.len() > MAX_REFERENCE {
            return None;
        }
    }
    Some(output)
}

/// 解出内嵌 data URL 的字节。
pub fn inline(value: &str) -> Option<Vec<u8>> {
    if value.len() > MAX_REFERENCE {
        return None;
    }
    let (header, data) = value.split_once(',')?;
    if !header
        .get(..11)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:image/"))
    {
        return None;
    }
    let decoded = percent_decode(data)?;
    let bytes = if header
        .split(';')
        .any(|part| part.eq_ignore_ascii_case("base64"))
    {
        let encoded: Vec<_> = decoded
            .into_iter()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?
    } else {
        decoded
    };
    (!bytes.is_empty() && bytes.len() <= MAX_BYTES).then_some(bytes)
}

/// 按文件头判定图片扩展名。
pub fn extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return None;
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("jpg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some("webp");
    }
    if bytes.starts_with(b"\0\0\x01\0") {
        return Some("ico");
    }
    let mut text = std::str::from_utf8(bytes)
        .ok()?
        .trim_start_matches('\u{feff}')
        .trim_start();
    if text.contains('\0') {
        return None;
    }
    if text.starts_with("<?xml") {
        text = text.split_once("?>")?.1.trim_start();
    }
    while text.starts_with("<!--") {
        text = text.split_once("-->")?.1.trim_start();
    }
    if text.starts_with("<svg")
        && text
            .as_bytes()
            .get(4)
            .is_some_and(|byte| byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/'))
    {
        Some("svg")
    } else {
        None
    }
}

/// 扩展名 → MIME。
pub fn mime(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

/// 文件名是否合法（`{64 hex}.{known ext}`）。
pub fn filename(name: &str) -> bool {
    name.split_once('.').is_some_and(|(hash, extension)| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && mime(extension).is_some()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_resolves_bare_names_and_keeps_absolute() {
        // 绝对 https / data URI 原样接受（单候选）
        assert_eq!(
            market("https://example.com/a.svg"),
            vec!["https://example.com/a.svg".to_string()]
        );
        assert_eq!(
            market("data:image/png;base64,AAA"),
            vec!["data:image/png;base64,AAA".to_string()]
        );
        // 裸名字 → svg/png 两个上游资源候选
        let candidates = market("kiro-color");
        assert_eq!(candidates.len(), 2);
        assert!(candidates[0].ends_with("/icons/kiro-color.svg"));
        assert!(candidates[1].ends_with("/icons/kiro-color.png"));
        // 非法/占位一律空
        assert!(market("").is_empty());
        assert!(market("generic").is_empty());
        assert!(market("../etc/passwd").is_empty());
        assert!(market("a b").is_empty());
        assert!(market("http://example.com/a.png").is_empty(), "只收 https");
    }

    #[test]
    fn own_accepts_data_and_https_only() {
        assert!(own("data:image/png;base64,AAA").is_some());
        assert!(own("https://example.com/a.png").is_some());
        assert!(own("http://example.com/a.png").is_none(), "只收 https");
        assert!(own("https://user:pw@example.com/a.png").is_none());
        assert!(own("https://example.com/a.png#x").is_none());
        assert!(own("not a url").is_none());
    }

    #[test]
    fn inline_decodes_base64_and_percent() {
        let png = b"\x89PNG\r\n\x1a\nxxxx";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let url = format!("data:image/png;base64,{b64}");
        assert_eq!(inline(&url).as_deref(), Some(&png[..]));
        // 百分号编码（非 base64）
        let percent = "data:image/png,%89PNG%0D%0A%1A%0Axxxx";
        assert_eq!(inline(percent).as_deref(), Some(&png[..]));
        assert!(inline("data:text/plain,abc").is_none());
    }

    #[test]
    fn extension_detects_common_formats() {
        assert_eq!(extension(b"\x89PNG\r\n\x1a\nrest"), Some("png"));
        assert_eq!(extension(b"\xff\xd8\xffrest"), Some("jpg"));
        assert_eq!(extension(b"GIF89a...."), Some("gif"));
        assert_eq!(extension(b"RIFF\0\0\0\0WEBPVP8 "), Some("webp"));
        assert_eq!(extension(b"<svg xmlns=\"...\"></svg>"), Some("svg"));
        assert_eq!(extension(b"not an image"), None);
    }

    #[test]
    fn filename_requires_digest_and_known_ext() {
        let name = format!("{}.png", "a".repeat(64));
        assert!(filename(&name));
        assert!(!filename(&format!("{}.bmp", "a".repeat(64))));
        assert!(!filename(&format!("{}.png", "a".repeat(63))));
        assert!(!filename("../etc/passwd"));
    }
}
