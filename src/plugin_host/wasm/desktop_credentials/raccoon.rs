// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 小浣熊（Raccoon Office）本机登录态读取器。
//!
//! ── 来源 ────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/raccoon.rs`（v3.5.0-beta.7，
//! AGPL-3.0）。上游职责：定位小浣熊官方客户端的 `auth.json`，把登录态投影成
//! 白名单字段交给插件。本文件与该上游行为逐条对齐：
//!
//! 1. **候选路径顺序**（常量，非参数）：
//!    `RACCOON_BOX_AGENT_CONFIG_DIR` → `BOX_AGENT_CONFIG_DIR`（各自末尾追加
//!    `auth.json`）→ `BOX_AGENT_HOME`（追加 `config/auth.json`）→ 客户端配置根
//!    （`%APPDATA%/office-raccoon`，Windows 追加 `~/.office-raccoon`）里的
//!    `.raccoon-lab-client.json` 标记判定 → 兜底 `~/.box-agent/config/auth.json`。
//! 2. **标记判定**：首个候选根目录存在且**没有**标记文件 ⇒ 已是新版布局，直接
//!    用 `~/.box-agent/config/auth.json`；否则逐个读标记，要求
//!    `application == "@raccoon/office-desktop"` 且 `version == 1`，命中即用该根
//!    下的 `agent/config/auth.json`。
//! 3. **字段投影**：按别名组取值，**同组内顶层优先、嵌套 `auth` 兜底**，输出统一
//!    用组内首个别名做键（见 [`groups`]）。`accessToken` 缺失或空白 ⇒ 失败。
//!
//! ── 只读边界 ────────────────────────────────────────────────────
//! - 路径全部来自常量与进程环境变量，**不接受任何调用参数**（接受路径即等于
//!   任意文件读取）。
//! - 只经父模块 [`super::regular`] / [`super::object`] 做 `stat` + `read`：符号
//!   链接、Windows reparse 点、超限、非对象一律判不可读；不写、不删、不加锁。
//! - 投影结果经 [`super::content`] 包成 `{sourceId, sourceRevision, content}`，
//!   不回传未筛选的原始 JSON。
//! - 上游同目录的 `refresh`（Token 续期）属于另一组能力（`desktop.refresh.*`），
//!   不在本移植范围内，故不引入 `mod refresh`。

use std::path::PathBuf;

use serde_json::Value;

use crate::plugin_host::wasm::wire::WireError;

use super::{absolute_env, app_data, content, home, invalid, object};

/// 登录态字段别名组：组内任一别名命中即取值，输出键固定为组内首个别名。
const GROUPS: &[&[&str]] = &[
    &["accessToken", "access_token", "token", "auth_token"],
    &["refreshToken", "refresh_token"],
    &["expiresAt", "tokenExpiresAt"],
    &["userId"],
    &["officeIdentity", "office_identity"],
    &["officeOrgName", "office_org_name"],
    &["officeOrgRole", "office_org_role"],
];

/// 上游 `selected()`：按固定顺序解析出 `auth.json` 的绝对路径。
///
/// 可见性保持 `pub(super)`（上游同口径），供父模块与 [`read`] 使用，不外扩。
pub(super) fn selected() -> Result<PathBuf, WireError> {
    if let Some(root) = absolute_env("RACCOON_BOX_AGENT_CONFIG_DIR")
        .or_else(|| absolute_env("BOX_AGENT_CONFIG_DIR"))
    {
        return Ok(root.join("auth.json"));
    }
    if let Some(root) = absolute_env("BOX_AGENT_HOME") {
        return Ok(root.join("config/auth.json"));
    }
    let home = home()?;
    // 非 Windows 上第二个候选根目录不参与探测（`roots` 不再被 push）。
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut roots = vec![app_data()?.join("office-raccoon")];
    #[cfg(windows)]
    roots.push(home.join(".office-raccoon"));
    let marker = ".raccoon-lab-client.json";
    // 首个根目录在、却没有标记文件：客户端已迁移到新版布局，直接读新位置。
    if roots
        .first()
        .is_some_and(|root| root.is_dir() && !root.join(marker).exists())
    {
        return Ok(home.join(".box-agent/config/auth.json"));
    }
    for root in roots {
        if let Ok(value) = object(&root.join(marker), 4096) {
            if value["application"] == "@raccoon/office-desktop" && value["version"] == 1 {
                return Ok(root.join("agent/config/auth.json"));
            }
        }
    }
    Ok(home.join(".box-agent/config/auth.json"))
}

/// `desktop.credentials.read` 的小浣熊分支：定位 → 读 → 投影 → 包信封。
pub(super) fn read() -> Result<Value, WireError> {
    let path = selected()?;
    let value = object(&path, 256 * 1024)?;
    let projected = project(&value)?;
    content(&path, projected)
}

/// 白名单投影：别名组内顶层优先，嵌套 `auth` 兜底；`accessToken` 必填。
fn project(value: &Value) -> Result<Value, WireError> {
    let present = |value: &Value| {
        !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty())
    };
    let mut result = serde_json::json!({});
    for aliases in GROUPS {
        // 同字段的别名也按顶层优先：不能让嵌套 accessToken 盖过顶层 access_token。
        let selected = aliases
            .iter()
            .find_map(|key| value.get(*key).filter(|value| present(value)))
            .or_else(|| {
                aliases
                    .iter()
                    .find_map(|key| value["auth"].get(*key).filter(|value| present(value)))
            });
        if let Some(value) = selected {
            // GROUPS 的每组第一个是输出键；空组（配置错误）不能 panic，直接跳过。
            if let Some(key) = aliases.first() {
                result[*key] = value.clone();
            }
        }
    }
    if result["accessToken"]
        .as_str()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(invalid());
    }
    Ok(result)
}
