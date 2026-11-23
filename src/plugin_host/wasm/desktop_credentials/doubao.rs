// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 豆包工作版（Doubao Work）本机登录态读取器 —— Chromium Cookie 库 + DPAPI（仅 Windows）。
//!
//! ── 来源 ────────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/doubao.rs`（v3.5.0-beta.7，
//! AGPL-3.0）。上游职责：读豆包工作版客户端的 Chromium Cookie 库，用 `Local
//! State` 里的 safeStorage 主密钥（DPAPI 解出）解 `v10` 密文，把已登录账号的
//! Cookie 投影给插件。行为逐条对齐：
//!
//! 1. **固定路径**（常量，非参数）：`%LOCALAPPDATA%/DoubaoWork/User Data`；
//!    `Local State` ≤ 8 MiB；账号档只认 `Default` 与 `Profile *`
//!    （目录项 > 2048 或档位 > 32 即失败，取前 32 个排序后处理）。
//! 2. **库文件上限**：`Network/Cookies` 与 `-wal` / `-shm` / `-journal`
//!    各自 ≤ 32 MiB；复制后的副本同样 ≤ 32 MiB，超限即失败、不截断。
//! 3. **Cookie 选择**：`host_key IN ('.doubao.com','doubao.com',
//!    'www.doubao.com','.www.doubao.com') AND path='/' LIMIT 257`；逐行校验
//!    `name`（非空、≤ 256、ASCII 可打印且不含 `=;,"\`）、过期时间
//!    （Chromium 微秒 → Unix 秒，`expires_utc <= 0` 视为会话 Cookie）、值
//!    （非空、≤ 16 KiB、不含控制字符与 `;`）；单库累计 ≤ 64 KiB；扫描行数
//!    > 256 即失败。
//! 4. **账号唯一**：只有含 `sessionid` / `sessionid_ss` 的档算一个账号；
//!    账号数必须正好 1，否则报 `desktop_credentials_ambiguous`（多账号不猜）。
//! 5. **投影**：`{cookies:[{name,value,domain,path,expires}], clientVersion?,
//!    channel?, deviceId?, iid?}`，其中 `clientVersion` / `channel` 从客户端
//!    `Application/*/manifest.json`（含注册表登记的安装根，≤ 64 KiB）里取
//!    **唯一一致**的那份；设备标识取 `Local State` 的 `aha.device.device_id`
//!    → `deviceId` 与 `aha.device.install_id` → `iid`（A2A v3.5.0 双键口径，
//!    新版客户端签到需要 iid）；整体 ≤ 256 KiB。
//!
//! ── 只读边界 ────────────────────────────────────────────────────────
//! - 路径只由常量 + `LOCALAPPDATA` 拼成，**不接受任何调用参数**（也不接受
//!   SQL），没有任何写回官方目录的路径。
//! - 库文件只经父模块 [`super::super::regular`] 做符号链接 / reparse 位 / 类型
//!   校验；查询只发生在**副本**上（见下），官方客户端的 WAL 与锁一概不碰。
//! - 不回传主密钥、`encrypted_value` 或任何未筛选的原始行；失败一律父模块
//!   [`super::super::invalid`]，不回显底层错误。
//!
//! ── 与上游的等价改写（如实声明）──────────────────────────────────────
//! - 上游用 `rusqlite::backup` 把活库整份搬到 `tempfile::tempdir()` 再查；
//!   本仓 `rusqlite` 未开 `backup` feature、`tempfile` 只在 dev-dependencies，
//!   故改成 [`snapshot`]：把主库与 `-wal` / `-shm` / `-journal` 原样复制进
//!   [`Scratch`]（`std::env::temp_dir()` + 进程号 + 原子计数，`Drop` 时删除）。
//!   语义相同——只查副本、不加锁、不改官方 journal；差异是复制不是一致性快照，
//!   客户端正在写时切出的副本可能不完整，这种情况按「不可读」失败而不猜。
//! - 上游用 `windows` crate 的 `CryptUnprotectData` / `RegGetValueW`；本仓没有
//!   该依赖，故用 `extern "system"` 直连 `crypt32` / `kernel32` / `advapi32`，
//!   `CRYPT_INTEGER_BLOB` 用 `#[repr(C)]` 镜像（风格同本目录 `astudio.rs`）。
//! - 上游 `zeroize::Zeroizing` → 普通 `Vec<u8>`：主密钥与明文只在函数作用域内
//!   短命持有；DPAPI 出参仍以 `LocalFree` 释放。
//! - 上游 `aes-gcm 0.11` 的 `AeadInOut::decrypt_in_place` → `0.10` 的
//!   `Aead::decrypt`：空 AAD、`ciphertext‖tag` 一个缓冲、nonce 12 字节、
//!   tag 在末尾 16 字节，切分点不变。
//! - 上游 `managed_task::declaration::parse_object` → 本文件内的
//!   [`parse_object`]：同口径（字节上限、UTF-8、逐层拒重复键、容器深度 ≤ 32、
//!   必须顶层对象）。
//! - 上游 `crate::server::logging::now_ms() / 1000` → `chrono::Utc::now()
//!   .timestamp()`（同为 Unix 秒，只用于过滤过期 Cookie）。
//! - 上游非 Windows 分支报 `desktop_platform_unsupported`；本仓按 lead 口径
//!   与父模块对齐，统一报 [`super::super::invalid`]。

use serde_json::Value;

use crate::plugin_host::wasm::wire::WireError;

/// `desktop.credentials.read` 的豆包工作版分支（Windows）。
#[cfg(windows)]
pub(super) fn read() -> Result<Value, WireError> {
    windows::read()
}

/// 非 Windows：上游没有这条读取链对应的 Cookie 库 / DPAPI 组合，直接判不可用。
#[cfg(not(windows))]
pub(super) fn read() -> Result<Value, WireError> {
    Err(super::invalid())
}

#[cfg(windows)]
mod windows {
    use std::path::{Path, PathBuf};

    use aes_gcm::aead::{Aead as _, Payload};
    use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
    use serde_json::{json, Value};
    use sha2::{Digest as _, Sha256};

    use crate::plugin_host::wasm::wire::WireError;

    use super::super::{absolute_env, file, invalid, regular};

    /// 上游 `types::MAX_JSON_DEPTH`：JSON 容器（对象 / 数组）最大嵌套层数。
    const MAX_JSON_DEPTH: usize = 32;

    /// `Local State` 上限（上游同值 8 MiB）。
    const LOCAL_STATE_MAXIMUM: u64 = 8 * 1024 * 1024;

    /// `os_crypt.encrypted_key` 的 base64 原文上限（上游同值 16 KiB）。
    const ENCODED_MAXIMUM: usize = 16 * 1024;

    /// Cookie 库与其副文件的上限（上游同值 32 MiB）。
    const DATABASE_MAXIMUM: u64 = 32 * 1024 * 1024;

    pub(super) fn read() -> Result<Value, WireError> {
        let root = absolute_env("LOCALAPPDATA")
            .ok_or_else(invalid)?
            .join("DoubaoWork")
            .join("User Data");
        let client_metadata = metadata(&root);
        let local = file(&root.join("Local State"), LOCAL_STATE_MAXIMUM)?;
        let state = parse_object(&local, LOCAL_STATE_MAXIMUM as usize)?;
        let encoded = state["os_crypt"]["encrypted_key"]
            .as_str()
            .filter(|value| value.len() <= ENCODED_MAXIMUM)
            .ok_or_else(invalid)?;
        let blob = STANDARD.decode(encoded).map_err(|_| invalid())?;
        let key = unprotect(blob.strip_prefix(b"DPAPI").ok_or_else(invalid)?)?;
        let mut profiles = Vec::new();
        let mut scanned = 0;
        for entry in std::fs::read_dir(&root).map_err(|_| invalid())? {
            scanned += 1;
            if scanned > 2048 {
                return Err(invalid());
            }
            let entry = entry.map_err(|_| invalid())?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == "Default" || name.starts_with("Profile ") {
                profiles.push(entry.path());
            }
            if profiles.len() > 32 {
                return Err(invalid());
            }
        }
        profiles.sort();
        let mut accounts = Vec::new();
        for profile in profiles {
            let source = profile.join("Network").join("Cookies");
            if !source.try_exists().map_err(|_| invalid())? {
                continue;
            }
            let copy = snapshot(&source)?;
            let path = copy.join("Cookies");
            // 一致性副本只读打开：与父模块 `database()` 同口径，绝不给客户端库加写锁。
            let db = rusqlite::Connection::open_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| invalid())?;
            let version: i64 = db
                .query_row(
                    "SELECT CAST(value AS INTEGER) FROM meta WHERE key='version'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| invalid())?;
            let mut stmt = db
                .prepare(
                    "SELECT host_key,name,value,encrypted_value,path,expires_utc FROM cookies WHERE host_key IN ('.doubao.com','doubao.com','www.doubao.com','.www.doubao.com') AND path='/' LIMIT 257",
                )
                .map_err(|_| invalid())?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })
                .map_err(|_| invalid())?;
            let mut cookies = Vec::new();
            let mut cookie_bytes = 0usize;
            let mut scanned_rows = 0usize;
            let now = chrono::Utc::now().timestamp();
            for row in rows {
                let (host, name, plain, encrypted, path, expires) = row.map_err(|_| invalid())?;
                scanned_rows += 1;
                if scanned_rows > 256
                    || name.is_empty()
                    || name.len() > 256
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_graphic() && !b"=;,\"\\".contains(&byte))
                {
                    return Err(invalid());
                }
                let expires = if expires <= 0 {
                    None
                } else {
                    Some(expires / 1_000_000 - 11_644_473_600)
                };
                if expires.is_some_and(|expires| expires <= now) {
                    continue;
                }
                let value = if !plain.is_empty() {
                    plain
                } else {
                    decrypt(&encrypted, &key, &host, version)?
                };
                if value.is_empty() {
                    continue;
                }
                if value.len() > 16 * 1024 || value.chars().any(|ch| ch.is_control() || ch == ';') {
                    return Err(invalid());
                }
                cookie_bytes = cookie_bytes
                    .checked_add(name.len() + value.len() + 3)
                    .filter(|size| *size <= 64 * 1024)
                    .ok_or_else(invalid)?;
                cookies.push(
                    json!({"name":name,"value":value,"domain":host,"path":path,"expires":expires}),
                );
            }
            if cookies.iter().any(signed_in) {
                accounts.push(cookies);
            }
        }
        if accounts.len() != 1 {
            return Err(WireError::with_code(
                "未找到唯一的豆包工作已登录账号；多个客户端账号请改用网页登录或手工凭证",
                "desktop_credentials_ambiguous",
            ));
        }
        let mut value = json!({"cookies":accounts.remove(0)});
        if let Some(fields) = client_metadata.as_object() {
            for (key, field) in fields {
                value[key] = field.clone();
            }
        }
        // 设备标识双键（A2A v3.5.0 对齐）：新版客户端把 install_id 也作为
        // 签到/上报参数，只读 device_id 会让新客户端的凭据缺 iid 被上游拒。
        // 两键同口径校验（1..=64 字节 ASCII 字母数字 / - / _），字符串与
        // 整数两种落盘形态都接受（客户端有版本差异）。
        for (source_key, target_key) in [("device_id", "deviceId"), ("install_id", "iid")] {
            let entry = &state["aha"]["device"][source_key];
            if let Some(text) = entry.as_str().filter(|text| valid_device(text)) {
                value[target_key] = json!(text);
            } else if let Some(number) = entry.as_u64() {
                value[target_key] = json!(number.to_string());
            }
        }
        if value.to_string().len() > 256 * 1024 {
            return Err(invalid());
        }
        Ok(value)
    }

    /// 客户端版本 / 频道：从 `Application` 下与注册表登记安装根里的
    /// `manifest.json` 取，只有「唯一一致」的那份才算数，否则不带这两个字段。
    fn metadata(root: &Path) -> Value {
        let Some(application) = root.parent().map(|root| root.join("Application")) else {
            return Value::Null;
        };
        let mut paths = Vec::new();
        for installed in installed_roots() {
            paths.push(installed.join("manifest.json"));
            paths.push(installed.join("app").join("manifest.json"));
        }
        let plain = application.join("manifest.json");
        if plain.is_file() {
            paths.push(plain);
        }
        if let Ok(entries) = std::fs::read_dir(&application) {
            for entry in entries.take(32).flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with(|ch: char| ch.is_ascii_digit()) && tagged(&name, b".-_") {
                    let path = entry.path().join("manifest.json");
                    if path.is_file() {
                        paths.push(path);
                    }
                }
            }
        }
        let mut found = Vec::new();
        for path in paths {
            let Ok(bytes) = file(&path, 64 * 1024) else {
                continue;
            };
            let Ok(value) = parse_object(&bytes, 64 * 1024) else {
                continue;
            };
            let version = value["version"].as_str().unwrap_or("");
            if !tagged(version, b".-_") {
                continue;
            }
            let channel = &value["channel"];
            let mut metadata = json!({"clientVersion":version});
            if let Some(raw) = channel.as_str().filter(|raw| tagged(raw, b"-_")) {
                metadata["channel"] = json!(raw);
            } else if let Some(raw) = channel.as_u64() {
                metadata["channel"] = json!(raw.to_string());
            }
            if !found.contains(&metadata) {
                found.push(metadata);
            }
        }
        if found.len() == 1 {
            found.remove(0)
        } else {
            Value::Null
        }
    }

    /// 注册表登记的安装根（`HKCU` / `HKLM` 的 `Uninstall\DoubaoWork`）。
    fn installed_roots() -> Vec<PathBuf> {
        use std::ffi::c_void;

        // 与 `windows` crate 的 `RegGetValueW` 同签名；只链系统库，不引第三方 crate。
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
        /// `HKEY_LOCAL_MACHINE`（预定义句柄，`0x80000002`）。
        const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as isize;
        /// `RRF_RT_REG_SZ`：只接受 `REG_SZ`。
        const RRF_RT_REG_SZ: u32 = 0x0000_0002;

        let key = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\DoubaoWork"
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut roots = Vec::new();
        for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
            for field in ["InstallLocation", "UninstallString"] {
                let name = field.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
                let mut data = [0u16; 2048];
                let mut size = (data.len() * 2) as u32;
                let status = unsafe {
                    RegGetValueW(
                        hive,
                        key.as_ptr(),
                        name.as_ptr(),
                        RRF_RT_REG_SZ,
                        std::ptr::null_mut(),
                        data.as_mut_ptr().cast(),
                        &mut size,
                    )
                };
                if status != 0 || size < 2 || size as usize > data.len() * 2 {
                    continue;
                }
                let end = match data.iter().position(|unit| *unit == 0) {
                    Some(end) => end,
                    None => data.len(),
                };
                let Ok(raw) = String::from_utf16(&data[..end]) else {
                    continue;
                };
                let raw = raw.trim();
                let path = if field == "UninstallString" {
                    if let Some(quoted) = raw.strip_prefix('"') {
                        quoted.split_once('"').map(|(path, _)| path)
                    } else {
                        raw.strip_suffix("uninstall.exe").map(|_| raw)
                    }
                } else {
                    Some(raw.trim_matches('"'))
                };
                let Some(path) = path else {
                    continue;
                };
                let path = PathBuf::from(path);
                if !path.is_absolute() {
                    continue;
                }
                let root = if field == "UninstallString" {
                    path.parent().map(Path::to_path_buf)
                } else {
                    Some(path)
                };
                if let Some(root) = root.filter(|root| launcher(root)) {
                    roots.push(root);
                }
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }

    /// 版本 / 频道 / 目录名形状：`1..=64` 字节，只含 ASCII 字母数字与给定分隔符。
    fn tagged(raw: &str, extra: &[u8]) -> bool {
        !raw.is_empty()
            && raw.len() <= 64
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
    }

    /// `deviceId` 形状：`1..=64` 字节的 ASCII 字母数字 / `-` / `_`。
    fn valid_device(device: &str) -> bool {
        !device.is_empty()
            && device.len() <= 64
            && device
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    }

    /// 安装根判定：目录里确实有 `DoubaoWork.exe`。
    fn launcher(root: &Path) -> bool {
        root.join("DoubaoWork.exe").is_file()
    }

    /// 这个账号是否拿到了豆包的会话 Cookie（`sessionid` / `sessionid_ss`）。
    fn signed_in(cookie: &Value) -> bool {
        matches!(cookie["name"].as_str(), Some("sessionid" | "sessionid_ss"))
    }

    /// 把活库与其 `-wal` / `-shm` / `-journal` 复制到私有临时目录（上游用
    /// `rusqlite::backup` 做同一件事；本仓 rusqlite 未开 `backup` feature）。
    fn snapshot(source: &Path) -> Result<Scratch, WireError> {
        const SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];
        if regular(source)?.len() > DATABASE_MAXIMUM {
            return Err(invalid());
        }
        for suffix in SUFFIXES {
            let sidecar = sidecar(source, suffix);
            if sidecar.try_exists().map_err(|_| invalid())?
                && regular(&sidecar)?.len() > DATABASE_MAXIMUM
            {
                return Err(invalid());
            }
        }
        let scratch = Scratch::acquire()?;
        let target = scratch.join("Cookies");
        std::fs::copy(source, &target).map_err(|_| invalid())?;
        for suffix in SUFFIXES {
            let from = sidecar(source, suffix);
            if from.try_exists().map_err(|_| invalid())? {
                std::fs::copy(&from, sidecar(&target, suffix)).map_err(|_| invalid())?;
            }
        }
        let info = std::fs::metadata(&target).map_err(|_| invalid())?;
        if info.len() > DATABASE_MAXIMUM {
            return Err(invalid());
        }
        Ok(scratch)
    }

    /// `Cookies` 的副文件路径（`-wal` / `-shm` / `-journal`）。
    fn sidecar(path: &Path, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}{suffix}", path.to_string_lossy()))
    }

    /// 私有临时目录：`temp_dir()` + 进程号 + 原子计数，`Drop` 时整体删除。
    ///
    /// 上游用 `tempfile::tempdir()`；AIGX 的 `tempfile` 只在 dev-dependencies 里，
    /// 故手写同语义的最小替身：唯一目录、用完即删、失败也不留在磁盘上。
    struct Scratch(PathBuf);

    impl Scratch {
        fn acquire() -> Result<Self, WireError> {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = format!("aigx-doubao-{}-{sequence}", std::process::id());
            let path = std::env::temp_dir().join(name);
            std::fs::create_dir(&path).map_err(|_| invalid())?;
            Ok(Scratch(path))
        }

        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 解 Chromium `v10` Cookie 密文：`v10 ‖ nonce(12B) ‖ ciphertext ‖ tag(16B)`。
    fn decrypt(blob: &[u8], key: &[u8], host: &str, version: i64) -> Result<String, WireError> {
        if blob.len() > 20 * 1024 || blob.len() < 31 || !blob.starts_with(b"v10") {
            return Err(invalid());
        }
        // `Payload::from` 就是 `{ msg, aad: &[] }`：空 AAD，`ciphertext‖tag` 一个缓冲。
        let payload = Payload::from(&blob[15..]);
        let plain = Aes256Gcm::new_from_slice(key)
            .map_err(|_| invalid())?
            .decrypt(Nonce::from_slice(&blob[3..15]), payload)
            .map_err(|_| invalid())?;
        let data = if version >= 24 {
            // Cookie 库 schema ≥ 24：明文前 32 字节是 `sha256(host_key)` 校验。
            let hash = Sha256::digest(host.as_bytes());
            if plain.len() <= 32 || plain[..32] != hash[..] {
                return Err(invalid());
            }
            &plain[32..]
        } else {
            plain.as_slice()
        };
        String::from_utf8(data.to_vec()).map_err(|_| invalid())
    }

    /// DPAPI 解封 safeStorage 主密钥：成功时必须正好 32 字节。
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
        if status == 0 || output.0.pb_data.is_null() || output.0.cb_data != 32 {
            return Err(invalid());
        }
        let key = unsafe { std::slice::from_raw_parts(output.0.pb_data, 32) };
        Ok(key.to_vec())
    }

    /// `CryptUnprotectData` 的出参；`Drop` 时 `LocalFree`（上游用 `windows::core::Owned`）。
    struct Output(dpapi::Blob);

    impl Output {
        fn empty() -> Self {
            Output(dpapi::Blob::default())
        }
    }

    impl Drop for Output {
        fn drop(&mut self) {
            if self.0.pb_data.is_null() {
                return;
            }
            let _ = unsafe { dpapi::LocalFree(self.0.pb_data.cast()) };
        }
    }

    /// 只声明用到的 `crypt32` / `kernel32` 符号（`windows` crate 的等价 FFI 镜像）。
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

        // 与 `windows` crate 的 `CryptUnprotectData` 同签名；只链系统库。
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

    /// 严格 JSON 形状检查（对标上游 `managed_task::strict_json::validate`）：
    /// 只检查结构、不执行表达式；每一层对象都拒绝重复键（含转义后相同的键）。
    struct Shape {
        // 标量不增加深度；根对象/数组算第一层，最多 32 层容器。
        containers: usize,
    }

    impl<'de> DeserializeSeed<'de> for Shape {
        type Value = ();

        fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
            deserializer.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for Shape {
        type Value = ();

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("bounded JSON with unique object keys")
        }

        fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_string<E: de::Error>(self, _: String) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E: de::Error>(self) -> Result<(), E> {
            Ok(())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
            if self.containers >= MAX_JSON_DEPTH {
                return Err(de::Error::custom("JSON depth exceeded"));
            }
            while sequence
                .next_element_seed(Shape {
                    containers: self.containers + 1,
                })?
                .is_some()
            {}
            Ok(())
        }

        fn visit_map<A: MapAccess<'de>>(self, mut object: A) -> Result<(), A::Error> {
            if self.containers >= MAX_JSON_DEPTH {
                return Err(de::Error::custom("JSON depth exceeded"));
            }
            let mut keys = std::collections::BTreeSet::new();
            while let Some(key) = object.next_key::<String>()? {
                if !keys.insert(key) {
                    return Err(de::Error::custom("duplicate JSON field"));
                }
                object.next_value_seed(Shape {
                    containers: self.containers + 1,
                })?;
            }
            Ok(())
        }
    }

    /// 只要顶层对象的严格 JSON 解析：上游
    /// `managed_task::declaration::parse_object::<Value>(bytes, limit)` 的同口径替换
    /// （AIGX 的 `managed_task` 没有 declaration 子模块）。
    fn parse_object(bytes: &[u8], limit: usize) -> Result<Value, WireError> {
        if bytes.len() > limit || std::str::from_utf8(bytes).is_err() {
            return Err(invalid());
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        Shape { containers: 0 }
            .deserialize(&mut deserializer)
            .map_err(|_| invalid())?;
        deserializer.end().map_err(|_| invalid())?;
        let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        Ok(value)
    }
}
