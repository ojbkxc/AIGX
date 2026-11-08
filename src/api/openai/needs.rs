//! 请求体 → 能力需求（#40 路由多规则 §1「能力过滤」的数据面**推导层**）。
//!
//! ## 本模块只做一件事
//!
//! 读请求体里的**布尔信号**（这次请求要不要工具 / 要不要图像输入 / 要不要
//! 视频输入），填进 [`RequestNeeds`] 的三个字段。**「这个信号对应哪个能力
//! 标签」不在这里**：唯一的映射链是
//! `RequestNeeds::required_tags()`（`channel/routing_policy.rs`）→
//! `model::capability::CapabilityMask::from_tags` —— 本模块一个字面标签名都
//! 不写（写了就是第二套词表，「目录说支持、选路说不支持」的无声分裂由此而来）。
//!
//! ## 口径来源
//!
//! 对齐上游 Agent2API `routing_groups::RequestNeeds::from_body`
//! （`desktop-tauri/src-tauri/server/src/core/routing_groups.rs:17`）—— 字段判定
//! 同形，便于逐字段回归比对。在此基础上加两处 AIGX 侧信号（本仓把
//! `/v1/images/*` 独立成 handler，请求体里没有 `messages`）：
//!
//! - 图像类端点（[`image_endpoint`]）：端点本身就是图像语义，不看 body；
//! - `input` 列表（[`from_chat_body`]）：OpenAI Responses 形态把消息放在
//!   `input` 而不是 `messages`，逐条扫描口径与 `messages` 一致。
//!
//! ## 性能与降级
//!
//! 纯内存字段读取：`Value` 是**已解析好**的 body，本模块零分配、零 IO、
//! 不做第二次序列化；只判空 / 借用读 `as_str` / `as_array`。
//! 请求体里没有信号时返回 `RequestNeeds::default()`（三布尔全 false）
//! = **不做能力过滤**，绝不默认拒绝（#40-B 降级口径「宁放勿误伤」）。

use crate::channel::routing_policy::RequestNeeds;
use serde_json::Value;

/// 消息 content 数组里「这一段是图片输入」的 part type（大小写不敏感）。
///
/// `image_url` = OpenAI chat / `input_image` = OpenAI Responses /
/// `image` = Anthropic Messages —— 三家同一个含义，三个写法。
const IMAGE_PART_TYPES: [&str; 3] = ["image_url", "input_image", "image"];

/// 同上，视频输入（上游 `from_body` 的三写法）。
const VIDEO_PART_TYPES: [&str; 3] = ["video_url", "input_video", "video"];

/// 图像类端点（`/v1/images/generations`、`/v1/images/edits`）的能力需求。
///
/// 不需要解析 body：edits 的输入图走 multipart（不在 JSON body 里），
/// generations 的语义由路径本身给定。端点级口径按 #40 工单取「图像类端点 →
/// `images`」；即便推不出（例如老账号目录无能力标签），下游也是**放行**
/// （见模块头「降级」），不会误伤。
pub(crate) fn image_endpoint() -> RequestNeeds {
    RequestNeeds {
        images: true,
        ..RequestNeeds::default()
    }
}

/// chat / messages / responses 类请求体 → 能力需求。
///
/// - `tools` / `functions` 非空数组 → `tools`；
/// - `tool_choice` 非空且不是显式 `"none"` → `tools`
///   （`"none"` 是「明确不要工具」，按「宁放勿误伤」不计需求）；
/// - 任一消息：`role` ∈ {`tool`, `function`}、`function_call` 存在、
///   `tool_calls` 非空、`type == "function_call_output"`（Responses 的工具
///   回灌形态）→ `tools`；
/// - 任一 content part 的 `type` 命中 [`IMAGE_PART_TYPES`] → `images`，
///   命中 [`VIDEO_PART_TYPES`] → `video`。
///
/// 逐条消息扫描 `messages`（chat / Anthropic）与 `input`（Responses）两个
/// 列表：两个都扫而不是「二选一」，因为供应商侧对同一份 body 的形态并不统一，
/// 漏扫的代价是**静默少过滤**（宁放勿误伤方向），但会让能力过滤对那条链路
/// 完全失效 —— 那正是本层要修的东西。
pub(crate) fn from_chat_body(body: &Value) -> RequestNeeds {
    let mut needs = RequestNeeds {
        tools: non_empty_array(body, "tools") || non_empty_array(body, "functions"),
        ..RequestNeeds::default()
    };
    if !needs.tools {
        needs.tools = match body.get("tool_choice") {
            // 显式 "none"：客户端明确关掉了工具调用，不算需求
            Some(Value::String(s)) => !s.eq_ignore_ascii_case("none"),
            Some(Value::Null) | None => false,
            // 对象形态（{"type":"function","function":{...}}）/ 其它取值
            Some(_) => true,
        };
    }
    for list in [body.get("messages"), body.get("input")] {
        for item in list.and_then(Value::as_array).into_iter().flatten() {
            scan_item(item, &mut needs);
        }
    }
    needs
}

/// 单条消息 / 输入项 → 需求（就地累加，不分配）。
fn scan_item(item: &Value, needs: &mut RequestNeeds) {
    if !needs.tools {
        needs.tools = matches!(
            item.get("role").and_then(Value::as_str),
            Some("tool" | "function")
        ) || item.get("function_call").is_some()
            || non_empty_array(item, "tool_calls")
            || item.get("type").and_then(Value::as_str) == Some("function_call_output");
    }
    for part in item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(kind) = part.get("type").and_then(Value::as_str) else {
            continue;
        };
        if IMAGE_PART_TYPES
            .iter()
            .any(|t| kind.eq_ignore_ascii_case(t))
        {
            needs.images = true;
        } else if VIDEO_PART_TYPES
            .iter()
            .any(|t| kind.eq_ignore_ascii_case(t))
        {
            needs.video = true;
        }
    }
}

/// `container[key]` 是非空数组（字段缺失 / 非数组 / 空数组一律 `false`）。
fn non_empty_array(container: &Value, key: &str) -> bool {
    container
        .get(key)
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::capability::CapabilityMask;
    use serde_json::json;

    /// 把需求走**唯一映射链**换成能力位（resolve 侧接的就是这一条）。
    fn mask(needs: RequestNeeds) -> CapabilityMask {
        CapabilityMask::from_tags(needs.required_tags())
    }

    #[test]
    fn plain_chat_has_no_needs() {
        // 降级口径的锚点：读不出信号 = 空需求 = 不做能力过滤（不是拒绝）
        let body = json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
        });
        let needs = from_chat_body(&body);
        assert!(needs.is_empty());
        assert_eq!(mask(needs), CapabilityMask::EMPTY);
        // 无 messages / 空 body 同样为空
        assert!(from_chat_body(&json!({})).is_empty());
    }

    #[test]
    fn tools_field_and_tool_choice() {
        let with_tools = json!({"tools": [{"type": "function", "function": {"name": "f"}}]});
        assert!(from_chat_body(&with_tools).tools);
        // 空数组 / 非数组不算
        assert!(!from_chat_body(&json!({"tools": []})).tools);
        assert!(!from_chat_body(&json!({"tools": "f"})).tools);
        // legacy functions 字段同样认
        assert!(from_chat_body(&json!({"functions": [{"name": "f"}]})).tools);
        // tool_choice：对象/required 算，显式 none 不算
        assert!(from_chat_body(&json!({"tool_choice": "required"})).tools);
        assert!(from_chat_body(&json!({"tool_choice": {"type": "function"}})).tools);
        assert!(!from_chat_body(&json!({"tool_choice": "none"})).tools);
        assert!(!from_chat_body(&json!({"tool_choice": null})).tools);
    }

    #[test]
    fn tool_role_and_tool_calls_mark_tools() {
        // 工具回灌：role=tool / role=function / function_call / tool_calls
        for body in [
            json!({"messages": [{"role": "tool", "content": "{}"}]}),
            json!({"messages": [{"role": "function", "content": "{}"}]}),
            json!({"messages": [{"role": "assistant", "function_call": {"name": "f"}}]}),
            json!({"messages": [{"role": "assistant", "tool_calls": [{"id": "1"}]}]}),
        ] {
            assert!(from_chat_body(&body).tools, "应识别为工具需求: {body}");
        }
        // assistant 带空 tool_calls 不算
        assert!(
            !from_chat_body(&json!({"messages": [{"role": "assistant", "tool_calls": []}]})).tools
        );
    }

    #[test]
    fn content_parts_mark_images_and_video() {
        // 三家写法的图片 part 都认
        for kind in IMAGE_PART_TYPES {
            let part = json!({"type": kind, "image_url": {"url": "data:image/png;base64,AA"}});
            let body = json!({"messages": [{"role": "user", "content": [
                {"type": "text", "text": "看图"},
                part,
            ]}]});
            let needs = from_chat_body(&body);
            assert!(needs.images, "{kind} 应识别为图像需求");
            assert!(!needs.tools && !needs.video);
            assert!(mask(needs).covers(CapabilityMask::VISION));
        }
        // 视频 part → video 位（本仓暂无视频端点，只从消息体推）
        for kind in VIDEO_PART_TYPES {
            let body = json!({"messages": [{"role": "user", "content": [{"type": kind}]}]});
            let needs = from_chat_body(&body);
            assert!(needs.video, "{kind} 应识别为视频需求");
            assert!(mask(needs).covers(CapabilityMask::VIDEO));
        }
        // 纯文本 / 字符串 content / 无 type 的 part：不误判
        assert!(
            from_chat_body(&json!({"messages": [{"role": "user", "content": "hi"}]})).is_empty()
        );
        assert!(from_chat_body(
            &json!({"messages": [{"role": "user", "content": [{"text": "x"}]}]})
        )
        .is_empty());
    }

    #[test]
    fn responses_input_shape_is_scanned() {
        // OpenAI Responses：消息在 `input`，工具回灌是 function_call_output
        let body = json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": [
                    {"type": "input_text", "text": "看图"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AA"},
                ]},
                {"type": "function_call_output", "call_id": "c1", "output": "{}"},
            ],
        });
        let needs = from_chat_body(&body);
        assert!(needs.images);
        assert!(needs.tools);
        let m = mask(needs);
        assert!(m.covers(CapabilityMask::VISION));
        assert!(m.covers(CapabilityMask::TOOL));
        assert!(!m.covers(CapabilityMask::VIDEO));
    }

    #[test]
    fn image_endpoint_needs_images_only() {
        let needs = image_endpoint();
        assert!(needs.images);
        assert!(!needs.tools && !needs.video);
        assert_eq!(mask(needs), CapabilityMask::VISION);
    }

    #[test]
    fn all_three_bits_wire_to_the_capability_mask() {
        // 接线锚点：三个布尔各自映射到唯一映射链上的那一位（掩码即 resolve
        // 侧粗筛/精筛的输入），顺序与 required_tags 的实现无关。
        let needs = RequestNeeds {
            tools: true,
            images: true,
            video: true,
        };
        assert_eq!(
            mask(needs),
            CapabilityMask::TOOL
                .union(CapabilityMask::VISION)
                .union(CapabilityMask::VIDEO)
        );
    }
}
