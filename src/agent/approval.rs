//! 审批矩阵——高危写操作挂起人工确认（阶段 2）。
//!
//! 链路：runner 遇高危工具 → [`AgentApprovals::request`] 生成 request_id 并挂起
//! → 推 `approval_request` 事件给前端 → 前端弹卡 → `POST /api/agent/.../approve`
//! → [`AgentApprovals::resolve`] 唤醒 runner → 允许则执行 / 拒绝则跳过。
//!
//! 参考 rust-tunnel `approval.rs` 的 allow_once / allow_always / reject 语义。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::oneshot;

/// 审批结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResult {
    /// 允许一次。
    Approved,
    /// 拒绝。
    Denied,
    /// 本会话总是允许该工具。
    RememberAllow,
}

/// 挂起的审批请求表：`request_id → (工具名, 唤醒 sender)`。
type Pending = HashMap<String, (String, oneshot::Sender<ApprovalResult>)>;

/// 审批运行时（挂在 [`crate::agent::AgentState`]）。
#[derive(Clone, Default)]
pub struct AgentApprovals {
    pending: Arc<Mutex<Pending>>,
    /// 本会话已记住允许的工具名集合（进程内存态，重启清零）。
    remembered: Arc<Mutex<HashMap<String, HashSet<String>>>>,
}

impl AgentApprovals {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否已记住允许该工具（本会话免审）。
    pub fn is_remembered(&self, session_id: &str, tool: &str) -> bool {
        self.remembered
            .lock()
            .get(session_id)
            .is_some_and(|set| set.contains(tool))
    }

    /// 记入本会话允许集。
    pub fn remember(&self, session_id: &str, tool: &str) {
        self.remembered
            .lock()
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.to_string());
    }

    /// 挂起一个审批请求，返回 (`request_id`, 等待结果的 receiver)。
    pub fn request(&self, tool: &str) -> (String, oneshot::Receiver<ApprovalResult>) {
        let request_id = format!("{:032x}", rand::random::<u128>());
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .insert(request_id.clone(), (tool.to_string(), tx));
        (request_id, rx)
    }

    /// 前端审批响应：唤醒对应 pending。
    pub fn resolve(&self, request_id: &str, result: ApprovalResult) -> bool {
        if let Some((_, tx)) = self.pending.lock().remove(request_id) {
            let _ = tx.send(result);
            true
        } else {
            false
        }
    }

    /// 移除挂起请求（超时/断连清理）。
    pub fn remove(&self, request_id: &str) {
        self.pending.lock().remove(request_id);
    }

    /// 当前挂起数（测试用）。
    #[allow(dead_code)]
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }
}

/// 审批等待超时（与 rust-tunnel 一致 5 分钟）。
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
