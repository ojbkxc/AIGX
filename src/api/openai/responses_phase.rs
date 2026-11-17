//! Responses 助手消息的 `phase` 阶段标记（对齐 Agent2API `responses_phase.rs`）。
//!
//! ## 这个标记做什么
//!
//! Codex 桌面端只在最后一条助手消息带 `phase: "final_answer"` 时，才把这一轮
//! 的过程（中间正文、工具调用前的说明）折叠起来。网关把 Chat 转 Responses 时
//! 默认不带该字段，于是第三方模型的答复在 Codex 里会摊开成长条。
//!
//! 本模块只做**纯判定**，由调用方在开关开启且客户端识别为 Codex 时调用：
//! - 正常结束（`stop`）且没有工具调用、正文非空 → `final_answer`
//! - 有工具调用（`stop` / `tool_calls` / `function_call`）→ 工具调用前的正文补 `commentary`
//! - 错误、截断（`length` / `max_tokens` / `content_filter`）、缺结束证据 → 不补
//!
//! 「不补」是硬约束：猜错比不补更糟 —— 客户端会把过程正文误当最终答案折叠掉。

use serde_json::Value;

/// 截断 / 内容过滤的 `incomplete_details.reason`。
///
/// `None` = 不算未完成（正常结束或没有停止原因）。
pub(crate) fn incomplete_reason(finish: Option<&str>) -> Option<&'static str> {
    let finish = finish?.trim();
    let length = ["length", "max_tokens", "max_output_tokens"]
        .iter()
        .any(|word| finish.eq_ignore_ascii_case(word));
    if length {
        Some("max_output_tokens")
    } else if finish.eq_ignore_ascii_case("content_filter") {
        Some("content_filter")
    } else {
        None
    }
}

/// 判定一条助手消息该带什么 phase。
///
/// `completed` = 本轮已正常收尾（不是错误也不是截断）。`has_tool_calls` 看**本轮**
/// 是否产生了客户端工具调用。
pub(crate) fn message_phase(
    finish: Option<&str>,
    has_tool_calls: bool,
    completed: bool,
) -> Option<&'static str> {
    if !completed {
        return None;
    }
    let finish = finish?.trim();
    if finish.eq_ignore_ascii_case("stop") {
        Some(if has_tool_calls {
            "commentary"
        } else {
            "final_answer"
        })
    } else if has_tool_calls
        && (finish.eq_ignore_ascii_case("tool_calls")
            || finish.eq_ignore_ascii_case("function_call"))
    {
        Some("commentary")
    } else {
        None
    }
}

/// 响应体是否算「成功」（无 error / is_error 标记，status 为 completed 或缺失）。
pub(crate) fn response_is_successful(body: &Value) -> bool {
    let flag = |key: &str| {
        body.get(key).is_some_and(|value| match value {
            Value::Bool(flag) => *flag,
            Value::String(text) => !text.is_empty() && text != "false",
            Value::Number(number) => number.as_f64().is_some_and(|n| n != 0.0),
            // 对象/数组形态的 `error`（上游最常见的 `{"error":{"message":...}}`）
            // 本身就是「有错」的证据 —— 空对象也算（防御性：宁可判失败）。
            Value::Object(_) | Value::Array(_) => true,
            Value::Null => false,
        })
    };
    !flag("error")
        && !flag("is_error")
        && body
            .get("status")
            .is_none_or(|status| status.is_null() || status.as_str() == Some("completed"))
}

/// 把 phase 写进 output item（已有非空 phase / `final_answer` 配空正文时跳过）。
///
/// 「已有 phase 不覆盖」：上游原生就可能带了自己的阶段标记，尊重它。
/// 「空正文不补 final_answer」：补了会让客户端把那一条空消息当成最终答案。
pub(crate) fn apply(item: &mut Value, phase: Option<&str>) {
    let Some(phase) = phase else { return };
    if item.get("phase").is_some_and(|value| !value.is_null()) {
        return;
    }
    if phase == "final_answer" {
        let text = item.get("content").map(content_text).unwrap_or_default();
        if text.trim().is_empty() {
            return;
        }
    }
    if let Some(object) = item.as_object_mut() {
        object.insert("phase".into(), Value::String(phase.into()));
    }
}

/// 把 `content` 拍平成文本（字符串 / 文本块数组 / 其它 → 空）。
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 无工具调用的整段回复：补 `final_answer`。
    #[test]
    fn text_without_tool_calls_is_final_answer() {
        assert_eq!(
            message_phase(Some("stop"), false, true),
            Some("final_answer")
        );
        let mut item = json!({
            "type": "message",
            "content": [{ "type": "output_text", "text": "hi" }],
        });
        apply(&mut item, Some("final_answer"));
        assert_eq!(item["phase"], json!("final_answer"));
    }

    /// 工具调用前的正文只是过程说明：补 `commentary`（即使以 stop 结束）。
    #[test]
    fn text_before_tool_calls_is_commentary() {
        assert_eq!(message_phase(Some("stop"), true, true), Some("commentary"));
        assert_eq!(
            message_phase(Some("tool_calls"), true, true),
            Some("commentary")
        );
        assert_eq!(
            message_phase(Some("function_call"), true, true),
            Some("commentary")
        );
    }

    /// 截断 / 内容过滤 / 未完成 / 未知停止原因：一个字都不补。
    #[test]
    fn incomplete_or_unknown_finish_adds_no_phase() {
        assert_eq!(message_phase(Some("length"), false, true), None);
        assert_eq!(message_phase(Some("max_tokens"), false, true), None);
        assert_eq!(message_phase(Some("content_filter"), false, true), None);
        assert_eq!(message_phase(Some("stop"), false, false), None);
        assert_eq!(message_phase(None, false, true), None);
        assert_eq!(incomplete_reason(Some("length")), Some("max_output_tokens"));
        assert_eq!(
            incomplete_reason(Some("content_filter")),
            Some("content_filter")
        );
        assert_eq!(incomplete_reason(Some("stop")), None);
    }

    /// 空正文不补 `final_answer`（否则客户端把空消息当最终答案折叠）。
    #[test]
    fn empty_content_never_gets_final_answer() {
        let mut item = json!({ "type": "message", "content": [] });
        apply(&mut item, Some("final_answer"));
        assert!(item.get("phase").is_none());
        // 但 commentary 允许空（工具调用前的空说明无正文）
        let mut item = json!({ "type": "message", "content": [] });
        apply(&mut item, Some("commentary"));
        assert_eq!(item["phase"], json!("commentary"));
    }

    /// 上游自带 phase 不覆盖。
    #[test]
    fn existing_phase_is_not_overwritten() {
        let mut item = json!({ "type": "message", "phase": "final_answer", "content": "x" });
        apply(&mut item, Some("commentary"));
        assert_eq!(item["phase"], json!("final_answer"));
    }

    /// 错误 / 非 completed 状态的响应不算成功。
    #[test]
    fn failure_bodies_are_not_successful() {
        assert!(response_is_successful(&json!({ "status": "completed" })));
        assert!(response_is_successful(&json!({})));
        assert!(!response_is_successful(
            &json!({ "error": { "message": "x" } })
        ));
        assert!(!response_is_successful(&json!({ "is_error": true })));
        assert!(!response_is_successful(&json!({ "status": "failed" })));
        assert!(!response_is_successful(&json!({ "status": "incomplete" })));
    }
}
