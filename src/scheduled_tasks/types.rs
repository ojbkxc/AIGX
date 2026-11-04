//! 间隔型定时任务的配置形状与键名常量（移植自 Agent2API `config/types.rs`）。
//!
//! ── 两类任务（区别是**谁来执行**，不是可配性）─────────────────
//!   - `Runner::Backend`：凭证自动维护、模型目录刷新、软件版本检查。后端循环执行。
//!   - `Runner::Frontend`：日志页 / 请求日志页 / 报表页自动刷新。定时器长在页面。
//!
//! ── 配置存哪 ───────────────────────────────────────────────
//! 六条任务的开关与间隔整份存进 `AppConfig.scheduled_tasks`（TOML，随配置持久化），
//! 不在 KV 里另存一份 —— 配置是「用户意图」，状态（上次执行 / 冷却）才是「运行记录」。

use serde::{Deserialize, Serialize};

/// 整份定时任务配置在 `AppConfig` 里的键（仅文档用途，实际由 `AppConfig.scheduled_tasks` 承载）。
pub const KEY_SCHEDULED_TASKS: &str = "scheduledTasks";

/// 凭证自动维护（后端任务）。
pub const KEY_CREDENTIAL_MAINTENANCE: &str = "credentialMaintenance";
/// 模型目录刷新（后端任务）。
pub const KEY_MODEL_REFRESH: &str = "modelRefresh";
/// 日志页自动刷新（前端任务）。
pub const KEY_LOGS_AUTO_REFRESH: &str = "logsAutoRefresh";
/// 请求日志页自动刷新（前端任务）。
pub const KEY_REQUESTS_AUTO_REFRESH: &str = "requestsAutoRefresh";
/// 报表自动刷新（前端任务）。
pub const KEY_REPORT_AUTO_REFRESH: &str = "reportAutoRefresh";
/// 软件版本检查（后端任务）。
pub const KEY_UPDATE_CHECK: &str = "updateCheck";

/// 凭证自动维护默认间隔（分钟）。
pub const DEFAULT_CREDENTIAL_MAINTENANCE_MINUTES: i64 = 10;

/// 模型目录刷新默认间隔（分钟）。
///
/// AIGX 只做 models.dev 元数据刷新（能力位 + 思考档位），上游变化不频繁，
/// 默认一天一次足够；Agent2API 的 60 分钟是为「模型上下架频繁」的桌面场景设的。
pub const DEFAULT_MODEL_REFRESH_MINUTES: i64 = 1440;

/// 日志页自动刷新默认间隔（秒）。
pub const DEFAULT_LOGS_AUTO_REFRESH_SECONDS: i64 = 1;
/// 请求日志页自动刷新默认间隔（秒）。
pub const DEFAULT_REQUESTS_AUTO_REFRESH_SECONDS: i64 = 1;
/// 报表自动刷新默认间隔（秒）。
pub const DEFAULT_REPORT_AUTO_REFRESH_SECONDS: i64 = 1;

/// 软件版本检查默认间隔（分钟）。
pub const DEFAULT_UPDATE_CHECK_MINUTES: i64 = 20;

/// 后端任务间隔下限（分钟）。
pub const INTERVAL_MIN_MINUTES: i64 = 1;
/// 后端任务间隔上限（分钟）。
pub const INTERVAL_MAX_MINUTES: i64 = 1440;
/// 前端任务间隔下限（秒）。
pub const INTERVAL_MIN_SECONDS: i64 = 1;
/// 前端任务间隔上限（秒）。
pub const INTERVAL_MAX_SECONDS: i64 = 600;

/// 一条间隔型任务的开关与间隔。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct IntervalTask {
    /// 是否启用。
    pub enabled: bool,
    /// 间隔（单位见 `TaskDef.unit`）。
    pub interval: i64,
}

/// 六条间隔型任务的配置整份（随 `AppConfig` 持久化）。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct ScheduledSettings {
    /// 凭证自动维护（后端）。
    pub credential_maintenance: IntervalTask,
    /// 模型目录刷新（后端）。
    pub model_refresh: IntervalTask,
    /// 软件版本检查（后端）。
    pub update_check: IntervalTask,
    /// 日志页自动刷新（前端）。
    pub logs_auto_refresh: IntervalTask,
    /// 请求日志页自动刷新（前端）。
    pub requests_auto_refresh: IntervalTask,
    /// 报表自动刷新（前端）。
    pub report_auto_refresh: IntervalTask,
}

impl Default for ScheduledSettings {
    fn default() -> Self {
        Self {
            credential_maintenance: IntervalTask {
                enabled: true,
                interval: DEFAULT_CREDENTIAL_MAINTENANCE_MINUTES,
            },
            model_refresh: IntervalTask {
                enabled: true,
                interval: DEFAULT_MODEL_REFRESH_MINUTES,
            },
            update_check: IntervalTask {
                enabled: true,
                interval: DEFAULT_UPDATE_CHECK_MINUTES,
            },
            logs_auto_refresh: IntervalTask {
                enabled: true,
                interval: DEFAULT_LOGS_AUTO_REFRESH_SECONDS,
            },
            requests_auto_refresh: IntervalTask {
                enabled: true,
                interval: DEFAULT_REQUESTS_AUTO_REFRESH_SECONDS,
            },
            report_auto_refresh: IntervalTask {
                enabled: true,
                interval: DEFAULT_REPORT_AUTO_REFRESH_SECONDS,
            },
        }
    }
}

/// 改一条任务的 patch：只传要改的字段（`None` = 不改）。
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntervalTaskPatch {
    /// 是否启用（`None` = 不改）。
    pub enabled: Option<bool>,
    /// 间隔（`None` = 不改）。
    pub interval: Option<i64>,
}
