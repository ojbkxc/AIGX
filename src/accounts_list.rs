//! 账号列表查询与分页（对齐 Agent2API v3.5.0-beta.2 `core/accounts_list.rs`）。
//!
//! 参考树的 `AccountListQuery` 支持关键词、provider 多选、启用/状态/到期/用量过滤、
//! 指定 id、折叠 provider、移动端口径、只取元数据、时区偏移等；`summary()` 给计数，
//! `page()` 给过滤 + 排序 + 分页后的列表与分组统计，`detail()` 给单账号详情。
//!
//! AIGX 的账号来自 [`AccountSessionStore::all`]（内存 + FileStore 落盘）。这里在
//! `AccountSession` 之上实现同一套查询契约：
//! - 启用 = `status != "disabled"`；到期 = `expires_at` 在过去；
//! - 限流中（limited）= 任一 `cooldowns` 到期时间 > now；
//! - 用量状态：有错误 → `failed`，有用量 → `has`，否则 `unqueried`。
//!
//! ## beta.2 增量
//! - `page()` 返回 `counts`（双级分段计数）与 `mobileCounts`（移动端口径）；
//! - `account_json()` 新增 `needsRelogin` / `available` / `hidden` / `rateLimits` /
//!   `balance` / `balanceError` / `allowRouteSkip` / `updatedAt` / `createdAt` 字段；
//! - `usage_state()` 新增 `failed` 状态（`last_balance_error` 非空时）；
//! - `detail()` 单账号详情函数。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::account_session::model::AccountSession;

fn default_page_size() -> usize {
    50
}

/// 账号列表查询（camelCase，与前端/A2A 同口径）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountListQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default)]
    pub providers: Vec<String>,
    #[serde(default)]
    pub enabled: String,
    #[serde(default)]
    pub limit: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub expiry: String,
    #[serde(default)]
    pub usage: String,
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub collapsed: Vec<String>,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
    #[serde(default)]
    pub mobile: bool,
    #[serde(default)]
    pub group_total_filtered: bool,
    #[serde(default)]
    pub metadata_only: bool,
    #[serde(default)]
    pub utc_offset_minutes: i32,
}

/// 汇总计数。
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccountCounts {
    pub total: usize,
    pub enabled: usize,
    pub disabled: usize,
    pub limited: usize,
    pub available: usize,
}

fn is_enabled(session: &AccountSession) -> bool {
    session.status != "disabled"
}

fn is_limited(session: &AccountSession, now: i64) -> bool {
    session.cooldowns.values().any(|until| *until > now)
}

fn is_expired(session: &AccountSession, now: i64) -> bool {
    session.expires_at.is_some_and(|at| at > 0 && at <= now)
}

/// 7 天内到期（对齐上游 `soon` 判定）。
fn is_expiring_soon(session: &AccountSession, now: i64) -> bool {
    session
        .expires_at
        .is_some_and(|at| at > now && at <= now + 7 * 86_400)
}

/// 长期有效（无过期时间或 0）。
fn is_long_lived(session: &AccountSession) -> bool {
    session.expires_at.map(|at| at <= 0).unwrap_or(true)
}

fn usage_state(session: &AccountSession) -> &'static str {
    // 对齐上游 `usage_state`：有错误 → "failed"，有用量 → "has"，否则 "unqueried"。
    if session.last_balance_error.is_some() {
        "failed"
    } else if session.last_balance.is_some()
        || session.success_count > 0
        || session.last_used_at.is_some()
    {
        "has"
    } else {
        "unqueried"
    }
}

fn matches_keyword(session: &AccountSession, keyword: &str) -> bool {
    if keyword.is_empty() {
        return true;
    }
    let needle = keyword.to_lowercase();
    [
        session.name.as_str(),
        session.id.as_str(),
        session.provider.as_str(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&needle))
}

/// 汇总（不做过滤，仅按可见账号统计）。
pub fn summary(sessions: &[AccountSession], now: i64) -> AccountCounts {
    let mut counts = AccountCounts::default();
    for session in sessions {
        counts.total += 1;
        if is_enabled(session) {
            counts.enabled += 1;
            if is_limited(session, now) {
                counts.limited += 1;
            } else {
                counts.available += 1;
            }
        } else {
            counts.disabled += 1;
        }
    }
    counts
}

/// 单账号详情（对齐上游 `accounts_list::detail`）。
///
/// 返回 `None` 表示账号不存在。调用方根据 `None` 返回 404。
pub fn detail(sessions: &[AccountSession], id: &str, now: i64) -> Option<Value> {
    sessions
        .iter()
        .find(|session| session.id == id)
        .map(|session| {
            let mut json = account_json(session, now);
            json["detailsLoaded"] = json!(true);
            json
        })
}

fn account_json(session: &AccountSession, now: i64) -> Value {
    let limited = is_limited(session, now);
    let enabled = is_enabled(session);
    json!({
        "id": session.id,
        "name": session.name,
        "provider": session.provider,
        "providerLabel": session.provider,
        "channelId": session.channel_id,
        "enabled": enabled,
        "status": session.status,
        "expiresAt": session.expires_at,
        "expired": is_expired(session, now),
        "listLimited": limited,
        "needsRelogin": session.needs_relogin || session.status == "needs_relogin",
        "available": enabled && !limited && !session.needs_relogin,
        "successCount": session.success_count,
        "errorCount": session.error_count,
        "lastUsedAt": session.last_used_at,
        "usageState": usage_state(session),
        "detailsLoaded": true,
        "isGuest": false,
        "hidden": session.hidden,
        "rateLimits": session.rate_limits,
        "balance": session.last_balance,
        "balanceError": session.last_balance_error,
        "allowRouteSkip": session.allow_route_skip,
        "updatedAt": session.updated_at,
        "createdAt": session.created_at,
    })
}

/// 分页查询。`ids_only = true` 时只返回匹配的 id 列表。
pub fn page(
    sessions: &[AccountSession],
    query: &AccountListQuery,
    ids_only: bool,
    now: i64,
) -> Value {
    let keyword = query.q.trim().to_lowercase();
    let ids: std::collections::HashSet<&str> = query.ids.iter().map(String::as_str).collect();
    let mut matched: Vec<&AccountSession> = Vec::new();
    let mut groups: std::collections::BTreeMap<String, (usize, usize, usize, usize)> =
        std::collections::BTreeMap::new();
    for session in sessions {
        let enabled = is_enabled(session);
        let limited = is_limited(session, now);
        let expired = is_expired(session, now);
        let entry = groups
            .entry(session.provider.clone())
            .or_insert((0, 0, 0, 0));
        entry.0 += 1;
        if enabled {
            entry.1 += 1;
        } else {
            entry.2 += 1;
        }
        if enabled && limited {
            entry.3 += 1;
        }

        let search = matches_keyword(session, &keyword);
        let provider_match =
            query.providers.is_empty() || query.providers.iter().any(|p| p == &session.provider);
        let enabled_match = match query.enabled.as_str() {
            "enabled" => enabled,
            "disabled" => !enabled,
            _ => true,
        };
        let status_match = match query.status.as_str() {
            "enabled" => enabled,
            "disabled" => !enabled,
            "limited" => enabled && limited,
            "relogin" => session.status == "needs_relogin",
            "expired" => expired,
            _ => true,
        };
        let expiry_match = match query.expiry.as_str() {
            "expired" => expired,
            "soon" => is_expiring_soon(session, now),
            "longlived" => is_long_lived(session),
            _ => true,
        };
        let limit_match = match query.limit.as_str() {
            "limited" => enabled && limited,
            "normal" => enabled && !limited,
            _ => true,
        };
        let usage_match =
            query.usage.is_empty() || query.usage == "all" || query.usage == usage_state(session);
        let id_match = ids.is_empty() || ids.contains(session.id.as_str());
        if search
            && provider_match
            && enabled_match
            && status_match
            && expiry_match
            && limit_match
            && usage_match
            && id_match
        {
            matched.push(session);
        }
    }

    let total = matched.len();
    if ids_only {
        return json!({
            "ids": matched.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            "total": total,
        });
    }

    let display: Vec<&AccountSession> = matched
        .iter()
        .copied()
        .filter(|session| !query.collapsed.iter().any(|p| p == &session.provider))
        .collect();
    let display_total = display.len();
    let size = query.page_size.clamp(1, 200);
    let offset = if display_total == 0 {
        0
    } else {
        query.offset.min((display_total - 1) / size * size)
    };
    let page_items: Vec<Value> = if query.metadata_only {
        Vec::new()
    } else {
        display
            .iter()
            .skip(offset)
            .take(size)
            .map(|session| account_json(session, now))
            .collect()
    };
    // ── 双级计数（对齐上游 `counts` / `mobileCounts`）──────────
    //
    // `counts` 是状态/限额双级分段的计数：
    //   - enabledAll/disabled = 按 enabled 维度
    //   - limitAll/normal/limited = 按 limit 维度（仅 enabled 账号参与）
    // `mobile_counts` 在 `query.mobile = true` 时同口径（上游移动端单独请求）。
    let mut enabled_all = 0usize;
    let mut disabled_count = 0usize;
    let mut limit_all = 0usize;
    let mut normal_count = 0usize;
    let mut limited_count = 0usize;
    for session in sessions {
        let en = is_enabled(session);
        let lim = is_limited(session, now);
        if en {
            enabled_all += 1;
            limit_all += 1;
            if lim {
                limited_count += 1;
            } else {
                normal_count += 1;
            }
        } else {
            disabled_count += 1;
        }
    }
    let counts = json!({
        "enabledAll": enabled_all,
        "enabled": enabled_all,
        "disabled": disabled_count,
        "limitAll": limit_all,
        "normal": normal_count,
        "limited": limited_count,
    });
    let mobile_counts = if query.mobile {
        json!({
            "enabledAll": enabled_all,
            "enabled": enabled_all,
            "disabled": disabled_count,
            "limitAll": limit_all,
            "normal": normal_count,
            "limited": limited_count,
        })
    } else {
        Value::Null
    };

    let group_json: Vec<Value> = groups
        .iter()
        .map(|(provider, (total, enabled, disabled, limited))| {
            json!({
                "provider": provider,
                "rank": 1,
                "total": total,
                "enabled": enabled,
                "disabled": disabled,
                "limited": limited,
                "matched": matched.iter().filter(|s| &s.provider == provider).count(),
            })
        })
        .collect();
    json!({
        "accounts": page_items,
        "matchedIds": matched.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        "displayIds": display.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        "summary": summary(sessions, now),
        "counts": counts,
        "mobileCounts": mobile_counts,
        "groups": group_json,
        "total": total,
        "displayTotal": display_total,
        "offset": offset,
        "pageSize": size,
        "indexOnly": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, provider: &str, status: &str) -> AccountSession {
        let mut s = AccountSession::new("ch", id);
        s.id = id.to_string();
        s.provider = provider.to_string();
        s.status = status.to_string();
        s
    }

    #[test]
    fn summary_counts_enabled_disabled_and_limited() {
        let now = 1000;
        let mut limited = session("b", "acme", "enabled");
        limited.cooldowns.insert("*".into(), now + 100);
        let sessions = vec![
            session("a", "acme", "enabled"),
            limited,
            session("c", "acme", "disabled"),
        ];
        let counts = summary(&sessions, now);
        assert_eq!(counts.total, 3);
        assert_eq!(counts.enabled, 2);
        assert_eq!(counts.disabled, 1);
        assert_eq!(counts.limited, 1);
        assert_eq!(counts.available, 1);
    }

    #[test]
    fn page_filters_by_provider_and_status_and_paginates() {
        let now = 1000;
        let sessions = vec![
            session("a", "acme", "enabled"),
            session("b", "acme", "disabled"),
            session("c", "other", "enabled"),
        ];
        let query = AccountListQuery {
            providers: vec!["acme".into()],
            enabled: "enabled".into(),
            page_size: 1,
            ..Default::default()
        };
        let out = page(&sessions, &query, false, now);
        assert_eq!(out["total"], json!(1));
        assert_eq!(out["accounts"].as_array().unwrap().len(), 1);
        assert_eq!(out["accounts"][0]["id"], json!("a"));
    }

    #[test]
    fn ids_only_returns_ids() {
        let now = 1000;
        let sessions = vec![
            session("a", "acme", "enabled"),
            session("b", "acme", "enabled"),
        ];
        let query = AccountListQuery {
            ids: vec!["b".into()],
            ..Default::default()
        };
        let out = page(&sessions, &query, true, now);
        assert_eq!(out["ids"], json!(["b"]));
        assert_eq!(out["total"], json!(1));
    }

    #[test]
    fn detail_returns_account_with_details_loaded() {
        let now = 1000;
        let sessions = vec![
            session("a", "acme", "enabled"),
            session("b", "acme", "disabled"),
        ];
        let detail = detail(&sessions, "a", now).unwrap();
        assert_eq!(detail["id"], json!("a"));
        assert_eq!(detail["detailsLoaded"], json!(true));
        assert_eq!(detail["needsRelogin"], json!(false));
        assert_eq!(detail["available"], json!(true));
    }

    #[test]
    fn detail_returns_none_for_missing() {
        let now = 1000;
        let sessions = vec![session("a", "acme", "enabled")];
        assert!(detail(&sessions, "nonexistent", now).is_none());
    }

    #[test]
    fn page_includes_counts_and_mobile_counts() {
        let now = 1000;
        let mut limited = session("b", "acme", "enabled");
        limited.cooldowns.insert("*".into(), now + 100);
        let sessions = vec![
            session("a", "acme", "enabled"),
            limited,
            session("c", "acme", "disabled"),
        ];
        let query = AccountListQuery::default();
        let out = page(&sessions, &query, false, now);
        assert_eq!(out["counts"]["enabledAll"], json!(2));
        assert_eq!(out["counts"]["disabled"], json!(1));
        assert_eq!(out["counts"]["limited"], json!(1));
        assert_eq!(out["counts"]["normal"], json!(1));
        // mobile = false → mobileCounts is null
        assert!(out["mobileCounts"].is_null());
    }

    #[test]
    fn page_mobile_counts_when_mobile_query() {
        let now = 1000;
        let sessions = vec![
            session("a", "acme", "enabled"),
            session("b", "acme", "disabled"),
        ];
        let query = AccountListQuery {
            mobile: true,
            ..Default::default()
        };
        let out = page(&sessions, &query, false, now);
        assert_eq!(out["mobileCounts"]["enabledAll"], json!(1));
        assert_eq!(out["mobileCounts"]["disabled"], json!(1));
    }

    #[test]
    fn usage_state_failed_when_balance_error() {
        let mut s = session("a", "acme", "enabled");
        s.last_balance_error = Some("timeout".into());
        assert_eq!(usage_state(&s), "failed");
    }
}
