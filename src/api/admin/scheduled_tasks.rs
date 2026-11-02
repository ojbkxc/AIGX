//! 定时任务路由（设置页「定时任务」页的数据源）。移植自 Agent2API `api/scheduled_tasks.rs`。
//!
//!   GET   /api/scheduled-tasks             读取间隔型任务的开关、间隔与运行状态
//!   PATCH /api/scheduled-tasks/{id}        改一条任务（`{enabled?, interval?}`）
//!   POST  /api/scheduled-tasks/{id}/run    立即执行一条**后端**任务
//!
//! ── 为什么与自动签到是两组接口 ───────────────────────────────
//! 自动签到是**每天定点**型（时刻字符串 + 当天去重 + 启动补签），间隔型任务是
//! 「等间隔重复」，两者的配置形状与调度语义都不同。界面上它们同在「定时任务」页
//! 展示，但读写各走各的接口 —— 强行合并成一套形状会让两边都长出一堆「本类型不适用」
//! 的字段。前端因此在这一页调两组接口。
//!
//! ── POST 别名 ───────────────────────────────────────────────
//! `configure_handler` 同时受理 PATCH 与 POST：PATCH 是规范写法，POST 是别名 ——
//! 浏览器直连时 PATCH 会触发 CORS 预检，同源虽不预检，但留 POST 别名更稳妥。
//!
//! ── 错误信封 ────────────────────────────────────────────────
//! 参数校验失败（未知 id / 间隔越界 / 没有需要更新的字段）返回**管理信封**的 400
//! `{success:false,error:"…"}`，与 `/api/retention` 同一口径。
//! 「正在执行中」（run 撞上同一任务已在跑）同样是 400 管理信封：它是**并发状态**
//! 而不是请求格式问题，前端能按一个套路处理。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::common::{error_response, verify_admin};
use crate::api::openai::AppState;
use crate::scheduled_tasks::{self, types::IntervalTaskPatch};

/// `GET /api/scheduled-tasks`：读取全部间隔型任务的开关、间隔与运行状态。
pub async fn list_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(scheduled_tasks::list(&state).await).into_response())
}

/// `PATCH /api/scheduled-tasks/{id}`：改一条任务（`{enabled?}` / `{interval?}`，只改传进来的字段）。
pub async fn configure_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    // 空 body 视为 `{}` → 「没有需要更新的字段」的 400（与 Agent2API 一致）。
    let payload: Value = if body.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&body).map_err(|error| {
            error_response(&format!("请求体解析失败: {error}"), StatusCode::BAD_REQUEST)
        })?
    };
    let mut patch = IntervalTaskPatch::default();
    if let Some(object) = payload.as_object() {
        if let Some(value) = object.get("enabled") {
            // 与 auto_checkin 同一口径：`enabled !== undefined` 时只认严格 true，
            // 其余（null / 字符串 / 0）一律置为 false。前端传的是 input.checked。
            patch.enabled = Some(value == &Value::Bool(true));
        }
        if let Some(value) = object.get("interval") {
            match value.as_i64().or_else(|| {
                value
                    .as_f64()
                    .filter(|raw| raw.is_finite() && raw.fract() == 0.0)
                    .map(|raw| raw as i64)
            }) {
                Some(interval) => patch.interval = Some(interval),
                // 非数字（含 "abc" / null / 小数）直接 400：静默忽略会让界面显示
                // 「保存成功」而值没变，比报错更难排查。
                None => {
                    return Err(error_response(
                        &format!("「{id}」的间隔必须是整数"),
                        StatusCode::BAD_REQUEST,
                    ))
                }
            }
        }
    }
    match scheduled_tasks::configure(&state, &id, patch).await {
        Ok(task) => Ok(Json(task).into_response()),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}

/// `POST /api/scheduled-tasks/{id}/run`：立即执行一条后端任务。
///
/// 只对 `runner = backend` 的任务有效（前端三个刷新任务的定时器长在页面上，后端跑不了）。
/// 响应里带**刷新后的那条任务**（含新的 `lastRunAt` / `lastResult` / `nextRunAt`）：
/// 界面点完按钮就能就地重绘状态栏，不必再跑一趟 GET。
pub async fn run_handler(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    match scheduled_tasks::run_now(&state, &id).await {
        Ok(summary) => Ok(Json(json!({
            "summary": summary,
            "task": scheduled_tasks::task_by_id(&state, &id).await,
        }))
        .into_response()),
        Err(message) => Err(error_response(&message, StatusCode::BAD_REQUEST)),
    }
}
