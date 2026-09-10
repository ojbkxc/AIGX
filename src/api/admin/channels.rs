//! 渠道管理 API（P0-W1 自 `admin.rs` 迁移）
//!
//! 提供渠道列表、添加、更新、删除、健康检查等功能。
//!
//! ## 路径说明
//!
//! - 使用 `crate::` 访问主 crate 的类型和资源

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin, verify_user};

// 这里需要引用主 crate 的 Channel 和相关类型
use crate::channel::{Channel, ChannelType};

/// GET /api/channels/:id/health-archive — 渠道健康档案（C1）。
///
/// 返回该渠道最近 N 天（默认 30，上限 30）的健康趋势：
/// 每日成功率 / P95 延迟 / 熔断次数 / 最后错误。用于渠道行展开的
/// 「健康档案」面板（ROADMAP P1-14）。
pub async fn handle_channel_health_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;

    let days = 30u32;
    let snapshots = state.channel_store.health_archive().query(&id, days);
    let data: Vec<Value> = snapshots
        .iter()
        .map(|s| {
            json!({
                "day": s.day,
                "success": s.success,
                "failure": s.failure,
                "trips": s.trips,
                "success_rate": (s.success_rate() * 100.0).round() / 100.0,
                "p95_ms": s.p95_ms(),
                "last_error": s.last_error,
            })
        })
        .collect();

    Ok(Json(json!({
        "success": true,
        "data": {
            "channel_id": id,
            "days": days,
            "snapshots": data,
        }
    })))
}

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
    #[serde(default)]
    pub model_mapping: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub cost_pricing: std::collections::HashMap<String, crate::pricing::ModelPrice>,
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
            model_mapping: self.model_mapping.clone(),
            cost_pricing: self.cost_pricing.clone(),
            last_error: None,
            last_used_at: None,
            discovered_models: Vec::new(),
            response_time: None,
            test_time: None,
            balance: None,
            credit: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 构造渠道 JSON 响应（脱敏 API Key）
/// `seq`：展示用短编号（列表接口按创建顺序注入 1..N；单渠道场景传 None 留空）。
pub fn mask_channel(ch: &Channel, seq: Option<u64>) -> Value {
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
        "seq": seq.unwrap_or(0),
        "name": ch.name,
        "channel_type": ch.channel_type.as_str(),
        "base_url": ch.base_url,
        "api_key": masked_key,
        "priority": ch.priority,
        "weight": ch.weight,
        "status": ch.status,
        "models": ch.models,
        "model_mapping": ch.model_mapping,
        "cost_pricing": ch.cost_pricing,
        "account_id": ch.account_id,
        "last_error": ch.last_error,
        "last_used_at": ch.last_used_at,
        "response_time": ch.response_time,
        "test_time": ch.test_time,
        "balance": ch.balance,
        "credit": ch.credit,
        "created_at": ch.created_at,
        "updated_at": ch.updated_at,
    })
}

/// 列出所有渠道（支持分页）。
///
/// 查询参数（均可选，向后兼容）：
/// - `page`：从 1 开始，缺省 1
/// - `page_size`：缺省 0 = 不分页，返回全量（旧前端/脚本兼容）
/// - `search`：按名称/类型/base_url 模糊过滤
///
/// 分页时返回 `{ data, page, page_size, total }`；不分页时 data 保持原数组形状。
pub async fn handle_list_channels(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    // 注入展示用短编号：按创建时间升序 1..N（同列表排序键，新增渠道追加新号，
    // 既有渠道编号稳定不变；删除渠道不回收编号——避免错位显示）
    let all = state.channel_store.list();
    let mut sorted: Vec<&Channel> = all.iter().collect();
    sorted.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let id_to_seq: std::collections::HashMap<&str, u64> = sorted
        .iter()
        .enumerate()
        .map(|(i, ch)| (ch.id.as_str(), (i + 1) as u64))
        .collect();

    // 服务端搜索过滤（名称/类型/base_url/模型名，前端一致）
    let search = params.get("search").map(|s| s.to_lowercase()).unwrap_or_default();
    let filtered: Vec<&Channel> = if search.is_empty() {
        all.iter().collect()
    } else {
        all.iter()
            .filter(|ch| {
                ch.name.to_lowercase().contains(&search)
                    || ch.channel_type.as_str().to_lowercase().contains(&search)
                    || ch.base_url.to_lowercase().contains(&search)
                    || ch.models.iter().any(|m| m.to_lowercase().contains(&search))
            })
            .collect()
    };
    let total = filtered.len();

    let page = params
        .get("page")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&p| p >= 1)
        .unwrap_or(1);
    let page_size = params
        .get("page_size")
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(0);

    let channels: Vec<Value> = if page_size > 0 {
        let start = (page - 1) * page_size;
        let end = start.saturating_add(page_size).min(total);
        filtered[start..end]
            .iter()
            .map(|ch| mask_channel(ch, id_to_seq.get(ch.id.as_str()).copied()))
            .collect()
    } else {
        // 不分页：全量（兼容旧调用方）
        filtered
            .iter()
            .map(|ch| mask_channel(ch, id_to_seq.get(ch.id.as_str()).copied()))
            .collect()
    };
    Ok(Json(json!({
        "success": true,
        "data": channels,
        "page": page,
        "page_size": page_size,
        "total": total
    })))
}

/// 列出可用模型（登录用户即可，对齐 new-api `GET /api/user/models`）
///
/// 聚合所有启用渠道声明的模型，供 Playground 模型下拉使用。
/// 不返回渠道明细/密钥，普通用户与管理员共用同一份可用模型清单。
/// P1：附带元信息（owned_by/context_length/capabilities）。
/// 按用户分组 allowed_models 白名单过滤（None/空=不限，对齐 UserGroup::allows_model）。
pub async fn handle_available_models(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let user = verify_user(&state, &headers).await?;
    // 组白名单：None 与 Some(空) 均表示不限
    let group_allow: Option<Vec<String>> = state
        .user_group_store
        .get(&user.group)
        .and_then(|g| g.allowed_models);
    let unrestricted = match &group_allow {
        None => true,
        Some(list) => list.is_empty(),
    };
    let allow_set = group_allow.unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let models: Vec<Value> = state
        .channel_store
        .list()
        .into_iter()
        .filter(|c| c.is_enabled())
        .flat_map(|c| {
            let owned_by =
                crate::model::metadata::owned_by_for_channel_type(c.channel_type.as_str());
            c.models.into_iter().map(move |m| (m, owned_by))
        })
        .filter(|(m, _)| {
            !m.is_empty()
                && seen.insert(m.clone())
                && (unrestricted || allow_set.iter().any(|a| a == m))
        })
        .map(|(m, owned_by)| {
            let meta = state.model_metadata.get(&m, owned_by);
            let price = state.pricing_store.get_price(&m);
            json!({
                "id": m,
                "owned_by": meta.owned_by,
                "context_length": meta.context_length,
                "capabilities": meta.capabilities,
                "price_input": price.as_ref().map(|p| p.input_price).unwrap_or(0.0),
                "price_output": price.as_ref().map(|p| p.output_price).unwrap_or(0.0),
                "price_type": price.as_ref().map(|p| p.price_type.as_str()).unwrap_or("token"),
            })
        })
        .collect();
    Ok(Json(json!({ "success": true, "data": models })))
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
        Ok(c) => Ok(Json(
            json!({ "success": true, "data": mask_channel(&c, None) }),
        )),
        Err(e) => Err(error_response(
            &format!("Failed to add channel: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

/// 更新渠道信息
///
/// PUT 全量更新：保留既有测试结果/余额等运维字段（to_channel 不携带这些，
/// 旧实现直接 add 会把它们清零），再覆盖表单提交的字段。
pub async fn handle_update_channel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChannelRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let _config = verify_admin(&state, &headers).await?;
    let mut ch = body.to_channel(id.clone());
    // 保留运维字段（test_time/response_time/balance/credit/discovered_models/
    // last_used_at/created_at）：编辑表单不提交这些，不应被清空
    if let Some(prev) = state.channel_store.get(&id) {
        ch.response_time = prev.response_time;
        ch.test_time = prev.test_time;
        ch.balance = prev.balance;
        ch.credit = prev.credit;
        ch.discovered_models = prev.discovered_models;
        ch.last_used_at = prev.last_used_at;
        ch.last_error = prev.last_error;
        ch.created_at = prev.created_at;
        // api_key 留空 → 保留原密钥（前端编辑时留空表示不变）
        if ch.api_key.is_empty() {
            ch.api_key = prev.api_key;
        }
    }
    match state.channel_store.update(&id, ch) {
        Ok(c) => Ok(Json(
            json!({ "success": true, "data": mask_channel(&c, None) }),
        )),
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
