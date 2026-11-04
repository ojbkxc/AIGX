//! 账号导入器 —— 从本机其他 AI 客户端的配置里捞账号。
//!
//! ## 移植来源
//!
//! Agent2API `core/import_ccswitch.rs`（MIT 许可，见
//! `Agent2API-main/LICENSE`）。本模块是它的 AIGX 适配版：扫描逻辑与字段
//! 提取逐条对应，**落地方式不同** —— A2A 导入到「自定义提供商 + 账号」，
//! AIGX 导入到「账号型渠道 + 账号池账号」。
//!
//! ## 为什么读别人的数据库是安全的
//!
//! cc-switch 是本机桌面工具，数据在 `~/.cc-switch/cc-switch.db`。本模块
//! **只读**打开它（`SQLITE_OPEN_READ_ONLY`）—— cc-switch 可能正在运行，
//! 对它的库零写入（连 WAL 都不由我们触发）。读到的 API Key 不离开本机：
//! 导入动作本来就要把它写进 AIGX 的账号库。
//!
//! ## 口径
//!
//! - **不猜**：读不到、解析失败、字段缺失一律降级成「该条不可导入」并给出
//!   原因文案，绝不编造端点或凭证。
//! - **不静默丢弃**：cc-switch 里不支持的 app_type（Gemini CLI / OpenCode
//!   等）也列出来，标灰并写明原因 —— 让用户知道列表里少了谁。

use std::collections::HashSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// 可导入的 app_type（cc-switch `AppType::as_str()` 的实际取值）。
///
/// `claude-desktop` 与 `claude` 是**同一形状**的配置（都是 Anthropic 形态的
/// env map），故一并可导。
const SUPPORTED_TYPES: [&str; 3] = ["claude", "codex", "claude-desktop"];

/// 其余 app_type → 界面显示名（灰显行的「不支持」说明用）。
fn unsupported_label(app_type: &str) -> &'static str {
    match app_type {
        "gemini" => "Gemini CLI",
        "grokbuild" => "Grok Build",
        "opencode" => "OpenCode",
        "openclaw" => "OpenClaw",
        "hermes" => "Hermes",
        "pi" => "Pi",
        _ => "该客户端",
    }
}

/// 用户主目录（Windows 优先 USERPROFILE，与 Unix HOME 同序）。
fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// cc-switch 数据库路径（`~/.cc-switch/cc-switch.db`）。
///
/// 不复刻 cc-switch 的旧位置回退分支：默认位置读不到就如实报「未找到」——
/// 猜一个旧位置等于把另一个用户的配置静默导入。
fn cc_switch_db_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".cc-switch").join("cc-switch.db"))
}

/// JSON 字符串字段（缺失 / 非字符串 → 空串）。
fn string_field(value: Option<&Value>, key: &str) -> String {
    value
        .and_then(|item| item.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// 多个候选键里第一个非空字符串。
///
/// cc-switch 的 preset 会把键留成空串占位，所以必须「跳过空串」而不是
/// 「跳过缺失」。
fn first_non_empty(env: Option<&Value>, keys: &[&str]) -> String {
    let Some(env) = env else { return String::new() };
    for key in keys {
        let text = string_field(Some(env), key);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

/// Codex 的 base_url：从 TOML 文档里取**激活** provider 的地址。
///
/// 优先 `model_provider`（激活名）→ `model_providers.<名>.base_url`，回退
/// 顶层 `base_url`。非激活的 `[model_providers.*]` 段绝不读（遗留段不该泄进
/// 导入结果）。
///
/// 解析走 `toml::from_str::<toml::Table>`（文档级入口）。`toml::Value::from_str`
/// 在 toml 0.8/0.9 里语义是「单个内联值」，对文档级输入会报错。
fn extract_codex_base_url(config_text: &str) -> String {
    let Ok(doc) = toml::from_str::<toml::Table>(config_text) else {
        return String::new();
    };
    if let Some(active) = doc.get("model_provider").and_then(|v| v.as_str()) {
        if let Some(url) = doc
            .get("model_providers")
            .and_then(|providers| providers.get(active))
            .and_then(|provider| provider.get("base_url"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return url.to_string();
        }
    }
    doc.get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// Codex 的 API Key：`auth.OPENAI_API_KEY` 优先，回退 TOML 里激活 provider
/// （或顶层）的 `experimental_bearer_token`。
fn extract_codex_api_key(settings: &Value, config_text: &str) -> String {
    let from_auth = string_field(settings.get("auth"), "OPENAI_API_KEY");
    if !from_auth.is_empty() {
        return from_auth;
    }
    let Ok(doc) = toml::from_str::<toml::Table>(config_text) else {
        return String::new();
    };
    if let Some(active) = doc.get("model_provider").and_then(|v| v.as_str()) {
        let token = doc
            .get("model_providers")
            .and_then(|providers| providers.get(active))
            .and_then(|provider| provider.get("experimental_bearer_token"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("");
        if !token.is_empty() {
            return token.to_string();
        }
    }
    doc.get("experimental_bearer_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// Codex 的默认模型（config.toml 顶层 `model`）。
fn extract_codex_model(config_text: &str) -> String {
    let Ok(doc) = toml::from_str::<toml::Table>(config_text) else {
        return String::new();
    };
    doc.get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// Claude Code 的 `[1M]` 后缀是**本地能力标记**（声明 100 万上下文），
/// 上游一般不接受，转发前必须剥离。
///
/// 返回 `(上游模型名, 需要映射的客户端名)`：客户端名只在确实带标记时非空 ——
/// 客户端发过来的正是带标记的名字，没有 alias → 上游名这条映射会被拒掉。
fn split_one_m_marker(model: &str) -> (String, String) {
    const MARKER: &str = "[1M]";
    let trimmed = model.trim();
    if trimmed.len() > MARKER.len()
        && trimmed
            .to_ascii_lowercase()
            .ends_with(&MARKER.to_ascii_lowercase())
    {
        let clean = trimmed[..trimmed.len() - MARKER.len()]
            .trim_end()
            .to_string();
        if !clean.is_empty() {
            return (clean, trimmed.to_string());
        }
    }
    (trimmed.to_string(), String::new())
}

/// 一个 claude 类条目要登记的模型：(清单, 映射)。
///
/// 取值两个来源，顺序即优先级：
///   1. `ANTHROPIC_MODEL` —— 主模型；
///   2. `ANTHROPIC_DEFAULT_*_MODEL` —— 档位覆盖。
///
/// `*_MODEL_NAME` 一律不取：那是 Claude Code 模型菜单里的**显示名**，不是
/// 发给上游的 id。
///
/// 上限 6 个：这是一份「开箱能用」的起步清单，不是上游全量目录。
fn collect_claude_models(env: Option<&Value>) -> (Vec<String>, Vec<Value>) {
    const MAX_MODELS: usize = 6;
    let mut models: Vec<String> = Vec::new();
    let mut mappings: Vec<Value> = Vec::new();
    let push = |raw: &str, models: &mut Vec<String>, mappings: &mut Vec<Value>| {
        let (clean, alias) = split_one_m_marker(raw);
        if clean.is_empty() {
            return;
        }
        if !models
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&clean))
            && models.len() < MAX_MODELS
        {
            models.push(clean.clone());
        }
        if !alias.is_empty() {
            let known = mappings.iter().any(|item| {
                item.get("alias")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.eq_ignore_ascii_case(&alias))
            });
            if !known && mappings.len() < MAX_MODELS {
                mappings.push(json!({ "alias": alias, "target": clean }));
            }
        }
    };
    push(
        &string_field(env, "ANTHROPIC_MODEL"),
        &mut models,
        &mut mappings,
    );
    if let Some(obj) = env.and_then(Value::as_object) {
        for (key, value) in obj {
            if !key.starts_with("ANTHROPIC_DEFAULT_") || !key.ends_with("_MODEL") {
                continue;
            }
            if let Some(text) = value.as_str() {
                push(text, &mut models, &mut mappings);
            }
        }
    }
    (models, mappings)
}

/// 一条 cc-switch 供应商记录 → 扫描条目。
fn scan_entry(
    id: &str,
    app_type: &str,
    name: &str,
    notes: &str,
    settings: &Value,
    existing_names: &HashSet<String>,
) -> Value {
    let mut base_url = String::new();
    let mut api_key = String::new();
    let mut protocol = String::new();
    let mut models: Vec<String> = Vec::new();
    let mut model_mappings: Vec<Value> = Vec::new();

    match app_type {
        // Anthropic 形态：Claude Code 与 Claude Desktop 配置同形状
        "claude" | "claude-desktop" => {
            let env = settings.get("env");
            base_url = string_field(env, "ANTHROPIC_BASE_URL");
            api_key = first_non_empty(
                env,
                &[
                    "ANTHROPIC_AUTH_TOKEN",
                    "ANTHROPIC_API_KEY",
                    "OPENROUTER_API_KEY",
                    "GOOGLE_API_KEY",
                ],
            );
            protocol = "anthropic".to_string();
            (models, model_mappings) = collect_claude_models(env);
        }
        // Codex 形态：key 在 auth / TOML，base_url 在 TOML 字符串里
        "codex" => {
            let config_text = string_field(Some(settings), "config");
            base_url = extract_codex_base_url(&config_text);
            api_key = extract_codex_api_key(settings, &config_text);
            protocol = "openai_compatible".to_string();
            let model = extract_codex_model(&config_text);
            if !model.is_empty() {
                models.push(model);
            }
        }
        _ => {}
    }

    // 不可导入的原因（确定性排序：类型 → 端点 → 凭证 → 同名）
    let reason = if !SUPPORTED_TYPES.contains(&app_type) {
        format!(
            "cc-switch 的 {} 配置，暂不支持导入",
            unsupported_label(app_type)
        )
    } else if base_url.is_empty() {
        if app_type.starts_with("claude") {
            "官方直连配置，没有可导入的自定义端点".to_string()
        } else {
            "未配置自定义端点".to_string()
        }
    } else if api_key.is_empty() {
        "缺少 API Key".to_string()
    } else if existing_names.contains(name) {
        "本地已存在同名提供商，跳过".to_string()
    } else {
        String::new()
    };
    let importable = reason.is_empty();

    json!({
        "id": id,
        "appType": app_type,
        "name": name,
        "notes": notes,
        // 明文字段只对可导入的条目给出（灰显行不需要，少一处凭证流转）
        "baseUrl": if importable { base_url.as_str() } else { "" },
        "apiKey": if importable { api_key.as_str() } else { "" },
        "protocol": protocol,
        "models": if importable { models } else { Vec::new() },
        "modelMappings": if importable { model_mappings } else { Vec::new() },
        "importable": importable,
        "reason": reason,
    })
}

/// 扫描结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportScan {
    /// cc-switch 是否可用（装了、数据在、结构对）。
    pub available: bool,
    /// 数据库路径（便于用户确认扫的是哪个文件）。
    #[serde(default)]
    pub path: String,
    /// 不可用 / 部分失败的原因。
    #[serde(default)]
    pub reason: String,
    /// 扫描到的条目。
    #[serde(default)]
    pub providers: Vec<Value>,
    /// 条目总数。
    #[serde(default)]
    pub total: usize,
}

/// 扫描 cc-switch 数据库。**永不失败**：错误都在返回值里。
pub fn scan(existing_names: &HashSet<String>) -> ImportScan {
    let Some(path) = cc_switch_db_path() else {
        return ImportScan {
            available: false,
            path: String::new(),
            reason: "无法定位用户主目录，找不到 cc-switch 的数据文件".to_string(),
            providers: Vec::new(),
            total: 0,
        };
    };
    let path_text = path.to_string_lossy().to_string();
    if !path.exists() {
        return ImportScan {
            available: false,
            path: path_text,
            reason: "未找到 cc-switch 的数据库（~/.cc-switch/cc-switch.db）——请确认本机安装了 cc-switch，并在里面添加过供应商".to_string(),
            providers: Vec::new(),
            total: 0,
        };
    }

    #[cfg(feature = "sqlite-kv")]
    {
        use rusqlite::OpenFlags;

        let conn = match rusqlite::Connection::open_with_flags(
            &path,
            // 只读 + 不创建：cc-switch 可能正在运行，我们对它的库零写入
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(conn) => conn,
            Err(error) => {
                return ImportScan {
                    available: false,
                    path: path_text,
                    reason: format!("无法读取 cc-switch 数据库（可能正被占用）: {error}"),
                    providers: Vec::new(),
                    total: 0,
                };
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT id, app_type, name, settings_config, COALESCE(notes, '') FROM providers",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                return ImportScan {
                    available: false,
                    path: path_text,
                    reason: format!(
                        "cc-switch 数据库结构与预期不符（{error}）：可能是不受支持的旧版本"
                    ),
                    providers: Vec::new(),
                    total: 0,
                };
            }
        };

        let mut rows: Vec<(String, String, String, String, String)> = Vec::new();
        let mapped = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0).unwrap_or_default(),
                row.get::<_, String>(1).unwrap_or_default(),
                row.get::<_, String>(2).unwrap_or_default(),
                row.get::<_, String>(3).unwrap_or_default(),
                row.get::<_, String>(4).unwrap_or_default(),
            ))
        });
        let Ok(mapped) = mapped else {
            return ImportScan {
                available: false,
                path: path_text,
                reason: "读取 cc-switch 数据失败：数据库可能正被写入，稍后重试".to_string(),
                providers: Vec::new(),
                total: 0,
            };
        };
        for row in mapped.flatten() {
            rows.push(row);
        }

        let mut entries: Vec<Value> = Vec::new();
        for (id, app_type, name, settings_text, notes) in &rows {
            if name.trim().is_empty() {
                continue;
            }
            let settings: Value = serde_json::from_str(settings_text).unwrap_or(Value::Null);
            let settings = if settings.is_object() {
                settings
            } else {
                Value::Object(Map::new())
            };
            entries.push(scan_entry(
                id,
                app_type,
                name.trim(),
                notes.trim(),
                &settings,
                existing_names,
            ));
        }

        // 可导入的在前（claude → codex → 名称），不支持 / 跳过的在后按
        // app_type 归堆 —— 用户要勾的东西先看到。
        let rank = |entry: &Value| -> (usize, String, String) {
            let importable = entry.get("importable").and_then(Value::as_bool) == Some(true);
            (
                if importable { 0 } else { 1 },
                string_field(Some(entry), "appType"),
                string_field(Some(entry), "name").to_lowercase(),
            )
        };
        entries.sort_by_key(rank);

        ImportScan {
            available: true,
            path: path_text,
            reason: String::new(),
            total: entries.len(),
            providers: entries,
        }
    }
    #[cfg(not(feature = "sqlite-kv"))]
    {
        ImportScan {
            available: false,
            path: path_text,
            reason: "当前构建未启用 SQLite 后端，无法读取 cc-switch 数据库".to_string(),
            providers: Vec::new(),
            total: 0,
        }
    }
}

/// 一条待导入的条目（管理面勾选后提交）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportEntry {
    /// cc-switch 条目 id。
    pub id: String,
    /// 展示名（作为账号名）。
    pub name: String,
    /// 上游地址。
    pub base_url: String,
    /// API Key。
    pub api_key: String,
    /// 协议（anthropic / openai_compatible）。
    pub protocol: String,
    /// 初始模型清单。
    #[serde(default)]
    pub models: Vec<String>,
    /// 模型映射（alias → target）。
    #[serde(default)]
    pub model_mappings: Vec<Value>,
}

// ── Agent2API 账号库导入 ────────────────────────────────────────────────
//
// A2A 的账号库是 SQLite（`accounts(id, provider, priority, enabled,
// added_at, data)`，`data` 是整条扁平 JSON）。AIGX 账号池与其同源，
// 按本节的映射规则把记录搬成 `AccountSession`：
//
// - **幂等键**：A2A 记录 `id` 直接做 `AccountSession.id` —— 重复导入
//   命中同一条记录并更新（「跟着 A2A 账号更新」），不堆副本。
// - **通用键**：`accessToken` → `credential.secret`；`refreshToken` →
//   `refresh_secret`；`expiresAt`（秒/毫秒归一）→ `expires_at`。
// - **具名字段**：`registry::CREDENTIAL_FIELDS` 模板里的键 →
//   `credential.fields`；模板没列但记录里有的**字符串**字段也搬（宽容：
//   上游加了新字段而模板未跟上时不丢数据），通用键/非凭证键除外。
// - **officeace 兼容**：AK/SK/STS/project_id 同时拼 JSON 写进
//   `refresh_secret` —— 现有签到链路（`checkin::claim::
//   officeace_temp_creds`）从那里读；新代码应读 `fields`。

/// 通用键（AIGX 一等字段承担，不进 `fields`）。
const A2A_GENERIC_KEYS: [&str; 8] = [
    "accessToken",
    "access_token",
    "refreshToken",
    "refresh_token",
    "expiresAt",
    "expires_at",
    "userId",
    "user_id",
];

/// 非凭证键（记录的归属/元信息，AIGX `AccountSession` 一等字段承担）。
const A2A_NON_CREDENTIAL_KEYS: [&str; 7] = [
    "id", "provider", "priority", "enabled", "added_at", "addedAt", "baseUrl",
];

/// `credential.secret` 的候选键（按优先级）。
const A2A_SECRET_KEYS: [&str; 6] = [
    "accessToken",
    "access_token",
    "access_key_id",
    "modelAppKey",
    "apiKey",
    "token",
];

/// 从记录里取第一个非空字符串键。
fn a2a_first(record: &Map<String, Value>, keys: &[&str]) -> String {
    for key in keys {
        if let Some(text) = record.get(*key).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    String::new()
}

/// `expiresAt` 归一（秒/毫秒都认，归一到秒；非法值 → None）。
fn a2a_expires_at(record: &Map<String, Value>) -> Option<i64> {
    for key in ["expiresAt", "expires_at", "expires"] {
        let raw = record
            .get(key)
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.trim().parse().ok()));
        if let Some(ts) = raw {
            if ts <= 0 {
                continue;
            }
            // 毫秒时间戳（> 约 2001-09-09 的秒值 1e12）归一到秒
            return Some(if ts > 1_000_000_000_000 {
                ts / 1000
            } else {
                ts
            });
        }
    }
    None
}

/// 把一条 Agent2API 账号记录（`accounts.data` 的扁平 JSON）映射成
/// AIGX `AccountSession` 草稿。`id` / `provider` / 任何凭证都没有的记录
/// 返回 None（调用方计入 skipped）。
pub fn map_agent2api_record(
    channel_id: &str,
    record: &Value,
) -> Option<super::model::AccountSession> {
    use super::model::{AccountSession, Credential};

    let obj = record.as_object()?;
    let id = a2a_first(obj, &["id"]);
    let provider_raw = a2a_first(obj, &["provider"]);
    if id.is_empty() || provider_raw.is_empty() {
        return None;
    }
    let provider = super::registry::normalize_kind(&provider_raw);

    let secret = a2a_first(obj, &A2A_SECRET_KEYS);
    let refresh = a2a_first(obj, &["refreshToken", "refresh_token"]);
    // officeace 双键形态：网关凭据 modelAppKey 做 secret（签名主键），
    // modelAppSecret 进 fields。
    let has_any_credential = !secret.is_empty() || !refresh.is_empty();
    if !has_any_credential {
        return None;
    }

    let mut credential = Credential {
        kind: super::model::CREDENTIAL_KIND_SESSION.to_string(),
        secret,
        fields: std::collections::HashMap::new(),
    };

    // 具名字段：模板声明的键 + 记录里其余字符串键（宽容搬运，见模块注释）。
    let template = super::registry::credential_fields_for(&provider);
    for field in template {
        let value = a2a_first(obj, &[field.name]);
        if !value.is_empty() {
            credential.set_field(field.name, &value);
        }
    }
    for (key, value) in obj {
        let Some(text) = value.as_str() else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if A2A_GENERIC_KEYS.contains(&key.as_str())
            || A2A_NON_CREDENTIAL_KEYS.contains(&key.as_str())
            || A2A_SECRET_KEYS.contains(&key.as_str())
            || key == "refreshToken"
            || key == "refresh_token"
            || key == "name"
            || key == "nickname"
            || credential.fields.contains_key(key)
        {
            continue;
        }
        credential.set_field(key, text);
    }

    let mut session = AccountSession::new(channel_id, "");
    session.id = id.clone();
    session.provider = provider;
    session.name = {
        let name = a2a_first(obj, &["name", "nickname", "email"]);
        if name.is_empty() {
            let short: String = id.chars().take(8).collect();
            format!("{provider_raw}-{short}")
        } else {
            name
        }
    };
    // S5：导入的 base_url 会被网关主动请求（探活/转发），必须过 SSRF 校验。
    // 单条不合规 → 返回 None，由调用方计入 skipped，不影响其余记录。
    let base_url = a2a_first(obj, &["baseUrl", "base_url", "apiHost", "api_host"]);
    if !base_url.is_empty() {
        if let Err(rej) = crate::net_guard::validate_outbound_url(&base_url, false) {
            tracing::warn!(
                "agent2api import skipped record {}: base_url rejected: {}",
                id,
                rej.message()
            );
            return None;
        }
    }
    session.base_url = base_url;
    session.refresh_secret = refresh;
    session.expires_at = a2a_expires_at(obj);
    session.credential = credential;

    // officeace 兼容：AK/SK/STS/project_id 拼 JSON 进 refresh_secret
    // （现有签到链路从 refresh_secret JSON 读，见 claim::officeace_temp_creds）。
    if session.provider == "officeace" {
        let ak = session.credential.field_plain("accessKeyId");
        let sk = session.credential.field_plain("secretAccessKey");
        let sts = session.credential.field_plain("securityToken");
        let project = session.credential.field_plain("projectId");
        if !ak.is_empty() && !sk.is_empty() {
            session.refresh_secret = json!({
                "access_key_id": ak,
                "secret_access_key": sk,
                "security_token": sts,
                "project_id": project,
            })
            .to_string();
        }
    }
    Some(session)
}

/// Agent2API 导入结果统计。
#[derive(Debug, Clone, Default, Serialize)]
pub struct Agent2ApiImportReport {
    /// 新导入条数。
    pub imported: usize,
    /// 命中既有记录（同 id）被更新的条数。
    pub updated: usize,
    /// 跳过条数（缺 id/provider/凭证）。
    pub skipped: usize,
    /// 逐条错误（provider 未知等）。
    pub errors: Vec<String>,
    /// 读取来源（库路径或 "records"）。
    pub source: String,
}

/// 从 Agent2API 的 SQLite 账号库读出全部启用账号记录。
/// 只读打开（A2A 可能在运行，对它的库零写入）。
#[cfg(feature = "sqlite-kv")]
pub fn read_agent2api_db(path: &std::path::Path) -> Result<Vec<Value>, String> {
    use rusqlite::OpenFlags;

    let conn = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("无法打开 Agent2API 数据库 {}: {e}", path.display()))?;
    let mut stmt = conn
        .prepare("SELECT data FROM accounts WHERE enabled = 1")
        .map_err(|e| format!("Agent2API 数据库结构与预期不符（accounts 表）: {e}"))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| format!("读取 Agent2API 账号失败: {e}"))?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        match serde_json::from_str::<Value>(&row) {
            Ok(value) if value.is_object() => out.push(value),
            _ => continue,
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_m_marker_split() {
        let (clean, alias) = split_one_m_marker("claude-sonnet-4[1M]");
        assert_eq!(clean, "claude-sonnet-4");
        assert_eq!(alias, "claude-sonnet-4[1M]");
        let (clean, alias) = split_one_m_marker("gpt-5");
        assert_eq!(clean, "gpt-5");
        assert!(alias.is_empty());
    }

    #[test]
    fn codex_base_url_prefers_active_provider() {
        let text = r#"
model_provider = "active"
model = "gpt-5-codex"

[model_providers.active]
base_url = "https://active.example/v1"

[model_providers.stale]
base_url = "https://stale.example/v1"
"#;
        assert_eq!(
            extract_codex_base_url(text),
            "https://active.example/v1".to_string()
        );
    }

    #[test]
    fn codex_api_key_prefers_auth() {
        let settings = json!({"auth": {"OPENAI_API_KEY": "sk-auth"}});
        assert_eq!(extract_codex_api_key(&settings, ""), "sk-auth".to_string());
    }

    #[test]
    fn claude_models_collect() {
        let env = json!({
            "ANTHROPIC_MODEL": "claude-sonnet-4[1M]",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4"
        });
        let (models, mappings) = collect_claude_models(Some(&env));
        assert!(models.contains(&"claude-sonnet-4".to_string()));
        assert!(models.contains(&"claude-haiku-4".to_string()));
        assert_eq!(mappings.len(), 1);
    }

    #[test]
    fn agent2api_record_maps_trae_fields() {
        let record = json!({
            "id": "trae-solo-u1",
            "provider": "trae",
            "accessToken": "at-xxx",
            "refreshToken": "rt-yyy",
            "expiresAt": 1_900_000_000_000i64,
            "uid": "u1",
            "deviceId": "d-1",
            "machineId": "m-1",
            "variant": "solo",
            "nickname": "小明"
        });
        let session = map_agent2api_record("ch-1", &record).expect("trae 记录应可映射");
        assert_eq!(session.id, "trae-solo-u1");
        assert_eq!(session.provider, "trae");
        assert_eq!(session.name, "小明");
        assert_eq!(session.credential.secret, "at-xxx");
        assert_eq!(session.refresh_secret, "rt-yyy");
        // 毫秒时间戳归一到秒
        assert_eq!(session.expires_at, Some(1_900_000_000));
        assert_eq!(session.credential.field_plain("uid"), "u1");
        assert_eq!(session.credential.field_plain("deviceId"), "d-1");
        assert_eq!(session.credential.field_plain("machineId"), "m-1");
        assert_eq!(session.credential.field_plain("variant"), "solo");
    }

    #[test]
    fn agent2api_record_maps_raccoon_alias_and_unknown_string_fields() {
        let record = json!({
            "id": "racc-1",
            "provider": "raccoon",
            "accessToken": "at-zzz",
            "someFutureField": "ff-1"
        });
        let session = map_agent2api_record("ch-1", &record).expect("别名应归一");
        // raccoon → xiaohuanxiong（registry 别名归一）
        assert_eq!(session.provider, "xiaohuanxiong");
        // 模板没列的字符串字段宽容搬运（上游加新字段不丢数据）
        assert_eq!(session.credential.field_plain("someFutureField"), "ff-1");
    }

    #[test]
    fn agent2api_record_maps_officeace_dual_credentials() {
        let record = json!({
            "id": "oa-1",
            "provider": "officeace",
            "modelAppKey": "app-key",
            "modelAppSecret": "app-secret",
            "accessKeyId": "AK",
            "secretAccessKey": "SK",
            "securityToken": "STS",
            "projectId": "proj-1"
        });
        let session = map_agent2api_record("ch-1", &record).expect("officeace 记录应可映射");
        assert_eq!(session.credential.secret, "app-key");
        assert_eq!(
            session.credential.field_plain("modelAppSecret"),
            "app-secret"
        );
        assert_eq!(session.credential.field_plain("accessKeyId"), "AK");
        // 现有签到链路读 refresh_secret JSON —— 导入时同步拼好
        let refresh: Value = serde_json::from_str(&session.refresh_secret).unwrap();
        assert_eq!(refresh["access_key_id"], "AK");
        assert_eq!(refresh["secret_access_key"], "SK");
        assert_eq!(refresh["project_id"], "proj-1");
    }

    #[test]
    fn agent2api_record_skips_incomplete() {
        assert!(map_agent2api_record("ch-1", &json!({"provider": "trae"})).is_none());
        assert!(map_agent2api_record("ch-1", &json!({"id": "x", "provider": "trae"})).is_none());
        assert!(map_agent2api_record("ch-1", &json!("not-an-object")).is_none());
    }

    #[test]
    fn agent2api_record_skips_internal_base_url() {
        // S5：本机环回与云元数据地址都必须在导入阶段被 SSRF 守卫跳过。
        for base in ["http://127.0.0.1/x", "http://169.254.169.254/"] {
            let record = json!({
                "id": "ssrf-1",
                "provider": "trae",
                "sessionToken": "tok",
                "baseUrl": base
            });
            assert!(
                map_agent2api_record("ch-1", &record).is_none(),
                "internal base_url must be skipped: {base}"
            );
        }
    }
}
