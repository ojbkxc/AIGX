//! 账号池周期维护 —— 由 `cron::Scheduler` 周期调用。
//!
//! 职责：
//! 1. [`sweep`]：清理到期冷却记录（避免 `cooldowns` 无限膨胀）、按
//!    `expires_at` 同步凭证过期状态（过期 → `expired`，续期后自动恢复 `enabled`）；
//! 2. [`check_relogin`]：「登录态需要人工处理」的两类**站内**提醒（#35）——
//!    C1 不可自动续期家已被上游连续拒绝（`needs_relogin`）、C2 固定寿命
//!    上游到期前 3 天。
//!
//! 为什么提醒放在这里而不是拒绝凭证的当场：数据面每次 401 都要过的路径
//! 不该顺手拿告警锁（Phase1 性能红线：热路径零额外同步），而状态本身
//! （`needs_relogin` → 逐出选路）是即时生效的，这里只负责**让人看见**，
//! 一个巡检周期（≤5 分钟）的延迟换热路径干净，值。

use std::sync::Arc;

use super::store::AccountSessionStore;
use crate::notify::alert::{AlertKind, AlertLevel};
use crate::notify::alert_patrol::SharedAlertEvaluator;

/// 提醒静默窗口（秒）。账号级去重位是 `relogin_notified_at`（持久化），
/// 这里的静默只是「同一轮告警风暴别刷屏」的第二道闸。
const NOTICE_SILENCE_SECS: u64 = 24 * 3600;

const DAY_SECS: i64 = 24 * 3600;

/// 执行一轮维护，返回本轮变更的账号数（0 = 无事发生，cron 日志降噪）。
pub fn sweep(sessions: &AccountSessionStore) -> u64 {
    sessions.sweep(chrono::Utc::now().timestamp())
}

/// 登录态提醒巡检（#35 C1 + C2）。返回本轮**发出**的提醒条数。
///
/// 两类提醒，每条账号只可能命中一类（`needs_relogin` 优先——已经死了的人
/// 不需要再听「快死了」）：
/// - **C1 `AccountReloginRequired`**：不可自动续期的家被上游连续拒了
///   ≥3 次，配置态已转 `needs_relogin`（账号已逐出选路）。这里补上
///   「让人看见」这一步：此前它只有一条 cooldown，面板上一片 `enabled`。
/// - **C2 `AccountReloginSoon`**：固定寿命上游（目前只有 loomy 14 天，
///   见 `model::RELOGIN_KNOWN_TTL_SECS`）到期前 3 天。
///
/// 去重：`relogin_notified_at` 落盘后本轮登录态不再重复发（lead 口径的
/// 「已确认不再发」）。重登会刷新 `logged_in_at` 并清空该位，于是下一轮
/// 到期能重新提醒。
///
/// 只写**站内告警面**（`AlertRuleEvaluator::raise_event`：active + history），
/// **不经 NotifyService** —— 不动 Telegram/Email/Slack/Webhook 那 4 个渠道。
pub fn check_relogin(sessions: &AccountSessionStore, alerts: &SharedAlertEvaluator) -> u64 {
    let now = chrono::Utc::now().timestamp();
    let mut raised = 0u64;
    for session in sessions.all() {
        let (kind, level, message) = if session.relogin_required_pending() {
            (
                AlertKind::AccountReloginRequired {
                    account_id: session.id.clone(),
                },
                AlertLevel::Warning,
                format!("账号 {} 登录态已失效，需重新登录", session.name),
            )
        } else if session.relogin_reminder_pending(now) {
            // 「N 天后需重新登录」——天数给的是提醒文案的主体信息，算不出来
            // （无确证寿命）就不该走到这一支
            let days = session
                .relogin_due_at()
                .map(|due| (due - now + DAY_SECS - 1) / DAY_SECS)
                .unwrap_or(0);
            (
                AlertKind::AccountReloginSoon {
                    account_id: session.id.clone(),
                },
                AlertLevel::Warning,
                format!(
                    "账号 {} 登录态即将失效，请提前重登（{days} 天后到期）",
                    session.name
                ),
            )
        } else {
            continue;
        };
        // 先占发再落消息：`raise_event` 的静默窗口若压制了本次（同一账号
        // 24h 内已发过同名告警），就不写去重位 —— 下一轮还会重试，
        // 不会出现「标记了提醒但用户从没看到」的漏报。
        let fired = alerts
            .lock()
            .raise_event(&kind, level, message, NOTICE_SILENCE_SECS)
            .is_some();
        if fired {
            sessions.mark_relogin_notified(&session.id, now);
            raised += 1;
        }
    }
    raised
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_session::model::AccountSession;
    use crate::notify::alert::AlertRuleEvaluator;
    use crate::storage::FileStore;

    /// 独立账号池 + 独立告警评估器。凭证留空：`seal_or_keep` 对空串短路，
    /// 不碰全局 `secret::data_dir`（省掉跨模块换钥匙的锁麻烦）。
    fn harness() -> (AccountSessionStore, SharedAlertEvaluator, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let sessions = AccountSessionStore::new(Arc::new(FileStore::new(dir.path().to_path_buf())));
        let alerts: SharedAlertEvaluator =
            Arc::new(parking_lot::Mutex::new(AlertRuleEvaluator::new(Vec::new())));
        (sessions, alerts, dir)
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    /// 造一个 loomy 账号（`days_ago` 天前登录）。
    fn loomy(store: &AccountSessionStore, id: &str, name: &str, days_ago: i64) -> AccountSession {
        let mut session = AccountSession::new("ch-1", name);
        session.id = id.to_string();
        session.provider = "loomy".to_string();
        session.logged_in_at = Some(now() - days_ago * DAY_SECS);
        store.upsert(session).unwrap()
    }

    fn kinds(alerts: &SharedAlertEvaluator) -> Vec<AlertKind> {
        alerts
            .lock()
            .active_alerts()
            .into_iter()
            .map(|a| a.kind)
            .collect()
    }

    #[test]
    fn relogin_soon_fires_within_three_days() {
        // 到期前 3 天**命中**：12 天前登录（14 天寿命 → 剩 2 天）
        let (store, alerts, _dir) = harness();
        let account = loomy(&store, "a-soon", "loomy-main", 12);
        assert_eq!(check_relogin(&store, &alerts), 1);
        assert_eq!(
            kinds(&alerts),
            vec![AlertKind::AccountReloginSoon {
                account_id: account.id.clone()
            }]
        );
        let message = &alerts.lock().active_alerts()[0].message;
        assert!(message.contains("loomy-main"), "文案要带账号名：{message}");
        assert!(
            message.contains("2 天后到期"),
            "文案要带剩余天数：{message}"
        );
        assert!(message.contains("请提前重登"), "文案要给动作：{message}");
        assert_eq!(alerts.lock().active_alerts()[0].level, AlertLevel::Warning);
    }

    #[test]
    fn relogin_soon_does_not_fire_outside_window_or_without_ttl() {
        // **不命中**的两种：窗口外（剩 4 天）、以及没有确证寿命的家（openai）
        let (store, alerts, _dir) = harness();
        loomy(&store, "a-fresh", "loomy-fresh", 10);
        let mut other = AccountSession::new("ch-1", "openai-key");
        other.id = "b-fresh".to_string();
        other.provider = "openai".to_string();
        other.logged_in_at = Some(now() - 13 * DAY_SECS);
        store.upsert(other).unwrap();
        assert_eq!(check_relogin(&store, &alerts), 0);
        assert!(alerts.lock().active_alerts().is_empty());
    }

    #[test]
    fn relogin_soon_is_sent_once_per_login() {
        // 「已确认不再发」：去重位落盘后同一轮登录态不再重复提醒（含重启场景，
        // 因为写的是账号 KV 而不是内存）
        let (store, alerts, _dir) = harness();
        loomy(&store, "a-twice", "loomy-main", 12);
        assert_eq!(check_relogin(&store, &alerts), 1);
        assert_eq!(check_relogin(&store, &alerts), 0);
        assert_eq!(check_relogin(&store, &alerts), 0);
        assert_eq!(alerts.lock().active_alerts().len(), 1);
        assert!(store.get("a-twice").unwrap().relogin_notified_at.is_some());
        // 重登：刷新锚点 + 清去重位（真实路径是 upsert 换凭据 / 登录落盘）
        let mut again = store.get("a-twice").unwrap();
        again.logged_in_at = Some(now() - 12 * DAY_SECS);
        again.relogin_notified_at = None;
        store.upsert(again).unwrap();
        // 静默窗口（24h）内同名告警被压制，且**不占去重位**（否则等于
        // 「记了发过但没人看到」）
        assert_eq!(check_relogin(&store, &alerts), 0);
        assert!(store.get("a-twice").unwrap().relogin_notified_at.is_none());
    }

    #[test]
    fn needs_relogin_raises_required_alert_once() {
        // C1：连续三次被拒 → 状态已转 + 账号已逐出选路，这里补「让人看见」
        let (store, alerts, _dir) = harness();
        let account = loomy(&store, "a-dead", "loomy-dead", 2);
        assert!(!store.record_credential_rejection(&account.id, "upstream 401"));
        assert!(!store.record_credential_rejection(&account.id, "upstream 401"));
        assert!(store.record_credential_rejection(&account.id, "upstream 403"));
        assert_eq!(check_relogin(&store, &alerts), 1);
        assert_eq!(
            kinds(&alerts),
            vec![AlertKind::AccountReloginRequired {
                account_id: account.id.clone()
            }]
        );
        assert!(alerts.lock().active_alerts()[0]
            .message
            .contains("需重新登录"));
        assert_eq!(check_relogin(&store, &alerts), 0, "同一轮不重复发");
    }

    #[test]
    fn required_wins_over_soon_for_the_same_account() {
        // 同一账号既「已过 11 天」又被拒到 needs_relogin：只发 Required 一条，
        // 不发「快到期」这种自相矛盾的二次噪音
        let (store, alerts, _dir) = harness();
        let account = loomy(&store, "a-both", "loomy-both", 12);
        for _ in 0..3 {
            store.record_credential_rejection(&account.id, "upstream 401");
        }
        assert_eq!(check_relogin(&store, &alerts), 1);
        assert_eq!(kinds(&alerts).len(), 1);
        assert!(matches!(
            kinds(&alerts)[0],
            AlertKind::AccountReloginRequired { .. }
        ));
    }

    #[test]
    fn relogin_after_required_alert_rearms_for_another_death() {
        // 重登恢复 → 再次死掉 → 能再喊一次（人工置回 enabled 会清计数与去重位）
        let (store, alerts, _dir) = harness();
        let account = loomy(&store, "a-yoyo", "loomy-yoyo", 2);
        for _ in 0..3 {
            store.record_credential_rejection(&account.id, "upstream 401");
        }
        assert_eq!(check_relogin(&store, &alerts), 1);
        assert!(store.set_status(&account.id, "enabled"));
        for _ in 0..3 {
            store.record_credential_rejection(&account.id, "upstream 401");
        }
        assert!(store.get(&account.id).unwrap().needs_relogin());
        // 面板上先「已处理」掉旧告警（腾开同名静默位），再提醒新一轮
        assert!(alerts
            .lock()
            .resolve(&AlertKind::AccountReloginRequired {
                account_id: account.id.clone()
            })
            .is_some());
        assert_eq!(check_relogin(&store, &alerts), 1);
    }

    #[test]
    fn healthy_accounts_are_untouched() {
        // 零打扰：正常账号（新建、无锚点、非 loomy）不进任何提醒分支，
        // 也不该被写 `relogin_notified_at`
        let (store, alerts, _dir) = harness();
        let fresh = AccountSession::new("ch-1", "brand-new");
        let fresh_id = store.upsert(fresh).unwrap().id.clone();
        assert_eq!(check_relogin(&store, &alerts), 0);
        assert!(alerts.lock().active_alerts().is_empty());
        assert!(store.get(&fresh_id).unwrap().relogin_notified_at.is_none());
    }
}
