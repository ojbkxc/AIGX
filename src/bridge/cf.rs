//! Cloudflare Workers AI 桥接实现
//!
//! 将 AIGX 的 CfApiClient 适配为 Bridge trait 实现，
//! 使 Cloudflare 提供商可以通过统一的 Bridge/Hub 架构进行调度。
//!
//! ## 架构说明：AI Binding 方式
//!
//! 本模块通过 HTTP 调用 cf-ai-gw Worker（Cloudflare Workers 上部署的网关），
//! cf-ai-gw 内部使用 **AI Binding**（`env.AI.run(model, input)`）直接调用
//! Cloudflare Workers AI 模型，而非通过 REST API 调用 `api.cloudflare.com`。
//!
//! 架构链路：
//! ```text
//! AIGX (Rust) --HTTP--> cf-ai-gw Worker --AI Binding--> Cloudflare Workers AI
//! ```
//!
//! 优势：
//! - AI Binding 在 Worker 内部零延迟调用，无需额外 API Token
//! - 自动享受 Cloudflare 免费额度（@cf/ 开头的模型）
//! - 多账号负载均衡在 AIGX 层面实现，cf-ai-gw Worker 保持无状态

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatDelta, ChatFormat,
    ChatMessage, ChatResponse, EmbeddingObject, EmbeddingRequest, EmbeddingResponse,
    EmbeddingUsage, FinishReason, ResponsesPassthrough, Role, UpstreamWire, UsageStats,
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
            let mut msg = serde_json::json!({
                "role": role,
                "content": m.content_str(),
            });
            if let Some(tid) = &m.tool_call_id {
                msg["tool_call_id"] = Value::String(tid.clone());
            }
            if let Some(tool_calls) = &m.tool_calls {
                msg["tool_calls"] = Value::Array(
                    tool_calls
                        .iter()
                        .map(|tc| {
                            serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.function_name,
                                    "arguments": tc.arguments,
                                }
                            })
                        })
                        .collect(),
                );
            }
            msg
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

/// 解析 OpenAI 风格 SSE chunk（CF stream:true 输出格式），转为内部 ChatChunk
fn parse_openai_chunk(
    json_str: &str,
    id: &str,
    model: &str,
) -> Option<std::result::Result<ChatChunk, BridgeError>> {
    let v: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            return Some(Err(BridgeError::UpstreamDecode(format!(
                "failed to parse SSE chunk: {e}"
            ))));
        }
    };

    let delta_content = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());

    // 流式推理内容增量（OpenAI delta.reasoning_content，Anthropic thinking）
    let delta_reasoning = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("delta"))
        .and_then(|d| d.get("reasoning_content"))
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());

    let tool_calls = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("delta"))
        .and_then(|d| d.get("tool_calls"))
        .and_then(|tc| tc.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| super::ToolCallDelta {
                    index: t.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize,
                    id: t.get("id").and_then(|i| i.as_str()).map(String::from),
                    function_name: t
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from),
                    arguments: t
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|a| a.as_str())
                        .map(String::from),
                })
                .collect()
        });

    let finish_reason = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|first| first.get("finish_reason"))
        .and_then(|f| f.as_str())
        .map(|s| match s {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "content_filter" => FinishReason::ContentFilter,
            "tool_calls" => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        });

    if delta_content.is_none()
        && delta_reasoning.is_none()
        && tool_calls.is_none()
        && finish_reason.is_none()
    {
        return None;
    }

    Some(Ok(ChatChunk {
        id: id.to_string(),
        model: model.to_string(),
        delta: ChatDelta {
            content: delta_content,
            tool_calls,
            reasoning: delta_reasoning,
        },
        finish_reason,
        usage: None,
    }))
}

/// Cloudflare Workers AI Bridge
pub struct CloudflareBridge {
    client: Arc<CfApiClient>,
    model_mapper: Arc<ModelMapper>,
}

impl CloudflareBridge {
    pub fn new(client: Arc<CfApiClient>, model_mapper: Arc<ModelMapper>) -> Self {
        Self {
            client,
            model_mapper,
        }
    }

    /// 构建 CF 聊天请求体
    fn build_cf_body(&self, req: &ChatFormat) -> Value {
        let messages = convert_to_cf_messages(&req.messages);
        let mut body = serde_json::json!({
            "messages": messages,
            "max_tokens": req.max_tokens.unwrap_or(2048),
            "temperature": req.temperature.unwrap_or(1.0),
            "top_p": req.top_p.unwrap_or(1.0),
        });
        if let Some(tools) = &req.tools {
            body["tools"] = serde_json::json!(tools);
        }
        body
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

        // 提取 CF 非流式响应中的工具调用（OpenAI 兼容结构）
        let tool_calls = result
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|t| super::ToolCall {
                        id: t
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string(),
                        function_name: t
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string(),
                        arguments: t
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
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

        let prompt_tokens = req
            .messages
            .iter()
            .map(|m| estimate_tokens(m.content_str()))
            .sum();
        let completion_tokens = estimate_tokens(&response_text);

        let mut msg = ChatMessage::assistant(&response_text);
        msg.tool_calls = tool_calls;

        let finish_reason = result
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|first| first.get("finish_reason"))
            .and_then(|f| f.as_str())
            .map(|s| match s {
                "stop" => FinishReason::Stop,
                "length" => FinishReason::Length,
                "content_filter" => FinishReason::ContentFilter,
                "tool_calls" => FinishReason::ToolCalls,
                _ => FinishReason::Stop,
            })
            .unwrap_or(FinishReason::Stop);

        Ok(ChatResponse {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model: req.model.clone(),
            message: msg,
            finish_reason,
            usage: UsageStats::new(prompt_tokens, completion_tokens),
        })
    }

    async fn chat_stream(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let cf_model = self.model_mapper.resolve(&req.model);
        let mut cf_body = self.build_cf_body(req);
        // CF Workers AI: 流式输出需显式带 stream:true，返回 OpenAI 风格 SSE
        cf_body["stream"] = Value::Bool(true);

        let byte_stream = self
            .client
            .run_text_stream(&cf_model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
        let model = req.model.clone();

        let mut decoder = crate::sse::SseDecoder::new();
        let id_clone = id.clone();
        let model_clone = model.clone();

        let stream = byte_stream.flat_map(
            move |res: std::result::Result<bytes::Bytes, crate::proxy::CfError>| {
                let id = id_clone.clone();
                let model = model_clone.clone();
                let iter: Vec<std::result::Result<ChatChunk, BridgeError>> = match res {
                    Ok(bytes) => {
                        let events = decoder.feed(bytes.as_ref());
                        events
                            .into_iter()
                            .filter_map(|ev| match ev {
                                crate::sse::SseEvent::Data(json_str) => {
                                    parse_openai_chunk(&json_str, &id, &model)
                                }
                                crate::sse::SseEvent::Done => Some(Ok(ChatChunk {
                                    id: id.clone(),
                                    model: model.clone(),
                                    delta: ChatDelta::default(),
                                    finish_reason: Some(FinishReason::Stop),
                                    usage: None,
                                })),
                            })
                            .collect()
                    }
                    Err(e) => vec![Err(Self::map_cf_error(&e))],
                };
                futures::stream::iter(iter)
            },
        );

        Ok(Box::pin(stream))
    }

    async fn embed(
        &self,
        req: &EmbeddingRequest,
        _ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        let mut embeddings = Vec::new();

        // AI Binding 方式：cf-ai-gw 的 /v1/embeddings 一次接收 input 数组
        let cf_body = serde_json::json!({ "input": req.input });
        let result = self
            .client
            .run_embedding(&req.model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        // cf-ai-gw 返回 OpenAI 格式 { object:"list", data:[{embedding,index}] }
        let data = result
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        for embedding_data in &data {
            if let Some(vector) = embedding_data.get("embedding").and_then(|e| e.as_array()) {
                let vec_f64: Vec<f64> = vector.iter().filter_map(|v| v.as_f64()).collect();
                embeddings.push(EmbeddingObject {
                    index: embedding_data
                        .get("index")
                        .and_then(|i| i.as_u64())
                        .unwrap_or(0) as usize,
                    embedding: vec_f64,
                });
            }
        }

        // 用量从响应 usage 提取，否则估算
        let total_tokens = if let Some(usage) = result.get("usage") {
            usage
                .get("total_tokens")
                .or_else(|| usage.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or_else(|| req.input.iter().map(|t| estimate_tokens(t)).sum())
        } else {
            req.input.iter().map(|t| estimate_tokens(t)).sum()
        };

        Ok(EmbeddingResponse {
            data: embeddings,
            usage: EmbeddingUsage {
                prompt_tokens: total_tokens,
                total_tokens,
            },
        })
    }

    async fn complete(&self, body: &Value, _ctx: &BridgeContext) -> Result<Value, BridgeError> {
        let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("");
        let prompt = body.get("prompt").and_then(|p| p.as_str()).unwrap_or("");

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
        let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("sdxl");
        let prompt = body
            .get("prompt")
            .and_then(|p| p.as_str())
            .ok_or_else(|| BridgeError::Config("missing prompt field".into()))?;

        // AI Binding 方式：cf-ai-gw 的 /v1/images/generations 返回
        // OpenAI 格式 { data: [ { b64_json | url } ] }（内部经 AI Binding 生成）
        let cf_body = serde_json::json!({ "prompt": prompt });
        let result = self
            .client
            .run_image(model, cf_body)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        // cf-ai-gw 已按 OpenAI 兼容格式返回 data 数组
        if let Some(data) = result.get("data").and_then(|d| d.as_array()) {
            return Ok(serde_json::json!({
                "created": chrono::Utc::now().timestamp(),
                "data": data
            }));
        }

        // 兜底：单张图片结果
        let image_data = if let Some(img) = result.get("image").and_then(|i| i.as_str()) {
            serde_json::json!({ "b64_json": img })
        } else if let Some(url) = result.get("url").and_then(|u| u.as_str()) {
            serde_json::json!({ "url": url })
        } else {
            serde_json::json!({ "b64_json": result.to_string() })
        };

        Ok(serde_json::json!({
            "created": chrono::Utc::now().timestamp(),
            "data": [image_data]
        }))
    }

    /// Responses API 透传：body 原样转发给 cf-ai-gw Worker 的 /v1/responses
    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        _ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        // body 含 model 字段，透传时原样保留；映射仅用于错误信息展示
        let model = body.get("model").and_then(|m| m.as_str()).unwrap_or("");
        let cf_model = self.model_mapper.resolve(model);

        let outcome = self
            .client
            .run_responses_passthrough(&cf_model, body.clone(), stream)
            .await
            .map_err(|e| Self::map_cf_error(&e))?;

        Ok(match outcome {
            crate::proxy::CfResponsesOutcome::Json(v) => ResponsesPassthrough::Json(v),
            crate::proxy::CfResponsesOutcome::Stream(s) => {
                // 字节流中的 CfError 惰性映射为 BridgeError（复用统一映射）
                ResponsesPassthrough::Stream(Box::pin(
                    s.map(|res| res.map_err(|e| Self::map_cf_error(&e))),
                ))
            }
        })
    }
}
