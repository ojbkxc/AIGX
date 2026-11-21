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
    pub locks: Mutex<HashMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    pub sessions: Mutex<HashMap<String, (i64, Value)>>,
    pub(crate) login_state: Mutex<super::state::Transient>,
    pub(crate) interactions: super::login::Interactions,
    pub login_proxies: Mutex<HashMap<String, (i64, Value)>>,
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
    pub trace: Value,
    pub sink: Option<Sink>,
    pub head_sent: bool,
    pub resources: HashMap<String, &'static str>,
    pub blobs: super::resources::Resources,
    pub guards: HashMap<String, OwnedMutexGuard<()>>,
    pub calls: u32,
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
            trace: params.get("plugin_trace").cloned().unwrap_or(Value::Null),
            sink,
            head_sent: false,
            resources: HashMap::new(),
            blobs: Default::default(),
            guards: HashMap::new(),
            calls: 0,
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
