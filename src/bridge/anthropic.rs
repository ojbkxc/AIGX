//! Anthropic Messages API 原生 Bridge — 对接 Anthropic 原生 `/v1/messages`。
//!
//! 参照 aisix `provider-anthropic`（`bridge.rs` + `wire.rs`）的协议适配：
//! - 鉴权头：`x-api-key: <key>` + `anthropic-version: 2023-06-01`（不是 Bearer）
//! - `system` 提升为顶层字段，不混入 messages
//! - `max_tokens` 必填，缺失时用 `ChatFormat::anthropic_max_tokens()` 兜底 4096
//! - tools 用 Anthropic 形状 `{name, description, input_schema}`（非 OpenAI 的
//!   `{type:"function", function:{...}}`）
//! - `stop_sequences` / `top_k` 透传
//!
//! 与 `OpenaiCompatibleBridge` 的区别：后者把所有渠道当 OpenAI 兼容上游
//! （`/chat/completions` + Bearer），对接真正的 Anthropic 原生 API 会 401/400。
//! 本 Bridge 专供 `ChannelType::Anthropic` 渠道，AIGX 自身的 `/v1/messages`
//! 入口经 `ChatFormat` 归一化后可真正透传到 Anthropic 原生上游。

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::{
    capture_upstream_error_http, Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream,
    ChatDelta, ChatFormat, ChatMessage, ChatResponse, FinishReason, Role, UpstreamErrorView,
    UpstreamWire, UsageStats,
};

/// Anthropic API 版本头（与官方 SDK 一致）
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Anthropic 原生上游 Bridge。
///
/// 持有上游 base_url 与 api_key，将归一化 `ChatFormat` 转为 Anthropic
/// `/v1/messages` 请求体，执行 HTTP 调用并解析响应（含 SSE 流式）。
pub struct AnthropicBridge {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicBridge {
    /// 构造 Bridge，复用外部传入的 `reqwest::Client`（应来自 AppState.http_client）。
    pub fn with_client(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            // 归一化 base_url：去掉末尾斜杠；裸 host 自动补 /v1（与 OpenaiCompatibleBridge
            // 同规则，避免拼出 `https://api.anthropic.com/messages`）。
            base_url: super::openai::normalize_base_url(base_url.into()),
            api_key: api_key.into(),
            client,
        }
    }

    fn messages_url(&self) -> String {
        format!("{}/messages", self.base_url.trim_end_matches('/'))
    }

    /// OpenAI `tool_choice` → Anthropic `tool_choice`。
    ///
    /// 参照 new-api `MapOpenAIToolChoice`：
    /// - `"auto"` → `{"type":"auto"}`
    /// - `"required"` → `{"type":"any"}`
    /// - `"none"` → `{"type":"none"}`
    /// - `{"type":"function","function":{"name":X}}` → `{"type":"tool","name":X}`
    /// - 已是 Anthropic 形状（含 `type` 且为 auto/any/none/tool）→ 原样透传
    fn translate_tool_choice(tc: &Value) -> Option<Value> {
        if let Some(s) = tc.as_str() {
            return match s {
                "auto" | "required" | "none" => {
                    let ty = if s == "required" { "any" } else { s };
                    Some(serde_json::json!({ "type": ty }))
                }
                _ => None,
            };
        }
        if let Some(obj) = tc.as_object() {
            // 已是 Anthropic 形状：{type: auto|any|none|tool, name?: ...}
            if let Some(ty) = obj.get("type").and_then(|t| t.as_str()) {
                if matches!(ty, "auto" | "any" | "none" | "tool") {
                    return Some(tc.clone());
                }
            }
            // OpenAI 形状：{type:"function", function:{name}}
            if obj.get("type").and_then(|t| t.as_str()) == Some("function") {
                if let Some(name) = obj
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                {
                    return Some(serde_json::json!({ "type": "tool", "name": name }));
                }
            }
        }
        None
    }

    /// OpenAI `reasoning_effort` → Claude `thinking`。
    ///
    /// 参照 new-api `RenderClaude`：effort low/medium/high 映射到 budget_tokens
    /// 启发式（这里用经验值，无模型元数据时保守取小值）。
    fn translate_reasoning_effort(effort: &Option<String>) -> Option<Value> {
        let effort = effort.as_deref()?;
        let budget = match effort {
            "low" => 1024,
            "high" => 8192,
            _ => 4096,
        };
        Some(serde_json::json!({
            "type": "enabled",
            "budget_tokens": budget,
        }))
    }

    /// OpenAI `web_search_options` → Claude `web_search_20250305` 内置工具。
    ///
    /// 参照 new-api：`{user_location:{approximate:{...}}}` 时注入带
    /// `user_location` 的 web_search 工具；否则注入纯 `{type,name}`。
    fn translate_web_search_tool(options: &Option<Value>) -> Option<Value> {
        let options = options.as_ref()?;
        let mut tool = serde_json::Map::new();
        tool.insert("type".into(), Value::String("web_search_20250305".into()));
        tool.insert("name".into(), Value::String("web_search".into()));
        if let Some(approx) = options
            .get("user_location")
            .and_then(|u| u.get("approximate"))
        {
            let mut loc = serde_json::Map::new();
            loc.insert("type".into(), Value::String("approximate".into()));
            for key in ["timezone", "country", "region", "city"] {
                if let Some(v) = approx.get(key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        loc.insert(key.into(), Value::String(v.to_string()));
                    }
                }
            }
            tool.insert("user_location".into(), Value::Object(loc));
        }
        Some(Value::Object(tool))
    }

    /// 将归一化 `ChatFormat` 转为 Anthropic `/v1/messages` 请求体。
    ///
    /// 参照 aisix `split_system` + `build_request`：
    /// - 连续的头部 system 消息合并为顶层 `system` 字符串
    /// - 非头部 system 消息降级为 user 轮次（保留语义不丢弃）
    /// - assistant 轮的 `tool_calls` 翻译为 `tool_use` 块（多轮工具回放）
    /// - tool 角色消息翻译为 `tool_result` 块
    fn build_body(&self, req: &ChatFormat, stream: bool) -> Value {
        // 参照 new-api：合并连续同角色纯文本消息。Anthropic Messages API 要求
        // user/assistant 交替，连续同角色会 400；此处先合并，避免拒绝。
        let mut merged: Vec<ChatMessage> = Vec::new();
        for m in &req.messages {
            if let Some(last) = merged.last_mut() {
                let both_plain = last.content_blocks.is_none()
                    && last.tool_calls.is_none()
                    && m.content_blocks.is_none()
                    && m.tool_calls.is_none();
                if both_plain && last.role == m.role && last.role != Role::Tool {
                    let prev = last.content.clone().unwrap_or_default();
                    let cur = m.content.clone().unwrap_or_default();
                    if !prev.is_empty() || !cur.is_empty() {
                        last.content = Some(if prev.is_empty() {
                            cur
                        } else if cur.is_empty() {
                            prev
                        } else {
                            format!("{} {}", prev, cur)
                        });
                        continue;
                    }
                }
            }
            merged.push(m.clone());
        }

        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();
        let mut seen_non_system = false;

        for m in &merged {
            match m.role {
                Role::System => {
                    if seen_non_system {
                        // 非头部 system：降级为 user 轮次；空内容兜底 "..."（参照
                        // new-api 的空 content 占位，Anthropic 拒绝空字符串）。
                        let text = m.content_str();
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": if text.is_empty() { "..." } else { text },
                        }));
                    } else {
                        system_parts.push(m.content_str().to_string());
                    }
                }
                Role::User => {
                    seen_non_system = true;
                    messages.push(user_message(m));
                }
                Role::Assistant => {
                    seen_non_system = true;
                    messages.push(assistant_message(m));
                }
                Role::Tool => {
                    seen_non_system = true;
                    let tool_use_id = m.tool_call_id.as_deref().unwrap_or("");
                    // 参照 new-api：tool_result 合并进前一条 user 轮次（若存在），
                    // 否则独立成一条 user 轮次；空 content 兜底 "..."。
                    let result_content = m.content_str();
                    let result_content = if result_content.is_empty() { "..." } else { result_content };
                    let result_block = serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": result_content,
                    });
                    if let Some(last) = messages.last_mut() {
                        if last.get("role").and_then(|r| r.as_str()) == Some("user") {
                            // 前一条 user 的 content 是数组 → 直接追加 tool_result
                            if let Some(arr) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                                arr.push(result_block);
                                continue;
                            }
                            // 前一条 user 的 content 是字符串 → 转为数组并追加（参照
                            // new-api：把 string content 提升为 content blocks 后合并）
                            if let Some(text) = last
                                .get("content")
                                .and_then(|c| c.as_str())
                                .map(String::from)
                            {
                                last["content"] = serde_json::json!([
                                    {"type": "text", "text": text},
                                    result_block,
                                ]);
                                continue;
                            }
                        }
                    }
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": [result_block],
                    }));
                }
            }
        }

        // Anthropic 要求首条消息为 user；首条为 assistant/tool 时补占位 user
        // （参照 new-api placeholderUserMessage）。
        let first_role = messages
            .first()
            .and_then(|m| m.get("role"))
            .and_then(|r| r.as_str());
        if !messages.is_empty() && first_role != Some("user") {
            messages.insert(
                0,
                serde_json::json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "..."}],
                }),
            );
        }

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            // Anthropic 要求 max_tokens 必填；缺失兜底 4096
            "max_tokens": req.anthropic_max_tokens(),
            "stream": stream,
        });
        if !system_parts.is_empty() {
            body["system"] = Value::String(system_parts.join("\n\n"));
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        if let Some(k) = req.top_k {
            body["top_k"] = serde_json::json!(k);
        }
        if let Some(stop) = &req.stop {
            body["stop_sequences"] = serde_json::json!(stop);
        }
        if let Some(tools) = &req.tools {
            let translated = translate_tools(tools);
            if !translated.is_empty() {
                body["tools"] = Value::Array(translated);
            }
        }
        // web_search_options → web_search_20250305 内置工具（参照 new-api
        // `OpenAIChatRequestToClaudeMessages` 的 WebSearchOptions 注入）。
        if let Some(web) = Self::translate_web_search_tool(&req.web_search_options) {
            let mut arr = body
                .get("tools")
                .and_then(|t| t.as_array())
                .cloned()
                .unwrap_or_default();
            arr.push(web);
            body["tools"] = Value::Array(arr);
        }
        // tool_choice：OpenAI 形状 → Anthropic 形状（参照 new-api
        // `MapOpenAIToolChoice`：auto→auto、required→any、none→none、
        // {function:{name}}→{type:"tool",name}）。已是 Anthropic 形状则原样透传。
        if let Some(tc) = &req.tool_choice {
            if let Some(translated) = Self::translate_tool_choice(tc) {
                body["tool_choice"] = translated;
            }
        }
        // reasoning_effort → Claude thinking（参照 new-api `ApplyReasoning`）。
        // 仅当请求体未显式携带原生 `thinking`（extra 合并前已透传）时翻译，
        // 避免覆盖用户显式指定。low/medium/high → budget_tokens 启发式映射。
        // 同时按 new-api `ConstrainThinkingSampling`：注入 thinking 时若客户端
        // 未显式传采样参数则清除，避免 Anthropic 因 temperature/top_k 与
        // thinking 冲突而 400。
        if body.get("thinking").is_none() {
            if let Some(thinking) = Self::translate_reasoning_effort(&req.reasoning_effort) {
                body["thinking"] = thinking;
                if req.temperature.is_none() {
                    body.as_object_mut().map(|o| o.remove("temperature"));
                }
                if req.top_k.is_none() {
                    body.as_object_mut().map(|o| o.remove("top_k"));
                }
            }
        }
        // extra（metadata 等）原样合并到顶层
        if let Some(extra) = &req.extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }
        body
    }

    /// 构建带鉴权头的 POST 请求（`x-api-key` + `anthropic-version`）。
    fn post(&self, url: &str, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self.client.post(url).json(body);
        if !self.api_key.is_empty() {
            req = req.header("x-api-key", &self.api_key);
        }
        req = req.header("anthropic-version", ANTHROPIC_VERSION);
        req
    }
}

/// 用户消息：优先转发 typed content blocks（OpenAI 形状 → Anthropic 形状），
/// 否则单文本块。Anthropic 拒绝空 content 数组，空时兜底 `"..."` 占位
/// （参照 new-api 的空 content 兜底）。
///
/// 参照 new-api `OpenAIChatRequestToClaudeMessages` 的媒体转换：
/// - `text` 块原样保留
/// - `image_url`（base64 data URI）→ Anthropic `image` 块
/// - `image_url`（`input_image` 形状）→ Anthropic `image` 块
/// - PDF 等文档 → Anthropic `document` 块
/// - 无法解析（http(s) URL 需异步下载，build_body 为同步）→ 静默丢弃
fn user_message(m: &ChatMessage) -> Value {
    if let Some(blocks) = &m.content_blocks {
        let mut converted: Vec<Value> = Vec::new();
        for b in blocks {
            match b.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    let text = b.get("text").and_then(|t| t.as_str()).unwrap_or("");
                    if !text.is_empty() {
                        converted.push(serde_json::json!({"type": "text", "text": text}));
                    }
                }
                Some("image_url") => {
                    if let Some(img) = translate_image_block(b) {
                        converted.push(img);
                    }
                }
                _ => {}
            }
        }
        if !converted.is_empty() {
            return serde_json::json!({ "role": "user", "content": converted });
        }
    }
    let text = m.content_str();
    serde_json::json!({
        "role": "user",
        "content": [{"type": "text", "text": if text.is_empty() { "..." } else { text }}],
    })
}

/// OpenAI 图片块 → Anthropic `image` / `document` 块。
///
/// 仅支持 base64 data URI（同步可解析）。http(s) URL 需要异步下载，AIGX 的
/// `build_body` 为同步函数无法处理，故静默丢弃（与 new-api 下载失败时跳过一致）。
fn translate_image_block(b: &Value) -> Option<Value> {
    // 两种 OpenAI 形状：
    // 1. {"type":"image_url","image_url":{"url":"data:image/png;base64,..."}}
    // 2. {"type":"input_image","image_url":"data:..."}（Responses API 形状）
    let url = b
        .get("image_url")
        .and_then(|u| {
            u.as_str()
                .map(String::from)
                .or_else(|| u.get("url").and_then(|x| x.as_str()).map(String::from))
        })?;

    let (media_type, data) = parse_data_uri(&url)?;

    let block_type = if media_type == "application/pdf" {
        "document"
    } else {
        "image"
    };
    Some(serde_json::json!({
        "type": block_type,
        "source": {
            "type": "base64",
            "media_type": media_type,
            "data": data,
        }
    }))
}

/// 解析 `data:<mime>;base64,<payload>` 为 `(mime, payload)`。
fn parse_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let comma = rest.find(',')?;
    let meta = &rest[..comma];
    let payload = &rest[comma + 1..];
    if !meta.ends_with(";base64") {
        return None;
    }
    let mime = meta.trim_end_matches(";base64").to_string();
    if mime.is_empty() || payload.is_empty() {
        return None;
    }
    Some((mime, payload.to_string()))
}

/// 助手消息：文本块 + tool_calls 翻译为 tool_use 块。
fn assistant_message(m: &ChatMessage) -> Value {
    let mut content: Vec<Value> = Vec::new();
    let text = m.content_str();
    if !text.is_empty() {
        content.push(serde_json::json!({"type": "text", "text": text}));
    }
    if let Some(tool_calls) = &m.tool_calls {
        for tc in tool_calls {
            let input: Value = serde_json::from_str(&tc.arguments)
                .unwrap_or_else(|_| Value::Object(Default::default()));
            content.push(serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.function_name,
                "input": input,
            }));
        }
    }
    if content.is_empty() {
        content.push(serde_json::json!({"type": "text", "text": "..."}));
    }
    serde_json::json!({ "role": "assistant", "content": content })
}

/// OpenAI 形状 tools → Anthropic 形状 tools：
/// `{type:"function",function:{name,description,parameters}}`
/// → `{name,description,input_schema}`
fn translate_tools(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|t| {
            if t.get("type").and_then(|v| v.as_str()) != Some("function") {
                return None;
            }
            let function = t.get("function")?.as_object()?;
            let name = function.get("name")?.as_str()?;
            let mut tool = serde_json::Map::new();
            tool.insert("name".into(), name.into());
            if let Some(desc) = function.get("description") {
                tool.insert("description".into(), desc.clone());
            }
            // 参照 new-api `FunctionParametersToInputSchema`：parameters →
            // input_schema，缺失 type/properties 时补默认值。
            if let Some(params) = function.get("parameters") {
                tool.insert("input_schema".into(), normalize_input_schema(params));
            }
            Some(Value::Object(tool))
        })
        .collect()
}

/// 参照 new-api `FunctionParametersToInputSchema`：补默认 `type: object` 与
/// `properties: {}`，保证 Anthropic input_schema 合法。
fn normalize_input_schema(params: &Value) -> Value {
    let mut schema = params.clone();
    if let Some(obj) = schema.as_object_mut() {
        if !obj.contains_key("type") {
            obj.insert("type".into(), Value::String("object".into()));
        }
        if !obj.contains_key("properties") {
            obj.insert(
                "properties".into(),
                Value::Object(Default::default()),
            );
        }
    }
    schema
}

/// 解析 Anthropic 非流式响应为内部 `ChatResponse`。
///
/// 参照 aisix `response_into_chat_response`：
/// - text 块拼接为 message.content
/// - tool_use 块翻译为 message.tool_calls（arguments 为 JSON 字符串）
/// - stop_reason 映射 finish_reason
/// - usage 含 cache_creation/cache_read
fn parse_response(json: &Value, fallback_model: &str) -> ChatResponse {
    let mut saw_text = false;
    let text = json
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        saw_text = true;
                        b.get("text").and_then(|t| t.as_str()).map(String::from)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let content = if saw_text { Some(text) } else { None };

    // 非流式 thinking 块（扩展思考模型）：映射为 reasoning（参照 new-api
    // `ResponseClaude2OpenAI` 的 thinking 块处理）。
    let reasoning = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                    b.get("thinking").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
        })
        .filter(|s| !s.is_empty());

    let tool_calls = json
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| {
            let calls: Vec<super::ToolCall> = arr
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                        return None;
                    }
                    Some(super::ToolCall {
                        id: b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                        function_name: b
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: b
                            .get("input")
                            .map(|i| match i {
                                Value::Null => "{}".to_string(),
                                other => serde_json::to_string(other)
                                    .unwrap_or_else(|_| "{}".to_string()),
                            })
                            .unwrap_or_else(|| "{}".to_string()),
                    })
                })
                .collect();
            if calls.is_empty() {
                None
            } else {
                Some(calls)
            }
        })
        .map(|calls: Vec<super::ToolCall>| {
            calls
                .into_iter()
                .map(|mut tc| {
                    tc.arguments = super::tool_repair::repair_tool_arguments(&tc.arguments);
                    tc
                })
                .collect()
        });

    let mut msg = ChatMessage::assistant(content.unwrap_or_default());
    msg.tool_calls = tool_calls;
    msg.reasoning = reasoning;

    let finish_reason = json
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .map(parse_stop_reason)
        .unwrap_or(FinishReason::Stop);

    let usage = parse_usage(json);

    ChatResponse {
        id: json
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("msg")
            .to_string(),
        model: json
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(fallback_model)
            .to_string(),
        message: msg,
        finish_reason,
        usage,
    }
}

/// Anthropic stop_reason → 内部 FinishReason
///
/// 参照 new-api `reasonmap.ClaudeStopReasonToOpenAIFinishReason`：
/// - `end_turn` / `stop_sequence` → stop
/// - `max_tokens` → length
/// - `tool_use` → tool_calls
/// - `pause_turn` → length（服务端可续轮的未完成响应，视作截断而非成功停止）
/// - `refusal` → content_filter
fn parse_stop_reason(s: &str) -> FinishReason {
    match s {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" | "pause_turn" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

/// 从 Anthropic 响应解析 usage（含 cache_creation/cache_read）
fn parse_usage(json: &Value) -> UsageStats {
    let u = json.get("usage");
    let prompt = u
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = u
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_creation = u
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let cache_read = u
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_prompt_tokens: 0,
        cache_creation_tokens: cache_creation,
        cache_read_tokens: cache_read,
    }
}

/// 解析 Anthropic 错误响应：`{type:"error", error:{type, message}}`
fn parse_anthropic_error(body: &[u8]) -> Option<UpstreamErrorView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    Some(UpstreamErrorView {
        kind: err.get("type").and_then(|t| t.as_str()).map(String::from),
        message: err
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from),
        code: None,
        param: None,
    })
}

/// 解析单条 Anthropic SSE 事件 JSON 为 0 或 1 个 `ChatChunk`。
///
/// 参照 aisix `StreamState::to_chunk`：只对 `content_block_delta.text_delta`
/// 和终态 `message_delta` 产出 chunk；其他事件（content_block_start/stop、
/// ping、message_start/message_stop）的状态由中间层流式编码器处理，
/// 这里无需重复发块。
fn parse_stream_event(
    payload: &str,
    id: &str,
    model: &str,
    state: &mut StreamState,
) -> Option<std::result::Result<ChatChunk, BridgeError>> {
    let v: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(BridgeError::UpstreamDecode(format!(
                "failed to parse Anthropic SSE event: {e}"
            ))));
        }
    };

    let event_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

    // 捕获 message_start 中的 prompt token / cache 计数
    if event_type == "message_start" {
        if let Some(message) = v.get("message") {
            if let Some(usage) = message.get("usage") {
                state.id = message
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or(id)
                    .to_string();
                state.model = message
                    .get("model")
                    .and_then(|m| m.as_str())
                    .unwrap_or(model)
                    .to_string();
                state.input_tokens = usage
                    .get("input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                state.cache_creation_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
                state.cache_read_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0);
            }
        }
        return None;
    }

    // 终态 message_delta：产出 finish_reason + usage
    if event_type == "message_delta" {
        // max-wins 累积 input/cache 计数（部分后端只在此帧携带）
        if let Some(usage) = v.get("usage") {
            if let Some(t) = usage.get("input_tokens").and_then(|t| t.as_u64()) {
                state.input_tokens = state.input_tokens.max(t);
            }
            if let Some(t) = usage
                .get("cache_creation_input_tokens")
                .and_then(|t| t.as_u64())
            {
                state.cache_creation_tokens = state.cache_creation_tokens.max(t);
            }
            if let Some(t) = usage.get("cache_read_input_tokens").and_then(|t| t.as_u64()) {
                state.cache_read_tokens = state.cache_read_tokens.max(t);
            }
        }
        let finish = v
            .get("delta")
            .and_then(|d| d.get("stop_reason"))
            .and_then(|s| s.as_str())
            .map(parse_stop_reason);
        let completion = v
            .get("usage")
            .and_then(|u| u.get("output_tokens"))
            .and_then(|t| t.as_u64());
        let usage = completion.map(|c| UsageStats {
            prompt_tokens: state.input_tokens,
            completion_tokens: c,
            total_tokens: state.input_tokens + c,
            cached_prompt_tokens: 0,
            cache_creation_tokens: state.cache_creation_tokens,
            cache_read_tokens: state.cache_read_tokens,
        });
        if finish.is_none() && usage.is_none() {
            return None;
        }
        return Some(Ok(ChatChunk {
            id: state.id.clone(),
            model: state.model.clone(),
            delta: ChatDelta::default(),
            finish_reason: finish,
            usage,
        }));
    }

    // 流内 error 帧（overloaded_error 等）
    if event_type == "error" {
        let err = v.get("error").cloned().unwrap_or(Value::Null);
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("upstream reported a stream error")
            .to_string();
        return Some(Err(BridgeError::UpstreamStatus {
            status: 502,
            message,
            parsed: None,
            wire: UpstreamWire::Anthropic,
            retry_after: None,
        }));
    }

    // text_delta / input_json_delta（tool_use 参数增量）
    if event_type == "content_block_delta" {
        if let Some(delta) = v.get("delta") {
            match delta.get("type").and_then(|t| t.as_str()) {
                Some("text_delta") => {
                    if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                        return Some(Ok(ChatChunk {
                            id: state.id.clone(),
                            model: state.model.clone(),
                            delta: ChatDelta {
                                content: Some(text.to_string()),
                                tool_calls: None,
                                reasoning: None,
                            },
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                // 思考内容增量（扩展思考模型）：映射为 OpenAI reasoning_content。
                // 参照 new-api `StreamResponseClaude2OpenAI` 的 thinking_delta 分支。
                Some("thinking_delta") => {
                    if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                        return Some(Ok(ChatChunk {
                            id: state.id.clone(),
                            model: state.model.clone(),
                            delta: ChatDelta {
                                content: None,
                                tool_calls: None,
                                reasoning: Some(thinking.to_string()),
                            },
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                // 签名增量：new-api 以 "\n" 作为 reasoning 占位（签名本身
                // 不可流式回放给客户端，仅保留换行分隔）。
                Some("signature_delta") => {
                    return Some(Ok(ChatChunk {
                        id: state.id.clone(),
                        model: state.model.clone(),
                        delta: ChatDelta {
                            content: None,
                            tool_calls: None,
                            reasoning: Some("\n".to_string()),
                        },
                        finish_reason: None,
                        usage: None,
                    }));
                }
                // 引用增量：AIGX 的 ChatDelta 无 annotations 承载位，静默跳过
                // （citations_delta 不产出 chunk，避免污染内容流）。
                Some("citations_delta") => {}
                // tool_use 参数增量：Anthropic 以 input_json_delta 分片发送
                // tool 调用参数。映射为 OpenAI 风格的 tool_calls 增量（index 按
                // content_block 的 index），供下游流式编码器拼装 tool_use 块。
                Some("input_json_delta") => {
                    if let Some(pj) = delta.get("partial_json").and_then(|p| p.as_str()) {
                        let block_index = v
                            .get("index")
                            .and_then(|i| i.as_u64())
                            .unwrap_or(0) as usize;
                        // 密集索引重映射（参照 new-api ClaudeToChatStreamState）：
                        // tool_use 的 content_block index 与文本/thinking 块共用同一
                        // 序号空间，直接透传会在下游 tool_calls 数组留下空洞。这里
                        // 将工具块映射为独立的密集索引。
                        let tool_index = state.tool_index(block_index);
                        return Some(Ok(ChatChunk {
                            id: state.id.clone(),
                            model: state.model.clone(),
                            delta: ChatDelta {
                                content: None,
                                tool_calls: Some(vec![super::ToolCallDelta {
                                    index: tool_index,
                                    id: None,
                                    function_name: None,
                                    arguments: Some(pj.to_string()),
                                }]),
                                reasoning: None,
                            },
                            finish_reason: None,
                            usage: None,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    // content_block_start（tool_use）：产出带 id/name 的 tool_call 增量，
    // 供下游流式编码器开启 tool_use 内容块。
    if event_type == "content_block_start" {
        if let Some(block) = v.get("content_block") {
            if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                let block_index = v
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as usize;
                let id = block
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(String::from);
                let name = block
                    .get("name")
                    .and_then(|n| n.as_str())
                    .map(String::from);
                // 密集索引重映射（与 input_json_delta 分支一致）
                let tool_index = state.tool_index(block_index);
                return Some(Ok(ChatChunk {
                    id: state.id.clone(),
                    model: state.model.clone(),
                    delta: ChatDelta {
                        content: None,
                        tool_calls: Some(vec![super::ToolCallDelta {
                            index: tool_index,
                            id,
                            function_name: name,
                            arguments: None,
                        }]),
                        reasoning: None,
                    },
                    finish_reason: None,
                    usage: None,
                }));
            }
        }
    }

    None
}

/// 流式状态：跨事件携带 message id / model / prompt token 计数
#[derive(Clone)]
struct StreamState {
    id: String,
    model: String,
    input_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    /// content_block index → 密集 tool_calls index 映射（参照 new-api
    /// `ClaudeToChatStreamState.toolIndexByContentBlock`）
    tool_index_by_block: std::collections::BTreeMap<usize, usize>,
    next_tool_index: usize,
}

impl Default for StreamState {
    fn default() -> Self {
        Self {
            id: String::new(),
            model: String::new(),
            input_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            tool_index_by_block: std::collections::BTreeMap::new(),
            next_tool_index: 0,
        }
    }
}

impl StreamState {
    /// 将 Anthropic content_block index 映射为密集的 tool_calls index。
    /// 首次遇到某工具块时分配下一个连续索引，之后复用。
    fn tool_index(&mut self, block_index: usize) -> usize {
        if let Some(i) = self.tool_index_by_block.get(&block_index) {
            return *i;
        }
        let i = self.next_tool_index;
        self.next_tool_index += 1;
        self.tool_index_by_block.insert(block_index, i);
        i
    }
}

#[async_trait]
impl Bridge for AnthropicBridge {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let body = self.build_body(req, false);
        let resp = self
            .post(self.messages_url(), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Anthropic,
                parse_anthropic_error,
            )
            .await);
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;

        Ok(parse_response(&json, &req.model))
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let body = self.build_body(req, true);
        let resp = self
            .post(self.messages_url(), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::Anthropic,
                parse_anthropic_error,
            )
            .await);
        }

        let id = format!("msg_{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let byte_stream = resp.bytes_stream();

        // 参照 openai.rs 的 unfold 模式：把可变解码器与流状态作为种子放入
        // unfold 闭包，避免 `flat_map` 闭包捕获 `&mut decoder`/`&mut state`
        // 跨 poll 边界（不符合 `Send` 约束）。
        let stream = futures::stream::unfold(
            (
                byte_stream,
                crate::sse::SseDecoder::new(),
                StreamState {
                    id: id.clone(),
                    model: model.clone(),
                    ..Default::default()
                },
                id,
                model,
                Vec::<std::result::Result<ChatChunk, BridgeError>>::new(),
            ),
            |(mut byte_stream, mut decoder, mut state, id, model, mut pending)| async move {
                use futures::StreamExt;
                loop {
                    // 优先吐出上一批积压的 chunk
                    if !pending.is_empty() {
                        let first = pending.remove(0);
                        return Some((first, (byte_stream, decoder, state, id, model, pending)));
                    }
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            let events = decoder.feed(bytes.as_ref());
                            for ev in events {
                                match ev {
                                    crate::sse::SseEvent::Data(payload) => {
                                        if let Some(chunk) =
                                            parse_stream_event(&payload, &id, &model, &mut state)
                                        {
                                            pending.push(chunk);
                                        }
                                    }
                                    // Anthropic 不用 [DONE] 标记，SseDecoder 的 Done
                                    // 事件在此不会触发；流结束由 message_stop 事件处理。
                                    crate::sse::SseEvent::Done => {}
                                }
                            }
                            // 继续循环：下一次迭代吐出积压 chunk 或拉取更多数据
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(BridgeError::Transport(e.to_string())),
                                (byte_stream, decoder, state, id, model, pending),
                            ));
                        }
                        None => {
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(stream))
    }
}

/// 构造一个 Anthropic 原生 Bridge 的 `Arc` 实例（供 ChannelStore/Hub 调度使用）。
pub fn make_bridge(base_url: &str, api_key: &str, client: &reqwest::Client) -> Arc<dyn Bridge> {
    Arc::new(AnthropicBridge::with_client(
        base_url,
        api_key,
        client.clone(),
    ))
}
