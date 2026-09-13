//! Agent 工具注册表——管理面 API 的"手"。
//!
//! 三层风险分级（安全底线）：
//! - [`RiskLevel::ReadOnly`]：查/搜/统计/诊断，Agent 自由调用，无审批。
//! - [`RiskLevel::LowRisk`]：低危写（启停/测活/重置熔断/加密钥/加渠道），
//!   Agent 直接做，自动审计留痕，可用对向工具回滚。
//! - [`RiskLevel::HighRisk`]：高危写（删用户/动余额/删订单/改价/清日志），
//!   必须经审批矩阵（[`crate::agent::approval`]）人工确认，阶段二接入。
//!
//! 执行方式：进程内直调现有管理面 handler（与 [`crate::api::admin::mcp`]
//! 同源思路——复用 verify_admin 校验、副作用、错误构造，不经 HTTP 端口）。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use crate::api::admin::{
    self, handle_list_accounts, handle_list_channels, handle_list_groups, handle_list_keys,
    handle_list_orders, handle_list_plans, handle_list_pricing, handle_list_redemptions,
    handle_list_request_logs, handle_list_users,
};
use crate::api::openai::AppState;

/// 工具风险等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// 只读（查/搜/统计/诊断）。
    ReadOnly,
    /// 低危写（可回滚，自动审计）。
    LowRisk,
    /// 高危写（须审批矩阵人工确认）。
    HighRisk,
}

impl RiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskLevel::ReadOnly => "readonly",
            RiskLevel::LowRisk => "low",
            RiskLevel::HighRisk => "high",
        }
    }
}

/// 工具条目元数据。
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub risk: RiskLevel,
}

/// 全部工具注册表（阶段一先落只读层 + 低危写；高危写阶段二补齐）。
pub fn tool_specs() -> Vec<ToolSpec> {
    let no_args = || json!({ "type": "object", "properties": {}, "required": [] });
    let paged = || {
        json!({
            "type": "object",
            "properties": {
                "page": { "type": "integer", "minimum": 1, "description": "页码，默认 1" },
                "size": { "type": "integer", "minimum": 1, "description": "每页条数，默认 20" }
            },
            "required": []
        })
    };
    vec![
        // ── 诊断×3 ──
        ToolSpec { name: "aigx_diagnostics_summary", description: "只读：系统体检摘要（渠道统计/今日用量/24h 错误概况/运行信息）", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_diagnostics_channels", description: "只读：启用渠道批量测活（并发探测，约需 30 秒）", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_diagnostics_breakers", description: "只读：熔断器状态快照（三态/失败计数/冷却剩余）", schema: no_args(), risk: RiskLevel::ReadOnly },
        // ── 查询×11 ──
        ToolSpec { name: "aigx_list_channels", description: "只读：列出渠道（search 模糊过滤 + page/page_size 分页）", schema: json!({ "type":"object","properties":{ "search":{"type":"string","description":"按名称/类型/base_url/模型模糊过滤"}, "page":{"type":"integer","minimum":1}, "page_size":{"type":"integer","minimum":1} }, "required":[] }), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_users", description: "只读：列出用户（分页，敏感字段脱敏）", schema: paged(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_keys", description: "只读：列出 API 密钥（脱敏）", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_accounts", description: "只读：列出 CF 账号池（脱敏）", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_orders", description: "只读：列出订单（keyword 过滤 + 分页）", schema: json!({ "type":"object","properties":{ "keyword":{"type":"string"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] }), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_plans", description: "只读：列出套餐模板", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_redemptions", description: "只读：列出兑换码（分页）", schema: paged(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_list_groups", description: "只读：列出用户分组", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_query_logs", description: "只读：查询请求日志（user/model/channel/start/end 过滤 + 分页）", schema: json!({ "type":"object","properties":{ "user":{"type":"string"}, "model":{"type":"string"}, "channel":{"type":"string"}, "start":{"type":"integer"}, "end":{"type":"integer"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] }), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_get_pricing", description: "只读：列出全部模型定价", schema: no_args(), risk: RiskLevel::ReadOnly },
        ToolSpec { name: "aigx_cost_report", description: "只读：成本/用量聚合报表（按模型统计调用量与成本）", schema: no_args(), risk: RiskLevel::ReadOnly },
        // ── 低危写×4（复用 mcp 已有的渠道写）──
        ToolSpec { name: "aigx_channel_enable", description: "写操作（可回滚）：启用渠道", schema: channel_id_args(), risk: RiskLevel::LowRisk },
        ToolSpec { name: "aigx_channel_disable", description: "写操作（可回滚）：禁用渠道", schema: channel_id_args(), risk: RiskLevel::LowRisk },
        ToolSpec { name: "aigx_channel_test", description: "写操作（可回滚）：测试渠道连通性并落库结果", schema: channel_id_args(), risk: RiskLevel::LowRisk },
        ToolSpec { name: "aigx_reset_circuit_breaker", description: "写操作（可回滚）：重置渠道熔断器", schema: channel_id_args(), risk: RiskLevel::LowRisk },
    ]
}

fn channel_id_args() -> Value {
    json!({
        "type": "object",
        "properties": { "channel_id": { "type": "string", "description": "渠道 ID" } },
        "required": ["channel_id"]
    })
}

/// 查找工具（白名单）。
pub fn find_tool(name: &str) -> Option<ToolSpec> {
    tool_specs().into_iter().find(|t| t.name == name)
}

/// 工具执行结果。
pub struct ToolOutcome {
    /// 序列化后的文本结果（喂回 LLM）。
    pub text: String,
    /// 是否成功。
    pub ok: bool,
}

type HandlerResult = Result<Json<Value>, (axum::http::StatusCode, Json<Value>)>;

/// 分发执行工具（进程内直调 handler）。
///
/// 返回序列化文本（成功 = 数据 JSON；失败 = 脱敏错误消息）。
pub async fn exec_tool(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> Result<ToolOutcome, (i64, String)> {
    // 参数提取辅助
    let str_arg = |k: &str| args.get(k).and_then(|v| v.as_str()).map(|s| s.to_string());

    let result: HandlerResult = match name {
        "aigx_diagnostics_summary" => {
            admin::handle_diagnostics_summary(State(state.clone()), headers.clone()).await
        }
        "aigx_diagnostics_channels" => {
            admin::handle_diagnostics_channels(State(state.clone()), headers.clone()).await
        }
        "aigx_diagnostics_breakers" => {
            admin::handle_diagnostics_breakers(State(state.clone()), headers.clone()).await
        }
        "aigx_list_channels" => {
            let q = crate::api::admin::mcp::args_to_string_map(args);
            handle_list_channels(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_users" => {
            let q = crate::api::admin::mcp::parse_struct_args::<crate::api::admin::users::ListUsersQuery>(name, args)?;
            handle_list_users(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_keys" => {
            handle_list_keys(State(state.clone()), headers.clone()).await
        }
        "aigx_list_accounts" => {
            handle_list_accounts(State(state.clone()), headers.clone()).await
        }
        "aigx_list_orders" => {
            let q = crate::api::admin::mcp::args_to_string_map(args);
            handle_list_orders(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_plans" => {
            handle_list_plans(State(state.clone()), headers.clone()).await
        }
        "aigx_list_redemptions" => {
            let q = crate::api::admin::mcp::parse_struct_args::<crate::api::admin::logs::AuditLogQuery>(name, args)?;
            handle_list_redemptions(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_groups" => {
            handle_list_groups(State(state.clone()), headers.clone()).await
        }
        "aigx_query_logs" => {
            let q = crate::api::admin::mcp::parse_struct_args::<crate::api::admin::logs::RequestLogQuery>(name, args)?;
            handle_list_request_logs(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_get_pricing" => {
            handle_list_pricing(State(state.clone()), headers.clone()).await
        }
        "aigx_cost_report" => cost_report(state).await,
        "aigx_channel_enable" => {
            let id = require_channel_id(args)?;
            admin::handle_patch_channel(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(json!({ "enabled": true })),
            )
            .await
        }
        "aigx_channel_disable" => {
            let id = require_channel_id(args)?;
            admin::handle_patch_channel(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(json!({ "enabled": false })),
            )
            .await
        }
        "aigx_channel_test" => {
            let id = require_channel_id(args)?;
            admin::handle_test_channel(State(state.clone()), headers.clone(), Path(id)).await
        }
        "aigx_reset_circuit_breaker" => {
            let id = require_channel_id(args)?;
            admin::handle_reset_channel_circuit(State(state.clone()), headers.clone(), Path(id)).await
        }
        _ => {
            return Err((
                -32601,
                format!("Unknown tool: {name} (not in agent whitelist)"),
            ))
        }
    };

    Ok(handler_to_outcome(result))
}

fn handler_to_outcome(result: HandlerResult) -> ToolOutcome {
    match result {
        Ok(Json(v)) => ToolOutcome { text: v.to_string(), ok: true },
        Err((_, Json(body))) => {
            let msg = body
                .get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| body.to_string());
            ToolOutcome { text: crate::error_translate::sanitize_error_message(&msg), ok: false }
        }
    }
}

fn require_channel_id(args: &Value) -> Result<String, (i64, String)> {
    args.get("channel_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| (-32602, "Invalid params: missing required argument channel_id".to_string()))
}

/// 成本/用量聚合报表（只读，阶段一先做简单聚合）。
///
/// 从 usage_tracker 的当日聚合取按模型的用量与成本。若 usage_tracker
/// 未提供聚合接口，则降级返回空报表（不报错）。
async fn cost_report(state: &AppState) -> HandlerResult {
    // 阶段一：复用诊断 summary 的口径（渠道统计 + 今日用量），
    // 成本精细聚合留阶段三（需 pricing × usage 联表）。
    let summary = admin::handle_diagnostics_summary(State(state.clone()), HeaderMap::new()).await;
    match summary {
        Ok(Json(v)) => Ok(Json(json!({ "success": true, "data": v, "note": "成本精细聚合见阶段三" }))),
        Err(e) => Err(e),
    }
}

/// 把工具集序列化为 OpenAI function-calling 的 `tools` 数组。
pub fn openai_tools() -> Vec<Value> {
    tool_specs()
        .into_iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.schema,
                }
            })
        })
        .collect()
}

/// 供 runner 使用的共享引用（避免每个工具都 clone 一次）。
#[allow(dead_code)]
pub type ToolExecutor = Arc<dyn Fn(&AppState, &HeaderMap, &str, &Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ToolOutcome, (i64, String)>> + Send>> + Send + Sync>;