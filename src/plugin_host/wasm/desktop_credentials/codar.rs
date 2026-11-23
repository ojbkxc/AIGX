// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Codar 本机配置的固定只读读取器。
//!
//! ── 来源（固定，不接受插件参数）────────────────────────────────────
//! 数据根只有两个可能：环境变量 `CODAR_HOME`（必须是绝对路径，否则直接判无效）
//! 或 home 兜底 `~/.codar`；只读其中的 `config.json`。
//!
//! ── 投影 ──────────────────────────────────────────────────────────
//! `provider.active_profile`（缺省 `speedai`）选出 profile；profiles 先看
//! `provider.provider_profiles.<active>`，再退 `provider_profiles.<active>`，
//! 都没有则把 `provider` 自身当 profile。最终只投 `speedai.token`（JWT）：
//! - 必须是三段式、≤ 8192 字节、字符集限定 `A-Za-z0-9-_` 与 `.`；
//! - 载荷段用 base64url（无 padding，允许尾部 `=`）解出 JSON，只取 `username`；
//! - 输出 `{profile, username, token}` 三个字段，其余一概不返回。
//!
//! ── 只读边界 ──────────────────────────────────────────────────────
//! 只 stat + read（经 `super::object`，路径链上 symlink / Windows reparse 一律拒收），
//! 不写回官方配置、不发起网络请求、不读其它客户端目录。
//!
//! ── 与上游的等价改写（如实声明）───────────────────────────────────
//! - 上游用 `zeroize::Zeroizing` 包住 `file()` 的字节，再用
//!   `managed_task::declaration::parse_object` 做「顶层必须是对象 + 严格 JSON」的解析；
//!   本仓没有 `zeroize`，也没有 `strict_json`，故改用父模块既有的
//!   `super::object(path, 256 KiB)`：同样「大小上限 + 顶层必须对象 + 解析失败即失败」。
//!   唯一差异是 serde_json 对重复键取后者（上游严格解析会拒收）；本文件只投影
//!   白名单字段，重复键不可能放大可见内容。
//! - 上游返回 `proto::WireError`；本仓统一为 `crate::plugin_host::wasm::wire::WireError`。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{home, invalid, object};

/// 从选定 profile 里取 JWT 并解出 `username`；任一环不合格就整条拒收。
fn candidate(profile: &str, value: &Value) -> Option<Value> {
    let token = value["speedai"]["token"].as_str()?.trim();
    if token.is_empty()
        || token.len() > 8192
        || token.split('.').count() != 3
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(token.split('.').nth(1)?.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let username = claims["username"].as_str()?.trim();
    if username.is_empty() || username.len() > 256 || username.chars().any(char::is_control) {
        return None;
    }
    Some(json!({ "profile": profile, "username": username, "token": token }))
}

pub(super) fn read() -> Result<Value, WireError> {
    let root = match std::env::var_os("CODAR_HOME").filter(|path| !path.is_empty()) {
        Some(path) => {
            let path = std::path::PathBuf::from(path);
            if !path.is_absolute() {
                return Err(invalid());
            }
            path
        }
        None => home()?.join(".codar"),
    };
    let value = object(&root.join("config.json"), 256 * 1024)?;
    let provider = &value["provider"];
    let active = provider["active_profile"].as_str().unwrap_or("speedai");
    if active.is_empty() || active.len() > 128 || active.chars().any(char::is_control) {
        return Err(invalid());
    }
    let profiles = provider["provider_profiles"]
        .as_object()
        .or_else(|| value["provider_profiles"].as_object());
    if let Some(selected) = profiles.and_then(|profiles| profiles.get(active)) {
        return candidate(active, selected).ok_or_else(invalid);
    }
    candidate(active, provider).ok_or_else(invalid)
}
