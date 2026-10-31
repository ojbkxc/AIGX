//! KukuAI（百度文库 GenFlowPro，`kuku.baidu.com`）登录态 Bridge。
//!
//! 协议抄自 Agent2API `providers/kuku`：Cookie（`BDUSS` / `STOKEN`）鉴权，
//! 每次对话三步：
//! 1. `GET /api/genflowpro/common/userreport` 换会话三件套（`bdstoken`/`uinfo`/`uk`）；
//! 2. `POST /wenchain/genflowpro/sendmsg` 建会话（拿 `session_id` / `reply_id`）；
//! 3. `POST /wenchain/genflow/idallochstr` 分配算力 + `sessionswitch`（容错）；
//! 4. `POST /wenchain/genflowpro/sse/getchatcontent` 拿 SSE（`TEXT_BLOCK_DELTA` 增量）。
//!
//! 三件套按 Cookie 做进程级缓存（600 秒，与 Agent2API 同值）。

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

const BASE_URL: &str = "https://kuku.baidu.com";
const APP_ID: i64 = 123971023;
const CHANNEL: &str = "chunlei";
const CHANNEL_NAME: &str = "kuku_web_genflowpro_v1";
const WEB_QUERY: &str = "clienttype=400&app_id=123971023&web=1&channel=chunlei&version=1.4.4";
const THINK_MODE: i64 = 3;
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";
const TOKEN_TTL_MS: i64 = 600_000;

pub struct KukuBridge {
    base: String,
    cookie: String,
    client: reqwest::Client,
}

#[derive(Clone)]
struct Triple {
    bdstoken: String,
    uinfo: String,
    uk: String,
}

impl KukuBridge {
    pub fn new(
        base: impl Into<String>,
        cookie: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        let cookie = cookie.into();
        let cookie = super::normalize_credential(&cookie, &["cookie:", "cookie="]);
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            cookie: cookie.to_string(),
            client,
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Cookie".to_string(), self.cookie.clone()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            ("Referer".to_string(), format!("{BASE_URL}/genflowpro")),
            ("Origin".to_string(), BASE_URL.to_string()),
            ("Accept-Language".to_string(), "zh-CN,zh;q=0.9".to_string()),
        ]
    }

    async fn get_json(&self, url: &str) -> Result<Value, BridgeError> {
        let mut req = self.client.get(url);
        for (name, value) in self.headers() {
            req = req.header(name, value);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("KukuAI 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        resp.json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))
    }

    async fn post_json(&self, url: &str, body: &Value) -> Result<Value, BridgeError> {
        let mut req = self.client.post(url);
        for (name, value) in self.headers() {
            req = req.header(name, value);
        }
        let resp = req
            .json(body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("KukuAI 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        resp.json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))
    }

    /// 取（必要时刷新）会话三件套。
    async fn tokens(&self) -> Result<Triple, BridgeError> {
        let now = chrono::Utc::now().timestamp_millis();
        if let Some(triple) = cached_token(&self.cookie, now) {
            return Ok(triple);
        }
        let value = self
            .get_json(&format!(
                "{}/api/genflowpro/common/userreport?{WEB_QUERY}",
                self.base
            ))
            .await?;
        let errno = value.get("errno").and_then(Value::as_i64).unwrap_or(-1);
        if errno != 0 {
            let message = value
                .get("show_msg")
                .or_else(|| value.get("errmsg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            return Err(BridgeError::upstream_status(
                401,
                format!("KukuAI 会话校验未通过（errno={errno}：{message}），请重新粘贴完整 Cookie"),
            ));
        }
        let data = value.get("data").cloned().unwrap_or(Value::Null);
        let text_of = |key: &str| -> String {
            data.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        let uk = data
            .get("uk")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                data.get("uk")
                    .and_then(Value::as_i64)
                    .map(|n| n.to_string())
            })
            .unwrap_or_default();
        let triple = Triple {
            bdstoken: text_of("bdstoken"),
            uinfo: text_of("uinfo"),
            uk,
        };
        if triple.bdstoken.is_empty() {
            return Err(BridgeError::upstream_status(
                502,
                "KukuAI userreport 返回缺少 bdstoken（上游响应异常）",
            ));
        }
        store_token(&self.cookie, &triple, now);
        Ok(triple)
    }

    fn query(triple: &Triple) -> String {
        format!(
            "{WEB_QUERY}&bdstoken={}&uinfo={}&uk={}",
            triple.bdstoken, triple.uinfo, triple.uk
        )
    }

    async fn create_session(
        &self,
        triple: &Triple,
        prompt: &str,
        model: &str,
    ) -> Result<(String, String), BridgeError> {
        let query = Self::query(triple);
        let csi = uuid::Uuid::new_v4().to_string();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let show_text = json!({
            "skills": [], "experts": [], "model_name": model,
            "model_display_name": model, "think_mode": THINK_MODE,
            "richInputContent": [{"type": "text", "text": prompt}], "fileInfo": []
        });
        let client_added = json!({
            "client_session_id": csi, "permission_type": 0, "clienttype": 400,
            "app_id": APP_ID, "web": 1, "channel": CHANNEL, "version": "1.4.4",
            "bdstoken": triple.bdstoken, "uinfo": triple.uinfo, "uk": triple.uk
        });
        let body = json!({
            "type": "message", "sub_type": "chat_create",
            "client_session_id": csi, "session_id": "", "uk": "", "cid": 0,
            "channel": CHANNEL_NAME, "device_type": 400,
            "created_at": now_ms, "msg_type": 1, "sync_type": 0, "sync_id": "",
            "v": short_hex(24),
            "data": {
                "project_type": 1, "text": prompt,
                "rich_input_params": [{
                    "id": "textId", "version": "1.0", "type": "text",
                    "text": prompt, "data": {"content": prompt}
                }],
                "fsid": [], "quotes": [], "skills": [], "experts": [],
                "model_name": model, "model_display_name": model,
                "think_mode": THINK_MODE,
                "show_text": show_text.to_string(),
                "premake_data": {}, "custom_instructions": "",
                "memory_sign": true, "skill_dig_sign": true,
                "client_added": client_added.to_string()
            }
        });
        let value = self
            .post_json(
                &format!("{}/wenchain/genflowpro/sendmsg?{query}", self.base),
                &body,
            )
            .await?;
        let code = value
            .get("status")
            .and_then(|status| status.get("code"))
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        if code != 0 {
            let message = value
                .get("status")
                .and_then(|status| status.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            return Err(BridgeError::upstream_status(
                502,
                format!("KukuAI 建会话失败（status.code={code}：{message}）"),
            ));
        }
        let info = value.get("data").cloned().unwrap_or(Value::Null);
        let session_id = info
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let reply_id = info
            .get("reply_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if session_id.is_empty() {
            return Err(BridgeError::upstream_status(
                502,
                "KukuAI 建会话返回缺少 session_id（上游响应异常）",
            ));
        }
        Ok((session_id, reply_id))
    }
}

#[async_trait]
impl Bridge for KukuBridge {
    fn name(&self) -> &'static str {
        "kuku"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self.chat_stream(req, ctx).await?;
        let mut content = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
        }
        Ok(ChatResponse {
            id: format!("kuku-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message: ChatMessage::assistant(content),
            finish_reason: FinishReason::Stop,
            usage: UsageStats::default(),
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let triple = self.tokens().await?;
        let prompt = build_prompt(req);
        let (session_id, reply_id) = self.create_session(&triple, &prompt, &req.model).await?;
        let query = Self::query(&triple);

        // 分配算力 + 切会话：容错（浏览器也会调这两步，失败不阻断对话）
        let _ = self
            .post_json(
                &format!("{}/wenchain/genflow/idallochstr?{query}", self.base),
                &json!({ "channel": CHANNEL_NAME, "gen_type": 2 }),
            )
            .await;
        let _ = self
            .post_json(
                &format!(
                    "{}/api/genflowpro/workspace/sessionswitch?{query}",
                    self.base
                ),
                &json!({ "session_id": session_id, "op": 1, "full_access_enabled": 0 }),
            )
            .await;

        let url = format!(
            "{}/wenchain/genflowpro/sse/getchatcontent?{query}",
            self.base
        );
        let payload = chat_payload(&prompt, &req.model, &session_id, &reply_id);
        let mut builder = self.client.post(&url).header("Accept", "text/event-stream");
        for (name, value) in self.headers() {
            builder = builder.header(name, value);
        }
        let resp = builder
            .json(&payload)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("KukuAI 对话失败：{}", head(&text)),
            ));
        }

        let id = format!("kuku-{}", uuid::Uuid::new_v4());
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
    ended: bool,
}

enum Decoded {
    Nothing,
    Chunk(ChatChunk),
    Done,
    Err(BridgeError),
}

fn decode_event(event: &SseEvent, id: &str, model: &str) -> Decoded {
    match event {
        SseEvent::Done => Decoded::Done,
        SseEvent::Data(payload) => {
            let Ok(value) = serde_json::from_str::<Value>(payload) else {
                return Decoded::Nothing;
            };
            match value.get("type").and_then(Value::as_str) {
                Some("TEXT_BLOCK_DELTA") => {
                    let delta = value
                        .get("data")
                        .and_then(|data| data.get("delta"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    if delta.is_empty() {
                        return Decoded::Nothing;
                    }
                    Decoded::Chunk(ChatChunk {
                        id: id.to_string(),
                        model: model.to_string(),
                        delta: ChatDelta {
                            content: Some(delta.to_string()),
                            reasoning: None,
                            tool_calls: None,
                        },
                        finish_reason: None,
                        usage: None,
                    })
                }
                Some("REPLY_END") | Some("DIALOGUE_END") => Decoded::Done,
                Some("ERROR") => {
                    let message = value
                        .get("data")
                        .and_then(|data| data.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("上游错误");
                    Decoded::Err(BridgeError::upstream_status(
                        502,
                        format!("KukuAI 流内错误：{message}"),
                    ))
                }
                _ => Decoded::Nothing,
            }
        }
    }
}

fn chat_payload(prompt: &str, model: &str, session_id: &str, reply_id: &str) -> Value {
    let show_text = json!({
        "skills": [], "experts": [], "model_name": model,
        "model_display_name": model, "think_mode": THINK_MODE,
        "richInputContent": [{"type": "text", "text": prompt}], "fileInfo": []
    });
    json!({
        "project_type": 1, "text": prompt,
        "rich_input_params": [{
            "id": "textId", "version": "1.0", "type": "text",
            "text": prompt, "data": {"content": prompt}
        }],
        "fsid": [], "quotes": [], "skills": [], "experts": [],
        "model_name": model, "model_display_name": model,
        "think_mode": THINK_MODE,
        "show_text": show_text.to_string(),
        "premake_data": {}, "custom_instructions": "",
        "memory_sign": true, "skill_dig_sign": true,
        "session_id": session_id, "reply_id": reply_id
    })
}

/// OpenAI messages 拍平成单条输入（上游没有多轮接续接口）。
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

// ── 会话三件套的进程级缓存（key = Cookie 串）────────────────────────

fn slots() -> &'static Mutex<HashMap<String, (Triple, i64)>> {
    static SLOTS: OnceLock<Mutex<HashMap<String, (Triple, i64)>>> = OnceLock::new();
    SLOTS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cached_token(key: &str, now: i64) -> Option<Triple> {
    let guard = slots().lock().ok()?;
    let (triple, refreshed_at) = guard.get(key)?;
    if now - *refreshed_at >= TOKEN_TTL_MS {
        return None;
    }
    Some(triple.clone())
}

fn store_token(key: &str, triple: &Triple, now: i64) {
    if let Ok(mut guard) = slots().lock() {
        guard.insert(key.to_string(), (triple.clone(), now));
    }
}

fn short_hex(len: usize) -> String {
    uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(len)
        .collect()
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
            model: "auto".into(),
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
    fn text_block_delta_becomes_content() {
        let payload = r#"{"type":"TEXT_BLOCK_DELTA","data":{"delta":"你"}}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Chunk(chunk) => assert_eq!(chunk.delta.content.as_deref(), Some("你")),
            _ => panic!("TEXT_BLOCK_DELTA 应产出正文增量"),
        }
    }

    #[test]
    fn reply_end_finishes_stream() {
        let payload = r#"{"type":"REPLY_END"}"#;
        match decode_event(&SseEvent::Data(payload.to_string()), "x", "m") {
            Decoded::Done => {}
            _ => panic!("REPLY_END 应结束流"),
        }
    }

    #[test]
    fn single_message_prompt_passes_through() {
        assert_eq!(build_prompt(&format(vec![user("你好")])), "你好");
    }
}
