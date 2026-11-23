// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 库库（kuku / baidugenflowpro）本机登录态读取器。
//!
//! ── 来源 ────────────────────────────────────────────────────────
//! 移植自上游 `plugin_host/wasm/desktop_credentials/kuku.rs`（v3.5.0-beta.7，
//! AGPL-3.0）。上游职责：只读打开库库官方客户端的 Chromium `Network/Cookies`
//! 库，把百度根路径登录 Cookie 投影给插件。本文件与该上游行为逐条对齐：
//!
//! 1. **固定路径**（常量 + 进程环境变量，非参数）：
//!    `app_data()/baidugenflowpro/Network/Cookies`，其次
//!    `%LOCALAPPDATA%/baidugenflowpro/Network/Cookies`（去重后取第一个存在的）。
//! 2. **固定查询**：`host_key` 为 `baidu.com` / `.baidu.com` / `*.baidu.com`、
//!    `path = '/'`、`name` 在 `BDUSS/BDUSS_BFESS/STOKEN/PTOKEN/PTOKEN_BFESS/
//!    BAIDUID/BAIDUID_BFESS/PANPSC/BIDUPSID/PSTM/PASS_STOKEN` 白名单内
//!    （`NOCASE`），`LIMIT 257` 用于在 256 条上限之外多取一条做溢出判定。
//! 3. **失败条件**：
//!    - 库文件不存在、打不开、查询失败；
//!    - 行数超过 256；
//!    - `host_key` 去掉前导 `.` 后不是 `baidu.com` 也不是其子域（防脏数据混入）；
//!    - 明文值超 8192 字节，或含 `;` / 非可见 ASCII（防 Cookie 注入拼接）；
//!    - 出现**同名不同值**的 Cookie（同一客户库里混了多个账号，无法判定用哪个）；
//!    - 只拿到加密值（`encrypted_value` 非空而 `value` 为空）：不静默丢登录材料，
//!      也**不调用系统解密**，直接如实报不可用；
//!    - 结果里没有明文 `BDUSS` / `BDUSS_BFESS`。
//!
//! ── 只读边界 ────────────────────────────────────────────────────
//! - 路径与 SQL 全部写死在本文件，**不接受任何调用参数**（接受路径/SQL 即等于
//!   任意文件读取与任意查询）。
//! - 只经父模块 [`super::database`] 以 `SQLITE_OPEN_READ_ONLY` 打开（含 `-wal` /
//!   `-shm` 的符号链接校验、64 MiB 上限），只 `SELECT`，不写库、不做 `VACUUM`。
//! - 返回形状与上游同形：`{"cookies":[...]}`（上游此处**不**套
//!   `{sourceId, sourceRevision, content}` 信封，各 provider 的插件侧按此解析）。

use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{absolute_env, app_data, database, invalid};

/// `desktop.credentials.read` 的库库分支：定位固定 Cookie 库 → 只读查询 → 投影。
pub(super) fn read() -> Result<Value, WireError> {
    let mut roots = Vec::new();
    if let Ok(base) = app_data() {
        roots.push(base.join("baidugenflowpro"));
    }
    if let Some(base) = absolute_env("LOCALAPPDATA") {
        let root = base.join("baidugenflowpro");
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    // 路径写死：只有 Browser 侧的 Network/Cookies，不接受插件指定。
    let path = roots
        .into_iter()
        .map(|root| root.join("Network/Cookies"))
        .find(|path| path.exists())
        .ok_or_else(invalid)?;
    let db = database(&path)?;
    parse(&db)
}

/// 从已只读打开的客户端库解析出百度根路径 Cookie 列表。
pub(super) fn parse(db: &rusqlite::Connection) -> Result<Value, WireError> {
    // 仅查询客户端协议需要的根路径百度 Cookie，不因库中其它站点条目挤掉登录态。
    let mut query = db.prepare("SELECT name, value, host_key, path, encrypted_value FROM cookies WHERE (host_key = 'baidu.com' OR host_key = '.baidu.com' OR host_key LIKE '%.baidu.com') AND path = '/' AND name COLLATE NOCASE IN ('BDUSS','BDUSS_BFESS','STOKEN','PTOKEN','PTOKEN_BFESS','BAIDUID','BAIDUID_BFESS','PANPSC','BIDUPSID','PSTM','PASS_STOKEN') LIMIT 257")
        .map_err(|_| invalid())?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })
        .map_err(|_| invalid())?;
    let mut cookies: Vec<Value> = Vec::new();
    let mut encrypted_only = false;
    for (index, row) in rows.enumerate() {
        // LIMIT 257：第 257 行说明白名单 Cookie 异常膨胀，按不可读处理。
        if index >= 256 {
            return Err(invalid());
        }
        let (name, value, domain, path, encrypted) = row.map_err(|_| invalid())?;
        let domain_check = domain.trim_start_matches('.').to_ascii_lowercase();
        if domain_check != "baidu.com" && !domain_check.ends_with(".baidu.com") {
            return Err(invalid());
        }
        if value.is_empty() {
            // 只拿到加密值：记为「只有密文」，绝不静默丢弃本次会话的登录材料。
            encrypted_only |= !encrypted.is_empty();
            continue;
        }
        if value.len() > 8192
            || !value
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte) && byte != b';')
        {
            return Err(invalid());
        }
        if let Some(previous) = cookies.iter().find(|cookie| {
            cookie["name"]
                .as_str()
                .is_some_and(|key| key.eq_ignore_ascii_case(&name))
        }) {
            if previous["value"] != value {
                return Err(WireError::with_code(
                    "库库客户端含不同账号的同名 Cookie，请在官方客户端重新登录后导入",
                    "desktop_credentials_unavailable",
                ));
            }
            continue;
        }
        if cookies.len() >= 128 {
            return Err(invalid());
        }
        cookies.push(json!({"name":name,"value":value,"domain":domain,"path":path}));
    }
    // 不能静默丢掉本次会话的加密材料；同名相同明文副本只去重，不调用系统解密。
    if encrypted_only {
        return Err(WireError::with_code(
            "库库客户端的 Cookie 已改为加密格式，请改用网页登录或粘贴 Cookie",
            "desktop_credentials_unavailable",
        ));
    }
    if !cookies.iter().any(|row| {
        row["name"].as_str().is_some_and(|name| {
            ["BDUSS", "BDUSS_BFESS"]
                .iter()
                .any(|key| name.eq_ignore_ascii_case(key))
        })
    }) {
        return Err(WireError::with_code(
            "库库官方客户端未提供有效的明文 BDUSS，请先登录并关闭客户端后重试",
            "desktop_credentials_unavailable",
        ));
    }
    Ok(json!({"cookies":cookies}))
}
