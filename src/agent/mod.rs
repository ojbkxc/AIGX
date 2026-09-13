//! AI 运维 Agent 工作台——自举式运维：Agent 用 AIGX 自己的渠道推理，去运维 AIGX 自己。
//!
//! 分层（阶段一落地）：
//! - [`llm`]：自环推理——进程内直调 bridge（复用渠道调度/熔断/亲和），
//!   不经 HTTP 端口与计费，模型/渠道由 `[agent]` 配置决定。
//! - [`tools`]：工具注册表（只读/低危/高危三层）——阶段一先落只读层，
//!   复用现有管理面 handler。
//! - [`runner`]：多轮循环（LLM 推理 → 解析工具调用 → 执行 → 回填）。
//! - [`session`]：会话持久化（FileStore KV，`agent_session:`/`agent_msg:` 前缀）。
//! - [`approval`]：审批矩阵（高危写挂起人工确认，阶段二接入）。
//! - [`audit`]：全程审计留痕 + 决策回放。
//!
//! 安全底线：写操作分低危（自动+审计）与高危（审批矩阵人工确认），
//! 见 [`tools::RiskLevel`]；Agent 默认只读观察员，切角色才解锁写工具。

pub mod api;
pub mod approval;
pub mod audit;
pub mod llm;
pub mod runner;
pub mod session;
pub mod tools;

use std::sync::Arc;

use crate::config::AgentConfig;
use crate::storage::FileStore;

/// Agent 运行时共享状态（挂在 [`crate::api::openai::AppState`] 上）。
///
/// - `store`：会话持久化 KV（与业务共用的 FileStore）。
/// - `config`：`[agent]` 配置快照。
/// - `approvals`：高危写审批 pending 表（阶段二接入 runner）。
/// - `session_store`：会话/消息持久化。
#[derive(Clone)]
pub struct AgentState {
    /// 会话持久化用的 KV 存储（与业务共用的 FileStore）。
    pub store: Arc<FileStore>,
    /// `[agent]` 配置快照（启动时加载，后续可经配置更新刷新）。
    pub config: AgentConfig,
    /// 审批运行时（阶段二接入）。
    pub approvals: approval::AgentApprovals,
    /// 会话/消息持久化。
    pub session_store: session::AgentSessionStore,
}

impl AgentState {
    /// 构造 Agent 运行时状态。
    pub fn new(store: Arc<FileStore>, config: AgentConfig) -> Self {
        let session_store = session::AgentSessionStore::new(store.clone());
        Self {
            store,
            config,
            approvals: approval::AgentApprovals::new(),
            session_store,
        }
    }
}
