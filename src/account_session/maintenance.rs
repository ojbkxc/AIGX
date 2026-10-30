//! 账号池周期维护 —— 由 `cron::Scheduler` 周期调用。
//!
//! 职责：清理已到期的冷却记录（避免 `cooldowns` 无限膨胀）、按 `expires_at`
//! 同步凭证过期状态（过期 → `expired`，续期后自动恢复 `enabled`）。

use super::store::AccountSessionStore;

/// 执行一轮维护，返回本轮变更的账号数（0 = 无事发生，cron 日志降噪）。
pub fn sweep(sessions: &AccountSessionStore) -> u64 {
    sessions.sweep(chrono::Utc::now().timestamp())
}
