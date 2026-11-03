//! 粘贴式登录（校验 + 归一化）的几个家：MonkeyCode / KukuAI / OrcaTerm /
//! Antigravity（仅凭证校验）/ Command Code。
//!
//! 这些家**没有可代跑的网页登录**（登录态是浏览器 Cookie、或上游要图形验证
//! 码 / 微信扫码 / 自定义协议回调），网关能做的是：用户自己登录后把凭证粘进来，
//! 网关**归一化**（剥掉 `Cookie:` / `Bearer ` / 引号等包装）并**校验一次**
//! （打一个只读端点），通过后才落账号 —— 这样「粘错了东西」在添加时就有
//! 明确报错，而不是等到转发时得到误导性的 401。
//!
//! 参照 Agent2API 的 `monkeycode/login.rs` / `kuku/login.rs` / `orcaterm/login.rs`
//! 与 `commandcode/login.rs` 的归一化规则；AIGX 把「校验」放在这里，
//! 「网页登录」那一条仍由前端引导用户去官方站点完成。
//!
//! ── 与登录任务表的关系 ────────────────────────────────────
//! 本模块**不**创建 `LoginFlow` 任务（没有轮询/回调可言），而是直接返回
//! `Credentials`，由调用方走 `save_credentials` 落账号。因此 `start()` 抛出的
//! 可读错误会直接出现在「添加账号」弹窗里。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use serde_json::Value;

use super::Credentials;

/// 粘贴的凭证长度上限（超过这个长度一定是粘错了东西）
const MAX_SECRET_LENGTH: usize = 8192;

/// MonkeyCode 的 session cookie 名
const MONKEYCODE_COOKIE: &str = "monkeycode_ai_session";

/// 归一化「整段 Cookie / `name=value` / 裸值 / 带引号」形态，取指定 cookie 名
/// 的值；`cookie_name` 为空时按「整段 Cookie 原样保留」处理。
pub fn normalize_cookie(raw: &str, cookie_name: &str) -> String {
    let mut text = raw.trim();
    // 整行 `Cookie: ...` 头形态
    if let Some(rest) = text
        .strip_prefix("Cookie:")
        .or_else(|| text.strip_prefix("cookie:"))
    {
        text = rest.trim();
    }
    // `name=value` 形态：优先取指定 cookie 名的值
    if !cookie_name.is_empty() {
        for part in text.split(';') {
            let part = part.trim();
            if let Some(value) = part.strip_prefix(cookie_name) {
                if let Some(value) = value.strip_prefix('=') {
                    return value
                        .trim()
                        .trim_matches(|ch| ch == '"' || ch == '\'')
                        .chars()
                        .filter(|ch| !ch.is_whitespace())
                        .collect();
                }
            }
        }
        // 指定了 cookie 名但没找到：取第一个分号之前的内容
        if let Some((head, _)) = text.split_once(';') {
            text = head.trim();
        }
    }
    // cookie_name 为空 = 调用方要整段 Cookie（KukuAI 需要全部 cookie），原样保留
    text.trim_matches(|ch| ch == '"' || ch == '\'')
        .trim()
        .to_string()
}

/// 归一化 Bearer / 裸 token / 带引号形态
pub fn normalize_token(raw: &str) -> String {
    let mut text = raw.trim();
    for prefix in [
        "Authorization:",
        "authorization:",
        "Bearer ",
        "bearer ",
        "x-api-key:",
        "ot_session=",
    ] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    text.trim_matches(|ch| ch == '"' || ch == '\'')
        .trim()
        .to_string()
}

fn check_length(secret: &str, label: &str) -> Result<(), String> {
    if secret.is_empty() {
        return Err(format!("请粘贴 {label} 的凭证"));
    }
    if secret.chars().count() > MAX_SECRET_LENGTH {
        return Err(format!("{label} 的凭证过长，请确认粘贴的是正确的值"));
    }
    Ok(())
}

fn text_of(object: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = object.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

// ── MonkeyCode ──────────────────────────────────────────────────────

/// 用粘贴的 session 校验 MonkeyCode 登录态，返回可直接落账号的凭证。
///
/// 校验端点 `GET /api/v1/users/status`（200 + `code:0` 即已登录）。
/// `site` 为 `monkeycode-ai.com`（国内）或 `monkeycode-ai.net`（国际）。
pub async fn verify_monkeycode(
    client: &reqwest::Client,
    site: &str,
    raw_session: &str,
) -> Result<Credentials, String> {
    let session = normalize_cookie(raw_session, MONKEYCODE_COOKIE);
    check_length(&session, "MonkeyCode session")?;
    let base = if site.trim().is_empty() {
        "https://monkeycode-ai.com"
    } else {
        site.trim().trim_end_matches('/')
    };
    let url = format!("{base}/api/v1/users/status");
    let response = client
        .get(&url)
        .header("Cookie", format!("{MONKEYCODE_COOKIE}={session}"))
        .send()
        .await
        .map_err(|error| format!("MonkeyCode 登录态校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let code = payload.get("code").and_then(Value::as_i64);
    if !(200..300).contains(&status) || code != Some(0) {
        let upstream = text_of(&payload, &["message", "msg", "error"]);
        return Err(if upstream.is_empty() {
            format!(
                "MonkeyCode 登录态校验失败（HTTP {status}）：请确认 session 有效且站点选对（国内 .com / 国际 .net）"
            )
        } else {
            format!("MonkeyCode 登录态校验失败：{upstream}")
        });
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let user = data.get("user").cloned().unwrap_or_else(|| data.clone());
    let name = text_of(&user, &["name", "nickname", "display_name", "email"]);
    Ok(Credentials {
        name: if name.is_empty() {
            "MonkeyCode".to_string()
        } else {
            format!("MonkeyCode {name}")
        },
        secret: session,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── KukuAI ──────────────────────────────────────────────────────────

/// Cookies SQLite 文件（base64）→ 整段 Cookie 串（供 [`verify_kuku`] 校验）。
///
/// 移植自 Agent2API `kuku/credentials.rs` 的 `read_credentials_from_path`
/// （issue 同源：客户端 Cookies 文件不是 Chromium 加密存储，`encrypted_value`
/// 为空，凭据全在明文 `value` 列，rusqlite 只读打开直读即可，无需 DPAPI）。
///
/// 与上游的差异：上游接 `authFileB64` 落临时文件再读；这里同样先解 base64
/// 落临时文件（rusqlite 不接受内存库外打开），读完即删。读出的全部百度域
/// Cookie 拼成 `name=value; …` 串交给现有 `verify_kuku` 走同一套校验/归一化，
/// 「解析」与「校验」共用一份口径，不会分叉。
#[cfg(feature = "sqlite-kv")]
pub fn kuku_cookie_from_sqlite_b64(b64: &str) -> Result<String, String> {
    use base64::Engine;

    // 粘贴框的长度上限（8 KiB）对 base64 的 Cookies 文件不够（实测几 MB 级），
    // 单独给一个上限：40 MiB 的 base64 ≈ 30 MiB 文件，远超正常 Cookies 库。
    const MAX_B64_LENGTH: usize = 40 * 1024 * 1024;
    if b64.is_empty() {
        return Err("缺少 Cookies 文件内容（authFileB64）".to_string());
    }
    if b64.len() > MAX_B64_LENGTH {
        return Err("Cookies 文件过大，请确认选择的是 Network\\Cookies 文件".to_string());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|error| format!("Cookies 文件内容不是有效的 base64：{error}"))?;
    let dir = std::env::temp_dir().join("aigx-kuku-import");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建临时目录失败：{error}"))?;
    let path = dir.join(format!("cookies-{}", std::process::id()));
    // 写临时副本再读：临时副本不存在「被客户端占用」一说；读后即删。
    std::fs::write(&path, &bytes).map_err(|error| format!("写入临时文件失败：{error}"))?;
    let result = kuku_cookie_from_sqlite_path(&path);
    let _ = std::fs::remove_file(&path);
    result
}

/// 从指定路径的 Cookies SQLite 读出整段 Cookie 串（见
/// [`kuku_cookie_from_sqlite_b64`]；路径版供测试直接用真实文件）。
#[cfg(feature = "sqlite-kv")]
pub fn kuku_cookie_from_sqlite_path(path: &std::path::Path) -> Result<String, String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| {
        format!(
            "不是有效的 KukuAI Cookies 数据库（请选择 KukuAI 客户端数据目录下 Network\\Cookies 文件）：{error}"
        )
    })?;
    // 读全部百度域 cookie（不只 BDUSS/STOKEN）：缺 BAIDUID 等会被判「未登录」；
    // 统计类（Hm_* / HMACCOUNT）对认证无用，剔除。
    let mut stmt = conn
        .prepare(
            "SELECT name, value FROM cookies \
             WHERE host_key LIKE '%baidu%' AND value != '' \
               AND name NOT LIKE 'Hm\\_%' ESCAPE '\\' AND name != 'HMACCOUNT'",
        )
        .map_err(|error| {
            format!("Cookies 库里找不到 cookies 表（不是 Chromium Cookies 文件）：{error}")
        })?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| format!("读取 Cookies 库失败：{error}"))?;
    let mut bduss = String::new();
    let mut stoken = String::new();
    let mut extras: Vec<(String, String)> = Vec::new();
    for row in rows.flatten() {
        let (name, value) = row;
        if name.eq_ignore_ascii_case("BDUSS") && bduss.is_empty() {
            bduss = value;
        } else if name.eq_ignore_ascii_case("STOKEN") && stoken.is_empty() {
            stoken = value;
        } else if !name.eq_ignore_ascii_case("BDUSS")
            && !name.eq_ignore_ascii_case("STOKEN")
            && !name.eq_ignore_ascii_case("gfprotpl")
            && !extras
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(&name))
        {
            extras.push((name, value));
        }
    }
    // BDUSS 必填（百度通行证登录态，远长于 8；只拦明显残缺的值）
    if bduss.chars().count() < 8 {
        return Err(
            "文件里没有有效的 BDUSS（请选择 KukuAI 客户端登录后生成的 Network\\Cookies 文件）"
                .to_string(),
        );
    }
    let mut parts = vec![format!("BDUSS={bduss}")];
    if !stoken.is_empty() {
        parts.push(format!("STOKEN={stoken}"));
    }
    parts.extend(
        extras
            .into_iter()
            .map(|(name, value)| format!("{name}={value}")),
    );
    Ok(parts.join("; "))
}

/// 用粘贴的 Cookie 校验 KukuAI 登录态（BDUSS 必填）。
pub async fn verify_kuku(
    client: &reqwest::Client,
    raw_cookie: &str,
) -> Result<Credentials, String> {
    // KukuAI 要整段 Cookie（BDUSS / STOKEN / PTOKEN），不做按名提取
    let cookie = normalize_cookie(raw_cookie, "");
    check_length(&cookie, "KukuAI Cookie")?;
    if !cookie.contains("BDUSS=") {
        return Err("KukuAI 的 Cookie 缺少 BDUSS（请粘贴完整 Cookie）".to_string());
    }
    let url = "https://kuku.baidu.com/api/user/info";
    let response = client
        .get(url)
        .header("Cookie", cookie.clone())
        .send()
        .await
        .map_err(|error| format!("KukuAI 登录态校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    let payload: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let ok = (200..300).contains(&status)
        && (payload.get("errno").and_then(Value::as_i64) == Some(0)
            || payload.get("code").and_then(Value::as_i64) == Some(0)
            || payload.get("data").is_some());
    if !ok {
        let upstream = text_of(&payload, &["errmsg", "msg", "message"]);
        return Err(if upstream.is_empty() {
            format!("KukuAI 登录态校验失败（HTTP {status}）：Cookie 可能已过期")
        } else {
            format!("KukuAI 登录态校验失败：{upstream}")
        });
    }
    let data = payload.get("data").cloned().unwrap_or(Value::Null);
    let name = text_of(&data, &["uk", "name", "nickname", "user_name"]);
    Ok(Credentials {
        name: if name.is_empty() {
            "KukuAI".to_string()
        } else {
            format!("KukuAI {name}")
        },
        secret: cookie,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── OrcaTerm ────────────────────────────────────────────────────────

/// 用粘贴的 accessToken / ot_session 校验 OrcaTerm 登录态。
///
/// 上游没有公开的「校验」端点，这里用**一次最小请求**验证令牌形态：
/// 只做长度与非空校验，真正是否有效留给首次转发暴露（与 Agent2API
/// 的 best-effort 口径一致 —— 它也不在上游侧做前置校验）。
pub async fn verify_orcaterm(raw_token: &str) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "OrcaTerm accessToken")?;
    // OrcaTerm 的 JWT 是三段点分；ot_session 是不透明串。两种都收，
    // 但明显不是令牌形态的（极短）直接拒绝，避免把说明文字粘进来。
    if token.chars().count() < 16 {
        return Err("OrcaTerm 的令牌过短，请粘贴完整的 accessToken 或 ot_session".to_string());
    }
    Ok(Credentials {
        name: "OrcaTerm".to_string(),
        secret: token,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

// ── Command Code ────────────────────────────────────────────────────

/// 归一化并校验 Command Code 的 `user_` API Key（best-effort 探活）。
pub async fn verify_commandcode(
    client: &reqwest::Client,
    raw_key: &str,
) -> Result<Credentials, String> {
    let key = normalize_token(raw_key);
    check_length(&key, "Command Code API Key")?;
    if !key.starts_with("user_") {
        return Err("Command Code 的 API Key 应以 user_ 开头".to_string());
    }
    // 探活：GET /alpha/billing/credits，只有 401/403 才判无效
    let url = "https://api.commandcode.ai/alpha/billing/credits";
    let response = client
        .get(url)
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .map_err(|error| format!("Command Code 校验请求失败：{error}"))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(format!(
            "Command Code API Key 无效（HTTP {status}）：请到控制台重新生成"
        ));
    }
    Ok(Credentials {
        name: "Command Code".to_string(),
        secret: key,
        base_url: None,
        extra_headers: HashMap::new(),
        expires_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_cookie_extracts_named_value() {
        assert_eq!(
            normalize_cookie(
                "Cookie: a=1; monkeycode_ai_session=abc123; b=2",
                MONKEYCODE_COOKIE
            ),
            "abc123"
        );
        assert_eq!(
            normalize_cookie("monkeycode_ai_session=xyz", MONKEYCODE_COOKIE),
            "xyz"
        );
        assert_eq!(normalize_cookie("\"abc\"", MONKEYCODE_COOKIE), "abc");
    }

    #[test]
    fn normalize_cookie_keeps_full_cookie_when_no_name() {
        let full = "BDUSS=abc; STOKEN=def";
        assert_eq!(normalize_cookie(full, ""), full);
    }

    #[test]
    fn normalize_token_strips_prefixes() {
        assert_eq!(normalize_token("Bearer user_abc"), "user_abc");
        // `Authorization: Bearer x` 会被逐层剥成裸值
        assert_eq!(normalize_token("Authorization: Bearer x"), "x");
        assert_eq!(normalize_token("ot_session=abc"), "abc");
    }

    #[test]
    fn check_length_rejects_empty_and_huge() {
        assert!(check_length("", "x").is_err());
        assert!(check_length(&"a".repeat(MAX_SECRET_LENGTH + 1), "x").is_err());
        assert!(check_length("abc", "x").is_ok());
    }

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn kuku_sqlite_import_reads_baidu_cookies() {
        // 就地建一个最小 Cookies 库（与 Chromium 同 schema 关键列），验证：
        // 百度域 cookie 全读出、Hm_* 统计类剔除、BDUSS 缺失报错、非 SQLite 报错。
        let dir = std::env::temp_dir().join(format!("aigx-kuku-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cookies");

        // ① 正常库
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT);
             INSERT INTO cookies VALUES
               ('.baidu.com', 'BDUSS', 'bduss-value-xxxxxxxx'),
               ('.baidu.com', 'STOKEN', 'stoken-value'),
               ('.baidu.com', 'BAIDUID', 'baiduid-value'),
               ('.baidu.com', 'Hm_lpvt_abc', 'stat'),
               ('.baidu.com', 'HMACCOUNT', 'stat2'),
               ('.other.com', 'BDUSS', 'not-baidu'),
               ('.baidu.com', 'gfprotpl', 'genflowpro'),
               ('.baidu.com', 'empty', '');",
        )
        .unwrap();
        drop(conn);
        let cookie = kuku_cookie_from_sqlite_path(&path).unwrap();
        assert!(cookie.starts_with("BDUSS=bduss-value-xxxxxxxx"));
        assert!(cookie.contains("STOKEN=stoken-value"));
        assert!(cookie.contains("BAIDUID=baiduid-value"));
        assert!(!cookie.contains("Hm_"));
        assert!(!cookie.contains("HMACCOUNT"));
        assert!(!cookie.contains("not-baidu"));
        assert!(!cookie.contains("gfprotpl"));
        let _ = std::fs::remove_file(&path);

        // ② 没有 BDUSS 的库
        let path2 = dir.join("Cookies2");
        let conn = rusqlite::Connection::open(&path2).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT);
             INSERT INTO cookies VALUES ('.baidu.com', 'BAIDUID', 'x');",
        )
        .unwrap();
        drop(conn);
        let error = kuku_cookie_from_sqlite_path(&path2).unwrap_err();
        assert!(error.contains("BDUSS"));
        let _ = std::fs::remove_file(&path2);

        // ③ 非 SQLite 文件
        let path3 = dir.join("Cookies3");
        std::fs::write(&path3, b"not a database").unwrap();
        assert!(kuku_cookie_from_sqlite_path(&path3).is_err());
        let _ = std::fs::remove_file(&path3);
        let _ = std::fs::remove_dir(&dir);

        // ④ base64 版：空输入拒绝
        assert!(kuku_cookie_from_sqlite_b64("").is_err());
    }
}
