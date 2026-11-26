//! A2A 兼容别名端点。
//!
//! ── 为什么需要这一层 ────────────────────────────────────────
//! Agent2API v3.5.1 把这些能力做成了**独立端点**（`api/timeouts_api.rs`、
//! `api/retry_api.rs`、`api/cors.rs`、`api/stats_api.rs`、`api/logs_api.rs`、
//! `api/cloud_sync.rs`、`api/backup_api.rs`、`http.rs` 的 `/api/session/login/*`），
//! 外部脚本、文档与老客户端都按这些路径与字段名调用。AIGX 把同样的能力
//! 折进了别的路径（`/api/limits`、`/api/dashboard/report/*`、
//! `/api/logs/requests*`、`/api/admin/sync/*`、`/api/backup/*`、
//! `/api/account-sessions/login/*`）—— 功能都在，但**路径与字段名不同**，
//! 照着 A2A 写的调用方会 404。
//!
//! 本模块只做「路径 + 字段名」的翻译，**不引入第二份真相**：
//! - 配置类（timeouts / retry / cors）读写走同一条 [`update_admin_config`]
//!   （字段级读-改-写，不做整包覆盖）；
//! - 只读类（stats / logs / cloud-sync / backup）**直接调用 AIGX 既有 handler**，
//!   不发 HTTP 自请求、不复制查询逻辑；
//! - 登录类（`/api/session/login/*`）是**同一个 handler 挂两个 path**。
//!
//! ── 字段映射（配置类）───────────────────────────────────────
//! | A2A 字段 | AIGX 字段 | 单位 |
//! |---|---|---|
//! | `connectTimeoutSeconds` | `usage.connect_timeout_secs` | 秒 |
//! | `headersTimeoutSeconds` | `usage.headers_timeout_secs` | 秒 |
//! | `streamIdleTimeoutSeconds` | `usage.stream_idle_secs` | 秒 |
//! | `bodyTimeoutSeconds` | `usage.body_timeout_secs` | 秒 |
//! | `retryCount` | `usage.retry_self_count`（同账号原地重发） | 次 |
//! | `retryCrossProviderCount` | `usage.retry_switch_count`（换账号） | 次 |
//! | `retryIntervalSeconds` | `usage.retry_interval_ms` ÷ 1000 | 秒 |
//! | `noRetryStatusCodes` | `usage.retry_on_status` | 状态码数组 |
//! | `corsEnabled` | `cors_origins` 是否含 `*` | 布尔 |
//!
//! ── 路径映射（只读 / 执行类）────────────────────────────────
//! | A2A 路径 | AIGX 落点 | 说明 |
//! |---|---|---|
//! | `GET /api/stats/summary` | `dashboard::handle_report_{overview,heatmap,cache_rates,cache_trend_24h,daily_trend}` 组合 | `overview` 逐字段改名（见下） |
//! | `GET /api/stats/requests` | `logs::handle_list_request_logs` | 行逐字段改名（见下） |
//! | `GET /api/stats/requests/detail` | `RequestLogStore::get_by_request_id` | 整条 `RequestLog` 原样 |
//! | `GET /api/stats/requests/filters` | `RequestLogStore::list_window` 现算 | 只取**明细里出现过**的值 |
//! | `GET /api/stats/requests/raw` | 同上 + `RequestLog.debug.{request_body,response_body}` | 正文快照 |
//! | `POST /api/stats/requests/terminate` | `AppState::inflight::terminate` | 与 `/api/logs/inflight/:id/terminate` 同一动作 |
//! | `GET /api/logs/stats` | `crate::runtime_log::snapshot` | 运行日志环形缓冲 |
//! | `GET /api/logs/download` | `crate::runtime_log::export_jsonl` | JSONL 附件 |
//! | `POST /api/cloud-sync/test` | `sync::engine::test_connection` | 不落库 |
//! | `POST /api/cloud-sync/upload` | `sync::engine::upload(force)` | = `/api/admin/sync/now` |
//! | `POST /api/cloud-sync/download` | `sync::engine::pull(mode)` | = `/api/admin/sync/pull` |
//! | `GET /api/cloud-sync/remote` | `sync::transport::Transport::get_manifest` | 远端 manifest 摘要 |
//! | `POST /api/backup/upload` | `backup::handle_backup_import`（同 handler） | 文档契约不是 multipart |
//! | `GET /api/backup/progress` | 常量 `{status:"idle"}` | AIGX 是同步恢复，无进度状态机 |
//! | `/api/session/login/{start,wait,cancel,callback,sms/send,sms/verify,oauth/start}` | `account_login::handle_login_*` | 同 handler 挂第二个 path |
//!
//! ── `overview` 字段映射与单位 ───────────────────────────────
//! | A2A 字段 | AIGX 字段 | 单位换算 |
//! |---|---|---|
//! | `requests` | `total_requests` | 条 |
//! | `success` | `success_requests` | 条 |
//! | `failures` | `total_requests - success_requests` | 条 |
//! | `successRate` | 由 `success_requests ÷ total_requests` 重算 | **AIGX 的 `success_rate` 是百分数 0–100，A2A 是比值 0–1** |
//! | `totalTokens` | `input_tokens + output_tokens` | token |
//! | `promptTokens` / `completionTokens` | `input_tokens` / `output_tokens` | token |
//! | `costUsd` | `total_cost` ÷ (`fx` × `quota_per_yuan`) | **额度 → 美元等值**（见 [`quota_to_usd`]） |
//! | `activeDays` / `streak` | 同名 | 天 |
//!
//! ── 请求行字段映射与单位 ────────────────────────────────────
//! | A2A 字段 | AIGX 字段 | 单位换算 |
//! |---|---|---|
//! | `id` | `request_id`（缺省回落 `id`） | 字符串 |
//! | `ts` | `created_at` × 1000 | **AIGX 是 unix 秒，A2A 是 unix 毫秒** |
//! | `model` / `upstreamModel` | `model` | 上游真名 |
//! | `clientModel` | `origin_model`（缺省回落 `model`） | 客户端请求名 |
//! | `status` | `status_code` + `error_msg` 派生 | `ok` / `failed` / `running` |
//! | `durationMs` | `latency_ms` | 毫秒（同名同单位） |
//! | `firstResponseMs` | `first_response_ms` | 毫秒 |
//! | `attempts` | `attempts` | 次 |
//! | `promptTokens` / `completionTokens` / `totalTokens` | `input_tokens` / `output_tokens` / 两者之和 | token |
//! | `provider` / `phase` / `client` / `apiKeyId` / `error` | 同名（`key_id` → `apiKeyId`） | — |
//! | `costUsd` | `cost` 经 [`quota_to_usd`] | 额度 → 美元等值 |
//!
//! ── 明确**不做**的路径（无数据源，不造空壳）─────────────────
//! - `POST /api/stats/requests/compact`：AIGX 没有「请求统计库」这个独立库，
//!   也没有暴露给 `AppState` 的压缩入口（`VACUUM` 在
//!   `storage::sqlite::SqliteStore` 内部）。拿 `/api/logs/agg/rebuild` 顶替是
//!   **另一种语义**（重建日聚合 ≠ 压缩库），所以不给这个别名。
//! - `GET /api/model-access/{keys,models}`：AIGX 没有网关 Key 作用域概念
//!   （`frontend/src/components/chat/AsstSettingsDrawer.tsx` 已注明「AIGX 无该
//!   概念」），`src/` 里 grep `model_access` / `model-access` 零命中 —— 无数据源。
//! - `GET /api/session/login/methods`：A2A 回的是运行期可查的
//!   `LoginMethodMetadata`（`methods[]` / `prompts[]` / `instructions` /
//!   `dynamicPrompts`）。AIGX 的登录方式是编译期内置（`account_login/` 18 家 +
//!   插件宿主声明），最近的近似是 `/api/account-sessions/providers` 里的
//!   `login.methods`，**形状不同**，照抄会把客户端读 `methods[].kind` 读成 null。
//! - `POST /api/stats/requests` 的 `DELETE`、`GET /api/stats/requests/clear-preview`：
//!   AIGX 对应物是 `/api/logs/requests/clear` 与 `/api/logs/requests/clear-preview`，
//!   **路径已存在且语义等价**，不需要别名（本次任务也未要求）。
//!
//! ── 校验顺序（与 A2A 一致）────────────────────────────────
//! 先全部校验，再统一落盘：任一字段非法则整体 400 且配置不动，避免
//! 「连接超时改了、流空闲没改」这种半套设置。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap / expect / panic。

use std::collections::HashMap;

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{Json, Response};
use axum::routing::{get, post};
use serde_json::{json, Value};

use super::common::{
    admin_id_from_session, error_response, record_audit, update_admin_config, verify_admin,
};
use crate::api::openai::AppState;

/// 超时上下界（秒）。与 A2A `TIMEOUT_MIN_SECONDS` / `TIMEOUT_MAX_SECONDS` 同量级。
const TIMEOUT_MIN_SECONDS: u64 = 1;
const TIMEOUT_MAX_SECONDS: u64 = 3600;
/// 重试次数上界（与 config 的换号硬上限一致）。
const RETRY_MAX_COUNT: u32 = 100;
/// 重试间隔上界（秒）。
const RETRY_MAX_INTERVAL_SECONDS: u64 = 300;
/// 直接换号的状态码范围。
const STATUS_CODE_MIN: u16 = 100;
const STATUS_CODE_MAX: u16 = 599;

/// 报表时段的合法取值（与 A2A `stats_api::RANGES` 逐字一致）。
const STATS_RANGES: [&str; 7] = ["today", "yesterday", "7", "30", "month", "all", "custom"];
/// 报表时段缺省值：与 A2A `DEFAULT_RANGE` 一致。
const STATS_DEFAULT_RANGE: &str = "7";
/// `range=all` 的可回溯上限（天）。
///
/// AIGX 的报表窗口在 `dashboard::resolve_window` 里硬夹 365 天（护栏：日粒度
/// 聚合表不无限回溯）；A2A 的 `all` 是「全表」。这里把 `all` 映射成 **最近 365
/// 天**并在响应里带 `rangeClamped: true`，让调用方看得出这个差 —— 静默给一小段
/// 数据比给一个明确的夹紧标记更容易被误读成「就这么多」。
const STATS_ALL_MAX_DAYS: u32 = 365;
/// `/api/stats/requests` 的每页硬上限（与 AIGX 日志列表 `PAGE_SIZE_MAX` 同量级）。
const STATS_REQUESTS_MAX_LIMIT: usize = 500;
/// `/api/stats/requests` 的每页缺省条数（与 A2A `DEFAULT_LIMIT` 一致）。
const STATS_REQUESTS_DEFAULT_LIMIT: usize = 50;
/// 筛选候选 / 原始正文的窗口回溯上限（秒）：一年。
///
/// A2A 的 `filter_options` 不限时间（它整表 GROUP BY）。AIGX 的明细在 KV 里
/// 按 `reqlog:{ts}:{id}` 分键，整表枚举在十万级保留量下是秒级 —— 与
/// `dashboard::handle_report_top_sources` 的取舍一致：取一年窄窗 + 条数护栏，
/// 超限在响应里标 `sampled: true`。
const STATS_SCAN_WINDOW_SECS: i64 = 365 * 86_400;
/// 上述窗口读的条数护栏。
const STATS_SCAN_MAX_ROWS: usize = 20_000;

/// 管理面路由。
///
/// 每条都是「同一个 handler / 同一个数据源、第二个 path」，没有第二份实现。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        // ── 配置类（A2A 独立端点 → AIGX 配置字段）────────────────
        .route("/api/timeouts", get(get_timeouts).put(put_timeouts))
        .route("/api/retry", get(get_retry).put(put_retry))
        .route("/api/cors", get(get_cors).put(put_cors))
        // ── 报表（A2A stats_api.rs → AIGX /api/dashboard/report/*）──
        .route("/api/stats/summary", get(stats_summary))
        .route("/api/stats/requests", get(stats_requests))
        .route("/api/stats/requests/detail", get(stats_request_detail))
        .route("/api/stats/requests/filters", get(stats_request_filters))
        .route("/api/stats/requests/raw", get(stats_request_raw))
        .route(
            "/api/stats/requests/terminate",
            post(stats_request_terminate),
        )
        // ── 运行日志（A2A logs_api.rs → AIGX runtime_log 环形缓冲）──
        .route("/api/logs/stats", get(logs_stats))
        .route("/api/logs/download", get(logs_download))
        // ── 云同步（A2A cloud_sync.rs → AIGX /api/admin/sync/*）────
        .route("/api/cloud-sync/test", post(cloud_sync_test))
        .route("/api/cloud-sync/upload", post(cloud_sync_upload))
        .route("/api/cloud-sync/download", post(cloud_sync_download))
        .route("/api/cloud-sync/remote", get(cloud_sync_remote))
        // ── 整库备份（A2A backup_api.rs → AIGX /api/backup/*）──────
        .route(
            "/api/backup/upload",
            post(super::backup::handle_backup_import),
        )
        .route("/api/backup/progress", get(backup_progress))
        // ── 登录（A2A http.rs → AIGX /api/account-sessions/login/*）──
        // 只挂 AIGX 已有的 handler；`interaction` / `wasm-callback` 两条已由
        // `plugin_login::router()` 注册在同一个 path 上，这里**不再重复**注册
        // （axum 同一个 path+method 注册两遍会在启动时 panic）。
        .route(
            "/api/session/login/start",
            post(super::account_login::handle_login_start),
        )
        .route(
            "/api/session/login/wait",
            get(super::account_login::handle_login_wait),
        )
        .route(
            "/api/session/login/cancel",
            post(super::account_login::handle_login_cancel),
        )
        .route(
            "/api/session/login/callback",
            post(super::account_login::handle_login_callback),
        )
        .route(
            "/api/session/login/sms/send",
            post(super::account_login::handle_login_sms_send),
        )
        .route(
            "/api/session/login/sms/verify",
            post(super::account_login::handle_login_sms_verify),
        )
        .route(
            "/api/session/login/oauth/start",
            post(super::account_login::handle_login_oauth_start),
        )
}

/// 解析 JSON 对象体（体积上限 + 必须是对象）。
fn json_object(body: &Bytes) -> Result<Value, (StatusCode, Json<Value>)> {
    if body.len() > 64 * 1024 {
        return Err(error_response("请求体过大", StatusCode::PAYLOAD_TOO_LARGE));
    }
    let parsed: Value = serde_json::from_slice(body)
        .map_err(|e| error_response(&format!("请求不是有效 JSON：{e}"), StatusCode::BAD_REQUEST))?;
    if !parsed.is_object() {
        return Err(error_response(
            "请求体必须是 JSON 对象",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(parsed)
}

/// 取一个 u64 字段（缺省 / null = 不改）。
fn u64_field(value: &Value, key: &str) -> Result<Option<u64>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => raw
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("{key} 必须是整数")),
    }
}

/// 取一个 u32 字段（缺省 / null = 不改）。
fn u32_field(value: &Value, key: &str) -> Result<Option<u32>, String> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(raw) => {
            let number = raw.as_u64().ok_or_else(|| format!("{key} 必须是整数"))?;
            let bounded = u32::try_from(number).map_err(|_| format!("{key} 超出范围"))?;
            Ok(Some(bounded))
        }
    }
}

/// 夹到区间内并把越界当错误（配置面不做静默钳制：用户填 9999 要看到报错）。
fn bound_i<T: PartialOrd + Copy + std::fmt::Display>(
    key: &str,
    value: T,
    min: T,
    max: T,
) -> Result<T, String> {
    if value < min || value > max {
        return Err(format!("{key} 必须在 {min} 与 {max} 之间"));
    }
    Ok(value)
}

/// GET /api/timeouts
pub async fn get_timeouts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": timeouts_json(&config),
    })))
}

fn timeouts_json(config: &crate::config::AppConfig) -> Value {
    json!({
        "connectTimeoutSeconds": config.usage.connect_timeout_secs,
        "headersTimeoutSeconds": config.usage.headers_timeout_secs,
        "streamIdleTimeoutSeconds": config.usage.stream_idle_secs,
        "bodyTimeoutSeconds": config.usage.body_timeout_secs,
    })
}

/// PUT /api/timeouts —— 四项可部分提交，全部校验通过才落盘。
pub async fn put_timeouts(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let payload = json_object(&body)?;
    let connect = u64_field(&payload, "connectTimeoutSeconds")
        .and_then(|value| {
            value
                .map(|v| {
                    bound_i(
                        "connectTimeoutSeconds",
                        v,
                        TIMEOUT_MIN_SECONDS,
                        TIMEOUT_MAX_SECONDS,
                    )
                })
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let headers_to = u64_field(&payload, "headersTimeoutSeconds")
        .and_then(|value| {
            value
                .map(|v| {
                    bound_i(
                        "headersTimeoutSeconds",
                        v,
                        TIMEOUT_MIN_SECONDS,
                        TIMEOUT_MAX_SECONDS,
                    )
                })
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let stream = u64_field(&payload, "streamIdleTimeoutSeconds")
        .and_then(|value| {
            value
                .map(|v| {
                    bound_i(
                        "streamIdleTimeoutSeconds",
                        v,
                        TIMEOUT_MIN_SECONDS,
                        TIMEOUT_MAX_SECONDS,
                    )
                })
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let body_to = u64_field(&payload, "bodyTimeoutSeconds")
        .and_then(|value| {
            value
                .map(|v| {
                    bound_i(
                        "bodyTimeoutSeconds",
                        v,
                        TIMEOUT_MIN_SECONDS,
                        TIMEOUT_MAX_SECONDS,
                    )
                })
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let updated = update_admin_config(&state, &headers, move |config| {
        if let Some(value) = connect {
            config.usage.connect_timeout_secs = value;
        }
        if let Some(value) = headers_to {
            config.usage.headers_timeout_secs = value;
        }
        if let Some(value) = stream {
            config.usage.stream_idle_secs = value;
        }
        if let Some(value) = body_to {
            config.usage.body_timeout_secs = value;
        }
        Ok(())
    })
    .await?;
    Ok(Json(
        json!({ "success": true, "data": timeouts_json(&updated) }),
    ))
}

/// GET /api/retry
pub async fn get_retry(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(
        json!({ "success": true, "data": retry_json(&config) }),
    ))
}

fn retry_json(config: &crate::config::AppConfig) -> Value {
    json!({
        "retryCount": config.usage.retry_self_count,
        "retryCrossProviderCount": config.usage.retry_switch_count,
        // AIGX 内部以毫秒存；A2A 对外以秒为单位，向下取整（0 = 不等待）。
        "retryIntervalSeconds": config.usage.retry_interval_ms / 1000,
        "noRetryStatusCodes": config.usage.retry_on_status,
    })
}

/// PUT /api/retry —— 四项可部分提交，全部校验通过才落盘。
pub async fn put_retry(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let payload = json_object(&body)?;
    let retry_count = u32_field(&payload, "retryCount")
        .and_then(|value| {
            value
                .map(|v| bound_i("retryCount", v, 0, RETRY_MAX_COUNT))
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let switch_count = u32_field(&payload, "retryCrossProviderCount")
        .and_then(|value| {
            value
                .map(|v| bound_i("retryCrossProviderCount", v, 0, RETRY_MAX_COUNT))
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let interval_seconds = u64_field(&payload, "retryIntervalSeconds")
        .and_then(|value| {
            value
                .map(|v| bound_i("retryIntervalSeconds", v, 0, RETRY_MAX_INTERVAL_SECONDS))
                .transpose()
        })
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    // 状态码数组：全部在 100–599 内，去重升序（与 /api/limits 的存法一致）。
    let codes = match payload.get("noRetryStatusCodes") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            let mut codes: Vec<u16> = Vec::with_capacity(items.len());
            for item in items {
                let code = item.as_u64().ok_or_else(|| {
                    error_response("noRetryStatusCodes 只能包含整数", StatusCode::BAD_REQUEST)
                })?;
                let code = u16::try_from(code).map_err(|_| {
                    error_response("noRetryStatusCodes 只能包含整数", StatusCode::BAD_REQUEST)
                })?;
                if !(STATUS_CODE_MIN..=STATUS_CODE_MAX).contains(&code) {
                    return Err(error_response(
                        "noRetryStatusCodes 的取值必须在 100 与 599 之间",
                        StatusCode::BAD_REQUEST,
                    ));
                }
                codes.push(code);
            }
            codes.sort_unstable();
            codes.dedup();
            Some(codes)
        }
        Some(_) => {
            return Err(error_response(
                "noRetryStatusCodes 必须是数组",
                StatusCode::BAD_REQUEST,
            ))
        }
    };
    let updated = update_admin_config(&state, &headers, move |config| {
        if let Some(value) = retry_count {
            config.usage.retry_self_count = value;
        }
        if let Some(value) = switch_count {
            config.usage.retry_switch_count = value;
        }
        if let Some(seconds) = interval_seconds {
            config.usage.retry_interval_ms = seconds.saturating_mul(1000);
        }
        if let Some(codes) = codes {
            config.usage.retry_on_status = codes;
        }
        Ok(())
    })
    .await?;
    Ok(Json(
        json!({ "success": true, "data": retry_json(&updated) }),
    ))
}

/// GET /api/cors —— `corsEnabled` = 允许来源里含通配 `*`。
///
/// A2A 的开关语义是「网关面对任意来源放行」，AIGX 的来源是列表；两者等价
/// 表达就是「列表里有没有 `*`」。同时把 `origins` 原样带出，让调用方能看到
/// 更细的配置，不必猜。
pub async fn get_cors(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({ "success": true, "data": cors_json(&config) })))
}

fn cors_json(config: &crate::config::AppConfig) -> Value {
    let origins = config.cors_origins.clone();
    let enabled = origins.iter().any(|origin| is_wildcard(origin));
    json!({
        "corsEnabled": enabled,
        "origins": origins,
    })
}

/// 是否通配来源（`*`）。
fn is_wildcard(origin: &str) -> bool {
    origin.trim() == "*"
}

/// PUT /api/cors —— `{corsEnabled}`；true → `["*"]`，false → 清空。
///
/// 显式来源列表不会被本端点改写（只有「开/关通配」两种动作），要配细粒度
/// 来源请走 `/api/gateway/settings`。
pub async fn put_cors(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let payload = json_object(&body)?;
    let Some(raw) = payload.get("corsEnabled") else {
        return Err(error_response("缺少 corsEnabled", StatusCode::BAD_REQUEST));
    };
    let Some(enabled) = raw.as_bool() else {
        return Err(error_response(
            "corsEnabled 必须是 true 或 false",
            StatusCode::BAD_REQUEST,
        ));
    };
    let updated = update_admin_config(&state, &headers, move |config| {
        if enabled {
            if !config.cors_origins.iter().any(|origin| is_wildcard(origin)) {
                config.cors_origins.push("*".to_string());
            }
        } else {
            config.cors_origins.retain(|origin| !is_wildcard(origin));
        }
        Ok(())
    })
    .await?;
    Ok(Json(
        json!({ "success": true, "data": cors_json(&updated) }),
    ))
}

// ══════════════════════════════════════════════════════════════════
// 报表：`/api/stats/*`（A2A `api/stats_api.rs`）
//
// 全部**直接调用 AIGX 既有 handler**（`dashboard::handle_report_*`、
// `logs::handle_list_request_logs`、`logs::RequestLogQuery`）：不发 HTTP 自请求，
// 也不复制一份查询逻辑。别名层只做两件事 —— 参数名换算（offset/limit ↔
// page/size、毫秒 ↔ 秒、range ↔ 自然日区间）与响应字段改名。
// ══════════════════════════════════════════════════════════════════

/// 查询串：A2A 的口径是「有则用、无则忽略」，HashMap 比给每个参数定义结构体
/// 更贴合（与 A2A `stats_api.rs` 的 `type Params` 同一写法）。
type Params = HashMap<String, String>;

/// 取「有效」的文本参数：空串与全空白一律当缺省，返回值已 trim。
///
/// 前端把输入框清空时会发出 `?model=`，这类值若交给下游过滤会变成
/// 「模型名等于空串」这种永不命中的条件（页面看起来是空的）。
fn text_of(params: &Params, key: &str) -> Option<String> {
    params
        .get(key)
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// A2A 的 `start` / `end` 是 **unix 毫秒**，AIGX 的日志过滤是 **unix 秒**。
///
/// 非法 / 空串 / 非正数一律当「没给」—— 与 A2A 同一取舍（筛选参数不合法就
/// 当它不存在，为一次筛选让整页报错不划算）。毫秒 → 秒**向下取整**：区间是
/// 闭区间，向下取整最多多含一条边界记录，比向上取整漏掉它更符合「筛这一段」
/// 的直觉。
fn ms_param_to_secs(params: &Params, key: &str) -> Option<i64> {
    let text = params
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())?;
    let ms = text.parse::<i64>().ok()?;
    if ms <= 0 {
        return None;
    }
    Some(ms / 1000)
}

/// `/api/stats/requests` 的分页：A2A 是 `offset` / `limit`，AIGX 日志列表是
/// `page` / `size`（页码从 1 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatsPage {
    offset: usize,
    limit: usize,
}

impl StatsPage {
    /// 生效的分页：`offset` 缺省/负数/非数字 → 0；`limit` 缺省 50、夹在 1..=500
    /// （夹紧只在这里做一处，与 AIGX 日志列表的 `PAGE_SIZE_MAX` 同量级）。
    fn from_params(params: &Params) -> Self {
        let offset = params
            .get("offset")
            .map(|text| text.trim())
            .and_then(|text| text.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(0) as usize;
        let limit = params
            .get("limit")
            .map(|text| text.trim())
            .and_then(|text| text.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .map(|value| (value as usize).clamp(1, STATS_REQUESTS_MAX_LIMIT))
            .unwrap_or(STATS_REQUESTS_DEFAULT_LIMIT);
        Self { offset, limit }
    }

    /// 换算成 AIGX 日志列表的页码（1 起）。
    ///
    /// `offset` 不是 `limit` 的整数倍时页码表达不了它，这里**向上取整**（少丢行），
    /// 再由 [`Self::snapped`] 如实标出「第一行不是你要的那一行」—— 静默吸附会
    /// 让翻页脚本跳行而查不出原因。
    fn page(&self) -> usize {
        self.offset / self.limit + 1
    }

    /// 实际生效窗口的首行下标。
    fn first_offset(&self) -> usize {
        (self.page() - 1) * self.limit
    }

    /// 是否发生了对齐吸附（请求的 offset 无法用整页表达）。
    fn snapped(&self) -> bool {
        self.first_offset() != self.offset
    }
}

/// 自然日区间 `[start, end]` 的天数（含两端），夹在 `1..=365`。
fn span_days(start: &str, end: &str) -> u32 {
    let Ok(from) = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d") else {
        return 1;
    };
    let Ok(to) = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d") else {
        return 1;
    };
    let span = (to - from).num_days() + 1;
    span.clamp(1, i64::from(STATS_ALL_MAX_DAYS)) as u32
}

/// 解析 `range` / `startDate` / `endDate` → （生效 range、跨度天数、起、止）。
///
/// 返回的 `(start, end)` 一律是可直接喂给 `dashboard::ReportQuery` 的
/// `YYYY-MM-DD`；`days` 只作兜底（`resolve_window` 在 start/end 合法时优先用它
/// 们，但格外的天数当晚给报表缓存键用）。
///
/// 非法 `range` 一律 400 并列出合法值 —— 与 A2A 同口径：报表区间是用户显式选的
/// 值，静默换成别的区间会让页面显示的与选项对不上。
/// 报表区间解析的返回：`(生效 range 原文, 跨度天数, start, end)`。
/// 用别名压住 clippy::type_complexity（-D warnings 下嵌套元组返回会直接报错）。
type StatsRangeResult = Result<(String, u32, String, String), (StatusCode, Json<Value>)>;

fn resolve_stats_range(params: &Params) -> StatsRangeResult {
    let requested = text_of(params, "range").unwrap_or_else(|| STATS_DEFAULT_RANGE.to_string());
    if !STATS_RANGES.contains(&requested.as_str()) {
        return Err(error_response(
            &format!(
                "range 取值非法: {requested}（合法值: {}）",
                STATS_RANGES.join("、")
            ),
            StatusCode::BAD_REQUEST,
        ));
    }
    let today = crate::log::daily_agg::today_key();
    let today_date = chrono::NaiveDate::parse_from_str(&today, "%Y-%m-%d").map_err(|error| {
        error_response(
            &format!("报表日键不可解析（{today}）：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let as_text = |date: chrono::NaiveDate| date.format("%Y-%m-%d").to_string();
    let (start, end) = match requested.as_str() {
        "today" => (today.clone(), today.clone()),
        "yesterday" => {
            let yesterday = today_date - chrono::Duration::days(1);
            (as_text(yesterday), as_text(yesterday))
        }
        "7" => (
            as_text(today_date - chrono::Duration::days(6)),
            today.clone(),
        ),
        "30" => (
            as_text(today_date - chrono::Duration::days(29)),
            today.clone(),
        ),
        "month" => {
            // 本月 1 号 → 今天（A2A 的 month 是「本自然月」）。直接改日键的日段，
            // 不做日期运算：日键是定长 `YYYY-MM-DD`，取前 7 字节加 `-01` 即本月首日。
            let prefix = today.get(0..7).unwrap_or(today.as_str());
            (format!("{prefix}-01"), today.clone())
        }
        "all" => (
            as_text(today_date - chrono::Duration::days(i64::from(STATS_ALL_MAX_DAYS) - 1)),
            today.clone(),
        ),
        _ => {
            // custom：两个日期都必须给且合法（A2A 同口径，含「起止倒置 → 400」）
            let start = text_of(params, "startDate").ok_or_else(|| {
                error_response("range=custom 时必须提供 startDate", StatusCode::BAD_REQUEST)
            })?;
            let end = text_of(params, "endDate").ok_or_else(|| {
                error_response("range=custom 时必须提供 endDate", StatusCode::BAD_REQUEST)
            })?;
            let start_date =
                chrono::NaiveDate::parse_from_str(&start, "%Y-%m-%d").map_err(|_| {
                    error_response(
                        &format!("startDate 必须是 YYYY-MM-DD（收到: {start}）"),
                        StatusCode::BAD_REQUEST,
                    )
                })?;
            let end_date = chrono::NaiveDate::parse_from_str(&end, "%Y-%m-%d").map_err(|_| {
                error_response(
                    &format!("endDate 必须是 YYYY-MM-DD（收到: {end}）"),
                    StatusCode::BAD_REQUEST,
                )
            })?;
            if start_date > end_date {
                return Err(error_response(
                    &format!("开始日期不能晚于结束日期（startDate={start}, endDate={end}）"),
                    StatusCode::BAD_REQUEST,
                ));
            }
            (as_text(start_date), as_text(end_date))
        }
    };
    let days = span_days(&start, &end);
    Ok((requested, days, start, end))
}

/// 取 AIGX 报表 handler 的 `data` 段（它们统一回 `{success,data}`）。
///
/// 缺 `data` 时回 `null` 而不是 `{}`：报表前端把 null 当「无数据」，
/// 给一个空对象会渲染成一行空行。
fn payload_of(response: &Json<Value>) -> Value {
    response.0.get("data").cloned().unwrap_or(Value::Null)
}

/// 额度 → 美元等值。
///
/// AIGX 的 `cost` / `total_cost` 是**配额额度**（`1 元 = quota_per_yuan 额度`，
/// 写向公式见 `pricing::usd_cost_to_quota`），A2A 的 `costUsd` 是美元。反向
/// 换算：`usd = quota / (fx × quota_per_yuan)`。
///
/// 两点口径必须写清，不然后人会把这两个数当同一个东西：
/// 1. 这是**按当前兑换倍率折算的美元等值**（含销售倍率），不是上游原始美元
///    成本 —— AIGX 的上游成本另存 `channel_cost`，也没有独立的美元字段；
/// 2. `usd_cost_to_quota` 向上取整（`+0.999999`），所以反向值可能比原始美元
///    略小（不足 1 个额度单位），报表量级下可忽略。
fn quota_to_usd(quota: i64) -> f64 {
    let rate = crate::pricing::exchange_rate::usd_to_cny_rate() * crate::pricing::quota_per_yuan();
    quota_to_usd_with(quota, rate)
}

/// [`quota_to_usd`] 的纯函数变体（单测直接喂倍率，不碰全局快照）。
///
/// `rate` 是 `fx × quota_per_yuan` 的乘积；非正 / NaN / ∞ 时回 0.0 —— NaN 序列化
/// 不进 JSON，会让整个报表响应变成 500，这类配置被写坏的极端情况绝不能传染到响应层。
fn quota_to_usd_with(quota: i64, rate: f64) -> f64 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0.0;
    }
    quota as f64 / rate
}

/// AIGX 报表 `overview` → A2A `overview`（字段名与单位见文件头表格）。
fn stats_overview_json(source: &Value) -> Value {
    let total = source
        .get("total_requests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let success = source
        .get("success_requests")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let input_tokens = source
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = source
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cost = source
        .get("total_cost")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // A2A 的 successRate 是 0–1 的比值；AIGX 的 success_rate 是 0–100 的百分数。
    // 这里重算比值而不是拿 AIGX 那个百分数除 100：AIGX 侧已经 round 到两位小数，
    // 再除一次会把误差放大到 1e-4 量级。零请求时给 null（0.0 会被读成「全失败」）。
    let success_rate = if total > 0 {
        Some(success as f64 / total as f64)
    } else {
        None
    };
    json!({
        "requests": total,
        "success": success,
        "failures": total.saturating_sub(success),
        "successRate": success_rate,
        "totalTokens": input_tokens.saturating_add(output_tokens),
        "promptTokens": input_tokens,
        "completionTokens": output_tokens,
        "costUsd": quota_to_usd(cost),
        "activeDays": source.get("active_days").and_then(Value::as_u64).unwrap_or(0),
        "streak": source.get("streak").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// A2A 的三态请求状态：`ok` / `failed` / `running`。
///
/// AIGX 的 `RequestLog` 没有状态字符串：`status_code == 0` 是在途占位行（转发
/// 一开始就写），有 `error_msg` 或 4xx/5xx 是失败，其余成功 —— 与 AIGX 日志列表
/// 的 `status=ok/error` 过滤口径同源（`RequestLogStore::list_with_filter_ext`）。
fn request_status(status_code: u64, error: Option<&str>) -> &'static str {
    if status_code == 0 {
        return "running";
    }
    if error.is_some_and(|text| !text.trim().is_empty()) || status_code >= 400 {
        return "failed";
    }
    "ok"
}

/// 一条 AIGX 请求日志 → A2A 请求行（映射表见文件头）。
fn stats_row_json(row: &Value) -> Value {
    let input_tokens = row.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
    let output_tokens = row
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let status_code = row.get("status_code").and_then(Value::as_u64).unwrap_or(0);
    let error = row
        .get("error_msg")
        .and_then(Value::as_str)
        .map(str::to_string);
    let id = row
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| row.get("id").and_then(Value::as_str))
        .unwrap_or_default();
    let model = row.get("model").and_then(Value::as_str).unwrap_or_default();
    let client_model = row
        .get("origin_model")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(model);
    // AIGX 的 created_at 是 unix **秒**；A2A 行里的 ts 是 unix **毫秒**。
    let ts = row
        .get("created_at")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .saturating_mul(1000);
    json!({
        "id": id,
        "ts": ts,
        "model": model,
        // AIGX 的 model 就是「上游真名」（用户请求名另存 origin_model），
        // 与 A2A 的 upstreamModel 同义
        "upstreamModel": model,
        "clientModel": client_model,
        "provider": row.get("provider").and_then(Value::as_str).unwrap_or_default(),
        "status": request_status(status_code, error.as_deref()),
        "durationMs": row.get("latency_ms").and_then(Value::as_u64).unwrap_or(0),
        "firstResponseMs": row.get("first_response_ms").and_then(Value::as_u64),
        "attempts": row.get("attempts").and_then(Value::as_u64).unwrap_or(0),
        "error": error,
        "promptTokens": input_tokens,
        "completionTokens": output_tokens,
        "totalTokens": input_tokens.saturating_add(output_tokens),
        "phase": row.get("phase").and_then(Value::as_str).unwrap_or_default(),
        "client": row.get("client_name").and_then(Value::as_str).unwrap_or_default(),
        "apiKeyId": row.get("key_id").and_then(Value::as_str),
        // AIGX 独有的一维（A2A 没有）：缓存命中标记，报表按它算命中率
        "cacheHit": row.get("cache_hit").and_then(Value::as_bool).unwrap_or(false),
        "costUsd": quota_to_usd(row.get("cost").and_then(Value::as_i64).unwrap_or(0)),
    })
}

/// GET /api/stats/summary?range=7&startDate=&endDate=
///
/// 组合 AIGX 的五条报表 handler（概览 / 热力图 / 缓存命中率 / 近 24h 命中趋势 /
/// 日趋势）。`providers` / `accounts` 两个维度 AIGX 的报表没有对应实现，故不
/// 出现在响应里（前端按「存在则展示」消费，缺席不会崩）。
pub async fn stats_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let (range, days, start, end) = resolve_stats_range(&params)?;
    let report_query = || super::dashboard::ReportQuery {
        days,
        start: Some(start.clone()),
        end: Some(end.clone()),
    };
    let overview = super::dashboard::handle_report_overview(
        State(state.clone()),
        headers.clone(),
        Query(report_query()),
    )
    .await?;
    let heatmap = super::dashboard::handle_report_heatmap(
        State(state.clone()),
        headers.clone(),
        Query(report_query()),
    )
    .await?;
    let cache_rates = super::dashboard::handle_report_cache_rates(
        State(state.clone()),
        headers.clone(),
        Query(report_query()),
    )
    .await?;
    // 近 24h 命中趋势的窗口固定 24 小时，不吃 days/start/end（与 AIGX 原端点一致）
    let cache_trend =
        super::dashboard::handle_report_cache_trend_24h(State(state.clone()), headers.clone())
            .await?;
    let daily_trend = super::dashboard::handle_report_daily_trend(
        State(state.clone()),
        headers.clone(),
        Query(report_query()),
    )
    .await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "range": range,
            "startDate": start,
            "endDate": end,
            // AIGX 的报表窗口硬夹 365 天（`dashboard::resolve_window`），所以
            // range=all 实际只覆盖最近一年 —— 显式标出来，别让「全量」被读成
            // 「全部历史」。其余档位本来就是窗口内的，标记为 false。
            "rangeClamped": range == "all",
            "overview": stats_overview_json(&payload_of(&overview)),
            "heatmap": payload_of(&heatmap),
            "cacheRates": payload_of(&cache_rates),
            "cacheTrend24h": payload_of(&cache_trend),
            "dailyTrend": payload_of(&daily_trend),
        }
    })))
}

/// GET /api/stats/requests?offset=&limit=&model=&provider=&status=&start=&end=
///
/// `provider` / `apiKey` 两个维度 AIGX 的日志存储没有对应过滤列（`RequestLogStore
/// ::list_with_filter_ext` 只认 user/model/channel/status/keyword/client/时间窗），
/// 这里**不假装筛过**：把被忽略的参数名放进 `ignoredFilters` 如实回给调用方。
pub async fn stats_requests(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let page = StatsPage::from_params(&params);
    let start = ms_param_to_secs(&params, "start");
    let end = ms_param_to_secs(&params, "end");
    let mut ignored: Vec<&str> = Vec::new();
    if text_of(&params, "provider").is_some() {
        ignored.push("provider");
    }
    if text_of(&params, "apiKey").is_some() {
        ignored.push("apiKey");
    }
    let requested_status = text_of(&params, "status");
    let status = requested_status
        .as_deref()
        .filter(|value| matches!(*value, "ok" | "error"))
        .map(str::to_string);
    if requested_status.is_some() && status.is_none() {
        // `running` 在 AIGX 不是日志过滤值（在途行要读 `inflight` 表）
        ignored.push("status");
    }
    let query = super::logs::RequestLogQuery {
        user: None,
        model: text_of(&params, "model"),
        channel: text_of(&params, "channel"),
        status,
        keyword: text_of(&params, "keyword"),
        client: text_of(&params, "client"),
        start,
        end,
        page: page.page(),
        size: page.limit,
        // 两个字段都填：AIGX 的 `effective_size` 是 `size` 优先、`page_size` 兜底
        page_size: Some(page.limit),
    };
    let response =
        super::logs::handle_list_request_logs(State(state.clone()), headers.clone(), Query(query))
            .await?;
    let payload = payload_of(&response);
    let rows: Vec<Value> = payload
        .get("data")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(stats_row_json).collect())
        .unwrap_or_default();
    let total = payload.get("total").and_then(Value::as_u64).unwrap_or(0);
    Ok(Json(json!({
        "success": true,
        "data": {
            "entries": rows,
            "total": total,
            // AIGX 的日志列表只回「命中总数」，没有再拆一层「筛选命中数」，
            // 所以 matched 与 total 同值（A2A 那边是两个独立 COUNT）
            "matched": total,
            "offset": page.first_offset(),
            "limit": page.limit,
            "offsetSnapped": page.snapped(),
            "ignoredFilters": ignored,
        }
    })))
}

/// GET /api/stats/requests/detail?id=<request_id>
///
/// A2A 还认 `rowId`（SQLite 的 rowid）。AIGX 的明细没有数值主键，只有字符串
/// `id` / `request_id` —— 给一条明确的 400 比拿 rowId 去撞 request_id 好。
pub async fn stats_request_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let id = text_of(&params, "id").ok_or_else(|| {
        error_response(
            "缺少 id 参数（AIGX 的请求明细按 request_id 查询，没有 rowId 维度）",
            StatusCode::BAD_REQUEST,
        )
    })?;
    let Some(log) = state.log_store.requests.get_by_request_id(&id) else {
        return Err(error_response(
            "未找到该请求的详细记录",
            StatusCode::NOT_FOUND,
        ));
    };
    let mut data = serde_json::to_value(&log).map_err(|error| {
        error_response(
            &format!("请求明细序列化失败: {error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    if let Some(object) = data.as_object_mut() {
        // A2A 用这个标记告诉前端「详情是完整加载的」（列表行是窄行）。
        // AIGX 的 `RequestLog` 落库即全量，天然为 true。
        object.insert("detailsLoaded".into(), Value::Bool(true));
    }
    Ok(Json(json!({ "success": true, "data": data })))
}

/// GET /api/stats/requests/filters —— 筛选下拉的候选清单。
///
/// 清单来自**明细里实际出现过的值**，不是当前配置清单：历史请求用过的模型名、
/// 已删除账号所属的 provider 同样要能筛到。
pub async fn stats_request_filters(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let now = chrono::Utc::now().timestamp();
    let (logs, sampled) = state
        .log_store
        .requests
        .list_window(now - STATS_SCAN_WINDOW_SECS, now, STATS_SCAN_MAX_ROWS)
        .map_err(|error| {
            error_response(
                &format!("筛选选项读取失败: {error}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    let data = filters_json(&logs, sampled);
    Ok(Json(json!({ "success": true, "data": data })))
}

/// 从一批请求日志里数出模型 / 供应商候选（纯函数，单测直接喂样本）。
///
/// 排序：出现次数降序、并列按名字升序（与 A2A `filter_options` 同一口径）。
fn filters_json(logs: &[crate::log::RequestLog], sampled: bool) -> Value {
    let mut models: HashMap<String, u64> = HashMap::new();
    let mut providers: HashMap<String, u64> = HashMap::new();
    for log in logs {
        let model = log.model.trim();
        if !model.is_empty() {
            *models.entry(model.to_string()).or_insert(0) += 1;
        }
        if let Some(provider) = log.provider.as_deref().map(str::trim) {
            if !provider.is_empty() {
                *providers.entry(provider.to_string()).or_insert(0) += 1;
            }
        }
    }
    let sorted = |map: HashMap<String, u64>| {
        let mut rows: Vec<(String, u64)> = map.into_iter().collect();
        rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        rows
    };
    let model_rows = sorted(models);
    let provider_rows = sorted(providers);
    json!({
        // A2A 的 models 是纯名字数组（下拉直接吃），不是对象数组
        "models": model_rows
            .iter()
            .map(|(model, _)| Value::String(model.clone()))
            .collect::<Vec<Value>>(),
        // AIGX 没有「provider id → 中文展示名」的稳定映射表（`providers` 字段
        // 已经是各家自己的 id），所以 label 原样回 id，不让调用方读到空名
        "providers": provider_rows
            .iter()
            .map(|(id, count)| json!({ "id": id, "label": id, "requests": count }))
            .collect::<Vec<Value>>(),
        // 一年窗口 + 2 万条护栏是否触发（触发后清单可能不全）
        "sampled": sampled,
    })
}

/// GET /api/stats/requests/raw?id=<request_id> —— 单条请求的原始正文。
///
/// 数据源是 `RequestLog.debug`（`log_body` 开关打开时才采集）。A2A 的
/// `truncated` 是「旧版本存的正文可能不完整」标记；AIGX 的快照固定截断到
/// `MAX_SNAPSHOT_BYTES` 却没有单独记「是否截过」，所以不回那个布尔（假字段），
/// 改为把上限值回给调用方，由长度自己判断。
pub async fn stats_request_raw(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let id = text_of(&params, "id")
        .ok_or_else(|| error_response("缺少 id 参数", StatusCode::BAD_REQUEST))?;
    let Some(log) = state.log_store.requests.get_by_request_id(&id) else {
        return Err(error_response(
            &format!("未找到该请求的原始报文（id: {id}）"),
            StatusCode::NOT_FOUND,
        ));
    };
    let request_body = log
        .debug
        .as_ref()
        .and_then(|snapshot| snapshot.request_body.clone());
    let response_body = log
        .debug
        .as_ref()
        .and_then(|snapshot| snapshot.response_body.clone());
    Ok(Json(json!({
        "success": true,
        "data": {
            "id": id,
            "requestBody": request_body,
            "responseBody": response_body,
            "maxBodyBytes": crate::log::LogDebugSnapshot::MAX_SNAPSHOT_BYTES,
        }
    })))
}

/// POST /api/stats/requests/terminate?id=<inflight id> —— 终止一条在途请求。
///
/// 与 `/api/logs/inflight/:id/terminate` **同一个动作**（`inflight.terminate`），
/// 只是 A2A 从查询串取 id、AIGX 从路径段取。
pub async fn stats_request_terminate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<Params>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let id = text_of(&params, "id")
        .ok_or_else(|| error_response("缺少 id 参数", StatusCode::BAD_REQUEST))?;
    if !state.inflight.terminate(&id) {
        return Err(error_response(
            "这条请求已不在进行中（可能已结束，或来自上次启动遗留的进行中行）",
            StatusCode::NOT_FOUND,
        ));
    }
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(&state, &admin_id, "request.terminate", &id, None, None);
    Ok(Json(
        json!({ "success": true, "data": { "terminated": true, "id": id } }),
    ))
}

// ══════════════════════════════════════════════════════════════════
// 运行日志：`/api/logs/{stats,download}`（A2A `api/logs_api.rs`）
//
// AIGX 的对应物是 `crate::runtime_log`（tracing 环形缓冲，500 条），亦即
// `/api/logs/runtime{,/export}` 的数据源 —— 不是 `log_store` 里的请求日志。
// ══════════════════════════════════════════════════════════════════

/// GET /api/logs/stats —— 运行日志的分级 / 分类计数。
pub async fn logs_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let snapshot = crate::runtime_log::snapshot(None);
    Ok(Json(
        json!({ "success": true, "data": logs_stats_of(&snapshot) }),
    ))
}

/// 从 `runtime_log::snapshot` 的读数组装计数（纯函数，单测直接喂样本）。
fn logs_stats_of(snapshot: &Value) -> Value {
    let empty: Vec<Value> = Vec::new();
    let entries = snapshot
        .get("entries")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let levels = ["error", "warn", "info", "debug", "trace"];
    let mut by_level: HashMap<String, u64> = HashMap::new();
    let mut by_category: HashMap<String, u64> = HashMap::new();
    for entry in entries {
        let level = entry.get("level").and_then(Value::as_str).unwrap_or("info");
        *by_level.entry(level.to_string()).or_insert(0) += 1;
        if let Some(category) = entry
            .get("category")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            *by_category.entry(category.to_string()).or_insert(0) += 1;
        }
    }
    let mut level_counts = serde_json::Map::new();
    for level in levels {
        level_counts.insert(
            level.to_string(),
            json!(by_level.get(level).copied().unwrap_or(0)),
        );
    }
    let mut category_rows: Vec<(String, u64)> = by_category.into_iter().collect();
    category_rows.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    json!({
        "total": entries.len(),
        // 级别白名单是 `RuntimeLogEntry.level` 的文档口径（error/warn/info/debug/trace），
        // 前沿顺序与 A2A `logs_store::LEVELS` 一致（严重度降序）
        "levels": levels,
        "byLevel": Value::Object(level_counts),
        "categories": category_rows
            .into_iter()
            .map(|(category, count)| json!({ "category": category, "count": count }))
            .collect::<Vec<Value>>(),
        // A2A 的 max 是保留容量上限；AIGX 是环形缓冲条数上限
        "max": snapshot.get("maxEntries").and_then(Value::as_u64).unwrap_or(0),
        "latestId": snapshot.get("latestId").and_then(Value::as_u64).unwrap_or(0),
    })
}

/// GET /api/logs/download —— 直接把运行日志当 JSONL 附件回。
///
/// 与 `/api/logs/runtime/export` 同内容同格式，只多一个 `Content-Disposition`
/// （A2A 的调用方按附件保存，靠它起文件名）。
pub async fn logs_download(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let body = crate::runtime_log::export_jsonl();
    let filename = format!("aigx-logs-{}.jsonl", log_file_stamp());
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson; charset=utf-8")
        .body(Body::from(body))
        .map_err(|error| {
            error_response(
                &format!("日志导出响应构造失败: {error}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    // 文件名用 HeaderValue 构造（含非 ASCII 时兜底不 panic）
    if let Ok(value) = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\"")) {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

/// 导出文件名的时间戳：冒号在 Windows 文件名里非法，一律换成 `-`。
fn log_file_stamp() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H-%M-%S").to_string()
}

// ══════════════════════════════════════════════════════════════════
// 云同步：`/api/cloud-sync/{test,upload,download,remote}`
// （A2A `api/cloud_sync.rs` → AIGX `/api/admin/sync/*` + `sync::engine`）
// ══════════════════════════════════════════════════════════════════

/// 解析可选的 JSON 对象体（空 body = `None`；非对象 / 非法 = 400）。
fn json_object_opt(body: &Bytes) -> Result<Option<Value>, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(None);
    }
    if body.len() > 64 * 1024 {
        return Err(error_response("请求体过大", StatusCode::PAYLOAD_TOO_LARGE));
    }
    let parsed: Value = serde_json::from_slice(body).map_err(|error| {
        error_response(
            &format!("请求不是有效 JSON：{error}"),
            StatusCode::BAD_REQUEST,
        )
    })?;
    if !parsed.is_object() {
        return Err(error_response(
            "请求体必须是 JSON 对象",
            StatusCode::BAD_REQUEST,
        ));
    }
    Ok(Some(parsed))
}

/// 云同步设置：空 body = 用已保存的设置（与 `/api/admin/sync/test` 同语义）。
fn sync_save_input(
    body: &Bytes,
) -> Result<crate::sync::config::SaveInput, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(crate::sync::config::SaveInput::default());
    }
    serde_json::from_slice(body).map_err(|error| {
        error_response(&format!("请求体解析失败: {error}"), StatusCode::BAD_REQUEST)
    })
}

/// `engine::SyncError` → 状态码：配置类 400（要改设置），其余 502（重试 / 换线路）。
fn map_sync_error(error: crate::sync::engine::SyncError) -> (StatusCode, Json<Value>) {
    let status = if error.config {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::BAD_GATEWAY
    };
    error_response(&error.message, status)
}

/// A2A 的 `mode` → `MergeMode`（纯函数，便于单测钉死白名单与报错）。
fn merge_mode_of(
    text: &str,
) -> Result<crate::sync::snapshot::MergeMode, (StatusCode, Json<Value>)> {
    let mode = text.trim();
    let mode = if mode.is_empty() { "merge" } else { mode };
    crate::sync::snapshot::MergeMode::parse(mode)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))
}

/// POST /api/cloud-sync/test —— 测试连接（不落库）。
pub async fn cloud_sync_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let input = sync_save_input(&body)?;
    let data = crate::sync::engine::test_connection(&state, input)
        .await
        .map_err(map_sync_error)?;
    Ok(Json(json!({ "success": true, "data": data })))
}

/// POST /api/cloud-sync/upload `{force?: bool}` —— 立即上传。
pub async fn cloud_sync_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let force = json_object_opt(&body)?
        .and_then(|payload| payload.get("force").and_then(Value::as_bool))
        .unwrap_or(false);
    let data = crate::sync::engine::upload(&state, true, force)
        .await
        .map_err(map_sync_error)?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    let status = data
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    record_audit(
        &state,
        &admin_id,
        "sync.now",
        &format!("A2A 别名手动云同步：{status}"),
        None,
        None,
    );
    Ok(Json(json!({ "success": true, "data": data })))
}

/// POST /api/cloud-sync/download `{mode?: "merge"|"local"|"remote"}` —— 下载并应用。
pub async fn cloud_sync_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mode_text = json_object_opt(&body)?
        .and_then(|payload| {
            payload
                .get("mode")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_default();
    let mode = merge_mode_of(&mode_text)?;
    let data = crate::sync::engine::pull(&state, mode)
        .await
        .map_err(map_sync_error)?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    let written = data.get("written").and_then(Value::as_u64).unwrap_or(0);
    record_audit(
        &state,
        &admin_id,
        "sync.pull",
        &format!(
            "A2A 别名云同步下载：模式 {}，写入 {written} 键",
            mode.as_str()
        ),
        None,
        None,
    );
    Ok(Json(json!({ "success": true, "data": data })))
}

/// GET /api/cloud-sync/remote —— 远端快照摘要（二次确认弹窗用）。
///
/// A2A 回的是远端快照的 `manifest` 摘要；AIGX 的同一个东西就是
/// `Transport::get_manifest` 读到的 `SyncManifest`（上传时最后写入的那份）。
pub async fn cloud_sync_remote(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let config = crate::sync::config::load(&state.scheduled_task_store);
    // 配置类失败（地址 / 密码 / 路径非法）是用户要改设置 → 400，不是远端故障
    let transport = crate::sync::transport::Transport::from_config(&config)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let remote = transport
        .get_manifest()
        .await
        .map_err(|message| error_response(&message, StatusCode::BAD_GATEWAY))?;
    let Some((_, manifest)) = remote else {
        return Ok(Json(json!({
            "success": true,
            "data": {
                "available": false,
                // 远端还没有上传过任何快照（不是错误：首次上传前的正常状态）
                "remotePath": transport.display_path(),
            }
        })));
    };
    Ok(Json(json!({
        "success": true,
        "data": {
            "available": true,
            "remotePath": transport.display_path(),
            "device": manifest.device,
            // AIGX 的 created_at 是毫秒（`sync::now_ms`），A2A 也是毫秒 → 同单位
            "createdAt": manifest.created_at,
            "entries": manifest.entry_count,
            "bytes": manifest.snapshot_bytes,
            "contentHash": manifest.content_hash,
            "format": manifest.format,
            "appVersion": manifest.app_version,
        }
    })))
}

// ══════════════════════════════════════════════════════════════════
// 整库备份：`/api/backup/{upload,progress}`（A2A `api/backup_api.rs`）
// ══════════════════════════════════════════════════════════════════

/// GET /api/backup/progress —— A2A 的恢复进度查询。
///
/// AIGX 的导入 / 恢复是**请求内同步完成**的（`backup::handle_backup_import` 与
/// 加密导入都在同一次请求里写完 KV 再返回），没有 A2A 那种跨请求的进度状态机。
/// 按「不假装有进度」的约定固定回 `idle`，并把原因写进 `note` —— 前端据此隐藏
/// 进度条，而不是转圈等到超时。
pub async fn backup_progress(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "status": "idle",
            "note": "AIGX 为同步恢复：导入在请求内完成，没有跨请求的进度状态机",
        }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with() -> crate::config::AppConfig {
        crate::config::AppConfig::default()
    }

    #[test]
    fn timeouts_json_uses_a2a_field_names() {
        let mut config = config_with();
        config.usage.connect_timeout_secs = 30;
        config.usage.headers_timeout_secs = 300;
        config.usage.stream_idle_secs = 300;
        config.usage.body_timeout_secs = 120;
        let value = timeouts_json(&config);
        assert_eq!(value["connectTimeoutSeconds"].as_u64(), Some(30));
        assert_eq!(value["headersTimeoutSeconds"].as_u64(), Some(300));
        assert_eq!(value["streamIdleTimeoutSeconds"].as_u64(), Some(300));
        assert_eq!(value["bodyTimeoutSeconds"].as_u64(), Some(120));
    }

    #[test]
    fn retry_json_converts_millis_to_seconds() {
        let mut config = config_with();
        config.usage.retry_self_count = 3;
        config.usage.retry_switch_count = 32;
        config.usage.retry_interval_ms = 5_500;
        config.usage.retry_on_status = vec![402];
        let value = retry_json(&config);
        assert_eq!(value["retryCount"].as_u64(), Some(3));
        assert_eq!(value["retryCrossProviderCount"].as_u64(), Some(32));
        assert_eq!(value["retryIntervalSeconds"].as_u64(), Some(5));
        assert_eq!(value["noRetryStatusCodes"][0].as_u64(), Some(402));
    }

    #[test]
    fn cors_json_maps_wildcard_to_enabled() {
        let mut config = config_with();
        config.cors_origins = vec!["https://panel.example.com".into()];
        assert_eq!(cors_json(&config)["corsEnabled"].as_bool(), Some(false));
        config.cors_origins.push("*".into());
        assert_eq!(cors_json(&config)["corsEnabled"].as_bool(), Some(true));
    }

    #[test]
    fn bounds_reject_out_of_range_values() {
        assert!(bound_i(
            "connectTimeoutSeconds",
            0u64,
            TIMEOUT_MIN_SECONDS,
            TIMEOUT_MAX_SECONDS
        )
        .is_err());
        assert!(bound_i(
            "connectTimeoutSeconds",
            9_999u64,
            TIMEOUT_MIN_SECONDS,
            TIMEOUT_MAX_SECONDS
        )
        .is_err());
        assert!(bound_i(
            "connectTimeoutSeconds",
            30u64,
            TIMEOUT_MIN_SECONDS,
            TIMEOUT_MAX_SECONDS
        )
        .is_ok());
        assert!(bound_i("retryCount", 101u32, 0, RETRY_MAX_COUNT).is_err());
    }

    #[test]
    fn u32_field_rejects_fractional_and_negative() {
        let payload = json!({ "retryCount": 1.5 });
        assert!(u32_field(&payload, "retryCount").is_err());
        let payload = json!({ "retryCount": -1 });
        assert!(u32_field(&payload, "retryCount").is_err());
        let payload = json!({ "retryCount": null });
        assert_eq!(u32_field(&payload, "retryCount").unwrap(), None);
    }
}

#[cfg(test)]
mod alias_extension_tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Params {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    /// `overview` 的字段名必须与 A2A 逐字一致，且 `successRate` 是**比值**——
    /// 这是最容易搞错的一处：AIGX 内部的 `success_rate` 是百分数（75.0），
    /// A2A 的 `successRate` 是 0–1（0.75）。搞错就是「成功率 7500%」。
    #[test]
    fn overview_uses_a2a_field_names_and_ratio() {
        let source = json!({
            "total_requests": 200,
            "success_requests": 150,
            "input_tokens": 120,
            "output_tokens": 30,
            "total_cost": 0,
            "active_days": 9,
            "streak": 4,
            "success_rate": 75.0,
        });
        let value = stats_overview_json(&source);
        assert_eq!(value["requests"].as_u64(), Some(200));
        assert_eq!(value["success"].as_u64(), Some(150));
        assert_eq!(value["failures"].as_u64(), Some(50));
        let rate = value["successRate"].as_f64().unwrap_or_default();
        assert!(
            (rate - 0.75).abs() < 1e-9,
            "successRate 必须是 0–1 的比值，实际 {rate}"
        );
        assert_eq!(value["totalTokens"].as_u64(), Some(150));
        assert_eq!(value["promptTokens"].as_u64(), Some(120));
        assert_eq!(value["completionTokens"].as_u64(), Some(30));
        assert_eq!(value["activeDays"].as_u64(), Some(9));
        assert_eq!(value["streak"].as_u64(), Some(4));
        // 零请求时给 null 而不是 0.0：0.0 会被前端读成「全失败」
        let empty = stats_overview_json(&json!({}));
        assert!(empty["successRate"].is_null());
        assert_eq!(empty["requests"].as_u64(), Some(0));
    }

    /// 额度 → 美元的换算，以及「配置被写坏时不产出 NaN」的兜底。
    #[test]
    fn quota_conversion_uses_rate_and_survives_broken_config() {
        // 1 额度 = 1 美元
        assert!((quota_to_usd_with(1_000, 1.0) - 1_000.0).abs() < 1e-9);
        // fx=7（1 美元 = 7 元）、1 元 = 200 额度 ⇒ 1400 额度 = 1 美元
        assert!((quota_to_usd_with(1_400, 1_400.0) - 1.0).abs() < 1e-9);
        // 倍率 0 / 负数 / NaN：回 0.0。NaN 序列化不进 JSON，会把整个报表响应变 500
        assert_eq!(quota_to_usd_with(100, 0.0), 0.0);
        assert_eq!(quota_to_usd_with(100, -5.0), 0.0);
        assert_eq!(quota_to_usd_with(100, f64::NAN), 0.0);
    }

    /// 请求行：秒 → 毫秒（×1000）、token 合计、状态派生、request_id 回落。
    #[test]
    fn request_row_converts_seconds_to_millis() {
        let row = json!({
            "id": "log-1",
            "request_id": "req-9",
            "model": "glm-4-plus",
            "origin_model": "gpt-4o",
            "status_code": 200,
            "input_tokens": 10,
            "output_tokens": 5,
            "created_at": 1_700_000_000,
            "latency_ms": 1_234,
            "first_response_ms": 300,
            "attempts": 2,
            "phase": "done",
            "client_name": "Claude Code",
            "key_id": "k-1",
            "cache_hit": true,
            "cost": 0,
        });
        let value = stats_row_json(&row);
        assert_eq!(value["id"].as_str(), Some("req-9"));
        // AIGX 的 created_at 是 unix 秒；A2A 的 ts 是 unix 毫秒
        assert_eq!(value["ts"].as_i64(), Some(1_700_000_000_000));
        assert_eq!(value["model"].as_str(), Some("glm-4-plus"));
        assert_eq!(value["upstreamModel"].as_str(), Some("glm-4-plus"));
        assert_eq!(value["clientModel"].as_str(), Some("gpt-4o"));
        assert_eq!(value["totalTokens"].as_u64(), Some(15));
        assert_eq!(value["durationMs"].as_u64(), Some(1_234));
        assert_eq!(value["firstResponseMs"].as_u64(), Some(300));
        assert_eq!(value["status"].as_str(), Some("ok"));
        assert_eq!(value["apiKeyId"].as_str(), Some("k-1"));
        assert_eq!(value["cacheHit"].as_bool(), Some(true));
        // request_id 缺省 / 空白时回落日志 id（不回空串，否则调用方拿着空 id 再查）
        let bare = stats_row_json(&json!({ "id": "log-2", "request_id": "", "created_at": 1 }));
        assert_eq!(bare["id"].as_str(), Some("log-2"));
        assert_eq!(bare["ts"].as_i64(), Some(1_000));
    }

    #[test]
    fn request_status_is_three_state() {
        assert_eq!(request_status(0, None), "running");
        assert_eq!(request_status(200, None), "ok");
        assert_eq!(request_status(500, None), "failed");
        assert_eq!(request_status(200, Some("上游超时")), "failed");
        // 全空白的错误文案不算错误（与 `list_with_filter_ext` 的判据一致）
        assert_eq!(request_status(200, Some("   ")), "ok");
    }

    /// 分页换算：offset/limit → page/size，并对「无法整页表达」如实标记。
    #[test]
    fn pagination_maps_offset_to_page_and_flags_snapping() {
        let page = StatsPage::from_params(&params(&[("offset", "100"), ("limit", "50")]));
        assert_eq!(page.offset, 100);
        assert_eq!(page.limit, 50);
        assert_eq!(page.page(), 3); // 第 3 页首行正好是第 100 条
        assert_eq!(page.first_offset(), 100);
        assert!(!page.snapped());

        // 非整页对齐：向上取整到第 1 页，并标出吸附（不能静默跳行）
        let page = StatsPage::from_params(&params(&[("offset", "7"), ("limit", "50")]));
        assert_eq!(page.page(), 1);
        assert_eq!(page.first_offset(), 0);
        assert!(page.snapped());

        // 非法 / 越界：offset 当 0，limit 缺省 50、上限 500
        let page = StatsPage::from_params(&params(&[("offset", "-3"), ("limit", "abc")]));
        assert_eq!(page.offset, 0);
        assert_eq!(page.limit, STATS_REQUESTS_DEFAULT_LIMIT);
        let page = StatsPage::from_params(&params(&[("limit", "9999")]));
        assert_eq!(page.limit, STATS_REQUESTS_MAX_LIMIT);
        let page = StatsPage::from_params(&params(&[("limit", "0")]));
        assert_eq!(page.limit, STATS_REQUESTS_DEFAULT_LIMIT);
    }

    /// 毫秒 → 秒的换算与「非法就忽略」。
    #[test]
    fn millis_params_convert_and_ignore_garbage() {
        let value = params(&[("start", "1700000000123")]);
        assert_eq!(ms_param_to_secs(&value, "start"), Some(1_700_000_000));
        let value = params(&[("start", "")]);
        assert_eq!(ms_param_to_secs(&value, "start"), None);
        let value = params(&[("start", "yesterday")]);
        assert_eq!(ms_param_to_secs(&value, "start"), None);
        let value = params(&[("start", "-5")]);
        assert_eq!(ms_param_to_secs(&value, "start"), None);
        let value = params(&[]);
        assert_eq!(ms_param_to_secs(&value, "end"), None);
    }

    /// `range` 白名单、custom 的两个日期必填、起止倒置拒绝。
    #[test]
    fn range_rejects_unknown_and_requires_custom_dates() {
        let (status, body) = resolve_stats_range(&params(&[("range", "42")]))
            .err()
            .unwrap_or((StatusCode::OK, Json(json!({}))));
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body.0["success"].as_bool(), Some(false));

        assert!(resolve_stats_range(&params(&[("range", "custom")])).is_err());
        assert!(resolve_stats_range(&params(&[
            ("range", "custom"),
            ("startDate", "2026-05-02"),
            ("endDate", "2026-05-01"),
        ]))
        .is_err());
        assert!(resolve_stats_range(&params(&[
            ("range", "custom"),
            ("startDate", "2026-13-01"),
            ("endDate", "2026-05-01"),
        ]))
        .is_err());

        let Ok((range, days, start, end)) = resolve_stats_range(&params(&[
            ("range", "custom"),
            ("startDate", "2026-05-01"),
            ("endDate", "2026-05-03"),
        ])) else {
            panic!("合法 custom 区间应被接受");
        };
        assert_eq!(range, "custom");
        assert_eq!(days, 3);
        assert_eq!(start, "2026-05-01");
        assert_eq!(end, "2026-05-03");
    }

    /// 缺省 range 是 7（与 A2A `DEFAULT_RANGE` 一致），today 是单日窗口。
    #[test]
    fn range_defaults_to_seven_days_and_today_is_single_day() {
        let Ok((range, days, start, end)) = resolve_stats_range(&params(&[])) else {
            panic!("缺省 range 应可解析");
        };
        assert_eq!(range, STATS_DEFAULT_RANGE);
        assert_eq!(days, 7);
        assert_eq!(span_days(&start, &end), 7);

        let Ok((_, today_days, today_start, today_end)) =
            resolve_stats_range(&params(&[("range", "today")]))
        else {
            panic!("today 应可解析");
        };
        assert_eq!(today_days, 1);
        assert_eq!(today_start, today_end);

        // all 夹在 365 天（AIGX 报表窗口上限），flag 由 handler 置 true
        let Ok((_, all_days, _, _)) = resolve_stats_range(&params(&[("range", "all")])) else {
            panic!("all 应可解析");
        };
        assert_eq!(all_days, STATS_ALL_MAX_DAYS);
    }

    /// 运行日志统计：白名单里的级别缺数据时补 0（前端画固定条数）。
    #[test]
    fn logs_stats_counts_levels_and_categories() {
        let snapshot = json!({
            "entries": [
                { "level": "error", "category": "aigx::api" },
                { "level": "info", "category": "aigx::api" },
                { "level": "info", "category": "aigx::sync" },
                { "level": "warn" },
            ],
            "maxEntries": 500,
            "latestId": 42,
        });
        let value = logs_stats_of(&snapshot);
        assert_eq!(value["total"].as_u64(), Some(4));
        assert_eq!(value["byLevel"]["error"].as_u64(), Some(1));
        assert_eq!(value["byLevel"]["info"].as_u64(), Some(2));
        assert_eq!(value["byLevel"]["warn"].as_u64(), Some(1));
        assert_eq!(value["byLevel"]["trace"].as_u64(), Some(0));
        assert_eq!(value["levels"][0].as_str(), Some("error"));
        assert_eq!(
            value["categories"][0]["category"].as_str(),
            Some("aigx::api")
        );
        assert_eq!(value["categories"][0]["count"].as_u64(), Some(2));
        assert_eq!(value["max"].as_u64(), Some(500));
        assert_eq!(value["latestId"].as_u64(), Some(42));
        // 空缓冲不能炸（启动后第一秒就是这个状态）
        let empty = logs_stats_of(&json!({}));
        assert_eq!(empty["total"].as_u64(), Some(0));
    }

    /// 筛选候选：次数降序 + 名字升序，providers 的 label 不空。
    #[test]
    fn filters_sort_by_count_then_name_and_keep_sampled_flag() {
        let mut logs: Vec<crate::log::RequestLog> = Vec::new();
        for (model, provider) in [
            ("b-model", Some("p-2")),
            ("a-model", Some("p-1")),
            ("a-model", Some("p-1")),
        ] {
            let mut log = crate::log::RequestLog::new();
            log.model = model.to_string();
            log.provider = provider.map(str::to_string);
            logs.push(log);
        }
        let value = filters_json(&logs, true);
        assert_eq!(value["models"][0].as_str(), Some("a-model"));
        assert_eq!(value["models"][1].as_str(), Some("b-model"));
        assert_eq!(value["providers"][0]["id"].as_str(), Some("p-1"));
        // AIGX 没有稳定的 provider id → 展示名映射，label 原样回 id（不能是空串）
        assert_eq!(value["providers"][0]["label"].as_str(), Some("p-1"));
        assert_eq!(value["providers"][0]["requests"].as_u64(), Some(2));
        assert_eq!(value["sampled"].as_bool(), Some(true));
    }

    /// 云同步的 `mode`：缺省 merge，白名单外一律 400。
    #[test]
    fn merge_mode_rejects_unknown_and_defaults_to_merge() {
        assert_eq!(
            merge_mode_of("").ok().map(|mode| mode.as_str()),
            Some("merge")
        );
        assert_eq!(
            merge_mode_of("  ").ok().map(|mode| mode.as_str()),
            Some("merge")
        );
        assert!(merge_mode_of("remote").is_ok());
        assert!(merge_mode_of("local").is_ok());
        assert!(merge_mode_of("overwrite-everything").is_err());
    }

    /// 可选对象体：空 body 合法（用已保存的设置），标量与数组一律拒绝。
    #[test]
    fn optional_object_body_accepts_empty_and_rejects_scalars() {
        assert!(json_object_opt(&Bytes::new()).is_ok());
        assert!(json_object_opt(&Bytes::from_static(b"[]")).is_err());
        assert!(json_object_opt(&Bytes::from_static(b"42")).is_err());
        assert!(json_object_opt(&Bytes::from_static(b"{\"force\":true}")).is_ok());
    }
}
