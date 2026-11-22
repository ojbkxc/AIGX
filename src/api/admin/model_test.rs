//! 模型测试 —— `POST /api/models/test`。
//!
//! 统一模型管理页的「测试」按钮：钉住 `(provider, model)`（可选再钉某个账号或
//! 某个渠道），沿真实上游链路发一条最小对话，把结论（用时 / 首字延迟 / 用量 /
//! 回复 / 错误）结构化返回。对照 Agent2API `api::model_test`。
//!
//! 口径：
//! - 仅管理员（渠道凭证与账号凭证都是运营资产，普通用户走 Playground）。
//! - 单发不顺延：不像数据面那样换账号/换渠道重试 —— 测试要的是「这条链路到底
//!   通不通」的确定结论，重试会把失败掩盖掉。
//! - 响应永远 200，结论在 `data.ok` / `data.error`（与 Agent2API 同约定）。
//! - 每次测试落请求日志（`key_id = "model_test"`），管理员不计费、不进配额扣减。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::account_session::adapter;
use crate::account_session::model::{AccountSession, CREDENTIAL_KIND_NONE};
use crate::bridge::{
    Bridge, BridgeContext, BridgeError, ChatFormat, ChatMessage, ResponsesPassthrough, Role,
    UsageStats,
};
use crate::channel::{Channel, ChannelType};

/// 单次测试的总时间预算（构造 + 上游响应）。
const TEST_TIMEOUT: Duration = Duration::from_secs(150);
/// 默认测试提示词（与 Agent2API `DEFAULT_TEST_PROMPT` 同句）。
const DEFAULT_PROMPT: &str = "你好";

#[derive(Debug, Deserialize)]
pub struct ModelTestRequest {
    /// 统一模型管理视图里的 provider（渠道类型或账号 provider）
    pub provider: String,
    /// 模型 id（视图行里的上游名）
    pub model: String,
    /// 钉住某个账号（账号池来源）；与 channel_id 同时给出时优先
    #[serde(default)]
    pub account_id: Option<String>,
    /// 钉住某个渠道（渠道来源）
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// low / medium / high / off 等，原样透传给 bridge
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// 默认 true：同时验证 SSE 链路与首字延迟
    #[serde(default = "default_stream")]
    pub stream: bool,
    /// 对话协议：`chat_completions`（默认，走该来源原生链路，完全向后兼容）/
    /// `responses`（OpenAI Responses 透传）/ `anthropic`（按 Anthropic 线格式构造）。
    #[serde(default)]
    pub protocol: Option<String>,
    /// 测试出口：`direct` / `pool` / `pool:<id>` / `node:<id>` / 代理 URL；
    /// 缺省（None/空）= 沿用账号自身代理 / 渠道默认直连。
    #[serde(default)]
    pub proxy: Option<String>,
    /// `proxy` 的别名（A2A 桌面端叫「测试节点」）；与 `proxy` 同时给出时 `proxy` 优先。
    #[serde(default)]
    pub node: Option<String>,
}

/// 测试对话协议（A2A `model-test-dialog` 的三选一）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestProtocol {
    ChatCompletions,
    Responses,
    Anthropic,
}

impl TestProtocol {
    fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            None | Some("chat_completions") => Ok(Self::ChatCompletions),
            Some("responses") => Ok(Self::Responses),
            Some("anthropic") => Ok(Self::Anthropic),
            Some(other) => Err(format!(
                "不支持的 protocol「{other}」：仅支持 chat_completions / responses / anthropic"
            )),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::Anthropic => "anthropic",
        }
    }
}

/// 把测试出口描述归一为 `proxy_pool::resolve_proxy` 能识别的 spec。
///
/// - `direct` / 空 → `None`（直连，不使用任何代理）；
/// - `pool` → `rotate`（代理池轮询）；
/// - `pool:<id>` 原样；`node:<id>` → `pool:<id>`（「测试节点」语义）；
/// - 其余按代理 URL 原样透传。
fn normalize_proxy_spec(raw: Option<&str>) -> Option<String> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    if raw.eq_ignore_ascii_case("direct") {
        return None;
    }
    if raw.eq_ignore_ascii_case("pool") {
        return Some("rotate".to_string());
    }
    if let Some(id) = raw.strip_prefix("node:") {
        return Some(format!("pool:{}", id.trim()));
    }
    Some(raw.to_string())
}

fn default_stream() -> bool {
    true
}

/// 选中的承载方（账号或渠道），附带构造好的 Bridge 与上游模型名。
struct Picked {
    kind: &'static str, // "account" / "channel"
    channel_id: String,
    name: String,
    account_id: Option<String>,
    bridge: Arc<dyn Bridge>,
    upstream_model: String,
    /// 生效上游地址 / 凭证 / 出口 Client：供 `protocol=anthropic` 时按需
    /// 重建同一来源的 Anthropic 线格式桥（不改变默认 chat 行为）。
    base_url: String,
    secret: String,
    client: Arc<reqwest::Client>,
}

/// POST /api/models/test
pub async fn handle_model_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ModelTestRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let provider = body.provider.trim().to_string();
    let model = body.model.trim().to_string();
    if provider.is_empty() || model.is_empty() {
        return Err(error_response(
            "provider 与 model 不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }

    // 对话协议：默认 chat_completions = 走来源原生链路（完全向后兼容）。
    let protocol = match TestProtocol::parse(body.protocol.as_deref()) {
        Ok(p) => p,
        Err(message) => return Err(error_response(&message, StatusCode::BAD_REQUEST)),
    };

    // 测试出口：`proxy` 优先于 `node`；空 = 沿用账号自身代理 / 渠道默认直连。
    let proxy_raw = body.proxy.as_deref().or(body.node.as_deref());
    let client_override = match test_client_override(&state, proxy_raw) {
        Ok(c) => c,
        Err(message) => {
            // 出口配置有问题是「结论」而非 500：200 + ok:false，前端原样展示
            return Ok(Json(json!({
                "success": true,
                "data": { "ok": false, "error": message, "model": model, "provider": provider }
            })));
        }
    };

    let picked = match pick_source(&state, &provider, &model, &body, client_override) {
        Ok(p) => p,
        Err(message) => {
            // 没有可用来源也是「结论」：200 + ok:false，前端原样展示
            return Ok(Json(json!({
                "success": true,
                "data": { "ok": false, "error": message, "model": model, "provider": provider }
            })));
        }
    };

    let prompt = body
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_PROMPT)
        .to_string();
    let system_text = body
        .system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(sys) = &system_text {
        messages.push(ChatMessage {
            role: Role::System,
            content: Some(sys.clone()),
            content_blocks: None,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        });
    }
    messages.push(ChatMessage {
        role: Role::User,
        content: Some(prompt.clone()),
        content_blocks: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
    });

    let reasoning_effort = body
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let req = ChatFormat {
        model: picked.upstream_model.clone(),
        messages,
        tools: None,
        max_tokens: None,
        temperature: None,
        top_p: None,
        stream: body.stream,
        top_k: None,
        stop: None,
        tool_choice: None,
        reasoning_effort,
        web_search_options: None,
        extra: None,
    };
    let ctx = BridgeContext::new(uuid::Uuid::new_v4().to_string(), model.clone());

    let started = Instant::now();
    let outcome = match protocol {
        TestProtocol::ChatCompletions => {
            tokio::time::timeout(
                TEST_TIMEOUT,
                execute(picked.bridge.clone(), req, ctx, body.stream),
            )
            .await
        }
        TestProtocol::Anthropic => {
            // 同一来源、按 Anthropic 线格式重建桥（不改变默认 chat 行为）。
            let bridge = crate::bridge::anthropic::make_bridge(
                &picked.base_url,
                &picked.secret,
                picked.client.as_ref(),
            );
            tokio::time::timeout(TEST_TIMEOUT, execute(bridge, req, ctx, body.stream)).await
        }
        TestProtocol::Responses => {
            // Responses 透传：构造 Responses 形状请求体，非流式解析回包。
            let payload =
                responses_request(&picked.upstream_model, &prompt, system_text.as_deref());
            tokio::time::timeout(
                TEST_TIMEOUT,
                execute_responses(picked.bridge.clone(), payload, ctx),
            )
            .await
        }
    };
    let duration_ms = started.elapsed().as_millis() as u64;

    let source_json = json!({
        "kind": picked.kind,
        "channel_id": picked.channel_id,
        "name": picked.name,
        "account_id": picked.account_id,
    });

    let data = match outcome {
        Ok(Ok(out)) => {
            write_test_log(
                &state,
                &picked,
                &model,
                true,
                200,
                None,
                &out.usage,
                duration_ms,
            );
            state.usage_tracker.accumulate(
                out.usage.prompt_tokens,
                out.usage.completion_tokens,
                0,
                0,
                0,
                0.0,
            );
            json!({
                "ok": true,
                "reply": out.reply,
                "reasoning": out.reasoning,
                "upstream_model": out.upstream_model,
                "duration_ms": duration_ms,
                "ttfb_ms": out.ttfb_ms,
                "prompt_tokens": out.usage.prompt_tokens,
                "completion_tokens": out.usage.completion_tokens,
                "total_tokens": out.usage.total_tokens,
                "stream": body.stream,
                "protocol": protocol.as_str(),
                "source": source_json,
            })
        }
        Ok(Err(e)) => {
            let status = match &e {
                BridgeError::UpstreamStatus { status, .. } => *status,
                _ => 502,
            };
            let message = e.to_string();
            write_test_log(
                &state,
                &picked,
                &model,
                false,
                status,
                Some(&message),
                &UsageStats::default(),
                duration_ms,
            );
            json!({
                "ok": false,
                "error": message,
                "status": status,
                "duration_ms": duration_ms,
                "stream": body.stream,
                "protocol": protocol.as_str(),
                "source": source_json,
            })
        }
        Err(_) => {
            let message = format!("测试超时（{}s 未返回）", TEST_TIMEOUT.as_secs());
            write_test_log(
                &state,
                &picked,
                &model,
                false,
                504,
                Some(&message),
                &UsageStats::default(),
                duration_ms,
            );
            json!({
                "ok": false,
                "error": message,
                "status": 504,
                "duration_ms": duration_ms,
                "stream": body.stream,
                "protocol": protocol.as_str(),
                "source": source_json,
            })
        }
    };

    Ok(Json(json!({ "success": true, "data": data })))
}

/// 一次测试的结构化产出。
struct TestOutcome {
    reply: String,
    reasoning: String,
    upstream_model: String,
    usage: UsageStats,
    ttfb_ms: Option<u64>,
}

/// 流式：边收边累积，记录首字延迟；非流式：一次性读回。
async fn execute(
    bridge: Arc<dyn Bridge>,
    req: ChatFormat,
    ctx: BridgeContext,
    stream: bool,
) -> Result<TestOutcome, BridgeError> {
    if !stream {
        let started = Instant::now();
        let resp = bridge.chat(&req, &ctx).await?;
        return Ok(TestOutcome {
            reply: resp.message.content_str().to_string(),
            reasoning: resp.message.reasoning.clone().unwrap_or_default(),
            upstream_model: resp.model,
            usage: resp.usage,
            ttfb_ms: Some(started.elapsed().as_millis() as u64),
        });
    }

    let started = Instant::now();
    let mut s = bridge.chat_stream(&req, &ctx).await?;
    let mut reply = String::new();
    let mut reasoning = String::new();
    let mut ttfb_ms: Option<u64> = None;
    let mut usage = UsageStats::default();
    let mut upstream_model = req.model.clone();
    while let Some(item) = s.next().await {
        let chunk = item?;
        if !chunk.model.is_empty() {
            upstream_model = chunk.model.clone();
        }
        let has_delta = chunk.delta.content.is_some() || chunk.delta.reasoning.is_some();
        if ttfb_ms.is_none() && has_delta {
            ttfb_ms = Some(started.elapsed().as_millis() as u64);
        }
        if let Some(c) = chunk.delta.content {
            reply.push_str(&c);
        }
        if let Some(r) = chunk.delta.reasoning {
            reasoning.push_str(&r);
        }
        if let Some(u) = chunk.usage {
            usage = u;
        }
    }
    Ok(TestOutcome {
        reply,
        reasoning,
        upstream_model,
        usage,
        ttfb_ms,
    })
}

/// 解析测试出口并构造出口 Client。
///
/// - 空 → `Ok(None)`：沿用账号自身代理 / 渠道默认直连；
/// - `direct` → 强制直连（显式无代理）；
/// - `pool` / `pool:<id>` / `node:<id>` / 代理 URL → 经 `proxy_pool` 解析后建 client。
fn test_client_override(
    state: &AppState,
    raw: Option<&str>,
) -> Result<Option<Arc<reqwest::Client>>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if raw.eq_ignore_ascii_case("direct") {
        return state
            .account_sessions
            .egress()
            .client(None)
            .map(Some)
            .map_err(|e| format!("测试出口（直连）构造失败：{e}"));
    }
    let spec =
        normalize_proxy_spec(Some(raw)).expect("非空 raw 归一后必为 Some（direct 已在上方返回）");
    let resolved = crate::proxy_pool::resolve_proxy(Some(&spec), "model_test")
        .map_err(|e| format!("测试出口解析失败：{e}"))?;
    state
        .account_sessions
        .egress()
        .client(resolved.as_deref())
        .map(Some)
        .map_err(|e| format!("测试出口构造失败：{e}"))
}

/// 构造 OpenAI Responses 形状的最小请求体（system → `instructions`）。
fn responses_request(model: &str, prompt: &str, instructions: Option<&str>) -> Value {
    let mut body = json!({
        "model": model,
        "input": prompt,
        "stream": false,
    });
    if let Some(sys) = instructions {
        body["instructions"] = Value::String(sys.to_string());
    }
    body
}

/// Responses 透传执行：非流式 JSON 回包 → 统一 [`TestOutcome`]。
async fn execute_responses(
    bridge: Arc<dyn Bridge>,
    payload: Value,
    ctx: BridgeContext,
) -> Result<TestOutcome, BridgeError> {
    let started = Instant::now();
    match bridge.responses_passthrough(&payload, false, &ctx).await? {
        ResponsesPassthrough::Json(value) => Ok(responses_outcome(value, started.elapsed())),
        ResponsesPassthrough::Stream(_) => Err(BridgeError::Config(
            "responses 测试不支持流式回包（已请求非流式）".to_string(),
        )),
    }
}

/// 从 Responses JSON 回包提取回复 / 用量 / 上游模型名。
fn responses_outcome(value: Value, elapsed: Duration) -> TestOutcome {
    let reply = value
        .get("output_text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            let mut text = String::new();
            if let Some(items) = value.get("output").and_then(Value::as_array) {
                for item in items {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if let Some(t) = part.get("text").and_then(Value::as_str) {
                                text.push_str(t);
                            }
                        }
                    }
                }
            }
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_default();
    let mut usage = UsageStats::default();
    if let Some(u) = value.get("usage") {
        usage.prompt_tokens = u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
        usage.completion_tokens = u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
        usage.total_tokens = u
            .get("total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(usage.prompt_tokens + usage.completion_tokens);
    }
    TestOutcome {
        reply,
        reasoning: String::new(),
        upstream_model: value
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        usage,
        ttfb_ms: Some(elapsed.as_millis() as u64),
    }
}

/// 选来源：钉账号 > 钉渠道 > 自动（账号优先，其次渠道）。
fn pick_source(
    state: &AppState,
    provider: &str,
    model: &str,
    body: &ModelTestRequest,
    client_override: Option<Arc<reqwest::Client>>,
) -> Result<Picked, String> {
    // ① 钉账号
    if let Some(id) = body
        .account_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let session = state
            .account_sessions
            .get(id)
            .ok_or_else(|| format!("账号 {id} 不存在"))?;
        return pick_account(state, &session, provider, model, client_override);
    }

    // ② 钉渠道
    if let Some(id) = body
        .channel_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let channel = state
            .channel_store
            .get(id)
            .ok_or_else(|| format!("渠道 {id} 不存在"))?;
        // 账号型渠道：渠道本身无凭证，落到其下第一个匹配账号
        if channel.channel_type == ChannelType::Account {
            let session = state
                .account_sessions
                .for_channel(&channel.id)
                .into_iter()
                .find(|s| account_matches(s, provider, model));
            let Some(session) = session else {
                return Err(format!("渠道「{}」下没有可用账号", channel.name));
            };
            return pick_account(state, &session, provider, model, client_override);
        }
        return pick_channel(&channel, provider, model, client_override);
    }

    // ③ 自动：先账号后渠道（与视图行 sources 的展示序一致）
    let session = state
        .account_sessions
        .all()
        .into_iter()
        .find(|s| account_matches(s, provider, model));
    if let Some(session) = session {
        return pick_account(state, &session, provider, model, client_override);
    }
    let channel = state
        .channel_store
        .list()
        .into_iter()
        .find(|ch| channel_matches(ch, provider, model));
    if let Some(channel) = channel {
        return pick_channel(&channel, provider, model, client_override);
    }
    Err("该模型在此提供商下没有可用来源（启用账号或启用渠道）".to_string())
}

/// 账号是否可承载 `(provider, model)`：启用 + provider 匹配 + 白名单含该模型
/// （白名单为空 = 跟随渠道清单，视为可承载）。
fn account_matches(session: &AccountSession, provider: &str, model: &str) -> bool {
    if session.status != "enabled" {
        return false;
    }
    let sp = if session.provider.is_empty() {
        crate::account_session::registry::DEFAULT_PROVIDER_KIND
    } else {
        session.provider.as_str()
    };
    if sp != provider {
        return false;
    }
    session.models.is_empty()
        || session
            .models
            .iter()
            .any(|m| m.as_str().eq_ignore_ascii_case(model))
}

fn channel_matches(channel: &Channel, provider: &str, model: &str) -> bool {
    channel.is_enabled()
        && channel.channel_type != ChannelType::Account
        && channel.channel_type.as_str() == provider
        && channel
            .effective_models()
            .iter()
            .any(|m| m.as_str().eq_ignore_ascii_case(model))
}

fn pick_account(
    state: &AppState,
    session: &AccountSession,
    provider: &str,
    model: &str,
    client_override: Option<Arc<reqwest::Client>>,
) -> Result<Picked, String> {
    if !account_matches(session, provider, model) {
        return Err(format!(
            "账号「{}」不可用于该模型（未启用 / provider 不符 / 白名单不含）",
            if session.name.is_empty() {
                &session.id
            } else {
                &session.name
            }
        ));
    }
    let base_url =
        adapter::effective_base_url(session).map_err(|e| format!("账号链路构造失败：{e}"))?;
    let secret = if session.credential.kind == CREDENTIAL_KIND_NONE {
        String::new()
    } else {
        session.credential.secret_plain()
    };
    // 显式出口覆盖优先；缺省沿用账号自身代理。
    let client = match &client_override {
        Some(c) => c.clone(),
        None => state
            .account_sessions
            .client_for(session)
            .map_err(|e| format!("账号出口构造失败：{e}"))?,
    };
    let bridge =
        adapter::build_bridge_with_client(session, &state.account_sessions, client_override)
            .map_err(|e| format!("账号链路构造失败：{e}"))?;
    Ok(Picked {
        kind: "account",
        channel_id: session.channel_id.clone(),
        name: if session.name.is_empty() {
            session.id.clone()
        } else {
            session.name.clone()
        },
        account_id: Some(session.id.clone()),
        bridge,
        // 账号白名单里的名字即上游真名（选号层不做渠道映射）
        upstream_model: model.to_string(),
        base_url,
        secret,
        client,
    })
}

fn pick_channel(
    channel: &Channel,
    provider: &str,
    model: &str,
    client_override: Option<Arc<reqwest::Client>>,
) -> Result<Picked, String> {
    if !channel_matches(channel, provider, model) {
        return Err(format!(
            "渠道「{}」不可用于该模型（未启用 / 类型不符 / 模型不在白名单）",
            channel.name
        ));
    }
    let key = channel.decode_api_key();
    let http: Arc<reqwest::Client> = match client_override {
        Some(client) => client,
        None => Arc::new(
            reqwest::Client::builder()
                // 强制 HTTP/1.1：与共享 client 同口径（防 h2 PING 帧掩蔽读超时）
                .http1_only()
                .timeout(TEST_TIMEOUT)
                .build()
                .map_err(|e| format!("HTTP client error: {e}"))?,
        ),
    };
    let bridge: Arc<dyn Bridge> = match channel.channel_type {
        ChannelType::OpenaiCompatible => crate::bridge::openai::make_bridge_with_quirks(
            &channel.base_url,
            &key,
            http.as_ref(),
            channel.quirks.clone(),
        ),
        ChannelType::Anthropic => crate::bridge::anthropic::make_bridge_with_quirks(
            &channel.base_url,
            &key,
            http.as_ref(),
            channel.quirks.clone(),
        ),
        ChannelType::Gemini => {
            crate::bridge::gemini::make_bridge(&channel.base_url, &key, http.as_ref())
        }
        ChannelType::Zai => crate::bridge::zai::make_bridge_with_quirks(
            &channel.base_url,
            &key,
            http.as_ref(),
            channel.quirks.clone(),
        ),
        // Cloudflare 走 quota_monitor 专项；Account 已在上方分流到账号
        other => return Err(format!("渠道类型 {} 暂不支持对话测试", other.as_str())),
    };
    // 渠道级模型映射：上游真名 = 映射后的名字（与探活 / 数据面同口径）
    let upstream_model = channel
        .resolve_channel_mapping(model)
        .unwrap_or_else(|| model.to_string());
    Ok(Picked {
        kind: "channel",
        channel_id: channel.id.clone(),
        name: channel.name.clone(),
        account_id: None,
        bridge,
        upstream_model,
        base_url: channel.base_url.clone(),
        secret: key,
        client: http,
    })
}

/// 测试请求落日志（`key_id = "model_test"`）：管理员不计费，只留可追溯记录。
#[allow(clippy::too_many_arguments)]
fn write_test_log(
    state: &AppState,
    picked: &Picked,
    model: &str,
    ok: bool,
    status_code: u16,
    error: Option<&str>,
    usage: &UsageStats,
    latency_ms: u64,
) {
    let log = crate::log::RequestLog {
        purge_applied: false,
        special_route: None,
        id: uuid::Uuid::new_v4().to_string(),
        user_id: None,
        key_id: Some("model_test".to_string()),
        channel_id: Some(picked.channel_id.clone()),
        channel_name: Some(picked.name.clone()),
        model: model.to_string(),
        origin_model: Some(model.to_string()),
        input_tokens: usage.prompt_tokens,
        output_tokens: usage.completion_tokens,
        cost: 0,
        channel_cost: 0,
        latency_ms,
        status_code,
        error_msg: error.map(|e| e.chars().take(500).collect()),
        ip: None,
        client_name: String::new(),
        client_version: String::new(),
        request_id: None,
        created_at: chrono::Utc::now().timestamp(),
        candidate_channels: Vec::new(),
        cache_hit: false,
        conversation: None,
        filtered_channels: Vec::new(),
        selected_channel: None,
        debug: None,
        first_response_ms: None,
        attempts: 1,
        sensitive_hits: Vec::new(),
        phase: String::new(),
        route_config: None,
        route_truncated: 0,
        attempt_details: Vec::new(),
        attempt_details_truncated: 0,
        account_skips: Vec::new(),
    };
    if let Err(e) = state.log_store.requests.add(log) {
        tracing::warn!("model_test request log write failed: {e}");
    }
    crate::metrics::global().record_request(
        model,
        &picked.channel_id,
        if ok { "ok" } else { "error" },
        latency_ms,
    );
    if ok {
        crate::metrics::global().record_tokens(model, "prompt", usage.prompt_tokens);
        crate::metrics::global().record_tokens(model, "completion", usage.completion_tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_parse_defaults_to_chat_completions() {
        assert_eq!(
            TestProtocol::parse(None).unwrap(),
            TestProtocol::ChatCompletions
        );
        assert_eq!(
            TestProtocol::parse(Some("")).unwrap(),
            TestProtocol::ChatCompletions
        );
        assert_eq!(
            TestProtocol::parse(Some("chat_completions")).unwrap(),
            TestProtocol::ChatCompletions
        );
        assert_eq!(
            TestProtocol::parse(Some("responses")).unwrap(),
            TestProtocol::Responses
        );
        assert_eq!(
            TestProtocol::parse(Some("anthropic")).unwrap(),
            TestProtocol::Anthropic
        );
        assert!(TestProtocol::parse(Some("bogus")).is_err());
    }

    #[test]
    fn proxy_spec_normalization() {
        assert_eq!(normalize_proxy_spec(None), None);
        assert_eq!(normalize_proxy_spec(Some("  ")), None);
        assert_eq!(normalize_proxy_spec(Some("direct")), None);
        assert_eq!(
            normalize_proxy_spec(Some("pool")),
            Some("rotate".to_string())
        );
        assert_eq!(
            normalize_proxy_spec(Some("pool:abc")),
            Some("pool:abc".to_string())
        );
        assert_eq!(
            normalize_proxy_spec(Some("node:n1")),
            Some("pool:n1".to_string())
        );
        assert_eq!(
            normalize_proxy_spec(Some("http://127.0.0.1:8080")),
            Some("http://127.0.0.1:8080".to_string())
        );
    }

    #[test]
    fn responses_request_shape() {
        let body = responses_request("gpt-5", "hi", Some("be nice"));
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["input"], "hi");
        assert_eq!(body["stream"], false);
        assert_eq!(body["instructions"], "be nice");
        let no_sys = responses_request("m", "p", None);
        assert!(no_sys.get("instructions").is_none());
    }

    #[test]
    fn responses_outcome_extracts_text_and_usage() {
        let v = serde_json::json!({
            "model": "gpt-5",
            "output": [{ "content": [
                { "type": "output_text", "text": "hello " },
                { "type": "output_text", "text": "world" }
            ] }],
            "usage": { "input_tokens": 3, "output_tokens": 5, "total_tokens": 8 }
        });
        let out = responses_outcome(v, std::time::Duration::from_millis(12));
        assert_eq!(out.reply, "hello world");
        assert_eq!(out.upstream_model, "gpt-5");
        assert_eq!(out.usage.prompt_tokens, 3);
        assert_eq!(out.usage.completion_tokens, 5);
        assert_eq!(out.usage.total_tokens, 8);
    }
}
