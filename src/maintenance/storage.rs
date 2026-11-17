// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 维护记录存储（AIGX 适配：FileStore KV，而不是上游的 SQLite 表）。
//!
//! ── 为什么不用两张新表 ──────────────────────────────────────
//! AIGX 的轻量 KV（`FileStore`）已承载告警规则/任务状态/工具服务配置；
//! 维护记录是**低频写入 + 有界保留**（30 天 / 2000 条），形状简单，用
//! `maintenance:runs`（整份轮次索引）+ `maintenance:items:{run_id}`（逐轮
//! 明细）两个键即可，不引入 schema 版本与迁移。
//!
//! ── 并发与心跳 ──────────────────────────────────────────────
//! 「读改写整份索引」由进程内一把 `parking_lot::Mutex` 串行化（单实例语义，
//! 与 `scheduled_tasks::task_state` 同口径）。Recorder 每 30 秒续一次
//! 心跳（`maintenance:heartbeats` 里 run_id → 最后心跳毫秒）；读取路径把
//! 「running 且心跳超过 180 秒」的轮次标为 interrupted —— 进程被杀后不会
//! 永远显示「执行中」。
//!
//! ── 与上游的差异 ────────────────────────────────────────────
//! 上游用数据库事务保证「明细写入 + 摘要更新」原子；FileStore 无事务，
//! 顺序是「先写明细，再更新摘要」。若在两步之间崩溃，恢复逻辑会把该轮标为
//! interrupted（error 文案提示核对账号状态），不会留下「完成但缺明细」的假象。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use crate::storage::FileStore;

use super::types::{self, Detail, Item, Query, Run};

pub const RETENTION_DAYS: i64 = 30;
pub const MAX_RECORDS: usize = 2000;
const RUNS_KEY: &str = "maintenance:runs";
const HEARTBEATS_KEY: &str = "maintenance:heartbeats";
const ITEMS_PREFIX: &str = "maintenance:items:";
/// 心跳超过该时长（毫秒）的 running 轮次视为中断（与上游 180s 对齐）。
const INTERRUPT_AFTER_MS: i64 = 180_000;
const HEARTBEAT_MS: u64 = 30_000;

static STORAGE_LOCK: Mutex<()> = Mutex::new(());

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn items_key(run_id: &str) -> String {
    format!("{ITEMS_PREFIX}{run_id}")
}

fn read_runs(store: &FileStore) -> Result<Vec<Run>, String> {
    let runs: Option<Vec<Run>> = store
        .get(RUNS_KEY)
        .map_err(|error| format!("读取维护记录失败：{error}"))?;
    Ok(runs.unwrap_or_default())
}

fn write_runs(store: &FileStore, runs: &[Run]) -> Result<(), String> {
    store
        .put(RUNS_KEY, &runs)
        .map_err(|error| format!("保存维护记录失败：{error}"))
}

/// 在锁内读改写整份轮次索引。
fn change_runs<T>(store: &FileStore, mutate: impl FnOnce(&mut Vec<Run>) -> T) -> Result<T, String> {
    let _guard = STORAGE_LOCK.lock();
    let mut runs = read_runs(store)?;
    let result = mutate(&mut runs);
    write_runs(store, &runs)?;
    Ok(result)
}

fn read_heartbeats(store: &FileStore) -> Result<HashMap<String, i64>, String> {
    let map: Option<HashMap<String, i64>> = store
        .get(HEARTBEATS_KEY)
        .map_err(|error| format!("读取维护心跳失败：{error}"))?;
    Ok(map.unwrap_or_default())
}

fn write_heartbeats(store: &FileStore, map: &HashMap<String, i64>) -> Result<(), String> {
    store
        .put(HEARTBEATS_KEY, &map)
        .map_err(|error| format!("保存维护心跳失败：{error}"))
}

/// 在锁内更新单轮心跳（返回 false = 轮次已不在 running，心跳任务应退出）。
fn touch_heartbeat(store: &FileStore, run_id: &str) -> Result<bool, String> {
    let _guard = STORAGE_LOCK.lock();
    let runs = read_runs(store)?;
    let Some(run) = runs.iter().find(|run| run.id == run_id) else {
        return Ok(false);
    };
    if run.status != "running" {
        return Ok(false);
    }
    let mut heartbeats = read_heartbeats(store)?;
    heartbeats.insert(run_id.to_string(), now_ms());
    write_heartbeats(store, &heartbeats)?;
    Ok(true)
}

fn clear_heartbeat(store: &FileStore, run_id: &str) {
    let _guard = STORAGE_LOCK.lock();
    if let Ok(mut heartbeats) = read_heartbeats(store) {
        if heartbeats.remove(run_id).is_some() {
            let _ = write_heartbeats(store, &heartbeats);
        }
    }
}

/// 把心跳超时的 running 轮次标为 interrupted（读取路径调用）。
fn recover_interrupted(store: &FileStore) -> Result<(), String> {
    let cutoff = now_ms().saturating_sub(INTERRUPT_AFTER_MS);
    let stale: Vec<String> = {
        let _guard = STORAGE_LOCK.lock();
        let runs = read_runs(store)?;
        let heartbeats = read_heartbeats(store)?;
        let mut stale = Vec::new();
        for run in &runs {
            if run.status != "running" {
                continue;
            }
            let last = heartbeats.get(&run.id).copied().unwrap_or(run.started_at);
            if last < cutoff {
                stale.push(run.id.clone());
            }
        }
        if !stale.is_empty() {
            let mut heartbeats = heartbeats;
            for id in &stale {
                heartbeats.remove(id);
            }
            write_heartbeats(store, &heartbeats)?;
        }
        stale
    };
    if stale.is_empty() {
        return Ok(());
    }
    change_runs(store, |runs| {
        for run in runs.iter_mut() {
            if stale.contains(&run.id) {
                run.status = "interrupted".to_string();
                run.finished_at = Some(run.started_at.max(cutoff));
                run.error = Some("上次执行中断，没有完整结果；请核对账号状态".to_string());
            }
        }
    })
}

fn prune(runs: &mut Vec<Run>) {
    let cutoff = now_ms().saturating_sub(RETENTION_DAYS * 86_400_000);
    runs.retain(|run| run.status == "running" || run.started_at >= cutoff);
    // 新→旧排序后截断，保证「最近 2000 轮」语义。
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    runs.truncate(MAX_RECORDS);
}

pub struct Recorder {
    store: Arc<FileStore>,
    pub run: Run,
    clock: Instant,
    heartbeat: tokio::task::JoinHandle<()>,
    failure: Option<String>,
    finished: bool,
}

impl Recorder {
    pub fn start(
        store: Arc<FileStore>,
        task_id: &str,
        source: &str,
        retry_of: Option<&str>,
    ) -> Result<Self, String> {
        let now = now_ms();
        let mut random = [0_u8; 12];
        getrandom::getrandom(&mut random).map_err(|error| format!("无法创建维护记录：{error}"))?;
        let id = format!(
            "{now}-{}",
            random
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let run = Run {
            id,
            task_id: task_id.to_string(),
            source: source.to_string(),
            retry_of: retry_of.map(str::to_string),
            started_at: now,
            finished_at: None,
            duration_ms: None,
            status: "running".to_string(),
            refreshed: 0,
            skipped: 0,
            failed: 0,
            total: 0,
            error: None,
            metadata: None,
        };
        change_runs(&store, |runs| {
            runs.push(run.clone());
            prune(runs);
        })?;
        {
            let _guard = STORAGE_LOCK.lock();
            let mut heartbeats = read_heartbeats(&store)?;
            heartbeats.insert(run.id.clone(), now);
            write_heartbeats(&store, &heartbeats)?;
        }
        let run_id = run.id.clone();
        let heartbeat_store = store.clone();
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(HEARTBEAT_MS)).await;
                if !matches!(touch_heartbeat(&heartbeat_store, &run_id), Ok(true)) {
                    break;
                }
            }
        });
        Ok(Self {
            store,
            run,
            clock: Instant::now(),
            heartbeat,
            failure: None,
            finished: false,
        })
    }

    pub fn finish(mut self, results: Vec<Item>, metadata: Option<Item>) -> Result<Detail, String> {
        self.failure =
            Some("维护已执行，但完整结果未能保存；请先核对账号状态，不要反复执行".to_string());
        let (refreshed, skipped, failed) = types::summary(&results);
        self.run.finished_at = Some(now_ms());
        self.run.duration_ms = Some(self.clock.elapsed().as_millis().min(i64::MAX as u128) as i64);
        self.run.status = types::status(refreshed, failed, metadata.as_ref()).to_string();
        self.run.refreshed = refreshed;
        self.run.skipped = skipped;
        self.run.failed = failed;
        self.run.total = results.len();
        self.run.metadata = metadata;

        // 明细先落盘，再更新摘要（见模块头的差异说明）。
        let items_write = self
            .store
            .put(&items_key(&self.run.id), &results)
            .map_err(|error| format!("维护明细保存失败：{error}"));
        if let Err(error) = items_write {
            let message = types::safe_message(&format!(
                "维护已执行，但明细保存失败：{error}；请勿因记录失败反复执行"
            ));
            self.failure = Some(message.clone());
            return Err(message);
        }
        let run = self.run.clone();
        let update = change_runs(&self.store, |runs| {
            // 先取位置再改：不要在 `if let Some(existing) = iter_mut().find()`
            // 的 else 分支里再 `push` —— 借用会延伸进两个分支（E0499）。
            match runs.iter().position(|item| item.id == run.id) {
                Some(index) => runs[index] = run.clone(),
                None => {
                    runs.push(run.clone());
                    prune(runs);
                }
            }
        });
        if let Err(error) = update {
            self.failure = Some(types::safe_message(&error));
            return Err(error);
        }
        clear_heartbeat(&self.store, &self.run.id);
        self.heartbeat.abort();
        self.finished = true;
        Ok(Detail {
            run: self.run.clone(),
            results,
        })
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.heartbeat.abort();
        if self.finished {
            return;
        }
        let message = self
            .failure
            .clone()
            .unwrap_or_else(|| "执行中断，结果可能已部分生效，请先核对账号状态".to_string());
        let run_id = self.run.id.clone();
        let _ = change_runs(&self.store, |runs| {
            if let Some(run) = runs.iter_mut().find(|run| run.id == run_id) {
                if run.status == "running" {
                    run.status = "interrupted".to_string();
                    run.finished_at = Some(now_ms());
                    run.error = Some(message);
                }
            }
        });
        clear_heartbeat(&self.store, &run_id);
    }
}

/// 列表查询（含过滤 / 分页 / 关键词）。
pub fn list(store: &FileStore, query: &Query) -> Result<Value, String> {
    recover_interrupted(store)?;
    let limit = if query.limit == 0 {
        20
    } else {
        query.limit.min(100)
    };
    let offset = query.offset;
    let keyword = query
        .keyword
        .trim()
        .to_lowercase()
        .chars()
        .take(200)
        .collect::<String>();
    let mut runs = read_runs(store)?;
    runs.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
    let filtered: Vec<Run> = runs
        .into_iter()
        .filter(|run| run_matches(store, run, query, &keyword))
        .collect();
    let total = filtered.len();
    let items: Vec<Run> = filtered.into_iter().skip(offset).take(limit).collect();
    Ok(json!({ "items": items, "total": total, "offset": offset, "limit": limit }))
}

fn run_matches(store: &FileStore, run: &Run, query: &Query, keyword: &str) -> bool {
    if !query.task_id.is_empty() && run.task_id != query.task_id {
        return false;
    }
    match query.source.as_str() {
        "" => {}
        "automatic" => {
            if !matches!(run.source.as_str(), "automatic" | "passive") {
                return false;
            }
        }
        "manual" => {
            if run.source != "manual" {
                return false;
            }
        }
        _ => return false,
    }
    match query.status.as_str() {
        "" => {}
        "failed" => {
            if !matches!(run.status.as_str(), "partial" | "failed" | "interrupted") {
                return false;
            }
        }
        "success" => {
            if !matches!(run.status.as_str(), "completed" | "skipped") {
                return false;
            }
        }
        _ => return false,
    }
    if keyword.is_empty() {
        return true;
    }
    if run.task_id.to_lowercase().contains(keyword)
        || run.id.to_lowercase().contains(keyword)
        || run
            .error
            .as_deref()
            .is_some_and(|text| text.to_lowercase().contains(keyword))
    {
        return true;
    }
    read_items(store, &run.id).is_ok_and(|items| {
        items.iter().any(|item| {
            item.provider.to_lowercase().contains(keyword)
                || item
                    .account_name
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(keyword))
                || item
                    .account_id
                    .as_deref()
                    .is_some_and(|id| id.to_lowercase().contains(keyword))
                || item
                    .message
                    .as_deref()
                    .is_some_and(|message| message.to_lowercase().contains(keyword))
        })
    })
}

pub fn latest(store: &FileStore) -> Result<Value, String> {
    let runs = read_runs(store)?;
    let mut latest = serde_json::Map::new();
    for task_id in [super::CREDENTIAL, super::MODEL] {
        if let Some(run) = runs
            .iter()
            .filter(|run| run.task_id == task_id)
            .max_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)))
        {
            latest.insert(task_id.to_string(), json!(run));
        }
    }
    Ok(Value::Object(latest))
}

fn read_items(store: &FileStore, run_id: &str) -> Result<Vec<Item>, String> {
    let items: Option<Vec<Item>> = store
        .get(&items_key(run_id))
        .map_err(|error| format!("维护明细读取失败：{error}"))?;
    Ok(items.unwrap_or_default())
}

pub fn detail(store: &FileStore, id: &str) -> Result<Detail, String> {
    recover_interrupted(store)?;
    let runs = read_runs(store)?;
    let run = runs
        .into_iter()
        .find(|run| run.id == id)
        .ok_or_else(|| "维护记录不存在或已清理".to_string())?;
    let results = read_items(store, id)?;
    Ok(Detail { run, results })
}

/// 删除一轮记录（管理面删除能力的最小实现；上游在记录页提供同样的入口）。
pub fn remove(store: &FileStore, id: &str) -> Result<(), String> {
    change_runs(store, |runs| {
        runs.retain(|run| run.id != id);
    })?;
    store
        .delete(&items_key(id))
        .map_err(|error| format!("维护明细删除失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maintenance::types::Item;
    use std::sync::Arc;

    fn temp_store() -> (Arc<FileStore>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        (Arc::new(FileStore::new(dir.path().to_path_buf())), dir)
    }

    fn item(status: &str) -> Item {
        Item {
            id: "0".into(),
            provider: "workbuddy".into(),
            provider_label: "WorkBuddy".into(),
            account_id: Some("acc-1".into()),
            account_name: Some("Acc One".into()),
            status: status.into(),
            message: None,
            count: None,
            refreshed_at: None,
            expires_before: None,
            expires_after: None,
            credential_changed: Some(true),
            duration_ms: Some(12),
            fixed: None,
            retry_at: None,
            forced: false,
            invalid: false,
        }
    }

    #[tokio::test]
    async fn recorder_persists_detail_and_summary() {
        let (store, _dir) = temp_store();
        let recorder =
            Recorder::start(store.clone(), "credentialMaintenance", "manual", None).unwrap();
        let run_id = recorder.run.id.clone();
        assert_eq!(list(&store, &Query::default()).unwrap()["total"], 1);
        let finished = recorder
            .finish(vec![item("refreshed"), item("failed")], None)
            .unwrap();
        assert_eq!(finished.run.status, "partial");
        assert_eq!(finished.run.refreshed, 1);
        assert_eq!(finished.run.failed, 1);
        let stored = detail(&store, &run_id).unwrap();
        assert_eq!(stored.results.len(), 2);
        assert_eq!(stored.run.id, run_id);
        assert_eq!(
            latest(&store).unwrap()["credentialMaintenance"]["id"],
            run_id
        );
    }

    #[tokio::test]
    async fn running_round_with_stale_heartbeat_is_interrupted() {
        let (store, _dir) = temp_store();
        let recorder = Recorder::start(store.clone(), "modelRefresh", "manual", None).unwrap();
        let run_id = recorder.run.id.clone();
        // 停掉心跳并手工把心跳拨回 10 分钟前，再模拟「进程消失」（跳过 Drop）
        recorder.heartbeat.abort();
        {
            let mut heartbeats = read_heartbeats(&store).unwrap();
            heartbeats.insert(run_id.clone(), now_ms() - 600_000);
            write_heartbeats(&store, &heartbeats).unwrap();
        }
        std::mem::forget(recorder);
        let stored = detail(&store, &run_id).unwrap();
        assert_eq!(stored.run.status, "interrupted");
        assert!(stored.run.error.unwrap().contains("中断"));
    }

    #[tokio::test]
    async fn list_filters_and_pagination() {
        let (store, _dir) = temp_store();
        for index in 0..3 {
            let recorder = Recorder::start(
                store.clone(),
                if index == 0 {
                    "credentialMaintenance"
                } else {
                    "modelRefresh"
                },
                if index == 0 { "automatic" } else { "manual" },
                None,
            )
            .unwrap();
            recorder
                .finish(
                    vec![item(if index == 2 { "failed" } else { "refreshed" })],
                    None,
                )
                .unwrap();
        }
        let all = list(&store, &Query::default()).unwrap();
        assert_eq!(all["total"], 3);
        assert_eq!(all["items"].as_array().unwrap().len(), 3);
        // 任务过滤
        let credential = list(
            &store,
            &Query {
                task_id: "credentialMaintenance".into(),
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(credential["total"], 1);
        // 状态过滤：failed（partial）有 1 条
        let failed = list(
            &store,
            &Query {
                status: "failed".into(),
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(failed["total"], 1);
        // 关键词命中明细账号名
        let keyword = list(
            &store,
            &Query {
                keyword: "acc one".into(),
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(keyword["total"], 3);
        // 分页
        let page = list(
            &store,
            &Query {
                limit: 1,
                offset: 1,
                ..Query::default()
            },
        )
        .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["total"], 3);
    }

    #[tokio::test]
    async fn discard_marks_interrupted_immediately() {
        let (store, _dir) = temp_store();
        let recorder =
            Recorder::start(store.clone(), "credentialMaintenance", "manual", None).unwrap();
        let run_id = recorder.run.id.clone();
        drop(recorder);
        let stored = detail(&store, &run_id).unwrap();
        assert_eq!(stored.run.status, "interrupted");
    }
}
