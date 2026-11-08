//! 数据管理 API — 对照 Agent2API「设置 → 数据」（DataPane + api/storage_api.rs + api/backup_api.rs）。
//!
//! AIGX 的业务数据全部在 FileStore KV（SQLite/PG 后端），因此备份 = KV 全量
//! 键值导出为一份 JSON 文档；恢复 = 导入该文档（写回 KV）。
//!
//! - `GET /api/storage`：存储概览（数据目录、KV 总键数、按前缀分组的键数、
//!   SQLite 库文件大小）——只读，供面板展示「都存了些什么」
//! - `GET /api/backup/export`：全量导出（`include_logs=false` 时跳过
//!   `reqlog:` / `auditlog:` 大体积日志键，对照 A2A 的 includeLogs）
//! - `POST /api/backup/import`：导入恢复（先自动导出一份全量备份到
//!   `{data_dir}/backups/` 再写回，对照 A2A「恢复前自动备份」）
//!
//! 载体用 `serde_json::Value`：FileStore 的 get/put 本来就是 JSON 进出，
//! 无需给两个后端加新原语。

use std::collections::BTreeMap;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, record_audit, verify_admin};

/// 备份文档格式版本（未来结构变更时的兼容判据）。
const BACKUP_FORMAT_VERSION: u32 = 1;

/// 日志类键前缀（include_logs=false 时排除；加密导出复用同一口径）。
pub(crate) const LOG_PREFIXES: &[&str] = &["reqlog:", "auditlog:"];

/// 面板展示的键前缀分组（存储概览用）。
const STORAGE_GROUPS: &[(&str, &str)] = &[
    ("channel:", "渠道"),
    ("apikey_", "API Key"),
    ("user:", "用户"),
    ("reqlog:", "请求日志"),
    ("auditlog:", "审计日志"),
    ("price:", "模型定价"),
    ("ratio_config", "倍率配置"),
    ("model_meta:", "模型元信息"),
    ("model_rules", "模型管理规则"),
    ("model_custom_map", "全局模型映射"),
    ("acctses:", "账号池"),
    ("proxy_pool_items", "网络代理池"),
    ("checkin:", "签到"),
    ("ratelimit_config", "限流配置"),
    ("ip_filter", "IP 黑白名单"),
    ("notify_config", "通知配置"),
    ("alert_", "告警"),
    ("routing_policy", "路由策略"),
    ("gateway_prompt", "系统提示词"),
];

/// 备份文档。
#[derive(Debug, Serialize, Deserialize)]
pub struct BackupDocument {
    /// 格式版本
    pub version: u32,
    /// 导出时间（unix 秒）
    pub exported_at: i64,
    /// 是否含日志键
    pub include_logs: bool,
    /// KV 全量键值（键序稳定：BTreeMap）
    pub kv: BTreeMap<String, Value>,
}

/// GET /api/storage —— 存储概览。
pub async fn handle_storage_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let all_keys = state.alert_store.list("").map_err(|e| {
        error_response(
            &format!("读取存储失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    for (prefix, label) in STORAGE_GROUPS {
        let count = all_keys.iter().filter(|k| k.starts_with(prefix)).count();
        if count > 0 {
            groups.insert((*label).to_string(), json!(count));
        }
    }

    // 数据目录（面板展示「数据存在哪」；与备份/恢复路径同源）+ SQLite 库大小
    // （data_dir/data/aigx.db；PG 后端时 db_size 为 None）
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let data_dir_display = data_dir.display().to_string();
    let db_size = std::fs::metadata(data_dir.join("data").join("aigx.db"))
        .ok()
        .map(|m| m.len());

    let admin_id = crate::api::admin::common::admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "storage.overview",
        "查询存储概览",
        None,
        None,
    );

    Ok(Json(json!({
        "success": true,
        "data": {
            "total_keys": all_keys.len(),
            "groups": groups,
            "db_size_bytes": db_size,
            "data_dir": data_dir_display,
        }
    })))
}

// 明文导出端点已移除（#33 安全缺口本体）：全量 KV 含凭证/密钥/用户表，
// admin 磁盘出现一份绕过 enc1: 的整库明文副本。改用加密导出三件套
//（backup_crypto.rs）；明文**导入**保留（存量旧包迁移必需）。
// BackupExportQuery 随之删除（Query 参数只服务于明文导出）。

/// POST /api/backup/import —— 导入恢复。
///
/// 安全顺序（对照 A2A db/backup.rs）：
/// 1. 先把**当前**全量 KV（含日志）自动备份到 `{data_dir}/backups/`——
///    导入坏了还能回滚；
/// 2. 校验备份文档版本与结构；
/// 3. 逐键写回（`serde_json::Value` 直通，不感知具体类型）。
///
/// 注意：导入是合并写（upsert）而非清库重放——被备份里没有的键保留现值。
/// 这与 A2A 的整库替换语义不同，是有意为之：AIGX 的 KV 里混着运行时状态
/// （断路器游标类），清库重放会把不认识的键全删掉。
pub async fn handle_backup_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(doc): Json<BackupDocument>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    if doc.version != BACKUP_FORMAT_VERSION {
        return Err(error_response(
            &format!(
                "备份版本不匹配：文档 v{}，当前 v{BACKUP_FORMAT_VERSION}",
                doc.version
            ),
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }
    if doc.kv.is_empty() {
        return Err(error_response(
            "备份文档为空（kv 无条目）",
            axum::http::StatusCode::BAD_REQUEST,
        ));
    }

    // 1. 恢复前自动备份当前全量（含日志，防丢）——与加密导入共用同一辅助
    let pre_path = snapshot_current(&state).await?;

    // 2. 逐键写回（合并 upsert）
    let mut written = 0usize;
    let mut skipped = 0usize;
    for (key, value) in &doc.kv {
        if key.is_empty() || key.len() > 512 {
            skipped += 1;
            continue;
        }
        match state.alert_store.put(key, value) {
            Ok(()) => written += 1,
            Err(e) => {
                skipped += 1;
                tracing::warn!("导入键 {key} 失败: {e}");
            }
        }
    }

    let admin_id = crate::api::admin::common::admin_id_from_session(&state, &headers).await;
    record_audit(
        &state,
        &admin_id,
        "backup.import",
        &format!(
            "导入备份：写入 {written} 键，跳过 {skipped}，恢复前快照 {}",
            pre_path.display()
        ),
        None,
        None,
    );
    tracing::info!(
        "备份导入完成: 写入 {written} 键 / 跳过 {skipped}（恢复前备份: {}）",
        pre_path.display()
    );

    Ok(Json(json!({
        "success": true,
        "data": {
            "written": written,
            "skipped": skipped,
            "pre_import_backup": pre_path.display().to_string(),
        }
    })))
}

/// 恢复前自动备份当前全量 KV（含日志）到 `{data_dir}/backups/pre-import-<ts>.json`。
///
/// 明文导入与加密导入（backup_crypto）共用同一安全顺序；同步文件 IO，
/// 冷路径（低频管理操作）。
pub(crate) async fn snapshot_current(
    state: &AppState,
) -> Result<std::path::PathBuf, (axum::http::StatusCode, Json<Value>)> {
    let snapshot_keys = state.alert_store.list("").map_err(|e| {
        error_response(
            &format!("列出当前键失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let mut snapshot = BTreeMap::new();
    for key in &snapshot_keys {
        if let Ok(Some(v)) = state.alert_store.get::<Value>(key) {
            snapshot.insert(key.clone(), v);
        }
    }
    let pre_doc = BackupDocument {
        version: BACKUP_FORMAT_VERSION,
        exported_at: chrono::Utc::now().timestamp(),
        include_logs: true,
        kv: snapshot,
    };
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let backups_dir = data_dir.join("backups");
    std::fs::create_dir_all(&backups_dir).map_err(|e| {
        error_response(
            &format!("创建备份目录失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    let pre_path = backups_dir.join(format!(
        "pre-import-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    ));
    let pre_content = serde_json::to_string(&pre_doc).map_err(|e| {
        error_response(
            &format!("序列化恢复前备份失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    std::fs::write(&pre_path, pre_content).map_err(|e| {
        error_response(
            &format!("写入恢复前备份失败: {e}"),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
        )
    })?;
    Ok(pre_path)
}
