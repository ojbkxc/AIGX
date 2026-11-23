// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! Command Code 官方 CLI 登录文件的固定只读快照读取器。
//!
//! ── 来源（固定，不接受插件参数）────────────────────────────────────
//! 只读 `~/.commandcode/auth.json`（`super::home()` + 常量后缀）。
//!
//! ── 投影与失败条件 ────────────────────────────────────────────────
//! 只投 `apiKey` 一个字段，并且必须满足：`user_` 前缀、前缀后非空、
//! 其后只含 `A-Za-z0-9` 与 `_-`、全长 ≤ 8192。不投影 `remoteControl`、
//! BYOK、代理设置或客户端全局配置。
//!
//! ── 只读边界 ──────────────────────────────────────────────────────
//! 只 stat + read（经 `super::object`，路径链上 symlink / Windows reparse 一律拒收），
//! 不写回官方文件、不读远程控制或其他客户端凭据。
//!
//! ── 与上游的等价改写（如实声明）───────────────────────────────────
//! 上游返回 `proto::WireError`；本仓统一为
//! `crate::plugin_host::wasm::wire::WireError`，其余逐行同口径（含
//! `super::content()` 的 `{sourceId, sourceRevision, content}` 外壳）。

use serde_json::json;

use crate::plugin_host::wasm::wire::WireError;

use super::{content, home, invalid, object};

pub(super) fn read() -> Result<serde_json::Value, WireError> {
    let path = home()?.join(".commandcode/auth.json");
    let value = object(&path, 64 * 1024)?;
    let key = value["apiKey"]
        .as_str()
        .map(str::trim)
        .filter(|key| {
            key.len() <= 8192
                && key.strip_prefix("user_").is_some_and(|body| {
                    !body.is_empty()
                        && body
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                })
        })
        .ok_or_else(invalid)?;
    // apiKey 是官方读取形态；不投影 remoteControl、BYOK 或客户端全局配置。
    content(&path, json!({ "apiKey": key }))
}
