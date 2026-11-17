// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 维护（凭证维护 / 模型目录刷新）执行记录与排期。
//!
//! ── 分工 ────────────────────────────────────────────────────
//! `scheduled_tasks` 只决定「何时做」；本模块负责「做一次并留下完整记录」：
//!   · 凭证维护：逐账号 `account_session::refresh::refresh_one`，每条账号一条
//!     明细（成功 / 跳过 / 失败，失败可带 `invalid` 标记）；
//!   · 模型目录刷新：调用 `scheduled_tasks::model_refresh_work`（models.dev +
//!     渠道清单 + 目录对账），元数据结果记在 `run.metadata` 一条。
//!
//! 排期状态仍委托 `scheduled_tasks::task_state`（租约 / 所有者 / 心跳 /
//! 失败退避）；执行记录落在 [`storage`]（FileStore KV，有界保留）。
//!
//! ── 手动执行与重试 ──────────────────────────────────────────
//! `/api/maintenance/run/{task}` 与自动任务走同一 `execute`；手动执行
//! **越过失败冷却**（Bypass）但不越过在途租约与最短请求间隔。重试按记录里
//! 的失败项点名原账号（`forced`），账号被删/禁用时不偷偷换号，如实记 skipped。

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
    matches!(task_id, CREDENTIAL | MODEL)
}

pub fn summary(detail: &Detail) -> String {
    let run = &detail.run;
    let unit = if run.task_id == CREDENTIAL {
        "个账号"
    } else {
        "项"
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
    guard: crate::scheduled_tasks::task_state::RunGuard,
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
    let guard = match crate::scheduled_tasks::task_state::claim(
        &request.task_id,
        interval,
        manual,
        backoff,
        1_000,
    )? {
        crate::scheduled_tasks::task_state::Claim::Acquired(guard) => guard,
        crate::scheduled_tasks::task_state::Claim::Deferred(state_state) => {
            return Err(state_state.waiting_message())
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

async fn complete(
    state: &AppState,
    request: Request,
    prepared: Prepared,
) -> Result<Detail, String> {
    complete_prepared(state, request, prepared).await
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
    let sessions = state.account_sessions.all();
    let snapshots = types::accounts(&sessions);
    let mut items: Vec<Item> = Vec::new();
    let mut metadata: Option<Item> = None;
    let mut hard_failed = 0usize;
    let mut refreshed = 0usize;

    if request.task_id == CREDENTIAL {
        let targets = select_accounts(&sessions, &request);
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
        let (work_summary, ok, count) = crate::scheduled_tasks::model_refresh_work(state).await;
        metadata = Some(types::item(
            &json!({
                "provider": "models.dev",
                "providerLabel": "models.dev",
                "status": if ok { "refreshed" } else { "failed" },
                "message": if ok { Value::Null } else { json!(work_summary) },
                "count": count,
            }),
            0,
            &HashMap::new(),
        ));
        if ok {
            refreshed = 1;
        } else {
            hard_failed = 1;
        }
        tracing::info!("[Maintenance] 模型目录更新：{work_summary}");
    }

    let success = if request.task_id == CREDENTIAL {
        // 与上游意图一致：部分成功不退避；全失败（不含 invalid）才退避。
        hard_failed == 0 || refreshed > 0
    } else {
        metadata
            .as_ref()
            .is_none_or(|item| item.status == "refreshed")
            || !items.iter().any(|item| item.status == "failed")
    };
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
                refresh::needs_refresh(session, now)
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

    #[test]
    fn task_constants_match_scheduled_tasks() {
        assert_eq!(CREDENTIAL, "credentialMaintenance");
        assert_eq!(MODEL, "modelRefresh");
        assert!(is_maintenance_task(CREDENTIAL));
        assert!(is_maintenance_task(MODEL));
        assert!(!is_maintenance_task("updateCheck"));
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
        let targets = select_accounts(&sessions, &request);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].id, "a");
        assert!(targets[0].session.is_some());
        // 点名 + 不存在的账号
        let mut request = Request::full(CREDENTIAL, true);
        request.account_ids = Some(vec!["a".into(), "gone".into()]);
        let targets = select_accounts(&sessions, &request);
        assert_eq!(targets.len(), 2);
        assert!(targets
            .iter()
            .any(|target| target.session.is_none() && target.id == "gone"));
        // 强制名单即使不临期也选
        let mut request = Request::full(CREDENTIAL, true);
        request.forced_account_ids = vec!["b".into()];
        let targets = select_accounts(&sessions, &request);
        assert!(targets.iter().any(|target| target.id == "b"));
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
