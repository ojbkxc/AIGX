//! Agent 工具——兼容层。
//!
//! 工具的**声明与执行体**已统一到 [`crate::agent::registry`]（Agent / MCP 共用
//! 一份注册表，spec 与 exec 同处一条声明）。本文件保留历史导出名与执行辅助
//! 函数，避免 runner / settings / mcp 的引用面大改：
//!
//! - [`RiskLevel`] / [`ToolOutcome`]：风险分级与执行结果（registry 消费）；
//! - `tool_specs` / `find_tool` / `exec_tool` / `openai_tools`：re-export registry；
//! - `require_arg` / `parse_tool_args` / `handler_to_outcome` 等：registry 的
//!   执行体共用的参数解析与结果包装。

use axum::Json;
use serde_json::Value;

use crate::api::openai::AppState;

pub use crate::agent::registry::{
    all_tools as tool_specs, exec_tool, exec_user_tool, find_tool, openai_tools, openai_tools_of,
    user_exposed_tools, ExecResult, ToolDef,
};

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

/// 工具执行结果。
pub struct ToolOutcome {
    /// 序列化后的文本结果（喂回 LLM）。
    pub text: String,
    /// 是否成功。
    pub ok: bool,
}

pub(crate) type HandlerResult = Result<Json<Value>, (axum::http::StatusCode, Json<Value>)>;

/// 取必填字符串参数（缺失/非字符串 → -32602）。
pub(crate) fn require_arg(args: &Value, key: &str) -> Result<String, (i64, String)> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            (
                -32602,
                format!("Invalid params: missing required argument {key}"),
            )
        })
}

pub(crate) fn require_channel_id(args: &Value) -> Result<String, (i64, String)> {
    require_arg(args, "channel_id")
}

/// 从工具参数反序列化为 handler 的请求体 struct（字段名同源，类型不符 → -32602）。
pub(crate) fn parse_tool_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: &Value,
) -> Result<T, (i64, String)> {
    serde_json::from_value(args.clone())
        .map_err(|e| (-32602, format!("Invalid params for tool {tool}: {e}")))
}

pub(crate) fn handler_to_outcome(result: HandlerResult) -> ToolOutcome {
    match result {
        Ok(Json(v)) => ToolOutcome {
            text: crate::api::admin::common::truncate_tool_text(v.to_string()),
            ok: true,
        },
        Err((_, Json(body))) => {
            let msg = body
                .get("error")
                .and_then(|e| e.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| body.to_string());
            ToolOutcome {
                text: crate::error_translate::sanitize_error_message(&msg),
                ok: false,
            }
        }
    }
}

/// 成本/用量聚合报表（按模型：请求/token/销售价/成本/利润）。
pub(crate) async fn cost_report(state: &AppState) -> HandlerResult {
    use serde_json::json;
    let logs = state.log_store.requests.list_all();
    let mut agg: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();
    for log in logs {
        let e = agg.entry(log.model.clone()).or_insert_with(|| {
            json!({
                "model": log.model.clone(),
                "requests": 0u64,
                "input_tokens": 0u64,
                "output_tokens": 0u64,
                "cost": 0i64,
                "channel_cost": 0i64,
                "profit": 0i64,
            })
        });
        e["requests"] = json!(e["requests"].as_u64().unwrap_or(0) + 1);
        e["input_tokens"] = json!(e["input_tokens"].as_u64().unwrap_or(0) + log.input_tokens);
        e["output_tokens"] = json!(e["output_tokens"].as_u64().unwrap_or(0) + log.output_tokens);
        e["cost"] = json!(e["cost"].as_i64().unwrap_or(0) + log.cost);
        e["channel_cost"] = json!(e["channel_cost"].as_i64().unwrap_or(0) + log.channel_cost);
        e["profit"] = json!(e["profit"].as_i64().unwrap_or(0) + (log.cost - log.channel_cost));
    }
    let mut rows: Vec<serde_json::Value> = agg.into_values().collect();
    rows.sort_by(|a, b| {
        b["cost"]
            .as_i64()
            .unwrap_or(0)
            .cmp(&a["cost"].as_i64().unwrap_or(0))
    });
    Ok(Json(json!({ "success": true, "data": rows })))
}
