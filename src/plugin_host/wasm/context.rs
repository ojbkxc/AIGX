// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 单次 Wasm 调用的上下文 —— 账号、会话、代理、资源、锁、流出口。
//!
//! 与上游结构一致，差异只在「宿主能力的取得方式」：上游直接调
//! `callbacks` / `script::*`，这里走 [`WasmOptions`] 注入的三个 trait。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{OwnedMutexGuard, Semaphore};

use super::access::{WasmAccountAccess, WasmEgress, WasmStreamSink};
use super::manifest::WasmManifest;

/// 一个 Wasm 包跨调用共享的状态（= 上游 `context::Shared`）。
pub struct Shared {
    pub manifest: WasmManifest,
    pub accounts: Arc<dyn WasmAccountAccess>,
    pub egress: Arc<dyn WasmEgress>,
    pub(crate) cancel: super::Cancel,
    pub slots: Arc<Semaphore>,
    pub control_slots: Arc<Semaphore>,
    /// `concurrency_hard_limit` 声明的**对话/图片调用**额外执行硬上限
    /// （对齐上游 `Shared::execution_slots`，context.rs:32）。与 [`Self::slots`]
    /// 同时获取，因此对话/图片调用的实际并发上限 = min(声明并发, 本信号量容量)。
    /// `None` = 未声明，不加额外限制。
    pub hard_limit_slots: Option<Arc<Semaphore>>,
    pub locks: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    pub sessions: Mutex<HashMap<String, (i64, Value)>>,
    pub(crate) login_state: Mutex<super::state::Transient>,
    pub(crate) interactions: super::login::Interactions,
    pub login_proxies: Mutex<HashMap<String, (i64, Value)>>,
}

/// 由 `wasm.json` 的 `concurrency_hard_limit` 生成对话/图片调用的执行硬上限。
///
/// 对齐上游 `Shared::new` 的 `execution_slots`（context.rs:32）：
/// `manifest.concurrency_hard_limit.map(|limit| Arc::new(Semaphore::new(limit)))`。
/// `None` 表示不额外限流，此时并发只由 [`Shared::slots`]（声明并发）决定。
pub(crate) fn execution_hard_limit(manifest: &WasmManifest) -> Option<Arc<Semaphore>> {
    manifest
        .concurrency_hard_limit
        .map(|limit| Arc::new(Semaphore::new(limit)))
}

impl Shared {
    pub fn new(
        manifest: WasmManifest,
        accounts: Arc<dyn WasmAccountAccess>,
        egress: Arc<dyn WasmEgress>,
    ) -> Arc<Self> {
        Arc::new(Self {
            slots: Arc::new(Semaphore::new(manifest.concurrency)),
            control_slots: Arc::new(Semaphore::new(2)),
            hard_limit_slots: execution_hard_limit(&manifest),
            manifest,
            accounts,
            egress,
            cancel: super::Cancel::new(),
            locks: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            login_state: Mutex::new(Default::default()),
            interactions: Default::default(),
            login_proxies: Mutex::new(HashMap::new()),
        })
    }

    /// 取（或新建）账号级互斥锁。
    pub fn account_lock(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        match locks.get(key).and_then(std::sync::Weak::upgrade) {
            Some(lock) => lock,
            None => {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key.to_string(), Arc::downgrade(&lock));
                lock
            }
        }
    }
}

/// 流式出口（= 上游 `context::Sink`）。
#[derive(Clone)]
pub(crate) struct Sink {
    pub out: Arc<dyn WasmStreamSink>,
    pub credit: Arc<Semaphore>,
    pub cancel: super::Cancel,
}

/// 单次调用的上下文（= 上游 `context::Context`）。
pub struct Context {
    pub shared: Arc<Shared>,
    /// 本次调用（含其所有宿主能力）的最晚截止时刻。beta.4 用它约束
    /// `lock.acquire` 的等待上界（`call_deadline.min(now+10s)`）。
    pub(super) call_deadline: tokio::time::Instant,
    pub account: String,
    pub identity: String,
    pub flow: String,
    pub session: String,
    pub proxy: Value,
    pub proxy_selected: bool,
    pub user_agent: Option<String>,
    /// 客户端原本是否要求流式（`request.context` 的唯一取值来源）。
    ///
    /// `None` = 本次调用不是对话请求（模型目录、签到、登录等），插件问
    /// `request.context` 时应拿到 `forbidden` 而不是编一个默认值。
    pub client_stream: Option<bool>,
    pub trace: Value,
    pub sink: Option<Sink>,
    pub head_sent: bool,
    pub resources: HashMap<String, &'static str>,
    pub blobs: super::resources::Resources,
    pub guards: HashMap<String, OwnedMutexGuard<()>>,
    pub calls: u32,
    /// 本次调用打开的协议转换器（`protocol.open` 登记、`push` 归还、
    /// `close`/Drop 摘除）。句柄**只属于当前调用**，与上游一致。
    pub(super) converters: HashMap<String, super::protocol::Conversion>,
}

impl Context {
    pub fn new(
        shared: Arc<Shared>,
        params: &Value,
        sink: Option<Sink>,
        call_deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            shared,
            call_deadline,
            account: params["account"].as_str().unwrap_or("").to_string(),
            identity: String::new(),
            flow: params["flow"].as_str().unwrap_or("").to_string(),
            session: params["session"].as_str().unwrap_or("").to_string(),
            proxy: params.get("proxy").cloned().unwrap_or(Value::Null),
            proxy_selected: params.get("proxy").is_some(),
            user_agent: None,
            client_stream: params.get("client_stream").and_then(Value::as_bool),
            trace: params.get("plugin_trace").cloned().unwrap_or(Value::Null),
            sink,
            head_sent: false,
            resources: HashMap::new(),
            blobs: Default::default(),
            guards: HashMap::new(),
            calls: 0,
            converters: HashMap::new(),
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        // 未显式关闭的出站连接在调用结束时统一关闭（与上游同处置）。
        // 只在 tokio 运行时内 spawn；drop 发生在非 async 上下文时直接跳过
        // （宿主侧的会话 reaper 会兜底回收）。
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        for (id, kind) in &self.resources {
            let params = serde_json::json!({ "id": id });
            match *kind {
                "http" => {
                    let egress = self.shared.egress.clone();
                    let session = id.clone();
                    handle.spawn(async move {
                        let _ = egress.close(&session).await;
                    });
                }
                "ws" => {
                    let egress = self.shared.egress.clone();
                    handle.spawn(async move {
                        let _ = egress.ws_close(params).await;
                    });
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(hard: Option<usize>) -> WasmManifest {
        WasmManifest {
            provider: "demo".into(),
            abi: super::super::manifest::ABI,
            requires: vec![],
            origins: vec![],
            helpers: vec![],
            memory_mib: super::super::manifest::DEFAULT_MEMORY_MIB,
            concurrency: 16,
            verified: false,
            account_origin: None,
            desktop_launch: None,
            managed_login: None,
            managed_task: None,
            concurrency_hard_limit: hard,
        }
    }

    #[test]
    fn absent_hard_limit_adds_no_semaphore() {
        assert!(execution_hard_limit(&manifest(None)).is_none());
    }

    #[test]
    fn hard_limit_semaphore_clamps_conversation_concurrency() {
        // 声明并发 16，硬上限 2：额外信号量只有 2 个许可，第三个必须等，
        // 因此对话/图片调用实际并发被钳到 min(16, 2) = 2。
        assert_eq!(manifest(Some(2)).concurrency, 16);
        let slots = execution_hard_limit(&manifest(Some(2))).unwrap();
        assert_eq!(slots.available_permits(), 2);
        let first = slots.clone().try_acquire_owned().unwrap();
        let _second = slots.clone().try_acquire_owned().unwrap();
        assert!(slots.clone().try_acquire_owned().is_err());
        drop(first);
        assert!(slots.clone().try_acquire_owned().is_ok());
    }
}
