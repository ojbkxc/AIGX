//! 智谱清言（`chatglm.cn`）的**粘贴式登录**与本机登录态读取。
//!
//! ── 登录方式（上游表单 / account_store 揭示）─────────────────
//! 智谱清言没有可代跑的网页登录：登录态是浏览器 Cookie 里的 JWT
//! （`chatglm_token`）。网关能做的是：用户自己登录后把 JWT 粘进来（或导入
//! 本机桌面客户端的 Cookies 文件），网关**归一化**（剥掉 `Cookie:` / `Bearer `
//! / 引号等包装）、**校验形态**（三段 JWT）、解码载荷取展示名与续期令牌，
//! 通过后才落账号 —— 与 MonkeyCode / KukuAI 同一取舍：粘错东西在添加时就有
//! 明确报错，而不是等到转发时得到误导性的 401。
//!
//! ── 续期 ────────────────────────────────────────────────────
//! 令牌到期后可用 `refresh_token` 换新：`POST
//! /chatglm/user-api/user/refresh`（实测可用）。refresh_token 的来源：
//!   - 粘贴 JWT 时，从 JWT 载荷里读 `refresh_token` / `refreshToken` 声明；
//!   - 导入本机 Cookies 时，读刷新态 cookie（名字含 `refresh`）。
//!
//! 两者都读不出就**不落续期令牌**（账号照样能用，只是不会自动续期）——
//! 与上游把续期实现整体搬到插件、网关只留网关职责的口径一致。
//!
//! ── 与登录任务表的关系 ────────────────────────────────────
//! 本模块**不**创建 `LoginFlow` 任务（没有轮询/回调可言），直接返回
//! `Credentials`，由调用方走 `save_credentials`（chatglm 走带续期令牌的
//! 那一支）落账号。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use serde_json::Value;

use super::paste::normalize_token;
use super::Credentials;

/// 粘贴的凭证长度上限（超过这个长度一定是粘错了东西）
const MAX_SECRET_LENGTH: usize = 8192;

/// 智谱清言的访问令牌 cookie 名（`chatglm_token`）。
///
/// 仅在 Cookies 文件导入路径（`chatglm_cookie_from_sqlite_path`，sqlite-kv
/// feature 门控）里用；无 sqlite-kv 构建下不引用，故同样门控以避免 dead_code。
#[cfg(feature = "sqlite-kv")]
const CHATGLM_TOKEN_COOKIE: &str = "chatglm_token";

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

/// 解码 JWT 载荷（中间段，base64url 无填充）为 `Value`；不是合法 JWT 返回 `None`。
///
/// 与上游 `chatglm_accounts.rs` 的 `guest_claim` 同一口径：只读载荷、不解签。
fn decode_jwt_payload(token: &str) -> Option<Value> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let raw = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE_NO_PAD.decode(format!("{payload}==")))
        .ok()?;
    serde_json::from_slice(&raw).ok()
}

/// JWT 载荷的 `exp`（秒）；解不出返回 `None`。
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = decode_jwt_payload(token)?;
    payload.get("exp").and_then(Value::as_i64)
}

// ── 粘贴式登录 ───────────────────────────────────────────────────

/// 用粘贴的 JWT 校验智谱清言登录态，返回可直接落账号的凭证。
///
/// 校验口径：**三段 JWT 形态**（点分三段、每段可 base64url 解码）。不解签、
/// 不打电话 —— 上游把令牌探活搬到了插件，网关只做形态校验与载荷读取。
/// 读不出 JWT 载荷时退化为「非空 + 长度」校验（与 OrcaTerm 的 best-effort
/// 同款），避免把说明文字粘进来。
///
/// 续期令牌从载荷的 `refresh_token` / `refreshToken` 声明里读；读出就放进
/// `extra_headers["refresh_token"]`，由调用方取出后随 `refresh_secret` 落盘。
pub async fn verify_chatglm(
    _client: &reqwest::Client,
    raw_token: &str,
) -> Result<Credentials, String> {
    let token = normalize_token(raw_token);
    check_length(&token, "智谱清言 JWT")?;
    let is_jwt = {
        let segments: Vec<&str> = token.split('.').collect();
        segments.len() == 3 && segments.iter().all(|seg| !seg.is_empty())
    };
    if !is_jwt && token.chars().count() < 16 {
        return Err("智谱清言的令牌过短，请粘贴完整的 chatglm_token JWT".to_string());
    }
    let payload = decode_jwt_payload(&token);
    // 展示名：载荷里的用户标识；读不出就固定前缀
    let user_id = payload
        .as_ref()
        .map(|value| {
            text_of(
                value,
                &["userId", "user_id", "uid", "accountId", "username", "name"],
            )
        })
        .unwrap_or_default();
    let name = if user_id.is_empty() {
        "ChatGLM".to_string()
    } else {
        format!("ChatGLM {user_id}")
    };
    // 续期令牌：载荷声明里读；读不出就空（账号照样能用，只是不自动续期）
    let refresh_token = payload
        .as_ref()
        .map(|value| text_of(value, &["refresh_token", "refreshToken"]))
        .filter(|value| !value.is_empty());
    let mut extra_headers = HashMap::new();
    if let Some(refresh) = refresh_token {
        extra_headers.insert("refresh_token".to_string(), refresh);
    }
    let expires_at = jwt_exp(&token);
    Ok(Credentials {
        name,
        secret: token,
        base_url: None,
        extra_headers,
        expires_at,
    })
}

// ── 本机登录态读取（桌面客户端 Cookies 文件）────────────────────

/// 智谱清言桌面客户端 Cookies SQLite 文件（base64）→ (访问 JWT, 续期令牌)。
///
/// 移植自 KukuAI 的 `kuku_cookie_from_sqlite_b64` 同一读取套路：客户端
/// Cookies 文件不是 Chromium 加密存储，`encrypted_value` 为空，凭据全在明文
/// `value` 列，rusqlite 只读打开直读即可。读完即删临时副本。
///
/// 与上游的差异：上游接 `authFileB64` 落临时文件再读；这里同样先解 base64
/// 落临时文件（rusqlite 不接受内存库外打开），读完即删。读出的智谱域 Cookie
/// 里取 `chatglm_token`（访问 JWT）与刷新态 cookie（名字含 `refresh`）交给
/// `verify_chatglm` 走同一套校验/归一化，「解析」与「校验」共用一份口径。
#[cfg(feature = "sqlite-kv")]
pub fn chatglm_cookie_from_sqlite_b64(b64: &str) -> Result<(String, Option<String>), String> {
    use base64::Engine;

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
    let dir = std::env::temp_dir().join("aigx-chatglm-import");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建临时目录失败：{error}"))?;
    let path = dir.join(format!("cookies-{}", std::process::id()));
    std::fs::write(&path, &bytes).map_err(|error| format!("写入临时文件失败：{error}"))?;
    let result = chatglm_cookie_from_sqlite_path(&path);
    let _ = std::fs::remove_file(&path);
    result
}

/// 从指定路径的 Cookies SQLite 读出 (访问 JWT, 续期令牌)（见
/// [`chatglm_cookie_from_sqlite_b64`]；路径版供测试直接用真实文件）。
#[cfg(feature = "sqlite-kv")]
pub fn chatglm_cookie_from_sqlite_path(
    path: &std::path::Path,
) -> Result<(String, Option<String>), String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| {
        format!(
            "不是有效的智谱清言 Cookies 数据库（请选择客户端数据目录下 Network\\Cookies 文件）：{error}"
        )
    })?;
    // 读全部智谱域 cookie（不只 chatglm_token）：缺身份类 cookie 会让后续
    // 校验判「未登录」；统计类（Hm_* / HMACCOUNT）对认证无用，剔除。
    let mut stmt = conn
        .prepare(
            "SELECT name, value FROM cookies \
             WHERE host_key LIKE '%chatglm%' AND value != '' \
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
    let mut access = String::new();
    let mut refresh = String::new();
    for row in rows.flatten() {
        let (name, value) = row;
        let lower = name.to_ascii_lowercase();
        if lower == CHATGLM_TOKEN_COOKIE && access.is_empty() {
            access = value;
        } else if lower.contains("refresh") && refresh.is_empty() {
            refresh = value;
        }
    }
    if access.chars().count() < 8 {
        return Err(
            "文件里没有有效的 chatglm_token（请选择客户端登录后生成的 Network\\Cookies 文件）"
                .to_string(),
        );
    }
    Ok((
        access,
        if refresh.is_empty() {
            None
        } else {
            Some(refresh)
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_jwt_payload_reads_claims() {
        // {"is_guest":false,"refresh_token":"rt-1","exp":9999999999}
        let payload =
            "eyJpc19ndWVzdCI6ZmFsc2UsInJlZnJlc2hfdG9rZW4iOiJydC0xIiwiZXhwIjo5OTk5OTk5OTk5fQ";
        let token = format!("header.{payload}.sig");
        let value = decode_jwt_payload(&token).expect("应当解出载荷");
        assert_eq!(
            value.get("refresh_token").and_then(Value::as_str),
            Some("rt-1")
        );
        assert_eq!(jwt_exp(&token), Some(9999999999));
    }

    #[tokio::test]
    async fn verify_chatglm_extracts_refresh_from_jwt() {
        let payload = "eyJyZWZyZXNoX3Rva2VuIjoicnQtMSIsImV4cCI6OTk5OTk5OTk5OX0";
        let token = format!("h.{payload}.s");
        let creds = verify_chatglm(&reqwest::Client::new(), &token)
            .await
            .expect("JWT 应当通过校验");
        assert_eq!(creds.secret, token);
        assert_eq!(
            creds.extra_headers.get("refresh_token").map(String::as_str),
            Some("rt-1"),
            "续期令牌应当从载荷读出并放进 extra_fields"
        );
        assert_eq!(creds.expires_at, Some(9999999999));
    }

    #[tokio::test]
    async fn verify_chatglm_without_refresh_still_passes() {
        // 载荷里没有 refresh_token：账号照样落，只是不自动续期
        let payload = "eyJpc19ndWVzdCI6dHJ1ZX0";
        let token = format!("h.{payload}.s");
        let creds = verify_chatglm(&reqwest::Client::new(), &token)
            .await
            .expect("无续期令牌的 JWT 也应当通过");
        assert!(!creds.extra_headers.contains_key("refresh_token"));
        assert_eq!(creds.name, "ChatGLM", "读不出用户标识时用固定前缀");
    }

    #[tokio::test]
    async fn verify_chatglm_rejects_empty_and_huge() {
        assert!(verify_chatglm(&reqwest::Client::new(), "").await.is_err());
        assert!(
            verify_chatglm(&reqwest::Client::new(), &"a".repeat(MAX_SECRET_LENGTH + 1))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn verify_chatglm_rejects_too_short_non_jwt() {
        // 既不是三段 JWT、又过短：拒绝（避免把说明文字粘进来）
        assert!(verify_chatglm(&reqwest::Client::new(), "abc")
            .await
            .is_err());
    }

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn chatglm_sqlite_import_reads_token_and_refresh() {
        let dir = std::env::temp_dir().join(format!("aigx-chatglm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cookies");

        // ① 正常库：chatglm_token + 刷新态 cookie
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT);
             INSERT INTO cookies VALUES
               ('.chatglm.cn', 'chatglm_token', 'jwt-access-token-value'),
               ('.chatglm.cn', 'chatglm_refresh', 'refresh-token-value'),
               ('.chatglm.cn', 'Hm_lpvt_abc', 'stat'),
               ('.other.com', 'chatglm_token', 'not-chatglm'),
               ('.chatglm.cn', 'empty', '');",
        )
        .unwrap();
        drop(conn);
        let (access, refresh) = chatglm_cookie_from_sqlite_path(&path).unwrap();
        assert_eq!(access, "jwt-access-token-value");
        assert_eq!(refresh.as_deref(), Some("refresh-token-value"));
        let _ = std::fs::remove_file(&path);

        // ② 没有 chatglm_token 的库
        let path2 = dir.join("Cookies2");
        let conn = rusqlite::Connection::open(&path2).unwrap();
        conn.execute_batch(
            "CREATE TABLE cookies (host_key TEXT, name TEXT, value TEXT);
             INSERT INTO cookies VALUES ('.chatglm.cn', 'someOther', 'x');",
        )
        .unwrap();
        drop(conn);
        let error = chatglm_cookie_from_sqlite_path(&path2).unwrap_err();
        assert!(error.contains("chatglm_token"));
        let _ = std::fs::remove_file(&path2);

        // ③ 非 SQLite 文件
        let path3 = dir.join("Cookies3");
        std::fs::write(&path3, b"not a database").unwrap();
        assert!(chatglm_cookie_from_sqlite_path(&path3).is_err());
        let _ = std::fs::remove_file(&path3);
        let _ = std::fs::remove_dir(&dir);

        // ④ base64 版：空输入拒绝
        assert!(chatglm_cookie_from_sqlite_b64("").is_err());
    }
}
