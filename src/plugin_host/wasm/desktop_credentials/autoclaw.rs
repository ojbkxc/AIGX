// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! AutoClaw（autoclaw / autoclaw-intl）本机登录态读取器 —— safeStorage 形态。
//!
//! ── 来源 ────────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/autoclaw.rs`（v3.5.0-beta.7，
//! AGPL-3.0）。上游职责：定位 AutoClaw 官方客户端的数据根，读 `auth.json`，
//! 用 `Local State` 里的 safeStorage 主密钥（DPAPI 解出）做 AES-256-GCM 解封，
//! 再把 JWT 里能读到的账号信息投影给插件。行为逐条对齐：
//!
//! 1. **固定数据根**（常量 + 进程环境变量，非参数）：
//!    `AUTOCLAW_USER_DATA_DIR`（必须绝对路径）→ `app_data()/AutoClaw`。
//! 2. **首选 `auth.json`**（≤ 256 KiB、必须是 JSON 对象）：`token` /
//!    `refreshToken` 各自解封（`enc:` 前缀走 safeStorage，否则按明文剥一层
//!    `Bearer `）；`token` 为空即失败。结果为 `{deviceId, email, token,
//!    refreshToken, userId, expiresAt?, source:"auth-file"}`，并经
//!    [`super::content`] 包成 `{sourceId, sourceRevision, content}`。
//! 3. **兜底网关配置**：没有 `auth.json` 时读
//!    `~/.openclaw-autoclaw/openclaw.json`（≤ 256 KiB），把
//!    `models.providers.*.models[].headers` 里的 `X-Authorization` /
//!    `x-authorization` 逐个解封、排序去重；**必须恰好 1 个**不同 token，
//!    否则判不可读（0 个或多账号都算歧义，不猜），`source:"gateway-config"`。
//! 4. **JWT 投影**：`token` 第二段做 base64url(no pad) 解码，取 `user_id`
//!    （字符串优先，整数转字符串，都没有给空串）、`device_id`（`deviceId`
//!    缺失时兜底）、`exp`（乘 1000 得毫秒 `expiresAt`）；`email` 缺失时兜底
//!    `userInfo.email`。JWT 解不开不算失败，只是这些字段为空。
//! 5. **信封整形**：`content` 的信封里去掉 `sourceId`（与其它 provider 同形）；
//!    `provider == "autoclaw-intl"` 时补 `"provider": "autoclaw-intl"`。
//!
//! ── 只读边界 ────────────────────────────────────────────────────────
//! - 路径只由常量 + 进程环境变量拼成，**不接受任何调用参数**（接受路径即等于
//!   任意文件读取）。
//! - 只经父模块 [`super::file`] / [`super::object`] 做 `stat` + `read`：符号
//!   链接、Windows reparse 点、超限、非对象一律判不可读；不写、不删、不加锁。
//! - 不回传密钥、`Local State` 原文，也不回传解封过程中的任何原始字节；失败
//!   一律 [`super::invalid`]，不回显底层错误。
//!
//! ── 与上游的等价改写（如实声明）───────────────────────────────────────
//! - 上游的 DPAPI / AES 在 [`crypto`] 里走 `windows` crate + `aes-gcm 0.11`；
//!   本仓没有 `windows` 依赖且 `aes-gcm` 是 `0.10`，故 `crypto` 改为
//!   `extern "system"` 直连 `crypt32` / `kernel32` + `Aead::decrypt`，
//!   密文布局、nonce 长度、tag 位置、失败条件不变（详见 `crypto` 模块头）。
//! - 上游 `zeroize::Zeroizing` → 普通 `Vec<u8>`：解出的明文只在 [`crypto`]
//!   的作用域里短命持有，AIGX 不引 zeroize、也不改 Cargo.toml。
//! - 上游 `mod crypto` 只在 Windows 编译并在本文件里留 `#[cfg(not(windows))]`
//!   分支；本文件**不分平台**地调用 [`crypto::decrypt`]（`file()` 照读
//!   `Local State`），非 Windows 由 `crypto` 内部返回 [`super::invalid`]，
//!   观察到的失败形状与上游一致，但 6 平台的类型检查走同一份代码。
//! - 上游同文件的 `seal`（重新封存）与 `decrypt(provider, payload)`（把外部
//!   传入的 `token`/`refreshToken` 就地解封）在本仓暂无调用方，未移植，不留
//!   死代码。

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{absolute_env, app_data, content, file, home, invalid, object, project};

mod crypto;

/// `Local State` 上限（上游同值 2 MiB）。
const LOCAL_STATE_MAXIMUM: u64 = 2 * 1024 * 1024;

/// `auth.json` / `openclaw.json` 上限（上游同值 256 KiB）。
const AUTH_MAXIMUM: u64 = 256 * 1024;

/// 解封 token / 明文 token 的剥壳：`enc:` 走 safeStorage，否则按明文去空白。
fn unseal(raw: &str, state: &std::path::Path) -> Result<String, WireError> {
    if !raw.starts_with("enc:") {
        let trimmed = raw.trim();
        let plain = trimmed.strip_prefix("Bearer ").unwrap_or(trimmed);
        return Ok(plain.to_string());
    }
    crypto::decrypt(raw, &file(state, LOCAL_STATE_MAXIMUM)?)
}

/// JWT 的第二段（payload）解成 JSON；解不开给 `Value::Null`（不算失败）。
fn claims(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part.trim_end_matches('=')).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// 白名单投影：只从 JWT 与 `auth.json` 取固定几个字段，其余一律不返回。
fn projected(token: String, refresh: String, original: &Value) -> Value {
    let claims = claims(&token);
    let mut result = project(original, &["deviceId", "email"]);
    result["token"] = json!(token);
    result["refreshToken"] = json!(refresh);
    result["userId"] = match claims["user_id"].as_str() {
        Some(value) => json!(value),
        None => match claims["user_id"].as_i64() {
            Some(value) => json!(value.to_string()),
            None => json!(""),
        },
    };
    if result["deviceId"].is_null() {
        result["deviceId"] = claims["device_id"].clone();
    }
    if let Some(seconds) = claims["exp"].as_i64() {
        result["expiresAt"] = json!(seconds.saturating_mul(1000));
    }
    if result["email"].is_null() {
        result["email"] = original["userInfo"]["email"].clone();
    }
    result
}

/// `desktop.credentials.read` 的 AutoClaw 分支：定位 → 读 → 解封 → 投影 → 包信封。
pub(super) fn read(provider: &str) -> Result<Value, WireError> {
    let root = absolute_env("AUTOCLAW_USER_DATA_DIR")
        .unwrap_or(app_data()?.join("AutoClaw"));
    let path = root.join("auth.json");
    let mut response = if path.exists() {
        let value = object(&path, AUTH_MAXIMUM)?;
        let state = root.join("Local State");
        let token = unseal(value["token"].as_str().unwrap_or(""), &state)?;
        let refresh = unseal(value["refreshToken"].as_str().unwrap_or(""), &state)?;
        if token.is_empty() {
            return Err(invalid());
        }
        let mut result = content(&path, projected(token, refresh, &value))?;
        result["source"] = json!("auth-file");
        result
    } else {
        let path = home()?.join(".openclaw-autoclaw/openclaw.json");
        let value = object(&path, AUTH_MAXIMUM)?;
        let mut tokens = Vec::new();
        for model in value["models"]["providers"]
            .as_object()
            .into_iter()
            .flat_map(|items| items.values())
            .flat_map(|provider| provider["models"].as_array().into_iter().flatten())
        {
            for key in ["X-Authorization", "x-authorization"] {
                if let Some(token) = model["headers"][key]
                    .as_str()
                    .filter(|token| !token.is_empty())
                {
                    tokens.push(unseal(token, &root.join("Local State"))?);
                }
            }
        }
        tokens.sort();
        tokens.dedup();
        if tokens.len() != 1 {
            return Err(invalid());
        }
        let mut result = content(
            &path,
            projected(tokens.remove(0), String::new(), &json!({})),
        )?;
        result["source"] = json!("gateway-config");
        result
    };
    response
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("sourceId");
    if provider == "autoclaw-intl" {
        response["provider"] = json!(provider);
    }
    Ok(response)
}
