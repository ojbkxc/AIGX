//! 图片兼容设置管理面（对齐 Agent2API 3.4.0 会话图片压缩 / 3.5.0 图片兼容）。
//!
//! `GET/PUT /api/image-compat/settings`：压缩参数（开关/长边/质量/阈值）、
//! 视觉兼容（总开关/模式/视觉模型/备选链/辅助识图提示词）。
//!
//! 与 `settings.rs` 的 chat/report 面同款契约：GET 出 camelCase、PUT 入
//! snake_case（并收 camelCase 别名），缺省字段保持原值；**保存即热生效**
//!（写 `config.toml` 后立刻刷新进程级快照，数据面下一个请求即用新值）。
//!
//! 独立成文件（不并入 `settings.rs`）：该文件多人共用，本面自成一块，
//! 减少共享工作树的冲突面。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{error_response, verify_admin};

/// PUT 请求体（缺省字段保持原值；snake_case 主导，收 camelCase 别名）。
#[derive(Debug, Deserialize)]
pub struct ImageCompatSettingsRequest {
    pub enabled: Option<bool>,
    #[serde(alias = "maxEdge")]
    pub max_edge: Option<u32>,
    pub quality: Option<u8>,
    #[serde(alias = "thresholdKb")]
    pub threshold_kb: Option<u32>,
    #[serde(alias = "visionEnabled")]
    pub vision_enabled: Option<bool>,
    pub mode: Option<String>,
    #[serde(alias = "visionModel")]
    pub vision_model: Option<String>,
    #[serde(alias = "fallbackModels")]
    pub fallback_models: Option<Vec<String>>,
    #[serde(alias = "describePrompt")]
    pub describe_prompt: Option<String>,
}

/// 配置 → 规范 JSON（camelCase，与 chat/report settings 同款手构）。
fn settings_json(cfg: &crate::image_compat::ImageCompatConfig) -> Value {
    json!({
        "enabled": cfg.enabled,
        "maxEdge": cfg.max_edge,
        "quality": cfg.quality,
        "thresholdKb": cfg.threshold_kb,
        "visionEnabled": cfg.vision_enabled,
        "mode": cfg.mode,
        "visionModel": cfg.vision_model,
        "fallbackModels": cfg.fallback_models,
        "describePrompt": cfg.describe_prompt,
        // 边界值随响应下发：前端输入框直接用，避免前后端各写一套常量
        "maxEdgeMin": crate::image_compat::MAX_EDGE_MIN,
        "maxEdgeMax": crate::image_compat::MAX_EDGE_MAX,
        "qualityMin": crate::image_compat::QUALITY_MIN,
        "qualityMax": crate::image_compat::QUALITY_MAX,
        "thresholdKbMin": crate::image_compat::THRESHOLD_KB_MIN,
        "thresholdKbMax": crate::image_compat::THRESHOLD_KB_MAX,
        "defaultDescribePrompt": crate::image_compat::DEFAULT_DESCRIBE_PROMPT,
    })
}

/// GET /api/image-compat/settings —— 查询图片压缩与视觉兼容设置。
pub async fn handle_get_image_compat_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": settings_json(&config.image_compat),
    })))
}

/// PUT /api/image-compat/settings —— 保存图片压缩与视觉兼容设置（写盘 + 热生效）。
pub async fn handle_update_image_compat_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ImageCompatSettingsRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut config = verify_admin(&state, &headers).await?;
    if let Some(v) = body.enabled {
        config.image_compat.enabled = v;
    }
    if let Some(v) = body.max_edge {
        if !(crate::image_compat::MAX_EDGE_MIN..=crate::image_compat::MAX_EDGE_MAX).contains(&v) {
            return Err(error_response(
                &format!(
                    "长边上限必须在 {}~{} 之间（像素）",
                    crate::image_compat::MAX_EDGE_MIN,
                    crate::image_compat::MAX_EDGE_MAX
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
        config.image_compat.max_edge = v;
    }
    if let Some(v) = body.quality {
        if !(crate::image_compat::QUALITY_MIN..=crate::image_compat::QUALITY_MAX).contains(&v) {
            return Err(error_response(
                &format!(
                    "压缩质量必须在 {}~{} 之间",
                    crate::image_compat::QUALITY_MIN,
                    crate::image_compat::QUALITY_MAX
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
        config.image_compat.quality = v;
    }
    if let Some(v) = body.threshold_kb {
        if v > crate::image_compat::THRESHOLD_KB_MAX {
            return Err(error_response(
                &format!(
                    "触发阈值必须在 {}~{} KB 之间（0 = 每张都尝试压缩）",
                    crate::image_compat::THRESHOLD_KB_MIN,
                    crate::image_compat::THRESHOLD_KB_MAX
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
        config.image_compat.threshold_kb = v;
    }
    if let Some(v) = body.vision_enabled {
        config.image_compat.vision_enabled = v;
    }
    if let Some(v) = body.mode.as_deref() {
        let mode = v.trim();
        if !mode.eq_ignore_ascii_case(crate::image_compat::MODE_DESCRIBE)
            && !mode.eq_ignore_ascii_case(crate::image_compat::MODE_SWITCH)
        {
            return Err(error_response(
                &format!(
                    "模式只能是 '{}'（辅助识图）或 '{}'（切换视觉模型）",
                    crate::image_compat::MODE_DESCRIBE,
                    crate::image_compat::MODE_SWITCH
                ),
                StatusCode::BAD_REQUEST,
            ));
        }
        config.image_compat.mode = mode.to_ascii_lowercase();
    }
    if let Some(v) = body.vision_model.as_deref() {
        config.image_compat.vision_model = v.trim().to_string();
    }
    if let Some(v) = body.fallback_models {
        config.image_compat.fallback_models = v
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
    }
    if let Some(v) = body.describe_prompt {
        config.image_compat.describe_prompt = v;
    }
    // 组合校验：开启视觉兼容却没配视觉模型 = 配置无效（数据面会静默不生效，
    // 与其让运维以为开了，不如保存时直接拒绝）。
    if config.image_compat.vision_enabled && config.image_compat.vision_model.trim().is_empty() {
        return Err(error_response(
            "开启图片兼容时必须选择视觉模型",
            StatusCode::BAD_REQUEST,
        ));
    }
    // 生效值统一归一化后落盘（越界/脏值不写进 config.toml）
    config.image_compat = config.image_compat.normalized();
    let snapshot = config.image_compat.clone();
    match state.config_manager.update(config).await {
        Ok(()) => {
            // 保存即热生效：数据面每个请求读进程快照
            crate::image_compat::set_settings(snapshot.clone());
            let updated = state.config_manager.get().await;
            Ok(Json(json!({
                "success": true,
                "data": settings_json(&updated.image_compat),
            })))
        }
        Err(e) => Err(error_response(
            &format!("Failed to save image compat settings: {e}"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )),
    }
}
