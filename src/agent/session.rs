//! Agent 会话持久化——用 FileStore（PG kv）存会话与消息。
//!
//! 键设计：
//! - `agent_session:{id}` → 会话元信息（标题/模型/渠道/角色/创建时间）
//! - `agent_msg:{session_id}:{seq}` → 单条消息（role/content/tool_calls）
//!
//! seq 用 8 位零填充保证字典序 = 时间序，分页取最新一条用 `list_latest_keys`。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::storage::FileStore;

const SESSION_PREFIX: &str = "agent_session:";
const MSG_PREFIX: &str = "agent_msg:";

/// 会话角色（对齐审批矩阵语义）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentRole {
    /// 观察员：只读工具，不能写。
    Observer,
    /// 运维员：可写（低危自动 + 高危审批）。
    Operator,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self::Observer
    }
}

/// 会话元信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: String,
    pub title: String,
    pub model: String,
    pub channel: String,
    pub role: AgentRole,
    pub created_at: i64,
}

/// 单条消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub role: String,
    pub content: String,
    /// 工具调用 JSON（assistant 消息携带）。
    #[serde(default)]
    pub tool_calls: Option<serde_json::Value>,
    /// 工具结果（tool 消息携带）。
    #[serde(default)]
    pub tool_result: Option<serde_json::Value>,
}

/// Agent 会话存储。
pub struct AgentSessionStore {
    store: Arc<FileStore>,
}

impl AgentSessionStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }

    fn session_key(id: &str) -> String {
        format!("{SESSION_PREFIX}{id}")
    }

    fn msg_key(session_id: &str, seq: u64) -> String {
        format!("{MSG_PREFIX}{session_id}:{seq:08}")
    }

    /// 创建会话。
    pub fn create(
        &self,
        id: &str,
        title: &str,
        model: &str,
        channel: &str,
        role: AgentRole,
    ) -> anyhow::Result<()> {
        let s = AgentSession {
            id: id.to_string(),
            title: title.to_string(),
            model: model.to_string(),
            channel: channel.to_string(),
            role,
            created_at: now_ts(),
        };
        self.store.put(&Self::session_key(id), &s)
    }

    /// 读会话。
    pub fn get(&self, id: &str) -> anyhow::Result<Option<AgentSession>> {
        self.store.get(&Self::session_key(id))
    }

    /// 列出全部会话（按创建时间倒序）。
    pub fn list(&self) -> anyhow::Result<Vec<AgentSession>> {
        let keys = self.store.list(SESSION_PREFIX)?;
        let mut out = Vec::new();
        for k in keys {
            if let Some(s) = self.store.get::<AgentSession>(&k)? {
                out.push(s);
            }
        }
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    /// 追加一条消息，返回 seq。
    pub fn append_message(&self, session_id: &str, msg: &AgentMessage) -> anyhow::Result<u64> {
        let seq = self.next_seq(session_id)?;
        self.store.put(&Self::msg_key(session_id, seq), msg)?;
        Ok(seq)
    }

    /// 读会话全部消息（按 seq 升序）。
    pub fn messages(&self, session_id: &str) -> anyhow::Result<Vec<AgentMessage>> {
        let keys = self.store.list(&format!("{MSG_PREFIX}{session_id}:"))?;
        let mut out = Vec::new();
        for k in keys {
            if let Some(m) = self.store.get::<AgentMessage>(&k)? {
                out.push(m);
            }
        }
        Ok(out)
    }

    /// 下一序号（= 已有消息数）。
    fn next_seq(&self, session_id: &str) -> anyhow::Result<u64> {
        let keys = self.store.list(&format!("{MSG_PREFIX}{session_id}:"))?;
        Ok(keys.len() as u64)
    }

    /// 删除会话及其消息。
    pub fn delete(&self, id: &str) -> anyhow::Result<()> {
        for k in self.store.list(&format!("{MSG_PREFIX}{id}:"))? {
            self.store.delete(&k)?;
        }
        self.store.delete(&Self::session_key(id))
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
