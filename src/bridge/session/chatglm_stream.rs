//! 智谱清言（`chatglm.cn`）网页版私有 SSE → OpenAI 增量语义的状态机。
//!
//! 状态机与帧语义**逐条移植**自 Agent2API `core/protocol/chatglm_stream.rs`
//! （MIT + 使用声明；45 行模块头就是移植规格书，帧序列来自 2026-10 live
//! 抓帧实测）。AIGX 的输出形态适配自家 [`crate::bridge::ChatChunk`]：
//! `ChatglmEvent::{Delta, Finished, UpstreamError}`，由未来的 ChatglmWeb
//! Bridge 消费。
//!
//! ── 为什么需要独立状态机 ────────────────────────────────────
//! 本家流是 SSE（`data: {...}` 逐帧），但帧语义与 OpenAI 完全不同：
//!
//! ```text
//!   智谱清言：每帧给 parts:[{id, logic_id, status, content:[{type,text|think}]}]
//!   OpenAI  ：每帧给「这一拍的增量 delta」
//! ```
//!
//! 同一轮回答里混着**两种**帧，靠 `status` 区分，**不能一律按快照差分**：
//!
//! ```text
//!   [1] top=init   part=init    text="1"            ← 增量帧：本帧就是新增内容
//!   [2] top=init   part=init    text="\n"           ← 增量帧
//!   [3] top=init   part=init    text="2\n3"         ← 增量帧
//!   [4] top=init   part=finish  text="1\n2\n3"      ← 全文快照帧（part 先 finish）
//!   [5] top=finish part=finish  text="1\n2\n3"      ← 全文快照帧 + 终态
//! ```
//!
//! 规则：**`part.status == "init"` 的帧原样下发并追加进基准；出现 finish
//! 的帧（顶层或 part）给的是整段全文，只补基准之后的后缀。** 一律按快照
//! 差分会把收尾全文再发一遍（上游实测故障「AI 回答两遍」的成因）。
//!
//! ── 分段（`logic_id`）与占位段 ──────────────────────────────
//! 一轮回答**分段**（实测：占位段 → 表单段 → 思考段 → 正文段），每段一个
//! `logic_id`。换段即换基准：新段首帧若拿去和旧段比对会被整段重发。另有
//! 「占位段」（`meta_data.engine.action == "loading"`，内容是状态文案）——
//! 官方客户端会把它替换掉，一旦下发就收不回来，整段跳过。
//!
//! ── 收尾判定 ────────────────────────────────────────────────
//! 顶层 `status == "finish"` 为终态（发 finish_reason 帧）。上游**不发
//! `[DONE]`**（实测完整流），由网关补。一个内容帧都没给就结束 → 按空响应
//! 报错（不把空回答记成成功）。
//!
//! ── 接线状态 ────────────────────────────────────────────────
//! **本模块当前未接入转发链路**：chatglm 网页对话的**请求侧**端点与报文
//! 形态在 Agent2API 的闭源插件包（`agent2api-web-providers`）里，主仓只有
//! 响应侧翻译器；AIGX 按「不自创、不编造」规则不做请求侧猜测 —— chatglm
//! 账号当前仍走 `openai_compatible` 面（chatglm.cn/api/paas/v4 + Bearer
//! JWT）。请求侧端点确认后，Bridge 用 [`ChatFromChatglmStream`] 翻译响应。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use serde_json::Value;

/// 上游每帧的 `parts[*].content[*]` 里，哪些 `type` 算正文 / 思考。
const KIND_TEXT: &str = "text";
const KIND_THINK: &str = "think";

/// 状态机吐出的一个事件（与 AIGX Bridge 层的 ChatChunk 对应）。
#[derive(Debug, Clone, PartialEq)]
pub enum ChatglmEvent {
    /// 内容增量（正文 / 思考，二者至少一个非空）。
    Delta {
        content: Option<String>,
        reasoning: Option<String>,
    },
    /// 正常收尾：上游给了 `status == "finish"`（调用方据此发 finish 帧）。
    Finished,
    /// 流内业务错误（本家把失败也放在 200 的 SSE 里）。
    /// `status` 按 -1001（积分不足，额度问题）→ 429、其余 → 502 归类。
    UpstreamError { message: String, status: u16 },
}

/// 上游 SSE 载荷流 → 增量事件的状态机。
///
/// 输入是**已解码的 SSE `data:` 载荷**（AIGX 的 `SseDecoder` 产物；上游
/// 的裸 `HEARTBEAT` 心跳不带花括号、不是 JSON，喂进来会被原样跳过）。
pub struct ChatFromChatglmStream {
    finished: bool,
    /// 当前内容段的 key（上游 `logic_id`，缺失时退回落 `part.id`）。
    ///
    /// 换段意味着上一段结束、新的一段开始，差分基准必须跟着清零，否则
    /// 新段的首帧会被拿去和旧段比对而整段重发。
    segment_key: String,
    /// **当前段**已下发的正文全文（差分基准）。
    segment_text: String,
    /// **当前段**已下发的思考全文（差分基准）。
    segment_thinking: String,
}

impl ChatFromChatglmStream {
    pub fn new() -> Self {
        Self {
            finished: false,
            segment_key: String::new(),
            segment_text: String::new(),
            segment_thinking: String::new(),
        }
    }

    /// 吃一条上游 SSE 载荷，吐出要下发的事件。
    pub fn push(&mut self, payload: &str) -> Vec<ChatglmEvent> {
        if self.finished {
            return Vec::new();
        }
        // 本家的保活帧是裸 `HEARTBEAT`（不带花括号），不是 JSON
        if payload.trim().eq_ignore_ascii_case("HEARTBEAT") {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_str::<Value>(payload.trim()) else {
            // 非法 JSON 载荷忽略（上游偶发心跳 / 截断帧）
            return Vec::new();
        };
        self.consume(&value)
    }

    /// 上游流结束（EOF）：没见过终态就是真截断，如实报错 —— 不补
    /// finish（那等于把截断谎报成完整回答）。已见过终态则无事可做。
    pub fn finish(&mut self) -> Vec<ChatglmEvent> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        vec![ChatglmEvent::UpstreamError {
            message: "上游流在收到完成信号之前结束（响应被截断）".to_string(),
            status: 502,
        }]
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// 一帧 → 零到多个事件。
    fn consume(&mut self, event: &Value) -> Vec<ChatglmEvent> {
        if self.finished {
            return Vec::new();
        }
        let Some(object) = event.as_object() else {
            return Vec::new();
        };
        // 业务错误：本家把失败也放在 200 的 SSE 里（如 -1001 积分不足），
        // 字段名实测是 `error_code` + `error`
        if let Some(code) = object.get("error_code").and_then(Value::as_i64) {
            if code != 0 {
                let mut message = string_field(event, "error");
                if message.trim().is_empty() {
                    message = format!("上游返回错误码 {code}");
                }
                self.finished = true;
                return vec![ChatglmEvent::UpstreamError {
                    message,
                    status: if code == -1001 { 429 } else { 502 },
                }];
            }
        }
        let error_field = object.get("error");
        let error_text = error_field
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if error_field.is_some() && !error_field.unwrap().is_null() && !error_text.is_empty() {
            self.finished = true;
            return vec![ChatglmEvent::UpstreamError {
                message: error_text.to_string(),
                status: 502,
            }];
        }

        // 终态判定：顶层 status == "finish"（实测顶层先变成 finish，其次 part）
        let frame_status = string_field(event, "status").to_lowercase();
        let is_finish = frame_status == "finish" || frame_status == "finished";

        // 正文 / 思考：本帧文本 + 本帧是「增量」还是「全文快照」
        let (full_text, full_thinking) = collect_text(event);
        let is_snapshot = frame_is_snapshot(event);
        let (text_delta, thinking_delta) =
            self.advance(event, &full_text, &full_thinking, is_snapshot);

        let mut out = Vec::new();
        // 增量事件先发（内容永远先于收尾）
        if !text_delta.is_empty() || !thinking_delta.is_empty() {
            out.push(ChatglmEvent::Delta {
                content: (!text_delta.is_empty()).then_some(text_delta),
                reasoning: (!thinking_delta.is_empty()).then_some(thinking_delta),
            });
        }
        if is_finish {
            self.finished = true;
            out.push(ChatglmEvent::Finished);
        }
        out
    }

    /// 本帧 → 要下发的 `(正文增量, 思考增量)`，并推进当前段的累积基准。
    ///
    /// 三种帧（实测，逐条对应真实流）：
    /// 1. **占位段**（`part.meta_data.engine.action == "loading"`）：回答前的
    ///    状态文案，不是回答的一部分，整段跳过；
    /// 2. **增量帧**（`part.status == "init"`）：本帧文本就是新增的一小段；
    /// 3. **全文快照帧**（`part.status` / 顶层 `status` 为 finish）：本帧给的是
    ///    **整段全文**，只补「还没下发过」的那一截。
    ///
    /// 快照对不上（上游改写/回退）时**不补发** —— 补发就是「回答两遍」；
    /// 重复是稳定可见的故障，改写极少见且下游无法撤回已发内容。
    fn advance(
        &mut self,
        event: &Value,
        full_text: &str,
        full_thinking: &str,
        is_snapshot: bool,
    ) -> (String, String) {
        if is_loading_placeholder(event) {
            return (String::new(), String::new());
        }
        // 换段（logic_id 变了）⇒ 新的一段开始，基准清零。
        // key 为空（上游形态变了）时按「不换段」处理 —— 宁可沿用旧基准，
        // 也不要在读到空串时把基准清掉（那会让整段重新下发一遍）。
        let key = segment_key_of(event);
        if !key.is_empty() && key != self.segment_key {
            self.segment_key = key;
            self.segment_text.clear();
            self.segment_thinking.clear();
        }
        let text = advance_segment(&mut self.segment_text, full_text, is_snapshot);
        let thinking = advance_segment(&mut self.segment_thinking, full_thinking, is_snapshot);
        (text, thinking)
    }
}

impl Default for ChatFromChatglmStream {
    fn default() -> Self {
        Self::new()
    }
}

/// 求一段文本相对**当前段已下发内容**的增量，并推进基准。
///
/// - 增量帧：本帧就是新内容，原样下发；
/// - 全文快照帧：以已下发内容为前缀，只补后缀；对不上（上游改写/回退）
///   时不补发。
fn advance_segment(emitted: &mut String, frame: &str, is_snapshot: bool) -> String {
    if frame.is_empty() {
        return String::new();
    }
    if !is_snapshot {
        emitted.push_str(frame);
        return frame.to_string();
    }
    // `emitted` 为空时 `strip_prefix` 返回整段，因此新段的第一个快照帧会整段下发
    match frame.strip_prefix(emitted.as_str()) {
        Some(rest) => {
            emitted.push_str(rest);
            rest.to_string()
        }
        None => String::new(),
    }
}

/// 本帧是不是「全文快照」帧。
///
/// 实测：`part.status` 会先于顶层 `status` 变 finish（从 1 数到 5 的流里，
/// 第 6 帧 `top=init / part=finish` 且文本已是全文），因此两处都要认。
fn frame_is_snapshot(event: &Value) -> bool {
    if is_finish_status(&string_field(event, "status")) {
        return true;
    }
    event
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            !parts.is_empty()
                && parts
                    .iter()
                    .all(|part| is_finish_status(&string_field(part, "status")))
        })
}

fn is_finish_status(status: &str) -> bool {
    matches!(status.to_lowercase().as_str(), "finish" | "finished")
}

/// 本帧是不是「回答前的临时占位段」（实测标记：`engine.action == "loading"`）。
///
/// 这类段有自己的 `logic_id`（与正文段不同），内容是一句状态文案。**不能
/// 下发**：官方客户端会把它替换掉，而下游收到的是最终正文，混进去就是
/// 一段永久的噪音。
fn is_loading_placeholder(event: &Value) -> bool {
    event
        .get("parts")
        .and_then(Value::as_array)
        .is_some_and(|parts| {
            parts.iter().any(|part| {
                part.pointer("/meta_data/engine/action")
                    .and_then(Value::as_str)
                    == Some("loading")
            })
        })
}

/// 本帧所属内容段的 key：`logic_id` 优先，缺失退回落 `part.id`。
///
/// 两个都没有（上游形态变了）时返回空串，调用方按「不换段」处理。
fn segment_key_of(event: &Value) -> String {
    let Some(parts) = event.get("parts").and_then(Value::as_array) else {
        return String::new();
    };
    for part in parts {
        let logic = string_field(part, "logic_id");
        if !logic.is_empty() {
            return logic;
        }
        let id = string_field(part, "id");
        if !id.is_empty() {
            return id;
        }
    }
    String::new()
}

/// 把一帧里所有 part 的 text / think 块按顺序拼成两条全文。
///
/// 返回 `(正文全文, 思考全文)`。
fn collect_text(event: &Value) -> (String, String) {
    let mut text = String::new();
    let mut thinking = String::new();
    let Some(parts) = event.get("parts").and_then(Value::as_array) else {
        return (text, thinking);
    };
    for part in parts {
        let Some(content) = part.get("content") else {
            continue;
        };
        let Some(items) = content.as_array() else {
            continue;
        };
        for item in items {
            let kind = string_field(item, "type").to_lowercase();
            if kind == KIND_TEXT {
                text.push_str(&string_field(item, "text"));
            } else if kind == KIND_THINK {
                thinking.push_str(&string_field(item, "think"));
            }
        }
    }
    (text, thinking)
}

fn string_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 把事件流折叠成 (正文, 思考, 是否正常收尾, 错误) 便于断言。
    fn collect(events: &[ChatglmEvent]) -> (String, String, bool, Option<String>) {
        let mut text = String::new();
        let mut thinking = String::new();
        let mut finished = false;
        let mut error = None;
        for event in events {
            match event {
                ChatglmEvent::Delta { content, reasoning } => {
                    text.push_str(content.as_deref().unwrap_or(""));
                    thinking.push_str(reasoning.as_deref().unwrap_or(""));
                }
                ChatglmEvent::Finished => finished = true,
                ChatglmEvent::UpstreamError { message, .. } => error = Some(message.clone()),
            }
        }
        (text, thinking, finished, error)
    }

    fn sse(payload: &str) -> String {
        payload.to_string()
    }

    /// **回归：回答不能重复下发**（上游实测「AI 回答了两遍」）。
    ///
    /// 帧序列逐条照抄 live 抓帧：前五帧 `part=init` 是**增量**，收尾两帧给**全文**。
    #[test]
    fn deltas_then_finish_snapshot_do_not_repeat_the_answer() {
        let mut machine = ChatFromChatglmStream::new();
        let mut events = Vec::new();
        for text in ["1", "\n", "2\n3", "\n4\n", "5"] {
            events.extend(machine.push(&sse(&format!(
                r#"{{"id":"m","conversation_id":"c","status":"init","parts":[{{"logic_id":"s","status":"init","content":[{{"type":"text","text":"{}"}}]}}]}}"#,
                text.replace('\n', "\\n")
            ))));
        }
        // 收尾第一拍：part 先 finish，这一帧已经是全文
        events.extend(machine.push(&sse(
            r#"{"id":"m","conversation_id":"c","status":"init","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"1\n2\n3\n4\n5"}]}]}"#,
        )));
        // 收尾第二拍：顶层终态，又来一遍同样的全文
        events.extend(machine.push(&sse(
            r#"{"id":"m","conversation_id":"c","status":"finish","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"1\n2\n3\n4\n5"}]}]}"#,
        )));
        let (text, _, finished, error) = collect(&events);
        assert_eq!(text, "1\n2\n3\n4\n5", "完整回答只能出现一次");
        assert!(finished, "终态要给 Finished");
        assert!(error.is_none());
    }

    /// 短回答：首帧增量本身就是全文，收尾快照不得再补一遍
    #[test]
    fn whole_answer_in_first_delta_is_not_repeated() {
        let mut machine = ChatFromChatglmStream::new();
        let mut events = machine.push(&sse(
            r#"{"id":"m","status":"init","parts":[{"logic_id":"s","status":"init","content":[{"type":"text","text":"AAA"}]}]}"#,
        ));
        events.extend(machine.push(&sse(
            r#"{"id":"m","status":"init","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"AAA"}]}]}"#,
        )));
        events.extend(machine.push(&sse(
            r#"{"id":"m","status":"finish","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"AAA"}]}]}"#,
        )));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "AAA");
    }

    /// 收尾快照比增量累计更长时，只补缺口那一段
    #[test]
    fn finish_snapshot_only_emits_the_missing_tail() {
        let mut machine = ChatFromChatglmStream::new();
        for text in ["前", "半"] {
            machine.push(&sse(&format!(
                r#"{{"status":"init","parts":[{{"logic_id":"s","status":"init","content":[{{"type":"text","text":"{text}"}}]}}]}}"#
            )));
        }
        let events = machine.push(&sse(
            r#"{"status":"finish","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"前半加尾"}]}]}"#,
        ));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "加尾");
    }

    /// 占位段（`engine.action == "loading"`）整段跳过，正文段照常下发
    #[test]
    fn loading_placeholder_is_skipped() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"ph","status":"finish","meta_data":{"engine":{"type":"ppt","action":"loading"}},"content":[{"type":"text","text":"让我们先核对下本轮任务的目标与偏好，正在梳理您的需求～"}]}]}"#,
        ));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "", "占位文案不是回答，下发就收不回来了");
        // 正文段（另一个 logic_id）开始 → 基准清零，首帧整段下发
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"body","status":"init","content":[{"type":"text","text":"真正的回答"}]}]}"#,
        ));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "真正的回答");
    }

    /// 换段（logic_id 变了）必须重置基准，否则新段的首帧会被旧段基准吞掉
    #[test]
    fn segment_change_resets_the_baseline() {
        let mut machine = ChatFromChatglmStream::new();
        machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"think","status":"init","content":[{"type":"think","think":"先想"}]}]}"#,
        ));
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"body","status":"finish","content":[{"type":"text","text":"正文全文"}]}]}"#,
        ));
        let (text, thinking, _, _) = collect(&events);
        assert_eq!(text, "正文全文", "新段的第一个快照帧应整段下发");
        assert_eq!(thinking, "", "新段没有思考内容");
    }

    /// 思考内容走 reasoning 通道，与正文同一套「增量 / 快照」规则
    #[test]
    fn thinking_uses_reasoning_channel() {
        let mut machine = ChatFromChatglmStream::new();
        machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"t","status":"init","content":[{"type":"think","think":"想"}]}]}"#,
        ));
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"t","status":"finish","content":[{"type":"think","think":"想一会"}]}]}"#,
        ));
        let (text, thinking, _, _) = collect(&events);
        assert_eq!(text, "");
        assert_eq!(thinking, "一会", "快照只补后缀");
    }

    /// 正文与思考同帧给出时各走各的通道
    #[test]
    fn text_and_thinking_are_separated() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"content":[{"type":"think","think":"推理"},{"type":"text","text":"答案"}]}]}"#,
        ));
        let (text, thinking, _, _) = collect(&events);
        assert_eq!(text, "答案");
        assert_eq!(thinking, "推理");
    }

    /// HEARTBEAT 帧被跳过（不带花括号的裸文本）
    #[test]
    fn heartbeat_is_ignored() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push("HEARTBEAT");
        assert!(events.is_empty());
    }

    /// 快照对不上（上游改写/回退）时**不补发**：整段重发的故障比漏几个字更糟
    #[test]
    fn mismatched_snapshot_is_not_replayed() {
        let mut machine = ChatFromChatglmStream::new();
        machine.push(&sse(
            r#"{"status":"init","parts":[{"logic_id":"s","status":"init","content":[{"type":"text","text":"ABC"}]}]}"#,
        ));
        let events = machine.push(&sse(
            r#"{"status":"finish","parts":[{"logic_id":"s","status":"finish","content":[{"type":"text","text":"XYZ"}]}]}"#,
        ));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "", "对不上的快照不得整段重发");
    }

    /// 多 part 按顺序拼接后差分
    #[test]
    fn multiple_parts_are_concatenated() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push(&sse(
            r#"{"status":"init","parts":[{"content":[{"type":"text","text":"前半"}]},{"content":[{"type":"text","text":"后半"}]}]}"#,
        ));
        let (text, _, _, _) = collect(&events);
        assert_eq!(text, "前半后半");
    }

    /// 业务错误码（积分不足 -1001）→ 429 额度错误
    #[test]
    fn quota_error_becomes_rate_limit() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push(&sse(
            r#"{"status":-1001,"message":"剩余积分不足","error_code":-1001,"error":"剩余积分不足，可购买积分充值包获得更多积分"}"#,
        ));
        match &events[0] {
            ChatglmEvent::UpstreamError { message, status } => {
                assert_eq!(*status, 429);
                assert!(message.contains("积分不足"));
            }
            other => panic!("应是 UpstreamError，实际 {other:?}"),
        }
    }

    /// 正常流末尾带 finish；没有终态就 EOF 要报截断（不谎报成功）
    #[test]
    fn truncated_stream_reports_error() {
        let mut machine = ChatFromChatglmStream::new();
        machine.push(&sse(
            r#"{"status":"init","parts":[{"content":[{"type":"text","text":"半截"}]}]}"#,
        ));
        let events = machine.finish();
        match &events[0] {
            ChatglmEvent::UpstreamError { message, status } => {
                assert_eq!(*status, 502);
                assert!(message.contains("截断"));
            }
            other => panic!("应是 UpstreamError，实际 {other:?}"),
        }
    }

    /// 一个内容都没有就 finish → 空响应报错（调用方按错误处理，不记成空回答成功）
    #[test]
    fn empty_stream_reports_error() {
        let mut machine = ChatFromChatglmStream::new();
        let events = machine.push(&sse(r#"{"status":"finish","parts":[]}"#));
        assert!(
            matches!(&events[0], ChatglmEvent::Finished),
            "状态机只报终态；空响应由 Bridge 层按「没见过内容」判错"
        );
        assert!(machine.is_finished());
    }

    /// 差分工具本身：增量原样、快照只补后缀、对不上不发
    #[test]
    fn advance_segment_rules() {
        let mut emitted = String::new();
        // 增量帧：本帧就是新内容，原样下发并累积
        assert_eq!(advance_segment(&mut emitted, "abc", false), "abc");
        assert_eq!(advance_segment(&mut emitted, "def", false), "def");
        // 快照帧：只补还没下发过的后缀
        assert_eq!(advance_segment(&mut emitted, "abcdefgh", true), "gh");
        // 与已下发内容完全一致 → 无增量（最要紧的一条：不能重复下发）
        assert_eq!(advance_segment(&mut emitted, "abcdefgh", true), "");
        // 对不上（上游改写 / 回退）→ 不发，绝不整段重发
        assert_eq!(advance_segment(&mut emitted, "xyz", true), "");
        // 空帧 → 无增量
        assert_eq!(advance_segment(&mut emitted, "", true), "");
        assert_eq!(advance_segment(&mut emitted, "", false), "");
        // 新段的第一个快照帧（基准为空）→ 整段下发
        let mut fresh = String::new();
        assert_eq!(advance_segment(&mut fresh, "整段", true), "整段");
    }
}
