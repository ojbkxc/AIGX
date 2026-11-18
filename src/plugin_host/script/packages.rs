//! npm 风格脚本包：清单校验、入口解析、tarball 安全解包。
//!
//! ## 包形状
//!
//! 一个脚本包就是一个 npm 包：
//! ```text
//! package.json         name / version / (dependencies) / main|exports
//! dist/index.mjs       入口（可用 exports["./server"] / main 指定）
//! ```
//! 入口必须导出 OpenCode v1 的 `auth.provider`（运行时由 Bun 宿主校验，
//! 网关这里只做**静态**的「入口文件存在」检查——不执行第三方代码）。
//!
//! ## 安全边界（为什么要这么多限制）
//!
//! 包内容来自公网（npm / Magpie / GitHub），是**不可信输入**：
//! - 压缩体 ≤32MB、解包 ≤128MB、条目 ≤4096：防 zip/tar bomb 撑爆磁盘
//! - 必须单一根目录：npm tarball 的标准布局是 `package/…`，多根说明是
//!   手工拼的包，形态不可预期
//! - 拒绝符号链接 / 硬链接 / 绝对路径 / `..`：防解包越出目标目录
//! - 拒绝重复路径：同名条目后写覆盖前写，可绕过内容检查
//! - 拒绝含安装脚本的包不做拦截（依赖安装在 dependencies.rs 里用
//!   `--ignore-scripts` 兜底），但**入口必须真实存在**

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// 压缩体字节上限。
pub const MAX_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024;
/// 解包后总字节上限。
pub const MAX_UNPACKED_BYTES: u64 = 128 * 1024 * 1024;
/// tar 条目数上限。
pub const MAX_ENTRIES: usize = 4096;
/// package.json 字节上限（清单不该很大，防把它当数据通道）。
pub const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// 从 package.json 里读出的必要信息。
#[derive(Debug, Clone)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct Manifest {
    /// 包名（npm 口径）。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 入口相对路径（相对包根）。
    pub entry: String,
    /// 是否声明了依赖（决定要不要跑 `bun install`）。
    pub has_dependencies: bool,
}

/// 校验包名：npm 规则（小写、可 scoped），限制长度。
///
/// 为什么限 180：包名会成为目录哈希的输入与展示字段，过长无实际用途，
/// 只会放大日志/UI 的脏数据面。
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("包名为空");
    }
    if name.len() > 180 {
        bail!("包名过长：{} 字符（上限 180）", name.len());
    }
    let body = name.strip_prefix('@').unwrap_or(name);
    if body.is_empty() {
        bail!("包名无效：{name}");
    }
    for ch in body.chars() {
        if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_' | '.' | '/'))
        {
            bail!("包名含非法字符 {ch:?}（只允许小写字母数字与 -_.@/）：{name}");
        }
    }
    if body.starts_with('/') || body.ends_with('/') || body.contains("//") || body.contains("..") {
        bail!("包名路径形态非法：{name}");
    }
    Ok(())
}

/// 包 id 的目录名（与 `ScriptPackage::package_dir_name` 同一算法）。
///
/// 这里重复实现一次是为了让**尚未构造 ScriptPackage** 的调用方（解包前）
/// 也能算出目标目录；算法变更必须两处同时改。
pub fn dir_name_for(package_name: &str) -> String {
    let digest = Sha256::digest(package_name.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("script-{}", &hex[..24])
}

/// 版本串校验（入路径前）。
///
/// 字符集之外还必须拒掉**路径危险形态**：`unpack_tarball` 直接把 version
/// 拼进 `packages/{hash}/{version}`，而 `version=".."` 会让 dest 变成
/// `packages/{hash}/..`（= 整个 packages 根，`remove_dir_all` 会删掉全部
/// 已装包）；`version="."` 同理指向 hash 目录本身。合法 semver 不含连续
/// 点、不以点开头，所以收紧不会误伤真实包。
pub fn validate_version(version: &str) -> Result<()> {
    if version.is_empty() {
        bail!("版本号为空");
    }
    if version.len() > 128 {
        bail!("版本号过长：{} 字符", version.len());
    }
    if !version
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-'))
    {
        bail!("版本号含非法字符：{version}（只允许字母数字与 .+-）");
    }
    // 路径段安全：拒绝纯点段（"."/".."/"..."）与任何连续点（`..` 会
    // 被 Filesystem 解释为上跳；semver 本身不允许连续点）。
    if version.contains("..") {
        bail!("版本号含连续点，拒绝（可能是路径穿越）：{version}");
    }
    if version.starts_with('.') {
        bail!("版本号不得以 . 开头：{version}");
    }
    if !version.chars().any(|c| c.is_ascii_alphanumeric()) {
        bail!("版本号必须含字母或数字：{version}");
    }
    Ok(())
}

/// 解析 package.json 的入口与其依赖声明。
///
/// 入口优先级（对齐 OpenCode v1 约定）：`exports["./server"]` > `main` >
/// `exports["."]`（字符串或对象的 default）> `exports`（字符串）>
/// `index.{ts,tsx,js,mjs,cjs}`。
pub fn resolve_entry(manifest_json: &serde_json::Value) -> Result<String> {
    fn str_at(v: &serde_json::Value, path: &[&str]) -> Option<String> {
        let mut cur = v;
        for k in path {
            cur = cur.get(*k)?;
        }
        cur.as_str().map(str::to_string)
    }

    // exports 可能是字符串，也可能是 { ".": ... , "./server": ... }
    let from_exports_server = str_at(manifest_json, &["exports", "./server"]).or_else(|| {
        // "./server" 也可能是 { types, default } 形态
        manifest_json
            .get("exports")
            .and_then(|e| e.get("./server"))
            .and_then(|e| e.get("default"))
            .and_then(|d| d.as_str())
            .map(str::to_string)
    });

    let entry = from_exports_server
        .or_else(|| {
            manifest_json
                .get("main")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            manifest_json.get("exports").and_then(|e| {
                if let Some(s) = e.as_str() {
                    return Some(s.to_string());
                }
                e.get(".").and_then(|d| {
                    d.as_str().map(str::to_string).or_else(|| {
                        d.get("default")
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                    })
                })
            })
        });

    match entry {
        Some(e) if !e.is_empty() => Ok(e),
        _ => {
            // 没有显式入口时尝试约定文件名（存在性由调用方在解包后检查）。
            for candidate in [
                "index.ts",
                "index.tsx",
                "index.js",
                "index.mjs",
                "index.cjs",
            ] {
                if manifest_json
                    .get("__files")
                    .and_then(|f| f.as_array())
                    .is_some_and(|arr| arr.iter().any(|x| x.as_str() == Some(candidate)))
                {
                    return Ok(candidate.to_string());
                }
            }
            bail!("package.json 未声明入口（exports[\"./server\"] / main / index.*）");
        }
    }
}

/// 把清单里的入口串解析成**安全的相对路径**；绝对路径 / `..` / 盘符前缀
/// 一律拒绝（返回 None）。
///
/// 入口串来自 package.json（第三方内容）：`main`/`exports` 写 `../../x`
/// 或 `C:\x` 时，`root.join(entry)` 会逃出包目录（Windows 上 `join` 绝对
/// 路径直接替换 base）。解包通路还能靠「必须在 collected 里」兜住，但
/// Local 通路直接 `dir.join(entry).is_file()` 检查——不做这层校验会让
/// 包把入口指到目录外，宿主随之加载任意文件。
pub fn safe_entry_rel(entry: &str) -> Option<PathBuf> {
    let trimmed = entry.trim_start_matches("./");
    if trimmed.is_empty() {
        return None;
    }
    let p = PathBuf::from(trimmed);
    for c in p.components() {
        if !matches!(c, std::path::Component::Normal(_)) {
            return None;
        }
    }
    Some(p)
}

/// 解包 npm tarball（`.tgz`：gzip + tar），只接受单一根目录。
///
/// 返回解包后的包根路径（`{packages_root}/{dir_name}/{version}`）。
pub fn unpack_tarball(bytes: &[u8], package_name: &str, packages_root: &Path) -> Result<Manifest> {
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        bail!(
            "包体过大：{} bytes（上限 {MAX_ARCHIVE_BYTES}）",
            bytes.len()
        );
    }
    validate_name(package_name)?;

    let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);

    // 第一遍：只扫描元数据，确认布局与总量；不落盘。
    let mut total: u64 = 0;
    let mut count = 0usize;
    let mut roots: Option<String> = None;
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let entries = archive
        .entries()
        .context("读取 tar 条目失败（不是合法 tar.gz）")?;

    let mut collected: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut manifest_json: Option<serde_json::Value> = None;

    for entry in entries {
        let mut entry = entry.context("读取 tar 条目失败")?;
        count += 1;
        if count > MAX_ENTRIES {
            bail!("tar 条目过多（上限 {MAX_ENTRIES}）");
        }
        let header = entry.header();
        let kind = header.entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            bail!("包内含链接条目，拒绝（防解包越界）");
        }
        let path = entry.path().context("读取条目路径失败")?.into_owned();
        // 拒绝绝对路径与任何 `..` 分量：无论如何都不该出现在 npm 包里。
        if path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            bail!("包内路径越界：{}", path.display());
        }
        let mut comps = path.components();
        let first = match comps.next() {
            Some(std::path::Component::Normal(s)) => s.to_string_lossy().to_string(),
            _ => bail!("包内路径形态非法：{}", path.display()),
        };
        match &roots {
            None => roots = Some(first.clone()),
            Some(r) if r == &first => {}
            Some(r) => bail!("包内存在多个根目录：{r} 与 {first}（必须单一根）"),
        }
        let rel: PathBuf = comps.collect();
        if rel.as_os_str().is_empty() {
            continue; // 根目录本身
        }
        // 收集出的相对路径必须只含 Normal 分量。Windows 下 `pkg/C:/evil`
        // 的 `C:` 段会被解析成盘符前缀，`staging.join(rel)` 会**整体替换**
        // base（写到 staging 之外）；ParentDir 已在上面拒绝，这里补
        // Prefix / RootDir / CurDir（跨平台正确，Unix 上 `C:` 是普通名）。
        if rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("包内路径形态非法：{}", path.display());
        }
        if !seen.insert(rel.clone()) {
            bail!("包内路径重复：{}", rel.display());
        }
        if !kind.is_file() {
            continue; // 目录条目，内容已在后续文件里
        }
        let size = header.size().unwrap_or(0);
        total = total.saturating_add(size);
        if total > MAX_UNPACKED_BYTES {
            bail!("解包总量过大：超过 {MAX_UNPACKED_BYTES} bytes");
        }
        let mut content = Vec::with_capacity(size as usize);
        std::io::copy(&mut entry, &mut content).context("读取条目内容失败")?;
        if rel == Path::new("package.json") {
            if content.len() as u64 > MAX_MANIFEST_BYTES {
                bail!("package.json 过大：{} bytes", content.len());
            }
            manifest_json =
                Some(serde_json::from_slice(&content).context("package.json 不是合法 JSON")?);
        }
        collected.push((rel, content));
    }

    let manifest_json = manifest_json.context("包内缺少 package.json")?;
    let name = manifest_json
        .get("name")
        .and_then(|v| v.as_str())
        .context("package.json 缺少 name")?;
    let version = manifest_json
        .get("version")
        .and_then(|v| v.as_str())
        .context("package.json 缺少 version")?;
    if name != package_name {
        bail!("package.json 的 name（{name}）与安装来源（{package_name}）不一致");
    }
    validate_name(name)?;
    validate_version(version)?;

    let entry = resolve_entry(&manifest_json)?;
    let has_dependencies = ["dependencies", "optionalDependencies", "peerDependencies"]
        .iter()
        .any(|k| {
            manifest_json
                .get(*k)
                .and_then(|d| d.as_object())
                .is_some_and(|o| !o.is_empty())
        });

    // 入口必须在包内真实存在（扫描一遍 collected 判断）。
    let Some(entry_rel) = safe_entry_rel(&entry) else {
        bail!("入口路径非法（绝对路径或含 ..）：{entry}");
    };
    if !collected.iter().any(|(p, _)| p == &entry_rel) {
        bail!("入口文件不存在：{entry}");
    }

    // 第二遍：真正落盘（staging → rename，避免半截目录被当成有效包）。
    //
    // staging 名必须**每次唯一**：同名同版本的并发安装若共用固定 staging
    // 路径，一方的 `remove_dir_all` 会删掉另一方正在写的文件，rename 还会
    // 互相踩踏，最终可能提交出混合内容的目录。pid + 进程内单调序足够。
    let dir_name = dir_name_for(name);
    let dest = packages_root.join(&dir_name).join(version);
    let staging = packages_root.join(format!(
        ".{dir_name}.{version}.{}.{}.staging",
        std::process::id(),
        STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("创建暂存目录失败：{}", staging.display()))?;
    // 任何失败路径都必须清掉 staging，否则会在 packages 根下留半成品。
    if let Err(err) = write_staging(&staging, &collected) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    commit_staging(&staging, &dest)?;

    Ok(Manifest {
        name: name.to_string(),
        version: version.to_string(),
        entry,
        has_dependencies,
    })
}

/// staging 目录名唯一序（包安装进程内单调）。
static STAGING_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 把收集到的文件写进 staging（任何一步失败都由调用方清 staging）。
fn write_staging(staging: &Path, files: &[(PathBuf, Vec<u8>)]) -> Result<()> {
    for (rel, content) in files {
        let out = staging.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败：{}", parent.display()))?;
        }
        std::fs::write(&out, content)
            .with_context(|| format!("写入文件失败：{}", out.display()))?;
    }
    Ok(())
}

/// 把 staging 提交为 dest：旧版本先移到备份，提交失败则还原。
///
/// 直接 `remove_dir_all(dest)` 再 rename 的写法在 rename 失败时会把
/// **旧版本一并毁掉**（磁盘满/杀软锁文件/Windows 目录占用都可能触发）。
/// 备份 + 还原保证「失败后旧版本仍在」。
fn commit_staging(staging: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let backup = PathBuf::from(format!("{}.old", staging.display()));
    let _ = std::fs::remove_dir_all(&backup);
    let had_old = dest.exists();
    if had_old {
        std::fs::rename(dest, &backup).context("暂存旧版本目录失败")?;
    }
    match std::fs::rename(staging, dest) {
        Ok(()) => {
            if had_old {
                let _ = std::fs::remove_dir_all(&backup);
            }
            Ok(())
        }
        Err(err) => {
            if had_old {
                // 还原旧版本；还原失败只能记录（此时旧版本在 backup 里）
                if let Err(rb) = std::fs::rename(&backup, dest) {
                    tracing::warn!(
                        "[Script] 提交失败且旧版本还原失败（旧版本在 {}）：{rb}",
                        backup.display()
                    );
                }
            }
            let _ = std::fs::remove_dir_all(staging);
            Err(err).context("提交包目录失败")
        }
    }
}

/// 校验包体的 SRI 完整性声明（`sha512-<base64>` / `sha256-<base64>`，
/// 可多条空格分隔）。
///
/// **必须至少真校验一条可识别的算法**：调用方只在「声明存在」时才调本
/// 函数，若声明里全是未知算法而这里静默返回 Ok，`integrity: "sha1-…"`
/// 之类的声明就会变成「有声明但零校验」的完整性绕过。未知算法本身跳过
///（SRI 允许多算法并存），但整条声明没有一条能验 → 报错。
pub fn verify_integrity(bytes: &[u8], integrity: &str) -> Result<()> {
    use base64::Engine as _;
    use sha2::Sha512;

    let mut checked_any = false;
    for part in integrity.split_whitespace() {
        let Some((algo, b64)) = part.split_once('-') else {
            continue;
        };
        // 先判算法再解码：未知算法连 base64 都不必解（避免畸形未知算法
        // 因解码失败而报「base64 错误」这种误导信息）。
        let expected = match algo {
            "sha512" => Sha512::digest(bytes).to_vec(),
            "sha256" => Sha256::digest(bytes).to_vec(),
            _ => continue, // 未知算法不参与判定
        };
        let declared = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .with_context(|| format!("integrity base64 解码失败：{algo}"))?;
        if declared != expected {
            bail!("包完整性校验失败（{algo} 摘要不匹配）");
        }
        checked_any = true;
    }
    if !checked_any {
        bail!("完整性声明里没有可校验的算法（支持 sha512/sha256）：{integrity}");
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// 构造一个 npm 风格 tarball（`package/…` 单根）。
    pub(crate) fn make_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, format!("package/{name}"), *content)
                    .unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        gz.finish().unwrap()
    }

    fn manifest(name: &str, version: &str, entry: &str) -> Vec<u8> {
        format!(r#"{{"name":"{name}","version":"{version}","main":"{entry}"}}"#).into_bytes()
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("@magpie-community/kiro").is_ok());
        assert!(validate_name("plain-pkg").is_ok());
        assert!(validate_name("Bad-Upper").is_err());
        assert!(validate_name("../escape").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("@/x").is_err());
    }

    #[test]
    fn version_validation() {
        assert!(validate_version("1.2.3-beta.1+build").is_ok());
        assert!(validate_version("../x").is_err());
        assert!(validate_version("").is_err());
        // 路径危险形态：纯点段 / 连续点 / 点开头（会逃出 packages/{hash}/）
        for bad in [".", "..", "...", "1..2", ".1.0", "+", "-"] {
            assert!(validate_version(bad).is_err(), "应拒绝版本 {bad:?}");
        }
    }

    #[test]
    fn unpack_rejects_dot_dot_version_without_touching_packages_root() {
        // 恶意 package.json version=".." 曾能把 dest 解析成 packages 根，
        // remove_dir_all 会删光全部已装包。必须在落盘前拒绝。
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("packages");
        std::fs::create_dir_all(&root).unwrap();
        let victim = root.join("script-victim").join("1.0.0");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("keep.txt"), b"keep").unwrap();

        let tar = make_tarball(&[
            (
                "package.json",
                br#"{"name":"my-pkg","version":"..","main":"index.mjs"}"#,
            ),
            ("index.mjs", b"x"),
        ]);
        let err = unpack_tarball(&tar, "my-pkg", &root).unwrap_err();
        assert!(err.to_string().contains("版本号"), "{err}");
        // 既有包目录必须毫发无损
        assert!(victim.join("keep.txt").is_file(), "不得删除既有包目录");
    }

    #[test]
    fn verify_integrity_rejects_unknown_algorithm_only() {
        // 全未知算法不能「静默通过」（有声明但零校验 = 绕过）。
        let err = verify_integrity(b"data", "sha1-abc=").unwrap_err();
        assert!(err.to_string().contains("没有可校验的算法"), "{err}");
        // 畸形/空声明同样拒绝
        assert!(verify_integrity(b"data", "garbage").is_err());
        assert!(verify_integrity(b"data", "").is_err());
    }

    #[test]
    fn verify_integrity_sha512_and_sha256_roundtrip() {
        use base64::Engine as _;
        use sha2::{Digest, Sha256, Sha512};
        let data = b"hello-tarball";
        let s512 = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(data));
        let s256 = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(data));
        assert!(verify_integrity(data, &format!("sha512-{s512}")).is_ok());
        assert!(verify_integrity(data, &format!("sha256-{s256}")).is_ok());
        // 多算法并存：已知算法必须真校验
        assert!(verify_integrity(data, &format!("sha512-{s512} sha256-{s256}")).is_ok());
        // 哈希不符 → 拒绝
        let bad = base64::engine::general_purpose::STANDARD.encode(Sha512::digest(b"other"));
        assert!(verify_integrity(data, &format!("sha512-{bad}")).is_err());
        // 未知算法混在已知算法里：已知算法校验通过即放行
        assert!(verify_integrity(data, &format!("sha512-{s512} sha1-whatever")).is_ok());
    }

    #[test]
    fn reinstall_replaces_dest_and_leaves_no_staging_or_backup() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("packages");
        let tar = make_tarball(&[
            ("package.json", &manifest("my-pkg", "1.0.0", "index.mjs")),
            ("index.mjs", b"v1"),
        ]);
        unpack_tarball(&tar, "my-pkg", &root).unwrap();
        // 同版本重装
        let tar2 = make_tarball(&[
            ("package.json", &manifest("my-pkg", "1.0.0", "index.mjs")),
            ("index.mjs", b"v2"),
        ]);
        unpack_tarball(&tar2, "my-pkg", &root).unwrap();
        let dest = root.join(dir_name_for("my-pkg")).join("1.0.0");
        assert_eq!(std::fs::read(dest.join("index.mjs")).unwrap(), b"v2");
        // 不应残留 staging / backup
        let leftovers: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".staging") || n.contains(".old"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "不应残留 staging/backup：{leftovers:?}"
        );
    }

    #[test]
    fn staging_names_are_unique() {
        // 同名同版本并发安装不得共用 staging 路径。
        let root = Path::new("/tmp/packages");
        let mk = || {
            root.join(format!(
                ".x.{}.{}.{}.staging",
                "1.0.0",
                std::process::id(),
                STAGING_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ))
        };
        assert_ne!(mk(), mk());
    }

    #[test]
    fn safe_entry_rel_accepts_relative_and_rejects_escape() {
        assert_eq!(
            safe_entry_rel("./dist/server.mjs"),
            Some(PathBuf::from("dist/server.mjs"))
        );
        assert_eq!(
            safe_entry_rel("index.mjs"),
            Some(PathBuf::from("index.mjs"))
        );
        assert!(safe_entry_rel("").is_none());
        assert!(safe_entry_rel("../outside.js").is_none());
        assert!(safe_entry_rel("a/../../b.js").is_none());
        assert!(safe_entry_rel("/etc/passwd").is_none());
        #[cfg(windows)]
        {
            assert!(safe_entry_rel(r"C:\evil.js").is_none());
            assert!(safe_entry_rel("C:evil.js").is_none());
        }
    }

    #[test]
    fn dir_name_is_hash_based_and_stable() {
        let a = dir_name_for("@magpie-community/kiro");
        let b = dir_name_for("@magpie-community/kiro");
        assert_eq!(a, b);
        assert!(a.starts_with("script-") && a.len() == 31);
        assert!(!a.contains('/'));
        assert_ne!(a, dir_name_for("@magpie-community/agnes"));
    }

    #[test]
    fn entry_resolution_priority() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"exports":{"./server":"./dist/server.mjs"},"main":"index.js"}"#,
        )
        .unwrap();
        assert_eq!(resolve_entry(&v).unwrap(), "./dist/server.mjs");

        let v: serde_json::Value = serde_json::from_str(r#"{"main":"index.js"}"#).unwrap();
        assert_eq!(resolve_entry(&v).unwrap(), "index.js");

        let v: serde_json::Value = serde_json::from_str(r#"{"exports":"./x.mjs"}"#).unwrap();
        assert_eq!(resolve_entry(&v).unwrap(), "./x.mjs");

        let v: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(resolve_entry(&v).is_err());
    }

    #[test]
    fn unpacks_valid_package() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("packages");
        let tar = make_tarball(&[
            ("package.json", &manifest("my-pkg", "1.0.0", "index.mjs")),
            (
                "index.mjs",
                b"export default () => ({ auth: { provider: 'x' } })",
            ),
        ]);
        let m = unpack_tarball(&tar, "my-pkg", &root).unwrap();
        assert_eq!(m.name, "my-pkg");
        assert_eq!(m.version, "1.0.0");
        assert_eq!(m.entry, "index.mjs");
        assert!(!m.has_dependencies);
        // 目录名是 hash
        let expected = root.join(dir_name_for("my-pkg")).join("1.0.0");
        assert!(expected.join("index.mjs").is_file());
        assert!(expected.join("package.json").is_file());
    }

    #[test]
    fn rejects_missing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let tar = make_tarball(&[("index.mjs", b"x")]);
        let err = unpack_tarball(&tar, "my-pkg", &dir.path().join("p")).unwrap_err();
        assert!(err.to_string().contains("package.json"), "{err}");
    }

    #[test]
    fn rejects_missing_entry_file() {
        let dir = tempfile::tempdir().unwrap();
        let tar = make_tarball(&[
            (
                "package.json",
                &manifest("my-pkg", "1.0.0", "dist/nope.mjs"),
            ),
            ("index.mjs", b"x"),
        ]);
        let err = unpack_tarball(&tar, "my-pkg", &dir.path().join("p")).unwrap_err();
        assert!(err.to_string().contains("入口文件不存在"), "{err}");
    }

    #[test]
    fn rejects_name_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let tar = make_tarball(&[
            ("package.json", &manifest("other-pkg", "1.0.0", "index.mjs")),
            ("index.mjs", b"x"),
        ]);
        let err = unpack_tarball(&tar, "my-pkg", &dir.path().join("p")).unwrap_err();
        assert!(err.to_string().contains("不一致"), "{err}");
    }

    #[test]
    fn rejects_traversal_paths() {
        // 手工造一个含 ../ 的 tar。
        //
        // `tar::Builder` 自己就拒绝带 `..` 的路径（append_data 报
        // "paths in archives must not have `..`"），所以这里**手写 512
        // 字节 GUN 头**，绕过 builder 的路径校验——被防护的正是这种只有
        // 到解包阶段才暴露的恶意包。
        let content = b"evil";
        let mut header = [0u8; 512];
        let name = b"../escape.txt";
        header[..name.len()].copy_from_slice(name);
        header[100..107].copy_from_slice(b"0000644"); // mode
        header[108..115].copy_from_slice(b"0000000"); // uid
        header[116..123].copy_from_slice(b"0000000"); // gid
        header[124..135].copy_from_slice(b"00000000004"); // size（八进制 4）
        header[136..147].copy_from_slice(b"00000000000"); // mtime
        header[148..156].copy_from_slice(b"        "); // checksum 占位（空格）
        header[156] = b'0'; // typeflag：普通文件
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let sum: u32 = header.iter().map(|b| *b as u32).sum();
        let cksum = format!("{sum:06o}\0 ");
        header[148..156].copy_from_slice(cksum.as_bytes());

        let mut tar_bytes = header.to_vec();
        tar_bytes.extend_from_slice(content);
        tar_bytes.resize(1024, 0); // 头 + 体（体补齐到块边界）
        tar_bytes.resize(2048, 0); // 两个全零块 = tar EOF

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, &tar_bytes).unwrap();
        let bytes = gz.finish().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let err = unpack_tarball(&bytes, "my-pkg", &dir.path().join("p")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("越界") || msg.contains("形态非法"), "{msg}");
    }

    #[test]
    fn rejects_oversized_archive() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![0u8; (MAX_ARCHIVE_BYTES + 1) as usize];
        let err = unpack_tarball(&big, "my-pkg", &dir.path().join("p")).unwrap_err();
        assert!(err.to_string().contains("包体过大"), "{err}");
    }
}
