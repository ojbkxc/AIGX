//! OpenAI 兼容 Bridge — 对接第三方 OpenAI 兼容上游（DeepSeek/OpenRouter/Together 等）。
//!
//! 参照 aisix provider-openai 的 wire-shape 适配 + VFaka PaymentProvider trait 模式
//! （统一 trait 抽象，每个上游一个实现）。复用 bridge::mod 中已定义的 Bridge trait。
//!
//! 该 Bridge 持有上游 base_url 与 api_key，将归一化 ChatFormat 转为 OpenAI chat.completions
//! 请求体，执行 HTTP 调用，解析响应。流式响应转为 ChatChunk 流。

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

use super::{
    Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream, ChatFormat, ChatMessage,
    ChatResponse, EmbeddingRequest, EmbeddingResponse, FinishReason, ResponsesPassthrough, Role,
    UpstreamWire, UsageStats,
};

/// OpenAI 兼容上游 Bridge。
///
/// 每个 Channel（openai_compatible 类型）对应一个 OpenaiCompatibleBridge 实例。
/// 由 ChannelStore 在调度时构造（或缓存）。
///
/// 性能：`client` 由调用方共享传入（AppState.http_client），避免每次请求新建
/// reqwest::Client（含连接池/TLS 握手开销）。reqwest::Client 内部已基于 Arc，
/// clone 廉价。
pub struct OpenaiCompatibleBridge {
    name: String,
    base_url: String,
    api_key: String,
    client: reqwest::Client,
}

impl OpenaiCompatibleBridge {
    /// 构造 Bridge，复用外部传入的 `reqwest::Client`（推荐从 AppState.http_client 取）。
    pub fn with_client(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            name: name.into(),
            // 归一化 base_url：去掉末尾斜杠；若不带路径（host 直连，如
            // `https://cf-ai-gw.pages.dev`）则自动补 `/v1`，与 OpenAI SDK 约定一致，
            // 避免构造出 `https://host/chat/completions`（上游 404）。
            base_url: normalize_base_url(base_url.into()),
            api_key: api_key.into(),
            client,
        }
    }

    /// 兼容旧调用：内部新建 Client。不推荐，优先用 `with_client`。
    #[deprecated(note = "use with_client to share a reqwest::Client from AppState")]
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .expect("reqwest client");
        Self::with_client(name, base_url, api_key, client)
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// Responses API 端点。与 chat_url 同规则：渠道 base_url 习惯上已含
    /// `/v1` 前缀（chat 拼接 `/chat/completions`），此处对称拼接
    /// `/responses`，避免对已含 `/v1` 的 base_url 重复拼出
    /// `/v1/v1/responses`（SDK 双前缀问题，参考 aisix build_v1_url）。
    fn responses_url(&self) -> String {
        format!("{}/responses", self.base_url.trim_end_matches('/'))
    }

    fn completions_url(&self) -> String {
        format!("{}/completions", self.base_url.trim_end_matches('/'))
    }

    fn images_url(&self) -> String {
        format!("{}/images/generations", self.base_url.trim_end_matches('/'))
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
                let mut msg = serde_json::json!({
                    "role": role,
                    "content": m.content.clone().unwrap_or_default(),
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
            .collect();

        let mut body = serde_json::json!({
            "model": req.model,
            "messages": messages,
            "stream": req.stream,
        });
        if let Some(tools) = &req.tools {
            body["tools"] = serde_json::json!(tools);
        }
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
            let tool_calls = json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("tool_calls"))
                .and_then(|tc| tc.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| {
                            Some(super::ToolCall {
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
                        })
                        .collect()
                })
                .map(|calls: Vec<super::ToolCall>| {
                    // 对上游返回的参数做健壮化，保证下游拿到合法 JSON
                    calls
                        .into_iter()
                        .map(|mut tc| {
                            tc.arguments = super::tool_repair::repair_tool_arguments(&tc.arguments);
                            tc
                        })
                        .collect()
                });
            let reasoning = json
                .get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("message"))
                .and_then(|m| m.get("reasoning_content"))
                .or_else(|| {
                    json.get("choices")
                        .and_then(|c| c.get(0))
                        .and_then(|c| c.get("message"))
                        .and_then(|m| m.get("reasoning"))
                })
                .and_then(|r| r.as_str())
                .map(String::from);
            let mut msg = ChatMessage::assistant(content);
            msg.reasoning = reasoning;
            msg.tool_calls = tool_calls;
            msg
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

    /// 文本补全：透传 /completions 请求体，返回 OpenAI 格式结果。
    ///
    /// 与 Chat Completions 不同，text_completion 使用顶层 `prompt` 字段。
    /// body 原样转发给上游的 /completions 端点，上游 2xx 时返回完整 JSON。
    async fn complete(
        &self,
        body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let resp = self
            .client
            .post(self.completions_url())
            .bearer_auth(&self.api_key)
            .json(body)
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
        Ok(json)
    }

    /// 图片生成：透传 /images/generations 请求体，返回 OpenAI 格式结果。
    async fn generate_image(
        &self,
        body: &Value,
        _ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let resp = self
            .client
            .post(self.images_url())
            .bearer_auth(&self.api_key)
            .json(body)
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
        Ok(json)
    }

    /// Responses API 透传：body 原样转发给上游的 /responses 端点。
    ///
    /// body（含 model/input/stream 等全部字段）不重写、不增删，
    /// 上游 2xx 时非流式返回完整 JSON、流式返回 SSE 原始字节流；
    /// 非流式默认共享 Client 的 120s 请求超时（与 chat 一致）。
    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        _ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        let resp = self
            .client
            .post(self.responses_url())
            .bearer_auth(&self.api_key)
            .json(body)
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

        if stream {
            // SSE 原始字节流原样透传（保留 text/event-stream wire 格式），
            // 调用方旁路解析提取 usage 计费
            let byte_stream = resp.bytes_stream().map(|chunk| {
                chunk.map_err(|e| BridgeError::Transport(e.to_string()))
            });
            Ok(ResponsesPassthrough::Stream(Box::pin(byte_stream)))
        } else {
            let json: Value = resp
                .json()
                .await
                .map_err(|e| BridgeError::UpstreamDecode(e.to_string()))?;
            Ok(ResponsesPassthrough::Json(json))
        }
    }
}

// ── 辅助解析函数 ────────────────────────────────────────────────────

/// 归一化 OpenAI 兼容上游 base_url：
/// - 去除末尾 `/`
/// - 若 URL 路径为空或仅为 `/`（host 直连，未带 `/v1`），自动补 `/v1`
///
/// 覆盖 cf-ai-gw（`https://cf-ai-gw.pages.dev`）这类“host 即 OpenAI 兼容根”
/// 的上游：其真实端点需 `/v1` 前缀（`/v1/chat/completions`）。
pub(crate) fn normalize_base_url(base_url: String) -> String {
    let trimmed = base_url.trim_end_matches('/').to_string();
    // 判断 host 之后是否已有路径段（如 /v1、/foo）。scheme 后的首段斜杠用于
    // 分隔 host 与路径；若无额外路径 → 补 /v1，与 OpenAI SDK 约定一致。
    let has_path_segment = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .map(|rest| rest.contains('/'))
        .unwrap_or(false);
    if has_path_segment {
        trimmed
    } else {
        format!("{trimmed}/v1")
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_base_url;

    #[test]
    fn normalizes_host_only_base_url() {
        assert_eq!(
            normalize_base_url("https://cf-ai-gw.pages.dev".into()),
            "https://cf-ai-gw.pages.dev/v1"
        );
        assert_eq!(
            normalize_base_url("https://cf-ai-gw.pages.dev/".into()),
            "https://cf-ai-gw.pages.dev/v1"
        );
    }

    #[test]
    fn keeps_existing_path_segments() {
        assert_eq!(
            normalize_base_url("https://api.deepseek.com/v1".into()),
            "https://api.deepseek.com/v1"
        );
        assert_eq!(
            normalize_base_url("https://openrouter.ai/api/v1".into()),
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            normalize_base_url("https://api.deepseek.com".into()),
            "https://api.deepseek.com/v1"
        );
    }
}

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
        // B17：兼容 "data:" 后无空格的 SSE 实现（规范允许仅一个冒号，
        // 部分网关/代理会剥掉空格），原先只匹配 "data: " 会导致整帧被忽略
        if let Some(data) = line.strip_prefix("data:").map(str::trim_start) {
            if data.trim() == "[DONE]" {
                return Some(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: super::ChatDelta::default(),
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
                let tool_calls = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("tool_calls"))
                    .and_then(|tc| tc.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| {
                                Some(super::ToolCallDelta {
                                    index: t
                                        .get("index")
                                        .and_then(|i| i.as_u64())
                                        .unwrap_or(0) as usize,
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
                            })
                            .collect()
                    });
                // 流式推理增量：DeepSeek 系用 reasoning_content，OpenAI o 系列用 reasoning
                let reasoning = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| {
                        d.get("reasoning_content")
                            .or_else(|| d.get("reasoning"))
                    })
                    .and_then(|r| r.as_str())
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
                    delta: super::ChatDelta { content, tool_calls, reasoning },
                    finish_reason: finish,
                });
            }
        }
    }
    None
}

/// 构造一个 OpenAI 兼容 Bridge 的 Arc 实例（供 ChannelStore/Hub 调度使用）。
///
/// 性能：复用外部传入的 `client`（应来自 AppState.http_client），避免每次请求
/// 新建 reqwest::Client。`client.clone()` 廉价（内部 Arc）。
pub fn make_bridge(base_url: &str, api_key: &str, client: &reqwest::Client) -> Arc<dyn Bridge> {
    Arc::new(OpenaiCompatibleBridge::with_client(
        "openai_compatible",
        base_url,
        api_key,
        client.clone(),
    ))
}