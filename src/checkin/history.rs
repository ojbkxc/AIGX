// Ported from Agent2API v3.5.0-beta.3 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 签到轮次历史台账 —— 签到中心「最近签到记录」时间线的数据源（对齐 A2A
//! `core/checkin_history.rs`）。
//!
//! ## 为什么需要它
//!
//! 定时条目只有 `last_result` 一条（被下一轮覆盖），账号页只能看到「最后一次」
//! 的结果；签到中心的时间线要回看多轮（定时 / 手动 / 启动补签），所以把每轮
//! 批量签到的汇总追加进 KV 的 `checkin:history`。
//!
//! ## 只记批量轮次，单账号签到不记
//!
//! 时间线记录的是「一轮」的成败总览；单账号签到是账号页 / 明细行上的点操作，
//! 它的效果已经通过 `dailyCheckin` 反映在每日签到的账号状态里，再进时间线只会
//! 把「今天 09:12 签了浣熊-C」这种碎行混进轮次记录里。记账点在
//! `scheduler::run_targets_with_reason`（批量 / 定时 / 补签 / 重试轮共用），
//! 单账号入口 `handle_checkin_run` 不走那里。
//!
//! ## 容量
//!
//! 保留最近 [`MAX_ENTRIES`]（20）条：一天最多定时 1 轮 + 手动若干轮，20 条
//! 足够回看两三天；再大只会把 KV 撑大，没有查询价值。
//!
//! ## 写失败不报错
//!
//! 记账点在签到响应路径上，上游积分已经领到 —— 台账缺一条不该让那次签到
//! 报错（与 `record_with_reason` 的取舍同构）。

use serde_json::{json, Value};

use super::store::CheckinStore;

/// KV 里承载签到历史的键
const HISTORY_KEY: &str = "checkin:history";
/// 台账保留条数（新条目在前，超出裁掉最老的）
pub const MAX_ENTRIES: usize = 20;

/// 读台账（新在前）。非数组 / 缺失都给空表 —— 与 `auto_checkin` 的兜底
/// 口径一致：旧库里没有这个字段，读出来必须是合法默认值。
pub fn list(store: &CheckinStore) -> Vec<Value> {
    store
        .file_store()
        .get::<Vec<Value>>(HISTORY_KEY)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 追加一轮签到汇总。
///
/// `failures` 是失败明细（`名字（原因）` 的前 5 条由这里截断）——
/// 与 `lastResult.failed` 同一份解释：签到中心时间线与定时任务卡看到的
/// 是同一句话。
pub fn record(
    store: &CheckinStore,
    reason: &str,
    succeeded: usize,
    active: usize,
    total: usize,
    skipped: usize,
    failures: &[String],
) {
    let entry = json!({
        "at": chrono::Utc::now().timestamp_millis(),
        // 日期键按签到执行时区（与调度器「今天」口径一致）
        "date": super::scheduler::today(),
        "reason": reason,
        "succeeded": succeeded,
        // 活跃保活数（WorkBuddy 国际版）：与 succeeded 分开统计 —— 保活的
        // 成功不落 checkinAt，台账里也要单独一列
        "active": active,
        "total": total,
        "skipped": skipped,
        "failed": failures.iter().take(5).cloned().collect::<Vec<_>>(),
        "failedCount": failures.len(),
    });
    let mut entries = list(store);
    entries.insert(0, entry);
    entries.truncate(MAX_ENTRIES);
    if let Err(error) = store.file_store().put(HISTORY_KEY, &entries) {
        // 台账写不进去不影响这次签到本身的结果（积分已经在上游领到了）
        tracing::warn!("[Checkin] 签到历史台账写入失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn temp_store() -> Arc<crate::storage::FileStore> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-history-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(crate::storage::FileStore::new(dir))
    }

    #[test]
    fn record_prepends_and_caps_entries() {
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        assert!(list(&store).is_empty());
        record(&store, "第一轮", 2, 0, 3, 0, &["A（失败）".to_string()]);
        record(&store, "第二轮", 1, 1, 2, 0, &[]);
        let entries = list(&store);
        assert_eq!(entries.len(), 2);
        // 新条目在前
        assert_eq!(entries[0]["reason"], json!("第二轮"));
        assert_eq!(entries[0]["active"], json!(1));
        assert_eq!(entries[1]["failedCount"], json!(1));
        // 超出上限裁掉最老的
        for index in 0..MAX_ENTRIES + 5 {
            record(&store, &format!("轮次 {index}"), 0, 0, 0, 0, &[]);
        }
        let entries = list(&store);
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(
            entries[0]["reason"],
            json!(format!("轮次 {}", MAX_ENTRIES + 4))
        );
    }
}
