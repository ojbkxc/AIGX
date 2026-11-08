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
//! 新增工具只改本文件一条声明。管理面路由的覆盖对账由本文件 `tests` 的台账闸门
//! 负责，唯一数据源是**真实路由表**（`main.rs` + `agent/api.rs` 里注册的
//! `.route(…)`，口径与 CI 的 `tools/extract_routes.py` 对齐）：每条真实路由必须
//! 落在「工具 `covers` ∪ `EXEMPT_PREFIXES` ∪ `KNOWN_GAPS`」三处之一，都不落则
//! `every_real_route_is_registered` 会红；路径有写方法却只被 ReadOnly 工具 owns 的，
//! 由 `write_side_pending_matches_reality` 单独盯（`WRITE_SIDE_PENDING`）。
//! ——不再靠手写域清单（旧 `REQUIRED_DOMAINS` / `openapi_domains_have_tool_coverage`
//! 的病灶见 tests 开头：清单与路由表脱钩后，绿灯只代表清单没人改）。
//!
//! ## v2（#34）：`user_exec` 双执行体
//!
//! v1 只服务管理员域（单一 `exec` 直调管理面 handler）。v2 起每条工具可另声明
//! 用户域执行体 [`ToolDef::user_exec`]，形成双执行体：
//!
//! - **管理员**：走 [`exec_tool`]（registry `exec`，内部 `verify_admin`），原状不变；
//! - **普通用户**：先过判定层 [`super::user_tool_policy::user_tool_allowed`]
//!   （组 allowlist + risk 封顶 LowRisk），放行后经 [`exec_user_tool`] 入口执行
//!   `user_exec`（内部 `verify_user` + `user_id` 数据域过滤）；HighRisk 在执行侧
//!   二次拦截（下发即过滤 + 执行兜底双保险）。写工具执行前另有二次确认门
//!   （[`super::confirm`]，P0 铁律不可降级）。
//!
//! 相关演进：f263249c 权限存储与判定层 → 392a1c99 双身份工具回路 →
//! ea13cb64 写工具二次确认门。

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
    /// 管理员域执行体（直调管理面 handler，内部 `verify_admin`）。
    pub exec: ExecFn,
    /// 终端用户域执行体（#34 双执行体）：**用户域 handler**——内部 `verify_user`
    /// 身份校验 + `user_id` 数据域过滤，只读写调用者自己的数据；禁直接构造 HTTP
    /// （网络出口仅限 channel/bridge）；结果一律过 [`handler_to_outcome`]（16K 截断）。
    ///
    /// `None` = 该工具对终端用户不可见（工具作者显式声明才对用户暴露）；
    /// 即便声明了，判定层仍会按「risk 封顶 LowRisk + 组 allowlist」二次过滤。
    pub user_exec: Option<ExecFn>,
}

/// 声明构造器（保持调用点紧凑）。`user_exec` 默认为 `None`——
/// 需要用户域执行体的工具用 [`ToolDef::user_exec`] 链式声明。
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
        user_exec: None,
    }
}

impl ToolDef {
    /// 声明终端用户域执行体（构建期链式调用；避免改动 [`td`] 的既有调用点）。
    #[must_use]
    pub fn user_exec(mut self, f: ExecFn) -> Self {
        self.user_exec = Some(f);
        self
    }

    /// 是否对终端用户暴露：声明了用户域执行体且非高危（D2 硬封顶）。
    pub fn is_user_exposed(&self) -> bool {
        self.user_exec.is_some() && self.risk != RiskLevel::HighRisk
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

// ── 终端用户域执行体（#34 双执行体）────────────────────────────────
//
// 执行约束（设计稿 §5.3）：内部 `verify_user` 身份校验 + `user_id` 数据域限定；
// 禁直接构造 HTTP（网络出口仅限 channel/bridge）；结果一律过 `handler_to_outcome`
// （16K 截断 + 脱敏）。只服务终端用户会话——管理员会话仍走各自的 `exec`。

/// 用户域日志分页上限（收紧：用户侧一次最多 20 条；管理员面不设此限）。
const USER_LOG_PAGE_MAX: usize = 20;

/// 用户域日志窗口天数上限。
const USER_LOG_DAYS_MAX: i64 = 30;

/// 用户域：查本人额度/用量（`WHERE user_id = ?`；近 30 天请求数与消耗）。
async fn user_usage_domain(state: &AppState, headers: &HeaderMap) -> HandlerResult {
    let u = crate::api::admin::common::verify_user(state, headers).await?;
    let since = chrono::Utc::now().timestamp() - 30 * 86_400;
    let logs = state
        .log_store
        .requests
        .list_by_user(&u.id, Some(since), None);
    let input_tokens: u64 = logs.iter().map(|l| l.input_tokens).sum();
    let output_tokens: u64 = logs.iter().map(|l| l.output_tokens).sum();
    let cost: i64 = logs.iter().map(|l| l.cost).sum();
    Ok(Json(json!({
        "success": true,
        "data": {
            "user_id": u.id,
            "quota": u.quota,
            "used_quota": u.used_quota,
            "remaining": u.remaining(),
            "window_days": 30,
            "requests": logs.len(),
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "cost": cost,
        }
    })))
}

/// 用户域：查本人请求日志（`user_id` 限定 + 分页/窗口收紧 + 剥离管理员排障字段）。
///
/// 与管理员口径的 `aigx_query_logs` 差异：不接受 `user`/`channel` 过滤（恒为本人），
/// 不回传渠道名/IP/原始请求名（`log::RequestLog` 注释明确「用户不展示」）。
async fn user_logs_domain(state: &AppState, headers: &HeaderMap, args: &Value) -> HandlerResult {
    let u = crate::api::admin::common::verify_user(state, headers).await?;
    let page = args
        .get("page")
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
        .max(1) as usize;
    let size = args
        .get("size")
        .and_then(|v| v.as_u64())
        .unwrap_or(USER_LOG_PAGE_MAX as u64)
        .clamp(1, USER_LOG_PAGE_MAX as u64) as usize;
    let days = args
        .get("days")
        .and_then(|v| v.as_i64())
        .unwrap_or(7)
        .clamp(1, USER_LOG_DAYS_MAX);
    let since = chrono::Utc::now().timestamp() - days * 86_400;
    let logs = state
        .log_store
        .requests
        .list_by_user(&u.id, Some(since), None);
    let total = logs.len();
    let rows: Vec<Value> = logs
        .into_iter()
        .skip((page - 1).saturating_mul(size))
        .take(size)
        .map(|l| {
            json!({
                "ts": l.created_at,
                "model": l.model,
                "status": l.status_code,
                "input_tokens": l.input_tokens,
                "output_tokens": l.output_tokens,
                "latency_ms": l.latency_ms,
                "cost": l.cost,
                "error": l.error_msg,
            })
        })
        .collect();
    Ok(Json(json!({
        "success": true,
        "data": { "total": total, "page": page, "size": size, "days": days, "logs": rows }
    })))
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
///
/// 本函数**不看 `disabled_tools`**——它是「注册表口径」而非「下发口径」；
/// 需要管理员开关参与时用 [`visible_tools`] / [`find_visible_tool`]。
pub fn mcp_tools() -> impl Iterator<Item = &'static ToolDef> {
    all_tools().iter().filter(|t| t.risk != RiskLevel::HighRisk)
}

/// `disabled_tools` 命中判定——Agent(runner)/MCP/assistant 三面共用的唯一口径（#35 批1 后半）。
///
/// 全等比较、不放宽：`api/admin/settings.rs` 的 PUT 已对每项 trim + 去空，工具名是
/// `aigx_*` 蛇形全名；改成前缀/忽略大小写匹配只会误伤邻近工具（禁 `aigx_key_add`
/// 顺带干掉 `aigx_key_delete` 这类事故）。
pub fn tool_disabled(name: &str, disabled: &[String]) -> bool {
    disabled.iter().any(|d| d == name)
}

/// 下发给外部消费方的「可见集」= 非高危 ∩ 未被管理员禁用。
///
/// 「关了就是关了」：可见集与 [`find_visible_tool`] 的准入判定同源，因此不存在
/// 「列表里没了但还能调」或「列表里还在但调不动」两种分叉。
pub fn visible_tools(disabled: &[String]) -> Vec<&'static ToolDef> {
    mcp_tools()
        .filter(|t| !tool_disabled(t.name, disabled))
        .collect()
}

/// 准入查找（fail-closed）：不存在 / 高危 / 被禁用三种情况一律 `None`。
pub fn find_visible_tool(name: &str, disabled: &[String]) -> Option<&'static ToolDef> {
    find_tool(name).filter(|t| t.risk != RiskLevel::HighRisk && !tool_disabled(t.name, disabled))
}

/// OpenAI function-calling 的 `tools` 数组（Agent 用，**全量含高危**——
/// Agent 面有审批矩阵兜底；聊天助手面用 [`openai_tools_of`] 传过滤子集，
/// 不要改本函数语义）。
pub fn openai_tools() -> Vec<Value> {
    openai_tools_of(all_tools().iter())
}

/// [`openai_tools`] 的可过滤构造层（#34：assistant 只传非 HighRisk 子集）。
pub fn openai_tools_of(tools: impl Iterator<Item = &'static ToolDef>) -> Vec<Value> {
    tools
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

/// 终端用户可下发的工具集（#34「下发即过滤」）：声明了 `user_exec` 且非高危。
///
/// 组 allowlist 判定在此之上再筛一层；HighRisk 封顶在这里与判定层双重生效
/// （双保险，任一失效都不至于把高危工具漏给终端用户）。
pub fn user_exposed_tools() -> impl Iterator<Item = &'static ToolDef> {
    all_tools().iter().filter(|t| t.is_user_exposed())
}

/// 终端用户工具执行入口：只走 `user_exec`（与 [`exec_tool`] 的管理员执行体并列）。
///
/// 未知工具 / 未声明用户域执行体 / 高危 → 拒绝（执行侧兜底，不依赖调用方预筛）。
pub async fn exec_user_tool(
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
    let Some(user_exec) = tool.user_exec else {
        return Err((
            -32601,
            format!("Tool {name} has no user execution body (not available to end users)"),
        ));
    };
    if tool.risk == RiskLevel::HighRisk {
        return Err((
            -32603,
            format!("Tool {name} is high-risk and never exposed to end users"),
        ));
    }
    user_exec(state, headers, args).await
}

// ── #35 批1 执行体辅助 ─────────────────────────────────────────────

/// 把「挂起人工审批数」注入只读视图：优先落进 `data` 段，没有 `data` 对象
/// 才落顶层（`/api/assistant/tools` 这类自带结构的视图保持原形状）。
fn with_pending_approvals(mut v: Value, pending: usize) -> Value {
    let obj = if v.get("data").and_then(Value::as_object).is_some() {
        v.get_mut("data").and_then(Value::as_object_mut)
    } else {
        v.as_object_mut()
    };
    if let Some(obj) = obj {
        obj.insert("pendingApprovals".to_string(), json!(pending));
    }
    v
}

/// `GET /api/scheduled-tasks` 的执行体（[`admin::scheduled_tasks::list_handler`]）返回
/// `Result<Response, ...>`（已 `into_response`，进不了 [`out`] 的 `HandlerResult`）。
/// 这里按其**同款口径**（`verify_admin` + `scheduled_tasks::list`）组一份 JSON 信封，
/// 语义与路由一致——不是绕开鉴权，是绕开 `Response` 装箱。
async fn scheduled_tasks_status(state: &AppState, headers: &HeaderMap) -> HandlerResult {
    crate::api::admin::common::verify_admin(state, headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": crate::scheduled_tasks::list(state).await,
    })))
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
        )
        .user_exec(|s, h, a| Box::pin(async move { out(user_logs_domain(s, h, a).await) })),
        td(
            "aigx_query_usage",
            "本人额度与用量",
            "只读：当前调用者的额度与近 30 天用量（配额/已用/剩余/请求数/tokens）",
            ReadOnly,
            "",
            &["/api/user/records"],
            no_args,
            |s, h, _a| Box::pin(async move { out(user_usage_domain(s, h).await) }),
        )
        .user_exec(|s, h, _a| Box::pin(async move { out(user_usage_domain(s, h).await) })),
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
            "新增访问令牌",
            "写操作（可回滚）：新增 API 访问令牌（POST /api/tokens；#35 批1 收敛：原指向无路由的 \
             legacy /api/keys，现与 `aigx_list_tokens` 同数据源 api_key_store）",
            LowRisk,
            "name",
            &["/api/tokens"],
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
                    out(admin::handle_add_token(State(s.clone()), h.clone(), Json(body)).await)
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
            "删除访问令牌",
            "高危写：删除 API 访问令牌（DELETE /api/tokens/{id}；不可逆，需审批。#35 批1 收敛：原指向无路由的 legacy /api/keys/{id}）",
            HighRisk,
            "key_id",
            &["/api/tokens/{id}"],
            || {
                json!({
                    "type": "object",
                    "properties": { "key_id": { "type": "string", "description": "令牌 ID" } },
                    "required": ["key_id"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_arg(a, "key_id")?;
                    out(admin::handle_delete_token(State(s.clone()), h.clone(), Path(id)).await)
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
                        // Agent 侧一律全清（不带按模型清除的 body）
                        None,
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
        )
        // #34 用户域：只读公开目录（`handle_available_models` 内部 verify_user +
        // 用户分组模型白名单过滤，与管理员面共用同一份可用模型口径）。
        .user_exec(|s, h, _a| {
            Box::pin(async move {
                out(admin::handle_available_models(State(s.clone()), h.clone()).await)
            })
        }),
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
        )
        // #34 用户域：本人每日签到（`handle_do_checkin` 内部 verify_user，只给
        // 自己入账；管理面的账号池签到走上面的 `exec`，两条执行体互不串域）。
        .user_exec(|s, h, _a| {
            Box::pin(async move { out(admin::handle_do_checkin(State(s.clone()), h.clone()).await) })
        }),
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
        // ══ #35 批1：只读观测补齐（按域聚合，一条工具 owns 多条同域路由）══
        //
        // 本段全部是「既有管理面 GET handler 直调」：不新增路由、不改 handler、
        // 不引入写侧。写侧（POST/PUT/DELETE）按批 2 的高危审批基线逐条论证。
        // 分派口径：`view`/`key` 走 enum 白名单（防模型瞎传），未知值直接 -32602。
        td(
            "aigx_dashboard_stats",
            "运维大盘视图",
            "只读：运维大盘视图（view 选 realtime/channelHealth/consumptionTrend/modelDistribution/cacheSavings/userRanking/schedulerStatus/cacheTrend24h；days 或 start/end 决定窗口）",
            ReadOnly,
            "view",
            &[
                "/api/dashboard/realtime",
                "/api/dashboard/channel_health",
                "/api/dashboard/consumption_trend",
                "/api/dashboard/model_distribution",
                "/api/dashboard/cache_savings",
                "/api/dashboard/user_ranking",
                "/api/dashboard/scheduler_status",
                "/api/dashboard/report/cache_trend_24h",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["realtime","channelHealth","consumptionTrend","modelDistribution","cacheSavings","userRanking","schedulerStatus","cacheTrend24h"], "description": "视图种类" },
                        "days": { "type": "integer", "minimum": 1, "maximum": 365, "description": "时间范围（天），默认 30；仅趋势/分布/健康/排行视图消费" },
                        "start": { "type": "string", "description": "起始日 YYYY-MM-DD（给了就覆盖 days）" },
                        "end": { "type": "string", "description": "结束日 YYYY-MM-DD" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::dashboard::DashboardQuery =
                        parse_tool_args("aigx_dashboard_stats", a)?;
                    out(match view.as_str() {
                        "realtime" => admin::handle_realtime(State(s.clone()), h.clone()).await,
                        "channelHealth" => admin::handle_channel_health(State(s.clone()), h.clone(), Query(q)).await,
                        "consumptionTrend" => admin::handle_consumption_trend(State(s.clone()), h.clone(), Query(q)).await,
                        "modelDistribution" => admin::handle_model_distribution(State(s.clone()), h.clone(), Query(q)).await,
                        "cacheSavings" => admin::handle_cache_savings(State(s.clone()), h.clone(), Query(q)).await,
                        "userRanking" => admin::handle_user_ranking(State(s.clone()), h.clone(), Query(q)).await,
                        "schedulerStatus" => admin::handle_scheduler_status(State(s.clone()), h.clone()).await,
                        "cacheTrend24h" => admin::handle_report_cache_trend_24h(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_system_health",
            "系统资源与健康",
            "只读：系统资源快照与健康检查（view 选 system=CPU/内存/负载进程、app=精简健康态、probe=healthz 存活探针）",
            ReadOnly,
            "view",
            &[
                "/api/monitor/system",
                "/api/monitor/health",
                "/api/monitor/healthz",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["system", "app", "probe"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "system" => admin::handle_monitor_system(State(s.clone()), h.clone()).await,
                        "app" => admin::handle_health(State(s.clone()), h.clone()).await,
                        "probe" => admin::handle_healthz(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_security_watch",
            "安全事件观测",
            "只读：安全汇总与安全事件流（view 选 summary/events；events 可按 type/range 过滤并分页）",
            ReadOnly,
            "view",
            &["/api/monitor/security", "/api/monitor/security/events"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["summary", "events"], "description": "视图种类" },
                        "type": { "type": "string", "description": "事件类型过滤（仅 events）" },
                        "range": { "type": "string", "description": "时间范围（仅 events，如 today/7d）" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::security::SecurityEventsQuery =
                        parse_tool_args("aigx_security_watch", a)?;
                    out(match view.as_str() {
                        "summary" => admin::handle_security_summary(State(s.clone()), h.clone()).await,
                        "events" => admin::handle_security_events(State(s.clone()), h.clone(), Query(q)).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_log_watch",
            "运行日志与审计",
            "只读：运行日志/在途请求/审计日志/清理预演（view 选 runtime/inflight/audits/clearPreview；runtime 用 since_id 增量拉，audits 分页，clearPreview 只统计不落刀）",
            ReadOnly,
            "view",
            &[
                "/api/logs/runtime",
                "/api/logs/inflight",
                "/api/logs/audits",
                "/api/logs/requests/clear-preview",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["runtime", "inflight", "audits", "clearPreview"], "description": "视图种类" },
                        "since_id": { "type": "integer", "minimum": 0, "description": "运行日志未读水位（只回 id 大于此值的行）" },
                        "page": { "type": "integer", "minimum": 1, "description": "审计日志页码" },
                        "size": { "type": "integer", "minimum": 1, "description": "审计日志每页条数" },
                        "days": { "type": "integer", "minimum": 0, "description": "clearPreview 只统计最近 N 天（0/缺省=全部）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "runtime" => {
                            let q: admin::logs::RuntimeLogsQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_get_runtime_logs(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "inflight" => admin::handle_list_inflight(State(s.clone()), h.clone()).await,
                        "audits" => {
                            let q: admin::logs::AuditLogQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_list_audit_logs(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "clearPreview" => {
                            let q: admin::logs::ClearPreviewQuery =
                                parse_tool_args("aigx_log_watch", a)?;
                            admin::handle_clear_preview(State(s.clone()), h.clone(), Query(q)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_alert_console",
            "告警台",
            "只读：当前活跃告警与历史告警（view 选 active/history；history 可 limit 限条数，上限 500）",
            ReadOnly,
            "view",
            &["/api/alerts/active", "/api/alerts/history"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["active", "history"], "description": "视图种类" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "history 返回条数，默认 100" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let q: admin::legacy::AlertHistoryQuery =
                        parse_tool_args("aigx_alert_console", a)?;
                    out(match view.as_str() {
                        "active" => admin::handle_alerts_active(State(s.clone()), h.clone()).await,
                        "history" => admin::handle_alerts_history(State(s.clone()), h.clone(), Query(q)).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_plugin_status",
            "插件/内置供应商目录状态",
            "只读：插件清单与在途/更新状态（view 选 list/progress/updates/release；内置 provider 目录随版本发布，progress/updates/release 为空态说明）",
            ReadOnly,
            "view",
            &[
                "/api/plugins",
                "/api/plugins/progress",
                "/api/plugins/update",
                "/api/plugins/release",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["list", "progress", "updates", "release"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "list" => admin::plugins::handle_list_plugins(State(s.clone()), h.clone()).await,
                        "progress" => {
                            admin::plugins::handle_get_plugin_progress(State(s.clone()), h.clone()).await
                        }
                        "updates" => {
                            admin::plugins::handle_check_plugin_updates(State(s.clone()), h.clone()).await
                        }
                        "release" => {
                            admin::plugins::handle_refresh_plugin_release(State(s.clone()), h.clone()).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_usage_analytics",
            "用量分析",
            "只读：用量汇总/趋势/按模型分布（view 选 summary/trend/models；summary 可带 user/channel/model 等过滤参数）",
            ReadOnly,
            "view",
            &["/api/usage/summary", "/api/usage/trend", "/api/usage/models"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["summary", "trend", "models"], "description": "视图种类" },
                        "user": { "type": "string", "description": "按用户过滤（仅 summary）" },
                        "channel": { "type": "string", "description": "按渠道过滤（仅 summary）" },
                        "model": { "type": "string", "description": "按模型过滤（仅 summary）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "summary" => {
                            let q = crate::api::admin::mcp::args_to_string_map(a);
                            admin::handle_usage_summary(State(s.clone()), h.clone(), Query(q)).await
                        }
                        "trend" => admin::handle_usage_trend(State(s.clone()), h.clone()).await,
                        "models" => admin::handle_usage_models(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_network_status",
            "网络层状态",
            "只读：网络层（CF 账号池）状态与账号明细（view 选 status/accounts；含池规模/健康统计/活跃数，仅管理员）",
            ReadOnly,
            "view",
            &["/api/network/status", "/api/network/accounts"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["status", "accounts"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "status" => admin::health_check(State(s.clone()), h.clone()).await,
                        "accounts" => admin::list_network_accounts(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_routing_watch",
            "路由决策观测",
            "只读：粘性映射/排队快照/单请求路由回放（view 选 sticky/queue/replay；replay 需 request_id，`route_replay` 开关关闭的历史请求回放字段为缺省）",
            ReadOnly,
            "view",
            &[
                "/api/routing/sticky",
                "/api/routing/queue",
                "/api/routing/replay/{request_id}",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["sticky", "queue", "replay"], "description": "视图种类" },
                        "request_id": { "type": "string", "description": "回放目标请求 ID（view=replay 必填）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "sticky" | "queue" => {
                            let q = crate::api::admin::mcp::args_to_string_map(a);
                            if view == "sticky" {
                                admin::handle_get_route_sticky(State(s.clone()), h.clone(), Query(q)).await
                            } else {
                                admin::handle_routing_queue(State(s.clone()), h.clone(), Query(q)).await
                            }
                        }
                        "replay" => {
                            let id = require_arg(a, "request_id")?;
                            admin::logs::handle_get_route_replay(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_upstream_watch",
            "上游账号池接入观测",
            "只读：上游账号（Agent2API 型）接入侧观测（view 选 connections/importSources/desktopSources/usageSnapshot；不含登录流程与导出）",
            ReadOnly,
            "view",
            &[
                "/api/account-sessions/connections",
                "/api/account-sessions/import/sources",
                "/api/account-sessions/desktop/sources",
                "/api/account-sessions/usage/snapshot",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["connections", "importSources", "desktopSources", "usageSnapshot"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "connections" => {
                            admin::account_sessions::handle_account_session_connections(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "importSources" => {
                            admin::account_sessions::handle_scan_import_sources(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "desktopSources" => {
                            admin::desktop_import::handle_desktop_sources(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        "usageSnapshot" => {
                            admin::account_sessions::handle_usage_snapshot(
                                State(s.clone()),
                                h.clone(),
                            )
                            .await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_subscription_watch",
            "订阅与套餐观测",
            "只读：套餐清单/调用者自身订阅/指定用户的订阅（view 选 plans/self/userSubscriptions；后者需 user_id）",
            ReadOnly,
            "view",
            &[
                "/api/subscription/plans",
                "/api/subscription/self",
                "/api/subscription/admin/users/{id}/subscriptions",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["plans", "self", "userSubscriptions"], "description": "视图种类" },
                        "user_id": { "type": "string", "description": "用户 ID（view=userSubscriptions 必填）" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "plans" => admin::handle_subscription_plans(State(s.clone()), h.clone()).await,
                        "self" => admin::handle_subscription_self(State(s.clone()), h.clone()).await,
                        "userSubscriptions" => {
                            let id = require_arg(a, "user_id")?;
                            admin::handle_subscription_admin_list(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_channel_detail",
            "渠道明细",
            "只读：单渠道上游余额与健康归档（view 选 balance/healthArchive，需 channel_id；与 `aigx_account_balance`（账号池侧）分属两个对象）",
            ReadOnly,
            "channel_id",
            &["/api/channels/{id}/balance", "/api/channels/{id}/health-archive"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "channel_id": { "type": "string", "description": "渠道 ID" },
                        "view": { "type": "string", "enum": ["balance", "healthArchive"], "description": "视图种类" }
                    },
                    "required": ["channel_id", "view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let id = require_channel_id(a)?;
                    let view = require_arg(a, "view")?;
                    out(match view.as_str() {
                        "balance" => admin::handle_get_channel_balance(State(s.clone()), h.clone(), Path(id)).await,
                        "healthArchive" => {
                            admin::handle_channel_health_archive(State(s.clone()), h.clone(), Path(id)).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    })
                })
            },
        ),
        td(
            "aigx_config_view",
            "配置现状视图",
            "只读：按 key 读一份配置/现状视图（全局热生效配置的读取面；写侧另有一批工具，本工具不改任何东西）。key 取值：settings=模型映射、limits=限额、securityPolicy=安全策略、sanitize=脱敏规则、gatewaySettings/gatewayPrompt=网关设置与系统提示词、updateSettings=更新设置、logsRetention=日志保留、queue=队列、ratelimit=限流、ratios=倍率、ipFilter=IP 黑白名单、notify=通知配置（凭据字段已脱敏）、clineHeaders=转发头、userToolsPolicy/userToolsCatalog=终端用户工具面、routing/routingDefaults/routingProfiles=路由策略、alertsRules=告警规则、pricingSyncConfig/exchangeRates=定价同步与汇率、modelMetadata=模型元数据、debug/debugTraffic=调试开关与在途抓包、reportSettings=报表时区偏移与存量口径、checkinKeepalive/checkinBenefits=签到保活与权益",
            ReadOnly,
            "key",
            &[
                "/api/settings",
                "/api/limits",
                "/api/security/policy",
                "/api/sanitize",
                "/api/gateway/settings",
                "/api/gateway/prompt",
                "/api/update/settings",
                "/api/logs/retention",
                "/api/queue",
                "/api/ratelimit/config",
                "/api/ratios",
                "/api/ip/filter",
                "/api/notify/config",
                "/api/cline-headers",
                "/api/user-tools/policy",
                "/api/user-tools/catalog",
                "/api/routing",
                "/api/routing/defaults",
                "/api/routing/profiles",
                "/api/alerts/rules",
                "/api/pricing/sync-config",
                "/api/pricing/exchange-rates",
                "/api/models/metadata",
                "/api/debug",
                "/api/debug/traffic",
                "/api/report/settings",
                "/api/checkin-center/benefits/keepalive",
                "/api/checkin-center/benefits",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "key": { "type": "string", "enum": [
                            "settings","limits","securityPolicy","sanitize","gatewaySettings","gatewayPrompt",
                            "updateSettings","logsRetention","queue","ratelimit","ratios","ipFilter","notify",
                            "clineHeaders","userToolsPolicy","userToolsCatalog","routing","routingDefaults",
                            "routingProfiles","alertsRules","pricingSyncConfig","exchangeRates","modelMetadata",
                            "debug","debugTraffic","reportSettings","checkinKeepalive","checkinBenefits"
                        ], "description": "配置视图 key" }
                    },
                    "required": ["key"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let key = require_arg(a, "key")?;
                    out(match key.as_str() {
                        "settings" => admin::handle_get_settings(State(s.clone()), h.clone()).await,
                        "limits" => admin::handle_get_limits(State(s.clone()), h.clone()).await,
                        "securityPolicy" => admin::handle_get_security_policy(State(s.clone()), h.clone()).await,
                        "sanitize" => admin::handle_get_sanitize(State(s.clone()), h.clone()).await,
                        "gatewaySettings" => admin::handle_get_gateway_settings(State(s.clone()), h.clone()).await,
                        "gatewayPrompt" => admin::handle_get_gateway_prompt(State(s.clone()), h.clone()).await,
                        "updateSettings" => {
                            admin::update::handle_get_update_settings(State(s.clone()), h.clone()).await
                        }
                        "logsRetention" => admin::handle_get_log_retention(State(s.clone()), h.clone()).await,
                        "queue" => admin::handle_get_queue(State(s.clone()), h.clone()).await,
                        "ratelimit" => admin::handle_get_ratelimit_config(State(s.clone()), h.clone()).await,
                        "ratios" => admin::handle_get_ratios(State(s.clone()), h.clone()).await,
                        "ipFilter" => admin::handle_get_ip_filter(State(s.clone()), h.clone()).await,
                        "notify" => admin::handle_get_notify_config(State(s.clone()), h.clone()).await,
                        "clineHeaders" => admin::handle_get_cline_headers(State(s.clone()), h.clone()).await,
                        "userToolsPolicy" => {
                            admin::user_tools::handle_get_user_tool_policy(State(s.clone()), h.clone()).await
                        }
                        "userToolsCatalog" => {
                            admin::user_tools::handle_get_user_tool_catalog(State(s.clone()), h.clone()).await
                        }
                        "routing" => admin::handle_get_routing(State(s.clone()), h.clone()).await,
                        "routingDefaults" => admin::handle_routing_defaults(State(s.clone()), h.clone()).await,
                        "routingProfiles" => {
                            admin::handle_get_routing_profiles(State(s.clone()), h.clone()).await
                        }
                        "alertsRules" => admin::handle_alert_rules_list(State(s.clone()), h.clone()).await,
                        "pricingSyncConfig" => {
                            admin::handle_get_price_sync_config(State(s.clone()), h.clone()).await
                        }
                        "exchangeRates" => admin::handle_get_exchange_rates(State(s.clone()), h.clone()).await,
                        "modelMetadata" => {
                            admin::handle_get_model_metadata(State(s.clone()), h.clone()).await
                        }
                        "debug" => admin::handle_get_debug(State(s.clone()), h.clone()).await,
                        "debugTraffic" => admin::handle_get_debug_traffic(State(s.clone()), h.clone()).await,
                        "reportSettings" => {
                            admin::handle_get_report_settings(State(s.clone()), h.clone()).await
                        }
                        "checkinKeepalive" => admin::handle_get_keepalive(State(s.clone()), h.clone()).await,
                        "checkinBenefits" => admin::handle_benefits_list(State(s.clone()), h.clone()).await,
                        _ => return Err((-32602, format!("Invalid params: unknown config key {key}"))),
                    })
                })
            },
        ),
        td(
            "aigx_agent_self_status",
            "助手自身状态",
            "只读：AI 运维助手自身的运行态（view 选 runtime=生效配置+disabledTools+effectiveModel、saved=设置页保存值、assistant=聊天助手工具面）。附带 pendingApprovals（当前挂起的人工审批数）——AI 看不到自己的开关与审批队列是运维盲区（#35 批1）。**不提供写侧**：助手不改自己的边界",
            ReadOnly,
            "view",
            &[
                "/api/agent/config",
                "/api/agent/settings",
                "/api/assistant/tools",
            ],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "view": { "type": "string", "enum": ["runtime", "saved", "assistant"], "description": "视图种类" }
                    },
                    "required": ["view"]
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let view = require_arg(a, "view")?;
                    let pending = s
                        .agent_state
                        .as_deref()
                        .map(|agent| agent.approvals.pending_count())
                        .unwrap_or(0);
                    let r = match view.as_str() {
                        "runtime" => {
                            crate::agent::api::handle_get_config(State(s.clone()), h.clone()).await
                        }
                        "saved" => admin::handle_get_agent_settings(State(s.clone()), h.clone()).await,
                        "assistant" => {
                            admin::assistant::handle_assistant_tools(State(s.clone()), h.clone()).await
                        }
                        _ => return Err((-32602, format!("Invalid params: unknown view {view}"))),
                    };
                    let r = match r {
                        Ok(Json(v)) => Json(with_pending_approvals(v, pending)),
                        e => return out(e),
                    };
                    out(Ok(r))
                })
            },
        ),
        td(
            "aigx_scheduled_task_list",
            "定时任务清单",
            "只读：全部间隔型定时任务的开关/间隔/上次与下次运行状态（触发与改配置是写侧，批 2 走审批）",
            ReadOnly,
            "",
            &["/api/scheduled-tasks"],
            no_args,
            |s, h, _a| Box::pin(async move { out(scheduled_tasks_status(s, h).await) }),
        ),
        td(
            "aigx_proxy_pool_status",
            "代理池清单",
            "只读：代理池条目与可用性统计（增删改/测活/sync-clash 属写侧，批 2）",
            ReadOnly,
            "",
            &["/api/proxy-pool"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::proxy_pool_api::handle_pool_list(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_cache_stats",
            "缓存命中率",
            "只读：响应缓存统计（命中率/条目数/节省量；清缓存是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/cache/stats"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_cache_stats(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_storage_overview",
            "存储占用",
            "只读：存储占用总览（KV/文件/日志各段大小；备份导入导出属写侧与二进制流，不给）",
            ReadOnly,
            "",
            &["/api/storage"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_storage_overview(State(s.clone()), h.clone()).await) })
            },
        ),
        td(
            "aigx_ticket_console",
            "工单台",
            "只读：管理员工单列表（可按 id/status/reply_status 过滤并分页；回复与关闭是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/admin/tickets"],
            || {
                json!({
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "按工单 ID 精确查" },
                        "status": { "type": "integer", "description": "工单状态过滤" },
                        "reply_status": { "type": "integer", "description": "回复状态过滤" },
                        "page": { "type": "integer", "minimum": 1 },
                        "page_size": { "type": "integer", "minimum": 1 }
                    },
                    "required": []
                })
            },
            |s, h, a| {
                Box::pin(async move {
                    let q: admin::tickets::AdminTicketQuery =
                        parse_tool_args("aigx_ticket_console", a)?;
                    out(admin::handle_admin_list_tickets(State(s.clone()), h.clone(), Query(q)).await)
                })
            },
        ),
        td(
            "aigx_pricing_gaps",
            "缺定价模型清单",
            "只读：出现在流量里但没有定价的模型（定价漂移排障入口；同步/估算/改价是写侧，批 2）",
            ReadOnly,
            "",
            &["/api/pricing/missing-models"],
            no_args,
            |s, h, _a| {
                Box::pin(async move {
                    out(admin::handle_missing_pricing_models(State(s.clone()), h.clone()).await)
                })
            },
        ),
        td(
            "aigx_token_activity",
            "令牌今日消耗",
            "只读：今日 API 令牌消耗概览（回读明文 key 的 /api/tokens/{id}/key 不给助手）",
            ReadOnly,
            "",
            &["/api/tokens/today"],
            no_args,
            |s, h, _a| {
                Box::pin(async move { out(admin::handle_tokens_today(State(s.clone()), h.clone()).await) })
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ═══════════════════════════════════════════════════════════════════════
    // #35 批1：覆盖率闸门——数据源换成**真实路由表**，不再是手写域清单
    //
    // 旧闸门（本批删除）手写 17 条 REQUIRED_DOMAINS 前缀 + 13 条裸豁免前缀，
    // 判据是「这个前缀下有没有工具 covers」。前缀是人手抄的，只抄了抄的人记得的域。
    // 病灶：手写清单与真实路由表脱钩后，绿灯只代表「清单没被人想起来改」，
    // 不代表工具面真的覆盖了项目。同一批数据的实证：
    //   · 清单里的 `/api/keys` 根本没注册过路由，工具的 covers 也跟着写了幽灵
    //     `/api/keys`，闸门一路绿；
    //   · 301 条 /api 路由里真正被工具 owns 的只有 60 条（20%），闸门照样绿。
    // 新闸门把唯一数据源换成路由注册源码：`main.rs` + `agent/api.rs` 里每条
    // `.route("…")` 都是必须被登记的对象，登记去向只有三个：
    //   工具 covers（有工具）／ EXEMPT_PREFIXES（有意不给）／ KNOWN_GAPS（有意欠着，带理由与归属批次）。
    // 三处都不落 → 红（A）；落了但理由已失效 → 也红（B/C/D/E）。
    // 于是闸门「对新缺口立刻变红、对已登记缺口允许绿」，且批 2 补掉一条却忘了删
    // 台账行时同样变红——台账跟着实现走，而不是实现跟着台账躺。
    //
    // ── 与 CI 提取器 tools/extract_routes.py 的口径对齐（逐条）──
    //   文件集合  ROUTE_FILES = [src/main.rs, src/agent/api.rs]   ←→ ROUTE_SOURCES（include_str! 编译期内联，不依赖 cwd，也不依赖 CI 镜像里有没有 python）
    //   触发写法  正则 `\.route\s*\(`                             ←→ 找 `.route` 后「只允许空白再跟 (」；`.route_layer(` / `set_route_replay` 一律不算
    //   路径实参  第一个顶层实参里第一个字符串字面量，须以 / 开头  ←→ 同；**有意不同**：CI 对不合状的静默跳过，本测试 panic（宁可红得难看，不许悄悄少算一条）
    //   规范化    `:id` / `:id<正则>` → `{id}`、去结尾 /          ←→ norm_route_path 逐字符同逻辑
    //   方法集合  第二个顶层实参文本里扫 get|post|put|delete|
    //             patch|head|options 且后跟 `(`，关键字前一个字符
    //             不能是标识符字符（挡 handle_delete_xxx）        ←→ scan_methods 同规则；同样不排除字符串内文本（现网 0 处差异）
    //   顶层逗号  只取 args[1]，第三个及以后实参不看              ←→ 同样在第一个顶层逗号处截断
    //   范围      管理面 /api* + /api-docs* + /swagger-ui*         ←→ gate_universe 同（见其文档）
    // 2026-11-09 对 373 处 `.route(` 全量对跑：338 条路径、方法集合逐条相同，
    // 无非字面量路径、无注释掉的 `.route(`、无单实参调用。将来路由写法变了导致两边
    // 分叉时，`route_extractor_is_still_anchored` 与各处 panic 会响，不会假绿。
    //
    // ── 粒度：路径，不是「路径 + 方法」──
    // 一条路径只要有工具 owns 就算登记。理由：风险级与审批是**工具**的属性，方法级
    // 台账会把 301 条炸成 500+ 行、红到没人能推；而方法级欠债其实集中且可枚举，
    // 单列成 WRITE_SIDE_PENDING（E）盯住——那才是真盲区。例：`/api/settings` 有 GET
    // 有 PUT，aigx_config_view 只 owns GET，在 A/B 里它是「已覆盖」，只有 E 看得出
    // 「AI 看得见却动不了」。反例存档：`/api/routing/preview` 是 POST 却是纯投影
    // 计算，按「方法=写就欠债」的死规则会被误判，所以 E 判的是「该路径的写方法
    // 有没有任何非 ReadOnly 工具 owns」，而不是「有没有写方法」。
    // ═══════════════════════════════════════════════════════════════════════

    use std::collections::{BTreeMap, BTreeSet};

    /// 路由提取源，与 `tools/extract_routes.py::ROUTE_FILES` 一一对应。
    const ROUTE_SOURCES: [&str; 2] = [include_str!("../main.rs"), include_str!("api.rs")];

    /// 写方法：路径有它们、owns 它的工具却全是 ReadOnly ⇒ 方法级欠债。
    const WRITE_VERBS: [&str; 4] = ["post", "put", "patch", "delete"];

    /// HTTP 方法关键字表（同 CI 的 `METHOD_RE`）。
    const METHOD_WORDS: [&str; 7] = ["get", "post", "put", "delete", "patch", "head", "options"];

    fn is_ident_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    /// 跳过 ASCII 空白，返回第一个非空白字节下标（越界则返回 len）。
    fn skip_ws(src: &[u8], mut i: usize) -> usize {
        while i < src.len() && src[i].is_ascii_whitespace() {
            i += 1;
        }
        i
    }

    /// `start` 应在开引号之后；返回收尾引号下标，反斜杠转义整体跳过。
    fn quote_end(src: &[u8], mut i: usize) -> usize {
        while i < src.len() {
            match src[i] {
                b'\\' => i += 2,
                b'"' => return i,
                _ => i += 1,
            }
        }
        panic!("路由源码里有没收尾的字符串字面量——提取器口径已与 CI 分叉，请同步本测试与 tools/extract_routes.py");
    }

    /// `:id` / `:id<正则>` → `{id}`，并去掉结尾多余的 `/`。对齐 python `norm_path`。
    fn norm_route_path(p: &str) -> String {
        let cs: Vec<char> = p.chars().collect();
        let mut out = String::with_capacity(p.len());
        let mut i = 0usize;
        while i < cs.len() {
            // 同 CI 正则的 `(?<=/)`：冒号前必须是 `/`，且紧跟字母/下划线才算参数段
            if cs[i] == ':' && i > 0 && cs[i - 1] == '/' && i + 1 < cs.len() {
                let mut name = String::new();
                let mut j = i + 1;
                if cs[j].is_ascii_alphabetic() || cs[j] == '_' {
                    while j < cs.len() && (cs[j].is_ascii_alphanumeric() || cs[j] == '_') {
                        name.push(cs[j]);
                        j += 1;
                    }
                    if j < cs.len() && cs[j] == '<' {
                        let mut k = j + 1;
                        while k < cs.len() && cs[k] != '>' {
                            k += 1;
                        }
                        if k < cs.len() {
                            j = k + 1;
                        }
                    }
                    out.push('{');
                    out.push_str(&name);
                    out.push('}');
                    i = j;
                    continue;
                }
            }
            out.push(cs[i]);
            i += 1;
        }
        while out.len() > 1 && out.ends_with('/') {
            out.pop();
        }
        out
    }

    /// 扫 handler 表达式文本里的 HTTP 方法（对齐 CI：不排除字符串内的文本，
    /// 只要求关键字前一个字符不是标识符字符、后面紧跟 `(`）。
    fn scan_methods(handler: &str) -> BTreeSet<String> {
        let hb = handler.as_bytes();
        let mut ms = BTreeSet::new();
        for word in METHOD_WORDS {
            let mut s = 0usize;
            while let Some(rel) = handler[s..].find(word) {
                let k = s + rel;
                s = k + word.len();
                if k > 0 && is_ident_byte(hb[k - 1]) {
                    continue;
                }
                let t = skip_ws(hb, k + word.len());
                if t < hb.len() && hb[t] == b'(' {
                    ms.insert(word.to_string());
                }
            }
        }
        ms
    }

    /// 提取「真实路径 → HTTP 方法集合」。口径见本节开头与 CI 的对齐表；
    /// 遇到不认识的写法直接 panic，绝不静默少算一条路由。
    fn extract_real_routes() -> BTreeMap<String, BTreeSet<String>> {
        const ROUTE_MARK: &str = ".route";
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for src in ROUTE_SOURCES {
            let b = src.as_bytes();
            let mut pos = 0usize;
            while let Some(rel) = src[pos..].find(ROUTE_MARK) {
                let at = pos + rel;
                pos = at + ROUTE_MARK.len();
                let mut j = skip_ws(b, at + ROUTE_MARK.len());
                if j >= b.len() || b[j] != b'(' {
                    continue; // `.route_layer(` / `set_route_replay`：不是路由注册
                }
                j = skip_ws(b, j + 1);
                if j >= b.len() || b[j] != b'"' {
                    panic!("`.route(` 的第一个实参不是字符串字面量：{:.60}", &src[at..]);
                }
                let q = quote_end(b, j + 1);
                let raw = &src[j + 1..q];
                // 括号配对取第二个顶层实参（= handler 表达式）；字符串里的括号跳过。
                let mut depth = 1usize;
                let mut m = q + 1;
                let mut start2: Option<usize> = None;
                // end2 由下面两个 break 点产出；预赋 None 是死值（clippy unused_assignments）
                let end2 = loop {
                    if m >= b.len() {
                        panic!("`.route({raw})` 的调用括号未闭合");
                    }
                    match b[m] {
                        b'"' => m = quote_end(b, m + 1) + 1,
                        b'(' => {
                            depth += 1;
                            m += 1;
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break Some(m);
                            }
                            m += 1;
                        }
                        b',' if depth == 1 => {
                            if start2.is_none() {
                                start2 = Some(skip_ws(b, m + 1));
                                m += 1;
                            } else {
                                break Some(m); // 第三个及以后实参不看（同 CI）
                            }
                        }
                        _ => m += 1,
                    }
                };
                let handler = match (start2, end2) {
                    (Some(s), Some(e)) if e > s => &src[s..e],
                    _ => "",
                };
                if !raw.starts_with('/') {
                    continue; // 对齐 CI：非 `/` 开头不算路由（现网 0 处）
                }
                let mut ms = scan_methods(handler);
                if ms.is_empty() {
                    // 对齐 CI 的 {"?"}。真出现说明路由写法超出两边口径，
                    // 由 route_extractor_is_still_anchored 响，不留在这儿糊过去。
                    ms.insert("?".to_string());
                }
                out.entry(norm_route_path(raw)).or_default().extend(ms);
            }
        }
        out
    }

    /// 闸门宇宙 = 管理面（与 openapi.rs 的范围口径一致）：`/api*`、`/api-docs*`、
    /// `/swagger-ui*`。数据面 `/v1/*` 是产品协议面（OpenAI/Anthropic 兼容，自带
    /// `/v1/models` 自描述），`/health`/`/livez`/`/readyz`/`/metrics` 是编排器探针——
    /// 都不是「AI 可运维的管理面」，故不入账（35 条，逐条核过）。
    fn gate_universe(real: &BTreeMap<String, BTreeSet<String>>) -> BTreeSet<String> {
        real.keys()
            .filter(|p| p.starts_with("/api") || p.starts_with("/swagger-ui"))
            .cloned()
            .collect()
    }

    /// 工具 covers 的倒排：规范化后的真实路径 → owns 它的工具名。
    /// 注意是**精确路径**命中，不是前缀——前缀式匹配正是旧闸门虚报覆盖率的病根。
    fn covers_index() -> BTreeMap<String, Vec<&'static str>> {
        let mut m: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
        for t in all_tools() {
            for c in t.covers {
                m.entry(norm_route_path(c)).or_default().push(t.name);
            }
        }
        m
    }

    /// 裸前缀豁免命中判据：等于该前缀，或落在该前缀之下。
    fn is_exempt(p: &str) -> bool {
        EXEMPT_PREFIXES
            .iter()
            .any(|(prefix, _)| p == *prefix || p.starts_with(&format!("{prefix}/")))
    }

    fn ledger_paths(ledger: &[(&str, &[&str])]) -> BTreeSet<String> {
        ledger
            .iter()
            .flat_map(|(_, paths)| paths.iter())
            .map(|p| norm_route_path(p))
            .collect()
    }

    /// 该路径现算出来的写方法（无写方法 → 空表；顺序固定，报错可读）。
    fn write_verbs_of(real: &BTreeMap<String, BTreeSet<String>>, p: &str) -> Vec<&'static str> {
        match real.get(p) {
            Some(ms) => WRITE_VERBS
                .iter()
                .copied()
                .filter(|w| ms.contains(*w))
                .collect(),
            None => Vec::new(),
        }
    }

    /// 有意**不给 AI 面**的域（裸前缀 + 理由）。判据是「这个域的功能本身不提供」，
    /// 不是「这个域暂时没工具」——后者必须进 `KNOWN_GAPS`。每条必须命中 ≥1 条真实
    /// 路由（见 `exempt_prefixes_are_alive_and_not_covered`），否则就是僵尸例外：
    /// 旧表里的 `/api/payment` 就是这么一条（0 条路由，本批已删）。
    const EXEMPT_PREFIXES: &[(&str, &str)] = &[
        ("/api/auth", "登录/会话签发：人在环流程，且会让凭据过一遍 LLM"),
        (
            "/api/chat/sessions",
            "用户私域聊天记录，不给运维 Agent（旧裸前缀 `/api/chat` 太宽，本批收窄；`/api/chat/settings` 是欠债，已进 KNOWN_GAPS）",
        ),
        ("/api/epay", "支付回调（上游签名校验），不是运维动作"),
        ("/api/stripe", "支付回调（同上）"),
        ("/api/topup", "充值下单/跳转页"),
        ("/api/user/epay", "用户侧支付发起（钱 + 人在环）"),
        ("/api/user/stripe", "用户侧支付发起（同上）"),
        ("/api/tickets", "工单是用户↔人工的通道；Agent 侧另有 /api/admin/tickets"),
        ("/api/prompts", "提示词库：未纳入运维工具面规划"),
        ("/api/oauth", "上游 OAuth 跳转，必须浏览器在场"),
        ("/api/mcp", "MCP 服务自身端点（是 Agent 入口，不是被管对象）"),
        (
            "/api/agent/sessions",
            "Agent 会话存储：让 AI 读写自己的会话历史 = 自我改写（lead 裁定①不做）",
        ),
        (
            "/api/agent/approvals",
            "审批队列 REST：审批必须人在环；AI 只能经 aigx_agent_self_status 读现状，不许自己批自己",
        ),
        ("/api-docs", "OpenAPI 静态文档入口"),
        ("/swagger-ui", "文档 UI"),
        (
            "/api/prices",
            "lead 裁定④：已废弃/与 /api/pricing 重复，转豁免不补工具",
        ),
        (
            "/api/upgrade",
            "lead 裁定④：已废弃，与 /api/update 重复，转豁免不补工具",
        ),
    ];

    /// 已知缺口台账：真实存在、暂无工具、且**已逐条论证过为什么现在不做**。
    /// 每条带理由 + 归属批次；批 2/批 3 补掉后必须删掉该行，否则
    /// `gap_ledger_has_no_stale_entries` 红。欠债有归属，才不会像旧 `REQUIRED_DOMAINS`
    /// 那样半年后没人记得某条为什么被豁免。
    const KNOWN_GAPS: &[(&str, &[&str])] = &[
        (
            "批 2·写侧配置族（改完全局热生效，按批1定调②一律 HighRisk 进审批）",
            &[
                "/api/chat/settings",
                "/api/zcode/captcha-pool",
                "/api/update/cleanup-artifacts",
                "/api/network/config/{config_id}",
                "/api/ip/blacklist",
                "/api/ip/blacklist/{pattern}",
                "/api/ip/whitelist",
                "/api/ip/whitelist/{pattern}",
                "/api/routing/profiles/group/{id}",
            ],
        ),
        (
            "批 2·写侧资源族（单对象增删改；可逆且作用域受限的才逐条论证降 LowRisk）",
            &[
                "/api/channels/{id}/patch",
                "/api/groups/{name}",
                "/api/plans/{id}",
                "/api/plans/{id}/issue",
                "/api/redemptions/{id}",
                "/api/redemptions/batch",
                "/api/redemptions/redeem",
                "/api/models/meta/{model}",
                "/api/models/custom",
                "/api/models/custom/remove",
                "/api/models/alias/reasoning",
                "/api/models/alias/state",
                "/api/models/capabilities",
                "/api/accounts/{id}",
                "/api/network/accounts/{account_id}",
                "/api/proxy-pool/update",
                "/api/proxy-pool/remove",
                "/api/proxy-pool/enable",
                "/api/proxy-pool/sync-clash",
                "/api/plugins/enable",
                "/api/plugins/provider",
                "/api/plugins/uninstall",
                "/api/scheduled-tasks/{id}",
                "/api/tokens/{id}/reset_used",
                "/api/tokens/{id}/rotate",
                "/api/orders/{id}/complete",
                "/api/checkin-center/schedules/remove",
                "/api/checkin-center/records/mark-kept-active",
                "/api/users/self",
                "/api/admin/tickets/close",
                "/api/admin/tickets/reply",
                "/api/alerts/resolve",
            ],
        ),
        (
            "批 2·运维动作族（测活/刷新/清理/重建/终止；含上游真金与外发通知，逐条论证）",
            &[
                "/api/channels/chat_test",
                "/api/channels/fetch_models",
                "/api/models/refresh",
                "/api/models/metadata/refresh",
                "/api/models/test",
                "/api/accounts/test",
                "/api/proxy-pool/test",
                "/api/proxy-pool/test-account",
                "/api/notify/test-email",
                "/api/notify/test-slack",
                "/api/notify/test-webhook",
                "/api/notify/test-telegram",
                "/api/alerts/test",
                "/api/logs/agg/rebuild",
                "/api/logs/agg/tz-migrate",
                "/api/logs/audits/clear",
                "/api/logs/requests/clear",
                "/api/logs/inflight/{id}/terminate",
                "/api/cache/clear",
                "/api/routing/sticky/clear",
                "/api/routing/preview",
                "/api/pricing/estimate",
                "/api/pricing/sync",
                "/api/network/restart",
                "/api/checkin-center/schedules/run",
                "/api/checkin-center/benefits/run",
                "/api/scheduled-tasks/{id}/run",
                "/api/checkin",
            ],
        ),
        (
            "批 2·资金与账户安全族（钱/2FA 语义，定级需 lead 单独拍板，不在批1 讨论范围）",
            &[
                "/api/aff_transfer",
                "/api/subscription/balance/pay",
                "/api/subscription/admin/bind",
                "/api/subscription/admin/subscriptions/{id}/cancel",
                "/api/users/{id}/2fa",
            ],
        ),
        (
            "不给 AI 面·凭据/隐私/二进制流（明文密钥、含凭据的导出、加密备份、CSV 流、管理员个人域）",
            &[
                "/api/tokens/{id}/key",
                "/api/account-sessions/export",
                "/api/backup/download",
                "/api/backup/export-encrypted",
                "/api/backup/import",
                "/api/backup/import-encrypted",
                "/api/logs/requests/export",
                "/api/logs/runtime/export",
                "/api/aff",
                "/api/users/me",
                "/api/orders/me",
                "/api/session",
                "/api/session/refresh",
            ],
        ),
        (
            "不给 AI 面·人在环流程（登录/验证码/导入向导/批量重算：需要浏览器或人工在场，工具化只会让 AI 卡在等人）",
            &[
                "/api/account-sessions/login/callback",
                "/api/account-sessions/login/cancel",
                "/api/account-sessions/login/oauth/captcha-config",
                "/api/account-sessions/login/oauth/start",
                "/api/account-sessions/login/paste",
                "/api/account-sessions/login/sms/send",
                "/api/account-sessions/login/sms/verify",
                "/api/account-sessions/login/start",
                "/api/account-sessions/login/wait",
                "/api/account-sessions/desktop-import",
                "/api/account-sessions/import",
                "/api/account-sessions/import/agent2api",
                "/api/account-sessions/import/json",
                "/api/account-sessions/batch",
                "/api/account-sessions/usage",
                "/api/zcode/captcha",
            ],
        ),
        (
            "不给 AI 面·人工试跑与对话入口（playground 真金烧上游 / assistant 对话本身；工具回路另有条目）",
            &[
                "/api/playground/chat",
                "/api/playground/images",
                "/api/playground/transcriptions",
                "/api/playground/tts",
                "/api/assistant/chat",
            ],
        ),
        (
            "批 2·观测补漏（需具体账号 ID、账号缺席即 404 的窄视图，等批 2 一起定 enum 口径）",
            &[
                "/api/zcode/captcha/config",
            ],
        ),
        (
            "批 3·形状不合（handler 不吃 State / 返回裸 Json 而非 HandlerResult / 公开探测接口），要工具化得先改 handler",
            &[
                "/api/users/check",
                "/api/public/geo-lang",
            ],
        ),
    ];

    /// 方法级欠债台账：路径有写方法（POST/PUT/PATCH/DELETE），但 owns 它的工具**全是**
    /// ReadOnly——「AI 看得见这个东西，却动不了」。路径粒度看不出这层（该路径算已覆盖），
    /// 所以单列本表，并由 E 按真实方法集现算逐条对跑：批 2 给某条加了写工具后该行必须删。
    /// 这也是 lead 裁定②（写侧 config 族一律 HighRisk 进审批）的机器化落点，
    /// 免得「哪些写侧要给、给什么级别」退化成口头知识。
    const WRITE_SIDE_PENDING: &[(&str, &[&str])] = &[
        (
            "批 2·写侧配置族（lead 裁定②：全局热生效的 config 一律 HighRisk 进审批；读侧已由 aigx_config_view 提供）",
            &[
                "/api/settings",
                "/api/limits",
                "/api/security/policy",
                "/api/sanitize",
                "/api/gateway/settings",
                "/api/gateway/prompt",
                "/api/update/settings",
                "/api/logs/retention",
                "/api/queue",
                "/api/ratelimit/config",
                "/api/ratios",
                "/api/ip/filter",
                "/api/notify/config",
                "/api/cline-headers",
                "/api/user-tools/policy",
                "/api/routing",
                "/api/routing/profiles",
                "/api/alerts/rules",
                "/api/pricing/sync-config",
                "/api/pricing/exchange-rates",
                "/api/models/metadata",
                "/api/debug",
                "/api/report/settings",
                "/api/checkin-center/benefits/keepalive",
            ],
        ),
        (
            "批 2·运维动作族（清理/重算：删日志、清流量缓冲、刷新汇总——可逆性差，逐条论证）",
            &[
                "/api/logs/audits",
                "/api/logs/requests",
                "/api/debug/traffic",
                "/api/usage/summary",
            ],
        ),
        (
            "批 2·写侧资源族（单对象 create：列表工具只 owns GET，POST 侧无人）",
            &[
                "/api/accounts",
                "/api/plans",
                "/api/users",
                "/api/proxy-pool",
            ],
        ),
        (
            "不给 AI 面·Agent 自身边界（lead 裁定①：只读给 aigx_agent_self_status，PUT /api/agent/settings 永不让 AI 改自己）",
            &[
                "/api/agent/settings",
            ],
        ),
    ];

    /// 哨兵：提取器自身必须仍在真实地提取到东西。
    /// 若有人把路由注册改成 nest / 宏 / 动态拼接，宇宙会（半静默地）缩小甚至归零，
    /// 那么 A–E 五条断言会因为「空集 ⊆ 任何东西」而全体假绿——那正是本闸门要根治的病，
    /// 所以先给闸门自己上一道：量级地板 + 口径漂移必须在这里响。
    #[test]
    fn route_extractor_is_still_anchored() {
        let real = extract_real_routes();
        let universe = gate_universe(&real);
        assert!(
            universe.len() >= 200,
            "路由提取异常：管理面只提取到 {} 条（量级应为 ~300）。多半是路由写法变了导致提取口径分叉，\
             此时 A–E 会全体假绿，必须先同步本测试与 tools/extract_routes.py",
            universe.len()
        );
        assert!(
            real.values().all(|ms| !ms.is_empty()),
            "存在方法集为空的路由（提取器缺陷）"
        );
        let unknown = real
            .values()
            .flatten()
            .filter(|m| m.as_str() == "?")
            .count();
        assert_eq!(
            unknown, 0,
            "有 `.route(…)` 的 handler 里扫不出 HTTP 方法：本测试与 CI 提取器口径分叉，\
             或新写法（any()/fallback）需要两边同时支持"
        );
    }

    /// A：真实路由表里不允许有「谁都没登记」的路径。
    /// 登记去向 = 工具 covers ∪ `EXEMPT_PREFIXES` ∪ `KNOWN_GAPS`。
    #[test]
    fn every_real_route_is_registered() {
        let real = extract_real_routes();
        let universe = gate_universe(&real);
        let covered = covers_index();
        let gaps = ledger_paths(KNOWN_GAPS);
        let mut orphans: Vec<String> = Vec::new();
        for p in &universe {
            if covered.contains_key(p) || gaps.contains(p.as_str()) || is_exempt(p) {
                continue;
            }
            orphans.push(format!(
                "    {p}  [写方法: {}]",
                write_verbs_of(&real, p).join("+")
            ));
        }
        assert!(
            orphans.is_empty(),
            "管理面路由未登记（{}/{} 条）：\n{}\n\
             修法三选一——① 给某条工具的 covers 加这条路径；② 有意不给 AI 面 → 进 `EXEMPT_PREFIXES` 并写理由；\
             ③ 有意先欠着 → 进 `KNOWN_GAPS` 并写理由 + 归属批次。",
            orphans.len(),
            universe.len(),
            orphans.join("\n")
        );
    }

    /// B：台账不许腐烂。每条 `KNOWN_GAPS` 必须仍是真实路由、仍无工具、且不与豁免重叠。
    #[test]
    fn gap_ledger_has_no_stale_entries() {
        let universe = gate_universe(&extract_real_routes());
        let covered = covers_index();
        for &(reason, paths) in KNOWN_GAPS {
            for p in paths {
                let np = norm_route_path(p);
                assert!(
                    universe.contains(&np),
                    "`KNOWN_GAPS` 僵尸行：`{np}`（批次「{reason}」）已不是真实路由，删掉它"
                );
                assert!(
                    !covered.contains_key(&np),
                    "`KNOWN_GAPS` 过时行：`{np}` 已被工具 covers（{:?}），从「{reason}」删掉它",
                    covered.get(&np).map(|v| v.join(",")).unwrap_or_default()
                );
                assert!(
                    !is_exempt(&np),
                    "`KNOWN_GAPS` 与 `EXEMPT_PREFIXES` 重叠：`{np}` 既算豁免又算欠债，二者留一个"
                );
            }
        }
    }

    /// C：工具 `covers` 必须命中真实路由——幽灵 `/api/keys`（有 covers、无路由）就是这么抓的。
    #[test]
    fn tool_covers_point_at_real_routes() {
        let universe = gate_universe(&extract_real_routes());
        let mut ghosts = Vec::new();
        for t in all_tools() {
            for c in t.covers {
                if !universe.contains(&norm_route_path(c)) {
                    ghosts.push(format!("    {} → covers `{c}`", t.name));
                }
            }
        }
        assert!(
            ghosts.is_empty(),
            "工具 covers 指向不存在的路由（{} 条）：\n{}\n\
             要么路由本就没有（covers 改指真实路由，参考本批 aigx_key_add/aigx_key_delete 的收敛），\
             要么路由已删（删掉 covers 里的这条）。",
            ghosts.len(),
            ghosts.join("\n")
        );
    }

    /// D：豁免必须活着（否则是僵尸例外），且不与工具 covers 矛盾。
    /// 后半段沿用旧闸门的判据：豁免域的本意是「不提供该域的功能」，但「以该域为
    /// **数据源**」的工具不算提供功能——`aigx_openapi_search` 的 covers 是
    /// `/api/openapi.json`，它读 OpenAPI 文档去**发现**别的端点，而不是提供「读
    /// OpenAPI 文档」这个能力。所以矛盾判据是「cover 落在该域**之下**」（比该域更长
    /// 的子路径），而不是「等于该域」。
    #[test]
    fn exempt_prefixes_are_alive_and_not_covered() {
        let universe = gate_universe(&extract_real_routes());
        for &(prefix, reason) in EXEMPT_PREFIXES {
            assert!(
                !reason.trim().is_empty(),
                "豁免 `{prefix}` 没写理由（理由会腐烂成口头知识）"
            );
            assert!(
                universe
                    .iter()
                    .any(|p| p.as_str() == prefix || p.starts_with(&format!("{prefix}/"))),
                "僵尸豁免：前缀 `{prefix}`（理由：{reason}）下已无任何真实路由，删掉这条豁免"
            );
        }
        for t in all_tools() {
            for c in t.covers {
                let np = norm_route_path(c);
                for &(prefix, reason) in EXEMPT_PREFIXES {
                    assert!(
                        !(np.starts_with(prefix) && np.len() > prefix.len()),
                        "豁免域与 covers 矛盾：工具 {} 覆盖了 `{np}`，而 `{prefix}` 声明整域不给 AI 面（{reason}）",
                        t.name
                    );
                }
            }
        }
    }

    /// E：写侧欠债台账必须与真实方法集现算结果逐条一致（不多不少）。
    #[test]
    fn write_side_pending_matches_reality() {
        let real = extract_real_routes();
        let covered = covers_index();
        let read_only: BTreeSet<&'static str> = all_tools()
            .iter()
            .filter(|t| t.risk == RiskLevel::ReadOnly)
            .map(|t| t.name)
            .collect();
        // 路径 → 现算依据（写方法 + owns 它的工具），报错时直接给出，不用人去复算
        let mut computed: BTreeMap<String, String> = BTreeMap::new();
        for p in real.keys().filter(|p| p.starts_with("/api/")) {
            let wv = write_verbs_of(&real, p);
            if wv.is_empty() {
                continue; // 纯只读路径，没有方法级欠债可言
            }
            let owners = match covered.get(p) {
                Some(o) if !o.is_empty() => o,
                // 没有任何工具 owns ⇒ 路径级欠债，归 KNOWN_GAPS 管（A/B 已盯）
                _ => continue,
            };
            if owners.iter().all(|n| read_only.contains(*n)) {
                computed.insert(
                    p.clone(),
                    format!("write={} owners={}", wv.join("+"), owners.join(",")),
                );
            }
        }
        let listed = ledger_paths(WRITE_SIDE_PENDING);
        let mut unlisted: Vec<String> = Vec::new();
        for (p, why) in &computed {
            if !listed.contains(p.as_str()) {
                unlisted.push(format!("    {p}  [{why}]"));
            }
        }
        let mut stale: Vec<String> = Vec::new();
        for p in &listed {
            if !computed.contains_key(p.as_str()) {
                stale.push(format!("    {p}"));
            }
        }
        assert!(
            unlisted.is_empty() && stale.is_empty(),
            "`WRITE_SIDE_PENDING` 与真实方法集分叉（漏记 {} 条 / 过时 {} 条）。\n\
             漏记（该路径有写方法、owns 它的工具却全是 ReadOnly，台账没登记）：\n{}\n\
             过时（台账里还欠着，实际已有非只读工具 owns）：\n{}\n\
             背景：批 2 给某条加了写工具后，必须删掉该条台账行（本测试就是逼着删）。",
            unlisted.len(),
            stale.len(),
            unlisted.join("\n"),
            stale.join("\n")
        );
    }

    /// 台账卫生：台账内不得重复登记，组不得空、理由不得空，两条台账语义互斥。
    /// （旧 `REQUIRED_DOMAINS` 的烂法之一就是同一条径写两遍、谁也不负责。）
    #[test]
    fn ledgers_are_unique_and_reasoned() {
        for (tag, ledger) in [
            ("KNOWN_GAPS", KNOWN_GAPS),
            ("WRITE_SIDE_PENDING", WRITE_SIDE_PENDING),
        ] {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            for &(reason, paths) in ledger {
                assert!(!reason.trim().is_empty(), "{tag} 有一组没写理由/归属批次");
                assert!(
                    !paths.is_empty(),
                    "{tag} 有一组空路径列表（「{reason}」），删掉它"
                );
                for p in paths {
                    let np = norm_route_path(p);
                    assert!(seen.insert(np.clone()), "{tag} 重复登记 `{np}`");
                }
            }
        }
        // 语义互斥：`KNOWN_GAPS` 要求「无工具 owns」，`WRITE_SIDE_PENDING` 要求「有工具 owns」。
        let gaps = ledger_paths(KNOWN_GAPS);
        let pending = ledger_paths(WRITE_SIDE_PENDING);
        for p in &gaps {
            assert!(
                !pending.contains(p.as_str()),
                "台账冲突：`{p}` 既算无工具欠债（`KNOWN_GAPS`）又算写侧欠债（`WRITE_SIDE_PENDING`）"
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
        let list = crate::api::admin::mcp::tools_list_result(&[]);
        let names: Vec<&str> = list["tools"]
            .as_array()
            .expect("tools/list 有 tools 数组")
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default())
            .collect();
        let expect: Vec<&str> = mcp_tools().map(|t| t.name).collect();
        assert_eq!(
            names, expect,
            "MCP tools/list 与 registry::mcp_tools 不一致（disabled=[] 时）"
        );
    }

    /// 三面（Agent/MCP/assistant）共用的禁用口径（#35 批1 后半）：列与调同时消失，
    /// 且判定只认全等——前缀/大小写变体不得误伤邻近工具。
    #[test]
    fn disabled_tools_drop_from_visibility_and_admission() {
        let victim = mcp_tools()
            .next()
            .expect("非高危工具面非空")
            .name
            .to_string();
        let disabled = vec![victim.clone()];

        assert!(tool_disabled(&victim, &disabled));
        // 判定不放宽：改一个字符/换大小写都不算命中
        assert!(!tool_disabled(&format!("{victim}_typo"), &disabled));
        assert!(!tool_disabled(&victim.to_uppercase(), &disabled));
        assert!(!tool_disabled("", &disabled));

        let names: Vec<&str> = visible_tools(&disabled).iter().map(|t| t.name).collect();
        assert!(
            !names.iter().any(|n| *n == victim.as_str()),
            "禁用后仍在可见集: {victim}"
        );
        assert_eq!(
            visible_tools(&[]).len() - 1,
            names.len(),
            "禁用一条只少一条，其余工具不受影响"
        );
        // 列里没了 ≠ 能调：准入判定与可见集同源
        assert!(find_visible_tool(&victim, &disabled).is_none());
        assert!(find_visible_tool(&victim, &[]).is_some());

        // 高危与禁用是两条独立门槛：高危无需禁用也不给（Agent 面另有审批矩阵）
        let high = all_tools()
            .iter()
            .find(|t| t.risk == RiskLevel::HighRisk)
            .expect("存在高危工具")
            .name
            .to_string();
        assert!(
            find_visible_tool(&high, &[]).is_none(),
            "高危不得进管理面准入"
        );
        assert!(
            visible_tools(&[]).iter().all(|t| t.name != high.as_str()),
            "高危不得出现在可见集"
        );
    }

    /// #34 用户域工具台账（首发四件套；新增用户域工具必须同步本表）。
    const USER_TOOLS: &[&str] = &[
        "aigx_checkin_run",
        "aigx_query_usage",
        "aigx_query_logs",
        "aigx_available_models",
    ];

    /// 防漂移（#34）：用户域工具集 = 声明了 `user_exec` 且非高危的工具，
    /// 必须与 [`USER_TOOLS`] 台账逐一对齐；高危工具永不声明 `user_exec`（D2）。
    #[test]
    fn user_tool_surface_declared_and_capped() {
        for t in all_tools() {
            if t.user_exec.is_some() {
                assert_ne!(
                    t.risk,
                    RiskLevel::HighRisk,
                    "高危工具 {} 不得声明 user_exec（D2 硬封顶）",
                    t.name
                );
                assert!(!t.covers.is_empty(), "用户域工具 {} 须声明 covers", t.name);
            }
        }
        for name in USER_TOOLS {
            let t = find_tool(name).unwrap_or_else(|| panic!("台账工具 {name} 不在注册表"));
            assert!(t.user_exec.is_some(), "{name} 未声明 user_exec");
            assert!(t.is_user_exposed(), "{name} 应对终端用户暴露");
        }
        let exposed: Vec<&str> = user_exposed_tools().map(|t| t.name).collect();
        assert_eq!(
            exposed.len(),
            USER_TOOLS.len(),
            "用户域工具集 {:?} 与 USER_TOOLS 台账不一致",
            exposed
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
