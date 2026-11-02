//! Qoder 登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/qoder`：
//! - 推理：`POST {gateway}/algo/api/v2/service/pro/sse/agent_chat_generation`
//!   （`FetchKeys=llm_model_result&AgentId=agent_common&Encode=1`）；
//! - 鉴权是 COSY 自签名（见 [`super::qoder_cosy`]），**请求体先编码再签名**；
//! - 响应 SSE 多包一层信封 `{statusCodeValue, body}`，内层才是 OpenAI chunk；
//! - 免费模型繁忙走**排队制**（HTTP 403 + 业务码 10605，或 200 信封内的同族
//!   JSON），排队态在首帧下发之前按上游建议退避重发（见 [`queue_backoff`]）。
//!
//! 账号身份（uid / email / name / machine_id）从访问令牌的 JWT 声明里读，
//! 读不到时用令牌哈希派生稳定值（上游只把它当展示与设备标识）。

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, ToolCall, ToolCallDelta, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

use super::qoder_cosy::{self, CosyIdentity};

const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation";
const CHAT_QUERY: &str = "FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
const SESSION_TYPE: &str = "qodercli";
const AGENT_ID: &str = "agent_common";
const CHAT_TASK: &str = "FREE_INPUT";
const MAX_OUTPUT_TOKENS: u32 = 32_000;

pub struct QoderBridge {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl QoderBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        let token = token.into();
        let token = super::normalize_credential(&token, &["authorization:", "bearer"]);
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.to_string(),
            client,
        }
    }

    fn url(&self) -> String {
        format!("{}{CHAT_PATH}?{CHAT_QUERY}", self.base)
    }
}

#[async_trait]
impl Bridge for QoderBridge {
    fn name(&self) -> &'static str {
        "qoder"
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
            id: format!("qoder-{}", uuid::Uuid::new_v4()),
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
        let identity = identity_of(&self.token);
        // ── 为什么这里是一个循环（对照 Agent2API qoder::forward_conversation）──
        // 上游对免费模型走排队制，繁忙时回「暂不可服务，建议 N 秒后再来」
        // （403 + 10605，或 200 的信封首帧里同族 JSON）。那是**可以原地等
        // 一会儿重发**的状态（官方 CLI 就这么做），所以「构造 → 发送 →
        // 预读首帧」每轮重跑：排队就睡一觉再来，其它错误按原样返回。
        // 首帧之后（流已开始下发）不再退避 —— 那时客户端的 HTTP 头早已
        // 发出，换不了任何东西，排队帧只能在流里落定成错误。
        let mut queue_waits: usize = 0;
        let mut queue_waited_ms: u64 = 0;
        loop {
            // 每轮都重新构造：COSY 签名覆盖 request_id 与请求体，**重放同一份
            // 签名**会被上游判成重复请求（403 / code 103），所以退避之后必须
            // 换一份新的 request_id 与签名（build_body 每轮生成新 id）。
            let mut state = match self.attempt_open(req, &identity).await {
                Ok(state) => state,
                Err(Attempt::Fatal(e)) => return Err(e),
                Err(Attempt::Queued(queued)) => {
                    if let Err(e) =
                        queue_backoff(&mut queue_waits, &mut queue_waited_ms, &queued).await
                    {
                        return Err(e);
                    }
                    continue;
                }
            };
            // 首帧预读：拿到 HTTP 200 不能直接返回 —— 排队/业务错误恰恰写在
            // 200 的信封里。此刻还没有任何字节下发，排队可以无损退避重发。
            // 等待上限与编排层首帧守卫同一预算（响应头等待超时）。
            let deadline = crate::bridge::sse_first_event_deadline();
            let head = tokio::time::timeout(deadline, advance(&mut state)).await;
            let first = match head {
                Err(_) => {
                    return Err(BridgeError::Timeout {
                        elapsed_ms: deadline.as_millis() as u64,
                        cause: "Qoder 首帧等待超时（上游建流后未下发事件）".to_string(),
                    })
                }
                Ok(Step::Queued(queued)) => {
                    if let Err(e) =
                        queue_backoff(&mut queue_waits, &mut queue_waited_ms, &queued).await
                    {
                        return Err(e);
                    }
                    continue;
                }
                // 上游正常关闭（空流）：返回空流，交由下游空响应机制处理
                Ok(Step::End) => return Ok(futures::stream::empty().boxed()),
                Ok(Step::Item(item)) => item,
            };
            // 续流：状态机原样带下去，预读帧补回流首（编排层的首帧守卫会
            // 立即拿到已到达的首帧，不会二次等待）
            let rest = futures::stream::unfold(state, |mut state| async move {
                match advance(&mut state).await {
                    Step::Item(item) => Some((item, state)),
                    Step::End => None,
                    // 中途才出现的排队信封：头已发出、退不了避，只能在流里
                    // 落定成 503 错误收尾（与上游流内错误同一语义）
                    Step::Queued(queued) => {
                        state.flow.ended = true;
                        Some((Err(queued_error(&queued, 0)), state))
                    }
                }
            });
            return Ok(futures::stream::once(async move { first })
                .chain(rest)
                .boxed());
        }
    }
}

impl QoderBridge {
    /// 一次上游往返：构造 body → 编码 → 签名 → 发送 → 建流状态。
    ///
    /// HTTP 非 2xx 的响应体先过排队识别（403 + 10605 是排队族的主形态）；
    /// 其它失败按原样交回（`Attempt::Fatal`）。
    async fn attempt_open(
        &self,
        req: &ChatFormat,
        identity: &Identity,
    ) -> Result<StreamState, Attempt> {
        let body = build_body(req, &identity.user_id);
        let encoded = qoder_cosy::encode_body(body.to_string().as_bytes());
        let url = self.url();
        let mut headers = qoder_cosy::build_auth_headers(
            Some(&encoded),
            &url,
            &CosyIdentity {
                user_id: &identity.user_id,
                auth_token: &self.token,
                name: &identity.name,
                email: &identity.email,
                machine_id: &identity.machine_id,
            },
        )
        .map_err(|e| Attempt::Fatal(BridgeError::Config(e)))?;
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        headers.push(("Accept".to_string(), "text/event-stream".to_string()));
        headers.push(("Cache-Control".to_string(), "no-cache".to_string()));
        headers.push(("Accept-Encoding".to_string(), "identity".to_string()));
        headers.push(("X-Model-Key".to_string(), req.model.clone()));
        headers.push(("X-Model-Source".to_string(), "model".to_string()));

        let mut builder = self.client.post(&url).body(encoded);
        for (name, value) in &headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let resp = builder
            .send()
            .await
            .map_err(|e| Attempt::Fatal(BridgeError::Transport(e.to_string())))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            if let Some(queued) = scan_queue_signals(&text) {
                return Err(Attempt::Queued(queued));
            }
            return Err(Attempt::Fatal(BridgeError::upstream_status(
                status,
                format!("Qoder 返回 HTTP {status}：{}", head(&text)),
            )));
        }

        Ok(StreamState {
            bytes: resp.bytes_stream().boxed(),
            decoder: SseDecoder::new(),
            pending: VecDeque::new(),
            flow: Flow {
                id: format!("qoder-{}", uuid::Uuid::new_v4()),
                model: req.model.clone(),
                ended: false,
            },
        })
    }
}

/// 流推进状态（原 `stream::unfold` 元组的具名化：预读与续流共用一台状态机）。
struct StreamState {
    bytes: BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
    decoder: SseDecoder,
    pending: VecDeque<Result<ChatChunk, BridgeError>>,
    flow: Flow,
}

/// 状态机推进一步的结果。
enum Step {
    /// 产出一个下发项（或内建错误帧）
    Item(Result<ChatChunk, BridgeError>),
    /// 读到排队信封（该轮响应的剩余字节作废，由调用方决定退避或落定）
    Queued(Queued),
    /// 流已终止且无积压
    End,
}

/// 推进状态机直到产出：pending 有货先出货；否则持续读网络包喂解码器。
async fn advance(state: &mut StreamState) -> Step {
    loop {
        if let Some(item) = state.pending.pop_front() {
            return Step::Item(item);
        }
        if state.flow.ended {
            return Step::End;
        }
        match state.bytes.next().await {
            Some(Ok(chunk)) => {
                let events = state.decoder.feed(chunk.as_ref());
                for event in events {
                    match decode_event(&event, &state.flow.id, &state.flow.model) {
                        Decoded::Nothing => {}
                        Decoded::Chunk(chunk) => state.pending.push_back(Ok(chunk)),
                        Decoded::Done => {
                            state.pending.push_back(Ok(stop_chunk(&state.flow)));
                            state.flow.ended = true;
                        }
                        Decoded::Err(err) => {
                            state.pending.push_back(Err(err));
                            state.flow.ended = true;
                        }
                        Decoded::Queued(queued) => return Step::Queued(queued),
                    }
                }
            }
            Some(Err(e)) => {
                state
                    .pending
                    .push_back(Err(BridgeError::Transport(e.to_string())));
                state.flow.ended = true;
            }
            None => {
                state.pending.push_back(Ok(stop_chunk(&state.flow)));
                state.flow.ended = true;
            }
        }
    }
}

/// 流收尾帧（正常 stop，无增量）。
fn stop_chunk(flow: &Flow) -> ChatChunk {
    ChatChunk {
        id: flow.id.clone(),
        model: flow.model.clone(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
    }
}

struct Flow {
    id: String,
    model: String,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done,
    Err(BridgeError),
    Queued(Queued),
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done,
        SseEvent::Data(payload) => {
            let trimmed = payload.trim();
            if trimmed.is_empty() {
                return Decoded::Nothing;
            }
            if trimmed == "[DONE]" {
                return Decoded::Done;
            }
            let Ok(envelope) = serde_json::from_str::<Value>(trimmed) else {
                return Decoded::Nothing;
            };
            if let Some(status) = envelope.get("statusCodeValue").and_then(Value::as_i64) {
                if status != 200 {
                    let raw = envelope
                        .get("body")
                        .map(|body| {
                            body.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| body.to_string())
                        })
                        .unwrap_or_default();
                    // 信封业务错误先判排队：排队帧交回调用方退避重发（HTTP 200
                    // 里还没下发任何字节，重发无损），其它错误照旧落定成流内错误
                    if let Some(queued) = scan_queue_signals(&raw) {
                        return Decoded::Queued(queued);
                    }
                    return Decoded::Err(BridgeError::upstream_status(
                        status as u16,
                        format!("Qoder 上游错误（{status}）：{}", head(&raw)),
                    ));
                }
            }
            let Some(inner) = envelope.get("body") else {
                return Decoded::Nothing;
            };
            if inner.as_str() == Some("[DONE]") {
                return Decoded::Done;
            }
            let chunk = match inner {
                Value::String(text) => match serde_json::from_str::<Value>(text) {
                    Ok(parsed) => parsed,
                    Err(_) => return Decoded::Nothing,
                },
                other => other.clone(),
            };
            chunk_to_delta(&chunk, id, model)
        }
    }
}

fn chunk_to_delta(chunk: &Value, id: &str, model: &str) -> Decoded {
    let choice = chunk.get("choices").and_then(|c| c.get(0));
    let mut delta = ChatDelta::default();
    if let Some(inner) = choice.and_then(|c| c.get("delta")) {
        if let Some(text) = inner.get("content").and_then(Value::as_str) {
            if !text.is_empty() {
                delta.content = Some(text.to_string());
            }
        }
        if let Some(text) = inner.get("reasoning_content").and_then(Value::as_str) {
            if !text.is_empty() {
                delta.reasoning = Some(text.to_string());
            }
        }
        if let Some(calls) = inner.get("tool_calls").and_then(Value::as_array) {
            let parsed: Vec<ToolCallDelta> = calls
                .iter()
                .enumerate()
                .map(|(index, call)| ToolCallDelta {
                    index: call
                        .get("index")
                        .and_then(Value::as_u64)
                        .map(|value| value as usize)
                        .unwrap_or(index),
                    id: call.get("id").and_then(Value::as_str).map(str::to_string),
                    function_name: call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    arguments: call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect();
            if !parsed.is_empty() {
                delta.tool_calls = Some(parsed);
            }
        }
    }
    let usage = chunk.get("usage").map(usage_of);
    let finish = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(Value::as_str)
        .map(map_finish);
    if let Some(reason) = finish {
        return Decoded::Chunk(ChatChunk {
            id: id.to_string(),
            model: model.to_string(),
            delta,
            finish_reason: Some(reason),
            usage,
        });
    }
    if delta.content.is_none() && delta.reasoning.is_none() && delta.tool_calls.is_none() {
        return match usage {
            Some(stats) => Decoded::Chunk(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: ChatDelta::default(),
                finish_reason: None,
                usage: Some(stats),
            }),
            None => Decoded::Nothing,
        };
    }
    Decoded::Chunk(ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta,
        finish_reason: None,
        usage,
    })
}

fn map_finish(raw: &str) -> FinishReason {
    // length 族先走共享别名表（续写判据依赖它，见 crate::bridge::is_length_finish）
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
    let number = |key: &str| -> u64 { usage.get(key).and_then(Value::as_i64).unwrap_or(0) as u64 };
    let prompt = number("prompt_tokens");
    let completion = number("completion_tokens");
    let total = number("total_tokens");
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

/// 账号身份（uid / name / email / machine_id）。
pub(crate) struct Identity {
    pub(crate) user_id: String,
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) machine_id: String,
}

/// 从凭证 token 里解出 COSY 签名要用的身份字段；解不出时用令牌哈希派生稳定值。
///
/// 账号池的模型目录拉取（`account_session::catalog`）要签同一个 COSY 头，
/// 所以这里是 `pub(crate)` —— 两处共用一份解析口径，避免签名身份与
/// 转发链路对不上。
pub(crate) fn identity_of(token: &str) -> Identity {
    let claims = decode_jwt_claims(token);
    let text = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| claims.get(*key).and_then(Value::as_str))
            .unwrap_or("")
            .to_string()
    };
    let mut user_id = text(&["uid", "user_id", "userId", "sub"]);
    if user_id.is_empty() {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"qoder-uid");
        hasher.update(token.as_bytes());
        user_id = format!("{:x}", hasher.finalize())[..24].to_string();
    }
    let machine_id = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"qoder-machine");
        hasher.update(token.as_bytes());
        format!("{:x}", hasher.finalize())[..32].to_string()
    };
    Identity {
        user_id,
        name: text(&["name", "nickname"]),
        email: text(&["email"]),
        machine_id,
    }
}

fn decode_jwt_claims(token: &str) -> Value {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return Value::Null;
    }
    let payload = parts[1];
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload));
    let Ok(bytes) = decoded else {
        return Value::Null;
    };
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

/// ChatFormat → Qoder 固定信封。
pub fn build_body(req: &ChatFormat, user_id: &str) -> Value {
    let mut messages: Vec<Value> = Vec::new();
    for message in &req.messages {
        let text = message.content_str();
        if text.trim().is_empty() {
            continue;
        }
        let role = match message.role {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
            _ => "user",
        };
        let mut entry = Map::new();
        entry.insert("role".to_string(), json!(role));
        entry.insert("content".to_string(), json!(text));
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
                        "function": {
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

    let max_tokens = req
        .max_tokens
        .filter(|value| *value > 0)
        .unwrap_or(MAX_OUTPUT_TOKENS)
        .min(MAX_OUTPUT_TOKENS);

    let mut parameters = Map::new();
    parameters.insert("max_tokens".to_string(), json!(max_tokens));
    if let Some(effort) = &req.reasoning_effort {
        let level = effort.trim().to_lowercase();
        if !matches!(level.as_str(), "" | "auto" | "none" | "off") {
            parameters.insert("enable_thinking".to_string(), json!(true));
            parameters.insert("reasoning_effort".to_string(), json!(level));
        }
    }

    let last_user_text = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();

    let session_id = session_id_for(user_id, &req.model);
    let request_id = uuid::Uuid::new_v4().to_string();

    json!({
        "request_id": request_id,
        "request_set_id": request_id,
        "chat_record_id": request_id,
        "session_id": session_id,
        "stream": true,
        "chat_task": CHAT_TASK,
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": SESSION_TYPE,
        "agent_id": AGENT_ID,
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": Value::Null,
        "aliyun_user_type": "",
        "system": "",
        "messages": messages,
        "tools": req.tools.clone().map(Value::Array).unwrap_or(json!([])),
        "parameters": Value::Object(parameters),
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": Value::Null,
            "extra": {
                "context": [],
                "modelConfig": { "key": req.model },
                "originalContent": last_user_text,
            },
            "features": [],
            "text": last_user_text,
        },
        "model_config": { "key": req.model },
        "business": {
            "product": "cli",
            "version": "1.0.0",
            "type": "agent",
            "stage": "start",
            "id": uuid::Uuid::new_v4().to_string(),
            "name": last_user_text.chars().take(30).collect::<String>(),
            "begin_at": chrono::Utc::now().timestamp_millis(),
        },
    })
}

fn session_id_for(user_id: &str, model: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"qoder-session");
    hasher.update([0u8]);
    hasher.update(user_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(model.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!(
        "{}-{}-{}-{}-{}",
        &hash[..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..32]
    )
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

// ── 排队等待（对照 Agent2API qoder::chat::Queued / queue_backoff）──────────
//
// 上游对免费模型（如 qfmodel）走排队制：空闲直接出字，繁忙时回一句
// 「暂不可服务，建议 N 秒后再来」——**不是错误，是暂时排不上号**。官方
// CLI 与参考实现（qoder2api / 9router）的处理都是按建议时长睡一觉再发
// 同一请求。排队族的主形态是 HTTP 403 + 业务码 10605，正文常是多层嵌套
// 的 JSON 字符串（`{"code":"403","message":"{\"code\":\"10605\",…}"}`），
// 取值要顺着 message/body/data 一路钻；另一形态是 HTTP 200 的信封首帧
// （statusCodeValue=403 + 同族 body），因此调用方必须**预读首帧**才能
// 无损重发（见 chat_stream 的循环）。
//
// 排队与一般错误的唯一区别是「还能再试」：做成类型而不是看状态码猜，
// 是因为 403 会同时骗过鉴权续期与熔断分类（10605 曾被误报成登录失效）。

/// 一次上游往返的结果分类（桥内私有，不进入 BridgeError 体系）。
enum Attempt {
    /// 排队态：调用方退避后重建请求重发
    Queued(Queued),
    /// 其它失败：原样交回编排层（换渠道 / 熔断都按既有语义）
    Fatal(BridgeError),
}

/// 上游排队态信号（业务码 10605 那一族字段）。
struct Queued {
    /// 上游建议的重试间隔（毫秒，已带缺省与 5~30 秒钳制）
    retry_after_ms: u64,
    /// 上游原文（截断；进日志与最终错误体，便于排障）
    raw: String,
    /// 排障信息（`queueType` / `queueCount` / 服务可用性）
    detail: String,
}

/// 排队中：官方错误码表 "10605 Model request is queued"。
const QUEUE_CODE: &str = "10605";
/// 嵌套 JSON 的钻取深度上限（防病态输入下的环；实测两层就到底了）。
const SIGNAL_SCAN_DEPTH: usize = 5;

/// JS 真值语义（`isQueued` / `serviceAvailable` 上游给过布尔与数字两种形态）。
fn signal_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// 顺着嵌套 JSON 抠排队信号。
///
/// 判定：业务码 10605 是硬信号；`isQueued` 为真；或「声明服务不可用 +
/// 带了队列档位」的组合（上游换码时的兜底）。正文不是 JSON 时走裸文本
/// 兜底（转义形态把反斜杠去掉即可匹配）。
fn scan_queue_signals(raw: &str) -> Option<Queued> {
    let mut codes: Vec<String> = Vec::new();
    let mut is_queued = false;
    let mut retry_after_secs: Option<u64> = None;
    let mut queue_type: Option<String> = None;
    let mut service_available: Option<bool> = None;
    let mut queue_count: Option<i64> = None;

    let mut current = Some(raw.to_string());
    let mut depth = 0usize;
    while let Some(text) = current.take() {
        depth += 1;
        if depth > SIGNAL_SCAN_DEPTH {
            break;
        }
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            break;
        };
        if let Some(code) = value.get("code") {
            match code {
                Value::String(text) => codes.push(text.trim().to_string()),
                Value::Number(number) => codes.push(number.to_string()),
                _ => {}
            }
        }
        if value.get("isQueued").map(signal_truthy).unwrap_or(false) {
            is_queued = true;
        }
        if retry_after_secs.is_none() {
            retry_after_secs = ["retryAfterSeconds", "waitTime"]
                .iter()
                .find_map(|key| value.get(*key).and_then(seconds_of));
        }
        if queue_type.is_none() {
            queue_type = value
                .get("queueType")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if service_available.is_none() {
            service_available = value.get("serviceAvailable").map(signal_truthy);
        }
        if queue_count.is_none() {
            queue_count = value.get("queueCount").and_then(Value::as_i64);
        }
        // 下一层：内嵌的 JSON 字符串（上游一层套一层地放 message / body / data）
        current = ["message", "body", "data", "error"]
            .iter()
            .find_map(|key| value.get(*key).and_then(Value::as_str))
            .map(str::to_string)
            .filter(|text| text.trim_start().starts_with('{'));
    }

    let hit = codes.iter().any(|code| code == QUEUE_CODE)
        || is_queued
        || (queue_type.is_some() && service_available == Some(false))
        || raw_has_queue_code(raw);
    if !hit {
        return None;
    }
    let mut notes: Vec<String> = Vec::new();
    if let Some(kind) = &queue_type {
        notes.push(format!("队列 {kind}"));
    }
    if let Some(count) = queue_count {
        notes.push(format!("排队 {count}"));
    }
    if service_available == Some(false) {
        notes.push("上游声明服务不可用".to_string());
    }
    Some(Queued {
        retry_after_ms: queue_wait_ms(retry_after_secs),
        raw: raw.chars().take(300).collect(),
        detail: notes.join("，"),
    })
}

/// 秒数的两种形态（数字 / 数字字符串）。
fn seconds_of(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
}

/// 裸文本兜底：正文不是 JSON 时也能认出业务码（`\"code\":\"10605\"` 这类
/// 转义形态把反斜杠去掉就与普通形态同形）。
fn raw_has_queue_code(raw: &str) -> bool {
    let flat: String = raw.chars().filter(|ch| *ch != '\\').collect();
    flat.contains(&format!("\"code\":\"{QUEUE_CODE}\""))
        || flat.contains(&format!("\"code\":{QUEUE_CODE}"))
        || flat.contains(&format!("\"code\": \"{QUEUE_CODE}\""))
}

/// 单次排队等待的时长（毫秒）：跟随上游建议，缺省 15 秒，钳在 5~30 秒。
///
/// 上限 30 秒与实测的建议值区间（9~30 秒）同档：等得比上游建议更久没有
/// 意义，只会把客户端的首字等待拖长。
fn queue_wait_ms(retry_after_secs: Option<u64>) -> u64 {
    const MIN_SECS: u64 = 5;
    const MAX_SECS: u64 = 30;
    const FALLBACK_SECS: u64 = 15;
    retry_after_secs
        .unwrap_or(FALLBACK_SECS)
        .clamp(MIN_SECS, MAX_SECS)
        * 1000
}

/// 排队态落定：**503** + 一句「不是登录态或额度问题」。
///
/// 状态码不能是 401 / 429：那两档在编排层与本项目内部都带动作（刷新凭证 /
/// 落限额冷却），排队既不是凭证问题也不是账号问题。与参考实现
/// （qoder2api 插件的 `qoder_model_busy`）取同一档。
fn queued_error(queued: &Queued, waited_ms: u64) -> BridgeError {
    let waited = if waited_ms >= 1000 {
        waited_ms / 1000
    } else {
        0
    };
    let waited_note = if waited > 0 {
        format!("（已按上游建议等待 {waited} 秒仍不可服务）")
    } else {
        String::new()
    };
    BridgeError::upstream_status(
        503,
        format!(
            "qoder_model_busy：上游模型排队中，暂不可服务{waited_note}（上游原文：{}）",
            queued.raw
        ),
    )
}

/// 排队退避：还能等就睡一觉并返回 `Ok`（调用方重新签名再发一次）；
/// 预算用尽返回 `Err`（排队落定成 503，文案带已等待时长与上游原文）。
///
/// ── 等待预算为什么要看「响应头等待超时」────────────────────────
/// 排队等待发生在**首帧之前**（客户端的 200 头还没发出），吃掉的是用户给
/// 「等待响应超时」的那份预算（与首帧守卫同一快照，实时生效）。用户把它
/// 调小就说明不接受一次请求卡那么久 —— 那就不等，直接给一条能看懂的排队
/// 错误。`max_waits = 0` = 不等待：排队即 503 快速失败。
async fn queue_backoff(
    waits: &mut usize,
    waited_ms: &mut u64,
    queued: &Queued,
) -> Result<(), BridgeError> {
    let (max_waits, wait_seconds) = crate::config::queue_settings();
    // 单次时长：设置里写死了就用它，否则跟随上游建议（已在分类时钳过）
    let wait_ms = if wait_seconds > 0 {
        wait_seconds.saturating_mul(1000)
    } else {
        queued.retry_after_ms
    };
    let budget_ms = crate::bridge::sse_first_event_deadline().as_millis() as u64;
    if *waits >= max_waits as usize || waited_ms.saturating_add(wait_ms) > budget_ms {
        return Err(queued_error(queued, *waited_ms));
    }
    *waits += 1;
    *waited_ms += wait_ms;
    tracing::info!(
        "⏳ Qoder 上游模型排队中{}，{} 秒后重试（第 {}/{} 次）",
        if queued.detail.is_empty() {
            String::new()
        } else {
            format!("（{}）", queued.detail)
        },
        wait_ms / 1000,
        *waits,
        max_waits,
    );
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    Ok(())
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
            model: "qwen3.8-max".into(),
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
    fn body_carries_fixed_envelope_fields() {
        let body = build_body(&format(vec![user("你好")]), "uid-1");
        assert_eq!(body["session_type"], "qodercli");
        assert_eq!(body["agent_id"], "agent_common");
        assert_eq!(body["chat_task"], "FREE_INPUT");
        assert_eq!(body["stream"], true);
        assert_eq!(body["chat_context"]["text"], "你好");
        assert_eq!(body["parameters"]["max_tokens"], MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn session_id_is_stable_per_user_and_model() {
        let a = session_id_for("uid-1", "qwen3.8-max");
        let b = session_id_for("uid-1", "qwen3.8-max");
        let c = session_id_for("uid-2", "qwen3.8-max");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn envelope_body_is_unwrapped_and_parsed() {
        let payload =
            r#"{"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"你\"}}]}"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.content.as_deref(), Some("你")),
            _ => panic!("信封内层应是 OpenAI chunk"),
        }
    }

    #[test]
    fn business_error_in_envelope_becomes_bridge_error() {
        let payload = r#"{"statusCodeValue":429,"body":"{\"message\":\"quota\"}"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Err(BridgeError::UpstreamStatus { status, .. }) => assert_eq!(status, 429),
            _ => panic!("信封业务错误应转上游错误"),
        }
    }

    #[test]
    fn identity_falls_back_to_token_hash_without_jwt() {
        let identity = identity_of("not-a-jwt-token");
        assert!(!identity.user_id.is_empty());
        assert_eq!(identity.machine_id.len(), 32);
    }

    #[test]
    fn queue_signal_recognizes_nested_10605() {
        // 真实形态：HTTP 403 正文三层嵌套（外层 code=403，内层 message 又是 JSON 字符串）
        let raw = r#"{"code":"403","message":"{\"code\":\"10605\",\"message\":\"{\\\"isQueued\\\":true,\\\"retryAfterSeconds\\\":20,\\\"queueType\\\":\\\"p3\\\",\\\"serviceAvailable\\\":false}\"}"}"#;
        let queued = scan_queue_signals(raw).expect("嵌套 10605 必须识别成排队");
        assert_eq!(queued.retry_after_ms, 20_000);
        assert!(queued.detail.contains("队列 p3"));
        assert!(queued.detail.contains("服务不可用"));
    }

    #[test]
    fn queue_signal_falls_back_to_bare_text() {
        let raw = "upstream returned error: \\\"code\\\":\\\"10605\\\"";
        assert!(scan_queue_signals(raw).is_some());
        let not_queue =
            r#"{"code":"403","message":"{\"code\":\"105\",\"message\":\"token expired\"}"}"#;
        assert!(
            scan_queue_signals(not_queue).is_none(),
            "鉴权失效绝不能判成排队"
        );
    }

    #[test]
    fn queue_wait_ms_follows_and_clamps_suggestion() {
        assert_eq!(queue_wait_ms(None), 15_000);
        assert_eq!(queue_wait_ms(Some(20)), 20_000);
        assert_eq!(queue_wait_ms(Some(2)), 5_000);
        assert_eq!(queue_wait_ms(Some(300)), 30_000);
        // 数字字符串形态同值（上游给过 "20"）
        let raw = r#"{"isQueued":true,"waitTime":"12"}"#;
        assert_eq!(scan_queue_signals(raw).unwrap().retry_after_ms, 12_000);
    }
}
