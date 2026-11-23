// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! AStudio（astron-code / acode）本机登录态读取器。
//!
//! ── 来源（全部固定，不接受插件参数）────────────────────────────────
//! 候选数据根按上游顺序拼装：
//! 1. 环境变量 `ASTUDIO_HOME`、`SYNARA_HOME`（必须是绝对路径才算数）；
//! 2. Windows 当前用户注册表 `HKCU\Software\AStudioStorage` 的
//!    `ProductionDataRoot` / `CanaryDataRoot` / `DevelopmentDataRoot`
//!    （`REG_SZ`，≤ 8 KiB，必须绝对路径）；
//! 3. home 兜底 `~/.acode/acode`。
//!
//! 每个数据根读两份文件：
//! - `userdata/astron-session.json`：只投影 `modelBearerToken` / `uid` / `token` /
//!   `accountId` / `ssoSessionId` / `banned`；
//! - 配套 `runtime/acode-home/config.toml`：只取
//!   `model_providers.astron-spark` 的 `experimental_bearer_token`（记作 `key`）与 `uid`。
//!   config 必须来自**同一个数据根**，不能把另一个频道（canary/dev）的模型密钥
//!   接到当前网页账号上；两处 `uid` 都非空且不相等时判不可读。
//!
//! ── 只读边界 ──────────────────────────────────────────────────────
//! 只做 `symlink_metadata` / `stat` / `read`：不写回官方文件、不跟随符号链接
//! （`super::regular` 在整条路径链上拦截 symlink 与 Windows reparse 位）、
//! 不做解密、不读浏览器 Cookie。`banned` 为真时直接判不可读，不返回任何字段。
//!
//! ── 与上游的等价改写（如实声明）───────────────────────────────────
//! - 上游用 `windows` crate 的 `RegGetValueW` 读注册表；本仓既没有该依赖也不新增依赖，
//!   故用 `extern "system"` 直接声明同签名系统 FFI（只链系统 `advapi32`），
//!   语义一致：`RRF_RT_REG_SZ`、8 KiB 上限、只收绝对路径。
//! - 上游用 `zeroize::Zeroizing` 包住 `file()` 读出的字节；本仓没有 `zeroize` 依赖，
//!   改为普通 `Vec<u8>`，解析后即丢弃，不外泄、不落盘。
//! - 上游返回 `proto::WireError`；本仓统一为 `crate::plugin_host::wasm::wire::WireError`。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{absolute_env, file, home, invalid, object, project};

#[cfg(windows)]
fn registry_root(name: &str) -> Option<PathBuf> {
    use std::ffi::c_void;

    // 与 `windows` crate 的 `RegGetValueW` 同签名；只链系统库，不引入第三方 crate。
    #[allow(non_snake_case)]
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    /// `HKEY_CURRENT_USER`（预定义句柄，`0x80000001`）。
    const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as isize;
    /// `RRF_RT_REG_SZ`：只接受 `REG_SZ`。
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;
    /// 上游同口径：注册表值上限 8 KiB（含结尾 NUL）。
    const MAXIMUM: usize = 8192;

    let subkey: Vec<u16> = "Software\\AStudioStorage"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let value: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buffer = vec![0u16; MAXIMUM / 2];
    let mut size = u32::try_from(buffer.len() * 2).ok()?;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 || size == 0 || size as usize > MAXIMUM {
        return None;
    }
    // 成功时 `size` 含结尾 NUL，按上游口径剥掉。
    let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
    let text = String::from_utf16(&buffer[..length]).ok()?;
    let path = PathBuf::from(text);
    path.is_absolute().then_some(path)
}

pub(super) fn read() -> Result<Value, WireError> {
    // 非 Windows 上注册表分支被裁掉，`roots` 不再被 push —— 不让 `unused_mut` 变红。
    #[cfg_attr(not(windows), allow(unused_mut))]
    let mut roots = ["ASTUDIO_HOME", "SYNARA_HOME"]
        .into_iter()
        .filter_map(absolute_env)
        .collect::<Vec<_>>();
    #[cfg(windows)]
    for name in [
        "ProductionDataRoot",
        "CanaryDataRoot",
        "DevelopmentDataRoot",
    ] {
        if let Some(root) = registry_root(name) {
            roots.push(root);
        }
    }
    let mut candidates: Vec<(PathBuf, PathBuf)> = roots
        .iter()
        .map(|root| {
            (
                root.join("userdata/astron-session.json"),
                root.join("runtime/acode-home/config.toml"),
            )
        })
        .collect();
    let fallback = home()?.join(".acode/acode");
    candidates.push((
        fallback.join("userdata/astron-session.json"),
        fallback.join("config.toml"),
    ));
    for (session, config) in &candidates {
        if !exists(session)? {
            continue;
        }
        let value = object(session, 256 * 1024)?;
        if value["banned"] == true {
            return Err(invalid());
        }
        let mut credentials = project(
            &value,
            &[
                "modelBearerToken",
                "uid",
                "token",
                "accountId",
                "ssoSessionId",
                "banned",
            ],
        );
        // 只从同一数据根取配套票据，不能把另一个频道的模型密钥接到当前网页账号。
        // 官方 config 与 session 的 Bearer 用途可不同；网页 token 绝不投成模型凭据。
        let configured = config_credentials(config)?;
        if !configured.is_null() {
            let config_uid = scalar(&configured["uid"]);
            let session_uid = scalar(&credentials["uid"]);
            if !config_uid.is_empty()
                && config_uid != "0"
                && !session_uid.is_empty()
                && session_uid != "0"
                && config_uid != session_uid
            {
                return Err(invalid());
            }
            credentials["key"] = configured["key"].clone();
            if session_uid.is_empty() || session_uid == "0" {
                credentials["uid"] = configured["uid"].clone();
            }
        }
        let model = ["key", "modelBearerToken"]
            .iter()
            .any(|key| present(&credentials[*key]));
        let web = present(&credentials["ssoSessionId"])
            || present(&credentials["token"]) && !scalar(&credentials["accountId"]).is_empty();
        let legacy_model = present(&credentials["token"]) && !web;
        if model || web || legacy_model {
            return Ok(json!({ "credentials": credentials }));
        }
    }
    for (_, config) in candidates {
        let credentials = config_credentials(&config)?;
        if !credentials.is_null() {
            return Ok(json!({ "credentials": credentials }));
        }
    }
    Err(invalid())
}

fn exists(path: &Path) -> Result<bool, WireError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(invalid()),
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_string(),
        Value::Number(value) if value.is_i64() || value.is_u64() => value.to_string(),
        _ => String::new(),
    }
}

fn present(value: &Value) -> bool {
    value.as_str().is_some_and(|value| !value.trim().is_empty())
}

fn config_credentials(path: &Path) -> Result<Value, WireError> {
    if !exists(path)? {
        return Ok(Value::Null);
    }
    let bytes = file(path, 256 * 1024)?;
    let raw = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
    let value: toml::Value = toml::from_str(raw).map_err(|_| invalid())?;
    let Some(provider) = value
        .get("model_providers")
        .and_then(|value| value.get("astron-spark"))
    else {
        return Ok(Value::Null);
    };
    let key = provider
        .get("experimental_bearer_token")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if key.is_empty() {
        return Ok(Value::Null);
    }
    let uid = provider
        .get("uid")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    Ok(json!({ "key": key, "uid": uid }))
}
