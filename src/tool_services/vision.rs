// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 视觉工具服务：配置「提供商 + 上游模型 + 思考等级 + 内部调用 Key」，
//! 运行时用内部 Key 打本进程网关完成推理。
//!
//! ── 为什么走网关回环而不是直接转发 ──────────────────────────
//! 与上游视觉服务同一做法：协议转换、出站脱敏、重试、故障转移与请求日志
//! 全部复用网关既有管线，视觉服务本身只负责「把图片与提示词组装成一条
//! OpenAI Chat 请求」。调用方传入的 http(s) 图片会先抓下来转成 data URI，
//! 保证只认 base64 的 Anthropic 上游也能收到图片；抓取本身也做公网校验
//! （复用搜索抓取的白名单口径），并限制大小与超时。
//!
//! ── 额度与限制记在哪 ────────────────────────────────────────
//! 记在配置里选中的那把网关 Key 上（与普通客户端请求同一口径）。Key 被
//! 删除 / 停用后，详情页会明确提示「已失效」，运行时返回可读错误而不是
//! 静默失败。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::auth::{ApiKey, ApiKeyStore};

use super::ToolServices;

/// 客户端配置 MCP 时使用的服务名（同时作为 package_name 展示）。
pub const VISION_SERVICE_NAME: &str = "vision";
pub const VISION_SERVICE_VERSION: &str = "1.0.0";

/// 入参限制：与网络搜索 MCP 的批量上限保持一致。
pub const VISION_MAX_IMAGES: usize = 10;
pub const VISION_MAX_PROMPT_LENGTH: usize = 16_000;
/// 单张 URL 图片抓取的大小与超时上限。
const VISION_IMAGE_FETCH_LIMIT_BYTES: usize = 20 * 1024 * 1024;
const VISION_IMAGE_FETCH_TIMEOUT_MS: u64 = 30_000;

/// 思考等级白名单（空串 = 不传，交给映射上的思考覆盖）。
pub const REASONING_LEVELS: [&str; 4] = ["minimal", "low", "medium", "high"];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VisionConfig {
    /// 提供商 id（如 `workbuddy` / `custom-xxx`）
    pub provider: String,
    /// 提供商展示名快照
    pub provider_label: String,
    /// 上游模型 id（目录里的原始 id）
    pub upstream_model: String,
    /// 请求网关时使用的对外模型名（保存时确定，运行时不再解析）
    pub downstream_model: String,
    /// 思考等级；空 = 不传，交给映射上的思考覆盖
    pub reasoning: String,
    /// 内部调用网关使用的网关 Key id
    pub api_key_id: String,
    /// Key 名称快照：Key 被删除后详情页仍能提示是哪个 Key 失效
    pub api_key_name: String,
}

pub fn is_reasoning_level(value: &str) -> bool {
    value.is_empty() || REASONING_LEVELS.contains(&value)
}

/// 模型目录里的一行（视觉保存/解析校验用）。
///
/// 由 API 层从模型管理视图投影（`model::manage::build_view`），
/// `tool_services` 因此不依赖渠道 / 账号存储，纯逻辑可单测。
#[derive(Clone, Debug)]
pub struct VisionCatalogModel {
    pub provider: String,
    pub provider_label: String,
    pub id: String,
    /// 目录行是否启用（上游 `manage_view.models[].enabled`）。
    pub enabled: bool,
    /// 能力三态：`None` = 目录未声明（与上游「能力未声明 ≠ 支持」同口径）。
    pub supports_images: Option<bool>,
    /// 目录把该模型标成图片生成（上游 `api/tools.rs` 的 `kind != "image"` 口径）。
    pub image_model: bool,
}

/// 校验并解析视觉目标。
///
/// 语义基准 = 上游 `api/tools.rs` 的保存路径 + `api/vision_compat.rs` 的
/// `validate_available`：provider+model 必须在模型目录中、该行已启用、
/// 不是图片生成模型、且 `supportsImages = true`；返回
/// `(provider 展示名, 匹配的目录模型 id)`，后者用作 `downstream_model`。
pub fn resolve_vision_target(
    models: &[VisionCatalogModel],
    provider: &str,
    upstream_model: &str,
) -> Result<(String, String), String> {
    let matched = models
        .iter()
        .find(|item| item.provider == provider && item.id == upstream_model)
        .ok_or_else(|| "该提供商下没有这个模型，请先在模型管理页确认".to_string())?;
    if !matched.enabled {
        return Err("所选视觉模型尚未启用，请先在模型管理中启用".into());
    }
    if matched.image_model {
        return Err("图片生成模型不能作为视觉模型，请选择支持图片输入的对话模型".into());
    }
    match matched.supports_images {
        Some(true) => {
            let label = if matched.provider_label.trim().is_empty() {
                provider.to_string()
            } else {
                matched.provider_label.clone()
            };
            Ok((label, matched.id.clone()))
        }
        Some(false) => Err("所选模型未声明支持图片输入，请选择视觉模型".into()),
        None => Err("所选模型未声明图片能力，请先在模型管理中确认支持图片".into()),
    }
}

/// 网关 Key 是否可用（A2A `is_usable` 的 AIGX 口径：启用且未过期）。
pub fn is_usable_key(key: &ApiKey) -> bool {
    key.is_enabled() && !key.is_expired()
}

/// 掩码（`sk-…abcd` 形态；不泄露完整密钥）。
pub fn mask_key(key: &str) -> String {
    let visible: String = key.chars().filter(|ch| *ch != '-').collect();
    if visible.chars().count() <= 8 {
        return "••••".to_string();
    }
    let head: String = visible.chars().take(4).collect();
    let tail: String = visible
        .chars()
        .skip(visible.chars().count().saturating_sub(4))
        .collect();
    format!("{head}••••{tail}")
}

/// 管理端 Key 下拉选项：仅列出可用的网关 Key（启用且未过期）。
pub fn vision_key_options(keys: &ApiKeyStore) -> Vec<Value> {
    keys.list()
        .into_iter()
        .filter(is_usable_key)
        .map(|entry| {
            json!({
                "id": entry.id,
                "name": entry.name,
                "key_prefix": mask_key(&entry.key),
            })
        })
        .collect()
}

fn key_available(keys: &ApiKeyStore, key_id: &str) -> bool {
    keys.list()
        .into_iter()
        .find(|entry| entry.id == key_id)
        .is_some_and(|entry| is_usable_key(&entry))
}

/// 对外（管理端）展示的配置：不返回任何 Key 密文；附当前 Key 的可用性。
pub fn vision_config(services: &ToolServices, keys: &ApiKeyStore) -> Option<Value> {
    let config = services.load().vision?;
    let available = key_available(keys, &config.api_key_id);
    let mut value = serde_json::to_value(&config).unwrap_or(Value::Null);
    value["configured"] = json!(true);
    value["key_available"] = json!(available);
    Some(value)
}

pub fn save_vision_config(
    services: &ToolServices,
    keys: &ApiKeyStore,
    config: VisionConfig,
    catalog: &[VisionCatalogModel],
) -> Result<Value, String> {
    if config.provider.trim().is_empty() || config.upstream_model.trim().is_empty() {
        return Err("请选择提供商与上游模型".into());
    }
    if !is_reasoning_level(config.reasoning.trim()) {
        return Err("思考等级无效".into());
    }
    // 与上游一致：目录校验先于保存，provider + model 必须是「这家、这个
    // 已启用、支持图片输入的模型」，downstream_model 由匹配到的目录 id
    // 决定（请求体里的值不再生效，防止旧配置写进未知模型）。
    let provider = config.provider.trim().to_string();
    let upstream_model = config.upstream_model.trim().to_string();
    let (catalog_label, model_id) = resolve_vision_target(catalog, &provider, &upstream_model)?;
    if config.api_key_id.trim().is_empty() {
        return Err("请选择内部调用 API Key".into());
    }
    let available = keys
        .list()
        .into_iter()
        .find(|entry| entry.id == config.api_key_id)
        .ok_or("API Key 不存在")?;
    if !is_usable_key(&available) {
        return Err("API Key 已停用或不可用，请重新选择".into());
    }
    let mut config = config;
    config.provider = provider;
    // 展示名以模型目录为准（上游保存路径不采用请求体里的快照）。
    config.provider_label = catalog_label;
    config.upstream_model = upstream_model;
    config.reasoning = config.reasoning.trim().to_string();
    config.downstream_model = model_id;
    config.api_key_name = available.name.clone();
    services.update(|value| value.vision = Some(config.clone()))?;
    vision_config(services, keys).ok_or_else(|| "视觉配置保存后读取失败".into())
}

pub fn clear_vision_config(services: &ToolServices) {
    let _ = services.update(|value| value.vision = None);
}

/* ─── 入参校验 ─────────────────────────────────────────────── */

fn normalize_image_ref(value: &Value) -> String {
    let Some(text) = value.as_str().map(str::trim) else {
        return String::new();
    };
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return text.to_string();
    }
    if lower.starts_with("data:image/") && lower.contains(";base64,") {
        return text.to_string();
    }
    String::new()
}

/// 解析并校验 MCP 工具入参。
pub fn parse_vision_args(args: &Value) -> Result<(Vec<String>, String), String> {
    let images = args.get("images").and_then(Value::as_array);
    let Some(images) = images else {
        return Err(format!(
            "images must contain between 1 and {VISION_MAX_IMAGES} image references"
        ));
    };
    if images.is_empty() || images.len() > VISION_MAX_IMAGES {
        return Err(format!(
            "images must contain between 1 and {VISION_MAX_IMAGES} image references"
        ));
    }
    let normalized: Vec<String> = images.iter().map(normalize_image_ref).collect();
    if normalized.iter().any(String::is_empty) {
        return Err("each image must be an http(s) URL or an image data URI".into());
    }
    let prompt = args
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if prompt.is_empty() {
        return Err("prompt must be a non-empty string".into());
    }
    if prompt.chars().count() > VISION_MAX_PROMPT_LENGTH {
        return Err(format!(
            "prompt must be at most {VISION_MAX_PROMPT_LENGTH} characters"
        ));
    }
    Ok((normalized, prompt))
}

/* ─── 运行 ─────────────────────────────────────────────────── */

/// 抓取 URL 图片并转成 data URI。
///
/// SSRF 防护：只接受公网 http/https（与搜索抓取同一白名单），抓取客户端
/// 禁止跟随重定向（重定向目标不再过一次白名单），大小按流式累计上限。
async fn fetch_image_as_data_uri(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|_| "Invalid image URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Image must be an http(s) URL".into());
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| "Invalid image URL".to_string())?;
    if !super::normalize::is_public_web_fetch_host(host) {
        return Err("Image URL must point to a public host".into());
    }
    let response = client
        .get(url)
        .timeout(Duration::from_millis(VISION_IMAGE_FETCH_TIMEOUT_MS))
        .header("user-agent", "AIGX-VisionTool/1.0")
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "Image fetch timed out (30s)".to_string()
            } else {
                format!("Image fetch failed: {error}")
            }
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "Image fetch failed with HTTP {}",
            response.status().as_u16()
        ));
    }
    let mime = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if !mime.starts_with("image/") {
        return Err("The fetched URL does not return an image".into());
    }
    if let Some(length) = response.content_length() {
        if length as usize > VISION_IMAGE_FETCH_LIMIT_BYTES {
            return Err("Image exceeds the 20MB limit".into());
        }
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("Image fetch failed: {error}"))?;
    if bytes.len() > VISION_IMAGE_FETCH_LIMIT_BYTES {
        return Err("Image exceeds the 20MB limit".into());
    }
    use base64::Engine as _;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// 运行一次视觉分析：组消息 → 抓 URL 图 → 调本进程网关 → 取回复文本。
pub async fn run_vision(
    services: &ToolServices,
    keys: &ApiKeyStore,
    client: &reqwest::Client,
    base_url: &str,
    images: Vec<String>,
    prompt: String,
) -> Result<Value, String> {
    let config = services
        .load()
        .vision
        .ok_or("Vision model is not configured; ask the administrator to configure it")?;
    if config.downstream_model.is_empty() {
        return Err("Vision model is not configured; ask the administrator to configure it".into());
    }
    let key = keys
        .list()
        .into_iter()
        .find(|entry| entry.id == config.api_key_id && is_usable_key(entry))
        .ok_or(
            "The configured API Key is unavailable (deleted or disabled); re-save the vision config",
        )?;

    let images = futures::future::join_all(images.into_iter().map(|reference| async move {
        if reference.to_ascii_lowercase().starts_with("data:") {
            Ok(reference)
        } else {
            fetch_image_as_data_uri(client, &reference).await
        }
    }))
    .await;
    let mut content = vec![json!({ "type": "text", "text": prompt })];
    for image in images {
        content.push(json!({ "type": "image_url", "image_url": { "url": image? } }));
    }

    let mut body = json!({
        "model": config.downstream_model,
        "messages": [{ "role": "user", "content": content }],
        "stream": false,
    });
    if !config.reasoning.is_empty() {
        body["reasoning_effort"] = json!(config.reasoning);
    }

    let timeout = super::timeout_seconds(services, super::VISION_SERVICE_ID);
    let response = client
        .post(format!(
            "{}/v1/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .timeout(Duration::from_secs(timeout))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", key.key))
        .header("x-aigx-source", "vision")
        .header("user-agent", "AIGX-VisionTool/1.0")
        .json(&body)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                format!("Vision model request timed out ({timeout}s)")
            } else {
                "Unable to reach the AIGX gateway".to_string()
            }
        })?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let data: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if !status.is_success() {
        let message = data
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| data.get("message").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| {
                if text.is_empty() {
                    format!("gateway returned HTTP {}", status.as_u16())
                } else {
                    text.chars().take(500).collect()
                }
            });
        return Err(message);
    }
    let reply = data
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if reply.is_empty() {
        return Err("The vision model returned an empty response".into());
    }
    Ok(json!({
        "reply": reply,
        "model": data.get("model").cloned().unwrap_or(json!(config.upstream_model)),
        "usage": data.get("usage").cloned().unwrap_or(Value::Null),
    }))
}

/// 管理端「测试连接」：用当前配置跑一次最小视觉请求（1x1 PNG）。
pub async fn test_vision(
    services: &ToolServices,
    keys: &ApiKeyStore,
    client: &reqwest::Client,
    base_url: &str,
) -> Value {
    let started = super::now_ms();
    let result = run_vision(
        services,
        keys,
        client,
        base_url,
        vec!["data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==".into()],
        "Reply with the single word: ok".into(),
    )
    .await;
    let duration = super::now_ms() - started;
    match result {
        Ok(payload) => json!({
            "success": true,
            "duration_ms": duration,
            "reply": payload.get("reply").and_then(Value::as_str).unwrap_or("").chars().take(500).collect::<String>(),
        }),
        Err(error) => json!({
            "success": false,
            "duration_ms": duration,
            "error": error,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn harness() -> (ToolServices, ApiKeyStore, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(crate::storage::FileStore::new(dir.path().to_path_buf()));
        let keys = ApiKeyStore::new(store.clone(), dir.path().to_path_buf());
        (ToolServices::new(store), keys, dir)
    }

    fn config(api_key_id: &str) -> VisionConfig {
        VisionConfig {
            provider: "workbuddy".into(),
            provider_label: "WorkBuddy".into(),
            upstream_model: "glm-5v".into(),
            downstream_model: String::new(),
            reasoning: "low".into(),
            api_key_id: api_key_id.into(),
            api_key_name: String::new(),
        }
    }

    fn config_with_model(api_key_id: &str, model: &str) -> VisionConfig {
        let mut value = config(api_key_id);
        value.upstream_model = model.to_string();
        value
    }

    /// 模型目录桩：`glm-5v` 是唯一「已启用 + 支持图片」的对话模型。
    fn vision_catalog() -> Vec<VisionCatalogModel> {
        vec![
            VisionCatalogModel {
                provider: "workbuddy".into(),
                provider_label: "WorkBuddy 国内版".into(),
                id: "glm-5v".into(),
                enabled: true,
                supports_images: Some(true),
                image_model: false,
            },
            VisionCatalogModel {
                provider: "workbuddy".into(),
                provider_label: "WorkBuddy 国内版".into(),
                id: "text-only".into(),
                enabled: true,
                supports_images: Some(false),
                image_model: false,
            },
            VisionCatalogModel {
                provider: "workbuddy".into(),
                provider_label: "WorkBuddy 国内版".into(),
                id: "img-gen".into(),
                enabled: true,
                supports_images: None,
                image_model: true,
            },
            VisionCatalogModel {
                provider: "workbuddy".into(),
                provider_label: "WorkBuddy 国内版".into(),
                id: "disabled-vision".into(),
                enabled: false,
                supports_images: Some(true),
                image_model: false,
            },
        ]
    }

    #[test]
    fn reasoning_levels_validation() {
        assert!(is_reasoning_level(""));
        assert!(is_reasoning_level("low"));
        assert!(is_reasoning_level("high"));
        assert!(!is_reasoning_level("ultra"));
        assert!(!is_reasoning_level("HIGH"));
    }

    #[test]
    fn save_rejects_missing_fields_and_unknown_key() {
        let (services, keys, _dir) = harness();
        let mut bad = config("");
        assert!(save_vision_config(&services, &keys, bad.clone(), &vision_catalog()).is_err());
        bad = config("no-such-key");
        assert!(save_vision_config(&services, &keys, bad.clone(), &vision_catalog()).is_err());
        bad = config("");
        bad.provider = String::new();
        assert!(save_vision_config(&services, &keys, bad, &vision_catalog()).is_err());
    }

    #[test]
    fn save_sets_downstream_model_and_key_snapshot() {
        let (services, keys, _dir) = harness();
        let key = keys.generate("vision-internal").unwrap();
        let saved =
            save_vision_config(&services, &keys, config(&key.id), &vision_catalog()).unwrap();
        assert_eq!(saved["downstreamModel"], "glm-5v");
        assert_eq!(saved["apiKeyName"], "vision-internal");
        assert_eq!(saved["configured"], true);
        // provider_label 来自模型目录（请求体里的快照不再作为权威）
        assert_eq!(saved["providerLabel"], "WorkBuddy 国内版");
        // 与上游同口径：动态补充的两个字段用 snake_case
        assert_eq!(saved["key_available"], true);
        // 清除
        clear_vision_config(&services);
        assert!(vision_config(&services, &keys).is_none());
    }

    /// 目录里不存在的模型 → 拒绝（下游模型不能靠请求体自说自话）。
    #[test]
    fn save_rejects_unknown_model() {
        let (services, keys, _dir) = harness();
        let key = keys.generate("vision-internal").unwrap();
        let error = save_vision_config(
            &services,
            &keys,
            config_with_model(&key.id, "ghost-model"),
            &vision_catalog(),
        )
        .unwrap_err();
        assert!(error.contains("没有这个模型"), "{error}");
        let mut foreign = config(&key.id);
        foreign.provider = "other-provider".into();
        assert!(save_vision_config(&services, &keys, foreign, &vision_catalog()).is_err());
    }

    /// 非视觉模型（能力位 false）/ 图片生成模型 / 未启用行 → 拒绝。
    #[test]
    fn save_rejects_non_vision_and_disabled_models() {
        let (services, keys, _dir) = harness();
        let key = keys.generate("vision-internal").unwrap();
        let error = save_vision_config(
            &services,
            &keys,
            config_with_model(&key.id, "text-only"),
            &vision_catalog(),
        )
        .unwrap_err();
        assert!(error.contains("未声明支持图片"), "{error}");
        let error = save_vision_config(
            &services,
            &keys,
            config_with_model(&key.id, "img-gen"),
            &vision_catalog(),
        )
        .unwrap_err();
        assert!(error.contains("图片生成模型"), "{error}");
        let error = save_vision_config(
            &services,
            &keys,
            config_with_model(&key.id, "disabled-vision"),
            &vision_catalog(),
        )
        .unwrap_err();
        assert!(error.contains("尚未启用"), "{error}");
    }

    /// 接受视觉模型并以匹配到的目录 id 填 downstream_model（请求体给的值被覆盖）。
    #[test]
    fn save_accepts_vision_model_and_fills_downstream_id() {
        let (services, keys, _dir) = harness();
        let key = keys.generate("vision-internal").unwrap();
        let mut requested = config(&key.id);
        requested.downstream_model = "spoofed-model".into();
        let saved = save_vision_config(&services, &keys, requested, &vision_catalog()).unwrap();
        assert_eq!(saved["downstreamModel"], "glm-5v");
    }

    #[test]
    fn disabled_key_is_not_usable() {
        let (services, keys, _dir) = harness();
        let key = keys.generate("vision-internal").unwrap();
        // 停用后再保存必须被拒（`ApiKeyStore::update` 会按 status 重算 is_active，
        // 只改 is_active 会被冲掉——必须改 status）
        keys.update(&key.id, |k| {
            k.status = "disabled".to_string();
        })
        .unwrap();
        assert!(save_vision_config(&services, &keys, config(&key.id), &vision_catalog()).is_err());
    }

    #[test]
    fn parse_vision_args_limits() {
        assert!(parse_vision_args(&json!({})).is_err());
        assert!(parse_vision_args(&json!({ "images": [], "prompt": "x" })).is_err());
        assert!(
            parse_vision_args(&json!({ "images": ["file:///etc/passwd"], "prompt": "x" })).is_err()
        );
        assert!(parse_vision_args(
            &json!({ "images": ["https://example.com/a.png"], "prompt": "" })
        )
        .is_err());
        let (images, prompt) = parse_vision_args(&json!({
            "images": ["https://example.com/a.png", "data:image/png;base64,AAA"],
            "prompt": "describe"
        }))
        .unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(prompt, "describe");
        let too_many: Vec<String> = (0..11)
            .map(|_| "https://example.com/x.png".to_string())
            .collect();
        assert!(parse_vision_args(&json!({ "images": too_many, "prompt": "x" })).is_err());
    }

    #[test]
    fn mask_key_never_leaks_prefix_shape() {
        assert_eq!(mask_key("sk-1234567890abcdef"), "sk12••••cdef");
        assert_eq!(mask_key("short"), "••••");
    }
}
