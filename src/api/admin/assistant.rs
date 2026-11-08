//! 聊天助手工具回路（#34，面向管理员的 Chat 内工具调用；终端用户 v1 纯聊天）。
//!
//! ── 定位与身份模型（v1 契约，lead 定稿前先看这段）──────────────
//! 复用 agent 三层 registry 的工具面（[`crate::agent::registry::mcp_tools`]，
//! 非 HighRisk ≈53 个）给**管理员**的聊天页提供工具回路；终端用户 v1
//! **不带工具**（纯聊天+计费）。原因：registry 的 exec 闭包直连管理面
//! handler（内部 verify_admin），普通用户的会话头会被 401/403 拦截——
//! 若为此铸造「内部 admin 会话」就是隐藏管理员账号（安全反模式）；若把
//! 53 个工具改成身份感知执行是 8 人周级重构。v1 先让管理员在 Chat 页
//! 用上工具（与 Agent.tsx 同权限语义：headers 原样透传，审批矩阵与审计
//! 零绕过——非 HighRisk 免审批、写操作审计照记），终端用户工具待权限
//! 模型决策。
//!
//! ── 回路语义 ─────────────────────────────────────────────────
//! messages 进 → 上游（带 openai_tools schema）→ 返回 tool_calls → 逐个
//! 执行（registry::exec_tool，复用 find_tool/审计）→ role:tool 结果回灌
//! → 继续生成，最多 [`MAX_ROUNDS`] 轮（对齐 A2A 12 轮口径）。终止：
//! 无 tool_calls / 达轮数上限 / 上游错误。每轮 usage 累计，计费走
//! playground 同链（订阅池优先+钱包兜底，管理员豁免），**一条请求日志
//! 归集全程**（usage 求和、attempts=轮数）。
//!
//! ── 有界（硬约束）───────────────────────────────────────────
//! - 轮数：[`MAX_ROUNDS`] = 12；
//! - 单条工具结果：复用 `truncate_tool_text`（16K 字符口径）；
//! - 全程工具结果总量：[`MAX_TOOL_TOTAL_CHARS`] = 64KiB，超限停止执行
//!   后续工具并如实回灌占位（不静默丢）。
//!
//! ── 开关 ────────────────────────────────────────────────────
//! `assistant_tools_enabled`（默认 **关**）：关闭时 /tools 返回空清单、
//! /chat 不注入 tools（纯聊天），行为与 playground 等价。
//!
//! 性能影响：温/冷路径（管理页聊天，低频）；每轮一次上游调用 + 工具
//! 执行（registry exec 直调管理面 handler，进程内）；无新增全局锁/
//! 无限结构；开关关闭时本模块只多两次 verify_user 与一次纯透传循环
//!（无工具轮=单次上游调用，与 playground 等价）。

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};
use crate::agent::tools::{self, RiskLevel, ToolOutcome};
use crate::bridge::{Bridge, BridgeContext, ChatFormat, Role};
use crate::user::User;

/// 工具回路的轮数上限（对齐 A2A assistant MAX_ROUNDS=12）。
pub const MAX_ROUNDS: u32 = 12;
/// 单次请求全部工具结果的字符总量上限（超出即停执行，如实回灌）。
pub const MAX_TOOL_TOTAL_CHARS: usize = 65536;

/// 工具回路是否开启（进程级快照，main 启动写入 + settings 保存即生效）。
pub fn tools_enabled() -> bool {
    crate::config::assistant_tools_enabled()
}

/// 工具目录（GET /api/assistant/tools）。
///
/// 管理员（开关开）→ 非 HighRisk 全集（≈53，registry::mcp_tools 同口径）；
/// 普通用户（开关开）→ user_exec 声明集 ∩ 组 allowlist（#34 v2 双身份，
/// user_tool_allowed 判定，设计稿 D1-D8）；开关关 → 空清单（前端隐藏工具入口）。
pub async fn handle_assistant_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let tool_json = |t: &crate::agent::registry::ToolDef| {
        json!({
            "name": t.name,
            "label": t.label,
            "description": t.description,
            "schema": (t.schema)(),
            "risk": match t.risk {
                RiskLevel::ReadOnly => "read_only",
                RiskLevel::LowRisk => "low_risk",
                RiskLevel::HighRisk => "high_risk",
            },
        })
    };
    let tools: Vec<Value> = if user.is_admin() {
        if !tools_enabled() {
            Vec::new()
        } else {
            tools::tool_specs()
                .iter()
                .filter(|t| t.risk != RiskLevel::HighRisk)
                .map(tool_json)
                .collect()
        }
    } else if tools_enabled() {
        // #34 v2：普通用户域——user_exec 声明 ∩ 组 allowlist（判定函数带
        // 14 单测；执行侧 exec_user_tool 还有一层兜底，两重防线）
        let policy = state.user_tool_policy.snapshot();
        tools::user_exposed_tools()
            .filter(|t| {
                crate::agent::user_tool_policy::user_tool_allowed(
                    &policy,
                    &user.group,
                    t.name,
                    t.risk,
                    t.user_exec.is_some(),
                ) == crate::agent::user_tool_policy::ToolPermission::Allow
            })
            .map(tool_json)
            .collect()
    } else {
        Vec::new()
    };
    Ok(Json(json!({
        "success": true,
        "data": { "tools": tools, "toolsEnabled": tools_enabled() }
    })))
}

/// 请求体消息（DTO：ChatMessage 无 serde derive，role 用字符串便于前端）。
#[derive(Debug, Deserialize)]
pub struct AssistantMessageIn {
    /// `system` / `user` / `assistant` / `tool`（小写不敏感）
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
}

/// #34-② 计费/落日志守卫：正常完成走 [`Self::settle`]；中途上游错误与
/// 客户端断开（axum drop 整个 handler future）由 Drop 兜底——已消耗轮次
/// 照记不漏（与 #35 数据面结论同源不同面：助手面把「上游请求已发出」
/// 作为记账起点；写路径全部同步、LogWriter 异步队列不阻塞 Drop）。
struct AssistantBilling {
    state: AppState,
    user_id: String,
    is_admin: bool,
    model: String,
    channel_id: String,
    channel_name: String,
    prompt: u64,
    completion: u64,
    rounds: u32,
    started: std::time::Instant,
    error: Option<String>,
    handled: bool,
}

impl AssistantBilling {
    fn write(&mut self) {
        super::playground::record_playground_usage(
            &self.state,
            &self.user_id,
            self.is_admin,
            "assistant",
            &self.model,
            Some(&self.channel_id),
            Some(&self.channel_name),
            self.prompt,
            self.completion,
            self.started.elapsed().as_millis() as u64,
        );
        let mut log = crate::log::RequestLog::new();
        log.user_id = Some(self.user_id.clone());
        log.channel_id = Some(self.channel_id.clone());
        log.channel_name = Some(self.channel_name.clone());
        log.model = self.model.clone();
        log.input_tokens = self.prompt;
        log.output_tokens = self.completion;
        log.latency_ms = self.started.elapsed().as_millis() as u64;
        log.status_code = if self.error.is_some() { 502 } else { 200 };
        log.error_msg = self.error.clone();
        log.attempts = self.rounds;
        self.state.log_store.record_request(log);
        self.handled = true;
    }

    fn settle(mut self) {
        self.write();
    }
}

impl Drop for AssistantBilling {
    fn drop(&mut self) {
        if !self.handled {
            self.write();
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct AssistantChatRequest {
    pub model: String,
    pub messages: Vec<AssistantMessageIn>,
    /// 渠道 id（管理员可指定；普通用户忽略，走模型路由）
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 轮数上限（缺省 12，钳到 [`MAX_ROUNDS`]）
    #[serde(default)]
    pub max_rounds: Option<u32>,
}

fn to_role(s: &str) -> Option<Role> {
    match s.to_ascii_lowercase().as_str() {
        "system" => Some(Role::System),
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "tool" => Some(Role::Tool),
        _ => None,
    }
}

/// POST /api/assistant/chat —— 工具回路聊天（非流式 v1）。
///
/// 响应 `data`：`{content, rounds, tool_calls: [{name, ok, summary}],
/// usage: {prompt_tokens, completion_tokens}}`；多轮 token 归集进同一条
/// 请求日志（usage 求和、attempts=轮数）。
pub async fn handle_assistant_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AssistantChatRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user: User = verify_user(&state, &headers).await?;
    if body.model.trim().is_empty() {
        return Err(error_response("model is required", StatusCode::BAD_REQUEST));
    }
    if body.messages.is_empty() {
        return Err(error_response(
            "messages must not be empty",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 余额预检 + 分组模型白名单（与 playground 同一 guard）
    if let Some(resp) = super::playground::playground_guard(&state, &user, &body.model) {
        let status = resp.status();
        return Err((
            status,
            Json(
                json!({ "success": false, "error": "guard rejected（余额不足或模型不在分组白名单）" }),
            ),
        ));
    }
    // #34 v2：普通用户也走工具回路（tools_enabled 总闸 + 双身份执行体分派）
    let tools_on = tools_enabled();
    // 普通用户的可执行集：user_exec 声明 ∩ 组 allowlist（与 /tools 同判定）
    let policy = state.user_tool_policy.snapshot();
    let max_rounds = body.max_rounds.unwrap_or(MAX_ROUNDS).clamp(1, MAX_ROUNDS);

    let ch = pick_channel(
        &state,
        &body.model,
        body.channel_id.as_deref(),
        user.is_admin(),
    )?;
    let bridge = make_channel_bridge(&state, &ch)?;

    let mut billing = AssistantBilling {
        state: state.clone(),
        user_id: user.id.clone(),
        is_admin: user.is_admin(),
        model: body.model.clone(),
        channel_id: ch.id.clone(),
        channel_name: ch.name.clone(),
        prompt: 0,
        completion: 0,
        rounds: 0,
        started: std::time::Instant::now(),
        error: None,
        handled: false,
    };
    let mut messages: Vec<crate::bridge::ChatMessage> = body
        .messages
        .iter()
        .map(|m| crate::bridge::ChatMessage {
            role: to_role(&m.role).unwrap_or(Role::User),
            content: Some(m.content.clone()),
            reasoning: None,
            content_blocks: None,
            name: m.name.clone(),
            tool_call_id: m.tool_call_id.clone(),
            tool_calls: None,
        })
        .collect();

    let mut total_prompt: u64 = 0;
    let mut total_completion: u64 = 0;
    let mut rounds: u32 = 0;
    let mut tool_trace: Vec<Value> = Vec::new();
    let mut tool_total_chars: usize = 0;
    // 延迟初始化：两条 break 路径都先赋值（clippy unused_assignments——
    // String::new() 初值从未被读）
    let final_content;
    // #34-①（lead 评审）：openai_tools() 是全量 73 个含 20 高危——助手面
    // 只给 mcp_tools() 非 HighRisk 子集（exec 前的 deny 是第二重，不是唯一一重）
    // Schema 按身份分派：管理员=非 HighRisk 全集；普通用户=可执行集
    let tools_schema: Vec<Value> = if tools_on {
        if user.is_admin() {
            tools::openai_tools_of(
                tools::tool_specs()
                    .iter()
                    .filter(|t| t.risk != RiskLevel::HighRisk),
            )
        } else {
            tools::openai_tools_of(tools::user_exposed_tools().filter(|t| {
                crate::agent::user_tool_policy::user_tool_allowed(
                    &policy,
                    &user.group,
                    t.name,
                    t.risk,
                    t.user_exec.is_some(),
                ) == crate::agent::user_tool_policy::ToolPermission::Allow
            }))
        }
    } else {
        Vec::new()
    };

    loop {
        rounds += 1;
        billing.rounds = rounds;
        let req = ChatFormat {
            model: body.model.clone(),
            messages: messages.clone(),
            tools: if tools_on && rounds <= max_rounds {
                Some(tools_schema.clone())
            } else {
                None
            },
            tool_choice: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            top_k: None,
            stop: None,
            reasoning_effort: None,
            web_search_options: None,
            extra: None,
        };
        let ctx = BridgeContext::new(format!("assistant-{}", uuid::Uuid::new_v4()), &body.model);
        let resp = match bridge.chat(&req, &ctx).await {
            Ok(r) => r,
            // #34-②：中途失败 → Drop 兜底把已消耗轮次记账+落日志（error 标记）
            Err(e) => {
                billing.error = Some(format!("上游调用失败: {e}"));
                return Err(error_response(
                    &format!("上游调用失败: {e}"),
                    StatusCode::BAD_GATEWAY,
                ));
            }
        };
        total_prompt += resp.usage.prompt_tokens;
        total_completion += resp.usage.completion_tokens;
        billing.prompt = total_prompt;
        billing.completion = total_completion;
        let content = resp.message.content_str().to_string();
        match resp.message.tool_calls.clone() {
            Some(calls) if !calls.is_empty() && rounds < max_rounds && tools_on => {
                // 助手消息（含 tool_calls）回灌，保持 OpenAI 多轮形状
                messages.push(crate::bridge::ChatMessage {
                    role: Role::Assistant,
                    content: Some(content.clone()),
                    reasoning: None,
                    content_blocks: None,
                    name: None,
                    tool_call_id: None,
                    tool_calls: Some(calls.clone()),
                });
                for call in calls {
                    // 总量兜底：超限停止执行后续工具，占位回灌（不静默丢）
                    if tool_total_chars > MAX_TOOL_TOTAL_CHARS {
                        tool_trace.push(json!({
                            "name": call.function_name, "ok": false,
                            "summary": "工具结果总量超限，已停止执行",
                        }));
                        continue;
                    }
                    // 白名单双保险：openai_tools 只给非 HighRisk，上游幻觉出
                    // 高危名也在此拦截（审计留痕）
                    let Some(def) = tools::find_tool(&call.function_name) else {
                        tool_trace.push(json!({
                            "name": call.function_name, "ok": false,
                            "summary": format!("未知工具: {}", call.function_name),
                        }));
                        messages.push(crate::bridge::ChatMessage {
                            role: Role::Tool,
                            content: Some(format!("未知工具: {}", call.function_name)),
                            reasoning: None,
                            content_blocks: None,
                            name: Some(call.function_name.clone()),
                            tool_call_id: Some(call.id.clone()),
                            tool_calls: None,
                        });
                        continue;
                    };
                    // #34 v2 双身份执行分派：
                    // - 管理员：registry exec_tool（非 HighRisk，与 Agent 同语义）
                    // - 普通用户：user_tool_allowed 判定（Allowlist/组启用/非高危）
                    //   → exec_user_tool（user_exec 执行体）；拒绝原因回灌 LLM
                    let out: ToolOutcome = if user.is_admin() {
                        let args: Value = serde_json::from_str(&call.arguments)
                            .unwrap_or_else(|_| json!({ "_raw": call.arguments }));
                        match tools::exec_tool(&state, &headers, &call.function_name, &args).await {
                            Ok(o) => o,
                            Err((_code, msg)) => ToolOutcome {
                                text: msg,
                                ok: false,
                            },
                        }
                    } else {
                        let perm = crate::agent::user_tool_policy::user_tool_allowed(
                            &policy,
                            &user.group,
                            &call.function_name,
                            def.risk,
                            def.user_exec.is_some(),
                        );
                        if perm != crate::agent::user_tool_policy::ToolPermission::Allow {
                            super::common::record_audit(
                                &state,
                                &user.email,
                                "assistant_tool_denied",
                                def.name,
                                None,
                                Some(json!({ "reason": format!("{perm:?}") })),
                            );
                            ToolOutcome {
                                text: format!(
                                    "工具 {} 不对当前用户开放（策略拒绝：{perm:?}）",
                                    def.label
                                ),
                                ok: false,
                            }
                        } else {
                            let args: Value = serde_json::from_str(&call.arguments)
                                .unwrap_or_else(|_| json!({ "_raw": call.arguments }));
                            match crate::agent::registry::exec_user_tool(
                                &state,
                                &headers,
                                &call.function_name,
                                &args,
                            )
                            .await
                            {
                                Ok(o) => o,
                                Err((_code, msg)) => ToolOutcome {
                                    text: msg,
                                    ok: false,
                                },
                            }
                        }
                    };
                    tool_total_chars += out.text.chars().count();
                    tool_trace.push(json!({
                        "name": call.function_name,
                        "ok": out.ok,
                        "summary": out.text.chars().take(200).collect::<String>(),
                    }));
                    messages.push(crate::bridge::ChatMessage {
                        role: Role::Tool,
                        content: Some(out.text),
                        reasoning: None,
                        content_blocks: None,
                        name: Some(call.function_name.clone()),
                        tool_call_id: Some(call.id.clone()),
                        tool_calls: None,
                    });
                }
            }
            _ => {
                final_content = content;
                break;
            }
        }
        if rounds >= max_rounds {
            final_content = content;
            break;
        }
    }

    // 计费 + 一条请求日志（守卫 settle；Drop 兜底已覆盖中途失败/断开）
    billing.settle();

    Ok(Json(json!({
        "success": true,
        "data": {
            "content": final_content,
            "rounds": rounds,
            "tool_calls": tool_trace,
            "usage": { "prompt_tokens": total_prompt, "completion_tokens": total_completion },
        }
    })))
}

/// 渠道选择（与 playground 同口径：管理员可指定，普通用户按模型路由）。
fn pick_channel(
    state: &AppState,
    model: &str,
    channel_id: Option<&str>,
    is_admin: bool,
) -> Result<crate::channel::Channel, (StatusCode, Json<Value>)> {
    if let Some(cid) = channel_id.filter(|s| !s.trim().is_empty()) {
        if !is_admin {
            return Err(error_response(
                "Only administrators can target a specific channel",
                StatusCode::FORBIDDEN,
            ));
        }
        return state
            .channel_store
            .get(cid.trim())
            .ok_or_else(|| error_response("Channel not found", StatusCode::NOT_FOUND));
    }
    let enabled: Vec<crate::channel::Channel> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|c| c.is_enabled())
        .collect();
    let picked = enabled
        .iter()
        .filter(|c| c.auto_pick_supports(model))
        .max_by_key(|c| c.priority)
        .cloned()
        .or_else(|| {
            enabled
                .iter()
                .filter(|c| c.models.is_empty())
                .max_by_key(|c| c.priority)
                .cloned()
        });
    picked.ok_or_else(|| {
        error_response(
            &format!("No enabled channel provides model '{model}'"),
            StatusCode::BAD_REQUEST,
        )
    })
}

/// 按渠道类型构造协议桥（账号型/CF 不支持——助手走渠道凭证直连）。
fn make_channel_bridge(
    state: &AppState,
    ch: &crate::channel::Channel,
) -> Result<Arc<dyn Bridge>, (StatusCode, Json<Value>)> {
    use crate::channel::ChannelType;
    let key = ch.decode_api_key();
    let client = state.http_client.clone();
    let bridge: Arc<dyn Bridge> = match ch.channel_type {
        ChannelType::OpenaiCompatible => crate::bridge::openai::make_bridge_with_quirks(
            &ch.base_url,
            &key,
            &client,
            ch.quirks.clone(),
        ),
        ChannelType::Anthropic => crate::bridge::anthropic::make_bridge_with_quirks(
            &ch.base_url,
            &key,
            &client,
            ch.quirks.clone(),
        ),
        ChannelType::Gemini => crate::bridge::gemini::make_bridge(&ch.base_url, &key, &client),
        ChannelType::Zai => crate::bridge::zai::make_bridge(&ch.base_url, &key, &client),
        ChannelType::Account | ChannelType::Cloudflare => {
            return Err(error_response(
                "该渠道类型不支持聊天助手（账号型请到账号池；CF 走数据面）",
                StatusCode::BAD_REQUEST,
            ));
        }
    };
    Ok(bridge)
}
