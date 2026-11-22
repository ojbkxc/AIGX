// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 账号文件导入 —— `authFileContent` / `authFileB64` / `authFileName`。
//!
//! 对齐 A2A `api/accounts_export.rs::import_account_file` 的可观察行为：
//!
//! - `authFileContent`：文本形态（JSON 账号文件，或某家的凭证明文）；
//! - `authFileB64`：base64 形态（二进制文件也走它，例如浏览器 Cookies 的
//!   SQLite 库）；
//! - `authFileName`：仅用于错误文案与「按扩展名拒绝」的判断。
//!
//! ## 解析顺序
//!
//! 1. base64 解码 → 若 `SQLite format 3\0` 开头，按 Cookies 库读副本；
//! 2. 文本 → JSON：`format = agent2api-account|aigx-account` 的导出记录、
//!    `{secret,public}` 双段形态、`{accounts:[...]}` 批量文件、或整份对象
//!    视为凭证；
//! 3. 非 JSON 文本 = 单条 token（`{token: "..."}`）。
//!
//! Cookies SQLite 只读**副本**：原文件可能正被浏览器占用（WAL 锁），
//! 直接打开会得到 `database is locked`，把字节写到临时文件再 `READ_ONLY`
//! 打开是唯一稳的路子（与 A2A 的导入源同因）。

use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use serde_json::{json, Map, Value};

/// 账号文件格式标识（导出端写入、导入端校验）。
pub const ACCOUNT_FILE_FORMAT: &str = "agent2api-account";

/// 解析结果（直接喂给 `plugin_upsert_account_with_source_and_proxy`）。
#[derive(Debug, Clone)]
#[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
pub struct ParsedAuthFile {
    /// provider（导出记录里自带时与选择项核对过）。
    pub provider: String,
    pub name: String,
    /// 身份提示（uid / email / id，取不到为空）。
    pub identity: String,
    /// 不透明凭证对象。
    pub secret: Value,
    /// 公开视图补充（导出记录里的展示字段）。
    pub public: Value,
}

fn decode_b64(raw: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    if let Ok(bytes) = STANDARD.decode(cleaned.as_bytes()) {
        return Ok(bytes);
    }
    if let Ok(bytes) = STANDARD_NO_PAD.decode(cleaned.as_bytes()) {
        return Ok(bytes);
    }
    if let Ok(bytes) = URL_SAFE_NO_PAD.decode(cleaned.as_bytes()) {
        return Ok(bytes);
    }
    Err("authFileB64 不是合法的 base64".to_string())
}

fn is_sqlite(bytes: &[u8]) -> bool {
    bytes.starts_with(b"SQLite format 3\0")
}

/// 从 Cookies SQLite 库读出 `name=value; …`（忽略无法解密的 `encrypted_value`）。
#[cfg(feature = "sqlite-kv")]
fn cookies_from_sqlite(bytes: &[u8]) -> Result<String, String> {
    use rusqlite::OpenFlags;

    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("aigx-cookies-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("无法创建 Cookies 导入临时目录: {e}"))?;
    let path = dir.join("Cookies");
    std::fs::write(&path, bytes).map_err(|e| format!("无法写入 Cookies 副本: {e}"))?;
    let result = (|| -> Result<String, String> {
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| format!("Cookies 库打不开（副本已写入临时目录）: {e}"))?;
        let mut stmt = conn
            .prepare("SELECT name, value FROM cookies WHERE value IS NOT NULL AND value <> ''")
            .map_err(|e| format!("Cookies 库结构与预期不符（cookies 表）: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| format!("读取 Cookies 失败: {e}"))?;
        let mut pairs: Vec<String> = Vec::new();
        for row in rows.flatten() {
            let (name, value) = row;
            if name.trim().is_empty() {
                continue;
            }
            pairs.push(format!("{}={}", name.trim(), value));
        }
        // 同名 cookie 去重（不同 host 可能有同名项，后写覆盖）
        let mut seen = std::collections::HashSet::new();
        pairs.retain(|pair| {
            let name = pair.split('=').next().unwrap_or("");
            seen.insert(name.to_string())
        });
        if pairs.is_empty() {
            return Err("Cookies 库里没有可用的明文 cookie".to_string());
        }
        Ok(pairs.join("; "))
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

#[cfg(not(feature = "sqlite-kv"))]
fn cookies_from_sqlite(_bytes: &[u8]) -> Result<String, String> {
    Err("当前构建未启用 SQLite 支持，无法读取 Cookies 库".to_string())
}

fn provider_of_record(record: &Value) -> String {
    record
        .get("provider")
        .and_then(Value::as_str)
        .map(crate::account_session::registry::normalize_kind)
        .unwrap_or_default()
}

fn identity_of(value: &Value) -> String {
    for key in [
        "uid",
        "userId",
        "user_id",
        "accountId",
        "account_id",
        "email",
        "login",
        "username",
        "id",
    ] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return text.trim().to_string();
            }
        }
        if let Some(number) = value.get(key).and_then(Value::as_i64) {
            return number.to_string();
        }
    }
    String::new()
}

/// 解析一份账号文件；`provider` 是「添加账号」表单选中的家（可空 = 由文件决定）。
///
/// `script_provider`：该 provider 是否由**脚本插件**声明接管（对齐 A2A
/// `import_account_file` 的 `known_provider` 判定）。脚本插件家的原生导出
/// 账号文件（`format = agent2api-account|aigx-account` 且不含插件 `secret`
/// 双段）**不得**当原生账号文件解析 —— 那应交给插件自己的导入契约；这里明确
/// 拒绝，避免把别家的凭据错存成插件的 opaque secret。
pub fn parse_auth_file(
    provider: &str,
    name: &str,
    content: Option<&str>,
    b64: Option<&str>,
    file_name: Option<&str>,
    script_provider: bool,
) -> Result<ParsedAuthFile, String> {
    // 空 provider = 由文件自报（不要走 normalize_kind：它把空串归一成默认家，
    // 会让「文件自带 provider」的导入被误判成与默认家不一致）
    let trimmed = provider.trim();
    let selected = if trimmed.is_empty() {
        String::new()
    } else {
        crate::account_session::registry::normalize_kind(trimmed)
    };
    let mut text = content
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    if text.is_none() {
        if let Some(raw) = b64.map(str::trim).filter(|t| !t.is_empty()) {
            let bytes = decode_b64(raw)?;
            if is_sqlite(&bytes) {
                let cookies = cookies_from_sqlite(&bytes)?;
                if selected.is_empty() {
                    return Err("导入 Cookies 库时必须选择提供商".to_string());
                }
                return Ok(ParsedAuthFile {
                    provider: selected,
                    name: name.trim().to_string(),
                    identity: String::new(),
                    secret: json!({ "cookie": cookies }),
                    public: Value::Object(Map::new()),
                });
            }
            let decoded = String::from_utf8(bytes)
                .map_err(|_| "authFileB64 解码后既不是 SQLite 库也不是 UTF-8 文本".to_string())?;
            text = Some(decoded.trim().to_string());
        }
    }
    let Some(text) = text else {
        return Err("缺少账号文件内容（authFileContent / authFileB64）".to_string());
    };
    if text.is_empty() {
        return Err("账号文件内容为空".to_string());
    }

    let parsed: Option<Value> = if text.starts_with('{') || text.starts_with('[') {
        Some(serde_json::from_str(&text).map_err(|e| format!("账号文件不是合法 JSON: {e}"))?)
    } else {
        None
    };

    match parsed {
        Some(Value::Array(items)) => {
            // 数组形态 = 账号清单，取第一个匹配 provider 的条目
            let chosen = items
                .iter()
                .find(|item| selected.is_empty() || provider_of_record(item) == selected)
                .or_else(|| items.first())
                .ok_or_else(|| "账号文件里没有任何条目".to_string())?;
            parse_record(selected, name, chosen, script_provider)
        }
        Some(Value::Object(object)) => {
            // `{accounts: [...]}` 批量文件
            if let Some(Value::Array(items)) = object.get("accounts") {
                let chosen = items
                    .iter()
                    .find(|item| selected.is_empty() || provider_of_record(item) == selected)
                    .or_else(|| items.first())
                    .ok_or_else(|| "账号文件里没有任何条目".to_string())?;
                return parse_record(selected, name, chosen, script_provider);
            }
            parse_record(selected, name, &Value::Object(object), script_provider)
        }
        Some(_) => Err("账号文件顶层必须是对象或数组".to_string()),
        None => {
            // 纯文本 = 单条 token
            if selected.is_empty() {
                return Err("粘贴纯文本凭证时必须选择提供商".to_string());
            }
            Ok(ParsedAuthFile {
                provider: selected,
                name: name.trim().to_string(),
                identity: String::new(),
                secret: json!({ "token": text.trim() }),
                public: Value::Object(Map::new()),
            })
        }
    }
    .map_err(|error| {
        let label = file_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("authFile");
        format!("{label}: {error}")
    })
}

fn parse_record(
    selected: String,
    name: &str,
    record: &Value,
    script_provider: bool,
) -> Result<ParsedAuthFile, String> {
    let record_provider = provider_of_record(record);
    let format = record.get("format").and_then(Value::as_str);
    let is_account_file = matches!(format, Some(ACCOUNT_FILE_FORMAT) | Some("aigx-account"));
    // 对齐 A2A `import_account_file`：脚本插件 provider 不走原生账号文件导入。
    // 仅拦「纯原生导出记录」（没有插件 `secret` 双段）—— 带 `secret` 的导出是
    // 插件原生形态，仍按插件凭据解析（re-import 自身导出文件不失效）。
    if is_account_file && script_provider && !record.get("secret").is_some_and(Value::is_object) {
        return Err(
            "脚本插件 provider 不支持导入原生账号文件：请通过该插件的登录/导入流程添加账号"
                .to_string(),
        );
    }
    if !selected.is_empty() && !record_provider.is_empty() && record_provider != selected {
        return Err(format!(
            "账号文件的提供商（{record_provider}）与当前选择（{selected}）不一致，请选择对应的提供商后再导入"
        ));
    }
    let provider = if !selected.is_empty() {
        selected
    } else if !record_provider.is_empty() {
        record_provider
    } else {
        return Err("账号文件缺少 provider，且表单未选择提供商".to_string());
    };
    let merged_name = record
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| name.trim().to_string());

    // 双段形态 `{secret, public}` 优先（插件原生形态）
    let (secret, public) = if let Some(secret) = record.get("secret").filter(|v| v.is_object()) {
        (
            secret.clone(),
            record
                .get("public")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new())),
        )
    } else if is_account_file {
        // 导出记录：整份对象就是凭证载体，保留展示字段给公开投影
        let mut secret = Map::new();
        for key in [
            "accessToken",
            "refreshToken",
            "token",
            "apiKey",
            "api_key",
            "cookie",
            "cookies",
            "uid",
            "email",
            "accountId",
            "edition",
            "endpoint",
            "platform",
            "access_key_id",
            "secret_access_key",
            "security_token",
        ] {
            if let Some(value) = record.get(key) {
                if !value.is_null() {
                    secret.insert(key.to_string(), value.clone());
                }
            }
        }
        if let Some(Value::Object(extra)) = record.get("credential_fields") {
            for (key, value) in extra {
                secret.insert(key.clone(), value.clone());
            }
        }
        if secret.is_empty() {
            secret = record.clone().as_object().cloned().unwrap_or_default();
        }
        let mut public = Map::new();
        for key in ["uid", "email", "nickname", "name", "edition", "endpoint"] {
            if let Some(value) = record.get(key) {
                if !value.is_null() {
                    public.insert(key.to_string(), value.clone());
                }
            }
        }
        (Value::Object(secret), Value::Object(public))
    } else {
        // 单段：整份对象就是凭据（各家 auth 文件的常见形态）
        (record.clone(), Value::Object(Map::new()))
    };
    let identity = identity_of(&secret);
    Ok(ParsedAuthFile {
        provider,
        name: merged_name,
        identity,
        secret,
        public,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_double_section_file() {
        let file = json!({
            "provider": "kiro",
            "name": "kiro-1",
            "secret": { "accessToken": "tok", "uid": "u-1" },
            "public": { "uid": "u-1", "edition": "intl" }
        });
        let parsed =
            parse_auth_file("kiro", "", Some(&file.to_string()), None, None, false).unwrap();
        assert_eq!(parsed.provider, "kiro");
        assert_eq!(parsed.name, "kiro-1");
        assert_eq!(parsed.identity, "u-1");
        assert_eq!(parsed.secret["accessToken"], "tok");
        assert_eq!(parsed.public["edition"], "intl");
    }

    #[test]
    fn parses_exported_account_file() {
        let file = json!({
            "format": ACCOUNT_FILE_FORMAT,
            "id": "kiro-account-abc",
            "provider": "kiro",
            "name": "exported",
            "uid": "u-9",
            "accessToken": "tok-9"
        });
        let parsed = parse_auth_file(
            "",
            "",
            Some(&file.to_string()),
            None,
            Some("kiro.json"),
            false,
        )
        .unwrap();
        assert_eq!(parsed.provider, "kiro");
        assert_eq!(parsed.secret["accessToken"], "tok-9");
        assert_eq!(parsed.public["uid"], "u-9");
        assert_eq!(parsed.identity, "u-9");
    }

    #[test]
    fn script_provider_native_account_file_is_rejected() {
        // 纯原生导出记录（无插件 secret 双段）→ 脚本插件 provider 不得当原生导入。
        let file = json!({
            "format": ACCOUNT_FILE_FORMAT,
            "id": "kiro-account-abc",
            "provider": "kiro",
            "accessToken": "tok-9"
        });
        let error = parse_auth_file(
            "kiro",
            "",
            Some(&file.to_string()),
            None,
            Some("kiro.json"),
            true,
        )
        .unwrap_err();
        assert!(error.contains("脚本插件 provider"), "{error}");
        // 反向对照：带插件 secret 双段的自身导出仍可导入（不误伤 re-import）。
        let with_secret = json!({
            "format": ACCOUNT_FILE_FORMAT,
            "provider": "kiro",
            "secret": { "accessToken": "tok-9" },
            "public": { "uid": "u-9" }
        });
        let parsed =
            parse_auth_file("kiro", "", Some(&with_secret.to_string()), None, None, true).unwrap();
        assert_eq!(parsed.secret["accessToken"], "tok-9");
    }

    #[test]
    fn provider_mismatch_is_rejected() {
        let file = json!({ "provider": "kiro", "secret": { "token": "t" } });
        let error =
            parse_auth_file("astudio", "", Some(&file.to_string()), None, None, false).unwrap_err();
        assert!(error.contains("不一致"), "{error}");
    }

    #[test]
    fn base64_text_is_decoded() {
        let text = json!({ "provider": "kiro", "secret": { "token": "b64-token" } }).to_string();
        let encoded = STANDARD.encode(text.as_bytes());
        let parsed = parse_auth_file("kiro", "", None, Some(&encoded), None, false).unwrap();
        assert_eq!(parsed.secret["token"], "b64-token");
    }

    #[test]
    fn raw_text_becomes_token() {
        let parsed =
            parse_auth_file("kiro", "", Some("  plain-token  "), None, None, false).unwrap();
        assert_eq!(parsed.secret["token"], "plain-token");
    }

    #[test]
    #[cfg(feature = "sqlite-kv")]
    fn sqlite_cookies_file_is_read_as_copy() {
        // 造一个最小 Cookies 库：只建导入路径查询的列
        let dir = std::env::temp_dir().join(format!("aigx-cookie-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Cookies");
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE cookies (name TEXT, value TEXT);\
                 INSERT INTO cookies (name, value) VALUES ('sid', 'abc');\
                 INSERT INTO cookies (name, value) VALUES ('tok', 'xyz');",
            )
            .unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        let encoded = STANDARD.encode(&bytes);
        let parsed =
            parse_auth_file("kiro", "", None, Some(&encoded), Some("Cookies"), false).unwrap();
        let cookie = parsed.secret["cookie"].as_str().unwrap();
        assert!(cookie.contains("sid=abc"), "{cookie}");
        assert!(cookie.contains("tok=xyz"), "{cookie}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
