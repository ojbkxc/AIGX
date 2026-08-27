//! 工具调用参数健壮解析。
//!
//! 借鉴 ds-free-api `tool_parser` 的"归一化 + 修复"思路（独立实现，不复制其代码）：
//! 部分上游/模型输出的工具参数可能混入全角符号或轻微 JSON 畸形。修复是尽力而为，
//! 任何一步失败都原样返回，绝不阻塞正常转发。

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

/// 修复工具调用参数 JSON，返回可安全下发的 arguments 字符串。
///
/// 策略（逐步降级）：原样 → 全角归一化 → 剥离包裹 → 去尾部逗号 → 原样兜底。
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
        if let Some(fixed) = fix_trailing_comma(stripped) {
            return fixed;
        }
        if try_parse(stripped).is_some() {
            return stripped.to_string();
        }
    }
    if let Some(fixed) = fix_trailing_comma(&normalized) {
        return fixed;
    }
    raw.to_string()
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
}
