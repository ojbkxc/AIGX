//! 安装来源解析 —— 把运维输入的字符串解析成 [`InstallSource`] 四态。
//!
//! ## 为什么解析层要做这么多拒绝
//!
//! 安装源字符串是**运维手输**（管理面粘贴）或目录带出的外部输入。npm
//! 包名会被拼进 registry URL 与磁盘路径、GitHub ref 会被拼进 API URL 与
//! codeload URL——任何一处字符集放任，都会变成 URL 注入或路径穿越的
//! 入口。所以每个变体在解析期就把字符集钉死，落盘期再校验一道。
//!
//! ## GitHub 的信任链
//!
//! 只信官方两跳：`api.github.com` 解析 ref → 40 位 commit SHA →
//! `codeload.github.com` 下载 tarball。不接受任何镜像（镜像可篡改包内
//! 容，而我们的完整性校验只有 sha512 来自 registry 元数据——GitHub 源
//! 没有 registry 元数据，因此**源域名本身就是信任根**）。ref 只接受
//! SHA / 分支名 / tag 名，拒绝 `..` 与控制字符（后者是 URL/头注入面）。

#![allow(dead_code)] // 脚本包来源解析（预留分支：npm/git 属地在安装链路外）
use serde::Serialize;

/// 一次安装的来源描述（解析产物，不再含字符串歧义）。
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub enum InstallSource {
    /// `file:` 前缀 —— 本机目录（必须绝对路径）。
    Local {
        /// 本机目录的绝对路径。
        path: String,
    },
    /// 裸包名或 `@scope/name` —— npm registry。
    Npm {
        /// npm 包名。
        name: String,
        /// 版本或 dist-tag（空 = latest）。
        version: String,
    },
    /// `magpie:` 前缀 —— Magpie 市场。
    ///
    /// 下载优先走 **magpie-community/plugins 仓库 dist 分支的 tgz**
    /// （`https://raw.githubusercontent.com/magpie-community/plugins/dist/{file}`）：
    /// 实测多数 `@magpie-community/*` 官方包未发布到 npm（4/5 抽样 404），
    /// dist tgz 是它们唯一在架的分发渠道，且 dist index 声明的 sha512
    /// integrity 可闭环校验。npm registry 仅作 archive_url 缺省时的备选
    /// （覆盖 agent-aider 这类 npm 在架包）。
    ///
    /// `parse("magpie:name")` 只产出 name/version（两个 None）；operations
    /// 装包时从目录条目（magpie-catalog 的 Entry）把 file/integrity 填入。
    Magpie {
        /// npm 包名。
        name: String,
        /// 版本（空 = 清单/dist 的 latest）。
        version: String,
        /// dist tgz 下载 URL（None = 走 npm registry 备选）。
        archive_url: Option<String>,
        /// tarball 的完整性声明（`sha512-base64` / `sha256-base64`，与
        /// npm `dist.integrity` 同形态）。
        integrity: Option<String>,
    },
    /// `github:owner/repo#ref` 或 `https://github.com/owner/repo`。
    Github {
        /// `owner/repo`。
        repository: String,
        /// SHA / 分支 / tag（空 = 默认分支）。
        reference: String,
    },
}

impl InstallSource {
    /// 对应的 [`crate::plugin_host::script::ScriptSource`]。
    pub fn script_source(&self) -> crate::plugin_host::script::ScriptSource {
        match self {
            Self::Local { .. } => crate::plugin_host::script::ScriptSource::Local,
            Self::Npm { .. } => crate::plugin_host::script::ScriptSource::Npm,
            Self::Magpie { .. } => crate::plugin_host::script::ScriptSource::Magpie,
            Self::Github { .. } => crate::plugin_host::script::ScriptSource::Github,
        }
    }

    /// `source_ref` 口径（与 types.rs 的 ScriptPackage::source_ref 同形）：
    /// magpie=`{name}`；npm=`{name[@version]}`；github=`{repo}{@ref}`；
    /// local=路径。
    pub fn source_ref(&self) -> String {
        match self {
            Self::Local { path } => path.clone(),
            Self::Npm { name, version } => {
                if version.is_empty() {
                    name.clone()
                } else {
                    format!("{name}@{version}")
                }
            }
            Self::Magpie { name, version, .. } => {
                if version.is_empty() {
                    name.clone()
                } else {
                    format!("{name}@{version}")
                }
            }
            Self::Github {
                repository,
                reference,
            } => {
                if reference.is_empty() {
                    repository.clone()
                } else {
                    format!("{repository}@{reference}")
                }
            }
        }
    }
}

/// 安装来源错误。
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("安装来源为空")]
    Empty,
    #[error("不支持的安装来源前缀: {0}（支持 file: / magpie: / github: / https://github.com/… / 裸 npm 包名）")]
    UnknownPrefix(String),
    #[error("file: 来源必须是绝对路径: {0}")]
    NotAbsolute(String),
    #[error("包名无效: {0}（{1}）")]
    BadName(String, &'static str),
    #[error("GitHub 仓库无效: {0}（{1}）")]
    BadRepository(String, &'static str),
    #[error("GitHub ref 无效: {0}")]
    BadRef(String),
    #[error("URL 不是 https 或主机不是 github.com: {0}")]
    BadUrl(String),
    #[error("解析 GitHub ref 失败: {0}")]
    Resolve(String),
}

/// 解析安装来源字符串。
///
/// 前缀判定顺序：`file:` → `magpie:` → `github:` → `https://github.com`
/// → 其余按 npm 包名处理。
pub fn parse(input: &str) -> Result<InstallSource, SourceError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(SourceError::Empty);
    }

    if let Some(rest) = input.strip_prefix("file:") {
        let path = rest.trim();
        // Windows 盘符（C:\…）与 Unix 根路径都由 is_absolute 判定。
        if !Path::new(path).is_absolute() {
            return Err(SourceError::NotAbsolute(path.to_string()));
        }
        // 路径内容不再限制字符集——本机路径合法形态太多（Windows 盘符、
        // 中文目录），绝对路径 + 后续 canonicalize 越界检查已足够。
        return Ok(InstallSource::Local {
            path: path.to_string(),
        });
    }

    if let Some(rest) = input.strip_prefix("magpie:") {
        let (name, version) = split_name_version(rest.trim())?;
        return Ok(InstallSource::Magpie {
            name,
            version,
            // dist tgz 的 file/integrity 由 operations 从目录条目补充——
            // 解析层不联网查目录（目录缓存归 magpie-catalog）。
            archive_url: None,
            integrity: None,
        });
    }

    if let Some(rest) = input.strip_prefix("github:") {
        return parse_github_shorthand(rest.trim());
    }

    if input.starts_with("https://github.com/") || input.starts_with("https://github.com:") {
        return parse_github_https(input);
    }

    // `http://github.com` 等明文 HTTP 直接拒——升级重定向可被中间人利用
    // 换包；只收 https。
    if input.starts_with("http://") {
        return Err(SourceError::BadUrl(input.to_string()));
    }

    // 其余一律按 npm 包名。会被 split_name_version 拒掉非法形态。
    let (name, version) = split_name_version(input)?;
    Ok(InstallSource::Npm { name, version })
}

/// GitHub 解析产物：40 位 commit SHA + 下载 URL。
pub struct GithubResolved {
    /// 40 位 commit SHA（api.github.com 解析所得）。
    pub commit: String,
    /// codeload tarball URL（`codeload.github.com/{repo}/tar.gz/{commit}`）。
    pub tarball_url: String,
}

/// 把 GitHub ref 解析成 40 位 commit SHA，再给出 codeload tarball URL。
///
/// 信任链：**api.github.com 回答什么 SHA，就只信什么 SHA**；下载只走
/// codeload.github.com。`reference` 为空时让 API 解析默认分支（`/commits`
/// 不带 ref 即默认分支 HEAD）。
///
/// 40 位十六进制 SHA 直接用（免去一次 API 调用）；其余 ref 走 API。
pub async fn resolve_github(
    client: &reqwest::Client,
    repository: &str,
    reference: &str,
) -> Result<GithubResolved, SourceError> {
    validate_repository(repository)?;
    validate_ref(reference)?;

    // ref 本身已是 40 位 hex → 不用问 API（SHA 是内容寻址，无需解析）
    let is_sha = reference.len() == 40
        && reference.chars().all(|c| c.is_ascii_hexdigit())
        && reference
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    let commit: String = if is_sha {
        reference.to_ascii_lowercase()
    } else {
        let url = if reference.is_empty() {
            format!("https://api.github.com/repos/{repository}/commits")
        } else {
            // ref 已被 validate_ref 钉死字符集（无 / 开头结尾、无控制字符），
            // 但 ref 内部可含 `/`（如 feature/x）——必须逐段 encode，否则
            // API 把它当路径段。用 percent-encoding 逐字节转义非安全字符。
            let encoded = encode_ref(reference);
            format!("https://api.github.com/repos/{repository}/commits/{encoded}")
        };
        let resp = client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "AIGX-gateway")
            .send()
            .await
            .map_err(|e| SourceError::Resolve(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(SourceError::Resolve(format!(
                "api.github.com 返回 HTTP {}（ref={reference:?}）",
                resp.status()
            )));
        }
        let v: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| SourceError::Resolve(format!("响应不是 JSON: {e}")))?;
        extract_commit_sha(&v, reference.is_empty())?
    };

    let tarball_url = format!("https://codeload.github.com/{repository}/tar.gz/{commit}");
    Ok(GithubResolved {
        commit,
        tarball_url,
    })
}

/// 从 GitHub commits API 响应提取 commit SHA 并校验 40 位 hex。
///
/// 指定 ref → `/commits/{ref}` 返回**单个 commit 对象**；
/// 未指定 ref → `/commits` 返回**默认分支的 commit 列表**（JSON 数组，取第一条）。
/// 早期实现一律按对象取 `sha`，导致无 ref 的 selector（如 `github:owner/repo`）
/// 永远报「响应缺 sha 字段」。
fn extract_commit_sha(v: &serde_json::Value, default_branch: bool) -> Result<String, SourceError> {
    let sha = if default_branch {
        v.as_array()
            .and_then(|list| list.first())
            .and_then(|commit| commit.get("sha"))
            .and_then(|s| s.as_str())
            .ok_or_else(|| {
                SourceError::Resolve("响应缺 sha 字段（默认分支 commit 列表为空）".to_string())
            })?
    } else {
        v.get("sha")
            .and_then(|s| s.as_str())
            .ok_or_else(|| SourceError::Resolve("响应缺 sha 字段".to_string()))?
    }
    .to_string();
    if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(SourceError::Resolve(format!(
            "API 返回的 sha 不是 40 位 hex: {sha}"
        )));
    }
    Ok(sha.to_ascii_lowercase())
}

/// ref 段的 URL 转义（只转义必转的字符；validate_ref 已保证字符集安全）。
fn encode_ref(reference: &str) -> String {
    let mut out = String::with_capacity(reference.len());
    for b in reference.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            // `/` 在 ref 里合法（feature/x），API 路径里必须转义
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `@scope/name@version` / `name@version` / `name` → (name, version)。
///
/// scoped 包名形如 `@a/b`，版本段取**最后一个** `@` 之后——scope 前缀的
/// `@` 不是分隔符。裸包名没有 `/`，直接按最后一个 `@` 切。
fn split_name_version(input: &str) -> Result<(String, String), SourceError> {
    let (name, version) = if let Some(stripped) = input.strip_prefix('@') {
        // scoped：@scope/name[@version]——scope 与 name 里不允许再有 @
        let (scope_name, version) = match stripped.rfind('@') {
            Some(idx) => (&stripped[..idx], stripped[idx + 1..].to_string()),
            None => (stripped, String::new()),
        };
        (format!("@{scope_name}"), version)
    } else {
        match input.rfind('@') {
            Some(idx) => (input[..idx].to_string(), input[idx + 1..].to_string()),
            None => (input.to_string(), String::new()),
        }
    };
    validate_package_name(&name)?;
    // 版本不校验 semver 语义（dist-tag 也合法），只拒会污染 URL/路径的字符
    validate_version_ish(&version)?;
    Ok((name, version))
}

/// npm 包名规则（packages.rs 落盘前再校验一道，这里管解析期快速失败）：
/// ≤180 字符、小写字母数字 `-_./`、scoped 形态 `@scope/name`。
pub fn validate_package_name(name: &str) -> Result<(), SourceError> {
    if name.is_empty() {
        return Err(SourceError::BadName(name.to_string(), "为空"));
    }
    if name.len() > 180 {
        return Err(SourceError::BadName(name.to_string(), "超过 180 字符"));
    }
    let scoped = name.starts_with('@');
    if scoped {
        // 必须正好一个 '/'：@scope/name
        let slash_count = name.matches('/').count();
        if slash_count != 1 {
            return Err(SourceError::BadName(
                name.to_string(),
                "scoped 名必须形如 @scope/name（恰好一个 /）",
            ));
        }
    } else if name.contains('/') {
        return Err(SourceError::BadName(name.to_string(), "裸包名不允许 /"));
    }

    for (i, ch) in name.char_indices() {
        let ok = ch.is_ascii_lowercase()
            || ch.is_ascii_digit()
            || matches!(ch, '-' | '_' | '.' | '/' | '@');
        if !ok {
            return Err(SourceError::BadName(
                name.to_string(),
                "只允许小写字母数字 - _ . / @",
            ));
        }
        // `@` 只允许出现在 scoped 名的第 0 位（`@scope/name`）：中间的
        // `@`（如 `@a/b@c`）不是合法 npm 名，且会污染 registry URL。
        if ch == '@' && i > 0 {
            return Err(SourceError::BadName(
                name.to_string(),
                "只允许开头一个 @（scoped 前缀）",
            ));
        }
        // npm 规则：name 不得以 . 或 _ 开头（scoped 的 @ 前缀除外）
        if i == 0 && matches!(ch, '.' | '_') {
            return Err(SourceError::BadName(name.to_string(), "不得以 . 或 _ 开头"));
        }
        // scoped 段内不允许空段（@/name、@scope/ 都拒）
        if ch == '/' {
            // 位置检查：@ 后一段非空、/ 后一段非空
            // （这里的检查不完美，交给 validate_scoped 段校验兜底）
        }
    }
    if scoped {
        let inner = &name[1..];
        let (scope, pkg) = inner
            .split_once('/')
            .ok_or_else(|| SourceError::BadName(name.to_string(), "scoped 名缺 /"))?;
        if scope.is_empty() || pkg.is_empty() {
            return Err(SourceError::BadName(name.to_string(), "scope 或包名段为空"));
        }
    }
    Ok(())
}

/// 版本/ dist-tag 的字符集钉死：`[A-Za-z0-9.+-]`。
///
/// 不做 semver 解析——npm 的 dist-tag（latest/next/beta…）与完整 semver
/// range 都合法；只要保证该段能安全进 URL 与磁盘路径即可。
fn validate_version_ish(version: &str) -> Result<(), SourceError> {
    if version.is_empty() {
        return Ok(());
    }
    if version.len() > 64 {
        return Err(SourceError::BadName(
            version.to_string(),
            "版本段超过 64 字符",
        ));
    }
    // semver 与 npm dist-tag 都不含下划线/空格等——字符集钉死到
    // [A-Za-z0-9.+-]，与 version_dir 消毒口径一致；连续点/点开头不是
    // 合法 semver，也拒绝（它们会进 source_ref 与包版本目录）。
    if version
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')))
        || version.contains("..")
        || version.starts_with('.')
    {
        return Err(SourceError::BadName(
            version.to_string(),
            "版本段只允许字母数字 . - +，且不得含连续点或以点开头",
        ));
    }
    Ok(())
}

/// 解析 `github:owner/repo#ref`。
fn parse_github_shorthand(rest: &str) -> Result<InstallSource, SourceError> {
    // `#ref` 取第一个 `#`——ref 段本身不允许 `#`（后面字符集校验兜底）
    let (repo, reference) = match rest.split_once('#') {
        Some((r, rf)) => (r.trim().to_string(), rf.trim().to_string()),
        None => (rest.trim().to_string(), String::new()),
    };
    validate_repository(&repo)?;
    validate_ref(&reference)?;
    Ok(InstallSource::Github {
        repository: repo,
        reference,
    })
}

/// 解析 `https://github.com/owner/repo[/tree/ref|/archive/…|.git]`。
///
/// 只接受 `github.com`（443 隐含在 https 里）；URL 里带账号信息
/// （`user:token@github.com`）直接拒——凭据不该出现在安装源字符串里，
/// 且 netloc 混淆是经典的 SSRF 绕过形态。
fn parse_github_https(input: &str) -> Result<InstallSource, SourceError> {
    let rest = input
        .strip_prefix("https://")
        .ok_or_else(|| SourceError::BadUrl(input.to_string()))?;
    // 拒账号信息：host 段前不允许出现 `@`（出现在路径里合法，如 /tree/@ref? ——
    // 实际 GitHub tag 可以含 @，所以只在**第一个 / 之前**查 @）
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host_part = &rest[..host_end];
    let host_ok = host_part.eq_ignore_ascii_case("github.com")
        || host_part.eq_ignore_ascii_case("github.com:443");
    if host_part.contains('@') || !host_ok {
        return Err(SourceError::BadUrl(input.to_string()));
    }
    let path = &rest[host_end..];
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    // 接受形态：/owner/repo 与 /owner/repo/tree/<ref>
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() < 2 {
        return Err(SourceError::BadRepository(
            input.to_string(),
            "路径必须是 /owner/repo",
        ));
    }
    if segments.len() > 4 || (segments.len() == 4 && segments[2] != "tree") {
        return Err(SourceError::BadRepository(
            input.to_string(),
            "只支持 /owner/repo 与 /owner/repo/tree/<ref>",
        ));
    }
    let repo = format!("{}/{}", segments[0], segments[1]);
    validate_repository(&repo)?;
    let reference = if segments.len() == 4 {
        segments[3].to_string()
    } else {
        String::new()
    };
    validate_ref(&reference)?;
    Ok(InstallSource::Github {
        repository: repo,
        reference,
    })
}

/// `owner/repo` 校验：GitHub 仓库名字符集 `[A-Za-z0-9._-]`、owner 同、
/// 各段非空非 `.`/`..`。会被拼进 api.github.com 与 codeload URL，字符集
/// 放任 = URL 注入。
fn validate_repository(repo: &str) -> Result<(), SourceError> {
    let (owner, name) = repo
        .split_once('/')
        .ok_or_else(|| SourceError::BadRepository(repo.to_string(), "缺 owner/repo 形态"))?;
    let seg_ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    if !seg_ok(owner) || !seg_ok(name) {
        return Err(SourceError::BadRepository(
            repo.to_string(),
            "owner/repo 段只允许字母数字 . _ -，且非空非 . 非 ..",
        ));
    }
    Ok(())
}

/// GitHub ref 校验：SHA / 分支 / tag 名。拒绝 `..`、空白、控制字符、
/// `~^:?*[\` 与以 `/` 开头结尾（git refname 规则的超集——这里不求完全
/// 等价 git 规则，只求「进 URL 后无歧义、无注入」）。
fn validate_ref(reference: &str) -> Result<(), SourceError> {
    if reference.is_empty() {
        return Ok(());
    }
    if reference.len() > 256 {
        return Err(SourceError::BadRef(reference.to_string()));
    }
    let bad = reference.contains("..")
        || reference.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        || reference.starts_with('/')
        || reference.ends_with('/')
        || reference.ends_with(".lock");
    if bad {
        return Err(SourceError::BadRef(reference.to_string()));
    }
    Ok(())
}

use std::path::Path;

/// npm registry 元数据里取出的 tarball 定位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NpmTarball {
    /// 下载 URL（已校验属地 registry.npmjs.org）。
    pub url: String,
    /// `dist.integrity`（`sha512-base64` / `sha256-base64`，可能含多算法）。
    pub integrity: Option<String>,
}

/// 从 npm registry 元数据里解析出指定版本的 tarball URL 与 integrity。
///
/// `version` 为空时取 `dist-tags.latest`。**tarball URL 必须属于
/// registry.npmjs.org**：registry 元数据是外部输入，`dist.tarball` 字段
/// 可以写任何 URL——放任它等于给「把包内容指到攻击者站点」开了门
/// （配套的 integrity 也是攻击者给的，校验闭环被打穿）。
pub fn resolve_npm_tarball(
    meta: &serde_json::Value,
    version: &str,
) -> Result<NpmTarball, SourceError> {
    let versions = meta
        .get("versions")
        .and_then(|v| v.as_object())
        .ok_or_else(|| SourceError::Resolve("registry 元数据缺 versions".into()))?;

    let resolved_version: &str = if version.is_empty() {
        let latest = meta
            .get("dist-tags")
            .and_then(|t| t.get("latest"))
            .and_then(|l| l.as_str())
            .ok_or_else(|| SourceError::Resolve("registry 元数据缺 dist-tags.latest".into()))?;
        // latest 指向的版本必须真实存在（防元数据自相矛盾）
        if !versions.contains_key(latest) {
            return Err(SourceError::Resolve(format!(
                "dist-tags.latest={latest} 不在 versions 里"
            )));
        }
        latest
    } else {
        // 显式版本/dist-tag：先试精确版本键，再试 dist-tag。
        if versions.contains_key(version) {
            version
        } else {
            let tag = meta
                .get("dist-tags")
                .and_then(|t| t.get(version))
                .and_then(|l| l.as_str())
                .ok_or_else(|| SourceError::Resolve(format!("版本不存在：{version}")))?;
            if !versions.contains_key(tag) {
                return Err(SourceError::Resolve(format!(
                    "dist-tag {version} 指向 {tag}，但该版本不在 versions 里"
                )));
            }
            tag
        }
    };

    let v = &versions[resolved_version];
    let tarball = v
        .get("dist")
        .and_then(|d| d.get("tarball"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| SourceError::Resolve(format!("版本 {resolved_version} 缺 dist.tarball")))?;

    // 属地校验：host 必须是 registry.npmjs.org（https）。
    // 不接受子域/镜像/内网地址——npm 的 integrity 由 registry 声明，
    // 只有从 registry 自己的 tarball 拿内容，这条校验链才闭环。
    let parsed = url::Url::parse(tarball)
        .map_err(|_| SourceError::Resolve(format!("dist.tarball 不是合法 URL: {tarball}")))?;
    if parsed.scheme() != "https" || parsed.host_str() != Some("registry.npmjs.org") {
        return Err(SourceError::Resolve(format!(
            "dist.tarball 不属于 registry.npmjs.org（{tarball}）——拒绝非官方源"
        )));
    }

    let integrity = v
        .get("dist")
        .and_then(|d| d.get("integrity"))
        .and_then(|i| i.as_str())
        .map(str::to_string);

    Ok(NpmTarball {
        url: tarball.to_string(),
        integrity,
    })
}

/// registry 元数据 URL（scoped 包名要转义 `/`）。
pub fn npm_meta_url(name: &str) -> Result<String, SourceError> {
    validate_package_name(name)?;
    Ok(format!(
        "https://registry.npmjs.org/{}",
        name.replace('/', "%2F")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_sha_from_default_branch_list() {
        let v = serde_json::json!([
            {"sha": "5b48ec11493c5eb7afe5d84f566704572d87afcc"},
            {"sha": "0000000000000000000000000000000000000000"}
        ]);
        assert_eq!(
            extract_commit_sha(&v, true).unwrap(),
            "5b48ec11493c5eb7afe5d84f566704572d87afcc"
        );
    }

    #[test]
    fn extract_sha_from_single_commit_object() {
        let v = serde_json::json!({"sha": "5B48EC11493C5EB7AFE5D84F566704572D87AFCC"});
        assert_eq!(
            extract_commit_sha(&v, false).unwrap(),
            "5b48ec11493c5eb7afe5d84f566704572d87afcc"
        );
    }

    #[test]
    fn extract_sha_rejects_missing_or_invalid() {
        assert!(extract_commit_sha(&serde_json::json!([]), true).is_err());
        assert!(extract_commit_sha(&serde_json::json!({}), false).is_err());
        assert!(extract_commit_sha(&serde_json::json!({"sha": "short"}), false).is_err());
    }

    #[test]
    fn parse_npm_bare_and_scoped() {
        assert_eq!(
            parse("left-pad").unwrap(),
            InstallSource::Npm {
                name: "left-pad".into(),
                version: "".into()
            }
        );
        assert_eq!(
            parse("@magpie-community/kiro").unwrap(),
            InstallSource::Npm {
                name: "@magpie-community/kiro".into(),
                version: "".into()
            }
        );
        assert_eq!(
            parse("@magpie-community/kiro@1.2.3").unwrap(),
            InstallSource::Npm {
                name: "@magpie-community/kiro".into(),
                version: "1.2.3".into()
            }
        );
        // dist-tag 也收
        assert_eq!(
            parse("some-pkg@beta").unwrap(),
            InstallSource::Npm {
                name: "some-pkg".into(),
                version: "beta".into()
            }
        );
    }

    #[test]
    fn parse_rejects_bad_names() {
        assert!(parse("").is_err());
        assert!(parse("@only-scope").is_err()); // 缺 /
        assert!(parse("UPPER").is_err()); // 大写
        assert!(parse("a/b").is_err()); // 裸名带 /
        assert!(parse("pkg;rm -rf").is_err()); // 注入字符
        assert!(parse("pkg@ver with space").is_err());
        // `@` 只能出现在 scoped 前缀位
        assert!(parse("@a/b@c@1.0").is_err());
        // 版本段的路径危险形态
        assert!(parse("pkg@..").is_err());
        assert!(parse("pkg@1..2").is_err());
        assert!(parse("pkg@.hidden").is_err());
    }

    #[test]
    fn parse_local_requires_absolute() {
        assert!(parse("file:relative/path").is_err());
        // 平台原生绝对路径（Windows 上 `/opt/...` 是「当前盘符根 + opt」，
        // 不算绝对路径——测试按平台取对应形态）。
        #[cfg(windows)]
        let abs = r"file:C:\tools\kiro";
        #[cfg(windows)]
        let want = r"C:\tools\kiro";
        #[cfg(not(windows))]
        let abs = "file:/opt/plugins/kiro";
        #[cfg(not(windows))]
        let want = "/opt/plugins/kiro";
        let ok = parse(abs).unwrap();
        match ok {
            InstallSource::Local { path } => assert_eq!(path, want),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_magpie_prefix() {
        let ok = parse("magpie:@magpie-community/kiro").unwrap();
        // 先取 source_ref 再解构：`match` 按值移动字段后不能再借 `ok`
        assert_eq!(ok.source_ref(), "@magpie-community/kiro");
        match ok {
            InstallSource::Magpie {
                name,
                version,
                archive_url,
                integrity,
            } => {
                assert_eq!(name, "@magpie-community/kiro");
                assert_eq!(version, "");
                assert_eq!(archive_url, None);
                assert_eq!(integrity, None);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parse_github_shorthand() {
        assert_eq!(
            parse("github:owner/repo#v1.0").unwrap(),
            InstallSource::Github {
                repository: "owner/repo".into(),
                reference: "v1.0".into()
            }
        );
        assert_eq!(
            parse("github:owner/repo").unwrap(),
            InstallSource::Github {
                repository: "owner/repo".into(),
                reference: "".into()
            }
        );
        // ref 拒 ..
        assert!(parse("github:owner/repo#../../etc").is_err());
        // repo 段拒 ..
        assert!(parse("github:../repo").is_err());
    }

    #[test]
    fn parse_github_https_forms() {
        for (input, ref_, repo) in [
            ("https://github.com/owner/repo", "", "owner/repo"),
            ("https://github.com/owner/repo.git", "", "owner/repo"),
            ("https://github.com/owner/repo/tree/v2", "v2", "owner/repo"),
            ("https://github.com/owner/repo/", "", "owner/repo"),
        ] {
            let ok = parse(input).unwrap();
            assert_eq!(
                ok,
                InstallSource::Github {
                    repository: repo.into(),
                    reference: ref_.into()
                },
                "input={input}"
            );
        }
        // 拒：账号信息、别的域名、明文 HTTP、多余路径段
        assert!(parse("https://user@github.com/owner/repo").is_err());
        assert!(parse("https://ghmirror.example/owner/repo").is_err());
        assert!(parse("http://github.com/owner/repo").is_err());
        assert!(parse("https://github.com/owner").is_err());
        assert!(parse("https://github.com/owner/repo/issues/1").is_err());
    }

    #[test]
    fn source_ref_npm_includes_version() {
        let s = InstallSource::Npm {
            name: "pkg".into(),
            version: "1.0.0".into(),
        };
        assert_eq!(s.source_ref(), "pkg@1.0.0");
        let g = InstallSource::Github {
            repository: "o/r".into(),
            reference: "main".into(),
        };
        assert_eq!(g.source_ref(), "o/r@main");
    }

    #[test]
    fn encode_ref_escapes_slash() {
        assert_eq!(encode_ref("feature/x"), "feature%2Fx");
        assert_eq!(encode_ref("v1.0"), "v1.0");
    }

    #[test]
    fn npm_meta_url_escapes_scope() {
        assert_eq!(
            npm_meta_url("@magpie-community/kiro").unwrap(),
            "https://registry.npmjs.org/@magpie-community%2Fkiro"
        );
        assert_eq!(
            npm_meta_url("left-pad").unwrap(),
            "https://registry.npmjs.org/left-pad"
        );
        assert!(npm_meta_url("BAD/NAME").is_err());
    }

    #[test]
    fn resolve_npm_tarball_latest_and_pinned() {
        let meta: serde_json::Value = serde_json::from_str(
            r#"{
              "dist-tags": {"latest": "1.0.0"},
              "versions": {
                "1.0.0": {"dist": {
                  "tarball": "https://registry.npmjs.org/my-pkg/-/my-pkg-1.0.0.tgz",
                  "integrity": "sha512-abc=="
                }},
                "0.9.0": {"dist": {
                  "tarball": "https://registry.npmjs.org/my-pkg/-/my-pkg-0.9.0.tgz",
                  "integrity": "sha256-def=="
                }}
              }
            }"#,
        )
        .unwrap();
        // latest
        let t = resolve_npm_tarball(&meta, "").unwrap();
        assert!(t.url.ends_with("my-pkg-1.0.0.tgz"));
        assert_eq!(t.integrity.as_deref(), Some("sha512-abc=="));
        // 精确版本
        let t = resolve_npm_tarball(&meta, "0.9.0").unwrap();
        assert!(t.url.ends_with("my-pkg-0.9.0.tgz"));
        // dist-tag
        let meta2: serde_json::Value = serde_json::from_str(
            r#"{
              "dist-tags": {"latest": "1.0.0", "beta": "0.9.0"},
              "versions": {
                "1.0.0": {"dist": {"tarball": "https://registry.npmjs.org/p/-/p-1.0.0.tgz"}},
                "0.9.0": {"dist": {"tarball": "https://registry.npmjs.org/p/-/p-0.9.0.tgz"}}
              }
            }"#,
        )
        .unwrap();
        let t = resolve_npm_tarball(&meta2, "beta").unwrap();
        assert!(t.url.ends_with("p-0.9.0.tgz"));
    }

    #[test]
    fn resolve_npm_tarball_rejects_foreign_tarball() {
        let meta: serde_json::Value = serde_json::from_str(
            r#"{
              "dist-tags": {"latest": "1.0.0"},
              "versions": {
                "1.0.0": {"dist": {"tarball": "https://evil.example/pkg.tgz"}}
              }
            }"#,
        )
        .unwrap();
        let err = resolve_npm_tarball(&meta, "").unwrap_err();
        assert!(err.to_string().contains("registry.npmjs.org"), "{err}");
    }

    #[test]
    fn resolve_npm_tarball_rejects_missing_version() {
        let meta: serde_json::Value = serde_json::from_str(
            r#"{
              "dist-tags": {"latest": "1.0.0"},
              "versions": {"1.0.0": {"dist": {"tarball": "https://registry.npmjs.org/p.tgz"}}}
            }"#,
        )
        .unwrap();
        assert!(resolve_npm_tarball(&meta, "9.9.9").is_err());
    }
}
