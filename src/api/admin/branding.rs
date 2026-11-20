//! 面板品牌自定义（对标 Agent2API v3.5.0-beta.3 `core/branding.rs`）。
//!
//! 配置项落在 `[admin]` 段（config.toml）：
//! - `panel_name`（默认 `AIGX`）
//! - `logo_url`（默认 `/logo.png`；**外链/站内路径**形态，作为未上传 Logo 时的回落）
//! - `favicon_url`（可选；为空回落到生效 logo_url）
//!
//! ── Logo 上传（对齐 A2A 的品牌自定义）─────────────────────────
//! 管理员可上传 PNG / JPG / WebP（≤2 MiB、≤4096×4096），后端统一重编码为
//! 静态 PNG（≤512 长边，剥离元数据）后存进 FileStore（键 `branding:logo`）。
//! 上传后的生效 Logo 地址是**公开端点**
//! `GET /api/public/branding/logo?v=<sha256>` —— 登录页在拿到会话前就要
//! 渲染 Logo，`<img src>` 也带不了 Authorization 头，所以这个端点免鉴权；
//! 只回图片字节，不回任何配置。`?v=<hash>` 同时承担 Cache-Control 的
//! ETag/304 语义：换图即换 URL，浏览器不会拿旧图。
//!
//! 三个地址都经 [`crate::config::validate_brand_url`] 校验：只允许
//! `http(s)://` 绝对 URL 或站内 `/` 开头路径，拒绝 `javascript:` / `data:`
//! 等可执行协议（防 `<img src>` / `<link href>` 注入）。空串与相对路径也拒绝。
//!
//! 端点：
//! - `GET /api/public/branding`（免鉴权）：登录页在拿到会话前就要渲染品牌；
//!   只回名称/Logo/favicon/标题，不含任何敏感配置。
//! - `GET /api/public/branding/logo`（免鉴权）：已上传 Logo 的 PNG 字节。
//! - `GET /api/branding`（管理员）：读取当前值 + 默认值（便于前端画「恢复默认」）。
//! - `PUT /api/branding`（管理员）：整体保存；`logo` 传 data URL = 上传，
//!   传 `null` = 清除上传（回落 logo_url）。
//!
//! 保存后**立即生效**：品牌渲染完全由前端读取配置决定，无进程级缓存。

use std::io::Cursor;

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::openai::AppState;
use super::common::{error_response, record_audit, verify_admin};
use crate::storage::FileStore;

/// 品牌设置请求体（缺省字段保持原值；`favicon_url` 传空串 = 清除）。
#[derive(Debug, Deserialize)]
pub struct BrandingRequest {
    pub panel_name: Option<String>,
    pub logo_url: Option<String>,
    pub favicon_url: Option<String>,
    /// `None` = 不改；`null` = 清除上传；`"data:image/...;base64,..."` = 上传。
    ///
    /// 用 `deserialize_with` 把「字段缺省」与「显式 null」区分开：serde 默认
    /// 把两者都折成 `None`，那样「移除已上传 Logo」就永远传达不到后端。
    #[serde(default, deserialize_with = "deserialize_optional_value")]
    pub logo: Option<Value>,
}

/// 保留 `null` 的三态解析（缺省 → `None`；`null` → `Some(Value::Null)`）。
fn deserialize_optional_value<'de, D>(deserializer: D) -> Result<Option<Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Value::deserialize(deserializer).map(Some)
}

/// 面板名称上限（字符）：过长名称会撑坏登录页/侧栏排版。
const PANEL_NAME_MAX_CHARS: usize = 64;

/// Logo 上限：上传原图 ≤2 MiB（与上游 `MAX_LOGO_BYTES` 同值）。
pub const MAX_LOGO_BYTES: usize = 2 * 1024 * 1024;

/// 上传 Logo 的 KV 键（与 debug_traffic / account 等前缀互不冲突）。
const LOGO_KEY: &str = "branding:logo";

/// 公开 Logo 端点（生效地址 = 本路径 + `?v=<etag>`）。
const LOGO_PATH: &str = "/api/public/branding/logo";

/// 持久化的 Logo：重编码后的静态 PNG（base64）+ 内容哈希（ETag/URL 版本号）。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredLogo {
    png: String,
    etag: String,
}

/// 品牌配置的规范 JSON 形状（公开端点与管理端点共用）。
///
/// `logo_url`：已上传 Logo 时指向公开 Logo 端点；否则回落到配置值/默认值。
/// `favicon_url` 始终给出**生效值**（未设置 = 生效 logo_url），前端无需再回落。
fn branding_payload(config: &crate::config::AppConfig, logo_etag: Option<&str>) -> Value {
    let admin = &config.admin;
    let name = admin.panel_name.trim();
    let panel_name = if name.is_empty() {
        crate::config::default_panel_name()
    } else {
        name.to_string()
    };
    let configured_logo = if admin.logo_url.trim().is_empty() {
        crate::config::default_logo_url()
    } else {
        admin.logo_url.trim().to_string()
    };
    let logo_uploaded = logo_etag.is_some();
    let logo_url = match logo_etag {
        Some(etag) => format!("{LOGO_PATH}?v={etag}"),
        None => configured_logo,
    };
    let favicon_url = admin
        .favicon_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| logo_url.clone());
    json!({
        "panel_name": panel_name,
        "logo_url": logo_url,
        "favicon_url": favicon_url,
        "logo_uploaded": logo_uploaded,
        // 页面标题：`名称 — AI 中转网关`（前端可自行再加工，这里给默认拼接式）
        "title": format!("{panel_name} — AI 中转网关"),
    })
}

/// 读取已上传 Logo 元数据（不过滤/不校验库错误：品牌展示失败按「未上传」降级）。
fn load_logo_meta(store: &FileStore) -> Option<StoredLogo> {
    store.get::<StoredLogo>(LOGO_KEY).ok().flatten()
}

/// 内容哈希（hex）：用作 ETag 与 `?v=` 版本号。
fn logo_etag(png_base64: &str) -> String {
    format!("{:x}", Sha256::digest(png_base64.as_bytes()))
}

/// 把上传的 data URL 规范化为静态 PNG（base64）。
///
/// 校验顺序（与上游 `normalize_logo` 一致）：
/// 1. 只认 `data:image/png|jpeg|webp;base64,` 三种声明；
/// 2. base64 文本长度先按解码尺寸估算（避免超大输入白解码）；
/// 3. 解码后非空且 ≤2 MiB；magic 必须与声明格式一致；
/// 4. 解码长宽 ≤4096（同时限制内存分配），再缩到 ≤512 长边；
/// 5. 统一重编码为 PNG，剥离 EXIF/ICC 等元数据；结果 ≤2 MiB。
pub fn normalize_logo_data_url(data: &str) -> Result<String, String> {
    let (header, encoded) = data.split_once(',').ok_or("品牌 Logo 数据无效")?;
    let format = match header {
        "data:image/png;base64" => image::ImageFormat::Png,
        "data:image/jpeg;base64" => image::ImageFormat::Jpeg,
        "data:image/webp;base64" => image::ImageFormat::WebP,
        _ => return Err("品牌 Logo 仅支持 PNG、JPG、WebP 图片".into()),
    };
    if encoded.len() > MAX_LOGO_BYTES.div_ceil(3) * 4 {
        return Err("品牌 Logo 不能超过 2 MiB".into());
    }
    let bytes = STANDARD.decode(encoded).map_err(|_| "品牌 Logo 数据无效")?;
    if bytes.is_empty() || bytes.len() > MAX_LOGO_BYTES {
        return Err("品牌 Logo 必须是非空且不超过 2 MiB 的图片".into());
    }
    if image::guess_format(&bytes).ok() != Some(format) {
        return Err("品牌 Logo 内容与图片格式不匹配".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    // image::Limits 是 #[non_exhaustive]：crate 外不能结构体字面量，只能先
    // default 再逐项覆盖（clippy 的 field_reassign_with_default 在此豁免）
    #[allow(clippy::field_reassign_with_default)]
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|_| "无法读取品牌 Logo，图片长宽不能超过 4096 像素")?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return Err("品牌 Logo 尺寸无效".into());
    }
    // 只缩小、不放大小图：thumbnail 会按目标框等比缩放（10×10 会被放大到
    // 512×512），与「自动缩小至 512 像素以内」的语义相反。
    let normalized = if decoded.width() > 512 || decoded.height() > 512 {
        image::DynamicImage::ImageRgba8(decoded.thumbnail(512, 512).to_rgba8())
    } else {
        image::DynamicImage::ImageRgba8(decoded.to_rgba8())
    };
    let mut output = Cursor::new(Vec::new());
    normalized
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|_| "品牌 Logo 转换失败")?;
    if output.get_ref().len() > MAX_LOGO_BYTES {
        return Err("品牌 Logo 转换后过大".into());
    }
    Ok(STANDARD.encode(output.into_inner()))
}

/// Logo 写入动作（PUT 解析阶段决定，配置保存成功后执行）。
enum LogoAction {
    Unchanged,
    Clear,
    Save(StoredLogo),
}

/// GET /api/public/branding —— 公开只读品牌信息（免鉴权）。
///
/// 登录页 / 注册页在无会话状态下也要渲染名称与 Logo，因此该端点不能走
/// `verify_user`；返回值仅含展示用的地址，不泄露任何配置细节。
pub async fn handle_get_public_branding(State(state): State<AppState>) -> Json<Value> {
    let config = state.config_manager.get().await;
    let etag = load_logo_meta(&state.alert_store).map(|logo| logo.etag);
    Json(json!({
        "success": true,
        "data": branding_payload(&config, etag.as_deref()),
    }))
}

/// GET /api/public/branding/logo —— 公开读取已上传的品牌 Logo（PNG 字节）。
///
/// 带 ETag / `If-None-Match` → 304；`Cache-Control: public, max-age=0,
/// must-revalidate`：每次都校验，换图立即生效（`?v=` 变化也会直接换 URL）。
pub async fn handle_get_public_branding_logo(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    let Some(meta) = load_logo_meta(&state.alert_store) else {
        return (StatusCode::NOT_FOUND, "品牌 Logo 未设置").into_response();
    };
    let bytes = match STANDARD.decode(meta.png.as_bytes()) {
        Ok(bytes) if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => bytes,
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "品牌 Logo 数据无效").into_response(),
    };
    let etag = format!("\"{}\"", meta.etag);
    let not_modified = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().trim_start_matches("W/") == etag)
        });
    let mut response = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Body::from(bytes).into_response()
    };
    let out = response.headers_mut();
    out.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    out.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=0, must-revalidate"),
    );
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag) {
        out.insert(header::ETAG, value);
    }
    response
}

/// GET /api/branding —— 管理员读取品牌设置（含默认值，便于「恢复默认」）。
pub async fn handle_get_branding(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    let etag = load_logo_meta(&state.alert_store).map(|logo| logo.etag);
    Ok(Json(json!({
        "success": true,
        "data": {
            "current": branding_payload(&config, etag.as_deref()),
            "defaults": {
                "panel_name": crate::config::default_panel_name(),
                "logo_url": crate::config::default_logo_url(),
                "favicon_url": crate::config::default_logo_url(),
            },
        }
    })))
}

/// PUT /api/branding —— 管理员保存品牌设置（保存即生效，无需重启）。
///
/// `logo` 字段三态：省略 = 保持；`null` = 清除上传；data URL = 校验+重编码后保存。
/// Logo 校验在配置写锁之外先跑（失败即 400 且配置与已存 Logo 都不动）。
pub async fn handle_update_branding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrandingRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 审计 before 快照取自鉴权时刻（只读用，不参与写入）
    let before_config = verify_admin(&state, &headers).await?;
    let before_etag = load_logo_meta(&state.alert_store).map(|logo| logo.etag);
    let before = branding_payload(&before_config, before_etag.as_deref());

    // 先在写锁之外校验/重编码 Logo（图片解码最坏几十毫秒，别持配置锁）
    let action = match body.logo {
        None => LogoAction::Unchanged,
        Some(Value::Null) => LogoAction::Clear,
        Some(Value::String(data)) => {
            let normalized = tokio::task::spawn_blocking(move || normalize_logo_data_url(&data))
                .await
                .map_err(|_| {
                    error_response(
                        "品牌 Logo 处理任务中断，请重试",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    )
                })?
                .map_err(|message| error_response(&message, StatusCode::BAD_REQUEST))?;
            let etag = logo_etag(&normalized);
            LogoAction::Save(StoredLogo {
                png: normalized,
                etag,
            })
        }
        Some(_) => {
            return Err(error_response(
                "品牌 Logo 必须是上传的图片或 null",
                StatusCode::BAD_REQUEST,
            ))
        }
    };

    // 字段级读-改-写（P2）：校验在写锁内跑，失败即 400 且配置未动
    let config = super::common::update_admin_config(&state, &headers, |config| {
        if let Some(raw) = &body.panel_name {
            let name = raw.trim();
            if name.is_empty() {
                return Err("面板名称不能为空".to_string());
            }
            if name.chars().count() > PANEL_NAME_MAX_CHARS {
                return Err(format!("面板名称过长（上限 {PANEL_NAME_MAX_CHARS} 字符）"));
            }
            // 控制字符会破坏日志与响应头（若将来用于 title），一律拒绝
            if name.chars().any(char::is_control) {
                return Err("面板名称不能包含控制字符".to_string());
            }
            config.admin.panel_name = name.to_string();
        }

        if let Some(raw) = &body.logo_url {
            config.admin.logo_url = crate::config::validate_brand_url(raw)
                .map_err(|e| format!("Logo 地址非法：{e}"))?;
        }

        if let Some(raw) = &body.favicon_url {
            let trimmed = raw.trim();
            config.admin.favicon_url = if trimmed.is_empty() {
                // 空串 = 清除 favicon（回落 logo_url）
                None
            } else {
                Some(
                    crate::config::validate_brand_url(trimmed)
                        .map_err(|e| format!("favicon 地址非法：{e}"))?,
                )
            };
        }
        Ok(())
    })
    .await?;

    // 配置已落盘；再写 Logo（KV 写入失败不把已保存的配置回滚，只报 500）
    match action {
        LogoAction::Unchanged => {}
        LogoAction::Clear => {
            let _ = state.alert_store.delete(LOGO_KEY);
        }
        LogoAction::Save(logo) => {
            state.alert_store.put(LOGO_KEY, &logo).map_err(|_| {
                error_response("品牌 Logo 保存失败", StatusCode::INTERNAL_SERVER_ERROR)
            })?;
        }
    }
    let after_etag = load_logo_meta(&state.alert_store).map(|logo| logo.etag);
    let after = branding_payload(&config, after_etag.as_deref());

    record_audit(
        &state,
        &super::common::admin_id_from_session(&state, &headers).await,
        "branding.update",
        "branding",
        Some(before),
        Some(after.clone()),
    );
    Ok(Json(json!({
        "success": true,
        "data": after,
        "restartRequired": false,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(panel: &str, logo: &str, favicon: Option<&str>) -> crate::config::AppConfig {
        let mut cfg = crate::config::AppConfig::default();
        cfg.admin.panel_name = panel.to_string();
        cfg.admin.logo_url = logo.to_string();
        cfg.admin.favicon_url = favicon.map(str::to_string);
        cfg
    }

    /// 构造一张真实 PNG（10×10 红色）的 data URL。
    fn png_data_url(width: u32, height: u32) -> String {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]));
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        format!(
            "data:image/png;base64,{}",
            STANDARD.encode(out.into_inner())
        )
    }

    #[test]
    fn defaults_render_without_config() {
        let cfg = crate::config::AppConfig::default();
        let payload = branding_payload(&cfg, None);
        assert_eq!(payload["panel_name"], "AIGX");
        assert_eq!(payload["logo_url"], "/logo.png");
        assert_eq!(payload["logo_uploaded"], false);
        // favicon 未设置 → 回落 logo
        assert_eq!(payload["favicon_url"], "/logo.png");
        assert_eq!(payload["title"], "AIGX — AI 中转网关");
    }

    #[test]
    fn uploaded_logo_wins_over_configured_url() {
        let cfg = cfg_with(
            "My Gateway",
            "https://cdn.example.com/logo.png",
            Some("/favicon.ico"),
        );
        let payload = branding_payload(&cfg, Some("abc123"));
        assert_eq!(payload["logo_url"], "/api/public/branding/logo?v=abc123");
        assert_eq!(payload["logo_uploaded"], true);
        // 显式 favicon 仍然独立
        assert_eq!(payload["favicon_url"], "/favicon.ico");
    }

    #[test]
    fn custom_branding_wins_and_favicon_separate() {
        let cfg = cfg_with(
            "My Gateway",
            "https://cdn.example.com/logo.png",
            Some("/favicon.ico"),
        );
        let payload = branding_payload(&cfg, None);
        assert_eq!(payload["panel_name"], "My Gateway");
        assert_eq!(payload["logo_url"], "https://cdn.example.com/logo.png");
        assert_eq!(payload["favicon_url"], "/favicon.ico");
        assert_eq!(payload["title"], "My Gateway — AI 中转网关");
    }

    /// 空串/纯空白的名称与 logo 回落到默认值（不把空串渲染到页面上）。
    #[test]
    fn blank_values_fall_back_to_defaults() {
        let cfg = cfg_with("   ", "  ", Some("  "));
        let payload = branding_payload(&cfg, None);
        assert_eq!(payload["panel_name"], "AIGX");
        assert_eq!(payload["logo_url"], "/logo.png");
        assert_eq!(payload["favicon_url"], "/logo.png");
    }

    #[test]
    fn normalizes_upload_to_static_png() {
        let encoded = normalize_logo_data_url(&png_data_url(10, 10)).expect("accept png");
        let bytes = STANDARD.decode(encoded).expect("valid base64");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = image::load_from_memory(&bytes).expect("valid png");
        assert_eq!(decoded.width(), 10);
        assert_eq!(decoded.height(), 10);
    }

    #[test]
    fn rejects_mismatched_or_unbounded_uploads() {
        let png = png_data_url(4, 4);
        // 声明 jpeg 但内容是 PNG → 拒绝（magic 校验）
        let mismatched = png.replacen("data:image/png", "data:image/jpeg", 1);
        assert!(normalize_logo_data_url(&mismatched).is_err());
        // 非 data URL / 不支持的类型
        assert!(normalize_logo_data_url("https://example.com/a.png").is_err());
        assert!(normalize_logo_data_url("data:image/gif;base64,R0lGODlh").is_err());
        // 正向对照：同内容按 PNG 声明可以接受
        assert!(normalize_logo_data_url(&png).is_ok());
    }
}
