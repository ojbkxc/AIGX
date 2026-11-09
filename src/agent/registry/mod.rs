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
///
/// 可见性 `pub(super)`（#35 批2 拆模块）：子模块靠 `use super::*` 即可取用，
/// 此提升只为「不依赖 glob」时也能按名调用，**不跨 crate**——公开面零变化。
#[allow(clippy::too_many_arguments)]
pub(super) fn td(
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

/// 禁用拒绝的统一文案——runner 的执行侧兜底、[`disabled_reject`]（即
/// [`exec_tool`] / [`exec_user_tool`] 的入口门槛）、MCP 的拒绝回包都说这一句。
///
/// 同一个拒绝在三个面上有三种措辞只会变成排障噪音（#35 批1 后半，lead 追加要求）。
pub fn disabled_tool_message(name: &str) -> String {
    format!("工具 {name} 已被管理员禁用（disabled_tools），未执行")
}

/// 执行入口的禁用门槛：命中返回 JSON-RPC 错误（`-32601`，与「未知工具」同码不同文）。
///
/// 这一层把「约定安全」变成「结构安全」：此前 `disabled_tools` 只在三个调用方
/// 各自预筛（runner 兜底 / MCP `find_tool_spec` / assistant `exec_denial`），
/// 今天无缺口，但新增第四个调用方忘加即漏——正是本仓 `mark_unhealthy` 只补一处
/// 的同形态。纯函数以便单测直接钉死（`exec_tool` 需要 [`AppState`]，单测不构造）。
pub fn disabled_reject(name: &str, disabled: &[String]) -> Option<(i64, String)> {
    tool_disabled(name, disabled).then(|| (-32601, disabled_tool_message(name)))
}

/// 落盘配置里的禁用清单（三面共用同一取法）。
///
/// **不要**改读 `state.agent_state` 的运行期快照：Agent 未启用时它是 `None`，
/// 「关了就是关了」会变成「没开 Agent 时禁用不生效」。
pub async fn disabled_tools_of(state: &AppState) -> Vec<String> {
    state.config_manager.get().await.agent.disabled_tools
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
///
/// 入口自带禁用门槛（[`disabled_reject`]）：调用方**不需要**先筛也能拦住被禁用的
/// 工具——调用方的预筛仍然保留，它们负责各自的 UX 与审计文案，这里只保证「走这个
/// 门就一定执行不到」（#35 批1 后半）。
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
    if let Some(err) = disabled_reject(name, &disabled_tools_of(state).await) {
        return Err(err);
    }
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
/// 未知工具 / 被管理员禁用 / 未声明用户域执行体 / 高危 → 拒绝（执行侧兜底，不依赖
/// 调用方预筛；禁用那一刀见 [`disabled_reject`]，与 [`exec_tool`] 同一门槛）。
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
    if let Some(err) = disabled_reject(name, &disabled_tools_of(state).await) {
        return Err(err);
    }
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

// ── 子模块（#35 批2 拆模块：纯机械切分；声明顺序 = 组装顺序）──────────────────
mod account_pool;
mod account_sessions;
mod admin_writes;
mod checkin;
mod diagnostics;
mod models;
mod observe;
mod queries;
mod report;
mod update_ops;
// 批2 并行单元：每个 owner 独占一个文件（#24 的台账改名落 tests.rs / observe.rs，
// 不单开文件）。**本 assembler 由 #4 一次写定**——7 个 owner 只在各自的 `w_*` 文件里
// 追加 `td(...)`，谁都不许再改本文件。
mod w_config;
mod w_logs;
mod w_models;
mod w_plans;
mod w_pool_tickets;
mod w_resources;

#[cfg(test)]
mod tests;

/// 全量工具的组装点：各域子模块按声明顺序拼接。
///
/// **顺序敏感**——`api/admin/user_tools.rs` 生成的用户工具目录按此顺序展示，
/// 拆分前后顺序逐条一致（拆分 = 纯机械，不改任何工具的相对次序）。
///
/// 批2 新工具按单元落在各自的 `w_*` 文件；此处一次串接到末尾，owner 不再回改本文件。
fn build_tools() -> Vec<ToolDef> {
    let mut v = Vec::new();
    v.extend(diagnostics::tools());
    v.extend(queries::tools());
    v.extend(admin_writes::tools());
    v.extend(account_sessions::tools());
    v.extend(update_ops::tools());
    v.extend(models::tools());
    v.extend(checkin::tools());
    v.extend(report::tools());
    v.extend(account_pool::tools());
    v.extend(observe::tools());
    // ── 批2 并行单元（各自文件独占，互不覆盖）──
    v.extend(w_config::tools()); // U-C
    v.extend(w_models::tools()); // U-A
    v.extend(w_resources::tools()); // U-B
    v.extend(w_logs::tools()); // U-D
    v.extend(w_pool_tickets::tools()); // U-E
    v.extend(w_plans::tools()); // U-F
    v
}
