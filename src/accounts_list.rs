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
//!
//! ## beta.3 增量（账号模块对齐）
//! - `page_with_sorting()`：按服务端排序（`AccountSorting::account_order`）输出，
//!   与选路先试次序同源；`page()` 保留无排序快照的旧入口；
//! - `groups[].balance`：逐组余额汇总（A2A `accounts_list/balance.rs` 口径：
//!   counted/sum/unlimited/failed/uncountable/pending/partial/ratio/expiring）；
//! - `mobileCounts` 细分：`{status:{all,enabled,disabled,limited,relogin,expired},
//!   expiry:{all,soon,expired,longlived}, usage:{all,has,unqueried,failed}}`
//!   （`query.mobile = true` 时 `counts` 也切换成这份嵌套结构，与上游
//!   `snapshot["counts"] = mobile_counts` 同口径）；
//! - `summary.available` 与上游 `count()` 对齐：启用且非 needs_relogin 计数。

// 本模块的公开 API 覆盖 beta.2 的完整契约面，部分符号为后续接线/线表预留。
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

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
///
/// `available` 与上游 `accounts_list::count()` 对齐：**启用且非
/// needs_relogin** 即计入，不受冷却影响（冷却只影响 `limited`，不把账号
/// 从「有凭证可用」里抹掉）。
pub fn summary(sessions: &[AccountSession], now: i64) -> AccountCounts {
    let mut counts = AccountCounts::default();
    for session in sessions {
        counts.total += 1;
        if is_enabled(session) {
            counts.enabled += 1;
            if is_limited(session, now) {
                counts.limited += 1;
            }
            if session.status != crate::account_session::model::STATUS_NEEDS_RELOGIN
                && !session.needs_relogin
            {
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

pub(crate) fn account_json(session: &AccountSession, now: i64) -> Value {
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
        // A2A 兼容形态：每账号余额查询配置与余额不足处理。
        // `usageQuery.interval` 单位为秒；AIGX 关闭态 = 无间隔（enabled:false）。
        "usageQuery": {
            "enabled": session.usage_query_interval_secs.is_some_and(|v| v > 0),
            "interval": session.usage_query_interval_secs.unwrap_or(0),
            "retries": session.usage_query.normalized().retries,
            "timeoutSeconds": session.usage_query.normalized().timeout_seconds,
        },
        "lowBalance": session.low_balance_view(),
        "allowRouteSkip": session.allow_route_skip,
        "updatedAt": session.updated_at,
        "createdAt": session.created_at,
    })
}

/// 分页查询。`ids_only = true` 时只返回匹配的 id 列表。
///
/// 兼容入口：不注入服务端排序快照（按传入切片顺序输出）。HTTP 三件套
/// （list/query/selection）走 [`page_with_sorting`]，与选路先试次序一致。
pub fn page(
    sessions: &[AccountSession],
    query: &AccountListQuery,
    ids_only: bool,
    now: i64,
) -> Value {
    page_with_sorting(sessions, query, ids_only, now, None)
}

/// 分页查询（带服务端排序快照）。
///
/// `sorting` 的 `account_order`（providerOrder → accountOrder 展平）就是
/// 调度先试次序；展示序与它一致，前端不再自己重排（对齐 A2A
/// `core::accounts_list::page` 的位置表）。未提供的账号按原顺序垫底。
pub fn page_with_sorting(
    sessions: &[AccountSession],
    query: &AccountListQuery,
    ids_only: bool,
    now: i64,
    sorting: Option<&crate::account_session::sorting::AccountSorting>,
) -> Value {
    let keyword = query.q.trim().to_lowercase();
    let ids: HashSet<&str> = query.ids.iter().map(String::as_str).collect();
    let positions: HashMap<&str, usize> = sorting
        .map(|sorting| {
            sorting
                .providers
                .iter()
                .flat_map(|provider| sorting.account_order.get(provider).into_iter().flatten())
                .enumerate()
                .map(|(index, id)| (id.as_str(), index))
                .collect()
        })
        .unwrap_or_default();
    let mut ordered: Vec<&AccountSession> = sessions.iter().collect();
    if !positions.is_empty() {
        ordered.sort_by_key(|session| {
            positions
                .get(session.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
    }

    let mut matched: Vec<&AccountSession> = Vec::new();
    // 分组按「排序后首次出现」建行，rank = 建行序（对齐上游 groups.push 口径）。
    let mut groups: Vec<Value> = Vec::new();
    let mut group_indexes: HashMap<String, usize> = HashMap::new();
    let mut counts = json!({
        "enabledAll": 0, "enabled": 0, "disabled": 0,
        "limitAll": 0, "normal": 0, "limited": 0,
    });
    // 移动端口径的细分计数：状态 / 到期 / 用量三段（对齐上游 mobile_counts）。
    let mut mobile_counts = json!({
        "status": { "all": 0, "enabled": 0, "disabled": 0, "limited": 0, "relogin": 0, "expired": 0 },
        "expiry": { "all": 0, "soon": 0, "expired": 0, "longlived": 0 },
        "usage": { "all": 0, "has": 0, "unqueried": 0, "failed": 0 },
    });
    for session in &ordered {
        let enabled = is_enabled(session);
        let limited = is_limited(session, now);
        let expired = is_expired(session, now);
        let soon = is_expiring_soon(session, now);
        let long_lived = is_long_lived(session);
        let needs_relogin = session.needs_relogin
            || session.status == crate::account_session::model::STATUS_NEEDS_RELOGIN;
        let group_index = *group_indexes
            .entry(session.provider.clone())
            .or_insert_with(|| {
                groups.push(json!({
                    "provider": session.provider.clone(),
                    "rank": groups.len() + 1,
                    "total": 0,
                    "enabled": 0,
                    "disabled": 0,
                    "limited": 0,
                    "matched": 0,
                }));
                groups.len() - 1
            });
        increment(&mut groups[group_index], "total");
        increment(
            &mut groups[group_index],
            if enabled { "enabled" } else { "disabled" },
        );
        if enabled && limited {
            increment(&mut groups[group_index], "limited");
        }

        let search = matches_keyword(session, &keyword);
        let provider_match =
            query.providers.is_empty() || query.providers.iter().any(|p| p == &session.provider);
        // 移动端细分：只看关键词（不看 provider 下拉——那是桌面端的筛选）。
        if query.mobile && search {
            let status = &mut mobile_counts["status"];
            increment(status, "all");
            increment(status, if enabled { "enabled" } else { "disabled" });
            if enabled && limited {
                increment(status, "limited");
            }
            if needs_relogin {
                increment(status, "relogin");
            }
            if expired {
                increment(status, "expired");
            }
            let expiry = &mut mobile_counts["expiry"];
            increment(expiry, "all");
            if soon {
                increment(expiry, "soon");
            }
            if expired {
                increment(expiry, "expired");
            }
            if long_lived {
                increment(expiry, "longlived");
            }
            let usage = &mut mobile_counts["usage"];
            increment(usage, "all");
            increment(usage, usage_state(session));
        }
        // 桌面端双级计数：关键词 + provider 命中后的 enabled 维度；
        // `limitAll` 再按 enabled 筛选细分（与上游同口径）。
        let enabled_match = match query.enabled.as_str() {
            "enabled" => enabled,
            "disabled" => !enabled,
            _ => true,
        };
        if search && provider_match {
            increment(&mut counts, "enabledAll");
            increment(&mut counts, if enabled { "enabled" } else { "disabled" });
            if enabled_match {
                increment(&mut counts, "limitAll");
                if enabled {
                    increment(&mut counts, if limited { "limited" } else { "normal" });
                }
            }
        }

        let status_match = match query.status.as_str() {
            "enabled" => enabled,
            "disabled" => !enabled,
            "limited" => enabled && limited,
            "relogin" => needs_relogin,
            "expired" => expired,
            _ => true,
        };
        let expiry_match = match query.expiry.as_str() {
            "expired" => expired,
            "soon" => soon,
            "longlived" => long_lived,
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
            increment(&mut groups[group_index], "matched");
            matched.push(*session);
        }
    }

    let total = matched.len();
    let sorting_json = sorting
        .map(|sorting| sorting.to_json(now))
        .unwrap_or(Value::Null);
    if ids_only {
        return json!({
            "ids": matched.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            "total": total,
            "sorting": sorting_json,
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

    // ── 逐组余额汇总（A2A `accounts_list/balance.rs` 口径）──────────
    //
    // `groupTotalFiltered=true` 时汇总**命中的成员**（工具栏「按当前筛选」），
    // 否则汇总整组。形状含 counted/sum/unit/unlimited/failed/uncountable/
    // pending/partial/ratio/expiring；前端分组带据此渲染组余额徽章。
    let balance_members: &[&AccountSession] = if query.group_total_filtered {
        &matched
    } else {
        &ordered
    };
    for group in &mut groups {
        let provider = group["provider"].as_str().unwrap_or("").to_string();
        let members: Vec<&AccountSession> = balance_members
            .iter()
            .copied()
            .filter(|session| session.provider == provider)
            .collect();
        group["balance"] = balance_summary(
            &members,
            query.group_total_filtered,
            now,
            query.utc_offset_minutes,
        );
    }

    // `mobile=true` 时 `counts` 切换成嵌套细分（上游 `snapshot["counts"] =
    // mobile_counts`）；`mobileCounts` 额外保留一份，供只看移动端口径的调用方。
    let counts_json = if query.mobile {
        mobile_counts.clone()
    } else {
        counts
    };
    let mobile_json = if query.mobile {
        mobile_counts
    } else {
        Value::Null
    };
    json!({
        "accounts": page_items,
        "matchedIds": matched.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        "displayIds": display.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
        "summary": summary(sessions, now),
        "counts": counts_json,
        "mobileCounts": mobile_json,
        "groups": groups,
        "sorting": sorting_json,
        "total": total,
        "displayTotal": display_total,
        "offset": offset,
        "pageSize": size,
        "indexOnly": false,
    })
}

fn increment(value: &mut Value, key: &str) {
    value[key] = json!(value[key].as_u64().unwrap_or(0) + 1);
}

/// JSON 数字（数字或数字字符串）。
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value| value.is_finite())
}

/// 「这家没有余额读数」类错误（不计失败，归 uncountable，与上游
/// `usage_not_configured` / `usage_unavailable` 同义）。
fn is_uncountable_error(message: &str) -> bool {
    [
        "不支持",
        "未公开",
        "没有余额",
        "无余额",
        "usage_not_configured",
        "usage_unavailable",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// 组余额汇总（`members` 为该组账号；`filtered` 决定 `scope` 取值）。
fn balance_summary(
    members: &[&AccountSession],
    filtered: bool,
    now: i64,
    offset_minutes: i32,
) -> Value {
    let mut total = 0usize;
    let mut counted = 0usize;
    let mut failed = 0usize;
    let mut pending = 0usize;
    let mut uncountable = 0usize;
    let mut partial = 0usize;
    let mut unlimited = 0usize;
    let mut sum = 0.0f64;
    let mut unit = String::new();
    let mut additive: Option<bool> = None;
    let mut ratio: Option<Value> = None;
    let mut packs: Vec<Value> = Vec::new();
    for session in members {
        total += 1;
        if let Some(error) = session.last_balance_error.as_deref() {
            if is_uncountable_error(error) {
                uncountable += 1;
            } else {
                failed += 1;
            }
            continue;
        }
        let Some(balance) = session.last_balance.as_ref() else {
            pending += 1;
            continue;
        };
        if balance
            .get("statisticsError")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
            || balance
                .get("warnings")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.is_empty())
        {
            partial += 1;
        }
        packs.extend(expiring_packs(balance, now));
        if balance.get("unlimited").and_then(Value::as_bool) == Some(true) {
            unlimited += 1;
            continue;
        }
        let value = number(balance.get("totalLeft").unwrap_or(&balance["available"]));
        let candidate = ratio_of(balance, value);
        let next_unit = balance
            .get("unit")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("积分");
        let next_additive = next_unit != "%" && value.is_some();
        if value.is_none() && candidate.is_none() {
            uncountable += 1;
            continue;
        }
        if additive.is_some_and(|previous| previous != next_additive)
            || next_additive && !unit.is_empty() && next_unit != unit
        {
            uncountable += 1;
            continue;
        }
        additive = Some(next_additive);
        if unit.is_empty() {
            unit = next_unit.to_string();
        }
        counted += 1;
        if next_additive {
            sum += value.unwrap_or(0.0);
        } else if candidate.as_ref().is_some_and(|candidate| {
            ratio
                .as_ref()
                .is_none_or(|previous| number(&candidate["percent"]) < number(&previous["percent"]))
        }) {
            ratio = candidate;
        }
    }
    let tomorrow = (now + i64::from(offset_minutes) * 60_000).div_euclid(86_400_000) * 86_400_000
        + 86_400_000
        - i64::from(offset_minutes) * 60_000;
    let today: Vec<Value> = packs
        .iter()
        .filter(|pack| pack["expireAt"].as_i64().unwrap_or(0) < tomorrow)
        .cloned()
        .collect();
    json!({
        "counted": counted,
        "total": total,
        "sum": if counted > 0 && additive != Some(false) { Some((sum * 100.0).round() / 100.0) } else { None },
        "unlimited": unlimited,
        "unit": unit,
        "failed": failed,
        "uncountable": uncountable,
        "pending": pending,
        "partial": partial,
        "scope": if filtered { "filtered" } else { "group" },
        "additive": additive != Some(false),
        "ratio": ratio,
        "expiring": {
            "count": packs.len(),
            "todayCount": today.len(),
            "amounts": expiring_amounts(&packs),
            "todayAmounts": expiring_amounts(&today),
        },
    })
}

/// 逐份「7 天内到期」额度包（AIGX 的 `wallets[]` / A2A 的 `credits[]` 双读）。
fn expiring_packs(balance: &Value, now: i64) -> Vec<Value> {
    let unit = balance.get("unit").and_then(Value::as_str).unwrap_or(
        if balance.get("totalLeft").is_some() {
            "积分"
        } else {
            ""
        },
    );
    let rows = balance
        .get("credits")
        .and_then(Value::as_array)
        .or_else(|| balance.get("wallets").and_then(Value::as_array));
    let Some(rows) = rows else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|pack| {
            for key in ["snapshot", "record", "daily", "rolling", "unlimited"] {
                if pack.get(key).and_then(Value::as_bool) == Some(true) {
                    return None;
                }
            }
            let remain = number(pack.get("remain").unwrap_or(&pack["balance"]));
            let expiry = pack
                .get("expireAt")
                .or_else(|| pack.get("expire_at"))
                .and_then(number)? as i64;
            let total = number(&pack["total"]);
            let used = number(&pack["used"]);
            if remain.is_some_and(|value| value <= 0.0)
                || total == Some(-1.0)
                || remain.is_none()
                    && (number(&pack["remainingPercent"]).is_some_and(|value| value <= 0.0)
                        || total
                            .zip(used)
                            .is_some_and(|(total, used)| total > 0.0 && used >= total))
                || number(&pack["startAt"]).is_some_and(|value| value > now as f64)
                || expiry <= now
                || expiry > now + 7 * 86_400
            {
                return None;
            }
            Some(json!({
                "expireAt": expiry,
                "remain": remain,
                "unit": pack.get("unit").and_then(Value::as_str).unwrap_or(unit),
            }))
        })
        .collect()
}

fn expiring_amounts(packs: &[Value]) -> Vec<Value> {
    let mut grouped = std::collections::BTreeMap::<String, (usize, usize, Option<f64>)>::new();
    for pack in packs {
        let unit = pack.get("unit").and_then(Value::as_str).unwrap_or("");
        let unit = if matches!(
            unit.to_ascii_lowercase().as_str(),
            "credit" | "credits" | "积分"
        ) {
            "积分"
        } else {
            unit
        };
        let item = grouped.entry(unit.to_string()).or_default();
        item.0 += 1;
        let remain = number(&pack["remain"]);
        if remain.is_none() {
            item.1 += 1;
        }
        if !unit.is_empty() && unit != "%" {
            if let Some(remain) = remain {
                item.2 = Some(item.2.unwrap_or(0.0) + remain);
            }
        }
    }
    grouped
        .into_iter()
        .map(|(unit, (count, unknown_count, sum))| {
            json!({ "unit": unit, "count": count, "unknownCount": unknown_count, "sum": sum })
        })
        .collect()
}

/// 百分比读数（A2A `ratio_of` 口径：短窗口被周/月额度耗尽时按 0% 计）。
fn ratio_of(balance: &Value, value: Option<f64>) -> Option<Value> {
    let rows: Vec<&Value> = balance
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|row| row["aside"] != true)
        .collect();
    let exhausted: HashSet<&str> = rows
        .iter()
        .filter(|row| {
            matches!(row["window"].as_str(), Some("weekly" | "monthly"))
                && row["unlimited"] != true
                && (number(&row["remainingPercent"]).is_some_and(|value| value <= 0.0)
                    || number(&row["remaining"]).is_some_and(|value| value <= 0.0))
        })
        .map(|row| row["group"].as_str().unwrap_or(""))
        .collect();
    let mut adjusted = false;
    let windows: Vec<Value> = rows
        .iter()
        .map(|row| {
            let short = matches!(row["window"].as_str(), Some("daily" | "5h"));
            let blocked = short && exhausted.contains(row["group"].as_str().unwrap_or(""));
            let percent = number(&row["remainingPercent"]).map(|value| value.clamp(0.0, 100.0));
            if blocked && (row["unlimited"] == true || percent != Some(0.0)) {
                adjusted = true;
            }
            let label = [
                row["group"].as_str().unwrap_or(""),
                row["window"]
                    .as_str()
                    .or(row["label"].as_str())
                    .unwrap_or(""),
            ]
            .into_iter()
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
            json!({
                "label": label,
                "percent": if blocked { Some(0.0) } else { percent },
                "unlimited": !blocked && row["unlimited"] == true,
                "shortWindow": short,
            })
        })
        .collect();
    let short = windows.iter().any(|row| {
        row["shortWindow"] == true
            && (row["unlimited"] == true || number(&row["percent"]).is_some())
    });
    let percent = windows
        .iter()
        .filter(|row| !short || row["shortWindow"] == true)
        .filter_map(|row| {
            if row["unlimited"] == true {
                Some(100.0)
            } else {
                number(&row["percent"])
            }
        })
        .reduce(f64::min);
    if let Some(percent) = percent {
        return Some(json!({
            "percent": percent,
            "hasShortWindow": short,
            "adjusted": adjusted,
            "windows": windows,
        }));
    }
    value.map(|value| {
        json!({
            "percent": value.clamp(0.0, 100.0),
            "hasShortWindow": false,
            "adjusted": false,
            "windows": [{
                "label": "",
                "percent": value.clamp(0.0, 100.0),
                "unlimited": false,
            }],
        })
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
        // 上游 `count()` 口径：available = 启用且非 needs_relogin，冷却不扣
        assert_eq!(counts.available, 2);
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
        // 上游双级计数：enabledAll 是关键词+provider 命中数；limitAll 再按
        // enabled 筛选细分
        assert_eq!(out["counts"]["enabledAll"], json!(3));
        assert_eq!(out["counts"]["enabled"], json!(2));
        assert_eq!(out["counts"]["disabled"], json!(1));
        assert_eq!(out["counts"]["limitAll"], json!(3));
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
        // 细分三段：status / expiry / usage（上游 mobile_counts 形状）
        assert_eq!(out["mobileCounts"]["status"]["all"], json!(2));
        assert_eq!(out["mobileCounts"]["status"]["enabled"], json!(1));
        assert_eq!(out["mobileCounts"]["status"]["disabled"], json!(1));
        assert_eq!(out["mobileCounts"]["expiry"]["all"], json!(2));
        assert_eq!(out["mobileCounts"]["expiry"]["longlived"], json!(2));
        assert_eq!(out["mobileCounts"]["usage"]["all"], json!(2));
        assert_eq!(out["mobileCounts"]["usage"]["unqueried"], json!(2));
        // mobile = true 时 counts 也切成嵌套细分（上游 snapshot["counts"] 口径）
        assert_eq!(out["counts"]["status"]["all"], json!(2));
        assert!(out["counts"].get("enabledAll").is_none());
    }

    #[test]
    fn page_orders_by_server_sorting_snapshot() {
        let now = 1000;
        let sessions = vec![
            session("a", "acme", "enabled"),
            session("b", "acme", "enabled"),
            session("c", "acme", "enabled"),
        ];
        let settings = crate::account_session::sorting::AccountSortSettings::default();
        let sorting = crate::account_session::sorting::AccountSorting::build(&sessions, settings);
        // 默认规则 = expires 升序；三把账号都没 expires_at → 回落到 priority/sort_order/
        // created_at 稳定序。手工改写排序表验证「按 account_order 输出」。
        let mut sorting = sorting;
        sorting
            .account_order
            .insert("acme".to_string(), vec!["c".into(), "a".into(), "b".into()]);
        // `#[derive(Default)]` 的 usize 字段是 0（serde 缺省 50 只作用于反序列化）
        let query = AccountListQuery {
            page_size: 50,
            ..Default::default()
        };
        let out = page_with_sorting(&sessions, &query, false, now, Some(&sorting));
        assert_eq!(
            out["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
        assert_eq!(out["groups"][0]["rank"], json!(1));
        assert_eq!(out["groups"][0]["provider"], json!("acme"));
    }

    #[test]
    fn group_balance_summarizes_last_balance() {
        let now = 1_700_000_000;
        let mut a = session("a", "acme", "enabled");
        a.last_balance = Some(json!({
            "available": 10.0,
            "unit": "积分",
            "wallets": [
                { "total": 100, "remain": 10, "expireAt": now + 3600 },
            ],
        }));
        let mut b = session("b", "acme", "enabled");
        b.last_balance = Some(json!({ "available": 5.0, "unit": "积分" }));
        let mut failed = session("c", "acme", "enabled");
        failed.last_balance_error = Some("timeout".into());
        let sessions = vec![a, b, failed];
        let query = AccountListQuery::default();
        let out = page(&sessions, &query, false, now);
        let balance = &out["groups"][0]["balance"];
        assert_eq!(balance["total"], json!(3));
        assert_eq!(balance["counted"], json!(2));
        assert_eq!(balance["failed"], json!(1));
        assert_eq!(balance["sum"], json!(15.0));
        assert_eq!(balance["unit"], json!("积分"));
        assert_eq!(balance["scope"], json!("group"));
        assert_eq!(balance["expiring"]["count"], json!(1));
        assert_eq!(balance["expiring"]["amounts"][0]["sum"], json!(10.0));
    }

    #[test]
    fn usage_state_failed_when_balance_error() {
        let mut s = session("a", "acme", "enabled");
        s.last_balance_error = Some("timeout".into());
        assert_eq!(usage_state(&s), "failed");
    }
}
