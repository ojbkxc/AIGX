// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! AutoClaw safeStorage 主密钥解封（`Local State` → DPAPI → AES-256-GCM）。
//!
//! ── 来源 ────────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/autoclaw/crypto.rs`
//! （v3.5.0-beta.7，AGPL-3.0）。上游职责：用 DPAPI 解出 Chromium safeStorage
//! 主密钥，再用它 AES-256-GCM 解 `enc:` 密文。行为逐条对齐：
//!
//! 1. **主密钥位置**：`Local State` 的 `os_crypt.encrypted_key`（字符串，
//!    ≤ 16384 字节）→ base64 标准解码 → 必须带 `DPAPI` 前缀（剥掉后非空）。
//! 2. **DPAPI 解密**：`CryptUnprotectData(blob, -, -, -, -,
//!    CRYPTPROTECT_UI_FORBIDDEN)`（`0x1`：禁止弹 UI，免得宿主进程卡在对话框
//!    上）；出参必须是 32 字节，否则判失败；出参缓冲区一律 `LocalFree` 释放。
//! 3. **密文布局**：`enc:` + base64 标准解码 = `v10 ‖ nonce(12B) ‖ ciphertext
//!    ‖ tag(16B)`；总长必须 > 31 且以 `v10` 开头；AAD 为空，tag 在末尾 16 字节。
//! 4. **明文校验**：UTF-8 → `trim` → 剥一层 `Bearer ` → 非空、≤ 32768 字节、
//!    不含控制字符与空白，否则判失败。
//!
//! ── 只读边界 ────────────────────────────────────────────────────────
//! 本文件只做纯计算：不读文件、不写文件、不落盘；`Local State` 的字节由调用方
//! （`super`，即 [`super::read`]）读好传进来。失败一律父模块 [`invalid`]，不回显
//! 底层错误（DPAPI 的 `GetLastError`、aes-gcm 的 `Error` 都不外传）。
//!
//! ── 与上游的等价改写（如实声明）───────────────────────────────────────
//! - 上游用 `windows` crate 的 `CryptUnprotectData` / `HLOCAL`；本仓没有该依赖
//!   也不新增依赖，故用 `extern "system"` 直接声明同签名系统 FFI（只链系统
//!   `crypt32` / `kernel32`）。`CRYPT_INTEGER_BLOB` 用 `#[repr(C)]` 镜像
//!   （`cb_data: u32` / `pb_data: *mut u8`），字段名改成 snake_case 不影响 ABI。
//! - 上游 `zeroize::Zeroizing` + 出参缓冲区的 `zeroize()` → 普通 `Vec<u8>`：
//!   解得的主密钥只在 [`decrypt`] 的作用域里短命持有；出参仍以 `LocalFree`
//!   释放（`Output` 的 `Drop`），不漏内存。
//! - 上游 `aes-gcm 0.11` 的 `AeadInOut::decrypt_in_place` → `0.10` 的
//!   `Aead::decrypt`：同样空 AAD、`ciphertext‖tag` 当作一个缓冲，nonce 仍是
//!   12 字节、tag 仍在末尾 16 字节，切分点不变。
//! - [`key`] / [`unprotect`] 的可见性与失败形状保持：`key` 是模块内私有，
//!   上游返回带着缓冲区的封装，这里按本仓要求直接返回 32 字节 `Vec<u8>`。
//! - 上游的 `encrypt`（`seal` 用）在本仓暂无调用方，未移植。

use aes_gcm::aead::{Aead as _, Payload};
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;

use crate::plugin_host::wasm::wire::WireError;

use super::super::invalid;

/// `os_crypt.encrypted_key` 的 base64 原文上限（上游同值 16 KiB）。
const ENCODED_MAXIMUM: usize = 16 * 1024;

/// safeStorage 主密钥长度（AES-256）。
#[cfg(windows)]
const KEY_LENGTH: usize = 32;

/// 解 `enc:` 前缀的 auth 密文（safeStorage，DPAPI 解出的主密钥）。
pub(super) fn decrypt(raw: &str, local_state: &[u8]) -> Result<String, WireError> {
    let key = key(local_state)?;
    let encrypted = STANDARD
        .decode(raw.strip_prefix("enc:").ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    if encrypted.len() <= 31 || !encrypted.starts_with(b"v10") {
        return Err(invalid());
    }
    // `Payload::from` 就是 `{ msg, aad: &[] }`：空 AAD，`ciphertext‖tag` 一个缓冲。
    let payload = Payload::from(&encrypted[15..]);
    let plain = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| invalid())?
        .decrypt(Nonce::from_slice(&encrypted[3..15]), payload)
        .map_err(|_| invalid())?;
    let token = std::str::from_utf8(&plain).map_err(|_| invalid())?.trim();
    let token = token.strip_prefix("Bearer ").unwrap_or(token);
    if token.is_empty()
        || token.len() > 32768
        || token
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(invalid());
    }
    Ok(token.to_string())
}

/// 从 `Local State` 里取 safeStorage 主密钥（32 字节）。
fn key(local_state: &[u8]) -> Result<Vec<u8>, WireError> {
    let state: Value = serde_json::from_slice(local_state).map_err(|_| invalid())?;
    let encoded = state["os_crypt"]["encrypted_key"]
        .as_str()
        .filter(|value| value.len() <= ENCODED_MAXIMUM)
        .ok_or_else(invalid)?;
    let blob = STANDARD.decode(encoded).map_err(|_| invalid())?;
    let blob = blob
        .strip_prefix(b"DPAPI")
        .filter(|blob| !blob.is_empty())
        .ok_or_else(invalid)?;
    unprotect(blob)
}

/// DPAPI 解封（Windows）：成功时必须正好 32 字节。
#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, WireError> {
    let input = dpapi::Blob {
        cb_data: blob.len() as u32,
        pb_data: blob.as_ptr().cast_mut(),
    };
    let mut output = Output::empty();
    let status = unsafe {
        dpapi::CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dpapi::UI_FORBIDDEN,
            &mut output.0,
        )
    };
    if status == 0 || output.0.pb_data.is_null() || output.0.cb_data != KEY_LENGTH as u32 {
        return Err(invalid());
    }
    let key = unsafe { std::slice::from_raw_parts(output.0.pb_data, KEY_LENGTH) };
    Ok(key.to_vec())
}

/// 非 Windows：没有 DPAPI，明文 token 仍可读（走调用方的明文分支），密文判不可用。
#[cfg(not(windows))]
fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, WireError> {
    Err(invalid())
}

/// `CryptUnprotectData` 的出参；`Drop` 时 `LocalFree`（上游用 `windows::core::Owned`）。
#[cfg(windows)]
struct Output(dpapi::Blob);

#[cfg(windows)]
impl Output {
    fn empty() -> Self {
        Output(dpapi::Blob::default())
    }
}

#[cfg(windows)]
impl Drop for Output {
    fn drop(&mut self) {
        if self.0.pb_data.is_null() {
            return;
        }
        let _ = unsafe { dpapi::LocalFree(self.0.pb_data.cast()) };
    }
}

/// 只声明用到的 `crypt32` / `kernel32` 符号（`windows` crate 的等价 FFI 镜像）。
#[cfg(windows)]
mod dpapi {
    use std::ffi::c_void;

    /// `CRYPT_INTEGER_BLOB`（`windows` crate 同名结构体的 `#[repr(C)]` 镜像）。
    #[derive(Default)]
    #[repr(C)]
    pub(super) struct Blob {
        pub(super) cb_data: u32,
        pub(super) pb_data: *mut u8,
    }

    /// `CRYPTPROTECT_UI_FORBIDDEN`：解密期间不允许弹任何 UI。
    pub(super) const UI_FORBIDDEN: u32 = 0x1;

    // 与 `windows` crate 的 `CryptUnprotectData` 同签名；只链系统库，不引第三方 crate。
    #[allow(non_snake_case)]
    #[link(name = "crypt32")]
    extern "system" {
        pub(super) fn CryptUnprotectData(
            data_in: *const Blob,
            description: *mut *mut u16,
            entropy: *const Blob,
            reserved: *mut c_void,
            prompt: *mut c_void,
            flags: u32,
            data_out: *mut Blob,
        ) -> i32;
    }

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    extern "system" {
        pub(super) fn LocalFree(handle: *mut c_void) -> *mut c_void;
    }
}
