//! 安装编排：来源 → 下载 → 校验 → 解包 → 依赖 → 登记。
//!
//! 这一层只做**串接**，各步的实现分在：
//! - `source.rs` 解析安装来源（含 GitHub commit 固定）
//! - `packages.rs` 校验与安全解包
//! - `dependencies.rs` JS 依赖安装
//! - `storage.rs` KV + 磁盘投影
//!
//! ## 四条下载通路的差异
//!
//! | 来源 | 取包方式 | 完整性 |
//! |---|---|---|
//! | `magpie:` | dist tgz（raw.githubusercontent 的 dist 分支）| dist index 的 sha512 |
//! | npm | `registry.npmjs.org/{name}/-/{name}-{version}.tgz` | `dist.integrity` |
//! | `github:` | codeload tarball（commit 固定） | 无（靠 commit SHA 固定内容）|
//! | `file:` | 本机目录（不解包，校验清单后直接登记） | 无（本机信任） |
//!
//! ## 为什么 npm 下载要单独拼 URL
//!
//! `registry.npmjs.org` 的 tarball 路径是 `/{name}/-/{basename}-{version}.tgz`，
//! 其中 scoped 包的 `basename` 去掉了 `@scope/` 前缀（`@magpie-community/x`
//! → `x-1.0.0.tgz`）。不按这个规则拼会 404。**且必须校验 tarball 域名**：
//! `dist.tarball` 字段理论上可被指到任意主机，只接受 registry.npmjs.org。

use std::path::Path;

use anyhow::{bail, Context, Result};

use super::source::InstallSource;
use super::types::{ScriptPackage, ScriptProviderInfo, ScriptSource};

/// 单次下载字节上限（与 packages.rs 的压缩体上限一致）。
const MAX_DOWNLOAD_BYTES: usize = 32 * 1024 * 1024;

/// 安装结果：落盘后的包元数据（未写 KV——由 operations 决定何时登记）。
#[derive(Debug)]
pub struct InstalledPackage {
    /// 目录名（`script-{hash}`）。
    pub dir_name: String,
    /// 包名。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 入口相对路径。
    pub entry: String,
    /// 来源类型。
    pub source: ScriptSource,
    /// 来源定位串（`magpie:{name}` / `name@ver` / `repo@ref` / 路径）。
    pub source_ref: String,
}

/// 把来源下载并解包到 `{script_root}/packages/`，返回元数据。
///
/// `declared_providers` 是目录条目声明的 provider（magpie 通路带出）；
/// 其它通路为空，由 js-host 握手上报真实 id。
pub async fn fetch_and_unpack(
    client: &reqwest::Client,
    source: &InstallSource,
    script_root: &Path,
    declared_providers: Vec<ScriptProviderInfo>,
) -> Result<InstalledPackage> {
    let packages_root = script_root.join("packages");

    let (archive, name, expected_integrity): (Vec<u8>, String, Option<String>) = match source {
        InstallSource::Local { path } => {
            // 本机目录：不解包，直接把目录当作包根校验后「就地登记」。
            return install_local(path, script_root, source, declared_providers).await;
        }
        InstallSource::Magpie {
            name,
            version,
            archive_url,
            integrity,
        } => {
            // magpie 官方包的权威源就是 dist tgz（archive_url 必填，entry_to_source
            // 已经拦过）。只有第三方 recommended 条目（走 npm 发布）才缺它。
            match archive_url {
                Some(u) => {
                    let bytes = download(client, u).await?;
                    (bytes, name.clone(), integrity.clone())
                }
                None => {
                    let meta = fetch_npm_meta(client, name).await?;
                    let resolved = super::source::resolve_npm_tarball(&meta, version)
                        .with_context(|| format!("解析 npm 包 {name} 的 tarball 失败"))?;
                    let bytes = download(client, &resolved.url).await?;
                    (bytes, name.clone(), resolved.integrity)
                }
            }
        }
        InstallSource::Npm { name, version } => {
            // npm 通路的 URL 与 integrity 都从 registry 元数据解析（source.rs 的
            // `resolve_npm_tarball` 是**唯一事实源**：它校验 dist.tarball 属地
            // registry.npmjs.org，避免元数据把包指到第三方站点）。
            let meta = fetch_npm_meta(client, name).await?;
            let resolved = super::source::resolve_npm_tarball(&meta, version)
                .with_context(|| format!("解析 npm 包 {name} 的 tarball 失败"))?;
            let bytes = download(client, &resolved.url).await?;
            (bytes, name.clone(), resolved.integrity)
        }
        InstallSource::Github {
            repository,
            reference,
        } => {
            let resolved = super::source::resolve_github(client, repository, reference)
                .await
                .with_context(|| format!("解析 GitHub 仓库 {repository} 失败"))?;
            let bytes = download(client, &resolved.tarball_url).await?;
            // GitHub tarball 没有 integrity 声明；内容由 commit SHA 固定。
            // 包名从 tarball 内的 package.json 读，这里先用 repo 名占位。
            let fallback_name = repository
                .split('/')
                .next_back()
                .unwrap_or("unknown")
                .to_string();
            (bytes, fallback_name, None)
        }
    };

    // GitHub 通路的实际包名在 tarball 内，需先探测；其余通路已知。
    let actual_name = if matches!(source, InstallSource::Github { .. }) {
        peek_package_name(&archive).unwrap_or(name)
    } else {
        name
    };

    if let Some(integrity) = &expected_integrity {
        super::packages::verify_integrity(&archive, integrity).context("包完整性校验失败")?;
    } else if !matches!(source, InstallSource::Github { .. }) {
        // npm/magpie 通路没有 integrity 属于异常：索引里本该有。拒绝而不是放过。
        bail!("该来源未提供完整性声明，拒绝安装（{actual_name}）");
    }

    let manifest = super::packages::unpack_tarball(&archive, &actual_name, &packages_root)
        .context("解包失败")?;

    let (source_kind, source_ref) = match source {
        InstallSource::Magpie { name, .. } => (ScriptSource::Magpie, format!("magpie:{name}")),
        InstallSource::Npm { name, version } => (
            ScriptSource::Npm,
            if version.is_empty() {
                name.clone()
            } else {
                format!("{name}@{version}")
            },
        ),
        InstallSource::Github {
            repository,
            reference,
        } => (
            ScriptSource::Github,
            if reference.is_empty() {
                repository.clone()
            } else {
                format!("{repository}@{reference}")
            },
        ),
        InstallSource::Local { path } => (ScriptSource::Local, path.clone()),
    };

    let _ = declared_providers; // 由 operations 从目录条目带出并登记，这里不透传

    Ok(InstalledPackage {
        dir_name: super::packages::dir_name_for(&manifest.name),
        name: manifest.name,
        version: manifest.version,
        entry: manifest.entry,
        source: source_kind,
        source_ref,
    })
}

/// 本机目录安装：只校验 package.json，不复制、不解包。
///
/// 为什么不复制：`file:` 来源是运维自己的目录，多半是开发中的包，
/// 复制一份会让「改了源码但运行时还是旧副本」成为常态。直接登记原路径，
/// 由 js-host 直接加载。
async fn install_local(
    path: &str,
    script_root: &Path,
    source: &InstallSource,
    _declared: Vec<ScriptProviderInfo>,
) -> Result<InstalledPackage> {
    let dir = Path::new(path);
    if !dir.is_dir() {
        bail!("本机目录不存在：{path}");
    }
    let manifest_bytes = std::fs::read(dir.join("package.json"))
        .with_context(|| format!("读取 package.json 失败：{path}"))?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("package.json 不是合法 JSON")?;
    let name = manifest
        .get("name")
        .and_then(|v| v.as_str())
        .context("package.json 缺少 name")?
        .to_string();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .context("package.json 缺少 version")?
        .to_string();
    super::packages::validate_name(&name)?;
    super::packages::validate_version(&version)?;
    let entry = super::packages::resolve_entry(&manifest)?;
    // 入口路径校验：绝对路径 / `..` / 盘符前缀一律拒绝（否则 dir.join(entry)
    // 会把入口指到目录外，宿主随之加载任意文件）。
    let Some(entry_rel) = super::packages::safe_entry_rel(&entry) else {
        bail!("入口路径非法（绝对路径或含 ..）：{entry}");
    };
    let entry_abs = dir.join(&entry_rel);
    // 入口必须真实存在
    if !entry_abs.is_file() {
        bail!("入口文件不存在：{}", entry_abs.display());
    }
    let _ = script_root;
    let _ = source;
    Ok(InstalledPackage {
        dir_name: super::packages::dir_name_for(&name),
        name,
        version,
        entry,
        source: ScriptSource::Local,
        source_ref: path.to_string(),
    })
}

/// 拉 npm registry 的完整元数据（versions / dist-tags / dist）。
///
/// 解析交给 `source::resolve_npm_tarball`：它比硬拼 URL 路径可靠——
/// registry 的 tarball 路径规则（scoped 包 basename 去 scope）随包而异，
/// 直接读 `dist.tarball` 才是权威值，且能顺带拿到 integrity。
async fn fetch_npm_meta(client: &reqwest::Client, name: &str) -> Result<serde_json::Value> {
    super::packages::validate_name(name)?;
    // 全量元数据（不带版本段）：一个请求同时拿到 versions + dist-tags。
    let url = format!("https://registry.npmjs.org/{}", name.replace('/', "%2f"));
    let resp = client
        .get(&url)
        .header("Accept", "application/json")
        .send()
        .await
        .with_context(|| format!("查询 npm registry 失败：{name}"))?;
    if !resp.status().is_success() {
        bail!(
            "包在 npm 不存在或查询失败：{name}（HTTP {}）",
            resp.status()
        );
    }
    resp.json().await.context("解析 npm registry 响应失败")
}

/// 从 tarball 里只读**根级** package.json 的 name（GitHub 通路用）。
///
/// GitHub codeload 布局是 `{repo}-{sha}/package.json`；仓库里还可能有
/// `examples/x/package.json`、`packages/y/package.json` 等嵌套清单，且
/// tar 顺序不保证根清单在前。只按「恰好两个路径分量且末段是
/// package.json」判定根清单，避免取到嵌套包的 name 导致后续解包因
/// 「name 与安装来源不一致」失败。
fn peek_package_name(archive: &[u8]) -> Option<String> {
    use std::io::Read as _;
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive));
    let mut ar = tar::Archive::new(decoder);
    for entry in ar.entries().ok()? {
        let entry = entry.ok()?;
        let path = entry.path().ok()?.into_owned();
        let comps: Vec<_> = path.components().collect();
        let is_root_manifest = comps.len() == 2
            && matches!(comps[1], std::path::Component::Normal(n) if n == "package.json");
        if !is_root_manifest {
            continue;
        }
        // 有界读取：清单超限按「读不到」处理（不把大文件读进内存）。
        let mut buf = Vec::new();
        entry.take(256 * 1024 + 1).read_to_end(&mut buf).ok()?;
        if buf.len() > 256 * 1024 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_slice(&buf).ok()?;
        return v.get("name").and_then(|n| n.as_str()).map(str::to_string);
    }
    None
}

/// 有界下载。
async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("下载失败：{url}"))?;
    if !resp.status().is_success() {
        bail!("下载失败：HTTP {}（{url}）", resp.status());
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_DOWNLOAD_BYTES {
            bail!("包体过大：{len} bytes（上限 {MAX_DOWNLOAD_BYTES}）");
        }
    }
    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("读取失败：{url}"))?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        bail!(
            "包体过大：{} bytes（上限 {MAX_DOWNLOAD_BYTES}）",
            bytes.len()
        );
    }
    Ok(bytes.to_vec())
}

/// 把目录条目转成安装来源（operations 装包时调）。
///
/// 三条分支的口径：
/// - Magpie：**必须**有 `archive_url` + `integrity`（实测 npm 上多数官方包 404，
///   dist tgz 是权威源；没有完整性声明的包一律拒绝——宁可不装也不装无校验的包）
/// - Npm：直接按包名装，integrity 由 install 阶段现查 npm registry
/// - Github：`source_ref` 是 `owner/repo[@ref]`，拆开交给 resolve_github
/// - Local：`source_ref` 就是绝对路径
pub fn entry_to_source(entry: &super::types::ScriptCatalogEntry) -> Result<InstallSource> {
    match entry.source {
        ScriptSource::Magpie => {
            let archive_url = entry
                .archive_url
                .clone()
                .filter(|u| !u.is_empty())
                .context("Magpie 条目缺少 dist tgz 下载地址（archive_url）")?;
            if !archive_url
                .starts_with("https://raw.githubusercontent.com/magpie-community/plugins/")
            {
                bail!("Magpie 下载地址不在官方 dist 分支：{archive_url}");
            }
            let integrity = entry
                .integrity
                .clone()
                .filter(|i| !i.is_empty())
                .context("Magpie 条目缺少 integrity，拒绝安装无校验的包")?;
            Ok(InstallSource::Magpie {
                name: entry.name.clone(),
                version: String::new(),
                archive_url: Some(archive_url),
                integrity: Some(integrity),
            })
        }
        ScriptSource::Npm => Ok(InstallSource::Npm {
            name: entry.name.clone(),
            version: String::new(),
        }),
        ScriptSource::Github => {
            let (repository, reference) = match entry.source_ref.split_once('@') {
                Some((repo, r)) => (repo.to_string(), r.to_string()),
                None => (entry.source_ref.clone(), String::new()),
            };
            Ok(InstallSource::Github {
                repository,
                reference,
            })
        }
        ScriptSource::Local => Ok(InstallSource::Local {
            path: entry.source_ref.clone(),
        }),
    }
}

/// 把安装结果转成待登记的包元数据。
pub fn to_package(
    installed: InstalledPackage,
    declared_providers: Vec<ScriptProviderInfo>,
    repository: Option<String>,
    summary: Option<String>,
) -> ScriptPackage {
    ScriptPackage {
        id: installed.name,
        label: String::new(),
        version: installed.version,
        source: installed.source,
        publisher: Default::default(),
        providers: declared_providers,
        entry: Some(installed.entry),
        source_ref: installed.source_ref,
        repository,
        summary,
        installed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        enabled: false,
        trusted: false,
        options: Default::default(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_npm_meta_rejects_bad_name() {
        // 名称校验在发请求前就拒（不浪费一次网络往返）
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = crate::test_http_client();
        assert!(rt.block_on(fetch_npm_meta(&client, "../evil")).is_err());
        assert!(rt.block_on(fetch_npm_meta(&client, "")).is_err());
    }

    #[test]
    fn peek_name_reads_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let _ = dir;
        // 复用 packages 的测试构造器
        let tar = super::super::packages::tests::make_tarball(&[
            (
                "package.json",
                br#"{"name":"gh-pkg","version":"1.0.0","main":"index.js"}"#,
            ),
            ("index.js", b"x"),
        ]);
        assert_eq!(peek_package_name(&tar).as_deref(), Some("gh-pkg"));
    }

    #[test]
    fn peek_name_ignores_nested_manifest_before_root() {
        // tar 顺序里先出现 examples/x/package.json 时，不能取错包名。
        let tar = super::super::packages::tests::make_tarball(&[
            (
                "examples/demo/package.json",
                br#"{"name":"demo","version":"9.9.9"}"#,
            ),
            (
                "package.json",
                br#"{"name":"gh-pkg","version":"1.0.0","main":"index.js"}"#,
            ),
            ("index.js", b"x"),
        ]);
        assert_eq!(peek_package_name(&tar).as_deref(), Some("gh-pkg"));
    }

    #[test]
    fn install_local_rejects_entry_escaping_package_dir() {
        let dir = tempfile::tempdir().unwrap();
        // 包目录外的文件：入口若指向它会逃出包根
        std::fs::write(dir.path().join("outside.js"), b"x").unwrap();
        let pkg = dir.path().join("pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            br#"{"name":"p","version":"1.0.0","main":"../outside.js"}"#,
        )
        .unwrap();
        let src = InstallSource::Local {
            path: pkg.to_string_lossy().into_owned(),
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let res = rt.block_on(fetch_and_unpack(
            &crate::test_http_client(),
            &src,
            dir.path(),
            vec![],
        ));
        let msg = match res {
            Ok(_) => panic!("越界入口必须被拒绝"),
            Err(e) => format!("{e:#}"),
        };
        assert!(msg.contains("入口路径非法"), "{msg}");
    }

    #[test]
    fn to_package_sets_source_and_entry() {
        let installed = InstalledPackage {
            dir_name: "script-abc".into(),
            name: "@x/y".into(),
            version: "1.0.0".into(),
            entry: "index.mjs".into(),
            source: ScriptSource::Magpie,
            source_ref: "magpie:@x/y".into(),
        };
        let pkg = to_package(installed, vec![], None, None);
        assert_eq!(pkg.id, "@x/y");
        assert_eq!(pkg.source, ScriptSource::Magpie);
        assert_eq!(pkg.entry.as_deref(), Some("index.mjs"));
        assert!(!pkg.enabled && !pkg.trusted, "新装包默认不启用、不信任");
    }

    #[test]
    fn entry_to_source_prefers_dist_archive() {
        // Magpie 官方包必须带 dist tgz + integrity（npm 上多数 404）。
        let entry = super::super::types::ScriptCatalogEntry {
            name: "@magpie-community/agent-agy".into(),
            archive_url: Some(
                "https://raw.githubusercontent.com/magpie-community/plugins/dist/tgz/x.tgz".into(),
            ),
            integrity: Some("sha512-abc".into()),
            source: ScriptSource::Magpie,
            ..Default::default()
        };
        let src = entry_to_source(&entry).unwrap();
        match src {
            InstallSource::Magpie {
                archive_url,
                integrity,
                ..
            } => {
                assert!(archive_url.unwrap().contains("/dist/tgz/"));
                assert_eq!(integrity.as_deref(), Some("sha512-abc"));
            }
            other => panic!("应为 Magpie 变体，实际 {other:?}"),
        }
    }

    #[test]
    fn entry_to_source_requires_integrity_for_magpie() {
        let entry = super::super::types::ScriptCatalogEntry {
            name: "@magpie-community/x".into(),
            archive_url: Some("https://example.com/x.tgz".into()),
            integrity: None,
            source: ScriptSource::Magpie,
            ..Default::default()
        };
        assert!(entry_to_source(&entry).is_err(), "缺 integrity 必须拒绝");
    }

    #[test]
    fn entry_to_source_third_party_goes_github_or_npm() {
        let entry = super::super::types::ScriptCatalogEntry {
            name: "some-pkg".into(),
            source: ScriptSource::Npm,
            source_ref: "some-pkg".into(),
            ..Default::default()
        };
        assert!(matches!(
            entry_to_source(&entry).unwrap(),
            InstallSource::Npm { .. }
        ));

        let entry = super::super::types::ScriptCatalogEntry {
            name: "repo-pkg".into(),
            source: ScriptSource::Github,
            source_ref: "owner/repo".into(),
            ..Default::default()
        };
        assert!(matches!(
            entry_to_source(&entry).unwrap(),
            InstallSource::Github { .. }
        ));
    }
}
