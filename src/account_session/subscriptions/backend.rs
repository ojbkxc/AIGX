//! 订阅型上游的 `Bridge` 实现（chatgpt-subscription / codex-subscription）。
//!
//! 两家共用「Chat → Responses → 上游 → Responses → Chat」这条链路，差异只在
//! 基址、鉴权头与 token 端点（见 [`Flavor`]）：
//! - **chatgpt-subscription**：`api.openai.com/v1/responses`，`Authorization: Bearer`；
//! - **codex-subscription**：`chatgpt.com/backend-api/codex/responses`，
//!   `Authorization: Bearer` + `ChatGPT-Account-Id` + `originator: codex_cli_rs` + UA。
//!
//! 同时实现 `responses_passthrough`：`/v1/responses` 客户端可原样转发，
//! 不经 Chat 转换。
//!
//! 账号令牌续期在每次请求前按需触发（见 [`super::credentials`]）。

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use serde_json::Value;

use crate::account_session::model::AccountSession;
use crate::account_session::store::AccountSessionStore;
use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatFormat, ChatResponse,
    ResponsesPassthrough,
};

use super::{credentials, responses};

/// 订阅后端口味的差异点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// Sign in with ChatGPT（`api.openai.com/v1/responses`）。
    Chatgpt,
    /// Codex 官方 OAuth（`chatgpt.com/backend-api/codex/responses`）。
    Codex,
}

impl Flavor {
    fn user_agent(self) -> String {
        match self {
            Self::Chatgpt => format!("AIGX/{}", env!("CARGO_PKG_VERSION")),
            Self::Codex => format!(
                "codex_cli_rs/{} ({}; {}) AIGX/{}",
                CODEX_VERSION,
                std::env::consts::OS,
                std::env::consts::ARCH,
                env!("CARGO_PKG_VERSION")
            ),
        }
    }
}

/// Codex 参考协议版本（对齐上游 `codex_subscription/http.rs::REFERENCE_VERSION`）。
const CODEX_VERSION: &str = "0.171.0";

/// 提前多久触发续期（秒）。与上游 `MARGIN_MS = 3 分钟` 同档。
const REFRESH_MARGIN_SECS: i64 = 180;

/// 一个订阅账号的 Bridge。
pub struct SubscriptionBridge {
    flavor: Flavor,
    /// 上游基址（账号级 `base_url` 优先，否则 registry 默认）。
    base_url: String,
    /// Responses 端点路径。
    responses_path: String,
    /// 初始令牌（内存兜底；每次请求以存储里的最新值为准）。
    initial_token: String,
    /// Codex 的 `ChatGPT-Account-Id`。
    account_id: String,
    client: Arc<reqwest::Client>,
    store: AccountSessionStore,
    session_id: String,
}

impl SubscriptionBridge {
    fn url(&self) -> String {
        format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            self.responses_path
        )
    }

    /// 取当前可用 access token：临期则先续期并 CAS 落盘。
    async fn access_token(&self) -> Result<String, BridgeError> {
        let Some(session) = self.store.get(&self.session_id) else {
            return Ok(self.initial_token.clone());
        };
        let token = session.credential.secret_plain();
        let now = chrono::Utc::now().timestamp();
        let expiring = session
            .expires_at
            .is_none_or(|expires| expires <= now.saturating_add(REFRESH_MARGIN_SECS));
        if !expiring && !token.trim().is_empty() {
            return Ok(token);
        }
        match credentials::refresh(self.flavor, &self.client, &session).await {
            Ok(tokens) => {
                let mut next = session.credential.clone();
                next.secret = tokens.access.clone();
                if let Some(id_token) = tokens.id_token.as_ref() {
                    if next.fields.contains_key("idToken") {
                        next.fields.insert("idToken".to_string(), id_token.clone());
                    }
                }
                let outcome = self.store.update_tokens_cas(
                    &session.id,
                    &session.credential.secret_plain(),
                    next,
                    &tokens.refresh,
                    tokens.expires_at,
                );
                // 并发刷新（CAS 冲突）：回读最新令牌，别把别人刚写的新值覆盖掉。
                if outcome == crate::account_session::store::CasOutcome::Stale {
                    if let Some(fresh) = self.store.get(&session.id) {
                        return Ok(fresh.credential.secret_plain());
                    }
                }
                Ok(tokens.access)
            }
            // 临期但续期失败：旧令牌尚未真正过期时继续用，避免一次网络抖动打死请求。
            Err(error) if !token.trim().is_empty() && !expiring_token_is_dead(&session, now) => {
                tracing::warn!(
                    "account {} subscription token refresh failed, using existing token: {error}",
                    self.session_id
                );
                Ok(token)
            }
            Err(error) => Err(error),
        }
    }

    /// 给请求套上鉴权头（`Accept` 按流式/非流式区分）。
    fn authorize(
        &self,
        mut request: reqwest::RequestBuilder,
        token: &str,
        accept: &str,
    ) -> reqwest::RequestBuilder {
        request = request
            .header("Authorization", format!("Bearer {token}"))
            .header("Accept", accept)
            .header("User-Agent", self.flavor.user_agent());
        match self.flavor {
            Flavor::Chatgpt => request,
            Flavor::Codex => {
                request = request
                    .header("originator", "codex_cli_rs")
                    .header("version", CODEX_VERSION);
                if !self.account_id.is_empty() {
                    request = request.header("ChatGPT-Account-Id", self.account_id.as_str());
                }
                request
            }
        }
    }
}

/// 旧令牌是否已「确实过期」（刷新失败时决定还能不能用）。
fn expiring_token_is_dead(session: &AccountSession, now: i64) -> bool {
    session
        .expires_at
        .is_some_and(|expires| expires > 0 && expires <= now)
}

fn transport(error: reqwest::Error) -> BridgeError {
    BridgeError::Transport(error.to_string())
}

fn status_error(status: u16, body: &str) -> BridgeError {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(400).collect());
    BridgeError::upstream_status(status, message)
}

/// 由账号构造订阅 Bridge（基址按账号覆盖优先）。
pub fn build(
    session: &AccountSession,
    sessions: &AccountSessionStore,
    flavor: Flavor,
) -> Result<Arc<dyn Bridge>, BridgeError> {
    let spec = crate::account_session::registry::subscription_spec(&session.provider)
        .ok_or_else(|| BridgeError::Config(format!("未知订阅 provider：{}", session.provider)))?;
    let base_url = crate::account_session::adapter::effective_base_url(session)?;
    let client = sessions.client_for(session)?;
    let account_id = if flavor == Flavor::Codex {
        session.credential.field_plain("accountId")
    } else {
        String::new()
    };
    Ok(Arc::new(SubscriptionBridge {
        flavor,
        base_url,
        responses_path: spec.responses_path.to_string(),
        initial_token: session.credential.secret_plain(),
        account_id,
        client,
        store: sessions.clone(),
        session_id: session.id.clone(),
    }))
}

/// 流式解析状态：原始字节流 + 行缓冲 + 待发队列。
struct StreamFlow {
    inner: futures::stream::BoxStream<'static, Result<Bytes, reqwest::Error>>,
    buffer: String,
    sse: responses::StreamState,
    pending: VecDeque<ChatChunk>,
    done: bool,
}

/// 从缓冲里取出一个完整 SSE 块（`\n\n` 或 `\r\n\r\n` 分隔）。
fn take_sse_block(buffer: &mut String) -> Option<String> {
    let bytes = buffer.as_bytes();
    let mut split: Option<(usize, usize)> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            split = Some((i, 2));
            break;
        }
        if i + 3 < bytes.len() && &bytes[i..i + 4] == b"\r\n\r\n" {
            split = Some((i, 4));
            break;
        }
        i += 1;
    }
    let (pos, len) = split?;
    let block = buffer[..pos].to_string();
    // 用 `replace_range` 而非 `drain`：后者返回的 `Drain` 语句级忽略在
    // `-D warnings` 下可能触发 `unused_must_use`。
    buffer.replace_range(..pos + len, "");
    Some(block)
}

/// 取一个 SSE 块里的 `data:` 载荷（多行 `data:` 用 `\n` 拼接）。
fn sse_payloads(block: &str) -> Vec<String> {
    let mut payload = String::new();
    for line in block.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            if !payload.is_empty() {
                payload.push('\n');
            }
            payload.push_str(rest.trim_start());
        }
    }
    if payload.is_empty() {
        Vec::new()
    } else {
        vec![payload]
    }
}

#[async_trait]
impl Bridge for SubscriptionBridge {
    fn name(&self) -> &'static str {
        match self.flavor {
            Flavor::Chatgpt => "chatgpt_subscription",
            Flavor::Codex => "codex_subscription",
        }
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let token = self.access_token().await?;
        let body = responses::chat_to_responses(req, false);
        let request = self.authorize(
            self.client.post(self.url()).json(&body),
            &token,
            "application/json",
        );
        let response = request.send().await.map_err(transport)?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(transport)?;
        if !(200..300).contains(&status) {
            return Err(status_error(status, &text));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|_| BridgeError::UpstreamDecode("订阅上游返回了非 JSON 响应".to_string()))?;
        responses::responses_to_chat(&value, &req.model).map_err(BridgeError::UpstreamDecode)
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let token = self.access_token().await?;
        let body = responses::chat_to_responses(req, true);
        let request = self.authorize(
            self.client.post(self.url()).json(&body),
            &token,
            "text/event-stream",
        );
        let response = request.send().await.map_err(transport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.map_err(transport)?;
            return Err(status_error(status, &text));
        }
        let flow = StreamFlow {
            inner: response.bytes_stream().boxed(),
            buffer: String::new(),
            sse: responses::StreamState::default(),
            pending: VecDeque::new(),
            done: false,
        };
        let stream = futures::stream::try_unfold(flow, |mut flow| async move {
            loop {
                if let Some(chunk) = flow.pending.pop_front() {
                    return Ok(Some((chunk, flow)));
                }
                if flow.done {
                    return Ok(None);
                }
                if let Some(block) = take_sse_block(&mut flow.buffer) {
                    for payload in sse_payloads(&block) {
                        if payload.trim() == "[DONE]" {
                            flow.done = true;
                            continue;
                        }
                        if let Ok(event) = serde_json::from_str::<Value>(&payload) {
                            for chunk in responses::sse_event_to_chunks(&event, &mut flow.sse) {
                                flow.pending.push_back(chunk);
                            }
                            if flow.sse.finished {
                                flow.done = true;
                            }
                        }
                    }
                    continue;
                }
                match flow.inner.next().await {
                    Some(Ok(bytes)) => {
                        flow.buffer.push_str(&String::from_utf8_lossy(&bytes));
                    }
                    Some(Err(error)) => return Err(BridgeError::Transport(error.to_string())),
                    None => return Ok(None),
                }
            }
        });
        Ok(stream.boxed())
    }

    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        _ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        let token = self.access_token().await?;
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        let request = self.authorize(self.client.post(self.url()).json(body), &token, accept);
        let response = request.send().await.map_err(transport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let text = response.text().await.map_err(transport)?;
            return Err(status_error(status, &text));
        }
        if stream {
            let stream = response
                .bytes_stream()
                .map(|item| item.map_err(|error| BridgeError::Transport(error.to_string())))
                .boxed();
            Ok(ResponsesPassthrough::Stream(stream))
        } else {
            let value: Value = response.json().await.map_err(transport)?;
            Ok(ResponsesPassthrough::Json(value))
        }
    }
}
