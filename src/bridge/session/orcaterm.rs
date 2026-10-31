//! OrcaTerm（腾讯云遨驰终端）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/orcaterm`：
//! `POST /assistant/conversation` 建会话 → `POST /assistant/chat?…` 拿 SSE，
//! 鉴权是 `Authorization: Bearer <oauth_access_token>`。
//! 上游按 `conversationId` 留存历史且只有一条 `start` 输入通道，因此多轮
//! history 在这里拍平成单条 prompt（与 Agent2API 的 `build_prompt` 同口径）。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

/// 对话 SSE 的查询串（客户端逐字使用）。
const ASSISTANT_QUERY: &str = "name=orcaterm&mode=0&csrfCode=&sseresume=true";
/// 桌面客户端 UA（上游按它识别客户端形态）。
const USER_AGENT: &str = "OrcaTerm/1.6.4";

pub struct OrcaTermBridge {
    base: String,
    token: String,
    client: reqwest::Client,
}

impl OrcaTermBridge {
    pub fn new(base: impl Into<String>, token: impl Into<String>, client: reqwest::Client) -> Self {
        let token = token.into();
        let token = super::normalize_credential(&token, &["authorization:", "bearer"]);
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: token.to_string(),
            client,
        }
    }

    fn headers(&self, seq_id: &str) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.token),
            ),
            ("X-Product".to_string(), "orcaterm".to_string()),
            ("x-seqid".to_string(), seq_id.to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            (
                "Origin".to_string(),
                "https://orcaterm.cloud.tencent.com".to_string(),
            ),
            (
                "Referer".to_string(),
                "https://orcaterm.cloud.tencent.com/".to_string(),
            ),
        ]
    }

    fn post(
        &self,
        url: String,
        headers: &[(String, String)],
        body: &Value,
    ) -> reqwest::RequestBuilder {
        let mut req = self.client.post(url).header("Accept", "text/event-stream");
        for (name, value) in headers {
            req = req.header(name.as_str(), value);
        }
        req.json(body)
    }

    /// 建会话：成功后返回 `cid-<uuid>-orcaterm`。
    async fn create_conversation(&self) -> Result<String, BridgeError> {
        let uuid = uuid::Uuid::new_v4().to_string();
        let url = format!("{}/assistant/conversation", self.base);
        let body = json!({
            "id": uuid.clone(),
            "title": "新对话",
            "ts": chrono::Utc::now().timestamp_millis(),
            "customTitle": "",
            "aiTitle": "",
            "tags": ["orcaterm"],
        });
        let resp = self
            .post(url, &self.headers(&uuid), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("OrcaTerm 建会话失败：{}", head(&text)),
            ));
        }
        let value: Value = resp
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;
        if value.get("code").and_then(Value::as_i64).unwrap_or(-1) != 0 {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            let code = value
                .get("code")
                .map(|code| code.to_string())
                .unwrap_or_default();
            return Err(BridgeError::upstream_status(
                502,
                format!("OrcaTerm 建会话失败（code={code}：{message}）"),
            ));
        }
        Ok(format!("cid-{uuid}-orcaterm"))
    }

    async fn open_stream(
        &self,
        conversation_id: &str,
        model: &str,
        prompt: &str,
    ) -> Result<reqwest::Response, BridgeError> {
        let seq_id = uuid::Uuid::new_v4().to_string();
        let url = format!("{}/assistant/chat?{ASSISTANT_QUERY}", self.base);
        let body = json!({
            "conversationId": conversation_id,
            "user": {
                "id": "",
                "setting": {
                    "model": model,
                    "mcpServers": [],
                    "uiServers": [],
                    "tools": [],
                    "approvedTools": [],
                    "approvedMCPTools": [],
                    "enableAutoSubtaskExecution": false,
                    "env": [],
                    "hideToolMessage": true,
                    "language": "zh",
                },
            },
            "input": {
                "type": "start",
                "input": prompt,
                "id": seq_id,
                "emphasisPrompt": "",
                "files": [],
            },
            "stream": true,
        });
        let resp = self
            .post(url, &self.headers(&seq_id), &body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("OrcaTerm 对话失败：{}", head(&text)),
            ));
        }
        Ok(resp)
    }
}

#[async_trait]
impl Bridge for OrcaTermBridge {
    fn name(&self) -> &'static str {
        "orcaterm"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        let mut reasoning = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
            if let Some(text) = chunk.delta.reasoning {
                reasoning.push_str(&text);
            }
        }
        let mut message = ChatMessage::assistant(content);
        if !reasoning.is_empty() {
            message.reasoning = Some(reasoning);
        }
        Ok(ChatResponse {
            id: format!("orcaterm-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message,
            finish_reason: FinishReason::Stop,
            usage: UsageStats::default(),
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let prompt = build_prompt(req);
        let conversation = self.create_conversation().await?;
        let resp = self.open_stream(&conversation, &req.model, &prompt).await?;

        let id = format!("orcaterm-{}", uuid::Uuid::new_v4());
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
                                match decode_event(&event, &flow.id, &flow.model) {
                                    Decoded::Nothing => {}
                                    Decoded::Chunk(chunk) => pending.push_back(Ok(chunk)),
                                    Decoded::Done(chunk) => {
                                        pending.push_back(Ok(chunk));
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
                            if let Some(event) = decoder.finish() {
                                if let Decoded::Chunk(chunk) =
                                    decode_event(&event, &flow.id, &flow.model)
                                {
                                    pending.push_back(Ok(chunk));
                                }
                            }
                            pending.push_back(Ok(finish_chunk(&flow.id, &flow.model)));
                            flow.ended = true;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

/// 流式跨事件状态。
struct Flow {
    id: String,
    model: String,
    /// 已见到终止信号（不再读上游）。
    ended: bool,
}

/// 单条出站项。
enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done(ChatChunk),
    Err(BridgeError),
}

fn finish_chunk(id: &str, model: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
    }
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done(finish_chunk(id, model)),
        SseEvent::Data(payload) => {
            if let Some(error) = payload.trim().strip_prefix("[ERROR]") {
                return Decoded::Err(BridgeError::upstream_status(
                    502,
                    format!("OrcaTerm 流内错误：{}", head(error)),
                ));
            }
            let Ok(parsed) = serde_json::from_str::<Value>(payload) else {
                return Decoded::Nothing;
            };
            if parsed.get("type").and_then(Value::as_str) != Some("ai") {
                return Decoded::Nothing;
            }
            let reasoning_flag = parsed
                .get("reasoning")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let text = if reasoning_flag {
                parsed
                    .get("reasoningContent")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            } else {
                parsed.get("content").and_then(Value::as_str).unwrap_or("")
            };
            if text.is_empty() {
                return Decoded::Nothing;
            }
            let mut delta = ChatDelta::default();
            if reasoning_flag {
                delta.reasoning = Some(text.to_string());
            } else {
                delta.content = Some(text.to_string());
            }
            Decoded::Chunk(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta,
                finish_reason: None,
                usage: None,
            })
        }
    }
}

/// OpenAI messages 拍平成单条输入（上游只有 `start` 一条输入通道）。
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

    fn chat_format(messages: Vec<ChatMessage>) -> ChatFormat {
        ChatFormat {
            model: "m".into(),
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
    fn single_message_passes_through_verbatim() {
        assert_eq!(build_prompt(&chat_format(vec![user("你好")])), "你好");
    }

    #[test]
    fn multi_turn_history_is_flattened_with_role_prefixes() {
        let prompt = build_prompt(&chat_format(vec![user("第一问"), user("第二问")]));
        assert!(prompt.contains("[用户] 第一问"));
        assert!(prompt.contains("[用户] 第二问"));
        assert!(prompt.contains("请回应其中最后一条用户消息"));
    }

    #[test]
    fn done_event_maps_to_finish() {
        match decode_event(&SseEvent::Done, "x", "m") {
            Decoded::Done(chunk) => assert_eq!(chunk.finish_reason, Some(FinishReason::Stop)),
            _ => panic!("Done 事件应映射为结束"),
        }
    }

    #[test]
    fn reasoning_frame_is_split_from_content() {
        let payload = r#"{"type":"ai","reasoning":true,"reasoningContent":"想…"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Chunk(chunk) => {
                assert_eq!(chunk.delta.reasoning.as_deref(), Some("想…"));
                assert!(chunk.delta.content.is_none());
            }
            _ => panic!("推理帧应产出 reasoning delta"),
        }
    }

    #[test]
    fn error_frame_surfaces_as_bridge_error() {
        let payload = "[ERROR]: [LLM_ERROR]: quota".to_string();
        match decode_event(&SseEvent::Data(payload), "x", "m") {
            Decoded::Err(BridgeError::UpstreamStatus { message, .. }) => {
                assert!(message.contains("LLM_ERROR"));
            }
            _ => panic!("[ERROR] 帧应转成上游错误"),
        }
    }
}
