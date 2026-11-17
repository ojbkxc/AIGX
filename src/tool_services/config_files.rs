//! 本机客户端配置文件的格式解析 / 渲染与目标路径表。
//!
//! 工具服务安装（Skill 包 / MCP 条目写进本机客户端）复用本模块；「客户端
//! 接入」管理面已移除，这里只保留安装路径需要的两件事：
//!
//! - [`ConfigFormat`] + [`parse`] / [`render`]：JSON / TOML / YAML 的解析与
//!   无损渲染（TOML 用 `toml_edit` 原位补丁，注释、键顺序与空白逐字保留）。
//! - [`Target`]：四个客户端的固定路径表（仅环境变量做目录覆盖，不接受任何
//!   运行时传入路径）。
//!
//! JSON 保留原文件 BOM 与 CRLF 风格；渲染后重新解析并与期望值比对，不一致
//! 就拒绝写入。

use std::path::PathBuf;

use serde_json::{Map, Value};

/// 目标文件的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Json,
    Toml,
    Yaml,
}

/// 解析配置文本 → JSON 值。空文本视为空文档（`{}`）。
///
/// **不做宽松修复**：解析失败就是失败，调用方必须拒绝写入 —— 覆盖一个
/// 手写坏掉的文件比报错危险得多。
pub fn parse(format: ConfigFormat, text: &str) -> Result<Value, String> {
    match format {
        ConfigFormat::Json => parse_json(text),
        ConfigFormat::Toml => parse_toml(text),
        ConfigFormat::Yaml => parse_yaml(text),
    }
}

/// 渲染：`original` 只用于保留 BOM/换行风格与（TOML）注释装饰。
pub fn render(format: ConfigFormat, original: &str, desired: &Value) -> Result<String, String> {
    match format {
        ConfigFormat::Json => render_json(original, desired),
        ConfigFormat::Toml => render_toml(original, desired),
        ConfigFormat::Yaml => render_yaml(original, desired),
    }
}

fn parse_json(text: &str) -> Result<Value, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(text).map_err(|e| format!("现有配置不是合法 JSON：{e}"))
}

fn render_json(original: &str, desired: &Value) -> Result<String, String> {
    let bom = original.starts_with('\u{feff}');
    let crlf = original.contains("\r\n");
    let mut text =
        serde_json::to_string_pretty(desired).map_err(|e| format!("JSON 序列化失败：{e}"))?;
    text.push('\n');
    if crlf {
        text = text.replace('\n', "\r\n");
    }
    if bom {
        text.insert(0, '\u{feff}');
    }
    if parse_json(&text)? != *desired {
        return Err("JSON 渲染结果与预期不一致，已停止写入".into());
    }
    Ok(text)
}

fn parse_toml(text: &str) -> Result<Value, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let table: toml::Table =
        toml::from_str(text).map_err(|e| format!("现有配置不是合法 TOML：{e}"))?;
    serde_json::to_value(table).map_err(|e| format!("TOML 无法转换为 JSON：{e}"))
}

/// TOML 原位补丁渲染（移植自 Agent2API `core/agents/toml_editing.rs`）。
fn render_toml(original: &str, desired: &Value) -> Result<String, String> {
    let bom = original.starts_with('\u{feff}');
    let crlf = original.contains("\r\n");
    let text = original.trim_start_matches('\u{feff}');
    let before = parse_toml(text)?;
    if before == *desired {
        return Ok(original.into());
    }
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("TOML 编辑失败：{e}"))?;
    let before_map = before.as_object().ok_or("TOML 根必须是表")?;
    let after_map = desired.as_object().ok_or("TOML 根必须是表")?;
    patch(document.as_table_mut(), before_map, after_map)?;
    let mut rendered = document.to_string();
    if crlf {
        rendered = rendered.replace("\r\n", "\n").replace('\n', "\r\n");
    }
    if bom {
        rendered.insert(0, '\u{feff}');
    }
    if parse_toml(&rendered)? != *desired {
        return Err("TOML 编辑结果与预期不一致，已停止写入".into());
    }
    Ok(rendered)
}

fn patch(
    table: &mut dyn toml_edit::TableLike,
    before: &Map<String, Value>,
    after: &Map<String, Value>,
) -> Result<(), String> {
    for key in before.keys().filter(|key| !after.contains_key(*key)) {
        table.remove(key);
    }
    for (key, value) in after {
        if before.get(key) == Some(value) {
            continue;
        }
        if let (Some(old), Some(new)) = (
            before.get(key).and_then(Value::as_object),
            value.as_object(),
        ) {
            let child = table
                .get_mut(key)
                .and_then(toml_edit::Item::as_table_like_mut)
                .ok_or_else(|| format!("TOML 字段 {key} 不是表，拒绝覆盖"))?;
            patch(child, old, new)?;
        } else {
            // 继承旧条目的装饰（toml_edit 解析器把独立注释行挂到**下一个键
            // 的 leaf_decor.prefix**；Table::insert 的 Occupied 分支会执行
            // key_mut().fmt() 把它抹掉——必须先存后补）：
            // - Key 层：前置注释 / 空行（独立注释行的归属地）
            // - Value 层：行内前导空白与行尾注释
            let old_key_decor = table
                .get_key_value(key)
                .map(|(old_key, _)| old_key.leaf_decor().clone());
            let old_val_decor = table
                .get(key)
                .and_then(toml_edit::Item::as_value)
                .map(|old_val| old_val.decor().clone());
            let mut replacement = item(value)?;
            if let Some(new_val) = replacement.as_value_mut() {
                if let Some(decor) = old_val_decor {
                    *new_val.decor_mut() = decor;
                }
            }
            table.insert(key, replacement);
            if let Some(decor) = old_key_decor {
                if let Some(mut new_key) = table.key_mut(key) {
                    *new_key.leaf_decor_mut() = decor;
                }
            }
        }
    }
    Ok(())
}

fn item(value: &Value) -> Result<toml_edit::Item, String> {
    if let Some(object) = value.as_object() {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        for (key, value) in object {
            table.insert(key, item(value)?);
        }
        return Ok(toml_edit::Item::Table(table));
    }
    // 标量/数组交给 toml 序列化器生成合法字面量，再包一层文档解析取回 Item
    let text = toml::to_string(&serde_json::json!({ "value": value }))
        .map_err(|e| format!("TOML 值无法生成：{e}"))?;
    let mut document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| format!("TOML 值无法解析：{e}"))?;
    document
        .remove("value")
        .ok_or_else(|| "TOML 值缺失".to_string())
}

fn parse_yaml(text: &str) -> Result<Value, String> {
    let text = text.trim_start_matches('\u{feff}');
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(text).map_err(|e| format!("现有配置不是合法 YAML：{e}"))?;
    serde_json::to_value(value).map_err(|e| format!("YAML 无法转换为 JSON：{e}"))
}

fn render_yaml(original: &str, desired: &Value) -> Result<String, String> {
    let bom = original.starts_with('\u{feff}');
    let crlf = original.contains("\r\n");
    let mut text =
        serde_yaml_ng::to_string(desired).map_err(|e| format!("YAML 序列化失败：{e}"))?;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    if crlf {
        text = text.replace('\n', "\r\n");
    }
    if bom {
        text.insert(0, '\u{feff}');
    }
    if parse_yaml(&text)? != *desired {
        return Err("YAML 渲染结果与预期不一致，已停止写入".into());
    }
    Ok(text)
}

/// 一个目标的配置文件类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// 主配置（settings.json / config.toml / settings.yaml）
    Config,
    /// DSH 的独立凭据文件（`.credentials.yaml`）
    Credentials,
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

    /// 主配置文件的路径。
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn toml_keeps_comments_and_order() {
        let original = r#"# 顶部注释
model = "gpt-5" # 行尾注释

[model_providers.old]
# 老段落注释
name = "old"
base_url = "https://old.example.com"
"#;
        let mut desired = parse(ConfigFormat::Toml, original).unwrap();
        let providers = desired
            .as_object_mut()
            .unwrap()
            .entry("model_providers")
            .or_insert_with(|| json!({}));
        providers.as_object_mut().unwrap().insert(
            "aigx".into(),
            json!({"name": "AIGX", "base_url": "https://aigx.example.com", "wire_api": "responses"}),
        );
        desired
            .as_object_mut()
            .unwrap()
            .insert("model_provider".into(), json!("aigx"));
        let rendered = render(ConfigFormat::Toml, original, &desired).unwrap();
        assert!(rendered.contains("# 顶部注释"), "顶部注释丢失: {rendered}");
        assert!(
            rendered.contains("# 老段落注释"),
            "段内注释丢失: {rendered}"
        );
        assert!(rendered.contains("# 行尾注释"), "行尾注释丢失: {rendered}");
        // 无关的老段落保留
        assert!(rendered.contains("[model_providers.old]"));
        // 新内容确实写进去了
        assert!(rendered.contains("[model_providers.aigx]"));
        assert_eq!(parse(ConfigFormat::Toml, &rendered).unwrap(), desired);
    }

    #[test]
    fn toml_roundtrip_no_change_is_byte_identical() {
        let original = "# 注释\r\nmodel = \"x\"\r\n";
        let desired = parse(ConfigFormat::Toml, original).unwrap();
        assert_eq!(
            render(ConfigFormat::Toml, original, &desired).unwrap(),
            original
        );
    }

    #[test]
    fn json_keeps_unrelated_fields() {
        let original =
            r#"{"mcpServers":{"fs":{"command":"npx"}},"env":{"ANTHROPIC_BASE_URL":"https://old"}}"#;
        let mut desired = parse(ConfigFormat::Json, original).unwrap();
        desired["env"]["ANTHROPIC_BASE_URL"] = json!("https://new");
        desired["env"]["ANTHROPIC_AUTH_TOKEN"] = json!("sk-test");
        let rendered = render(ConfigFormat::Json, original, &desired).unwrap();
        assert!(rendered.contains("\"mcpServers\""));
        assert!(rendered.contains("npx"));
        assert!(rendered.contains("https://new"));
        assert_eq!(parse(ConfigFormat::Json, &rendered).unwrap(), desired);
    }

    #[test]
    fn yaml_structural_roundtrip() {
        let original = "llm-pi-ai:\n  providers:\n    old:\n      api: openai-completions\n";
        let desired = parse(ConfigFormat::Yaml, original).unwrap();
        let rendered = render(ConfigFormat::Yaml, original, &desired).unwrap();
        assert_eq!(parse(ConfigFormat::Yaml, &rendered).unwrap(), desired);
    }
}
