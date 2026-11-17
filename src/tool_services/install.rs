// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 工具服务的「本机接入」：把 Skill 包 / MCP 配置写进本机客户端。
//!
//! ── 落点复用现有客户端路径表 ────────────────────────────────
//! AIGX 的「Agent 配置」页已有四个客户端目标
//! （`client_config::targets::Target`：claude-code / codex / claude-desktop /
//! dsh）与 JSON / TOML / YAML 的无损渲染器（`client_config::edit`）。本模块
//! 不另维护一份客户端清单：
//!   · Skill：写到客户端配置目录下的 `skills/<package>/…`，包含 SKILL.md、
//!     skill.json、运行时脚本与 `.agent2api.json`（API Key）；目录里有
//!     `skill.json` 且 `name` 匹配才视为「我们装的」，卸载不会删别人的目录；
//!   · MCP：改写客户端配置文件里本服务那一项（JSON / TOML / YAML 由既有
//!     渲染器处理，注释与其余设置原样保留），HTTP 形态填 url + Authorization，
//!     视觉分析还可以用本地桥接（stdio）形态以支持本地图片路径。
//!
//! ── 与上游的两个差异 ────────────────────────────────────────
//! 1. 上游走 `agents::resources` 的预览+确认令牌；AIGX 现有 client_config 的
//!    预览令牌接口面向「整条配置记录」，不覆盖 MCP server 项。此处直接改写
//!    单个 `mcpServers.<name>` 键（原子写 + 渲染后回读校验由 edit::render 保证）；
//! 2. DSH 的凭据文件（`.credentials.yaml`）不参与：MCP 条目把 Authorization
//!    内联在 headers 里，Skill 用包内配置文件，两者都不需要 DSH 凭据引用。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::client_config::edit;
use crate::client_config::targets::Target;

#[derive(Clone, Debug)]
pub struct InstallOutcome {
    pub client_id: String,
    pub label: String,
    pub ok: bool,
    pub path: String,
    pub error: Option<String>,
}

impl InstallOutcome {
    fn ok(client_id: &str, label: &str, path: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            label: label.into(),
            ok: true,
            path: path.into(),
            error: None,
        }
    }
    fn failed(client_id: &str, label: &str, error: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            label: label.into(),
            ok: false,
            path: String::new(),
            error: Some(error.into()),
        }
    }
    pub fn to_json(&self) -> Value {
        json!({
            "clientId": self.client_id,
            "label": self.label,
            "ok": self.ok,
            "path": self.path,
            "error": self.error,
        })
    }
}

fn skill_root(def: Target) -> PathBuf {
    def.config_path()
        .parent()
        .map(|dir| dir.join("skills"))
        .unwrap_or_else(|| PathBuf::from("skills"))
}

fn skill_dir(root: &Path, service: &str) -> PathBuf {
    root.join(super::package_name(service))
}

fn marker_is_ours(target: &Path, service: &str) -> bool {
    let Ok(bytes) = std::fs::read(target.join("skill.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value.get("name").and_then(Value::as_str) == Some(super::package_name(service))
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建目录 {} 失败：{error}", parent.display()))?;
    }
    let temp = path.with_extension(format!(
        "tmp-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
    ));
    std::fs::write(&temp, content)
        .map_err(|error| format!("写入 {} 失败：{error}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        format!("落盘 {} 失败：{error}", path.display())
    })
}

/* ─── Skill 目标 ───────────────────────────────────────────── */

pub fn skill_targets(service: &str) -> Vec<Value> {
    let mut rows = Vec::new();
    for def in Target::ALL {
        let root = skill_root(def);
        let target = skill_dir(&root, service);
        let installed = target.exists() && marker_is_ours(&target, service);
        let config_exists = def.config_path().exists();
        rows.push(json!({
            "key": def.id(),
            "label": def.label(),
            "path": target.display().to_string(),
            "detected": config_exists,
            "installed": installed,
            "supported": true,
        }));
    }
    rows
}

pub fn install_skill(
    service: &str,
    variant: &str,
    base_url: &str,
    api_key: &str,
    client_ids: &[String],
) -> Vec<InstallOutcome> {
    let mut outcomes = Vec::new();
    if service != super::WEB_SEARCH_SERVICE_ID {
        return client_ids
            .iter()
            .map(|id| InstallOutcome::failed(id, id, "该服务没有 Skill 安装包"))
            .collect();
    }
    let files = super::build_skill_files(base_url, variant);
    let mut contents: Vec<(String, String)> = files
        .into_iter()
        .map(|file| (file.path.to_string(), file.content))
        .collect();
    contents.push((
        ".agent2api.json".into(),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&json!({ "api_key": api_key })).unwrap_or_default()
        ),
    ));

    for client_id in client_ids {
        let Some(def) = Target::parse(client_id) else {
            outcomes.push(InstallOutcome::failed(client_id, client_id, "未知客户端"));
            continue;
        };
        let target = skill_dir(&skill_root(def), service);
        match write_skill_pack(&target, &contents) {
            Ok(()) => outcomes.push(InstallOutcome::ok(
                client_id,
                def.label(),
                target.display().to_string(),
            )),
            Err(error) => outcomes.push(InstallOutcome::failed(client_id, def.label(), error)),
        }
    }
    outcomes
}

fn write_skill_pack(target: &Path, files: &[(String, String)]) -> Result<(), String> {
    if target.exists() && !target.is_dir() {
        return Err(format!("{} 已存在且不是目录", target.display()));
    }
    std::fs::create_dir_all(target).map_err(|error| format!("创建 Skill 目录失败：{error}"))?;
    for (relative, content) in files {
        let path = target.join(relative);
        atomic_write(&path, content)?;
    }
    Ok(())
}

pub fn uninstall_skill(service: &str, client_ids: &[String]) -> Vec<InstallOutcome> {
    let mut outcomes = Vec::new();
    for client_id in client_ids {
        let Some(def) = Target::parse(client_id) else {
            outcomes.push(InstallOutcome::failed(client_id, client_id, "未知客户端"));
            continue;
        };
        let target = skill_dir(&skill_root(def), service);
        if !target.exists() || !marker_is_ours(&target, service) {
            outcomes.push(InstallOutcome::failed(client_id, def.label(), "未安装"));
            continue;
        }
        if std::fs::symlink_metadata(&target).is_ok_and(|meta| meta.file_type().is_symlink()) {
            outcomes.push(InstallOutcome::failed(
                client_id,
                def.label(),
                "Skill 目录是链接，拒绝删除",
            ));
            continue;
        }
        match std::fs::remove_dir_all(&target) {
            Ok(()) => outcomes.push(InstallOutcome::ok(
                client_id,
                def.label(),
                target.display().to_string(),
            )),
            Err(error) => outcomes.push(InstallOutcome::failed(
                client_id,
                def.label(),
                format!("删除失败：{error}"),
            )),
        }
    }
    outcomes
}

/* ─── MCP 目标 ─────────────────────────────────────────────── */

fn server_name(service: &str) -> &'static str {
    super::package_name(service)
}

/// 各客户端配置文件里 MCP server 的根键。
fn mcp_root_key(def: Target) -> &'static str {
    match def {
        // Codex 的 MCP 配置是 `[mcp_servers.<name>]`
        Target::Codex => "mcp_servers",
        // 其余三家按通用 `mcpServers`（与 Claude Desktop 同形态）
        _ => "mcpServers",
    }
}

fn read_snapshot(def: Target) -> Result<(String, Value), String> {
    let path = def.config_path();
    let original = std::fs::read_to_string(&path).unwrap_or_default();
    let value = edit::parse(def.format(), &original)?;
    Ok((original, value))
}

fn installed_server(def: Target, name: &str) -> bool {
    read_snapshot(def)
        .ok()
        .and_then(|(_, value)| {
            value
                .get(mcp_root_key(def))
                .and_then(Value::as_object)
                .map(|servers| servers.contains_key(name))
        })
        .unwrap_or(false)
}

pub fn mcp_targets(service: &str) -> Vec<Value> {
    let name = server_name(service);
    let mut rows = Vec::new();
    for def in Target::ALL {
        let path = def.config_path();
        rows.push(json!({
            "key": def.id(),
            "label": def.label(),
            "path": path.display().to_string(),
            "detected": path.exists(),
            "installed": installed_server(def, name),
            "supported": true,
            "note": match def {
                Target::Codex => Some("写入 [mcp_servers.<name>]；Codex 需重启后生效。"),
                Target::ClaudeCode => Some("写入 settings.json 的 mcpServers；若客户端使用 .mcp.json 项目级配置，请手工迁移。"),
                _ => None,
            },
        }));
    }
    rows
}

/// 视觉本地桥接脚本的落点（写一次、安装时复用）。
fn ensure_bridge_script() -> Result<PathBuf, String> {
    let dir = crate::account_session::secret::data_dir().join("tools");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建工具目录失败：{error}"))?;
    let path = dir.join("vision-bridge.js");
    let current = std::fs::read(&path).ok();
    if current.as_deref() != Some(super::VISION_BRIDGE_SCRIPT.as_bytes()) {
        atomic_write(&path, super::VISION_BRIDGE_SCRIPT)?;
    }
    Ok(path)
}

fn timeout_entry(def: Target, timeout: u64) -> Value {
    if timeout == 0 {
        return Value::Null;
    }
    let millis = timeout.saturating_mul(1000);
    match def {
        Target::ClaudeCode => json!({ "timeout": millis }),
        Target::Codex => json!({ "tool_timeout_sec": timeout }),
        Target::Dsh => json!({ "toolCallTimeoutMs": millis }),
        Target::ClaudeDesktop => Value::Null,
    }
}

fn merge_entry(mut entry: Value, extra: Value) -> Value {
    if let (Some(object), Some(extra)) = (entry.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            object.insert(key.clone(), value.clone());
        }
    }
    entry
}

fn mcp_entry(def: Target, url: &str, api_key: &str, bridge: Option<&Path>, timeout: u64) -> Value {
    let authorization = format!("Bearer {api_key}");
    let extra = timeout_entry(def, timeout);
    let base = match bridge {
        Some(path) => {
            let bridge_path = path.display().to_string();
            let mut env = json!({
                "AGENT2API_VISION_ENDPOINT": url,
                "AGENT2API_VISION_KEY": api_key,
            });
            if timeout > 0 {
                env["AGENT2API_VISION_TIMEOUT_MS"] = json!(timeout.saturating_mul(1000) + 60_000);
            }
            json!({ "command": "node", "args": [bridge_path], "env": env })
        }
        None => match def {
            Target::Codex => json!({
                "url": url,
                "http_headers": { "Authorization": authorization },
            }),
            _ => json!({
                "type": "http",
                "url": url,
                "headers": { "Authorization": authorization },
            }),
        },
    };
    merge_entry(base, extra)
}

fn write_mcp_entry(def: Target, name: &str, config: Value) -> Result<String, String> {
    let (original, mut value) = read_snapshot(def)?;
    let root = mcp_root_key(def);
    let object = value
        .as_object_mut()
        .ok_or_else(|| "客户端配置根节点不是对象".to_string())?;
    let servers = object.entry(root.to_string()).or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .ok_or_else(|| "客户端配置里的 MCP 根节点不是对象".to_string())?;
    servers.insert(name.to_string(), config);
    let rendered = edit::render(def.format(), &original, &value)?;
    atomic_write(&def.config_path(), &rendered)?;
    Ok(def.config_path().display().to_string())
}

fn remove_mcp_entry(def: Target, name: &str) -> Result<bool, String> {
    let (original, mut value) = read_snapshot(def)?;
    let root = mcp_root_key(def);
    let Some(servers) = value.get_mut(root).and_then(Value::as_object_mut) else {
        return Ok(false);
    };
    if servers.remove(name).is_none() {
        return Ok(false);
    }
    let rendered = edit::render(def.format(), &original, &value)?;
    atomic_write(&def.config_path(), &rendered)?;
    Ok(true)
}

pub fn install_mcp(
    services: &super::ToolServices,
    service: &str,
    mode: &str,
    base_url: &str,
    api_key: &str,
    client_ids: &[String],
) -> Vec<InstallOutcome> {
    let name = server_name(service);
    let url = format!(
        "{}{}",
        base_url.trim_end_matches('/'),
        super::endpoint(service, "mcp")
    );
    let timeout = super::timeout_seconds(services, service);
    let bridge = if service == super::VISION_SERVICE_ID && mode == "bridge" {
        match ensure_bridge_script() {
            Ok(path) => Some(path),
            Err(error) => {
                return client_ids
                    .iter()
                    .map(|client_id| {
                        let label = Target::parse(client_id)
                            .map(|def| def.label())
                            .unwrap_or(client_id.as_str());
                        InstallOutcome::failed(client_id, label, error.clone())
                    })
                    .collect();
            }
        }
    } else {
        None
    };

    let mut outcomes = Vec::new();
    for client_id in client_ids {
        let Some(def) = Target::parse(client_id) else {
            outcomes.push(InstallOutcome::failed(client_id, client_id, "未知客户端"));
            continue;
        };
        let config = mcp_entry(def, &url, api_key, bridge.as_deref(), timeout);
        match write_mcp_entry(def, name, config) {
            Ok(path) => outcomes.push(InstallOutcome::ok(client_id, def.label(), path)),
            Err(error) => outcomes.push(InstallOutcome::failed(client_id, def.label(), error)),
        }
    }
    outcomes
}

pub fn uninstall_mcp(service: &str, client_ids: &[String]) -> Vec<InstallOutcome> {
    let name = server_name(service);
    let mut outcomes = Vec::new();
    for client_id in client_ids {
        let Some(def) = Target::parse(client_id) else {
            outcomes.push(InstallOutcome::failed(client_id, client_id, "未知客户端"));
            continue;
        };
        match remove_mcp_entry(def, name) {
            Ok(true) => outcomes.push(InstallOutcome::ok(
                client_id,
                def.label(),
                def.config_path().display().to_string(),
            )),
            Ok(false) => outcomes.push(InstallOutcome::failed(client_id, def.label(), "未配置")),
            Err(error) => outcomes.push(InstallOutcome::failed(client_id, def.label(), error)),
        }
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_are_stable() {
        assert_eq!(super::super::package_name("web-search"), "web-search");
        assert_eq!(super::super::package_name("vision"), "vision");
    }

    #[test]
    fn mcp_root_keys_by_client() {
        assert_eq!(mcp_root_key(Target::Codex), "mcp_servers");
        assert_eq!(mcp_root_key(Target::ClaudeDesktop), "mcpServers");
        assert_eq!(mcp_root_key(Target::ClaudeCode), "mcpServers");
        assert_eq!(mcp_root_key(Target::Dsh), "mcpServers");
    }

    #[test]
    fn bridge_uses_vision_endpoint_env() {
        let entry = mcp_entry(
            Target::ClaudeDesktop,
            "http://127.0.0.1:9527/mcp/vision",
            "sk-test",
            Some(Path::new("/tmp/vision-bridge.js")),
            60,
        );
        assert_eq!(entry["command"], "node");
        assert_eq!(
            entry["env"]["AGENT2API_VISION_ENDPOINT"],
            "http://127.0.0.1:9527/mcp/vision"
        );
        assert_eq!(entry["env"]["AGENT2API_VISION_TIMEOUT_MS"], 120_000);
    }

    #[test]
    fn http_entry_carries_authorization() {
        let entry = mcp_entry(
            Target::Codex,
            "http://127.0.0.1:9527/mcp/web-search",
            "sk-test",
            None,
            30,
        );
        assert_eq!(entry["http_headers"]["Authorization"], "Bearer sk-test");
        assert_eq!(entry["tool_timeout_sec"], 30);
    }
}
