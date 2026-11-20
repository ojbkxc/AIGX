//! 账号列表查询/分页 + A2A `/api/accounts*` canonical 端点
//! （对齐 Agent2API v3.5.0-beta.2 `core/accounts_list.rs` +
//! `api/accounts_list.rs` / `accounts_batch.rs` / `accounts_export.rs` /
//! `accounts_usage.rs` / `accounts.rs` 的方法分发）。
//!
//! ## 端点面
//!
//! | 方法 | 路径 | 说明 |
//! |---|---|---|
//! | GET | `/api/accounts/list` | AIGX 阴影入口：分页列表（envelope） |
//! | GET | `/api/accounts/summary` | 计数汇总（envelope） |
//! | GET | `/api/accounts/ids` | 只回 id（envelope） |
//! | POST | `/api/accounts/query` | A2A `AccountListQuery` → 分页页（raw） |
//! | POST | `/api/accounts/selection` | 同上，只回 id（raw） |
//! | GET | `/api/accounts/:id` | 单账号详情（raw，`detailsLoaded: true`） |
//! | POST | `/api/accounts/current` | 全局队列置顶 `{id}` |
//! | POST | `/api/accounts/reorder` | 整队重排 `{queue:[id…]}` |
//! | POST | `/api/accounts/:id/move` | 相邻交换 `{direction: up|down}` |
//! | POST | `/api/accounts/batch` | 批量 enable/disable/proxy/concurrency/usageQuery/limiters/remove |
//! | GET | `/api/accounts/export` | 导出全部账号（含 token；换机器继续用） |
//! | POST | `/api/accounts/import` | 导入账号（`{accounts:[…]}`；同 id 合并更新） |
//! | POST | `/api/accounts/refresh` | 刷新指定（或当前）账号 token `{id?, force?}` |
//! | GET | `/api/accounts/usage` | 逐账号同步查询余额 `?id=`（缺省批量） |
//! | GET | `/api/accounts/connections` | 逐账号活跃连接数（只含非零项） |
//! | POST | `/api/accounts/checkin` | 签到 `{id?}`（串行） |
//! | PATCH | `/api/accounts/:id` | 部分更新（代理/状态/限制器…） |
//! | DELETE | `/api/accounts/:id` | 删除账号 |
//!
//! 已由其它模块提供的 A2A 影子：`GET/PUT /api/accounts/sorting`、
//! `POST /api/accounts/refresh-expiring`、`POST /api/accounts/export-files`
//! （`account_sessions.rs`）；`GET/POST /api/accounts/{id}/campaigns*`
//! （`checkin_schedules.rs::router()`）。
//!
//! ## 响应形状
//!
//! 新 canonical 端点返回 **A2A 原始 JSON**（无 `{success,data}` 信封），
//! 与上游 `ok_json` 同口径；既有 AIGX 阴影三件套（list/summary/ids）保留
//! envelope 以免破坏既有调用方。错误仍走管理面 `{success:false,error}`。
//!
//! ## 与 AIGX 语义的映射
//!
//! - A2A `priority` 越小越先试；AIGX `priority` **越大**越先试 →
//!   `reorder`/`current` 按「队首 priority 最大」写入；
//! - A2A `move` 交换相邻两账号的队列键（priority + 次序）→ 这里交换
//!   `priority` 与 `sort_order` 两个键；
//! - A2A `usage` 带 `?id=` 是「手点某一行余额」的单查，**不看启用状态** →
//!   直接复用 `usage_query::query_manual` 的既有口径。

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::account_session::model::{AccountSession, Credential};
use crate::api::openai::AppState;

use super::common::{error_response, verify_admin};

// ── 共享：服务端排序快照 ────────────────────────────────────────────

/// 读取排序设置并构建当前账号池的排序快照（列表/查询与选路同源）。
fn sorting_snapshot(state: &AppState) -> crate::account_session::sorting::AccountSorting {
    let settings =
        crate::account_session::sorting::AccountSortSettings::load(&state.account_sessions);
    crate::account_session::sorting::AccountSorting::build(&state.account_sessions.all(), settings)
}

// ── GET /api/accounts/list（AIGX 阴影，envelope）────────────────────

/// GET /api/accounts/list —— A2A 形状的账号列表（分页）。
///
/// `?view=ids` 只返回 id；`?view=summary` 只返回计数；缺省返回分页列表 + 分组。
pub async fn handle_accounts_list_query(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<crate::accounts_list::AccountListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(&state);
    let now = chrono::Utc::now().timestamp();
    let data =
        crate::accounts_list::page_with_sorting(&sessions, &query, false, now, Some(&sorting));
    Ok(Json(json!({ "success": true, "data": data })))
}

/// GET /api/accounts/summary —— 账号计数汇总。
pub async fn handle_accounts_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    Ok(Json(json!({
        "success": true,
        "data": crate::accounts_list::summary(&sessions, now),
    })))
}

/// GET /api/accounts/ids —— 只返回满足查询条件的账号 id。
pub async fn handle_accounts_ids(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<crate::accounts_list::AccountListQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(&state);
    let now = chrono::Utc::now().timestamp();
    let data =
        crate::accounts_list::page_with_sorting(&sessions, &query, true, now, Some(&sorting));
    Ok(Json(json!({ "success": true, "data": data })))
}

// ── POST /api/accounts/query | selection（raw A2A 形状）─────────────

/// POST /api/accounts/query —— 从请求体反序列化 `AccountListQuery`，返回分页列表。
///
/// 对齐上游 `api/accounts_list.rs::query(state, body, false)`。
pub async fn handle_accounts_query_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let filter = serde_json::from_slice::<crate::accounts_list::AccountListQuery>(&body)
        .map_err(|_| error_response("账号筛选参数无效", StatusCode::BAD_REQUEST))?;
    validate_query(&filter)?;
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(&state);
    let now = chrono::Utc::now().timestamp();
    let data =
        crate::accounts_list::page_with_sorting(&sessions, &filter, false, now, Some(&sorting));
    Ok(Json(data))
}

/// POST /api/accounts/selection —— 同 query 但只返回 id 列表（`ids_only = true`）。
///
/// 对齐上游 `api/accounts_list.rs::query(state, body, true)`。
pub async fn handle_accounts_selection_post(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let filter = serde_json::from_slice::<crate::accounts_list::AccountListQuery>(&body)
        .map_err(|_| error_response("账号筛选参数无效", StatusCode::BAD_REQUEST))?;
    validate_query(&filter)?;
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(&state);
    let now = chrono::Utc::now().timestamp();
    let data =
        crate::accounts_list::page_with_sorting(&sessions, &filter, true, now, Some(&sorting));
    Ok(Json(data))
}

/// 校验分页与筛选参数范围（对齐上游 `query()` 的边界检查）。
fn validate_query(
    filter: &crate::accounts_list::AccountListQuery,
) -> Result<(), (StatusCode, Json<Value>)> {
    if !(1..=200).contains(&filter.page_size)
        || filter.q.len() > 4096
        || filter
            .ids
            .iter()
            .chain(&filter.providers)
            .chain(&filter.collapsed)
            .any(|id| id.len() > 512)
    {
        return Err(error_response(
            "账号分页或筛选参数超出范围",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(())
}

// ── GET / POST /api/accounts（canonical 根路径）─────────────────────

/// 账号池快照（A2A `GET /api/accounts` 无分页时的返回形状）。
///
/// `{currentAccountId, currentAccountIds, accounts, providers, hiddenAccounts,
/// sorting, indexOnly, total}`；隐藏账号不进 `accounts`，只在
/// `hiddenAccounts` 里给数量与原因。
fn accounts_snapshot(state: &AppState, index_only: bool) -> Value {
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(state);
    let now = chrono::Utc::now().timestamp();
    let mut visible: Vec<AccountSession> = Vec::new();
    let mut hidden: Vec<AccountSession> = Vec::new();
    for session in sessions {
        if session.hidden {
            hidden.push(session);
        } else {
            visible.push(session);
        }
    }
    json!({
        "currentAccountId": current_account_id(&visible),
        "currentAccountIds": crate::account_session::plugin::current_account_ids(&visible),
        "accounts": visible
            .iter()
            .map(|session| crate::accounts_list::account_json(session, now))
            .collect::<Vec<_>>(),
        "providers": crate::account_session::plugin::provider_summary(&visible),
        "hiddenAccounts": super::account_sessions::hidden_accounts_view(state, &hidden),
        "sorting": sorting.to_json(now),
        "indexOnly": index_only,
        "total": visible.len(),
    })
}

/// GET /api/accounts —— 账号列表（A2A canonical）。
///
/// - `?view=index`：索引快照（同 `accounts_snapshot`，`indexOnly: true`）；
/// - `?limit=N&offset=M&q=&provider=&enabled=&limitState=`：分页列表
///   （`AccountListQuery`，1~=200 条/页）；缺省返回完整快照。
pub async fn handle_accounts_root(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if params.get("view").map(String::as_str) == Some("index") {
        return Ok(Json(accounts_snapshot(&state, true)));
    }
    let Some(limit) = params.get("limit") else {
        return Ok(Json(accounts_snapshot(&state, false)));
    };
    let size = limit
        .parse::<usize>()
        .map_err(|_| error_response("每页账号数量无效", StatusCode::BAD_REQUEST))?;
    if !(1..=200).contains(&size) {
        return Err(error_response(
            "每页账号数量必须是 1~200",
            StatusCode::BAD_REQUEST,
        ));
    }
    let offset = match params.get("offset") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| error_response("账号分页位置无效", StatusCode::BAD_REQUEST))?,
        None => 0,
    };
    let provider = params
        .get("provider")
        .map(String::as_str)
        .filter(|value| !value.is_empty() && *value != "all");
    let filter = crate::accounts_list::AccountListQuery {
        page_size: size,
        offset,
        q: params.get("q").cloned().unwrap_or_default(),
        providers: provider.map(str::to_string).into_iter().collect(),
        enabled: params.get("enabled").cloned().unwrap_or_default(),
        limit: params.get("limitState").cloned().unwrap_or_default(),
        ..Default::default()
    };
    let sessions = state.account_sessions.all();
    let sorting = sorting_snapshot(&state);
    let now = chrono::Utc::now().timestamp();
    Ok(Json(crate::accounts_list::page_with_sorting(
        &sessions,
        &filter,
        false,
        now,
        Some(&sorting),
    )))
}

/// POST /api/accounts —— 手动添加账号（A2A canonical）。
///
/// payload 与 `POST /api/accounts/import` 的单个账号记录同形；上层语义与
/// 上游 `add_account` 一致：按 provider 分派到对应建号路径（插件家走插件
/// 合同，原生家走账号池建号），成功后回 `{account, created, list}`。
pub async fn handle_add_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let request = import_request(&payload);
    if request.provider.trim().is_empty() {
        return Err(error_response(
            "缺少 provider 字段",
            StatusCode::BAD_REQUEST,
        ));
    }
    let Json(response) = super::account_sessions::handle_create_account_session(
        State(state.clone()),
        headers,
        Json(request),
    )
    .await?;
    let account = response.get("data").cloned().unwrap_or(Value::Null);
    Ok(Json(json!({
        "account": account,
        "created": true,
        "list": accounts_snapshot(&state, false),
    })))
}

// ── GET /api/accounts/:id（raw）─────────────────────────────────────

/// GET /api/accounts/:id —— 单账号详情（对齐上游 `accounts_list::detail`）。
///
/// 返回该账号的完整 JSON 投影（含 `detailsLoaded: true`）。
pub async fn handle_account_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let sessions = state.account_sessions.all();
    let now = chrono::Utc::now().timestamp();
    match crate::accounts_list::detail(&sessions, &id, now) {
        Some(account) => Ok(Json(account)),
        None => Err(error_response(
            "账号不存在，请刷新后重试",
            StatusCode::NOT_FOUND,
        )),
    }
}

// ── POST /api/accounts/current ──────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct CurrentAccountBody {
    #[serde(default)]
    pub id: String,
}

/// POST /api/accounts/current —— 把账号置顶（仅调整全局优先级）。
///
/// A2A 语义：`promote_to_front` 把该账号放到全局队列第一位，**不删不建**
/// 其它账号。AIGX 的 `priority` 数值越大越先试，因此写入「全池最大 + 10」；
/// 已在第一位时返回 `{changed:false, reason}`（幂等）。
pub async fn handle_set_current_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CurrentAccountBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body.id.trim();
    if id.is_empty() {
        return Err(error_response("缺少账号 id", StatusCode::BAD_REQUEST));
    }
    let sessions = state.account_sessions.all();
    let Some(target) = sessions.iter().find(|session| session.id == id).cloned() else {
        return Err(error_response("账号不存在", StatusCode::NOT_FOUND));
    };
    let max_priority = sessions
        .iter()
        .filter(|session| session.id != id)
        .map(|session| session.priority)
        .max();
    let first_sort_order = sessions
        .iter()
        .filter(|session| session.id != id && Some(session.priority) == max_priority)
        .map(|session| session.sort_order)
        .min();
    let already_first = match max_priority {
        None => true,
        Some(max) => {
            target.priority > max
                || (target.priority == max
                    && first_sort_order.is_none_or(|first| target.sort_order <= first))
        }
    };
    if already_first {
        return Ok(Json(json!({
            "id": id,
            "changed": false,
            "reason": "已在全局队列第一位",
        })));
    }
    let mut next = target;
    next.priority = max_priority.unwrap_or(0).saturating_add(10);
    next.sort_order = 0;
    next.updated_at = chrono::Utc::now().timestamp();
    state.account_sessions.upsert(next.clone()).map_err(|e| {
        error_response(
            &format!("保存账号失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(json!({
        "id": id,
        "changed": true,
        "priority": next.priority,
    })))
}

// ── POST /api/accounts/reorder ──────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct ReorderBody {
    /// 目标队列（从队首到队尾的账号 id）。
    #[serde(default)]
    pub queue: Vec<String>,
}

/// POST /api/accounts/reorder —— 整队重排（分组模式拖拽后提交整条队列）。
///
/// 校验与上游 `store_order::reorder_accounts` 同口径：`queue` 必须是
/// **全部账号的一个排列**（未知 / 重复 / 缺项一律 400）。AIGX 的 priority
/// 越大越先试，因此队首写最大 priority、队尾写 1，`sort_order` 同步写
/// 数组下标（同 priority 内的稳定次序）。
pub async fn handle_reorder_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ReorderBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let queue: Vec<String> = body
        .queue
        .iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    if queue.is_empty() {
        return Err(error_response(
            "缺少 queue 字段（账号 id 数组，从队首到队尾）",
            StatusCode::BAD_REQUEST,
        ));
    }
    let sessions = state.account_sessions.all();
    let known: std::collections::HashSet<&str> =
        sessions.iter().map(|session| session.id.as_str()).collect();
    // 隐藏账号不在账号页列表里（前端拿不到它们的 id），因此只要求
    // **全部可见账号**的一个排列；隐藏账号保持原队尾，不参与本次重排。
    let visible: std::collections::HashSet<&str> = sessions
        .iter()
        .filter(|session| !session.hidden)
        .map(|session| session.id.as_str())
        .collect();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in &queue {
        if !known.contains(id.as_str()) {
            return Err(error_response(
                &format!("queue 包含未知账号：{id}"),
                StatusCode::BAD_REQUEST,
            ));
        }
        if !seen.insert(id.as_str()) {
            return Err(error_response(
                &format!("queue 包含重复账号：{id}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    if !visible.is_subset(&seen) {
        return Err(error_response(
            &format!(
                "queue 必须是全部可见账号的一个排列（可见 {} 个，queue 覆盖 {} 个）",
                visible.len(),
                visible.intersection(&seen).count()
            ),
            StatusCode::BAD_REQUEST,
        ));
    }
    let now = chrono::Utc::now().timestamp();
    let mut updated = 0usize;
    for (index, id) in queue.iter().enumerate() {
        let Some(mut session) = state.account_sessions.get(id) else {
            continue;
        };
        let priority = (queue.len() - index) as i64;
        let sort_order = index as i64;
        if session.priority == priority && session.sort_order == sort_order {
            continue;
        }
        session.priority = priority;
        session.sort_order = sort_order;
        session.updated_at = now;
        state.account_sessions.upsert(session).map_err(|e| {
            error_response(
                &format!("保存账号顺序失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
        updated += 1;
    }
    Ok(Json(json!({
        "updated": updated,
        "skipped": 0,
        "total": queue.len(),
    })))
}

// ── POST /api/accounts/:id/move ─────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct MoveAccountBody {
    #[serde(default)]
    pub direction: String,
}

/// POST /api/accounts/:id/move —— 与相邻账号交换优先级 `{direction: up|down}`。
///
/// 与 `reorder` 的分工：这条服务 ↑/↓ 箭头，一次只换两行；`reorder` 服务
/// 拖拽，一次定整队。direction 非 `"down"` 一律按 `"up"`（上游同口径）。
pub async fn handle_move_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<MoveAccountBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if id.is_empty() {
        return Err(error_response("缺少账号 id", StatusCode::BAD_REQUEST));
    }
    let down = body
        .as_ref()
        .map(|Json(body)| body.direction.as_str() == "down")
        .unwrap_or(false);
    let mut list = state.account_sessions.all();
    list.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.sort_order.cmp(&b.sort_order))
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
    let Some(index) = list.iter().position(|session| session.id == id) else {
        return Err(error_response("账号不存在", StatusCode::NOT_FOUND));
    };
    let neighbor = if down {
        index.checked_add(1).filter(|next| *next < list.len())
    } else {
        index.checked_sub(1)
    };
    let Some(neighbor) = neighbor else {
        return Ok(Json(json!({
            "id": id,
            "moved": false,
            "reason": if down { "已是最后一位" } else { "已是第一位" },
        })));
    };
    let now = chrono::Utc::now().timestamp();
    let mut current = list[index].clone();
    let mut other = list[neighbor].clone();
    std::mem::swap(&mut current.priority, &mut other.priority);
    std::mem::swap(&mut current.sort_order, &mut other.sort_order);
    current.updated_at = now;
    other.updated_at = now;
    for session in [current, other] {
        state.account_sessions.upsert(session).map_err(|e| {
            error_response(
                &format!("保存账号顺序失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    }
    Ok(Json(json!({
        "id": id,
        "moved": true,
        "direction": if down { "down" } else { "up" },
    })))
}

// ── POST /api/accounts/batch ────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct BatchAccountsBody {
    #[serde(default)]
    pub ids: Vec<String>,
    #[serde(default)]
    pub action: String,
    /// 批量改代理：字符串 URL / `pool:<id>` / `rotate`，或 `null`（改直连）。
    #[serde(default)]
    pub proxy: Option<Value>,
    /// 批量并发上限（0 = 不限；0..=999）。
    #[serde(default, rename = "maxConcurrent")]
    pub max_concurrent: Option<Value>,
    /// 批量查询设置 `{enabled?, intervalSeconds?, retries?, timeoutSeconds?}`。
    #[serde(default, rename = "usageQuery")]
    pub usage_query: Option<Value>,
    /// 限制器动作：append / replace / enable / disable / clear。
    #[serde(default, rename = "limiterMode")]
    pub limiter_mode: Option<String>,
    #[serde(default, rename = "limiterGroups")]
    pub limiter_groups: Option<Value>,
    #[serde(default)]
    pub limiters: Option<Value>,
}

/// POST /api/accounts/batch —— 批量操作。
///
/// action 对齐上游：`enable` / `disable` / `proxy` / `concurrency` /
/// `usageQuery` / `limiters` / `remove`。单账号失败不中断整批：
/// 返回 `{action, ok:[…], failed:[…], skipped:[], total}`。
pub async fn handle_batch_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BatchAccountsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // ids 去重 + 过滤空串（上游逐字同口径）
    let mut ids: Vec<String> = Vec::new();
    for id in &body.ids {
        let id = id.trim();
        if !id.is_empty() && !ids.iter().any(|existing| existing == id) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        return Err(error_response(
            "缺少要操作的账号 id",
            StatusCode::BAD_REQUEST,
        ));
    }
    let action = body.action.trim().to_string();
    // 参数前置校验：一次 400，不逐账号重复同一条参数错误。
    let proxy_value: Option<Option<String>> = match action.as_str() {
        "proxy" => match body.proxy.as_ref() {
            None => {
                return Err(error_response(
                    "批量修改代理时缺少 proxy 字段",
                    StatusCode::BAD_REQUEST,
                ))
            }
            Some(Value::Null) => Some(None),
            Some(Value::String(text)) => {
                let text = text.trim();
                Some(if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                })
            }
            Some(_) => {
                return Err(error_response(
                    "批量修改代理只支持字符串（URL / pool:<id> / rotate）或 null",
                    StatusCode::BAD_REQUEST,
                ))
            }
        },
        _ => None,
    };
    let concurrency_value: Option<u32> = match action.as_str() {
        "concurrency" => {
            let Some(value) = body.max_concurrent.as_ref() else {
                return Err(error_response(
                    "批量设置并发上限时缺少 maxConcurrent 字段",
                    StatusCode::BAD_REQUEST,
                ));
            };
            let parsed = value
                .as_u64()
                .or_else(|| value.as_i64().filter(|n| *n >= 0).map(|n| n as u64));
            match parsed {
                Some(n) if n <= 999 => Some(n as u32),
                _ => {
                    return Err(error_response(
                        "并发上限必须是 0~999 的整数（0 = 不限）",
                        StatusCode::BAD_REQUEST,
                    ))
                }
            }
        }
        _ => None,
    };
    // usageQuery：`{enabled?, intervalSeconds?, retries?, timeoutSeconds?}`。
    let usage_patch: Option<(
        Option<u64>,
        Option<crate::account_session::model::UsageQuerySettings>,
    )> = match action.as_str() {
        "usageQuery" => {
            let Some(value) = body.usage_query.as_ref() else {
                return Err(error_response(
                    "批量查询设置缺少 usageQuery 字段",
                    StatusCode::BAD_REQUEST,
                ));
            };
            if !value.is_object() {
                return Err(error_response(
                    "批量查询设置必须是 JSON 对象",
                    StatusCode::BAD_REQUEST,
                ));
            }
            let mut interval: Option<u64> = None;
            let enabled = value.get("enabled").and_then(Value::as_bool);
            if enabled == Some(false) {
                interval = Some(0);
            } else if let Some(seconds) = value
                .get("intervalSeconds")
                .or_else(|| value.get("interval_seconds"))
                .and_then(Value::as_u64)
            {
                interval = Some(seconds);
            }
            let settings =
                if value.get("retries").is_some() || value.get("timeoutSeconds").is_some() {
                    Some(
                        serde_json::from_value::<crate::account_session::model::UsageQuerySettings>(
                            value.clone(),
                        )
                        .map_err(|e| {
                            error_response(
                                &format!("批量查询设置格式错误：{e}"),
                                StatusCode::BAD_REQUEST,
                            )
                        })?,
                    )
                } else {
                    None
                };
            if interval.is_none() && settings.is_none() {
                return Err(error_response(
                        "usageQuery 未包含可应用的字段（enabled / intervalSeconds / retries / timeoutSeconds）",
                        StatusCode::BAD_REQUEST,
                    ));
            }
            Some((interval, settings))
        }
        _ => None,
    };
    // 限制器：mode 与规则前置校验（与 `/api/account-sessions/batch` 同口径）。
    enum LimiterMode {
        Append,
        Replace,
        Toggle(bool),
        Clear,
    }
    let limiter_mode: Option<LimiterMode> = match action.as_str() {
        "limiters" => {
            let Some(mode) = body.limiter_mode.as_deref() else {
                return Err(error_response(
                    "批量限制器操作缺少 limiterMode 字段",
                    StatusCode::BAD_REQUEST,
                ));
            };
            Some(match mode {
                "append" => LimiterMode::Append,
                "replace" => LimiterMode::Replace,
                "enable" => LimiterMode::Toggle(true),
                "disable" => LimiterMode::Toggle(false),
                "clear" => LimiterMode::Clear,
                other => {
                    return Err(error_response(
                        &format!("不支持的限制器操作：{other}"),
                        StatusCode::BAD_REQUEST,
                    ))
                }
            })
        }
        _ => None,
    };
    let incoming_rules: Vec<crate::account_session::model::AccountLimiterRule> =
        match (&limiter_mode, body.limiters.as_ref()) {
            (Some(LimiterMode::Append | LimiterMode::Replace), Some(value)) => {
                let rules: Vec<crate::account_session::model::AccountLimiterRule> =
                    serde_json::from_value(value.clone()).map_err(|e| {
                        error_response(
                            &format!("批量限制器规则格式错误：{e}"),
                            StatusCode::BAD_REQUEST,
                        )
                    })?;
                crate::account_session::model::validate_limiter_rules(&rules)
                    .map_err(|msg| error_response(&msg, StatusCode::BAD_REQUEST))?;
                rules
            }
            (Some(LimiterMode::Append | LimiterMode::Replace), None) => {
                return Err(error_response(
                    "追加或替换限制器时必须提供 limiters 数组",
                    StatusCode::BAD_REQUEST,
                ))
            }
            _ => Vec::new(),
        };
    let incoming_groups: Option<Vec<crate::account_session::limiter::LimiterGroup>> =
        match body.limiter_groups.as_ref() {
            Some(value) => Some(serde_json::from_value(value.clone()).map_err(|e| {
                error_response(
                    &format!("批量限制条件组格式错误：{e}"),
                    StatusCode::BAD_REQUEST,
                )
            })?),
            None => None,
        };
    match action.as_str() {
        "remove" | "enable" | "disable" | "proxy" | "concurrency" | "usageQuery" | "limiters" => {}
        other => {
            return Err(error_response(
                &format!(
                    "不支持的批量操作: {}",
                    if other.is_empty() { "(空)" } else { other }
                ),
                StatusCode::BAD_REQUEST,
            ))
        }
    }

    let now = chrono::Utc::now().timestamp();
    let mut ok: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for id in &ids {
        let Some(prev) = state.account_sessions.get(id) else {
            failed.push(json!({ "id": id, "error": "账号不存在" }));
            continue;
        };
        let mut next = prev.clone();
        let mut changes: Vec<&str> = Vec::new();
        match action.as_str() {
            "remove" => match state.account_sessions.remove(id) {
                Ok(Some(_)) => {
                    ok.push(json!({ "id": id, "name": prev.name, "changes": ["removed"] }))
                }
                Ok(None) => failed.push(json!({ "id": id, "error": "账号不存在" })),
                Err(e) => {
                    failed.push(json!({ "id": id, "name": prev.name, "error": e.to_string() }))
                }
            },
            "enable" | "disable" => {
                let status = if action == "enable" {
                    "enabled"
                } else {
                    "disabled"
                };
                if next.status != status {
                    next.status = status.to_string();
                    changes.push("status");
                }
            }
            "proxy" => {
                let target = proxy_value.clone().flatten();
                if next.proxy != target {
                    next.proxy = target;
                    changes.push("proxy");
                }
            }
            "concurrency" => {
                let value = concurrency_value.unwrap_or(0);
                if next.max_concurrency != value {
                    next.max_concurrency = value;
                    changes.push("maxConcurrency");
                }
            }
            "usageQuery" => {
                if let Some((interval, settings)) = usage_patch.as_ref() {
                    if let Some(interval) = interval {
                        let target = if *interval == 0 {
                            None
                        } else {
                            Some(*interval)
                        };
                        if next.usage_query_interval_secs != target {
                            next.usage_query_interval_secs = target;
                            changes.push("usageQuery");
                        }
                    }
                    if let Some(settings) = settings {
                        let normalized = settings.normalized();
                        if next.usage_query != normalized {
                            next.usage_query = normalized;
                            changes.push("usageQuery");
                        }
                    }
                }
            }
            "limiters" => {
                if let Some(groups) = incoming_groups.as_ref() {
                    if next.limiter_groups != *groups {
                        next.limiter_groups = groups.clone();
                        changes.push("limiterGroups");
                    }
                }
                let rules = match limiter_mode.as_ref() {
                    Some(LimiterMode::Append) => {
                        let mut rules = next.limiters.clone();
                        for rule in &incoming_rules {
                            if !rules.contains(rule) {
                                rules.push(rule.clone());
                            }
                        }
                        Some(rules)
                    }
                    Some(LimiterMode::Replace) => Some(incoming_rules.clone()),
                    Some(LimiterMode::Toggle(enabled)) => {
                        let mut rules = next.limiters.clone();
                        for rule in &mut rules {
                            rule.enabled = *enabled;
                        }
                        Some(rules)
                    }
                    Some(LimiterMode::Clear) => Some(Vec::new()),
                    None => None,
                };
                if let Some(rules) = rules {
                    if next.limiters != rules {
                        next.limiters = rules;
                        changes.push("limiters");
                    }
                }
            }
            _ => {}
        }
        if action == "remove" {
            continue;
        }
        if changes.is_empty() {
            ok.push(json!({ "id": id, "name": prev.name, "changes": [] }));
            continue;
        }
        next.updated_at = now;
        match state.account_sessions.upsert(next) {
            Ok(saved) => ok.push(json!({
                "id": saved.id,
                "name": saved.name,
                "changes": changes,
            })),
            Err(e) => failed.push(json!({ "id": id, "name": prev.name, "error": e.to_string() })),
        }
    }
    Ok(Json(json!({
        "action": action,
        "ok": ok,
        "failed": failed,
        "skipped": [],
        "total": ids.len(),
    })))
}

// ── GET /api/accounts/export ────────────────────────────────────────

/// 单账号导出记录（明文凭证；导出即备份语义）。
fn export_record(session: &AccountSession) -> Value {
    let data_dir = crate::account_session::secret::data_dir();
    let mut credential_fields = Map::new();
    for name in session.credential.field_names() {
        credential_fields.insert(
            name.to_string(),
            json!(session.credential.field_plain(name)),
        );
    }
    let refresh_token = if session.refresh_secret.is_empty() {
        String::new()
    } else {
        crate::account_session::secret::open_secret(&session.refresh_secret, &data_dir)
    };
    let mut record = json!({
        "format": crate::account_session::auth_file::ACCOUNT_FILE_FORMAT,
        "version": 1,
        "id": session.id,
        "provider": session.provider,
        "name": session.name,
        "channel_id": session.channel_id,
        "base_url": session.base_url,
        "chat_path": session.chat_path,
        "proxy": session.proxy,
        "status": session.status,
        "enabled": session.status != "disabled",
        "priority": session.priority,
        "sort_order": session.sort_order,
        "weight": session.weight,
        "max_concurrency": session.max_concurrency,
        "maxConcurrent": session.max_concurrency,
        "models": session.models,
        "extra_headers": session.extra_headers,
        "expires_at": session.expires_at,
        "credential_kind": session.credential.kind,
        "api_key": session.credential.secret_plain(),
        "credential_fields": credential_fields,
        "refresh_token": refresh_token,
        "usage_query_interval_secs": session.usage_query_interval_secs,
        "usageQuery": session.usage_query,
        "hidden": session.hidden,
        "allowRouteSkip": session.allow_route_skip,
        "userAgent": session.user_agent,
        "rateLimits": session.rate_limits,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
    });
    if let Some(secret) = &session.plugin_secret {
        record["secret"] = secret.clone();
        // 顶层镜像身份字段：与 A2A 导出记录同形，导入端两种读法都认
        if let Some(object) = secret.as_object() {
            for key in [
                "uid",
                "email",
                "accountId",
                "edition",
                "endpoint",
                "platform",
            ] {
                if let Some(value) = object.get(key) {
                    record[key] = value.clone();
                }
            }
        }
    }
    if let Some(public) = &session.plugin_public {
        record["public"] = public.clone();
    }
    record
}

/// GET /api/accounts/export —— 导出全部账号（含 token，换机器后导入继续用）。
pub async fn handle_export_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let accounts: Vec<Value> = state
        .account_sessions
        .all()
        .iter()
        .map(export_record)
        .collect();
    Ok(Json(json!({
        "version": 1,
        "exportedAt": chrono::Utc::now().timestamp_millis(),
        "customProviders": [],
        "accounts": accounts,
    })))
}

// ── POST /api/accounts/import ───────────────────────────────────────

fn field_text(entry: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        entry
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn field_i64(entry: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        entry.get(*key).and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.trim().parse().ok())
        })
    })
}

fn field_u32(entry: &Value, keys: &[&str]) -> Option<u32> {
    field_i64(entry, keys).and_then(|value| u32::try_from(value).ok())
}

fn field_object(entry: &Value, keys: &[&str]) -> Option<Value> {
    keys.iter()
        .find_map(|key| entry.get(*key).filter(|value| value.is_object()).cloned())
}

fn field_strings(entry: &Value, key: &str) -> Vec<String> {
    entry
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn field_headers(entry: &Value) -> Option<std::collections::HashMap<String, String>> {
    entry
        .get("extra_headers")
        .or_else(|| entry.get("extraHeaders"))
        .and_then(|value| {
            value.as_object().map(|object| {
                object
                    .iter()
                    .filter_map(|(key, value)| {
                        value.as_str().map(|text| (key.clone(), text.to_string()))
                    })
                    .collect()
            })
        })
}

/// 把 A2A 导出记录映射成 AIGX 建号请求。
///
/// 凭证读取顺序（与 `auth_file::parse_auth_file` 的导出侧对称）：
/// `plugin_secret` / `secret` 对象 → 插件不透明凭证；否则
/// `api_key` / `accessToken` / `token` / `cookie` → `api_key` 主密钥。
fn import_request(entry: &Value) -> super::account_sessions::AccountSessionRequest {
    use super::account_sessions::{AccountSessionRequest, AutoChannelSpec};
    let provider = field_text(entry, &["provider"]).unwrap_or_default();
    let channel_id = field_text(entry, &["channel_id", "channelId"]).unwrap_or_default();
    let plugin_secret = field_object(entry, &["plugin_secret", "pluginSecret", "secret"]);
    let api_key = field_text(
        entry,
        &[
            "api_key",
            "apiKey",
            "accessToken",
            "access_token",
            "token",
            "cookie",
        ],
    );
    let auto_channel = if channel_id.is_empty() {
        Some(AutoChannelSpec {
            name: None,
            base_url: None,
            quirks: None,
            balance_scheme: None,
            model_mapping: Default::default(),
            cost_pricing: Default::default(),
        })
    } else {
        None
    };
    AccountSessionRequest {
        channel_id,
        name: field_text(entry, &["name"]).unwrap_or_default(),
        provider,
        base_url: field_text(entry, &["base_url", "baseUrl"]).unwrap_or_default(),
        chat_path: field_text(entry, &["chat_path", "chatPath"]),
        api_key,
        proxy: field_text(entry, &["proxy"]),
        status: field_text(entry, &["status"]).unwrap_or_else(|| "enabled".to_string()),
        priority: field_i64(entry, &["priority"]),
        weight: field_u32(entry, &["weight"]),
        max_concurrency: field_u32(entry, &["max_concurrency", "maxConcurrent"]),
        models: field_strings(entry, "models"),
        extra_headers: field_headers(entry),
        refresh_token: field_text(entry, &["refresh_token", "refreshToken"]),
        credential_kind: field_text(entry, &["credential_kind", "credentialKind"]),
        expires_at: field_i64(entry, &["expires_at", "expiresAt"]),
        usage_query_interval_secs: field_i64(
            entry,
            &["usage_query_interval_secs", "usageQueryIntervalSecs"],
        )
        .and_then(|value| u64::try_from(value).ok())
        .or_else(|| {
            // A2A 嵌套 `usageQuery`：enabled=false → 关闭；缺省开启、间隔 60s
            let config = entry.get("usageQuery")?.as_object()?;
            if config.get("enabled").and_then(Value::as_bool) == Some(false) {
                return None;
            }
            Some(
                config
                    .get("interval")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .unwrap_or(60),
            )
        }),
        limiters: None,
        limiter_groups: None,
        auto_channel,
        auth_file_content: field_text(entry, &["authFileContent", "auth_file_content"]),
        auth_file_b64: field_text(entry, &["authFileB64", "auth_file_b64"]),
        auth_file_name: field_text(entry, &["authFileName", "auth_file_name"]),
        plugin_secret,
        plugin_public: field_object(entry, &["plugin_public", "public"]),
        plugin_identity: field_text(
            entry,
            &[
                "plugin_identity",
                "pluginIdentity",
                "uid",
                "email",
                "accountId",
            ],
        ),
        hidden: entry.get("hidden").and_then(Value::as_bool),
        allow_route_skip: entry
            .get("allowRouteSkip")
            .or_else(|| entry.get("allow_route_skip"))
            .and_then(Value::as_bool),
        user_agent: field_text(entry, &["userAgent", "user_agent"]),
        usage_query: entry
            .get("usageQuery")
            .or_else(|| entry.get("usage_query"))
            .filter(|value| value.is_object())
            .and_then(|value| {
                serde_json::from_value::<crate::account_session::model::UsageQuerySettings>(
                    value.clone(),
                )
                .ok()
            }),
        low_balance: entry
            .get("lowBalance")
            .or_else(|| entry.get("low_balance"))
            .filter(|value| value.is_object())
            .and_then(|value| {
                serde_json::from_value::<crate::account_session::model::LowBalanceSetting>(
                    value.clone(),
                )
                .ok()
            }),
        rate_limits: entry
            .get("rateLimits")
            .or_else(|| entry.get("rate_limits"))
            .and_then(Value::as_object)
            .cloned(),
    }
}

/// POST /api/accounts/import —— 导入账号（merge：按 id 匹配，命中更新、未命中追加）。
///
/// 请求体兼容管理信封壳（`{success,data:{accounts}}` 的旧导出文件）。
/// 响应 `{total, added, updated, skipped, failed, errors, importedIds}`，
/// 与上游 `account_transfer::import_accounts` 同口径。
pub async fn handle_import_accounts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // 兼容 `{success,data}` 信封壳（旧「导出」把整个信封存成了文件）
    let inner = payload
        .as_object()
        .filter(|object| !object.contains_key("accounts"))
        .and_then(|object| object.get("data"))
        .filter(|data| data.get("accounts").is_some())
        .cloned();
    let payload = inner.unwrap_or(payload);
    let Some(items) = payload.get("accounts").and_then(Value::as_array) else {
        return Err(error_response(
            "缺少 accounts 数组",
            StatusCode::BAD_REQUEST,
        ));
    };
    if items.is_empty() {
        return Err(error_response(
            "accounts 为空，没有可导入的账号",
            StatusCode::BAD_REQUEST,
        ));
    }
    if items.len() > 500 {
        return Err(error_response(
            "单次最多导入 500 个账号",
            StatusCode::BAD_REQUEST,
        ));
    }
    let mut added = 0usize;
    let mut updated = 0usize;
    let mut failed = 0usize;
    let mut errors: Vec<Value> = Vec::new();
    let mut imported_ids: Vec<String> = Vec::new();
    for entry in items {
        let entry_id = entry.get("id").and_then(Value::as_str).unwrap_or("").trim();
        let request = import_request(entry);
        if request.provider.trim().is_empty() {
            failed += 1;
            errors.push(json!({ "id": entry_id, "message": "账号缺少 provider" }));
            continue;
        }
        if !entry_id.is_empty() && state.account_sessions.get(entry_id).is_some() {
            match super::account_sessions::handle_update_account_session(
                State(state.clone()),
                headers.clone(),
                Path(entry_id.to_string()),
                Json(request),
            )
            .await
            {
                Ok(Json(response)) => {
                    updated += 1;
                    imported_ids.push(entry_id.to_string());
                    if let Some(id) = response
                        .pointer("/data/id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                    {
                        if let Some(slot) = imported_ids.last_mut() {
                            *slot = id;
                        }
                    }
                }
                Err((_, Json(error))) => {
                    failed += 1;
                    errors.push(json!({
                        "id": entry_id,
                        "message": error.get("error").and_then(Value::as_str).unwrap_or("导入失败"),
                    }));
                }
            }
        } else {
            match super::account_sessions::handle_create_account_session(
                State(state.clone()),
                headers.clone(),
                Json(request),
            )
            .await
            {
                Ok(Json(response)) => {
                    added += 1;
                    if let Some(id) = response.pointer("/data/id").and_then(Value::as_str) {
                        imported_ids.push(id.to_string());
                    }
                }
                Err((_, Json(error))) => {
                    failed += 1;
                    errors.push(json!({
                        "id": entry_id,
                        "message": error.get("error").and_then(Value::as_str).unwrap_or("导入失败"),
                    }));
                }
            }
        }
    }
    Ok(Json(json!({
        "total": items.len(),
        "added": added,
        "updated": updated,
        "skipped": 0,
        "failed": failed,
        "errors": errors,
        "importedIds": imported_ids,
        "customProviders": [],
    })))
}

// ── POST /api/accounts/refresh ──────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct RefreshAccountBody {
    /// 目标账号；缺省 = 全局队首的可用账号。
    #[serde(default)]
    pub id: Option<String>,
    /// true（缺省）= 强制刷新；false = 仅临期才刷。
    #[serde(default)]
    pub force: Option<bool>,
}

/// 全局队首可用账号（启用且持有凭证、无需重登）。
fn current_account_id(sessions: &[AccountSession]) -> Option<String> {
    let mut list: Vec<&AccountSession> = sessions
        .iter()
        .filter(|session| {
            session.status != "disabled"
                && session.status != crate::account_session::model::STATUS_NEEDS_RELOGIN
                && !session.needs_relogin
                && (!session.credential.secret.is_empty() || session.plugin_secret.is_some())
        })
        .collect();
    list.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(a.sort_order.cmp(&b.sort_order))
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
    list.first().map(|session| session.id.clone())
}

/// POST /api/accounts/refresh —— 刷新指定（或当前）账号的 token `{id?, force?}`。
pub async fn handle_refresh_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<RefreshAccountBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = match body
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => id.to_string(),
        None => current_account_id(&state.account_sessions.all())
            .ok_or_else(|| error_response("当前没有可用账号", StatusCode::BAD_REQUEST))?,
    };
    let result = super::account_sessions::handle_account_refresh(
        State(state),
        headers,
        Path(id.clone()),
        Json(super::account_sessions::RefreshBody {
            force: body.force.unwrap_or(true),
        }),
    )
    .await?;
    let Json(response) = result;
    if response.get("success").and_then(Value::as_bool) == Some(false) {
        let message = response
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("刷新失败");
        return Err(error_response(message, StatusCode::BAD_GATEWAY));
    }
    Ok(Json(json!({
        "refreshedId": id,
        "result": response.get("data").cloned().unwrap_or(Value::Null),
    })))
}

// ── GET /api/accounts/usage ─────────────────────────────────────────

/// GET /api/accounts/usage —— 逐账号并发查询余额 / 积分汇总
/// （`?id=` 单查；缺省批量）。同步等待查询完成。
///
/// 与 `/api/account-sessions/usage`（POST）同一实现（`usage_query::query_manual`）；
/// 语义差异只在「带 id 的单查不看启用状态」——这一条由 core 的既有口径保证。
pub async fn handle_accounts_usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = params
        .get("id")
        .map(String::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty());
    let (results, skipped) = crate::account_session::usage_query::query_manual(
        &state.account_sessions,
        &state.http_client,
        id,
    )
    .await;
    Ok(Json(json!({ "results": results, "skipped": skipped })))
}

// ── GET /api/accounts/connections ───────────────────────────────────

/// GET /api/accounts/connections —— 逐账号活跃连接数（账号页「连接数」列）。
///
/// `counts` **只含非零项**（界面按「缺失即 0」处理，轮询体更小）。
pub async fn handle_accounts_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut counts = Map::new();
    for session in state.account_sessions.all() {
        if session.in_flight > 0 {
            counts.insert(session.id, json!(session.in_flight));
        }
    }
    Ok(Json(json!({ "counts": Value::Object(counts) })))
}

// ── POST /api/accounts/checkin ──────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct CheckinBody {
    /// 指定账号 id；空 = 全部
    #[serde(default)]
    pub id: String,
}

/// POST /api/accounts/checkin —— 签到（串行，跳过范围外的提供商）`{id?}`。
///
/// 复用 `checkin_center::handle_checkin_run`（手动签到链，与签到中心同一
/// 执行体），把结果映射成 A2A 形状：
/// `{results, succeeded, active, total, skipped, failed, failedCount}`。
pub async fn handle_accounts_checkin(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<CheckinBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let id = body
        .as_ref()
        .map(|Json(body)| body.id.trim().to_string())
        .filter(|id| !id.is_empty());
    let request = super::checkin_center::RunRequest {
        account_ids: id.iter().cloned().collect(),
        providers: Vec::new(),
    };
    let Json(response) = super::checkin_center::handle_checkin_run(
        State(state.clone()),
        headers,
        Some(Json(request)),
    )
    .await?;
    let rows = response
        .pointer("/data/results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut results: Vec<Value> = Vec::with_capacity(rows.len());
    let mut succeeded = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for row in &rows {
        let success = row.get("success").and_then(Value::as_bool).unwrap_or(false);
        let outcome = row.get("outcome").and_then(Value::as_str).unwrap_or("");
        let message = row.get("message").and_then(Value::as_str).unwrap_or("");
        let row_id = row.get("account_id").cloned().unwrap_or(Value::Null);
        // A2A `succeeded` = 真领取 + 今日已领取（两者都表示「今天完成了」）
        if outcome == "success" || outcome == "already" {
            succeeded += 1;
        }
        if !success && outcome == "failed" {
            failed.push(format!(
                "{}: {message}",
                row.get("account_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
        }
        results.push(json!({
            "id": row_id,
            "name": row.get("account_name").cloned().unwrap_or(Value::Null),
            "provider": row.get("provider").cloned().unwrap_or(Value::Null),
            "durationMs": Value::Null,
            "claim": {
                "success": success,
                "outcome": outcome,
                "msg": message,
            },
            "error": if success { Value::Null } else { json!(message) },
        }));
    }
    let failed_count = failed.len();
    Ok(Json(json!({
        "results": results,
        "succeeded": succeeded,
        "active": 0,
        "total": results.len(),
        "skipped": 0,
        "failed": failed.iter().take(5).cloned().collect::<Vec<_>>(),
        "failedCount": failed_count,
    })))
}

// ── PATCH / DELETE /api/accounts/:id（canonical 影子）───────────────

/// PATCH /api/accounts/:id —— 部分更新（名称/状态/优先级/权重/并发/代理/
/// 限制器/usageQuery/UA…）。请求体直接交给 `AccountSessionPatch`。
pub async fn handle_patch_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let patch = serde_json::from_slice::<super::account_sessions::AccountSessionPatch>(&body)
        .map_err(|e| error_response(&format!("账号更新参数无效：{e}"), StatusCode::BAD_REQUEST))?;
    let Json(response) = super::account_sessions::handle_patch_account_session(
        State(state),
        headers,
        Path(id),
        Json(patch),
    )
    .await?;
    let account = response.get("data").cloned().unwrap_or(Value::Null);
    Ok(Json(json!({
        "account": account,
        "changes": response.get("changes").cloned().unwrap_or_else(|| json!([])),
    })))
}

/// DELETE /api/accounts/:id —— 删除账号。
pub async fn handle_delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let Json(response) = super::account_sessions::handle_delete_account_session(
        State(state),
        headers,
        Path(id.clone()),
    )
    .await?;
    Ok(Json(json!({
        "deleted": true,
        "id": id,
        "account": response.get("data").cloned().unwrap_or(Value::Null),
    })))
}

// ── 凭证替换（既有）─────────────────────────────────────────────────

/// 凭证替换请求（对齐参考树 `Replacement`）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialReplacement {
    pub credential_json: String,
    pub confirmed: bool,
    #[serde(default)]
    pub expected_updated_at: i64,
    #[serde(default)]
    pub authorize_refresh: bool,
}

/// 从粘贴的凭证 JSON 构造 [`Credential`]。
///
/// 主密钥优先取 `cookie` / `token` / `accessToken` / `apiKey` / `key`；其余字符串
/// 字段进 `fields`（加密落盘）。非对象输入原样作为 `secret`（kind=token）。
fn credential_from_json(raw: &str) -> Result<Credential, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("凭证内容不能为空".into());
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(Credential {
            kind: "token".into(),
            secret: trimmed.to_string(),
            fields: Default::default(),
        });
    };
    let object = value
        .as_object()
        .ok_or("凭证 JSON 必须是对象或非空字符串")?;
    let mut secret = String::new();
    let mut kind = "api_key".to_string();
    let mut fields = std::collections::HashMap::new();
    for (key, value) in object {
        let Some(text) = value.as_str() else { continue };
        match key.as_str() {
            "cookie" => {
                secret = text.to_string();
                kind = "cookie".into();
            }
            "token" | "accessToken" | "access_token" if secret.is_empty() => {
                secret = text.to_string();
                kind = "token".into();
            }
            "apiKey" | "api_key" | "key" if secret.is_empty() => {
                secret = text.to_string();
                kind = "api_key".into();
            }
            _ => {
                fields.insert(key.clone(), text.to_string());
            }
        }
    }
    if secret.is_empty() {
        return Err("凭证 JSON 缺少主密钥（cookie / token / accessToken / apiKey）".into());
    }
    Ok(Credential {
        kind,
        secret,
        fields,
    })
}

/// PUT /api/accounts/:id/credentials/replace —— 替换账号登录凭证。
///
/// `expected_updated_at` CAS：与当前 `updated_at` 不符返回 409（提示刷新后重试）。
/// `authorize_refresh` 目前仅记录（AIGX 各家续期链路独立接入，见 `account_session::refresh`）。
pub async fn handle_replace_account_credentials(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CredentialReplacement>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if body.credential_json.len() > 1024 * 1024 {
        return Err(error_response(
            "凭证输入过大，请只粘贴当前账号的完整凭证 JSON",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !body.confirmed || body.expected_updated_at < 0 {
        return Err(error_response(
            "请先确认更新当前账号的登录凭证",
            StatusCode::BAD_REQUEST,
        ));
    }
    let session = state
        .account_sessions
        .get(&id)
        .ok_or_else(|| error_response("账号不存在", StatusCode::NOT_FOUND))?;
    if body.expected_updated_at != 0 && session.updated_at != body.expected_updated_at {
        return Err(error_response(
            "账号已变更，请刷新后重试",
            StatusCode::CONFLICT,
        ));
    }
    let next = credential_from_json(&body.credential_json)
        .map_err(|msg| error_response(&msg, StatusCode::BAD_REQUEST))?;
    let old_secret = session.credential.secret_plain();
    let authorized_refresh = body.authorize_refresh;
    match state
        .account_sessions
        .update_credential_cas(&id, &old_secret, next)
    {
        crate::account_session::store::CasOutcome::Updated => {
            let updated = state.account_sessions.get(&id);
            tracing::info!(
                "account {id} credentials replaced (authorize_refresh={authorized_refresh})"
            );
            Ok(Json(json!({
                "updated": true,
                "authorizeRefresh": authorized_refresh,
                "account": updated.map(|s| {
                    json!({
                        "id": s.id,
                        "name": s.name,
                        "provider": s.provider,
                        "updatedAt": s.updated_at,
                    })
                }),
            })))
        }
        crate::account_session::store::CasOutcome::Stale => Err(error_response(
            "账号凭证已被并发刷新，本次替换被拒绝，请刷新后重试",
            StatusCode::CONFLICT,
        )),
        crate::account_session::store::CasOutcome::Missing => {
            Err(error_response("账号不存在", StatusCode::NOT_FOUND))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_from_json_extracts_primary_secret() {
        let cred =
            credential_from_json(r#"{"cookie":"abc","uid":"42","refreshToken":"r"}"#).unwrap();
        assert_eq!(cred.kind, "cookie");
        assert_eq!(cred.secret, "abc");
        assert_eq!(cred.fields.get("uid"), Some(&"42".to_string()));
    }

    #[test]
    fn credential_from_json_plain_string_is_token() {
        let cred = credential_from_json("opaque-token").unwrap();
        assert_eq!(cred.kind, "token");
        assert_eq!(cred.secret, "opaque-token");
    }

    #[test]
    fn credential_from_json_requires_primary() {
        assert!(credential_from_json(r#"{"uid":"42"}"#).is_err());
        assert!(credential_from_json("   ").is_err());
    }

    #[test]
    fn import_request_maps_a2a_record_fields() {
        let entry = json!({
            "id": "acct-1",
            "provider": "kiro",
            "name": "团队 A",
            "apiKey": "sk-1",
            "refreshToken": "rt-1",
            "baseUrl": "https://example.com",
            "maxConcurrent": 5,
            "models": ["m1", "m2"],
            "allowRouteSkip": false,
            "userAgent": "ua/1",
            "hidden": true,
        });
        let request = import_request(&entry);
        assert_eq!(request.provider, "kiro");
        assert_eq!(request.name, "团队 A");
        assert_eq!(request.api_key.as_deref(), Some("sk-1"));
        assert_eq!(request.refresh_token.as_deref(), Some("rt-1"));
        assert_eq!(request.base_url, "https://example.com");
        assert_eq!(request.max_concurrency, Some(5));
        assert_eq!(request.models, vec!["m1".to_string(), "m2".to_string()]);
        assert_eq!(request.allow_route_skip, Some(false));
        assert_eq!(request.user_agent.as_deref(), Some("ua/1"));
        assert_eq!(request.hidden, Some(true));
        // 无 channel_id → 自动建/复用账号池渠道
        assert!(request.auto_channel.is_some());
    }

    #[test]
    fn import_request_keeps_plugin_secret_object() {
        let entry = json!({
            "provider": "kiro",
            "plugin_secret": { "accessToken": "tok" },
            "plugin_public": { "edition": "intl" },
            "uid": "u-1",
        });
        let request = import_request(&entry);
        assert_eq!(
            request.plugin_secret.as_ref().unwrap()["accessToken"],
            json!("tok")
        );
        assert_eq!(
            request.plugin_public.as_ref().unwrap()["edition"],
            json!("intl")
        );
        assert_eq!(request.plugin_identity.as_deref(), Some("u-1"));
    }

    #[test]
    fn export_record_carries_credentials_and_plugin_contract() {
        let mut session = AccountSession::new("ch", "acct-1");
        session.id = "acct-1".to_string();
        session.provider = "kiro".to_string();
        session.credential.kind = "api_key".to_string();
        session.credential.secret = "sk-plain".to_string();
        let record = export_record(&session);
        assert_eq!(record["id"], json!("acct-1"));
        assert_eq!(record["provider"], json!("kiro"));
        assert_eq!(record["api_key"], json!("sk-plain"));
        assert_eq!(
            record["format"],
            json!(crate::account_session::auth_file::ACCOUNT_FILE_FORMAT)
        );
    }

    #[test]
    fn current_account_prefers_highest_priority() {
        let mut low = AccountSession::new("ch", "low");
        low.id = "low".into();
        low.priority = 1;
        low.credential.secret = "k".into();
        let mut high = AccountSession::new("ch", "high");
        high.id = "high".into();
        high.priority = 9;
        high.credential.secret = "k".into();
        assert_eq!(current_account_id(&[low, high]), Some("high".to_string()));
    }
}
