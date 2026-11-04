//! 签到调度 —— 30 秒轮询，按计划时刻触发，当天去重。
//!
//! ## 为什么是轮询而不是 cron
//!
//! 与 Agent2API 同一结论：进程可能休眠、机器时钟可能被调整，固定 timer
//! 在这些情况下会漂移或跳过。每 30 秒重读一次墙上时钟、按「今天的时刻是否
//! 已过 + 今天是否已触发」判断，天然自愈，且重启后能补签当天漏掉的。
//!
//! ## 时区
//!
//! 计划时刻固定按 **UTC+8** 解释（与 Agent2API 一致），不跟随服务器本地
//! 时区 —— 部署在海外服务器时「早上 9 点」仍然指北京时间 9 点。
//!
//! ## 串行执行
//!
//! 账号逐个签到、中间留间隔（`gap_ms`）。并发打上游是风控画像里的典型特征，
//! 这条链宁可慢一点。
//!
//! ## 国际版保活链
//!
//! `workbuddy-intl` 在每日调度里走 **Full** 链（探测 + 条件领取 + 桌面/网页
//! 最小对话保活），对齐 Agent2API「定时与批量签到恒走 `WorkbuddyActivity::Full`」：
//! 每日活跃奖励的判据是网页通道那一下（`POST /console/as/conversations/`），
//! 只探测+领取会让 intl 账号的保活计数恒为 0、`kept_active` 只能靠前端手动回写。
//! 保活计数与 `kept_active` 台账标记由调度一次产出，`handle_mark_kept_active`
//! 仅保留作补偿入口。

use std::sync::Arc;
use std::time::Duration;

use chrono::{FixedOffset, Utc};

use super::benefits::keepalive;
use super::benefits::{BenefitOutcome, BenefitResult};
use super::claim::{self, supports_checkin};
use super::store::{CheckinOutcome, CheckinRecord, CheckinStore, ScheduleCounts};
use crate::account_session::AccountSessionStore;

const TICK: Duration = Duration::from_secs(30);

/// UTC+8 固定偏移。
fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap())
}

/// 北京时间的今天（YYYY-MM-DD）。
pub fn today() -> String {
    Utc::now()
        .with_timezone(&beijing())
        .format("%Y-%m-%d")
        .to_string()
}

/// 北京时间的当前 HH:MM。
fn now_hhmm() -> String {
    Utc::now()
        .with_timezone(&beijing())
        .format("%H:%M")
        .to_string()
}

/// 启动调度循环（后台 task，随进程生命周期）。
pub fn spawn(
    store: Arc<CheckinStore>,
    sessions: AccountSessionStore,
    egress: Arc<crate::egress::EgressCache>,
) {
    tokio::spawn(async move {
        // 启动时先跑一轮：进程在计划时刻之后才启动时，当天还能补上
        run_due(&store, &sessions, &egress).await;
        loop {
            tokio::time::sleep(TICK).await;
            run_due(&store, &sessions, &egress).await;
        }
    });
}

/// 跑一轮到期的计划。
///
/// 「到期」= 计划时刻已过（按 UTC+8 的 HH:MM 比较）且今天还没触发过。
/// 启动时也走这一条：进程在时刻之后才起来，当天仍能补上；时刻未到的
/// 计划不会被提前跑掉。
async fn run_due(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    egress: &crate::egress::EgressCache,
) {
    let settings = store.settings();
    if !settings.enabled {
        return;
    }
    let date = today();
    let now = now_hhmm();
    // 刷新时间超出 90 天的桶先清掉：这个 store 没有别的清理入口，
    // 不清的话台账会随天数无限增长
    let _ = store.prune_records();
    for schedule in store.schedules() {
        if !schedule.enabled {
            continue;
        }
        if schedule.last_fired_date == date {
            continue;
        }
        // 时刻格式非法时跳过（保存时已校验，这里只防脏数据）
        if !is_hhmm(&schedule.time) {
            continue;
        }
        // 时刻未到不跑：避免早上启动时把晚上 21:00 的计划提前执行掉
        if schedule.time.as_str() > now.as_str() {
            continue;
        }
        let outcome = run_schedule(
            store,
            sessions,
            egress,
            &schedule.providers,
            settings.gap_ms,
        )
        .await;
        let counts = ScheduleCounts {
            ok: outcome.ok,
            keepalive: outcome.keepalive,
            failed: outcome.failed,
        };
        let _ = store.mark_fired(&schedule.id, &date, &outcome.summary, &counts);
    }
}

/// `HH:MM` 格式校验。
pub fn is_hhmm(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 5 || bytes[2] != b':' {
        return false;
    }
    let (Ok(h), Ok(m)) = (value[0..2].parse::<u32>(), value[3..5].parse::<u32>()) else {
        return false;
    };
    h < 24 && m < 60
}

/// 一次计划执行的结果汇总（`summary` 进台账，计数供界面展示）。
#[derive(Debug, Clone, Default)]
pub struct ScheduleOutcome {
    /// 一句话摘要
    pub summary: String,
    /// 成功数（含「今日已领」）
    pub ok: usize,
    /// 附带保活数（`workbuddy-intl` 的 Full 链顺带保活点亮的账号数）
    pub keepalive: usize,
    /// 失败数
    pub failed: usize,
}

/// 跑一次指定 provider 范围的签到，返回计数汇总。
pub async fn run_schedule(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    egress: &crate::egress::EgressCache,
    providers: &[String],
    gap_ms: u64,
) -> ScheduleOutcome {
    let targets = collect_targets(sessions, providers);
    if targets.is_empty() {
        return ScheduleOutcome {
            summary: "没有可签到的账号".to_string(),
            ..Default::default()
        };
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut keepalive = 0usize;
    let mut total = 0usize;
    for session in targets {
        total += 1;
        let client = match egress.client(session.proxy.as_deref()) {
            Ok(client) => client,
            Err(e) => {
                record(
                    store,
                    &session,
                    "schedule",
                    CheckinOutcome::Failed,
                    &format!("出口配置错误：{e}"),
                    false,
                );
                failed += 1;
                continue;
            }
        };
        // workbuddy-intl 走 Full 链（探测 + 条件领取 + 桌面/网页保活），
        // 对齐 Agent2API「定时与批量签到恒走 WorkbuddyActivity::Full」；
        // 其余家走普通 claim。保活计数与 kept_active 台账标记一次产出，
        // 不再依赖前端手动回写。
        let (outcome, message, kept_active) = if session.provider == "workbuddy-intl" {
            let benefit = keepalive::run(store.file_store(), &client, &session, "full").await;
            let hits = keepalive_hits(&benefit);
            if hits > 0 {
                keepalive += 1;
            }
            (benefit_outcome(&benefit), benefit.message, hits > 0)
        } else {
            let result = claim::claim_for(&client, &session).await;
            (result.outcome, result.msg, false)
        };
        if outcome.is_ok() {
            ok += 1;
        } else if outcome == CheckinOutcome::Failed {
            failed += 1;
        }
        record(
            store,
            &session,
            "schedule",
            outcome,
            &message,
            kept_active,
        );
        if gap_ms > 0 {
            tokio::time::sleep(Duration::from_millis(gap_ms)).await;
        }
    }
    ScheduleOutcome {
        summary: if keepalive > 0 {
            format!("成功 {ok}/{total}，保活 {keepalive}")
        } else {
            format!("成功 {ok}/{total}")
        },
        ok,
        failed,
        keepalive,
    }
}

/// BenefitResult → 台账结果分类。
///
/// Done / Already 算「今天完成了」，Pending 算未完成（不算失败），
/// Failed / Skipped 算失败 —— 与 `claim::ClaimResult` 的口径一致。
fn benefit_outcome(benefit: &BenefitResult) -> CheckinOutcome {
    match (benefit.ok, benefit.outcome) {
        (true, BenefitOutcome::Done) => CheckinOutcome::Success,
        (true, BenefitOutcome::Already) => CheckinOutcome::Already,
        (false, BenefitOutcome::Pending) => CheckinOutcome::Pending,
        _ => CheckinOutcome::Failed,
    }
}

/// 保活链点亮的通道数（桌面 + 网页各一发，0–2）。
///
/// 读 `keepalive::run` 落盘的记忆（`benefit:keepalive:{id}` 的 `keepalive_ok`）——
/// 保活计数只认「真的点亮了通道」的账号，两发都失败时保活链算没跑成
/// （`keepalive::run` 会返回 Pending，稍后重试）。
fn keepalive_hits(benefit: &BenefitResult) -> usize {
    benefit
        .data
        .get("keepalive_ok")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize
}

/// 收集参与签到的账号（启用 + 凭证未过期 + provider 在白名单内）。
pub fn collect_targets(
    sessions: &AccountSessionStore,
    providers: &[String],
) -> Vec<crate::account_session::model::AccountSession> {
    let now = Utc::now().timestamp();
    sessions
        .all()
        .into_iter()
        .filter(|s| s.is_enabled() && !s.is_expired(now))
        .filter(|s| supports_checkin(&s.provider))
        .filter(|s| providers.is_empty() || providers.iter().any(|p| p == &s.provider))
        .collect()
}

/// 写一条台账。`kept_active` 标记本次执行链是否附带保活动作。
pub fn record(
    store: &CheckinStore,
    session: &crate::account_session::model::AccountSession,
    source: &str,
    outcome: CheckinOutcome,
    message: &str,
    kept_active: bool,
) {
    let _ = store.append_record(CheckinRecord {
        ts: Utc::now().timestamp(),
        date: today(),
        account_id: session.id.clone(),
        account_name: session.name.clone(),
        provider: session.provider.clone(),
        source: source.to_string(),
        outcome,
        ok: outcome.is_ok(),
        kept_active,
        message: message.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_validation() {
        assert!(is_hhmm("09:00"));
        assert!(is_hhmm("23:59"));
        assert!(!is_hhmm("24:00"));
        assert!(!is_hhmm("9:00"));
        assert!(!is_hhmm("09-00"));
        assert!(!is_hhmm(""));
        assert!(!is_hhmm("aa:bb"));
    }

    #[test]
    fn today_is_beijing() {
        // 只验证格式；具体日期随运行时间变化
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
    }
}
