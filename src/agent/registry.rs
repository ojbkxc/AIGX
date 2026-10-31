//! 工具统一注册表 —— Agent / MCP / 设置页 / 前端审批卡的**单一来源**。
//!
//! ## 为什么存在
//!
//! 此前 `agent::tools` 与 `api::admin::mcp` 各养一份静态工具表（45 vs 19），
//! 同名工具描述都漂移了；新功能域（模型管理/签到/报表/…）两边都跟不上。
//! 本模块把「一条工具 = 一份声明（元数据 + 执行体）」钉死在一个结构里：
//!
//! - [`ToolDef`]：name / label（中文短名，审批卡用）/ description（喂 LLM）/
//!   schema / risk / target_key（审计主参数）/ covers（覆盖的管理路由，防漂移测试用）/ exec；
//! - [`all_tools`] 全量；[`mcp_tools`] 只出非高危（MCP 面约定）；
//! - 防漂移测试（本文件 `tests`）：域覆盖 / MCP 非高危子集 / spec 合法性。
//!
//! 新增工具只改本文件一条声明；新增管理域必须给某个工具 `covers` 命中或进
//! 测试豁免表，否则 `openapi_domains_have_tool_coverage` 会红。

use std::future::Future;
use std::pin::Pin;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use super::tools::{
    handler_to_outcome, parse_tool_args, require_arg, require_channel_id, HandlerResult, RiskLevel,
    ToolOutcome,
};
use crate::api::admin;
use crate::api::openai::AppState;

/// 执行体：进程内直调 handler。参数错返回 `(JSON-RPC 码, 消息)`。
pub type ExecResult = Result<ToolOutcome, (i64, String)>;
type ExecFuture<'a> = Pin<Box<dyn Future<Output = ExecResult> + Send + 'a>>;
pub type ExecFn = for<'a> fn(&'a AppState, &'a HeaderMap, &'a Value) -> ExecFuture<'a>;

/// 一条工具的完整声明（spec + exec 同处，杜绝两处漂移）。
pub struct ToolDef {
    /// 全局唯一，`aigx_` 前缀。
    pub name: &'static str,
    /// 中文短名（审批卡 / 前端标签）。
    pub label: &'static str,
    /// 完整描述（含「只读：/写操作（可回滚）：/高危写：」风险前缀，喂 LLM）。
    pub description: &'static str,
    /// JSON Schema（惰性构造：`static` 持有不了 `Value`）。
    pub schema: fn() -> Value,
    pub risk: RiskLevel,
    /// 审计 target 主参数名；空串 = 回退参数摘要。
    pub target_key: &'static str,
    /// 本工具覆盖的管理面路由前缀（防漂移测试）。
    pub covers: &'static [&'static str],
    pub exec: ExecFn,
}

/// 声明构造器（保持调用点紧凑）。
#[allow(clippy::too_many_arguments)]
fn td(
    name: &'static str,
    label: &'static str,
    description: &'static str,
    risk: RiskLevel,
    target_key: &'static str,
    covers: &'static [&'static str],
    schema: fn() -> Value,
    exec: ExecFn,
) -> ToolDef {
    ToolDef {
        name,
        label,
        description,
        schema,
        risk,
        target_key,
        covers,
        exec,
    }
}

// ── schema 惯用形状 ────────────────────────────────────────────────

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "required": [] })
}

fn paged() -> Value {
    json!({
        "type": "object",
        "properties": {
            "page": { "type": "integer", "minimum": 1, "description": "页码，默认 1" },
            "size": { "type": "integer", "minimum": 1, "description": "每页条数，默认 20" }
        },
        "required": []
    })
}

fn channel_id_args() -> Value {
    json!({
        "type": "object",
        "properties": { "channel_id": { "type": "string", "description": "渠道 ID" } },
        "required": ["channel_id"]
    })
}

fn account_id_args() -> Value {
    json!({
        "type": "object",
        "properties": { "account_id": { "type": "string", "description": "账号会话 ID" } },
        "required": ["account_id"]
    })
}

/// handler 结果 → 工具结果（截断 + 脱敏在 `handler_to_outcome` 内）。
fn out(result: HandlerResult) -> ExecResult {
    Ok(handler_to_outcome(result))
}

// ── 注册表 ─────────────────────────────────────────────────────────

static TOOLS: std::sync::OnceLock<Vec<ToolDef>> = std::sync::OnceLock::new();

/// 全量工具（惰性构建一次）。
pub fn all_tools() -> &'static [ToolDef] {
    TOOLS.get_or_init(build_tools).as_slice()
}

/// 按名查找。
pub fn find_tool(name: &str) -> Option<&'static ToolDef> {
    all_tools().iter().find(|t| t.name == name)
}

/// MCP 面：只暴露非高危（高危写留给 Agent 审批矩阵，外部客户端不给碰）。
pub fn mcp_tools() -> impl Iterator<Item = &'static ToolDef> {
    all_tools().iter().filter(|t| t.risk != RiskLevel::HighRisk)
}

/// OpenAI function-calling 的 `tools` 数组（Agent 用）。
pub fn openai_tools() -> Vec<Value> {
    all_tools()
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": (t.schema)(),
                }
            })
        })
        .collect()
}

/// 分发执行（Agent / MCP 共用入口）。
pub async fn exec_tool(
    state: &AppState,
    headers: &HeaderMap,
    name: &str,
    args: &Value,
) -> ExecResult {
    let Some(tool) = find_tool(name) else {
        return Err((
            -32601,
            format!("Unknown tool: {name} (not in agent whitelist)"),
        ));
    };
    (tool.exec)(state, headers, args).await
}

fn build_tools() -> Vec<ToolDef> {
    use RiskLevel::{HighRisk, LowRisk, ReadOnly};
    vec![
        // ── 诊断 ×3 ──
        td(
            "aigx_diagnostics_summary",
            "系统体检摘要",
            "只读：系统体检摘要（渠道统计/今日用量/24h 错误概况/运行信息）",
            ReadOnly,
            "",
            &["/api/diagnostics/summary"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_summary(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_diagnostics_channels",
            "启用渠道批量测活",
            "只读：启用渠道批量测活（并发探测，约需 30 秒）",
            ReadOnly,
            "",
            &["/api/diagnostics/channels"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_channels(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_diagnostics_breakers",
            "熔断器快照",
            "只读：熔断器状态快照（三态/失败计数/冷却剩余）",
            ReadOnly,
            "",
            &["/api/diagnostics/breakers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_diagnostics_breakers(State(s.clone()), h.clone()).await)
                })
            },
        ),
        // ── 查询 ×11 ──
        td(
            "aigx_list_channels",
            "列出渠道",
            "只读：列出渠道（search 模糊过滤 + page/page_size 分页）",
            ReadOnly,
            "",
            &["/api/channels"],
            || {
                json!({ "type":"object","properties":{ "search":{"type":"string","description":"按名称/类型/base_url/模型模糊过滤"}, "page":{"type":"integer","minimum":1}, "page_size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::handle_list_channels(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_users",
            "列出用户",
            "只读：列出用户（分页，敏感字段脱敏）",
            ReadOnly,
            "",
            &["/api/users"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::users::ListUsersQuery,
                    >("aigx_list_users", a)?;
                    out(admin::handle_list_users(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_keys",
            "列出 API 密钥",
            "只读：列出 API 密钥（脱敏）",
            ReadOnly,
            "",
            &["/api/keys"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_keys(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_list_tokens",
            "列出访问令牌",
            "只读：列出 API 访问令牌（管理员可携 all=1 查全部，脱敏）",
            ReadOnly,
            "",
            &["/api/tokens"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        crate::api::admin::tokens::ListTokensQuery,
                    >("aigx_list_tokens", a)?;
                    out(admin::handle_list_tokens(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_accounts",
            "列出 CF 账号池",
            "只读：列出 CF 账号池（脱敏）",
            ReadOnly,
            "",
            &["/api/accounts"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_list_accounts(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_list_orders",
            "列出订单",
            "只读：列出订单（keyword 过滤 + 分页）",
            ReadOnly,
            "",
            &["/api/orders"],
            || {
                json!({ "type":"object","properties":{ "keyword":{"type":"string"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::handle_list_orders(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_plans",
            "列出套餐模板",
            "只读：列出套餐模板",
            ReadOnly,
            "",
            &["/api/plans"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_plans(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_list_redemptions",
            "列出兑换码",
            "只读：列出兑换码（分页）",
            ReadOnly,
            "",
            &["/api/redemptions"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::redemptions::AuditLogQuery,
                    >("aigx_list_redemptions", a)?;
                    out(admin::handle_list_redemptions(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_list_groups",
            "列出用户分组",
            "只读：列出用户分组",
            ReadOnly,
            "",
            &["/api/groups"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_groups(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_query_logs",
            "查询请求日志",
            "只读：查询请求日志（user/model/channel/start/end 过滤 + 分页）",
            ReadOnly,
            "",
            &["/api/logs/requests"],
            || {
                json!({ "type":"object","properties":{ "user":{"type":"string"}, "model":{"type":"string"}, "channel":{"type":"string"}, "start":{"type":"integer"}, "end":{"type":"integer"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::logs::RequestLogQuery,
                    >("aigx_query_logs", a)?;
                    out(admin::handle_list_request_logs(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_get_pricing",
            "列出模型定价",
            "只读：列出全部模型定价",
            ReadOnly,
            "",
            &["/api/pricing"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_list_pricing(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_cost_report",
            "成本用量报表",
            "只读：成本/用量聚合报表（按模型统计调用量与成本）",
            ReadOnly,
            "",
            &["/api/logs/requests"],
            no_args,
            |s, _h, _a| Box::pin(async move { out(super::tools::cost_report(s).await) }),
        ),
        // ── 低危写（渠道/密钥）──
        td(
            "aigx_channel_enable",
            "启用渠道",
            "写操作（可回滚）：启用渠道",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(json!({ "enabled": true })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_disable",
            "禁用渠道",
            "写操作（可回滚）：禁用渠道",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(json!({ "enabled": false })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_test",
            "测试渠道连通性",
            "写操作（可回滚）：测试渠道连通性并落库结果",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}/test"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_test_channel(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_reset_circuit_breaker",
            "重置渠道熔断器",
            "写操作（可回滚）：重置渠道熔断器",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}/reset-circuit"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    out(admin::handle_reset_channel_circuit(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_channel_add",
            "新增渠道",
            "写操作（可回滚）：新增渠道（name/base_url/api_key/type）",
            LowRisk,
            "name",
            &["/api/channels"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "渠道名" },
                        "base_url": { "type": "string", "description": "上游 base URL" },
                        "api_key": { "type": "string", "description": "上游 API key" },
                        "channel_type": { "type": "string", "description": "openai_compatible/anthropic/gemini/zai/cloudflare/account（account = 账号型渠道，凭证在账号池里加）" }
                    },
                    "required": ["name", "base_url", "api_key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::channels::ChannelRequest = parse_tool_args("aigx_channel_add", a)?;
                    out(admin::handle_add_channel(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_channel_update",
            "部分更新渠道",
            "写操作（可回滚）：部分更新渠道字段（name/base_url/priority/weight/models/enabled…）",
            LowRisk,
            "channel_id",
            &["/api/channels/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string", "description": "渠道 ID" },
                        "name": { "type": "string" },
                        "base_url": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "models": { "type": "array", "items": { "type": "string" } },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["channel_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_channel_id(a)?;
                    let mut patch = a.clone();
                    if let Some(obj) = patch.as_object_mut() {
                        obj.remove("channel_id");
                    }
                    out(admin::handle_patch_channel(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(patch),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_key_add",
            "新增 API 密钥",
            "写操作（可回滚）：新增 API 密钥",
            LowRisk,
            "name",
            &["/api/keys"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "密钥名" },
                        "user_id": { "type": "string", "description": "归属用户 ID（可空）" },
                        "group": { "type": "string", "description": "分组（可空）" }
                    },
                    "required": ["name"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::tokens::KeyRequest = parse_tool_args("aigx_key_add", a)?;
                    out(admin::handle_add_key(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        // ── 高危写（审批矩阵人工确认）──
        td(
            "aigx_user_delete",
            "删除用户",
            "高危写：删除用户（不可逆，需审批）",
            HighRisk,
            "user_id",
            &["/api/users/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "user_id": { "type": "string", "description": "用户 ID" } },
                    "required": ["user_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "user_id")?;
                    out(admin::handle_delete_user(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_user_manage",
            "启用/禁用用户",
            "高危写：启用/禁用用户（需审批）",
            HighRisk,
            "user_id",
            &["/api/users/manage"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "user_id": { "type": "string", "description": "用户 ID" },
                        "action": { "type": "string", "enum": ["enable", "disable"], "description": "enable/disable" }
                    },
                    "required": ["user_id", "action"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::users::ManageUserRequest = parse_tool_args("aigx_user_manage", a)?;
                    out(admin::handle_manage_user(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_channel_delete",
            "删除渠道",
            "高危写：删除渠道（不可逆，需审批）",
            HighRisk,
            "channel_id",
            &["/api/channels/{id}"],
            channel_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "channel_id")?;
                    // Agent 路径免密码：直调 core（删除 + 审计），不走 HTTP handler 的
                    // 密码二次认证——高危工具已经审批矩阵人工确认（runner.rs），
                    // 审批本身即同等强度的身份复核；此处仍有 verify_admin 兜底。
                    out(admin::channels::delete_channel_core(s, h, &id).await)
                })
            },
        ),
        td(
            "aigx_order_delete",
            "删除订单",
            "高危写：删除订单（不可逆，需审批）",
            HighRisk,
            "trade_no",
            &["/api/orders/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "trade_no": { "type": "string", "description": "订单号" } },
                    "required": ["trade_no"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let trade_no = require_arg(a, "trade_no")?;
                    out(admin::handle_delete_order(State(s.clone()), h.clone(), Path(trade_no)).await)
                })
            },
        ),
        td(
            "aigx_pricing_upsert",
            "新增/更新模型定价（旧版）",
            "高危写：新增/更新模型定价（需审批；推荐 aigx_model_price_upsert）",
            HighRisk,
            "model_name",
            &["/api/pricing"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "model_name": { "type": "string" },
                        "input_price": { "type": "number" },
                        "output_price": { "type": "number" },
                        "price_type": { "type": "string", "description": "token/image 等" }
                    },
                    "required": ["model_name", "input_price", "output_price", "price_type"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::pricing::PriceRequest = parse_tool_args("aigx_pricing_upsert", a)?;
                    out(admin::handle_add_pricing(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_pricing_delete",
            "删除模型定价（旧版）",
            "高危写：删除模型定价（需审批；推荐 aigx_model_price_remove）",
            HighRisk,
            "model_name",
            &["/api/pricing/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "model_name": { "type": "string" } },
                    "required": ["model_name"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let model_name = require_arg(a, "model_name")?;
                    out(admin::handle_delete_pricing(
                        State(s.clone()),
                        h.clone(),
                        Path(model_name),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_logs_cleanup",
            "清理过期日志",
            "高危写：按保留天数清理请求/审计日志（需审批）",
            HighRisk,
            "",
            &["/api/logs/cleanup"],
            || {
                json!({
                    "type": "object",
                    "properties": { "days": { "type": "integer", "minimum": 1, "description": "保留天数" } },
                    "required": ["days"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let days = a.get("days").and_then(|v| v.as_u64());
                    out(admin::handle_cleanup_logs(
                        State(s.clone()),
                        h.clone(),
                        Some(Json(admin::logs::LogCleanupBody { days })),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_key_delete",
            "删除 API 密钥",
            "高危写：删除 API 密钥（不可逆，需审批）",
            HighRisk,
            "key_id",
            &["/api/keys/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "key_id": { "type": "string", "description": "密钥 ID" } },
                    "required": ["key_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "key_id")?;
                    out(admin::handle_delete_key(State(s.clone()), h.clone(), Path(id)).await)
                })
            },
        ),
        td(
            "aigx_group_upsert",
            "新增/更新分组",
            "高危写：新增/更新用户分组倍率（需审批）",
            HighRisk,
            "name",
            &["/api/groups"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "ratio": { "type": "number" }
                    },
                    "required": ["name", "ratio"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::legacy::GroupRequest = parse_tool_args("aigx_group_upsert", a)?;
                    out(admin::handle_upsert_group(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        // ── 账号池（账号会话）──
        td(
            "aigx_list_account_sessions",
            "列出账号池账号",
            "只读：列出账号池账号（channel_id/search 过滤）",
            ReadOnly,
            "",
            &["/api/account-sessions"],
            || {
                json!({ "type":"object","properties":{ "channel_id":{"type":"string"}, "search":{"type":"string"} }, "required":[] })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::args_to_string_map(a);
                    out(admin::account_sessions::handle_list_account_sessions(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_stats",
            "账号池统计",
            "只读：账号池统计（成功率/冷却/并发）",
            ReadOnly,
            "",
            &["/api/account-sessions/stats"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_account_session_stats(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_test",
            "单号探活",
            "写操作（可回滚）：单号探活（走真实推理链路）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/test"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_test_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_enable",
            "启用账号",
            "写操作（可回滚）：启用账号池账号",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::AccountSessionPatch::status_only(
                            "enabled",
                        )),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_disable",
            "禁用账号",
            "写操作（可回滚）：禁用账号池账号",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::AccountSessionPatch::status_only(
                            "disabled",
                        )),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_reset_cooldown",
            "清除账号冷却",
            "写操作（可回滚）：清除账号冷却与最近错误",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/reset-cooldown"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_reset_account_session_cooldowns(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_add",
            "新增账号",
            "写操作（可回滚）：新增账号池账号（channel_id/provider/api_key…）",
            LowRisk,
            "name",
            &["/api/account-sessions"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "api_key": { "type": "string", "description": "登录态/密钥凭证（必填）" },
                        "name": { "type": "string" },
                        "base_url": { "type": "string" },
                        "proxy": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "max_concurrency": { "type": "integer" },
                        "models": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["channel_id", "api_key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    // 账号必须有凭证，空 key 建号没有意义（前端表单同样禁止）
                    require_arg(a, "api_key")?;
                    let body: admin::account_sessions::AccountSessionRequest =
                        parse_tool_args("aigx_account_session_add", a)?;
                    out(admin::account_sessions::handle_create_account_session(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_update",
            "更新账号",
            "写操作（可回滚）：更新账号属性（name/status/priority/weight/max_concurrency）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "name": { "type": "string" },
                        "status": { "type": "string" },
                        "priority": { "type": "integer" },
                        "weight": { "type": "integer" },
                        "max_concurrency": { "type": "integer" }
                    },
                    "required": ["account_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    let body: admin::account_sessions::AccountSessionPatch =
                        parse_tool_args("aigx_account_session_update", a)?;
                    out(admin::account_sessions::handle_patch_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_delete",
            "删除账号",
            "高危写：删除账号池账号（不可逆，需审批）",
            HighRisk,
            "account_id",
            &["/api/account-sessions/{id}"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_delete_account_session(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_session_bulk_import",
            "批量导入账号",
            "高危写：批量导入账号池账号（每行一个凭证，需审批）",
            HighRisk,
            "channel_id",
            &["/api/account-sessions/bulk"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "credentials": { "type": "string", "description": "多行凭证，每行一个" }
                    },
                    "required": ["channel_id", "credentials"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::BulkImportRequest =
                        parse_tool_args("aigx_account_session_bulk_import", a)?;
                    out(admin::account_sessions::handle_bulk_import_account_sessions(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        // ── 服务器自动更新 ──
        td(
            "aigx_update_status",
            "更新状态",
            "只读：服务器更新状态（版本/下载进度/待重启）",
            ReadOnly,
            "",
            &["/api/update/status"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_status(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_check_update",
            "检查更新",
            "写操作（可回滚）：立即检查 GitHub Release（只读网络请求）",
            LowRisk,
            "",
            &["/api/update/check"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_check(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_apply_update",
            "应用更新",
            "高危写：下载并应用更新（需审批，会重启服务）",
            HighRisk,
            "",
            &["/api/update/apply"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    // handler 新增了可选 body（force 标记）—— Agent 走默认路径
                    out(
                        admin::update::handle_update_apply(State(s.clone()), h.clone(), None).await,
                    )
                })
            },
        ),
        td(
            "aigx_restart_service",
            "重启服务",
            "高危写：重启网关服务（需审批）",
            HighRisk,
            "",
            &["/api/update/restart"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_restart(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_rollback_update",
            "回滚更新",
            "高危写：回滚到上一版本（需审批）",
            HighRisk,
            "",
            &["/api/update/rollback"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::update::handle_update_rollback(State(s.clone()), h.clone()).await)
                })
            },
        ),
        // ── 统一模型管理 ──
        td(
            "aigx_models_view",
            "模型管理视图",
            "只读：统一模型管理视图（渠道+账号池模型合流，含启停/来源/定价/别名）",
            ReadOnly,
            "",
            &["/api/models/manage"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::model_manage::handle_manage_view(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_model_set_state",
            "模型启停",
            "写操作（可回滚）：按 (provider, model) 启用/禁用模型",
            LowRisk,
            "id",
            &["/api/models/state"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "provider": { "type": "string" },
                        "id": { "type": "string", "description": "模型 ID" },
                        "enabled": { "type": "boolean" }
                    },
                    "required": ["provider", "id", "enabled"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::StateRequest =
                        parse_tool_args("aigx_model_set_state", a)?;
                    out(admin::model_manage::handle_set_state(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_model_price_upsert",
            "设置模型定价",
            "高危写：设置模型定价与倍率（需审批）",
            HighRisk,
            "model",
            &["/api/models/price"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "model": { "type": "string" },
                        "input_price": { "type": "number" },
                        "output_price": { "type": "number" },
                        "cache_price": { "type": "number" },
                        "price_type": { "type": "string", "description": "token/count" },
                        "model_ratio": { "type": "number" }
                    },
                    "required": ["model", "input_price", "output_price"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::PriceBody =
                        parse_tool_args("aigx_model_price_upsert", a)?;
                    out(admin::model_manage::handle_upsert_model_price(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_price_remove",
            "删除模型定价",
            "高危写：删除模型定价（需审批）",
            HighRisk,
            "model",
            &["/api/models/price/remove"],
            || {
                json!({
                    "type": "object",
                    "properties": { "model": { "type": "string" } },
                    "required": ["model"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::PriceRemoveBody =
                        parse_tool_args("aigx_model_price_remove", a)?;
                    out(admin::model_manage::handle_remove_price(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_model_alias_set",
            "设置模型别名",
            "写操作（可回滚）：设置全局模型别名（alias → target）",
            LowRisk,
            "alias",
            &["/api/models/alias"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "alias": { "type": "string" },
                        "target": { "type": "string" }
                    },
                    "required": ["alias", "target"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::AliasBody =
                        parse_tool_args("aigx_model_alias_set", a)?;
                    out(admin::model_manage::handle_set_alias(State(s.clone()), h.clone(), Json(body)).await)
                })
            },
        ),
        td(
            "aigx_model_alias_remove",
            "删除模型别名",
            "写操作（可回滚）：删除全局模型别名",
            LowRisk,
            "alias",
            &["/api/models/alias/remove"],
            || {
                json!({
                    "type": "object",
                    "properties": { "alias": { "type": "string" } },
                    "required": ["alias"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::model_manage::AliasRemoveBody =
                        parse_tool_args("aigx_model_alias_remove", a)?;
                    out(admin::model_manage::handle_remove_alias(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_models_meta_list",
            "模型元信息",
            "只读：模型元信息覆盖（owned_by/上下文/能力）",
            ReadOnly,
            "",
            &["/api/models/meta"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::pricing::handle_model_meta_list(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_available_models",
            "可用模型清单",
            "只读：用户侧可用模型聚合（启用渠道模型并集）",
            ReadOnly,
            "",
            &["/api/models/available"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_available_models(State(s.clone()), h.clone()).await)
                })
            },
        ),
        // ── 签到中心 ──
        td(
            "aigx_checkin_overview",
            "签到总览",
            "只读：签到中心聚合快照（今日进度/计划/台账摘要）",
            ReadOnly,
            "",
            &["/api/checkin-center"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::checkin_center::handle_checkin_center(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_checkin_run",
            "立即签到",
            "写操作（可回滚）：立即执行签到（可按账号/供应商范围）",
            LowRisk,
            "",
            &["/api/checkin-center/run"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_ids": { "type": "array", "items": { "type": "string" } },
                        "providers": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::RunRequest =
                        parse_tool_args("aigx_checkin_run", a)?;
                    out(admin::checkin_center::handle_checkin_run(
                        State(s.clone()),
                        h.clone(),
                        Some(Json(body)),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_records",
            "签到台账",
            "只读：签到记录台账（分页）",
            ReadOnly,
            "",
            &["/api/checkin-center/records"],
            paged,
            |s, h, a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::checkin_center::RecordsQuery,
                    >("aigx_checkin_records", a)?;
                    out(admin::checkin_center::handle_checkin_records(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_records_clear",
            "清空签到台账",
            "高危写：清空签到记录台账（不可逆，需审批）",
            HighRisk,
            "",
            &["/api/checkin-center/records"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    let q = crate::api::admin::mcp::parse_struct_args::<
                        admin::checkin_center::RecordsQuery,
                    >("aigx_checkin_records_clear", &json!({}))?;
                    out(admin::checkin_center::handle_clear_checkin_records(
                        State(s.clone()),
                        h.clone(),
                        Query(q),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_settings_get",
            "读签到设置",
            "只读：读取签到中心设置",
            ReadOnly,
            "",
            &["/api/checkin-center/settings"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::checkin_center::handle_get_checkin_settings(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_settings_set",
            "改签到设置",
            "写操作（可回滚）：修改签到中心设置",
            LowRisk,
            "",
            &["/api/checkin-center/settings"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean" },
                        "interval": { "type": "integer" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: crate::checkin::store::CheckinSettings =
                        parse_tool_args("aigx_checkin_settings_set", a)?;
                    out(admin::checkin_center::handle_update_checkin_settings(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedules_list",
            "签到计划列表",
            "只读：列出定时签到计划",
            ReadOnly,
            "",
            &["/api/checkin-center/schedules"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::checkin_center::handle_list_checkin_schedules(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_upsert",
            "新增/改签到计划",
            "写操作（可回滚）：新增或修改定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "enabled": { "type": "boolean" },
                        "time": { "type": "string", "description": "HH:MM" },
                        "providers": { "type": "array", "items": { "type": "string" } }
                    },
                    "required": ["label", "time"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleBody =
                        parse_tool_args("aigx_checkin_schedule_upsert", a)?;
                    out(admin::checkin_center::handle_upsert_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_remove",
            "删除签到计划",
            "写操作（可回滚）：删除定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleIdBody =
                        parse_tool_args("aigx_checkin_schedule_remove", a)?;
                    out(admin::checkin_center::handle_remove_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_checkin_schedule_run",
            "立即执行签到计划",
            "写操作（可回滚）：立即执行指定定时签到计划",
            LowRisk,
            "id",
            &["/api/checkin-center/schedules"],
            || {
                json!({
                    "type": "object",
                    "properties": { "id": { "type": "string" } },
                    "required": ["id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::checkin_center::ScheduleIdBody =
                        parse_tool_args("aigx_checkin_schedule_run", a)?;
                    out(admin::checkin_center::handle_run_checkin_schedule(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        // ── 报表（一个工具按 kind 分派 7 个报表）──
        td(
            "aigx_dashboard_report",
            "运维报表",
            "只读：运维报表（report 选 overview/models/heatmap/top_sources/token_trend/cache_rates/daily_trend；普通用户自动只看自己）",
            ReadOnly,
            "",
            &[
                "/api/dashboard/report/overview",
                "/api/dashboard/report/models",
                "/api/dashboard/report/heatmap",
                "/api/dashboard/report/top_sources",
                "/api/dashboard/report/token_trend",
                "/api/dashboard/report/cache_rates",
                "/api/dashboard/report/daily_trend",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "report": { "type": "string", "enum": ["overview", "models", "heatmap", "top_sources", "token_trend", "cache_rates", "daily_trend"], "description": "报表种类" },
                        "days": { "type": "integer", "minimum": 1, "maximum": 365, "description": "时间范围（天），默认 30" }
                    },
                    "required": ["report"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let kind = require_arg(a, "report")?;
                    let q: admin::dashboard::ReportQuery =
                        parse_tool_args("aigx_dashboard_report", a)?;
                    out(match kind.as_str() {
                        "overview" => admin::dashboard::handle_report_overview(State(s.clone()), h.clone(), Query(q)).await,
                        "models" => admin::dashboard::handle_report_models(State(s.clone()), h.clone(), Query(q)).await,
                        "heatmap" => admin::dashboard::handle_report_heatmap(State(s.clone()), h.clone(), Query(q)).await,
                        "top_sources" => admin::dashboard::handle_report_top_sources(State(s.clone()), h.clone(), Query(q)).await,
                        "token_trend" => admin::dashboard::handle_report_token_trend(State(s.clone()), h.clone(), Query(q)).await,
                        "cache_rates" => admin::dashboard::handle_report_cache_rates(State(s.clone()), h.clone(), Query(q)).await,
                        "daily_trend" => admin::dashboard::handle_report_daily_trend(State(s.clone()), h.clone(), Query(q)).await,
                        _ => {
                            return Err((-32602, format!("Invalid params: unknown report kind {kind}")));
                        }
                    })
                })
            },
        ),
        // ── 账号余额 / token 刷新 / 模型目录 ──
        td(
            "aigx_account_balance",
            "账号余额",
            "只读：账号级余额/额度查询（qoder/autoclaw/小浣熊/zcode）",
            ReadOnly,
            "account_id",
            &["/api/account-sessions/{id}/balance"],
            account_id_args,
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    out(admin::account_sessions::handle_account_balance(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_refresh_token",
            "刷新账号 token",
            "写操作（可回滚）：刷新账号 access/refresh token（force=true 无条件刷）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/{id}/refresh"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "force": { "type": "boolean", "description": "true=无条件刷；默认仅临期才刷" }
                    },
                    "required": ["account_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "account_id")?;
                    let force = a.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
                    out(admin::account_sessions::handle_account_refresh(
                        State(s.clone()),
                        h.clone(),
                        Path(id),
                        Json(admin::account_sessions::RefreshBody { force }),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_catalog_sync",
            "同步模型目录",
            "写操作（可回滚）：同步账号上游模型目录（可按账号/供应商，apply 写回白名单）",
            LowRisk,
            "account_id",
            &["/api/account-sessions/catalog/sync"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "account_id": { "type": "string" },
                        "provider": { "type": "string" },
                        "force": { "type": "boolean", "description": "忽略缓存强制回源" },
                        "apply": { "type": "boolean", "description": "写回账号模型白名单" },
                        "replace": { "type": "boolean", "description": "apply 时整体替换而非合并" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::CatalogSyncRequest =
                        parse_tool_args("aigx_account_catalog_sync", a)?;
                    out(admin::account_sessions::handle_sync_account_catalog(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_catalog_providers",
            "目录支持供应商",
            "只读：支持模型目录发现的供应商清单",
            ReadOnly,
            "",
            &["/api/account-sessions/catalog/providers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_catalog_providers(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_rotation_get",
            "读账号轮换策略",
            "只读：读取账号池轮换/冷却策略",
            ReadOnly,
            "",
            &["/api/account-sessions/rotation"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_get_rotation_policy(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_rotation_set",
            "改账号轮换策略",
            "写操作（可回滚）：修改账号池轮换/冷却策略",
            LowRisk,
            "",
            &["/api/account-sessions/rotation"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "base_cooldown_secs": { "type": "integer" },
                        "max_cooldown_secs": { "type": "integer" },
                        "max_attempts": { "type": "integer" }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let body: admin::account_sessions::RotationPolicyRequest =
                        parse_tool_args("aigx_account_rotation_set", a)?;
                    out(admin::account_sessions::handle_set_rotation_policy(
                        State(s.clone()),
                        h.clone(),
                        Json(body),
                    )
                    .await)
                })
            },
        ),
        td(
            "aigx_account_providers",
            "账号供应商清单",
            "只读：账号池支持的供应商清单（57 家）",
            ReadOnly,
            "",
            &["/api/account-sessions/providers"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::account_sessions::handle_list_account_providers(
                        State(s.clone()),
                        h.clone(),
                    )
                    .await)
                })
            },
        ),
        // ── 能力自发现（新 API 落进 OpenAPI 文档即可被搜到）──
        td(
            "aigx_openapi_search",
            "搜索管理 API",
            "只读：按关键词搜索管理面 API 清单（路径/方法/摘要），用于发现尚未封装成工具的端点",
            ReadOnly,
            "",
            &["/api/openapi.json"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "关键词（路径/摘要/标签子串，可中文）" },
                        "tag": { "type": "string", "description": "按标签过滤（如 账号池/渠道/定价）" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "返回条数上限，默认 20" }
                    },
                    "required": ["query"]
                })
            },
            |_s, _h, a| {
                Box::pin(async move {
                    let query = require_arg(a, "query")?;
                    let tag = a.get("tag").and_then(|v| v.as_str()).map(str::to_string);
                    let limit = a.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                    let items = crate::api::admin::openapi::search_openapi(&query, tag.as_deref(), limit);
                    Ok(ToolOutcome {
                        text: crate::api::admin::common::truncate_tool_text(
                            json!({ "success": true, "data": { "query": query, "total": items.len(), "items": items } })
                                .to_string(),
                        ),
                        ok: true,
                    })
                })
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 管理域路由前缀：每个域必须被某个工具的 covers 命中（或进豁免表）。
    /// 新增管理端点时：给工具 covers 加前缀，或显式加进 EXEMPT_PREFIXES。
    const REQUIRED_DOMAINS: &[&str] = &[
        "/api/models/manage",
        "/api/models/state",
        "/api/models/price",
        "/api/models/alias",
        "/api/models/meta",
        "/api/models/available",
        "/api/checkin-center",
        "/api/dashboard/report",
        "/api/account-sessions",
        "/api/channels",
        "/api/users",
        "/api/tokens",
        "/api/keys",
        "/api/pricing",
        "/api/logs",
        "/api/diagnostics",
        "/api/update",
    ];

    /// 明确不给工具的域（人工维护的边界：认证/支付/工单/Agent 自身…）。
    const EXEMPT_PREFIXES: &[&str] = &[
        "/api/auth",
        "/api/payment",
        "/api/epay",
        "/api/stripe",
        "/api/topup",
        "/api/tickets",
        "/api/prompts",
        "/api/oauth",
        "/api/agent",
        "/api/mcp",
        "/api-docs",
        "/swagger-ui",
    ];

    #[test]
    fn openapi_domains_have_tool_coverage() {
        for domain in REQUIRED_DOMAINS {
            assert!(
                all_tools().iter().any(|t| t
                    .covers
                    .iter()
                    .any(|c| c.starts_with(domain) || domain.starts_with(c))),
                "管理域 {domain} 没有任何工具覆盖（请加工具 covers，或更新 REQUIRED_DOMAINS/EXEMPT_PREFIXES）"
            );
        }
        for domain in EXEMPT_PREFIXES {
            // 豁免域本意是「不提供这个域的功能」。
            //
            // 但「以该域为**数据源**」的工具不算提供功能 ——
            // `aigx_openapi_search` 的 covers 就是 `/api/openapi.json`：
            // 它读 OpenAPI 文档去**发现**别的端点，而不是提供「读 OpenAPI
            // 文档」这个能力。所以判据是「cover 落在该豁免域**之下**」
            // （比该域更长，即真的是它的子路径），而不是「等于该域」。
            assert!(
                !all_tools().iter().any(|t| t
                    .covers
                    .iter()
                    .any(|c| { c.starts_with(domain) && c.len() > domain.len() })),
                "豁免域 {domain} 被工具覆盖了（EXEMPT_PREFIXES 与 covers 矛盾）"
            );
        }
    }

    #[test]
    fn mcp_surface_is_non_high_risk_subset() {
        for t in mcp_tools() {
            assert_ne!(
                t.risk,
                RiskLevel::HighRisk,
                "MCP 不得暴露高危工具 {}",
                t.name
            );
        }
        // MCP tools/list 必须与注册表同源（不允许再养第二份白名单）
        let list = crate::api::admin::mcp::tools_list_result();
        let names: Vec<&str> = list["tools"]
            .as_array()
            .expect("tools/list 有 tools 数组")
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default())
            .collect();
        let expect: Vec<&str> = mcp_tools().map(|t| t.name).collect();
        assert_eq!(
            names, expect,
            "MCP tools/list 与 registry::mcp_tools 不一致"
        );
    }

    #[test]
    fn tool_defs_valid_and_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for t in all_tools() {
            assert!(t.name.starts_with("aigx_"), "{} 须 aigx_ 前缀", t.name);
            assert!(seen.insert(t.name), "工具名重复: {}", t.name);
            assert!(!t.label.is_empty(), "{} label 不能为空", t.name);
            assert!(!t.description.is_empty(), "{} description 不能为空", t.name);
            let tag = match t.risk {
                RiskLevel::ReadOnly => "只读",
                RiskLevel::LowRisk => "写操作",
                RiskLevel::HighRisk => "高危写",
            };
            assert!(
                t.description.contains(tag),
                "{} 描述未标注风险语义（应含「{tag}」）",
                t.name
            );
            let schema = (t.schema)();
            assert_eq!(schema["type"], "object", "{} schema.type", t.name);
            assert!(
                schema["properties"].is_object(),
                "{} schema.properties",
                t.name
            );
            if !t.target_key.is_empty() {
                assert!(
                    schema["properties"].get(t.target_key).is_some(),
                    "{} target_key={} 不在 schema.properties",
                    t.name,
                    t.target_key
                );
            }
            assert!(!t.covers.is_empty(), "{} 须声明 covers", t.name);
        }
    }
}
