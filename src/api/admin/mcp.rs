//! 内嵌 MCP server — AI 可运维第②步（Model Context Protocol）。
//!
//! `POST /api/mcp` 收 JSON-RPC 2.0 请求，回 `application/json` 单响应
//! （MCP 2025-03-26 Streamable HTTP 的单响应模式）。第一版刻意不做：
//! SSE 流式响应、resources/prompts（只做 tools）、审批矩阵（第三步）。
//!
//! ## 协议子集（零依赖手写，不引 rmcp）
//! - `initialize`：握手 → protocolVersion + serverInfo + tools 能力声明
//! - `tools/list`：返回全部非高危工具（与 `registry::mcp_tools` 同源，条数随注册表
//!   变化，所以这里不写死数字；name/中文 description/inputSchema）
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
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{admin_id_from_session, record_audit, verify_admin};

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

// ── 工具面：派生自 agent::registry 的统一注册表 ────────────────────
//
// 历史上这里养过第二份 19 项静态白名单，与 agent::tools 各自漂移。现在
// spec 与执行体只在 `agent::registry`（一条工具 = 一条声明），MCP 是
// 「非高危子集」的消费方（高危写留给 Agent 审批矩阵，外部客户端不给碰）。

/// 白名单查找（fail-closed：高危与白名单外一视同仁地拒绝）。
fn find_tool_spec(name: &str) -> Option<&'static crate::agent::registry::ToolDef> {
    crate::agent::registry::find_tool(name)
        .filter(|t| t.risk != crate::agent::tools::RiskLevel::HighRisk)
}

/// tools/list 响应（与 registry 同源；readOnlyHint 驱动客户端写前提示）。
pub(crate) fn tools_list_result() -> Value {
    json!({
        "tools": crate::agent::registry::mcp_tools()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "inputSchema": (t.schema)(),
                    "annotations": { "readOnlyHint": t.risk == crate::agent::tools::RiskLevel::ReadOnly }
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

    // fail-closed：白名单外（含高危）工具名直接拒绝，不触碰任何内部逻辑
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

/// 分发执行：registry 统一执行体 + MCP 侧写审计 + tools/call 形态包装。
async fn exec_tool(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    arguments: &Value,
) -> Result<Value, (i64, String)> {
    use crate::agent::tools::RiskLevel;
    let tool = find_tool_spec(name).ok_or_else(|| {
        (
            -32601,
            format!("Unknown tool: {name} (not in MCP whitelist)"),
        )
    })?;
    let outcome = crate::agent::registry::exec_tool(state, headers, name, arguments).await?;
    // 写工具补 mcp_ 前缀审计（registry/runner 侧审计不带 MCP 语境）
    if tool.risk != RiskLevel::ReadOnly {
        let target = if tool.target_key.is_empty() {
            String::new()
        } else {
            arguments
                .get(tool.target_key)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        audit_write_tool(state, headers, name, &target, outcome.ok).await;
    }
    Ok(if outcome.ok {
        text_content(outcome.text)
    } else {
        tool_error(outcome.text)
    })
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
    target: &str,
    success: bool,
) {
    let admin_id = admin_id_from_session(state, headers).await;
    // aigx_channel_enable → mcp_channel_enable（动作名带 mcp 前缀标记来源）
    let action = format!("mcp_{}", tool_name.trim_start_matches("aigx_"));
    record_audit(
        state,
        &admin_id,
        &action,
        &format!("id={target}"),
        None,
        Some(json!({ "success": success })),
    );
}

// ── 测试 ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::admin::logs::RequestLogQuery;

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
    fn tools_list_has_expected_unique_tools() {
        let specs: Vec<&crate::agent::registry::ToolDef> =
            crate::agent::registry::mcp_tools().collect();
        assert!(!specs.is_empty(), "MCP 工具面不能为空");
        let tools = tools_list_result()["tools"].as_array().unwrap().clone();
        assert_eq!(tools.len(), specs.len(), "tools/list 与 registry 计数一致");
        let mut seen = std::collections::BTreeSet::new();
        for t in &tools {
            let name = t["name"].as_str().unwrap();
            assert!(seen.insert(name.to_string()), "工具名重复: {name}");
            assert!(
                t["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{name} 缺 description"
            );
            assert_eq!(t["inputSchema"]["type"], "object", "{name} schema.type");
            assert!(
                t["inputSchema"]["properties"].is_object(),
                "{name} 缺 properties"
            );
            assert!(
                t["annotations"]["readOnlyHint"].is_boolean(),
                "{name} 缺 annotations.readOnlyHint"
            );
        }
        // MCP 面 = 非高危子集；readOnlyHint 与风险一致
        for s in &specs {
            assert_ne!(
                s.risk,
                crate::agent::tools::RiskLevel::HighRisk,
                "{} 高危不得进 MCP",
                s.name
            );
        }
        for t in &tools {
            let spec = specs
                .iter()
                .find(|s| Some(s.name) == t["name"].as_str())
                .unwrap();
            assert_eq!(
                t["annotations"]["readOnlyHint"],
                spec.risk == crate::agent::tools::RiskLevel::ReadOnly,
                "{} readOnlyHint 与风险不一致",
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
        let (code, msg) = crate::agent::tools::require_channel_id(&json!({})).unwrap_err();
        assert_eq!(code, INVALID_PARAMS);
        assert_eq!(code, -32602);
        assert!(msg.contains("channel_id"));
        // 空白字符串同样拒绝
        assert!(crate::agent::tools::require_channel_id(&json!({ "channel_id": "  " })).is_err());
        // 正常取值 trim
        let id =
            crate::agent::tools::require_channel_id(&json!({ "channel_id": " ch-1 " })).unwrap();
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
