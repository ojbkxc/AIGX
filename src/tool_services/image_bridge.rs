// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成 MCP 本地桥接脚本（stdio，Node >= 18）。
//!
//! 与视觉桥接同一边界：网关只**下发脚本**（`/api/tools/mcp-bridge/image-generation.cjs`），
//! 由客户端以 `node image-generation-bridge.cjs` 的 stdio MCP 形态挂载。
//! 脚本负责三件 HTTP 模式做不到的事：
//!   1. `edit_images` 的本地绝对路径参考图（读文件转 data URI，限制在管理员
//!      授权的输出 / 参考目录内，拒绝符号链接）；
//!   2. 生成完成后把图片下载到授权输出目录并返回**真实本地路径**
//!      （同名不同内容拒绝覆盖）；
//!   3. 提交/查询响应丢失时保留 `uncertain` 语义，提示用原 request_id 查询。
//!
//! 配置经环境变量注入（由面板写入客户端 MCP 配置时生成，命名沿用上游
//! `AGENT2API_IMAGE_*` 以免已接入客户端失效）：
//!   AGENT2API_IMAGE_ENDPOINT        远程 MCP 端点完整 URL（含 /mcp/image-generation）
//!   AGENT2API_IMAGE_KEY             网关 API Key（Bearer）
//!   AGENT2API_IMAGE_OUTPUT_DIR      授权输出目录（绝对路径）
//!   AGENT2API_IMAGE_REFERENCE_DIRS  额外参考目录（JSON 数组，最多 16 个）
//!
//! ── 安装链路（本波补齐）──────────────────────────────────────
//! 上游 `image_bridge.rs` 的 Node 探测 / 脚本落盘 / 客户端配置注入由安装流程
//! 复用；AIGX 的 [`prepare`] / [`entry`] 与 `install.rs` 的 `install_mcp`
//! 生图 + bridge 分支对接（`Agent 配置` 页安装 MCP 时选 bridge 模式）。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::config_files::Target;

/// 桥接脚本正文（`include_str!`，与上游 `image_bridge.cjs` 逐字一致）。
pub const IMAGE_BRIDGE_SCRIPT: &str = include_str!("image_bridge.cjs");

/// 安装生图桥接时管理员填写的目录选项。
#[derive(Default)]
pub struct ImageBridgeOptions {
    pub output_directory: String,
    pub reference_directories: Vec<String>,
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 展开 `~` / `~/` 前缀（与客户端配置路径同口径）。
fn expand(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        return home_dir().join(rest);
    }
    if trimmed == "~" {
        return home_dir();
    }
    PathBuf::from(trimmed)
}

fn reject_links(path: &Path) -> Result<(), String> {
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return Err(format!("{} 是符号链接，拒绝使用", path.display()));
    }
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))?;
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::write(&temp, bytes)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("落盘 {} 失败：{error}", path.display())
    })
}

fn directory(raw: &str) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("请选择图片目录".into());
    }
    let path = expand(raw);
    if !path.is_absolute() || !path.is_dir() {
        return Err("请选择已存在的绝对目录".into());
    }
    reject_links(&path)?;
    path.canonicalize()
        .map_err(|error| format!("目录不可用：{error}"))
}

fn node_binary() -> Result<PathBuf, String> {
    let executable = if cfg!(windows) { "node.exe" } else { "node" };
    let mut candidates: Vec<PathBuf> =
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(executable))
            .collect();
    if cfg!(windows) {
        for key in ["ProgramFiles", "LOCALAPPDATA"] {
            if let Some(dir) = std::env::var_os(key) {
                candidates.push(PathBuf::from(dir).join("nodejs").join(executable));
            }
        }
    } else {
        candidates.extend([
            PathBuf::from("/usr/local/bin/node"),
            PathBuf::from("/opt/homebrew/bin/node"),
            PathBuf::from("/usr/bin/node"),
        ]);
    }
    for path in candidates.into_iter().filter(|path| path.is_file()) {
        let mut command = Command::new(&path);
        command
            .args([
                "-e",
                "process.exit(Number(process.versions.node.split('.')[0]) >= 18 ? 0 : 1)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let Ok(mut child) = command.spawn() else {
            continue;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success() {
                        return Ok(path);
                    }
                    break;
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
    Err("未找到 Node.js 18 或更新版本；请安装后重试，或改用远程 HTTP 接入".into())
}

fn ensure_script() -> Result<PathBuf, String> {
    let dir = crate::account_session::secret::data_dir().join("tools");
    reject_links(&dir)?;
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建工具目录失败：{error}"))?;
    let path = dir.join("image-generation-bridge.cjs");
    reject_links(&path)?;
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes == IMAGE_BRIDGE_SCRIPT.as_bytes() {
            return Ok(path);
        }
        if !bytes.starts_with(b"// Agent2API image-generation bridge") {
            return Err("图片桥接路径已有其他文件，未覆盖".into());
        }
    }
    atomic_write(&path, IMAGE_BRIDGE_SCRIPT.as_bytes())?;
    Ok(path)
}

/// 已解析好的桥接落点（Node 可执行 + 脚本 + 授权目录）。
pub struct PreparedBridge {
    node: PathBuf,
    script: PathBuf,
    output: PathBuf,
    references: Vec<PathBuf>,
}

/// 校验目录选项并定位 Node / 脚本（安装时调用一次，随后复用到各客户端）。
pub fn prepare(options: &ImageBridgeOptions) -> Result<PreparedBridge, String> {
    let output = directory(&options.output_directory)?;
    if options.reference_directories.len() > 16 {
        return Err("参考图目录最多 16 个".into());
    }
    let references = options
        .reference_directories
        .iter()
        .map(|value| directory(value))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PreparedBridge {
        node: node_binary()?,
        script: ensure_script()?,
        output,
        references,
    })
}

/// 生成某个客户端的 stdio MCP 条目（`command` / `args` / `env`）。
pub fn entry(_def: Target, url: &str, key: &str, bridge: &PreparedBridge) -> Value {
    let node = bridge.node.display().to_string();
    let script = bridge.script.display().to_string();
    let references: Vec<String> = bridge
        .references
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    let env = json!({
        "AGENT2API_IMAGE_ENDPOINT": url,
        "AGENT2API_IMAGE_KEY": key,
        "AGENT2API_IMAGE_OUTPUT_DIR": bridge.output.display().to_string(),
        "AGENT2API_IMAGE_REFERENCE_DIRS": serde_json::to_string(&references).unwrap_or_else(|_| "[]".into()),
    });
    json!({ "command": node, "args": [script], "env": env })
}
