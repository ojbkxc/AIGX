//! Trae（字节跳动 AI IDE）SOLO 通道登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/trae`：
//! - `POST {base}/api/agent/v3/llm_utils_chat`；
//! - 鉴权是 `Authorization: Cloud-IDE-JWT <token>` + 一批客户端身份头
//!   （`X-Cloudide-Token` / `X-Uid` / `X-Machine-Id` / `X-Device-Id` / `X-App-Id`…），
//!   身份头来自 provider 默认表 + 账号级额外请求头（账号没填的就不发）；
//! - 请求体是**白名单重建**的 SOLO 信封（`function=solo_work_lite`），
//!   多带字段会被上游拒绝；
//! - 响应 SSE 用 `event:` 区分帧（`output` / `token_usage` / `done` / `error`），
//!   **上游不发 `[DONE]`**。
//!
//! ── 会话路径只有一条（2026-11-25 从 wasm 与实测双向确认）──────
//! `agent2api-wasm-trae` 0.1.1 的 `trae.wasm` 里，`/api/agent/v3/*` 只有
//! `llm_utils_chat` 一个串（偏移 705904，紧邻 `https://trae-api-cn.mchost.guru`
//! 705862）；`/api/ide/v1/get_detail_param`（712824）与
//! `/api/ide/v1/batch_get_detail_param`（712584）是**模型目录**端点，
//! `chat_v3`（698644）与 `solo_work_*` / `solo_agent*` 是**同一枚举里的 function
//! 取值**，不是路径。所以 SOLO / IDE 两个变体的对话路径相同，
//! **不存在"SOLO 要走另一条 path"这回事**。
//!
//! 线上 `404 <html><center>TLB</center>` 的来源因此不是路径，而是**站**：
//! 7 个 `trae-solo-*` 账号把 `base_url` 写成 `https://api.trae.cn`（网页/控制台站），
//! 那个站由字节 TLB 直接回 404；同一份 token 换 `trae-api-cn.mchost.guru` 立即
//! 出 token（实测两种写法各打一次）。归一见 `account_session::adapter`。
//!
//! ── function 取值与模型表的对应（实测）──────────────────────
//! `function` 决定上游给你哪张模型表，选择不当就是流内 4001：
//! `/api/ide/v1/get_detail_param` 实测 `solo_work_lite`→54 个 config_name、
//! `solo_work_remote`→53、`solo_agent`→75、`chat_v3`→65，**这四个都含
//! `deepseek-v4.1-flash`**；`solo_coder`→41 个且**不含**它，于是
//! `function=solo_coder` + `deepseek-v4.1-flash` 必得
//! `event:error {"code":4001,"message":"We're sorry, the param is invalid…"}`。
//! 本通道固定用 `solo_work_lite`（A2A 的口径，也是四条里 catalog 最全的一条）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};

const CHAT_PATH: &str = "/api/agent/v3/llm_utils_chat";
const DEFAULT_MODEL: &str = "glm-5.2";
const DEFAULT_MAX_TOKENS: i64 = 1_000_000;
/// 真实 Trae 客户端 UA —— **两套**，由账号凭证里的 `variant` 决定。
///
/// 上游 WAF 会校验客户端身份：AIGX 早先用 `Trae/1.0 antigravity-cockpit-tools`
/// 一律被拦成 `{"code":"11-128","msg":"request illegal"}` —— 不是 token 失效
/// （那是 401），是请求被判成非法客户端。取值来自 A2A `agent2api-wasm-trae`
/// 0.1.1 wasm 的字面量（`trae/src/headers.rs`），那里 SOLO 与 IDE 是两个不同的串。
const UA_SOLO: &str = "VSCode 1.107.1 (TRAE SOLO CN)";
const UA_IDE: &str = "VSCode 1.107.1 (TRAE CN)";

/// 内部约定：adapter 把凭证里的 `variant` 以这个头名传进来，用它选 UA。
/// **不能发给上游**（见 `new` 里摘除）。
const VARIANT_HEADER: &str = "x-aigx-trae-variant";

/// 真实客户端固定随请求带的身份/渠道头。同样是 wasm 里的字面量，缺一个都可能被
/// 判非法客户端。`x-request-id` / `x-trae-request-id` 每次请求现生成（见 `request`）。
const DEFAULT_HEADERS: &[(&str, &str)] = &[
    ("accept", "*/*"),
    ("accept-language", "zh-CN"),
    ("x-user-region", "CN"),
    ("package-type", "stable_cn"),
    ("x-lgw-req-sdk-type", "3"),
    ("x-market-client-id", "VSCode 1.107.1"),
    // 下面两个是**独立的键**。早先这里写成 ("x-device-type", "x-app-version")
    // 一对 —— 那是把 wasm 里两个相邻的键名当成了键值，实际 `x-device-type`
    // 的值是客户端设备类别、`x-app-version` 是 IDE 版本。
    ("x-device-type", "ide"),
    ("x-app-version", "1.107.1"),
    ("x-ide-version", "1.107.1"),
    ("x-ide-version-type", "stable"),
    ("x-ide-version-code", "20260917"),
    ("x-plugin-channel", "icube-ai"),
    ("request-traffic-type", "prod"),
    ("sec-fetch-dest", "empty"),
    ("sec-fetch-mode", "no-cors"),
    ("sec-fetch-site", "same-origin"),
    // `x-uid` / `x-machine-id` / `x-device-id` / `x-app-version-code` 依赖账号
    // 凭证里的身份字段（wasm 里 token 那侧带 `uid` / `machineId` / `deviceId`），
    // AIGX 的 trae 账号目前 `extra_headers` 是空的，没有来源 —— 按契约
    // 「账号没填的就不发」，这里不编造值。
];

pub struct TraeBridge {
    base: String,
    client: reqwest::Client,
    headers: Vec<(String, String)>,
}

impl TraeBridge {
    pub fn new(
        base: impl Into<String>,
        credential: impl Into<String>,
        headers: Vec<(String, String)>,
        client: reqwest::Client,
    ) -> Self {
        let base = base.into().trim_end_matches('/').to_string();
        let mut merged = headers;

        // 取 variant（adapter 从凭证里带过来），用它决定 UA；取完即摘掉内部头，
        // 别把 AIGX 的内部约定发给上游。
        let variant = merged
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(VARIANT_HEADER))
            .map(|(_, value)| value.trim().to_lowercase())
            .unwrap_or_default();
        merged.retain(|(name, _)| !name.eq_ignore_ascii_case(VARIANT_HEADER));

        if !merged
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("user-agent"))
        {
            // `solo` 走 SOLO 那套 UA，其余（含缺省）走 IDE 那套 —— wasm 里
            // `trae_managed_variant_invalid` 说明变体必须与客户端形态一致。
            let ua = if variant == "solo" { UA_SOLO } else { UA_IDE };
            merged.push(("user-agent".to_string(), ua.to_string()));
        }
        // 客户端身份/渠道头：账号里显式配了同名头的以账号为准（下面 only 判断）
        for (name, value) in DEFAULT_HEADERS {
            if !merged
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
            {
                merged.push((name.to_string(), value.to_string()));
            }
        }
        // 认证头：真实客户端把 token 同时放在三处（`authorization` +
        // `x-cloudide-token` + `x-ide-token`）。早先这里把 credential 参数写成
        // `_credential` 整个忽略掉，请求里没有 authorization → 上游一律
        // `{"code":"11-128","msg":"request illegal"}`（不是 401，是「非法客户端」）。
        // 账号里显式配了同名头的以账号为准，不覆盖。
        let credential = credential.into();
        let credential = credential.trim();
        if !credential.is_empty() {
            let auth_value = format!("Cloud-IDE-JWT {credential}");
            for (name, value) in [
                ("authorization", auth_value.as_str()),
                ("x-cloudide-token", credential),
                ("x-ide-token", credential),
            ] {
                if !merged
                    .iter()
                    .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
                {
                    merged.push((name.to_string(), value.to_string()));
                }
            }
        }
        Self {
            base,
            client,
            headers: merged,
        }
    }

    fn request(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}{CHAT_PATH}", self.base))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");
        for (name, value) in &self.headers {
            // 私有桥自己管理 Content-Type / Accept，避免账号级额外头覆盖它们
            if name.eq_ignore_ascii_case("content-type") || name.eq_ignore_ascii_case("accept") {
                continue;
            }
            req = req.header(name.as_str(), value.as_str());
        }
        // 每请求现生成的两个追踪 id：真实客户端每次都换（wasm `trae/src/headers.rs`
        // 里 `x-request-id` / `x-trae-request-id`）。复用同一个值会被当成重放。
        let request_id = uuid::Uuid::new_v4().to_string();
        // `x-tt-trace-id` 是同类追踪头（wasm 里与 x-request-id 相邻构造），一样每请求换。
        let trace_id = uuid::Uuid::new_v4().simple().to_string();
        req = req
            .header("x-request-id", &request_id)
            .header("x-trae-request-id", request_id.as_str())
            .header("x-tt-trace-id", trace_id.as_str());
        req.json(body)
    }
}

#[async_trait]
impl Bridge for TraeBridge {
    fn name(&self) -> &'static str {
        "trae"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut usage = UsageStats::default();
        let mut finish = FinishReason::Stop;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
            if let Some(text) = chunk.delta.reasoning {
                reasoning.push_str(&text);
            }
            if let Some(deltas) = chunk.delta.tool_calls {
                for delta in deltas {
                    let Some(name) = delta.function_name else {
                        continue;
                    };
                    calls.push(ToolCall {
                        id: delta.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                        function_name: name,
                        arguments: delta.arguments.unwrap_or_default(),
                    });
                }
            }
            if let Some(reason) = chunk.finish_reason {
                finish = reason;
            }
            if let Some(stats) = chunk.usage {
                usage = stats;
            }
        }
        let mut message = ChatMessage::assistant(content);
        message.reasoning = (!reasoning.is_empty()).then_some(reasoning);
        message.tool_calls = (!calls.is_empty()).then_some(calls);
        Ok(ChatResponse {
            id: format!("trae-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message,
            finish_reason: finish,
            usage,
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let body = build_body(req);
        let resp = self
            .request(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("Trae 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("trae-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                Vec::<u8>::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    ended: false,
                    pending_usage: None,
                    event: String::new(),
                    data: String::new(),
                },
            ),
            |(mut bytes, mut buffer, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (bytes, buffer, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            buffer.extend_from_slice(&chunk);
                            while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n')
                            {
                                let line: Vec<u8> = buffer.drain(..=newline).collect();
                                let text = String::from_utf8_lossy(&line)
                                    .trim_end_matches(['\r', '\n'])
                                    .to_string();
                                if let Some((event, data)) = scan_line(&text, &mut flow) {
                                    apply_event(
                                        &event,
                                        &data,
                                        &flow.id,
                                        &flow.model,
                                        &mut flow.pending_usage,
                                        &mut pending,
                                        &mut flow.ended,
                                    );
                                }
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            // 没见到 done 就 EOF：补一条结束帧（上游不保证发 done）
                            pending.push_back(Ok(finish_chunk(
                                &flow.id,
                                &flow.model,
                                flow.pending_usage.take(),
                            )));
                            flow.ended = true;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

struct Flow {
    id: String,
    model: String,
    ended: bool,
    pending_usage: Option<UsageStats>,
    event: String,
    data: String,
}

/// 喂一行，攒出完整事件时返回 `(event, data)`。
fn scan_line(line: &str, flow: &mut Flow) -> Option<(String, String)> {
    if line.is_empty() {
        if flow.event.is_empty() && flow.data.trim().is_empty() {
            return None;
        }
        let event = std::mem::take(&mut flow.event);
        let data = std::mem::take(&mut flow.data);
        return Some((event, data));
    }
    if let Some(rest) = line.strip_prefix("event:") {
        flow.event = rest.trim().to_string();
    } else if let Some(rest) = line.strip_prefix("data:") {
        // 上游多行 data 直接拼接（参考实现同口径）
        flow.data.push_str(rest);
    }
    None
}

fn apply_event(
    event: &str,
    data: &str,
    id: &str,
    model: &str,
    pending_usage: &mut Option<UsageStats>,
    pending: &mut VecDeque<Result<ChatChunk, BridgeError>>,
    ended: &mut bool,
) {
    if data.trim().is_empty() {
        if event == "done" {
            pending.push_back(Ok(finish_chunk(id, model, pending_usage.take())));
            *ended = true;
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return;
    };
    match event {
        "output" => {
            let text = value.get("response").and_then(Value::as_str).unwrap_or("");
            let reasoning = value
                .get("reasoning_content")
                .and_then(Value::as_str)
                .unwrap_or("");
            let calls = value.get("tool_calls").and_then(Value::as_array);
            let mut delta = ChatDelta::default();
            if !text.is_empty() {
                delta.content = Some(text.to_string());
            }
            if !reasoning.is_empty() {
                delta.reasoning = Some(reasoning.to_string());
            }
            if let Some(calls) = calls {
                let parsed: Vec<ToolCallDelta> = calls
                    .iter()
                    .enumerate()
                    .filter_map(|(index, call)| {
                        let function = call.get("function")?;
                        Some(ToolCallDelta {
                            index,
                            id: call.get("id").and_then(Value::as_str).map(str::to_string),
                            function_name: function
                                .get("name")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                            arguments: function.get("arguments").map(|args| {
                                args.as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| args.to_string())
                            }),
                            signature: None,
                        })
                    })
                    .collect();
                if !parsed.is_empty() {
                    delta.tool_calls = Some(parsed);
                }
            }
            if delta.content.is_some() || delta.reasoning.is_some() || delta.tool_calls.is_some() {
                pending.push_back(Ok(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta,
                    finish_reason: None,
                    usage: None,
                }));
            }
        }
        "token_usage" => {
            *pending_usage = Some(usage_of(&value));
        }
        "done" => {
            let finish = map_finish(
                value
                    .get("finish_reason")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            pending.push_back(Ok(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta::default(),
                finish_reason: Some(finish),
                usage: pending_usage.take(),
            }));
            *ended = true;
        }
        "error" => {
            let code = value
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or_default();
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("上游错误");
            pending.push_back(Err(BridgeError::upstream_status(
                502,
                format!("Trae 流内错误：[{code}] {message}"),
            )));
            *ended = true;
        }
        _ => {}
    }
}

fn finish_chunk(id: &str, model: &str, usage: Option<UsageStats>) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage,
    }
}

fn map_finish(raw: &str) -> FinishReason {
    if crate::bridge::is_length_finish(raw) {
        return FinishReason::Length;
    }
    match raw.trim().to_ascii_lowercase().as_str() {
        "tool_calls" | "tool-calls" => FinishReason::ToolCalls,
        "content_filter" | "safety" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["prompt_tokens", "input_tokens"]);
    let completion = number(&["completion_tokens", "output_tokens"]);
    let total = number(&["total_tokens"]);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: if total > 0 {
            total
        } else {
            prompt + completion
        },
        cached_prompt_tokens: 0,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// ChatFormat → SOLO 信封（白名单重建：只搬上游认的键）。
pub fn build_body(req: &ChatFormat) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        let role = match message.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            _ => "user",
        };
        let mut entry = Map::new();
        entry.insert("role".to_string(), json!(role));
        // `content` 必须是**数组**，不能是字符串：Trae 端的 Go 类型是
        // `[]*idecopilot.LLMRawMessageContent`，发字符串会被上游
        // `json: cannot unmarshal string into Go struct field ... of type
        // []*idecopilot.LLMRawMessageContent` 直接 400 掉（线上实测）。
        //
        // ── 空文本**不能省略 content**（2026-11-25 实测修正）──────────
        // 早先这里写成 `if !text.is_empty()`，理由是"空数组更容易被判内容缺失"。
        // 实测正好相反：上游对**缺 content 或空数组**的消息一律回
        // `event:error data:{"code":4001,"message":"We're sorry, the param is invalid…"}`，
        // 而 `content:[{type:text,text:""}]` 能被接受。三种形态都单独打过一遍：
        //   system 无 content   → 4001      system content=[]  → 4001
        //   system 空 text 块   → 200       user   content=[]  → 4001
        //   tool   无 content   → 4001      tool   空 text 块  → 200
        // 客户端历史里"空 system 提示 / 空 tool 结果"极常见，所以这条是线上
        // `[4001] param is invalid` 的真因之一（同一条凭据换写法立即出 token）。
        //
        // 唯一例外是**只带 tool_calls 的 assistant**：上游按纯调用消息处理，
        // 省略 content 与给空 text 块都能过，保留省略以贴近真实客户端形态。
        let tool_calls_only_assistant =
            message.role == Role::Assistant && message.tool_calls.is_some();
        if !text.is_empty() {
            entry.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": text }]),
            );
        } else if !tool_calls_only_assistant {
            entry.insert(
                "content".to_string(),
                json!([{ "type": "text", "text": "" }]),
            );
        }
        if message.role == Role::Tool {
            if let Some(id) = &message.tool_call_id {
                entry.insert("tool_call_id".to_string(), json!(id));
            }
        }
        if let Some(calls) = &message.tool_calls {
            let calls: Vec<Value> = calls
                .iter()
                .map(|call| {
                    json!({
                        "id": call.id,
                        "type": "function",
                        "function_call": {
                            "name": call.function_name,
                            "arguments": call.arguments,
                        },
                    })
                })
                .collect();
            entry.insert("tool_calls".to_string(), Value::Array(calls));
        }
        messages.push(Value::Object(entry));
    }

    let model = req.model.trim();
    let model = if model.is_empty() {
        DEFAULT_MODEL
    } else {
        model
    };

    let mut out = Map::new();
    out.insert("messages".to_string(), Value::Array(messages));
    out.insert("function".to_string(), json!("solo_work_lite"));
    out.insert("stream".to_string(), json!(true));
    out.insert("config_name".to_string(), json!(model));
    out.insert("model".to_string(), json!(model));
    if let Some(tools) = &req.tools {
        // SOLO 要求 `function.parameters` 是 JSON 字符串
        let tools: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut tool = tool.clone();
                if let Some(parameters) = tool
                    .get_mut("function")
                    .and_then(|function| function.get_mut("parameters"))
                {
                    if parameters.is_object() {
                        let text = parameters.to_string();
                        *parameters = Value::String(text);
                    }
                }
                tool
            })
            .collect();
        out.insert("tools".to_string(), Value::Array(tools));
    }
    // `tool_choice` 必须是**裸字符串**：上游 Go 类型是 `string`，
    // 发 OpenAI 的对象形态（`{"type":"auto"}` / `{"type":"function","function":{"name":…}}`）
    // 会以 `json: cannot unmarshal object into Go struct field
    // LLMUtilsChatRequest.tool_choice of type string` 收场（实测 HTTP 400 + 4001）。
    // 折算规则与 A2A `trae/payload.rs::normalize_tool_choice` 一致：
    //   "none"（不论裸串还是对象）→ 不发 tool_choice，并连带删掉 tools；
    //   auto / required       → 同名裸串；
    //   {type:function,name}  → 函数名裸串（缺名回 auto）；
    //   其它形态               → 不发 tool_choice（不猜）。
    if let Some(choice) = &req.tool_choice {
        let mut suppress_tools = false;
        let mut out_choice: Option<String> = None;
        match choice {
            Value::String(name) => {
                if name.trim().eq_ignore_ascii_case("none") {
                    suppress_tools = true;
                } else {
                    out_choice = Some(name.trim().to_string());
                }
            }
            Value::Object(wrapper) => {
                let kind = wrapper
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_lowercase();
                match kind.as_str() {
                    "none" => suppress_tools = true,
                    "auto" | "required" => out_choice = Some(kind),
                    "function" => {
                        let name = wrapper
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                            .or_else(|| wrapper.get("name").and_then(Value::as_str))
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        out_choice = Some(if name.is_empty() {
                            "auto".to_string()
                        } else {
                            name
                        });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if suppress_tools {
            out.remove("tools");
        } else if let Some(choice) = out_choice {
            out.insert("tool_choice".to_string(), json!(choice));
        }
    }
    if let Some(temperature) = req.temperature {
        out.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = req.top_p {
        out.insert("top_p".to_string(), json!(top_p));
    }
    out.insert(
        "max_tokens".to_string(),
        json!(req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS as u32)),
    );
    if let Some(effort) = &req.reasoning_effort {
        let level = effort.trim().to_lowercase();
        if !matches!(level.as_str(), "" | "auto" | "none" | "off") {
            out.insert("reasoning_effort".to_string(), json!(effort));
        }
    }
    Value::Object(out)
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(text.to_string()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }
    }

    fn format(messages: Vec<ChatMessage>) -> ChatFormat {
        ChatFormat {
            model: "glm-5.2".into(),
            messages,
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: true,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        }
    }

    #[test]
    fn body_uses_solo_whitelist() {
        let body = build_body(&format(vec![user("你好")]));
        assert_eq!(body["function"], "solo_work_lite");
        assert_eq!(body["stream"], true);
        assert_eq!(body["config_name"], "glm-5.2");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("top_k").is_none());
    }

    /// `content` 必须是数组（线上实测：发字符串被 Trae 以 4001 拒掉，
    /// 报 `cannot unmarshal string into ... []*idecopilot.LLMRawMessageContent`）。
    #[test]
    fn message_content_is_array_of_text_parts() {
        let body = build_body(&format(vec![user("你好")]));
        let content = &body["messages"][0]["content"];
        assert!(content.is_array(), "content 必须是数组: {content}");
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "你好");
    }

    /// 空文本**必须**发空 text 块，不能省略 content（2026-11-25 实测）。
    ///
    /// 上游对「缺 content / 空数组」的消息回流内 4001（system、user、tool 三种角色
    /// 都实测过），而 `content:[{type:text,text:""}]` 能被接受 —— 客户端历史里的
    /// 空 system 提示与空 tool 结果因此成了线上 `[4001] param is invalid` 的来源。
    #[test]
    fn empty_text_still_sends_an_empty_text_part() {
        for role in [Role::System, Role::User, Role::Tool] {
            let mut message = user("");
            message.role = role;
            let body = build_body(&format(vec![message]));
            let content = &body["messages"][0]["content"];
            assert!(
                content.is_array(),
                "空文本也要给数组形态的 content: {content}"
            );
            assert_eq!(content[0]["type"], "text");
            assert_eq!(content[0]["text"], "");
        }
    }

    /// 只带 tool_calls 的 assistant 省略 content（上游按纯调用消息处理，两种形态都行）。
    #[test]
    fn tool_calls_only_assistant_omits_content() {
        let mut message = ChatMessage::assistant("");
        message.tool_calls = Some(vec![ToolCall {
            id: "c1".into(),
            function_name: "read_file".into(),
            arguments: "{}".into(),
        }]);
        let body = build_body(&format(vec![message]));
        assert!(
            body["messages"][0].get("content").is_none(),
            "纯 tool_calls 消息不该被补空 text 块: {}",
            body["messages"][0]
        );
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function_call"]["name"],
            "read_file"
        );
    }

    /// `tool_choice` 必须折成裸字符串；对象形态会被上游按类型错误拒掉（4001）。
    #[test]
    fn tool_choice_is_flattened_to_a_bare_string() {
        let cases: [(Value, Option<&str>, bool); 5] = [
            (json!({"type": "auto"}), Some("auto"), true),
            (json!({"type": "required"}), Some("required"), true),
            (
                json!({"type": "function", "function": {"name": "read_file"}}),
                Some("read_file"),
                true,
            ),
            (json!({"type": "function"}), Some("auto"), true),
            (json!({"type": "none"}), None, false),
        ];
        for (choice, want, tools_kept) in cases {
            let mut req = format(vec![user("hi")]);
            req.tools = Some(vec![json!({
                "type": "function",
                "function": {"name": "read_file", "parameters": {"type": "object"}}
            })]);
            req.tool_choice = Some(choice.clone());
            let body = build_body(&req);
            match want {
                Some(text) => assert_eq!(body["tool_choice"], json!(text), "choice={choice}"),
                None => assert!(body.get("tool_choice").is_none(), "choice={choice}"),
            }
            assert_eq!(body.get("tools").is_some(), tools_kept, "choice={choice}");
        }
    }

    /// 裸字符串形态原样透传（含函数名）。
    #[test]
    fn bare_tool_choice_string_survives() {
        let mut req = format(vec![user("hi")]);
        req.tool_choice = Some(json!("read_file"));
        assert_eq!(build_body(&req)["tool_choice"], "read_file");
    }

    #[test]
    fn output_event_becomes_delta() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "output",
            r#"{"response":"你","reasoning_content":"想"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.delta.content.as_deref(), Some("你"));
                assert_eq!(chunk.delta.reasoning.as_deref(), Some("想"));
            }
            _ => panic!("output 应产出增量"),
        }
    }

    #[test]
    fn done_event_finishes_with_pending_usage() {
        let mut pending = VecDeque::new();
        let mut usage = None;
        let mut ended = false;
        apply_event(
            "token_usage",
            r#"{"prompt_tokens":3,"completion_tokens":2}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        apply_event(
            "done",
            r#"{"finish_reason":"stop"}"#,
            "x",
            "m",
            &mut usage,
            &mut pending,
            &mut ended,
        );
        match pending.pop_front() {
            Some(Ok(chunk)) => {
                assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
                assert_eq!(chunk.usage.map(|u| u.prompt_tokens), Some(3));
            }
            _ => panic!("done 应产出结束帧"),
        }
        assert!(ended);
    }
}
