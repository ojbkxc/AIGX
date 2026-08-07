//! 健康检查与优雅关闭 — 借鉴 aisix-proxy/src/health.rs
//!
//! 提供 Kubernetes 标准的 /livez 和 /readyz 端点：
//! - `/livez` — 进程存活检查（ping + shutdown 状态）
//! - `/readyz` — 就绪检查（shutdown 状态 + 配置可用性）
//!
//! 状态机：
//! ```text
//!  Healthy (0) ──[4+ failures]──► Degraded (1) ──[8+ failures]──► Down (2)
//!     ▲                               │                               │
//!     └─────────[any success]─────────┴───────────────────────────────┘
//! ```

use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use axum::http::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

static X_CONTENT_TYPE_OPTIONS: HeaderName = HeaderName::from_static("x-content-type-options");
static NOSNIFF: HeaderValue = HeaderValue::from_static("nosniff");
static TEXT_PLAIN_UTF8: HeaderValue = HeaderValue::from_static("text/plain; charset=utf-8");

/// 进程存活状态
#[derive(Debug, Default)]
pub struct LivezState {
    shutting_down: AtomicBool,
}

impl LivezState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 标记正在关闭（优雅关闭信号）
    pub fn mark_shutting_down(&self) {
        self.shutting_down.store(true, Ordering::Relaxed);
    }

    fn shutdown_check(&self) -> Result<(), &'static str> {
        if self.shutting_down.load(Ordering::Relaxed) {
            Err("process is shutting down")
        } else {
            Ok(())
        }
    }

    /// 是否已收到优雅关闭信号
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Relaxed)
    }
}

/// 响应 /livez 请求
pub fn livez_response(livez: &LivezState, verbose: bool) -> Response {
    let mut body = String::new();
    let mut failed = false;

    body.push_str("[+]ping ok\n");
    match livez.shutdown_check() {
        Ok(()) => body.push_str("[+]shutdown ok\n"),
        Err(_) => {
            failed = true;
            body.push_str("[-]shutdown failed: draining\n");
        }
    }

    let headers = [
        (CONTENT_TYPE, TEXT_PLAIN_UTF8.clone()),
        (X_CONTENT_TYPE_OPTIONS.clone(), NOSNIFF.clone()),
    ];

    if failed {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            headers,
            format!("{body}livez check failed"),
        )
            .into_response();
    }

    if !verbose {
        return (StatusCode::OK, headers, "ok").into_response();
    }

    (
        StatusCode::OK,
        headers,
        format!("{body}livez check passed\n"),
    )
        .into_response()
}

/// 响应 /readyz 请求
pub fn readyz_response(
    livez: &LivezState,
    config_ready: bool,
    verbose: bool,
) -> Response {
    let mut body = String::new();
    let mut failed = false;

    match livez.shutdown_check() {
        Ok(()) => body.push_str("[+]shutdown ok\n"),
        Err(_) => {
            failed = true;
            body.push_str("[-]shutdown failed: draining\n");
        }
    }

    if config_ready {
        body.push_str("[+]config ok\n");
    } else {
        failed = true;
        body.push_str("[-]config failed: not ready\n");
    }

    let headers = [
        (CONTENT_TYPE, TEXT_PLAIN_UTF8.clone()),
        (X_CONTENT_TYPE_OPTIONS.clone(), NOSNIFF.clone()),
    ];

    if failed {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            headers,
            format!("{body}readyz check failed"),
        )
            .into_response();
    }

    if !verbose {
        return (StatusCode::OK, headers, "ok").into_response();
    }

    (
        StatusCode::OK,
        headers,
        format!("{body}readyz check passed\n"),
    )
        .into_response()
}

/// 数值健康等级
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(into = "u8")]
pub enum HealthLevel {
    Healthy,
    Degraded,
    Down,
}

impl From<HealthLevel> for u8 {
    fn from(h: HealthLevel) -> u8 {
        match h {
            HealthLevel::Healthy => 0,
            HealthLevel::Degraded => 1,
            HealthLevel::Down => 2,
        }
    }
}

/// 连续失败阈值
const DEGRADED_THRESHOLD: u32 = 4;
const DOWN_THRESHOLD: u32 = 8;

#[derive(Debug)]
struct Entry {
    consecutive_failures: AtomicU32,
}

impl Default for Entry {
    fn default() -> Self {
        Self {
            consecutive_failures: AtomicU32::new(0),
        }
    }
}

impl Entry {
    fn level(&self) -> HealthLevel {
        let n = self.consecutive_failures.load(Ordering::Relaxed);
        if n >= DOWN_THRESHOLD {
            HealthLevel::Down
        } else if n >= DEGRADED_THRESHOLD {
            HealthLevel::Degraded
        } else {
            HealthLevel::Healthy
        }
    }

    fn on_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn on_failure(&self) {
        let prev = self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        if prev > DOWN_THRESHOLD {
            self.consecutive_failures
                .store(DOWN_THRESHOLD + 1, Ordering::Relaxed);
        }
    }
}

/// 模型健康追踪器，共享于所有请求处理
#[derive(Debug, Default)]
pub struct HealthTracker {
    entries: DashMap<String, Entry>,
}

impl HealthTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次成功
    pub fn on_success(&self, model: &str) {
        if let Some(entry) = self.entries.get(model) {
            entry.on_success();
        }
    }

    /// 记录一次失败
    pub fn on_failure(&self, model: &str) {
        let entry = self
            .entries
            .entry(model.to_string())
            .or_insert_with(Entry::default);
        entry.on_failure();
    }

    /// 查询模型健康状态
    pub fn health(&self, model: &str) -> HealthLevel {
        self.entries
            .get(model)
            .map(|e| e.level())
            .unwrap_or(HealthLevel::Healthy)
    }

    /// 获取所有模型的健康状态
    pub fn all_health(&self) -> Vec<(String, HealthLevel)> {
        self.entries
            .iter()
            .map(|e| (e.key().clone(), e.value().level()))
            .collect()
    }
}

/// 判断配置可用性是否阻塞就绪检查。
/// `last_apply_age` 是自上次配置应用以来的时间；`None` 表示尚未应用。
pub fn config_readiness_block(last_apply_age: Option<Duration>) -> Option<&'static str> {
    match last_apply_age {
        None => Some("config not yet applied"),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_tracker_transitions() {
        let tracker = HealthTracker::new();
        assert_eq!(tracker.health("model1"), HealthLevel::Healthy);

        // 3 次失败仍然是 Healthy
        for _ in 0..3 {
            tracker.on_failure("model1");
        }
        assert_eq!(tracker.health("model1"), HealthLevel::Healthy);

        // 第 4 次变为 Degraded
        tracker.on_failure("model1");
        assert_eq!(tracker.health("model1"), HealthLevel::Degraded);

        // 第 8 次变为 Down
        for _ in 0..4 {
            tracker.on_failure("model1");
        }
        assert_eq!(tracker.health("model1"), HealthLevel::Down);

        // 一次成功恢复
        tracker.on_success("model1");
        assert_eq!(tracker.health("model1"), HealthLevel::Healthy);
    }

    #[test]
    fn livez_state_shutdown() {
        let state = LivezState::new();
        assert!(!state.is_shutting_down());
        assert!(state.shutdown_check().is_ok());

        state.mark_shutting_down();
        assert!(state.is_shutting_down());
        assert!(state.shutdown_check().is_err());
    }
}