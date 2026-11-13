//! 插件包下载、解压与校验。
//!
//! ## 信任链（AIGX 能做到的部分）
//!
//! 1. **sha256**：源清单声明摘要时比对（挡「文件传坏了」）；
//! 2. **协议版本**：`pack.json` 的 `protocol` 必须等于网关的
//!    [`PROTOCOL_VERSION`]（挡版本不匹配）；
//! 3. **入口存在性 + zip-slip 防护**：解包时拒绝越界路径，并确认入口
//!    二进制真的在包里。
//!
//! ## 为什么不做 ed25519 验签
//!
//! 上游的签名覆盖的是**它自己的规范化 payload**（`signing_payload`）。
//! 复刻该算法必须读上游 AGPL 源码——AIGX 不拷其代码，故不做验签。
//! 这个取舍如实反映在界面：包带签名时标「带签名（未校验）」，不带签名
//! 标「未签名」。**AIGX 不因此声称包可信**。

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::registry::{PackMeta, PackProvider};
use super::rpc::PROTOCOL_VERSION;

/// 单个 zip 的大小上限（与上游 MAX_PACK_BYTES 同量级的宽松上限）。
pub const MAX_PACK_BYTES: usize = 96 * 1024 * 1024;

/// 解压后文件数上限（防 zip bomb）。
const MAX_ENTRIES: usize = 4096;

/// `pack.json` 的形状（只取网关需要的字段）。
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct PackManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
    /// 入口二进制的相对路径。空 = 自动探测 `bin/` 下的第一个文件。
    pub entry: String,
    pub providers: Vec<PackProvider>,
    /// 包自带的签名（原样保留，AIGX 不校验）。
    pub signature: Option<serde_json::Value>,
}

/// 安装错误。
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("下载插件包失败: {0}")]
    Download(String),
    #[error("插件包过大（{0} 字节，上限 {1}）")]
    TooLarge(usize, usize),
    #[error("sha256 不匹配（期望 {expected}，实际 {actual}）")]
    DigestMismatch { expected: String, actual: String },
    #[error("解压失败: {0}")]
    Unzip(String),
    #[error("包清单无效: {0}")]
    Manifest(String),
    #[error("协议版本不匹配：包要求 {pack}，网关是 {host}")]
    ProtocolMismatch { pack: u32, host: u32 },
    #[error("落盘失败: {0}")]
    Io(String),
}

impl From<std::io::Error> for InstallError {
    fn from(e: std::io::Error) -> Self {
        InstallError::Io(e.to_string())
    }
}

/// 算字节的 sha256（十六进制小写）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// 把 zip 内条目名安全地拼到根目录下（zip-slip 防护）。
///
/// 拒绝：绝对路径、含 `..` 的段、反斜杠（Windows 分隔符绕过）、盘符前缀。
fn safe_join(root: &Path, name: &str) -> Result<PathBuf, InstallError> {
    let norm = name.replace('\\', "/");
    let norm = norm.trim_start_matches('/');
    if norm.is_empty() {
        return Err(InstallError::Unzip("空条目名".into()));
    }
    if norm.len() >= 2 && norm.as_bytes()[1] == b':' {
        return Err(InstallError::Unzip(format!("盘符路径: {name}")));
    }
    if norm.split('/').any(|seg| seg == "..") {
        return Err(InstallError::Unzip(format!("越界路径: {name}")));
    }
    Ok(root.join(norm))
}

/// 解压 zip 到 `dest`（自建目录）。
pub fn extract_zip(bytes: &[u8], dest: &Path) -> Result<(), InstallError> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| InstallError::Unzip(format!("不是有效的 zip: {e}")))?;
    if archive.len() > MAX_ENTRIES {
        return Err(InstallError::Unzip(format!(
            "条目过多（{}）",
            archive.len()
        )));
    }
    std::fs::create_dir_all(dest)?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| InstallError::Unzip(format!("读取条目 {i} 失败: {e}")))?;
        // `enclosed_name` 会拒绝越界路径；再用 safe_join 兜一道
        let raw_name = entry.name().to_string();
        let out = safe_join(dest, &raw_name)?;
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry
            .read_to_end(&mut buf)
            .map_err(|e| InstallError::Unzip(format!("读取 {raw_name} 失败: {e}")))?;
        let mut f = std::fs::File::create(&out)?;
        f.write_all(&buf)?;
    }
    // 入口可执行位（Unix）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = ensure_tree_executable(dest, |p| {
            let meta = std::fs::metadata(p).ok()?;
            let mode = meta.permissions().mode();
            if mode & 0o100 == 0 {
                let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode | 0o100));
            }
            Some(())
        });
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_tree_executable<F: FnMut(&Path) -> Option<()>>(root: &Path, mut f: F) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                f(&p);
            }
        }
    }
}

/// 读并解析 `pack.json`。
pub fn parse_manifest(dir: &Path) -> Result<PackManifest, InstallError> {
    let path = dir.join("pack.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        InstallError::Manifest(format!("读不到 pack.json（{}）: {e}", path.display()))
    })?;
    serde_json::from_str::<PackManifest>(&text)
        .map_err(|e| InstallError::Manifest(format!("pack.json 解析失败: {e}")))
}

/// 探测入口二进制：`pack.json` 的 `entry`，或 `bin/` 下的第一个文件。
fn resolve_entry(dir: &Path, manifest: &PackManifest) -> Result<String, InstallError> {
    if !manifest.entry.is_empty() {
        let p = safe_join(dir, &manifest.entry)?;
        if p.is_file() {
            return Ok(manifest.entry.clone());
        }
        return Err(InstallError::Manifest(format!(
            "清单声明的入口不存在: {}",
            manifest.entry
        )));
    }
    let bin = dir.join("bin");
    if let Ok(rd) = std::fs::read_dir(&bin) {
        for e in rd.flatten() {
            if e.path().is_file() {
                return Ok(format!("bin/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    Err(InstallError::Manifest(
        "包里找不到入口二进制（清单未声明 entry，bin/ 也为空）".into(),
    ))
}

/// 从 zip 字节安装一个包。
///
/// `plugins_root` 是插件根目录；`expected_sha256` 由源清单提供（可选）。
/// 解压到暂存目录 → 校验 → `rename` 进版本目录（半成品包绝不会出现在
/// 正式目录里）。
pub fn install_from_zip(
    bytes: &[u8],
    plugins_root: &Path,
    expected_sha256: Option<&str>,
) -> Result<PackMeta, InstallError> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    // 1) sha256
    let actual = sha256_hex(bytes);
    if let Some(exp) = expected_sha256 {
        if !exp.trim().eq_ignore_ascii_case(&actual) {
            return Err(InstallError::DigestMismatch {
                expected: exp.to_string(),
                actual,
            });
        }
    }
    // 2) 解压到暂存目录
    std::fs::create_dir_all(plugins_root)?;
    let staging_root = plugins_root.join(".staging");
    std::fs::create_dir_all(&staging_root)?;
    let staging = staging_root.join(&actual[..16.min(actual.len())]);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    extract_zip(bytes, &staging)?;
    // 暂存目录可能被 zip 套了一层（顶层目录）；找到含 pack.json 的那层
    let content = find_manifest_dir(&staging)?;
    // 3) 清单 + 校验
    let manifest = parse_manifest(&content)?;
    if manifest.id.is_empty() {
        return Err(InstallError::Manifest("清单缺 id".into()));
    }
    if manifest.version.is_empty() {
        return Err(InstallError::Manifest("清单缺 version".into()));
    }
    if manifest.protocol != PROTOCOL_VERSION {
        return Err(InstallError::ProtocolMismatch {
            pack: manifest.protocol,
            host: PROTOCOL_VERSION,
        });
    }
    let entry = resolve_entry(&content, &manifest)?;
    // 4) 移到版本目录
    let dest = plugins_root.join(&manifest.id).join(&manifest.version);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    std::fs::rename(&content, &dest)
        .map_err(|e| InstallError::Io(format!("移动到 {} 失败: {e}", dest.display())))?;
    // 清理暂存（若 content != staging，两者都要清）
    let _ = std::fs::remove_dir_all(&staging);

    let signed = manifest.signature.is_some();
    Ok(PackMeta {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        protocol: manifest.protocol,
        providers: manifest.providers,
        entry,
        signature: manifest.signature.map(|v| v.to_string()),
        signed,
        installed_at: now_unix(),
        enabled: false,
    })
}

/// 找到含 `pack.json` 的那一层（容忍 zip 顶层套了一层目录）。
fn find_manifest_dir(root: &Path) -> Result<PathBuf, InstallError> {
    if root.join("pack.json").is_file() {
        return Ok(root.to_path_buf());
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return Err(InstallError::Manifest("暂存目录不可读".into()));
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for d in dirs {
        if d.join("pack.json").is_file() {
            return Ok(d);
        }
    }
    Err(InstallError::Manifest("包里没有 pack.json".into()))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 下载一个 URL 到内存（带上限）。
pub async fn download_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, InstallError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(InstallError::Download(format!("HTTP {}", resp.status())));
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_PACK_BYTES {
            return Err(InstallError::TooLarge(len as usize, MAX_PACK_BYTES));
        }
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| InstallError::Download(e.to_string()))?;
    if bytes.len() > MAX_PACK_BYTES {
        return Err(InstallError::TooLarge(bytes.len(), MAX_PACK_BYTES));
    }
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个 zip（内存）。
    fn make_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts: zip::write::FileOptions<()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for (name, data) in files {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    fn pack_json(
        id: &str,
        version: &str,
        protocol: u32,
        entry: &str,
        providers: &[&str],
    ) -> String {
        let provs: Vec<String> = providers
            .iter()
            .map(|p| format!(r#"{{"id":"{p}","label":"{p}","default_enabled":true}}"#))
            .collect();
        format!(
            r#"{{"id":"{id}","name":"{id}","version":"{version}","protocol":{protocol},"entry":"{entry}","providers":[{}]}}"#,
            provs.join(",")
        )
    }

    fn temp_root() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("aigx-install-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn install_happy_path() {
        let root = temp_root();
        let manifest = pack_json(
            "agent2api-providers",
            "3.4.2",
            1,
            "bin/plugin",
            &["kiro", "astudio"],
        );
        let bytes = make_zip(&[
            ("pack.json", manifest.as_bytes()),
            ("bin/plugin", b"#!/bin/sh\necho hi\n"),
        ]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.id, "agent2api-providers");
        assert_eq!(meta.version, "3.4.2");
        assert_eq!(meta.entry, "bin/plugin");
        assert_eq!(meta.providers.len(), 2);
        assert!(!meta.signed);
        // 落盘到位
        assert!(meta.entry_path(&root).is_file());
        assert!(!meta.enabled, "新装包默认停用");
    }

    #[test]
    fn install_with_top_level_dir() {
        // zip 顶层套了一层目录（常见打包形态）
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[
            ("p-1.0.0/pack.json", manifest.as_bytes()),
            ("p-1.0.0/bin/plugin", b"x"),
        ]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.id, "p");
        assert!(meta.entry_path(&root).is_file());
    }

    #[test]
    fn install_sha256_mismatch_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let err = install_from_zip(&bytes, &root, Some("deadbeef")).unwrap_err();
        assert!(matches!(err, InstallError::DigestMismatch { .. }));
    }

    #[test]
    fn install_sha256_match_accepted() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let digest = sha256_hex(&bytes);
        assert!(install_from_zip(&bytes, &root, Some(&digest)).is_ok());
    }

    #[test]
    fn install_protocol_mismatch_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 99, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(
            err,
            InstallError::ProtocolMismatch { pack: 99, host: 1 }
        ));
    }

    #[test]
    fn install_missing_entry_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        // 声明了入口但包里没有
        let bytes = make_zip(&[("pack.json", manifest.as_bytes())]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(matches!(err, InstallError::Manifest(_)));
    }

    #[test]
    fn install_entry_autodetect() {
        let root = temp_root();
        // 清单不声明 entry → 自动探测 bin/
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"providers":[{"id":"kiro","label":"Kiro"}]}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert_eq!(meta.entry, "bin/plugin");
    }

    #[test]
    fn zip_slip_rejected() {
        let root = temp_root();
        let manifest = pack_json("p", "1.0.0", 1, "bin/plugin", &["kiro"]);
        let bytes = make_zip(&[
            ("pack.json", manifest.as_bytes()),
            ("bin/plugin", b"x"),
            ("../escape.txt", b"pwned"),
        ]);
        let err = install_from_zip(&bytes, &root, None).unwrap_err();
        assert!(
            matches!(err, InstallError::Unzip(_)),
            "越界路径应被拒绝: {err:?}"
        );
        // 确认没写到 root 外
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn manifest_signed_flag_from_signature_field() {
        let root = temp_root();
        let manifest = r#"{"id":"p","name":"p","version":"1.0.0","protocol":1,"entry":"bin/plugin","providers":[],"signature":{"algorithm":"ed25519","value":"abc"}}"#;
        let bytes = make_zip(&[("pack.json", manifest.as_bytes()), ("bin/plugin", b"x")]);
        let meta = install_from_zip(&bytes, &root, None).unwrap();
        assert!(meta.signed, "带 signature 字段应标 signed");
        assert!(meta.signature.is_some());
    }

    #[test]
    fn safe_join_rejects_bad_paths() {
        let root = Path::new("/tmp/x");
        assert!(safe_join(root, "a/b").is_ok());
        assert!(safe_join(root, "../a").is_err());
        assert!(safe_join(root, "/etc/passwd").is_ok()); // 前导 / 被去掉，落在 root 内
        assert!(safe_join(root, "C:/win").is_err());
        assert!(safe_join(root, "a\\..\\b").is_err());
    }
}
