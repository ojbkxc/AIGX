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
}
