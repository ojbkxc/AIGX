//! 工具调用参数健壮解析 —— 三层自修复管道。
//!
//! 借鉴 ds-free-api `tool_parser` 的"归一化 + 修复"思路（独立实现，不复制其代码）：
//! 部分上游/模型输出的工具参数可能混入全角符号或轻微 JSON 畸形。修复是尽力而为，
//! 任何一步失败都原样返回，绝不阻塞正常转发。
//!
//! 三层自修复策略（`repair_tool_arguments`，逐步降级，任何一层成功即返回）：
//! 1. 文本层：全角→半角归一化、剥离首尾包裹（```json 围栏 / XML 标签）
//! 2. 结构层：`repair_invalid_backslashes`（无效反斜杠转义）→
//!    `repair_unquoted_keys`（裸 key 加引号）→ 组合 `repair_json`
//! 3. 兜底层：去尾部逗号 → 原样返回

use serde_json::Value;

/// 全角 → 半角归一化（仅覆盖 JSON 语法相关的可逆符号）
fn normalize_fullwidth(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let n = match c {
            '：' => ':',
            '，' => ',',
            '；' => ';',
            '（' => '(',
            '）' => ')',
            '｛' => '{',
            '｝' => '}',
            '［' => '[',
            '］' => ']',
            '＂' => '"',
            '＇' => '\'',
            '＝' => '=',
            '｜' => '|',
            '　' => ' ',
            _ => c,
        };
        out.push(n);
    }
    out
}

fn try_parse(s: &str) -> Option<Value> {
    serde_json::from_str(s).ok()
}

/// 剥离首尾非 JSON 语法字符（如 ```json``` 围栏或 XML 标签包裹）
fn strip_wrapping(s: &str) -> &str {
    let start = s.find('{').or_else(|| s.find('[')).unwrap_or(0);
    let end = match s.rfind('}').or_else(|| s.rfind(']')) {
        Some(i) => i + 1,
        None => s.len(),
    };
    &s[start..end]
}

/// 去掉闭合括号前的多余逗号（如 `{"a":1,}` → `{"a":1}`）
fn fix_trailing_comma(s: &str) -> Option<String> {
    let t = s.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() < 2 {
        return None;
    }
    let last = *chars.last()?;
    if last != '}' && last != ']' {
        return None;
    }
    if *chars.get(chars.len() - 2)? != ',' {
        return None;
    }
    let mut fixed: String = chars[..chars.len() - 2].iter().collect();
    fixed.push(last);
    try_parse(&fixed).map(|_| fixed)
}

/// 修复无效反斜杠转义：合法的 JSON 转义（`\" \\ \/ \b \f \n \r \t \u`）保留，
/// 其余 `\x` 序列转义为 `\\x`（如 `C:\Users` → `C:\\Users`）。
fn repair_invalid_backslashes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&next)
                    if matches!(next, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') =>
                {
                    out.push('\\');
                    out.push(next);
                    chars.next();
                }
                Some(&next) => {
                    out.push('\\');
                    out.push('\\');
                    out.push(next);
                    chars.next();
                }
                None => {
                    out.push('\\');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// 为 `{`/`,` 后紧跟的裸 JSON key 补引号（如 `{name: 1}` → `{"name": 1}`）。
/// 仅当 key 后确为 `:` 时才加引号，避免误伤字符串字面量。
fn repair_unquoted_keys(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 32);
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        if (chars[i] == '{' || chars[i] == ',') && i + 1 < len {
            out.push(chars[i]);
            i += 1;
            while i < len && chars[i].is_whitespace() {
                out.push(chars[i]);
                i += 1;
            }
            if i < len && (chars[i].is_alphabetic() || chars[i] == '_') {
                let key_start = i;
                while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                if i < len && chars[i] == ':' {
                    out.push('"');
                    out.extend(&chars[key_start..i]);
                    out.push('"');
                } else {
                    out.extend(&chars[key_start..i]);
                    continue;
                }
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// 结构化修复：先反斜杠，再裸 key，任一步成功即返回。
fn repair_json(s: &str) -> Option<String> {
    let step1 = repair_invalid_backslashes(s);
    if serde_json::from_str::<Value>(&step1).is_ok() {
        return Some(step1);
    }
    let step2 = repair_unquoted_keys(&step1);
    if serde_json::from_str::<Value>(&step2).is_ok() {
        return Some(step2);
    }
    None
}

/// 修复工具调用参数 JSON，返回可安全下发的 arguments 字符串。
///
/// 三层自修复，逐步降级，任何一层成功即返回，全部失败则原样兜底：
/// 1. 原样即合法 → 直接返回
/// 2. 文本层：全角归一化 → 剥离包裹（```json 围栏 / XML 标签）
/// 3. 结构层：`repair_invalid_backslashes` → `repair_unquoted_keys` → `repair_json`
/// 4. 兜底层：去尾部逗号 → 原样返回
pub fn repair_tool_arguments(raw: &str) -> String {
    if try_parse(raw).is_some() {
        return raw.to_string();
    }
    let normalized = normalize_fullwidth(raw);
    if try_parse(&normalized).is_some() {
        return normalized;
    }
    let stripped = strip_wrapping(&normalized);
    if stripped != normalized {
        if try_parse(stripped).is_some() {
            return stripped.to_string();
        }
        if let Some(fixed) = repair_json(stripped) {
            return fixed;
        }
        if let Some(fixed) = fix_trailing_comma(stripped) {
            return fixed;
        }
    }
    if let Some(fixed) = repair_json(&normalized) {
        return fixed;
    }
    if let Some(fixed) = fix_trailing_comma(&normalized) {
        return fixed;
    }
    raw.to_string()
}

/// 读取流式工具调用增量，按 index 累积拼接参数片段，
/// 返回 `(index, id, function_name, 拼接后的完整 arguments)`。
///
/// OpenAI 流式协议将单个 tool_call 拆成多个 chunk：首个 chunk 携带
/// `id` + `function.name`，后续 chunk 仅携带 `function.arguments` 片段。
/// 上游可能输出畸形 JSON（裸 key、无效反斜杠），客户端期望完整且合法的
/// arguments，因此这里按 index 聚合并做与 `repair_tool_arguments` 一致的三层修复。
pub fn accumulate_tool_call_arguments(
    deltas: &[super::ToolCallDelta],
) -> Vec<(usize, Option<String>, Option<String>, String)> {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<usize, (Option<String>, Option<String>, String)> = BTreeMap::new();
    for d in deltas {
        let entry = acc.entry(d.index).or_default();
        // 首个 chunk 可能携带 id/name；后续 chunk 通常为 None，保留首个非 None
        if entry.0.is_none() {
            entry.0 = d.id.clone();
        }
        if entry.1.is_none() {
            entry.1 = d.function_name.clone();
        }
        if let Some(arg) = &d.arguments {
            entry.2.push_str(arg);
        }
    }
    acc.into_iter()
        .map(|(idx, (id, name, args))| {
            let repaired = repair_tool_arguments(&args);
            (idx, id, name, repaired)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_json_passthrough() {
        let raw = r#"{"city":"北京","days":3}"#;
        assert_eq!(repair_tool_arguments(raw), raw);
    }

    #[test]
    fn fullwidth_normalized() {
        let raw = r#"{"city"："北京"，"days"：3}"#;
        let fixed = repair_tool_arguments(raw);
        assert!(serde_json::from_str::<Value>(&fixed).is_ok());
        assert!(fixed.contains('"'));
    }

    #[test]
    fn wrapping_stripped() {
        let raw = "```json\n{\"a\":1}\n```";
        assert_eq!(repair_tool_arguments(raw), r#"{"a":1}"#);
    }

    #[test]
    fn trailing_comma_fixed() {
        let raw = r#"{"a":1,}"#;
        assert_eq!(repair_tool_arguments(raw), r#"{"a":1}"#);
    }

    #[test]
    fn unfixable_returns_raw() {
        let raw = "not json at all";
        assert_eq!(repair_tool_arguments(raw), raw);
    }

    #[test]
    fn invalid_backslash_repaired() {
        let raw = r#"{"path":"C:\Users\name"}"#;
        let fixed = repair_tool_arguments(raw);
        let v: Value = serde_json::from_str(&fixed).expect("backslash fix should parse");
        // 修复后 JSON 必须可解析；\U 被转义为 \\U（C:\Users 存活），
        // \n 是合法 JSON 转义，按原样保留（与 ds-free-api 行为一致）
        assert!(v["path"].as_str().unwrap().contains("C:\\Users"));
    }

    #[test]
    fn unquoted_keys_repaired() {
        let raw = r#"{city: "北京", days: 3}"#;
        let fixed = repair_tool_arguments(raw);
        let v: Value = serde_json::from_str(&fixed).expect("unquoted key fix should parse");
        assert_eq!(v["city"], "北京");
        assert_eq!(v["days"], 3);
    }

    #[test]
    fn combined_backslash_and_unquoted_keys() {
        let raw = r#"{path: "C:\Users\a"}"#;
        let fixed = repair_tool_arguments(raw);
        let v: Value = serde_json::from_str(&fixed).expect("combined repair should parse");
        // \U 无效转义 → 转义为 \\U；\a 无效转义 → \\a；路径字面量完整存活
        assert_eq!(v["path"], "C:\\Users\\a");
    }

    #[test]
    fn accumulate_joins_and_repairs() {
        let deltas = vec![
            super::super::ToolCallDelta {
                index: 0,
                id: Some("call_1".into()),
                function_name: Some("get_weather".into()),
                arguments: Some(r#"{city: "北"#.into()),
            },
            super::super::ToolCallDelta {
                index: 0,
                id: None,
                function_name: None,
                arguments: Some(r#"京"}"#.into()),
            },
        ];
        let out = super::super::tool_repair::accumulate_tool_call_arguments(&deltas);
        assert_eq!(out.len(), 1);
        let (idx, id, name, args) = &out[0];
        assert_eq!(*idx, 0);
        assert_eq!(id.as_deref(), Some("call_1"));
        assert_eq!(name.as_deref(), Some("get_weather"));
        let v: Value = serde_json::from_str(args).expect("accumulated+repaired should parse");
        assert_eq!(v["city"], "北京");
    }

    #[test]
    fn accumulate_multiple_indices() {
        let deltas = vec![
            super::super::ToolCallDelta {
                index: 0,
                id: Some("call_a".into()),
                function_name: Some("f_a".into()),
                arguments: Some(r#"{"a":1}"#.into()),
            },
            super::super::ToolCallDelta {
                index: 1,
                id: Some("call_b".into()),
                function_name: Some("f_b".into()),
                arguments: Some(r#"{"b":2}"#.into()),
            },
        ];
        let out = super::super::tool_repair::accumulate_tool_call_arguments(&deltas);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|(_, _, _, a)| serde_json::from_str::<Value>(a).is_ok()));
    }
}
