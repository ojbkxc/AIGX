//! Agent 审计——全程留痕 + 决策回放。
//!
//! 复用现有 `common::record_audit`（→ LogStore.audits），不新造表。
//! 每个 Agent 工具调用记一条：谁（admin 会话邮箱）、何时、经 Agent
//! （action 以 `agent_` 前缀标记）、工具名与目标、before/after。

use axum::http::HeaderMap;
use serde_json::{json, Value};

use crate::api::admin::common::{admin_id_from_session, record_audit};
use crate::api::openai::AppState;

/// 记录一次 Agent 工具调用审计。
///
/// - `tool_name`：完整工具名（`aigx_*`）。
/// - `target`：操作目标描述（如 `id=ch-123`）。
/// - `ok`：工具是否执行成功。
/// - `before` / `after`：可选的前后状态（JSON）。
pub async fn record_agent_action(
    state: &AppState,
    headers: &HeaderMap,
    tool_name: &str,
    target: &str,
    ok: bool,
    before: Option<Value>,
    after: Option<Value>,
) {
    let admin_id = admin_id_from_session(state, headers).await;
    // aigx_list_channels → agent_list_channels（动作名带 agent 前缀标记来源）
    let action = format!("agent_{}", tool_name.trim_start_matches("aigx_"));
    record_audit(
        state,
        &admin_id,
        &action,
        target,
        before,
        after
            .map(|mut a| {
                if let Some(obj) = a.as_object_mut() {
                    obj.insert("success".to_string(), json!(ok));
                }
                a
            })
            .or_else(|| Some(json!({ "success": ok }))),
    );
}
