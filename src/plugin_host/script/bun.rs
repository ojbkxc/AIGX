//! Bun 运行时的按需分发与校验。
//!
//! 脚本插件（Magpie / OpenCode 风格的 JS provider 脚本）需要 JS 运行时执行。
//! AIGX 本仓此前没有任何 JS 运行时基础设施，这里只负责**运行时的取得**：
//! 固定版本、按平台下载、哈希校验、原子落盘、可用性探针。脚本加载与协议
//! 由 `script::host_rs` / `script::host/*.mjs` 负责，本模块不碰。
//!
//! ## 为什么锁版本
//!
//! 脚本插件是按 npm 包分发的第三方 JS，运行时版本漂移会让「同一个脚本在
//! 不同机器上行为不同」。锁 1.4.2 只解决 AIGX 自己的一致性；脚本作者声明
//! 的 Bun API 兼容面由上游生态决定，本仓不追。
//!
//! ## 为什么必须 --no-env-file
//!
//! Bun 默认会读 cwd 下的 `.env` / `bunfig.toml`。脚本包目录是**第三方内容**，
//! 让一个第三方目录里的配置文件影响运行时行为，等于把「包内容」提升成
//! 「运行时配置」。启动脚本宿主时必须显式关闭。
//!
//! ## 为什么原子落盘
//!
//! 下载/解压可能被中断（进程被杀、磁盘满）。半截的 `bun` 二进制如果不带
//! 版本标记就留在最终路径上，下次 `ensure()` 会 probe 失败再下载——行为是对的，
//! 但「最终路径上永远要么没有、要么是完整的」更省事：先写临时目录，
//! 校验通过后 rename。

use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::plugin_host::script::types::script_plugins_root;

/// 锁定的 Bun 版本。
///
/// 与 Magpie / OpenCode 脚本生态当前使用的版本对齐；换版本要同步更新下方
/// 资产表的全部 SHA256（每一条都必须来自官方 Release 的实测摘要）。
pub const VERSION: &str = "1.4.2";

/// 单次下载的字节上限。Bun 的 zip 最大约 40MB，留足余量但拒绝无界流。
const MAX_DOWNLOAD_BYTES: usize = 200 * 1024 * 1024;

/// 解压后总字节上限。zip 的压缩比可以被构造得极高，必须限制展开量。
const MAX_EXTRACTED_BYTES: u64 = 300 * 1024 * 1024;

/// 探针超时。Bun 正常启动是毫秒级；15s 足够覆盖冷启动与杀软扫描。
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// 解压临时文件名的进程内单调序（配合 pid 保证同一台机器上唯一）。
static UNIQUE_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 一个平台对应的 Bun 资产。
#[derive(Clone, Copy, Debug)]
pub struct Asset {
    /// 官方 Release 里的 zip 文件名（不含版本前缀）。
    pub file: &'static str,
    /// 该 zip 的 SHA256（取自 GitHub Release API 的 digest，实测值）。
    pub sha256: &'static str,
    /// 解压后二进制在 zip 内的相对路径（zip 顶层目录名因平台而异）。
    pub inner: &'static str,
}

/// (os, arch, musl) → Bun 资产。无匹配返回 `None`（调用方决定报错口径）。
///
/// 为什么不用 `std::env::consts::ARCH` 一刀切：Linux 的 musl 与 glibc 是
/// **同一 arch 的不同 libc**，必须运行时探测（见 [`is_musl`]）。
fn asset_table(os: &str, arch: &str, musl: bool) -> Option<Asset> {
    let asset = match (os, arch, musl) {
        // Windows：用 baseline 版（不要求 AVX2），兼容性优先于峰值性能。
        ("windows", "x86_64", _) => Asset {
            file: "bun-windows-x64-baseline.zip",
            sha256: "78c221c2376f79731ccf4e4af0b3bb46d81fefa3296c5abee09ad8a1b21e68c6",
            inner: "bun-windows-x64-baseline/bun.exe",
        },
        ("windows", "aarch64", _) => Asset {
            file: "bun-windows-aarch64.zip",
            sha256: "a7a16b876a305fd1029c66dbd27007b4f6112ae896532f675878731a21e50cfd",
            inner: "bun-windows-aarch64/bun.exe",
        },
        ("macos", "aarch64", _) => Asset {
            file: "bun-darwin-aarch64.zip",
            sha256: "90987a3a16d7db556d886ac3d551e7b6d3edf0a1cf43acaed622e8676be1d12f",
            inner: "bun-darwin-aarch64/bun",
        },
        ("macos", "x86_64", _) => Asset {
            file: "bun-darwin-x64.zip",
            sha256: "80520d7e17526308c9185d261679ac6d27798d3803a0e9f7ff9121ab8affb012",
            inner: "bun-darwin-x64/bun",
        },
        ("linux", "x86_64", false) => Asset {
            file: "bun-linux-x64.zip",
            sha256: "36368faef7527875d5ffa52e53cd48021741f2a83eb6208a8dd64068d422a913",
            inner: "bun-linux-x64/bun",
        },
        ("linux", "aarch64", false) => Asset {
            file: "bun-linux-aarch64.zip",
            sha256: "54328bbc2d9c8e0c9f892c544d66c57a83b84139e34909e5ee81758f1ac8fda7",
            inner: "bun-linux-aarch64/bun",
        },
        ("linux", "x86_64", true) => Asset {
            file: "bun-linux-x64-musl.zip",
            sha256: "4835eca59d6da70f4674f5642f6e459dcadab773695b2ed9922d131057989742",
            inner: "bun-linux-x64-musl/bun",
        },
        ("linux", "aarch64", true) => Asset {
            file: "bun-linux-aarch64-musl.zip",
            sha256: "71760b6c8ea30623b81a4907cb815d48e2ea266f2e73e751534a44a0607950df",
            inner: "bun-linux-aarch64-musl/bun",
        },
        _ => return None,
    };
    Some(asset)
}

/// 当前平台 → Bun 资产。无匹配平台时返回明确错误（不猜）。
pub fn asset_for_current_platform() -> Result<Asset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let musl = if os == "linux" { is_musl() } else { false };
    asset_table(os, arch, musl).ok_or_else(|| {
        anyhow!("当前平台没有可用的 Bun {VERSION} 资产（os={os} arch={arch} musl={musl}）")
    })
}

/// 同一 OS/ARCH 下的**另一个 libc 变体**（musl ↔ glibc）。
///
/// 生产实测：`is_musl()` 在装了 glibc 的机器上也可能误判（musl loader 残留 /
/// 探测启发式），导致永远选 musl 资产、永远 probe 失败。修法不是改探测，
/// 而是「主资产失败就换另一个 libc 变体重试一次」——两个二进制覆盖同一
/// OS/ARCH 的全部现实场景。非 Linux（Windows/macOS 只有一个变体）返回 None。
fn alternate_asset(primary: &Asset) -> Option<Asset> {
    let os = std::env::consts::OS;
    if os != "linux" {
        return None;
    }
    let arch = std::env::consts::ARCH;
    let primary_musl = primary.file.contains("musl");
    let alt = asset_table(os, arch, !primary_musl)?;
    // 防御：主备相同（探测与资产表不一致时）就没有重试意义
    if alt.file == primary.file {
        None
    } else {
        Some(alt)
    }
}

/// 运行时安装目录：`{data_dir}/script-plugins/bun/{VERSION}/{asset-stem}/`。
///
/// 目录叶子取资产文件名（去掉 `.zip`）而非 env 派生的 platform-key：换资产
/// 重试时主/备资产必须落到**不同**目录，否则 glibc 版会写进 musl 版的路径、
/// 互相覆盖。资产名天然唯一且与内容一一对应。
pub fn install_dir(data_dir: &Path) -> Result<PathBuf> {
    install_dir_for(data_dir, &asset_for_current_platform()?)
}

fn install_dir_for(data_dir: &Path, asset: &Asset) -> Result<PathBuf> {
    Ok(script_plugins_root(data_dir)
        .join("bun")
        .join(VERSION)
        .join(asset.file.trim_end_matches(".zip")))
}

/// 可执行文件路径。
pub fn binary_path(data_dir: &Path) -> Result<PathBuf> {
    binary_path_for(data_dir, &asset_for_current_platform()?)
}

fn binary_path_for(data_dir: &Path, asset: &Asset) -> Result<PathBuf> {
    let suffix = if cfg!(windows) { "bun.exe" } else { "bun" };
    Ok(install_dir_for(data_dir, asset)?.join(suffix))
}

/// 运行期探测是否为 musl libc。
///
/// 判据：**存在 `ld-musl-*` loader 且不存在 glibc `ld-linux-*` loader**。
/// 只看前者会误判——生产实证（Debian 12 / glibc 2.36）上存在
/// `/lib/ld-musl-x86_64.so.1` 的第三方符号链接，仅凭它选 musl 资产会
/// 下载一个必然跑不起来的 Bun（40 行 `Error relocating`）。glibc loader
/// 的存在才是「这台机器能跑 glibc 二进制」的直接证据；glibc loader 常见
/// 位置：`/lib64/`、`/lib/`、`/lib/<multiarch>/`。
fn is_musl() -> bool {
    libc_is_musl(
        loader_present("/lib", "ld-musl-", 2),
        loader_present("/lib", "ld-linux-", 2)
            || loader_present("/lib64", "ld-linux-", 2)
            || loader_present("/usr/lib", "ld-linux-", 2),
    )
}

/// libc 判定（纯函数，便于单测）：musl loader 存在且 glibc loader 不存在。
fn libc_is_musl(has_musl_loader: bool, has_glibc_loader: bool) -> bool {
    has_musl_loader && !has_glibc_loader
}

/// 目录（含 depth 层内的子目录）下是否有以 `prefix` 开头的条目。
fn loader_present(dir: &str, prefix: &str, depth: u8) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(prefix) {
            return true;
        }
        if depth > 1 && entry.path().is_dir() {
            let sub = entry.path();
            if let Some(sub) = sub.to_str() {
                if loader_present(sub, prefix, depth - 1) {
                    return true;
                }
            }
        }
    }
    false
}

/// 下载地址（官方 Release，不接受镜像）。
fn download_url(asset: &Asset) -> String {
    format!(
        "https://github.com/oven-sh/bun/releases/download/bun-v{VERSION}/{}",
        asset.file
    )
}

/// 负缓存 TTL：两种资产都失败后，这段时间内直接复用上次错误，不再下载。
///
/// 生产实证：用户每点一次「启用」都会走 `ensure()`；若每次都不加判断地
/// 重下 73MB 然后必然失败，就是「无限循环式烧流量」。10 分钟足够让运维
/// 看到错误并修环境，又不会把一次网络抖动放大成长期不可用。
const UNAVAILABLE_TTL_SECS: i64 = 600;
/// 负缓存文件名（放在版本目录下，与资产变体无关）。
const UNAVAILABLE_FILE: &str = ".unavailable";

/// 确保运行时可用，返回可执行文件路径。
///
/// 已存在则先 probe（可能被降级/替换/架构不符 —— 不信任路径上「有文件」
/// 这件事本身）；不存在则下载 → 校验 → 解压 → 原子落盘 → probe。
///
/// ## 换资产重试（生产 P0）
///
/// `is_musl()` 是启发式探测，可能误判（musl loader 残留 / glibc 机器被
/// 判成 musl）。一旦判错，原逻辑会永远下载同一个跑不起来的 musl 包、
/// 每次调用白烧 73MB 且必然失败。修法：主资产失败后**换另一个 libc
/// 变体重试一次**（musl ↔ glibc）——两个二进制覆盖同一 OS/ARCH 的全部
/// 现实场景；两个都失败才落负缓存。
pub async fn ensure(data_dir: &Path) -> Result<PathBuf> {
    // 负缓存命中：直接复用上次错误（不再下载）。
    if let Some(err) = read_unavailable(data_dir) {
        return Err(err);
    }

    let primary = asset_for_current_platform()?;
    let first_err = match try_install(data_dir, &primary).await {
        Ok(path) => {
            clear_unavailable(data_dir);
            cleanup_stale_layout(data_dir, &primary);
            return Ok(path);
        }
        Err(e) => e,
    };

    // 主资产不可用 → 换 libc 变体重试一次（非 Linux 无备选）。
    if let Some(alt) = alternate_asset(&primary) {
        tracing::warn!(
            "[Script] 主资产 {} 不可用（{first_err:#}），改用备选 {} 重试",
            primary.file,
            alt.file
        );
        match try_install(data_dir, &alt).await {
            Ok(path) => {
                tracing::info!("[Script] 备选资产 {} 可用，Bun 就绪", alt.file);
                clear_unavailable(data_dir);
                cleanup_stale_layout(data_dir, &alt);
                return Ok(path);
            }
            Err(e2) => {
                let combined = anyhow!(
                    "Bun {VERSION} 安装失败：已尝试 musl 与 glibc 两种资产，均失败。\n\
                     主资产（{}）：{}\n备选（{}）：{}",
                    primary.file,
                    truncate_stderr(&format!("{first_err:#}")),
                    alt.file,
                    truncate_stderr(&format!("{e2:#}")),
                );
                write_unavailable(data_dir, &combined);
                return Err(combined);
            }
        }
    }

    // 非 Linux：无备选，直接失败并落负缓存。
    let err = anyhow!(
        "Bun {VERSION} 安装失败（资产 {}）：{}",
        primary.file,
        truncate_stderr(&format!("{first_err:#}"))
    );
    write_unavailable(data_dir, &err);
    Err(err)
}

/// 清理旧布局残留：`{script_plugins_root}/bun/{VERSION}/` 下，除当前资产
/// 目录之外、含 `bun`/`bun.exe` 的子目录一并删除。
///
/// 目录布局从 `{VERSION}/{platform-key}/` 改到 `{VERSION}/{asset-stem}/` 后，
/// 旧路径（如 `linux-x86_64-musl/bun-linux-x64-musl/bun`）不再被读取，
/// 但会永久占 73MB。成功拿到可用 Bun 后顺手回收。失败只 warn，不影响
/// `ensure` 成功。**不碰上一级的 `.unavailable` 负缓存文件**。
fn cleanup_stale_layout(data_dir: &Path, keep: &Asset) {
    let version_dir = script_plugins_root(data_dir).join("bun").join(VERSION);
    let keep_stem = keep.file.trim_end_matches(".zip");
    let Ok(entries) = std::fs::read_dir(&version_dir) else {
        return; // 目录不存在（全新安装）→ 无事可做
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue; // 跳过 .unavailable 等文件
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == keep_stem {
            continue; // 保留当前资产目录
        }
        // 只删「看起来是 Bun 安装目录」的（含 bun 二进制），不误删其它资产。
        // 旧布局是 `{platform-key}/{asset-stem}/bun`（二进制在两层下），
        // 所以递归找；深度限制 3 层足够且不会变成全盘扫描。
        if dir_contains_bun(&path, 3) {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => tracing::info!("[Script] 清理旧 Bun 布局残留：{}", path.display()),
                Err(err) => {
                    tracing::warn!(
                        "[Script] 清理旧 Bun 布局失败（忽略）{}：{err}",
                        path.display()
                    )
                }
            }
        }
    }
}

/// 目录（含子目录，深度受限）里是否有 `bun` / `bun.exe` 文件。
fn dir_contains_bun(dir: &Path, depth: usize) -> bool {
    if depth == 0 {
        return false;
    }
    if dir.join("bun").is_file() || dir.join("bun.exe").is_file() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        let p = e.path();
        p.is_dir() && dir_contains_bun(&p, depth - 1)
    })
}

/// 安装（或校验已有）单个资产，失败返回错误。
///
/// 关键：**已存在的二进制 probe 失败时不重下同一资产**——那是 P0 白烧
/// 73MB 的来源。删除坏文件后直接返回错误，让 [`ensure`] 换资产重试。
async fn try_install(data_dir: &Path, asset: &Asset) -> Result<PathBuf> {
    let path = binary_path_for(data_dir, asset)?;
    if path.is_file() {
        match probe(&path).await {
            Ok(()) => return Ok(path),
            Err(err) => {
                tracing::warn!(
                    "[Script] 已有 {} 无法通过探针（删除后不再重下同一资产）：{err:#}",
                    asset.file
                );
                let _ = std::fs::remove_dir_all(install_dir_for(data_dir, asset)?);
                bail!(
                    "已有 {} 无法运行（可能 libc/架构不匹配）：{}",
                    asset.file,
                    truncate_stderr(&format!("{err:#}"))
                );
            }
        }
    }
    download_and_install(data_dir, asset).await?;
    probe(&path).await?;
    Ok(path)
}

/// 负缓存文件路径（版本级，与资产变体无关）。
fn unavailable_path(data_dir: &Path) -> PathBuf {
    script_plugins_root(data_dir)
        .join("bun")
        .join(VERSION)
        .join(UNAVAILABLE_FILE)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 环境指纹：`is_musl()` 判定 + libc 路径特征。
///
/// 用于负缓存失效：运维装了 libstdc++/glibc 后环境指纹会变，命中负缓存
/// 时重算一次指纹，不一致就清缓存重试，无需干等 TTL。
fn env_fingerprint() -> String {
    let musl = if std::env::consts::OS == "linux" {
        is_musl()
    } else {
        false
    };
    // libc 路径特征：读 /proc/self/maps 里含 libc/ld-linux/ld-musl 的路径，
    // 拼起来取短 hash（路径字符串本身可能含地址前缀，只需区分「有没有换 libc」）。
    let libc_hint = std::fs::read_to_string("/proc/self/maps")
        .map(|maps| {
            let mut hits: Vec<String> = maps
                .lines()
                .filter_map(|l| l.split_whitespace().nth(5))
                .filter(|p| p.contains("libc") || p.contains("ld-linux") || p.contains("ld-musl"))
                .map(String::from)
                .collect();
            hits.sort();
            hits.dedup();
            hits.join(",")
        })
        .unwrap_or_default();
    let digest = Sha256::digest(libc_hint.as_bytes());
    let hash: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("musl={musl};libc={hash}")
}

/// 读负缓存；未过期返回缓存的错误，过期/缺失/损坏/环境指纹变化返回 None。
fn read_unavailable(data_dir: &Path) -> Option<anyhow::Error> {
    let bytes = std::fs::read(unavailable_path(data_dir)).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let at = v.get("at").and_then(|x| x.as_i64()).unwrap_or(0);
    let age = now_unix().saturating_sub(at);
    if !(0..UNAVAILABLE_TTL_SECS).contains(&age) {
        return None; // 时钟回拨（负）或已过期：允许重试
    }
    // 环境指纹变化（如运维补装 libstdc++/glibc）→ 立即失效重试
    let recorded = v.get("env").and_then(|x| x.as_str()).unwrap_or("");
    if recorded != env_fingerprint() {
        tracing::info!("[Script] Bun 负缓存环境指纹变化，清除并重试");
        clear_unavailable(data_dir);
        return None;
    }
    let msg = v
        .get("error")
        .and_then(|x| x.as_str())
        .unwrap_or("Bun 不可用");
    Some(anyhow!(
        "{msg}\n（负缓存命中：{} 秒内不再重试下载，避免反复烧流量）",
        UNAVAILABLE_TTL_SECS
    ))
}

fn write_unavailable(data_dir: &Path, err: &anyhow::Error) {
    let path = unavailable_path(data_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({
        "at": now_unix(),
        "env": env_fingerprint(),
        "error": format!("{err:#}"),
    });
    let _ = std::fs::write(&path, serde_json::to_vec(&body).unwrap_or_default());
}

fn clear_unavailable(data_dir: &Path) {
    let _ = std::fs::remove_file(unavailable_path(data_dir));
}

/// 截断 stderr 到「前 3 行 + 最后 1 行」。
///
/// 生产 API 返回过 5937 字节的原始 stderr，对用户毫无用处。这里只保留
/// 定位所需的头部（错误类型/首个堆栈）与尾部（真正的失败原因）。
fn truncate_stderr(text: &str) -> String {
    const HEAD: usize = 3;
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= HEAD + 1 {
        return lines.join("\n");
    }
    let head = lines[..HEAD].join("\n");
    let tail = lines[lines.len() - 1];
    format!("{head}\n…（省略 {} 行）…\n{tail}", lines.len() - HEAD - 1)
}

/// 下载 + 校验 + 解压 + 原子落盘。
async fn download_and_install(data_dir: &Path, asset: &Asset) -> Result<()> {
    let url = download_url(asset);
    tracing::info!("[Script] 下载 Bun {VERSION}：{url}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("构造 HTTP 客户端失败")?;
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("下载 Bun 失败：{url}"))?;
    if !resp.status().is_success() {
        bail!("下载 Bun 失败：HTTP {}", resp.status());
    }
    if let Some(len) = resp.content_length() {
        if len as usize > MAX_DOWNLOAD_BYTES {
            bail!("Bun 包过大：{len} bytes（上限 {MAX_DOWNLOAD_BYTES}）");
        }
    }
    let bytes = resp.bytes().await.context("读取 Bun 包失败")?;
    if bytes.len() > MAX_DOWNLOAD_BYTES {
        bail!(
            "Bun 包过大：{} bytes（上限 {MAX_DOWNLOAD_BYTES}）",
            bytes.len()
        );
    }

    // 哈希校验：官方 digest 与下载内容必须一致，否则拒绝落盘。
    let actual = hex(&Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(asset.sha256) {
        bail!("Bun 包 SHA256 不匹配：期望 {}，实际 {actual}", asset.sha256);
    }

    let target = install_dir_for(data_dir, asset)?;
    let payload = bytes.to_vec();
    let inner = asset.inner.to_string();
    let target_for_block = target.clone();
    tokio::task::spawn_blocking(move || extract_zip(&payload, &inner, &target_for_block))
        .await
        .context("解压任务失败")??;

    tracing::info!("[Script] Bun {VERSION} 已就绪：{}", target.display());
    Ok(())
}

/// 解压 zip 并只保留入口二进制，原子落盘。
///
/// 只取 `inner` 指定的那一个文件：Bun 的 zip 里还有 `bunx`、`bunffi` 等
/// 多份二进制，全部解出来既占空间又扩大了攻击面——脚本宿主只需要 `bun`。
fn extract_zip(bytes: &[u8], inner: &str, target_dir: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).context("Bun 包不是合法的 zip")?;

    // 先校验展开总量（防 zip bomb），再解。
    // 注意：`by_index` 返回的 entry 借住 `archive`，必须每轮结束前 drop，
    // 否则下一轮再借会报 "cannot borrow as mutable"。
    let mut total: u64 = 0;
    for i in 0..archive.len() {
        let size = {
            let entry = archive.by_index(i).context("读取 zip 条目失败")?;
            entry.size()
        };
        total = total.saturating_add(size);
        if total > MAX_EXTRACTED_BYTES {
            bail!("Bun 包展开过大：超过 {MAX_EXTRACTED_BYTES} bytes");
        }
    }

    let mut entry = archive
        .by_name(inner)
        .with_context(|| format!("Bun 包内缺少入口 {inner}"))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    std::io::copy(&mut entry, &mut buf)?;
    drop(entry);

    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("创建目录失败：{}", target_dir.display()))?;

    let name = if cfg!(windows) { "bun.exe" } else { "bun" };
    let final_path = target_dir.join(name);
    // 先写临时文件再 rename：最终路径上不会出现半截二进制。
    //
    // 临时名必须**唯一**：`ensure()` 有两个调用点（operations 安装依赖、
    // host_rs 起宿主），管理面并发安装两个包时两次 ensure 可能同时落到
    // 这里；固定名 `.bun.tmp` 会让两个解压互踩（一方 rename 走另一方还在
    // 写的文件）。pid + 进程内单调序足以在同一台机器上区分。
    let tmp = target_dir.join(format!(
        ".bun.{}.{}.tmp",
        std::process::id(),
        UNIQUE_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&tmp, &buf).context("写入临时文件失败")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .context("设置可执行权限失败")?;
    }
    std::fs::rename(&tmp, &final_path).context("原子落盘失败")?;
    Ok(())
}

/// 探针：跑 `bun --version` 并核对输出。
pub async fn probe(binary: &Path) -> Result<()> {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--version");
    scrub_env(&mut cmd);
    no_window(&mut cmd);
    // 超时后 future 被 drop：`kill_on_drop` 保证挂死的子进程不会残留。
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(PROBE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow!("bun --version 超时（{}s）", PROBE_TIMEOUT.as_secs()))?
        .with_context(|| format!("无法执行 {}", binary.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "bun --version 退出码 {:?}：{}（可能是 glibc/CPU 指令集不匹配）",
            output.status.code(),
            stderr.trim()
        );
    }
    let version = String::from_utf8_lossy(&output.stdout);
    let version = version.trim();
    if version != VERSION {
        bail!("Bun 版本不符：期望 {VERSION}，实际 {version}");
    }
    Ok(())
}

/// 清理会改变 Bun 行为的环境变量。
///
/// `BUN_OPTIONS` / `NODE_OPTIONS` 是「外部注入任意参数」的入口，
/// `NPM_CONFIG_*` 会把脚本宿主的依赖安装导向别的用户配置——三者都不该
/// 由网关进程继承给子进程。
pub fn scrub_env(cmd: &mut tokio::process::Command) {
    for (key, _) in std::env::vars() {
        if key.starts_with("BUN_") || key.starts_with("NPM_CONFIG_") || key == "NODE_OPTIONS" {
            cmd.env_remove(&key);
        }
    }
    cmd.env("DO_NOT_TRACK", "1");
}

/// 构造启动脚本宿主的参数：`bun run --no-env-file <host_dir>/main.mjs`。
///
/// cwd 必须是宿主目录（宿主用相对路径 import 同目录模块）。
pub fn run_command(binary: &Path, host_dir: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("run")
        .arg("--no-env-file")
        .arg(host_dir.join("main.mjs"))
        .current_dir(host_dir);
    scrub_env(&mut cmd);
    no_window(&mut cmd);
    cmd
}

/// Windows 下不弹控制台窗口。
///
/// 网关是后台服务，Bun 子进程弹窗会打断运维操作。`CREATE_NO_WINDOW` 只在
/// Windows 有效；非 Windows 平台编译期整段消失（`#[cfg]` 不留无用代码）。
fn no_window(cmd: &mut tokio::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_table_resolves_on_this_platform() {
        // 各 CI 平台都必须能解析出资产，否则脚本插件整体不可用。
        let asset = asset_for_current_platform().expect("当前平台应有 Bun 资产");
        assert!(asset.file.ends_with(".zip"));
        assert_eq!(asset.sha256.len(), 64, "SHA256 必须是 64 位 hex");
        assert!(asset.inner.ends_with("bun") || asset.inner.ends_with("bun.exe"));
    }

    #[test]
    fn download_url_points_at_official_release() {
        let asset = asset_for_current_platform().unwrap();
        let url = download_url(&asset);
        assert!(url.starts_with("https://github.com/oven-sh/bun/releases/download/bun-v1.4.2/"));
        assert!(url.ends_with(asset.file));
    }

    #[test]
    fn binary_path_lives_under_script_plugins_root() {
        let dir = Path::new("/tmp/data");
        let path = binary_path(dir).unwrap();
        assert!(path.starts_with(script_plugins_root(dir)));
        assert!(path.to_string_lossy().contains("bun"));
    }

    #[test]
    fn hex_is_lowercase_and_fixed_width() {
        assert_eq!(hex(&[0x00, 0xff, 0x0a]), "00ff0a");
    }

    #[test]
    fn extract_rejects_missing_entry() {
        // 造一个不含入口的合法 zip：必须报「缺少入口」而不是写半截文件。
        let mut buf = Vec::new();
        {
            use zip::write::SimpleFileOptions;
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            w.start_file("something/else.txt", SimpleFileOptions::default())
                .unwrap();
            use std::io::Write;
            w.write_all(b"hi").unwrap();
            w.finish().unwrap();
        }
        let dir = std::env::temp_dir().join("aigx_bun_test_missing_entry");
        let _ = std::fs::remove_dir_all(&dir);
        let err = extract_zip(&buf, "bun-x/bun", &dir).unwrap_err();
        assert!(err.to_string().contains("缺少入口"), "{err}");
        // 失败后最终路径上不应留下任何二进制
        assert!(!dir.join("bun").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 生产 P0 回归：Debian 12（glibc）上存在 `/lib/ld-musl-x86_64.so.1`
    /// 的第三方符号链接，只看 musl loader 会误判 → 下载 musl Bun →
    /// `Error relocating` 起不来。glibc loader 存在时必须判 glibc。
    #[test]
    fn libc_decision_prefers_glibc_when_both_loaders_exist() {
        assert!(!libc_is_musl(true, true), "两种 loader 都在 → glibc");
        assert!(libc_is_musl(true, false), "纯 musl（Alpine）");
        assert!(!libc_is_musl(false, true), "纯 glibc");
        assert!(!libc_is_musl(false, false), "探测不到 → 按 glibc");
    }

    #[test]
    fn alternate_asset_swaps_libc_on_linux() {
        // Linux 上主资产是 musl → 备选必须是 glibc（反之亦然），且不同目录。
        if std::env::consts::OS != "linux" {
            return; // 非 Linux 无备选变体
        }
        let musl = asset_table("linux", "x86_64", true).unwrap();
        let alt = alternate_asset(&musl).expect("musl 应有 glibc 备选");
        assert!(!alt.file.contains("musl"), "备选不应是 musl：{}", alt.file);
        assert_ne!(alt.file, musl.file);
        let glibc = asset_table("linux", "x86_64", false).unwrap();
        let back = alternate_asset(&glibc).expect("glibc 应有 musl 备选");
        assert!(
            back.file.contains("musl"),
            "反向备选应是 musl：{}",
            back.file
        );
    }

    #[test]
    fn alternate_asset_none_on_windows_and_macos() {
        if std::env::consts::OS == "linux" {
            return;
        }
        let a = asset_for_current_platform().unwrap();
        assert!(alternate_asset(&a).is_none(), "单变体平台不应有备选");
    }

    #[test]
    fn install_dir_is_asset_specific() {
        // 主/备资产必须落到不同目录，否则换资产重试会互相覆盖。
        let dir = Path::new("/tmp/data");
        let musl = asset_table("linux", "x86_64", true).unwrap();
        let glibc = asset_table("linux", "x86_64", false).unwrap();
        let d1 = install_dir_for(dir, &musl).unwrap();
        let d2 = install_dir_for(dir, &glibc).unwrap();
        assert_ne!(d1, d2, "不同资产目录必须不同");
        assert!(d1.to_string_lossy().contains("musl"));
        assert!(!d2.to_string_lossy().contains("musl"));
        // 都在 script_plugins_root 之下
        assert!(d1.starts_with(script_plugins_root(dir)));
        assert!(d2.starts_with(script_plugins_root(dir)));
    }

    #[test]
    fn negative_cache_roundtrip_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        // 初始无缓存
        assert!(read_unavailable(data).is_none());
        // 写入后命中
        write_unavailable(data, &anyhow!("musl 与 glibc 均失败"));
        let err = read_unavailable(data).expect("刚写入应命中");
        assert!(err.to_string().contains("musl 与 glibc"), "{err}");
        assert!(err.to_string().contains("负缓存"), "{err}");
        // 清除后不再命中
        clear_unavailable(data);
        assert!(read_unavailable(data).is_none());
    }

    #[test]
    fn negative_cache_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let path = unavailable_path(data);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 手工写一条「很久以前」的记录（带当前环境指纹，隔离 TTL 判据）
        let old = now_unix() - UNAVAILABLE_TTL_SECS - 5;
        let body = serde_json::json!({
            "at": old,
            "env": env_fingerprint(),
            "error": "old failure",
        });
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(read_unavailable(data).is_none(), "过期负缓存不应命中");
    }

    #[test]
    fn truncate_stderr_keeps_head_and_tail() {
        let text = "line1\nline2\nline3\nline4\nline5\nline6";
        let out = truncate_stderr(text);
        assert!(out.contains("line1"));
        assert!(out.contains("line6"), "尾部失败原因必须保留：{out}");
        assert!(out.contains("省略"), "应标注省略行数：{out}");
        // 短文本原样（不截断）
        assert_eq!(truncate_stderr("only one line"), "only one line");
    }

    #[tokio::test]
    async fn ensure_uses_negative_cache_without_downloading() {
        // 预置负缓存 → ensure 必须直接返回错误，不发任何网络请求。
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        write_unavailable(data, &anyhow!("cached failure marker"));
        let err = ensure(data).await.unwrap_err();
        assert!(err.to_string().contains("cached failure marker"), "{err}");
    }

    #[test]
    fn negative_cache_invalidated_on_env_change() {
        // 负缓存里记的 env 指纹与当前不一致 → 命中时应清除并返回 None（重试）。
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let path = unavailable_path(data);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let body = serde_json::json!({
            "at": now_unix(),
            "env": "musl=false;libc=deadbeef-stale",
            "error": "stale env failure",
        });
        std::fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
        assert!(
            read_unavailable(data).is_none(),
            "环境指纹变化应使负缓存失效"
        );
        // 且缓存文件已被清除
        assert!(!path.exists(), "失效后应删除负缓存文件");
    }

    #[test]
    fn cleanup_stale_layout_removes_old_dirs_keeps_current() {
        // 旧布局目录（含 bun 二进制）应被删；当前资产目录保留；
        // 上一级 .unavailable 文件不受影响。
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let version_dir = script_plugins_root(data).join("bun").join(VERSION);
        let keep = asset_for_current_platform().unwrap();
        let keep_stem = keep.file.trim_end_matches(".zip");

        // 当前资产目录（含 bun）
        let cur = version_dir.join(keep_stem);
        std::fs::create_dir_all(&cur).unwrap();
        std::fs::write(
            cur.join(if cfg!(windows) { "bun.exe" } else { "bun" }),
            b"x",
        )
        .unwrap();
        // 旧布局目录（含 bun）
        let old = version_dir
            .join("linux-x86_64-musl")
            .join("bun-linux-x64-musl");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("bun"), b"y").unwrap();
        // 无关目录（不含 bun，不应删）
        let other = version_dir.join("not-bun-dir");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("readme"), b"z").unwrap();
        // 负缓存文件
        let neg = version_dir.join(UNAVAILABLE_FILE);
        std::fs::write(&neg, b"{}").unwrap();

        cleanup_stale_layout(data, &keep);

        assert!(cur.is_dir(), "当前资产目录必须保留");
        assert!(!old.exists(), "含 bun 的旧布局目录应删除");
        assert!(other.is_dir(), "不含 bun 的无关目录不应删");
        assert!(neg.exists(), "负缓存文件不应被清理逻辑触碰");
    }
}
