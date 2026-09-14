//! Agent 多轮循环——LLM 推理 → 解析工具调用 → 执行 → 回填，直到产出最终答复。
//!
//! 阶段二：只读/低危写直接执行；高危写经审批矩阵挂起等人工确认
//! （[`crate::agent::approval::AgentApprovals`]）。
//!
//! 事件实时推送：`run` 通过 `on_event` 回调边跑边发（SSE 流式渲染），
//! 而非攒到最后一次性返回。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use crate::agent::approval::{self, AgentApprovals, ApprovalResult};
use crate::agent::audit::record_agent_action;
use crate::agent::llm;
use crate::agent::session::AgentRole;
use crate::agent::tools::{self, RiskLevel};
use crate::api::openai::AppState;
use crate::bridge::ChatMessage;
use crate::config::AgentConfig;
use axum::http::HeaderMap;

/// 系统提示词（观察员视角，默认只读优先）。
const SYSTEM_PROMPT: &str = "你是 AIGX AI 网关的运维助手，通过白名单工具运维网关。\
优先用只读工具查清现状再下结论；写操作仅在用户明确要求且工具为低危时执行。\
高危写操作会经人工审批。回答要简洁、给出关键数据与结论。";

/// 一轮执行的事件。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 本轮开始（第 N 轮）。
    Thinking { turn: usize },
    /// 工具调用开始。
    ToolCall { name: String, arguments: String },
    /// 工具执行结果。
    ToolResult {
        name: String,
        ok: bool,
        text: String,
    },
    /// 高危工具审批请求（前端弹卡）。
    ApprovalRequest {
        request_id: String,
        name: String,
        arguments: String,
    },
    /// 审批结果（允许/拒绝/超时）。
    ApprovalResolved { name: String, approved: bool },
    /// 最终答复。
    Final { content: String },
    /// 错误终止。
    Error { message: String },
}

/// 事件回调类型。
pub type EventSink =
    Arc<dyn Fn(AgentEvent) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// 运行 Agent 多轮循环（回调式，边跑边推事件）。
///
/// `messages`：用户输入作为最后一条消息；函数会在最前插入系统提示词。
/// `role`：会话角色——观察员只能执行只读工具，写工具一律拒绝。
#[allow(clippy::too_many_arguments)]
pub async fn run(
    state: &AppState,
    headers: &HeaderMap,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
    approvals: &AgentApprovals,
    session_id: &str,
    role: AgentRole,
    on_event: &EventSink,
) {
    let mut convo: Vec<ChatMessage> = Vec::new();
    convo.push(llm::system_message(SYSTEM_PROMPT.to_string()));
    convo.extend(messages);

    // 审批超时读配置（0/缺失回退缺省 300s），不再硬编码
    let approval_timeout = approval::approval_timeout_from(config.approval_timeout_secs);

    let mut turns = 0;
    loop {
        turns += 1;
        if turns > config.max_turns {
            on_event(AgentEvent::Error {
                message: format!("已达最大轮数 {}，强制结束", config.max_turns),
            })
            .await;
            return;
        }
        on_event(AgentEvent::Thinking { turn: turns }).await;

        let tools = tools::openai_tools();
        let (resp, _upstream, _cid) =
            match llm::chat_once(state, config, convo.clone(), Some(tools)).await {
                Ok(r) => r,
                Err(e) => {
                    on_event(AgentEvent::Error {
                        message: e.to_string(),
                    })
                    .await;
                    return;
                }
            };

        let msg = llm::response_message(resp);

        if let Some(calls) = &msg.tool_calls {
            if calls.is_empty() {
                let content = msg.content.unwrap_or_default();
                on_event(AgentEvent::Final {
                    content: content.clone(),
                })
                .await;
                return;
            }
            convo.push(msg.clone());

            for call in calls {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                on_event(AgentEvent::ToolCall {
                    name: call.function_name.clone(),
                    arguments: call.arguments.clone(),
                })
                .await;

                let spec = tools::find_tool(&call.function_name);
                // 工具审计 target：优先取主目标参数，回退原始参数摘要
                let target = tool_target(&call.function_name, &args);
                let outcome = match spec {
                    None => tools::ToolOutcome {
                        text: format!("Unknown tool: {}", call.function_name),
                        ok: false,
                    },
                    // 观察员：只读工具放行，写工具（低危/高危）一律拒绝
                    Some(ref s) if !role.allows_write() && s.risk != RiskLevel::ReadOnly => {
                        tools::ToolOutcome {
                            text: format!(
                                "当前会话角色为观察员，禁止执行写工具 {}（需切运维员角色）",
                                call.function_name
                            ),
                            ok: false,
                        }
                    }
                    Some(s) if s.risk == RiskLevel::HighRisk => {
                        // 审批矩阵：挂起等人工确认；RememberAllow 记入本会话免审集
                        let already = approvals.is_remembered(session_id, &call.function_name);
                        let result = if already {
                            ApprovalResult::Approved
                        } else {
                            let (req_id, rx) = approvals.request(&call.function_name);
                            on_event(AgentEvent::ApprovalRequest {
                                request_id: req_id.clone(),
                                name: call.function_name.clone(),
                                arguments: call.arguments.clone(),
                            })
                            .await;
                            tokio::time::timeout(approval_timeout, rx)
                                .await
                                .map(|r| r.unwrap_or(ApprovalResult::Denied))
                                .unwrap_or(ApprovalResult::Denied)
                        };
                        let approved = result != ApprovalResult::Denied;
                        if result == ApprovalResult::RememberAllow {
                            approvals.remember(session_id, &call.function_name);
                        }
                        // 审批决策落审计：谁在何时允许/拒绝了哪个高危工具（含免审来源）
                        let (action, decision) = if already {
                            ("agent_approval_remembered", "remembered(免审)")
                        } else if result == ApprovalResult::RememberAllow {
                            ("agent_approval_remember", "remember(本会话总是允许)")
                        } else if approved {
                            ("agent_approval_allow", "allow")
                        } else {
                            ("agent_approval_deny", "deny")
                        };
                        record_agent_action(
                            state,
                            headers,
                            action,
                            &format!("{} {target}", call.function_name),
                            approved,
                            None,
                            Some(serde_json::json!({
                                "decision": decision,
                                "session": session_id,
                            })),
                        )
                        .await;
                        if !approved {
                            on_event(AgentEvent::ApprovalResolved {
                                name: call.function_name.clone(),
                                approved: false,
                            })
                            .await;
                            tools::ToolOutcome {
                                text: "高危写操作已被拒绝（未获人工审批）".to_string(),
                                ok: false,
                            }
                        } else {
                            on_event(AgentEvent::ApprovalResolved {
                                name: call.function_name.clone(),
                                approved: true,
                            })
                            .await;
                            match tools::exec_tool(state, headers, &call.function_name, &args).await
                            {
                                Ok(o) => o,
                                Err((_code, msg)) => tools::ToolOutcome {
                                    text: msg,
                                    ok: false,
                                },
                            }
                        }
                    }
                    Some(s) => {
                        let out = match tools::exec_tool(state, headers, &call.function_name, &args)
                            .await
                        {
                            Ok(o) => o,
                            Err((_code, msg)) => tools::ToolOutcome {
                                text: msg,
                                ok: false,
                            },
                        };
                        // 工具调用落审计：写操作必留痕；只读操作成功时不刷审计噪音，
                        // 失败仍记录（便于排查"为什么查不到"）
                        if s.risk != RiskLevel::ReadOnly || !out.ok {
                            record_agent_action(
                                state,
                                headers,
                                &call.function_name,
                                &target,
                                out.ok,
                                None,
                                Some(serde_json::json!({
                                    "risk": s.risk.as_str(),
                                    "role": role.as_str(),
                                    "session": session_id,
                                })),
                            )
                            .await;
                        }
                        out
                    }
                };

                on_event(AgentEvent::ToolResult {
                    name: call.function_name.clone(),
                    ok: outcome.ok,
                    text: outcome.text.clone(),
                })
                .await;
                convo.push(llm::tool_message(call.id.clone(), outcome.text));
            }
            continue;
        }

        let content = msg.content.unwrap_or_default();
        on_event(AgentEvent::Final {
            content: content.clone(),
        })
        .await;
        return;
    }
}

/// 空 sink（测试用）。
#[allow(dead_code)]
pub fn null_sink() -> EventSink {
    Arc::new(|_ev| Box::pin(async {}))
}

/// 提取工具调用的人类可读目标（审计 target 列展示用）。
/// 按工具取主目标参数；查不到时回退参数键值摘要（截断防刷屏）。
fn tool_target(name: &str, args: &Value) -> String {
    let key = match name {
        "aigx_user_delete" | "aigx_user_manage" => "user_id",
        "aigx_channel_delete"
        | "aigx_channel_enable"
        | "aigx_channel_disable"
        | "aigx_channel_test"
        | "aigx_reset_circuit_breaker" => "channel_id",
        "aigx_channel_add" => "name",
        "aigx_order_delete" => "trade_no",
        "aigx_pricing_upsert" | "aigx_pricing_delete" => "model_name",
        "aigx_key_delete" => "key_id",
        "aigx_key_add" => "name",
        "aigx_group_upsert" => "name",
        _ => "",
    };
    if !key.is_empty() {
        if let Some(v) = args.get(key).and_then(|v| v.as_str()) {
            return format!("{key}={v}");
        }
    }
    let s = args.to_string();
    if s.chars().count() > 120 {
        let truncated: String = s.chars().take(120).collect();
        format!("{truncated}…")
    } else {
        s
    }
}
