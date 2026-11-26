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
//! - **受控任务登记表**：`managed_task.create`（旧名 `begin`）只有在该 provider
//!   已被批准时才成功，返回 `taskId`；`close`（旧名 `end`/`abort`）必须由同一
//!   provider × account × identity 的调用收尾，防止跨账号冒领。上游的
//!   `observe` / `read` 为只读登记查询。
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

/// 上游受控任务方法 → AIGX 内部操作。
///
/// 上游（v3.5.1 `wasm/managed_task/service.rs:23-27`）的方法集是
/// `create / observe / read / status / close`；AIGX 早先自建的是
/// `status / begin / end / abort`。两套名字都路由到同一组操作，确保
/// 「上游插件能跑、AIGX 现有插件不坏」：
///
/// - `create` ← `begin`：申请一个受控任务句柄；
/// - `close`  ← `end` / `abort`：结清句柄（`abort` 仅额外回报 `aborted=true`）；
/// - `observe` / `read`：新增的**只读**登记查询（不改变任何状态）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Create,
    Observe,
    Read,
    Status,
    Close { abort: bool },
}

fn route(method: &str) -> Option<Op> {
    match method {
        "managed_task.create" | "managed_task.begin" => Some(Op::Create),
        "managed_task.observe" => Some(Op::Observe),
        "managed_task.read" => Some(Op::Read),
        "managed_task.status" => Some(Op::Status),
        "managed_task.close" | "managed_task.end" => Some(Op::Close { abort: false }),
        "managed_task.abort" => Some(Op::Close { abort: true }),
        _ => None,
    }
}

/// 从参数里取任务句柄：上游用 `id`，AIGX 旧接口用 `taskId`，两者都接受。
fn task_id_param(params: &Value) -> Option<&str> {
    params
        .get("id")
        .or_else(|| params.get("taskId"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128)
}

fn invalid_task_id() -> WireError {
    WireError::with_code("受控任务标识无效", "bad_params")
}

impl Context {
    /// 分派受控任务方法（上游 `create/observe/read/status/close` + AIGX 旧别名）。
    pub(super) async fn managed_task(
        &mut self,
        method: &str,
        params: &Value,
    ) -> Result<Value, WireError> {
        self.permission("http.managed-task.v1")?;
        let provider = self.shared.manifest.provider.clone();
        match route(method) {
            Some(Op::Create) => self.managed_begin(&provider, params),
            Some(Op::Observe) => self.managed_observe(&provider, params),
            Some(Op::Read) => self.managed_read(&provider, params),
            Some(Op::Status) => self.managed_status(&provider, params),
            Some(Op::Close { abort }) => self.managed_finish(&provider, abort, params),
            None => Err(forbidden("未知的受控任务操作")),
        }
    }

    /// `status`：上游必须带 `{id}`；AIGX 旧调用不带参数 → 返回 provider 级视图。
    fn managed_status(&self, provider: &str, params: &Value) -> Result<Value, WireError> {
        let id = match task_id_param(params) {
            Some(id) => {
                self.owned_task(provider, id)?;
                Some(id.to_string())
            }
            // 带了 id/taskId 但形状非法 → bad_params；完全没带 → 旧式无参调用。
            None if params.get("id").is_some() || params.get("taskId").is_some() => {
                return Err(invalid_task_id())
            }
            None => None,
        };
        let mut value = status_json(provider);
        let approved = value["approved"].as_bool().unwrap_or(false);
        let has_id = id.is_some();
        value["id"] = id.map(Value::from).unwrap_or(Value::Null);
        value["status"] = json!(if !approved {
            "not_sent"
        } else if has_id {
            "owned"
        } else {
            "idle"
        });
        value["phase"] = json!(if approved { "owned" } else { "not_sent" });
        value["submitted"] = json!(approved);
        value["done"] = json!(false);
        Ok(value)
    }

    /// `create` / `begin`：上游传入 `{body}`（原始 JSON 文本）；旧 `begin` 无参数。
    fn managed_begin(&self, provider: &str, params: &Value) -> Result<Value, WireError> {
        if let Some(body) = params.get("body") {
            if body.as_str().is_none() {
                return Err(WireError::with_code(
                    "任务 body 必须是固定任务结构的原始 JSON 文本",
                    "bad_params",
                ));
            }
        }
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
        Ok(json!({
            "id": task_id.clone(),
            "taskId": task_id,
            "status": "created",
            "supported": true,
            "approved": true,
            "done": false,
        }))
    }

    /// `observe`：只读。校验句柄归属后回报「可观察」，不改登记状态。
    fn managed_observe(&self, provider: &str, params: &Value) -> Result<Value, WireError> {
        let id = task_id_param(params)
            .ok_or_else(invalid_task_id)?
            .to_string();
        self.owned_task(provider, &id)?;
        Ok(json!({
            "id": id.clone(),
            "taskId": id,
            "status": "observing",
            "done": false,
        }))
    }

    /// `read`：只读。AIGX 不做任务内容观测，只回报该句柄的登记状态。
    fn managed_read(&self, provider: &str, params: &Value) -> Result<Value, WireError> {
        let id = task_id_param(params)
            .ok_or_else(invalid_task_id)?
            .to_string();
        let started_at = self.owned_task(provider, &id)?;
        Ok(json!({
            "id": id.clone(),
            "taskId": id,
            "status": "pending",
            "phase": "owned",
            "startedAt": started_at,
            "done": false,
            "note": "AIGX 宿主不做任务内容观测，仅回报登记状态",
        }))
    }

    /// `close` / `end` / `abort`：结清句柄。`abort` 额外回报 `aborted=true`。
    fn managed_finish(
        &self,
        provider: &str,
        abort: bool,
        params: &Value,
    ) -> Result<Value, WireError> {
        let task_id = task_id_param(params).ok_or_else(invalid_task_id)?;
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
            "id": task_id,
            "taskId": task_id,
            "closed": true,
            "aborted": abort,
        }))
    }

    /// 校验句柄属于当前 provider × account × identity，返回登记的开始时刻。
    fn owned_task(&self, provider: &str, id: &str) -> Result<i64, WireError> {
        let account = self.account()?.to_string();
        let state = state().lock().map_err(|_| unavailable())?;
        match state.tasks.get(id) {
            Some(task)
                if task.provider == provider
                    && task.account == account
                    && task.identity == self.identity =>
            {
                Ok(task.started_at)
            }
            _ => Err(forbidden("受控任务不属于当前调用或账号")),
        }
    }
}

/// provider 级状态视图（`status` 的旧字段，逐字保留）。
fn status_json(provider: &str) -> Value {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    /// 测试间串行锁：本模块的状态是**进程级单例**（[`state()`]），而 cargo 默认
    /// 并行跑测试 —— 两个用例各自 `reset()` 再断言，彼此会把对方的批准/失效
    /// 状态踩掉。CI 上实测过 `approve_then_invalidate_revokes` 偶发
    /// `assertion failed: is_approved("p1")`（同一进程里另一个用例先 `invalidate()`）。
    ///
    /// 用一把锁把用例串起来，而不是让每个用例自己重试：顺序依赖不该靠运气。
    fn serial() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let lock = LOCK.get_or_init(|| Mutex::new(()));
        // 前一个用例 panic 会毒化这把锁；这里要把毒化恢复掉，否则一次失败
        // 会让后面所有用例连带 panic（真正的失败原因被淹没）。
        lock.lock().unwrap_or_else(|poison| poison.into_inner())
    }

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
        let _guard = serial();
        reset();
        approve("p1");
        assert!(is_approved("p1"));
        invalidate();
        assert!(!is_approved("p1"));
    }

    #[test]
    fn revoke_is_immediate() {
        let _guard = serial();
        reset();
        approve("p2");
        revoke("p2");
        assert!(!is_approved("p2"));
    }

    #[test]
    fn fail_closed_rejects_everything() {
        let _guard = serial();
        reset();
        approve("p3");
        fail_closed();
        assert!(!is_approved("p3"));
        reset();
    }

    #[test]
    fn generation_increments_on_invalidate() {
        let _guard = serial();
        reset();
        let before = state().lock().unwrap_or_else(|e| e.into_inner()).generation;
        invalidate();
        let after = state().lock().unwrap_or_else(|e| e.into_inner()).generation;
        assert!(after > before);
    }

    /// 上游五个方法名 + AIGX 三个旧别名都必须被路由到具体操作
    /// （否则会落进 `None` → `forbidden("未知的受控任务操作")`）。
    #[test]
    fn upstream_and_legacy_method_names_route() {
        for method in [
            "managed_task.create",
            "managed_task.observe",
            "managed_task.read",
            "managed_task.status",
            "managed_task.close",
        ] {
            assert!(route(method).is_some(), "上游方法 {method} 必须能分派");
        }
        assert_eq!(route("managed_task.create"), Some(Op::Create));
        assert_eq!(route("managed_task.begin"), Some(Op::Create));
        assert_eq!(route("managed_task.observe"), Some(Op::Observe));
        assert_eq!(route("managed_task.read"), Some(Op::Read));
        assert_eq!(route("managed_task.status"), Some(Op::Status));
        assert_eq!(
            route("managed_task.close"),
            Some(Op::Close { abort: false })
        );
        assert_eq!(route("managed_task.end"), Some(Op::Close { abort: false }));
        assert_eq!(route("managed_task.abort"), Some(Op::Close { abort: true }));
        assert_eq!(route("managed_task.teleport"), None);
    }

    #[test]
    fn task_id_accepts_upstream_and_legacy_param_names() {
        assert_eq!(task_id_param(&json!({ "id": "mt-abc" })), Some("mt-abc"));
        assert_eq!(
            task_id_param(&json!({ "taskId": "mt-abc" })),
            Some("mt-abc")
        );
        assert_eq!(task_id_param(&json!({})), None);
        assert_eq!(task_id_param(&json!({ "id": "" })), None);
        assert_eq!(task_id_param(&json!({ "id": 7 })), None);
    }

    #[test]
    fn status_json_reflects_approval() {
        let _guard = serial();
        reset();
        let pending = status_json("p4");
        assert_eq!(pending["approved"], json!(false));
        assert_eq!(pending["activeTasks"], json!(0));
        approve("p4");
        let approved = status_json("p4");
        assert_eq!(approved["approved"], json!(true));
        assert_eq!(approved["supported"], json!(true));
        reset();
    }

    #[test]
    fn forget_provider_drops_only_its_tasks() {
        let _guard = serial();
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
