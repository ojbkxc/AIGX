//! 连登管家的**调度骨架**（补签 → 档位兑换 → 抽奖），对齐 Agent2API
//! `core/billing/checkin.rs::spawn_streak_bonus`。
//!
//! ## 状态：上游端点未接入，骨架就绪
//!
//! A2A 的连登管家挂在签到末尾，账号当天签到成功后**异步**跑三件事：漏签日补签
//! → 连登档位兑换 → 抽奖。三件事的执行体在**插件**里（`admin_action
//! "growth.streak.batch"` 自己列本家账号、自己门控、自己错峰），网关只拿回
//! `{done, failed}` 两个计数记日志。
//!
//! AIGX 主仓**没有**对应的上游端点（`growth.streak.*` 一类），所以这里不实现
//! 抓取逻辑 —— 落的是**调度骨架**：能力表、串行闸、后台任务、落盘键、失败隔离
//! 全部就位，[`STREAK_PROVIDERS`] 一旦登记了某家、并把 [`run_account`] 里的
//! 三个空步骤接上端点，整条链自动生效，调用方（`scheduler`）一行都不用改。
//!
//! ## 为什么是后台任务而不是 await
//!
//! 与 A2A 同一结论：三件事每账号 4~6 个上游请求，十几个账号就是几十秒到几分钟
//! —— 签到是手动与定时共用的入口，同步 await 会让「立即全部签到」在管理 API
//! 的单请求超时（60 秒）上直接报错，而后端其实已经跑完。
//!
//! ## 失败隔离
//!
//! 这三件事在上游都是幂等的**附加收益**：任何一步出错只进日志，绝不把一次成功
//! 的签到报成失败（与 `scheduler::record` 的取舍同构）。
//!
//! ## 零开销
//!
//! [`spawn_after_batch`] 先判能力表：没有任何家支持时**直接返回、不 spawn**
//! （连一次 `tokio::spawn` 都不做）。上游端点未接入期间，这条调用等于一次
//! `Vec::iter().any()` 的空判断。
//!
//! ## 落盘键为什么不是 `checkin:streak:{id}`
//!
//! A2A 把连登天数与管家结果分开存；AIGX 的 `checkin:streak:{account_id}` 已被
//! [`crate::checkin::store::CheckinStore::mark_streak_days`] 占用（存的是连登
//! **天数** `u32`）。同名键塞对象会让天数读成 0，所以管家结果另开
//! `checkin:streak_task:{account_id}`，两者互不覆盖。

use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

use super::store::CheckinStore;

/// 管家结果键前缀（见模块头「落盘键」）。
const KEY_PREFIX: &str = "checkin:streak_task:";

/// 一家的连登管家能力：`steps` 是这家支持的动作，顺序即执行顺序。
///
/// **契约**：表里出现一家 = 主仓已接上该家的「补签 / 兑换 / 抽奖」端点。
/// 上游端点未接入 → 表保持空（当前状态），调用方不 spawn、零开销。
#[derive(Debug, Clone, Copy)]
pub struct StreakCapability {
    /// provider id（与签到白名单同口径）
    pub provider: &'static str,
    /// 支持的动作（`makeup` / `redeem` / `draw` 的子集，按执行顺序）
    pub steps: &'static [&'static str],
}

/// 支持连登管家的家（**当前为空**：上游端点未接入，骨架就绪）。
///
/// 接入第一家时把它加在这里，例如 A2A 只服务国内版个人账号的
/// `workbuddy`：`&[StreakCapability { provider: "workbuddy", steps:
/// &["makeup", "redeem", "draw"] }]`。
pub const STREAK_PROVIDERS: &[StreakCapability] = &[];

/// 这家是否支持连登管家。
pub fn supports(provider: &str) -> bool {
    STREAK_PROVIDERS
        .iter()
        .any(|capability| capability.provider == provider)
}

/// 一家支持的动作（不支持的给空切片）。
fn steps_of(provider: &str) -> &'static [&'static str] {
    STREAK_PROVIDERS
        .iter()
        .find(|capability| capability.provider == provider)
        .map(|capability| capability.steps)
        .unwrap_or(&[])
}

/// 本轮签到成功的一个账号（管家只要「今天签上了」的账号）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreakTarget {
    pub account_id: String,
    pub provider: String,
}

/// 一个账号的管家结果（落 `checkin:streak_task:{account_id}`）。
///
/// 落盘契约：读侧（管理界面的「连登管家」卡）还没接，字段先按最终形状定死，
/// 免得接线时再改一次键值格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(dead_code)] // 读侧未接（见模块头「上游端点未接入」）
pub struct StreakReport {
    /// unix 毫秒
    pub at: i64,
    pub account_id: String,
    pub provider: String,
    /// 计划执行的动作（来自能力表）
    pub steps: Vec<String>,
    /// 已完成的动作（端点未接入时恒空）
    pub done: Vec<String>,
    /// `endpoint_pending` = 骨架就绪、端点未接入；`done` = 三个动作都跑过
    pub status: String,
    pub message: String,
}

/// 进程内串行闸：同一时刻只允许一轮管家在跑（手动连点签到不会叠出两轮
/// 重复兑换 / 抽奖 —— 上游幂等，但重复打请求本身浪费且招风控，对齐 A2A）。
static RUNNING: AtomicBool = AtomicBool::new(false);

/// 批量签到成功后调用：对支持管家的账号各起一个后台任务。
///
/// 返回「本次 spawn 的账号数」。能力表为空 / 没有命中账号 / 抢不到串行闸 →
/// 返回 `0` 且**不做任何调度动作**（零开销）。
pub fn spawn_after_batch(store: &CheckinStore, targets: &[StreakTarget]) -> usize {
    let picked: Vec<StreakTarget> = targets
        .iter()
        .filter(|target| supports(&target.provider))
        .cloned()
        .collect();
    if picked.is_empty() {
        return 0;
    }
    if RUNNING.swap(true, Ordering::SeqCst) {
        // 上一轮仍在跑：这一轮跳过（只记日志，不影响签到结果）
        tracing::debug!("连登管家上一轮仍在执行，本轮跳过");
        return 0;
    }
    let file_store = store.file_store().clone();
    // spawn 前先取计数：`picked` 会被 move 进任务
    let spawned = picked.len();
    tokio::spawn(async move {
        for target in &picked {
            // 三件事都是「附加收益」：任何一步失败只进日志，绝不冒泡
            let report = run_account(target).await;
            let key = format!("{KEY_PREFIX}{}", target.account_id);
            if let Err(error) = file_store.put(&key, &report) {
                tracing::warn!(
                    account = %target.account_id,
                    %error,
                    "连登管家结果落盘失败"
                );
            }
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
    spawned
}

/// 一个账号的「补签 → 兑换 → 抽奖」。
///
/// **端点未接入**：按能力表的步骤逐条留痕，最终状态记 `endpoint_pending`。
/// 接端点时只改这一个函数（每步拿到 `Ok` 才进 `done`，失败进日志并止步）。
async fn run_account(target: &StreakTarget) -> StreakReport {
    let steps: Vec<String> = steps_of(&target.provider)
        .iter()
        .map(|step| (*step).to_string())
        .collect();
    // 没有上游端点：不伪造完成，如实记「骨架就绪」
    StreakReport {
        at: chrono::Utc::now().timestamp_millis(),
        account_id: target.account_id.clone(),
        provider: target.provider.clone(),
        steps,
        done: Vec::new(),
        status: "endpoint_pending".to_string(),
        message: "上游端点未接入：骨架就绪（补签 → 兑换 → 抽奖）".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_store() -> std::sync::Arc<crate::storage::FileStore> {
        // 仓库既有约定：tempfile 目录 + pid + 序号，各用例互不干扰
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("aigx-streak-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::sync::Arc::new(crate::storage::FileStore::new(dir))
    }

    /// S4：能力表为空时**不 spawn**（零开销），未知家一律不支持。
    #[test]
    fn no_spawn_when_capability_table_is_empty() {
        assert!(
            STREAK_PROVIDERS.is_empty(),
            "上游端点未接入前能力表必须为空；接一家就把它登记进来"
        );
        assert!(!supports("workbuddy"));
        assert!(steps_of("workbuddy").is_empty());
        let store = CheckinStore::new(temp_store());
        store.load().unwrap();
        let targets = vec![StreakTarget {
            account_id: "a1".to_string(),
            provider: "workbuddy".to_string(),
        }];
        // 没有 tokio 运行时也必须安全返回（走的是提前返回分支）
        assert_eq!(spawn_after_batch(&store, &targets), 0);
        assert_eq!(spawn_after_batch(&store, &[]), 0);
    }
}
