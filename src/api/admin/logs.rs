//! 日志 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供请求日志、审计日志的查询和导出功能。
//!
//! ## 路径说明
//!
//! - 使用 `super::super::common` 访问共享认证逻辑

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::Value;

use super::super::openai::AppState;
use super::common::{default_page, default_size, error_response, verify_admin, verify_user};

// 这里我们实际上需要引用主 crate 的 log_store
// 由于子模块内 super 跳到了 api::admin，需要主级引用

/// 把请求日志条目序列化为 JSON，并附加 user_email（user_id → 邮箱解析）。
///
/// 前端用户列展示邮箱而非 UUID；解析失败（用户已删）回退原 user_id。
/// 普通用户响应剥离 `debug`（请求/响应快照仅 admin 可见，对齐 new-api admin_info 剥离语义）。
fn logs_with_user_email(state: &AppState, logs: Vec<serde_json::Value>, admin: bool) -> Vec<Value> {
    logs.into_iter()
        .map(|mut l| {
            if let Some(obj) = l.as_object_mut() {
                if !admin {
                    // 普通用户只拿统计字段：原始报文（debug）与对话正文
                    // （conversation）都属于「管理员排障/审计」视图，不下发。
                    // 对话里可能含他人的系统提示词拼接、工具参数等敏感上下文。
                    obj.remove("debug");
                    obj.remove("conversation");
                    // 路由回放字段同理：候选/被过滤渠道与过滤原因暴露的是
                    // 站方内部拓扑（有几家渠道、谁被熔断、健康度），
                    // 普通用户只需知道自己这次请求的结果。
                    obj.remove("candidate_channels");
                    obj.remove("filtered_channels");
                    obj.remove("selected_channel");
                }
                if let Some(uid) = obj.get("user_id").and_then(|v| v.as_str()) {
                    if let Some(u) = state.user_store.get_by_id(uid) {
                        obj.insert("user_email".into(), Value::String(u.email));
                    }
                }
            }
            l
        })
        .collect()
}

/// 审计日志附加 admin_email（admin_id → 邮箱解析，"admin" 等旧值原样保留）。
fn audits_with_admin_email(state: &AppState, logs: Vec<serde_json::Value>) -> Vec<Value> {
    logs.into_iter()
        .map(|mut l| {
            if let Some(obj) = l.as_object_mut() {
                if let Some(aid) = obj.get("admin_id").and_then(|v| v.as_str()) {
                    if let Some(u) = state.user_store.get_by_id(aid) {
                        obj.insert("admin_email".into(), Value::String(u.email));
                    }
                }
            }
            l
        })
        .collect()
}

/// 请求日志查询参数
#[derive(Debug, Deserialize)]
pub struct RequestLogQuery {
    /// 用户过滤：支持 UUID 或邮箱（邮箱自动解析为对应 user_id）
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    /// 状态筛选：`ok`（2xx）/ `error`（非 2xx 或带错误信息）
    #[serde(default)]
    pub status: Option<String>,
    /// 关键词：大小写不敏感，匹配 model / 渠道名 / 错误信息 / 用户
    #[serde(default)]
    pub keyword: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
    /// `page_size` 别名（前端两种写法都认；`size` 优先）
    #[serde(default)]
    pub page_size: Option<usize>,
}

impl RequestLogQuery {
    /// 生效的每页条数（`size` 优先，其次 `page_size`，最后默认值）
    pub fn effective_size(&self) -> usize {
        self.page_size.unwrap_or(self.size).clamp(1, 500)
    }
}

/// 审计日志查询参数
#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default = "default_page")]
    pub page: usize,
    #[serde(default = "default_size")]
    pub size: usize,
    /// `page_size` 别名（前端两种写法都认；`size` 优先）
    #[serde(default)]
    pub page_size: Option<usize>,
}

impl AuditLogQuery {
    pub fn effective_size(&self) -> usize {
        self.page_size.unwrap_or(self.size).clamp(1, 500)
    }
}

/// 导出格式参数（format 之外的筛选参数与列表页共用，保证导出与所见一致）
#[derive(Debug, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
}

/// 列出请求日志（全员可见：管理员看全部，普通用户只看自己的）
///
/// 普通用户自动按 `user_id` 过滤，忽略 URL 中的 `user` 参数（安全）。
/// 管理员保持原行为，可按任意 user/model/channel 筛选。
pub async fn handle_list_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<RequestLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 尝试管理员验证；失败则回退到普通用户
    let admin = verify_admin(&state, &headers).await.is_ok();
    // 管理员按 user 参数过滤（UUID 或邮箱，邮箱解析为 user_id）
    let resolved_filter: Option<String>;
    let user_email: Option<String>;
    let (filter_user, filter_channel) = if admin {
        resolved_filter = q.user.as_deref().and_then(|u| {
            // 含 '@' 视为邮箱 → 解析 user_id；否则原样按 UUID 过滤
            if u.contains('@') {
                state.user_store.get_by_email(u).map(|usr| usr.id)
            } else {
                Some(u.to_string())
            }
        });
        (resolved_filter.as_deref(), q.channel.as_deref())
    } else {
        // 普通用户：必须登录，且只能看自己的
        let user = verify_user(&state, &headers).await?;
        user_email = Some(user.id);
        (user_email.as_deref(), None)
    };
    let (logs, total) = state.log_store.requests.list_with_filter_ext(
        filter_user,
        q.model.as_deref(),
        filter_channel,
        q.start,
        q.end,
        q.status.as_deref(),
        q.keyword.as_deref(),
        q.page,
        q.effective_size(),
    );
    // user_id → user_email 解析（前端用户列展示邮箱）；非 admin 剥离 debug 快照
    let data = logs_with_user_email(
        &state,
        logs.into_iter()
            .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
            .filter(|v| !v.is_null())
            .collect(),
        admin,
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "total": total,
        "page": q.page,
        "size": q.effective_size(),
    })))
}

/// 列出审计日志（仅管理员，审计日志保持 admin only）
pub async fn handle_list_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (logs, total) = state
        .log_store
        .audits
        .list_paged(q.page, q.effective_size());
    // admin_id → admin_email 解析（前端管理员列展示邮箱）
    let data = audits_with_admin_email(
        &state,
        logs.into_iter()
            .map(|l| serde_json::to_value(l).unwrap_or(Value::Null))
            .filter(|v| !v.is_null())
            .collect(),
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": data,
        "total": total,
        "page": q.page,
        "size": q.effective_size(),
    })))
}

/// 导出请求日志（全员可见：管理员导出全部，普通用户导出自己的）
///
/// 权限与 `handle_list_request_logs` 对齐：管理员看全部，普通用户按 user_id 过滤。
pub async fn handle_export_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ExportQuery>,
) -> Response {
    // 尝试管理员验证；失败则要求普通用户登录并按 user_id 过滤
    // 注意：RequestLog.user_id 存的是用户 UUID（非邮箱），过滤必须用 user.id
    let admin = verify_admin(&state, &headers).await.is_ok();
    let filter_user_id: Option<String> = if admin {
        // 管理员：user 参数支持邮箱（解析为 UUID）或 UUID 原样；与列表页语义一致
        q.user.as_deref().and_then(|u| {
            if u.contains('@') {
                state.user_store.get_by_email(u).map(|usr| usr.id)
            } else {
                Some(u.to_string())
            }
        })
    } else {
        // 普通用户：忽略 URL 中的 user 参数，强制只导出自己的（安全）
        match verify_user(&state, &headers).await {
            Ok(u) => Some(u.id),
            Err(e) => return e.into_response(),
        }
    };
    let fmt = q.format.as_deref().unwrap_or("json").to_lowercase();
    let u = filter_user_id.as_deref();
    let m = q.model.as_deref();
    let c = q.channel.as_deref();

    let result = match fmt.as_str() {
        "csv" => {
            let csv = state
                .log_store
                .requests
                .export_csv_filtered(u, m, c, q.start, q.end);
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "text/csv; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        "attachment; filename=\"request_logs.csv\"".to_string(),
                    ),
                ],
                csv,
            )
        }
        "json" => {
            let json = state
                .log_store
                .requests
                .export_json_filtered(u, m, c, q.start, q.end, admin);
            (
                StatusCode::OK,
                [
                    (
                        axum::http::header::CONTENT_TYPE,
                        "application/json; charset=utf-8".to_string(),
                    ),
                    (
                        axum::http::header::CONTENT_DISPOSITION,
                        "attachment; filename=\"request_logs.json\"".to_string(),
                    ),
                ],
                json,
            )
        }
        _ => {
            return error_response(
                "Unsupported format, use 'csv' or 'json'",
                StatusCode::BAD_REQUEST,
            )
            .into_response();
        }
    };

    axum::response::IntoResponse::into_response(result)
}

/// 批量删除请求日志（仅管理员）
///
/// Body: `{ "ids": ["id1", "id2", ...] }`；返回 `{ success, data: { removed } }`。
pub async fn handle_delete_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeleteLogsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.requests.delete_many(&body.ids);
    Ok(Json(
        serde_json::json!({ "success": true, "data": { "removed": removed } }),
    ))
}

/// 清空全部请求日志（仅管理员）
pub async fn handle_clear_request_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.requests.clear_all().map_err(|e| {
        error_response(
            &format!("Failed to clear: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(
        serde_json::json!({ "success": true, "data": { "removed": removed } }),
    ))
}

/// 批量删除审计日志（仅管理员）
pub async fn handle_delete_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DeleteLogsBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.audits.delete_many(&body.ids);
    Ok(Json(
        serde_json::json!({ "success": true, "data": { "removed": removed } }),
    ))
}

/// 清空全部审计日志（仅管理员）
pub async fn handle_clear_audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let removed = state.log_store.audits.clear_all().map_err(|e| {
        error_response(
            &format!("Failed to clear: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(Json(
        serde_json::json!({ "success": true, "data": { "removed": removed } }),
    ))
}

/// 批量删除日志的请求体
#[derive(Debug, Deserialize)]
pub struct DeleteLogsBody {
    #[serde(default)]
    pub ids: Vec<String>,
}

/// ── 日志保留配置（运维设置：保留天数 / 容量上限 / 手动清理）──────────

#[derive(Debug, Deserialize)]
pub struct LogRetentionBody {
    /// 保留天数（None = 不按天数清理）
    #[serde(default)]
    pub max_age_days: Option<u64>,
    /// 容量上限（None = 默认 10 万条）
    #[serde(default)]
    pub max_capacity: Option<usize>,
    /// 审计日志独立保留天数（None = 跟随请求日志天数）
    #[serde(default)]
    pub audit_max_age_days: Option<u64>,
    /// 日聚合保留天数（含今天 N 个自然日；None/0 = 永久——聚合行量级
    /// 极小，默认不清。清理的是报表历史，不影响计费/明细）
    #[serde(default)]
    pub daily_max_age_days: Option<u64>,
}

/// GET /api/logs/inflight — 当前在途请求快照（仅管理员）
///
/// RequestLog 只在请求结束后落库，过程中完全不可见。这里返回进程内登记的
/// 正在进行的请求，供管理员查看「谁在跑、跑了多久」并按需终止。
pub async fn handle_list_inflight(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let items = state.inflight.list();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": items,
        "total": items.len(),
    })))
}

/// GET /api/logs/requests/clear-preview?days=N — 清空前的预览（仅管理员）
///
/// 先告诉管理员「会删多少条、最早/最新是哪天」，再让他确认 —— 避免
/// 一键清空后发现删错了区间。`days=0` 表示全部。
pub async fn handle_clear_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ClearPreviewQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let days = q.days.unwrap_or(0);
    let cutoff = if days > 0 {
        chrono::Utc::now().timestamp() - (days as i64) * 86_400
    } else {
        i64::MIN
    };
    let all = state.log_store.requests.all_sorted_asc();
    let matched: Vec<_> = all.iter().filter(|l| l.created_at >= cutoff).collect();
    let oldest = matched.first().map(|l| l.created_at);
    let newest = matched.last().map(|l| l.created_at);
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "days": days,
            "total_all": all.len(),
            "would_remove": matched.len(),
            "oldest": oldest,
            "newest": newest,
        }
    })))
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct ClearPreviewQuery {
    /// 只统计最近 N 天；缺省/0 = 全部
    #[serde(default)]
    pub days: Option<u64>,
}

/// POST /api/logs/inflight/:id/terminate — 终止一条在途请求（仅管理员）
///
/// 触发取消信号，数据面的流式循环收到即中断上游流并落一条失败日志。
pub async fn handle_terminate_inflight(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if state.inflight.terminate(&id) {
        super::common::record_audit(
            &state,
            &super::common::admin_id_from_session(&state, &headers).await,
            "request.terminate",
            &id,
            None,
            None,
        );
        Ok(Json(serde_json::json!({ "success": true, "data": null })))
    } else {
        Err(error_response("请求不存在或已结束", StatusCode::NOT_FOUND))
    }
}

/// GET /api/logs/retention — 读取日志保留配置
pub async fn handle_get_log_retention(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let (days, capacity, audit_days, daily_days) = state.log_store.requests.retention_full();
    Ok(Json(serde_json::json!({
        "success": true,
        "data": {
            "max_age_days": days,
            "max_capacity": capacity.unwrap_or(100_000),
            "audit_max_age_days": audit_days,
            "daily_max_age_days": daily_days,
        }
    })))
}

/// PUT /api/logs/retention — 更新日志保留配置（持久化，后台任务每日执行）
pub async fn handle_update_log_retention(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LogRetentionBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    // 0 = 清除独立天数（回到跟随请求日志），与前端「清空输入」语义一致
    let audit_days = body.audit_max_age_days.filter(|d| *d > 0);
    // 日聚合档：0 同样视作「留空 = 永久不清」
    let daily_days = body.daily_max_age_days.filter(|d| *d > 0);
    state
        .log_store
        .requests
        .set_retention_full(body.max_age_days, body.max_capacity, audit_days, daily_days)
        .map_err(|e| {
            error_response(
                &format!("Failed to save retention: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    // 设置日聚合保留后同请求立即 prune 一次（蓝本 stats_api 同款语义），
    // 失败只 warn：配置已存，明日的 sweep 会补做。
    let mut daily_pruned: u64 = 0;
    if let Some(n) = daily_days {
        match state
            .log_store
            .daily
            .prune_before(&crate::log::daily_agg::cutoff_date_key(n))
        {
            Ok(m) => daily_pruned = m as u64,
            Err(e) => tracing::warn!("日聚合保留清理失败: {e}"),
        }
    }
    tracing::info!(
        "日志保留配置已更新: max_age_days={:?} max_capacity={:?} audit_max_age_days={:?} daily_max_age_days={:?}",
        body.max_age_days,
        body.max_capacity,
        body.audit_max_age_days,
        body.daily_max_age_days
    );
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "daily_pruned": daily_pruned }
    })))
}

/// POST /api/logs/cleanup — 手动触发一次按保留天数的清理
///
/// 不改配置，按当前配置立即执行一次；未配置天数时按 `days` 请求参数执行
///（给「立即清理 N 天前」按钮用）。
#[derive(Debug, Deserialize)]
pub struct LogCleanupBody {
    #[serde(default)]
    pub days: Option<u64>,
}

pub async fn handle_cleanup_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<Json<LogCleanupBody>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    let (req_days, _capacity, audit_days, daily_days) = state.log_store.requests.retention_full();
    let req_days = body.and_then(|Json(b)| b.days).or(req_days);
    if req_days.is_none() && audit_days.is_none() && daily_days.is_none() {
        return Err(error_response("未配置保留天数", StatusCode::BAD_REQUEST));
    }
    // 请求日志与审计日志各自按自己的窗口清理；审计未单独设天数时跟随请求日志
    let req_cutoff = req_days.map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400);
    let mut removed = 0u64;
    let mut requests = 0u64;
    if let Some(cutoff) = req_cutoff {
        let n = state
            .log_store
            .requests
            .delete_older_than(cutoff)
            .map_err(|e| error_response(&e.to_string(), StatusCode::INTERNAL_SERVER_ERROR))?;
        requests = n as u64;
        removed += requests;
    }
    // 日聚合档独立于明细：按配置的 daily_max_age_days 现算截止日键。
    // 明细删除不影响聚合数字（报表仍含已删请求），文案由前端确认框承担。
    let mut daily_rows = 0u64;
    if let Some(n) = daily_days {
        daily_rows = state
            .log_store
            .daily
            .prune_before(&crate::log::daily_agg::cutoff_date_key(n))
            .unwrap_or(0) as u64;
        removed += daily_rows;
    }
    let audit_cutoff = audit_days
        .map(|d| chrono::Utc::now().timestamp() - (d as i64) * 86400)
        .or(req_cutoff);
    let mut audits = 0u64;
    if let Some(cutoff) = audit_cutoff {
        audits = state
            .log_store
            .audits
            .delete_older_than(cutoff)
            .unwrap_or(0) as u64;
        removed += audits;
    }
    Ok(Json(serde_json::json!({
        "success": true,
        "data": { "removed": removed, "requests": requests, "audits": audits, "daily_rows": daily_rows, "cutoff": req_cutoff }
    })))
}

/// POST /api/logs/agg/rebuild — 手动重建日聚合（仅管理员，逃生门）。
///
/// 流程：清回填标记（报表读路径立刻退回明细现算，不出现半重建假数）→
/// 全量流式重读明细回填（只动 date<今天 的日子，今天归运行期记账）→
/// 重设标记。shrink 守卫在回填内做：既有行计数更大（明细已被 retention
/// 裁过）的行保留不动，宁可旧数不准也不虚构小值。
pub async fn handle_agg_rebuild(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _ = verify_admin(&state, &headers).await?;
    if let Err(e) = state.log_store.daily.clear_backfill_marker() {
        return Err(error_response(
            &format!("Failed to clear backfill marker: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    let report = state.log_store.backfill_history(5000).map_err(|e| {
        error_response(
            &format!("Agg rebuild failed: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    super::common::record_audit(
        &state,
        &super::common::admin_id_from_session(&state, &headers).await,
        "logs.agg_rebuild",
        "daily_agg",
        None,
        Some(serde_json::to_value(&report).unwrap_or_default()),
    );
    tracing::info!("日聚合手动重建完成: {report:?}");
    Ok(Json(serde_json::json!({
        "success": true,
        "data": report
    })))
}

/// GET /api/logs/runtime — 读取运行日志环形缓冲快照。
///
/// 对照 Agent2API「运行日志页」后端。`since_id` 查询参数用于未读水位筛选
/// （只返回 id > since_id 的新日志）。
pub async fn handle_get_runtime_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<RuntimeLogsQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(serde_json::json!({
        "success": true,
        "data": crate::runtime_log::snapshot(params.since_id),
    })))
}

/// GET /api/logs/runtime/export — 导出运行日志为 JSONL。
pub async fn handle_export_runtime_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let body = crate::runtime_log::export_jsonl();
    Ok((
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        body,
    )
        .into_response())
}

/// 运行日志查询参数。
#[derive(Debug, Deserialize)]
pub struct RuntimeLogsQuery {
    /// 未读水位：只返回 id 大于此值的日志（前端轮询用）。
    pub since_id: Option<u64>,
}
