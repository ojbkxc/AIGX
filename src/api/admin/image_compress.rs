//! 会话图片压缩设置（转发前压缩内嵌大图）。
//!
//! `GET/PUT /api/image-compress`，body 与响应都是：
//! `{ imageCompressEnabled, imageCompressMaxEdge, imageCompressQuality, imageCompressThresholdKb }`。
//!
//! - PUT 允许**部分字段**：缺省项保持原值；
//! - 全部校验通过才落盘（先校验、后批量写，避免半份脏配置）；
//! - 约束：enabled 默认 true；maxEdge 默认 2048（256~8192）；
//!   quality 默认 80（40~95）；thresholdKb 默认 512（0~10240，0 = 全部尝试压缩）。
//!
//! 只作用于对话链路（生图 / 图片编辑在转发层另有分支）。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::get,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_user};

const KEY_ENABLED: &str = "imageCompressEnabled";
const KEY_MAX_EDGE: &str = "imageCompressMaxEdge";
const KEY_QUALITY: &str = "imageCompressQuality";
const KEY_THRESHOLD_KB: &str = "imageCompressThresholdKb";

const DEFAULT_ENABLED: bool = true;
const DEFAULT_MAX_EDGE: i64 = 2048;
const DEFAULT_QUALITY: i64 = 80;
const DEFAULT_THRESHOLD_KB: i64 = 512;

const MAX_EDGE_MIN: i64 = 256;
const MAX_EDGE_MAX: i64 = 8192;
const QUALITY_MIN: i64 = 40;
const QUALITY_MAX: i64 = 95;
const THRESHOLD_KB_MIN: i64 = 0;
const THRESHOLD_KB_MAX: i64 = 10_240;

/// PUT 请求体：全部可选（部分更新）。收 camelCase 主名，兼容短别名。
#[derive(Debug, Deserialize)]
pub struct ImageCompressRequest {
    #[serde(rename = "imageCompressEnabled", alias = "enabled")]
    pub enabled: Option<bool>,
    #[serde(rename = "imageCompressMaxEdge", alias = "maxEdge")]
    pub max_edge: Option<i64>,
    #[serde(rename = "imageCompressQuality", alias = "quality")]
    pub quality: Option<i64>,
    #[serde(rename = "imageCompressThresholdKb", alias = "thresholdKb")]
    pub threshold_kb: Option<i64>,
}

fn store(state: &AppState) -> &crate::storage::FileStore {
    &state.scheduled_task_store
}

fn read_i64(state: &AppState, key: &str, default: i64) -> i64 {
    store(state)
        .get::<i64>(key)
        .ok()
        .flatten()
        .unwrap_or(default)
}

/// 当前生效设置（库里的值已由写入侧校验过范围）。
fn settings_json(state: &AppState) -> Value {
    json!({
        "imageCompressEnabled": store(state)
            .get::<bool>(KEY_ENABLED)
            .ok()
            .flatten()
            .unwrap_or(DEFAULT_ENABLED),
        "imageCompressMaxEdge": read_i64(state, KEY_MAX_EDGE, DEFAULT_MAX_EDGE),
        "imageCompressQuality": read_i64(state, KEY_QUALITY, DEFAULT_QUALITY),
        "imageCompressThresholdKb": read_i64(state, KEY_THRESHOLD_KB, DEFAULT_THRESHOLD_KB),
    })
}

/// GET /api/image-compress
pub async fn handle_get_image_compress(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;
    let mut body = settings_json(&state);
    body["success"] = Value::Bool(true);
    Ok(Json(body))
}

/// PUT /api/image-compress — 部分字段更新；全部校验通过才落盘。
pub async fn handle_put_image_compress(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImageCompressRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_user(&state, &headers).await?;

    // 先校验全部字段，任何一个越界都整体拒绝（不写半份）。
    if let Some(v) = body.max_edge {
        if !(MAX_EDGE_MIN..=MAX_EDGE_MAX).contains(&v) {
            return Err(error_response(
                &format!("图片长边上限必须在 {MAX_EDGE_MIN}~{MAX_EDGE_MAX} 像素之间"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    if let Some(v) = body.quality {
        if !(QUALITY_MIN..=QUALITY_MAX).contains(&v) {
            return Err(error_response(
                &format!("压缩质量必须在 {QUALITY_MIN}~{QUALITY_MAX} 之间"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    if let Some(v) = body.threshold_kb {
        if !(THRESHOLD_KB_MIN..=THRESHOLD_KB_MAX).contains(&v) {
            return Err(error_response(
                &format!("触发阈值必须在 {THRESHOLD_KB_MIN}~{THRESHOLD_KB_MAX} KB 之间"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }

    // 校验通过后批量写（sqlite 后端单事务；部分后端退化为逐条 put）。
    let mut entries: Vec<(String, Value)> = Vec::new();
    if let Some(v) = body.enabled {
        entries.push((KEY_ENABLED.to_string(), Value::Bool(v)));
    }
    if let Some(v) = body.max_edge {
        entries.push((KEY_MAX_EDGE.to_string(), json!(v)));
    }
    if let Some(v) = body.quality {
        entries.push((KEY_QUALITY.to_string(), json!(v)));
    }
    if let Some(v) = body.threshold_kb {
        entries.push((KEY_THRESHOLD_KB.to_string(), json!(v)));
    }
    if !entries.is_empty() {
        let refs: Vec<(&str, Value)> = entries
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        store(&state).write_batch(&refs).map_err(|error| {
            error_response(
                &format!("保存图片压缩设置失败：{error}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        })?;
    }

    let mut resp = settings_json(&state);
    resp["success"] = Value::Bool(true);
    Ok(Json(resp))
}

/// 管理面路由（由主控统一注册）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route(
        "/api/image-compress",
        get(handle_get_image_compress).put(handle_put_image_compress),
    )
}
