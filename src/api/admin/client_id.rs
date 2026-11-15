//! 客户端/来源识别：管理面规则 CRUD、识别预览、Top 客户端报表。
//!
//! ## 接口
//!
//! - `GET    /api/client-id`              规则列表 + 内置客户端目录 + 版本号
//! - `POST   /api/client-id/rules`        新建规则
//! - `PUT    /api/client-id/rules/:id`    更新规则
//! - `DELETE /api/client-id/rules/:id`    删除规则
//! - `POST   /api/client-id/preview`      用样例 UA/请求头试识别（不落库）
//! - `GET    /api/dashboard/report/top_clients` 客户端维度报表
//!
//! ## 生效语义
//!
//! 规则整份 JSON 落 FileStore KV；每次写操作走
//! [`crate::client_id::mutate_rules`]（互斥读改写 → 校验/编译 → 落库 →
//! 原子替换快照），**下一个请求立即生效**。校验失败整批拒绝，不做静默
//! 修正；非法正则在这里就被打回（运行期加载路径也做了防御，不会 panic）。
//!
//! ## 报表口径
//!
//! 管理员看全站 `by_client` 聚合维；普通用户按自己的用户行 `clients` 维
//! 收敛。日聚合未回填（或老聚合行缺该维）时回退到明细现算；老聚合行
//! 缺维度造成的差额并入 `unknown` 余量行，保证「Σ客户端 = 总请求」不破。

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, is_admin_user, record_audit, verify_user};
use crate::api::client_id::{self, ClientRule};
use crate::log::daily_agg;

/// 报表行（主档客户端维 / 用户行客户端维共用的投影）。
struct ClientRow {
    name: String,
    count: u64,
    tokens: u64,
    cost: i64,
    successful: u64,
    latency_sum: u64,
    /// 个人视图没有成功/延迟明细（null 而不是虚构 0）
    detailed: bool,
}

impl ClientRow {
    fn new(name: String, detailed: bool) -> Self {
        Self {
            name,
            count: 0,
            tokens: 0,
            cost: 0,
            successful: 0,
            latency_sum: 0,
            detailed,
        }
    }

    fn to_json(&self) -> Value {
        let tokens = if self.detailed {
            json!(self.tokens)
        } else {
            Value::Null
        };
        let success_rate = if self.detailed {
            json!(if self.count > 0 {
                (self.successful as f64 / self.count as f64 * 10000.0).round() / 100.0
            } else {
                0.0
            })
        } else {
            Value::Null
        };
        let avg_latency = if self.detailed {
            json!(self.latency_sum.checked_div(self.count).unwrap_or(0))
        } else {
            Value::Null
        };
        json!({
            "name": self.name,
            "count": self.count,
            "tokens": tokens,
            "cost": self.cost,
            "success_rate": success_rate,
            "avg_latency_ms": avg_latency,
        })
    }
}

/// 管理面鉴权：必须是管理员；返回操作者邮箱（审计用）。
async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, (StatusCode, Json<Value>)> {
    let user = verify_user(state, headers).await?;
    if !is_admin_user(&user) {
        return Err(error_response(
            "Admin access required",
            StatusCode::FORBIDDEN,
        ));
    }
    Ok(user.email)
}

/// 识别规则当前视图。
fn rules_view() -> Value {
    json!({
        "rules": client_id::current_rules(),
        "revision": client_id::revision(),
        "builtins": client_id::BUILTIN_CLIENTS,
        "declarationHeader": client_id::DECLARATION_HEADER,
        "declarationVersionHeader": client_id::DECLARATION_VERSION_HEADER,
        "matchModes": ["prefix", "contains", "regex"],
        "maxRules": client_id::MAX_RULES,
    })
}

/// GET /api/client-id
pub async fn handle_get_client_id(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = require_admin(&state, &headers).await?;
    Ok(Json(json!({ "success": true, "data": rules_view() })))
}

fn rule_from_payload(payload: Json<Value>) -> Result<ClientRule, (StatusCode, Json<Value>)> {
    serde_json::from_value::<ClientRule>(payload.0).map_err(|error| {
        error_response(
            &format!("规则内容不完整或字段类型错误：{error}"),
            StatusCode::BAD_REQUEST,
        )
    })
}

/// POST /api/client-id/rules
pub async fn handle_create_client_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let admin = require_admin(&state, &headers).await?;
    let mut rule = rule_from_payload(payload)?;
    if rule.id.trim().is_empty() {
        rule.id = uuid::Uuid::new_v4().to_string();
    }
    let rule_id = rule.id.clone();
    let audit_rule = rule.clone();
    let (_revision, rules) = client_id::mutate_rules(|rules| {
        rules.push(rule);
        Ok(())
    })
    .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    record_audit(
        &state,
        &admin,
        "client_id.rule_create",
        &rule_id,
        None,
        Some(json!(audit_rule)),
    );
    let created = rules.into_iter().find(|item| item.id == rule_id);
    Ok(Json(json!({
        "success": true,
        "data": { "rule": created, "view": rules_view() },
    })))
}

/// PUT /api/client-id/rules/:id
pub async fn handle_update_client_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    payload: Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let admin = require_admin(&state, &headers).await?;
    let mut rule = rule_from_payload(payload)?;
    rule.id = id.clone();
    let audit_rule = rule.clone();
    let (_revision, rules) = client_id::mutate_rules(|rules| {
        let Some(slot) = rules.iter_mut().find(|item| item.id == id) else {
            return Err("识别规则不存在".to_string());
        };
        *slot = rule;
        Ok(())
    })
    .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    record_audit(
        &state,
        &admin,
        "client_id.rule_update",
        &id,
        None,
        Some(json!(audit_rule)),
    );
    let updated = rules.into_iter().find(|item| item.id == id);
    Ok(Json(json!({
        "success": true,
        "data": { "rule": updated, "view": rules_view() },
    })))
}

/// DELETE /api/client-id/rules/:id
pub async fn handle_delete_client_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = require_admin(&state, &headers).await?;
    let (revision, rules) = client_id::mutate_rules(|rules| {
        let before = rules.len();
        rules.retain(|item| item.id != id);
        if rules.len() == before {
            return Err("识别规则不存在".to_string());
        }
        Ok(())
    })
    .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    record_audit(&state, &admin, "client_id.rule_delete", &id, None, None);
    Ok(Json(json!({
        "success": true,
        "data": { "rules": rules, "revision": revision, "view": rules_view() },
    })))
}

/// 预览请求体：`{ userAgent?, headers?: {name: value} }`
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreviewBody {
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default)]
    headers: Option<BTreeMap<String, String>>,
}

/// POST /api/client-id/preview — 用样例请求头试识别（不写库、不改转发）。
pub async fn handle_preview_client_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    payload: Json<PreviewBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = require_admin(&state, &headers).await?;
    let mut sample = HeaderMap::new();
    if let Some(user_agent) = payload.user_agent.as_deref() {
        let value = HeaderValue::from_str(user_agent)
            .map_err(|_| error_response("User-Agent 含非法字符", StatusCode::BAD_REQUEST))?;
        sample.insert("user-agent", value);
    }
    for (name, value) in payload.headers.iter().flatten() {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            error_response(&format!("请求头名不合法：{name}"), StatusCode::BAD_REQUEST)
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            error_response(&format!("请求头值不合法：{name}"), StatusCode::BAD_REQUEST)
        })?;
        sample.insert(name, value);
    }
    let identity = client_id::identify(&sample);
    Ok(Json(json!({
        "success": true,
        "data": {
            "clientName": identity.name,
            "clientVersion": identity.version,
            "source": identity.source,
        },
    })))
}

fn client_name_key(name: &str) -> &str {
    let name = name.trim();
    if name.is_empty() {
        client_id::UNKNOWN_CLIENT
    } else {
        name
    }
}

/// GET /api/dashboard/report/top_clients — 客户端维度 Top 榜。
///
/// 管理员 = 全站主档 `by_client`；普通用户 = 自己的用户行 `clients`。
/// 聚合未就绪时回退明细现算；老聚合行缺该维时差额并入 `unknown`。
pub async fn handle_report_top_clients(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<super::dashboard::ReportQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    let user_filter = if is_admin_user(&user) {
        None
    } else {
        Some(user.id.clone())
    };
    let detailed = user_filter.is_none();
    let (from, to, from_ts) =
        super::dashboard::resolve_window(q.days, q.start.as_deref(), q.end.as_deref());
    let window = format!("{from}:{to}");
    let scope = user_filter.clone();
    let data = super::dashboard::cached_report("top_clients", &window, scope.clone(), 120, {
        let state = state.clone();
        move || {
            let mut rows: Vec<ClientRow> = Vec::new();
            let agg_ready = state.log_store.daily.agg_available();
            match (&scope, agg_ready) {
                (None, true) => {
                    let days = state.log_store.daily.load_range(&from, &to);
                    let mut totals = (0u64, 0u64, 0i64);
                    for day in &days {
                        totals.0 += day.requests;
                        totals.1 = totals.1.saturating_add(day.tokens_in + day.tokens_out);
                        totals.2 = totals.2.saturating_add(day.cost);
                    }
                    for item in daily_agg::merge_clients(&days) {
                        let mut row = ClientRow::new(client_name_key(&item.name).to_string(), true);
                        row.count = item.count;
                        row.tokens = item.tokens_in + item.tokens_out;
                        row.cost = item.cost;
                        row.successful = item.successful;
                        row.latency_sum = item.latency_sum;
                        merge_row(&mut rows, row);
                    }
                    // 老聚合行没有 by_client：差额（请求数已知）并入 unknown。
                    let covered_requests: u64 = rows.iter().map(|row| row.count).sum();
                    let covered_tokens: u64 = rows.iter().map(|row| row.tokens).sum();
                    let covered_cost: i64 = rows.iter().map(|row| row.cost).sum();
                    let missing = ClientRow {
                        name: client_id::UNKNOWN_CLIENT.to_string(),
                        count: totals.0.saturating_sub(covered_requests),
                        tokens: totals.1.saturating_sub(covered_tokens),
                        cost: totals.2.saturating_sub(covered_cost),
                        successful: 0,
                        latency_sum: 0,
                        detailed: true,
                    };
                    if missing.count > 0 || missing.tokens > 0 || missing.cost != 0 {
                        merge_row(&mut rows, missing);
                    }
                }
                (Some(uid), true) => {
                    let user_rows = state.log_store.daily.load_user_range(uid, &from, &to);
                    let mut totals = (0u64, 0u64, 0i64);
                    for row in &user_rows {
                        totals.0 += row.count;
                        totals.1 = totals.1.saturating_add(row.tokens_in + row.tokens_out);
                        totals.2 = totals.2.saturating_add(row.cost);
                    }
                    for item in daily_agg::merge_user_clients(&user_rows) {
                        let mut row =
                            ClientRow::new(client_name_key(&item.name).to_string(), false);
                        row.count = item.count;
                        row.tokens = item.tokens;
                        row.cost = item.cost;
                        merge_row(&mut rows, row);
                    }
                    let covered_requests: u64 = rows.iter().map(|row| row.count).sum();
                    let missing = ClientRow {
                        name: client_id::UNKNOWN_CLIENT.to_string(),
                        count: totals.0.saturating_sub(covered_requests),
                        tokens: 0,
                        cost: 0,
                        successful: 0,
                        latency_sum: 0,
                        detailed: false,
                    };
                    if missing.count > 0 {
                        merge_row(&mut rows, missing);
                    }
                }
                _ => {
                    // 回退：直接扫窗口内明细（与 top_sources 的回退同口径）。
                    let to_ts_end = super::dashboard::date_start_ts(&to) + 86_400 - 1;
                    for log in state.log_store.requests.all_sorted_asc() {
                        if log.created_at < from_ts || log.created_at > to_ts_end {
                            continue;
                        }
                        if let Some(uid) = &scope {
                            if log.user_id.as_deref() != Some(uid.as_str()) {
                                continue;
                            }
                        }
                        let mut row =
                            ClientRow::new(client_name_key(&log.client_name).to_string(), detailed);
                        row.count += 1;
                        row.tokens = row
                            .tokens
                            .saturating_add(log.input_tokens + log.output_tokens);
                        row.cost += log.cost;
                        if log.status_code < 400 {
                            row.successful += 1;
                        }
                        row.latency_sum = row.latency_sum.saturating_add(log.latency_ms);
                        merge_row(&mut rows, row);
                    }
                }
            }
            rows.sort_by(|left, right| {
                right
                    .count
                    .cmp(&left.count)
                    .then(right.tokens.cmp(&left.tokens))
                    .then(right.cost.cmp(&left.cost))
            });
            rows.truncate(10);
            json!(rows.iter().map(ClientRow::to_json).collect::<Vec<Value>>())
        }
    })
    .await;
    Ok(Json(json!({ "success": true, "data": data })))
}

fn merge_row(rows: &mut Vec<ClientRow>, incoming: ClientRow) {
    if let Some(row) = rows
        .iter_mut()
        .find(|row| row.name.eq_ignore_ascii_case(&incoming.name))
    {
        row.count += incoming.count;
        row.tokens = row.tokens.saturating_add(incoming.tokens);
        row.cost = row.cost.saturating_add(incoming.cost);
        row.successful += incoming.successful;
        row.latency_sum = row.latency_sum.saturating_add(incoming.latency_sum);
    } else {
        rows.push(incoming);
    }
}
