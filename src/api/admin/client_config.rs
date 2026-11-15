//! 客户端接入多配置管理 API（对齐 Agent2API `api/agents.rs` 的非插件部分）。
//!
//! 端点（全部挂管理员鉴权）：
//!
//! | 方法 | 路径 | 说明 |
//! |---|---|---|
//! | GET | `/api/admin/client-configs?target=` | 目标定义 + 各目标配置列表 |
//! | POST | `/api/admin/client-configs` | 新增命名配置 |
//! | PUT | `/api/admin/client-configs/:id` | 改名 / 切换启用 / 改内容 |
//! | DELETE | `/api/admin/client-configs/:id` | 删除（不动磁盘文件） |
//! | POST | `/api/admin/client-configs/:id/preview` | 生成写入预览 + 确认令牌 |
//! | POST | `/api/admin/client-configs/:id/apply` | 凭令牌写入（写前自动备份） |
//! | GET | `/api/admin/client-configs/:id/backups` | 列出该目标的文件备份 |
//! | POST | `/api/admin/client-configs/:id/restore` | 用指定备份覆盖文件 |
//!
//! ## 安全口径
//!
//! - API Key 只在**请求体进入**与**写入目标文件**两个瞬间以明文存在；
//!   落库是 `enc1:` 信封，接口响应只回 `hasApiKey`，预览内容按行脱敏。
//! - 目标文件路径由 `client_config::targets` 固定，本模块不接受路径参数。
//! - 所有写操作记审计（不含凭据）。

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};
use crate::api::openai::AppState;
use crate::client_config::{
    self,
    plan::{self, FilePlan},
    store::{self, Record, Update},
    targets::{self, FileKind, Target},
    KEEP_BACKUPS,
};

type ApiError = (StatusCode, Json<Value>);

fn bad_request(message: &str) -> ApiError {
    error_response(message, StatusCode::BAD_REQUEST)
}

fn not_found() -> ApiError {
    error_response("配置不存在", StatusCode::NOT_FOUND)
}

fn internal(message: &str) -> ApiError {
    error_response(message, StatusCode::INTERNAL_SERVER_ERROR)
}

fn find_record(store: &crate::storage::FileStore, id: &str) -> Result<(Target, Record), ApiError> {
    match store::find(store, id) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err(not_found()),
        Err(error) => Err(bad_request(&error)),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    target: Option<String>,
}

/// `GET /api/admin/client-configs?target=`
pub async fn handle_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let target = match query.target.as_deref().filter(|id| !id.trim().is_empty()) {
        Some(id) => Some(Target::parse(id).ok_or_else(|| bad_request("未知的配置目标"))?),
        None => None,
    };
    let data = store::view(&state.alert_store, target).map_err(|error| bad_request(&error))?;
    Ok(Json(json!({ "success": true, "data": data })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateConfigRequest {
    pub target: String,
    pub name: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub models: Vec<Value>,
    #[serde(default)]
    pub options: Value,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// `POST /api/admin/client-configs`
pub async fn handle_create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateConfigRequest>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let target = Target::parse(&body.target).ok_or_else(|| bad_request("未知的配置目标"))?;
    targets::validate_for_target(
        target,
        &body.name,
        &body.base_url,
        &body.api_key,
        &body.model,
        &body.models,
        &body.options,
    )
    .map_err(|error| bad_request(&error))?;
    let enabled = match body.enabled {
        Some(enabled) => enabled,
        None => store::with_target(&state.alert_store, target, |records| {
            Ok(!records.iter().any(|record| record.enabled))
        })
        .map_err(|error| bad_request(&error))?,
    };
    let sealed = store::seal_key(body.api_key.trim());
    let record = store::create(
        &state.alert_store,
        target,
        &body.name,
        enabled,
        &body.base_url,
        &sealed,
        &body.model,
        body.models,
        body.options,
        &body.note,
    )
    .map_err(|error| bad_request(&error))?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "create_client_config",
        &format!("target={} id={}", record.target, record.id),
        None,
        Some(json!({
            "name": record.name,
            "baseUrl": record.base_url,
            "enabled": record.enabled,
            "hasApiKey": !record.api_key.is_empty(),
        })),
    );
    Ok(Json(json!({ "success": true, "data": record.view() })))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateConfigRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub base_url: Option<String>,
    /// 新 API Key；空串视为「不修改」。
    #[serde(default)]
    pub api_key: Option<String>,
    /// 显式清空已存凭据（优先于 apiKey）。
    #[serde(default)]
    pub clear_api_key: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub models: Option<Vec<Value>>,
    #[serde(default)]
    pub options: Option<Value>,
    #[serde(default)]
    pub note: Option<String>,
}

/// `PUT /api/admin/client-configs/:id`
///
/// 改名、切换启用（同一目标同一时刻只启用一份）、停用、改内容都走这里。
pub async fn handle_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<UpdateConfigRequest>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, existing) = find_record(&state.alert_store, &id)?;
    let name = body.name.as_deref().unwrap_or(&existing.name);
    let base_url = body.base_url.as_deref().unwrap_or(&existing.base_url);
    let model = body.model.as_deref().unwrap_or(&existing.model);
    let models = body.models.clone().unwrap_or_else(|| existing.models.clone());
    let options = body.options.clone().unwrap_or_else(|| existing.options.clone());
    let api_key_probe = body.api_key.as_deref().map(str::trim).unwrap_or("");
    targets::validate_for_target(target, name, base_url, api_key_probe, model, &models, &options)
        .map_err(|error| bad_request(&error))?;
    let sealed = if api_key_probe.is_empty() {
        None
    } else {
        Some(store::seal_key(api_key_probe))
    };
    let record = store::update(
        &state.alert_store,
        target,
        &id,
        Update {
            name: body.name.as_deref(),
            enabled: body.enabled,
            base_url: body.base_url.as_deref(),
            api_key_sealed: sealed.as_deref(),
            clear_api_key: body.clear_api_key,
            model: body.model.as_deref(),
            models: body.models,
            options: body.options,
            note: body.note.as_deref(),
        },
    )
    .map_err(|error| bad_request(&error))?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "update_client_config",
        &format!("target={} id={}", record.target, record.id),
        Some(json!({
            "name": existing.name,
            "enabled": existing.enabled,
            "baseUrl": existing.base_url,
        })),
        Some(json!({
            "name": record.name,
            "enabled": record.enabled,
            "baseUrl": record.base_url,
            "hasApiKey": !record.api_key.is_empty(),
        })),
    );
    Ok(Json(json!({ "success": true, "data": record.view() })))
}

/// `DELETE /api/admin/client-configs/:id` —— 只删记录，不碰已写入的客户端文件。
pub async fn handle_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, existing) = find_record(&state.alert_store, &id)?;
    let removed = store::delete(&state.alert_store, target, &id)
        .map_err(|error| bad_request(&error))?;
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "delete_client_config",
        &format!("target={} id={}", removed.target, removed.id),
        Some(json!({ "name": existing.name, "baseUrl": existing.base_url, "enabled": existing.enabled })),
        None,
    );
    Ok(Json(json!({ "success": true, "data": null })))
}

/// `POST /api/admin/client-configs/:id/preview`
///
/// 返回每个目标文件的 `before` / `after`（凭证已脱敏）、行级 diff 与
/// 确认令牌；真正的写入内容由服务端按记录重新生成，前端不能伪造。
pub async fn handle_preview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, record) = find_record(&state.alert_store, &id)?;
    let plan = plan::plan_for_record(target, &record).map_err(|error| bad_request(&error))?;
    let (mut view, _token) =
        plan::issue_token(&state.alert_store, &plan).map_err(|error| bad_request(&error))?;
    if let Some(object) = view.as_object_mut() {
        object.insert("record".into(), record.view());
    }
    Ok(Json(json!({ "success": true, "data": view })))
}

#[derive(Debug, Deserialize)]
pub struct ApplyConfigRequest {
    #[serde(default)]
    pub token: String,
}

/// `POST /api/admin/client-configs/:id/apply`
pub async fn handle_apply(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ApplyConfigRequest>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, record) = find_record(&state.alert_store, &id)?;
    let plan = plan::plan_for_record(target, &record).map_err(|error| bad_request(&error))?;
    plan::verify_token(&state.alert_store, &plan, &body.token)
        .map_err(|error| bad_request(&error))?;
    let changed_files: Vec<&FilePlan> = plan.files.iter().filter(|file| file.changed()).collect();
    if changed_files.is_empty() {
        store::clear_token(&state.alert_store, target).map_err(|error| bad_request(&error))?;
        return Ok(Json(json!({
            "success": true,
            "data": { "changed": false, "files": [], "appliedAt": Value::Null },
        })));
    }
    // 校验令牌 → 备份 → 写入 → 回读 的整段串行：并发应用同读旧内容再先后
    // 写会互相覆盖并让备份翻倍。锁与存储读改写锁分离，避免不可重入死锁。
    let backups = {
        let _guard = client_config::apply_lock().map_err(|error| bad_request(&error))?;
        plan::apply_plan(&plan).map_err(|error| bad_request(&error))?
    };
    let applied_at = client_config::now_ms();
    store::clear_token(&state.alert_store, target).map_err(|error| bad_request(&error))?;
    if let Err(error) = store::mark_applied(&state.alert_store, target, &id, applied_at) {
        tracing::warn!("[ClientConfig] 记录应用时间失败 id={id}: {error}");
    }
    let files: Vec<Value> = changed_files
        .iter()
        .zip(backups.iter())
        .map(|(file, backup)| {
            json!({
                "kind": file.kind.as_str(),
                "path": file.path.display().to_string(),
                "backupId": backup.id,
            })
        })
        .collect();
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "apply_client_config",
        &format!("target={} id={}", record.target, record.id),
        None,
        Some(json!({ "name": record.name, "files": files })),
    );
    Ok(Json(json!({
        "success": true,
        "data": { "changed": true, "files": files, "appliedAt": applied_at },
    })))
}

/// `GET /api/admin/client-configs/:id/backups`
pub async fn handle_backups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, record) = find_record(&state.alert_store, &id)?;
    let mut items = Vec::new();
    for file in target.files() {
        let entries = plan::list_backups(&file.path).map_err(|error| bad_request(&error))?;
        for entry in entries {
            items.push(json!({
                "id": entry.id,
                "kind": file.kind.as_str(),
                "path": file.path.display().to_string(),
                "fileName": entry.path.file_name().and_then(|name| name.to_str()).unwrap_or_default(),
                "size": entry.size,
                "createdAt": entry.created_at,
            }));
        }
    }
    items.sort_by(|a, b| {
        b.get("createdAt")
            .and_then(Value::as_i64)
            .cmp(&a.get("createdAt").and_then(Value::as_i64))
    });
    Ok(Json(json!({
        "success": true,
        "data": {
            "record": record.view(),
            "keep": KEEP_BACKUPS,
            "items": items,
        },
    })))
}

#[derive(Debug, Deserialize)]
pub struct RestoreConfigRequest {
    /// 备份 ID（`GET /backups` 返回的 `id`，兼容数字/字符串）。
    pub backup: Value,
    /// 可选文件类别（`config` / `credentials`）——DSH 两个文件可能落在同一
    /// 毫秒（同一份备份 ID），带上 kind 才无歧义；缺省取第一个命中。
    #[serde(default)]
    pub kind: Option<String>,
}

/// `POST /api/admin/client-configs/:id/restore`
pub async fn handle_restore(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<RestoreConfigRequest>,
) -> Result<Json<Value>, ApiError> {
    verify_admin(&state, &headers).await?;
    let (target, record) = find_record(&state.alert_store, &id)?;
    let backup_id = match &body.backup {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        _ => return Err(bad_request("备份 ID 必须是字符串或数字")),
    };
    let wanted_kind = match body.kind.as_deref().map(str::trim) {
        None | Some("") => None,
        Some("config") => Some(FileKind::Config),
        Some("credentials") => Some(FileKind::Credentials),
        Some(_) => return Err(bad_request("kind 只支持 config / credentials")),
    };
    let mut restored: Option<(FileKind, String, Vec<Value>)> = None;
    for file in target.files() {
        if wanted_kind.is_some_and(|kind| kind != file.kind) {
            continue;
        }
        let entries = plan::list_backups(&file.path).map_err(|error| bad_request(&error))?;
        if !entries.iter().any(|entry| entry.id == backup_id) {
            continue;
        }
        let _guard = client_config::apply_lock().map_err(|error| bad_request(&error))?;
        let text = plan::read_backup(&file.path, &backup_id)
            .map_err(|error| bad_request(&error))?;
        let restore_plan = plan::plan_for_restore(target, &file, text, &backup_id)
            .map_err(|error| bad_request(&error))?;
        let backups = plan::apply_plan(&restore_plan).map_err(|error| bad_request(&error))?;
        let safety: Vec<Value> = backups
            .iter()
            .map(|backup| json!({ "id": backup.id, "kind": file.kind.as_str() }))
            .collect();
        restored = Some((file.kind, backup_id.clone(), safety));
        break;
    }
    let Some((kind, backup_id, safety)) = restored else {
        return Err(bad_request("备份不存在"));
    };
    let admin_id = admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "restore_client_config",
        &format!("target={} id={}", record.target, record.id),
        None,
        Some(json!({
            "backupId": backup_id,
            "file": kind.as_str(),
            "name": record.name,
        })),
    );
    Ok(Json(json!({
        "success": true,
        "data": {
            "restored": true,
            "backupId": backup_id,
            "kind": kind.as_str(),
            "safetyBackups": safety,
        },
    })))
}
