//! Agent 工具注册表——管理面 API 的"手"。
//!
//! 三层风险分级（安全底线）：
//! - [`RiskLevel::ReadOnly`]：查/搜/统计/诊断，Agent 自由调用，无审批。
//! - [`RiskLevel::LowRisk`]：低危写（启停/测活/重置熔断/加密钥/加渠道/
//!   改渠道/账号池增改），Agent 直接做，自动审计留痕，可用对向工具回滚。
//! - [`RiskLevel::HighRisk`]：高危写（删用户/动余额/删订单/改价/清日志/
//!   删账号/批量导入），必须经审批矩阵（[`crate::agent::approval`]）人工确认。
//!
//! 执行方式：进程内直调现有管理面 handler（与 [`crate::api::admin::mcp`]
//! 同源思路——复用 verify_admin 校验、副作用、错误构造，不经 HTTP 端口）。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde_json::{json, Value};

use crate::api::admin::channels::ChannelRequest;
use crate::api::admin::legacy::GroupRequest;
use crate::api::admin::pricing::PriceRequest;
use crate::api::admin::tokens::KeyRequest;
use crate::api::admin::users::ManageUserRequest;
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
        ToolSpec {
            name: "aigx_diagnostics_summary",
            description: "只读：系统体检摘要（渠道统计/今日用量/24h 错误概况/运行信息）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_diagnostics_channels",
            description: "只读：启用渠道批量测活（并发探测，约需 30 秒）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_diagnostics_breakers",
            description: "只读：熔断器状态快照（三态/失败计数/冷却剩余）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        // ── 查询×11 ──
        ToolSpec {
            name: "aigx_list_channels",
            description: "只读：列出渠道（search 模糊过滤 + page/page_size 分页）",
            schema: json!({ "type":"object","properties":{ "search":{"type":"string","description":"按名称/类型/base_url/模型模糊过滤"}, "page":{"type":"integer","minimum":1}, "page_size":{"type":"integer","minimum":1} }, "required":[] }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_users",
            description: "只读：列出用户（分页，敏感字段脱敏）",
            schema: paged(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_keys",
            description: "只读：列出 API 密钥（脱敏）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_accounts",
            description: "只读：列出 CF 账号池（脱敏）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_orders",
            description: "只读：列出订单（keyword 过滤 + 分页）",
            schema: json!({ "type":"object","properties":{ "keyword":{"type":"string"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_plans",
            description: "只读：列出套餐模板",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_redemptions",
            description: "只读：列出兑换码（分页）",
            schema: paged(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_list_groups",
            description: "只读：列出用户分组",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_query_logs",
            description: "只读：查询请求日志（user/model/channel/start/end 过滤 + 分页）",
            schema: json!({ "type":"object","properties":{ "user":{"type":"string"}, "model":{"type":"string"}, "channel":{"type":"string"}, "start":{"type":"integer"}, "end":{"type":"integer"}, "page":{"type":"integer","minimum":1}, "size":{"type":"integer","minimum":1} }, "required":[] }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_get_pricing",
            description: "只读：列出全部模型定价",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_cost_report",
            description: "只读：成本/用量聚合报表（按模型统计调用量与成本）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        // ── 低危写×4（复用 mcp 已有的渠道写）──
        ToolSpec {
            name: "aigx_channel_enable",
            description: "写操作（可回滚）：启用渠道",
            schema: channel_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_channel_disable",
            description: "写操作（可回滚）：禁用渠道",
            schema: channel_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_channel_test",
            description: "写操作（可回滚）：测试渠道连通性并落库结果",
            schema: channel_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_reset_circuit_breaker",
            description: "写操作（可回滚）：重置渠道熔断器",
            schema: channel_id_args(),
            risk: RiskLevel::LowRisk,
        },
        // ── 低危写×2（新增：加渠道/加密钥，均可删）──
        ToolSpec {
            name: "aigx_channel_add",
            description: "写操作（可回滚）：新增渠道（name/base_url/api_key/type）",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "渠道名" },
                    "base_url": { "type": "string", "description": "上游 base URL" },
                    "api_key": { "type": "string", "description": "上游 API key" },
                    "channel_type": { "type": "string", "description": "openai_compatible/anthropic/gemini/zai/cloudflare/account（account = 账号型渠道，凭证在账号池里加）" }
                },
                "required": ["name", "base_url", "api_key"]
            }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_key_add",
            description: "写操作（可回滚）：新增 API 密钥",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "密钥名" },
                    "user_id": { "type": "string", "description": "归属用户 ID（可空）" },
                    "group": { "type": "string", "description": "分组（可空）" }
                },
                "required": ["name"]
            }),
            risk: RiskLevel::LowRisk,
        },
        // ── 高危写×9（审批矩阵，人工确认）──
        ToolSpec {
            name: "aigx_user_delete",
            description: "高危写：删除用户（不可逆，需审批）",
            schema: json!({
                "type": "object",
                "properties": { "user_id": { "type": "string", "description": "用户 ID" } },
                "required": ["user_id"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_user_manage",
            description: "高危写：启用/禁用用户（需审批）",
            schema: json!({
                "type": "object",
                "properties": {
                    "user_id": { "type": "string", "description": "用户 ID" },
                    "action": { "type": "string", "enum": ["enable", "disable"], "description": "enable/disable" }
                },
                "required": ["user_id", "action"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_channel_delete",
            description: "高危写：删除渠道（不可逆，需审批）",
            schema: json!({
                "type": "object",
                "properties": { "channel_id": { "type": "string", "description": "渠道 ID" } },
                "required": ["channel_id"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_order_delete",
            description: "高危写：删除订单（不可逆，需审批）",
            schema: json!({
                "type": "object",
                "properties": { "trade_no": { "type": "string", "description": "订单号" } },
                "required": ["trade_no"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_pricing_upsert",
            description: "高危写：新增/更新模型定价（需审批）",
            schema: json!({
                "type": "object",
                "properties": {
                    "model_name": { "type": "string" },
                    "input_price": { "type": "number" },
                    "output_price": { "type": "number" },
                    "price_type": { "type": "string", "description": "token/image 等" }
                },
                "required": ["model_name", "input_price", "output_price", "price_type"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_pricing_delete",
            description: "高危写：删除模型定价（需审批）",
            schema: json!({
                "type": "object",
                "properties": { "model_name": { "type": "string" } },
                "required": ["model_name"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_logs_cleanup",
            description: "高危写：清理日志（需审批）",
            schema: json!({
                "type": "object",
                "properties": { "days": { "type": "integer", "description": "保留天数" } },
                "required": []
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_key_delete",
            description: "高危写：删除 API 密钥（不可逆，需审批）",
            schema: json!({
                "type": "object",
                "properties": { "key_id": { "type": "string", "description": "密钥 ID" } },
                "required": ["key_id"]
            }),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_group_upsert",
            description: "高危写：新增/更新用户分组（需审批）",
            schema: json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "ratio": { "type": "number" }
                },
                "required": ["name", "ratio"]
            }),
            risk: RiskLevel::HighRisk,
        },
        // ── 账号池×6（账号型渠道的账号级调度：查/统计/测/启停/清冷却）──
        ToolSpec {
            name: "aigx_list_account_sessions",
            description: "只读：列出账号型渠道的账号（凭证/代理脱敏，可按渠道与关键词过滤）",
            schema: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "按渠道 ID 过滤" },
                    "search": { "type": "string", "description": "按名称/provider/地址/渠道名过滤" }
                },
                "required": []
            }),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_account_session_stats",
            description: "只读：按渠道聚合的账号池统计（总数/启用/冷却中/并发/成败）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_account_session_test",
            description: "写操作（可回滚）：单号探活（真实推理链路，成功后清该模型冷却）",
            schema: account_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_enable",
            description: "写操作（可回滚）：启用账号",
            schema: account_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_disable",
            description: "写操作（可回滚）：停用账号",
            schema: account_id_args(),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_reset_cooldown",
            description: "写操作（可回滚）：清除账号全部模型冷却与最近错误",
            schema: account_id_args(),
            risk: RiskLevel::LowRisk,
        },
        // ── 账号池写（AI 运维权限扩展：增/改/删/批量导入）──
        ToolSpec {
            name: "aigx_channel_update",
            description: "写操作（可回滚）：部分更新渠道（名称/地址/权重/优先级/模型/启停）",
            schema: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "渠道 ID" },
                    "name": { "type": "string" },
                    "base_url": { "type": "string" },
                    "priority": { "type": "integer" },
                    "weight": { "type": "integer", "minimum": 1 },
                    "models": { "type": "array", "items": { "type": "string" } },
                    "enabled": { "type": "boolean", "description": "true 启用 / false 停用" }
                },
                "required": ["channel_id"]
            }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_add",
            description: "写操作（可回滚）：账号型渠道下新增账号（凭证加密落库，可配代理/权重/并发/模型白名单）",
            schema: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "账号型渠道 ID" },
                    "name": { "type": "string", "description": "账号名（可空，缺省自动命名）" },
                    "provider": { "type": "string", "description": "上游类型（openai/anthropic/gemini/zai/cloudflare 等）" },
                    "base_url": { "type": "string", "description": "上游 base URL（可空 = 用渠道地址）" },
                    "api_key": { "type": "string", "description": "账号凭证" },
                    "proxy": { "type": "string", "description": "出口代理（可空）" },
                    "priority": { "type": "integer", "description": "优先级（越大越先用）" },
                    "weight": { "type": "integer", "minimum": 1 },
                    "max_concurrency": { "type": "integer", "minimum": 0, "description": "并发上限，0 = 不限" },
                    "models": { "type": "array", "items": { "type": "string" }, "description": "模型白名单（空 = 全部）" }
                },
                "required": ["channel_id", "api_key"]
            }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_update",
            description: "写操作（可回滚）：部分更新账号（名称/状态/优先级/权重/并发），不触碰凭证与地址",
            schema: json!({
                "type": "object",
                "properties": {
                    "account_id": { "type": "string", "description": "账号会话 ID" },
                    "name": { "type": "string" },
                    "status": { "type": "string", "enum": ["enabled", "disabled"] },
                    "priority": { "type": "integer" },
                    "weight": { "type": "integer", "minimum": 1 },
                    "max_concurrency": { "type": "integer", "minimum": 0 }
                },
                "required": ["account_id"]
            }),
            risk: RiskLevel::LowRisk,
        },
        ToolSpec {
            name: "aigx_account_session_delete",
            description: "高危写：删除账号（不可逆，需审批）",
            schema: account_id_args(),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_account_session_bulk_import",
            description: "高危写：批量导入账号凭证（每行一条，需审批）",
            schema: json!({
                "type": "object",
                "properties": {
                    "channel_id": { "type": "string", "description": "账号型渠道 ID" },
                    "provider": { "type": "string" },
                    "base_url": { "type": "string" },
                    "proxy": { "type": "string" },
                    "name_prefix": { "type": "string", "description": "账号名前缀（可空）" },
                    "priority": { "type": "integer" },
                    "weight": { "type": "integer" },
                    "max_concurrency": { "type": "integer" },
                    "models": { "type": "array", "items": { "type": "string" } },
                    "credentials": { "type": "string", "description": "每行一条：名称,Key 或仅 Key；# 开头与空行忽略" }
                },
                "required": ["channel_id", "credentials"]
            }),
            risk: RiskLevel::HighRisk,
        },
        // ── 服务器自动更新（查状态 / 检查 / 应用 / 重启 / 回滚）──
        ToolSpec {
            name: "aigx_update_status",
            description: "只读：服务器更新状态（当前/最新版本、下载进度、是否待重启、能否原地更新）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_check_update",
            description: "只读：立即检查 GitHub Release 是否有新版本（受最短间隔与限流冷却约束）",
            schema: no_args(),
            risk: RiskLevel::ReadOnly,
        },
        ToolSpec {
            name: "aigx_apply_update",
            description: "高危写：下载并应用最新版本（sha256 校验 → 替换二进制/static → 自动重启，服务会短暂中断）",
            schema: no_args(),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_restart_service",
            description: "高危写：重启 AIGX 服务（exec 自重启，约 2 秒后断开，期间请求会失败）",
            schema: no_args(),
            risk: RiskLevel::HighRisk,
        },
        ToolSpec {
            name: "aigx_rollback_update",
            description: "高危写：回滚到上一版本（使用 aigx.bak / static.bak，重启后生效）",
            schema: no_args(),
            risk: RiskLevel::HighRisk,
        },
    ]
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
            let q = crate::api::admin::mcp::parse_struct_args::<
                crate::api::admin::users::ListUsersQuery,
            >(name, args)?;
            handle_list_users(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_keys" => handle_list_keys(State(state.clone()), headers.clone()).await,
        "aigx_list_accounts" => handle_list_accounts(State(state.clone()), headers.clone()).await,
        "aigx_list_orders" => {
            let q = crate::api::admin::mcp::args_to_string_map(args);
            handle_list_orders(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_plans" => handle_list_plans(State(state.clone()), headers.clone()).await,
        "aigx_list_redemptions" => {
            let q = crate::api::admin::mcp::parse_struct_args::<
                crate::api::admin::redemptions::AuditLogQuery,
            >(name, args)?;
            handle_list_redemptions(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_list_groups" => handle_list_groups(State(state.clone()), headers.clone()).await,
        "aigx_query_logs" => {
            let q = crate::api::admin::mcp::parse_struct_args::<
                crate::api::admin::logs::RequestLogQuery,
            >(name, args)?;
            handle_list_request_logs(State(state.clone()), headers.clone(), Query(q)).await
        }
        "aigx_get_pricing" => handle_list_pricing(State(state.clone()), headers.clone()).await,
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
            admin::handle_reset_channel_circuit(State(state.clone()), headers.clone(), Path(id))
                .await
        }
        // ── 低危写（新增）──
        "aigx_channel_add" => {
            let body: ChannelRequest = parse_tool_args(name, args)?;
            admin::handle_add_channel(State(state.clone()), headers.clone(), Json(body)).await
        }
        "aigx_key_add" => {
            let body: KeyRequest = parse_tool_args(name, args)?;
            admin::handle_add_key(State(state.clone()), headers.clone(), Json(body)).await
        }
        // ── 高危写（runner 层经审批矩阵后调用）──
        "aigx_user_delete" => {
            let id = require_arg(args, "user_id")?;
            admin::handle_delete_user(State(state.clone()), headers.clone(), Path(id)).await
        }
        "aigx_user_manage" => {
            let body: ManageUserRequest = parse_tool_args(name, args)?;
            admin::handle_manage_user(State(state.clone()), headers.clone(), Json(body)).await
        }
        "aigx_channel_delete" => {
            let id = require_arg(args, "channel_id")?;
            // Agent 路径免密码：直调 core（删除 + 审计），不走 HTTP handler 的
            // 密码二次认证——高危工具已经审批矩阵人工确认（runner.rs），
            // 审批本身即同等强度的身份复核；此处仍有 verify_admin 兜底。
            admin::channels::delete_channel_core(state, headers, &id).await
        }
        "aigx_order_delete" => {
            let trade_no = require_arg(args, "trade_no")?;
            admin::handle_delete_order(State(state.clone()), headers.clone(), Path(trade_no)).await
        }
        "aigx_pricing_upsert" => {
            let body: PriceRequest = parse_tool_args(name, args)?;
            admin::handle_add_pricing(State(state.clone()), headers.clone(), Json(body)).await
        }
        "aigx_pricing_delete" => {
            let model_name = require_arg(args, "model_name")?;
            admin::handle_delete_pricing(State(state.clone()), headers.clone(), Path(model_name))
                .await
        }
        "aigx_logs_cleanup" => {
            let days = args.get("days").and_then(|v| v.as_u64());
            admin::handle_cleanup_logs(
                State(state.clone()),
                headers.clone(),
                Some(Json(crate::api::admin::logs::LogCleanupBody { days })),
            )
            .await
        }
        "aigx_key_delete" => {
            let id = require_arg(args, "key_id")?;
            admin::handle_delete_key(State(state.clone()), headers.clone(), Path(id)).await
        }
        "aigx_group_upsert" => {
            let body: GroupRequest = parse_tool_args(name, args)?;
            admin::handle_upsert_group(State(state.clone()), headers.clone(), Json(body)).await
        }
        // ── 账号池×6 ──
        "aigx_list_account_sessions" => {
            let q = crate::api::admin::mcp::args_to_string_map(args);
            admin::account_sessions::handle_list_account_sessions(
                State(state.clone()),
                headers.clone(),
                Query(q),
            )
            .await
        }
        "aigx_account_session_stats" => {
            admin::account_sessions::handle_account_session_stats(
                State(state.clone()),
                headers.clone(),
            )
            .await
        }
        "aigx_account_session_test" => {
            let id = require_arg(args, "account_id")?;
            admin::account_sessions::handle_test_account_session(
                State(state.clone()),
                headers.clone(),
                Path(id),
            )
            .await
        }
        "aigx_account_session_enable" => {
            let id = require_arg(args, "account_id")?;
            admin::account_sessions::handle_patch_account_session(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(admin::account_sessions::AccountSessionPatch::status_only(
                    "enabled",
                )),
            )
            .await
        }
        "aigx_account_session_disable" => {
            let id = require_arg(args, "account_id")?;
            admin::account_sessions::handle_patch_account_session(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(admin::account_sessions::AccountSessionPatch::status_only(
                    "disabled",
                )),
            )
            .await
        }
        "aigx_account_session_reset_cooldown" => {
            let id = require_arg(args, "account_id")?;
            admin::account_sessions::handle_reset_account_session_cooldowns(
                State(state.clone()),
                headers.clone(),
                Path(id),
            )
            .await
        }
        // ── 账号池写（权限扩展）──
        "aigx_channel_update" => {
            let id = require_channel_id(args)?;
            let mut patch = args.clone();
            if let Some(obj) = patch.as_object_mut() {
                // channel_id 走 Path，不进部分更新体
                obj.remove("channel_id");
            }
            admin::handle_patch_channel(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(patch),
            )
            .await
        }
        "aigx_account_session_add" => {
            // 账号必须有凭证，空 key 建号没有意义（前端表单同样禁止）
            require_arg(args, "api_key")?;
            let body: admin::account_sessions::AccountSessionRequest = parse_tool_args(name, args)?;
            admin::account_sessions::handle_create_account_session(
                State(state.clone()),
                headers.clone(),
                Json(body),
            )
            .await
        }
        "aigx_account_session_update" => {
            let id = require_arg(args, "account_id")?;
            let body: admin::account_sessions::AccountSessionPatch = parse_tool_args(name, args)?;
            admin::account_sessions::handle_patch_account_session(
                State(state.clone()),
                headers.clone(),
                Path(id),
                Json(body),
            )
            .await
        }
        "aigx_account_session_delete" => {
            let id = require_arg(args, "account_id")?;
            admin::account_sessions::handle_delete_account_session(
                State(state.clone()),
                headers.clone(),
                Path(id),
            )
            .await
        }
        "aigx_account_session_bulk_import" => {
            let body: admin::account_sessions::BulkImportRequest = parse_tool_args(name, args)?;
            admin::account_sessions::handle_bulk_import_account_sessions(
                State(state.clone()),
                headers.clone(),
                Json(body),
            )
            .await
        }
        // ── 服务器自动更新 ──
        "aigx_update_status" => {
            admin::update::handle_update_status(State(state.clone()), headers.clone()).await
        }
        "aigx_check_update" => {
            admin::update::handle_update_check(State(state.clone()), headers.clone()).await
        }
        "aigx_apply_update" => {
            admin::update::handle_update_apply(State(state.clone()), headers.clone()).await
        }
        "aigx_restart_service" => {
            admin::update::handle_update_restart(State(state.clone()), headers.clone()).await
        }
        "aigx_rollback_update" => {
            admin::update::handle_update_rollback(State(state.clone()), headers.clone()).await
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

/// 取必填字符串参数（缺失/非字符串 → -32602）。
fn require_arg(args: &Value, key: &str) -> Result<String, (i64, String)> {
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

/// 从工具参数反序列化为 handler 的请求体 struct（字段名同源，类型不符 → -32602）。
fn parse_tool_args<T: serde::de::DeserializeOwned>(
    tool: &str,
    args: &Value,
) -> Result<T, (i64, String)> {
    serde_json::from_value(args.clone())
        .map_err(|e| (-32602, format!("Invalid params for tool {tool}: {e}")))
}

fn handler_to_outcome(result: HandlerResult) -> ToolOutcome {
    match result {
        Ok(Json(v)) => ToolOutcome {
            text: v.to_string(),
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

fn require_channel_id(args: &Value) -> Result<String, (i64, String)> {
    args.get("channel_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .ok_or_else(|| {
            (
                -32602,
                "Invalid params: missing required argument channel_id".to_string(),
            )
        })
}

/// 成本/用量聚合报表（只读，阶段三：pricing × 请求日志联表）。
///
/// 从 `log_store.list_all()` 取全部请求日志，按模型聚合：
/// - requests：请求数
/// - input_tokens / output_tokens：token 用量
/// - cost：向用户收的销售价（配额单位）
/// - channel_cost：渠道成本（未配成本价时 = cost）
/// - profit：利润 = cost - channel_cost（仅当 channel_cost ≠ cost 时有意义）
async fn cost_report(state: &AppState) -> HandlerResult {
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
pub type ToolExecutor = Arc<
    dyn Fn(
            &AppState,
            &HeaderMap,
            &str,
            &Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolOutcome, (i64, String)>> + Send>,
        > + Send
        + Sync,
>;
