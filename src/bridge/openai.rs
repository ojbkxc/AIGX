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

use super::{
    apply_conditional_bearer, Bridge, BridgeContext, BridgeError, ChatChunk, ChatChunkStream,
    ChatFormat, ChatMessage, ChatResponse, EmbeddingRequest, EmbeddingResponse, FinishReason,
    RerankRequest, RerankResponse, RerankResult, ResponsesPassthrough, Role, UpstreamWire,
    UsageStats,
};

/// 上游鉴权方式。
///
/// 大部分供应商是 `Bearer`（API Key 或 JWT 同形）；登录态型供应商
/// （Cline / Qoder / Trae 等）用 Cookie 或「专用请求头 + 凭证模板」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// `Authorization: Bearer {credential}`（默认）
    Bearer,
    /// 凭证原样作为 `Cookie` 头发送（登录态抓包得到的整段 cookie）
    Cookie,
    /// 只用自定义请求头（凭证已由模板注入到具体头里）
    Headers,
}

/// OpenAI 兼容上游 Bridge。
///
/// 每个 Channel（openai_compatible 类型）对应一个 OpenaiCompatibleBridge 实例。
/// 由 ChannelStore 在调度时构造（或缓存）。
///
/// 性能：`client` 由调用方共享传入（AppState.http_client），避免每次请求新建
/// reqwest::Client（含连接池/TLS 握手开销）。reqwest::Client 内部已基于 Arc，
/// clone 廉价。
pub struct OpenaiCompatibleBridge {
    base_url: String,
    api_key: String,
    client: reqwest::Client,
    auth_mode: AuthMode,
    /// 登录态型供应商要求的专用请求头（已做凭证模板替换）
    session_headers: Vec<(String, String)>,
    /// 对话路径覆盖（登录态型供应商的私有接口路径）；空 = `/chat/completions`
    chat_path: Option<String>,
    /// 渠道级上游特判（预置供应商写入；缺省 = 无特判）
    quirks: crate::channel::ChannelQuirks,
}

impl OpenaiCompatibleBridge {
    /// 构造 Bridge，复用外部传入的 `reqwest::Client`（推荐从 AppState.http_client 取）。
    pub fn with_client(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            // 归一化 base_url：去掉末尾斜杠；若不带路径（host 直连，如
            // `https://cf-ai-gw.pages.dev`）则自动补 `/v1`，与 OpenAI SDK 约定一致，
            // 避免构造出 `https://host/chat/completions`（上游 404）。
            base_url: normalize_base_url(base_url.into()),
            api_key: api_key.into(),
            client,
            auth_mode: AuthMode::Bearer,
            session_headers: Vec::new(),
            chat_path: None,
            quirks: crate::channel::ChannelQuirks::default(),
        }
    }

    /// 挂上渠道级特判（urlSuffix / 静态头 / OpenCode 客户端伪装）。
    pub fn with_quirks(mut self, quirks: crate::channel::ChannelQuirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// 构造「登录态型供应商」Bridge：Cookie / 专用请求头鉴权。
    ///
    /// `credential` 为用户粘贴的 Cookie 或 JWT；`headers` 中的 `{credential}`
    /// 占位符会被替换成凭证明文（账号级额外头已在调用方合并进来）。
    pub fn with_session_auth(
        base_url: impl Into<String>,
        credential: impl Into<String>,
        client: reqwest::Client,
        auth_mode: AuthMode,
        headers: Vec<(String, String)>,
        chat_path: Option<String>,
    ) -> Self {
        let credential = credential.into();
        let params: Vec<(String, String)> = headers
            .into_iter()
            .map(|(name, value)| (name, value.replace("{credential}", &credential)))
            .collect();
        Self {
            base_url: normalize_base_url(base_url.into()),
            api_key: credential,
            client,
            auth_mode,
            session_headers: params,
            chat_path: chat_path
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty()),
            quirks: crate::channel::ChannelQuirks::default(),
        }
    }

    fn chat_url(&self) -> String {
        let url = if let Some(path) = &self.chat_path {
            // 绝对路径覆盖：`/api/chat` → `<base>/api/chat`
            format!(
                "{}/{}",
                self.base_url.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        } else {
            format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
        };
        self.quirks.append_suffix(&url)
    }

    /// Responses API 端点。与 chat_url 同规则：渠道 base_url 习惯上已含
    /// `/v1` 前缀（chat 拼接 `/chat/completions`），此处对称拼接
    /// `/responses`，避免对已含 `/v1` 的 base_url 重复拼出
    /// `/v1/v1/responses`（SDK 双前缀问题，参考 aisix build_v1_url）。
    fn responses_url(&self) -> String {
        self.quirks.append_suffix(&format!(
            "{}/responses",
            self.base_url.trim_end_matches('/')
        ))
    }

    fn completions_url(&self) -> String {
        self.quirks.append_suffix(&format!(
            "{}/completions",
            self.base_url.trim_end_matches('/')
        ))
    }

    fn images_url(&self) -> String {
        self.quirks.append_suffix(&format!(
            "{}/images/generations",
            self.base_url.trim_end_matches('/')
        ))
    }

    /// 按鉴权方式附加请求头：
    /// - `Bearer`：空密钥不发送 Authorization（见 `apply_conditional_bearer`）
    /// - `Cookie`：凭证原样放进 `Cookie` 头
    /// - `Headers`：只用登录态供应商要求的专用头
    ///
    /// 之后再叠加渠道级静态头（`quirks.headers`，同名覆盖），OpenCode 伪装
    /// 在 `apply_emulation` 里单独处理（它需要请求体参与）。
    fn auth_conditional(&self, mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        for (name, value) in &self.session_headers {
            req = req.header(name.as_str(), value.as_str());
        }
        let req = match self.auth_mode {
            AuthMode::Bearer => apply_conditional_bearer(req, &self.api_key),
            AuthMode::Cookie => {
                if self.api_key.is_empty() {
                    req
                } else {
                    req.header("cookie", &self.api_key)
                }
            }
            AuthMode::Headers => req,
        };
        self.quirks.apply_headers(req)
    }

    /// OpenCode 客户端伪装：按官方 CLI 形状补齐出站头与请求体。
    ///
    /// 只在 `quirks.client_emulation = "opencode"` 且**非 Go 面**时生效
    /// （Go 面按端点判，只补会话头 —— 见 `channel::emulation`）。
    /// 返回补齐后的请求体（非伪装路径原样返回）。
    fn apply_emulation(&self, mut body: Value) -> (reqwest::RequestBuilder, Value) {
        if !self.quirks.emulates_opencode() {
            return (
                self.auth_conditional(self.client.post(self.chat_url())),
                body,
            );
        }
        let go_face = crate::channel::emulation::is_opencode_go_endpoint(&self.base_url);
        let session = crate::channel::emulation::session_id(
            &crate::channel::emulation::conversation_seed(&body),
        );
        let mut headers: Vec<(String, String)> = Vec::new();
        if go_face {
            crate::channel::emulation::apply_go_headers(&mut headers, &session);
        } else {
            crate::channel::emulation::apply_headers(&mut headers, &self.api_key, &session);
            crate::channel::emulation::ensure_agent_shape(&mut body);
        }
        let mut req = self.client.post(self.chat_url());
        for (name, value) in &headers {
            req = req.header(name.as_str(), value.as_str());
        }
        req = self.quirks.apply_headers(req);
        (req, body)
    }

    /// 发流式请求并在本地聚合成一个非流式响应。
    ///
    /// 只为 OpenCode 免费档：上游拒绝非流式请求（`stream: true` 是过检条件
    /// 之一），而下游调用的是 `chat()`。聚合口径与下游 SSE 聚合器一致：
    /// 文本 / 推理 / 工具调用按 index 拼接，usage 取终帧。
    async fn chat_via_stream_aggregation(
        &self,
        req: &ChatFormat,
    ) -> Result<ChatResponse, BridgeError> {
        let mut stream = self
            .chat_stream(req, &BridgeContext::new("aggregate", &req.model))
            .await?;
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut calls: std::collections::BTreeMap<usize, super::ToolCall> =
            std::collections::BTreeMap::new();
        let mut usage = super::UsageStats::default();
        let mut finish = super::FinishReason::Stop;
        let mut id = String::new();
        let mut model = req.model.clone();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if !chunk.id.is_empty() {
                id = chunk.id.clone();
            }
            if !chunk.model.is_empty() {
                model = chunk.model.clone();
            }
            if let Some(text) = chunk.delta.content {
                content.push_str(&text);
            }
            if let Some(text) = chunk.delta.reasoning {
                reasoning.push_str(&text);
            }
            if let Some(deltas) = chunk.delta.tool_calls {
                for delta in deltas {
                    let entry = calls.entry(delta.index).or_default();
                    if let Some(call_id) = delta.id {
                        entry.id = call_id;
                    }
                    if let Some(name) = delta.function_name {
                        entry.function_name = name;
                    }
                    if let Some(args) = delta.arguments {
                        entry.arguments.push_str(&args);
                    }
                }
            }
            if let Some(reason) = chunk.finish_reason {
                finish = reason;
            }
            if let Some(stats) = chunk.usage {
                usage = stats;
            }
        }
        let tool_calls: Vec<super::ToolCall> = calls
            .into_values()
            .map(|mut call| {
                call.arguments = super::tool_repair::repair_tool_arguments(&call.arguments);
                call
            })
            .collect();
        let mut message = ChatMessage {
            role: Role::Assistant,
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
        };
        // 空 content 兜底：下游 OpenAI 客户端对 `content: null` 且无工具的
        // 响应会报错，与 SSE 聚合器的处置保持一致
        if message.content.is_none() && message.tool_calls.is_none() {
            message.content = Some(String::new());
        }
        Ok(ChatResponse {
            id: if id.is_empty() {
                format!("chatcmpl-{}", uuid::Uuid::new_v4())
            } else {
                id
            },
            model,
            message,
            finish_reason: finish,
            usage,
        })
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
                // 优先转发 content_blocks（vision/多模态 typed-block 数组），
                // 否则用 content（含 None → null，保留 OpenAI assistant
                // with tool_calls 的 content:null 历史回放形状）。
                let mut msg = match &m.content_blocks {
                    Some(blocks) => serde_json::json!({
                        "role": role,
                        "content": Value::Array(blocks.clone()),
                    }),
                    None => serde_json::json!({
                        "role": role,
                        "content": m.content.clone(),
                    }),
                };
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
        if let Some(tool_choice) = &req.tool_choice {
            body["tool_choice"] = serde_json::json!(tool_choice);
        }
        if let Some(effort) = &req.reasoning_effort {
            body["reasoning_effort"] = serde_json::json!(effort);
        }
        if let Some(web) = &req.web_search_options {
            body["web_search_options"] = serde_json::json!(web);
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
        if let Some(stop) = &req.stop {
            body["stop"] = serde_json::json!(stop);
        }
        if let Some(extra) = &req.extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    body[k] = v.clone();
                }
            }
        }
        body
    }
}

#[async_trait]
impl Bridge for OpenaiCompatibleBridge {
    fn name(&self) -> &'static str {
        "openai_compatible"
    }

    async fn chat(
        &self,
        req: &ChatFormat,
        _ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(false);

        // OpenCode 免费档只接受流式请求（见 channel::emulation）：此时改发
        // 流式再在本地聚合成一个响应，对下游仍是「非流式」语义。
        if self.quirks.emulates_opencode()
            && !crate::channel::emulation::is_opencode_go_endpoint(&self.base_url)
        {
            return self.chat_via_stream_aggregation(req).await;
        }

        let (request, body) = self.apply_emulation(body);
        let resp = request
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

        let (request, body) = self.apply_emulation(body);
        let resp = request
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
            (byte_stream, Vec::<u8>::new(), id, model),
            |(mut byte_stream, mut buf, id, model)| async move {
                use futures::StreamExt;
                loop {
                    // 以字节方式缓冲，多字节 UTF-8 字符跨 chunk 边界不会被截断损坏。
                    // 事件分隔符兼容 \n\n / \r\n\r\n / \r\r（部分上游输出 CRLF 行尾）
                    if let Some(end) = crate::sse::find_event_end_bytes(&buf) {
                        let event = String::from_utf8_lossy(&buf[..end]).to_string();
                        buf.drain(..end);
                        if let Some(chunk) = parse_sse_event(&event, &id, &model) {
                            return Some((Ok(chunk), (byte_stream, buf, id, model)));
                        }
                        continue;
                    }
                    // 拉取更多数据
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            buf.extend_from_slice(&bytes);
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(BridgeError::Transport(e.to_string())),
                                (byte_stream, buf, id, model),
                            ));
                        }
                        None => {
                            // 流结束：缓冲区里可能还有**最后一帧没有尾随空行**
                            // （部分上游省略最后的 `\n\n`）。直接丢弃会丢掉终帧
                            // 的 usage / finish_reason，因此这里补解析一次。
                            if !buf.is_empty() {
                                let tail = String::from_utf8_lossy(&buf).to_string();
                                buf.clear();
                                if let Some(chunk) = parse_sse_event(&tail, &id, &model) {
                                    return Some((Ok(chunk), (byte_stream, buf, id, model)));
                                }
                            }
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
            .auth_conditional(self.client.post(&url))
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
                            .map(|vals| vals.iter().filter_map(|v| v.as_f64()).collect())
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

    /// 重排序：POST {base}/rerank，兼容 Cohere/Jina 风格的请求与响应形状。
    ///
    /// 请求体：{model, query, documents: [{content}...], top_n?}；
    /// 响应体：{results: [{index, relevance_score}...], usage?}——
    /// 宽容解析：缺失字段一律默认（index=0 / score=0 / usage=0），
    /// 兼容不同上游把 document 写成纯字符串或 score 字段名不同的变体。
    async fn rerank(
        &self,
        req: &RerankRequest,
        _ctx: &BridgeContext,
    ) -> Result<RerankResponse, BridgeError> {
        let url = format!("{}/rerank", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": req.model,
            "query": req.query,
            "documents": req.documents.iter()
                .map(|d| serde_json::json!({ "content": d.content }))
                .collect::<Vec<_>>(),
            "top_n": req.top_n,
        });
        let resp = self
            .auth_conditional(self.client.post(&url))
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

        let results: Vec<RerankResult> = json
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|obj| RerankResult {
                        index: obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        relevance_score: obj
                            .get("relevance_score")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let usage = super::EmbeddingUsage {
            prompt_tokens: json
                .get("usage")
                .and_then(|u| u.get("prompt_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            total_tokens: json
                .get("usage")
                .and_then(|u| u.get("total_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
        };

        Ok(RerankResponse { results, usage })
    }

    /// 文本补全：透传 /completions 请求体，返回 OpenAI 格式结果。
    ///
    /// 与 Chat Completions 不同，text_completion 使用顶层 `prompt` 字段。
    /// body 原样转发给上游的 /completions 端点，上游 2xx 时返回完整 JSON。
    async fn complete(&self, body: &Value, _ctx: &BridgeContext) -> Result<Value, BridgeError> {
        let resp = self
            .auth_conditional(self.client.post(self.completions_url()))
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
            .auth_conditional(self.client.post(self.images_url()))
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
            .auth_conditional(self.client.post(self.responses_url()))
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
            let byte_stream = resp
                .bytes_stream()
                .map(|chunk| chunk.map_err(|e| BridgeError::Transport(e.to_string())));
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

fn parse_finish_reason(s: &str) -> FinishReason {
    match s {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "content_filter" => FinishReason::ContentFilter,
        "tool_calls" => FinishReason::ToolCalls,
        _ => FinishReason::Stop,
    }
}

/// T4：OpenAI 形状上游缓存命中子集的解析兜底链。
///
/// 不同上游把"prompt 缓存命中数"放在不同字段（均表示已包含在
/// prompt_tokens 内的子集）：
/// 1. `usage.prompt_cache_hit_tokens`（DeepSeek）
/// 2. `usage.prompt_tokens_details.cached_tokens`（OpenAI 新口径）
/// 3. `usage.cached_tokens`（Kimi/Moonshot）
fn extract_cache_hit(u: Option<&Value>) -> u64 {
    u.and_then(|u| u.get("prompt_cache_hit_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            u.and_then(|u| u.get("prompt_tokens_details"))
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
        })
        .or_else(|| {
            u.and_then(|u| u.get("cached_tokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0)
}

fn parse_usage(json: &Value) -> UsageStats {
    let u = json.get("usage");
    let prompt = u
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = u
        .and_then(|u| u.get("completion_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // T4：缓存命中兜底链 + 恒等式收敛——cache_hit = cache_hit.clamp(0,
    // prompt_tokens)，cache_miss = prompt_tokens - cache_hit（推导值，
    // 不落 UsageStats）。上游字段自相矛盾（如 hit > prompt）时收敛到
    // 合法区间，保证不变量 cache_hit ∈ [0, prompt_tokens] 不破。
    let cached_prompt_tokens = extract_cache_hit(u).clamp(0, prompt);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_prompt_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

/// 从 usage 子对象解析（流式 include_usage 终帧用）
fn parse_usage_from_value(u: &Value) -> UsageStats {
    let prompt = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
    let completion = u
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    // T4：与 parse_usage 同口径的兜底链 + 恒等式收敛（见其注释）
    let cached_prompt_tokens = extract_cache_hit(Some(u)).clamp(0, prompt);
    UsageStats {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        cached_prompt_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens: 0,
    }
}

fn parse_openai_error(body: &[u8]) -> Option<super::UpstreamErrorView> {
    let v: Value = serde_json::from_slice(body).ok()?;
    let err = v.get("error")?;
    Some(super::UpstreamErrorView {
        kind: err.get("type").and_then(|t| t.as_str()).map(String::from),
        message: err
            .get("message")
            .and_then(|m| m.as_str())
            .map(String::from),
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
                    usage: None,
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
                            .map(|t| super::ToolCallDelta {
                                index: t.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                    as usize,
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
                // 流式推理增量：DeepSeek 系用 reasoning_content，OpenAI o 系列用 reasoning
                let reasoning = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
                    .and_then(|r| r.as_str())
                    .map(String::from);
                let finish = v
                    .get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("finish_reason"))
                    .and_then(|f| f.as_str())
                    .map(parse_finish_reason);
                // stream_options.include_usage 终帧：usage 出现在顶层
                let usage = v.get("usage").map(parse_usage_from_value);
                return Some(ChatChunk {
                    id: id.to_string(),
                    model: model.to_string(),
                    delta: super::ChatDelta {
                        content,
                        tool_calls,
                        reasoning,
                    },
                    finish_reason: finish,
                    usage,
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
        base_url,
        api_key,
        client.clone(),
    ))
}

/// 带渠道级特判的构造（预置供应商渠道用）。
pub fn make_bridge_with_quirks(
    base_url: &str,
    api_key: &str,
    client: &reqwest::Client,
    quirks: crate::channel::ChannelQuirks,
) -> Arc<dyn Bridge> {
    Arc::new(
        OpenaiCompatibleBridge::with_client(base_url, api_key, client.clone()).with_quirks(quirks),
    )
}

#[cfg(test)]
mod tests {
    use super::normalize_base_url;
    use crate::bridge::{RerankDocument, RerankRequest, RerankResult};

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

    // ── rerank 请求/响应形状（纯序列化与解析逻辑，不发网络请求）──────────

    /// 验证 rerank 请求体形状：documents 序列化为 [{content}]、top_n 透传
    #[test]
    fn rerank_request_body_shape() {
        let req = RerankRequest {
            model: "rerank-v1".to_string(),
            query: "什么是网关".to_string(),
            documents: vec![
                RerankDocument {
                    content: "AIGX 是 Rust 网关".to_string(),
                },
                RerankDocument {
                    content: "无关文档".to_string(),
                },
            ],
            top_n: Some(1),
        };
        let body = serde_json::json!({
            "model": req.model,
            "query": req.query,
            "documents": req.documents.iter()
                .map(|d| serde_json::json!({ "content": d.content }))
                .collect::<Vec<_>>(),
            "top_n": req.top_n,
        });
        assert_eq!(body["model"], "rerank-v1");
        assert_eq!(body["documents"][0]["content"], "AIGX 是 Rust 网关");
        assert_eq!(body["top_n"], 1);
    }

    /// 模拟上游响应（Cohere/Jina 兼容形状）的宽容解析 + top_n 截断
    #[test]
    fn rerank_response_parse_and_truncate() {
        let upstream = serde_json::json!({
            "results": [
                {"index": 2, "relevance_score": 0.98},
                {"index": 0, "relevance_score": 0.75},
                {"index": 1, "relevance_score": 0.30}
            ],
            "usage": {"prompt_tokens": 42, "total_tokens": 42}
        });
        let results: Vec<RerankResult> = upstream
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|obj| RerankResult {
                        index: obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        relevance_score: obj
                            .get("relevance_score")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].index, 2);
        assert!((results[0].relevance_score - 0.98).abs() < 1e-9);

        // top_n 截断
        let mut truncated = results;
        truncated.truncate(2);
        assert_eq!(truncated.len(), 2);
        assert_eq!(truncated[1].index, 0);
    }

    /// 宽容解析：上游缺 results/usage 字段时不 panic，返回空结果与零用量
    #[test]
    fn rerank_response_lenient_parse_on_missing_fields() {
        let upstream = serde_json::json!({});
        let results: Vec<RerankResult> = upstream
            .get("results")
            .and_then(|r| r.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|obj| RerankResult {
                        index: obj.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
                        relevance_score: obj
                            .get("relevance_score")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0),
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(results.is_empty());

        let prompt_tokens = upstream
            .get("usage")
            .and_then(|u| u.get("prompt_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(prompt_tokens, 0);
    }

    /// handler 侧 documents 的宽容解析：[{content}] 与纯字符串两种形态
    #[test]
    fn rerank_documents_lenient_input_forms() {
        let body = serde_json::json!({
            "documents": [
                {"content": "结构化形态"},
                "纯字符串形态"
            ]
        });
        let docs: Vec<RerankDocument> = body
            .get("documents")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        if let Some(s) = v.as_str() {
                            Some(RerankDocument {
                                content: s.to_string(),
                            })
                        } else {
                            v.get("content")
                                .and_then(|c| c.as_str())
                                .map(|s| RerankDocument {
                                    content: s.to_string(),
                                })
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].content, "结构化形态");
        assert_eq!(docs[1].content, "纯字符串形态");
    }

    // ── T4：上游缓存命中兜底链 + 计费恒等式 clamp（纯解析，不发网络）────

    /// DeepSeek 老口径：usage.prompt_cache_hit_tokens 命中，优先级最高。
    #[test]
    fn usage_prefers_deepseek_cache_hit_field() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "prompt_cache_hit_tokens": 40,
                "prompt_cache_miss_tokens": 60,
                "prompt_tokens_details": { "cached_tokens": 25 }
            }
        });
        let stats = super::parse_usage(&json);
        assert_eq!(stats.prompt_tokens, 100);
        assert_eq!(
            stats.cached_prompt_tokens, 40,
            "DeepSeek 字段应优先于新口径"
        );
    }

    /// OpenAI 新口径：usage.prompt_tokens_details.cached_tokens。
    #[test]
    fn usage_reads_openai_cached_tokens_details() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "prompt_tokens_details": { "cached_tokens": 30 }
            }
        });
        let stats = super::parse_usage(&json);
        assert_eq!(stats.cached_prompt_tokens, 30);
    }

    /// Kimi/Moonshot：usage 顶层 cached_tokens。
    #[test]
    fn usage_reads_kimi_top_level_cached_tokens() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 10,
                "cached_tokens": 60
            }
        });
        let stats = super::parse_usage(&json);
        assert_eq!(stats.cached_prompt_tokens, 60);
    }

    /// 三路缓存字段都缺失时默认 0。
    #[test]
    fn usage_defaults_cache_hit_to_zero() {
        let json = serde_json::json!({
            "usage": { "prompt_tokens": 100, "completion_tokens": 10 }
        });
        let stats = super::parse_usage(&json);
        assert_eq!(stats.cached_prompt_tokens, 0);
    }

    /// 恒等式收敛：上游自相矛盾（hit > prompt）时 clamp 到 prompt，
    /// 保证 cache_hit ∈ [0, prompt_tokens]、cache_miss = prompt - hit 不为负。
    #[test]
    fn usage_clamps_oversized_cache_hit_to_prompt() {
        let json = serde_json::json!({
            "usage": {
                "prompt_tokens": 50,
                "completion_tokens": 5,
                "prompt_cache_hit_tokens": 80
            }
        });
        let stats = super::parse_usage(&json);
        assert_eq!(
            stats.cached_prompt_tokens, 50,
            "hit 超出 prompt 应收敛到 prompt"
        );
        assert_eq!(stats.prompt_tokens, 50, "prompt_tokens 原样保留");
    }

    /// 流式终帧（parse_usage_from_value）与非流式同口径。
    #[test]
    fn usage_from_value_uses_same_fallback_chain() {
        let u = serde_json::json!({
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "cached_tokens": 70
        });
        let stats = super::parse_usage_from_value(&u);
        assert_eq!(stats.cached_prompt_tokens, 70);

        let u = serde_json::json!({
            "prompt_tokens": 20,
            "completion_tokens": 5,
            "prompt_cache_hit_tokens": 99
        });
        let stats = super::parse_usage_from_value(&u);
        assert_eq!(stats.cached_prompt_tokens, 20, "流式路径同样 clamp 收敛");
    }

    /// 登录态型供应商：Cookie 与 `{credential}` 请求头模板真的到达上游。
    ///
    /// 起一个本地 OpenAI 兼容假上游，回显收到的 cookie / x-token，
    /// 断言 Bridge 构造的请求头与账号凭证一致（端到端，非仅序列化）。
    #[tokio::test]
    async fn session_auth_sends_cookie_and_templated_headers() {
        use crate::bridge::{Bridge, BridgeContext, ChatFormat, ChatMessage, Role};
        use axum::http::HeaderMap;
        use axum::{routing::post, Json, Router};

        async fn echo(headers: HeaderMap) -> Json<serde_json::Value> {
            let cookie = headers
                .get("cookie")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let token = headers
                .get("x-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            Json(serde_json::json!({
                "id": "chatcmpl-test",
                "model": "m",
                "choices": [{
                    "message": { "content": format!("cookie={cookie};token={token}") },
                    "finish_reason": "stop"
                }],
                "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
            }))
        }

        // 同时验证「对话路径覆盖」：/api/chat 而不是默认 /chat/completions
        let app = Router::new().route("/v1/api/chat", post(echo));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let bridge = super::OpenaiCompatibleBridge::with_session_auth(
            format!("http://{addr}/v1"),
            "sessionid=abc",
            reqwest::Client::new(),
            super::AuthMode::Cookie,
            vec![("x-token".to_string(), "{credential}".to_string())],
            Some("/api/chat".to_string()),
        );
        let req = ChatFormat {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("hi".to_string()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let ctx = BridgeContext::new("test-req", "m");
        let resp = bridge.chat(&req, &ctx).await.expect("chat should succeed");
        assert_eq!(
            resp.message.content_str(),
            "cookie=sessionid=abc;token=sessionid=abc"
        );
    }
}
