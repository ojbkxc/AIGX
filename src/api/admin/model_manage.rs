//! 统一模型管理 API —— `GET /api/models/manage` 与写接口。
//!
//! Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//! 对应上游 `api/model_manage.rs`（映射三元组 / 能力位覆盖 / 自定义模型）。
//!
//! 读：把渠道模型 + 账号池模型按 provider 聚合成一张表（见 `model::manage`）。
//! 写：
//! - `POST /api/models/state`     启停 `{provider, id, enabled}`
//! - `GET  /api/models/mappings`  逐条映射清单（`{mappings, dangling, reasoningLevels}`）
//! - `POST /api/models/mappings`  新增 / 更新映射 `{alias, target, provider?, reasoning?, enabled?, create?}`
//! - `POST /api/models/mappings/remove` 删除映射 `{alias, target, provider?}`
//! - `POST /api/models/capabilities` 能力位三态覆盖 `{provider, id, capabilities: {key: bool|null}}`
//! - `POST /api/models/price`     定价 `{model, input_price, output_price, cache_price?, price_type?, model_ratio?}`
//! - `POST /api/models/price/remove` 删除定价 `{model}`
//! - `POST /api/models/alias`     别名映射 `{alias, target}`（旧入口，写全局条目）
//! - `POST /api/models/alias/remove` 删除别名 `{alias}`
//! - `POST /api/models/alias/prefix` 批量前缀别名 `{prefix, sep?, merge?, targets[]}`
//!
//! 所有写接口返回最新的完整视图，前端就地重绘、不必再拉一次 —— 与
//! Agent2API `model_manage.rs` 的「写接口回读视图」约定一致。
//!
//! ## 为什么启停粒度是 `(provider, id)` 而不是模型名
//!
//! 同一个 `gpt-4o` 可能同时由 openai 直连渠道和某个中转账号承载。按模型名
//! 关停会连带关掉另一家 —— 管理员想表达的通常是「这家别再用这个模型了」。
//!
//! ## T5：映射的 provider 维度与三态协议
//!
//! 映射是 `{alias, target, provider(可空), reasoning(可空), enabled}`（见
//! `model::Mapping`）。存量 `alias → target` 在装载时迁成 `provider = None`
//! （= `*`，对所有承载 target 的家生效），行为逐字不变。
//!
//! `reasoning` / `enabled` / `create` 三个字段都是**三态**（键缺失 ≠ 显式值）：
//! 键缺失 = 不动；给值 = 显式设置。理由见各 handler 的注释 —— 两个三态字段
//! 各管各的，改等级不会顺手把开关打开，切开关不会顺手清掉等级。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin, verify_user};
use crate::model::capability_overrides;
use crate::model::manage::{build_view, view_json, ManageView};
use crate::pricing::{ModelPrice, RatioConfig};

/// 思考等级候选表（与 A2A `model_rules::REASONING_LEVELS` 逐字一致；
/// 随 manage 响应一并发给前端，前端不自己抄一份）。
pub const REASONING_LEVELS: [&str; 8] = [
    "off", "none", "minimal", "low", "medium", "high", "xhigh", "max",
];

/// 思考等级的长度上限（自定义等级也走这里，与 A2A `REASONING_MAX_CHARS` 同值）。
const REASONING_MAX_CHARS: usize = 32;

/// 归一一个思考等级：trim → 空 / 超长返回 None（= 不覆盖）。
///
/// **不校验是否在候选表里**：界面有「自定义输入」入口（与 A2A 同取向——
/// 表外值合法地存在于配置与界面里，只是转发侧另有闸门）。
fn normalize_reasoning(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.chars().count() > REASONING_MAX_CHARS {
        return None;
    }
    Some(trimmed.to_string())
}

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
                    source: "none".to_string(),
                    refreshed_at: 0,
                }
            });
            g.total += 1;
            g.enabled += 1;
        }
        view.providers = groups.into_values().collect();
        view.unpriced = view.models.iter().filter(|m| m.price.is_none()).count();
        // 客户页看不到映射（它们是运营配置），两个映射区块一并清空
        view.mapping_entries.clear();
        view.dangling.clear();
        view.mappings.clear();
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

/// GET /api/models/mappings —— 逐条映射清单（A2A `GET /api/models/mappings`）。
///
/// 响应 `{success, data: {mappings, dangling, reasoningLevels}}`：
/// - `mappings[]`：全量条目（含关闭条目；`dangling` / `carried` 标记）；
/// - `dangling[]`：未挂载条目（与 manage 视图同源）；
/// - `reasoningLevels`：候选档位表（前端下拉用，不自己抄一份）。
///
/// 为什么不复用 `/api/models/manage`：映射编辑弹窗只需要这一小块，
/// manage 视图要遍历全部渠道 + 账号（含目录缓存读），重拉一次太贵。
pub async fn handle_list_mappings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let view = current_view(&state);
    Ok(Json(json!({
        "success": true,
        "data": {
            "mappings": view.mapping_entries,
            "dangling": view.dangling,
            "reasoningLevels": REASONING_LEVELS,
        }
    })))
}

/// POST /api/models/mappings —— 新增 / 更新一条映射。
///
/// 请求体：`{alias, target, provider?, reasoning?, enabled?, create?}`。
///
/// ── `create` 三态（A2A `add_mapping` 同口径）──────────────────
/// - `true`：只新增；同三元组已存在 → 409「已绑定」（避免「加两次」的静默重复）；
/// - `false`：只编辑；三元组不存在 → 404；
/// - 缺失：兼容旧调用方的保存语义（有则改、无则建）。
///
/// ── `reasoning` / `enabled` 三态（键缺失 = 不动）────────────────
/// 管理页切开关只传 `(alias, target, provider, enabled)`，改等级只传
/// `(alias, target, provider, reasoning)` —— 若把「没传」当成「清空」，
/// 每次切开关都会顺手清掉思考等级（静默数据丢失）。
pub async fn handle_upsert_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MappingBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let alias = body.alias.trim();
    let target = body.target.trim();
    if alias.is_empty() {
        return Err(error_response("别名不能为空", StatusCode::BAD_REQUEST));
    }
    if target.is_empty() {
        return Err(error_response(
            "目标上游模型不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 新建的对外名走「宽松合法性」（允许中文等非 ASCII：千问办公的上游 id
    // 就是 `高级`）；target 走上游 id 字符集口径
    if body.create == Some(true) && !crate::model::alias_name_valid(alias) {
        return Err(error_response(
            "对外名称不能为空、不能含空白或控制字符，且不超过 128 个字符",
            StatusCode::BAD_REQUEST,
        ));
    }
    if !crate::model::alias_valid(target) {
        return Err(error_response(
            "目标模型 ID 只能包含字母、数字与 - _ . / :，且不超过 128 个字符",
            StatusCode::BAD_REQUEST,
        ));
    }
    let provider = body
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    // 思考等级：超长值在落盘前拒掉（读侧容错会丢弃它 —— 不留一条读回来
    // 就消失的脏数据）
    let reasoning: Option<Option<&str>> = match body.reasoning.as_deref() {
        None => None,
        Some(text) if text.trim().is_empty() => Some(None),
        Some(text) => {
            if normalize_reasoning(text).is_none() {
                return Err(error_response(
                    "思考等级过长（最多 32 个字符）",
                    StatusCode::BAD_REQUEST,
                ));
            }
            Some(Some(text))
        }
    };
    match state.model_mapper.upsert_mapping(
        alias,
        target,
        provider,
        reasoning,
        body.enabled,
        body.create,
    ) {
        Ok(result) => {
            if result.changed {
                let subject = match provider {
                    Some(name) => format!("{alias} → {target}（{name}）"),
                    None => format!("{alias} → {target}"),
                };
                tracing::info!("[Models] 保存映射 {subject}");
            }
        }
        Err(error) => {
            let message = error.to_string();
            let status = if message.starts_with("模型 ID") {
                StatusCode::CONFLICT
            } else {
                StatusCode::NOT_FOUND
            };
            return Err(error_response(&message, status));
        }
    }
    Ok(ok_view(&current_view(&state)))
}

/// POST /api/models/mappings/remove —— 删除一条映射（三元组精确定位）。
///
/// 自映射（`alias == target`）正常不能删 —— 它跟着模型行存在，要停用请关
/// 开关。例外是**模型行已经不在了**（上游下线 / 那家被移除或没有可用账号）：
/// 这时开关没有落点，删除是唯一出路（管理页「未挂载的映射」正是给这批条目
/// 留的入口）。判据与视图里标 `dangling` 的那一处同源（`host_row_exists`）。
pub async fn handle_remove_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MappingRemoveBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let alias = body.alias.trim();
    let target = body.target.trim();
    if alias.is_empty() || target.is_empty() {
        return Err(error_response(
            "缺少映射名或目标上游模型",
            StatusCode::BAD_REQUEST,
        ));
    }
    let provider = body
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty());
    if alias == target && host_row_exists(&current_view(&state), provider, target) {
        return Err(error_response(
            "原始 ID 的默认绑定不能删除，请关闭它的开关",
            StatusCode::BAD_REQUEST,
        ));
    }
    let removed = match state.model_mapper.remove_mapping(alias, target, provider) {
        Ok(removed) => removed,
        Err(error) => {
            return Err(error_response(
                &format!("删除映射失败：{error}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        }
    };
    if !removed {
        return Err(error_response(
            &format!("映射不存在: {alias} → {target}"),
            StatusCode::NOT_FOUND,
        ));
    }
    tracing::info!("[Models] 删除映射 {alias} → {target}");
    Ok(ok_view(&current_view(&state)))
}

/// `(provider, target)` 是否还能在管理视图里落到一行模型行。
///
/// 与 `model::manage::build_view` 把映射标成 `dangling` 的路径同源：
/// 有行 = 这家仍承载这个模型（含自定义登记）。
fn host_row_exists(view: &ManageView, provider: Option<&str>, target: &str) -> bool {
    view.models.iter().any(|row| {
        row.id == target && provider.is_none_or(|owner| row.provider.eq_ignore_ascii_case(owner))
    })
}

/// 一条映射请求体（POST /api/models/mappings）。
#[derive(Debug, Deserialize)]
pub struct MappingBody {
    pub alias: String,
    pub target: String,
    /// `None` = 全局条目（对所有承载 target 的家生效）
    #[serde(default)]
    pub provider: Option<String>,
    /// 三态：键缺失 = 不动；null / 空串 = 清除；字符串 = 设置
    #[serde(default)]
    pub reasoning: Option<String>,
    /// 三态：键缺失 = 不动；bool = 显式开 / 关
    #[serde(default)]
    pub enabled: Option<bool>,
    /// 三态：true = 只新增（已存在 → 409）；false = 只编辑（不存在 → 404）
    #[serde(default)]
    pub create: Option<bool>,
}

/// 删除一条映射的请求体（POST /api/models/mappings/remove）。
#[derive(Debug, Deserialize)]
pub struct MappingRemoveBody {
    pub alias: String,
    pub target: String,
    #[serde(default)]
    pub provider: Option<String>,
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
    if !matches!(body.price_type.as_str(), "token" | "count" | "free") {
        return Err(error_response(
            "计价方式必须是 token、count 或 free",
            StatusCode::BAD_REQUEST,
        ));
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

/// 批量前缀别名请求体（`POST /api/models/alias/prefix`）。
#[derive(Debug, Deserialize)]
pub struct AliasPrefixBody {
    /// 前缀（`{prefix}{sep}{target}` 拼出对外名）。
    pub prefix: String,
    /// 分隔符（默认 `-`；允许空串，即前缀直接贴模型 id）。
    #[serde(default = "default_alias_sep")]
    pub sep: String,
    /// `true` = 同别名已有映射时追加（合并到多家）；`false` = 冲突报错。
    #[serde(default = "default_merge")]
    pub merge: bool,
    /// 要加别名的模型 id 列表。
    #[serde(default)]
    pub targets: Vec<String>,
}

fn default_alias_sep() -> String {
    "-".to_string()
}

fn default_merge() -> bool {
    true
}

/// 计算前缀别名计划（纯函数，便于单测）。
///
/// 对每个 target 生成 `alias = prefix + sep + target`：
/// - 别名已是同一 target → 幂等跳过（重复点「批量添加」不报错）；
/// - 别名已映射到别的 target（同别名多家）：
///   `merge=true` → 追加一条（数据面按写入顺序取第一条，既有映射保持优先）；
///   `merge=false` → 报冲突，整批不落盘（先算完再写，避免半批成功）；
/// - 返回待写入的 `(alias, target)` 列表（已去重、保持 targets 顺序）。
fn plan_alias_prefix(
    existing: &[crate::model::Mapping],
    prefix: &str,
    sep: &str,
    merge: bool,
    targets: &[String],
) -> Result<Vec<(String, String)>, String> {
    let prefix = prefix.trim();
    if prefix.is_empty() {
        return Err("前缀不能为空".to_string());
    }
    let mut planned: Vec<(String, String)> = Vec::new();
    for raw in targets {
        let target = raw.trim();
        if target.is_empty() {
            return Err("目标模型不能为空".to_string());
        }
        if !crate::model::alias_valid(target) {
            return Err(format!(
                "目标模型「{target}」只能包含字母、数字与 - _ . / :，且不超过 128 个字符"
            ));
        }
        let alias = format!("{prefix}{sep}{target}");
        if !crate::model::alias_name_valid(&alias) {
            return Err(format!(
                "生成的别名「{alias}」不合法（不能含空白/控制字符，且不超过 128 个字符）"
            ));
        }
        if planned.iter().any(|(a, _)| a == &alias) {
            continue;
        }
        let existing_targets: Vec<&str> = existing
            .iter()
            // 只与**全局条目**比对：本接口写的是全局条目；provider 维度条目
            // 是另一条轴（只对某一家生效），不构成同名冲突。
            .filter(|m| m.provider.is_none() && m.alias == alias)
            .map(|m| m.target.as_str())
            .collect();
        if existing_targets.contains(&target) {
            continue; // 幂等：这条绑定已经在了
        }
        if !existing_targets.is_empty() && !merge {
            return Err(format!(
                "别名「{alias}」已被映射到 {}（merge=false 时不覆盖）",
                existing_targets.join(", ")
            ));
        }
        planned.push((alias, target.to_string()));
    }
    Ok(planned)
}

/// POST /api/models/alias/prefix —— 批量生成「前缀 + 模型 id」别名。
///
/// 前端模型管理页多选后调用（`batchAddAliasPrefix`）：`{prefix, sep, merge,
/// targets}`。全部校验/冲突判定先算完再落盘，保证「要么整批成功、要么一条
/// 都不写」。返回最新完整视图（与其它写接口一致）。
pub async fn handle_add_alias_prefix(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<AliasPrefixBody>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    verify_admin(&state, &headers).await?;
    let existing = state.model_mapper.all_mapping_entries();
    let planned = plan_alias_prefix(
        &existing,
        &body.prefix,
        &body.sep,
        body.merge,
        &body.targets,
    )
    .map_err(|message| {
        let status = if message.starts_with("别名「") {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        error_response(&message, status)
    })?;
    for (alias, target) in &planned {
        state
            .model_mapper
            .upsert_mapping(alias, target, None, None, None, None)
            .map_err(|e| {
                error_response(
                    &format!("保存别名失败：{e}"),
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            })?;
    }
    if !planned.is_empty() {
        tracing::info!("[Models] 批量添加前缀别名 {} 条", planned.len());
    }
    Ok(ok_view(&current_view(&state)))
}

/// POST /api/models/capabilities —— 覆盖某条模型对下游声明的能力位。
///
/// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
/// 对应上游 `api/model_manage.rs::set_capabilities`。
///
/// ── 两种请求形状并存 ────────────────────────────────────────
/// 1. **新（A2A 口径，三态）**：`{provider, id, capabilities: {键: bool|null}}`
///    —— 按 `(provider, id)` 逐键保存，`null` = 清除这一项（回到 inherit）。
///    存储是 `model::capability_overrides`，出口（`/v1/models` / Anthropic
///    列表 / 管理视图）统一应用，优先级 user > upstream > models.dev。
///    键名见 `capability_overrides::KEYS`（supportsToolCall /
///    supportsImages / supportsVideo / supportsReasoning /
///    supportsEmbedding / supportsRerank + supportsAudio / supportsChat）。
/// 2. **旧（标签集形态）**：`{model, capabilities: Vec<String>}`
///    —— 行为不变（写 `ModelMetadataRegistry`，整份替换该模型的标签集）。
///    旧前端 / 脚本继续走这条，不被新形状破坏。
///
/// 判据：请求体带 `provider` 键 → 新形态；否则旧形态。
#[derive(Debug, Deserialize)]
pub struct CapabilitiesBody {
    /// 旧形态：按模型名（无 provider 维度）。
    #[serde(default)]
    pub model: Option<String>,
    /// 新形态：provider 维度。
    #[serde(default)]
    pub provider: Option<String>,
    /// 新形态：模型 id。
    #[serde(default)]
    pub id: Option<String>,
    /// 旧形态：标签集（字符串数组）。
    #[serde(default)]
    pub capabilities: Option<Value>,
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
    match (body.provider.as_deref(), body.capabilities.as_ref()) {
        (Some(provider), Some(Value::Object(patch))) => {
            set_capability_overrides(provider, body.id.as_deref(), patch)
        }
        (Some(_), _) => Err(error_response(
            "capabilities 必须是对象（键 → bool|null）",
            StatusCode::BAD_REQUEST,
        )),
        (None, Some(capabilities)) => {
            set_legacy_capabilities(&state, body.model.as_deref(), capabilities)
        }
        (None, None) => Err(error_response(
            "缺少 capabilities 字段",
            StatusCode::BAD_REQUEST,
        )),
    }
}

/// 新形态：`(provider, id)` 三态覆盖。
fn set_capability_overrides(
    provider: &str,
    id: Option<&str>,
    patch: &Map<String, Value>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let provider = provider.trim();
    let id = id.map(str::trim).unwrap_or("");
    if provider.is_empty() || id.is_empty() {
        return Err(error_response(
            "provider 与 id 不能为空",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 值校验在这里就拦掉：不留一条读回来会被归一丢弃的脏数据。
    // `null` 是合法的「清除」信号（restore），不算非法值。
    for (key, value) in patch {
        if !capability_overrides::is_known_key(key) {
            return Err(error_response(
                &format!("未知的能力字段: {key}"),
                StatusCode::BAD_REQUEST,
            ));
        }
        if !value.is_null() && capability_overrides::normalize_value(key, value).is_none() {
            return Err(error_response(
                &format!("能力字段 {key} 取值非法（能力开关要布尔，清除用 null）"),
                StatusCode::BAD_REQUEST,
            ));
        }
    }
    let Some(store) = capability_overrides::current() else {
        return Err(error_response(
            "能力覆盖存储未初始化（服务启动接线缺失）",
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    };
    if let Err(error) = store.set(provider, id, patch) {
        return Err(error_response(
            &format!("保存能力覆盖失败：{error}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    let summary = patch
        .iter()
        .map(|(key, value)| match value {
            Value::Null => format!("{key}=恢复"),
            other => format!("{key}={other}"),
        })
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!("[Models] 保存模型能力 [{provider}] {id}: {summary}");
    Ok(Json(json!({
        "success": true,
        "data": {
            "provider": provider,
            "id": id,
            "capabilities": store.get(provider, id).map(Value::Object).unwrap_or(Value::Null),
            "overriddenKeys": store.overridden_keys(provider, id),
        }
    })))
}

/// 旧形态：整份替换 `ModelMetadataRegistry` 的标签集（行为不变）。
fn set_legacy_capabilities(
    state: &AppState,
    model: Option<&str>,
    capabilities: &Value,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let model = model.map(str::trim).unwrap_or("");
    if model.is_empty() {
        return Err(error_response("model 不能为空", StatusCode::BAD_REQUEST));
    }
    let Some(list) = capabilities.as_array() else {
        return Err(error_response(
            "缺少 capabilities 字段（或它不是数组）",
            StatusCode::BAD_REQUEST,
        ));
    };
    let mut tags: Vec<String> = Vec::with_capacity(list.len());
    for item in list {
        let Some(cap) = item.as_str() else {
            return Err(error_response(
                "能力标签必须是字符串",
                StatusCode::BAD_REQUEST,
            ));
        };
        if !valid_capability(cap) {
            return Err(error_response(
                &format!("未知的能力标签: {cap}"),
                StatusCode::BAD_REQUEST,
            ));
        }
        tags.push(cap.to_string());
    }
    let owned_by = state.model_metadata.get(model, None).owned_by;
    let context_length = state.model_metadata.get(model, None).context_length;
    // 能力覆盖不能顺手清掉思考等级：既有覆盖的 reasoning_levels 原样保留
    //（否则「改个能力标签 → 等级下拉回到空」）。
    let reasoning_levels = state
        .model_metadata
        .all_overrides()
        .iter()
        .find(|(name, _)| name.as_str() == model)
        .map(|(_, meta)| meta.reasoning_levels.clone())
        .unwrap_or_default();
    // 同 reasoning_levels：能力覆盖不能顺手清掉 tool_calling_mode
    let tool_calling_mode = state
        .model_metadata
        .all_overrides()
        .iter()
        .find(|(name, _)| name.as_str() == model)
        .and_then(|(_, meta)| meta.tool_calling_mode.clone());
    let meta = crate::model::metadata::ModelMetadata {
        owned_by,
        context_length,
        capabilities: tags.clone(),
        reasoning_levels,
        tool_calling_mode,
    };
    if let Err(e) = state.model_metadata.set_override(model.to_string(), meta) {
        return Err(error_response(
            &format!("保存能力覆盖失败：{e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        ));
    }
    Ok(Json(json!({
        "success": true,
        "data": { "model": model, "capabilities": tags }
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
    // 连带清掉针对这条模型的能力覆盖（与 A2A `remove_custom` 的
    // `capabilities.retain` 同语义）—— 留着就是一条永远读不回来的孤儿。
    if let Some(store) = capability_overrides::current() {
        if let Err(e) = store.remove_all_for(provider, id) {
            tracing::warn!("[Models] 清理自定义模型能力覆盖失败 [{provider}] {id}: {e}");
        }
    }
    Ok(ok_view(&current_view(&state)))
}

#[cfg(test)]
mod tests {
    use super::alias_valid;
    use super::{
        host_row_exists, normalize_reasoning, plan_alias_prefix, valid_capability, ManageView,
    };
    use crate::model::Mapping;

    fn mapping(alias: &str, target: &str) -> Mapping {
        Mapping {
            alias: alias.into(),
            target: target.into(),
            provider: None,
            reasoning: None,
            enabled: true,
        }
    }

    #[test]
    fn alias_prefix_plans_each_target() {
        let planned = plan_alias_prefix(
            &[],
            "team-",
            "-",
            true,
            &["gpt-4o".into(), "claude-3".into()],
        )
        .unwrap();
        assert_eq!(
            planned,
            vec![
                ("team--gpt-4o".to_string(), "gpt-4o".to_string()),
                ("team--claude-3".to_string(), "claude-3".to_string()),
            ]
        );
        // 空 sep = 前缀直接贴 id
        let planned = plan_alias_prefix(&[], "p_", "", true, &["m1".into()]).unwrap();
        assert_eq!(planned, vec![("p_m1".to_string(), "m1".to_string())]);
    }

    #[test]
    fn alias_prefix_is_idempotent() {
        let existing = vec![mapping("team--gpt-4o", "gpt-4o")];
        let planned = plan_alias_prefix(&existing, "team-", "-", true, &["gpt-4o".into()]).unwrap();
        assert!(planned.is_empty(), "已存在的同一绑定不重复写");
    }

    #[test]
    fn alias_prefix_conflict_without_merge() {
        let existing = vec![mapping("team--gpt-4o", "other-model")];
        let err =
            plan_alias_prefix(&existing, "team-", "-", false, &["gpt-4o".into()]).unwrap_err();
        assert!(err.contains("已被映射到 other-model"), "{err}");
    }

    #[test]
    fn alias_prefix_merges_into_existing_alias() {
        // merge=true：同别名已映射到别家 → 追加一条（同别名多家）
        let existing = vec![mapping("team--gpt-4o", "other-model")];
        let planned = plan_alias_prefix(&existing, "team-", "-", true, &["gpt-4o".into()]).unwrap();
        assert_eq!(
            planned,
            vec![("team--gpt-4o".to_string(), "gpt-4o".to_string())]
        );
    }

    #[test]
    fn alias_prefix_rejects_bad_input() {
        assert!(plan_alias_prefix(&[], "  ", "-", true, &["m".into()]).is_err());
        assert!(plan_alias_prefix(&[], "p-", "-", true, &["  ".into()]).is_err());
        assert!(
            plan_alias_prefix(&[], "p-", "-", true, &["has space".into()]).is_err(),
            "上游 id 字符集口径"
        );
        // 去重：同一 target 重复出现只写一条
        let planned = plan_alias_prefix(&[], "p-", "-", true, &["m".into(), "m".into()]).unwrap();
        assert_eq!(planned.len(), 1);
    }

    #[test]
    fn alias_rules() {
        assert!(alias_valid("gpt-4o"));
        assert!(alias_valid("pool/model"));
        assert!(alias_valid("qwen2.5:7b"));
        assert!(!alias_valid(""));
        assert!(!alias_valid("has space"));
        assert!(!alias_valid(&"x".repeat(129)));
    }

    /// 思考等级归一：空 / 超长 → None（= 不覆盖），其余 trim 后原样保留。
    /// 表外自定义值放行（A2A 同口径：界面有自定义输入入口）。
    #[test]
    fn reasoning_normalization() {
        assert_eq!(normalize_reasoning("  high ").as_deref(), Some("high"));
        assert_eq!(normalize_reasoning(""), None);
        assert_eq!(normalize_reasoning("   "), None);
        assert_eq!(
            normalize_reasoning("custom-level").as_deref(),
            Some("custom-level"),
            "表外值放行（能不能注入由转发侧另判）"
        );
        assert_eq!(normalize_reasoning(&"x".repeat(33)), None, "超 32 字符拒绝");
        assert!(normalize_reasoning(&"x".repeat(32)).is_some());
    }

    /// 自映射可删判据：模型行还在 → 不能删（请关开关）；行不在了 → 可删。
    #[test]
    fn default_binding_removal_guard() {
        let view = ManageView {
            providers: Vec::new(),
            models: Vec::new(),
            mappings: Default::default(),
            mapping_entries: Vec::new(),
            dangling: Vec::new(),
            capability_store_ready: true,
            unpriced: 0,
        };
        // 没有任何模型行 → 允许删（悬空映射的出口）
        assert!(!host_row_exists(&view, Some("catpaw"), "kimi-k3"));
        assert!(!host_row_exists(&view, None, "kimi-k3"));
    }

    #[test]
    fn capability_tag_whitelist() {
        for tag in [
            "chat",
            "vision",
            "video",
            "tools",
            "tool",
            "reasoning",
            "embedding",
            "rerank",
            "image",
            "audio",
        ] {
            assert!(valid_capability(tag), "{tag} 应合法");
        }
        assert!(!valid_capability("bogus"));
    }
}
