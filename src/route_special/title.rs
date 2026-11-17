// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 标题生成（title）路由：把客户端自动生成的标题请求改写到低成本模型。
//! 移植自 A2A `core/title_routing/mod.rs`、`config/title_routes.rs`。
//!
//! ── 判据从哪来（照搬 A2A 注释）──────────────────────────────
//! 标题请求不是独立端点，也没有统一的用途头，各家的标记方式不一样：
//!   · **Codex** 用请求头显式标注：`x-openai-subagent`
//!     （`thread_title` / `thread_title_reconsideration` / `title_generation`），
//!     或 `x-codex-turn-metadata` 的 `thread_source`；Responses 的
//!     `client_metadata` 里还有一份同样的 `x-codex-turn-metadata`。
//!   · **ZCode** 没有用途头，但 system 提示词固定，且只发两条消息、不带工具。
//!   · **Claude Code** 的 user 消息以 `Please write a 5-10 word title` 开头
//!     （`5-10` 与 `5–10` 两种连字符）。
//!
//! ── 为什么要求「不带工具」──
//! 提示词类判据（ZCode / Claude Code）只能靠措辞。带上工具说明的请求哪怕措辞
//! 相同也是正常对话，把它改写到便宜模型会答坏。Codex 有显式标记，不受此约束。

use std::collections::HashSet;

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::storage::FileStore;

use super::{client_label, CLIENT_CLAUDE_CODE, CLIENT_CODEX, CLIENT_ZCODE};

/// FileStore 持久化键（整块读写）。
pub const KEY_TITLE_ROUTES: &str = "title_model_routes";
/// 单条规则里客户端个数上限（宽松上限：挡住异常大载荷）。
pub const MAX_CLIENTS: usize = 32;
/// 规则条数上限。
pub const MAX_RULES: usize = 32;

/// ZCode 标题 sidecar 的 system 提示词首句。取首句而不是全文：后半段是给模型
/// 的措辞约束，客户端改版时更容易动。
const ZCODE_TITLE_PREAMBLE: &str = "Generate a concise title for this coding session.";
/// Claude Code 的标题请求开头（两种连字符都出现过）。
const CLAUDE_TITLE_PREFIXES: &[&str] = &[
    "Please write a 5-10 word title",
    "Please write a 5–10 word title",
];
/// Codex 在 `x-openai-subagent` / `thread_source` 里标记标题请求的取值。
const CODEX_TITLE_KINDS: &[&str] = &[
    "thread_title",
    "thread_title_reconsideration",
    "title_generation",
];

// ── 配置 ──────────────────────────────────────────────────────────────

/// 标题模型不可用时怎么办。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fallback {
    /// 退回客户端原模型重试一次（默认）：标题请求失败会直接影响会话列表显示。
    #[default]
    Original,
    /// 明确报错，不改用原模型。
    Error,
}

impl Fallback {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Error => "error",
        }
    }
}

/// 标题生成的一条规则：认哪些客户端、改成哪只模型、失败怎么办。
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TitleRule {
    /// 单条开关（总开关关掉时整块不生效）。
    #[serde(default)]
    pub enabled: bool,
    /// 参与识别的客户端 id。空 = 这条规则不生效（等同关闭）。
    #[serde(default)]
    pub clients: Vec<String>,
    /// 标题模型。空串 = 未配置。
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub fallback: Fallback,
}

/// 标题路由整块配置（按 `rules` 顺序匹配，第一条命中的生效）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TitleRoutes {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<TitleRule>,
}

impl Default for TitleRoutes {
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
pub struct TitleRoutesSnapshot {
    pub settings: Result<TitleRoutes, String>,
    pub revision: String,
}

impl Default for TitleRoutesSnapshot {
    fn default() -> Self {
        from_value(None)
    }
}

/// 模型名归一（大小写不敏感比较）。
pub fn model_key(model: &str) -> String {
    model.trim().to_lowercase()
}

/// 配置形状校验（不查模型是否存在 —— 放在管理面 PUT）。
pub fn validate(settings: &TitleRoutes) -> Result<(), String> {
    if settings.rules.len() > MAX_RULES {
        return Err(format!("标题生成路由最多支持 {MAX_RULES} 条规则"));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        let at = index + 1;
        if rule.clients.len() > MAX_CLIENTS {
            return Err(format!(
                "标题生成路由第 {at} 条规则最多支持 {MAX_CLIENTS} 个客户端"
            ));
        }
        let mut seen = HashSet::new();
        for client in &rule.clients {
            if client_label(client).is_none() {
                return Err(format!("标题生成路由不支持客户端「{client}」"));
            }
            if !seen.insert(client.as_str()) {
                return Err(format!(
                    "第 {at} 条规则里「{}」重复出现",
                    client_label(client).unwrap_or(client)
                ));
            }
        }
        // 只有真的启用时才要求配全：关掉以后保留半截配置不该变成非法文件。
        let live = settings.enabled && rule.enabled;
        if live && rule.clients.is_empty() {
            return Err(format!("启用标题生成路由第 {at} 条规则前请先选择客户端"));
        }
        let model = &rule.model;
        if model.trim().is_empty() {
            if live {
                return Err(format!("启用标题生成路由第 {at} 条规则前请先选择标题模型"));
            }
            continue;
        }
        if model.trim() != model
            || model.chars().count() > 256
            || model.chars().any(char::is_control)
        {
            return Err(format!(
                "第 {at} 条规则的标题模型 ID 不能超过 256 个字符、不能含控制字符或首尾空格"
            ));
        }
    }
    Ok(())
}

/// 修订号：对**存储值**求 sha256（与压缩路由同口径）。
pub fn revision(value: Option<&Value>) -> String {
    let raw = value.cloned().unwrap_or(Value::Null);
    format!("{:x}", Sha256::digest(raw.to_string().as_bytes()))
}

fn from_value(value: Option<&Value>) -> TitleRoutesSnapshot {
    let value = value.filter(|v| !v.is_null());
    let settings = match value {
        None => Ok(TitleRoutes::default()),
        Some(value) => serde_json::from_value::<TitleRoutes>(value.clone())
            .map_err(|error| format!("标题生成路由配置格式不正确：{error}"))
            .and_then(|settings| {
                validate(&settings)?;
                Ok(settings)
            }),
    };
    TitleRoutesSnapshot {
        settings,
        revision: revision(value),
    }
}

/// 从 `FileStore` 装载快照（`install` 启动时调用）。
pub fn load(store: &FileStore) -> std::sync::Arc<TitleRoutesSnapshot> {
    let raw: Option<Value> = match store.get(KEY_TITLE_ROUTES) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!("读取标题路由配置失败，按默认配置启动：{error}");
            None
        }
    };
    std::sync::Arc::new(from_value(raw.as_ref()))
}

/// 这条规则此刻是否真的会命中：自己开着、勾了客户端、配了模型。
fn effective(rule: &TitleRule) -> bool {
    rule.enabled && !rule.clients.is_empty() && !rule.model.trim().is_empty()
}

/// 当前生效的配置：总开关打开、且至少有一条真会命中的规则。
pub fn active(settings: &TitleRoutes) -> bool {
    settings.enabled && settings.rules.iter().any(effective)
}

/// 这次请求该走哪条规则：按列表顺序取第一条命中的（顺序即优先级）。
pub fn match_rule<'a>(settings: &'a TitleRoutes, client: &str) -> Option<&'a TitleRule> {
    settings
        .rules
        .iter()
        .filter(|rule| effective(rule))
        .find(|rule| rule.clients.iter().any(|id| id == client))
}

// ── 识别 ──────────────────────────────────────────────────────────────

/// 归一化：统一换行、去掉首尾空白，便于做前缀匹配。
fn normalized(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    }
}

fn text_of_content(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let parts = content.as_array()?;
    let mut joined = String::new();
    for part in parts {
        // Anthropic 的 text 块与 Responses 的 input_text 块同形。
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(text);
        }
    }
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

fn role_text(messages: &[Value], role: &str) -> Option<String> {
    let mut joined = String::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some(role) {
            continue;
        }
        if let Some(text) = message.get("content").and_then(text_of_content) {
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(&text);
        }
    }
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// 请求是否携带工具（含工具结果）。带工具的一律不当作提示词类标题请求。
fn has_tools(body: &Value) -> bool {
    if body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        return true;
    }
    if body.get("tool_choice").is_some() {
        return true;
    }
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages.iter().any(|message| {
                matches!(
                    message.get("role").and_then(Value::as_str),
                    Some("tool") | Some("function")
                ) || message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
            })
        })
}

/// 从 `x-codex-turn-metadata` 里读 `thread_source`。
///
/// `user` / `subagent` / 空都表示「正常轮次」，不算标题请求。
fn thread_source(metadata: &str) -> Option<String> {
    if !metadata.contains("thread_source") {
        return None;
    }
    let parsed: Value = serde_json::from_str(metadata).ok()?;
    let source = parsed
        .get("thread_source")
        .and_then(Value::as_str)
        .map(str::trim)?;
    match source {
        "" | "user" | "subagent" => None,
        other => Some(other.to_string()),
    }
}

fn is_codex_title_kind(kind: &str) -> bool {
    CODEX_TITLE_KINDS.contains(&kind)
}

/// Codex 是否明确标注了这次请求是标题生成。
fn codex_title(headers: &HeaderMap, body: &Value) -> bool {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
    };
    // ① 直接声明「这不是对话轮次，是 subagent 调用」。
    if let Some(kind) = header("x-openai-subagent").filter(|value| !value.is_empty()) {
        return is_codex_title_kind(kind);
    }
    // ② body 里的 client_metadata（Codex 的规范传输）。
    if let Some(metadata) = body.pointer("/client_metadata/x-codex-turn-metadata") {
        let text = match metadata.as_str() {
            Some(text) => text.to_string(),
            None => metadata.to_string(),
        };
        if let Some(source) = thread_source(&text) {
            return is_codex_title_kind(&source);
        }
    }
    // ③ 头部投影的 turn metadata。
    if let Some(text) = header("x-codex-turn-metadata") {
        if let Some(source) = thread_source(text) {
            return is_codex_title_kind(&source);
        }
    }
    // ④ 没有元数据时，退回 client_metadata 里平铺的 thread_source（旧版客户端）。
    if let Some(source) = body
        .pointer("/client_metadata/thread_source")
        .and_then(Value::as_str)
        .map(str::trim)
    {
        if !matches!(source, "" | "user" | "subagent") {
            return is_codex_title_kind(source);
        }
    }
    false
}

fn zcode_title(messages: &[Value]) -> bool {
    let Some(system) = role_text(messages, "system") else {
        return false;
    };
    if !normalized(&system)
        .trim_start()
        .starts_with(ZCODE_TITLE_PREAMBLE)
    {
        return false;
    }
    // 标题请求固定两条消息（system + 一段素材）；更多轮次说明这是正常对话。
    if messages.len() > 2 {
        return false;
    }
    role_text(messages, "user").is_some()
}

fn claude_title(messages: &[Value]) -> bool {
    let Some(user) = role_text(messages, "user") else {
        return false;
    };
    let text = normalized(&user);
    let trimmed = text.trim_start();
    CLAUDE_TITLE_PREFIXES
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// 判定这次请求是否属于某家客户端的标题生成（chat 协议 `messages`）。
///
/// Codex 用请求头，先判（省一次消息遍历）；提示词类判据要遍历消息结构，且
/// 带工具的一律不算。
pub fn detect(client: &str, headers: &HeaderMap, body: &Value) -> bool {
    if client == CLIENT_CODEX {
        return codex_title(headers, body);
    }
    if has_tools(body) {
        return false;
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    match client {
        CLIENT_ZCODE => zcode_title(messages),
        CLIENT_CLAUDE_CODE => claude_title(messages),
        _ => false,
    }
}

/// 在给定配置里判定命中：`(目标模型, 回退策略)`。
pub fn find_target(
    settings: &TitleRoutes,
    client: &str,
    headers: &HeaderMap,
    body: &Value,
) -> Option<(String, Fallback)> {
    if !settings.enabled {
        return None;
    }
    let rule = match_rule(settings, client)?;
    let model = rule.model.trim();
    if model.is_empty() {
        return None;
    }
    if !detect(client, headers, body) {
        return None;
    }
    Some((model.to_string(), rule.fallback))
}

/// 数据面综合判据：总开关 + 规则命中 + 判据命中。
pub fn resolve(client: &str, headers: &HeaderMap, body: &Value) -> Option<(String, Fallback)> {
    let snapshot = super::title_current();
    let settings = snapshot.settings.as_ref().ok()?;
    if !active(settings) {
        return None;
    }
    find_target(settings, client, headers, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn codex_title_three_header_forms() {
        let body = json!({ "messages": [] });
        // ① x-openai-subagent 直接声明。
        assert!(detect(
            CLIENT_CODEX,
            &headers(&[("x-openai-subagent", "thread_title")]),
            &body
        ));
        assert!(detect(
            CLIENT_CODEX,
            &headers(&[("x-openai-subagent", "title_generation")]),
            &body
        ));
        // 非标题 kind → 不认。
        assert!(!detect(
            CLIENT_CODEX,
            &headers(&[("x-openai-subagent", "other")]),
            &body
        ));
        // ② x-codex-turn-metadata 头部（JSON 字符串）。
        assert!(detect(
            CLIENT_CODEX,
            &headers(&[(
                "x-codex-turn-metadata",
                "{\"thread_source\":\"thread_title\"}"
            )]),
            &body
        ));
        // ③ body client_metadata 里的 x-codex-turn-metadata（对象形态）。
        let meta_obj = json!({
            "client_metadata": { "x-codex-turn-metadata": { "thread_source": "thread_title_reconsideration" } }
        });
        assert!(detect(CLIENT_CODEX, &HeaderMap::new(), &meta_obj));
        // ④ client_metadata 平铺 thread_source（旧版）。
        assert!(detect(
            CLIENT_CODEX,
            &HeaderMap::new(),
            &json!({ "client_metadata": { "thread_source": "thread_title" } })
        ));
        // thread_source = user → 正常轮次。
        assert!(!detect(
            CLIENT_CODEX,
            &headers(&[("x-codex-turn-metadata", "{\"thread_source\":\"user\"}")]),
            &body
        ));
    }

    #[test]
    fn zcode_title_requires_two_messages() {
        let system = "Generate a concise title for this coding session.";
        // system + user 两条 → 命中。
        assert!(detect(
            CLIENT_ZCODE,
            &HeaderMap::new(),
            &json!({
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": "some material" }
                ]
            })
        ));
        // 三条消息 → 正常对话，不命中。
        assert!(!detect(
            CLIENT_ZCODE,
            &HeaderMap::new(),
            &json!({
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": "hi" },
                    { "role": "assistant", "content": "hello" }
                ]
            })
        ));
        // 无 system → 不命中。
        assert!(!detect(
            CLIENT_ZCODE,
            &HeaderMap::new(),
            &json!({ "messages": [{ "role": "user", "content": "x" }] })
        ));
        // 带工具 → 不命中。
        assert!(!detect(
            CLIENT_ZCODE,
            &HeaderMap::new(),
            &json!({
                "tools": [{ "type": "function", "function": { "name": "t" } }],
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": "x" }
                ]
            })
        ));
    }

    #[test]
    fn claude_title_two_hyphen_variants() {
        let body_ascii = json!({
            "messages": [{ "role": "user", "content": "Please write a 5-10 word title for this session." }]
        });
        let body_en = json!({
            "messages": [{ "role": "user", "content": "Please write a 5–10 word title for this session." }]
        });
        assert!(detect(CLIENT_CLAUDE_CODE, &HeaderMap::new(), &body_ascii));
        assert!(detect(CLIENT_CLAUDE_CODE, &HeaderMap::new(), &body_en));
        // 普通请求 → 不命中。
        assert!(!detect(
            CLIENT_CLAUDE_CODE,
            &HeaderMap::new(),
            &json!({ "messages": [{ "role": "user", "content": "hello" }] })
        ));
    }

    #[test]
    fn find_target_gates_on_rule_and_flags() {
        let settings = TitleRoutes {
            enabled: true,
            rules: vec![TitleRule {
                enabled: true,
                clients: vec![CLIENT_ZCODE.into()],
                model: "cheap-model".into(),
                fallback: Fallback::Original,
            }],
        };
        let body = json!({
            "messages": [
                { "role": "system", "content": "Generate a concise title for this coding session." },
                { "role": "user", "content": "x" }
            ]
        });
        assert_eq!(
            find_target(&settings, CLIENT_ZCODE, &HeaderMap::new(), &body),
            Some(("cheap-model".to_string(), Fallback::Original))
        );
        // 未勾选的客户端 → 不命中。
        assert!(find_target(&settings, CLIENT_CODEX, &HeaderMap::new(), &body).is_none());
        // 总开关关闭 → 不命中。
        let mut off = settings;
        off.enabled = false;
        assert!(find_target(&off, CLIENT_ZCODE, &HeaderMap::new(), &body).is_none());
    }

    #[test]
    fn serde_three_states_null_missing_empty() {
        // 缺键 → 默认。
        assert!(from_value(None).settings.as_ref().unwrap().enabled);
        // 顶层 null → 视同缺键。
        assert!(from_value(Some(&Value::Null))
            .settings
            .as_ref()
            .unwrap()
            .rules
            .is_empty());
        // 缺字段（{}）→ 默认（enabled=true，空规则）。
        let missing = from_value(Some(&json!({})));
        assert!(missing.settings.as_ref().unwrap().enabled);
        // 空数组 → 显式空规则。
        let empty = from_value(Some(&json!({ "enabled": true, "rules": [] })));
        assert!(empty.settings.as_ref().unwrap().rules.is_empty());
        // 规则缺字段时用规则内默认（enabled=false / 空 clients / 空 model）。
        let rule_defaults = from_value(Some(&json!({ "enabled": true, "rules": [{}] })));
        let cfg = rule_defaults.settings.as_ref().unwrap();
        assert_eq!(cfg.rules.len(), 1);
        assert!(!cfg.rules[0].enabled);
        assert_eq!(cfg.rules[0].fallback, Fallback::Original);
    }

    #[test]
    fn validate_rejects_unknown_and_duplicate_clients() {
        let bad = TitleRoutes {
            enabled: true,
            rules: vec![TitleRule {
                enabled: true,
                clients: vec!["nope".into()],
                model: "m".into(),
                fallback: Fallback::Original,
            }],
        };
        assert!(validate(&bad).is_err());

        let dup = TitleRoutes {
            enabled: true,
            rules: vec![TitleRule {
                enabled: true,
                clients: vec![CLIENT_CODEX.into(), CLIENT_CODEX.into()],
                model: "m".into(),
                fallback: Fallback::Original,
            }],
        };
        assert!(validate(&dup).is_err());

        let live_without_model = TitleRoutes {
            enabled: true,
            rules: vec![TitleRule {
                enabled: true,
                clients: vec![CLIENT_CODEX.into()],
                model: String::new(),
                fallback: Fallback::Original,
            }],
        };
        assert!(validate(&live_without_model).is_err());
    }

    #[test]
    fn revision_matches_between_null_and_absent() {
        assert_eq!(revision(None), revision(Some(&Value::Null)));
    }
}
