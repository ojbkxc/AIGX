// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 上下文压缩（compact）路由：ZCode 的压缩请求模板识别、换模型后的参数收口，
//! 以及配置形状/持久化/校验。移植自 A2A `core/compact_routing/mod.rs`、
//! `core/compact_routing/parameters.rs`、`config/compact_routes.rs`。
//!
//! 判据只匹配**当前请求末尾的完整模板**，不搜索历史里的关键词；前文至少要有
//! 一次助手响应，孤立粘贴模板不算「已发生的会话压缩」。

use std::borrow::Cow;
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::storage::FileStore;

use super::CLIENT_ZCODE;

/// FileStore 持久化键（整块读写）。
pub const KEY_COMPACT_ROUTES: &str = "compact_model_routes";
/// 规则条数上限（A2A 同值：挡住异常大载荷，不挡正常配置）。
pub const MAX_COMPACT_ROUTES: usize = 200;

// 来源：ZCode core/compact/prompt.ts。模型用途仅存在于 CLI 内部，HTTP 没有
// 专用端点或用途头，所以只能靠模板识别。
const ZCODE_PREAMBLE: &str = "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.\n\n\
- Do NOT use Read, Bash, Grep, Glob, Edit, Write, or ANY other tool.\n\
- You already have all the context you need in the conversation above.\n\
- Tool calls will be REJECTED and will waste your only turn — you will fail the task.\n\
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.\n\n";
const ZCODE_TASK: &str = "Your task is to create a detailed summary of the conversation so far, paying close attention to the user's explicit requests and your previous actions.";
const ZCODE_TRAILER: &str = "\n\nREMINDER: Do NOT call any tools. Respond with plain text only — an <analysis> block followed by a <summary> block. Tool calls will be rejected and you will fail the task.";
const ZCODE_STRUCTURE: &[&str] = &[
    "This summary should be thorough in capturing technical details, code patterns, and architectural decisions",
    "Before providing your final summary, wrap your analysis in <analysis> tags",
    "Your summary should include the following sections:",
    "1. Primary Request and Intent:",
    "6. All user messages:",
    "9. Optional Next Step:",
    "Here's an example of how your output should be structured:",
    "</summary>\n</example>",
    "Please provide your summary based on the conversation so far, following this structure",
];

// ── 配置 ──────────────────────────────────────────────────────────────

/// 一条压缩路由：客户端 + 来源模型 → 压缩模型。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactRoute {
    pub client: String,
    pub source: String,
    pub target: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// 压缩路由整块配置。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactRoutes {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<CompactRoute>,
}

impl Default for CompactRoutes {
    fn default() -> Self {
        Self {
            enabled: true,
            rules: Vec::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// 运行期快照：`settings` 为 Err 时表示存储里的配置非法（数据面据此不生效）。
#[derive(Clone, Debug)]
pub struct CompactRoutesSnapshot {
    pub settings: Result<CompactRoutes, String>,
    pub revision: String,
}

impl Default for CompactRoutesSnapshot {
    fn default() -> Self {
        from_value(None)
    }
}

/// 模型名归一（大小写不敏感比较）。
pub fn model_key(model: &str) -> String {
    model.trim().to_lowercase()
}

/// 配置形状校验（不查模型是否存在 —— 那需要模型目录，放在管理面 PUT）。
pub fn validate(settings: &CompactRoutes) -> Result<(), String> {
    if settings.rules.len() > MAX_COMPACT_ROUTES {
        return Err(format!("压缩路由最多允许 {MAX_COMPACT_ROUTES} 条规则"));
    }
    let mut sources = HashSet::new();
    for rule in &settings.rules {
        if rule.client != CLIENT_ZCODE {
            return Err("压缩路由目前只支持 ZCode 客户端".into());
        }
        for model in [&rule.source, &rule.target] {
            if model.trim().is_empty()
                || model.trim() != model
                || model.chars().count() > 256
                || model.chars().any(char::is_control)
            {
                return Err(
                    "来源模型和压缩模型必须完整填写，模型 ID 不能超过 256 个字符或含控制字符"
                        .into(),
                );
            }
        }
        if model_key(&rule.source) == model_key(&rule.target) {
            return Err("来源模型和压缩模型不能相同".into());
        }
        if !sources.insert((rule.client.clone(), model_key(&rule.source))) {
            return Err(format!("同一客户端的来源模型不能重复：{}", rule.source));
        }
    }
    Ok(())
}

/// 修订号：对**存储值**求 sha256；键不存在或为 `null` 时对 `Value::Null` 求哈希
/// （与 A2A `revision()` 同口径，读写两端一致）。
pub fn revision(value: Option<&Value>) -> String {
    let raw = value.cloned().unwrap_or(Value::Null);
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

fn from_value(value: Option<&Value>) -> CompactRoutesSnapshot {
    // 顶层 `null` 视同「键不存在」：三者（缺键 / null / 空数组）都必须安全。
    let value = value.filter(|v| !v.is_null());
    let settings = match value {
        None => Ok(CompactRoutes::default()),
        Some(value) => serde_json::from_value::<CompactRoutes>(value.clone())
            .map_err(|error| format!("压缩路由配置格式不正确：{error}"))
            .and_then(|settings| {
                validate(&settings)?;
                Ok(settings)
            }),
    };
    CompactRoutesSnapshot {
        settings,
        revision: revision(value),
    }
}

/// 从 `FileStore` 装载快照（`install` 启动时调用）。
pub fn load(store: &FileStore) -> std::sync::Arc<CompactRoutesSnapshot> {
    let raw: Option<Value> = match store.get(KEY_COMPACT_ROUTES) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("读取压缩路由配置失败，按默认配置启动：{error}");
            None
        }
    };
    std::sync::Arc::new(from_value(raw.as_ref()))
}

// ── 识别 ──────────────────────────────────────────────────────────────

fn template_matches(text: &str) -> bool {
    let normalized = if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(text)
    };
    let Some(rest) = normalized.trim().strip_prefix(ZCODE_PREAMBLE) else {
        return false;
    };
    if !rest.starts_with(ZCODE_TASK) || !rest.ends_with(ZCODE_TRAILER) {
        return false;
    }
    // 固定段必须顺序出现；用户的 Additional Instructions 允许变化，不参与特征选择。
    let mut remaining = rest;
    for marker in ZCODE_STRUCTURE {
        let Some((_, tail)) = remaining.split_once(marker) else {
            return false;
        };
        remaining = tail;
    }
    true
}

/// 当前请求末尾是否为 ZCode 压缩模板（chat 协议 `messages`）。
///
/// 要求：最后一条是 user；前文至少一次 assistant；内容（字符串或最后一块
/// `text`）完整匹配模板。
pub fn detect_chat(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    let Some(last) = messages.last() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) != Some("user") {
        return false;
    }
    // 前文至少有一次助手响应；孤立粘贴模板不是已发生的会话压缩。
    if !messages[..messages.len().saturating_sub(1)]
        .iter()
        .any(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
    {
        return false;
    }
    match last.get("content") {
        Some(Value::String(text)) => template_matches(text),
        Some(Value::Array(parts)) => parts
            .last()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|part| part.get("text").and_then(Value::as_str))
            .is_some_and(template_matches),
        _ => false,
    }
}

/// 在给定配置里找命中 `(client, source)` 的启用规则，返回目标模型。
pub fn find_target(settings: &CompactRoutes, client: &str, source: &str) -> Option<String> {
    if !settings.enabled {
        return None;
    }
    settings
        .rules
        .iter()
        .find(|rule| {
            rule.enabled && rule.client == client && model_key(&rule.source) == model_key(source)
        })
        .map(|rule| rule.target.clone())
}

/// 数据面综合判据：总开关 + ZCode 模板 + `(client, source)` 规则。
///
/// 只认 ZCode（压缩模板只有 ZCode 有）。
pub fn resolve(client: &str, source: &str, body: &Value) -> Option<String> {
    if client != CLIENT_ZCODE {
        return None;
    }
    let snapshot = super::compact_current();
    let settings = snapshot.settings.as_ref().ok()?;
    if !settings.enabled {
        return None;
    }
    if !detect_chat(body) {
        return None;
    }
    find_target(settings, client, source)
}

// ── 换模型后的参数收口（移植自 parameters.rs）────────────────────────

/// 清掉来源模型的思考档位：它们描述的是原模型的语义，换模型后不应沿用。
///
/// `output_config` 可能还承载结构化输出格式，只移除其中的 `effort`。
pub fn clear_source_reasoning(body: &mut Value) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    for key in [
        "reasoning_effort",
        "reasoningEffort",
        "effort",
        "reasoning",
        "thinking",
        "enable_thinking",
        "thinking_budget",
    ] {
        object.remove(key);
    }
    if let Some(output) = object
        .get_mut("output_config")
        .and_then(Value::as_object_mut)
    {
        output.remove("effort");
        if output.is_empty() {
            object.remove("output_config");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 构造一段完整模板（Additional Instructions 处可替换）。
    fn template(extra: &str) -> String {
        let mut out = String::new();
        out.push_str(ZCODE_PREAMBLE);
        out.push_str(ZCODE_TASK);
        out.push('\n');
        for marker in ZCODE_STRUCTURE {
            out.push_str(marker);
            out.push('\n');
        }
        if !extra.is_empty() {
            out.push_str(extra);
            out.push('\n');
        }
        out.push_str(ZCODE_TRAILER);
        out
    }

    fn chat_body(last_content: Value, with_assistant: bool) -> Value {
        let mut messages = vec![json!({ "role": "user", "content": "hi" })];
        if with_assistant {
            messages.push(json!({ "role": "assistant", "content": "hello" }));
        }
        messages.push(json!({ "role": "user", "content": last_content }));
        json!({ "model": "some-model", "messages": messages })
    }

    #[test]
    fn compact_template_positive() {
        assert!(detect_chat(&chat_body(json!(template("")), true)));
        // 模板前有前导空白也认（模板整体 trim 后匹配）。
        assert!(detect_chat(&chat_body(
            json!(format!("\n  {}", template(""))),
            true
        )));
        // 内容块数组的最后一个 text 块。
        let blocks = json!([{ "type": "text", "text": template("") }]);
        assert!(detect_chat(&chat_body(blocks, true)));
    }

    #[test]
    fn compact_template_accepts_crlf() {
        let crlf = template("").replace('\n', "\r\n");
        assert!(detect_chat(&chat_body(json!(crlf), true)));
    }

    #[test]
    fn compact_template_accepts_extra_instructions() {
        assert!(detect_chat(&chat_body(
            json!(template("Additional Instructions: focus on API design.")),
            true
        )));
    }

    #[test]
    fn compact_template_rejects_isolation_and_missing_assistant() {
        // 孤立粘贴：没有助手响应。
        assert!(!detect_chat(&chat_body(json!(template("")), false)));
        // 只有 user 一条，且不是模板。
        assert!(!detect_chat(&json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "please summarize" }]
        })));
    }

    #[test]
    fn compact_template_rejects_with_tools_or_extra_text() {
        // 带工具的正常对话，即便措辞相近也不是压缩模板。
        let body = json!({
            "model": "m",
            "tools": [{ "type": "function", "function": { "name": "x" } }],
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" },
                { "role": "user", "content": template("") }
            ]
        });
        // detect_chat 只看模板本身；带工具由标题判据过滤。这里确认模板仍被识别
        // （压缩判据本来就不看 tools），以免误以为带工具会破坏模板匹配。
        assert!(detect_chat(&body));
        // 模板后带额外尾随文本（破环 TRAILER 结尾）→ 反例。
        let trailing = format!("{}\ntrailing", template(""));
        assert!(!detect_chat(&chat_body(json!(trailing), true)));
        // 缺少固定段 → 反例。
        let broken = template("").replace(ZCODE_STRUCTURE[0], "MISSING");
        assert!(!detect_chat(&chat_body(json!(broken), true)));
    }

    #[test]
    fn compact_find_target_matches_client_and_source_case_insensitive() {
        let settings = CompactRoutes {
            enabled: true,
            rules: vec![CompactRoute {
                client: CLIENT_ZCODE.into(),
                source: "GLM-5.3".into(),
                target: "glm-5.3-flash".into(),
                enabled: true,
            }],
        };
        assert_eq!(
            find_target(&settings, CLIENT_ZCODE, "glm-5.3").as_deref(),
            Some("glm-5.3-flash")
        );
        // 客户端不符 / 未启用 / 关闭总开关 → 不命中。
        assert!(find_target(&settings, "codex", "GLM-5.3").is_none());
        let mut disabled_rule = settings.clone();
        disabled_rule.rules[0].enabled = false;
        assert!(find_target(&disabled_rule, CLIENT_ZCODE, "GLM-5.3").is_none());
        let mut disabled = settings;
        disabled.enabled = false;
        assert!(find_target(&disabled, CLIENT_ZCODE, "GLM-5.3").is_none());
    }

    #[test]
    fn clear_source_reasoning_removes_effort_but_keeps_output_format() {
        let mut body = json!({
            "model": "m",
            "reasoning_effort": "high",
            "thinking_budget": 4096,
            "output_config": { "effort": "high", "format": { "type": "json_schema" } }
        });
        clear_source_reasoning(&mut body);
        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("thinking_budget").is_none());
        assert_eq!(
            body.pointer("/output_config/format/type")
                .and_then(Value::as_str),
            Some("json_schema")
        );
        assert!(body.pointer("/output_config/effort").is_none());
    }

    #[test]
    fn serde_three_states_null_missing_empty() {
        // 键不存在 → 默认（enabled=true，空规则）。
        let absent = from_value(None);
        let cfg = absent.settings.as_ref().unwrap();
        assert!(cfg.enabled);
        assert!(cfg.rules.is_empty());

        // 顶层 null → 视同缺键。
        let null = from_value(Some(&Value::Null));
        let cfg = null.settings.as_ref().unwrap();
        assert!(cfg.enabled);
        assert!(cfg.rules.is_empty());

        // 缺字段（{}）→ 默认。
        let missing = from_value(Some(&json!({})));
        assert!(missing.settings.as_ref().unwrap().enabled);

        // 空数组 → 显式空规则。
        let empty = from_value(Some(&json!({ "enabled": true, "rules": [] })));
        assert!(empty.settings.as_ref().unwrap().rules.is_empty());
    }

    #[test]
    fn revision_is_stable_and_differs_on_change() {
        assert_eq!(revision(None), revision(Some(&Value::Null)));
        let a = json!({ "enabled": true, "rules": [] });
        let b = json!({ "enabled": false, "rules": [] });
        assert_ne!(revision(Some(&a)), revision(Some(&b)));
        assert_eq!(revision(Some(&a)), revision(Some(&a.clone())));
    }

    #[test]
    fn validate_rejects_duplicates_and_same_model() {
        let dup = CompactRoutes {
            enabled: true,
            rules: vec![
                CompactRoute {
                    client: CLIENT_ZCODE.into(),
                    source: "A".into(),
                    target: "B".into(),
                    enabled: true,
                },
                CompactRoute {
                    client: CLIENT_ZCODE.into(),
                    source: "a".into(),
                    target: "C".into(),
                    enabled: true,
                },
            ],
        };
        assert!(validate(&dup).is_err());

        let same = CompactRoutes {
            enabled: true,
            rules: vec![CompactRoute {
                client: CLIENT_ZCODE.into(),
                source: "A".into(),
                target: "a".into(),
                enabled: true,
            }],
        };
        assert!(validate(&same).is_err());

        let bad_client = CompactRoutes {
            enabled: true,
            rules: vec![CompactRoute {
                client: "codex".into(),
                source: "A".into(),
                target: "B".into(),
                enabled: true,
            }],
        };
        assert!(validate(&bad_client).is_err());
    }
}
