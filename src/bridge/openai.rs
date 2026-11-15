//! OpenAI 兼容 Bridge — 对接第三方 OpenAI 兼容上游（DeepSeek/OpenRouter/Together 等）。
//!
//! 参照 aisix provider-openai 的 wire-shape 适配 + VFaka PaymentProvider trait 模式
//! （统一 trait 抽象，每个上游一个实现）。复用 bridge::mod 中已定义的 Bridge trait。
//!
//! 该 Bridge 持有上游 base_url 与 api_key，将归一化 ChatFormat 转为 OpenAI chat.completions
//! 请求体，执行 HTTP 调用，解析响应。流式响应转为 ChatChunk 流。

use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use super::{
    apply_conditional_bearer, is_length_finish, Bridge, BridgeContext, BridgeError, ChatChunk,
    ChatChunkStream, ChatFormat, ChatMessage, ChatResponse, EmbeddingRequest, EmbeddingResponse,
    FinishReason, ImageEditFormPart, RerankRequest, RerankResponse, RerankResult,
    ResponsesPassthrough, Role, UpstreamWire, UsageStats,
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
    /// 上游**只接受流式**对话请求（会话供应商如 workbuddy）：非流式入口
    /// `chat()` 改发流式再本地聚合成一个响应，见 [`Self::chat_via_stream_aggregation`]。
    force_stream_aggregate: bool,
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
            force_stream_aggregate: false,
            quirks: crate::channel::ChannelQuirks::default(),
        }
    }

    /// 挂上渠道级特判（urlSuffix / 静态头 / OpenCode 客户端伪装）。
    pub fn with_quirks(mut self, quirks: crate::channel::ChannelQuirks) -> Self {
        self.quirks = quirks;
        self
    }

    /// 标记「该上游只接受流式对话请求」：`chat()`（非流式入口）改发流式并在
    /// 本地聚合成一个非流式响应，对下游语义不变。
    pub fn with_force_stream_aggregate(mut self, on: bool) -> Self {
        self.force_stream_aggregate = on;
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
        let chat_path = chat_path
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        // 归一化 base_url 只在**未显式给出对话路径**时进行：此时按 OpenAI SDK
        // 约定给裸 host 补 `/v1`（`https://host` → `https://host/v1`）。
        //
        // 一旦显式给了 `chat_path`（workbuddy 的 `/v2/chat/completions`、
        // modelscope 的 `/api/v1/chat/completions` 等），**路径才是权威** ——
        // 再补 `/v1` 会拼出 `https://copilot.tencent.com/v1/v2/chat/completions`，
        // 上游直接 404（实测 `{"error_msg":"404 Route Not Found"}`）。
        let base_url = if chat_path.is_some() {
            base_url.into().trim().trim_end_matches('/').to_string()
        } else {
            normalize_base_url(base_url.into())
        };
        Self {
            base_url,
            api_key: credential,
            client,
            auth_mode,
            session_headers: params,
            chat_path,
            force_stream_aggregate: false,
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

    fn images_edits_url(&self) -> String {
        self.quirks.append_suffix(&format!(
            "{}/images/edits",
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
                // Map 直建：替代每条消息 json! 建树的逐字段 Map 分配
                let mut msg = serde_json::Map::with_capacity(4);
                msg.insert("role".to_string(), Value::String(role.to_string()));
                msg.insert(
                    "content".to_string(),
                    match &m.content_blocks {
                        Some(blocks) => Value::Array(blocks.clone()),
                        None => m.content.clone().map(Value::String).unwrap_or(Value::Null),
                    },
                );
                if let Some(tid) = &m.tool_call_id {
                    msg.insert("tool_call_id".to_string(), Value::String(tid.clone()));
                }
                if let Some(tool_calls) = &m.tool_calls {
                    msg.insert(
                        "tool_calls".to_string(),
                        Value::Array(
                            tool_calls
                                .iter()
                                .map(|tc| {
                                    let mut f = serde_json::Map::with_capacity(2);
                                    f.insert(
                                        "name".to_string(),
                                        Value::String(tc.function_name.clone()),
                                    );
                                    f.insert(
                                        "arguments".to_string(),
                                        Value::String(tc.arguments.clone()),
                                    );
                                    let mut item = serde_json::Map::with_capacity(3);
                                    item.insert("id".to_string(), Value::String(tc.id.clone()));
                                    item.insert(
                                        "type".to_string(),
                                        Value::String("function".into()),
                                    );
                                    item.insert("function".to_string(), Value::Object(f));
                                    Value::Object(item)
                                })
                                .collect(),
                        ),
                    );
                }
                Value::Object(msg)
            })
            .collect();

        // Map 直建 + 预分配：旧路径每个 json!() 条件插入都新建一棵 Map
        // （每请求 ~12 次），此处按已知字段数一次分配；字段形状与 json!
        // 逐字段一致（含透传 extra 覆盖写）。
        let mut body = serde_json::Map::with_capacity(16);
        body.insert("model".to_string(), Value::String(req.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));
        body.insert("stream".to_string(), Value::Bool(req.stream));
        if let Some(tools) = &req.tools {
            body.insert("tools".to_string(), Value::Array(tools.clone()));
        }
        if let Some(tool_choice) = &req.tool_choice {
            body.insert("tool_choice".to_string(), tool_choice.clone());
        }
        if let Some(effort) = &req.reasoning_effort {
            body.insert(
                "reasoning_effort".to_string(),
                Value::String(effort.clone()),
            );
        }
        if let Some(web) = &req.web_search_options {
            body.insert("web_search_options".to_string(), web.clone());
        }
        if let Some(mt) = req.max_tokens {
            body.insert("max_tokens".to_string(), Value::Number(mt.into()));
        }
        if let Some(t) = req.temperature {
            body.insert("temperature".to_string(), Value::from(t));
        }
        if let Some(p) = req.top_p {
            body.insert("top_p".to_string(), Value::from(p));
        }
        if let Some(stop) = &req.stop {
            body.insert(
                "stop".to_string(),
                Value::Array(stop.iter().map(|s| Value::String(s.clone())).collect()),
            );
        }
        if let Some(extra) = &req.extra {
            if let Some(obj) = extra.as_object() {
                for (k, v) in obj {
                    body.insert(k.clone(), v.clone());
                }
            }
        }
        Value::Object(body)
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
        ctx: &BridgeContext,
    ) -> Result<ChatResponse, BridgeError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(false);

        // OpenCode 免费档只接受流式请求（见 channel::emulation）；会话供应商
        // 里也有只收流式的家（workbuddy，见 registry::requires_stream）——
        // 两者都改发流式再在本地聚合成一个响应，对下游仍是「非流式」语义。
        if self.force_stream_aggregate
            || (self.quirks.emulates_opencode()
                && !crate::channel::emulation::is_opencode_go_endpoint(&self.base_url))
        {
            return self.chat_via_stream_aggregation(req).await;
        }

        let (request, body) = self.apply_emulation(body);
        let resp = crate::debug_traffic::traced(
            request
                .timeout(crate::bridge::nonstream_request_timeout())
                .json(&body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
        ctx: &BridgeContext,
    ) -> Result<ChatChunkStream, BridgeError> {
        let mut body = self.build_body(req);
        body["stream"] = serde_json::json!(true);

        let (request, body) = self.apply_emulation(body);
        // 流式请求改用宽松的总超时（详见 `Bridge::stream_request_timeout`）：
        // 共享 client 的 300s 是累计上限，会掐断长流；空闲兜底交给 read_timeout。
        let resp = crate::debug_traffic::traced(
            request
                .timeout(crate::bridge::stream_request_timeout())
                .json(&body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
                        // 零拷贝事件切片：缓冲字节来自逐事件产出的合法 UTF-8 流，
                        // 直接借用切片解析，省去每事件一次 String 分配；仅在遇到
                        // 非法字节（损坏流）时回退 lossy 重建。
                        let lossy;
                        let event: &str = match std::str::from_utf8(&buf[..end]) {
                            Ok(s) => s,
                            Err(_) => {
                                lossy = String::from_utf8_lossy(&buf[..end]).into_owned();
                                &lossy
                            }
                        };
                        // 换行保活帧：开关开启时直接丢（关掉时不做任何解析）
                        let keepalive = crate::config::strip_newline_keepalive()
                            && is_newline_keepalive_event(event);
                        let chunk = if keepalive {
                            None
                        } else {
                            parse_sse_event(event, &id, &model)
                        };
                        buf.drain(..end);
                        if let Some(chunk) = chunk {
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
                                let lossy;
                                let tail: &str = match std::str::from_utf8(&buf) {
                                    Ok(s) => s,
                                    Err(_) => {
                                        lossy = String::from_utf8_lossy(&buf).into_owned();
                                        &lossy
                                    }
                                };
                                let chunk = if crate::config::strip_newline_keepalive()
                                    && is_newline_keepalive_event(tail)
                                {
                                    None
                                } else {
                                    parse_sse_event(tail, &id, &model)
                                };
                                buf.clear();
                                if let Some(chunk) = chunk {
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
        ctx: &BridgeContext,
    ) -> Result<EmbeddingResponse, BridgeError> {
        let url = format!("{}/embeddings", self.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": req.model,
            "input": req.input,
        });
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(&url)).json(&body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
        ctx: &BridgeContext,
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
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(&url)).json(&body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
    async fn complete(&self, body: &Value, ctx: &BridgeContext) -> Result<Value, BridgeError> {
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(self.completions_url()))
                .json(body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(self.images_url()))
                .json(body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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

    /// 图片编辑：把 handler 解析好的 multipart 表单项重组为上游
    /// `/images/edits` 请求。字段值/文件字节原样转发（model 已由
    /// handler 按渠道映射改写），返回 OpenAI 格式结果。
    async fn edit_image(
        &self,
        parts: &[ImageEditFormPart],
        ctx: &BridgeContext,
    ) -> Result<Value, BridgeError> {
        let mut form = reqwest::multipart::Form::new();
        for part in parts {
            match part {
                ImageEditFormPart::Text { name, value } => {
                    form = form.text(name.clone(), value.clone());
                }
                ImageEditFormPart::File {
                    name,
                    filename,
                    content_type,
                    data,
                } => {
                    // 客户端声明的 content-type 可能畸形（mime_str 拒绝），
                    // 退回 application/octet-stream 重建，文件字节不受影响。
                    // 注：`Part::bytes` 的入参约束是 `Into<Cow<'static, [u8]>>`，
                    // 而 `bytes::Bytes` 没有实现该转换 —— 必须显式 `to_vec()`。
                    let file_part = reqwest::multipart::Part::bytes(data.to_vec())
                        .file_name(filename.clone())
                        .mime_str(content_type)
                        .unwrap_or_else(|_| {
                            reqwest::multipart::Part::bytes(data.to_vec())
                                .file_name(filename.clone())
                                .mime_str("application/octet-stream")
                                .expect("octet-stream is a valid mime type")
                        });
                    form = form.part(name.clone(), file_part);
                }
            }
        }
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(self.images_edits_url()))
                .multipart(form),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
    ///
    /// body（含 model/input/stream 等全部字段）不重写、不增删，
    /// 上游 2xx 时非流式返回完整 JSON、流式返回 SSE 原始字节流；
    /// 非流式默认共享 Client 的 120s 请求超时（与 chat 一致）。
    async fn responses_passthrough(
        &self,
        body: &Value,
        stream: bool,
        ctx: &BridgeContext,
    ) -> Result<ResponsesPassthrough, BridgeError> {
        let resp = crate::debug_traffic::traced(
            self.auth_conditional(self.client.post(self.responses_url()))
                .json(body),
            ctx,
        )
        .send()
        .await
        .map_err(|e| BridgeError::Transport(e.to_string()))?;
        crate::debug_traffic::traced_response(&resp, ctx);

        if !resp.status().is_success() {
            return Err(super::capture_upstream_error_http(
                ctx,
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
    if is_length_finish(s) {
        return FinishReason::Length;
    }
    match s {
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

/// SSE 事件是否「换行保活帧」（开关开启时在下发前丢弃）。
///
/// 与 [`parse_sse_event`] 同款 `data:` 前缀解析（兼容 `data:` 后无空格）；
/// `[DONE]` 与非 JSON 负载一律不算——它们不是保活节拍，丢掉会伤到收尾。
fn is_newline_keepalive_event(event: &str) -> bool {
    for line in event.lines() {
        if let Some(data) = line.strip_prefix("data:").map(str::trim_start) {
            if data.trim() == "[DONE]" {
                return false;
            }
            if let Ok(value) = serde_json::from_str::<Value>(data) {
                return crate::sse::is_newline_keepalive(&value);
            }
        }
    }
    false
}

/// OpenAI 流式 chunk 的轻量线格式。
///
/// 逐事件 `serde_json::from_str::<Value>` 会把整帧建成 Value 树（每事件
/// 十几次分配）；改用结构体一次解析，字段宽容口径与旧 Value 路径逐字段对齐：
/// - 缺字段 = 默认值 / None（旧 `.get().unwrap_or()` 口径）；
/// - 标量字段类型不合法（null/数值/对象当字符串用等）按旧 `.as_str()` /
///   `.as_u64()` 口径回退 None 或 0，单个畸形字段不拖垮整帧（见各 lenient
///   反序列化器）；
/// - 容器字段类型不合法（如 choices 是字符串）时旧路径整帧退化为「空 chunk」，
///   新路径整帧跳过——上游真实流量不会出现该形态（B17 级畸形已在行解析处理）。
///
/// 注意 `usage` 的反序列化保留「字段存在即 Some」语义：旧路径
/// `v.get("usage").map(parse_usage_from_value)` 对 `"usage": null` / `{}` /
/// 畸形形状都产出 Some(全零)，新路径必须逐形一致，否则流式终帧形状漂移。
#[derive(serde::Deserialize)]
struct ChunkWire {
    #[serde(default)]
    choices: Option<Vec<ChoiceWire>>,
    #[serde(default, deserialize_with = "de_usage_always_some")]
    usage: Option<UsageWire>,
}

#[derive(serde::Deserialize, Default)]
struct ChoiceWire {
    #[serde(default)]
    delta: DeltaWire,
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    finish_reason: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct DeltaWire {
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    content: Option<String>,
    // DeepSeek 系 reasoning_content / OpenAI o 系列 reasoning，两取一
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCallWire>>,
}

#[derive(serde::Deserialize)]
struct ToolCallWire {
    #[serde(default, deserialize_with = "de_lenient_u64")]
    index: u64,
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    id: Option<String>,
    #[serde(default)]
    function: Option<FunctionWire>,
}

#[derive(serde::Deserialize, Default)]
struct FunctionWire {
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    name: Option<String>,
    #[serde(default, deserialize_with = "de_opt_lenient_str")]
    arguments: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct UsageWire {
    #[serde(default, deserialize_with = "de_lenient_u64")]
    prompt_tokens: u64,
    #[serde(default, deserialize_with = "de_lenient_u64")]
    completion_tokens: u64,
    // T4 缓存命中兜底链字段：Option 区分「缺失/非法」与「0」——链式回退只在
    // 字段缺失或非法时继续，`0` 视为有效命中值（与旧 extract_cache_hit 同口径）
    #[serde(default, deserialize_with = "de_opt_lenient_u64")]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default, deserialize_with = "de_opt_lenient_u64")]
    cached_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<CacheDetailsWire>,
}

#[derive(serde::Deserialize, Default)]
struct CacheDetailsWire {
    #[serde(default, deserialize_with = "de_opt_lenient_u64")]
    cached_tokens: Option<u64>,
}

impl UsageWire {
    /// 转 UsageStats：缓存命中兜底链（DeepSeek 旧字段 → OpenAI details →
    /// Kimi 顶层）+ 恒等式收敛（`min(prompt)`，保证 hit ∈ [0, prompt]），
    /// 与 `parse_usage_from_value` 逐字段同口径。
    fn into_stats(self) -> UsageStats {
        let prompt = self.prompt_tokens;
        let completion = self.completion_tokens;
        let cached_prompt_tokens = self
            .prompt_cache_hit_tokens
            .or_else(|| self.prompt_tokens_details.and_then(|d| d.cached_tokens))
            .or(self.cached_tokens)
            .unwrap_or(0)
            .min(prompt);
        UsageStats {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: prompt + completion,
            cached_prompt_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
        }
    }
}

/// 整型字段的宽容解析：数值原样；null/浮点/字符串等旧 `.as_u64()` 口径
/// 回退 0 —— 单个畸形字段不拖垮整个 usage 对象。
fn de_lenient_u64<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u64::deserialize(d).unwrap_or(0))
}

/// Option 整型：缺失/非法 → None（兜底链继续下一位）；数值原样。
fn de_opt_lenient_u64<'de, D>(d: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(u64::deserialize(d).ok())
}

/// Option 字符串：字符串原样；其余（null/数值/布尔/对象）旧 `.as_str()`
/// 口径回退 None。
fn de_opt_lenient_str<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(String::deserialize(d).ok())
}

/// usage 字段反序列化：「存在即 Some」——null/畸形形状回退全零
/// UsageWire，与旧 `v.get("usage").map(parse_usage_from_value)` 对
/// Null/字符串/数组的宽容口径（.get() 得 None → 全零）逐形一致。
///
/// 先解析成 `Value` 再判形状：只有**对象**才转 `UsageWire`；数组/字符串/
/// 数字等非对象形状（含 `[1,2]`）直接 `Some(default)`。否则 serde 派生结构体
/// 的 struct-from-seq 会把数组元素**位置化**消费成 prompt/completion tokens
/// （`[1,2]` → prompt=1/completion=2/total=3），与旧口径相悖；而直接让非对象
/// 形状报错又会让整帧 `ChunkWire` 反序列化失败、`parse_sse_event` 返回 None，
/// 丢掉「存在即 Some → 必产出 chunk」的契约。故此处必须回退 default 而非报错。
fn de_usage_always_some<'de, D>(d: D) -> Result<Option<UsageWire>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: serde_json::Value = serde::Deserialize::deserialize(d)?;
    let Some(object) = value.as_object() else {
        return Ok(Some(UsageWire::default()));
    };
    let wire = serde_json::from_value::<UsageWire>(serde_json::Value::Object(object.clone()))
        .unwrap_or_default();
    Ok(Some(wire))
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
            let Ok(wire) = serde_json::from_str::<ChunkWire>(data) else {
                continue;
            };
            let (delta_wire, finish_wire) = match wire.choices {
                Some(mut cs) => {
                    let c = if cs.is_empty() {
                        ChoiceWire::default()
                    } else {
                        cs.remove(0)
                    };
                    (c.delta, c.finish_reason)
                }
                None => (DeltaWire::default(), None),
            };
            let reasoning = delta_wire.reasoning_content.or(delta_wire.reasoning);
            let tool_calls = delta_wire.tool_calls.map(|arr| {
                arr.into_iter()
                    .map(|t| {
                        let ToolCallWire {
                            index,
                            id,
                            function,
                        } = t;
                        let (name, arguments) = match function {
                            Some(f) => (f.name, f.arguments),
                            None => (None, None),
                        };
                        super::ToolCallDelta {
                            index: index as usize,
                            id,
                            function_name: name,
                            arguments,
                            signature: None,
                        }
                    })
                    .collect()
            });
            return Some(ChatChunk {
                id: id.to_string(),
                model: model.to_string(),
                delta: super::ChatDelta {
                    content: delta_wire.content,
                    tool_calls,
                    reasoning,
                },
                finish_reason: finish_wire.as_deref().map(parse_finish_reason),
                usage: wire.usage.map(|u| u.into_stats()),
            });
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

    /// 登录态供应商显式给了 `chat_path` 时，base_url 必须**原样使用**：
    /// 补 `/v1` 会拼出 `/v1/v2/chat/completions` 这类不存在的路由（实测
    /// workbuddy `copilot.tencent.com` 上游 404）。这是 v1.0.46 的生产缺陷。
    #[test]
    fn session_auth_keeps_base_when_chat_path_explicit() {
        let bridge = super::OpenaiCompatibleBridge::with_session_auth(
            "https://copilot.tencent.com",
            "tok",
            crate::test_http_client(),
            super::AuthMode::Bearer,
            Vec::new(),
            Some("/v2/chat/completions".to_string()),
        );
        assert_eq!(bridge.base_url, "https://copilot.tencent.com");
        assert_eq!(
            bridge.chat_url(),
            "https://copilot.tencent.com/v2/chat/completions"
        );
    }

    /// 未给 `chat_path` 时保留既有行为：裸 host 仍按 OpenAI 约定补 `/v1`
    /// （atomcode / chatglm 这类 API-Key 型登录态供应商依赖它）。
    #[test]
    fn session_auth_normalizes_when_chat_path_absent() {
        let bridge = super::OpenaiCompatibleBridge::with_session_auth(
            "https://atomgit.com",
            "tok",
            crate::test_http_client(),
            super::AuthMode::Bearer,
            Vec::new(),
            None,
        );
        assert_eq!(bridge.base_url, "https://atomgit.com/v1");
        assert_eq!(bridge.chat_url(), "https://atomgit.com/v1/chat/completions");

        // 空白 chat_path（provider 默认也是空串）等同「未给」
        let blank = super::OpenaiCompatibleBridge::with_session_auth(
            "https://atomgit.com",
            "tok",
            crate::test_http_client(),
            super::AuthMode::Bearer,
            Vec::new(),
            Some("   ".to_string()),
        );
        assert_eq!(blank.base_url, "https://atomgit.com/v1");
    }

    /// **全量扫描**登录态供应商的对话 URL 拼接：用真实构造函数
    /// `with_session_auth` 把 `SESSION_PROVIDERS` 每家跑一遍，钉住
    /// 「chat_path 非空 → URL = provider 默认 base + chat_path（不得多插 /v1）」
    /// 与「chat_path 为空 → 默认补 /v1」。
    ///
    /// 这是「其他家也不要出同族问题」的通用护栏：registry 表里任何一家把
    /// base 或 chat_path 写错（例如新增家抄了 `.../v1` 的 base 又给 `/v1/...`
    /// 的 chat_path），本测直接点名是哪一家。
    #[test]
    fn session_providers_resolve_urls_without_double_prefix() {
        use crate::account_session::registry::SESSION_PROVIDERS;
        let mut checked = 0usize;
        for (kind, meta) in SESSION_PROVIDERS {
            // 默认地址为空的（officeace 等「必须显式填地址」的家）没有可断言的 base
            if meta.base_url.trim().is_empty() {
                continue;
            }
            let bridge = super::OpenaiCompatibleBridge::with_session_auth(
                meta.base_url,
                "tok",
                crate::test_http_client(),
                super::AuthMode::Bearer,
                Vec::new(),
                Some(meta.chat_path.to_string()),
            );
            let expected = if meta.chat_path.trim().is_empty() {
                format!(
                    "{}/chat/completions",
                    normalize_base_url(meta.base_url.to_string())
                )
            } else {
                format!("{}{}", meta.base_url.trim_end_matches('/'), meta.chat_path)
            };
            assert_eq!(
                bridge.chat_url(),
                expected,
                "provider `{kind}` 对话 URL 拼接错误"
            );
            checked += 1;
        }
        // 扫描面自证：表非空且覆盖面足够，防「表被改空 / 迭代失效」→ 本测恒绿
        assert!(checked >= 25, "只覆盖了 {checked} 家，扫描面疑似失效");
    }

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

    /// Map 直建的 build_body 与旧 json! 建树序列化逐字段等价（P1 性能项
    /// 行为等价自检）：含 tools/tool_choice/extra 透传、content:null
    /// 工具消息、content_blocks 多模态形态、标量字段。
    #[test]
    fn build_body_matches_json_tree_shape() {
        use crate::bridge::{ChatFormat, ToolCall};
        let req = ChatFormat {
            model: "gpt-4".into(),
            messages: vec![
                crate::bridge::ChatMessage {
                    role: crate::bridge::Role::User,
                    content: Some("hi".into()),
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: None,
                    reasoning: None,
                },
                crate::bridge::ChatMessage {
                    role: crate::bridge::Role::Assistant,
                    content: None,
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: Some(vec![ToolCall {
                        id: "call_1".into(),
                        function_name: "f".into(),
                        arguments: "{}".into(),
                    }]),
                    reasoning: None,
                },
                crate::bridge::ChatMessage {
                    role: crate::bridge::Role::Tool,
                    content: None,
                    content_blocks: Some(vec![
                        serde_json::json!({"type": "text", "text": "block"}),
                    ]),
                    name: None,
                    tool_call_id: Some("call_1".into()),
                    tool_calls: None,
                    reasoning: None,
                },
            ],
            tools: Some(vec![serde_json::json!({
                "type": "function",
                "function": {"name": "f", "parameters": {}}
            })]),
            max_tokens: Some(100),
            temperature: Some(0.7),
            top_p: Some(0.9),
            stream: true,
            top_k: None,
            stop: Some(vec!["END".into()]),
            tool_choice: Some(serde_json::json!({"type": "function", "function": {"name": "f"}})),
            reasoning_effort: Some("high".into()),
            web_search_options: None,
            extra: Some(serde_json::json!({"seed": 42, "presence_penalty": 0.5})),
        };
        let bridge =
            super::OpenaiCompatibleBridge::with_client("https://x", "k", crate::test_http_client());
        let body = bridge.build_body(&req);

        // 期望形状 = 旧 json! 建树口径手工构造
        let expected = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{}"}}]},
                {"role": "tool", "content": [{"type": "text", "text": "block"}], "tool_call_id": "call_1"}
            ],
            "stream": true,
            "tools": [{"type": "function", "function": {"name": "f", "parameters": {}}}],
            "tool_choice": {"type": "function", "function": {"name": "f"}},
            "reasoning_effort": "high",
            "max_tokens": 100,
            "temperature": 0.7,
            "top_p": 0.9,
            "stop": ["END"],
            "seed": 42,
            "presence_penalty": 0.5,
        });
        assert_eq!(body, expected, "build_body 输出必须与 json! 建树逐字段一致");
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

    /// 流式终帧 usage 兜底链（UsageWire::into_stats）与非流式同口径。
    #[test]
    fn usage_wire_uses_same_fallback_chain() {
        let u: super::UsageWire = serde_json::from_str(
            r#"{"prompt_tokens":100,"completion_tokens":10,"cached_tokens":70}"#,
        )
        .unwrap();
        let stats = u.into_stats();
        assert_eq!(stats.cached_prompt_tokens, 70);

        let u: super::UsageWire = serde_json::from_str(
            r#"{"prompt_tokens":20,"completion_tokens":5,"prompt_cache_hit_tokens":99}"#,
        )
        .unwrap();
        let stats = u.into_stats();
        assert_eq!(stats.cached_prompt_tokens, 20, "流式路径同样 clamp 收敛");

        // 缓存命中字段缺失/为 0 时的行为：0 是有效命中值，链不回退
        let u: super::UsageWire = serde_json::from_str(
            r#"{"prompt_tokens":10,"prompt_cache_hit_tokens":0,"cached_tokens":5}"#,
        )
        .unwrap();
        let stats = u.into_stats();
        assert_eq!(stats.cached_prompt_tokens, 0, "0 命中值不应回退到次位字段");
    }

    /// 「usage 存在即 Some」语义：`"usage": null` / `{}` / 畸形形状都产出
    /// Some(全零)（旧 Value 路径 v.get("usage").map(parse_usage_from_value)
    /// 的逐形行为），不得因结构体解析退化为 None。
    #[test]
    fn usage_presence_shapes_all_yield_some() {
        for data in [
            r#"{"usage": null}"#,
            r#"{"usage": {}}"#,
            r#"{"usage": "oops"}"#,
            r#"{"usage": [1,2]}"#,
        ] {
            let chunk = super::parse_sse_event(&format!("data: {data}"), "id", "m")
                .unwrap_or_else(|| panic!("usage 各形状都必须产出 chunk：{data}"));
            assert!(chunk.usage.is_some(), "usage 字段存在即 Some：{data}");
            assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 0, "{data}");
        }
    }

    /// 流式 chunk 轻量结构体解析与旧 Value 路径逐字段等价：
    /// 文本增量 / 推理增量 / finish_reason / tool_calls 分片 / [DONE]。
    #[test]
    fn parse_sse_event_matches_value_path() {
        use crate::bridge::FinishReason;
        // 常规文本增量
        let chunk = super::parse_sse_event(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"he\"}}]}",
            "chatcmpl-1",
            "m1",
        )
        .unwrap();
        assert_eq!(chunk.id, "chatcmpl-1");
        assert_eq!(chunk.model, "m1");
        assert_eq!(chunk.delta.content.as_deref(), Some("he"));
        assert_eq!(chunk.finish_reason, None);

        // reasoning_content 与 reasoning 两取一（DeepSeek 系 / o 系列）
        let chunk = super::parse_sse_event(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"r1\"}}]}",
            "id",
            "m",
        )
        .unwrap();
        assert_eq!(chunk.delta.reasoning.as_deref(), Some("r1"));
        let chunk = super::parse_sse_event(
            "data: {\"choices\":[{\"delta\":{\"reasoning\":\"r2\"}}]}",
            "id",
            "m",
        )
        .unwrap();
        assert_eq!(chunk.delta.reasoning.as_deref(), Some("r2"));

        // finish_reason + include_usage 终帧
        let chunk = super::parse_sse_event(
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":3}}",
            "id",
            "m",
        )
        .unwrap();
        assert_eq!(chunk.finish_reason, Some(FinishReason::Length));
        let usage = chunk.usage.expect("usage 终帧必须产出");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens), (7, 3));

        // tool_calls 分片（id/name 首帧 + arguments 分片）。
        // raw string 写死 JSON 文本：arguments 值内含转义引号，
        // 普通字面量手打转义易错（`\":\"` 段会把转义引号打成裸引号）。
        let chunk = super::parse_sse_event(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"get_weather","arguments":"{\"city\":\""}}]}}]}"#,
            "id",
            "m",
        )
        .unwrap();
        let tcs = chunk.delta.tool_calls.expect("tool_calls 必须产出");
        assert_eq!(tcs.len(), 1);
        assert_eq!(tcs[0].index, 0);
        assert_eq!(tcs[0].id.as_deref(), Some("call_1"));
        assert_eq!(tcs[0].function_name.as_deref(), Some("get_weather"));
        assert_eq!(tcs[0].arguments.as_deref(), Some("{\"city\":\""));

        // [DONE]
        let chunk = super::parse_sse_event("data: [DONE]", "id", "m").unwrap();
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
        assert_eq!(chunk.delta.content, None);

        // 缺字段宽容：空 choices / 缺 delta 都产出空 chunk（不 panic、不丢帧）。
        // ChatDelta 未实现 PartialEq（bridge/mod.rs 不在本轮改动范围），
        // 逐字段断言。
        let chunk = super::parse_sse_event("data: {\"choices\":[]}", "id", "m").unwrap();
        assert!(chunk.delta.content.is_none());
        assert!(chunk.delta.tool_calls.is_none());
        assert!(chunk.delta.reasoning.is_none());
        let chunk = super::parse_sse_event("data: {\"choices\":[{}]}", "id", "m").unwrap();
        assert!(chunk.delta.content.is_none());
        assert!(chunk.delta.tool_calls.is_none());
        assert!(chunk.delta.reasoning.is_none());
        // 非对象 JSON 帧：新路径整帧跳过（旧路径会产出空 chunk 噪音帧）
        assert!(super::parse_sse_event("data: 42", "id", "m").is_none());
        assert!(super::parse_sse_event("data: \"hello\"", "id", "m").is_none());
        // 非 data 行不产出
        assert!(super::parse_sse_event("event: message", "id", "m").is_none());
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
            crate::test_http_client(),
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

    /// 只收流式的上游（workbuddy）：非流式入口 `chat()` 真的改发了流式请求，
    /// 并把它聚合成一个非流式响应返回。
    ///
    /// 起一个本地假上游：只有收到 `"stream":true` 才回 SSE 帧，否则回 400
    /// 「Non-stream ... not supported」（复刻 WorkBuddy 的真实拒法）。这样断言
    /// 的不只是「聚合函数能跑」，而是**非流式调用确实变成了流式请求**。
    #[tokio::test]
    async fn force_stream_aggregate_sends_stream_upstream_and_aggregates() {
        use crate::bridge::{Bridge, BridgeContext, ChatFormat, ChatMessage, Role};
        use axum::{routing::post, Json, Router};

        async fn upstream(Json(body): Json<serde_json::Value>) -> axum::response::Response {
            use axum::response::IntoResponse;
            if body.get("stream").and_then(|v| v.as_bool()) != Some(true) {
                // 非流式一律拒绝（WorkBuddy copilot.tencent.com 的真实行为）
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({
                        "code": 11101,
                        "msg": "Non-stream chat request is currently not supported"
                    })),
                )
                    .into_response();
            }
            let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"he\"}}]}\n\n\
data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"total_tokens\":3}}\n\n\
data: [DONE]\n\n";
            (
                [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                sse,
            )
                .into_response()
        }

        let app = Router::new().route("/v2/chat/completions", post(upstream));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let bridge = super::OpenaiCompatibleBridge::with_session_auth(
            format!("http://{addr}"),
            "tok",
            crate::test_http_client(),
            super::AuthMode::Bearer,
            Vec::new(),
            Some("/v2/chat/completions".to_string()),
        )
        .with_force_stream_aggregate(true);

        let req = ChatFormat {
            model: "auto".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: Some("你好".to_string()),
                content_blocks: None,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            tools: None,
            max_tokens: Some(1),
            temperature: None,
            top_p: None,
            stream: false, // 下游要的是非流式
            top_k: None,
            stop: None,
            tool_choice: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let resp = bridge
            .chat(&req, &BridgeContext::new("test-req", "auto"))
            .await
            .expect("强制流式聚合后应当成功");
        assert_eq!(resp.message.content_str(), "hello");
        assert_eq!(resp.usage.total_tokens, 3);

        // 对照：未开该标记时同样调用必须失败（证明开关真的在起作用，
        // 而不是「上游本来就不拒」）
        let plain = super::OpenaiCompatibleBridge::with_session_auth(
            format!("http://{addr}"),
            "tok",
            crate::test_http_client(),
            super::AuthMode::Bearer,
            Vec::new(),
            Some("/v2/chat/completions".to_string()),
        );
        let err = plain
            .chat(&req, &BridgeContext::new("test-req", "auto"))
            .await
            .expect_err("未开开关时必须被上游 400 拒掉");
        assert!(format!("{err}").contains("11101") || format!("{err}").contains("400"));
    }
}
