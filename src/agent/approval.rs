//! 审批矩阵——高危写操作挂起人工确认。
//!
//! 链路：runner 遇高危工具 → [`AgentApprovals::request`] 生成 request_id 并挂起
//! → 推 `approval_request` 事件给前端 → 前端弹卡 → `POST /api/agent/approvals/:id`
//! → [`AgentApprovals::resolve`] 唤醒 runner → 允许则执行 / 拒绝则跳过。
//!
//! ## 持久化（FileStore KV）
//!
//! 此前 pending/remembered 纯内存：超时后 pending 表项无人清理（泄漏），
//! 重启后「本会话总是允许」丢失。现在落盘：
//!
//! - `agent_approval:{request_id}` → [`StoredApproval`]（终态留痕，7 天后清扫）
//! - `agent_approval_remembered:{session_id}` → 免审工具名列表
//!
//! 重启后 pending **不复活**（oneshot sender 已死，恢复无意义）——启动清扫把
//! `status=="pending"` 改写为 `timeout`。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::storage::FileStore;

const PENDING_PREFIX: &str = "agent_approval:";
const REMEMBERED_PREFIX: &str = "agent_approval_remembered:";

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

/// 落盘的审批记录（pending → 终态留痕）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredApproval {
    pub request_id: String,
    pub tool: String,
    pub session_id: String,
    /// 参数摘要（脱敏由调用方负责，这里只存 JSON 文本）。
    #[serde(default)]
    pub args_json: String,
    pub created_at: i64,
    /// pending / approved / denied / remembered / timeout
    pub status: String,
}

/// 挂起的审批请求表：`request_id → (工具名, 唤醒 sender)`。
type Pending = HashMap<String, (String, oneshot::Sender<ApprovalResult>)>;

/// 审批运行时（挂在 [`crate::agent::AgentState`]）。
#[derive(Clone, Default)]
pub struct AgentApprovals {
    pending: Arc<Mutex<Pending>>,
    /// 本会话已记住允许的工具名集合。
    remembered: Arc<Mutex<HashMap<String, HashSet<String>>>>,
    /// None = 纯内存模式（测试用）；Some = 落盘。
    store: Option<Arc<FileStore>>,
}

impl AgentApprovals {
    /// 纯内存模式（测试）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 落盘模式：加载免审集合，并把上次残留的 pending 标记为 timeout。
    pub fn with_store(store: Arc<FileStore>) -> Self {
        let this = Self {
            pending: Arc::new(Mutex::new(Pending::new())),
            remembered: Arc::new(Mutex::new(HashMap::new())),
            store: Some(store.clone()),
        };
        // 免审集合回灌内存
        if let Ok(keys) = store.list(REMEMBERED_PREFIX) {
            for key in keys {
                if let Ok(Some(list)) = store.get::<Vec<String>>(&key) {
                    let session = key.trim_start_matches(REMEMBERED_PREFIX).to_string();
                    this.remembered
                        .lock()
                        .insert(session, list.into_iter().collect());
                }
            }
        }
        // stale pending 清扫：oneshot 已死，改写 timeout 留痕，不复活
        if let Ok(keys) = store.list(PENDING_PREFIX) {
            for key in keys {
                if let Ok(Some(mut rec)) = store.get::<StoredApproval>(&key) {
                    if rec.status == "pending" {
                        rec.status = "timeout".to_string();
                        let _ = store.put(&key, &rec);
                    }
                }
            }
        }
        this
    }

    /// 是否已记住允许该工具（本会话免审）。
    pub fn is_remembered(&self, session_id: &str, tool: &str) -> bool {
        self.remembered
            .lock()
            .get(session_id)
            .is_some_and(|set| set.contains(tool))
    }

    /// 记入本会话允许集（落盘）。
    pub fn remember(&self, session_id: &str, tool: &str) {
        let mut remembered = self.remembered.lock();
        remembered
            .entry(session_id.to_string())
            .or_default()
            .insert(tool.to_string());
        if let Some(store) = &self.store {
            let list: Vec<String> = remembered
                .get(session_id)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            let _ = store.put(&format!("{REMEMBERED_PREFIX}{session_id}"), &list);
        }
    }

    /// 挂起一个审批请求，返回 (`request_id`, 等待结果的 receiver)。
    pub fn request(
        &self,
        session_id: &str,
        tool: &str,
        args_json: &str,
    ) -> (String, oneshot::Receiver<ApprovalResult>) {
        let request_id = format!("{:032x}", rand::random::<u128>());
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .insert(request_id.clone(), (tool.to_string(), tx));
        if let Some(store) = &self.store {
            let rec = StoredApproval {
                request_id: request_id.clone(),
                tool: tool.to_string(),
                session_id: session_id.to_string(),
                args_json: args_json.to_string(),
                created_at: chrono::Utc::now().timestamp(),
                status: "pending".to_string(),
            };
            let _ = store.put(&format!("{PENDING_PREFIX}{request_id}"), &rec);
        }
        (request_id, rx)
    }

    /// 前端审批响应：唤醒对应 pending，并落终态。
    pub fn resolve(&self, request_id: &str, result: ApprovalResult) -> bool {
        if let Some((_, tx)) = self.pending.lock().remove(request_id) {
            let _ = tx.send(result);
            self.persist_status(
                request_id,
                match result {
                    ApprovalResult::Approved => "approved",
                    ApprovalResult::Denied => "denied",
                    ApprovalResult::RememberAllow => "remembered",
                },
            );
            true
        } else {
            false
        }
    }

    /// 移除挂起请求（超时/断连清理，落 timeout 留痕）。
    pub fn remove(&self, request_id: &str) {
        self.pending.lock().remove(request_id);
        self.persist_status(request_id, "timeout");
    }

    /// 删除某会话的免审集（会话删除时调用）。
    pub fn forget_session(&self, session_id: &str) {
        self.remembered.lock().remove(session_id);
        if let Some(store) = &self.store {
            let _ = store.delete(&format!("{REMEMBERED_PREFIX}{session_id}"));
        }
    }

    /// 当前挂起数（`aigx_agent_self_status` 只读工具汇报审批队列现状用，#35 批1）。
    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    fn persist_status(&self, request_id: &str, status: &str) {
        let Some(store) = &self.store else {
            return;
        };
        let key = format!("{PENDING_PREFIX}{request_id}");
        if let Ok(Some(mut rec)) = store.get::<StoredApproval>(&key) {
            rec.status = status.to_string();
            let _ = store.put(&key, &rec);
        }
    }
}

/// 审批等待超时（与 rust-tunnel 一致 5 分钟）。
/// 仅作为配置为 0 时的回退缺省值；实际超时读
/// `config.agent.approval_timeout_secs`（见 runner::run）。
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

/// 从配置计算审批超时（0/缺失回退缺省 300s）。
pub fn approval_timeout_from(secs: u64) -> Duration {
    if secs == 0 {
        APPROVAL_TIMEOUT
    } else {
        Duration::from_secs(secs)
    }
}
