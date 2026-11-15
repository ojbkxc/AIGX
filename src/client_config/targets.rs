//! 配置目标定义、默认路径、字段校验，以及「记录 → 目标文件内容」的生成。
//!
//! 目标与路径**写死**（仅用环境变量做目录覆盖，不接受 HTTP 传入路径）：
//!
//! | id | 默认主文件 | 目录覆盖变量 |
//! |---|---|---|
//! | `claude-code` | `~/.claude/settings.json` | `CLAUDE_CONFIG_DIR` |
//! | `codex` | `~/.codex/config.toml` | `CODEX_HOME` |
//! | `claude-desktop` | 见 [`claude_desktop_config_path`] | `CLAUDE_DESKTOP_CONFIG`（完整文件） |
//! | `dsh` | `~/.dsh/settings.yaml` | `DSH_HOME` |
//!
//! DSH 的密钥**不进** `settings.yaml`：设置里只写 `apiKeyEnv` 引用名，值写进
//! 同目录 `.credentials.yaml` 的 `refs`（`version: 1` 布局），与 DSH 自身
//! 设置页的派生口径一致。

use std::path::PathBuf;

use serde_json::{json, Map, Value};

use super::edit::{self, ConfigFormat};
use super::store::Record;

/// 一个目标的配置文件类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// 主配置（settings.json / config.toml / settings.yaml）
    Config,
    /// DSH 的独立凭据文件（`.credentials.yaml`）
    Credentials,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Config => "config",
            FileKind::Credentials => "credentials",
        }
    }
}

/// 一个目标的配置文件（路径由目标决定）。
#[derive(Debug, Clone)]
pub struct TargetFile {
    pub kind: FileKind,
    pub path: PathBuf,
}

/// 支持的客户端目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    ClaudeCode,
    Codex,
    ClaudeDesktop,
    Dsh,
}

impl Target {
    pub const ALL: [Target; 4] = [
        Target::ClaudeCode,
        Target::Codex,
        Target::ClaudeDesktop,
        Target::Dsh,
    ];

    pub fn parse(id: &str) -> Option<Target> {
        match id.trim() {
            "claude-code" => Some(Target::ClaudeCode),
            "codex" => Some(Target::Codex),
            "claude-desktop" => Some(Target::ClaudeDesktop),
            "dsh" => Some(Target::Dsh),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Target::ClaudeCode => "claude-code",
            Target::Codex => "codex",
            Target::ClaudeDesktop => "claude-desktop",
            Target::Dsh => "dsh",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Target::ClaudeCode => "Claude Code",
            Target::Codex => "Codex",
            Target::ClaudeDesktop => "Claude Desktop",
            Target::Dsh => "DSH (DeepSeek Harness)",
        }
    }

    pub fn format(self) -> ConfigFormat {
        match self {
            Target::ClaudeCode | Target::ClaudeDesktop => ConfigFormat::Json,
            Target::Codex => ConfigFormat::Toml,
            Target::Dsh => ConfigFormat::Yaml,
        }
    }

    pub fn files(self) -> Vec<TargetFile> {
        match self {
            Target::ClaudeCode => vec![TargetFile {
                kind: FileKind::Config,
                path: claude_settings_path(),
            }],
            Target::Codex => vec![TargetFile {
                kind: FileKind::Config,
                path: codex_config_path(),
            }],
            Target::ClaudeDesktop => vec![TargetFile {
                kind: FileKind::Config,
                path: claude_desktop_config_path(),
            }],
            Target::Dsh => {
                let settings = dsh_settings_path();
                let credentials = match settings.parent() {
                    Some(dir) => dir.join(".credentials.yaml"),
                    None => PathBuf::from(".credentials.yaml"),
                };
                vec![
                    TargetFile {
                        kind: FileKind::Config,
                        path: settings,
                    },
                    TargetFile {
                        kind: FileKind::Credentials,
                        path: credentials,
                    },
                ]
            }
        }
    }

    /// 主配置文件的路径（前端展示与备份定位用）。
    pub fn config_path(self) -> PathBuf {
        self.files()
            .into_iter()
            .find(|file| file.kind == FileKind::Config)
            .map(|file| file.path)
            .unwrap_or_default()
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn env_file(key: &str) -> Option<PathBuf> {
    env_dir(key)
}

/// Claude Code：`$CLAUDE_CONFIG_DIR` 或 `~/.claude` 下的 `settings.json`。
pub fn claude_settings_path() -> PathBuf {
    env_dir("CLAUDE_CONFIG_DIR")
        .unwrap_or_else(|| home_dir().join(".claude"))
        .join("settings.json")
}

/// Codex：`$CODEX_HOME` 或 `~/.codex` 下的 `config.toml`。
pub fn codex_config_path() -> PathBuf {
    env_dir("CODEX_HOME")
        .unwrap_or_else(|| home_dir().join(".codex"))
        .join("config.toml")
}

/// Claude Desktop 的 `claude_desktop_config.json`。
///
/// `CLAUDE_DESKTOP_CONFIG` 可指定完整文件路径（覆盖平台默认位置）。
pub fn claude_desktop_config_path() -> PathBuf {
    if let Some(path) = env_file("CLAUDE_DESKTOP_CONFIG") {
        return path;
    }
    if cfg!(windows) {
        return env_dir("APPDATA")
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join("Claude")
            .join("claude_desktop_config.json");
    }
    if cfg!(target_os = "macos") {
        return home_dir()
            .join("Library/Application Support/Claude")
            .join("claude_desktop_config.json");
    }
    env_dir("XDG_CONFIG_HOME")
        .unwrap_or_else(|| home_dir().join(".config"))
        .join("Claude")
        .join("claude_desktop_config.json")
}

/// DSH：`$DSH_HOME` 或 `~/.dsh` 下的 `settings.yaml`。
pub fn dsh_settings_path() -> PathBuf {
    env_dir("DSH_HOME")
        .unwrap_or_else(|| home_dir().join(".dsh"))
        .join("settings.yaml")
}

/// 目标定义投影（`GET /api/admin/client-configs` 返回给前端渲染）。
pub fn target_definitions() -> Vec<Value> {
    Target::ALL
        .iter()
        .map(|target| {
            let files = target
                .files()
                .into_iter()
                .map(|file| {
                    json!({
                        "kind": file.kind.as_str(),
                        "path": file.path.display().to_string(),
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "id": target.id(),
                "label": target.label(),
                "format": target.format().as_str(),
                "configPath": target.config_path().display().to_string(),
                "files": files,
            })
        })
        .collect()
}

/// 一组记录字段（新增/更新共用的校验入口）。
pub fn validate_fields(
    name: &str,
    base_url: &str,
    api_key: &str,
    model: &str,
    models: &[Value],
    options: &Value,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err("请填写配置名称（不超过 200 字）".into());
    }
    if name.chars().any(char::is_control) {
        return Err("配置名称不能包含控制字符".into());
    }
    let url_text = base_url.trim();
    if url_text.is_empty() {
        return Err("请填写接口地址".into());
    }
    let url = url::Url::parse(url_text).map_err(|_| "接口地址必须是完整的 http/https 地址")?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err("接口地址必须是 http/https，且不能包含账号、密码、查询串或片段".into());
    }
    if api_key.contains(['\r', '\n']) {
        return Err("API Key 不能包含换行".into());
    }
    if api_key.len() > 16 * 1024 {
        return Err("API Key 过长".into());
    }
    if model.chars().count() > 400 || model.chars().any(char::is_control) {
        return Err("模型 ID 无效".into());
    }
    validate_models(models)?;
    validate_options(options)?;
    Ok(())
}

fn validate_models(models: &[Value]) -> Result<(), String> {
    if models.len() > 200 {
        return Err("模型最多 200 个".into());
    }
    for item in models {
        match item {
            Value::String(id) => validate_model_id(id)?,
            Value::Object(map) => {
                let id = map
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or("models 的对象条目必须包含 id")?;
                validate_model_id(id)?;
                if let Some(input) = map.get("input") {
                    let list = input
                        .as_array()
                        .ok_or("模型条目的 input 必须是数组（text / image）")?;
                    for value in list {
                        match value.as_str() {
                            Some("text" | "image") => {}
                            _ => return Err("模型条目的 input 只支持 text / image".into()),
                        }
                    }
                }
                if let Some(efforts) = map.get("reasoningEfforts") {
                    validate_reasoning_efforts(efforts)?;
                }
            }
            _ => return Err("models 的每一项必须是字符串或含 id 的对象".into()),
        }
    }
    Ok(())
}

fn validate_model_id(id: &str) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() || id.chars().count() > 400 || id.chars().any(char::is_control) {
        return Err("模型 ID 不能为空且不能包含控制字符".into());
    }
    Ok(())
}

const DSH_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

fn validate_reasoning_efforts(value: &Value) -> Result<(), String> {
    match value {
        Value::Bool(_) => Ok(()),
        Value::Object(map) => {
            for (key, wire) in map {
                if !DSH_LEVELS.contains(&key.as_str()) {
                    return Err(format!("DSH 不支持的思考档位：{key}"));
                }
                if wire.is_null() {
                    continue;
                }
                let wire = wire
                    .as_str()
                    .ok_or("reasoningEfforts 的值必须是字符串或 null")?;
                if !DSH_LEVELS.contains(&wire) {
                    return Err(format!("DSH 不支持的思考档位取值：{wire}"));
                }
            }
            Ok(())
        }
        _ => Err("reasoningEfforts 必须是布尔值或档位映射".into()),
    }
}

fn validate_options(options: &Value) -> Result<(), String> {
    let Some(map) = options.as_object() else {
        if options.is_null() {
            return Ok(());
        }
        return Err("options 必须是对象".into());
    };
    for (key, value) in map {
        match key.as_str() {
            "route" => {
                let route = value.as_str().ok_or("options.route 必须是字符串")?;
                route_from_options(options)?;
                if route.chars().count() > 64 {
                    return Err("options.route 过长（最多 64 字符）".into());
                }
            }
            "wireApi" => {
                let wire = value.as_str().ok_or("options.wireApi 必须是字符串")?;
                if !matches!(wire, "responses" | "chat") {
                    return Err("options.wireApi 只支持 responses / chat".into());
                }
            }
            "api" => {
                let api = value.as_str().ok_or("options.api 必须是字符串")?;
                if !matches!(
                    api,
                    "openai-completions" | "openai-responses" | "anthropic-messages"
                ) {
                    return Err(
                        "options.api 只支持 openai-completions / openai-responses / anthropic-messages"
                            .into(),
                    );
                }
            }
            "authField" => {
                let field = value.as_str().ok_or("options.authField 必须是字符串")?;
                if !matches!(field, "token" | "key") {
                    return Err("options.authField 只支持 token / key".into());
                }
            }
            "setDefault" | "requiresOpenaiAuth" => {
                if !value.is_boolean() {
                    return Err(format!("options.{key} 必须是布尔值"));
                }
            }
            "effort" => {
                let effort = value.as_str().ok_or("options.effort 必须是字符串")?;
                if effort.chars().count() > 32 || effort.chars().any(char::is_control) {
                    return Err("options.effort 无效".into());
                }
            }
            "envKey" => {
                let name = value.as_str().ok_or("options.envKey 必须是字符串")?;
                if !valid_env_name(name) {
                    return Err("options.envKey 不是合法的环境变量名".into());
                }
            }
            _ => return Err(format!("不支持的 options 字段：{key}")),
        }
    }
    Ok(())
}

pub fn valid_env_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn option_str<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record
        .options
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn option_bool(record: &Record, key: &str) -> Option<bool> {
    record.options.get(key).and_then(Value::as_bool)
}

/// 记录里的主模型：`model` 优先，否则取 `models` 的第一项 id。
pub fn effective_model(record: &Record) -> String {
    let model = record.model.trim();
    if !model.is_empty() {
        return model.to_string();
    }
    record
        .models
        .iter()
        .find_map(|item| match item {
            Value::String(id) => Some(id.trim().to_string()),
            Value::Object(map) => map
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .map(str::to_string),
            _ => None,
        })
        .filter(|id| !id.is_empty())
        .unwrap_or_default()
}

fn route_from_options(options: &Value) -> Result<String, String> {
    let raw = options
        .get("route")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return Ok("aigx".into());
    }
    if raw.chars().count() > 64
        || !raw
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err("options.route 只能包含字母、数字、- 和 _".into());
    }
    Ok(raw.to_ascii_lowercase())
}

fn route_of(record: &Record) -> Result<String, String> {
    route_from_options(&record.options)
}

/// DSH 的凭据引用名：`<路由大写、非字母数字转下划线>_API_KEY`（与 DSH 设置页一致）。
pub fn credential_ref(route: &str) -> String {
    let mut name = String::new();
    for ch in route.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch.to_ascii_uppercase());
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    format!("{}_API_KEY", name.trim_matches('_'))
}

#[derive(Debug, Clone, Default)]
pub struct CurrentFiles {
    pub config: String,
    pub credentials: String,
}

#[derive(Debug, Clone)]
pub struct Composed {
    pub config: String,
    pub credentials: Option<String>,
    pub warnings: Vec<String>,
}

/// 按目标生成新的文件内容（纯函数，不落盘）。
pub fn compose(
    target: Target,
    record: &Record,
    api_key: &str,
    current: &CurrentFiles,
) -> Result<Composed, String> {
    match target {
        Target::ClaudeCode => compose_claude(record, api_key, current, false),
        Target::ClaudeDesktop => compose_claude(record, api_key, current, true),
        Target::Codex => compose_codex(record, api_key, current),
        Target::Dsh => compose_dsh(record, api_key, current),
    }
}

fn compose_claude(
    record: &Record,
    api_key: &str,
    current: &CurrentFiles,
    desktop: bool,
) -> Result<Composed, String> {
    let mut value = edit::parse(ConfigFormat::Json, &current.config)?;
    if !value.is_object() {
        return Err("现有配置根不是对象，拒绝覆盖".into());
    }
    let base = record.base_url.trim().trim_end_matches('/').to_string();
    let model = effective_model(record);
    let effort = option_str(record, "effort").map(str::to_string);
    let auth_field = if option_str(record, "authField") == Some("key") {
        "ANTHROPIC_API_KEY"
    } else {
        "ANTHROPIC_AUTH_TOKEN"
    };
    {
        let object = value.as_object_mut().expect("object checked");
        let env_slot = object.entry("env").or_insert_with(|| json!({}));
        if !env_slot.is_object() {
            return Err("现有配置的 env 不是对象，拒绝覆盖".into());
        }
        let env = env_slot.as_object_mut().expect("object checked");
        env.insert("ANTHROPIC_BASE_URL".into(), json!(base));
        if !api_key.is_empty() {
            let other = if auth_field == "ANTHROPIC_API_KEY" {
                "ANTHROPIC_AUTH_TOKEN"
            } else {
                "ANTHROPIC_API_KEY"
            };
            env.insert(auth_field.to_string(), json!(api_key));
            env.insert(other.to_string(), json!(""));
        }
        if model.is_empty() {
            env.remove("ANTHROPIC_MODEL");
        } else {
            env.insert("ANTHROPIC_MODEL".into(), json!(model.clone()));
        }
        if !desktop {
            match effort.as_deref() {
                Some(effort) if effort != "default" => {
                    env.insert("CLAUDE_CODE_EFFORT_LEVEL".into(), json!(effort));
                }
                _ => {
                    env.remove("CLAUDE_CODE_EFFORT_LEVEL");
                }
            }
        }
    }
    if !desktop {
        let object = value.as_object_mut().expect("object checked");
        if model.is_empty() {
            object.remove("model");
        } else {
            object.insert("model".into(), json!(model));
        }
    }
    let after = edit::render(ConfigFormat::Json, &current.config, &value)?;
    let mut warnings = vec![if desktop {
        "只更新 claude_desktop_config.json 的 env；mcpServers 等其余设置原样保留。".to_string()
    } else {
        "保留 hooks / MCP / 权限等其余设置，只更新 env 中的接入字段。".to_string()
    }];
    if desktop {
        warnings.push(
            "Claude Desktop 并不保证读取 env 中的自定义端点；写入前请确认你的客户端版本支持。"
                .to_string(),
        );
    }
    Ok(Composed {
        config: after,
        credentials: None,
        warnings,
    })
}

fn compose_codex(
    record: &Record,
    api_key: &str,
    current: &CurrentFiles,
) -> Result<Composed, String> {
    let mut value = edit::parse(ConfigFormat::Toml, &current.config)?;
    if !value.is_object() {
        return Err("现有 config.toml 根不是表，拒绝覆盖".into());
    }
    let route = route_of(record)?;
    let base = record.base_url.trim().trim_end_matches('/').to_string();
    let model = effective_model(record);
    let wire_api = option_str(record, "wireApi").unwrap_or("responses").to_string();
    let requires_auth = option_bool(record, "requiresOpenaiAuth").unwrap_or(false);
    let env_key = option_str(record, "envKey").map(str::to_string);
    let effort = option_str(record, "effort").map(str::to_string);
    let profile = value
        .get("profile")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string);

    let existing_entry = value
        .get("model_providers")
        .and_then(|providers| providers.get(&route))
        .cloned();
    let object = value.as_object_mut().expect("object checked");
    let providers = child_object(object, "model_providers")?;
    let mut entry = Map::new();
    entry.insert("name".into(), json!(record.name.trim()));
    entry.insert("base_url".into(), json!(base));
    entry.insert("wire_api".into(), json!(wire_api));
    entry.insert("requires_openai_auth".into(), json!(requires_auth));
    if !api_key.is_empty() {
        entry.insert("experimental_bearer_token".into(), json!(api_key));
    } else if let Some(old) = existing_entry.as_ref().and_then(Value::as_object) {
        // 未提供新 Key 时沿用同路由已有的凭据字段，避免「切换配置」把 Key 抹掉
        for key in ["experimental_bearer_token", "env_key"] {
            if let Some(item) = old.get(key) {
                entry.insert(key.into(), item.clone());
            }
        }
    }
    if let Some(env_key) = env_key {
        entry.remove("experimental_bearer_token");
        entry.insert("env_key".into(), json!(env_key));
    }
    providers.insert(route.clone(), Value::Object(entry));

    let object = value.as_object_mut().expect("object checked");
    object.insert("model_provider".into(), json!(route));
    if !model.is_empty() {
        object.insert("model".into(), json!(model));
    }
    match effort.as_deref() {
        Some(effort) if effort != "default" => {
            object.insert("model_reasoning_effort".into(), json!(effort));
        }
        Some(_) => {
            object.remove("model_reasoning_effort");
        }
        None => {}
    }

    let mut warnings = vec![
        "使用 toml_edit 原位修改 config.toml：注释、键顺序与空白保留。".to_string(),
    ];
    if let Some(profile) = profile {
        let mut mirrored = false;
        // 只在 profiles 表已存在时镜像；缺席时凭空创建一个空 [profiles]
        // 会改变用户文件形状。
        if let Some(Value::Object(profiles)) = object.get_mut("profiles") {
            if let Some(Value::Object(table)) = profiles.get_mut(&profile) {
                table.insert("model_provider".into(), json!(route));
                if !model.is_empty() {
                    table.insert("model".into(), json!(model));
                }
                if let Some(effort) = effort.as_deref() {
                    if effort == "default" {
                        table.remove("model_reasoning_effort");
                    } else {
                        table.insert("model_reasoning_effort".into(), json!(effort));
                    }
                }
                mirrored = true;
            }
        }
        if mirrored {
            warnings.push(format!(
                "config.toml 当前选择 profile「{profile}」；已同时写入该 profile 与顶层。"
            ));
        } else {
            warnings.push(format!(
                "config.toml 指定了 profile「{profile}」但找不到对应表；本次只写顶层（不带 --profile 时生效）。"
            ));
        }
    }
    let after = edit::render(ConfigFormat::Toml, &current.config, &value)?;
    Ok(Composed {
        config: after,
        credentials: None,
        warnings,
    })
}

fn child_object<'a>(
    object: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, String> {
    if !object.contains_key(key) {
        object.insert(key.to_string(), json!({}));
    }
    match object.get_mut(key).expect("key just ensured") {
        Value::Object(child) => Ok(child),
        _ => Err(format!("现有配置的 {key} 不是表，拒绝覆盖")),
    }
}

const DSH_NAMESPACE: &str = "llm-pi-ai";
const DSH_DEFAULT_MODEL: &str = "agent-default-model";

fn compose_dsh(
    record: &Record,
    api_key: &str,
    current: &CurrentFiles,
) -> Result<Composed, String> {
    let mut value = edit::parse(ConfigFormat::Yaml, &current.config)?;
    if !value.is_object() {
        return Err("现有 settings.yaml 根不是映射，拒绝覆盖".into());
    }
    let route = route_of(record)?;
    let base = record.base_url.trim().trim_end_matches('/').to_string();
    let api = option_str(record, "api")
        .unwrap_or("openai-completions")
        .to_string();
    let model = effective_model(record);
    let set_default = option_bool(record, "setDefault").unwrap_or(true);
    let models = dsh_models(record)?;
    let reference = credential_ref(&route);

    let object = value.as_object_mut().expect("object checked");
    let namespace = child_object(object, DSH_NAMESPACE)?;
    let providers = child_object(namespace, "providers")?;
    let existing = providers
        .get(&route)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut entry = existing;
    entry.insert("displayName".into(), json!(record.name.trim()));
    entry.insert("api".into(), json!(api));
    entry.insert("baseURL".into(), json!(base));
    if !api_key.is_empty() {
        // settings.yaml 只留引用名，密钥本体写 .credentials.yaml
        entry.insert("apiKeyEnv".into(), json!(reference));
    }
    entry.insert("models".into(), Value::Array(models));
    providers.insert(route.clone(), Value::Object(entry));

    let mut warnings = vec![
        "DSH 设置与凭据分两个文件：settings.yaml 只写 provider 定义（含 apiKeyEnv 引用名）。"
            .to_string(),
    ];
    if set_default {
        if model.is_empty() {
            warnings.push("未填写模型：settings.yaml 的默认模型选择未改动。".into());
        } else {
            let object = value.as_object_mut().expect("object checked");
            let default = child_object(object, DSH_DEFAULT_MODEL)?;
            default.insert("provider".into(), json!(route));
            default.insert("model".into(), json!(model));
        }
    }

    let config = edit::render(ConfigFormat::Yaml, &current.config, &value)?;
    let credentials = if api_key.is_empty() {
        None
    } else {
        warnings.push(format!(
            "密钥只写入 .credentials.yaml 的 refs.{reference}，settings.yaml 不含明文。"
        ));
        Some(compose_dsh_credentials(
            &current.credentials,
            &route,
            api_key,
        )?)
    };
    if api_key.is_empty() {
        warnings.push("本次未提供 API Key：settings.yaml 沿用已有 apiKeyEnv 引用，凭据文件未改动。".into());
    }
    Ok(Composed {
        config,
        credentials,
        warnings,
    })
}

fn dsh_models(record: &Record) -> Result<Vec<Value>, String> {
    record
        .models
        .iter()
        .map(|item| match item {
            Value::String(id) => Ok(json!({ "id": id.trim() })),
            Value::Object(map) => {
                let id = map
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .ok_or("模型条目缺少 id")?;
                let mut entry = Map::new();
                entry.insert("id".into(), json!(id));
                for key in [
                    "name",
                    "contextWindow",
                    "maxTokens",
                    "input",
                    "reasoningEfforts",
                ] {
                    if let Some(value) = map.get(key) {
                        entry.insert(key.to_string(), value.clone());
                    }
                }
                Ok(Value::Object(entry))
            }
            _ => Err("模型条目必须是字符串或含 id 的对象".into()),
        })
        .collect()
}

fn compose_dsh_credentials(current: &str, route: &str, api_key: &str) -> Result<String, String> {
    let mut value = edit::parse(ConfigFormat::Yaml, current)?;
    if !value.is_object() {
        return Err("现有 .credentials.yaml 根不是映射，拒绝覆盖".into());
    }
    validate_dsh_credentials_value(&value)?;
    let object = value.as_object_mut().expect("object checked");
    if object.is_empty() {
        object.insert("version".into(), json!(1));
    }
    let refs = child_object(object, "refs")?;
    refs.insert(credential_ref(route), json!(api_key));
    edit::render(ConfigFormat::Yaml, current, &value)
}

fn validate_dsh_credentials_value(value: &Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("DSH 凭据文件根必须是映射")?;
    if object.is_empty() {
        return Ok(());
    }
    match object.get("version").and_then(Value::as_u64) {
        Some(1) => {}
        Some(version) => {
            return Err(format!(
                "DSH 凭据文件的 version 是 {version}，本页只认 version 1 布局"
            ))
        }
        None => {
            return Err(
                "DSH 凭据文件缺少 version（旧扁平布局）：先启动一次 DSH 让它自行迁移，再回到本页接入"
                    .into(),
            )
        }
    }
    for key in object.keys() {
        if !matches!(key.as_str(), "version" | "refs" | "records") {
            return Err(format!(
                "DSH 凭据文件含有未知顶层字段 {key}，DSH 自身也会拒绝读取，请先处理"
            ));
        }
    }
    Ok(())
}

/// 校验 DSH 凭据文本（恢复备份前用；空文本视为「可恢复为空文件」）。
pub fn validate_dsh_credentials(text: &str) -> Result<(), String> {
    let value = edit::parse(ConfigFormat::Yaml, text)?;
    validate_dsh_credentials_value(&value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(target: &str) -> Record {
        Record {
            id: "cfg-1".into(),
            target: target.into(),
            name: "主线路".into(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            base_url: "https://gateway.example.com".into(),
            api_key: String::new(),
            model: "claude-sonnet-4-6".into(),
            models: vec![json!("claude-sonnet-4-6")],
            options: json!({}),
            note: String::new(),
            last_applied_at: 0,
        }
    }

    #[test]
    fn claude_compose_writes_env_and_preserves_hooks() {
        let current = CurrentFiles {
            config: r#"{"hooks":{"Stop":[]},"env":{"OLD":"1"}}"#.into(),
            credentials: String::new(),
        };
        let composed = compose(Target::ClaudeCode, &record("claude-code"), "sk-1", &current).unwrap();
        let value: Value = serde_json::from_str(&composed.config).unwrap();
        assert_eq!(value["hooks"]["Stop"], json!([]));
        assert_eq!(value["env"]["ANTHROPIC_BASE_URL"], json!("https://gateway.example.com"));
        assert_eq!(value["env"]["ANTHROPIC_AUTH_TOKEN"], json!("sk-1"));
        assert_eq!(value["env"]["ANTHROPIC_MODEL"], json!("claude-sonnet-4-6"));
        assert!(composed.credentials.is_none());
    }

    #[test]
    fn codex_compose_keeps_comments_and_sets_provider() {
        let current = CurrentFiles {
            config: "# 我的注释\nmodel = \"old\"\n".into(),
            credentials: String::new(),
        };
        let composed = compose(Target::Codex, &record("codex"), "sk-2", &current).unwrap();
        assert!(composed.config.contains("# 我的注释"));
        assert!(composed.config.contains("[model_providers.aigx]"));
        assert!(composed.config.contains("experimental_bearer_token = \"sk-2\""));
        assert!(composed.config.contains("model_provider = \"aigx\""));
    }

    #[test]
    fn dsh_key_only_in_credentials_file() {
        let current = CurrentFiles {
            config: "model: keep-me\n".into(),
            credentials: "version: 1\nrefs: {}\n".into(),
        };
        let composed = compose(Target::Dsh, &record("dsh"), "sk-dsh", &current).unwrap();
        assert!(!composed.config.contains("sk-dsh"), "settings.yaml 泄漏密钥");
        assert!(!composed.config.contains("experimental"));
        assert!(composed.config.contains("apiKeyEnv: AIGX_API_KEY"));
        let credentials = composed.credentials.expect("凭据文件应被写入");
        assert!(credentials.contains("sk-dsh"));
        assert!(credentials.contains("AIGX_API_KEY"));
        let settings: serde_yaml_ng::Value = serde_yaml_ng::from_str(&composed.config).unwrap();
        assert_eq!(
            settings["llm-pi-ai"]["providers"]["aigx"]["api"],
            serde_yaml_ng::Value::String("openai-completions".into())
        );
    }

    #[test]
    fn dsh_rejects_bad_credentials_shape() {
        let current = CurrentFiles {
            config: String::new(),
            credentials: "refs:\n  OLD: x\n".into(),
        };
        let err = compose(Target::Dsh, &record("dsh"), "sk", &current).unwrap_err();
        assert!(err.contains("version"), "{err}");
    }

    #[test]
    fn validation_rejects_bad_url_and_unknown_options() {
        assert!(validate_fields("n", "ftp://x", "", "", &[], &json!({})).is_err());
        assert!(validate_fields("n", "https://x?a=1", "", "", &[], &json!({})).is_err());
        assert!(validate_fields("n", "https://x", "", "", &[], &json!({"nope": 1})).is_err());
        assert!(validate_fields("n", "https://x", "", "", &[], &json!({"route": "a b"})).is_err());
        assert!(validate_fields("n", "https://x", "", "", &[], &json!({})).is_ok());
    }

    #[test]
    fn credential_ref_matches_dsh_convention() {
        assert_eq!(credential_ref("aigx"), "AIGX_API_KEY");
        assert_eq!(credential_ref("my-route_1"), "MY_ROUTE_1_API_KEY");
    }
}
