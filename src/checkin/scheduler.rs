//! 签到调度 —— 30 秒轮询，逐家判定时刻，当天逐家去重 + 失败重试。
//!
//! ## 为什么是轮询而不是 cron
//!
//! 与 Agent2API 同一结论：进程可能休眠、机器时钟可能被调整，固定 timer
//! 在这些情况下会漂移或跳过。每 30 秒重读一次墙上时钟、按「今天的时刻是否
//! 已过 + 这家今天是否已触发」判断，天然自愈，且重启后能补签当天漏掉的。
//!
//! ## 逐家时刻（C-3）
//!
//! 上游的每日活动各有窗口：qoder 10:00 开、zcode 19:00 开。全员同一时刻
//! 触发会拿到「窗口未开」并在同一分钟打满上游（风控画像）。生效时刻三档：
//! 条目 `provider_times` 覆盖 > 内置 [`PROVIDER_TIMES`] > 条目 `time`。
//!
//! ## 失败/窗口未开重试（C-2）
//!
//! 一轮跑完不等于当天结束：Pending（窗口未开）与 Failed 的账号按
//! `retry_gap_ms`（默认 10 分钟）重试，每账号每天最多 `retry_limit` 次
//!（默认 3）。次数记账落计划 KV，跨天自动作废。全部成功后仍留一轮「补签」
//! 捞漏网账号（对齐 A2A 的 makeup 轮）。
//!
//! ## 先落标记再执行（C-4）
//!
//! 执行前先 `mark_fired_provider`：中途被杀，重启后不会把整条计划重跑一遍。
//!
//! ## 时区
//!
//! 计划时刻按配置的 `timezone_offset` 解释（默认 UTC+8，可在签到设置中改为
//! 系统本地时区或其它偏移）。与 Agent2API 的「用户本地时区」口径对齐 ——
//! 海外部署时不再硬绑北京时间。
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

use chrono::{FixedOffset, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use super::benefits::keepalive;
use super::benefits::{BenefitOutcome, BenefitResult};
use super::claim::{self, supports_checkin};
use super::store::{CheckinOutcome, CheckinRecord, CheckinSchedule, CheckinStore, ScheduleCounts};
use crate::account_session::AccountSessionStore;

const TICK: Duration = Duration::from_secs(30);

/// 逐家内置生效时刻（窗口开启后 1 分钟——正好卡在开启那一秒时上游可能
/// 还没下发活动）。**只登记有实测依据的窗口**（对齐 A2A `PROVIDER_TIMES`）。
const PROVIDER_TIMES: [(&str, &str); 4] = [
    ("qoder", "10:01"),
    ("qoder-intl", "10:01"),
    ("zcode", "19:01"),
    ("zcode-intl", "19:01"),
];

/// 一家在某个条目里的**生效时刻**：条目覆盖 > 内置专属 > 条目自己的 time。
pub fn effective_time(schedule: &CheckinSchedule, provider: &str) -> String {
    schedule
        .provider_times
        .get(provider)
        .filter(|time| !time.is_empty())
        .cloned()
        .or_else(|| {
            PROVIDER_TIMES
                .iter()
                .find(|(id, _)| *id == provider)
                .map(|(_, time)| time.to_string())
        })
        .unwrap_or_else(|| schedule.time.clone())
}

/// `HH:MM` → 当日分钟数（格式非法返回 None）。
fn minutes_of(hhmm: &str) -> Option<i64> {
    if !is_hhmm(hhmm) {
        return None;
    }
    let h = hhmm[0..2].parse::<i64>().ok()?;
    let m = hhmm[3..5].parse::<i64>().ok()?;
    Some(h * 60 + m)
}

/// 分钟数 → `HH:MM`（跨天回绕到 24h 内）。
fn hhmm_of(minutes: i64) -> String {
    let m = minutes.rem_euclid(24 * 60);
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// 窗口分钟数（`end - start`；>=1 才算有效窗口）。
fn window_minutes(start: &str, end: &str) -> Option<i64> {
    let span = minutes_of(end)? - minutes_of(start)?;
    (span >= 1).then_some(span)
}

/// 稳定哈希（FNV-1a 64 位）：同账号跨进程、跨重启、跨版本恒定。
///
/// **不能用 `std::hash::DefaultHasher`**：std 文档明说其算法不保证跨版本稳定，
/// 换一个 Rust 版本重启就会让随机时刻漂移 —— 与「重启不漂」的契约冲突。
fn stable_hash(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 某条目对某 provider 的窗口 `(start, end)`。
///
/// `start` 走三档：`provider_times` 覆盖 > 内置 [`PROVIDER_TIMES`] > 条目 `time`。
/// `end` 仅在**回落条目 time**（无覆盖、无内置）且 `randomize=true`、`time_end`
/// 合法且晚于 start 时才给出 —— 专属时刻是实测窗口开启点，随机反而可能撞空窗。
pub fn effective_window(schedule: &CheckinSchedule, provider: &str) -> (String, Option<String>) {
    let start = effective_time(schedule, provider);
    let has_override = schedule
        .provider_times
        .get(provider)
        .is_some_and(|t| !t.is_empty());
    let has_builtin = PROVIDER_TIMES.iter().any(|(id, _)| *id == provider);
    let end = if schedule.randomize
        && !has_override
        && !has_builtin
        && window_minutes(&start, &schedule.time_end).is_some()
    {
        Some(schedule.time_end.clone())
    } else {
        None
    };
    (start, end)
}

/// 某账号在某条目某 provider 下的**生效时刻**（HH:MM）。
///
/// - 无窗口（未开启随机 / 有专属时刻）→ 窗口 start；
/// - 有窗口 → `start + hash(account_id) % 窗口分钟数`（**确定性**：同账号同日
///   同刻、重启不漂；不同账号可落在窗口内不同分钟）。
pub fn account_instant(schedule: &CheckinSchedule, provider: &str, account_id: &str) -> String {
    let (start, end) = effective_window(schedule, provider);
    let Some(end) = end else {
        return start;
    };
    let Some(span) = window_minutes(&start, &end) else {
        return start;
    };
    let base = minutes_of(&start).unwrap_or(0);
    let offset = (stable_hash(account_id) % span as u64) as i64;
    hhmm_of(base + offset)
}

/// 某家今天是否已跑过主轮（含存量兼容）。
///
/// 逐家标记为空 = 升级前的旧条目：回退用整条 `last_fired_date` 判定，避免
/// 升级当天把已签过的家全部重签一遍。
fn main_fired(schedule: &CheckinSchedule, provider: &str, today: &str) -> bool {
    if !schedule.last_fired_providers.is_empty() {
        return schedule
            .last_fired_providers
            .get(provider)
            .is_some_and(|date| date == today);
    }
    schedule.last_fired_date == today
}

/// 今天的主轮该跑哪些家：时刻已到、今天还没跑过、范围在条目内（空 = 全部）。
fn due_providers(schedule: &CheckinSchedule, now_hhmm: &str, today: &str) -> Vec<String> {
    schedule
        .providers
        .iter()
        .filter(|p| supports_checkin(p))
        .filter(|p| !main_fired(schedule, p, today))
        .filter(|p| {
            let time = effective_time(schedule, p);
            // 时刻格式非法时跳过（保存时已校验，这里只防脏数据）
            is_hhmm(&time) && time.as_str() <= now_hhmm
        })
        .cloned()
        .collect()
}

/// 一轮执行后的逐账号重试记账（当天有效，跨天作废）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct RetryLedger {
    /// `{account_id: 下次可重试的 unix 秒}`
    #[serde(default)]
    accounts: HashMap<String, i64>,
    /// `{account_id: 今天已重试几次}`
    #[serde(default)]
    attempts: HashMap<String, i64>,
}

/// 签到时区偏移（小时）。由 `set_settings` 更新，`beijing()` 读取。
/// 0 = 使用系统本地时区（`chrono::Local`）。
static CHECKIN_TZ_OFFSET: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(8);

/// 设置签到时区偏移（由 store 的 `set_settings` 调用）。
pub fn set_checkin_tz_offset(offset: i32) {
    CHECKIN_TZ_OFFSET.store(offset, std::sync::atomic::Ordering::Relaxed);
}

/// 获取当前签到时区偏移。
pub fn checkin_tz_offset() -> i32 {
    CHECKIN_TZ_OFFSET.load(std::sync::atomic::Ordering::Relaxed)
}

/// 按配置的偏移构造 FixedOffset（0 = 系统本地时区）。
pub(crate) fn beijing() -> FixedOffset {
    let offset = checkin_tz_offset();
    if offset == 0 {
        *chrono::Local::now().offset()
    } else {
        FixedOffset::east_opt(offset * 3600).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap())
    }
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
/// 逐家判定：「这家」的生效时刻已过且「这家」今天还没跑过 → 跑这一家
/// （对齐 A2A：同一条条目可在 00:01 / 10:01 / 19:01 各跑一批）。跑完
/// Pending / Failed 的账号进重试记账，之后每 tick 消化到期的重试与补签。
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
        // 时刻格式非法时跳过（保存时已校验，这里只防脏数据）
        if !is_hhmm(&schedule.time) {
            continue;
        }
        // ① 主轮：逐家到点即跑（执行前先落逐家标记 —— 中途被杀不会重跑）
        for provider in due_providers(&schedule, &now, &date) {
            // 区间窗：整个窗口内逐 tick 只跑「自己那刻已到」的账号；窗口进行中
            // 不落 provider 标记（否则剩下的账号再也不会被 this 轮捡起）。
            match effective_window(&schedule, &provider).1 {
                None => {
                    // 无随机窗口（或专属时刻）：一次性全量跑，回到原有行为
                    let _ = store.mark_fired_provider(&schedule.id, &provider, &date);
                    let targets = collect_targets(sessions, std::slice::from_ref(&provider));
                    let outcome = run_targets(
                        store,
                        sessions,
                        egress,
                        targets,
                        "schedule",
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
                Some(end) => {
                    // 区间窗：逐 tick 只跑「自己那刻已到」且今天还没落账的账号。
                    // 走到这里时窗口内的账号按各自随机时刻陆续到点（窗口已过则
                    // 全数到点，进程曾错过窗口也能自愈）。
                    let targets = window_due_targets(store, sessions, &schedule, &provider, &now);
                    if !targets.is_empty() {
                        let outcome = run_targets(
                            store,
                            sessions,
                            egress,
                            targets,
                            "schedule",
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
                    if now.as_str() >= end.as_str() {
                        // 窗口结束：本家今天到此为止（未签上的账号交给重试/补签轮）
                        let _ = store.mark_fired_provider(&schedule.id, &provider, &date);
                    }
                }
            }
        }
        // ② 重试轮：这家今天已跑过主轮，但仍有未签上的账号 —— 每账号每天
        // 最多 `retry_limit` 次、间隔 `retry_gap_minutes`；次数记账跨天作废。
        for provider in retry_due_providers(&schedule, &now, &date, store) {
            run_retry_round(store, sessions, egress, &schedule, &provider, &settings).await;
        }
        // ③ 补签轮：`makeup_time` 到达时，挑当天该条目下仍未签上的账号再跑一次
        // （`kind="makeup"`），每天至多一轮。
        run_makeup_round(store, sessions, egress, &schedule, &settings).await;
    }
}

/// 窗口进行中该跑的账号：逐账号生效时刻（区间随机）已到，且今天这家**还没
/// 落过账**的账号。今天已有记录的账号（含失败的）主轮不再重复 —— 失败交给
/// 重试轮收尾。
fn window_due_targets(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    schedule: &CheckinSchedule,
    provider: &str,
    now: &str,
) -> Vec<crate::account_session::model::AccountSession> {
    let ran: std::collections::HashSet<String> = store
        .recent_records(1, Some(provider), None, None, None)
        .unwrap_or_default()
        .into_iter()
        .map(|r| r.account_id)
        .collect();
    collect_targets(sessions, std::slice::from_ref(&provider.to_string()))
        .into_iter()
        .filter(|s| !ran.contains(&s.id))
        .filter(|s| account_instant(schedule, provider, &s.id).as_str() <= now)
        .collect()
}

/// 今天该重试的家：主轮已跑、时刻已过、且这家还有没签上的账号。
///
/// 判据用台账的「今天这家是否全部 is_ok」—— 与上游「补签轮看账号级
/// checkinAt」同一性质的事实判据（AIGX 的签到事实在台账里）。
fn retry_due_providers(
    schedule: &CheckinSchedule,
    now: &str,
    today: &str,
    store: &CheckinStore,
) -> Vec<String> {
    schedule
        .providers
        .iter()
        .filter(|p| supports_checkin(p))
        .filter(|p| main_fired(schedule, p, today))
        .filter(|p| {
            let time = effective_time(schedule, p);
            is_hhmm(&time) && time.as_str() <= now
        })
        .filter(|p| {
            // 该家今天还有未签上的账号（Pending/Failed 都算）
            let records = store
                .recent_records(1, Some(p), None, None, None)
                .unwrap_or_default();
            !records.is_empty() && records.iter().any(|r| !r.ok)
        })
        .cloned()
        .collect()
}

/// 重试 KV 键（按计划 id + 当天日期，跨天自动换键 = 作废）。
fn retry_key(schedule_id: &str, date: &str) -> String {
    format!("checkin:retry:{schedule_id}:{date}")
}

/// 读今天的重试记账（损坏 / 跨天给空表）。
fn read_retry_ledger(store: &CheckinStore, schedule_id: &str, date: &str) -> RetryLedger {
    store
        .file_store()
        .get::<RetryLedger>(&retry_key(schedule_id, date))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// 写回重试记账（含裁剪：只留还在名单里与次数已用尽的）。
fn write_retry_ledger(store: &CheckinStore, schedule_id: &str, date: &str, ledger: &RetryLedger) {
    let _ = store
        .file_store()
        .put(&retry_key(schedule_id, date), ledger);
}

/// 该条目的有效重试参数 `(上限, 间隔秒)`。上限 0 = 关闭重试；间隔：
/// `retry_gap_minutes>0` 用条目分钟数，否则回落全局 `settings.retry_gap_ms`。
fn effective_retry_policy(
    schedule: &CheckinSchedule,
    settings: &super::store::CheckinSettings,
) -> (i64, i64) {
    let limit = schedule.retry_limit as i64;
    let gap_secs = if schedule.retry_gap_minutes > 0 {
        schedule.retry_gap_minutes as i64 * 60
    } else {
        (settings.retry_gap_ms / 1000) as i64
    };
    (limit, gap_secs)
}

/// 一次重试轮：跑这家「今天还没签上」且「重试到点 / 次数未用尽」的账号。
///
/// 上限与间隔由**条目**驱动：`retry_limit=0`（显式关掉）时整轮跳过；
/// `retry_gap_minutes>0` 用条目分钟数，`<=0` 回落全局 `settings.retry_gap_ms`。
/// workbuddy-intl 的重试也走 Full 链（与主轮同口径），这里为控制改动面先按
/// 普通 claim 处理——intl 的 Pending 由下一轮主判据自然收敛。
async fn run_retry_round(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    _egress: &crate::egress::EgressCache,
    schedule: &CheckinSchedule,
    provider: &str,
    settings: &super::store::CheckinSettings,
) {
    // 该条目显式关掉重试（retry_limit=0）：不进重试轮
    let (limit, gap_secs) = effective_retry_policy(schedule, settings);
    if limit == 0 {
        return;
    }
    let date = today();
    let now = Utc::now().timestamp();
    let mut ledger = read_retry_ledger(store, &schedule.id, &date);
    // 今天这家还没签上的账号：台账倒序读一天的记录，按账号取**末次**结果
    //（`recent_records` 按 ts 倒序，先见到的就是最新的——用 entry 覆盖会拿成
    // 最旧的，必须只在缺失时插入）。
    let records = store
        .recent_records(1, Some(provider), None, None, None)
        .unwrap_or_default();
    // 账号 id → 末次记录（recent_records 按 ts 倒序，只在缺失时插入 =
    // 拿到的是最新一条）
    let mut last_by_account: HashMap<String, &CheckinRecord> = HashMap::new();
    for r in &records {
        last_by_account
            .entry(r.account_id.clone())
            .or_insert_with(|| r);
    }
    // 到点且次数未用尽的未签账号
    let due: Vec<String> = last_by_account
        .iter()
        .filter(|(_, r)| !r.ok && matches!(r.source.as_str(), "schedule" | "retry" | "makeup"))
        .filter(|(id, _)| ledger.attempts.get(id.as_str()).copied().unwrap_or(0) < limit)
        .filter(|(id, _)| ledger.accounts.get(id.as_str()).copied().unwrap_or(0) <= now)
        .map(|(id, _)| id.clone())
        .collect();
    let provider_filter = [provider.to_string()];
    let targets: Vec<crate::account_session::model::AccountSession> =
        collect_targets(sessions, &provider_filter)
            .into_iter()
            .filter(|s| due.contains(&s.id))
            .collect();
    if targets.is_empty() {
        return;
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    let mut total = 0usize;
    for session in targets {
        total += 1;
        let client = match sessions.client_for(&session) {
            Ok(client) => client,
            Err(_) => {
                failed += 1;
                continue;
            }
        };
        let result = claim::claim_for(&client, &session).await;
        if result.outcome.is_ok() {
            ok += 1;
        } else if result.outcome == CheckinOutcome::Failed {
            failed += 1;
        }
        record(store, &session, "retry", result.outcome, &result.msg, false);
        // 记账：签上了就摘掉；没签上 → 次数 +1、下次到点 = now + gap
        if result.outcome.is_ok() {
            ledger.accounts.remove(&session.id);
            ledger.attempts.remove(&session.id);
        } else {
            *ledger.attempts.entry(session.id.clone()).or_insert(0) += 1;
            ledger.accounts.insert(session.id.clone(), now + gap_secs);
        }
        if settings.gap_ms > 0 {
            tokio::time::sleep(Duration::from_millis(settings.gap_ms)).await;
        }
    }
    // 裁剪：名单只留次数未用尽的；次数记账留「还在名单」与「已用尽」两种
    ledger
        .accounts
        .retain(|id, _| ledger.attempts.get(id).copied().unwrap_or(0) < limit);
    ledger
        .attempts
        .retain(|id, count| ledger.accounts.contains_key(id) || *count >= limit);
    write_retry_ledger(store, &schedule.id, &date, &ledger);
    if total > 0 {
        let _ = store.mark_fired(
            &schedule.id,
            &date,
            &format!("重试 {ok}/{total}（{provider}）"),
            &ScheduleCounts {
                ok,
                keepalive: 0,
                failed,
            },
        );
    }
}

/// 补签轮 KV 键（按计划 id + 当天日期，跨天自动换键 = 允许次日再跑）。
fn makeup_key(schedule_id: &str, date: &str) -> String {
    format!("checkin:makeup:{schedule_id}:{date}")
}

/// 今天是否已跑过补签轮。
fn makeup_already_ran(store: &CheckinStore, schedule_id: &str, date: &str) -> bool {
    store
        .file_store()
        .get::<bool>(&makeup_key(schedule_id, date))
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// 条目覆盖的 provider 范围（空 = 全部支持的）。
fn schedule_provider_scope(schedule: &CheckinSchedule) -> Vec<String> {
    if schedule.providers.is_empty() {
        claim::supported_providers()
            .iter()
            .map(|p| p.to_string())
            .collect()
    } else {
        schedule
            .providers
            .iter()
            .filter(|p| supports_checkin(p))
            .cloned()
            .collect()
    }
}

/// 补签轮的账号：该条目 provider 范围内，今天**还没签上**（末次记录非 ok，
/// 或今天压根没跑过）的账号。今天已 ok 的账号不再补签。
pub(super) fn makeup_targets(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    schedule: &CheckinSchedule,
) -> Vec<crate::account_session::model::AccountSession> {
    let mut out = Vec::new();
    for provider in schedule_provider_scope(schedule) {
        let records = store
            .recent_records(1, Some(&provider), None, None, None)
            .unwrap_or_default();
        // 账号 id → 末次记录（倒序读、只在缺失时插入 = 最新一条）
        let mut last_by_account: HashMap<String, &CheckinRecord> = HashMap::new();
        for r in &records {
            last_by_account
                .entry(r.account_id.clone())
                .or_insert_with(|| r);
        }
        for session in collect_targets(sessions, std::slice::from_ref(&provider)) {
            let signed = last_by_account.get(&session.id).is_some_and(|r| r.ok);
            if !signed {
                out.push(session);
            }
        }
    }
    out
}

/// 补签轮：`makeup_time` 到达时，挑当天该条目下仍未签上的账号再跑一次
/// （`kind="makeup"`），每天至多一轮。
async fn run_makeup_round(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    egress: &crate::egress::EgressCache,
    schedule: &CheckinSchedule,
    settings: &super::store::CheckinSettings,
) {
    let makeup_time = schedule.makeup_time.trim();
    if makeup_time.is_empty() || !is_hhmm(makeup_time) {
        return;
    }
    if makeup_time > now_hhmm().as_str() {
        return; // 未到补签时刻
    }
    let date = today();
    if makeup_already_ran(store, &schedule.id, &date) {
        return;
    }
    // 先落「已跑」标记：中途被杀也不会反复补签
    let _ = store
        .file_store()
        .put(&makeup_key(&schedule.id, &date), &true);
    let targets = makeup_targets(store, sessions, schedule);
    if targets.is_empty() {
        return;
    }
    let outcome = run_targets(store, sessions, egress, targets, "makeup", settings.gap_ms).await;
    let _ = store.mark_fired(
        &schedule.id,
        &date,
        &format!("补签 {}", outcome.summary),
        &ScheduleCounts {
            ok: outcome.ok,
            keepalive: outcome.keepalive,
            failed: outcome.failed,
        },
    );
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
    run_targets(store, sessions, egress, targets, "schedule", gap_ms).await
}

/// 跑一批账号并落台账。`source` 同时写进 `source` 与 `kind`（manual / schedule /
/// makeup / retry）——主轮传 `"schedule"`、补签轮传 `"makeup"`。
pub(crate) async fn run_targets(
    store: &CheckinStore,
    sessions: &AccountSessionStore,
    _egress: &crate::egress::EgressCache,
    targets: Vec<crate::account_session::model::AccountSession>,
    source: &str,
    gap_ms: u64,
) -> ScheduleOutcome {
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
        // 出口统一走 `client_for`（解析 `pool:` / `rotate` 编码）——绕开
        // 它会把「pool:px_x」直接喂给 reqwest 导致 invalid proxy，配了
        // 代理池的账号签到必失败（C-1）。
        let client = match sessions.client_for(&session) {
            Ok(client) => client,
            Err(e) => {
                record(
                    store,
                    &session,
                    source,
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
        record(store, &session, source, outcome, &message, kept_active);
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
///
/// `kind` 与 `source` **同值双写**（旧读端只认 `source`，新读端用 `kind`）；
/// `error` 记失败原文（`is_ok()` 为真时留空 —— 成功/今日已领没有失败原因）。
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
        kind: source.to_string(),
        outcome,
        ok: outcome.is_ok(),
        kept_active,
        message: message.to_string(),
        error: if outcome.is_ok() {
            String::new()
        } else {
            message.to_string()
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule(time: &str, providers: &[&str]) -> CheckinSchedule {
        CheckinSchedule {
            id: "s1".to_string(),
            label: "每日".to_string(),
            enabled: true,
            time: time.to_string(),
            time_end: String::new(),
            randomize: false,
            retry_limit: 3,
            retry_gap_minutes: 5,
            makeup_time: String::new(),
            providers: providers.iter().map(|p| p.to_string()).collect(),
            provider_times: HashMap::new(),
            last_fired_date: String::new(),
            last_fired_providers: HashMap::new(),
            last_result: String::new(),
            last_ok: 0,
            last_keepalive: 0,
            last_failed: 0,
        }
    }

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

    #[test]
    fn effective_time_three_tier_precedence() {
        let mut s = schedule("00:01", &["qoder", "loomy"]);
        // 内置档：qoder 10:01 > 条目 00:01
        assert_eq!(effective_time(&s, "qoder"), "10:01");
        // 无内置的家回落条目时刻
        assert_eq!(effective_time(&s, "loomy"), "00:01");
        // 条目覆盖 > 内置
        s.provider_times
            .insert("qoder".to_string(), "08:30".to_string());
        assert_eq!(effective_time(&s, "qoder"), "08:30");
        // 空串 = 撤销覆盖 → 回落内置
        s.provider_times.insert("qoder".to_string(), String::new());
        assert_eq!(effective_time(&s, "qoder"), "10:01");
    }

    #[test]
    fn due_providers_respect_time_and_marks() {
        let mut s = schedule("00:01", &["qoder", "zcode", "loomy"]);
        // 早上 08:00：zcode（19:01）未到点、qoder（10:01）未到点、loomy（00:01）到点
        let due = due_providers(&s, "08:00", "2026-11-07");
        assert_eq!(due, vec!["loomy".to_string()]);
        // 中午 12:00：qoder 也到点了
        let due = due_providers(&s, "12:00", "2026-11-07");
        assert_eq!(due, vec!["qoder".to_string(), "loomy".to_string()]);
        // 逐家标记：qoder 今天跑过 → 不再出现
        s.last_fired_providers
            .insert("qoder".to_string(), "2026-11-07".to_string());
        let due = due_providers(&s, "12:00", "2026-11-07");
        assert_eq!(due, vec!["loomy".to_string()]);
        // 存量兼容：标记表为空但 last_fired_date=今天 → 全部视为已跑（升级当天不重签）
        s.last_fired_providers.clear();
        s.last_fired_date = "2026-11-07".to_string();
        assert!(due_providers(&s, "12:00", "2026-11-07").is_empty());
        // zcode 19:01 之后到点
        s.last_fired_date.clear();
        let due = due_providers(&s, "19:05", "2026-11-07");
        assert_eq!(
            due,
            vec![
                "qoder".to_string(),
                "zcode".to_string(),
                "loomy".to_string()
            ]
        );
    }

    #[test]
    fn window_randomization_is_deterministic_and_within_bounds() {
        let mut s = schedule("09:00", &["loomy"]);
        s.time_end = "10:00".to_string();
        s.randomize = true;
        let (start, end) = effective_window(&s, "loomy");
        assert_eq!(start, "09:00");
        assert_eq!(end.as_deref(), Some("10:00"));
        // 同一账号两次调用必须完全一致（确定性）
        let a1 = account_instant(&s, "loomy", "acct-1");
        let a2 = account_instant(&s, "loomy", "acct-1");
        assert_eq!(a1, a2, "同账号同刻必须一致（重启不漂）");
        // 落在 [09:00, 10:00) 内，且格式合法
        for id in ["a", "b", "acct-1", "acct-2", "zzz"] {
            let t = account_instant(&s, "loomy", id);
            assert!(is_hhmm(&t), "应是 HH:MM：{t}");
            assert!(t.as_str() >= "09:00" && t.as_str() < "10:00", "越界：{t}");
        }
        // 不同账号在窗口内**可以**不同（不强制全不同，但至少不应全体挤在 start）
        let spread: std::collections::HashSet<String> = (0..50)
            .map(|i| account_instant(&s, "loomy", &format!("u{i}")))
            .collect();
        assert!(spread.len() > 1, "50 个账号应落在不止一个分钟上");
    }

    #[test]
    fn window_off_without_randomize_or_valid_end() {
        let mut s = schedule("09:00", &["loomy"]);
        s.time_end = "10:00".to_string();
        // randomize 未开 → 无窗口
        assert!(effective_window(&s, "loomy").1.is_none());
        // 开了 randomize 但 time_end 非法 / 不晚于 start → 无窗口
        s.randomize = true;
        s.time_end = String::new();
        assert!(effective_window(&s, "loomy").1.is_none());
        s.time_end = "08:00".to_string(); // 早于 start
        assert!(effective_window(&s, "loomy").1.is_none());
        s.time_end = "aa:bb".to_string();
        assert!(effective_window(&s, "loomy").1.is_none());
    }

    #[test]
    fn builtin_time_opts_out_of_random_window() {
        // 有内置专属时刻的家（qoder 10:01）不参与区间随机：专属时刻是实测
        // 窗口开启点，随机到别处会撞空窗。
        let mut s = schedule("09:00", &["qoder"]);
        s.time_end = "11:00".to_string();
        s.randomize = true;
        let (start, end) = effective_window(&s, "qoder");
        assert_eq!(start, "10:01");
        assert!(end.is_none());
        assert_eq!(account_instant(&s, "qoder", "acct-1"), "10:01");
    }

    #[test]
    fn stable_hash_matches_fnv1a_reference() {
        // 固定值锚（防无意改动哈希算法导致随机时刻全体漂移）：
        // FNV-1a 64 位对 "a" = 0xaf63dc4c8601ec8c。
        assert_eq!(stable_hash("a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(stable_hash(""), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn effective_retry_policy_zero_disables_and_gap_falls_back() {
        let mut s = schedule("09:00", &["loomy"]);
        let settings = super::super::store::CheckinSettings {
            retry_gap_ms: 600_000, // 10 分钟（全局）
            ..Default::default()
        };
        // 条目分钟数优先
        s.retry_limit = 3;
        s.retry_gap_minutes = 5;
        assert_eq!(effective_retry_policy(&s, &settings), (3, 300));
        // retry_limit=0 → 关闭重试（limit 0）
        s.retry_limit = 0;
        assert_eq!(effective_retry_policy(&s, &settings).0, 0);
        // retry_gap_minutes<=0 → 回落全局 retry_gap_ms
        s.retry_limit = 2;
        s.retry_gap_minutes = 0;
        assert_eq!(effective_retry_policy(&s, &settings), (2, 600));
    }
}
