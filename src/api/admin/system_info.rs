//! `GET /api/system-info` —— 设置页「系统信息」的一站式读数。
//!
//! 对标 Agent2API v3.5.0-beta.3 `api/system_info.rs` + `core/system_info.rs`，
//! 按 AIGX 的实际形态裁剪：AIGX 是服务端网关（无桌面壳 / Windows 服务形态），
//! 数据库是统一 FileStore（SQLite 或 PostgreSQL），因此只报告真实存在的字段。
//!
//! ── 为什么合成一条端点，而不是让界面拼七八个现有接口 ───────────
//! 这个页面的用途是「出问题时截一张图 / 复制一段文本」：它必须**一次取全**，
//! 而且要能在别的接口都出错时仍然给出一份完整的环境快照。散着拼的话，
//! 界面得并发打 `/api/session`、`/api/storage`、`/api/update/status` 等
//! 多条，任何一条失败就得在页面上留个「—」——而那个「—」恰恰可能是排障
//! 最需要的一项。这里在服务端一次算完，失败的分项各自降级成 null，界面只
//! 负责显示。
//!
//! ── 挂管理员鉴权 ────────────────────────────────────────────
//! 响应里有数据目录、可执行文件路径、Key 数量、代理信任开关 —— 都是管理面
//! 信息，与 `/api/storage` 同级。免鉴权会把部署结构暴露给任何能访问端口的
//! 人。
//!
//! ── 只报「有没有」，不报值 ──────────────────────────────────
//! Key 只给条数；代理/出网只给「是否指定」。这个页面会被截图发到公开
//! issue 里，任何一处回显都会变成一次凭证泄露。
//!
//! ── 路径脱敏由后端做 ────────────────────────────────────────
//! `hideUsername` 走查询参数（`?hideUsername=1`），命中的路径在主目录前缀处
//! 换成 `~`。放后端是为了让界面「复制全部信息」拿到的也是脱敏后的文本 ——
//! 前端脱敏容易在剪贴板那条路上漏掉。开关本身是**纯展示偏好**，不落库：
//! 它只影响这一次请求的返回，刷新页面回到默认（显示完整路径）。
//!
//! ── 进程启动时刻 ────────────────────────────────────────────
//! [`record_start`] 由 `main` 在启动早期调用一次。不用懒初始化：那样
//! 「运行时长」会从**第一次打开设置页**开始算，而不是从进程启动开始算。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::Json,
};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::verify_admin;

/// 进程启动时刻（毫秒 Unix 时间戳），由 `main` 调用 [`record_start`] 记录。
static STARTED_AT: OnceLock<i64> = OnceLock::new();

/// 记录进程启动时刻（幂等；只在 main 启动早期调一次）。
///
/// 未接线时（main.rs 尚未加入调用）保留为可调用入口，避免 clippy 把「预留
/// 接线点」报成 dead_code；接线后该 allow 可删。
#[allow(dead_code)]
pub fn record_start() {
    let _ = STARTED_AT.set(now_ms());
}

/// 进程启动时刻（毫秒）；尚未记录时返回 0。
pub fn started_at_ms() -> i64 {
    STARTED_AT.get().copied().unwrap_or(0)
}

/// 已运行秒数；启动时刻未知时返回 0（界面显示「—」而不是一个假数字）。
pub fn uptime_seconds() -> i64 {
    let started = started_at_ms();
    if started <= 0 {
        return 0;
    }
    ((now_ms() - started) / 1000).max(0)
}

/// 运行形态：`docker` / `systemd` / `foreground`。
///
/// 判定顺序：容器 → systemd → 前台。AIGX 是服务端网关（无桌面壳），
/// Windows 上通常以前台/服务包装器运行，没有进程内可观察的差异。
pub fn deployment() -> &'static str {
    if in_container() {
        return "docker";
    }
    if std::env::var("INVOCATION_ID")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        return "systemd";
    }
    "foreground"
}

/// 是否跑在容器里：`AIGX_DEPLOY=docker` 显式声明、`/.dockerenv` 存在，
/// 或 `/proc/1/cgroup` 命中 docker / kubepods / containerd / lxc。
pub fn in_container() -> bool {
    if std::env::var("AIGX_DEPLOY")
        .map(|value| value.trim().eq_ignore_ascii_case("docker"))
        .unwrap_or(false)
    {
        return true;
    }
    if Path::new("/.dockerenv").exists() {
        return true;
    }
    if let Ok(text) = std::fs::read_to_string("/proc/1/cgroup") {
        let lower = text.to_ascii_lowercase();
        if lower.contains("docker")
            || lower.contains("kubepods")
            || lower.contains("containerd")
            || lower.contains("lxc")
        {
            return true;
        }
    }
    false
}

/// 编译目标系统（`std::env::consts::OS` 原值：windows / macos / linux）。
pub fn os() -> &'static str {
    std::env::consts::OS
}

/// 架构的展示写法：`x86_64` → `x64`、`aarch64` → `arm64`，其余原样。
pub fn arch_label() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        "x86" => "x86".to_string(),
        other => other.to_string(),
    }
}

/// 系统版本的可读描述（如 `Ubuntu 24.04 LTS`）。
///
/// Linux 读 `/etc/os-release`、macOS 读 `SystemVersion.plist`。Windows 版本
/// 需要注册表/`windows` crate（当前依赖树没有），返回空串由界面显示「—」，
/// 不 spawn 外部命令、不编造。
pub fn os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        linux_version()
    }
    #[cfg(target_os = "macos")]
    {
        macos_version()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        String::new()
    }
}

/// 用户主目录（脱敏时用来识别前缀）；拿不到返回空 PathBuf。
pub fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|value| PathBuf::from(value.trim()))
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_default()
}

/// 把主目录前缀替换成 `~`（`C:\Users\Bob\.aigx` → `~\.aigx`）。
pub fn mask_path(path: &Path) -> String {
    let text = path.to_string_lossy().to_string();
    let home = home_dir();
    if home.as_os_str().is_empty() {
        return text;
    }
    mask_text(&text, &home.to_string_lossy())
}

/// 字符串形态的脱敏（供 `to_string_lossy` 之后的路径复用）。
pub fn mask_text(text: &str, home: &str) -> String {
    // `\\?\C:\...` 这种扩展长度前缀会让 starts_with 失配，先剥掉再比
    let (prefix, body) = match text.strip_prefix(r"\\?\") {
        Some(rest) => (r"\\?\", rest),
        None => ("", text),
    };
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return text.to_string();
    }
    let body_matches = if cfg!(windows) {
        // 用 get() 而非切片：home 长度可能落在 body 的多字节字符中间（非 ASCII
        // 用户名），直接切片会 panic；非边界一律视为不匹配。
        body.get(..home.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(home))
    } else {
        body.starts_with(home)
    };
    if !body_matches {
        return text.to_string();
    }
    let Some(rest) = body.get(home.len()..) else {
        return text.to_string();
    };
    // 必须落在分隔符边界上：`C:\Users\Bob2` 不能被 `C:\Users\Bob` 命中
    if !rest.is_empty() && !rest.starts_with(['/', '\\']) {
        return text.to_string();
    }
    // 保留原路径用的分隔符（Windows 的 `~\.aigx` 与 POSIX 的 `~/.aigx` 各按
    // 原样返回）；没有余下路径时不需要分隔符。
    let separator = rest.chars().next().unwrap_or(std::path::MAIN_SEPARATOR);
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        format!("{prefix}~")
    } else {
        format!("{prefix}~{separator}{rest}")
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(target_os = "linux")]
fn linux_version() -> String {
    let Ok(text) = std::fs::read_to_string("/etc/os-release") else {
        return String::new();
    };
    // 只取 PRETTY_NAME，没有就退 NAME + VERSION_ID
    let field = |key: &str| -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}=")))
            .map(|value| value.trim().trim_matches('"').to_string())
            .filter(|value| !value.is_empty())
    };
    if let Some(pretty) = field("PRETTY_NAME") {
        return pretty;
    }
    match (field("NAME"), field("VERSION_ID")) {
        (Some(name), Some(version)) => format!("{name} {version}"),
        (Some(name), None) => name,
        _ => String::new(),
    }
}

#[cfg(target_os = "macos")]
fn macos_version() -> String {
    // `/System/Library/CoreServices/SystemVersion.plist` 是纯文本 XML，取
    // ProductVersion 的 <string> 即可；不 spawn `sw_vers`。
    let Ok(text) = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
    else {
        return String::new();
    };
    let Some(index) = text.find("<key>ProductVersion</key>") else {
        return String::new();
    };
    let rest = &text[index..];
    let Some(start) = rest.find("<string>") else {
        return String::new();
    };
    let rest = &rest[start + "<string>".len()..];
    let Some(end) = rest.find("</string>") else {
        return String::new();
    };
    let version = rest[..end].trim();
    if version.is_empty() {
        String::new()
    } else {
        format!("macOS {version}")
    }
}

/// 查询参数：`hideUsername=1` / `true` / `yes` / `on` 时把主目录前缀替换成 `~`。
#[derive(Debug, Default, serde::Deserialize)]
pub struct SystemInfoParams {
    #[serde(rename = "hideUsername", default)]
    pub hide_username: Option<String>,
}

/// GET /api/system-info —— 设置页「系统信息」的数据源（管理员）。
pub async fn handle_get_system_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<SystemInfoParams>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let hide = matches!(
        params.hide_username.as_deref().map(str::trim),
        Some("1") | Some("true") | Some("yes") | Some("on")
    );
    let show = |path: &Path| -> String {
        if hide {
            mask_path(path)
        } else {
            path.to_string_lossy().to_string()
        }
    };
    let show_opt = |path: Option<&Path>| -> Value {
        match path {
            Some(path) => Value::String(show(path)),
            None => Value::Null,
        }
    };

    let data_dir = crate::config::expand_path(&config.server.data_dir);
    let log_dir = match config.server.log_dir.as_deref() {
        Some(raw) if !raw.trim().is_empty() => crate::config::expand_path(raw),
        _ => data_dir.join("logs"),
    };
    let config_file = state.config_manager.path().to_path_buf();
    let exe = std::env::current_exe().ok();

    // 只有启用 postgres feature 且配置了 database.url 才真的走 PG；
    // 否则 main.rs 会回落到 SQLite（不能只看 URL 就说后端是 PostgreSQL）。
    let postgres = cfg!(feature = "postgres") && config.database.is_enabled();
    // SQLite 库文件由 FileStore 固定放在 `{data_dir}/data/aigx.db`（见 main.rs
    // `FileStore::open(data_dir.join("data"))`）；PG 后端没有本地库文件。
    let db_file = (!postgres).then(|| data_dir.join("data").join("aigx.db"));
    let db_bytes = db_file
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map(|meta| meta.len());
    let db_available = db_file.as_deref().map(Path::is_file);

    let accounts = state.account_pool.list();
    let active_accounts = state.account_pool.active_accounts().len();
    let channels = state.channel_store.list();
    let enabled_channels = channels
        .iter()
        .filter(|channel| channel.is_enabled())
        .count();
    let api_keys = state.api_key_store.enabled_count();
    let admin_registered = state.user_store.list().iter().any(|user| user.is_admin());

    Ok(Json(json!({
        "app": {
            "version": env!("CARGO_PKG_VERSION"),
            "license": "AIGX Source Available License",
            "sourceUrl": "https://github.com/ojbkxc/AIGX",
            "licenseUrl": "https://github.com/ojbkxc/AIGX/blob/main/LICENSE",
            "copyright": "Copyright (c) 2026 AIGX Authors",
            "includePrerelease": config.update.include_prerelease,
        },
        "runtime": {
            "deployment": deployment(),
            "container": in_container(),
            "os": os(),
            "osVersion": os_version(),
            "arch": arch_label(),
            "startedAt": started_at_ms(),
            "uptimeSeconds": uptime_seconds(),
            "pid": std::process::id(),
            "debugBuild": cfg!(debug_assertions),
        },
        "deployment": {
            "host": config.server.host.clone(),
            "port": config.server.port,
            "adminBind": config.server.admin_bind.clone(),
            "configFile": show(&config_file),
            "dataDir": show(&data_dir),
            "logDir": show(&log_dir),
            "dbBackend": if postgres { "postgres" } else { "sqlite" },
            "dbFile": show_opt(db_file.as_deref()),
            "dbBytes": db_bytes,
            "dbAvailable": db_available,
            "exePath": exe.as_deref().map(show).unwrap_or_default(),
            "uiHosted": crate::web::find_static_dir().is_some(),
            "selfUpdate": crate::update::supported(),
        },
        "security": {
            "adminRegistered": admin_registered,
            "apiKeyCount": api_keys,
            "dataPlaneKeyRequired": true,
            "trustProxyHeaders": config.trust_proxy_headers,
            "allowInternalUpstream": config.usage.allow_internal_upstream,
        },
        "features": {
            "debugMode": config.usage.debug_traffic,
            "logBody": config.usage.log_body,
            "sanitizeFingerprints": config.usage.sanitize_fingerprints,
            "cors": !config.cors_origins.is_empty() || !config.server_address.trim().is_empty(),
            "autoContinue": config.usage.auto_continue,
            "autoContinueMaxRounds": config.usage.auto_continue_max_rounds,
            "captchaEnabled": config.security.captcha_enabled,
            "updateBeta": config.update.include_prerelease,
        },
        "stats": {
            "accounts": accounts.len(),
            "activeAccounts": active_accounts,
            "channels": channels.len(),
            "enabledChannels": enabled_channels,
            "modelAliases": state.model_mapper.active_mappings().len(),
            "apiKeys": api_keys,
            "requests": state.log_store.requests.count_all(),
            "inFlight": state.inflight.len(),
        },
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_text_replaces_home_prefix_only_on_boundary() {
        let home = "/home/bob";
        assert_eq!(mask_text("/home/bob/.aigx", home), "~/.aigx");
        assert_eq!(mask_text("/home/bob", home), "~");
        // 正向对照：不在边界上的同前缀必须原样保留
        assert_eq!(mask_text("/home/bob2/.aigx", home), "/home/bob2/.aigx");
        assert_eq!(mask_text("/var/log/aigx", home), "/var/log/aigx");
    }

    #[test]
    fn mask_text_handles_windows_extended_prefix() {
        // 该用例不依赖宿主平台：非 Windows 下大小写敏感，但此处前后完全一致
        let result = mask_text(r"\\?\C:\Users\Bob\.aigx", r"C:\Users\Bob");
        assert!(result.starts_with(r"\\?\~"));
    }

    #[test]
    fn arch_label_maps_x64_and_arm64() {
        match std::env::consts::ARCH {
            "x86_64" => assert_eq!(arch_label(), "x64"),
            "aarch64" => assert_eq!(arch_label(), "arm64"),
            other => assert_eq!(arch_label(), other),
        }
    }
}
