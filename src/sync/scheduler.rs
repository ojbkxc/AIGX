//! 云同步调度循环。
//!
//! ── 为什么单独一个循环，不注册进 `scheduled_tasks::TASKS` ────────
//! 六条间隔型任务的开关 / 间隔在 `AppConfig.scheduled_tasks` 里；云同步的开关 /
//! 间隔在 [`super::config::SyncConfig`]（KV）里——它还要带凭据与远端路径，放进
//! TOML 配置既多一处真源，也会把凭据散到两个落点。因此这里自带一个常驻循环，
//! 但**排期机制完全复用** `scheduled_tasks::task_state`：持久化的
//! 上次尝试 / 下次执行 / 失败冷却 / 在途租约，跨重启保留，且与六条任务共享
//! 同一套「崩溃后租约自动过期」的兜底。任务键 [`TASK_KEY`]。
//!
//! ── 轮询判定，不是睡满间隔 ──────────────────────────────────
//! 每 [`TICK_MS`] 醒一次，读一次最新配置与排期：
//!   · 用户改完设置最多慢一个 tick 生效；
//!   · 机器休眠 / 改时钟后自愈（醒来比一次墙上时钟，错过的时点只补一次）；
//!   · 配置里 `enabled=false` 时整轮跳过，不触碰任何网络。
//!
//! ── 间隔从哪来 ──────────────────────────────────────────────
//! `SyncConfig.interval_secs` 是唯一真源。循环在内存里记一份「上次使用的间隔」，
//! 发现配置改了就 `reschedule` 一次；`min_interval_secs` 通过 `claim` 的
//! `min_gap_ms` 传入——**手动「立即同步」也不越过它**（与 A2A 的语义一致：
//! 最短请求间隔是保护远端的硬约束，不是排期偏好）。

use std::time::Duration;

use crate::api::openai::AppState;
use crate::scheduled_tasks::task_state::{self, Claim, ManualBackoff};

use super::{config, engine};

/// 调度循环的任务键（`task_state` 里区分各任务的 id）。
pub const TASK_KEY: &str = "cloudSync";

/// 判定间隔：10 秒。
const TICK_MS: u64 = 10_000;

/// 启动云同步调度循环（进程内只应调用一次；`main` 在 `scheduled_tasks::spawn` 后调用）。
pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        let mut applied_interval_ms: i64 = 0;
        loop {
            let sync_config = config::load(&state.scheduled_task_store);
            if sync_config.enabled {
                let interval_ms = sync_config.interval_secs.max(1).saturating_mul(1000);
                if applied_interval_ms != interval_ms {
                    if let Err(error) = task_state::reschedule(TASK_KEY, interval_ms) {
                        tracing::warn!("[CloudSync] 同步排期初始化失败：{error}");
                    } else {
                        applied_interval_ms = interval_ms;
                    }
                }
                let task_state = task_state::read(TASK_KEY).unwrap_or_default();
                let now = super::now_ms();
                if !task_state.running() && now >= task_state.due_at() {
                    run_once(&state, &sync_config, interval_ms).await;
                }
            } else {
                // 关闭期间不做任何事；重新开启后下一拍按「现在」重新排期
                applied_interval_ms = 0;
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    });
}

/// 跑一轮自动同步。`claim` 的租约防并发（手动「立即同步」与循环撞上时排队）。
async fn run_once(state: &AppState, sync_config: &config::SyncConfig, interval_ms: i64) {
    let min_gap_ms = sync_config.min_interval_secs.saturating_mul(1000);
    let guard = match task_state::claim(
        TASK_KEY,
        interval_ms,
        false,
        ManualBackoff::Respect,
        min_gap_ms,
    ) {
        Ok(Claim::Acquired(guard)) => guard,
        // 冷却中 / 已在执行：这一拍不跑（下一拍重新判定）
        Ok(Claim::Deferred(_)) => return,
        Err(error) => {
            tracing::warn!("[CloudSync] 调度占位失败：{error}");
            return;
        }
    };

    let (success, summary) = match engine::upload(state, false, false).await {
        Ok(value) => {
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let summary = match status {
                "uploaded" => format!(
                    "已上传 {} 字节（{} 键）",
                    value.get("bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    value.get("entries").and_then(|v| v.as_u64()).unwrap_or(0)
                ),
                "skipped" => value
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("已跳过")
                    .to_string(),
                other => other.to_string(),
            };
            (true, summary)
        }
        Err(error) => (false, error.message),
    };
    if let Err(error) = guard.finish(success, summary.clone(), None, 0, interval_ms) {
        tracing::warn!("[CloudSync] 调度状态落库失败：{error}");
    }
    if success {
        tracing::debug!("[CloudSync] 自动同步：{summary}");
    } else {
        tracing::warn!("[CloudSync] 自动同步失败：{summary}");
    }
}
