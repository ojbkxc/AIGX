// Ported from Agent2API v3.5.0-beta.7 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! workbuddy / workbuddy-intl、orcaterm、cline、quwork 四个「本机客户端登录态」读取器。
//!
//! ── 来源 ────────────────────────────────────────────────────────────
//! 上游：Agent2API v3.5.0-beta.7
//! `plugin_host/wasm/desktop_credentials/snapshots.rs`（AGPL-3.0，
//! Copyright (c) 2026 aimod-cc）。本文件只做宿主侧移植：读哪些固定路径、投影
//! 哪些字段、什么条件下失败，全部与上游逐条对齐。
//!
//! ── 只读边界（与父模块 README §边界 同口径）────────────────────────
//! 1. **路径写死**：`%LOCALAPPDATA%/CodeBuddyExtension/Data/Public/auth`、
//!    `ORCATERM_DATA_FILE`、`~/.cline/data/settings/providers.json`、
//!    `%APPDATA%/qurwork-desktop/qurwork-desktop.sqlite`、
//!    `~/.qurwork-desktop-dsh/.dsh/.credentials.yaml` 全是常量；唯一随参数动的
//!    只有 workbuddy 的文件名前缀（workbuddy / workbuddy-intl 二选一），
//!    不接受任何外部路径、文件名或 SQL。
//! 2. **只读**：只做 `stat` + `read`；SQLite 由 [`super::database`] 以
//!    `SQLITE_OPEN_READ_ONLY` 打开，绝不写入、不建表、不迁移。
//! 3. **只投影白名单字段**：所有结果都过 [`super::project`]，绝不返回未筛选的
//!    嵌套 JSON（workbuddy 的 account/auth 分组、cline 的 metadata.userInfo、
//!    quwork 的凭据容器都是显式逐键挑选）。
//! 4. **失败形状统一**：[`super::invalid`]（`desktop_credentials_unavailable`），
//!    中文短句「本机客户端登录态不可读、未登录或格式不支持…」，与上游同义。
//!
//! ── 与上游的差异（如实声明）────────────────────────────────────────
//! 上游 quwork 用 `managed_task::declaration::parse_object`（严格 JSON：字节上限、
//! UTF-8、逐层拒重复键、容器深度受限、必须顶层对象）解析会话正文与内嵌凭据串。
//! AIGX 的 `managed_task` 没有 `declaration` 子模块，故本文件内联同口径的
//! [`parse_object`]（深度上限沿用上游 `types::MAX_JSON_DEPTH = 32`），行为不变。
//! 其余字段、上限、失败条件逐行照抄，未做裁剪。

use rusqlite::OptionalExtension;
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Value};

use crate::plugin_host::wasm::wire::WireError;

use super::{absolute_env, app_data, content, database, home, invalid, object, project};

pub(super) fn workbuddy(provider: &str) -> Result<Value, WireError> {
    #[cfg(windows)]
    let base = absolute_env("LOCALAPPDATA").ok_or_else(invalid)?;
    #[cfg(target_os = "macos")]
    let base = home()?.join("Library/Application Support");
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = home()?.join(".local/share");
    let root = base.join("CodeBuddyExtension/Data/Public/auth");
    let prefix = if provider == "workbuddy-intl" {
        "workbuddy-desktop-ai"
    } else {
        "workbuddy-desktop"
    };
    let current = root.join(format!("{prefix}.info"));
    let path = if current.exists() {
        current
    } else {
        let entries = std::fs::read_dir(&root)
            .map_err(|_| invalid())?
            .take(2049)
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|_| invalid())?;
        if entries.len() > 2048 {
            return Err(invalid());
        }
        let name_prefix = format!("{prefix}.");
        let mut files = entries
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&name_prefix) && name.ends_with(".info"))
            })
            .collect::<Vec<_>>();
        files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        files.into_iter().next().ok_or_else(invalid)?
    };
    let value = object(&path, 256 * 1024)?;
    let account_keys = [
        "uid",
        "nickname",
        "type",
        "enterpriseId",
        "enterpriseName",
        "departmentFullName",
        "oneidAccountId",
        "idp",
    ];
    let auth_keys = [
        "accessToken",
        "access_token",
        "token",
        "refreshToken",
        "refresh_token",
        "expiresAt",
        "expires_at",
        "refreshExpiresAt",
        "refresh_expires_at",
        "domain",
        "tokenType",
    ];
    let mut result = project(&value, &["edition", "endpoint", "prefixPath", "platform"]);
    for (group, keys) in [("account", &account_keys[..]), ("auth", &auth_keys[..])] {
        let mut fields = project(&value[group], keys);
        // 官方嵌套字段优先；混合导出只补缺值，不把其他账号或地区字段拼入。
        for key in keys {
            let aliases: &[&str] = match (group, *key) {
                ("auth", "accessToken" | "access_token" | "token") => {
                    &["accessToken", "access_token", "token"]
                }
                ("auth", "refreshToken" | "refresh_token") => &["refreshToken", "refresh_token"],
                ("auth", "expiresAt" | "expires_at") => &["expiresAt", "expires_at"],
                ("auth", "refreshExpiresAt" | "refresh_expires_at") => {
                    &["refreshExpiresAt", "refresh_expires_at"]
                }
                _ => std::slice::from_ref(key),
            };
            if aliases.iter().any(|alias| {
                value[group].get(*alias).is_some_and(|field| {
                    !field.is_null() && !field.as_str().is_some_and(|text| text.trim().is_empty())
                })
            }) {
                continue;
            }
            if let Some(field) = value.get(*key) {
                fields[*key] = field.clone();
            }
        }
        result[group] = fields;
    }
    content(&path, result)
}

pub(super) fn orcaterm() -> Result<Value, WireError> {
    let path = match absolute_env("ORCATERM_DATA_FILE") {
        Some(path) => path,
        None => {
            #[cfg(any(windows, target_os = "macos"))]
            let root = app_data()?;
            #[cfg(not(any(windows, target_os = "macos")))]
            let root = match absolute_env("XDG_DATA_HOME") {
                Some(path) => path,
                None => home()?.join(".local/share"),
            };
            root.join("com.orcaterm-desktop.app/data.bin")
        }
    };
    content(
        &path,
        project(
            &object(&path, 256 * 1024)?,
            &["oauth_access_token", "oauth_expires_at", "auth_identity"],
        ),
    )
}

pub(super) fn cline() -> Result<Value, WireError> {
    let path = home()?.join(".cline/data/settings/providers.json");
    let value = object(&path, 256 * 1024)?;
    let auth = &value["providers"]["cline"]["settings"]["auth"];
    if !auth.is_object() {
        return Err(invalid());
    }
    let mut auth = project(
        auth,
        &[
            "accessToken",
            "refreshToken",
            "expiresAt",
            "accountId",
            "metadata",
        ],
    );
    if auth["metadata"].is_object() {
        auth["metadata"] = json!({"userInfo":project(&auth["metadata"]["userInfo"], &["subject", "clineUserId", "email", "name", "firstName", "lastName"])});
    }
    let mut response = content(
        &path,
        json!({"providers":{"cline":{"settings":{"auth":auth}}}}),
    )?;
    response
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("sourceId");
    Ok(response)
}

pub(super) fn quwork() -> Result<Value, WireError> {
    let path = app_data()?.join("qurwork-desktop/qurwork-desktop.sqlite");
    if path.exists() {
        let db = database(&path)?;
        let row: Option<String> = db
            .query_row(
                "SELECT session_json FROM auth_session WHERE id = 1 AND schema_version = 2 AND length(session_json) <= 262144",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| invalid())?;
        let row = row.ok_or_else(invalid)?;
        let value = parse_object(row.as_bytes(), 256 * 1024)?;
        return content(&path, quwork_projection(&value, 0)?);
    }
    let path = home()?.join(".qurwork-desktop-dsh/.dsh/.credentials.yaml");
    let raw = super::file(&path, 256 * 1024)?;
    let raw = std::str::from_utf8(&raw).map_err(|_| invalid())?;
    let raw = raw
        .lines()
        .find_map(|line| line.trim().strip_prefix("QURIVS_LLM_API_KEY:"))
        .ok_or_else(invalid)?
        .trim();
    let token = if raw.starts_with('"') {
        serde_json::from_str::<String>(raw).map_err(|_| invalid())?
    } else {
        raw.trim_matches('\'').to_string()
    };
    content(&path, json!({"llmToken":token}))
}

// 保留原客户端的凭据容器语义，只投影本家字段，不返回未筛选的嵌套 JSON。
fn quwork_projection(value: &Value, depth: usize) -> Result<Value, WireError> {
    if depth > 6 || !value.is_object() || value.to_string().len() > 256 * 1024 {
        return Err(invalid());
    }
    let mut input = value;
    for key in ["pluginSecret", "secret", "session", "data"] {
        if input[key].is_object() {
            input = &input[key];
        }
    }
    let nested = input["llmToken"]
        .as_str()
        .filter(|raw| raw.trim().starts_with('{'));
    let mut result = json!({});
    if let Some(raw) = input["credentials"]
        .as_str()
        .or(nested)
        .filter(|raw| !raw.trim().is_empty())
    {
        let raw = raw.trim().trim_start_matches('\u{feff}').trim();
        let projected = if raw.starts_with('{') {
            let value = parse_object(raw.as_bytes(), 256 * 1024)?;
            quwork_projection(&value, depth + 1)?
        } else {
            let raw = raw
                .lines()
                .find_map(|line| line.trim().strip_prefix("QURIVS_LLM_API_KEY:"))
                .map(str::trim)
                .unwrap_or(raw);
            let token = if raw.starts_with('"') {
                serde_json::from_str::<String>(raw).map_err(|_| invalid())?
            } else {
                raw.trim_matches('\'').to_string()
            };
            json!({"llmToken": token})
        };
        // 保留内外层的令牌和到期优先规则，由插件原解析器归一。
        result["credentials"] = json!(serde_json::to_string(&projected).map_err(|_| invalid())?);
    }
    for (target, aliases) in [
        ("accessToken", &["accessToken", "access_token", "token"][..]),
        ("llmToken", &["llmToken", "llm_token", "apiKey", "key"][..]),
    ] {
        if target == "llmToken" && nested.is_some() {
            continue;
        }
        if let Some(raw) = aliases
            .iter()
            .find_map(|key| input[*key].as_str().filter(|raw| !raw.trim().is_empty()))
        {
            result[target] = json!(raw);
        }
    }
    for key in [
        "expiresAt",
        "expire",
        "updatedAt",
        "name",
        "deviceIdentifier",
        "deviceUuid",
    ] {
        if let Some(value) = input.get(key).filter(|value| {
            !value.is_null() && !value.as_str().is_some_and(|text| text.trim().is_empty())
        }) {
            result[key] = value.clone();
        }
    }
    let user = if input["user"].is_object() {
        Some(&input["user"])
    } else if input.get("uid").is_some() {
        Some(input)
    } else {
        None
    };
    if let Some(user) = user {
        result["user"] = project(
            user,
            &[
                "id",
                "uid",
                "userId",
                "username",
                "display_name",
                "displayName",
                "nickname",
                "phone",
            ],
        );
    }
    Ok(result)
}

/// 上游 `types::MAX_JSON_DEPTH`：JSON 容器（对象/数组）最大嵌套层数。
const MAX_JSON_DEPTH: usize = 32;

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
