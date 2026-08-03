//! Cloudflare Workers AI 桥接实现
//!
//! 将 AIGX 的 CfApiClient 适配为 Bridge trait 实现，
//! 使 Cloudflare 提供商可以通过统一的 Bridge/Hub 架构进行调度。

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, EmbeddingObject, EmbeddingRequest, EmbeddingResponse,
    EmbeddingUsage, FinishReason, Role, UpstreamWire, UsageStats,
};

use crate::model::ModelMapper;
use crate::proxy::CfApiClient;

/// 将 OpenAI 消息格式转换为 CF 消息格式
fn convert_to_cf_messages(msgs: &[ChatMessage]) -> Vec<Value> {
    msgs.iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            serde_json::json!({
                "role": role,
                "content": m.content_str(),
            })
        })
        .collect()
}

/// 从 CF 响应中提取文本
fn extract_text_from_cf_response(result: &Value) -> Option<String> {
    if let Some(choices) = result.get("choices").and_then(|c| c.as_array()) {
        if let Some(first) = choices.first() {
            if let Some(content) = first
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                return Some(content.to_string());
            }
            if let Some(text) = first.get("text").and_then(|t| t.as_str()) {
                return Some(text.to_string());
            }
        }
    }
    if let Some(response) = result.get("response") {
        return match response {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
    }
    None
}

/// 估算 token 数（简化：每 4 字符算 1 token）
fn estimate_tokens(text: &str) -> u64 {
    (text.len() as f64 / 4.0).ceil() as u64
}

/// 将文本拆分为多个小块用于流式响应
fn split_text_chunks(text: &str, chunk_size: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + chunk_size * 3).min(chars.len());
        let chunk: String = chars[i..end].iter().collect();
        if !chunk.is_empty() {
            chunks.push(chunk);
        }
        i = end;
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

/// Cloudflare Workers AI Bridge
pub struct CloudflareBridge {
    client: Arc<CfApiClient>,
    model_mapper: Arc<ModelMapper>,
}

impl CloudflareBridge {
    pub fn new(
        client: Arc<CfApiClient>,
        model_mapper: Arc<ModelMapper>,
    ) -> Self {
        Self {
            client,
            model_mapper,
        }
    }

    /// 构建 CF 聊天请求体
    fn build_cf_body(&self, req: &ChatFormat) -> Value {
        let messages = convert_to_cf_messages(&req.messages);
        serde_json::json!({
            "messages": messages,
            "max_tokens": req.max_tokens.unwrap_or(2048),
            "temperature": req.temperature.unwrap_or(1.0),
            "top_p": req.top_p.unwrap_or(1.0),
        })
    }

    /// 将 CF 错误映射为 BridgeError
    fn map_cf_error(e: &crate::proxy::CfError) -> BridgeError {
        match e {
            crate::proxy::CfError::AuthError(msg) => BridgeError::AuthError(msg.clone()),
            crate::proxy::CfError::RateLimited { retry_after } => {
                if let Some(secs) = retry_after {
                    BridgeError::UpstreamStatus {
                        status: 429,
                        message: format!("rate limited, retry after {secs}s"),
                        parsed: None,
                        wire: UpstreamWire::Unknown,
                        retry_after: Some(Duration::from_secs(*secs)),
                    }
                } else {
                    BridgeError::RateLimited
                }
            }
            crate::proxy::CfError::ServerError(msg) => BridgeError::UpstreamStatus {
                status: 502,
                message: msg.clone(),
                parsed: None,
                wire: UpstreamWire::Unknown,
                retry_after: None,
            },
            crate::proxy::CfError::ModelNotFound(name) => BridgeError::ModelNotFound(name.clone()),
            crate::proxy::CfError::QuotaExceeded => BridgeError::UpstreamStatus {
                status: 429,
                message: "quota exceeded".into(),
                parsed: None,
                wire: UpstreamWire::Unknown,
                retry_after: None,
            },
            crate::proxy::CfError::AllAccountsFailed(msg) => {
                BridgeError::AllAccountsFailed(msg.clone())
            }
            crate::proxy::CfError::NetworkError(msg) => {
                if msg.contains("timeout") {
                    BridgeError::Timeout {
                        elapsed_ms: 120_000,
                        cause: msg.clone(),
                    }
                } else {
                    BridgeError::Transport(msg.clone())
                }
            }
        }
    }
}

#[async_trait]
impl Bridge for CloudflareBridge {
    fn name(&self) -> &'static str {
        "cloudflare"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let cf_model = self.model_mapper.resolve(&req.model);
        let cf_body = self.build_cf_body(req);

        let result = self
            .client
            .run_text(&cf_model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        let response_text = extract_text_from_cf_response(&result).unwrap_or_default();

        let prompt_tokens = req
            .messages
            .iter()
            .map(|m| estimate_tokens(m.content_str()))
            .sum();
        let completion_tokens = estimate_tokens(&response_text);

        Ok(ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message: ChatMessage::assistant(&response_text),
            finish_reason: FinishReason::Stop,
            usage: UsageStats::new(prompt_tokens, completion_tokens),
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let cf_model = self.model_mapper.resolve(&req.model);
        let cf_body = self.build_cf_body(req);

        let result = self
            .client
            .run_text(&cf_model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        let response_text = extract_text_from_cf_response(&result).unwrap_or_default();

        let chunks = split_text_chunks(&response_text, 5);
        let model = req.model.clone();
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());

        let stream = futures::stream::iter(chunks.into_iter().map(move |chunk| {
            Ok(ChatChunk {
                id: id.clone(),
                model: model.clone(),
                delta: ChatDelta {
                    content: Some(chunk),
                },
                finish_reason: None,
            })
        }));

        Ok(Box::pin(stream))
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        _ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        let mut embeddings = Vec::new();
        let mut total_tokens = 0u64;

        for (i, text) in req.input.iter().enumerate() {
            let cf_body = serde_json::json!({ "text": text });
            let result = self
                .client
                .run_embedding(&req.model, cf_body)
                .await
                .map_err(|e| Self::map_cf_error(&e))?;

            let data = result
                .get("data")
                .and_then(|d| d.as_array())
                .cloned()
                .unwrap_or_default();

            for (j, embedding_data) in data.iter().enumerate() {
                if let Some(vector) = embedding_data.get("embedding").and_then(|e| e.as_array()) {
                    let vec_f64: Vec<f64> =
                        vector.iter().filter_map(|v| v.as_f64()).collect();
                    embeddings.push(EmbeddingObject {
                        index: i * data.len() + j,
                        embedding: vec_f64,
                    });
                }
            }

            let tokens = estimate_tokens(text);
            total_tokens += tokens;
        }

        Ok(EmbeddingResponse {
            data: embeddings,
            usage: EmbeddingUsage {
                prompt_tokens: total_tokens,
                total_tokens,
            },
        })
    }

    async fn complete(
        &self,
        body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let prompt = body
            .get("prompt")
            .and_then(|p| p.as_str())
            .unwrap_or("");

        let cf_body = serde_json::json!({
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": body.get("max_tokens").or(Some(&Value::from(256))),
            "temperature": body.get("temperature").or(Some(&Value::from(1.0))),
        });

        let result = self
            .client
            .run_text(model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        let text = extract_text_from_cf_response(&result).unwrap_or_default();

        Ok(serde_json::json!({
            "id": format!("cmpl-{}", uuid::Uuid::new_v4()),
            "object": "text_completion",
            "created": chrono::Utc::now().timestamp(),
            "model": model,
            "choices": [
                {
                    "text": text,
                    "index": 0,
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": estimate_tokens(prompt),
                "completion_tokens": estimate_tokens(&text),
                "total_tokens": estimate_tokens(prompt) + estimate_tokens(&text),
            }
        }))
    }

    async fn generate_image(
        &self,
        body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("sdxl");
        let prompt = body
            .get("prompt")
            .and_then(|p| p.as_str())
            .ok_or_else(|| BridgeError::Config("missing prompt field".into()))?;

        let n = body
            .get("n")
            .and_then(|n| n.as_u64())
            .unwrap_or(1)
            .min(10) as usize;

        let mut images = Vec::new();

        for _ in 0..n {
            let cf_body = serde_json::json!({ "prompt": prompt });
            let result = self
                .client
                .run_image(model, cf_body)
                .await
                .map_err(|e| Self::map_cf_error(&e))?;

            let image_data = if let Some(img) = result.get("image").and_then(|i| i.as_str()) {
                serde_json::json!({ "b64_json": img })
            } else if let Some(url) = result.get("url").and_then(|u| u.as_str()) {
                serde_json::json!({ "url": url })
            } else {
                serde_json::json!({ "b64_json": result.to_string() })
            };

            images.push(image_data);
        }

        Ok(serde_json::json!({
            "created": chrono::Utc::now().timestamp(),
            "data": images
        }))
    }
}