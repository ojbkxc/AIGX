//! 健康检查与优雅关闭 — 借鉴 aisix-proxy/src/health.rs
//!
//! 提供 Kubernetes 标准的 /livez 和 /readyz 端点：
//! - `/livez` — 进程存活检查（ping；draining 不摘存活探针，摘流归 /readyz）
//! - `/readyz` — 就绪检查（shutdown 状态 + 配置可用性 + 存储可访问性）
//!
//! 状态机：
//! ```text
//!  Healthy (0) ──[4+ failures]──► Degraded (1) ──[8+ failures]──► Down (2)
//!     ▲                               │                               │
//!     └─────────[any success]─────────┴───────────────────────────────┘
//! ```

use axum::http::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

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
///
/// K8s 语义：`/livez` 只回答「进程还活着吗」。draining 期间进程仍存活、
/// 只是不再接新流量——若把 draining 判为失败（503），livenessProbe 会
/// 触发立即 SIGKILL，drain 窗口永远跑不完。因此 draining **不**是
/// livez 的失败项；摘流（503）是 `/readyz` 的职责。verbose 模式下如实
/// 标注 draining 状态供人工排查。
pub fn livez_response(livez: &LivezState, verbose: bool) -> Response {
    let mut body = String::new();

    body.push_str("[+]ping ok\n");
    if livez.is_shutting_down() {
        body.push_str("[+]shutdown ok (draining)\n");
    } else {
        body.push_str("[+]shutdown ok\n");
    }

    let headers = [
        (CONTENT_TYPE, TEXT_PLAIN_UTF8.clone()),
        (X_CONTENT_TYPE_OPTIONS.clone(), NOSNIFF.clone()),
    ];

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
///
/// `storage_ready` 由调用方做一次轻量读探测（不存在的键也算可访问，
/// 只有读操作本身报错才视为失败）。`/readyz` 被容器探针高频调用，
/// 因此这里不追加任何重查询。
pub fn readyz_response(
    livez: &LivezState,
    config_ready: bool,
    storage_ready: bool,
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

    if storage_ready {
        body.push_str("[+]storage ok\n");
    } else {
        failed = true;
        body.push_str("[-]storage failed: read error\n");
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

    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    fn on_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
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
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn on_success(&self, model: &str) {
        if let Some(entry) = self.entries.get(model) {
            entry.on_success();
        }
    }

    /// 记录一次失败
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn on_failure(&self, model: &str) {
        let entry = self.entries.entry(model.to_string()).or_default();
        entry.on_failure();
    }

    /// 查询模型健康状态
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
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

    /// K8s 探针语义：draining 期间 /livez 必须保持 200——返回 503 会让
    /// livenessProbe 立即 SIGKILL，20s drain 窗口永远跑不完；摘流是
    /// /readyz 的职责，draining 时 /readyz 必须 503。
    #[test]
    fn livez_stays_ok_during_draining() {
        let state = LivezState::new();
        assert_eq!(livez_response(&state, false).status(), StatusCode::OK);
        assert_eq!(livez_response(&state, true).status(), StatusCode::OK);

        state.mark_shutting_down();
        // 存活探针不摘：非 verbose 与 verbose 都保持 200
        assert_eq!(livez_response(&state, false).status(), StatusCode::OK);
        assert_eq!(livez_response(&state, true).status(), StatusCode::OK);
        // 摘流探针：draining 时 /readyz 503
        assert_eq!(
            readyz_response(&state, true, true, false).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[test]
    fn readyz_reflects_storage_failure() {
        let state = LivezState::new();
        assert_eq!(
            readyz_response(&state, true, true, false).status(),
            StatusCode::OK
        );
        // 存储读失败 → 503
        assert_eq!(
            readyz_response(&state, true, false, false).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        // 配置未就绪 → 503
        assert_eq!(
            readyz_response(&state, false, true, false).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
