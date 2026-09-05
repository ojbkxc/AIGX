//! 渠道管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供渠道列表、添加、更新、删除、健康检查等功能。
//!
//! ## 路径说明
//!
//! - 使用 `crate::` 访问主 crate 的类型和资源

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{admin_id_from_session, error_response, record_audit, verify_admin};

// 这里需要引用主 crate 的 Channel 和相关类型
use crate::channel::{Channel, ChannelType};

/// 渠道创建请求
#[derive(Debug, Deserialize)]
pub struct ChannelRequest {
    pub name: String,
    #[serde(default)]
    pub channel_type: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub priority: i64,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default = "default_enabled_status")]
    pub status: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub account_id: String,
}

fn default_weight() -> u32 {
    1
}

fn default_enabled_status() -> String {
    "enabled".to_string()
}

impl ChannelRequest {
    fn to_channel(&self, id: String) -> Channel {
        let now = chrono::Utc::now().timestamp();
        Channel {
            id,
            name: self.name.clone(),
            channel_type: ChannelType::from_str_lossy(&self.channel_type),
            base_url: self.base_url.clone(),
            api_key: self.api_key.clone(),
            priority: self.priority,
            weight: self.weight,
            status: self.status.clone(),
            models: self.models.clone(),
            account_id: self.account_id.clone(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// 构造渠道 JSON 响应（脱敏 API Key）
pub fn mask_channel(ch: &Channel) -> Value {
    let masked_key = if ch.api_key.is_empty() {
        String::new()
    } else if ch.api_key.chars().count() > 12 {
        format!(
            "{}...{}",
            &ch.api_key[..8],
            &ch.api_key[ch.api_key.len() - 4..]
        )
    } else {
        "****".to_string()
    };
    json!({
        "id": ch.id,
        "name": ch.name,
        "channel_type": ch.channel_type.as_str(),
        "base_url": ch.base_url,
        "api_key": masked_key,
        "priority": ch.priority,
        "weight": ch.weight,
        "status": ch.status,
        "models": ch.models,
        "account_id": ch.account_id,
        "last_error": ch.last_error,
        "last_used_at": ch.last_used_at,
        "created_at": ch.created_at,
        "updated_at": ch.updated_at,
    })
}

/// 列出所有渠道
pub async fn handle_list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let channels: Vec<Value> = state
        .channel_store
        .list()
        .iter()
        .map(mask_channel)
        .collect();
    Ok(Json(json!({ "success": true, "data": channels })))
}

/// 添加新渠道
pub async fn handle_add_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = body.to_channel(String::new());
    match state.channel_store.add(ch) {
        Ok(c) => Ok(Json(json!({ "success": true, "data": mask_channel(&c) }))),
        Err(e) => Err(error_response(
            &format!("Failed to add channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 更新渠道信息
pub async fn handle_update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let ch = body.to_channel(id);
    match state.channel_store.add(ch) {
        Ok(c) => Ok(Json(json!({ "success": true, "data": mask_channel(&c) }))),
        Err(e) => Err(error_response(
            &format!("Failed to update channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 删除渠道
pub async fn handle_delete_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    match state.channel_store.remove(&id) {
        Ok(_) => Ok(Json(json!({ "success": true, "data": null }))),
        Err(e) => Err(error_response(
            &format!("Failed to delete channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

// TODO: 继续迁移剩余 10 个 channel handler
// publish_channel, unpublish_channel, channel_health, channel_metrics 等
