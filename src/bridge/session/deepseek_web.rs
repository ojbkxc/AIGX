//! DeepSeek 网页版（`chat.deepseek.com`）登录态 Bridge —— 私有协议。
//!
//! 协议移植自 ds2api（MIT；`internal/deepseek/client/*` +
//! `internal/sse/parser.go`）与上游 Agent2API 的凭证形态注释：
//!
//! ── 请求链（每次对话）────────────────────────────────────────
//! 1. `POST /api/v0/chat_session/create` `{agent:"chat"}` →
//!    `data.biz_data.id`（或 `biz_data.chat_session.id`）= 会话 id；
//! 2. `POST /api/v0/chat/create_pow_challenge` `{target_path:
//!    "/api/v0/chat/completion"}` → `data.biz_data.challenge`（DeepSeekHashV1，
//!    本地求解出 nonce 后组 `x-ds-pow-response` 头，见 [`super::deepseek_pow`]）；
//! 3. `POST /api/v0/chat/completion` `{chat_session_id, model_type,
//!    parent_message_id:null, prompt, ref_file_ids:[], thinking_enabled,
//!    search_enabled}`，响应是 **SSE 补丁帧**（见下）。
//!
//! 凭证：网页端 userToken（Bearer）。用户常粘整段 Cookie 或
//! `userToken=…` 包装，这里按名提取 `userToken` 的值。
//!
//! ── 响应帧语义（SSE 补丁流，与 OpenAI 完全不同）──────────────
//! 每帧 `{"v": <值>, "p": "<JSON Patch 式路径>", "o": "<操作>"}`：
//!   - `p:"response/content"` / `p:"response/thinking_content"`：v 是**增量**
//!     文本（content=正文、thinking_content=思考）；
//!   - `p:"response/fragments"` + `o:"APPEND"`：v 是 fragment 数组
//!     `[{type:"RESPONSE"|"THINK", content:"…"}]`，content 是增量；
//!   - `p:"response/status"`（或嵌套片段 status）`v:"FINISHED"`：终态；
//!   - `p:"response/fragments/<n>/status"` 与 quasi_status / elapsed_secs /
//!     token_usage / pending_fragment / conversation_mode / search_status：
//!     噪声帧，跳过（ds2api 的 skip 表）。
//! 上游不发 `[DONE]`，由本模块在 FINISHED / EOF 时收尾。
//!
//! 模型映射：`deepseek-v4-flash`→model_type=default、`deepseek-v4-pro`→
//! expert、`deepseek-v4-vision`→vision；`-nothinking` 后缀关思考；
//! `deepseek-chat`/`deepseek-reasoner` 等旧名按 ds2api 的别名表归一。
//! 未知模型回落 default + 关思考（最保守，能用）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::VecDeque;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};

use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, FinishReason, Role, UsageStats,
};
use crate::sse::{SseDecoder, SseEvent};

use super::deepseek_pow::{solve, PowChallenge};

/// 上游补丁帧里要跳过的路径片段（ds2api `SkipContainsPatterns`）。
const SKIP_CONTAINS: &[&str] = &[
    "quasi_status",
    "elapsed_secs",
    "token_usage",
    "pending_fragment",
    "conversation_mode",
    "fragments/-1/status",
    "fragments/-2/status",
    "fragments/-3/status",
];

pub struct DeepSeekWebBridge {
    base: String,
    token: String,
    /// provider 默认头 + 账号级额外头（`{credential}` 已由 adapter 替换）。
    headers: Vec<(String, String)>,
    /// 账号级的整段 Cookie（可选；从额外头里取，部分风控场景需要）。
    cookie: Option<String>,
    client: reqwest::Client,
}

impl DeepSeekWebBridge {
    pub fn new(
        base: impl Into<String>,
        token: impl Into<String>,
        headers: Vec<(String, String)>,
        client: reqwest::Client,
    ) -> Self {
        let cookie = headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("cookie"))
            .map(|(_, value)| value.clone())
            .filter(|value| !value.trim().is_empty());
        let token = token.into();
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            token: extract_user_token(&token),
            headers,
            cookie,
            client,
        }
    }

    fn post(&self, path: &str, body: &Value) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .post(format!("{}{path}", self.base))
            .header("Content-Type", "application/json")
            .bearer_auth(&self.token);
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", cookie.as_str());
        }
        request.json(body)
    }

    /// 通用 JSON 响应的成败判定：HTTP 200 且 `code == 0` 且 `data.biz_code == 0`。
    async fn post_json(&self, path: &str, body: &Value) -> Result<Value, BridgeError> {
        let resp = self
            .post(path, body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(format!("DeepSeek {path} 请求失败：{e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status != 200 {
            return Err(BridgeError::upstream_status(
                status,
                format!("DeepSeek {path} 返回 HTTP {status}：{}", head(&text)),
            ));
        }
        let code = value.get("code").and_then(Value::as_i64).unwrap_or(-1);
        let biz_code = value
            .pointer("/data/biz_code")
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        if code != 0 || biz_code != 0 {
            let message = biz_message(&value);
            let http = if is_auth_failure(code, biz_code) {
                401
            } else {
                502
            };
            return Err(BridgeError::upstream_status(
                http,
                format!("DeepSeek {path} 业务失败：{message}"),
            ));
        }
        Ok(value)
    }

    async fn create_session(&self) -> Result<String, BridgeError> {
        let value = self
            .post_json("/api/v0/chat_session/create", &json!({ "agent": "chat" }))
            .await?;
        let id = value
            .pointer("/data/biz_data/id")
            .and_then(Value::as_str)
            .or_else(|| {
                value
                    .pointer("/data/biz_data/chat_session/id")
                    .and_then(Value::as_str)
            })
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| BridgeError::upstream_status(502, "DeepSeek 会话创建响应缺少会话 id"))?;
        Ok(id.to_string())
    }

    async fn pow_header(&self) -> Result<String, BridgeError> {
        let value = self
            .post_json(
                "/api/v0/chat/create_pow_challenge",
                &json!({ "target_path": "/api/v0/chat/completion" }),
            )
            .await?;
        let challenge = value
            .pointer("/data/biz_data/challenge")
            .and_then(PowChallenge::from_value)
            .ok_or_else(|| BridgeError::upstream_status(502, "DeepSeek PoW 挑战解析失败"))?;
        let answer = solve(&challenge).ok_or_else(|| {
            BridgeError::upstream_status(502, "DeepSeek PoW 求解失败（算法或难度异常）")
        })?;
        challenge
            .header_with_answer(answer)
            .ok_or_else(|| BridgeError::upstream_status(502, "DeepSeek PoW 头序列化失败"))
    }
}

#[async_trait]
impl Bridge for DeepSeekWebBridge {
    fn name(&self) -> &'static str {
        "deepseek-web"
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
            id: format!("deepseek-web-{}", uuid::Uuid::new_v4()),
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
        let session_id = self.create_session().await?;
        let pow = self.pow_header().await?;
        let model = resolve_model(&req.model);
        let prompt = build_prompt(req);
        let payload = json!({
            "chat_session_id": session_id,
            "model_type": model.model_type,
            "parent_message_id": Value::Null,
            "prompt": prompt,
            "ref_file_ids": [],
            "thinking_enabled": model.thinking,
            "search_enabled": false,
        });
        let resp = self
            .post("/api/v0/chat/completion", &payload)
            .header("x-ds-pow-response", &pow)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(format!("DeepSeek completion 请求失败：{e}")))?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let text = resp.text().await.unwrap_or_default();
            return Err(BridgeError::upstream_status(
                status,
                format!("DeepSeek completion 返回 HTTP {status}：{}", head(&text)),
            ));
        }

        let id = format!("deepseek-web-{}", uuid::Uuid::new_v4());
        let model_name = req.model.clone();
        let bytes = resp.bytes_stream();
        let stream = futures::stream::unfold(
            (
                bytes,
                SseDecoder::new(),
                VecDeque::<Result<ChatChunk, BridgeError>>::new(),
                Flow {
                    id,
                    model: model_name,
                    saw_content: false,
                    saw_finish: false,
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
                                decode_event(event, &mut flow, &mut pending);
                            }
                        }
                        Some(Err(e)) => {
                            pending.push_back(Err(BridgeError::Transport(e.to_string())));
                            flow.ended = true;
                        }
                        None => {
                            // 上游不发 [DONE]：EOF 时按「见过终态与否」收尾。
                            if !flow.saw_finish && flow.saw_content {
                                // 有内容但没等到 FINISHED：按截断报错（不谎报完整）
                                pending.push_back(Err(BridgeError::upstream_status(
                                    502,
                                    "DeepSeek 网页版流在收到完成信号之前结束（响应被截断）",
                                )));
                            } else if !flow.saw_finish && !flow.saw_content {
                                pending.push_back(Err(BridgeError::upstream_status(
                                    502,
                                    "DeepSeek 网页版返回空响应（没有任何内容）",
                                )));
                            }
                            // saw_finish = 终态帧已带 finish_reason，无需再补
                            flow.ended = true;
                        }
                    }
                }
            },
        );
        Ok(stream.boxed())
    }
}

/// 流内状态（EOF 收尾判定用）。
struct Flow {
    id: String,
    model: String,
    /// 已下发出过至少一个内容增量。
    saw_content: bool,
    /// 已下发出过 FINISHED 收尾帧。
    saw_finish: bool,
    ended: bool,
}

/// 解析一帧 SSE 事件 → 零到多个 chat chunk。
fn decode_event(
    event: SseEvent,
    flow: &mut Flow,
    pending: &mut VecDeque<Result<ChatChunk, BridgeError>>,
) {
    if flow.ended {
        return;
    }
    let payload = match event {
        SseEvent::Done => {
            // 上游意外发 [DONE] 也收下：补结束帧
            pending.push_back(Ok(finish_chunk(flow)));
            flow.ended = true;
            return;
        }
        SseEvent::Data(payload) => payload,
    };
    let trimmed = payload.trim();
    if trimmed.is_empty() || trimmed == "[DONE]" {
        if trimmed == "[DONE]" {
            pending.push_back(Ok(finish_chunk(flow)));
            flow.ended = true;
        }
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return; // 非 JSON 载荷（心跳等）忽略
    };
    let mut parts: Vec<(String, String)> = Vec::new(); // (text/thinking, 内容)
    let mut finished = false;
    collect_patch(&value, &mut parts, &mut finished);
    if let Some(code) = value.get("code").and_then(Value::as_i64) {
        if code != 0 {
            let message = biz_message(&value);
            pending.push_back(Err(BridgeError::upstream_status(
                if is_auth_failure(code, -1) { 401 } else { 502 },
                format!("DeepSeek 流内错误：{message}"),
            )));
            flow.ended = true;
            return;
        }
    }
    let mut delta = ChatDelta::default();
    for (kind, text) in parts {
        if text.is_empty() {
            continue;
        }
        flow.saw_content = true;
        if kind == "thinking" {
            delta
                .reasoning
                .get_or_insert_with(String::new)
                .push_str(&text);
        } else {
            delta
                .content
                .get_or_insert_with(String::new)
                .push_str(&text);
        }
    }
    if finished {
        flow.saw_finish = true;
        flow.ended = true;
        // 空响应判据：一个内容帧都没有就收尾 = 「成功但什么都没有」，
        // 报错而不是把空回答记成成功（与 chatglm 翻译器同口径）
        if !flow.saw_content {
            pending.push_back(Err(BridgeError::upstream_status(
                502,
                "DeepSeek 网页版返回空响应（没有任何内容）",
            )));
            return;
        }
        pending.push_back(Ok(ChatChunk {
            id: flow.id.clone(),
            model: flow.model.clone(),
            delta,
            finish_reason: Some(FinishReason::Stop),
            usage: Some(UsageStats::default()),
        }));
        return;
    }
    if delta.content.is_some() || delta.reasoning.is_some() {
        pending.push_back(Ok(ChatChunk {
            id: flow.id.clone(),
            model: flow.model.clone(),
            delta,
            finish_reason: None,
            usage: None,
        }));
    }
}

/// 递归收集一帧补丁里的内容增量。
///
/// 形态（ds2api `parser.go` 的精简版，只认 AIGX 需要的三类）：
/// 1. `p:"response/content"` / `"response/thinking_content"`：v = 增量文本
///    （v 也可能是 `{text}` 对象，取其 text）；
/// 2. `p:"response/fragments"` + `o:"APPEND"`：v = `[{type, content}]`；
/// 3. `v:"FINISHED"`（顶层 v 或嵌套 status 路径）＝ 终态。
fn collect_patch(value: &Value, parts: &mut Vec<(String, String)>, finished: &mut bool) {
    // 顶层 v 判终态
    if value.get("v").and_then(Value::as_str) == Some("FINISHED") {
        *finished = true;
        return;
    }
    let path = value
        .get("p")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if skip_path(&path) {
        return;
    }
    let has_v = value.get("v").is_some();
    if !has_v {
        // 数组形态：一帧里内嵌多条补丁（每条 {p, o, v}）
        if let Some(items) = value.as_array() {
            for item in items {
                collect_patch(item, parts, finished);
            }
        }
        return;
    }
    let v = value.get("v").cloned().unwrap_or(Value::Null);
    if path == "response/status" && v.as_str().map(str::trim) == Some("FINISHED") {
        *finished = true;
        return;
    }
    match path.as_str() {
        "response/content" => push_part(parts, "text", &v),
        "response/thinking_content" => push_part(parts, "thinking", &v),
        "response/fragments" => {
            if value
                .get("o")
                .and_then(Value::as_str)
                .map(|op| op.eq_ignore_ascii_case("APPEND"))
                .unwrap_or(false)
            {
                append_fragments(&v, parts);
            }
        }
        _ => {
            // 兜底：对象/数组里的嵌套补丁（`response` 包裹形态等）递归收集
            match &v {
                Value::Array(items) => {
                    for item in items {
                        collect_patch(item, parts, finished);
                    }
                }
                Value::Object(object) => {
                    if object.contains_key("p") || object.contains_key("fragments") {
                        collect_patch(&v, parts, finished);
                    }
                }
                _ => {}
            }
        }
    }
}

fn push_part(parts: &mut Vec<(String, String)>, kind: &str, v: &Value) {
    match v {
        Value::String(text) => {
            if text != "FINISHED" {
                parts.push((kind.to_string(), text.clone()));
            }
        }
        Value::Object(object) => {
            let text = object
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| object.get("content").and_then(Value::as_str))
                .unwrap_or("");
            if !text.is_empty() {
                parts.push((kind.to_string(), text.to_string()));
            }
        }
        Value::Array(items) => {
            for item in items {
                push_part(parts, kind, item);
            }
        }
        _ => {}
    }
}

/// `response/fragments` 的 APPEND 载荷：`[{type:"RESPONSE"|"THINK", content}]`。
fn append_fragments(v: &Value, parts: &mut Vec<(String, String)>) {
    let Some(items) = v.as_array() else {
        return;
    };
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .map(|t| t.to_ascii_uppercase())
            .unwrap_or_default();
        let text = object.get("content").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() {
            continue;
        }
        match kind.as_str() {
            "THINK" | "THINKING" => parts.push(("thinking".to_string(), text.to_string())),
            "RESPONSE" | "" => parts.push(("text".to_string(), text.to_string())),
            _ => parts.push(("text".to_string(), text.to_string())),
        }
    }
}

/// 噪声路径判定（ds2api skip 表）。
fn skip_path(path: &str) -> bool {
    // 片段级 status（`response/fragments/<n>/status`）是每片的完成态，非全文终态
    if path.starts_with("response/fragments/") && path.ends_with("/status") {
        return true;
    }
    SKIP_CONTAINS.iter().any(|fragment| path.contains(fragment))
}

/// 结束帧（空 delta + finish_reason stop）。
fn finish_chunk(flow: &Flow) -> ChatChunk {
    ChatChunk {
        id: flow.id.clone(),
        model: flow.model.clone(),
        delta: ChatDelta::default(),
        finish_reason: Some(FinishReason::Stop),
        usage: None,
    }
}

/// `data.biz_msg` / `msg` / `data.biz_data.msg` 的业务错误文案。
fn biz_message(value: &Value) -> String {
    for pointer in ["/data/biz_msg", "/msg", "/data/biz_data/msg", "/message"] {
        if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
            let text = text.trim();
            if !text.is_empty() {
                return text.to_string();
            }
        }
    }
    "未知错误".to_string()
}

/// 401/403 与业务码 40001-40003 = 登录态失效。
fn is_auth_failure(code: i64, biz_code: i64) -> bool {
    matches!(code, 40001..=40003) || matches!(biz_code, 40001..=40003)
}

/// 从整段 Cookie / `userToken=…` / 裸 token 里提取 userToken。
fn extract_user_token(raw: &str) -> String {
    let mut text = raw.trim();
    for prefix in ["Cookie:", "cookie:", "Bearer ", "bearer ", "Authorization:"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.trim();
        }
    }
    for part in text.split(';') {
        let part = part.trim();
        if let Some(value) = part
            .strip_prefix("userToken")
            .and_then(|v| v.strip_prefix('='))
        {
            return value.trim().trim_matches('"').to_string();
        }
    }
    text.trim_matches('"').to_string()
}

fn head(text: &str) -> String {
    text.chars().take(240).collect()
}

// ── 模型解析（对齐 ds2api `config/models.go`）──────────────────────

/// 解析后的上游模型参数。
struct ResolvedModel {
    model_type: &'static str,
    thinking: bool,
}

/// 请求模型名 → (model_type, thinking_enabled)。
///
/// `-nothinking` 后缀关思考；别名（deepseek-chat 等）归一后取类型；
/// 未知模型回落 default + 关思考（最保守）。
pub fn resolve_model(requested: &str) -> ResolvedModel {
    let raw = requested.trim();
    let (base, no_thinking) = match raw.strip_suffix("-nothinking") {
        Some(base) => (base, true),
        None => (raw, false),
    };
    let base = match base {
        "deepseek-chat" => "deepseek-v4-flash",
        "deepseek-reasoner" => "deepseek-v4-pro",
        "deepseek-chat-search" => "deepseek-v4-flash",
        "deepseek-reasoner-search" => "deepseek-v4-pro",
        other => other,
    };
    let (model_type, default_thinking) = match base {
        "deepseek-v4-flash" | "deepseek-v4-flash-search" => ("default", true),
        "deepseek-v4-pro" => ("expert", true),
        "deepseek-v4-vision" => ("vision", true),
        _ => ("default", false),
    };
    ResolvedModel {
        model_type,
        thinking: default_thinking && !no_thinking,
    }
}

/// 把 OpenAI messages 拍平成单条 prompt（单轮会话，与 CatPaw 同口径）。
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

    #[test]
    fn model_resolution_covers_aliases_and_nothinking() {
        assert_eq!(resolve_model("deepseek-v4-flash").model_type, "default");
        assert!(resolve_model("deepseek-v4-flash").thinking);
        assert_eq!(resolve_model("deepseek-v4-pro").model_type, "expert");
        assert_eq!(resolve_model("deepseek-chat").model_type, "default");
        assert_eq!(resolve_model("deepseek-reasoner").model_type, "expert");
        assert!(!resolve_model("deepseek-v4-flash-nothinking").thinking);
        // 未知模型：最保守回落
        assert_eq!(resolve_model("some-future-model").model_type, "default");
        assert!(!resolve_model("some-future-model").thinking);
    }

    #[test]
    fn extract_user_token_handles_wrappers() {
        assert_eq!(extract_user_token("abc"), "abc");
        assert_eq!(extract_user_token("Bearer abc"), "abc");
        assert_eq!(extract_user_token("userToken=xyz"), "xyz");
        assert_eq!(extract_user_token("Cookie: a=1; userToken=xyz; b=2"), "xyz");
    }

    #[test]
    fn patches_content_and_thinking_increment() {
        let mut flow = Flow {
            id: "x".into(),
            model: "m".into(),
            saw_content: false,
            saw_finish: false,
            ended: false,
        };
        let mut pending = VecDeque::new();
        decode_event(
            SseEvent::Data(r#"{"v":"你好","p":"response/content"}"#.into()),
            &mut flow,
            &mut pending,
        );
        assert_eq!(
            pending
                .pop_front()
                .unwrap()
                .unwrap()
                .delta
                .content
                .as_deref(),
            Some("你好")
        );
        decode_event(
            SseEvent::Data(r#"{"v":"思考中","p":"response/thinking_content"}"#.into()),
            &mut flow,
            &mut pending,
        );
        assert_eq!(
            pending
                .pop_front()
                .unwrap()
                .unwrap()
                .delta
                .reasoning
                .as_deref(),
            Some("思考中")
        );
    }

    #[test]
    fn fragments_append_is_collected() {
        let mut flow = Flow {
            id: "x".into(),
            model: "m".into(),
            saw_content: false,
            saw_finish: false,
            ended: false,
        };
        let mut pending = VecDeque::new();
        decode_event(
            SseEvent::Data(
                r#"{"p":"response/fragments","o":"APPEND","v":[{"type":"THINK","content":"想"},{"type":"RESPONSE","content":"答"}]}"#
                    .into(),
            ),
            &mut flow,
            &mut pending,
        );
        let chunk = pending.pop_front().unwrap().unwrap();
        assert_eq!(chunk.delta.reasoning.as_deref(), Some("想"));
        assert_eq!(chunk.delta.content.as_deref(), Some("答"));
    }

    #[test]
    fn noise_paths_are_skipped_and_finish_terminates() {
        let mut flow = Flow {
            id: "x".into(),
            model: "m".into(),
            saw_content: false,
            saw_finish: false,
            ended: false,
        };
        let mut pending = VecDeque::new();
        decode_event(
            SseEvent::Data(r#"{"v":"FINISHED","p":"response/fragments/-1/status"}"#.into()),
            &mut flow,
            &mut pending,
        );
        assert!(pending.is_empty(), "片段级 status 不是全文终态");
        decode_event(
            SseEvent::Data(r#"{"v":"FINISHED","p":"response/status"}"#.into()),
            &mut flow,
            &mut pending,
        );
        // 无内容就 FINISHED：报空响应错误（不记成空回答成功）
        let first = pending.pop_front().unwrap();
        assert!(first.is_err());
        assert!(flow.ended);
    }

    #[test]
    fn nested_array_frame_is_recursed() {
        let mut flow = Flow {
            id: "x".into(),
            model: "m".into(),
            saw_content: false,
            saw_finish: false,
            ended: false,
        };
        let mut pending = VecDeque::new();
        decode_event(
            SseEvent::Data(
                r#"[{"v":"片段1","p":"response/content"},{"v":"FINISHED","p":"response/status"}]"#
                    .into(),
            ),
            &mut flow,
            &mut pending,
        );
        assert_eq!(
            pending
                .pop_front()
                .unwrap()
                .unwrap()
                .delta
                .content
                .as_deref(),
            Some("片段1")
        );
        let tail = pending.pop_front().unwrap().unwrap();
        assert_eq!(tail.finish_reason, Some(FinishReason::Stop));
    }

    #[test]
    fn prompt_flattens_multi_turn_history() {
        let format = ChatFormat {
            model: "deepseek-v4-flash".into(),
            messages: vec![user("第一问"), user("第二问")],
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
        };
        let prompt = build_prompt(&format);
        assert!(prompt.contains("[用户] 第一问"));
        assert!(prompt.contains("[用户] 第二问"));
    }
}
