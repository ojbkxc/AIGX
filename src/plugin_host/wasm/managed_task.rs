// Ported from Agent2API v3.5.0-beta.2 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 受控任务（managed task）的**审批闸门 + 任务生命周期**。
//!
//! ## 对齐来源与边界（诚实）
//!
//! 上游的完整受控执行器（`wasm::managed_task::*`）是一整套绑定 monkeycode /
//! ACP 的对话、网络、回执、权限存储实现，且其真实审批入口与任务 ABI 在
//! 上游也处于关闭状态（见上游 `managed_task.rs` 头注）。AIGX 没有上游那套
//! 原生 provider/对话栈，因此这里落地**可复用、自洽的核心安全原语**：
//!
//! - **审批代次栅栏**（`Fence`）：按 provider 记录「是否已获运维明确批准」，
//!   任何配置变更 / 停用 / 卸载都会 `invalidate()` 递增代次，旧的批准与
//!   在途任务立即失效（fail-closed），与上游 `approval_fence` 同语义；
//! - **受控任务登记表**：`managed_task.begin` 只有在该 provider 已被批准时
//!   才成功，返回 `taskId`；`end`/`abort` 必须由同一 provider × account ×
//!   identity 的调用收尾，防止跨账号冒领。
//!
//! 插件声明 `http.managed-task.v1` 才可调用这些方法，且**必须先由宿主
//! 批准**。真实业务执行（ACP 驱动、模型选择）不在本模块，插件在网络侧做
//! 自己的调用，本模块只做「谁被允许、任务是否在途」的受控闸门。

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

use super::context::Context;
use super::wire::WireError;

const MAX_TASKS: usize = 128;

#[derive(Default)]
struct State {
    /// 审批代次（每次 invalidate / 配置变更递增）。
    generation: u64,
    /// 已批准（按 provider）。
    approved: HashSet<String>,
    /// 在途任务（taskId -> 绑定信息）。
    tasks: HashMap<String, Task>,
    /// 栅栏损坏（不可恢复）后一律拒绝。
    failed: bool,
}

struct Task {
    provider: String,
    account: String,
    identity: String,
    started_at: i64,
}

fn state() -> &'static Mutex<State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(State::default()))
}

fn unavailable() -> WireError {
    WireError::with_code("受控任务审批状态不可用", "plugin_unavailable")
}

fn forbidden(message: &str) -> WireError {
    WireError::with_code(message, "forbidden")
}

/// 批准某 provider 执行受控任务（运维显式确认；幂等）。
pub(crate) fn approve(provider: &str) {
    if let Ok(mut state) = state().lock() {
        if !state.failed {
            state.approved.insert(provider.to_string());
        }
    }
}

/// 撤销某 provider 的执行批准（立即生效；在途任务不强制中断但无法续期）。
pub(crate) fn revoke(provider: &str) {
    if let Ok(mut state) = state().lock() {
        state.approved.remove(provider);
    }
}

/// 该 provider 当前是否已批准。
pub fn is_approved(provider: &str) -> bool {
    state()
        .lock()
        .map(|state| !state.failed && state.approved.contains(provider))
        .unwrap_or(false)
}

/// 配置变更 / 停用 / 卸载时使全部批准与在途任务失效（fail-closed）。
pub(crate) fn invalidate() {
    if let Ok(mut state) = state().lock() {
        if state.failed {
            return;
        }
        state.generation = state.generation.saturating_add(1);
        state.approved.clear();
        state.tasks.clear();
    }
}

/// 不可恢复地关闭闸门（例如状态锁中毒）。
///
/// 当前没有内部调用方（锁中毒极少见）；保留为运维/上层可主动调用的
/// fail-closed 安全阀，按本模块分区 allow 标注以免掩盖真正的接线遗漏。
#[allow(dead_code)]
pub(crate) fn fail_closed() {
    if let Ok(mut state) = state().lock() {
        state.failed = true;
        state.approved.clear();
        state.tasks.clear();
    }
}

/// 摘除某 provider 的全部在途任务（停用时调用；批准由 `revoke`/`invalidate` 管）。
pub(crate) fn forget_provider(provider: &str) {
    if let Ok(mut state) = state().lock() {
        state.tasks.retain(|_, task| task.provider != provider);
    }
}

fn random_task_id() -> Result<String, WireError> {
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| unavailable())?;
    Ok(format!(
        "mtask_{}",
        nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

impl Context {
    /// 分派 `managed_task.status|begin|end|abort`。
    pub(super) async fn managed_task(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        self.permission("http.managed-task.v1")?;
        let provider = self.shared.manifest.provider.clone();
        match method {
            "managed_task.status" => Ok(self.managed_status(&provider)),
            "managed_task.begin" => self.managed_begin(&provider),
            "managed_task.end" | "managed_task.abort" => {
                self.managed_finish(&provider, method, params)
            }
            _ => Err(forbidden("未知的受控任务操作")),
        }
    }

    fn managed_status(&self, provider: &str) -> Value {
        let approved = is_approved(provider);
        let (generation, task_count, oldest_started_at) = state()
            .lock()
            .map(|state| {
                let oldest = state
                    .tasks
                    .values()
                    .filter(|task| task.provider == provider)
                    .map(|task| task.started_at)
                    .min();
                (
                    state.generation,
                    state
                        .tasks
                        .values()
                        .filter(|task| task.provider == provider)
                        .count(),
                    oldest,
                )
            })
            .unwrap_or((0, 0, None));
        json!({
            "supported": true,
            "approved": approved,
            "fenceGeneration": generation,
            "activeTasks": task_count,
            "oldestStartedAt": oldest_started_at,
        })
    }

    fn managed_begin(&self, provider: &str) -> Result<Value, WireError> {
        let account = self.account()?.to_string();
        let identity = self.identity.clone();
        let task_id = random_task_id()?;
        {
            let mut state = state().lock().map_err(|_| unavailable())?;
            if state.failed || !state.approved.contains(provider) {
                return Err(forbidden("该插件未获受控任务执行批准"));
            }
            if state.tasks.len() >= MAX_TASKS {
                return Err(WireError::with_code(
                    "受控任务数量已达上限",
                    "plugin_resource_limit",
                ));
            }
            state.tasks.insert(
                task_id.clone(),
                Task {
                    provider: provider.to_string(),
                    account,
                    identity,
                    started_at: super::now_ms(),
                },
            );
        }
        Ok(json!({ "taskId": task_id }))
    }

    fn managed_finish(
        &self,
        provider: &str,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        let task_id = params["taskId"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128)
            .ok_or_else(|| WireError::with_code("受控任务标识无效", "bad_params"))?;
        self.account()?;
        let account = self.account()?.to_string();
        let mut state = state().lock().map_err(|_| unavailable())?;
        let owns = state.tasks.get(task_id).is_some_and(|task| {
            task.provider == provider && task.account == account && task.identity == self.identity
        });
        if !owns {
            return Err(forbidden("受控任务不属于当前调用或账号"));
        }
        state.tasks.remove(task_id);
        Ok(json!({
            "taskId": task_id,
            "closed": true,
            "aborted": method == "managed_task.abort",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        if let Ok(mut state) = state().lock() {
            state.generation = 0;
            state.approved.clear();
            state.tasks.clear();
            state.failed = false;
        }
    }

    #[test]
    fn approve_then_invalidate_revokes() {
        reset();
        approve("p1");
        assert!(is_approved("p1"));
        invalidate();
        assert!(!is_approved("p1"));
    }

    #[test]
    fn revoke_is_immediate() {
        reset();
        approve("p2");
        revoke("p2");
        assert!(!is_approved("p2"));
    }

    #[test]
    fn fail_closed_rejects_everything() {
        reset();
        approve("p3");
        fail_closed();
        assert!(!is_approved("p3"));
        reset();
    }

    #[test]
    fn generation_increments_on_invalidate() {
        reset();
        let before = state().lock().unwrap_or_else(|e| e.into_inner()).generation;
        invalidate();
        let after = state().lock().unwrap_or_else(|e| e.into_inner()).generation;
        assert!(after > before);
    }

    #[test]
    fn forget_provider_drops_only_its_tasks() {
        reset();
        {
            let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
            state.tasks.insert(
                "t1".into(),
                Task {
                    provider: "a".into(),
                    account: "acc".into(),
                    identity: "id".into(),
                    started_at: 0,
                },
            );
            state.tasks.insert(
                "t2".into(),
                Task {
                    provider: "b".into(),
                    account: "acc".into(),
                    identity: "id".into(),
                    started_at: 0,
                },
            );
        }
        forget_provider("a");
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        assert!(!state.tasks.contains_key("t1"));
        assert!(state.tasks.contains_key("t2"));
    }
}
