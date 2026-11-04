//! Clash Verge 配置读取（对照 Agent2API `core::clash`）。
//!
//! 账号代理的解析只依赖「快照形状」，而快照自身的读取涉及三个候选目录、
//! 一个 YAML 文件，属于「外部环境探测」，与归一/解析这类纯逻辑分开。
//!
//! Clash Verge 侧的代理项是**严格镜像** —— 端口只在 Clash Verge 里改，
//! 这里每次都从 verge.yaml 实时读取，账号只记 listener_uid。Clash 里删掉了
//! 某个监听器，引用它的条目会解析失败并回退直连。

use std::path::{Path, PathBuf};

use serde_yaml::Value as Yaml;

/// Clash Verge 全局混合端口在池条目里的保留 uid（与真实节点名不会冲突）
pub const CLASH_MIXED_UID: &str = "__mixed__";

/// Clash 配置未找到时的原因文案
pub const CLASH_UNAVAILABLE: &str = "未找到 Clash Verge 配置";

/// Clash Verge 的应用目录名（Windows 下 `%APPDATA%\<这个名字>`）
const CLASH_APP_ID: &str = "io.github.clash-verge-rev.clash-verge-rev";

/// 快照缓存 TTL：Clash 改端口最多延迟 3 秒生效
const CLASH_CACHE_TTL_MS: i64 = 3000;

/// Clash Verge 监听器快照。
#[derive(Clone, Debug)]
pub struct ClashSnapshot {
    pub available: bool,
    pub dir: Option<String>,
    pub mixed_port: Option<u16>,
    pub listeners: Vec<ClashListener>,
    pub error: Option<String>,
}

impl ClashSnapshot {
    fn unavailable(error: &str, dir: Option<PathBuf>) -> Self {
        Self {
            available: false,
            dir: dir.map(|path| path.to_string_lossy().to_string()),
            mixed_port: None,
            listeners: Vec::new(),
            error: Some(error.to_string()),
        }
    }
}

/// Clash Verge 的一个混合监听器
#[derive(Clone, Debug)]
pub struct ClashListener {
    pub uid: String,
    pub name: String,
    pub port: u16,
    pub enabled: bool,
}

// ─── 配置目录与 YAML 读取 ───────────────────────────────────

/// 用户主目录。
fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Clash Verge 配置目录：候选顺序照抄基线 `clashConfigDir()`。
///
///   ① `%APPDATA%\<appId>`（Windows 的常规位置）
///   ② `~/AppData/Roaming/<appId>`（APPDATA 缺失时的回落）
///   ③ `~/.config/<appId>`（macOS / Linux）
pub fn clash_config_dir() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(appdata) = std::env::var("APPDATA") {
        let trimmed = appdata.trim();
        if !trimmed.is_empty() {
            candidates.push(Path::new(trimmed).join(CLASH_APP_ID));
        }
    }
    if let Some(home) = home_dir() {
        candidates.push(home.join("AppData").join("Roaming").join(CLASH_APP_ID));
        candidates.push(home.join(".config").join(CLASH_APP_ID));
    }
    candidates
        .into_iter()
        .find(|dir| dir.join("verge.yaml").exists())
}

/// 读一个 YAML 文件；缺失/解析失败/顶层不是映射都返回 None。
fn read_yaml(path: &Path) -> Option<Yaml> {
    if !path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Yaml = serde_yaml::from_str(&text).ok()?;
    match parsed {
        Yaml::Mapping(_) => Some(parsed),
        _ => None,
    }
}

/// YAML 里的端口值 → 端口号。
fn yaml_port(value: Option<&Yaml>) -> Option<u16> {
    let number = match value? {
        Yaml::Number(number) => number.as_f64()?,
        Yaml::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !number.is_finite() || number.fract() != 0.0 || !(1.0..=65535.0).contains(&number) {
        return None;
    }
    Some(number as u16)
}

/// 取 YAML 字符串并去空白（非字符串一律当空串）
fn yaml_text(value: Option<&Yaml>) -> String {
    match value {
        Some(Yaml::String(text)) => text.trim().to_string(),
        _ => String::new(),
    }
}

/// 读取 Clash Verge 的监听器快照。
///
/// Clash 未安装或配置不可读时 `available=false`，不抛异常 —— 这是正常状态。
pub fn read_clash_verge_config() -> ClashSnapshot {
    let Some(dir) = clash_config_dir() else {
        return ClashSnapshot::unavailable(CLASH_UNAVAILABLE, None);
    };
    let Some(verge) = read_yaml(&dir.join("verge.yaml")) else {
        return ClashSnapshot::unavailable("verge.yaml 解析失败", Some(dir));
    };

    // 手动配置的监听器：脏项（缺 proxy / 端口非法 / 非映射）一律跳过
    let mut listeners: Vec<ClashListener> = Vec::new();
    if let Some(items) = verge.get("verge_mixed_listeners").and_then(Yaml::as_sequence) {
        for item in items {
            let Some(mapping) = item.as_mapping() else {
                continue;
            };
            let proxy = yaml_text(mapping.get("proxy"));
            if proxy.is_empty() {
                continue;
            }
            let Some(port) = yaml_port(mapping.get("port")) else {
                continue;
            };
            let uid = {
                let text = yaml_text(mapping.get("uid"));
                if text.is_empty() {
                    format!("{port}:{proxy}")
                } else {
                    text
                }
            };
            let name = {
                let text = yaml_text(mapping.get("name"));
                if text.is_empty() {
                    proxy.clone()
                } else {
                    text
                }
            };
            listeners.push(ClashListener {
                uid,
                name,
                port,
                // 只有显式 false 才算禁用
                enabled: !matches!(mapping.get("enabled"), Some(Yaml::Bool(false))),
            });
        }
    }

    ClashSnapshot {
        available: true,
        dir: Some(dir.to_string_lossy().to_string()),
        mixed_port: yaml_port(verge.get("verge_mixed_port")),
        listeners,
        error: None,
    }
}

// ─── 快照缓存 ───────────────────────────────────────────────

/// 进程内快照缓存：`(取快照的时刻, 快照)`。
static CLASH_CACHE: std::sync::Mutex<Option<(i64, ClashSnapshot)>> =
    std::sync::Mutex::new(None);

/// 取快照（默认走 3 秒 TTL 缓存）
pub fn clash_snapshot() -> ClashSnapshot {
    clash_snapshot_with(false)
}

/// 取快照，`force=true` 时跳过缓存。
pub fn clash_snapshot_with(force: bool) -> ClashSnapshot {
    let now = chrono::Utc::now().timestamp_millis();
    let mut guard = match CLASH_CACHE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if !force {
        if let Some((at, snapshot)) = guard.as_ref() {
            if now - *at < CLASH_CACHE_TTL_MS {
                return snapshot.clone();
            }
        }
    }
    let snapshot = read_clash_verge_config();
    *guard = Some((now, snapshot.clone()));
    snapshot
}

/// Clash 代理可选项（GET /api/proxy-pool 响应里的 `clash` 字段）。
///
/// 不可用分支返回 `{available:false, error, dir, options:[]}`。
pub fn clash_proxy_options() -> serde_json::Value {
    use serde_json::json;

    let snapshot = clash_snapshot();
    if !snapshot.available {
        return json!({
            "available": false,
            "error": snapshot.error,
            "dir": snapshot.dir,
            "options": [],
        });
    }
    let mut options = Vec::new();
    if let Some(port) = snapshot.mixed_port {
        options.push(json!({
            "uid": CLASH_MIXED_UID,
            "name": "Clash 混合端口（按规则分流）",
            "port": port,
            "kind": "mixed",
            "enabled": true,
        }));
    }
    for listener in &snapshot.listeners {
        options.push(json!({
            "uid": listener.uid,
            "name": listener.name,
            "port": listener.port,
            "kind": "listener",
            "enabled": listener.enabled,
        }));
    }
    json!({
        "available": true,
        "error": serde_json::Value::Null,
        "dir": snapshot.dir,
        "options": options,
    })
}
