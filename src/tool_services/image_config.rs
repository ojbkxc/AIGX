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
//! AIGX 没有 `supports_image_generation` / `supports_image_editing` 两个能力位，
//! 本模块用模型元信息里的**能力标签**（`model::metadata` 的内置推断 /
//! models.dev / 管理员覆盖三档合并结果）代替：`image` → 生图，
//! `image_editing` / `image_edit` → 参考图编辑；未声明编辑标签时回退为生图
//! 能力（保持旧行为，宁放勿误伤，见 [`catalog`]）。账号校验对照账号池
//! （`AccountSessionStore`）中 provider 匹配且启用的账号；`account_id` 为空
//! 时不强制存在账号（渠道型 provider 没有账号，账号型 provider 由
//! `AccountBridge` 按优先级选号，见 image_runtime 的偏离说明）。
//!
//! 目录快照由 [`catalog`] 从 `api::admin::model_manage::current_view`（与
//! 「统一模型管理」页同一份聚合视图）+ 账号池构造，读写接口共用。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::ToolServices;
use crate::api::openai::AppState;

/// 图片操作（上游 `providers::images::ImageOperation` 的 AIGX 对应物）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageOperation {
    Generate,
    Edit,
}

impl ImageOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::Edit => "edit",
        }
    }
}

/// 管理面「可选模型」一行。
#[derive(Clone, Debug, Serialize)]
pub struct ImageModelRow {
    pub id: String,
    pub provider: String,
    pub provider_label: String,
    pub enabled: bool,
    pub generation: bool,
    pub editing: bool,
}

/// 管理面「可选账号」一行。
#[derive(Clone, Debug, Serialize)]
pub struct ImageAccountRow {
    pub id: String,
    pub name: String,
    pub provider: String,
    pub enabled: bool,
}

/// 目标校验用的目录快照（与上游 `catalog::manage_view` + 账号池同职责）。
#[derive(Clone, Debug, Default)]
pub struct ImageCatalog {
    pub models: Vec<ImageModelRow>,
    pub accounts: Vec<ImageAccountRow>,
}

/// 构造目录快照：模型来自统一模型管理视图（能力标签含 `image` 或编辑标签且
/// 启用），账号来自账号池（启用 + provider 命中任一图片模型）。
///
/// 能力位来源：`image` 标签 = 生图；`image_editing` / `image_edit` 标签 =
/// 参考图编辑。AIGX 的模型能力白名单目前没有独立编辑标签，因此**未声明编辑
/// 标签时回退为生图能力**（与旧行为一致，宁放勿误伤）；一旦模型元信息以后
/// 能声明独立编辑能力，这里无需再改。上游用 `supports_image_generation` /
/// `supports_image_editing` 两个 bool，本函数是它们在 AIGX 的对应物。
pub fn catalog(state: &AppState) -> ImageCatalog {
    let view = crate::api::admin::model_manage::current_view(state);
    let has_tag = |capabilities: &[String], tag: &str| {
        capabilities
            .iter()
            .any(|capability| capability.eq_ignore_ascii_case(tag))
    };
    let models: Vec<ImageModelRow> = view
        .models
        .into_iter()
        .filter(|model| {
            model.enabled
                && (has_tag(&model.capabilities, "image")
                    || has_tag(&model.capabilities, "image_editing")
                    || has_tag(&model.capabilities, "image_edit"))
        })
        .map(|model| {
            let generation = has_tag(&model.capabilities, "image");
            // 独立编辑标签优先；没有则回退到生图能力（保持旧行为）。
            let editing = generation
                || has_tag(&model.capabilities, "image_editing")
                || has_tag(&model.capabilities, "image_edit");
            ImageModelRow {
                id: model.id,
                provider: model.provider,
                provider_label: model.provider_label,
                enabled: model.enabled,
                generation,
                editing,
            }
        })
        .collect();
    let providers: std::collections::HashSet<&str> =
        models.iter().map(|model| model.provider.as_str()).collect();
    let accounts: Vec<ImageAccountRow> = state
        .account_sessions
        .all()
        .into_iter()
        .filter(|session| {
            session.status == "enabled" && providers.contains(session.provider.as_str())
        })
        .map(|session| ImageAccountRow {
            name: if session.name.is_empty() {
                session.id.clone()
            } else {
                session.name
            },
            provider: session.provider,
            id: session.id,
            enabled: true,
        })
        .collect();
    ImageCatalog { models, accounts }
}

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

/// 形状校验（非空 + 能力标签），上游 `validate_target` 的 AIGX 对应物。
///
/// `catalog` 为空（尚未配置渠道/账号）时只做非空校验 —— 与上游「配置保存
/// 失败」方向相反，这里选择**放行形状**、由运行时返回可读的 no-bridge
/// 错误：AIGX 的渠道可能后于工具服务配置，严格拒绝会让首次配置无法落盘。
pub fn validate_target(
    catalog: &ImageCatalog,
    target: &ImageTarget,
    operation: ImageOperation,
) -> Result<(), String> {
    let label = match operation {
        ImageOperation::Generate => "生图",
        ImageOperation::Edit => "参考图编辑",
    };
    if target.provider.trim().is_empty() || target.model.trim().is_empty() {
        return Err(invalid(format!("请选择{label}提供商与模型")));
    }
    if catalog.models.is_empty() {
        return Ok(());
    }
    let model = catalog.models.iter().find(|model| {
        model.provider == target.provider
            && model.id == target.model
            && model.enabled
            && match operation {
                ImageOperation::Generate => model.generation,
                ImageOperation::Edit => model.editing,
            }
    });
    if model.is_none() {
        return Err(invalid(
            "所选图片模型已停用或不支持该能力，请检查渠道、账号与模型目录",
        ));
    }
    if !target.account_id.is_empty()
        && !catalog
            .accounts
            .iter()
            .any(|account| account.id == target.account_id && account.provider == target.provider)
    {
        return Err(invalid("所选生图账号不存在、已停用或与所选提供商不匹配"));
    }
    Ok(())
}

/// 运行前校验（与保存时同一口径；返回可读配置错误）。
pub fn validate_target_or_error(
    catalog: &ImageCatalog,
    target: &ImageTarget,
    operation: ImageOperation,
) -> Result<(), super::image_jobs::ImageToolError> {
    validate_target(catalog, target, operation).map_err(|message| {
        super::image_jobs::ImageToolError::new(400, "image_configuration", message)
    })
}

/// 取某个操作的目标（编辑未启用 → `image_configuration`）。
pub fn target(
    config: &ImageConfig,
    operation: ImageOperation,
) -> Result<&ImageTarget, super::image_jobs::ImageToolError> {
    match operation {
        ImageOperation::Generate => Ok(&config.generation),
        ImageOperation::Edit => config.editing.as_ref().ok_or_else(|| {
            super::image_jobs::ImageToolError::new(400, "image_configuration", "尚未启用参考图编辑")
        }),
    }
}

pub fn save(
    services: &ToolServices,
    catalog: &ImageCatalog,
    mut config: ImageConfig,
) -> Result<Value, String> {
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
    validate_target(catalog, &config.generation, ImageOperation::Generate)?;
    if let Some(editing) = &config.editing {
        validate_target(catalog, editing, ImageOperation::Edit)?;
    }
    services.update(|value| value.image_generation = Some(config))?;
    Ok(management_view(services, catalog))
}

pub fn clear(services: &ToolServices) -> Result<(), String> {
    services
        .update(|value| value.image_generation = None)
        .map(|_| ())
}

/// 管理视图：配置 + 默认值 + 校验错误 + 可选模型/账号 + 服务超时。
pub fn management_view(services: &ToolServices, catalog: &ImageCatalog) -> Value {
    let config = load(services);
    let generation_error = config.as_ref().and_then(|value| {
        validate_target(catalog, &value.generation, ImageOperation::Generate).err()
    });
    let editing_error = config
        .as_ref()
        .and_then(|value| value.editing.as_ref())
        .and_then(|value| validate_target(catalog, value, ImageOperation::Edit).err());
    json!({
        "config": config,
        "defaults": ImageConfig::default(),
        "models": &catalog.models,
        "accounts": &catalog.accounts,
        "generation_error": generation_error,
        "editing_error": editing_error,
        "timeout_seconds": super::timeout_seconds(services, super::IMAGE_GENERATION_SERVICE_ID),
        "runtime_available": true,
        "runtime_note": "任务/文件面/MCP/本地桥接脚本已接线；模型能力用标签表达（image → 生图，image_editing/image_edit → 编辑，未声明编辑标签时回退为生图；对应上游 supports_image_generation/supports_image_editing）；MCP 生图使用管理员账号池，不写网关请求日志、不对网关 Key/用户计费（上游经内部 images 管线计用量）",
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

    fn sample_config() -> ImageConfig {
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

    fn sample_catalog() -> ImageCatalog {
        ImageCatalog {
            models: vec![
                ImageModelRow {
                    id: "glm-image".into(),
                    provider: "workbuddy".into(),
                    provider_label: "WorkBuddy".into(),
                    enabled: true,
                    generation: true,
                    editing: true,
                },
                ImageModelRow {
                    id: "dall-e-3".into(),
                    provider: "openai_compatible".into(),
                    provider_label: "OpenAI 兼容".into(),
                    enabled: true,
                    generation: true,
                    editing: true,
                },
            ],
            accounts: vec![ImageAccountRow {
                id: "acc-1".into(),
                name: "WorkBuddy A".into(),
                provider: "workbuddy".into(),
                enabled: true,
            }],
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
        let cat = sample_catalog();
        let mut bad = sample_config();
        bad.max_images = 11;
        assert!(save(&svc, &cat, bad).is_err());
        let mut bad = sample_config();
        bad.generation.provider = "  ".into();
        assert!(save(&svc, &cat, bad).is_err());
        let mut bad = sample_config();
        bad.editing = Some(ImageTarget::default());
        assert!(save(&svc, &cat, bad).is_err());
    }

    /// 上游语义校验：模型必须在目录里且带 image 能力、账号必须存在。
    #[test]
    fn save_rejects_uncatalogued_model_and_account() {
        let (svc, _dir) = services();
        let cat = sample_catalog();
        let mut bad = sample_config();
        bad.generation.model = "text-embedding-3-small".into();
        let error = save(&svc, &cat, bad).unwrap_err();
        assert!(error.contains("模型已停用或不支持"));

        let mut bad = sample_config();
        bad.generation.account_id = "missing-account".into();
        let error = save(&svc, &cat, bad).unwrap_err();
        assert!(error.contains("账号不存在"));

        // 空目录：只做形状校验（首次配置可落盘）
        let empty = ImageCatalog::default();
        assert!(save(&svc, &empty, sample_config()).is_ok());
    }

    #[test]
    fn save_and_clear_roundtrip() {
        let (svc, _dir) = services();
        let cat = sample_catalog();
        let view = save(&svc, &cat, sample_config()).unwrap();
        assert_eq!(view["config"]["max_images"], 2);
        assert_eq!(view["runtime_available"], true);
        assert_eq!(view["timeout_seconds"], 300);
        assert_eq!(view["models"].as_array().unwrap().len(), 2);
        assert_eq!(view["accounts"].as_array().unwrap().len(), 1);
        clear(&svc).unwrap();
        assert!(load(&svc).is_none());
    }

    /// 生成 / 编辑能力位分别生效：只有生图能力的模型不能用于编辑
    /// （`catalog` 里 `image_editing` 标签缺失时回退为生图，这里验证
    /// `validate_target` 确实按行上的 `editing` 位判定）。
    #[test]
    fn validate_target_honours_separate_editing_capability() {
        let catalog = ImageCatalog {
            models: vec![ImageModelRow {
                id: "gen-only".into(),
                provider: "workbuddy".into(),
                provider_label: "WorkBuddy".into(),
                enabled: true,
                generation: true,
                editing: false,
            }],
            accounts: Vec::new(),
        };
        let target = ImageTarget {
            provider: "workbuddy".into(),
            model: "gen-only".into(),
            account_id: String::new(),
        };
        assert!(validate_target(&catalog, &target, ImageOperation::Generate).is_ok());
        let error = validate_target(&catalog, &target, ImageOperation::Edit).unwrap_err();
        assert!(error.contains("不支持该能力"), "{error}");
    }
}
