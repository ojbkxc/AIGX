//! CatPaw（美团 AI 客户端）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/catpaw`：上游是一套 **conversation 会话协议**，
//! 一次对话 = `round`（提交消息）→ `event(running)` → `turn`（SSE 执行）。
//!
//! AIGX 里每个请求新建一个会话（多轮历史拍平成单条 user 消息，与 KukuAI 同口径），
//! 以保证任何客户端形态都能工作；`turn` 的 SSE 每帧是 `{"message":{...}}`，
//! 正文块是**全量**文本，这里做差分后转成 OpenAI 风格的增量。
//!
//! 凭证：`X-Passport-Token` 的值（账号页粘贴）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Map, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

const APP_KEY: &str = "fe_com.sankuai.catpaw.external.front";
const CLIENT_VERSION: &str = "1.0.1";
const SOURCE: &str = "CatX";
const MODE: &str = "CATX_APP";
const PERMISSION_MODE: &str = "unsafeBypassPermissions";
const TOOL_VERSION: &str = "2.0.2";

pub struct CatPawBridge {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl CatPawBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        }
    }

    fn headers(&self, accept: &str) -> Vec<(String, String)> {
        vec![
            ("Accept".to_string(), accept.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            (
                "M-TRACEID".to_string(),
                uuid::Uuid::new_v4().simple().to_string(),
            ),
            ("M-APPKEY".to_string(), APP_KEY.to_string()),
            ("gray-set".to_string(), "new-agent-sdk".to_string()),
            ("enableHeartBeat".to_string(), "true".to_string()),
            (
                "X-Agent-Version".to_string(),
                CLIENT_VERSION.to_string(),
            ),
            (
                "Cookie".to_string(),
                format!("X-Passport-Token={}", self.token.trim()),
            ),
        ]
    }

    fn post(&self, path: &str, body: &Value, accept: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .post(format!("{}{path}", self.base))
            .header("Accept", accept);
        for (name, value) in self.headers(accept) {
            req = req.header(name.as_str(), value.as_str());
        }
        req.json(body)
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<(), BridgeError> {
        let resp = self
            .post(path, body, "application/json")
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("CatPaw {path} 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl Bridge for CatPawBridge {
    fn name(&self) -> &'static str {
        "catpaw"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        let mut reasoning = String::new();
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
            if let Some(reason) = chunk.finish_reason {
                finish = reason;
            }
            if let Some(stats) = chunk.usage {
                usage = stats;
            }
        }
        let mut message = ChatMessage::assistant(content);
        message.reasoning = (!reasoning.is_empty()).then_some(reasoning);
        Ok(ChatResponse {
            id: format!("catpaw-{}", uuid::Uuid::new_v4()),
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
        let conversation_id = uuid::Uuid::new_v4().to_string();
        let turn_request_id = uuid::Uuid::new_v4().to_string();
        let prompt = build_prompt(req);
        let model_type = model_type_of(&req.model);
        let message = user_message(&prompt);

        let round = json!({
            "conversationId": conversation_id,
            "source": SOURCE,
            "messages": [message.clone()],
            "modelType": model_type,
            "mode": MODE,
            "permissionMode": PERMISSION_MODE,
            "toolVersion": TOOL_VERSION,
        });
        self.post_json("/api/agent/conversation/round", &round)
            .await?;
        self.post_json(
            "/api/agent/conversation/event",
            &json!({
                "conversationId": conversation_id,
                "eventType": "conversation",
                "data": { "status": "running" },
            }),
        )
        .await?;

        let turn = json!({
            "conversationId": conversation_id,
            "turnRequestId": turn_request_id,
            "source": SOURCE,
            "action": "turn",
            "message": message,
            "modelType": model_type,
            "mode": MODE,
            "permissionMode": PERMISSION_MODE,
            "toolVersion": TOOL_VERSION,
            "toolConfigs": [],
            "availableTools": [],
        });
        let resp = self
            .post("/api/agent/conversation/turn", &turn, "text/event-stream")
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("CatPaw turn 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("catpaw-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let bytes = resp.bytes_stream();

        let stream = futures::stream::unfold(
            (
                bytes,
                SseDecoder::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model,
                    previous_text: String::new(),
                    previous_reasoning: String::new(),
                    ended: false,
                },
            ),
            |(mut bytes, mut decoder, mut pending, mut flow)| async move {
                loop {
                    if let Some(item) = pending.pop_front() {
                        return Some((item, (bytes, decoder, pending, flow)));
                    }
                    if flow.ended {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => {
                            let events = decoder.feed(chunk.as_ref());
                            for event in events {
                                match decode_event(&event, &mut flow) {
                                    Decoded::Nothing => {}
                                    Decoded::Chunk(chunk) => pending.push_back(Ok(chunk)),
                                    Decoded::Done => {
                                        pending.push_back(Ok(ChatChunk {
                                            id: flow.id.clone(),
                                            model: flow.model.clone(),
                                            delta: ChatDelta::default(),
                                            finish_reason: Some(FinishReason::Stop),
                                            usage: None,
                                        }));
                                        flow.ended = true;
                                    }
                                    Decoded::Err(err) => {
                                        pending.push_back(Err(err));
                                        flow.ended = true;
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            pending.push_back(Ok(ChatChunk {
                                id: flow.id.clone(),
                                model: flow.model.clone(),
                                delta: ChatDelta::default(),
                                finish_reason: Some(FinishReason::Stop),
                                usage: None,
                            }));
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
    previous_text: String,
    previous_reasoning: String,
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done,
    Err(BridgeError),
}

fn decode_event(event: &SseEvent, flow: &mut Flow) -> Decoded {
    let payload = match event {
        SseEvent::Done => return Decoded::Done,
        SseEvent::Data(payload) => payload,
    };
    let trimmed = payload.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        return if trimmed == "[DONE]" {
            Decoded::Done
        } else {
            Decoded::Nothing
        };
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Decoded::Nothing;
    };
    if let Some(error) = value.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("上游错误");
        return Decoded::Err(BridgeError::upstream_status(
            502,
            format!("CatPaw 流内错误：{message}"),
        ));
    }
    let Some(message) = value.get("message") else {
        return Decoded::Nothing;
    };
    let finished = message
        .get("finished")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut text = String::new();
    let mut reasoning = String::new();
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        for block in blocks {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            let value = block.get("text").and_then(Value::as_str).unwrap_or("");
            if kind != "text" || value.is_empty() {
                continue;
            }
            if block
                .get("reasoning")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                reasoning.push_str(value);
            } else {
                text.push_str(value);
            }
        }
    }

    let mut delta = ChatDelta::default();
    if let Some(suffix) = suffix_after(&text, &flow.previous_text) {
        delta.content = Some(suffix);
        flow.previous_text = text.clone();
    }
    if let Some(suffix) = suffix_after(&reasoning, &flow.previous_reasoning) {
        delta.reasoning = Some(suffix);
        flow.previous_reasoning = reasoning.clone();
    }
    let usage = message.get("usage").map(usage_of);

    if finished {
        return Decoded::Chunk(ChatChunk {
            id: flow.id.clone(),
            model: flow.model.clone(),
            delta,
            finish_reason: Some(FinishReason::Stop),
            usage,
        });
    }
    if delta.content.is_none() && delta.reasoning.is_none() && usage.is_none() {
        return Decoded::Nothing;
    }
    Decoded::Chunk(ChatChunk {
        id: flow.id.clone(),
        model: flow.model.clone(),
        delta,
        finish_reason: None,
        usage,
    })
}

/// 全量文本的差分（跨帧拼接的正文在每帧都是完整前缀）。
fn suffix_after(current: &str, previous: &str) -> Option<String> {
    if current.len() <= previous.len() {
        return None;
    }
    if let Some(rest) = current.strip_prefix(previous) {
        return (!rest.is_empty()).then(|| rest.to_string());
    }
    let previous_chars = previous.chars().count();
    let suffix: String = current.chars().skip(previous_chars).collect();
    (!suffix.is_empty()).then_some(suffix)
}

fn usage_of(usage: &Value) -> UsageStats {
    let number = |keys: &[&str]| -> u64 {
        keys.iter()
            .find_map(|key| usage.get(*key).and_then(Value::as_i64))
            .unwrap_or(0) as u64
    };
    let prompt = number(&["prompt_tokens", "promptTokens", "input_tokens"]);
    let completion = number(&["completion_tokens", "completionTokens", "output_tokens"]);
    let total = number(&["total_tokens", "totalTokens"]);
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

/// 一轮会话的 user 消息（CatPaw 的块形态）。
fn user_message(prompt: &str) -> Value {
    json!({
        "type": "user",
        "messageId": uuid::Uuid::new_v4().to_string(),
        "content": [{ "type": "text", "text": prompt }],
        "finished": true,
    })
}

/// 上游的 `modelType`：模型名是整数时直接用，否则取 0（由上游按默认档处理）。
fn model_type_of(model: &str) -> i64 {
    model.trim().parse::<i64>().unwrap_or(0)
}

/// 把 OpenAI messages 拍平成单条 prompt（单轮会话）。
pub fn build_prompt(req: &ChatFormat) -> String {
    let messages: Vec<&ChatMessage> = req
        .messages
        .iter()
        .filter(|m| !m.content_str().trim().is_empty())
        .collect();
    if messages.is_empty() {
        return "你好".to_string();
    }
    if messages.len() == 1 {
        return messages[0].content_str().to_string();
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push("[以下是本轮之前的对话记录，供你了解上下文]".to_string());
    for message in messages {
        let role = match message.role {
            Role::System => "系统指令",
            Role::Assistant => "助手",
            _ => "用户",
        };
        lines.push(format!("[{role}] {}", message.content_str()));
    }
    lines.push("[对话记录结束；请回应其中最后一条用户消息]".to_string());
    lines.join("\n\n")
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
            model: "1".into(),
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
    fn model_type_parses_numeric_model_names() {
        assert_eq!(model_type_of("1"), 1);
        assert_eq!(model_type_of("glm"), 0);
    }

    #[test]
    fn user_message_uses_block_content() {
        let message = user_message("你好");
        assert_eq!(message["type"], "user");
        assert_eq!(message["content"][0]["type"], "text");
        assert_eq!(message["content"][0]["text"], "你好");
    }

    #[test]
    fn suffix_diff_emits_only_new_tail() {
        assert_eq!(suffix_after("你好世界", "你好"), Some("世界".to_string()));
        assert_eq!(suffix_after("你好", "你好"), None);
        assert_eq!(suffix_after("hi", "你好"), None);
    }

    #[test]
    fn finished_message_emits_finish_reason() {
        let mut flow = Flow {
            id: "x".into(),
            model: "m".into(),
            previous_text: String::new(),
            previous_reasoning: String::new(),
            ended: false,
        };
        let payload = r#"{"message":{"content":[{"type":"text","text":"你好"}],"finished":true}}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), &mut flow) {
            Decoded::Chunk(chunk) => {
                assert_eq!(chunk.delta.content.as_deref(), Some("你好"));
                assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
            }
            _ => panic!("finished 消息应产出结束帧"),
        }
    }

    #[test]
    fn prompt_flattens_multi_turn_history() {
        let prompt = build_prompt(&format(vec![user("第一问"), user("第二问")]));
        assert!(prompt.contains("[用户] 第一问"));
        assert!(prompt.contains("[用户] 第二问"));
    }
}
