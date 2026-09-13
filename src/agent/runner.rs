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

use crate::agent::approval::{AgentApprovals, ApprovalResult, APPROVAL_TIMEOUT};
use crate::agent::llm;
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
pub async fn run(
    state: &AppState,
    headers: &HeaderMap,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
    approvals: &AgentApprovals,
    session_id: &str,
    on_event: &EventSink,
) {
    let mut convo: Vec<ChatMessage> = Vec::new();
    convo.push(llm::system_message(SYSTEM_PROMPT.to_string()));
    convo.extend(messages);

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
                let outcome = match spec {
                    None => tools::ToolOutcome {
                        text: format!("Unknown tool: {}", call.function_name),
                        ok: false,
                    },
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
                            tokio::time::timeout(APPROVAL_TIMEOUT, rx)
                                .await
                                .map(|r| r.unwrap_or(ApprovalResult::Denied))
                                .unwrap_or(ApprovalResult::Denied)
                        };
                        let approved = result != ApprovalResult::Denied;
                        if result == ApprovalResult::RememberAllow {
                            approvals.remember(session_id, &call.function_name);
                        }
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
                    Some(_) => {
                        match tools::exec_tool(state, headers, &call.function_name, &args).await {
                            Ok(o) => o,
                            Err((_code, msg)) => tools::ToolOutcome {
                                text: msg,
                                ok: false,
                            },
                        }
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
