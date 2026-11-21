// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! MCP 端点的 JSON-RPC 骨架（Streamable HTTP，单端点 POST，dual-era）。
//!
//! 两个服务（网络搜索 / 视觉分析）共用同一套分发：initialize 走 legacy 握手，
//! `server/discover` 与直接调用走 modern（无握手、无会话）。端点本身无状态，
//! 两代语义可以共存而不需要维护会话。
//!
//! ── 与上游的差异 ────────────────────────────────────────────
//! 三个服务（web-search / vision / image-generation）均已移植：图片生成的
//! 任务存储、文件面与下载端点见 `image_jobs` / `image_runtime` 以及
//! `api::tool_runtime` 的 `/mcp/image-generation{,/files/{id}}`。与本文件
//! JSON-RPC 收发语义相关的差异只剩鉴权模型（AIGX 用网关 Key id 作为任务
//! owner，上游用 Key 哈希；见 `image_runtime::owner_of`）与模型能力位
//! （AIGX 用 `image` 标签，无独立的编辑能力位）。
//!
//! 协议细节与上游一致：
//!   · 通知（`notifications/*` 或没有 id）不产生响应；
//!   · 批量数组（2025-03-26）与单条消息都接；
//!   · 参数不合法 → JSON-RPC `INVALID_PARAMS`；业务失败 → `isError` 结果，
//!     让模型有机会自我纠正。

use serde_json::{json, Value};

use super::search::batch_items;
use crate::account_session::AccountSessionStore;
use crate::api::auth::ApiKeyStore;

use super::ToolServices;

pub const MCP_MODERN_VERSION: &str = "2026-07-28";
pub const MCP_LEGACY_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const MCP_LIST_TTL_MS: u64 = 3_600_000;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum McpServerKind {
    WebSearch,
    Vision,
    ImageGeneration,
}

/// 一次 MCP 请求的上下文（由 `api::tool_runtime` 组装）。
pub struct McpRequestContext<'a> {
    /// 工具服务配置/密钥存储
    pub services: &'a ToolServices,
    pub sessions: &'a AccountSessionStore,
    pub keys: &'a ApiKeyStore,
    /// 抓取/回环共用客户端（禁止重定向，见 tool_runtime 的构造）
    pub client: &'a reqwest::Client,
    /// 网关基址（视觉分析内部调用用）
    pub base_url: String,
    /// 网关 Key 的 provider 白名单（`None` = 不限）
    pub allowed_providers: Option<Vec<String>>,
    /// 网关 Key 的模型白名单（图片任务鉴权用；`None`/空 = 不限）
    pub allowed_models: Option<Vec<String>>,
    /// 调用者标识（日志/审计用；图片任务作为 owner 参与归属鉴权）
    pub owner: String,
    /// 图片生成运行时所需的应用状态（web-search / vision 不使用）。
    ///
    /// `Option` 只是为了单测能在没有完整 `AppState` 时构造上下文；生产路径
    /// 始终是 `Some`，图片生成分支在 `None` 时返回可读的 503 业务错误。
    pub state: Option<&'a crate::api::openai::AppState>,
}

pub struct McpSpec {
    pub server_info: Value,
    pub instructions: &'static str,
    pub tools: Vec<Value>,
}

pub fn web_search_spec() -> McpSpec {
    McpSpec {
        server_info: json!({
            "name": super::WEB_SEARCH_PACKAGE,
            "title": "Web Search",
            "version": super::WEB_SEARCH_VERSION,
        }),
        instructions: "Batched web search and page fetching. Upstream providers are chosen by the AIGX administrator; callers only pass queries or urls.",
        tools: vec![
            json!({
                "name": "web_search",
                "title": "Batched web search",
                "description": "Search 1-10 queries in parallel using the providers selected by the administrator, then merge duplicate sources across queries.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "description": "Queries to search, each executed in parallel (1-10).",
                            "items": { "type": "string", "minLength": 1 },
                            "minItems": 1,
                            "maxItems": 10
                        }
                    },
                    "required": ["queries"],
                    "additionalProperties": false
                }
            }),
            json!({
                "name": "web_fetch",
                "title": "Batched page fetching",
                "description": "Fetch and extract the readable content of 1-10 public http/https URLs using the single provider selected by the administrator.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "urls": {
                            "type": "array",
                            "description": "Page URLs to fetch and extract (1-10).",
                            "items": { "type": "string", "minLength": 1 },
                            "minItems": 1,
                            "maxItems": 10
                        }
                    },
                    "required": ["urls"],
                    "additionalProperties": false
                }
            }),
        ],
    }
}

pub fn vision_spec() -> McpSpec {
    McpSpec {
        server_info: json!({
            "name": super::VISION_PACKAGE,
            "title": "Vision Analysis",
            "version": super::VISION_VERSION,
        }),
        instructions: "Analyze images with a vision model selected by the AIGX administrator. Pass image URLs or data URIs together with a prompt; the model reply is returned as text.",
        tools: vec![json!({
            "name": "image_analyze",
            "title": "Image analysis",
            "description": "Analyze 1-10 images with the vision model selected by the administrator. Each image is an http(s) URL or an image data URI; the prompt describes what to look for.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "images": {
                        "type": "array",
                        "description": "Images to analyze (1-10), each an http(s) URL or an image data URI.",
                        "items": { "type": "string", "minLength": 1 },
                        "minItems": 1,
                        "maxItems": 10
                    },
                    "prompt": {
                        "type": "string",
                        "description": "What to look for in the images; the model reply is returned as text.",
                        "minLength": 1
                    }
                },
                "required": ["images", "prompt"],
                "additionalProperties": false
            }
        })],
    }
}

pub fn spec(kind: McpServerKind, services: &ToolServices) -> McpSpec {
    match kind {
        McpServerKind::WebSearch => web_search_spec(),
        McpServerKind::Vision => vision_spec(),
        McpServerKind::ImageGeneration => super::image_mcp::spec(services),
    }
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message.into() } })
}

fn tool_result(payload: Value) -> Value {
    json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&payload).unwrap_or_default() }],
        "structuredContent": payload,
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "resultType": "complete",
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
    })
}

fn negotiate_version(requested: Option<&str>) -> String {
    if let Some(value) = requested {
        if value == MCP_MODERN_VERSION || MCP_LEGACY_VERSIONS.contains(&value) {
            return value.to_string();
        }
    }
    MCP_LEGACY_VERSIONS[0].to_string()
}

/// `tools/list` / `server/discover` 的缓存元信息（与上游逐字段一致）：
/// 图片生成按 Key 作用域裁剪工具集，不能公开缓存，TTL 归零、作用域私有；
/// web-search / vision 对所有调用者形状相同，可公开缓存。
fn list_meta(kind: McpServerKind) -> (u64, &'static str) {
    if kind == McpServerKind::ImageGeneration {
        (0, "private")
    } else {
        (MCP_LIST_TTL_MS, "public")
    }
}

async fn call_tool(
    kind: McpServerKind,
    name: &str,
    args: &Value,
    ctx: &McpRequestContext<'_>,
) -> Result<Value, ToolCallError> {
    match kind {
        McpServerKind::WebSearch => {
            let operation = if name == "web_search" {
                "search"
            } else if name == "web_fetch" {
                "fetch"
            } else {
                return Err(ToolCallError::InvalidParams(format!(
                    "Unknown tool: {name}"
                )));
            };
            let field = if operation == "search" {
                "queries"
            } else {
                "urls"
            };
            let items = batch_items(args.get(field), field)
                .map_err(|error| ToolCallError::InvalidParams(error.message))?;
            let started = super::now_ms();
            let outcome = if operation == "search" {
                super::run_search(
                    ctx.services,
                    ctx.sessions,
                    ctx.client,
                    ctx.allowed_providers.as_deref(),
                    items.clone(),
                )
                .await
                .map(|run| (run.payload, run.successful))
            } else {
                super::run_fetch(ctx.services, ctx.sessions, ctx.client, items.clone())
                    .await
                    .map(|run| (run.payload, run.successful))
            };
            let duration = super::now_ms() - started;
            match outcome {
                Ok((payload, successful)) => {
                    tracing::debug!(
                        "[Tools] MCP web-search {operation} ({}): {} 条输入 / {successful} 条成功 / {duration}ms",
                        ctx.owner,
                        items.len()
                    );
                    if successful == 0 {
                        let mut result = tool_error(format!("All {operation} items failed"));
                        result["structuredContent"] = payload;
                        return Ok(result);
                    }
                    Ok(tool_result(payload))
                }
                Err(error) => {
                    tracing::debug!(
                        "[Tools] MCP web-search {operation} 失败（{}）：{}",
                        ctx.owner,
                        error.message
                    );
                    Ok(tool_error(error.message))
                }
            }
        }
        McpServerKind::Vision => {
            if name != "image_analyze" {
                return Err(ToolCallError::InvalidParams(format!(
                    "Unknown tool: {name}"
                )));
            }
            let (images, prompt) =
                super::vision::parse_vision_args(args).map_err(ToolCallError::InvalidParams)?;
            match super::run_vision(
                ctx.services,
                ctx.keys,
                ctx.client,
                &ctx.base_url,
                images,
                prompt,
            )
            .await
            {
                Ok(payload) => Ok(tool_result(payload)),
                Err(error) => Ok(tool_error(error)),
            }
        }
        McpServerKind::ImageGeneration => match super::image_runtime::call(name, args, ctx).await {
            Ok(result) => Ok(result),
            Err(error) => Ok(super::image_runtime::error_result(&error, args)),
        },
    }
}

enum ToolCallError {
    InvalidParams(String),
}

/// 处理单条 JSON-RPC 请求；通知类消息返回 None。
async fn dispatch_one(
    message: &Value,
    kind: McpServerKind,
    ctx: &McpRequestContext<'_>,
) -> Option<Value> {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(rpc_error(
            Value::Null,
            INVALID_REQUEST,
            "Invalid JSON-RPC 2.0 request",
        ));
    }
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(
            Value::Null,
            INVALID_REQUEST,
            "Invalid JSON-RPC 2.0 request",
        ));
    };
    if method.starts_with("notifications/") {
        return None;
    }
    // 上游同一口径：图片生成的请求 id 必须是字符串或数字。图片任务按 id
    // 幂等/查询，`null` 或对象等非法 id 无法与后续 `get_image_result` 对上，
    // 直接按 INVALID_REQUEST 拒绝，避免产生无法追踪的任务。
    if kind == McpServerKind::ImageGeneration
        && message.get("id").is_some()
        && !matches!(message.get("id"), Some(Value::String(_) | Value::Number(_)))
    {
        return Some(rpc_error(
            Value::Null,
            INVALID_REQUEST,
            "A string or number request ID is required",
        ));
    }
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        return None;
    }
    let spec = spec(kind, ctx.services);
    let params = message.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => {
            let requested = params.get("protocolVersion").and_then(Value::as_str);
            Some(rpc_result(
                id,
                json!({
                    "protocolVersion": negotiate_version(requested),
                    "capabilities": { "tools": {} },
                    "serverInfo": spec.server_info,
                    "instructions": spec.instructions,
                }),
            ))
        }
        "server/discover" => {
            let (ttl_ms, cache_scope) = list_meta(kind);
            Some(rpc_result(
                id,
                json!({
                    "resultType": "complete",
                    "ttlMs": ttl_ms,
                    "cacheScope": cache_scope,
                    "supportedVersions": [
                        MCP_MODERN_VERSION,
                        MCP_LEGACY_VERSIONS[0],
                        MCP_LEGACY_VERSIONS[1],
                        MCP_LEGACY_VERSIONS[2],
                        MCP_LEGACY_VERSIONS[3]
                    ],
                    "capabilities": { "tools": {} },
                    "_meta": { "io.modelcontextprotocol/serverInfo": spec.server_info },
                    "instructions": spec.instructions,
                }),
            ))
        }
        "ping" => Some(rpc_result(id, json!({}))),
        "tools/list" => {
            let (ttl_ms, cache_scope) = list_meta(kind);
            Some(rpc_result(
                id,
                json!({
                    "resultType": "complete",
                    "ttlMs": ttl_ms,
                    "cacheScope": cache_scope,
                    "tools": spec.tools,
                }),
            ))
        }
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            // 与上游一致：`edit_images` 即使未在 `tools/list` 中出现（管理员
            // 未启用编辑）也放行到运行时，返回「尚未启用参考图编辑」的
            // business error（isError 结果），而不是协议的 INVALID_PARAMS。
            let passthrough_edit = kind == McpServerKind::ImageGeneration && name == "edit_images";
            if !passthrough_edit
                && !spec
                    .tools
                    .iter()
                    .any(|tool| tool.get("name").and_then(Value::as_str) == Some(name))
            {
                return Some(rpc_error(
                    id,
                    INVALID_PARAMS,
                    format!("Unknown tool: {name}"),
                ));
            }
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(kind, name, &args, ctx).await {
                Ok(result) => Some(rpc_result(id, result)),
                Err(ToolCallError::InvalidParams(message)) => {
                    Some(rpc_error(id, INVALID_PARAMS, message))
                }
            }
        }
        _ => Some(rpc_error(
            id,
            METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        )),
    }
}

/// 处理一个请求体（单条或批量），返回响应体；纯通知返回 None。
pub async fn dispatch(
    body: &Value,
    kind: McpServerKind,
    ctx: &McpRequestContext<'_>,
) -> Option<Value> {
    if body.is_null() || (!body.is_object() && !body.is_array()) {
        return Some(rpc_error(
            Value::Null,
            PARSE_ERROR,
            "Request body must be a JSON-RPC 2.0 message",
        ));
    }
    let messages: Vec<&Value> = match body {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let mut responses = Vec::new();
    for message in messages {
        if let Some(response) = dispatch_one(message, kind, ctx).await {
            responses.push(response);
        }
    }
    if responses.is_empty() {
        return None;
    }
    if body.is_array() {
        Some(Value::Array(responses))
    } else {
        Some(responses.remove(0))
    }
}

/// 非 POST 方法：按规范用 405 明确告知（本类服务不提供服务端主动推流）。
pub async fn unsupported() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        [(axum::http::header::ALLOW, "POST")],
        axum::Json(json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32600, "message": "Only POST is supported on this MCP endpoint" }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_expose_expected_tools() {
        let search = web_search_spec();
        assert_eq!(search.tools.len(), 2);
        assert_eq!(search.tools[0]["name"], "web_search");
        assert_eq!(search.tools[1]["name"], "web_fetch");
        let vision = vision_spec();
        assert_eq!(vision.tools.len(), 1);
        assert_eq!(vision.tools[0]["name"], "image_analyze");
    }

    #[test]
    fn version_negotiation() {
        assert_eq!(
            negotiate_version(Some(MCP_MODERN_VERSION)),
            MCP_MODERN_VERSION
        );
        assert_eq!(negotiate_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(
            negotiate_version(Some("1999-01-01")),
            MCP_LEGACY_VERSIONS[0]
        );
        assert_eq!(negotiate_version(None), MCP_LEGACY_VERSIONS[0]);
    }

    // ── 分发层（无需完整 AppState：`state: None` 时仅图片执行分支不可用）──

    use crate::account_session::AccountSessionStore;
    use crate::api::auth::ApiKeyStore;
    use crate::storage::FileStore;
    use crate::tool_services::image_config::{ImageConfig, ImageTarget};
    use std::sync::Arc;

    struct Harness {
        services: ToolServices,
        sessions: AccountSessionStore,
        keys: ApiKeyStore,
        client: reqwest::Client,
        _dir: tempfile::TempDir,
    }

    impl Harness {
        fn new() -> Self {
            let dir = tempfile::TempDir::new().unwrap();
            let store = Arc::new(FileStore::new(dir.path().to_path_buf()));
            Self {
                services: ToolServices::new(store.clone()),
                sessions: AccountSessionStore::new(store.clone()),
                keys: ApiKeyStore::new(store, dir.path().to_path_buf()),
                client: crate::test_http_client(),
                _dir: dir,
            }
        }

        fn with_image_config(editing: bool) -> Self {
            let harness = Self::new();
            let target = ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            };
            let config = ImageConfig {
                generation: target.clone(),
                editing: editing.then_some(target),
                max_images: 2,
                max_concurrency: 2,
                retention_hours: 24,
                storage_mb: 512,
            };
            harness
                .services
                .update(|value| value.image_generation = Some(config))
                .unwrap();
            harness
        }

        fn ctx(&self) -> McpRequestContext<'_> {
            McpRequestContext {
                services: &self.services,
                sessions: &self.sessions,
                keys: &self.keys,
                client: &self.client,
                base_url: "http://127.0.0.1".into(),
                allowed_providers: None,
                allowed_models: None,
                owner: "key-1".into(),
                state: None,
            }
        }
    }

    fn rpc(method: &str, id: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method })
    }

    fn rpc_call(id: Value, name: &str, arguments: Value) -> Value {
        json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
    }

    #[tokio::test]
    async fn handshake_ping_and_notifications() {
        let harness = Harness::new();
        let ctx = harness.ctx();

        let init = dispatch(
            &json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": MCP_MODERN_VERSION }
            }),
            McpServerKind::WebSearch,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], MCP_MODERN_VERSION);
        assert_eq!(
            init["result"]["serverInfo"]["name"],
            super::super::WEB_SEARCH_PACKAGE
        );

        let ping = dispatch(&rpc("ping", json!(2)), McpServerKind::Vision, &ctx)
            .await
            .unwrap();
        assert_eq!(ping["result"], json!({}));

        // 通知（notifications/* 或无 id）不产生响应
        assert!(dispatch(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            McpServerKind::WebSearch,
            &ctx,
        )
        .await
        .is_none());
        assert!(
            dispatch(&rpc("ping", Value::Null), McpServerKind::WebSearch, &ctx)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn invalid_requests_and_unknown_methods() {
        let harness = Harness::new();
        let ctx = harness.ctx();

        let bad_version = dispatch(
            &json!({ "jsonrpc": "1.0", "id": 1, "method": "ping" }),
            McpServerKind::WebSearch,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(bad_version["error"]["code"], INVALID_REQUEST);

        let unknown = dispatch(&rpc("tools/nope", json!(3)), McpServerKind::WebSearch, &ctx)
            .await
            .unwrap();
        assert_eq!(unknown["error"]["code"], METHOD_NOT_FOUND);

        // 批量：notification 被跳过，只有 ping 一条响应
        let batch = dispatch(
            &json!([
                { "jsonrpc": "2.0", "method": "notifications/progress" },
                { "jsonrpc": "2.0", "id": 9, "method": "ping" }
            ]),
            McpServerKind::Vision,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(batch.as_array().unwrap().len(), 1);
        assert_eq!(batch[0]["id"], 9);
    }

    #[tokio::test]
    async fn tools_list_shapes_and_cache_scope() {
        let harness = Harness::with_image_config(false);
        let ctx = harness.ctx();

        let search = dispatch(&rpc("tools/list", json!(1)), McpServerKind::WebSearch, &ctx)
            .await
            .unwrap();
        let names: Vec<&str> = search["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["web_search", "web_fetch"]);
        assert_eq!(search["result"]["cacheScope"], "public");
        assert_eq!(search["result"]["ttlMs"], MCP_LIST_TTL_MS);

        // 编辑未启用：image-generation 只有 generate + get，且按私有/零 TTL 缓存
        let image = dispatch(
            &rpc("tools/list", json!(2)),
            McpServerKind::ImageGeneration,
            &ctx,
        )
        .await
        .unwrap();
        let names: Vec<&str> = image["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["generate_images", "get_image_result"]);
        assert_eq!(image["result"]["cacheScope"], "private");
        assert_eq!(image["result"]["ttlMs"], 0);
    }

    #[tokio::test]
    async fn tools_call_unknown_tool_is_invalid_params() {
        let harness = Harness::with_image_config(false);
        let ctx = harness.ctx();
        for kind in [
            McpServerKind::WebSearch,
            McpServerKind::Vision,
            McpServerKind::ImageGeneration,
        ] {
            let response = dispatch(&rpc_call(json!(1), "no_such_tool", json!({})), kind, &ctx)
                .await
                .unwrap();
            assert_eq!(response["error"]["code"], INVALID_PARAMS, "kind={kind:?}");
        }
    }

    #[tokio::test]
    async fn web_search_and_vision_call_paths() {
        let harness = Harness::new();
        let ctx = harness.ctx();

        // 参数不合法（协议级错误）
        let empty = dispatch(
            &rpc_call(json!(1), "web_search", json!({ "queries": [] })),
            McpServerKind::WebSearch,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(empty["error"]["code"], INVALID_PARAMS);

        // 合法参数但没有任何已配置来源 → 业务失败以 isError 结果返回（非协议错）
        let no_provider = dispatch(
            &rpc_call(json!(2), "web_search", json!({ "queries": ["rust"] })),
            McpServerKind::WebSearch,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(no_provider["result"]["isError"], true);

        // 视觉：参数不合法 → INVALID_PARAMS；合法但未配置 → isError
        let bad_args = dispatch(
            &rpc_call(
                json!(3),
                "image_analyze",
                json!({ "images": ["file:///etc/passwd"], "prompt": "x" }),
            ),
            McpServerKind::Vision,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(bad_args["error"]["code"], INVALID_PARAMS);

        let unconfigured = dispatch(
            &rpc_call(
                json!(4),
                "image_analyze",
                json!({ "images": ["https://example.com/a.png"], "prompt": "describe" }),
            ),
            McpServerKind::Vision,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(unconfigured["result"]["isError"], true);
    }

    #[tokio::test]
    async fn image_call_paths_and_edit_passthrough() {
        let harness = Harness::with_image_config(false);
        let ctx = harness.ctx();

        // 请求 id 类型：图片生成只接受字符串/数字（上游口径）
        let bad_id = dispatch(
            &json!({ "jsonrpc": "2.0", "id": { "nested": true }, "method": "ping" }),
            McpServerKind::ImageGeneration,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(bad_id["error"]["code"], INVALID_REQUEST);

        // request_id 形状不合法 → 业务错误结果
        let bad_request = dispatch(
            &rpc_call(
                json!(1),
                "generate_images",
                json!({ "request_id": "short", "prompt": "a cat" }),
            ),
            McpServerKind::ImageGeneration,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(bad_request["result"]["isError"], true);
        assert_eq!(
            bad_request["result"]["structuredContent"]["code"],
            "invalid_image_input"
        );

        // 编辑未启用：tools/list 未列出，但 tools/call 仍放行并返回业务错误
        let edit = dispatch(
            &rpc_call(
                json!(2),
                "edit_images",
                json!({
                    "request_id": "request-0001",
                    "prompt": "add a hat",
                    "images": ["https://example.com/a.png"]
                }),
            ),
            McpServerKind::ImageGeneration,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(edit["result"]["isError"], true);
        assert_eq!(
            edit["result"]["structuredContent"]["code"],
            "image_configuration"
        );

        // 查询不存在的任务 → 业务错误结果
        let missing = dispatch(
            &rpc_call(
                json!(3),
                "get_image_result",
                json!({ "job_id": "0".repeat(64) }),
            ),
            McpServerKind::ImageGeneration,
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(missing["result"]["isError"], true);
        assert_eq!(
            missing["result"]["structuredContent"]["code"],
            "image_job_not_found"
        );
    }
}
