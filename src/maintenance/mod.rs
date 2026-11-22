// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 维护（凭证维护 / 模型目录刷新）执行记录与排期。
//!
//! ── 分工 ────────────────────────────────────────────────────
//! `scheduled_tasks` 只决定「何时做」；本模块负责「做一次并留下完整记录」：
//!   · 凭证维护：逐账号 `account_session::refresh::refresh_one`，每条账号一条
//!     明细（成功 / 跳过 / 失败，失败可带 `invalid` 标记）；
//!   · 模型目录刷新：调用 `scheduled_tasks::model_refresh_work`（models.dev +
//!     渠道清单 + 目录对账），逐启用渠道一条明细（成功 / 跳过 / 失败 + 条数），
//!     models.dev 汇总另记在 `run.metadata` 一条。
//!
//! 排期状态仍委托 `scheduled_tasks::task_state`（租约 / 所有者 / 心跳 /
//! 失败退避）；执行记录落在 [`storage`]（FileStore KV，有界保留）。
//!
//! ── 手动执行与重试 ──────────────────────────────────────────
//! `/api/maintenance/run/{task}` 与自动任务走同一 `execute`；手动执行
//! **越过失败冷却**（Bypass）但不越过在途租约与最短请求间隔。重试按记录里
//! 的失败项点名原账号（`forced`），账号被删/禁用时不偷偷换号，如实记 skipped。
//!
//! ── 手动单账号刷新为何也走这里 ──────────────────────────────
//! `/api/account-sessions/:id/refresh` 的刷新效果与自动维护同源（都调
//! `refresh_one`），记录也应同源 —— 否则「手动刷新过什么」在维护记录里永远
//! 看不到。见 [`refresh_single_recorded`]：只补一条 `source=manual` 明细，
//! 记录失败**不影响**刷新结果（原响应体与错误语义一概不变）。

pub mod storage;
pub mod types;

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::account_session::refresh::{self, RefreshOutcome};
use crate::api::openai::AppState;
use crate::storage::FileStore;

use self::types::{Detail, Item};

pub const CREDENTIAL: &str = crate::scheduled_tasks::TASK_CREDENTIAL_MAINTENANCE;
pub const MODEL: &str = crate::scheduled_tasks::TASK_MODEL_REFRESH;
pub const UPDATE_CHECK: &str = crate::scheduled_tasks::TASK_UPDATE_CHECK;
/// 社区 / Magpie 插件目录检查（刷新目录 + 逐包更新检查）。
pub const PLUGIN_CATALOG: &str = crate::scheduled_tasks::TASK_PLUGIN_CATALOG;

/// 所有维护任务 id（`storage::latest()` / 维护总览遍历用）。
pub const ALL_TASKS: [&str; 4] = [CREDENTIAL, MODEL, UPDATE_CHECK, PLUGIN_CATALOG];

/// 维护任务运行时句柄（执行记录/排期状态存储）。
pub struct Maintenance {
    /// 底层 KV 存储（维护记录落盘用）。
    pub store: Arc<FileStore>,
}

impl Maintenance {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }
}

/// 启动期初始化（校验记录存储可读；无残留任务）。
pub fn init(store: Arc<FileStore>) {
    let count = storage::list(&store, &types::Query::default())
        .ok()
        .and_then(|value| value.get("total").and_then(Value::as_u64))
        .unwrap_or(0);
    tracing::debug!("[Maintenance] init：{count} 条历史记录");
}

/// 维护管理面路由（`/api/maintenance*` 由 `api::admin::maintenance::router()` 挂载）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
}

/// 一次维护请求（自动 / 手动 / 重试共用）。
pub struct Request {
    pub task_id: String,
    /// `automatic` / `manual`
    pub source: &'static str,
    /// 只维护点名的账号（None = 全部临期账号）
    pub account_ids: Option<Vec<String>>,
    /// 即使未临期也强制刷新（重试路径）
    pub forced_account_ids: Vec<String>,
    /// 模型任务是否刷新元数据（models.dev）
    pub metadata: bool,
    pub retry_of: Option<String>,
}

impl Request {
    pub fn full(task_id: &str, manual: bool) -> Self {
        Self {
            task_id: task_id.to_string(),
            source: if manual { "manual" } else { "automatic" },
            account_ids: None,
            forced_account_ids: Vec::new(),
            metadata: task_id == MODEL,
            retry_of: None,
        }
    }
}

pub fn is_maintenance_task(task_id: &str) -> bool {
    matches!(task_id, CREDENTIAL | MODEL | UPDATE_CHECK | PLUGIN_CATALOG)
}

pub fn summary(detail: &Detail) -> String {
    let run = &detail.run;
    // 软件版本检查：上游把「发现新版本 / 已是最新」写在明细消息里，直接回传
    // 比再拼一段计数更贴切。
    if run.task_id == UPDATE_CHECK {
        if let Some(message) = detail
            .results
            .first()
            .and_then(|item| item.message.as_deref())
        {
            return message.to_string();
        }
    }
    let unit = match run.task_id.as_str() {
        CREDENTIAL => "个账号",
        PLUGIN_CATALOG => "个插件",
        _ => "项",
    };
    let mut text = format!(
        "成功 {} {unit}，跳过 {}，失败 {}",
        run.refreshed, run.skipped, run.failed
    );
    if let Some(metadata) = &run.metadata {
        match metadata.status.as_str() {
            "refreshed" => text.push_str(&format!(
                "；models.dev 已更新 {} 条",
                metadata.count.unwrap_or(0)
            )),
            "failed" => text.push_str(&format!(
                "；models.dev 更新失败：{}",
                metadata.message.as_deref().unwrap_or("未知原因")
            )),
            _ => (),
        }
    }
    text
}

struct Prepared {
    /// 维护租约；`None` = 该任务的排期由别的模块持有租约（软件版本检查由
    /// 更新管理器自己 claim 同一个 `updateCheck` 排期键，这里不再重复占位）。
    guard: Option<crate::scheduled_tasks::task_state::RunGuard>,
    recorder: storage::Recorder,
    interval: i64,
}

async fn claim_and_record(state: &AppState, request: &Request) -> Result<Prepared, String> {
    if !is_maintenance_task(&request.task_id) {
        return Err("未知的维护任务".to_string());
    }
    let settings = state.config_manager.get().await.scheduled_tasks;
    let interval = crate::scheduled_tasks::task_interval_ms(&settings, &request.task_id);
    let manual = request.source == "manual";
    let backoff = if manual {
        crate::scheduled_tasks::task_state::ManualBackoff::Bypass
    } else {
        crate::scheduled_tasks::task_state::ManualBackoff::Respect
    };
    // 软件版本检查的排期键由更新管理器持有（手动/定时都走它的 claim）；
    // 这里不重复占位，否则会和更新管理器自己那一轮撞成「已在跑」。
    let guard = if request.task_id == UPDATE_CHECK {
        None
    } else {
        match crate::scheduled_tasks::task_state::claim(
            &request.task_id,
            interval,
            manual,
            backoff,
            1_000,
        )? {
            crate::scheduled_tasks::task_state::Claim::Acquired(guard) => Some(guard),
            crate::scheduled_tasks::task_state::Claim::Deferred(state_state) => {
                // 手动入口越过的是失败冷却：提示里不该再让用户「几小时后再试」，
                // 真正挡住这一轮的通常只是在途占位或最短请求间隔。
                return Err(if manual {
                    state_state.waiting_message_bypassing()
                } else {
                    state_state.waiting_message()
                });
            }
        }
    };
    let recorder = storage::Recorder::start(
        state.maintenance.store.clone(),
        &request.task_id,
        request.source,
        request.retry_of.as_deref(),
    )?;
    Ok(Prepared {
        guard,
        recorder,
        interval,
    })
}

/// 同步执行一次维护（后台任务循环与手动接口共用）。
pub async fn execute(state: &AppState, request: Request) -> Result<Detail, String> {
    let prepared = claim_and_record(state, &request).await?;
    complete(state, request, prepared).await
}

/// 建好记录立即返回 running 详情，工作在后台上跑（管理面「立即执行 / 重试」用）。
pub async fn launch_async(state: AppState, request: Request) -> Result<Detail, String> {
    let prepared = claim_and_record(&state, &request).await?;
    let started = Detail {
        run: prepared.recorder.run.clone(),
        results: Vec::new(),
    };
    let task_id = request.task_id.clone();
    let background_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = complete_prepared(&background_state, request, prepared).await {
            tracing::warn!("[Maintenance] {task_id}：{error}");
        }
    });
    Ok(started)
}

/// 手动刷新单个账号并落一条 `source=manual` 维护记录。
///
/// 复用 `account_session::refresh::refresh_one` 的全部语义（单飞 / CAS / 失败
/// 不改写凭据）；**原样返回** `RefreshOutcome`，调用方据此维持原来的响应体与
/// 错误码。记录写入失败只记一条 warn，绝不影响刷新结果 —— 用户按下刷新按钮
/// 要的是「凭证刷没刷成功」，不是「记录写没写进去」。
pub async fn refresh_single_recorded(
    state: &AppState,
    session: &crate::account_session::model::AccountSession,
) -> RefreshOutcome {
    let started = std::time::Instant::now();
    let before_expires = session.expires_at.filter(|value| *value > 0);
    let outcome = refresh::refresh_one(&state.account_sessions, &state.http_client, session).await;
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let after_expires = state
        .account_sessions
        .get(&session.id)
        .and_then(|current| current.expires_at)
        .filter(|value| *value > 0);
    if let Err(error) = record_manual_single(
        state.maintenance.store.clone(),
        session,
        &outcome,
        before_expires,
        after_expires,
        duration_ms,
    ) {
        tracing::warn!("[Maintenance] 手动刷新记录写入失败（不影响刷新结果）：{error}");
    }
    outcome
}

/// 把一次手动单账号刷新的结果写成一条轮次（一条明细）。
///
/// 抽成同步辅助函数是为了可单测：调用方（[`refresh_single_recorded`]）需要
/// `AppState` 才能真刷，而「记录形状是否正确」与网络无关。`Stale` 记 skipped
/// 而不是 failed —— 凭据已被别的路径更新，本次没有覆盖任何东西，也没有出错。
fn record_manual_single(
    store: Arc<FileStore>,
    session: &crate::account_session::model::AccountSession,
    outcome: &RefreshOutcome,
    before_expires: Option<i64>,
    after_expires: Option<i64>,
    duration_ms: i64,
) -> Result<Detail, String> {
    let recorder = storage::Recorder::start(store, CREDENTIAL, "manual", None)?;
    let snapshots = types::accounts(std::slice::from_ref(session));
    let (status, message, invalid, credential_changed) = match outcome {
        RefreshOutcome::Refreshed(_) => ("refreshed", None, false, true),
        RefreshOutcome::Stale => (
            "skipped",
            Some("凭据已被其他路径更新，未覆盖".to_string()),
            false,
            false,
        ),
        RefreshOutcome::Invalid(message) => ("failed", Some(message.clone()), true, false),
        RefreshOutcome::Failed(message) => ("failed", Some(message.clone()), false, false),
    };
    let raw = json!({
        "id": &session.id,
        "provider": &session.provider,
        "providerLabel": types::provider_label(&session.provider),
        "accountId": &session.id,
        "accountName": &session.name,
        "status": status,
        "message": message,
        "expiresBefore": before_expires.map(|value| value * 1000),
        "expiresAfter": after_expires.map(|value| value * 1000),
        "credentialChanged": credential_changed,
        "durationMs": duration_ms,
        // 手动按钮语义即「现在真打一次」，与维护重试点名同一口径。
        "forced": true,
        "invalid": invalid,
    });
    recorder.finish(vec![types::item(&raw, 0, &snapshots)], None)
}

async fn complete(
    state: &AppState,
    request: Request,
    prepared: Prepared,
) -> Result<Detail, String> {
    complete_prepared(state, request, prepared).await
}

/// 软件版本检查：动作与排期都委托更新管理器（它自己 claim / finish 同一个
/// `updateCheck` 排期键），这里只把结果写成一条维护记录。
async fn complete_update_check(
    state: &AppState,
    request: Request,
    recorder: storage::Recorder,
) -> Result<Detail, String> {
    let config = state.config_manager.get().await;
    let started = std::time::Instant::now();
    let raw = if !config.update.enabled {
        json!({
            "provider": "update",
            "providerLabel": "软件版本检查",
            "status": "skipped",
            "changeLabel": "已关闭",
            "message": "软件更新开关已关闭，跳过检查",
        })
    } else {
        let outcome = if request.source == "manual" {
            state.update.check(&config).await
        } else {
            state.update.check_scheduled(&config).await
        };
        match outcome {
            Ok(info) => {
                let has_update = info.get("hasUpdate").and_then(Value::as_bool) == Some(true);
                let current = info
                    .get("currentVersion")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let latest = info
                    .get("latestVersion")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if has_update && config.update.auto_apply {
                    if let Err(error) = state.update.start_apply(&config.update, false) {
                        tracing::warn!("[Maintenance] 自动应用失败：{error}");
                    }
                }
                let message = if has_update {
                    format!(
                        "发现新版本 {}（当前 {}）",
                        latest.as_deref().unwrap_or(""),
                        current.as_deref().unwrap_or(crate::update::CURRENT_VERSION)
                    )
                } else {
                    "已完成版本检查，已是最新版本".to_string()
                };
                json!({
                    "provider": "update",
                    "providerLabel": "软件版本检查",
                    "status": if has_update { "refreshed" } else { "skipped" },
                    "currentVersion": current,
                    "latestVersion": latest,
                    "changeLabel": if has_update { "发现新版本" } else { "已是最新" },
                    "message": message,
                })
            }
            Err(error) => json!({
                "provider": "update",
                "providerLabel": "软件版本检查",
                "status": "failed",
                "changeLabel": "检查失败",
                "message": error,
            }),
        }
    };
    let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let mut item = types::item(&raw, 0, &HashMap::new());
    item.duration_ms = Some(duration_ms);
    let detail = recorder.finish(vec![item], None);
    match &detail {
        Ok(detail) => tracing::info!("[Maintenance] {}", summary(detail)),
        Err(error) => tracing::error!("[Maintenance] {error}"),
    }
    detail
}

/// 社区 / Magpie 插件目录检查：强制刷新目录 → 逐包更新检查 → 一条维护记录。
///
/// 「Magpie 专属签名升级」不移植：只做目录刷新与更新提示，绝不自动升级。
async fn complete_plugin_catalog(
    state: &AppState,
    _request: Request,
    guard: crate::scheduled_tasks::task_state::RunGuard,
    recorder: storage::Recorder,
    interval: i64,
) -> Result<Detail, String> {
    let catalog = state.script_host.catalog();
    // 强制刷新（无视 TTL）；维护租约保证同一时刻只有一轮自动检查在跑。
    let refresh_started = std::time::Instant::now();
    catalog.refresh_now(true).await;
    let snapshot = catalog.status().await;
    let refresh_ms = refresh_started.elapsed().as_millis().min(i64::MAX as u128) as i64;
    let total = snapshot.official + snapshot.recommended + snapshot.third_party;
    let catalog_error = snapshot
        .market_error
        .clone()
        .or_else(|| snapshot.github_error.clone());

    let empty = HashMap::new();
    let mut items: Vec<Item> = Vec::new();
    match crate::plugin_host::script::operations::check_update_outcomes(&state.script_host).await {
        Ok(outcomes) => {
            for outcome in outcomes {
                let (status, change_label, message) = if let Some(error) = outcome.error.clone() {
                    ("failed", "检查失败".to_string(), error)
                } else if outcome.available {
                    (
                        "skipped",
                        format!("可更新 {}", outcome.latest),
                        format!("发现新版本 {}（可在插件页更新）", outcome.latest),
                    )
                } else {
                    (
                        "skipped",
                        "已是最新".to_string(),
                        "已经是最新版本".to_string(),
                    )
                };
                let raw = json!({
                    "provider": outcome.id,
                    "providerLabel": outcome.name,
                    "accountName": format!("{} · {}", outcome.current, outcome.source_label),
                    "status": status,
                    "currentVersion": outcome.current,
                    "latestVersion": (!outcome.latest.is_empty()).then_some(outcome.latest),
                    "changeLabel": change_label,
                    "message": message,
                });
                items.push(types::item(&raw, items.len(), &empty));
            }
        }
        Err(error) => {
            let raw = json!({
                "provider": "magpieUpdates",
                "providerLabel": "已安装脚本插件更新检查",
                "accountName": "脚本注册表",
                "status": "failed",
                "changeLabel": "检查失败",
                "message": error,
            });
            items.push(types::item(&raw, items.len(), &empty));
        }
    }

    let metadata = Some(types::item(
        &json!({
            "provider": "pluginCatalog",
            "providerLabel": "社区插件目录",
            "status": if catalog_error.is_some() { "failed" } else { "refreshed" },
            "count": total as u64,
            "changeLabel": if catalog_error.is_some() { "目录刷新失败" } else { "目录已刷新" },
            "message": match &catalog_error {
                Some(error) => format!("目录刷新失败：{error}"),
                None => format!("目录已刷新（共 {total} 个插件）"),
            },
            "durationMs": refresh_ms,
        }),
        0,
        &empty,
    ));

    let (refreshed, _, failed) = types::summary(&items);
    let success = failed == 0 || refreshed > 0;
    let detail_result = recorder.finish(items, metadata);
    let note = detail_result
        .as_ref()
        .map(summary)
        .unwrap_or_else(|error| error.clone());
    let run_value = detail_result
        .as_ref()
        .ok()
        .map(|detail| json!({ "runId": detail.run.id }));
    let schedule = guard.finish(success, note.clone(), run_value, 0, interval);
    if detail_result.is_err() {
        tracing::error!("[Maintenance] {note}");
    } else {
        tracing::info!("[Maintenance] {note}");
    }
    let detail = detail_result?;
    if let Err(error) = schedule {
        return Err(format!("维护结果已保存，但排期更新失败：{error}"));
    }
    Ok(detail)
}

async fn complete_prepared(
    state: &AppState,
    request: Request,
    prepared: Prepared,
) -> Result<Detail, String> {
    let Prepared {
        guard,
        recorder,
        interval,
    } = prepared;
    // 软件版本检查：排期由更新管理器负责（它已 claim / finish 同一个键）。
    if request.task_id == UPDATE_CHECK {
        return complete_update_check(state, request, recorder).await;
    }
    // 社区插件目录检查：刷新目录 + 逐包更新检查，维护租约在本轮内持有。
    if request.task_id == PLUGIN_CATALOG {
        let Some(guard) = guard else {
            return Err("插件检查缺少维护租约".to_string());
        };
        return complete_plugin_catalog(state, request, guard, recorder, interval).await;
    }
    let Some(guard) = guard else {
        return Err("维护任务缺少租约".to_string());
    };
    let sessions = state.account_sessions.all();
    let snapshots = types::accounts(&sessions);
    let mut items: Vec<Item> = Vec::new();
    let mut metadata: Option<Item> = None;
    let mut hard_failed = 0usize;
    let mut refreshed = 0usize;

    if request.task_id == CREDENTIAL {
        // 最小提前量 = max(provider 固定窗口, 维护间隔 + 5 分钟缓冲)：默认 10
        // 分钟间隔要能覆盖「下一轮维护前就会到期」的账号，否则会漏刷。
        let settings = state.config_manager.get().await.scheduled_tasks;
        let lead_secs = refresh::maintenance_lead_secs(settings.credential_maintenance.interval);
        let targets = select_accounts(&sessions, &request, lead_secs);
        for target in targets {
            let started = std::time::Instant::now();
            let before_expires = target.expires_at.filter(|value| *value > 0);
            let forced = request.forced_account_ids.iter().any(|id| id == &target.id);
            let outcome = match &target.session {
                Some(session) => {
                    refresh::refresh_one(&state.account_sessions, &state.http_client, session).await
                }
                None => RefreshOutcome::Failed("账号已不存在，未执行维护".to_string()),
            };
            let duration_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;
            let after_expires = state
                .account_sessions
                .get(&target.id)
                .and_then(|session| session.expires_at)
                .filter(|value| *value > 0);
            let (status, message, invalid, credential_changed) = match &outcome {
                RefreshOutcome::Refreshed(_) => {
                    refreshed += 1;
                    ("refreshed", None, false, true)
                }
                RefreshOutcome::Stale => (
                    "skipped",
                    Some("账号未处于临期窗口，未执行刷新".to_string()),
                    false,
                    false,
                ),
                RefreshOutcome::Invalid(message) => ("failed", Some(message.clone()), true, false),
                RefreshOutcome::Failed(message) => {
                    hard_failed += 1;
                    ("failed", Some(message.clone()), false, false)
                }
            };
            let raw = json!({
                "id": target.id,
                "provider": target.provider,
                "providerLabel": types::provider_label(&target.provider),
                "accountId": target.id,
                "accountName": target.name,
                "status": status,
                "message": message,
                "expiresBefore": before_expires.map(|value| value * 1000),
                "expiresAfter": after_expires.map(|value| value * 1000),
                "credentialChanged": credential_changed,
                "durationMs": duration_ms,
                "forced": forced,
                "invalid": invalid,
            });
            items.push(types::item(&raw, items.len(), &snapshots));
        }
        let note =
            format!("凭证维护：成功 {refreshed} 个，失败 {hard_failed} 个（失效标记见明细）");
        tracing::info!("[Maintenance] {note}");
    } else {
        let work = crate::scheduled_tasks::model_refresh_work(state).await;
        // 逐启用渠道一条明细（成功 / 跳过 / 失败 + 条数）：模型目录刷新此前只留
        // 一条 models.dev 汇总，某家渠道连续 401 时界面上看不出是谁坏了。
        for (index, raw) in work.results.iter().enumerate() {
            items.push(types::item(raw, index, &HashMap::new()));
        }
        metadata = Some(types::item(
            &json!({
                "provider": "models.dev",
                "providerLabel": "models.dev",
                "status": if work.ok { "refreshed" } else { "failed" },
                "message": if work.ok { Value::Null } else { json!(&work.summary) },
                "count": work.count,
            }),
            0,
            &HashMap::new(),
        ));
        // 明细计数 + models.dev 汇总共同决定这一轮成败：任一渠道失败仍是部分
        // 成功（不退避），全失败或元数据失败才退避。
        let (item_refreshed, _, item_failed) = types::summary(&items);
        refreshed = item_refreshed;
        hard_failed = item_failed + if work.ok { 0 } else { 1 };
        tracing::info!("[Maintenance] 模型目录更新：{}", work.summary);
    }

    // 与上游同一口径：部分成功不退避，全失败（不含 invalid）才退避。
    let success = hard_failed == 0 || refreshed > 0;
    let detail_result = recorder.finish(items, metadata);
    let note = match &detail_result {
        Ok(detail) => summary(detail),
        Err(error) => error.clone(),
    };
    let run_value = detail_result
        .as_ref()
        .ok()
        .map(|detail| json!({ "runId": detail.run.id }));
    let schedule = guard.finish(success, note.clone(), run_value, 0, interval);
    if detail_result.is_err() {
        tracing::error!("[Maintenance] {note}");
    } else {
        tracing::info!("[Maintenance] {note}");
    }
    let detail = detail_result?;
    if let Err(error) = schedule {
        return Err(format!("维护结果已保存，但排期更新失败：{error}"));
    }
    Ok(detail)
}

/// 维护目标（临期账号 / 点名账号）。
struct AccountTarget {
    /// `None` = 账号已不存在（点名重试时如实记 skipped）
    session: Option<crate::account_session::model::AccountSession>,
    id: String,
    provider: String,
    name: String,
    expires_at: Option<i64>,
}

fn select_accounts(
    sessions: &[crate::account_session::model::AccountSession],
    request: &Request,
    lead_secs: i64,
) -> Vec<AccountTarget> {
    let now = chrono::Utc::now().timestamp();
    let by_id: HashMap<&str, &crate::account_session::model::AccountSession> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();
    let describe = |session: &crate::account_session::model::AccountSession| AccountTarget {
        session: Some(session.clone()),
        id: session.id.clone(),
        provider: session.provider.clone(),
        name: if session.name.trim().is_empty() {
            session.id.clone()
        } else {
            session.name.clone()
        },
        expires_at: session.expires_at,
    };
    match &request.account_ids {
        Some(ids) => ids
            .iter()
            .map(|id| match by_id.get(id.as_str()) {
                Some(session) => describe(session),
                None => AccountTarget {
                    session: None,
                    id: id.clone(),
                    provider: String::new(),
                    name: id.clone(),
                    expires_at: None,
                },
            })
            .collect(),
        None => sessions
            .iter()
            .filter(|session| {
                refresh::needs_refresh_within(session, now, lead_secs)
                    || request
                        .forced_account_ids
                        .iter()
                        .any(|id| id == &session.id)
            })
            .map(describe)
            .collect(),
    }
}

/// 按上一轮记录重建重试请求（纯函数，便于单测）。
///
/// `item_id` = 明细的序号 id（前端传回）；`None` = 重试全部失败项。
/// `invalid`（refresh_token 已失效）不进重试集合：重试无意义。
fn build_retry_request(previous: &Detail, item_id: Option<&str>) -> Result<Request, String> {
    if previous.run.status == "running" {
        return Err("该次维护尚未结束".to_string());
    }
    let mut request = Request::full(&previous.run.task_id, true);
    request.retry_of = Some(previous.run.id.clone());
    request.metadata = previous
        .run
        .metadata
        .as_ref()
        .is_some_and(|item| item.status == "failed")
        && item_id.is_none();
    let failed: Vec<Item> = previous
        .results
        .iter()
        .filter(|item| {
            item.status == "failed" && !item.invalid && item_id.is_none_or(|id| item.id == id)
        })
        .cloned()
        .collect();
    if failed.is_empty() && !request.metadata {
        return Err("没有可重试的失败项".to_string());
    }
    if request.task_id == CREDENTIAL {
        request.account_ids = Some(
            failed
                .iter()
                .filter_map(|item| item.account_id.clone())
                .collect(),
        );
        request.forced_account_ids = failed
            .iter()
            .filter(|item| item.forced)
            .filter_map(|item| item.account_id.clone())
            .collect();
    }
    Ok(request)
}

/// 异步重试（管理面 handler 调用）。
pub async fn retry_async(
    state: AppState,
    run_id: &str,
    item_id: Option<&str>,
) -> Result<Detail, String> {
    let previous = storage::detail(&state.maintenance.store, run_id)?;
    let request = build_retry_request(&previous, item_id)?;
    // 模型刷新按整轮重跑（AIGX 的渠道级结果不是独立明细项）。
    launch_async(state, request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Arc<FileStore>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        (Arc::new(FileStore::new(dir.path().to_path_buf())), dir)
    }

    fn session(
        id: &str,
        provider: &str,
        expires: Option<i64>,
    ) -> crate::account_session::model::AccountSession {
        let mut session = crate::account_session::model::AccountSession::new("chan", id);
        session.id = id.to_string();
        session.provider = provider.to_string();
        session.name = format!("Name {id}");
        session.expires_at = expires;
        session
    }

    /// 缺口 1：手动单账号刷新必须落一条 `source=manual` 记录（一条明细，
    /// 含 provider / account / status / message / duration）。
    #[tokio::test]
    async fn manual_single_refresh_writes_manual_record() {
        let (store, _dir) = temp_store();
        let target = session("acc-1", "workbuddy", Some(1_700_003_600));
        let detail = record_manual_single(
            store.clone(),
            &target,
            &RefreshOutcome::Refreshed("token 已续期".to_string()),
            Some(1_700_000_000),
            Some(1_700_003_600),
            42,
        )
        .unwrap();
        assert_eq!(detail.run.task_id, CREDENTIAL);
        assert_eq!(detail.run.source, "manual");
        assert_eq!(detail.run.status, "completed");
        assert_eq!(detail.results.len(), 1);
        let item = &detail.results[0];
        assert_eq!(item.account_id.as_deref(), Some("acc-1"));
        assert_eq!(item.provider, "workbuddy");
        assert_eq!(item.status, "refreshed");
        assert_eq!(item.duration_ms, Some(42));
        assert!(item.forced);
        assert!(!item.invalid);
        assert_eq!(item.expires_before, Some(1_700_000_000_000));
        assert_eq!(item.expires_after, Some(1_700_003_600_000));

        let stored = storage::detail(&store, &detail.run.id).unwrap();
        assert_eq!(stored.results.len(), 1);
        // 维护记录按 manual 来源能筛出来（前端「手动」筛选同口径）。
        let manual = storage::list(
            &store,
            &types::Query {
                source: "manual".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(manual["total"], 1);
    }

    /// 失效与普通失败都记 failed，但只有 `Invalid` 带 invalid 标记（重试无意义）。
    #[tokio::test]
    async fn manual_single_refresh_marks_invalid_and_stale() {
        let (store, _dir) = temp_store();
        let target = session("acc-2", "trae", Some(2_000));
        let detail = record_manual_single(
            store.clone(),
            &target,
            &RefreshOutcome::Invalid("refresh token 失效".to_string()),
            None,
            None,
            5,
        )
        .unwrap();
        assert_eq!(detail.run.status, "failed");
        assert!(detail.results[0].invalid);
        assert_eq!(
            detail.results[0].message.as_deref(),
            Some("refresh token 失效")
        );

        let detail = record_manual_single(
            store.clone(),
            &target,
            &RefreshOutcome::Stale,
            None,
            None,
            1,
        )
        .unwrap();
        assert_eq!(detail.run.status, "skipped");
        assert_eq!(detail.results[0].status, "skipped");
    }

    #[test]
    fn task_constants_match_scheduled_tasks() {
        assert_eq!(CREDENTIAL, "credentialMaintenance");
        assert_eq!(MODEL, "modelRefresh");
        assert_eq!(UPDATE_CHECK, "updateCheck");
        assert_eq!(PLUGIN_CATALOG, "pluginCatalog");
        assert_eq!(ALL_TASKS.len(), 4);
        for task in ALL_TASKS {
            assert!(is_maintenance_task(task), "{task} 应属维护任务");
        }
        assert!(!is_maintenance_task("logsAutoRefresh"));
        assert!(!is_maintenance_task("unknown"));
    }

    #[test]
    fn request_full_shapes() {
        let auto = Request::full(CREDENTIAL, false);
        assert_eq!(auto.source, "automatic");
        assert!(!auto.metadata);
        let manual = Request::full(MODEL, true);
        assert_eq!(manual.source, "manual");
        assert!(manual.metadata);
    }

    #[test]
    fn summary_text_includes_metadata_failure() {
        let metadata = types::item(
            &json!({
                "provider": "models.dev",
                "status": "failed",
                "message": "HTTP 503",
            }),
            0,
            &HashMap::new(),
        );
        let detail = Detail {
            run: types::Run {
                id: "r".into(),
                task_id: MODEL.into(),
                source: "manual".into(),
                retry_of: None,
                started_at: 1,
                finished_at: Some(2),
                duration_ms: Some(1),
                status: "failed".into(),
                refreshed: 0,
                skipped: 0,
                failed: 1,
                total: 1,
                error: None,
                metadata: Some(metadata),
            },
            results: Vec::new(),
        };
        let text = summary(&detail);
        assert!(text.contains("失败 1"));
        assert!(text.contains("models.dev 更新失败"));
    }

    #[test]
    fn select_accounts_marks_missing_and_forces() {
        fn session(
            id: &str,
            provider: &str,
            expires: i64,
        ) -> crate::account_session::model::AccountSession {
            serde_json::from_value(json!({
                "id": id,
                "provider": provider,
                "name": format!("Name {id}"),
                "expires_at": expires,
                "refresh_secret": "refresh",
            }))
            .unwrap()
        }
        let now = chrono::Utc::now().timestamp();
        let mut sessions = vec![
            session("a", "workbuddy", now + 100_000),
            session("b", "kiro", now + 100),
        ];
        // 临期只有 a（workbuddy 在 supports_refresh 白名单且 60 秒内）
        sessions[0].expires_at = Some(now + 60);
        let request = Request::full(CREDENTIAL, true);
        let targets = select_accounts(&sessions, &request, 0);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, "a");
        assert!(targets[0].session.is_some());
        // 点名 + 不存在的账号
        let mut request = Request::full(CREDENTIAL, true);
        request.account_ids = Some(vec!["a".into(), "gone".into()]);
        let targets = select_accounts(&sessions, &request, 0);
        assert_eq!(targets.len(), 2);
        assert!(targets
            .iter()
            .any(|target| target.session.is_none() && target.id == "gone"));
        // 强制名单即使不临期也选
        let mut request = Request::full(CREDENTIAL, true);
        request.forced_account_ids = vec!["b".into()];
        let targets = select_accounts(&sessions, &request, 0);
        assert!(targets.iter().any(|target| target.id == "b"));
        // 维护提前量：10 分钟间隔 → lead = 600 + 300 = 900 秒；500 秒后到期的
        // 账号要提前选中（否则下一轮维护前就过期了），不叠加提前量时不在差集里。
        sessions[0].expires_at = Some(now + 500);
        let lead = refresh::maintenance_lead_secs(10);
        assert_eq!(lead, 900);
        let targets = select_accounts(&sessions, &Request::full(CREDENTIAL, true), lead);
        assert!(targets.iter().any(|target| target.id == "a"));
        let targets = select_accounts(&sessions, &Request::full(CREDENTIAL, true), 0);
        assert!(!targets.iter().any(|target| target.id == "a"));
    }
    fn failed_item(id: &str, account: &str, invalid: bool, forced: bool) -> Item {
        let mut item = types::item(
            &json!({
                "provider": "workbuddy",
                "accountId": account,
                "accountName": account,
                "status": "failed",
                "message": "boom",
                "invalid": invalid,
                "forced": forced,
            }),
            0,
            &HashMap::new(),
        );
        item.id = id.to_string();
        item
    }

    fn detail_with(run_status: &str, results: Vec<Item>, metadata: Option<Item>) -> Detail {
        Detail {
            run: types::Run {
                id: "run-1".into(),
                task_id: CREDENTIAL.into(),
                source: "manual".into(),
                retry_of: None,
                started_at: 1,
                finished_at: Some(2),
                duration_ms: Some(1),
                status: run_status.into(),
                refreshed: 0,
                skipped: 0,
                failed: results.len(),
                total: results.len(),
                error: None,
                metadata,
            },
            results,
        }
    }

    #[test]
    fn retry_request_selects_failed_non_invalid_items() {
        let detail = detail_with(
            "partial",
            vec![
                failed_item("0", "acc-1", false, true),
                failed_item("1", "acc-2", true, false), // invalid 不重试
                failed_item("2", "acc-3", false, false),
            ],
            None,
        );
        let request = build_retry_request(&detail, None).unwrap();
        assert_eq!(request.retry_of.as_deref(), Some("run-1"));
        assert_eq!(request.account_ids.unwrap(), vec!["acc-1", "acc-3"]);
        assert_eq!(request.forced_account_ids, vec!["acc-1"]);
        // 单条重试：只点名 itemId=2
        let request = build_retry_request(&detail, Some("2")).unwrap();
        assert_eq!(request.account_ids.unwrap(), vec!["acc-3"]);
        // 只选中 invalid 项 → 无可重试
        assert!(build_retry_request(&detail, Some("1")).is_err());
        // 运行中的轮次拒绝重试
        assert!(build_retry_request(
            &detail_with("running", vec![failed_item("0", "a", false, false)], None),
            None
        )
        .is_err());
    }

    #[test]
    fn retry_request_for_model_uses_metadata() {
        let metadata = types::item(
            &json!({ "provider": "models.dev", "status": "failed", "message": "HTTP 503" }),
            0,
            &HashMap::new(),
        );
        let detail = detail_with("failed", Vec::new(), Some(metadata));
        let mut detail = detail;
        detail.run.task_id = MODEL.into();
        let request = build_retry_request(&detail, None).unwrap();
        assert!(request.metadata);
        assert!(request.account_ids.is_none());
        // 有 itemId 时不重试元数据（且没有失败明细 → 无可重试）
        assert!(build_retry_request(&detail, Some("0")).is_err());
    }
}
