//! 统一模型管理 API —— `GET /api/models/manage` 与三个写接口。
//!
//! 读：把渠道模型 + 账号池模型按 provider 聚合成一张表（见 `model::manage`）。
//! 写：
//! - `POST /api/models/state`     启停 `{provider, id, enabled}`
//! - `POST /api/models/price`     定价 `{model, input_price, output_price, cache_price?, price_type?, model_ratio?}`
//! - `POST /api/models/price/remove` 删除定价 `{model}`
//! - `POST /api/models/alias`     别名映射 `{alias, target}`（写入全局 `model_custom_map`）
//! - `POST /api/models/alias/remove` 删除别名 `{alias}`
//!
//! 所有写接口返回最新的完整视图，前端就地重绘、不必再拉一次 —— 与
//! Agent2API `model_manage.rs` 的「写接口回读视图」约定一致。
//!
//! ## 为什么启停粒度是 `(provider, id)` 而不是模型名
//!
//! 同一个 `gpt-4o` 可能同时由 openai 直连渠道和某个中转账号承载。按模型名
//! 关停会连带关掉另一家 —— 管理员想表达的通常是「这家别再用这个模型了」。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin, verify_user};
use crate::model::manage::{build_view, view_json, ManageView};
use crate::pricing::{ModelPrice, RatioConfig};

/// 构建当前视图（所有读写接口共用）。
pub(crate) fn current_view(state: &AppState) -> ManageView {
    build_view(
        &state.channel_store.list(),
        &state.account_sessions,
        &state.model_rules,
        &state.model_metadata,
        &state.pricing_store,
        &state.model_mapper,
    )
}

fn ok_view(view: &ManageView) -> Json<Value> {
    Json(json!({ "success": true, "data": view_json(view) }))
}

/// GET /api/models/manage
///
/// 管理员拿到完整视图（含渠道/账号来源）；普通用户只拿「模型 + 价格 + 能力」，
/// 来源字段清空 —— 渠道名与账号名属于运营信息，不该出现在客户页面上。
/// 写接口一律仅管理员（见下）。
pub async fn handle_manage_view(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let is_admin = verify_admin(&state, &headers).await.is_ok();
    if !is_admin {
        // 非管理员走普通登录校验：未登录 / 禁用用户同样被拒
        let user = verify_user(&state, &headers).await?;
        // 客户只看得到「已启用 + 自己分组允许」的模型，且不带来源信息
        let allow: Option<Vec<String>> = state
            .user_group_store
            .get(&user.group)
            .and_then(|g| g.allowed_models);
        let unrestricted = match &allow {
            None => true,
            Some(list) => list.is_empty(),
        };
        let allow_set = allow.unwrap_or_default();
        let mut view = current_view(&state);
        view.models
            .retain(|m| m.enabled && (unrestricted || allow_set.iter().any(|a| a == &m.id)));
        for model in view.models.iter_mut() {
            model.sources.clear();
        }
        // 分组统计按过滤后的结果重算，避免左栏计数与表格对不上
        let mut groups: std::collections::BTreeMap<String, crate::model::manage::ProviderGroup> =
            std::collections::BTreeMap::new();
        for m in &view.models {
            let g = groups.entry(m.provider.clone()).or_insert_with(|| {
                crate::model::manage::ProviderGroup {
                    id: m.provider.clone(),
                    label: m.provider_label.clone(),
                    total: 0,
                    enabled: 0,
                    account: false,
                }
            });
            g.total += 1;
            g.enabled += 1;
        }
        view.providers = groups.into_values().collect();
        view.unpriced = view.models.iter().filter(|m| m.price.is_none()).count();
        return Ok(ok_view(&view));
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct StateRequest {
    pub provider: String,
    pub id: String,
    pub enabled: bool,
}

/// POST /api/models/state —— 启停某 provider 下的某模型。
pub async fn handle_set_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StateRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = body.provider.trim();
    let id = body.id.trim();
    if provider.is_empty() || id.is_empty() {
        return Err(error_response(
            "provider 与 id 不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    if let Err(e) = state.model_rules.set_enabled(provider, id, body.enabled) {
        return Err(error_response(
            &format!("保存模型状态失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct PriceBody {
    pub model: String,
    #[serde(default)]
    pub input_price: f64,
    #[serde(default)]
    pub output_price: f64,
    #[serde(default)]
    pub cache_price: Option<f64>,
    #[serde(default = "default_price_type")]
    pub price_type: String,
    /// 模型倍率；`None` = 不动（与价格共用一条接口时避免误清倍率）。
    #[serde(default)]
    pub model_ratio: Option<f64>,
}

fn default_price_type() -> String {
    "token".to_string()
}

/// POST /api/models/price —— 新增/更新定价（可选同时改模型倍率）。
pub async fn handle_upsert_model_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PriceBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = body.model.trim();
    if model.is_empty() {
        return Err(error_response("模型名不能为空", StatusCode::BAD_REQUEST));
    }
    if body.input_price < 0.0 || body.output_price < 0.0 {
        return Err(error_response("价格不能为负数", StatusCode::BAD_REQUEST));
    }
    if let Some(ratio) = body.model_ratio {
        if !ratio.is_finite() || ratio < 0.0 {
            return Err(error_response(
                "模型倍率必须是非负数",
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    let now = chrono::Utc::now().timestamp();
    let price = ModelPrice {
        model_name: model.to_string(),
        input_price: body.input_price,
        output_price: body.output_price,
        cache_price: body.cache_price,
        price_type: body.price_type.clone(),
        created_at: now,
        updated_at: now,
    };
    if let Err(e) = state.pricing_store.upsert_price(price) {
        return Err(error_response(
            &format!("保存价格失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    if let Some(ratio) = body.model_ratio {
        let mut ratios: RatioConfig = state.pricing_store.get_ratios();
        ratios.model_ratio.insert(model.to_string(), ratio);
        if let Err(e) = state.pricing_store.update_ratios(ratios) {
            return Err(error_response(
                &format!("保存模型倍率失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct PriceRemoveBody {
    pub model: String,
}

/// POST /api/models/price/remove —— 删除定价（倍率一并清除）。
pub async fn handle_remove_price(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<PriceRemoveBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = body.model.trim();
    if model.is_empty() {
        return Err(error_response("模型名不能为空", StatusCode::BAD_REQUEST));
    }
    if let Err(e) = state.pricing_store.delete_price(model) {
        return Err(error_response(
            &format!("删除价格失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    // 倍率也一并清掉，否则删完价格还留一条孤儿倍率
    let mut ratios: RatioConfig = state.pricing_store.get_ratios();
    if ratios.model_ratio.remove(model).is_some() {
        if let Err(e) = state.pricing_store.update_ratios(ratios) {
            return Err(error_response(
                &format!("清理模型倍率失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct AliasBody {
    pub alias: String,
    pub target: String,
    /// 映射开关（三态，与 Agent2API 同口径）：
    /// - 键缺失 → 不动已有状态（新建时默认启用）
    /// - `true` / `false` → 显式开 / 关
    ///
    /// 老前端不传这个键，行为与改造前完全一致。
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
}

/// 别名合法性：字母数字与 `- _ . / :`，≤128（与 Agent2API `alias_valid` 同口径）。
fn alias_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c))
}

/// POST /api/models/alias —— 新增/更新全局别名映射。
pub async fn handle_set_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AliasBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let alias = body.alias.trim();
    let target = body.target.trim();
    if !alias_valid(alias) {
        return Err(error_response(
            "别名只能包含字母、数字与 - _ . / :，且不超过 128 个字符",
            StatusCode::BAD_REQUEST,
        ));
    }
    if target.is_empty() {
        return Err(error_response("目标模型不能为空", StatusCode::BAD_REQUEST));
    }
    if let Err(e) = state
        .model_mapper
        .set_custom(alias.to_string(), target.to_string())
    {
        return Err(error_response(
            &format!("保存别名失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    // 三态：只在调用方显式给了才改开关状态。新建的映射默认启用。
    if let Some(enabled) = body.enabled {
        if let Err(e) = state.model_mapper.set_enabled(alias, enabled) {
            return Err(error_response(
                &format!("保存别名开关失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    }
    if let Some(effort) = &body.reasoning_effort {
        if let Err(e) = state.model_mapper.set_reasoning(alias, Some(effort)) {
            return Err(error_response(
                &format!("保存映射思考等级失败：{e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ));
        }
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct AliasStateBody {
    pub alias: String,
    pub enabled: bool,
}

/// POST /api/models/alias/state —— 只切开关，不改映射关系。
///
/// 单独一条接口的理由：切换开关是高频行内操作，带上 `target` 会让前端
/// 必须持有完整映射（渲染 chip 时有），但把「只改开关」和「改映射」分开
/// 之后，任何一次开关操作都不可能意外改掉映射目标。
pub async fn handle_set_alias_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AliasStateBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let alias = body.alias.trim();
    if alias.is_empty() {
        return Err(error_response("别名不能为空", StatusCode::BAD_REQUEST));
    }
    match state.model_mapper.set_enabled(alias, body.enabled) {
        Ok(true) => Ok(ok_view(&current_view(&state))),
        Ok(false) => Err(error_response(
            &format!("别名不存在：{alias}"),
            StatusCode::NOT_FOUND,
        )),
        Err(e) => Err(error_response(
            &format!("保存别名开关失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub struct AliasRemoveBody {
    pub alias: String,
}

/// POST /api/models/alias/remove —— 删除全局别名映射。
pub async fn handle_remove_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AliasRemoveBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let alias = body.alias.trim();
    if alias.is_empty() {
        return Err(error_response("别名不能为空", StatusCode::BAD_REQUEST));
    }
    if let Err(e) = state.model_mapper.remove_custom(alias) {
        return Err(error_response(
            &format!("删除别名失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct AliasReasoningBody {
    pub aliases: Vec<String>,
    pub effort: String,
}

pub async fn handle_set_alias_reasoning(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AliasReasoningBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let mut written = 0usize;
    for alias in &body.aliases {
        let a = alias.trim();
        if a.is_empty() {
            continue;
        }
        if state.model_mapper.all_mappings().contains_key(a) {
            if let Err(e) = state.model_mapper.set_reasoning(a, Some(&body.effort)) {
                return Err(error_response(
                    &format!("保存映射思考等级失败：{e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ));
            }
            written += 1;
        }
    }
    Ok(Json(json!({
        "success": true,
        "data": { "written": written, "view": view_json(&current_view(&state)) }
    })))
}

/// POST /api/models/capabilities —— 手动覆盖某模型的能力标签。
///
/// 对照 Agent2API `POST /api/models/capabilities`（基线是 6 键类型化模型）。
/// AIGX 的 `capabilities` 是 `Vec<String>` 标签集（chat / vision / tools /
/// reasoning / embedding / image / audio / video / rerank），语义更轻。
///
/// 覆盖写入 `ModelMetadataRegistry`（FileStore 持久化），优先级高于内置推断
/// 与 models.dev 补空位。只改 capabilities，保留现有 owned_by / context_length。
/// 空数组合法（表示「该模型无任何能力标签」，前端据此禁用）。
#[derive(Debug, Deserialize)]
pub struct CapabilitiesBody {
    pub model: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// 合法能力标签白名单（与 `infer_capabilities` / `CAPABILITY_HINTS` 同源）。
fn valid_capability(cap: &str) -> bool {
    matches!(
        cap,
        "chat"
            | "vision"
            | "video"
            | "tools"
            | "tool"
            | "reasoning"
            | "embedding"
            | "rerank"
            | "image"
            | "audio"
    )
}

pub async fn handle_set_capabilities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CapabilitiesBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let model = body.model.trim();
    if model.is_empty() {
        return Err(error_response("model 不能为空", StatusCode::BAD_REQUEST));
    }
    // 校验能力标签合法性
    for cap in &body.capabilities {
        if !valid_capability(cap) {
            return Err(error_response(
                &format!("未知的能力标签: {cap}"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    let owned_by = state.model_metadata.get(model, None).owned_by;
    let context_length = state.model_metadata.get(model, None).context_length;
    // 能力覆盖不能顺手清掉思考等级：既有覆盖的 reasoning_levels 原样保留
    //（否则「改个能力标签 → 等级下拉回到空」）。
    let reasoning_levels = state
        .model_metadata
        .all_overrides()
        .iter()
        .find(|(name, _)| name == model)
        .map(|(_, meta)| meta.reasoning_levels.clone())
        .unwrap_or_default();
    let meta = crate::model::metadata::ModelMetadata {
        owned_by,
        context_length,
        capabilities: body.capabilities.clone(),
        reasoning_levels,
    };
    if let Err(e) = state.model_metadata.set_override(model.to_string(), meta) {
        return Err(error_response(
            &format!("保存能力覆盖失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(Json(json!({
        "success": true,
        "data": { "model": model, "capabilities": body.capabilities }
    })))
}

#[derive(Debug, Deserialize)]
pub struct CustomModelBody {
    pub provider: String,
    pub id: String,
}

pub async fn handle_add_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CustomModelBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = body.provider.trim();
    let id = body.id.trim();
    if provider.is_empty() {
        return Err(error_response("provider 不能为空", StatusCode::BAD_REQUEST));
    }
    if id.is_empty() {
        return Err(error_response("模型 ID 不能为空", StatusCode::BAD_REQUEST));
    }
    if !alias_valid(id) {
        return Err(error_response(
            "模型 ID 只能包含字母、数字与 - _ . / :，且不超过 128 个字符",
            StatusCode::BAD_REQUEST,
        ));
    }
    if let Err(e) = state.model_mapper.add_custom_model(provider, id) {
        return Err(error_response(
            &format!("添加自定义模型失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(ok_view(&current_view(&state)))
}

#[derive(Debug, Deserialize)]
pub struct CustomModelRemoveBody {
    pub provider: String,
    pub id: String,
}

pub async fn handle_remove_custom_model(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CustomModelRemoveBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let provider = body.provider.trim();
    let id = body.id.trim();
    if provider.is_empty() || id.is_empty() {
        return Err(error_response(
            "provider 与模型 ID 不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    if let Err(e) = state.model_mapper.remove_custom_model(provider, id) {
        return Err(error_response(
            &format!("删除自定义模型失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(ok_view(&current_view(&state)))
}

#[cfg(test)]
mod tests {
    use super::alias_valid;

    #[test]
    fn alias_rules() {
        assert!(alias_valid("gpt-4o"));
        assert!(alias_valid("pool/model"));
        assert!(alias_valid("qwen2.5:7b"));
        assert!(!alias_valid(""));
        assert!(!alias_valid("has space"));
        assert!(!alias_valid(&"x".repeat(129)));
    }
}
