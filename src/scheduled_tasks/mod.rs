//! 间隔型定时任务注册表与调度循环（移植自 Agent2API `core/scheduled_tasks.rs`）。
//!
//! ── 两类任务（区别是**谁来执行**，不是可配性）─────────────────
//!   - `Runner::Backend`：凭证自动维护、模型目录刷新、软件版本检查。
//!     后端循环执行，因此有「上次执行 / 下次执行 / 立即执行」这些运行状态。
//!   - `Runner::Frontend`：日志页 / 请求日志页 / 报表页的自动刷新。定时器天然长在
//!     页面上（只在页面可见时该走），后端只存开关与间隔，界面自己读。
//!
//! ── 本模块只做「何时做」，不做事 ─────────────────────────────
//! 每条任务的动作在各自模块里（凭证维护走 `account_session::refresh`、模型目录
//! 走 `model::models_dev`、版本检查走 `update::UpdateManager`），本文件负责把它们
//! 按配置的开关与间隔串起来 —— 与 Agent2API 同一分工。
//!
//! ── 排期为什么不在这个模块里 ─────────────────────────────────
//! 「上次尝试 / 上次成功 / 下次执行 / 失败冷却 / 在途占位」全部委托给
//! `scheduled_tasks::task_state`（落 KV、跨重启、跨实例）。本模块的循环因此每次
//! tick 都从**持久化状态**重算「到点了吗」，而不是在内存里记 `nextRunAt`。
//!
//! ── 调度方式：轮询判定，不是「睡满间隔」──────────────────────
//! 循环每 `TICK_MS` 醒一次，每次重新判定。两个后果都是想要的：
//!   - **改完设置下一轮就生效**（最多慢一个 tick）；
//!   - **机器休眠 / 锁屏 / 改时钟后自愈**：醒来的第一次 tick 比一下墙上时钟就把
//!     错过的时点补上（只补一次，不补跑期间的每一轮）。

use std::time::Duration;

use serde_json::{json, Value};

use crate::api::openai::AppState;

pub mod task_state;
pub mod types;

use self::task_state::TaskState;
use self::types::{
    IntervalTask, IntervalTaskPatch, ScheduledSettings, DEFAULT_CREDENTIAL_MAINTENANCE_MINUTES,
    DEFAULT_LOGS_AUTO_REFRESH_SECONDS, DEFAULT_MODEL_REFRESH_MINUTES,
    DEFAULT_REPORT_AUTO_REFRESH_SECONDS, DEFAULT_REQUESTS_AUTO_REFRESH_SECONDS,
    DEFAULT_UPDATE_CHECK_MINUTES, INTERVAL_MAX_MINUTES, INTERVAL_MAX_SECONDS, INTERVAL_MIN_MINUTES,
    INTERVAL_MIN_SECONDS, KEY_CREDENTIAL_MAINTENANCE, KEY_LOGS_AUTO_REFRESH, KEY_MODEL_REFRESH,
    KEY_REPORT_AUTO_REFRESH, KEY_REQUESTS_AUTO_REFRESH, KEY_UPDATE_CHECK,
};

/// 调度循环的判定间隔。
///
/// 10 秒：backend 任务的间隔下限是 1 分钟（见 `INTERVAL_MIN_MINUTES`），
/// 10 秒的判定粒度意味着「到点后最多晚 10 秒执行」——用户感知不到，
/// 而空转代价只是每 10 秒一次读 KV。不做得更密：没有收益。
pub const TICK_MS: u64 = 10_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Runner {
    Backend,
    Frontend,
}

pub struct TaskDef {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub unit: &'static str,
    pub runner: Runner,
    pub min: i64,
    pub max: i64,
    pub default_interval: i64,
}

pub const TASK_CREDENTIAL_MAINTENANCE: &str = KEY_CREDENTIAL_MAINTENANCE;
pub const TASK_MODEL_REFRESH: &str = KEY_MODEL_REFRESH;
pub const TASK_UPDATE_CHECK: &str = KEY_UPDATE_CHECK;
pub const TASK_LOGS_AUTO_REFRESH: &str = KEY_LOGS_AUTO_REFRESH;
pub const TASK_REQUESTS_AUTO_REFRESH: &str = KEY_REQUESTS_AUTO_REFRESH;
pub const TASK_REPORT_AUTO_REFRESH: &str = KEY_REPORT_AUTO_REFRESH;

pub const TASKS: [TaskDef; 6] = [
    TaskDef {
        id: TASK_CREDENTIAL_MAINTENANCE,
        label: "凭证自动维护",
        description: "定期遍历账号，只刷新已过期或临期的凭证。重启后沿用上次排期，失败账号在冷却后重试；转发需要的按需续期不受此开关影响。",
        unit: "minutes",
        runner: Runner::Backend,
        min: INTERVAL_MIN_MINUTES,
        max: INTERVAL_MAX_MINUTES,
        default_interval: DEFAULT_CREDENTIAL_MAINTENANCE_MINUTES,
    },
    TaskDef {
        id: TASK_MODEL_REFRESH,
        label: "模型目录刷新",
        description: "定期联网拉取 models.dev 模型元数据（能力位与思考档位）。开关与间隔在此页配置；可在系统设置单独关闭 models.dev 补齐。失败保留上一版目录。",
        unit: "minutes",
        runner: Runner::Backend,
        min: INTERVAL_MIN_MINUTES,
        max: INTERVAL_MAX_MINUTES,
        default_interval: DEFAULT_MODEL_REFRESH_MINUTES,
    },
    TaskDef {
        id: TASK_UPDATE_CHECK,
        label: "软件版本检查",
        description: "定期向 GitHub 查询最新发布版本，默认每 20 分钟一次。开启自动应用时，发现新版本会下载替换并自重启。开关与间隔在此页配置。",
        unit: "minutes",
        runner: Runner::Backend,
        min: INTERVAL_MIN_MINUTES,
        max: INTERVAL_MAX_MINUTES,
        default_interval: DEFAULT_UPDATE_CHECK_MINUTES,
    },
    TaskDef {
        id: TASK_LOGS_AUTO_REFRESH,
        label: "日志页自动刷新",
        description: "停留在「用量日志」页时按此间隔重新拉取系统事件；页面不可见时不请求。",
        unit: "seconds",
        runner: Runner::Frontend,
        min: INTERVAL_MIN_SECONDS,
        max: INTERVAL_MAX_SECONDS,
        default_interval: DEFAULT_LOGS_AUTO_REFRESH_SECONDS,
    },
    TaskDef {
        id: TASK_REQUESTS_AUTO_REFRESH,
        label: "请求日志页自动刷新",
        description: "停留在「用量日志」页时按此间隔重新拉取请求日志；页面不可见时不请求。",
        unit: "seconds",
        runner: Runner::Frontend,
        min: INTERVAL_MIN_SECONDS,
        max: INTERVAL_MAX_SECONDS,
        default_interval: DEFAULT_REQUESTS_AUTO_REFRESH_SECONDS,
    },
    TaskDef {
        id: TASK_REPORT_AUTO_REFRESH,
        label: "报表自动刷新",
        description: "停留在「仪表盘」页时按此间隔重新拉取统计数据；页面不可见时不请求。",
        unit: "seconds",
        runner: Runner::Frontend,
        min: INTERVAL_MIN_SECONDS,
        max: INTERVAL_MAX_SECONDS,
        default_interval: DEFAULT_REPORT_AUTO_REFRESH_SECONDS,
    },
];

pub fn find(id: &str) -> Option<&'static TaskDef> {
    TASKS.iter().find(|task| task.id == id)
}

fn settings_of(settings: &ScheduledSettings, id: &str) -> IntervalTask {
    match id {
        TASK_CREDENTIAL_MAINTENANCE => settings.credential_maintenance,
        TASK_MODEL_REFRESH => settings.model_refresh,
        TASK_UPDATE_CHECK => settings.update_check,
        TASK_LOGS_AUTO_REFRESH => settings.logs_auto_refresh,
        TASK_REQUESTS_AUTO_REFRESH => settings.requests_auto_refresh,
        TASK_REPORT_AUTO_REFRESH => settings.report_auto_refresh,
        _ => IntervalTask {
            enabled: false,
            interval: 0,
        },
    }
}

fn interval_ms(task: &TaskDef, settings: &ScheduledSettings) -> i64 {
    let interval = settings_of(settings, task.id).interval.max(1);
    interval * if task.unit == "seconds" { 1000 } else { 60_000 }
}

fn state_key(id: &str) -> String {
    id.to_string()
}

fn task_json(
    task: &TaskDef,
    settings: &ScheduledSettings,
    state: Result<TaskState, String>,
) -> Value {
    let it = settings_of(settings, task.id);
    let backend = task.runner == Runner::Backend;
    let error = state.as_ref().err().cloned();
    let state = state.unwrap_or_default();
    let timestamp = |at: i64| if at > 0 { json!(at) } else { Value::Null };
    json!({
        "id": task.id,
        "label": task.label,
        "description": task.description,
        "unit": task.unit,
        "runner": if backend { "backend" } else { "frontend" },
        "enabled": it.enabled,
        "interval": it.interval,
        "min": task.min,
        "max": task.max,
        "defaultInterval": task.default_interval,
        "running": backend && state.running(),
        "lastRunAt": timestamp(state.last_run_at),
        "lastAttemptAt": timestamp(state.last_attempt_at),
        "lastSuccessAt": timestamp(state.last_success_at),
        "lastResult": error.or_else(|| state.last_result.clone()),
        "lastError": state.last_error,
        "retryAt": timestamp(state.retry_at),
        "nextRunAt": if backend && it.enabled {
            timestamp(state.due_at().max(chrono::Utc::now().timestamp_millis()))
        } else {
            Value::Null
        },
        "canRun": backend,
    })
}

pub async fn list(state: &AppState) -> Value {
    // 一次读出全部状态：本接口被「定时任务」页每 20 秒轮询一次，
    // 逐条任务各读一次 KV 没有意义。
    let settings = state.config_manager.get().await.scheduled_tasks;
    let states = task_state::read_all();
    json!({
        "tasks": TASKS
            .iter()
            .map(|task| {
                let state = match &states {
                    Ok(states) if task.runner == Runner::Backend =>
                        Ok(states.get(&state_key(task.id)).cloned().unwrap_or_default()),
                    Ok(_) => Ok(TaskState::default()),
                    Err(error) => Err(error.clone()),
                };
                task_json(task, &settings, state)
            })
            .collect::<Vec<_>>(),
    })
}

pub async fn task_by_id(state: &AppState, id: &str) -> Value {
    let task = match find(id) {
        Some(task) => task,
        None => return Value::Null,
    };
    let settings = state.config_manager.get().await.scheduled_tasks;
    let state_state = if task.runner == Runner::Backend {
        task_state::read(&state_key(task.id))
    } else {
        Ok(TaskState::default())
    };
    task_json(task, &settings, state_state)
}

/// 把 patch 落到 `settings` 的对应字段（按 id 选中那一条）。
fn apply_patch(settings: &mut ScheduledSettings, id: &str, patch: IntervalTaskPatch) {
    let target = match id {
        TASK_CREDENTIAL_MAINTENANCE => &mut settings.credential_maintenance,
        TASK_MODEL_REFRESH => &mut settings.model_refresh,
        TASK_UPDATE_CHECK => &mut settings.update_check,
        TASK_LOGS_AUTO_REFRESH => &mut settings.logs_auto_refresh,
        TASK_REQUESTS_AUTO_REFRESH => &mut settings.requests_auto_refresh,
        TASK_REPORT_AUTO_REFRESH => &mut settings.report_auto_refresh,
        _ => return,
    };
    if let Some(enabled) = patch.enabled {
        target.enabled = enabled;
    }
    if let Some(interval) = patch.interval {
        target.interval = interval;
    }
}

pub async fn configure(
    state: &AppState,
    id: &str,
    patch: IntervalTaskPatch,
) -> Result<Value, String> {
    let task = find(id).ok_or_else(|| format!("未知的定时任务: {id}"))?;
    if patch.enabled.is_none() && patch.interval.is_none() {
        return Err("没有需要更新的字段".to_string());
    }
    if let Some(interval) = patch.interval {
        if !(task.min..=task.max).contains(&interval) {
            let unit = if task.unit == "seconds" {
                "秒"
            } else {
                "分钟"
            };
            return Err(format!(
                "「{}」的间隔必须是 {}–{} {}",
                task.label, task.min, task.max, unit
            ));
        }
    }
    let mut config = state.config_manager.get().await;
    let before = settings_of(&config.scheduled_tasks, id);
    apply_patch(&mut config.scheduled_tasks, id, patch);
    let after = settings_of(&config.scheduled_tasks, id);
    // 新间隔按 patch 后的设置算——config 马上要被 update 移走，先取出来。
    let new_interval_ms = interval_ms(task, &config.scheduled_tasks);
    state
        .config_manager
        .update(config)
        .await
        .map_err(|error| format!("保存配置失败: {error}"))?;
    if task.runner == Runner::Backend {
        if !before.enabled && after.enabled {
            // 刚开启：保留「开启即跑一次」的语义（下一个 tick 就执行），
            // 而不是按上次尝试时间算出的旧排期干等。失败冷却仍然优先。
            task_state::schedule_now(&state_key(id))?;
        } else if before.interval != after.interval {
            // 改间隔：按**新**间隔从上次尝试起重新计时（要马上跑有「立即执行」按钮）
            task_state::reschedule(&state_key(id), new_interval_ms)?;
        }
    }
    if before != after {
        tracing::info!(
            "[Tasks] 定时任务「{}」已{}，间隔 {} {}",
            task.label,
            if after.enabled { "开启" } else { "关闭" },
            after.interval,
            if task.unit == "seconds" {
                "秒"
            } else {
                "分钟"
            }
        );
    }
    Ok(task_by_id(state, id).await)
}

pub async fn run_now(state: &AppState, id: &str) -> Result<String, String> {
    let task = find(id).ok_or_else(|| format!("未知的定时任务: {id}"))?;
    if task.runner != Runner::Backend {
        return Err(format!(
            "「{}」由界面自己刷新，无法在后端立即执行",
            task.label
        ));
    }
    run_backend(state, task, true).await
}

async fn run_backend(state: &AppState, task: &TaskDef, manual: bool) -> Result<String, String> {
    let settings = state.config_manager.get().await.scheduled_tasks;

    // 更新开关关闭时不占位直接跳过：占位是为了防并发与排期推进，开关关闭本就不该跑。
    if task.id == TASK_UPDATE_CHECK && !state.config_manager.get().await.update.enabled {
        return Ok("软件更新开关已关闭，跳过".to_string());
    }

    // 手动执行越不越过失败冷却：按**冷却记的是什么**分两类。
    let backoff = match task.id {
        TASK_MODEL_REFRESH | TASK_CREDENTIAL_MAINTENANCE => task_state::ManualBackoff::Bypass,
        _ => task_state::ManualBackoff::Respect,
    };
    let interval = interval_ms(task, &settings);
    let guard = match task_state::claim(task.id, interval, manual, backoff, 1_000)? {
        task_state::Claim::Acquired(guard) => guard,
        task_state::Claim::Deferred(state_state) => return Err(state_state.waiting_message()),
    };
    let (summary, success) = match task.id {
        TASK_UPDATE_CHECK => {
            let config = state.config_manager.get().await;
            // check 内部有冷却保护；失败记退避（连续失败按间隔指数退避，避免打爆限额）。
            match state.update.check(&config.update).await {
                Ok(info) => {
                    if config.update.auto_apply
                        && info.get("hasUpdate").and_then(Value::as_bool) == Some(true)
                    {
                        if let Err(e) = state.update.start_apply(&config.update, false) {
                            tracing::warn!("[Update] 自动应用失败：{e}");
                        }
                    }
                    if info.get("hasUpdate").and_then(Value::as_bool) == Some(true) {
                        let summary = format!(
                            "发现新版本 {}（当前 {}）",
                            info["latestVersion"].as_str().unwrap_or(""),
                            crate::update::CURRENT_VERSION
                        );
                        tracing::info!("[Update] {summary}");
                        (summary, true)
                    } else {
                        ("已完成版本检查".to_string(), true)
                    }
                }
                Err(error) => {
                    let summary = format!("版本检查失败（{error}）");
                    tracing::warn!("[Tasks] {summary}");
                    (summary, false)
                }
            }
        }
        TASK_CREDENTIAL_MAINTENANCE => {
            let refreshed = crate::account_session::refresh::refresh_expiring(
                &state.account_sessions,
                &state.http_client,
            )
            .await;
            let summary = format!("刷新 {refreshed} 个临期账号");
            if refreshed > 0 {
                tracing::info!("[Maintenance] 凭证自动维护：{summary}");
            }
            (summary, true)
        }
        TASK_MODEL_REFRESH => {
            if !crate::config::models_dev_enabled() {
                ("models.dev 元数据开关已关闭，跳过".to_string(), true)
            } else {
                match crate::model::models_dev::refresh(
                    &state.http_client,
                    &state.scheduled_task_store,
                )
                .await
                {
                    Ok(count) => {
                        let summary = format!("models.dev 元数据已更新 {count} 条");
                        tracing::info!("[Tasks] {summary}");
                        (summary, true)
                    }
                    Err(error) => {
                        let summary = format!("models.dev 元数据刷新失败（{error}）");
                        tracing::warn!("[Tasks] {summary}");
                        (summary, false)
                    }
                }
            }
        }
        _ => return Err("未知后端任务".to_string()),
    };
    guard.finish(success, summary.clone(), None, 0, interval)?;
    Ok(summary)
}

/// 调度循环：把后端任务按各自配置的间隔重复执行（由 `main` 起一次，进程内只有这一个循环）。
///
/// 循环体只做三件事：读配置 → 看谁到点了 → 跑它。所有排期状态都在 KV 里
/// （`scheduled_tasks::task_state`），因此**重启不重置节奏**。
///
/// 每 tick 读一次**全部**任务的状态（而不是逐条各读一次）：一次读库换三条任务
/// 的判定。存储不可用时整轮跳过 —— 那时发不出任何可信的判定，宁可这一轮什么都不做。
pub fn spawn(state: AppState) {
    task_state::install(state.scheduled_task_store.clone());
    tokio::spawn(async move {
        // 首轮先按持久化状态恢复排期：从未跑过的任务排到「现在」（于是全新安装
        // 启动即跑一次），跑过的沿用「上次尝试 + 间隔」。
        let init_settings = state.config_manager.get().await.scheduled_tasks;
        for task in TASKS.iter().filter(|task| task.runner == Runner::Backend) {
            if let Err(error) =
                task_state::reschedule(&state_key(task.id), interval_ms(task, &init_settings))
            {
                tracing::warn!("[Tasks] 恢复「{}」排期失败：{error}", task.label);
            }
        }
        loop {
            let Ok(states) = task_state::read_all() else {
                tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                continue;
            };
            for task in TASKS.iter().filter(|task| task.runner == Runner::Backend) {
                let settings = state.config_manager.get().await.scheduled_tasks;
                if !settings_of(&settings, task.id).enabled {
                    continue;
                }
                let Some(state_state) = states.get(&state_key(task.id)) else {
                    continue;
                };
                // 在跑（长任务）或还没到点（含失败冷却 / 占位租约）都跳过
                if state_state.running()
                    || chrono::Utc::now().timestamp_millis() < state_state.due_at()
                {
                    continue;
                }
                // 到点：跑一次（`run_backend` 内部按当前间隔重新排期）。
                if let Err(error) = run_backend(&state, task, false).await {
                    tracing::debug!("[Tasks] {}：{error}", task.label);
                }
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    });
}
