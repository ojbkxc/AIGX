//! 破甲技能包导入：把 ZIP / TAR / TGZ 归档安全解压到破甲技能目录。
//!
//! 参照 Agent2API `dsh-purge` 社区版 `lib/tar-import.js` 与 `lib/zip-extract.js`
//! 的**安全语义**（AGPL-3.0，Copyright (c) 2026 上游作者；此处仅对齐可观察的
//! 安全契约，Rust 代码为 AIGX 独立书写）。可观察契约：
//!
//! - 只接受普通文件与目录；符号链接 / 硬链接 / 设备等一律拒绝；
//! - 拒绝绝对路径、盘符、`..`、控制字符/冒号、Windows 保留名（con/prn/aux/nul/
//!   com1-9/lpt1-9）、尾随点或空格；
//! - 拒绝重复路径；
//! - 条目数上限 + 解压后总字节上限（防 zip / 压缩炸弹）；
//! - 先解压到技能目录下的临时目录，校验通过再**原子 rename** 到最终目录；任一步
//!   失败清理临时目录，绝不留下半成品；
//! - 归档内必须至少含一个 `SKILL.md`，否则拒绝——避免「导入成功却永不生效」
//!   的半死态（`skill_inject` 只扫 `SKILL.md`）。
//!
//! 流程与 AIGX 现有 `presets::apply(SkillFile)` 一致：最终落点是
//! `<skill_dir>/<name>/...`，`name` 为单级安全目录名。

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};

/// 单个归档输入上限（压缩态字节）。
pub const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
/// 解压后总字节上限。
pub const MAX_UNCOMPRESSED_BYTES: u64 = 128 * 1024 * 1024;
/// 归档条目数上限。
pub const MAX_ENTRIES: usize = 4096;
/// 单个路径分量长度上限。
const MAX_COMPONENT_LEN: usize = 200;
/// 递归查找 `SKILL.md` 的深度上限。
const MAX_SKILL_SCAN_DEPTH: usize = 8;

/// 归档格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    Tgz,
}

impl ArchiveFormat {
    /// 按魔数识别归档格式（ZIP / gzip / tar）。
    pub fn detect(bytes: &[u8]) -> Result<Self> {
        if bytes.len() >= 4 && &bytes[..2] == b"PK" {
            return Ok(Self::Zip);
        }
        if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            return Ok(Self::Tgz);
        }
        if bytes.len() > 262 && &bytes[257..262] == b"ustar" {
            return Ok(Self::Tar);
        }
        if tar_header_checksum_ok(bytes) {
            return Ok(Self::Tar);
        }
        bail!("无法识别的归档格式（仅支持 ZIP / TAR / TGZ）")
    }
}

/// 导入结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportReport {
    /// 实际落盘的目录名（单级，位于技能目录下）。
    pub dir: String,
    /// 写入的普通文件数。
    pub files: usize,
    /// 解压后总字节数。
    pub bytes: u64,
    /// 是否包含 `SKILL.md`（成功导入时恒为 true）。
    pub has_skill_md: bool,
}

/// 导入错误分类（决定 HTTP 状态）。
#[derive(Debug)]
pub enum ImportError {
    /// 目标目录已存在且未要求覆盖（409）。
    Exists(String),
    /// 超出大小/条目上限（413）。
    TooLarge(String),
    /// 归档内容不合法（400）。
    Invalid(String),
}

impl ImportError {
    pub fn message(&self) -> &str {
        match self {
            Self::Exists(m) | Self::TooLarge(m) | Self::Invalid(m) => m,
        }
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ImportError {}

/// 把内部 anyhow 错误归类：大小/条目超限 → TooLarge，其余 → Invalid。
fn classify(err: anyhow::Error) -> ImportError {
    let msg = format!("{err:#}");
    if msg.contains("大小上限") || msg.contains("条目数上限") {
        ImportError::TooLarge(msg)
    } else {
        ImportError::Invalid(msg)
    }
}

/// 单级目录名是否安全（与 `presets::is_safe_segment` 同口径：`[a-z0-9-]`）。
pub fn is_safe_dir_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// 把任意字符串规整成安全的单级目录名：小写、非 `[a-z0-9]` 折叠为 `-`、
/// 去首尾 `-`、截断 64。返回 None 表示规整后为空。
pub fn slugify(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 64 {
            break;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if is_safe_dir_name(&trimmed) {
        Some(trimmed)
    } else {
        None
    }
}

/// 从上传文件名推导目录名（去扩展名后规整）。
pub fn derive_dir_name(filename: &str) -> Option<String> {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);
    slugify(stem)
}

/// 导入归档到 `skill_root/<dir_name>`。
///
/// - `dir_name_input` 会先经 [`slugify`] 规整；规整后为空 → 报错。
/// - 目标目录已存在且 `overwrite=false` → [`ImportError::Exists`]。
/// - 成功返回实际写入统计。
pub fn import_archive(
    bytes: &[u8],
    format: ArchiveFormat,
    skill_root: &Path,
    dir_name_input: &str,
    overwrite: bool,
) -> std::result::Result<ImportReport, ImportError> {
    if bytes.is_empty() {
        return Err(ImportError::Invalid("归档为空".to_string()));
    }
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(ImportError::TooLarge(format!(
            "归档超过 {} MiB 上限",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        )));
    }
    let dir_name = slugify(dir_name_input).ok_or_else(|| {
        ImportError::Invalid("目录名为空或不合法（仅允许字母/数字/连字符）".to_string())
    })?;

    std::fs::create_dir_all(skill_root)
        .with_context(|| format!("创建技能目录失败：{}", skill_root.display()))
        .map_err(classify)?;

    let final_dir = skill_root.join(&dir_name);
    if final_dir.exists() && !overwrite {
        return Err(ImportError::Exists(format!(
            "目标目录已存在：{dir_name}（如需覆盖请传 overwrite=true）"
        )));
    }

    // 临时暂存目录（同分区，保证 rename 原子）。
    let staging = skill_root.join(format!(".import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&staging)
        .with_context(|| format!("创建暂存目录失败：{}", staging.display()))
        .map_err(classify)?;

    let extracted: Result<(usize, u64)> = match format {
        ArchiveFormat::Zip => extract_zip(bytes, &staging),
        ArchiveFormat::Tar => extract_tar(Cursor::new(bytes), &staging),
        ArchiveFormat::Tgz => {
            // 对解压流加原始上限，防 gzip 炸弹在 tar 解析前爆内存。
            let cap = MAX_UNCOMPRESSED_BYTES + 16 * 1024 * 1024;
            let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
            extract_tar(decoder.take(cap), &staging)
        }
    };

    let (files, total) = match extracted {
        Ok(v) => v,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(classify(e));
        }
    };

    if files == 0 {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ImportError::Invalid("归档内没有可导入的文件".to_string()));
    }
    if !has_skill_md(&staging, 0) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(ImportError::Invalid(
            "归档内未找到 SKILL.md（破甲技能注入需要入口文件）".to_string(),
        ));
    }

    // 落位。覆盖前校验目标是普通目录（不跟随符号链接）。
    if final_dir.exists() {
        let meta = std::fs::symlink_metadata(&final_dir).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            classify(e.into())
        })?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(ImportError::Exists(format!(
                "目标路径不是普通目录，拒绝覆盖：{dir_name}"
            )));
        }
        if let Err(e) = std::fs::remove_dir_all(&final_dir) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(classify(e.into()));
        }
    }
    if let Err(e) = std::fs::rename(&staging, &final_dir) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(classify(e.into()));
    }

    Ok(ImportReport {
        dir: dir_name,
        files,
        bytes: total,
        has_skill_md: true,
    })
}

// ── 解压实现 ────────────────────────────────────────────────

fn extract_zip(bytes: &[u8], staging: &Path) -> Result<(usize, u64)> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("ZIP 解析失败")?;
    if archive.len() > MAX_ENTRIES {
        bail!("归档条目数超过 {} 上限", MAX_ENTRIES);
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut files = 0usize;
    let mut total = 0u64;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("ZIP 条目读取失败")?;
        let raw_name = entry.name().to_string();

        // 拒绝符号链接（unix mode 的 S_IFLNK 位）。
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                bail!("归档含符号链接，拒绝解压：{raw_name}");
            }
        }

        let rel = safe_rel_path(&raw_name)?;
        if !seen.insert(rel.clone()) {
            bail!("归档含重复路径：{}", rel.display());
        }
        let is_dir = entry.is_dir() || raw_name.ends_with('/');
        if is_dir {
            let dir = safe_join(staging, &rel)?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建目录失败：{}", dir.display()))?;
            continue;
        }
        if files >= MAX_ENTRIES {
            bail!("归档条目数超过 {} 上限", MAX_ENTRIES);
        }

        let declared = entry.size();
        total = total
            .checked_add(declared)
            .filter(|t| *t <= MAX_UNCOMPRESSED_BYTES)
            .ok_or_else(|| anyhow::anyhow!("解压后超出大小上限"))?;

        let out = safe_join(staging, &rel)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .with_context(|| format!("写入文件失败：{}", out.display()))?;
        copy_bounded(&mut entry, &mut writer, declared)?;
        files += 1;
    }

    Ok((files, total))
}

fn extract_tar<R: Read>(reader: R, staging: &Path) -> Result<(usize, u64)> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut files = 0usize;
    let mut total = 0u64;

    for entry in archive.entries().context("TAR 解析失败")? {
        let mut entry = entry.context("TAR 条目读取失败")?;
        let entry_type = entry.header().entry_type();
        let raw_name = entry
            .path()
            .context("TAR 条目路径读取失败")?
            .to_string_lossy()
            .to_string();

        // 仅允许普通文件与目录；链接/设备等一律拒绝。
        if !(entry_type.is_file() || entry_type.is_dir()) {
            bail!("归档含非常规条目（链接/设备等），拒绝解压：{raw_name}");
        }

        let rel = safe_rel_path(&raw_name)?;
        if !seen.insert(rel.clone()) {
            bail!("归档含重复路径：{}", rel.display());
        }
        if entry_type.is_dir() {
            let dir = safe_join(staging, &rel)?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("创建目录失败：{}", dir.display()))?;
            continue;
        }
        if files >= MAX_ENTRIES {
            bail!("归档条目数超过 {} 上限", MAX_ENTRIES);
        }

        let declared = entry.size();
        total = total
            .checked_add(declared)
            .filter(|t| *t <= MAX_UNCOMPRESSED_BYTES)
            .ok_or_else(|| anyhow::anyhow!("解压后超出大小上限"))?;

        let out = safe_join(staging, &rel)?;
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .with_context(|| format!("写入文件失败：{}", out.display()))?;
        copy_bounded(&mut entry, &mut writer, declared)?;
        files += 1;
    }

    Ok((files, total))
}

/// 有界拷贝：实际读出的字节数不得超过 `limit`（防条目声明的 size 与实际不符）。
fn copy_bounded<R: Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    limit: u64,
) -> Result<u64> {
    let mut buf = [0u8; 64 * 1024];
    let mut written = 0u64;
    loop {
        let n = reader.read(&mut buf).context("读取条目数据失败")?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > limit {
            bail!("条目实际大小超出声明，拒绝解压");
        }
        writer.write_all(&buf[..n]).context("写入条目数据失败")?;
    }
    Ok(written)
}

// ── 路径安全 ────────────────────────────────────────────────

/// 把归档内的相对路径规整为安全的相对 `PathBuf`。
///
/// 拒绝：绝对路径、盘符、`..`/`.` 分量、空分量、控制字符/冒号、Windows 保留名、
/// 尾随点或空格、超长分量。
fn safe_rel_path(raw: &str) -> Result<PathBuf> {
    let normalized = raw.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    if normalized.starts_with('/') {
        bail!("归档含绝对路径，拒绝解压：{raw}");
    }
    let trimmed = normalized.trim_end_matches('/');
    if trimmed.is_empty() {
        bail!("归档含空路径条目");
    }
    let mut out = PathBuf::new();
    for part in trimmed.split('/') {
        validate_component(part, raw)?;
        out.push(part);
    }
    Ok(out)
}

fn validate_component(part: &str, raw: &str) -> Result<()> {
    if part.is_empty() {
        bail!("归档含空路径分量，拒绝解压：{raw}");
    }
    if part == "." || part == ".." {
        bail!("归档含相对父路径，拒绝解压：{raw}");
    }
    if part.chars().any(|c| (c as u32) < 0x20 || c == ':') {
        bail!("归档路径含非法字符，拒绝解压：{raw}");
    }
    if part.ends_with('.') || part.ends_with(' ') {
        bail!("归档路径以点或空格结尾，拒绝解压：{raw}");
    }
    if part.len() > MAX_COMPONENT_LEN {
        bail!("归档路径分量过长，拒绝解压：{raw}");
    }
    if is_windows_reserved(part) {
        bail!("归档路径命中 Windows 保留名，拒绝解压：{raw}");
    }
    Ok(())
}

fn is_windows_reserved(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).to_ascii_lowercase();
    matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || (stem.len() == 4
            && (stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
}

/// 把已校验的相对路径拼到根目录下，并做词法级包含性校验（纵深防御）。
fn safe_join(root: &Path, rel: &Path) -> Result<PathBuf> {
    for component in rel.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("路径含非法分量，拒绝解压：{}", rel.display());
        }
    }
    let out = root.join(rel);
    if !out.starts_with(root) {
        bail!("路径越界，拒绝解压：{}", rel.display());
    }
    Ok(out)
}

fn has_skill_md(dir: &Path, depth: usize) -> bool {
    if depth > MAX_SKILL_SCAN_DEPTH {
        return false;
    }
    let Ok(reader) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in reader.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if has_skill_md(&path, depth + 1) {
                return true;
            }
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case("SKILL.md"))
        {
            return true;
        }
    }
    false
}

fn tar_header_checksum_ok(buf: &[u8]) -> bool {
    if buf.len() < 512 {
        return false;
    }
    let stored = String::from_utf8_lossy(&buf[148..156]);
    let stored = stored.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    let Ok(expected) = u32::from_str_radix(stored, 8) else {
        return false;
    };
    let mut sum: u32 = 0;
    for (idx, &byte) in buf[..512].iter().enumerate() {
        sum += if (148..156).contains(&idx) {
            0x20
        } else {
            byte as u32
        };
    }
    sum == expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aigx-skill-import-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zip_with(entries: &[(&str, &[u8], Option<u32>)]) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buf);
            for (name, data, mode) in entries {
                let mut opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
                if let Some(mode) = mode {
                    opts = opts.unix_permissions(*mode);
                }
                writer.start_file(*name, opts).unwrap();
                writer.write_all(data).unwrap();
            }
            writer.finish().unwrap();
        }
        buf.into_inner()
    }

    fn tar_with(entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(*kind);
            header.set_cksum();
            builder.append_data(&mut header, *name, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    #[test]
    fn slugify_and_derive() {
        assert_eq!(slugify("My Skills Pack").as_deref(), Some("my-skills-pack"));
        assert_eq!(slugify("../../etc").as_deref(), Some("etc"));
        assert!(slugify("   ").is_none());
        assert!(slugify("!!!").is_none());
        assert_eq!(
            derive_dir_name("Solana Agent Kit.zip").as_deref(),
            Some("solana-agent-kit")
        );
        assert!(is_safe_dir_name("ok-name-1"));
        assert!(!is_safe_dir_name("-bad"));
        assert!(!is_safe_dir_name("UPPER"));
        assert!(!is_safe_dir_name("a/b"));
    }

    #[test]
    fn import_valid_zip_with_nested_skill_md() {
        let root = temp_root("zip-ok");
        let bytes = zip_with(&[
            ("README.md", b"hello", None),
            ("nested/SKILL.md", b"# skill", None),
        ]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        assert_eq!(fmt, ArchiveFormat::Zip);
        let report = import_archive(&bytes, fmt, &root, "My Skill", false).unwrap();
        assert_eq!(report.dir, "my-skill");
        assert!(report.has_skill_md);
        assert_eq!(report.files, 2);
        assert!(root.join("my-skill/nested/SKILL.md").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn import_valid_tar() {
        let root = temp_root("tar-ok");
        let bytes = tar_with(&[
            ("SKILL.md", b"body", tar::EntryType::Regular),
            ("dir/", b"", tar::EntryType::Directory),
        ]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        assert_eq!(fmt, ArchiveFormat::Tar);
        let report = import_archive(&bytes, fmt, &root, "tar-pack", false).unwrap();
        assert!(root.join("tar-pack/SKILL.md").exists());
        assert_eq!(report.files, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tgz_roundtrip() {
        let root = temp_root("tgz-ok");
        // 先造 tar，再 gzip。
        let tar_bytes = tar_with(&[("SKILL.md", b"tgz body", tar::EntryType::Regular)]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&tar_bytes).unwrap();
        let gz = encoder.finish().unwrap();
        let fmt = ArchiveFormat::detect(&gz).unwrap();
        assert_eq!(fmt, ArchiveFormat::Tgz);
        let report = import_archive(&gz, fmt, &root, "tgz-pack", false).unwrap();
        assert_eq!(report.dir, "tgz-pack");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_missing_skill_md() {
        let root = temp_root("no-skill");
        let bytes = zip_with(&[("README.md", b"x", None)]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        let err = import_archive(&bytes, fmt, &root, "pack", false).unwrap_err();
        assert!(matches!(err, ImportError::Invalid(_)));
        assert!(err.message().contains("SKILL.md"));
        // 失败后不留残留（暂存目录已清理）。
        assert!(std::fs::read_dir(&root).unwrap().next().is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_path_traversal() {
        let root = temp_root("traversal");
        let bytes = zip_with(&[("../escape/SKILL.md", b"x", None)]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        let err = import_archive(&bytes, fmt, &root, "pack", false).unwrap_err();
        assert!(matches!(err, ImportError::Invalid(_)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_symlink_tar_entry() {
        let root = temp_root("symlink-tar");
        let bytes = tar_with(&[("link", b"/etc/passwd", tar::EntryType::Symlink)]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        let err = import_archive(&bytes, fmt, &root, "pack", false).unwrap_err();
        assert!(err.message().contains("非常规条目"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_duplicate_paths() {
        let root = temp_root("dup");
        // zip 写入器拒绝重复文件名，用 tar 造重复条目。
        let bytes = tar_with(&[
            ("SKILL.md", b"a", tar::EntryType::Regular),
            ("SKILL.md", b"b", tar::EntryType::Regular),
        ]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        let err = import_archive(&bytes, fmt, &root, "pack", false).unwrap_err();
        assert!(matches!(err, ImportError::Invalid(_)));
        assert!(err.message().contains("重复路径"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn overwrite_policy() {
        let root = temp_root("overwrite");
        let bytes = zip_with(&[("SKILL.md", b"v1", None)]);
        let fmt = ArchiveFormat::detect(&bytes).unwrap();
        import_archive(&bytes, fmt, &root, "pack", false).unwrap();
        // 再次导入，不覆盖 → Exists
        let err = import_archive(&bytes, fmt, &root, "pack", false).unwrap_err();
        assert!(matches!(err, ImportError::Exists(_)));
        // 覆盖 → 成功
        let bytes2 = zip_with(&[("SKILL.md", b"v2", None)]);
        let fmt2 = ArchiveFormat::detect(&bytes2).unwrap();
        import_archive(&bytes2, fmt2, &root, "pack", true).unwrap();
        let body = std::fs::read_to_string(root.join("pack/SKILL.md")).unwrap();
        assert_eq!(body, "v2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_empty_archive_and_unknown_format() {
        assert!(matches!(
            import_archive(&[], ArchiveFormat::Zip, Path::new("/tmp/x"), "a", false),
            Err(ImportError::Invalid(_))
        ));
        assert!(ArchiveFormat::detect(b"not an archive at all........").is_err());
    }

    #[test]
    fn windows_reserved_and_trailing_rules() {
        assert!(is_windows_reserved("CON"));
        assert!(is_windows_reserved("lpt1"));
        assert!(is_windows_reserved("aux.txt"));
        assert!(!is_windows_reserved("console"));
        assert!(!is_windows_reserved("com0"));
        assert!(safe_rel_path("dir/file.md").is_ok());
        assert!(safe_rel_path("/abs/SKILL.md").is_err());
        assert!(safe_rel_path("C:/win/SKILL.md").is_err());
        assert!(safe_rel_path("dir/../escape").is_err());
        assert!(safe_rel_path("dir\\..\\escape").is_err());
        assert!(safe_rel_path("trail./x").is_err());
        assert!(safe_rel_path("nul").is_err());
    }
}
