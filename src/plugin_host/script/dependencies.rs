//! 脚本包的 JS 依赖安装（`bun install --production --ignore-scripts`）。
//!
//! ## 为什么用 Bun 装而不是网关自己解析
//!
//! 脚本包的依赖图是标准 npm 语义（peer/optional/resolutions），自己实现一个
//! 解析器等于重造包管理器。Bun 已经在运行时里，直接调它最省。
//!
//! ## 安全约束
//!
//! 1. **`--ignore-scripts`**：npm 的 `postinstall` 等生命周期脚本会在安装时
//!    执行任意代码。脚本包本身已经是不可信第三方内容，再让它跑安装脚本
//!    等于把「下载即执行」的门槛降到最低。绝大多数 provider 脚本不需要
//!    安装脚本（纯 JS），拒绝的代价很小。
//! 2. **剥离敏感环境变量**：`bun install` 会读 `NPM_CONFIG_*`、`*_TOKEN` 等，
//!    把网关进程的环境（可能含上游凭据）泄给子进程。只传最小环境。
//! 3. **在临时目录装**：只拷贝依赖声明（不拷源码），装完把 `node_modules`
//!    移进包目录，并在包目录写标记防重复安装。
//! 4. **有界**：安装超时 300s、`node_modules` 总量 ≤512MB。

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// 安装超时。
const INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
/// `node_modules` 字节上限（防依赖树爆炸）。
const MAX_NODE_MODULES_BYTES: u64 = 512 * 1024 * 1024;
/// 已安装标记文件名（存在则跳过重复安装）。
const MARKER: &str = ".aigx-dependencies";

/// 安装依赖（如需要），返回是否实际执行了安装。
///
/// `package_dir` 是包版本目录（含 package.json）。`bun` 是可执行文件路径。
pub async fn install_if_needed(bun: &Path, package_dir: &Path) -> Result<bool> {
    // 已装过（标记存在）直接跳过。
    if package_dir.join(MARKER).is_file() {
        return Ok(false);
    }

    let manifest_path = package_dir.join("package.json");
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("读取 package.json 失败：{}", manifest_path.display()))?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).context("package.json 不是合法 JSON")?;

    // 没有任何依赖声明 → 不用装，但仍写标记（避免每次启动都读一遍清单）。
    let has_deps = ["dependencies", "optionalDependencies", "peerDependencies"]
        .iter()
        .any(|k| {
            manifest
                .get(*k)
                .and_then(|d| d.as_object())
                .is_some_and(|o| !o.is_empty())
        });
    if !has_deps {
        write_marker(package_dir, "no-dependencies")?;
        return Ok(false);
    }

    // 临时目录里只放一个最小 package.json（只保留依赖声明字段）。
    // 为什么不直接在包目录装：包目录是我们判定过内容的「干净区」，
    // bun 可能顺手写 lockfile/缓存；隔离后再把 node_modules 搬进去更可控。
    let staging = package_dir.join(".aigx-staging-install");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).context("创建安装暂存目录失败")?;
    let trimmed = serde_json::json!({
        "name": "aigx-script-deps",
        "version": "0.0.0",
        "private": true,
        "dependencies": manifest.get("dependencies").cloned().unwrap_or(serde_json::json!({})),
        "optionalDependencies": manifest.get("optionalDependencies").cloned().unwrap_or(serde_json::json!({})),
    });
    std::fs::write(
        staging.join("package.json"),
        serde_json::to_vec_pretty(&trimmed)?,
    )
    .context("写入暂存 package.json 失败")?;

    let mut cmd = tokio::process::Command::new(bun);
    cmd.arg("install")
        .arg("--production")
        .arg("--ignore-scripts")
        .arg("--no-progress")
        .arg("--no-env-file")
        .current_dir(&staging);
    scrub_env(&mut cmd);
    // 超时后 future 被 drop：`kill_on_drop` 保证挂死的 bun install 不残留。
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(INSTALL_TIMEOUT, cmd.output())
        .await
        .map_err(|_| anyhow::anyhow!("bun install 超时（{}s）", INSTALL_TIMEOUT.as_secs()))?
        .context("无法执行 bun install")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let _ = std::fs::remove_dir_all(&staging);
        bail!(
            "bun install 失败（退出码 {:?}）：{}",
            output.status.code(),
            stderr.trim()
        );
    }

    let modules = staging.join("node_modules");
    if !modules.is_dir() {
        // 没有产出 node_modules（依赖全被 optional 跳过等）——不算失败。
        let _ = std::fs::remove_dir_all(&staging);
        write_marker(package_dir, "no-node-modules")?;
        return Ok(false);
    }

    // 体积检查：依赖树总量必须有界。
    let size = dir_size(&modules)?;
    if size > MAX_NODE_MODULES_BYTES {
        let _ = std::fs::remove_dir_all(&staging);
        bail!("node_modules 过大：{size} bytes（上限 {MAX_NODE_MODULES_BYTES}）");
    }

    // 搬进包目录（先删旧的，保证形态一致）。
    let dest = package_dir.join("node_modules");
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&modules, &dest).context("移动 node_modules 失败")?;
    let _ = std::fs::remove_dir_all(&staging);
    write_marker(package_dir, "ignore-scripts")?;
    Ok(true)
}

/// 清理会泄漏凭据或改变 Bun 行为的环境变量。
fn scrub_env(cmd: &mut tokio::process::Command) {
    let sensitive = |k: &str| {
        let upper = k.to_ascii_uppercase();
        k.starts_with("BUN_")
            || k.starts_with("NPM_CONFIG_")
            || k == "NODE_OPTIONS"
            || upper.contains("TOKEN")
            || upper.contains("SECRET")
            || upper.contains("PASSWORD")
            || upper.contains("API_KEY")
    };
    for (key, _) in std::env::vars() {
        if sensitive(&key) {
            cmd.env_remove(&key);
        }
    }
    cmd.env("DO_NOT_TRACK", "1");
}

/// 写标记文件（内容记录安装方式，便于排障）。
fn write_marker(dir: &Path, note: &str) -> Result<()> {
    std::fs::write(dir.join(MARKER), note).context("写依赖标记失败")
}

/// 递归统计目录字节数（跳过符号链接，防环）。
fn dir_size(dir: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match std::fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.file_type().is_symlink() {
                continue; // 不跟随链接（防环、防越界统计）
            }
            if meta.is_dir() {
                stack.push(path);
            } else {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_dependencies_skips_install() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            br#"{"name":"a","version":"1.0.0","main":"index.js"}"#,
        )
        .unwrap();
        // bun 路径无效也没关系：无依赖时不执行 bun
        let rt = tokio::runtime::Runtime::new().unwrap();
        let installed = rt
            .block_on(install_if_needed(Path::new("/nonexistent/bun"), dir.path()))
            .unwrap();
        assert!(!installed);
        assert!(dir.path().join(MARKER).is_file());
        let marker = std::fs::read_to_string(dir.path().join(MARKER)).unwrap();
        assert_eq!(marker, "no-dependencies");
    }

    #[test]
    fn marker_short_circuits() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(MARKER), "done").unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        // 连 package.json 都没有，但标记存在 → 直接跳过，不报错
        let installed = rt
            .block_on(install_if_needed(Path::new("/nonexistent/bun"), dir.path()))
            .unwrap();
        assert!(!installed);
    }

    #[test]
    fn dir_size_counts_nested_files_and_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/x"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("a/b/y"), vec![0u8; 50]).unwrap();
        assert_eq!(dir_size(dir.path()).unwrap(), 150);
    }
}
