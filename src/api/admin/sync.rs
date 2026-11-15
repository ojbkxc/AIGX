//! 云同步管理 API（设置页「数据 → 云同步」）。
//!
//! ```text
//! GET  /api/admin/sync/config   设置 + 远端路径（密码 / Secret 只回 has_xxx）
//! PUT  /api/admin/sync/config   保存设置（空密码 = 沿用旧值）
//! GET  /api/admin/sync/status   运行状态（上次成功 / 错误 / 哈希 / 连续失败）
//! POST /api/admin/sync/test     测试连通性（用请求体里未保存的新值）
//! POST /api/admin/sync/now      立即同步 { force?: bool }
//! POST /api/admin/sync/pull     拉取恢复 { mode?: "merge"|"local"|"remote" }
//! ```
//!
//! ── 为什么全挂 `verify_admin` ─────────────────────────────────
//! 上传会把全部业务数据（含 `enc1:` 凭据密文）送到远端；拉取会整体改写本机
//! 数据；保存会写入远端凭据。三者都是 `/api/config` 级别的敏感动作。
//!
//! ── 错误信封 ─────────────────────────────────────────────────
//! 与其它管理接口一致：`{success:false,error:"…"}`。配置类错误 400（要改设置），
//! 网络 / 远端错误 502（重试或换线路），本地落库失败 500。
//!
//! ── 保存后的排期调整 ─────────────────────────────────────────
//! 刚启用 → `schedule_now`（下一拍就跑一次，符合「打开开关就该同步」的预期）；
//! 改了间隔 → 按新间隔从上次尝试起重新计时。失败退避仍然优先（task_state 的
//! `due_at` 取 retry 与 next_run 的较大值），不会被「刚启用」绕过。

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};
use crate::api::openai::AppState;
use crate::sync::engine::{self, SyncError};
use crate::sync::snapshot::MergeMode;
use crate::sync::{config as sync_config, scheduler};

/// 解析可选请求体（空 body = 默认值）。
fn parse_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, (StatusCode, Json<Value>)> {
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|error| {
        error_response(&format!("请求体解析失败: {error}"), StatusCode::BAD_REQUEST)
    })
}

fn map_sync_error(error: SyncError) -> (StatusCode, Json<Value>) {
    let status = if error.config {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::BAD_GATEWAY
    };
    error_response(&error.message, status)
}

/// `GET /api/admin/sync/config`
pub async fn handle_get_sync_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let config = sync_config::load(&state.scheduled_task_store);
    Ok(Json(json!({
        "success": true,
        "data": config.public_json(),
    })))
}

/// `GET /api/admin/sync/status`
pub async fn handle_get_sync_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": engine::status_json(&state),
    })))
}

/// `PUT /api/admin/sync/config`
pub async fn handle_put_sync_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let input: sync_config::SaveInput = parse_body(&body)?;
    let store = &state.scheduled_task_store;
    let before = sync_config::load(store);
    let config = sync_config::apply_input(before.clone(), input)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    sync_config::save(store, &config)
        .map_err(|message| error_response(&message, StatusCode::INTERNAL_SERVER_ERROR))?;

    // 排期：刚启用 → 下一拍就跑；间隔变化 → 按新间隔重新计时
    if config.enabled && !before.enabled {
        if let Err(error) = crate::scheduled_tasks::task_state::schedule_now(scheduler::TASK_KEY) {
            tracing::warn!("[CloudSync] 启用后立即排期失败：{error}");
        }
    } else if config.enabled && before.interval_secs != config.interval_secs {
        let interval_ms = config.interval_secs.saturating_mul(1000);
        if let Err(error) =
            crate::scheduled_tasks::task_state::reschedule(scheduler::TASK_KEY, interval_ms)
        {
            tracing::warn!("[CloudSync] 间隔变更后重排期失败：{error}");
        }
    }

    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "sync.config_save",
        &format!(
            "保存云同步设置：后端 {}，远端 {}，{}",
            config.backend,
            config.remote_display(),
            if config.enabled {
                "已启用"
            } else {
                "未启用"
            }
        ),
        None,
        None,
    );

    Ok(Json(json!({
        "success": true,
        "data": config.public_json(),
    })))
}

/// `POST /api/admin/sync/test`
///
/// 请求体与 PUT 同形（可只传需要覆盖的字段）；空 body = 用已保存的设置探测。
pub async fn handle_sync_test(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let input: sync_config::SaveInput = parse_body(&body)?;
    let data = engine::test_connection(&state, input)
        .await
        .map_err(map_sync_error)?;
    Ok(Json(json!({ "success": true, "data": data })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct NowBody {
    /// 越过「内容未变化 / 最小间隔」两道闸门。
    force: bool,
}

/// `POST /api/admin/sync/now`
pub async fn handle_sync_now(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let input: NowBody = parse_body(&body)?;
    let data = engine::upload(&state, true, input.force)
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
        &format!("手动云同步：{status}"),
        None,
        None,
    );
    Ok(Json(json!({ "success": true, "data": data })))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PullBody {
    /// `merge`（默认）/ `local`（本机强制覆盖）/ `remote`（远端强制覆盖）。
    mode: String,
}

/// `POST /api/admin/sync/pull`
pub async fn handle_sync_pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let input: PullBody = parse_body(&body)?;
    let mode = MergeMode::parse(&input.mode)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    let data = engine::pull(&state, mode).await.map_err(map_sync_error)?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "sync.pull",
        &format!(
            "从云端拉取同步数据：模式 {}，写入 {} 键",
            mode.as_str(),
            data.get("written").and_then(Value::as_u64).unwrap_or(0)
        ),
        None,
        None,
    );
    Ok(Json(json!({ "success": true, "data": data })))
}
