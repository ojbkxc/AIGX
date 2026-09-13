//! Agent 多轮循环——LLM 推理 → 解析工具调用 → 执行 → 回填，直到产出最终答复。
//!
//! 阶段一：只读 + 低危写工具直接执行；高危写（阶段二）在审批矩阵接入前
//! 一律拒绝并提示（fail-closed，不静默放行）。
//!
//! 每轮产出事件（阶段一先返回事件列表，阶段二接 WebSocket 流式推送）：
//! `thinking`（本轮开始）/ `tool_call` / `tool_result` / `final`（最终答复）。

use std::sync::Arc;

use serde_json::Value;

use crate::agent::llm;
use crate::agent::tools::{self, RiskLevel};
use crate::api::openai::AppState;
use crate::bridge::ChatMessage;
use crate::config::AgentConfig;
use axum::http::HeaderMap;

/// 系统提示词（观察员视角，阶段一默认只读优先）。
const SYSTEM_PROMPT: &str = "你是 AIGX AI 网关的运维助手，通过白名单工具运维网关。\
优先用只读工具查清现状再下结论；写操作仅在用户明确要求且工具为低危时执行。\
回答要简洁、给出关键数据与结论。";

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
    /// 最终答复。
    Final { content: String },
    /// 错误终止。
    Error { message: String },
}

/// 运行结果。
pub struct RunOutcome {
    /// 最终答复内容（成功时非空）。
    pub final_content: Option<String>,
    /// 全程事件。
    pub events: Vec<AgentEvent>,
    /// 消耗轮数。
    pub turns: usize,
}

/// 运行 Agent 多轮循环（无流式，阶段一先走通非流式）。
///
/// `messages`：用户输入作为最后一条消息；函数会在最前插入系统提示词。
pub async fn run(
    state: &AppState,
    headers: &HeaderMap,
    config: &AgentConfig,
    messages: Vec<ChatMessage>,
) -> RunOutcome {
    let mut events = Vec::new();
    let mut convo: Vec<ChatMessage> = Vec::new();
    convo.push(llm::system_message(SYSTEM_PROMPT.to_string()));
    convo.extend(messages);

    let mut turns = 0;
    loop {
        turns += 1;
        if turns > config.max_turns {
            events.push(AgentEvent::Error {
                message: format!("已达最大轮数 {}，强制结束", config.max_turns),
            });
            return RunOutcome {
                final_content: None,
                events,
                turns,
            };
        }
        events.push(AgentEvent::Thinking { turn: turns });

        let tools = tools::openai_tools();
        let (resp, _upstream, _cid) =
            match llm::chat_once(state, config, convo.clone(), Some(tools)).await {
                Ok(r) => r,
                Err(e) => {
                    events.push(AgentEvent::Error {
                        message: e.to_string(),
                    });
                    return RunOutcome {
                        final_content: None,
                        events,
                        turns,
                    };
                }
            };

        let msg = llm::response_message(resp);

        // 有工具调用 → 逐个执行（阶段一只读/低危直跑，高危拒绝）
        if let Some(calls) = &msg.tool_calls {
            if calls.is_empty() {
                let content = msg.content.unwrap_or_default();
                events.push(AgentEvent::Final {
                    content: content.clone(),
                });
                return RunOutcome {
                    final_content: Some(content),
                    events,
                    turns,
                };
            }
            // 把 assistant 的工具调用消息回填进对话
            convo.push(msg.clone());

            let mut all_ok = true;
            for call in calls {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
                events.push(AgentEvent::ToolCall {
                    name: call.function_name.clone(),
                    arguments: call.arguments.clone(),
                });

                let spec = tools::find_tool(&call.function_name);
                let outcome = match spec {
                    None => tools::ToolOutcome {
                        text: format!("Unknown tool: {}", call.function_name),
                        ok: false,
                    },
                    Some(s) if s.risk == RiskLevel::HighRisk => tools::ToolOutcome {
                        text: "该工具为高危写操作，需审批矩阵（阶段二接入）；当前已安全拒绝。"
                            .to_string(),
                        ok: false,
                    },
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

                if !outcome.ok {
                    all_ok = false;
                }
                events.push(AgentEvent::ToolResult {
                    name: call.function_name.clone(),
                    ok: outcome.ok,
                    text: outcome.text.clone(),
                });
                // 工具结果作为 tool 消息回填
                convo.push(llm::tool_message(call.id.clone(), outcome.text));
            }
            // 工具执行完继续下一轮推理（把结果交给模型总结）
            let _ = all_ok;
            continue;
        }

        // 无工具调用 → 最终答复
        let content = msg.content.unwrap_or_default();
        events.push(AgentEvent::Final {
            content: content.clone(),
        });
        return RunOutcome {
            final_content: Some(content),
            events,
            turns,
        };
    }
}

/// 供外部（API 层）复用的共享引用类型。
#[allow(dead_code)]
pub type RunnerRef = Arc<
    dyn Fn(
            &AppState,
            &HeaderMap,
            &AgentConfig,
            Vec<ChatMessage>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RunOutcome> + Send>>
        + Send
        + Sync,
>;
