//! 定时系统任务框架（P1-17）—— 轻量 cron，自有实现（不引 crate）。
//!
//! 设计目标（参照 ROADMAP P1「运营自动化」）：
//! - 周期性后台任务的统一注册入口：模型元信息同步、渠道探活、
//!   告警巡检等不再各自 `tokio::spawn` + 硬编码 loop
//! - 每个任务带：名称（日志/监控标识）、周期、首次延迟、执行体
//! - 串行调度语义：同一任务上一轮未完成时跳过本轮（不堆积），
//!   单任务 panic 不影响其他任务（catch_unwind 隔离）
//!
//! 与现有后台任务的边界：quota_monitor / alert_patrol / channel prober
//! 已有自己的 spawn 协程，本框架先承接新任务（模型元信息同步、
//! 会话撤销表清扫），旧任务迁移为渐进式重构（不在本轮范围）。
//!
//! 百年工程红线：不引入 tokio-cron-scheduler / cron 等 crate；
//! 固定周期轮询的语义用 `tokio::time::interval` + missed tick 策略已足够
//!（AIGX 的定时任务是「周期巡检」而非「精确日历时刻」，无需 cron 表达式）。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 一次任务的静态描述。
pub struct TaskSpec {
    /// 任务名（日志标识，须唯一）
    pub name: &'static str,
    /// 执行周期
    pub interval: Duration,
    /// 启动后首次执行的延迟（默认等于 interval）
    pub first_run_delay: Duration,
    /// 任务执行体：返回本轮处理的条数（0 = 无事发生，用于日志降噪）
    pub run: Box<dyn Fn() -> Pin<Box<dyn Future<Output = u64> + Send>> + Send + Sync>,
}

/// 简易调度器：一个任务一个 tokio 协程，周期 sleep + 执行。
///
/// 同一任务串行（上一轮未完成时 `interval().tick()` 会等待——
/// tokio interval 默认 MissedTickBehavior::Burst，改为 Skip 避免
/// 任务卡顿后连环补跑）。
#[derive(Default)]
pub struct Scheduler {
    /// 已注册任务数（注册后自增；用于启动日志）
    registered: AtomicU64,
    /// 各任务累计执行轮数（任务名 → 轮数；监控/测试用）。
    /// Arc 包裹：DashMap 的 Clone 是深拷贝快照，spawn 进协程的必须是共享句柄，
    /// 否则外部读到的 tick_count 永远是注册时的 0。
    ticks: Arc<dashmap::DashMap<&'static str, u64>>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册并启动一个周期任务。
    pub fn spawn(&self, spec: TaskSpec) {
        self.registered.fetch_add(1, Ordering::Relaxed);
        self.ticks.insert(spec.name, 0);
        let ticks = Arc::clone(&self.ticks);
        let name = spec.name;
        let interval = spec.interval;
        let first_run = spec.first_run_delay;
        let run = Arc::new(spec.run);

        tokio::spawn(async move {
            // 首次延迟后再进入周期循环（避免启动风暴：所有任务同时首轮执行）
            tokio::time::sleep(first_run).await;
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.reset();
            loop {
                ticker.tick().await;
                // panic 隔离：任务体放到独立协程，panic 只丢本轮，不杀死调度协程。
                // (run)() 已返回 Pin<Box<dyn Future>>，可直接 spawn
                let handle = tokio::spawn((run)());
                let outcome = handle.await;
                match outcome {
                    Ok(processed) => {
                        *ticks.entry(name).or_insert(0) += 1;
                        if processed > 0 {
                            tracing::info!(task = name, processed, "scheduled task completed");
                        } else {
                            tracing::debug!(task = name, "scheduled task idle");
                        }
                    }
                    Err(e) => {
                        if e.is_panic() {
                            tracing::error!(
                                task = name,
                                "scheduled task panicked; will retry next tick"
                            );
                        } else {
                            tracing::error!(task = name, error = %e, "scheduled task cancelled");
                        }
                    }
                }
            }
        });
    }

    /// 已注册任务数。
    pub fn task_count(&self) -> u64 {
        self.registered.load(Ordering::Relaxed)
    }

    /// 某任务累计执行轮数（测试/监控用）。
    pub fn tick_count(&self, name: &str) -> u64 {
        self.ticks.get(name).map(|t| *t).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn scheduler_runs_task_periodically() {
        let scheduler = Scheduler::new();
        let counter = Arc::new(AtomicU64::new(0));
        let c2 = counter.clone();
        scheduler.spawn(TaskSpec {
            name: "test-counter",
            interval: Duration::from_secs(10),
            first_run_delay: Duration::from_secs(5),
            run: Box::new(move || {
                let c = c2.clone();
                Box::pin(async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    1
                })
            }),
        });

        // 时间推进：5s 首跑 + 3 个周期。
        // paused 模式下 spawn 的子任务由 sleep 自动让出推进：40s = 5+10+10+10
        tokio::time::sleep(Duration::from_secs(40)).await;
        // 给最后一轮任务体完成的调度机会（任务在独立协程，tick 后需再让出）
        tokio::task::yield_now().await;
        let executed = counter.load(Ordering::SeqCst);
        assert!(executed >= 3, "expected >= 3 runs, got {executed}");
        assert_eq!(scheduler.tick_count("test-counter"), executed);
    }

    #[tokio::test(start_paused = true)]
    async fn panicking_task_does_not_kill_scheduler() {
        let scheduler = Scheduler::new();
        let counter = Arc::new(AtomicU64::new(0));
        let c2 = counter.clone();
        scheduler.spawn(TaskSpec {
            name: "test-panic",
            interval: Duration::from_secs(10),
            first_run_delay: Duration::from_secs(1),
            run: Box::new(move || {
                let c = c2.clone();
                Box::pin(async move {
                    // 第一轮 panic，之后正常计数
                    if c.fetch_add(1, Ordering::SeqCst) == 0 {
                        panic!("boom");
                    }
                    1
                })
            }),
        });

        tokio::time::sleep(Duration::from_secs(35)).await;
        // 给最后一轮任务体完成的调度机会（任务在独立协程，tick 后需再让出）
        tokio::task::yield_now().await;
        let executed = counter.load(Ordering::SeqCst);
        // 第一轮 panic 后任务继续存活：35s = 1s 首跑 + 3 周期，边界取 >= 3
        assert!(
            executed >= 3,
            "expected >= 3 runs after panic, got {executed}"
        );
        assert!(executed > 1, "scheduler died after first panic");
        assert_eq!(scheduler.tick_count("test-panic"), executed - 1);
    }
}
