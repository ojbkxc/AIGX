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
//!
//! ── 与 `cron.rs` 的分工（页面模型只列本模块）──────────────────
//! `cron.rs` 承载**用户不可配**的运维基础设施任务（会话清扫、预留解冻等），
//! 不出现在设置页；本模块承载页面上的六条可配任务。新增后台任务先判断
//! 「用户该不该配它」：不该 → `cron.rs`，该 → 本模块 `TASKS`（并同步页面）。
//!
//! ── 「软件版本检查」的状态键是特例 ────────────────────────────
//! 它不在本模块内执行，`state_key` 委托给 `update::check_key`（按仓库隔离）：
//! 更新管理器的手动/定时检查与设置页「检查计划」读写同一份 `task_state`，
//! 因此页面上的「上次执行 / 下次执行」就是真实请求的排期，不会各排各的。

use std::time::Duration;

use serde_json::{json, Value};

use crate::api::openai::AppState;

pub mod task_state;
pub mod types;

use self::task_state::TaskState;
use self::types::{
    IntervalTask, IntervalTaskPatch, ScheduledSettings, DEFAULT_CREDENTIAL_MAINTENANCE_MINUTES,
    DEFAULT_LOGS_AUTO_REFRESH_SECONDS, DEFAULT_MAGPIE_PLUGIN_CHECK_MINUTES,
    DEFAULT_MODEL_REFRESH_MINUTES, DEFAULT_REPORT_AUTO_REFRESH_SECONDS,
    DEFAULT_REQUESTS_AUTO_REFRESH_SECONDS, DEFAULT_UPDATE_CHECK_MINUTES, INTERVAL_MAX_MINUTES,
    INTERVAL_MAX_SECONDS, INTERVAL_MIN_MINUTES, INTERVAL_MIN_SECONDS, KEY_CREDENTIAL_MAINTENANCE,
    KEY_LOGS_AUTO_REFRESH, KEY_MODEL_REFRESH, KEY_REPORT_AUTO_REFRESH, KEY_REQUESTS_AUTO_REFRESH,
    KEY_UPDATE_CHECK,
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
/// 社区 / Magpie 插件目录与已装插件检查（后端任务）。
///
/// 维护任务 id（`maintenance::PLUGIN_CATALOG`）与之同值；间隔取自
/// `ScheduledSettings.magpie_plugin_check`（与上游 `magpiePluginCheck` 同形）。
pub const TASK_PLUGIN_CATALOG: &str = "pluginCatalog";
pub const TASK_LOGS_AUTO_REFRESH: &str = KEY_LOGS_AUTO_REFRESH;
pub const TASK_REQUESTS_AUTO_REFRESH: &str = KEY_REQUESTS_AUTO_REFRESH;
pub const TASK_REPORT_AUTO_REFRESH: &str = KEY_REPORT_AUTO_REFRESH;

pub const TASKS: [TaskDef; 7] = [
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
        description: "定期向 GitHub 查询最新发布版本，默认每 20 分钟一次。检查结果、排期和限流冷却跨重启保留；手动检查也会合并重复请求并遵守在途占位。开关与间隔在「更新」页的「检查计划」里配置（定时任务页不再渲染这一条）。",
        unit: "minutes",
        runner: Runner::Backend,
        min: INTERVAL_MIN_MINUTES,
        max: INTERVAL_MAX_MINUTES,
        default_interval: DEFAULT_UPDATE_CHECK_MINUTES,
    },
    TaskDef {
        id: TASK_PLUGIN_CATALOG,
        label: "社区插件目录检查",
        description: "定期刷新社区 / Magpie 插件目录并检查已安装脚本插件的新版本，默认每 6 小时一次。检查结果统一进维护记录；发现新版本只提示，不自动升级。",
        unit: "minutes",
        runner: Runner::Backend,
        min: INTERVAL_MIN_MINUTES,
        max: INTERVAL_MAX_MINUTES,
        default_interval: DEFAULT_MAGPIE_PLUGIN_CHECK_MINUTES,
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

/// 某条任务**生效**的开关与间隔。
///
/// `pluginCatalog` 的 `checkMode=follow` 时开关与间隔都取「软件检查更新」计划
/// （`update_check`）：跟随时用户拨的是软件计划那一份，插件检查跟着走。
/// 自定义档（`plugin_catalog_follow=false`，AIGX 默认）取自己的
/// `magpie_plugin_check` —— 存量安装升级后行为逐字节不变。
fn settings_of(settings: &ScheduledSettings, id: &str) -> IntervalTask {
    match id {
        TASK_CREDENTIAL_MAINTENANCE => settings.credential_maintenance,
        TASK_MODEL_REFRESH => settings.model_refresh,
        TASK_UPDATE_CHECK => settings.update_check,
        TASK_PLUGIN_CATALOG => {
            if settings.plugin_catalog_follow {
                settings.update_check
            } else {
                settings.magpie_plugin_check
            }
        }
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

/// 某条任务在 `task_state` 里的排期键。
///
/// 「软件版本检查」**委托给更新管理器**：检查动作、失败冷却与「是否在跑」都
/// 记在 `update::check_key`（按仓库隔离）这一条上，本页只读同一份状态 ——
/// 否则定时循环和设置页的手动检查会各排各的，界面显示的「下次执行」永远
/// 对不上真实请求。其余任务用任务 id 本身（与 A2A `state_key` 同一口径）。
fn state_key(config: &crate::config::AppConfig, id: &str) -> String {
    if id == TASK_UPDATE_CHECK {
        crate::update::check_key(&config.update)
    } else {
        id.to_string()
    }
}

fn task_json(
    task: &TaskDef,
    config: &crate::config::AppConfig,
    state: Result<TaskState, String>,
) -> Value {
    let settings = &config.scheduled_tasks;
    let it = settings_of(settings, task.id);
    let backend = task.runner == Runner::Backend;
    let error = state.as_ref().err().cloned();
    let state = state.unwrap_or_default();
    let timestamp = |at: i64| if at > 0 { json!(at) } else { Value::Null };
    let mut item = json!({
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
    });
    if task.id == TASK_PLUGIN_CATALOG {
        // 排期来源（对齐 A2A `task_json` 的 pluginCatalog 分支）：
        //   follow = 跟随「软件检查更新」计划（开关与间隔都取那一份）
        //   custom = 自己一份开关与间隔（AIGX 默认，见 `ScheduledSettings`）
        // autoUpdate 映射自 `update.auto_apply`（AIGX 的「自动应用」比上游的
        // 「自动下载」激进一档，但落点同一处，如实映射）。
        item["checkMode"] = json!(if settings.plugin_catalog_follow {
            "follow"
        } else {
            "custom"
        });
        item["autoUpdate"] = json!(config.update.auto_apply);
    }
    item
}

pub async fn list(state: &AppState) -> Value {
    // 一次读出全部状态：本接口被「定时任务」页每 20 秒轮询一次，
    // 逐条任务各读一次 KV 没有意义。
    let config = state.config_manager.get().await;
    let states = task_state::read_all();
    json!({
        "tasks": TASKS
            .iter()
            .map(|task| {
                let state = match &states {
                    Ok(states) if task.runner == Runner::Backend =>
                        Ok(states.get(&state_key(&config, task.id)).cloned().unwrap_or_default()),
                    Ok(_) => Ok(TaskState::default()),
                    Err(error) => Err(error.clone()),
                };
                task_json(task, &config, state)
            })
            .collect::<Vec<_>>(),
    })
}

pub async fn task_by_id(state: &AppState, id: &str) -> Value {
    let task = match find(id) {
        Some(task) => task,
        None => return Value::Null,
    };
    let config = state.config_manager.get().await;
    let state_state = if task.runner == Runner::Backend {
        task_state::read(&state_key(&config, task.id))
    } else {
        Ok(TaskState::default())
    };
    task_json(task, &config, state_state)
}

/// 把 patch 落到 `settings` 的对应字段（按 id 选中那一条）。
///
/// 只处理 `enabled` / `interval`；`checkMode` / `autoUpdate` 的校验与落点见
/// [`validate_patch`] 与 [`configure`]（`autoUpdate` 落到 `update.auto_apply`）。
fn apply_patch(settings: &mut ScheduledSettings, id: &str, patch: &IntervalTaskPatch) {
    let target = match id {
        TASK_CREDENTIAL_MAINTENANCE => &mut settings.credential_maintenance,
        TASK_MODEL_REFRESH => &mut settings.model_refresh,
        TASK_UPDATE_CHECK => &mut settings.update_check,
        TASK_PLUGIN_CATALOG => &mut settings.magpie_plugin_check,
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

/// 校验 `checkMode` / `autoUpdate` 并返回是否 `follow`（纯函数，便于单测）。
///
/// 规则（对齐 A2A `configure_extended` 的 pluginCatalog 分支）：
/// - 两个字段只对 `pluginCatalog` 有意义（其它任务传入 → 400，避免静默无效）；
/// - `checkMode` 接受 `"follow"` / `"custom"`：跟随时开关与间隔取软件计划，
///   不落 `interval`（存一个不会被使用的值只会让人以为它生效）；
/// - `autoUpdate` 的落点是 `update.auto_apply`，不写 `scheduled_tasks`。
fn validate_patch(task: &TaskDef, patch: &IntervalTaskPatch) -> Result<bool, String> {
    let touches_plugin_opts = patch.check_mode.is_some() || patch.auto_update.is_some();
    if touches_plugin_opts && task.id != TASK_PLUGIN_CATALOG {
        return Err(format!("「{}」不支持 checkMode / autoUpdate", task.label));
    }
    match patch.check_mode.as_deref() {
        None => Ok(false),
        Some(mode) => {
            let mode = mode.trim();
            if mode.eq_ignore_ascii_case("follow") {
                Ok(true)
            } else if mode.eq_ignore_ascii_case("custom") {
                Ok(false)
            } else {
                Err(format!(
                    "「{}」的 checkMode 只支持 follow 或 custom（收到 {mode}）",
                    task.label
                ))
            }
        }
    }
}

/// 后端任务的排期变更（开关拨开 → 立即跑一轮；改间隔 → 按新间隔重排）。
///
/// `configure` 与插件设置 PATCH（`/api/plugins/settings` 的检查计划跟随全局）
/// 共用这一份口径：两处入口都改同一个 `scheduled_tasks` 配置，排期不能各写一套。
pub(crate) fn on_schedule_changed(
    config: &crate::config::AppConfig,
    id: &str,
    before: crate::scheduled_tasks::types::IntervalTask,
    after: crate::scheduled_tasks::types::IntervalTask,
) -> Result<(), String> {
    let Some(task) = find(id) else {
        return Ok(());
    };
    if task.runner != Runner::Backend || before == after {
        return Ok(());
    }
    let key = state_key(config, id);
    if !before.enabled && after.enabled {
        // 刚开启：保留「开启即跑一次」的语义（下一个 tick 就执行），
        // 而不是按上次尝试时间算出的旧排期干等。失败冷却仍然优先。
        task_state::schedule_now(&key)
    } else if before.interval != after.interval {
        // 改间隔：按**新**间隔从上次尝试起重新计时（要马上跑有「立即执行」按钮）
        task_state::reschedule(&key, interval_ms(task, &config.scheduled_tasks))
    } else {
        Ok(())
    }
}

pub async fn configure(
    state: &AppState,
    id: &str,
    patch: IntervalTaskPatch,
) -> Result<Value, String> {
    let task = find(id).ok_or_else(|| format!("未知的定时任务: {id}"))?;
    if patch.enabled.is_none()
        && patch.interval.is_none()
        && patch.check_mode.is_none()
        && patch.auto_update.is_none()
    {
        return Err("没有需要更新的字段".to_string());
    }
    // checkMode=follow：插件检查跟随软件更新计划，interval 由计划决定 ——
    // 跳过间隔范围强制校验，且不落 interval（避免存一个不会被使用的越界值）。
    let follow = validate_patch(task, &patch)?;
    if let Some(interval) = patch.interval {
        if !follow && !(task.min..=task.max).contains(&interval) {
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
    let mut effective = patch.clone();
    if follow {
        // 跟随态：开关与间隔都取软件计划，**两样都不落**插件检查自己的存档
        // （写进去也不生效，只会在切回 custom 时冒出一个没人设过的值）。
        effective.interval = None;
        effective.enabled = None;
    }
    apply_patch(&mut config.scheduled_tasks, id, &effective);
    // checkMode 的落点是 `plugin_catalog_follow`（生效口径的唯一真相）。
    // 只在显式传了 checkMode 时改：没传 = 不动（读-改-写语义）。
    if let Some(mode) = patch.check_mode.as_deref() {
        config.scheduled_tasks.plugin_catalog_follow = mode.trim().eq_ignore_ascii_case("follow");
    }
    // autoUpdate 的权威落点是软件更新的自动应用开关（与上游同口径）。
    if let Some(auto_update) = patch.auto_update {
        config.update.auto_apply = auto_update;
    }
    let after = settings_of(&config.scheduled_tasks, id);
    state
        .config_manager
        .update(config)
        .await
        .map_err(|error| format!("保存配置失败: {error}"))?;
    // 落盘后的权威配置：排期键要按（可能已变的）仓库/设置重算。
    let config = state.config_manager.get().await;
    on_schedule_changed(&config, id, before, after)?;
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
    let mut result = task_by_id(state, id).await;
    // 与 A2A 一致：把「是否写盘成功」带给界面（AIGX 的 ConfigManager 写失败会
    // 直接 Err，能走到这里即为 true；字段保留是为了前端契约同形）。
    result["saved"] = json!(true);
    Ok(result)
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

/// 某条任务的排期间隔（毫秒）。维护模块 claim 时与本循环共用同一口径。
pub(crate) fn task_interval_ms(settings: &ScheduledSettings, id: &str) -> i64 {
    find(id)
        .map(|task| interval_ms(task, settings))
        .unwrap_or(60_000)
}

/// 模型目录刷新任务是否开启。
///
/// 被动触发（`GET /v1/models` 后异步刷新）与调度循环必须用同一判据 —— 否则
/// 用户关掉任务后，被动路径仍在暗处联网，开关形同虚设。
pub(crate) fn model_refresh_enabled(settings: &ScheduledSettings) -> bool {
    settings.model_refresh.enabled
}

async fn run_backend(state: &AppState, task: &TaskDef, manual: bool) -> Result<String, String> {
    // 维护型任务（凭证 / 模型目录 / 软件版本 / 社区插件目录）：完整执行记录由
    // maintenance 模块负责，本函数只把结果摘要回给调用方（scheduled_tasks 仍
    // 只负责「何时做」）。
    if matches!(
        task.id,
        TASK_CREDENTIAL_MAINTENANCE | TASK_MODEL_REFRESH | TASK_UPDATE_CHECK | TASK_PLUGIN_CATALOG
    ) {
        let detail =
            crate::maintenance::execute(state, crate::maintenance::Request::full(task.id, manual))
                .await?;
        return Ok(crate::maintenance::summary(&detail));
    }
    Err("未知后端任务".to_string())
}

/// 一次模型目录刷新的结果。
///
/// `results` 是**逐启用渠道（provider）**的明细行（成功 / 跳过 / 失败 + 条数），
/// 由维护模块归成 `Item`；`summary`/`ok`/`count` 是 models.dev 元数据汇总
/// （仍记在 `run.metadata` 一条）。两者分开是为了「某家渠道坏了」能在维护明细里
/// 看出来，而元数据的成败不淹没在渠道行里。
pub(crate) struct ModelRefreshWork {
    pub summary: String,
    pub ok: bool,
    pub count: usize,
    pub results: Vec<Value>,
}

/// 目录刷新实际会拉取的上游渠道：启用的 OpenAI 兼容渠道。
///
/// 账号型渠道的模型来自账号池、插件渠道的上游由插件进程构造，网关在这里不会
/// 为它们拉 `/models`，所以它们不出现在逐项结果里（拿不到的数据不编造）。
pub(crate) fn model_refresh_targets(
    channels: &[crate::channel::Channel],
) -> Vec<crate::channel::Channel> {
    channels
        .iter()
        .filter(|ch| {
            ch.is_enabled() && ch.channel_type == crate::channel::ChannelType::OpenaiCompatible
        })
        .cloned()
        .collect()
}

/// 一行渠道级刷新结果（形状交给 `maintenance::types::item` 归一）。
fn model_refresh_row(
    provider: &str,
    label: &str,
    status: &'static str,
    message: Option<String>,
    count: usize,
    duration_ms: i64,
) -> Value {
    json!({
        "provider": provider,
        "providerLabel": label,
        "status": status,
        "message": message,
        "count": count,
        "durationMs": duration_ms,
    })
}

/// 模型目录刷新的一轮工作（models.dev + 渠道清单 + 目录对账）。
///
/// 由 `maintenance::execute`（自动 / 手动 / 重试 / 被动）调用。抽出来是为了让
/// 「维护明细」与「定时任务循环」共用同一份实现，而不是两边各写一遍。
pub(crate) async fn model_refresh_work(state: &AppState) -> ModelRefreshWork {
    if !crate::config::models_dev_enabled() {
        return ModelRefreshWork {
            summary: "models.dev 元数据开关已关闭，跳过".to_string(),
            ok: true,
            count: 0,
            results: Vec::new(),
        };
    }
    {
        let (count, catalog_ok, catalog_note) = match crate::model::models_dev::refresh(
            &state.http_client,
            &state.scheduled_task_store,
        )
        .await
        {
            Ok(count) => (count, true, String::new()),
            Err(error) => (0, false, format!("models.dev 元数据刷新失败（{error}）")),
        };
        // 顺带刷一遍渠道模型清单（对齐 A2A catalog_refresh：上游
        // 关模型后本地目录要等下次发现才收敛，期间请求吃 404/502——
        // 生产实证见 08-prod「目录漂移」）。只刷启用且带渠道级凭证的
        // openai_compat 渠道；逐渠道失败不判整轮失败（各渠道独立）。
        let mut refreshed_channels = 0usize;
        // 逐启用渠道的结果行（维护明细）；无渠道时为空的 Vec，不 panic。
        let mut results: Vec<Value> = Vec::new();
        // 目录对账（P2-6②）：声明 vs 发现差异 + 渠道级悬空映射统计（只读）
        let mut drift_channels: Vec<Value> = Vec::new();
        // 阶段2：清理候选（与对账同口径组装；守卫判定全在 plan_channel_prune）
        let mut prune_candidates: Vec<crate::channel::PruneCandidate> = Vec::new();
        for ch in model_refresh_targets(&state.channel_store.list()) {
            let started = std::time::Instant::now();
            // 模型目录的「提供商」就是渠道本身：用渠道名做 provider / 展示名，
            // 让维护明细里每一家渠道各成一行（同名渠道罕见，可接受折叠）。
            let provider = ch.name.as_str();
            let elapsed_ms = || started.elapsed().as_millis().min(i64::MAX as u128) as i64;
            // 启用但没配上游地址：不是失败，但也没拉到东西 —— 如实记 skipped。
            if ch.base_url.trim().is_empty() {
                results.push(model_refresh_row(
                    provider,
                    &ch.name,
                    "skipped",
                    Some("渠道未配置上游地址，未拉取模型清单".to_string()),
                    0,
                    elapsed_ms(),
                ));
                continue;
            }
            let api_key = ch.decode_api_key();
            match crate::api::admin::fetch_upstream_models(
                state,
                ch.channel_type,
                &ch.base_url,
                &api_key,
                &ch.quirks,
            )
            .await
            {
                Ok((_, models)) => {
                    if !models.is_empty() {
                        let model_count = models.len();
                        // 对账（P2-6②）：有效清单 = models 非空用白名单，空用本轮发现快照。
                        // 必须先于 save_discovered_models（其 move 掉 models）。
                        // 直接吃循环里 ch 的 model_mapping（无现取窗口，避免并发删改静默少报）。
                        // 阶段2：对账同时组「清理候选」——`alive_source` 与 `alive` 与判定
                        // 同口径；守卫判定（仅白名单源、单轮限量）在 plan_channel_prune。
                        if let Some(drift) = crate::channel::reconcile_channel_drift(&ch, &models) {
                            let stale: Vec<(String, String)> = drift["stale_mappings"]
                                .as_array()
                                .map(|arr| {
                                    arr.iter()
                                        .filter_map(|m| {
                                            Some((
                                                m["alias"].as_str()?.to_string(),
                                                m["target"].as_str()?.to_string(),
                                            ))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            if !stale.is_empty() {
                                let whitelist = !ch.models.is_empty();
                                prune_candidates.push(crate::channel::PruneCandidate {
                                    channel_id: ch.id.clone(),
                                    channel_name: ch.name.clone(),
                                    alive_source: if whitelist {
                                        "whitelist"
                                    } else {
                                        "discovered"
                                    },
                                    alive: if whitelist {
                                        ch.models.clone()
                                    } else {
                                        models.clone()
                                    },
                                    stale_mappings: stale,
                                });
                            }
                            drift_channels.push(drift);
                        }
                        let _ = state.channel_store.save_discovered_models(&ch.id, models);
                        refreshed_channels += 1;
                        results.push(model_refresh_row(
                            provider,
                            &ch.name,
                            "refreshed",
                            None,
                            model_count,
                            elapsed_ms(),
                        ));
                    } else {
                        results.push(model_refresh_row(
                            provider,
                            &ch.name,
                            "skipped",
                            Some("上游未返回模型清单".to_string()),
                            0,
                            elapsed_ms(),
                        ));
                    }
                }
                Err((status, body)) => {
                    let message = body
                        .get("message")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    tracing::warn!(
                        "[Tasks] 渠道「{}」模型清单刷新失败：HTTP {status} {message}",
                        ch.name
                    );
                    results.push(model_refresh_row(
                        provider,
                        &ch.name,
                        "failed",
                        Some(format!("HTTP {status} {message}")),
                        0,
                        elapsed_ms(),
                    ));
                }
            }
        }
        // ── #33 阶段2：渠道级悬空映射清理 ──
        // 守卫1 白名单源 / 守卫2 删前快照 / 守卫3 单轮限量 / 守卫4 幂等 /
        // 守卫5 留痕 / 守卫6 待修清单；判定全在 plan_channel_prune。
        // 本轮时刻取一次复用：快照/日志/drift 报告同一 `at`，三者可精确互证。
        let round_at = chrono::Utc::now().timestamp();
        let prune_plan = crate::channel::plan_channel_prune(
            prune_candidates,
            crate::channel::CHANNEL_PRUNE_LIMIT,
        );
        if prune_plan.over_limit {
            tracing::warn!(
                "[Tasks] 悬空映射清理本轮超限量（{} 条 > 上限 {}），只报告不删",
                prune_plan.total_stale,
                crate::channel::CHANNEL_PRUNE_LIMIT
            );
        }
        let pruned =
            crate::channel::execute_channel_prune(&state.channel_store, &prune_plan.deletions);
        // 守卫2+守卫5：删除明细落 KV 快照（可回填）+ 日志留痕，绝不静默删
        if crate::channel::should_persist_prune_snapshot(&pruned) {
            let snapshot = crate::channel::build_prune_snapshot(&pruned, round_at);
            if let Err(e) = state
                .scheduled_task_store
                .put("catalog:prune:last", &snapshot)
            {
                tracing::warn!("[Tasks] 悬空映射清理快照写入失败：{e}");
            }
            tracing::info!(
                "[Tasks] 悬空映射清理：删除 {} 条（快照 catalog:prune:last，可据 restore 回填；触发轮次 at={round_at}）",
                pruned.len()
            );
        }
        // 守卫6：待修清单（已删的「若本意保留请按正确大小写回填」+ 只报不删的）
        let mut pending_fix: Vec<Value> = Vec::new();
        for item in &pruned {
            pending_fix.push(json!({
                "channel_id": item.channel_id,
                "channel_name": item.channel_name,
                "alive_source": "whitelist",
                "alias": item.alias,
                "target": item.target,
                "action": "deleted",
            }));
        }
        for candidate in &prune_plan.deferred {
            for (alias, target) in &candidate.stale_mappings {
                pending_fix.push(json!({
                    "channel_id": candidate.channel_id,
                    "channel_name": candidate.channel_name,
                    "alive_source": candidate.alive_source,
                    "alias": alias,
                    "target": target,
                    "action": "deferred",
                    "reason": if prune_plan.over_limit { "over_limit" } else { "discovered_source" },
                }));
            }
        }
        // 对账报告落 KV（catalog:drift:latest）——诊断页/健康页可读
        let n_drift = drift_channels.len();
        let n_gone: usize = drift_channels
            .iter()
            .map(|c| {
                c["declared_not_found"]
                    .as_array()
                    .map(|a| a.len())
                    .unwrap_or(0)
            })
            .sum();
        let n_stale: usize = drift_channels
            .iter()
            .map(|c| c["stale_mappings"].as_array().map(|a| a.len()).unwrap_or(0))
            .sum();
        if n_drift > 0 {
            let report = json!({
                "at": round_at,
                "channels": drift_channels,
                "prune": {
                    "limit": crate::channel::CHANNEL_PRUNE_LIMIT,
                    "over_limit": prune_plan.over_limit,
                    "deleted_count": pruned.len(),
                    "snapshot_key": "catalog:prune:last",
                    "pending_fix": pending_fix,
                },
            });
            if let Err(e) = state
                .scheduled_task_store
                .put("catalog:drift:latest", &report)
            {
                tracing::warn!("[Tasks] 目录对账报告写入失败：{e}");
            }
        }
        let mut summary = format!("models.dev 元数据已更新 {count} 条");
        if refreshed_channels > 0 {
            summary.push_str(&format!("，刷新 {refreshed_channels} 个渠道清单"));
        }
        if n_drift > 0 {
            tracing::info!(
                "[Tasks] 目录对账：{n_drift} 个渠道有漂移迹象（{n_gone} 条疑似下线模型，{n_stale} 条悬空映射，详情见 catalog:drift:latest）"
            );
            summary.push_str(&format!(
                "，目录对账 {n_drift} 渠道有漂移迹象（{n_gone} 条疑似下线，{n_stale} 条悬空映射）"
            ));
        }
        if !pruned.is_empty() {
            summary.push_str(&format!("，悬空映射清理 {} 条", pruned.len()));
        } else if prune_plan.over_limit {
            summary.push_str(&format!(
                "，悬空映射 {} 条超限量只报告",
                prune_plan.total_stale
            ));
        }
        if !catalog_ok {
            summary.push_str(&format!("；{catalog_note}"));
            tracing::warn!("[Tasks] {summary}");
            ModelRefreshWork {
                summary,
                ok: false,
                count,
                results,
            }
        } else {
            tracing::info!("[Tasks] {summary}");
            ModelRefreshWork {
                summary,
                ok: true,
                count,
                results,
            }
        }
    }
}

/// 调度循环：把后端任务按各自配置的间隔重复执行（由 `main` 起一次，进程内只有这一个循环）。
///
/// 循环体只做三件事：读配置 → 看谁到点了 → 跑它。所有排期状态都在 KV 里
/// （`scheduled_tasks::task_state`），因此**重启不重置节奏**。
///
/// 每 tick 读一次**全部**任务的状态（而不是逐条各读一次）：一次读库换三条任务
/// 的判定。存储不可用时整轮跳过 —— 那时发不出任何可信的判定，宁可这一轮什么都不做。
pub fn spawn(state: AppState) {
    // 启动段（main.rs）已显式装填；这里保留兜底（幂等，缺装配时仍可用）。
    task_state::install(state.scheduled_task_store.clone());
    tokio::spawn(async move {
        // 首轮先按持久化状态恢复排期：从未跑过的任务排到「现在」（于是全新安装
        // 启动即跑一次），跑过的沿用「上次尝试 + 间隔」。
        let init_config = state.config_manager.get().await;
        for task in TASKS.iter().filter(|task| task.runner == Runner::Backend) {
            if let Err(error) = task_state::reschedule(
                &state_key(&init_config, task.id),
                interval_ms(task, &init_config.scheduled_tasks),
            ) {
                tracing::warn!("[Tasks] 恢复「{}」排期失败：{error}", task.label);
            }
        }
        loop {
            let Ok(states) = task_state::read_all() else {
                tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
                continue;
            };
            for task in TASKS.iter().filter(|task| task.runner == Runner::Backend) {
                // 逐条读当前配置：上一条若是长任务（更新检查），本拍后续任务的
                // 开关/间隔仍以最新配置为准（改完设置最多慢一个 tick 生效）。
                let config = state.config_manager.get().await;
                if !settings_of(&config.scheduled_tasks, task.id).enabled {
                    continue;
                }
                // 状态条目缺失（启动时 store 不可用 / KV 被清 / 新任务 id）
                // 不跳过：按「从未跑过」交给 run_backend → claim 重建排期，
                // 否则该任务会永久停摆直到下一次重启（C-7）。
                let Some(state_state) = states.get(&state_key(&config, task.id)) else {
                    if let Err(error) = task_state::reschedule(
                        &state_key(&config, task.id),
                        interval_ms(task, &config.scheduled_tasks),
                    ) {
                        tracing::warn!("[Tasks] 重建「{}」排期失败：{error}", task.label);
                    }
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

#[cfg(test)]
mod tests {
    use super::{
        settings_of, state_key, task_interval_ms, TASK_CREDENTIAL_MAINTENANCE, TASK_MODEL_REFRESH,
        TASK_PLUGIN_CATALOG, TASK_UPDATE_CHECK,
    };
    use crate::scheduled_tasks::types::{IntervalTask, IntervalTaskPatch, ScheduledSettings};

    /// 社区插件目录检查读取 `magpiePluginCheck` 配置，默认 360 分钟。
    #[test]
    fn plugin_catalog_reads_magpie_plugin_settings() {
        let mut settings = ScheduledSettings::default();
        assert_eq!(settings.magpie_plugin_check.interval, 360);
        assert_eq!(
            task_interval_ms(&settings, TASK_PLUGIN_CATALOG),
            360 * 60_000
        );
        super::apply_patch(
            &mut settings,
            TASK_PLUGIN_CATALOG,
            &IntervalTaskPatch {
                enabled: Some(false),
                interval: Some(120),
                ..Default::default()
            },
        );
        assert!(!settings.magpie_plugin_check.enabled);
        assert_eq!(settings.magpie_plugin_check.interval, 120);
        // 不影响其它任务
        assert!(settings.update_check.enabled);
    }

    /// checkMode 接受 follow / custom（对齐 A2A `configure_extended`）；
    /// checkMode/autoUpdate 仅对 pluginCatalog 生效。
    #[test]
    fn patch_validates_check_mode_and_auto_update() {
        let plugin = super::find(TASK_PLUGIN_CATALOG).unwrap();
        let credential = super::find(TASK_CREDENTIAL_MAINTENANCE).unwrap();
        // follow（大小写不敏感）接受，并回报 follow=true。
        assert!(super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                check_mode: Some("follow".into()),
                ..Default::default()
            }
        )
        .unwrap());
        assert!(super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                check_mode: Some("FOLLOW".into()),
                ..Default::default()
            }
        )
        .unwrap());
        // custom（AIGX 默认档）接受，并回报 follow=false —— 与 A2A 同两档，
        // 此前只认 follow 是「漏了一档」而不是有意偏离。
        assert!(!super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                check_mode: Some("custom".into()),
                ..Default::default()
            }
        )
        .unwrap());
        assert!(!super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                check_mode: Some("Custom".into()),
                ..Default::default()
            }
        )
        .unwrap());
        // 其它 checkMode 拒绝。
        assert!(super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                check_mode: Some("auto".into()),
                ..Default::default()
            }
        )
        .is_err());
        // 非 pluginCatalog 任务拒绝 checkMode / autoUpdate。
        assert!(super::validate_patch(
            credential,
            &IntervalTaskPatch {
                auto_update: Some(true),
                ..Default::default()
            }
        )
        .is_err());
        // 仅 autoUpdate（pluginCatalog）：接受，follow=false。
        assert!(!super::validate_patch(
            plugin,
            &IntervalTaskPatch {
                auto_update: Some(true),
                ..Default::default()
            }
        )
        .unwrap());
    }

    /// `plugin_catalog_follow` 是 pluginCatalog 的生效口径唯一真相：
    /// follow 时开关与间隔都取 `update_check`；custom 时取自己的存档。
    ///
    /// 这是「跟随时拨的是软件计划那一份」的实现侧判据 —— 两侧来回切一次，
    /// 各自的存档都不被对方改写（切回去原值原样回来）。
    #[test]
    fn plugin_catalog_follow_switches_effective_settings() {
        let mut settings = ScheduledSettings::default();
        settings.update_check = IntervalTask {
            enabled: true,
            interval: 20,
        };
        settings.magpie_plugin_check = IntervalTask {
            enabled: false,
            interval: 360,
        };

        // 默认 custom：取自己的存档（存量行为逐字节不变）
        assert!(!settings.plugin_catalog_follow);
        assert_eq!(
            settings_of(&settings, TASK_PLUGIN_CATALOG),
            IntervalTask {
                enabled: false,
                interval: 360
            }
        );

        // follow：开关与间隔都跟软件计划
        settings.plugin_catalog_follow = true;
        assert_eq!(
            settings_of(&settings, TASK_PLUGIN_CATALOG),
            IntervalTask {
                enabled: true,
                interval: 20
            }
        );

        // 切回 custom：自己的存档没被 follow 那段改写掉
        settings.plugin_catalog_follow = false;
        assert_eq!(
            settings_of(&settings, TASK_PLUGIN_CATALOG),
            IntervalTask {
                enabled: false,
                interval: 360
            }
        );

        // 间隔也走同一口径（claim 用 task_interval_ms，与 settings_of 同源）
        settings.plugin_catalog_follow = true;
        assert_eq!(task_interval_ms(&settings, TASK_PLUGIN_CATALOG), 20 * 60_000);
        settings.plugin_catalog_follow = false;
        assert_eq!(
            task_interval_ms(&settings, TASK_PLUGIN_CATALOG),
            360 * 60_000
        );
    }

    /// 维护模块 claim 与调度循环共用同一排期口径。
    #[test]
    fn task_interval_ms_matches_units() {
        let settings = ScheduledSettings::default();
        assert_eq!(
            task_interval_ms(&settings, TASK_CREDENTIAL_MAINTENANCE),
            settings.credential_maintenance.interval * 60_000
        );
        assert_eq!(
            task_interval_ms(&settings, TASK_MODEL_REFRESH),
            settings.model_refresh.interval * 60_000
        );
        assert_eq!(
            task_interval_ms(&settings, TASK_UPDATE_CHECK),
            settings.update_check.interval * 60_000
        );
        // 未知任务：1 分钟兜底（claim 不会因未知 id 卡死）
        assert_eq!(task_interval_ms(&settings, "nope"), 60_000);
    }

    /// 版本检查的状态键必须与更新管理器同一口径（按仓库隔离），
    /// 其余任务仍用任务 id（页面/调度/维护三方读同一行）。
    #[test]
    fn update_check_state_key_delegates_to_update_manager() {
        let config = crate::config::AppConfig::default();
        let key = state_key(&config, TASK_UPDATE_CHECK);
        assert_eq!(key, crate::update::check_key(&config.update));
        assert!(key.starts_with("updateCheck:website:"), "key = {key}");
        assert_eq!(
            state_key(&config, TASK_CREDENTIAL_MAINTENANCE),
            TASK_CREDENTIAL_MAINTENANCE
        );
        assert_eq!(state_key(&config, TASK_MODEL_REFRESH), TASK_MODEL_REFRESH);
    }

    /// 缺口 3 的开关判据：被动触发只在「模型目录刷新」任务开启时执行。
    #[test]
    fn passive_refresh_respects_enabled_flag() {
        let mut settings = ScheduledSettings::default();
        assert!(
            super::model_refresh_enabled(&settings),
            "默认应开启模型目录刷新（被动触发可执行）"
        );
        settings.model_refresh.enabled = false;
        assert!(
            !super::model_refresh_enabled(&settings),
            "任务关闭时被动触发必须不执行"
        );
    }

    /// 缺口 2：逐渠道结果只覆盖真正会拉取的上游（启用 OpenAI 兼容）；
    /// 无渠道时返回空集且不 panic。
    #[test]
    fn model_refresh_targets_filter_and_empty() {
        use crate::channel::Channel;
        let empty: Vec<Channel> = Vec::new();
        assert!(
            super::model_refresh_targets(&empty).is_empty(),
            "无渠道时应产出空结果集（不 panic）"
        );
        let channel = |id: &str, kind: &str, status: &str| -> Channel {
            serde_json::from_value(serde_json::json!({
                "id": id,
                "name": id,
                "channel_type": kind,
                "status": status,
            }))
            .unwrap()
        };
        let channels = vec![
            channel("a", "openai_compatible", "enabled"),
            channel("b", "openai_compatible", "disabled"),
            channel("c", "account", "enabled"),
        ];
        let targets = super::model_refresh_targets(&channels);
        assert_eq!(targets.len(), 1, "只保留启用且 OpenAI 兼容的渠道");
        assert_eq!(targets[0].id, "a");
    }

    /// 缺口 2：渠道结果行经 `types::item` 归一后带上 provider / status / count。
    #[test]
    fn model_refresh_rows_carry_status_and_count() {
        use std::collections::HashMap;
        let raw =
            super::model_refresh_row("openai_compatible", "My Channel", "refreshed", None, 12, 34);
        let item = crate::maintenance::types::item(&raw, 0, &HashMap::new());
        assert_eq!(item.provider, "openai_compatible");
        assert_eq!(item.provider_label, "My Channel");
        assert_eq!(item.status, "refreshed");
        assert_eq!(item.count, Some(12));
        assert_eq!(item.duration_ms, Some(34));

        let raw = super::model_refresh_row(
            "openai_compatible",
            "My Channel",
            "failed",
            Some("HTTP 401 Unauthorized".to_string()),
            0,
            9,
        );
        let item = crate::maintenance::types::item(&raw, 0, &HashMap::new());
        assert_eq!(item.status, "failed");
        assert_eq!(item.message.as_deref(), Some("HTTP 401 Unauthorized"));
    }
}
