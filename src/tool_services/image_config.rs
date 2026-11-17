// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成服务的配置面（`ToolConfig.image_generation`）。
//!
//! ── 本波范围 ────────────────────────────────────────────────
//! 上游的图片生成还有一整套运行时（任务表 `image_jobs`、文件面
//! `tool_images` / `tool_image_results`、MCP `image_mcp`、本地桥接
//! `image_bridge.cjs`）。AIGX 当前没有可路由的生图账号/模型目录接口，
//! 本波只移植**配置形状 + 管理视图**，让前端「工具服务」页能完整读写，
//! 并在报告里明确「生图运行时未移植」。
//!
//! ── 与上游的差异 ────────────────────────────────────────────
//! 上游保存时会校验「提供商 × 模型 × 账号」在模型目录/账号池里真实可用；
//! AIGX 的模型目录（T5）与账号池（T4）正在并行改造，本模块只做**形状校验**
//! （非空、数量/并发/保留期/存储上限），把目录校验留到运行时接入时补。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ToolServices;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageTarget {
    pub provider: String,
    pub model: String,
    pub account_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageConfig {
    pub generation: ImageTarget,
    pub editing: Option<ImageTarget>,
    pub max_images: u32,
    pub max_concurrency: u32,
    pub retention_hours: u32,
    pub storage_mb: u32,
}

impl Default for ImageConfig {
    fn default() -> Self {
        Self {
            generation: ImageTarget::default(),
            editing: None,
            max_images: 1,
            max_concurrency: 2,
            retention_hours: 24,
            storage_mb: 512,
        }
    }
}

pub(super) fn deserialize_stored<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ImageConfig>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// 读取并做形状校验（越界视为「未配置」，与上游 `load()` 同口径）。
pub fn load(services: &ToolServices) -> Option<ImageConfig> {
    let config = services.load().image_generation?;
    if !(1..=10).contains(&config.max_images)
        || !(1..=8).contains(&config.max_concurrency)
        || !(1..=168).contains(&config.retention_hours)
        || !(64..=4096).contains(&config.storage_mb)
    {
        return None;
    }
    Some(config)
}

fn invalid(message: impl Into<String>) -> String {
    message.into()
}

fn validate_target(target: &ImageTarget, label: &str) -> Result<(), String> {
    if target.provider.trim().is_empty() || target.model.trim().is_empty() {
        return Err(invalid(format!("请选择{label}提供商与模型")));
    }
    Ok(())
}

pub fn save(services: &ToolServices, mut config: ImageConfig) -> Result<Value, String> {
    for item in std::iter::once(&mut config.generation).chain(config.editing.iter_mut()) {
        item.provider = item.provider.trim().to_string();
        item.model = item.model.trim().to_string();
        item.account_id = item.account_id.trim().to_string();
    }
    if !(1..=10).contains(&config.max_images)
        || !(1..=8).contains(&config.max_concurrency)
        || !(1..=168).contains(&config.retention_hours)
        || !(64..=4096).contains(&config.storage_mb)
    {
        return Err(invalid(
            "数量须为 1–10，并发须为 1–8，保留时间须为 1–168 小时，存储须为 64–4096 MiB",
        ));
    }
    validate_target(&config.generation, "生图")?;
    if let Some(editing) = &config.editing {
        validate_target(editing, "参考图编辑")?;
    }
    services.update(|value| value.image_generation = Some(config))?;
    Ok(management_view(services))
}

pub fn clear(services: &ToolServices) -> Result<(), String> {
    services
        .update(|value| value.image_generation = None)
        .map(|_| ())
}

/// 管理视图：配置 + 默认值 + 形状校验错误 + 服务超时。
///
/// `models` / `accounts` 两个下拉数组本波恒为空（模型目录/账号池未接线），
/// 前端按空数组渲染「暂不可用」即可，不需要为空值特判。
pub fn management_view(services: &ToolServices) -> Value {
    let config = load(services);
    let generation_error = config
        .as_ref()
        .and_then(|value| validate_target(&value.generation, "生图").err());
    let editing_error = config
        .as_ref()
        .and_then(|value| value.editing.as_ref())
        .and_then(|value| validate_target(value, "参考图编辑").err());
    json!({
        "config": config,
        "defaults": ImageConfig::default(),
        "models": [],
        "accounts": [],
        "generation_error": generation_error,
        "editing_error": editing_error,
        "timeout_seconds": super::timeout_seconds(services, super::IMAGE_GENERATION_SERVICE_ID),
        "runtime_available": false,
        "runtime_note": "生图运行时（任务/文件面/MCP/本地桥接）未移植；本接口仅提供配置面",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn services() -> (ToolServices, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        (ToolServices::new(store), dir)
    }

    fn target() -> ImageConfig {
        ImageConfig {
            generation: ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            },
            editing: None,
            max_images: 2,
            max_concurrency: 2,
            retention_hours: 24,
            storage_mb: 512,
        }
    }

    #[test]
    fn defaults_and_load_bounds() {
        let (svc, _dir) = services();
        assert!(load(&svc).is_none());
        let defaults = ImageConfig::default();
        assert_eq!(defaults.max_images, 1);
        assert_eq!(defaults.storage_mb, 512);
    }

    #[test]
    fn save_rejects_out_of_range_and_empty_target() {
        let (svc, _dir) = services();
        let mut bad = target();
        bad.max_images = 11;
        assert!(save(&svc, bad).is_err());
        let mut bad = target();
        bad.generation.provider = "  ".into();
        assert!(save(&svc, bad).is_err());
        let mut bad = target();
        bad.editing = Some(ImageTarget::default());
        assert!(save(&svc, bad).is_err());
    }

    #[test]
    fn save_and_clear_roundtrip() {
        let (svc, _dir) = services();
        let view = save(&svc, target()).unwrap();
        assert_eq!(view["config"]["max_images"], 2);
        assert_eq!(view["runtime_available"], false);
        assert_eq!(view["timeout_seconds"], 300);
        clear(&svc).unwrap();
        assert!(load(&svc).is_none());
    }
}
