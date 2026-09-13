//! 内嵌 MCP server — AI 可运维第②步（Model Context Protocol）。
//!
//! `POST /api/mcp` 收 JSON-RPC 2.0 请求，回 `application/json` 单响应
//! （MCP 2025-03-26 Streamable HTTP 的单响应模式）。第一版刻意不做：
//! SSE 流式响应、resources/prompts（只做 tools）、审批矩阵（第三步）。
//!
//! ## 协议子集（零依赖手写，不引 rmcp）
//! - `initialize`：握手 → protocolVersion + serverInfo + tools 能力声明
//! - `tools/list`：返回 11 个白名单工具（name/中文 description/inputSchema）
//! - `tools/call`：白名单校验后执行工具，结果 JSON 序列化为 text content
//!
//! ## fail-closed
//! - 未知 method → JSON-RPC -32601；未知工具名 → isError 响应，
//!   不透传到任何内部逻辑
//! - 工具失败消息经 `sanitize_error_message` 脱敏后才进 content
//!   （不把上游 key/Bearer token 泄给 MCP 客户端）
//!
//! ## 工具执行方式：进程内直调现有 handler（方案 A 变体，非 HTTP 自环）
//! 摸底结论：写 handler（patch_channel/test_channel/reset_channel_circuit）
//! 本身不记审计，"自环 = 审计天然获得"不成立；且管理面路由无逻辑中间件
//! （仅 CORS，浏览器安全语义），进程内直调与 HTTP 自环逻辑严格等价，
//! 但免去端口依赖、共享 reqwest client 超时叠加与双重序列化开销。
//! 直调时手动构造 extractor 参数（State/Query/Path/Json），verify_admin
//! 校验、副作用、错误构造全部复用 handler 原有实现。
//!
//! ## 写操作审计
//! 三个写 handler 均无 `record_audit` 调用，MCP 层为 4 个写工具各补一条
//! 审计（复用 `common::record_audit` → LogStore.audits，不新造表）：
//! admin_id=会话邮箱、action=`mcp_*`、target=`id=<channel_id>`、
//! before=操作前渠道 status、after=HTTP 状态码与成功与否。

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{admin_id_from_session, record_audit, verify_admin};
use super::logs::RequestLogQuery;
use super::users::ListUsersQuery;
use super::{
    handle_diagnostics_breakers, handle_diagnostics_channels, handle_diagnostics_summary,
    handle_list_channels, handle_list_pricing, handle_list_request_logs, handle_list_users,
    handle_patch_channel, handle_reset_channel_circuit, handle_test_channel,
};

/// MCP 协议版本（2025-03-26 Streamable HTTP）。
const PROTOCOL_VERSION: &str = "2025-03-26";

/// MCP server 名称。
const SERVER_NAME: &str = "aigx";

/// JSON-RPC 2.0 错误码。
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// 单个工具调用外层兜底超时（秒）。
///
/// 渠道探测内部已有自己的边界（diagnostics/channels 30s 总 deadline、
/// `ChannelStore::test` 15s reqwest 超时），60s 只作 MCP 外层兜底。
const TOOL_CALL_TIMEOUT_SECS: u64 = 60;

/// POST /api/mcp — MCP Streamable HTTP 端点（JSON-RPC 2.0 单响应）。
///
/// 鉴权与诊断端点一致：`Authorization: Bearer <管理会话 token>`
/// （`verify_admin`），鉴权失败返回 HTTP 401/403，不进入 JSON-RPC 层。
pub async fn handle_mcp(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;

    // envelope 不完整（非 JSON / 缺 jsonrpc / 缺 method）按 parse 失败 → -32700
    let Some((id, method, params)) = parse_request(&body) else {
        return Ok(Json(jsonrpc_error(
            Value::Null,
            PARSE_ERROR,
            "Parse error: body is not a valid JSON-RPC 2.0 request",
        )));
    };

    let resp = match method.as_str() {
        "initialize" => jsonrpc_ok(id, initialize_result()),
        "tools/list" => jsonrpc_ok(id, tools_list_result()),
        "tools/call" => match handle_tools_call(&state, &headers, &params).await {
            Ok(result) => jsonrpc_ok(id, result),
            Err((code, msg)) => jsonrpc_error(id, code, &msg),
        },
        other => jsonrpc_error(id, METHOD_NOT_FOUND, &format!("Method not found: {other}")),
    };
    Ok(Json(resp))
}

// ── JSON-RPC envelope ──────────────────────────────────────────────

/// 解析请求 envelope：`(id, method, params)`。
///
/// id 原样保留（string/number 均可，响应透传）；params 缺省为空对象。
/// envelope 不完整视为 parse 失败（-32700），比逐项 -32600 更简单且
/// 同样 fail-closed。
fn parse_request(body: &[u8]) -> Option<(Value, String, Value)> {
    let v: Value = serde_json::from_slice(body).ok()?;
    if v.get("jsonrpc").and_then(|j| j.as_str()) != Some("2.0") {
        return None;
    }
    let method = v.get("method")?.as_str()?.to_string();
    let params = v.get("params").cloned().unwrap_or_else(|| json!({}));
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    Some((id, method, params))
}

/// 成功响应：`{ jsonrpc, id, result }`。
fn jsonrpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// 失败响应：`{ jsonrpc, id, error: { code, message } }`。
fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// initialize 握手响应。
fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
        "capabilities": { "tools": { "listChanged": false } },
    })
}

// ── 工具注册表（显式白名单，绝不动态发现） ──────────────────────────

/// 白名单工具条目。
struct ToolSpec {
    name: &'static str,
    /// 中文一句话描述，写明只读/写操作
    description: &'static str,
    /// 简化 JSON Schema：type/properties/required
    schema: Value,
    /// 是否写操作（驱动 tools/list 的 readOnlyHint 标注）
    write: bool,
}

/// 第一版白名单工具集（11 个）。
///
/// inputSchema 严格按对应 handler 实际接受的参数写：
/// - `GET /api/channels` 无 enabled 过滤参数（只有 search/page/page_size），
///   故 schema 不虚构 enabled
/// - 请求日志端点实际为 `/api/logs/requests`（user/model/channel/start/end
///   过滤 + page/size 分页）
fn tool_specs() -> Vec<ToolSpec> {
    let no_args = || json!({ "type": "object", "properties": {}, "required": [] });
    let channel_id_args = || {
        json!({
            "type": "object",
            "properties": {
                "channel_id": { "type": "string", "description": "渠道 ID" }
            },
            "required": ["channel_id"]
        })
    };
    vec![
        // ── 诊断×3（只读转发第①步端点）──
        ToolSpec {
            name: "aigx_diagnostics_summary",
            description: "只读：系统体检摘要（渠道统计/今日用量/24h 错误概况/运行信息）",
            schema: no_args(),
            write: false,
        },
        ToolSpec {
            name: "aigx_diagnostics_channels",
            description: "只读：启用渠道批量测活（并发探测，只探测不落状态，约需 30 秒）",
            schema: no_args(),
            write: false,
        },
        ToolSpec {
            name: "aigx_diagnostics_breakers",
            description: "只读：熔断器状态快照（三态/失败计数/失败类型/冷却剩余）",
            schema: no_args(),
            write: false,
        },
        // ── 查询×4 ──
        ToolSpec {
            name: "aigx_list_channels",
            description: "只读：列出渠道（支持 search 模糊过滤与 page/page_size 分页）",
            schema: json!({
                "type": "object",
                "properties": {
                    "search": { "type": "string", "description": "按名称/类型/base_url/模型模糊过滤" },
                    "page": { "type": "integer", "minimum": 1, "description": "页码，默认 1" },
                    "page_size": { "type": "integer", "minimum": 1, "description": "每页条数，缺省 0=不分页" }
                },
                "required": []
            }),
            write: false,
        },
        ToolSpec {
            name: "aigx_list_users",
            description: "只读：列出用户（page/size 分页，密钥与敏感字段按管理员口径脱敏）",
            schema: json!({
                "type": "object",
                "properties": {
                    "page": { "type": "integer", "minimum": 1, "description": "页码，默认 1" },
                    "size": { "type": "integer", "minimum": 1, "description": "每页条数，默认 20" }
                },
                "required": []
            }),
            write: false,
        },
        ToolSpec {
            name: "aigx_query_logs",
            description: "只读：查询请求日志（user/model/channel/start/end 过滤 + page/size 分页）",
            schema: json!({
                "type": "object",
                "properties": {
                    "user": { "type": "string", "description": "用户 UUID 或邮箱" },
                    "model": { "type": "string", "description": "按模型名过滤" },
                    "channel": { "type": "string", "description": "按渠道 ID 过滤" },
                    "start": { "type": "integer", "description": "起始 unix 时间戳（秒）" },
                    "end": { "type": "integer", "description": "结束 unix 时间戳（秒）" },
                    "page": { "type": "integer", "minimum": 1, "description": "页码，默认 1" },
                    "size": { "type": "integer", "minimum": 1, "description": "每页条数，默认 20" }
                },
                "required": []
            }),
            write: false,
        },
        ToolSpec {
            name: "aigx_get_pricing",
            description: "只读：列出全部模型定价（单价/缓存价/计价类型）",
            schema: no_args(),
            write: false,
        },
        // ── 写×4（可恢复原则：均可用对向工具或管理台人工回滚）──
        ToolSpec {
            name: "aigx_channel_enable",
            description: "写操作（可回滚）：启用渠道（PATCH enabled=true，持久化渠道状态）",
            schema: channel_id_args(),
            write: true,
        },
        ToolSpec {
            name: "aigx_channel_disable",
            description: "写操作（可回滚）：禁用渠道（PATCH enabled=false，持久化渠道状态）",
            schema: channel_id_args(),
            write: true,
        },
        ToolSpec {
            name: "aigx_channel_test",
            description: "写操作（可回滚）：测试渠道连通性并落库测试结果（test_time/response_time），联动 mark_healthy/mark_unhealthy 状态",
            schema: channel_id_args(),
            write: true,
        },
        ToolSpec {
            name: "aigx_reset_circuit_breaker",
            description: "写操作（可回滚）：重置渠道熔断器与健康追踪状态（故障恢复后解除熔断用）",
            schema: channel_id_args(),
            write: true,
        },
    ]
}

/// 白名单查找（未知工具在分发前被此函数挡下，fail-closed）。
fn find_tool_spec(name: &str) -> Option<ToolSpec> {
    tool_specs().into_iter().find(|t| t.name == name)
}

/// tools/list 响应。
///
/// `annotations.readOnlyHint` 为 MCP 2025-03-26 规范的 ToolAnnotations
/// 字段：写工具标注 readOnlyHint=false，客户端（Claude Code 等）可据此
/// 在调用前向用户提示"该工具会改动系统"。
fn tools_list_result() -> Value {
    json!({
        "tools": tool_specs()
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": t.schema,
                    "annotations": { "readOnlyHint": !t.write }
                })
            })
            .collect::<Vec<_>>()
    })
}

// ── tools/call ─────────────────────────────────────────────────────

/// tools/call：白名单校验 + 参数校验 + 超时兜底。
///
/// 返回 `Ok(tools/call result)`（成功与 isError 两形态均算协议内正常响应）
/// 或 `Err((JSON-RPC code, message))`（参数缺失/类型错 → -32602，
/// 超时 → -32603）。
async fn handle_tools_call(
    state: &AppState,
    headers: &HeaderMap,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let Some(name) = params.get("name").and_then(|n| n.as_str()) else {
        return Err((
            INVALID_PARAMS,
            "Invalid params: missing tool name".to_string(),
        ));
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Err((
            INVALID_PARAMS,
            format!("Invalid params: arguments of tool {name} must be an object"),
        ));
    }

    // fail-closed：白名单外工具名直接拒绝，不触碰任何内部逻辑
    if find_tool_spec(name).is_none() {
        return Ok(unknown_tool_result(name));
    }

    let exec = exec_tool(state, headers, name, &arguments);
    match tokio::time::timeout(Duration::from_secs(TOOL_CALL_TIMEOUT_SECS), exec).await {
        Ok(result) => result,
        Err(_) => Err((
            INTERNAL_ERROR,
            format!("Tool {name} timed out after {TOOL_CALL_TIMEOUT_SECS}s"),
        )),
    }
}

/// 白名单分发执行（进程内直调现有 handler）。
async fn exec_tool(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    arguments: &Value,
) -> Result<Value, (i64, String)> {
    match name {
        // ── 诊断×3（只读）──
        "aigx_diagnostics_summary" => {
            let r = handle_diagnostics_summary(State(state.clone()), headers.clone()).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_diagnostics_channels" => {
            let r = handle_diagnostics_channels(State(state.clone()), headers.clone()).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_diagnostics_breakers" => {
            let r = handle_diagnostics_breakers(State(state.clone()), headers.clone()).await;
            Ok(handler_to_tool_result(r))
        }
        // ── 查询×4（只读）──
        "aigx_list_channels" => {
            let q = args_to_string_map(arguments);
            let r = handle_list_channels(State(state.clone()), headers.clone(), Query(q)).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_list_users" => {
            let q = parse_struct_args::<ListUsersQuery>(name, arguments)?;
            let r = handle_list_users(State(state.clone()), headers.clone(), Query(q)).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_query_logs" => {
            let q = parse_struct_args::<RequestLogQuery>(name, arguments)?;
            let r = handle_list_request_logs(State(state.clone()), headers.clone(), Query(q)).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_get_pricing" => {
            let r = handle_list_pricing(State(state.clone()), headers.clone()).await;
            Ok(handler_to_tool_result(r))
        }
        // ── 写×4（MCP 层补审计）──
        "aigx_channel_enable" => write_channel_toggle(state, headers, arguments, name, true).await,
        "aigx_channel_disable" => {
            write_channel_toggle(state, headers, arguments, name, false).await
        }
        "aigx_channel_test" => {
            let id = require_channel_id(arguments)?;
            let r =
                handle_test_channel(State(state.clone()), headers.clone(), Path(id.clone())).await;
            audit_write_tool(state, headers, name, &id, None, &r).await;
            Ok(handler_to_tool_result(r))
        }
        "aigx_reset_circuit_breaker" => {
            let id = require_channel_id(arguments)?;
            let r = handle_reset_channel_circuit(
                State(state.clone()),
                headers.clone(),
                Path(id.clone()),
            )
            .await;
            audit_write_tool(state, headers, name, &id, None, &r).await;
            Ok(handler_to_tool_result(r))
        }
        // find_tool_spec 已挡掉未知工具；此分支仅作 fail-closed 兜底
        _ => Ok(unknown_tool_result(name)),
    }
}

/// enable/disable 公共路径：PATCH `{"enabled": <bool>}` + 审计。
///
/// before 记录操作前渠道 status（渠道不存在时 None——此时 handler 返回
/// 404，审计照样留痕谁在何时试图经 MCP 动哪个渠道）。
async fn write_channel_toggle(
    state: &AppState,
    headers: &HeaderMap,
    arguments: &Value,
    tool_name: &str,
    enabled: bool,
) -> Result<Value, (i64, String)> {
    let id = require_channel_id(arguments)?;
    let before = state.channel_store.get(&id).map(|c| c.status.clone());
    let r = handle_patch_channel(
        State(state.clone()),
        headers.clone(),
        Path(id.clone()),
        Json(json!({ "enabled": enabled })),
    )
    .await;
    audit_write_tool(state, headers, tool_name, &id, before, &r).await;
    Ok(handler_to_tool_result(r))
}

/// handler 结果 → tools/call result。
///
/// 成功：数据 JSON 序列化为 text content；失败：`isError: true` + 错误
/// 消息经 `sanitize_error_message` 脱敏（截断 500 字符、抹掉 Bearer
/// token 与 sk- 密钥）后进 content，不透传上游密钥。
fn handler_to_tool_result(result: Result<Json<Value>, (StatusCode, Json<Value>)>) -> Value {
    match result {
        Ok(Json(v)) => text_content(v.to_string()),
        Err((_, Json(body))) => {
            let msg = body
                .get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| body.to_string());
            tool_error(crate::error_translate::sanitize_error_message(&msg))
        }
    }
}

/// tools/call 成功形态：`{ content: [{ type, text }] }`。
fn text_content(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ] })
}

/// tools/call 失败形态：content + `isError: true`。
fn tool_error(text: String) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": true })
}

/// 未知工具 fail-closed 响应（不透传任何内部信息，只回工具名本身）。
fn unknown_tool_result(name: &str) -> Value {
    tool_error(format!("Unknown tool: {name} (not in MCP whitelist)"))
}

// ── 参数辅助 ───────────────────────────────────────────────────────

/// 取必填 channel_id 参数（缺失/非字符串/空白 → -32602）。
pub(crate) fn require_channel_id(arguments: &Value) -> Result<String, (i64, String)> {
    arguments
        .get("channel_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            (
                INVALID_PARAMS,
                "Invalid params: missing required argument channel_id".to_string(),
            )
        })
}

/// JSON object → `HashMap<String, String>`（list_channels 的 Query 形状）。
///
/// 标量值转字符串（string 原样、number/bool 转 to_string），
/// 数组/对象忽略（该 handler 只接受标量查询参数）。
pub(crate) fn args_to_string_map(arguments: &Value) -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Some(obj) = arguments.as_object() {
        for (k, v) in obj {
            let s = match v {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            };
            if let Some(s) = s {
                m.insert(k.clone(), s);
            }
        }
    }
    m
}

/// 强类型参数解析（与对应 handler 的 Query 结构同源，缺省字段走 serde
/// default；类型不符 → -32602）。
pub(crate) fn parse_struct_args<T: DeserializeOwned>(
    tool: &str,
    arguments: &Value,
) -> Result<T, (i64, String)> {
    serde_json::from_value(arguments.clone()).map_err(|e| {
        (
            INVALID_PARAMS,
            format!("Invalid params for tool {tool}: {e}"),
        )
    })
}

// ── 写操作审计 ─────────────────────────────────────────────────────

/// 写工具审计：复用 `common::record_audit`（→ LogStore.audits）。
///
/// 记录五要素：谁（admin 会话邮箱）、何时（created_at）、经 MCP
/// （action 以 `mcp_` 前缀标记）、工具名与目标渠道（action + target）、
/// 参数摘要与结果状态码（target=id=<channel> / before=操作前 status /
/// after=HTTP 状态码与成功与否）。
async fn audit_write_tool(
    state: &AppState,
    headers: &HeaderMap,
    tool_name: &str,
    channel_id: &str,
    before_status: Option<String>,
    result: &Result<Json<Value>, (StatusCode, Json<Value>)>,
) {
    let admin_id = admin_id_from_session(state, headers).await;
    let (status, success) = match result {
        Ok(_) => (200, true),
        Err((code, _)) => (code.as_u16(), false),
    };
    // aigx_channel_enable → mcp_channel_enable（动作名带 mcp 前缀标记来源）
    let action = format!("mcp_{}", tool_name.trim_start_matches("aigx_"));
    let before = before_status.map(|s| json!({ "status": s }));
    record_audit(
        state,
        &admin_id,
        &action,
        &format!("id={channel_id}"),
        before,
        Some(json!({ "status": status, "success": success })),
    );
}

// ── 测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_passthrough_string_and_number_id() {
        // string id 透传 + 缺省 params
        let (id, method, params) =
            parse_request(br#"{"jsonrpc":"2.0","id":"call-1","method":"tools/list"}"#)
                .expect("valid envelope");
        assert_eq!(id, json!("call-1"));
        assert_eq!(method, "tools/list");
        assert_eq!(params, json!({}));
        // number id 透传 + params 原样保留
        let (id, _, params) = parse_request(
            br#"{"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":"x"}}"#,
        )
        .expect("valid envelope");
        assert_eq!(id, json!(42));
        assert_eq!(params["name"], "x");
    }

    #[test]
    fn parse_request_rejects_bad_envelope() {
        // 非 JSON
        assert!(parse_request(b"not json").is_none());
        // 缺 jsonrpc 字段
        assert!(parse_request(br#"{"id":1,"method":"initialize"}"#).is_none());
        // jsonrpc 版本不符
        assert!(parse_request(br#"{"jsonrpc":"1.0","method":"x","id":1}"#).is_none());
        // 缺 method
        assert!(parse_request(br#"{"jsonrpc":"2.0","id":1}"#).is_none());
    }

    #[test]
    fn jsonrpc_response_envelope_roundtrip() {
        let ok = jsonrpc_ok(json!(7), json!({ "data": [] }));
        assert_eq!(ok["jsonrpc"], "2.0");
        assert_eq!(ok["id"], 7);
        assert_eq!(ok["result"]["data"], json!([]));
        assert!(ok.get("error").is_none());

        let err = jsonrpc_error(json!("e1"), METHOD_NOT_FOUND, "Method not found: bogus");
        assert_eq!(err["id"], "e1");
        assert_eq!(err["error"]["code"], -32601);
        assert_eq!(err["error"]["message"], "Method not found: bogus");
        assert!(err.get("result").is_none());
    }

    #[test]
    fn initialize_handshake_shape() {
        let init = initialize_result();
        assert_eq!(init["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["serverInfo"]["name"], SERVER_NAME);
        assert!(
            init["serverInfo"]["version"]
                .as_str()
                .is_some_and(|v| !v.is_empty()),
            "serverInfo.version 应为 CARGO_PKG_VERSION"
        );
        assert_eq!(init["capabilities"]["tools"]["listChanged"], false);
    }

    #[test]
    fn tools_list_has_eleven_unique_tools() {
        let specs = tool_specs();
        assert_eq!(specs.len(), 11, "第一版白名单固定 11 个工具");
        let tools = tools_list_result()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), 11);
        let mut seen = std::collections::BTreeSet::new();
        for t in &tools {
            let name = t["name"].as_str().unwrap();
            assert!(seen.insert(name.to_string()), "工具名重复: {name}");
            assert!(
                t["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{name} 缺中文 description"
            );
            assert_eq!(t["inputSchema"]["type"], "object", "{name} schema.type");
            assert!(
                t["inputSchema"]["properties"].is_object(),
                "{name} 缺 properties"
            );
            // MCP 规范标注：readOnlyHint 必须为布尔
            assert!(
                t["annotations"]["readOnlyHint"].is_boolean(),
                "{name} 缺 annotations.readOnlyHint"
            );
        }
        // 每个描述标注读/写语义；写工具 required: ["channel_id"]
        let writes: Vec<&ToolSpec> = specs.iter().filter(|s| s.write).collect();
        assert_eq!(writes.len(), 4, "写工具应为 4 个");
        for s in &specs {
            let tag = if s.write { "写操作" } else { "只读" };
            assert!(s.description.contains(tag), "{} 未标注读/写语义", s.name);
            if s.write {
                assert_eq!(
                    s.schema["required"],
                    json!(["channel_id"]),
                    "{} 应必填 channel_id",
                    s.name
                );
            }
        }
        // 写/只读与 annotations.readOnlyHint 一致（写工具必须标注 false）
        for t in &tools {
            let spec = specs
                .iter()
                .find(|s| t["name"].as_str() == Some(s.name))
                .unwrap();
            assert_eq!(
                t["annotations"]["readOnlyHint"], !spec.write,
                "{} readOnlyHint 与 write 标志不一致",
                spec.name
            );
        }
    }

    #[test]
    fn unknown_tool_fail_closed() {
        // 白名单外工具不进入 exec_tool，直接 fail-closed
        assert!(find_tool_spec("aigx_delete_everything").is_none());
        let r = unknown_tool_result("aigx_delete_everything");
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["type"], "text");
        let msg = r["content"][0]["text"].as_str().unwrap();
        assert!(
            msg.contains("Unknown tool"),
            "未知工具响应应含 Unknown tool 前缀，实际: {msg}"
        );
        // 破坏性名称不透传到任何内部路径（响应里只出现名字本身）
        assert_eq!(r["content"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn protocol_error_codes_and_required_args() {
        // 未知 method → -32601
        assert_eq!(METHOD_NOT_FOUND, -32601);
        // parse 失败 → -32700
        assert_eq!(PARSE_ERROR, -32700);
        // 写工具缺 channel_id → -32602
        let (code, msg) = require_channel_id(&json!({})).unwrap_err();
        assert_eq!(code, INVALID_PARAMS);
        assert_eq!(code, -32602);
        assert!(msg.contains("channel_id"));
        // 空白字符串同样拒绝
        assert!(require_channel_id(&json!({ "channel_id": "  " })).is_err());
        // 正常取值 trim
        let id = require_channel_id(&json!({ "channel_id": " ch-1 " })).unwrap();
        assert_eq!(id, "ch-1");
        // 超时 → -32603
        assert_eq!(INTERNAL_ERROR, -32603);
    }

    /// 真实工具 → 数据层冒烟：aigx_list_channels 的存储层路径。
    ///
    /// 独立 tempdir（硬约束 6：并行测试不共用 SQLite 文件）构造真实
    /// ChannelStore：空库返回空列表；工具参数映射与 handler 的
    /// Query<HashMap<String,String>> 形状一致。
    #[test]
    fn smoke_list_channels_reaches_empty_store() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = crate::channel::ChannelStore::new(std::sync::Arc::new(
            crate::storage::FileStore::new(dir.path().to_path_buf()),
        ));
        // 空库 → list 返回空（工具的数据层路径端到端在库）
        assert!(store.list().is_empty());

        // 参数映射：string 原样、number 转 string、数组忽略
        let args = json!({ "search": "foo", "page": 2, "page_size": 10, "tags": [1, 2] });
        let q = args_to_string_map(&args);
        assert_eq!(q.get("search").map(|s| s.as_str()), Some("foo"));
        assert_eq!(q.get("page").map(|s| s.as_str()), Some("2"));
        assert_eq!(q.get("page_size").map(|s| s.as_str()), Some("10"));
        assert!(!q.contains_key("tags"));
    }

    /// aigx_query_logs 的参数与 handler 实际接受的 RequestLogQuery 同源：
    /// 命名字段正确解析、缺省走 serde default。
    #[test]
    fn query_logs_args_match_handler_query_shape() {
        let q: RequestLogQuery = parse_struct_args(
            "aigx_query_logs",
            &json!({ "user": "a@b.c", "model": "gpt-4", "start": 100, "end": 200 }),
        )
        .unwrap();
        assert_eq!(q.user.as_deref(), Some("a@b.c"));
        assert_eq!(q.model.as_deref(), Some("gpt-4"));
        assert_eq!(q.start, Some(100));
        assert_eq!(q.end, Some(200));
        // 空 arguments → 全部缺省（page=1/size=20，与 handler 默认一致）
        let q: RequestLogQuery = parse_struct_args("aigx_query_logs", &json!({})).unwrap();
        assert_eq!(q.page, 1);
        assert_eq!(q.size, 20);
        assert_eq!(q.user, None);
        // 类型不符 → -32602（start 须为整数）
        let err = parse_struct_args::<RequestLogQuery>(
            "aigx_query_logs",
            &json!({ "start": "not-a-number" }),
        )
        .unwrap_err();
        assert_eq!(err.0, INVALID_PARAMS);
    }
}
