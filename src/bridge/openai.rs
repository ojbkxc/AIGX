//! OpenAI 兼容 Bridge — 对接第三方 OpenAI 兼容上游（DeepSeek/OpenRouter/Together 等）。
//!
//! 参照 aisix provider-openai 的 wire-shape 适配 + VFaka PaymentProvider trait 模式
//! （统一 trait 抽象，每个上游一个实现）。复用 bridge::mod 中已定义的 Bridge trait。
//!
//! 该 Bridge 持有上游 base_url 与 api_key，将归一化 ChatFormat 转为 OpenAI chat.completions
//! 请求体，执行 HTTP 调用，解析响应。流式响应转为 ChatChunk 流。

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatFormat, ChatMessage,
    ChatResponse, EmbeddingRequest, EmbeddingResponse, FinishReason, Role, UpstreamWire,
    UsageStats,
};

/// OpenAI 兼容上游 Bridge。
///
/// 每个 Channel（openai_compatible 类型）对应一个 OpenaiCompatibleBridge 实例。
/// 由 ChannelStore 在调度时构造（或缓存）。
pub struct OpenaiCompatibleBridge {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenaiCompatibleBridge {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key: api_key.into(),
            client,
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// 将 ChatFormat 转为 OpenAI 请求体
    fn build_body(&self, req: &ChatFormat) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                serde_json::json!({
                    "role": role,
                    "content": m.content.clone().unwrap_or_default(),
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": req.stream,
        });
        if let Some(mt) = req.max_tokens {
            body["max_tokens"] = serde_json::json!(mt);
        }
        if let Some(t) = req.temperature {
            body["temperature"] = serde_json::json!(t);
        }
        if let Some(p) = req.top_p {
            body["top_p"] = serde_json::json!(p);
        }
        body
    }
}

#[async_trait]
impl Bridge for OpenaiCompatibleBridge {
    fn name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn chat(&self, req: &ChatFormat, _ctx: &BridgeContext) -> Result<ChatResponse, BridgeError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(false);

        let resp = self
            .client
            .post(self.chat_url())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::OpenAI,
                parse_openai_error,
            )
            .await);
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;

        let id = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("chatcmpl")
            .to_string();
        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&req.model)
            .to_string();

        let message = {
            let content = json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .unwrap_or("")
                .to_string();
            ChatMessage::assistant(content)
        };

        let finish_reason = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .and_then(|f| f.as_str())
            .map(parse_finish_reason)
            .unwrap_or(FinishReason::Stop);

        let usage = parse_usage(&json);

        Ok(ChatResponse {
            id,
            model,
            message,
            finish_reason,
            usage,
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(true);

        let resp = self
            .client
            .post(self.chat_url())
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::OpenAI,
                parse_openai_error,
            )
            .await);
        }

        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();
        let byte_stream = resp.bytes_stream();

        let chunk_stream = futures::stream::unfold(
            (byte_stream, String::new(), id, model),
            |(mut byte_stream, mut buf, id, model)| async move {
                use futures::StreamExt;
                loop {
                    // 尝试从缓冲区解析一个 SSE 事件
                    if let Some(newline_pos) = buf.find("\n\n") {
                        let event = buf[..newline_pos].to_string();
                        buf = buf[newline_pos + 2..].to_string();
                        if let Some(chunk) = parse_sse_event(&event, &id, &model) {
                            return Some((Ok(chunk), (byte_stream, buf, id, model)));
                        }
                        continue;
                    }
                    // 拉取更多数据
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            buf.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(BridgeError::Transport(e.to_string())),
                                (byte_stream, buf, id, model),
                            ));
                        }
                        None => {
                            // 流结束
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(chunk_stream))
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        _ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": req.model,
            "input": req.input,
        });
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| BridgeError::Transport(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                resp.status(),
                resp,
                UpstreamWire::OpenAI,
                parse_openai_error,
            )
            .await);
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;

        let data: Vec<_> = json
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|obj| super::EmbeddingObject {
                        index: obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        embedding: obj
                            .get("embedding")
                            .and_then(|e| e.as_array())
                            .map(|vals| {
                                vals.iter()
                                    .filter_map(|v| v.as_f64())
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let prompt_tokens = json
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let total_tokens = json
            .get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        Ok(EmbeddingResponse {
            data,
            usage: super::EmbeddingUsage {
                prompt_tokens,
                total_tokens,
            },
        })
    }
}

// ── 辅助解析函数 ────────────────────────────────────────────────────

fn parse_finish_reason(s: &str) -> FinishReason {
    match s {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

fn parse_usage(json: &Value) -> UsageStats {
    let prompt = json
        .get("usage")
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = json
        .get("usage")
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    UsageStats::new(prompt, completion)
}

fn parse_openai_error(body: &[u8]) -> Option<super::UpstreamErrorView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    Some(super::UpstreamErrorView {
        kind: err.get("type").and_then(|t| t.as_str()).map(String::from),
        message: err.get("message").and_then(|m| m.as_str()).map(String::from),
        code: err.get("code").and_then(|c| c.as_str()).map(String::from),
        param: err.get("param").and_then(|p| p.as_str()).map(String::from),
    })
}

/// 解析单个 SSE 事件为 ChatChunk
fn parse_sse_event(event: &str, id: &str, model: &str) -> Option<ChatChunk> {
    for line in event.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data.trim() == "[DONE]" {
                return Some(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: super::ChatDelta { content: None },
                    finish_reason: Some(FinishReason::Stop),
                });
            }
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                let content = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(|c| c.as_str())
                    .map(String::from);
                let finish = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|f| f.as_str())
                    .map(parse_finish_reason);
                return Some(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: super::ChatDelta { content },
                    finish_reason: finish,
                });
            }
        }
    }
    None
}

/// 构造一个 OpenAI 兼容 Bridge 的 Arc 实例（供 ChannelStore/Hub 调度使用）
pub fn make_bridge(base_url: &str, api_key: &str) -> Arc<dyn Bridge> {
    Arc::new(OpenaiCompatibleBridge::new(
        "openai_compatible",
        base_url,
        api_key,
    ))
}