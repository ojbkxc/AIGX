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

/// 当前平台 → Bun 资产。无匹配平台时返回明确错误（不猜）。
///
/// 为什么不用 `std::env::consts::ARCH` 一刀切：Linux 的 musl 与 glibc 是
/// **同一 arch 的不同 libc**，必须运行时探测（见 [`is_musl`]）。
pub fn asset_for_current_platform() -> Result<Asset> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let musl = if os == "linux" { is_musl() } else { false };

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
        _ => bail!("当前平台没有可用的 Bun {VERSION} 资产（os={os} arch={arch} musl={musl}）"),
    };
    Ok(asset)
}

/// 运行时安装目录：`{data_dir}/script-plugins/bun/{VERSION}/{platform-key}/`。
pub fn install_dir(data_dir: &Path) -> Result<PathBuf> {
    let asset = asset_for_current_platform()?;
    let key = platform_key();
    Ok(script_plugins_root(data_dir)
        .join("bun")
        .join(VERSION)
        .join(key)
        .join(asset.file.trim_end_matches(".zip")))
}

/// 可执行文件路径。
pub fn binary_path(data_dir: &Path) -> Result<PathBuf> {
    let suffix = if cfg!(windows) { "bun.exe" } else { "bun" };
    Ok(install_dir(data_dir)?.join(suffix))
}

/// 平台键（用于目录分层；只影响布局，不参与安全判断）。
fn platform_key() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if os == "linux" && is_musl() {
        format!("{os}-{arch}-musl")
    } else {
        format!("{os}-{arch}")
    }
}

/// 运行期探测是否为 musl libc。
///
/// 判据：`/lib/ld-musl-*.so.1` 存在，或 `ldd --version` 输出里出现 musl。
/// 探测失败一律按 glibc 处理（多数发行版如此），错误的代价是下载到
/// 一个跑不起来的二进制——`probe()` 会当场发现并报可读错误。
fn is_musl() -> bool {
    if let Ok(entries) = std::fs::read_dir("/lib") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with("ld-musl-") {
                return true;
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

/// 确保运行时可用，返回可执行文件路径。
///
/// 已存在则先 probe（可能被降级/替换/架构不符 —— 不信任路径上「有文件」
/// 这件事本身）；不存在则下载 → 校验 → 解压 → 原子落盘 → probe。
pub async fn ensure(data_dir: &Path) -> Result<PathBuf> {
    let asset = asset_for_current_platform()?;
    let path = binary_path(data_dir)?;

    if path.is_file() {
        match probe(&path).await {
            Ok(()) => return Ok(path),
            Err(err) => {
                // 已有二进制跑不起来：删掉重来（可能是半截下载或平台换了）。
                tracing::warn!("[Script] 已有 Bun 无法通过探针，重新安装：{err:#}");
                let _ = std::fs::remove_dir_all(install_dir(data_dir)?);
            }
        }
    }

    download_and_install(data_dir, &asset).await?;
    probe(&path).await?;
    Ok(path)
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

    let target = install_dir(data_dir)?;
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
}
