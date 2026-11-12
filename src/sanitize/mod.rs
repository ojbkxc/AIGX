//! 出站指纹脱敏（对照 Agent2API `core/sanitize.rs`：剥离上游内容审核黑名单指纹）。
//!
//! ── 背景 ────────────────────────────────────────────────────
//! 客户端（Claude Code 类 CLI）在 system prompt 注入若干固定模板句，上游内容
//! 审核按**逐字精确匹配**拦截（非语义审核），一字改动即可绕过。策略：
//! 表头键值型指纹整段剥离；承载语义的模板句最小改写（换/插一个词，语义不变）。
//!
//! ── 规则集（内置硬编码 + 可配置用户词表）────────────────────
//! 内置规则是硬编码的静态数组（现行为，零变化）；在此之上支持管理员
//! 经 `/api/sanitize` 下发 `sanitize_rules` 用户词表（SPEC-1.0.49 §8），
//! 语义：`None`（缺省）= 内置规则；`Some([])` = 关闭全部脱敏；
//! `Some([...])` = 只用用户规则（内置不再生效）。用户词表走进程级快照
//! （[`set_rules`] / [`rules`]），转发层逐请求读取；开关
//! （`usage.sanitize_fingerprints`，默认开）同样在转发层读取。
//!
//! ── 与参考实现的差异（workaround 留痕）──────────────────────
//! 参考仓库的 `SANITIZE_REWRITES` 源码本身已被污染：「改写前」串（from）被
//! 它自己的净化规则改写成了「改写后」串（to），表头键名同样被 "header→hdr"
//! 缩写层污染。本文件按参考注释与幸存的 to 串反推原始规则——from 为客户端
//! 真实注入的指纹原文，to 为绕过上游逐字匹配的最小改写：
//! - `official CLI for Claude` → `official CLI tool for Claude`（插词，任务锚点）；
//! - `Default branch (you will usually use this for PRs)` →
//!   `Default branch (you will usually use this for PRs)`（换词；预检特征
//!   "Main branch (" 对应客户端注入形态）；
//! - Codex 身份句 `Codex CLI` → `Codex CLI tool`（插词，与身份句同型）；
//! - 反馈句 `To give feedback` → `To provide feedback`（参考注释明说 give→provide）；
//! - 裸数字 `11128`（上游反探测：请求体含其内部错误码即整单拦截）→ 插一个
//!   连字符改成 `11-128`（参考注释"改写保留可读性与指代"；零宽类字符实测
//!   被上游归一化剔除）；
//! - 表头键名 `x-anthropic-billing-header`：键值形态整段删除（以分号为分段
//!   边界）；残留的裸键名缩写为 `x-anthropic-billing-hdr`（参考注释"最小
//!   缩写 header→hdr"）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::bridge::ChatMessage;

/// 命中统计（P2-11）：进入净化的请求数 + 改写应用次数。
static STATS_HITS: AtomicU64 = AtomicU64::new(0);
static STATS_REWRITES: AtomicU64 = AtomicU64::new(0);

/// 特征预检：任一命中才进入净化（`str::contains` 快速路径，
/// 普通请求全不中 → 原样返回，零分配）。匹配大小写敏感（对应参考的
/// `strings.Contains` 口径）。
const SANITIZE_FEATURES: &[&str] = &[
    "x-anthropic-billing-header", // 表头键值段键名（完整拼写；hdr 是缩写后形态）
    "cc_entrypoint=",             // 尾随裸键值（截断前缀即可命中；可无表头键名）
    "You are Claude Code",        // 身份句（截断前缀即可命中）
    "Main branch (",              // 注入指令句（截断前缀即可命中）
    "You are a coding agent running in the Codex CLI", // Codex instructions 首段
    "github.com/anthropics/",     // 反馈句里的 Anthropic 仓库链接
    "11128",                      // 上游反探测：裸数字错误码
];

/// 改写层：全模板句逐字替换（每句只改一个词，语义不变）。
/// 匹配是大小写敏感的字面替换（对应 Go `strings.ReplaceAll`，与预检同口径）。
const SANITIZE_REWRITES: &[(&str, &str)] = &[
    // 身份句：真实模板无 tool 一词（上游黑名单按原句逐字拦截），插入后破坏匹配。
    (
        "You are Claude Code, Anthropic's official CLI for Claude",
        "You are Claude Code, Anthropic's official CLI tool for Claude",
    ),
    // git 上下文注入句：客户端注入 "Main branch (you will usually use this
    // for PRs)"，换词后语义不变（预检特征 "Main branch (" 是入站形态）。
    (
        "Main branch (you will usually use this for PRs)",
        "Default branch (you will usually use this for PRs)",
    ),
    // Codex instructions 首段：身份句同型插词。
    (
        "You are a coding agent running in the Codex CLI, a terminal-based coding assistant.",
        "You are a coding agent running in the Codex CLI tool, a terminal-based coding assistant.",
    ),
    (
        // 反馈句：整句带 Anthropic 仓库链接，上游按整句拦截（只留链接或只留
        // 半边均不拦，实测需整句同时出现）。give→provide 一词之差即可绕过。
        "To give feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
        "To provide feedback, users should report the issue at https://github.com/anthropics/claude-code/issues",
    ),
    (
        // 上游反探测：请求体里出现裸数字 11128 就整单拦截（与该数字的上下文
        // 无关："code=11128" / 裸 "11128" / "错误码 11128" 全部命中）。
        // 11128 正是本类拦截自身的错误码，上游据此识别"在讨论/回显其内部
        // 错误码"的请求。代价：用户对话中任何 11128 都会被改写——但这串数字
        // 出现在请求里本身就是拦截条件，不改写必然失败。插一个连字符保留
        // 可读性与指代（零宽类字符无效——实测上游会归一化剔除）。
        //
        // 注意：from 是**纯数字**，不是 `11-128`。参考实现源码的 from 串曾
        // 被其自身规则改写成 to 串，核对时以参考注释明确记载的原文 `11128`
        // 为准，不要把改写后的形态当成指纹原文。
        "11128",
        "11-128",
    ),
];

// ─── 正则（惰性编译一次）────────────────────────────────────
//
// `\s` 写成显式的 `[\t\n\f\r ]`、`\b` 用 `(?-u:...)` 关掉 Unicode 模式：
// 参考实现（Go 移植）里这两个类都是**纯 ASCII** 语义，与 Go 逐字对齐，
// 否则「上游实测能绕过」这条结论就不再成立。

fn hdr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 剥离层：表头键名即触发（与值无关），键值整段删除。
        // 值以分号为分段边界：`[^;\n]*` 而不是 `[^\n]*`，否则同一行分号后的
        // 其它键值也会被一并吞掉（过度删除）。
        Regex::new(r"(?i)x-anthropic-billing-header[\t\n\r ]*:[^;\n]*;?[\t\n\f\r ]*")
            .expect("header 剥离正则必须可编译")
    })
}

fn kv_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 剥离层：`cc_*=值` 键值段整段删除（尾随裸键值由调用方循环清理）。
        Regex::new(r"(?i)(?-u:\b)cc_[a-z0-9_]+=[^;\n]*;?[\t\n\f\r ]*")
            .expect("cc_ 键值正则必须可编译")
    })
}

fn bare_hdr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // 兜底层：裸键名（无冒号无值）同样是指纹——实测 assistant 消息里
        // 反引号引用裸键名即触发 11-128，而剥离层要求冒号、对裸串无效。
        // 键值形态被整段删除后，残留的裸键名做最小缩写（header→hdr）：
        // 破坏逐字匹配、语义不变、保留可读性。大小写不敏感，覆盖变体。
        Regex::new(r"(?i)x-anthropic-billing-header").expect("裸 header 正则必须可编译")
    })
}

/// 裸键名缩写目标（参考注释"最小缩写 header→hdr"）。
const BARE_HDR_REPLACEMENT: &str = "x-anthropic-billing-hdr";

/// 单段文本净化（保留原签名）。provider / role 缺省为空 → 仅无作用域的用户
/// 规则会生效；缺省（未配置用户词表）时走内置规则，行为与改造前完全一致。
pub fn sanitize_text(text: &str) -> String {
    sanitize_text_scoped("", "", text)
}

/// 内置规则净化。预检不中 → 原串返回（零分配）。
///
/// 顺序照抄参考实现：改写层 → 表头剥离层 → cc_ 键值循环清理 →
/// 裸键名缩写兜底。**顺序不能换**：改写在前是因为它要求原串完整（表头
/// 剥离会动到同一段文本的其它位置）；cc_ 循环在裸键名缩写之前，是因为
/// 缩写会把键名改掉、再清 cc_ 就认不出来了。
/// 与参考的差异：结尾不做 `trim()`（最小改写原则，不触碰未命中段的首尾空白）。
fn sanitize_text_builtin(text: &str) -> String {
    if !has_fingerprint(text) {
        return text.to_string();
    }
    // P2-11 命中统计：进入净化即计一次
    STATS_HITS.fetch_add(1, Ordering::Relaxed);
    let mut out = text.to_string();
    for (from, to) in SANITIZE_REWRITES {
        if out.contains(from) {
            tracing::debug!("指纹脱敏改写命中: {from:?}");
            STATS_REWRITES.fetch_add(1, Ordering::Relaxed);
            out = out.replace(from, to);
        }
    }
    let hdr = hdr_re();
    if hdr.is_match(&out) {
        tracing::debug!("指纹脱敏剥离 x-anthropic-billing-hdr 键值段");
        out = hdr.replace_all(&out, "").into_owned();
    }
    if out.contains("cc_") {
        let kv = kv_re();
        let mut prev = String::new();
        // 清尾随裸 kv：一次替换可能因相邻键值的新边界又暴露出下一个，
        // 循环到不动为止
        while prev != out {
            prev = out.clone();
            out = kv.replace_all(&out, "").into_owned();
        }
    }
    // 兜底：键值形态已在上面整段删除，这里只剩裸键名（引用/示例文本形态）。
    if bare_hdr_re().is_match(&out) {
        tracing::debug!("指纹脱敏缩写裸键名 x-anthropic-billing-hdr");
        out = bare_hdr_re()
            .replace_all(&out, BARE_HDR_REPLACEMENT)
            .into_owned();
    }
    out
}

/// 特征预检：先走 `contains` 快速路径（零分配）。`cc_entrypoint=` 是独立的
/// 裸键值特征（可能没有表头键名，必须单列否则整条净化被跳过）；表头键名还有
/// 大小写变体与裸键名形态，`contains` 大小写敏感会漏，必须再用 `(?i)` 正则兜底。
fn has_fingerprint(text: &str) -> bool {
    SANITIZE_FEATURES
        .iter()
        .any(|feature| text.contains(feature))
        || bare_hdr_re().is_match(text)
}

/// 净化一段多模态 content（保留原签名，provider/role 缺省为空）。
///
/// 仅测试使用（生产路径一律走带 provider/role 作用域的
/// [`sanitize_content_scoped`]）——因此 `cfg(test)` 标注，避免 dead_code 警告。
#[cfg(test)]
fn sanitize_content(value: &mut serde_json::Value) {
    sanitize_content_scoped("", "", value);
}

/// 净化一段多模态 content：字符串，或数组（只动带 `text` 字符串字段的分片）。
/// 与参考一致，**不判 `type`**：任何带 `text` 字符串的分片都过一遍。
fn sanitize_content_scoped(provider: &str, role: &str, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            let next = sanitize_text_scoped(provider, role, text);
            if next != *text {
                *text = next;
            }
        }
        serde_json::Value::Array(parts) => {
            for part in parts.iter_mut() {
                let Some(object) = part.as_object_mut() else {
                    continue;
                };
                if let Some(serde_json::Value::String(text)) = object.get_mut("text") {
                    let next = sanitize_text_scoped(provider, role, text);
                    if next != *text {
                        *text = next;
                    }
                }
            }
        }
        _ => {}
    }
}

/// 净化整个请求体（保留原签名；provider 缺省为空）。
pub fn strip(body: &mut serde_json::Value) {
    strip_for_provider("", body);
}

/// 净化整个请求体（serde_json 形态，供 Responses/Completions 等直接持有
/// `Value` 的转发路径使用，带 provider 作用域）。
///
/// 覆盖 `messages[]` 的 content / reasoning_content / tool_calls 参数
/// （arguments 是字符串化的 JSON，按文本净化即可）——与参考实现的作用面
/// 一致：顶层 `system`、`input` 等不在规则集的作用面。
pub fn strip_for_provider(provider: &str, body: &mut serde_json::Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for message in messages.iter_mut() {
        let Some(object) = message.as_object_mut() else {
            continue;
        };
        // 消息角色（用于按角色作用域的用户规则）；缺省空串 = 不匹配任何角色列表。
        let role = object
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        // content 与 tool_calls 各自独立判断：content 可为 null（工具调用轮），
        // 不能因 content 缺失跳过整条消息。
        if let Some(content) = object.get_mut("content") {
            sanitize_content_scoped(provider, &role, content);
        }
        // reasoning_content（思维链回填字段）实测同样携带指纹，同等净化。
        if let Some(serde_json::Value::String(reasoning)) = object.get_mut("reasoning_content") {
            let next = sanitize_text_scoped(provider, &role, reasoning);
            if next != *reasoning {
                *reasoning = next;
            }
        }
        if let Some(call_list) = object.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
            for call in call_list.iter_mut() {
                if let Some(serde_json::Value::String(arguments)) = call
                    .get_mut("function")
                    .and_then(|f| f.get_mut("arguments"))
                {
                    let next = sanitize_text_scoped(provider, &role, arguments);
                    if next != *arguments {
                        *arguments = next;
                    }
                }
            }
        }
    }
}

/// 净化归一化消息列表（保留原签名；provider 缺省为空）。
pub fn strip_chat_messages(messages: &mut [ChatMessage]) {
    sanitize_messages_for_provider("", messages);
}

/// 净化归一化消息列表（`ChatFormat` 无 serde 派生，桥接路径直接遍历，带
/// provider 作用域）。
///
/// 覆盖 content / 多模态 content_blocks 分片的 text / reasoning /
/// tool_calls 参数——与 [`strip_for_provider`] 的作用面对齐。openai
/// `chat_completions` 与 anthropic `messages` 两个主力端点共用本入口。
pub fn sanitize_messages_for_provider(provider: &str, messages: &mut [ChatMessage]) {
    for message in messages.iter_mut() {
        let role = role_str(message.role);
        if let Some(content) = message.content.as_deref() {
            let next = sanitize_text_scoped(provider, role, content);
            if next != content {
                message.content = Some(next);
            }
        }
        if let Some(blocks) = message.content_blocks.as_deref_mut() {
            for block in blocks.iter_mut() {
                if let Some(serde_json::Value::String(text)) = block.get_mut("text") {
                    let next = sanitize_text_scoped(provider, role, text);
                    if next != *text {
                        *text = next;
                    }
                }
            }
        }
        if let Some(reasoning) = message.reasoning.as_deref() {
            let next = sanitize_text_scoped(provider, role, reasoning);
            if next != reasoning {
                message.reasoning = Some(next);
            }
        }
        if let Some(calls) = message.tool_calls.as_deref_mut() {
            for call in calls.iter_mut() {
                let next = sanitize_text_scoped(provider, role, &call.arguments);
                if next != call.arguments {
                    call.arguments = next;
                }
            }
        }
    }
}

/// 命中统计快照（P2-11）：进入净化的请求数 + 改写应用次数。
pub fn snapshot() -> serde_json::Value {
    serde_json::json!({
        "hits": STATS_HITS.load(Ordering::Relaxed),
        "rewrites": STATS_REWRITES.load(Ordering::Relaxed),
    })
}

// ─── 用户可配置词表（SPEC-1.0.49 §8）─────────────────────────
//
// 对照 Agent2API `core/sanitize/rules.rs`（≤200 条、字面/regex、按角色与
// provider 作用域）。AIGX 侧只保留契约要求的字段（去掉参考实现的
// id/name/enabled/match_type 枚举，用 `is_regex` 布尔表达匹配类型）。
//
// 语义（配置项 `sanitize_rules`）：
// - `None`（缺省）= 用内置硬编码规则（现行为，零变化）；
// - `Some([])`     = 关闭全部脱敏；
// - `Some([...])`  = 只用用户规则（内置不再生效）。

/// 用户可配置脱敏词表的最大条数（对齐 Agent2API `MAX_RULES`）。超限必须报错，
/// 不静默截断。
pub const MAX_SANITIZE_RULES: usize = 200;

/// 单条用户脱敏规则。
///
/// `roles` / `providers` 为空表示「全部角色 / 全部 provider」；非空时规则
/// 只作用于命中列表的消息 / 渠道类型。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SanitizeRule {
    /// 匹配内容（字面串或正则源码）。
    #[serde(default)]
    pub pattern: String,
    /// 命中后替换成的文本。正则规则支持 `$1` 等捕获组引用（regex crate 语义）。
    #[serde(default)]
    pub replacement: String,
    /// `pattern` 按正则解释（true）还是按字面匹配（false）。
    #[serde(default)]
    pub is_regex: bool,
    /// 作用角色（`system` / `user` / `assistant` / `tool`）；空 = 全部。
    #[serde(default)]
    pub roles: Vec<String>,
    /// 作用 provider（渠道类型名）；空 = 全部。
    #[serde(default)]
    pub providers: Vec<String>,
}

/// 编译后的匹配器：字面替换走 `str::replace`，正则替换走 `Regex`。
enum Matcher {
    Literal,
    Regex(Regex),
}

/// 一条编译完成的规则（坏正则的条目已在 [`compile_rules`] 里被跳过）。
struct CompiledRule {
    pattern: String,
    matcher: Matcher,
    replacement: String,
    roles: Vec<String>,
    providers: Vec<String>,
}

impl CompiledRule {
    /// 该规则是否作用于 `(provider, role)`：两个作用域列表都为空 = 全部命中。
    fn applies(&self, provider: &str, role: &str) -> bool {
        (self.providers.is_empty() || self.providers.iter().any(|p| p == provider))
            && (self.roles.is_empty() || self.roles.iter().any(|r| r == role))
    }

    /// 命中则返回替换后的文本；未命中返回 `None`（调用方据此保守原串）。
    fn apply(&self, text: &str) -> Option<String> {
        match &self.matcher {
            Matcher::Literal => {
                if text.contains(&self.pattern) {
                    Some(text.replace(&self.pattern, &self.replacement))
                } else {
                    None
                }
            }
            Matcher::Regex(re) => {
                if re.is_match(text) {
                    Some(re.replace_all(text, self.replacement.as_str()).into_owned())
                } else {
                    None
                }
            }
        }
    }
}

/// 一份用户词表：`entries` 为原始规则（GET 回显/落盘往返），`compiled` 为
/// 编译产物（坏正则条目已被剔除，仅用于匹配）。
struct CompiledRules {
    entries: Vec<SanitizeRule>,
    compiled: Vec<CompiledRule>,
}

/// 用户词表进程级快照。外层 `None` = 未配置（走内置）；`Some` = 用户词表
/// （其 `compiled` 为空即「关闭脱敏」）。写少读多：读路径拷 `Arc` 后释放锁。
static USER_RULES: parking_lot::RwLock<Option<Arc<CompiledRules>>> = parking_lot::RwLock::new(None);

/// 编译用户词表：空 pattern 与编译失败的正则跳过并 `tracing::warn!`，不影响
/// 其它规则（SPEC §8 第 5 条）。
fn compile_rules(entries: &[SanitizeRule]) -> Vec<CompiledRule> {
    let mut compiled = Vec::with_capacity(entries.len());
    for (index, rule) in entries.iter().enumerate() {
        if rule.pattern.is_empty() {
            tracing::warn!("脱敏规则第 {} 条 pattern 为空，已跳过", index + 1);
            continue;
        }
        let matcher = if rule.is_regex {
            match Regex::new(&rule.pattern) {
                Ok(re) => Matcher::Regex(re),
                Err(error) => {
                    tracing::warn!("脱敏规则第 {} 条正则编译失败，已跳过：{error}", index + 1);
                    continue;
                }
            }
        } else {
            Matcher::Literal
        };
        compiled.push(CompiledRule {
            pattern: rule.pattern.clone(),
            matcher,
            replacement: rule.replacement.clone(),
            roles: rule.roles.clone(),
            providers: rule.providers.clone(),
        });
    }
    compiled
}

/// 写入用户词表快照。`None` = 恢复内置；`Some([])` = 关闭脱敏；`Some(非空)`
/// = 只用用户规则。超过 [`MAX_SANITIZE_RULES`] 条返回 `Err`（不静默截断）。
pub fn set_rules(rules: Option<Vec<SanitizeRule>>) -> Result<(), String> {
    let next = match rules {
        None => None,
        Some(entries) => {
            if entries.len() > MAX_SANITIZE_RULES {
                return Err(format!("脱敏词表最多 {MAX_SANITIZE_RULES} 条规则"));
            }
            let compiled = compile_rules(&entries);
            Some(Arc::new(CompiledRules { entries, compiled }))
        }
    };
    *USER_RULES.write() = next;
    Ok(())
}

/// 读取当前用户词表（`None` = 未配置 = 走内置；`Some([])` = 关闭）。
pub fn rules() -> Option<Vec<SanitizeRule>> {
    USER_RULES.read().as_ref().map(|set| set.entries.clone())
}

/// 取当前生效的用户词表（读锁内拷 `Arc` 即释放，匹配过程不持锁）。
fn active_rules() -> Option<Arc<CompiledRules>> {
    USER_RULES.read().as_ref().map(Arc::clone)
}

/// 角色枚举 → 小写角色名（与 [`SanitizeRule::roles`] 的取值口径一致）。
fn role_str(role: crate::bridge::Role) -> &'static str {
    use crate::bridge::Role;
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

/// 净化分派：未配置用户词表 → 内置规则（现行为）；已配置 → 只用用户规则。
fn sanitize_text_scoped(provider: &str, role: &str, text: &str) -> String {
    match active_rules() {
        None => sanitize_text_builtin(text),
        Some(set) => sanitize_text_with_rules(&set, provider, role, text),
    }
}

/// 按用户规则净化一段文本；无规则命中时原串返回（零改写）。
fn sanitize_text_with_rules(set: &CompiledRules, provider: &str, role: &str, text: &str) -> String {
    let mut out: Option<String> = None;
    for rule in &set.compiled {
        if !rule.applies(provider, role) {
            continue;
        }
        let current = out.as_deref().unwrap_or(text);
        if let Some(next) = rule.apply(current) {
            tracing::debug!("用户脱敏规则命中: {:?}", rule.pattern);
            STATS_REWRITES.fetch_add(1, Ordering::Relaxed);
            out = Some(next);
        }
    }
    match out {
        Some(next) => {
            STATS_HITS.fetch_add(1, Ordering::Relaxed);
            next
        }
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{ChatMessage, Role, ToolCall};

    // ── 用户可配置词表（SPEC-1.0.49 §8）────────────────────────
    //
    // 这些用例只走**纯核心**（`sanitize_text_with_rules` / `compile_rules`）与
    // `set_rules` 的「报错/复位」路径，绝不把全局快照置为 `Some`：`USER_RULES`
    // 是全进程共享的，置位会与并行执行的既有内置用例互相污染（内置分派依赖
    // 快照为 `None`）。全局接线正确性由「分派函数只多一层 match」保证。

    fn rule(pattern: &str, replacement: &str, is_regex: bool) -> SanitizeRule {
        SanitizeRule {
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            is_regex,
            roles: Vec::new(),
            providers: Vec::new(),
        }
    }

    fn compiled(rules: &[SanitizeRule]) -> CompiledRules {
        CompiledRules {
            entries: rules.to_vec(),
            compiled: compile_rules(rules),
        }
    }

    #[test]
    fn user_literal_rule_rewrites_positive_control() {
        // 正向对照：证明「替换确实生效」，避免后续「不变」断言是假绿。
        let set = compiled(&[rule("SECRET", "[redacted]", false)]);
        let out = sanitize_text_with_rules(&set, "", "system", "value SECRET here");
        assert_eq!(out, "value [redacted] here");
    }

    #[test]
    fn empty_user_rules_disable_sanitize() {
        // Given 一段会被某条用户规则命中的文本
        let text = "token sk-abc here";
        // 正向对照：非空用户词表确实改写同一文本
        let hit = compiled(&[rule("sk-abc", "<key>", false)]);
        assert_eq!(
            sanitize_text_with_rules(&hit, "", "", text),
            "token <key> here"
        );
        // When 用户词表为空（= 关闭全部脱敏）
        let empty = compiled(&[]);
        // Then 同一文本原样返回，不再被改写
        assert_eq!(sanitize_text_with_rules(&empty, "", "", text), text);
    }

    #[test]
    fn regex_user_rule_rewrites() {
        // Given 一条正则用户规则（带量词）
        let set = compiled(&[rule(r"secret-\d+", "***", true)]);
        // Then 正则命中并替换
        assert_eq!(
            sanitize_text_with_rules(&set, "", "", "token secret-12345 here"),
            "token *** here"
        );
    }

    #[test]
    fn provider_scope_isolates_rules() {
        // Given 一条只作用于 openai 的规则
        let mut only_openai = rule("sk-abc", "<key>", false);
        only_openai.providers = vec!["openai".to_string()];
        let set = compiled(&[only_openai]);
        // Then 命中 provider 才改写
        assert_eq!(
            sanitize_text_with_rules(&set, "openai", "", "key sk-abc"),
            "key <key>"
        );
        // 非命中 provider 原样（作用域隔离）
        assert_eq!(
            sanitize_text_with_rules(&set, "anthropic", "", "key sk-abc"),
            "key sk-abc"
        );
    }

    #[test]
    fn role_scope_isolates_rules() {
        // Given 一条只作用于 system 的规则
        let mut system_only = rule("internal-token", "<redacted>", false);
        system_only.roles = vec!["system".to_string()];
        let set = compiled(&[system_only]);
        // Then system 命中改写、user 不动
        assert_eq!(
            sanitize_text_with_rules(&set, "", "system", "internal-token"),
            "<redacted>"
        );
        assert_eq!(
            sanitize_text_with_rules(&set, "", "user", "internal-token"),
            "internal-token"
        );
    }

    #[test]
    fn too_many_rules_is_rejected_not_truncated() {
        // Given 201 条（超上限 1 条）
        let too_many: Vec<SanitizeRule> = (0..=MAX_SANITIZE_RULES)
            .map(|i| rule(&format!("p{i}"), "x", false))
            .collect();
        assert_eq!(too_many.len(), MAX_SANITIZE_RULES + 1);
        // Then 超限报错，而不是静默截断（写入口返回 Err，未落快照）
        assert!(set_rules(Some(too_many)).is_err());
        // 正好 200 条可编译（不报错、不截断）
        let just_right: Vec<SanitizeRule> = (0..MAX_SANITIZE_RULES)
            .map(|i| rule(&format!("q{i}"), "x", false))
            .collect();
        assert_eq!(compile_rules(&just_right).len(), MAX_SANITIZE_RULES);
        // 复位为「内置」：值与缺省相同，不影响并行用例
        set_rules(None).expect("复位内置必须成功");
    }

    #[test]
    fn bad_regex_skipped_good_rule_still_applies() {
        // Given 一条非法正则 + 一条好字面规则
        let set = compiled(&[
            rule("(", "x", true),       // 非法正则：应跳过
            rule("hello", "hi", false), // 好规则：应生效
        ]);
        // Then 坏正则被剔除（只剩 1 条编译产物），未影响好规则
        assert_eq!(set.compiled.len(), 1);
        // 且好规则照常生效（正向对照）
        assert_eq!(
            sanitize_text_with_rules(&set, "", "", "hello world"),
            "hi world"
        );
    }

    #[test]
    fn builtin_default_unchanged_for_scoped_entry() {
        // 内置缺省不变（正向对照）：provider 缺省的分派结果与旧入口逐字节一致。
        // 文本含纯 ASCII 的内置特征（表头键名），内置预检必命中。
        let text = "see x-anthropic-billing-header here";
        let mut legacy = [msg(Role::System, Some(text))];
        let mut scoped = [msg(Role::System, Some(text))];
        strip_chat_messages(&mut legacy);
        sanitize_messages_for_provider("", &mut scoped);
        assert_eq!(
            legacy[0].content, scoped[0].content,
            "provider 缺省两入口结果必须一致"
        );
        assert_ne!(
            legacy[0].content.as_deref(),
            Some(text),
            "内置必须命中并改写该文本"
        );
    }

    #[test]
    fn rule_serde_defaults_and_roundtrip() {
        // 契约：缺省字段可全部省略（is_regex 缺省 false，roles/providers 空 = 全部）
        let parsed: SanitizeRule =
            serde_json::from_value(serde_json::json!({"pattern": "a", "replacement": "b"}))
                .expect("缺省字段必须可解析");
        assert_eq!(parsed, rule("a", "b", false));
        // 序列化往返稳定（settings GET/PUT 依赖）
        let back = serde_json::to_value(&parsed).expect("规则必须可序列化");
        assert_eq!(back["pattern"], "a");
        assert_eq!(back["is_regex"], false);
        assert_eq!(back["roles"], serde_json::json!([]));
        assert_eq!(back["providers"], serde_json::json!([]));
    }

    /// 构造一条带文本的桥接消息，减少测试样板。
    fn msg(role: Role, content: Option<&str>) -> ChatMessage {
        ChatMessage {
            role,
            content: content.map(str::to_string),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    #[test]
    fn no_fingerprint_returns_input() {
        // Given 一段不含任何指纹特征的普通文本
        let text = "hello world, nothing to sanitize";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 一个字节都不改（预检零分配快速路径）
        assert_eq!(out, text);
    }

    #[test]
    fn numeric_error_code_11128_rewritten() {
        // Given 上游反探测的裸错误码原文
        let text = "upstream returned code 11128";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 必须破坏逐字匹配，而不是把真正的指纹漏放出去
        assert_eq!(out, "upstream returned code 11-128");
        assert!(!out.contains("11128"));
    }

    #[test]
    fn hyphenated_version_range_untouched() {
        // Given 用户文本里本来就是带连字符的版本区间（不是指纹）
        let text = "supported versions: 11-128";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 预检不命中，原样返回（避免误伤正常对话）
        assert_eq!(out, text);
    }

    #[test]
    fn standalone_cc_entrypoint_kv_removed() {
        // Given 只有 cc_ 键值、没有表头键名的一段文本（旧预检会漏）
        let text = "cc_entrypoint=cli;";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 整段删除，而不是因为预检不中而原样漏出
        assert_eq!(out, "");
    }

    #[test]
    fn header_kv_segment_removed_keeps_tail() {
        // Given 表头键值段后还有其它非指纹内容
        let text = "x-anthropic-billing-header: cc_version=2.1.0; keep this";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只删到分号为止，后面的内容不能被吞掉
        assert_eq!(out, "keep this");
    }

    #[test]
    fn bare_header_key_abbreviated() {
        // Given assistant 历史里反引号引用的裸键名（无冒号无值）
        let text = "refer to `x-anthropic-billing-header` in docs";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 最小缩写，破坏逐字匹配且保留可读性
        assert_eq!(out, "refer to `x-anthropic-billing-hdr` in docs");
    }

    #[test]
    fn mixed_case_bare_header_abbreviated() {
        // Given 大小写变体（contains 快速路径不命中，靠 (?i) 正则兜底）
        let text = "X-Anthropic-Billing-Header";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 仍然缩写为统一形态
        assert_eq!(out, "x-anthropic-billing-hdr");
    }

    #[test]
    fn claude_identity_sentence_rewritten() {
        // Given 客户端注入的身份句（带句号收尾）
        let text = "You are Claude Code, Anthropic's official CLI for Claude.";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只插一个词，原有句尾标点保留
        assert_eq!(
            out,
            "You are Claude Code, Anthropic's official CLI tool for Claude."
        );
    }

    #[test]
    fn multimodal_text_block_sanitized() {
        // Given content 为多模态数组，只有 text 分片带指纹
        let mut value = serde_json::json!([
            {"type": "text", "text": "cc_entrypoint=cli;"},
            {"type": "image_url", "image_url": {"url": "http://example.com/a.png"}}
        ]);
        // When 走 content 净化
        sanitize_content(&mut value);
        // Then 只改带 text 的分片，非文本分片原样保留
        assert_eq!(value[0]["text"], "");
        assert_eq!(value[1]["image_url"]["url"], "http://example.com/a.png");
    }

    #[test]
    fn tool_call_arguments_sanitized() {
        // Given 工具调用消息的 content 为 null、arguments 是字符串化 JSON
        let mut body = serde_json::json!({
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "function": {
                        "name": "Read",
                        "arguments": "{\"cmd\":\"echo cc_entrypoint=cli;\"}"
                    }
                }]
            }]
        });
        // When 走请求体净化
        strip(&mut body);
        // Then arguments 里的键值段被剥离（不能因 content 缺失跳过整条消息）
        let arguments = body["messages"][0]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments 应为字符串");
        assert!(!arguments.contains("cc_entrypoint="));
    }

    #[test]
    fn reasoning_content_sanitized() {
        // Given assistant 消息的 reasoning_content 携带指纹
        let mut body = serde_json::json!({
            "messages": [{
                "role": "assistant",
                "content": "ok",
                "reasoning_content": "code 11128"
            }]
        });
        // When 走请求体净化
        strip(&mut body);
        // Then 与 content 同等净化
        assert_eq!(body["messages"][0]["reasoning_content"], "code 11-128");
    }

    #[test]
    fn chat_message_fields_sanitized() {
        // Given content / content_blocks / reasoning / tool_calls 各带一处指纹
        let mut message = msg(Role::Assistant, Some("plain"));
        message.content_blocks = Some(vec![serde_json::json!({
            "type": "text",
            "text": "cc_entrypoint=cli;"
        })]);
        message.reasoning = Some("code 11128".to_string());
        message.tool_calls = Some(vec![ToolCall {
            id: "c1".to_string(),
            function_name: "Read".to_string(),
            arguments: "{\"p\":\"cc_entrypoint=cli;\"}".to_string(),
        }]);
        let mut messages = [message];
        // When 走桥接消息净化
        strip_chat_messages(&mut messages);
        // Then 四类字段全部生效
        assert_eq!(messages[0].content.as_deref(), Some("plain"));
        assert_eq!(messages[0].content_blocks.as_ref().unwrap()[0]["text"], "");
        assert_eq!(messages[0].reasoning.as_deref(), Some("code 11-128"));
        let tool_arguments = &messages[0].tool_calls.as_ref().unwrap()[0].arguments;
        assert!(!tool_arguments.contains("cc_entrypoint="));
    }

    #[test]
    fn leading_trailing_whitespace_preserved() {
        // Given 带首尾空白的文本（AIGX 有意区别于参考实现的末尾 trim）
        let text = "  cc_entrypoint=cli;x  ";
        // When 走单段净化
        let out = sanitize_text(text);
        // Then 只删键值段，不触碰未命中段的首尾空白
        assert_eq!(out, "  x  ");
    }
}
