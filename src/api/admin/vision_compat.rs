// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//!
//! 视觉兼容管理面 —— `GET/PUT /api/vision-compatibility`。
//!
//! 契约对齐上游 `server/api/vision_compat.rs`：
//! - `GET` → `{ config, revision, models, sharedTarget, maxTargets, maxModels, legacyExpanded }`
//!  （AIGX 统一包在 `{success, data}` 里，字段名一致）；
//! - `PUT` 入参 `{ config, revision?, consent? }`。
//!
//! ── 与旧端点 `/api/image-compat/settings` 的关系 ────────────────
//! 两个端点写同一份 `config.image_compat`，但视觉部分有两个形状：
//! - **新形状**（`config.image_compat.vision = Some(VisionSettings)`）由本端点写入，
//!   数据面以它为权威；
//! - **旧扁平形状**（`vision_enabled` / `mode` / `vision_model` / `fallback_models`）
//!   由旧端点写入；旧端点 GET 输出不变（`ImageCompatPanel.tsx` 不破）。
//!
//! 切换策略（不静默丢配置、不两套打架）：
//! - 本端点 PUT 一旦成功 → `vision = Some(...)` 成为权威；旧扁平字段**保留原值**
//!   供旧面板展示，但不再参与裁决；
//! - 旧端点 PUT 且本次**动了视觉字段** → 用旧扁平值重建等价的旧版形状并写回
//!   `vision`（等价于用户回到旧面板重新保存一遍），规则集随之让位 —— 旧面板
//!   仍可用，最后一个写入方赢。只改压缩参数的请求不触碰 `vision`。
//!
//! ── 本端点没做上游的哪几件事（诚实清单）──
//! - **revision 强一致**：上游 PUT 必须带 revision，不匹配 409。AIGX 旧端点没有
//!   revision 概念、`ConfigManager` 也无 CAS；这里收 `revision` 并在**提供时**校验
//!   （不匹配 409），未提供则放行 —— 避免老客户端直接 400。乐观并发的完整性弱于上游。
//! - **`legacyExpanded` 展开**：上游 GET 会把旧版「全局二选一 + 模型单独配置」
//!   按当前模型目录展开成两块名单。AIGX 不做目录展开（旧扁平形状直接换算成等价
//!   旧版设置返回，`legacyExpanded` 仅标记「读到的仍是旧形状」）。
//! - **`consent`**：来源是上游 `api/vision_compat.rs` 的 `SaveSettings.consent`
//!   （「旧版客户端保存前的人工授权确认」），上游现在同样只收不校验（改完即生效）；
//!   AIGX 行为一致。
//!
//! ── `prefer_current_route` 是结构性缺口 ────────────────────────
//! AIGX 的视觉裁决发生在**选路前**，「当前路由范围内的视觉模型」拿不到；
//! `vision::validate` 直接拒绝保存 `true`（配置了却不生效比不支持更糟），
//! GET 也一律回 `false`，避免面板暴露一个无效开关。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};
use crate::image_compat::vision::{
    self, Fallback, Mode, ModelSource, VisionSettings, VisionTarget,
};

/// PUT 请求体（对齐上游 `SaveSettings`：`config` + `revision` + `consent`）。
///
/// `config` 的解析使用 `VisionSettings` 的 serde 默认：缺字段 = 默认值、
/// `null` 的 Option 字段 = None、空数组 = 显式空名单；未知字段被拒绝。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisionCompatSettingsRequest {
    pub config: VisionSettings,
    /// 读取时的 revision。**提供时**不匹配返回 409；未提供放行（见模块头诚实清单）。
    #[serde(default)]
    pub revision: Option<String>,
    /// 旧版客户端的人工授权确认字段：保留声明，不校验。
    #[serde(default)]
    #[allow(dead_code)]
    pub consent: bool,
}

/// 当前生效的视觉设置（新形状优先；否则由旧扁平字段换算成等价旧版设置）。
///
/// 返回第二项仅说明配置来源（`true` = 由扁平字段换算，即新端点从未写过）。
fn current_settings(config: &crate::config::AppConfig) -> (VisionSettings, bool) {
    let compat = &config.image_compat;
    match &compat.vision {
        Some(settings) => (settings.clone(), false),
        None => (
            vision::legacy_from_flat(
                compat.vision_enabled,
                &compat.mode,
                &compat.vision_model,
                &compat.fallback_models,
            ),
            true,
        ),
    }
}

fn revision_of(settings: &VisionSettings) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_string(settings).unwrap_or_default();
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}

/// 模型目录投影（对齐上游 `routing_groups::choices` + `supportsImages` 三态）。
///
/// 行来自统一模型管理视图（provider × model），能力取
/// `capabilityFlags.supportsImages`（`true` / `false` / `null` = 未声明）。
fn model_choices(state: &AppState) -> Vec<Value> {
    let view = super::model_manage::current_view(state);
    view.models
        .iter()
        .map(|model| {
            let supports_images = model
                .capability_flags
                .get("supportsImages")
                .cloned()
                .unwrap_or(Value::Null);
            json!({
                "provider": model.provider,
                "providerLabel": model.provider_label,
                "model": model.id,
                "enabled": model.enabled,
                "capabilities": { "supportsImages": supports_images },
            })
        })
        .collect()
}

/// 目标必须「在目录里 + 已启用 + 明确支持图片」才允许保存（对齐上游
/// `validate_available`；AIGX 无“转发启用”独立开关，用 `enabled` 行状态表达）。
fn validate_available(choices: &[Value], target: &VisionTarget) -> Result<(), String> {
    let row = choices.iter().find(|row| {
        row["provider"]
            .as_str()
            .is_some_and(|p| p.eq_ignore_ascii_case(target.provider.trim()))
            && row["model"]
                .as_str()
                .is_some_and(|m| m.eq_ignore_ascii_case(target.model.trim()))
    });
    let Some(row) = row else {
        return Err("所选视觉模型已不在模型目录中，请重新选择".into());
    };
    if row["enabled"].as_bool() == Some(false) {
        return Err("所选视觉模型尚未启用，请先在模型管理中启用".into());
    }
    if row["capabilities"]["supportsImages"].as_bool() != Some(true) {
        return Err("请选择已确认支持图片的视觉模型；能力未声明时请先在模型管理中确认".into());
    }
    Ok(())
}

/// 视图（GET / PUT 成功后返回同一份）。
fn view(state: &AppState, config: &crate::config::AppConfig) -> Value {
    let (mut settings, _from_flat) = current_settings(config);
    // `legacyExpanded`：当前配置仍是旧版形状（全局二选一 + 模型单独配置）。
    // 上游用它表示「读到的旧版配置已按目录展开」；AIGX 不做目录展开（见模块头
    // 诚实清单），这里如实标记「仍是旧形状」，前端据此提示迁移。
    let legacy_expanded = settings.is_legacy();
    // 两块设置折成规则列表，再把规则投影回两块 —— 新老界面都看到真实配置。
    settings.fold_blocks();
    settings.project_blocks();
    // `prefer_current_route` 在 AIGX 不适用（选路前裁决）：对外一律回 false，
    // 面板因此不会展示一个存了也不生效的开关。PUT 侧由 `vision::validate` 拒绝 true。
    settings.reroute.prefer_current_route = false;
    for rule in &mut settings.rules {
        rule.prefer_current_route = false;
    }
    let shared_target = crate::image_compat::shared_vision_target(state);
    json!({
        "config": settings,
        "revision": revision_of(&current_settings(config).0),
        "models": model_choices(state),
        "sharedTarget": shared_target,
        "maxTargets": vision::MAX_TARGETS,
        "maxModels": vision::MAX_MODELS,
        "legacyExpanded": legacy_expanded,
    })
}

/// GET /api/vision-compatibility —— 读取视觉兼容配置 + 模型目录 + 共享视觉模型。
pub async fn handle_get_vision_compatibility(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": view(&state, &config),
    })))
}

/// PUT /api/vision-compatibility —— 保存视觉兼容配置（写盘 + 热生效）。
pub async fn handle_update_vision_compatibility(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<VisionCompatSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    // 1) 乐观并发（提供 revision 时强校验；未提供放行）
    if let Some(expected) = body.revision.as_deref() {
        let actual = revision_of(&current_settings(&config).0);
        if expected != actual {
            return Err(error_response(
                "图片兼容已在其他位置修改，请重新读取后再改；本次改动未生效",
                StatusCode::CONFLICT,
            ));
        }
    }
    // 2) 旧界面（只有两块设置）保存 → 折成规则列表；同一种方式已有多条规则时拒绝
    let mut settings = body.config;
    if settings.rules.is_empty() {
        let previous = current_settings(&config).0;
        let count = |mode: Mode| {
            previous
                .rules
                .iter()
                .filter(|rule| rule.mode == mode)
                .count()
        };
        if count(Mode::Assist) > 1 || count(Mode::Reroute) > 1 {
            return Err(error_response(
                "图片兼容已改为按规则配置；这一种方式下有多条规则时，请按规则方式修改",
                StatusCode::BAD_REQUEST,
            ));
        }
        settings.fold_blocks();
    }
    // 只要有规则，旧版顶层字段（`mode` / `overrides`）就必须清掉 —— 否则
    // `is_legacy()` 为真、`rule_for` 走旧全局分支而**忽略规则列表**。
    // 一条规则都没有时（纯旧配置回存）才保留旧字段、按旧语义继续生效。
    if !settings.rules.is_empty() {
        settings.clear_legacy();
    }
    // 3) 结构与组合校验（上限 / 必填 / 思考等级）
    vision::validate(&settings)
        .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
    // 4) 启用中的规则：视觉目标必须在目录里、已启用、支持图片
    if settings.enabled {
        let choices = model_choices(&state);
        for rule in settings
            .rules
            .iter()
            .filter(|rule| rule.enabled && !rule.models.is_empty())
        {
            match rule.mode {
                Mode::Assist => {
                    let target = match rule.source {
                        ModelSource::Shared => crate::image_compat::shared_vision_target(&state),
                        ModelSource::Custom => rule.target.clone(),
                    }
                    .ok_or_else(|| {
                        error_response(
                            "请先配置辅助识图的视觉模型（工具服务 → 视觉，或指定模型）",
                            StatusCode::BAD_REQUEST,
                        )
                    })?;
                    validate_available(&choices, &target)
                        .map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?;
                }
                Mode::Reroute => {
                    if rule.fallback == Fallback::Designated {
                        for target in &rule.targets {
                            validate_available(&choices, target)
                                .map_err(|m| error_response(&m, StatusCode::BAD_REQUEST))?;
                        }
                    }
                }
            }
        }
    }
    // 5) 投影两块设置后落盘（新端点从此成为视觉兼容的权威形状）
    settings.project_blocks();
    config.image_compat.vision = Some(settings.clone());
    let snapshot = config.image_compat.clone();
    match state.config_manager.update(config).await {
        Ok(()) => {
            // 保存即热生效：数据面每请求读进程快照
            crate::image_compat::set_settings(snapshot.clone());
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": view(&state, &updated),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save vision compatibility settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<Value> {
        vec![
            json!({
                "provider": "openai", "providerLabel": "OpenAI", "model": "gpt-4o",
                "enabled": true, "capabilities": { "supportsImages": true },
            }),
            json!({
                "provider": "openai", "providerLabel": "OpenAI", "model": "gpt-4.1",
                "enabled": true, "capabilities": { "supportsImages": false },
            }),
            json!({
                "provider": "openai", "providerLabel": "OpenAI", "model": "undeclared",
                "enabled": true, "capabilities": { "supportsImages": null },
            }),
            json!({
                "provider": "openai", "providerLabel": "OpenAI", "model": "disabled-vision",
                "enabled": false, "capabilities": { "supportsImages": true },
            }),
        ]
    }

    fn target(provider: &str, model: &str) -> VisionTarget {
        VisionTarget {
            provider: provider.into(),
            model: model.into(),
            reasoning: String::new(),
        }
    }

    /// 目录校验三态：在且支持 → 过；不支持 / 未声明 / 停用 / 不在目录 → 拒。
    #[test]
    fn validate_available_enforces_catalog_tristate() {
        let choices = choices();
        validate_available(&choices, &target("openai", "gpt-4o")).expect("支持图片的模型必须通过");
        // 大小写不敏感
        validate_available(&choices, &target("OpenAI", "GPT-4O")).expect("大小写不敏感");
        for (model, needle) in [
            ("gpt-4.1", "支持图片"),
            ("undeclared", "支持图片"),
            ("disabled-vision", "尚未启用"),
            ("ghost", "不在模型目录"),
        ] {
            let error = validate_available(&choices, &target("openai", model)).unwrap_err();
            assert!(
                error.contains(needle),
                "{model} 应以「{needle}」拒绝: {error}"
            );
        }
        // provider 不匹配 → 视为不在目录（provider+model 精确）
        let error = validate_available(&choices, &target("anthropic", "gpt-4o")).unwrap_err();
        assert!(error.contains("不在模型目录"), "{error}");
    }

    /// revision 对同一份配置稳定、配置一变就变（乐观并发的基础）。
    #[test]
    fn revision_changes_with_config() {
        let a = VisionSettings::default();
        let mut b = VisionSettings::default();
        assert_eq!(revision_of(&a), revision_of(&a.clone()));
        assert_ne!(revision_of(&a), revision_of(&b));
        b.rules.push(vision::VisionRule::default());
        assert_ne!(revision_of(&a), revision_of(&b));
    }
}
