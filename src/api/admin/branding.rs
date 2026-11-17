//! 面板品牌自定义（对标 Agent2API 3.5.0）。
//!
//! 配置项落在 `[admin]` 段（config.toml）：
//! - `panel_name`（默认 `AIGX`）
//! - `logo_url`（默认 `/logo.png`）
//! - `favicon_url`（可选；为空回落到 logo_url）
//!
//! 三个地址都经 [`crate::config::validate_brand_url`] 校验：只允许
//! `http(s)://` 绝对 URL 或站内 `/` 开头路径，拒绝 `javascript:` / `data:`
//! 等可执行协议（防 `<img src>` / `<link href>` 注入）。空串与相对路径也拒绝。
//!
//! 端点：
//! - `GET /api/public/branding`（免鉴权）：登录页在拿到会话前就要渲染品牌；
//!   只回名称/Logo/favicon 三项，不含任何敏感配置。
//! - `GET /api/branding`（管理员）：读取当前值 + 默认值（便于前端画「恢复默认」）。
//! - `PUT /api/branding`（管理员）：整体保存；空串 favicon 表示不设置。
//!
//! 保存后**立即生效**：品牌渲染完全由前端读取配置决定，无进程级缓存。

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::super::openai::AppState;
use super::common::{record_audit, verify_admin};

/// 品牌设置请求体（缺省字段保持原值；`favicon_url` 传空串 = 清除）。
#[derive(Debug, Deserialize)]
pub struct BrandingRequest {
    pub panel_name: Option<String>,
    pub logo_url: Option<String>,
    pub favicon_url: Option<String>,
}

/// 面板名称上限（字符）：过长名称会撑坏登录页/侧栏排版。
const PANEL_NAME_MAX_CHARS: usize = 64;

/// 品牌配置的规范 JSON 形状（公开端点与管理端点共用）。
///
/// `favicon_url` 始终给出**生效值**（未设置 = logo_url），前端无需再回落。
fn branding_payload(config: &crate::config::AppConfig) -> Value {
    let admin = &config.admin;
    let name = admin.panel_name.trim();
    let panel_name = if name.is_empty() {
        crate::config::default_panel_name()
    } else {
        name.to_string()
    };
    let logo_url = if admin.logo_url.trim().is_empty() {
        crate::config::default_logo_url()
    } else {
        admin.logo_url.trim().to_string()
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
        // 页面标题：`名称 — AI 中转网关`（前端可自行再加工，这里给默认拼接式）
        "title": format!("{panel_name} — AI 中转网关"),
    })
}

/// GET /api/public/branding —— 公开只读品牌信息（免鉴权）。
///
/// 登录页 / 注册页在无会话状态下也要渲染名称与 Logo，因此该端点不能走
/// `verify_user`；返回值仅含展示用的三个地址，不泄露任何配置细节。
pub async fn handle_get_public_branding(State(state): State<AppState>) -> Json<Value> {
    let config = state.config_manager.get().await;
    Json(json!({ "success": true, "data": branding_payload(&config) }))
}

/// GET /api/branding —— 管理员读取品牌设置（含默认值，便于「恢复默认」）。
pub async fn handle_get_branding(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let config = verify_admin(&state, &headers).await?;
    Ok(Json(json!({
        "success": true,
        "data": {
            "current": branding_payload(&config),
            "defaults": {
                "panel_name": crate::config::default_panel_name(),
                "logo_url": crate::config::default_logo_url(),
                "favicon_url": crate::config::default_logo_url(),
            },
        }
    })))
}

/// PUT /api/branding —— 管理员保存品牌设置（保存即生效，无需重启）。
pub async fn handle_update_branding(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BrandingRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // 审计 before 快照取自鉴权时刻（只读用，不参与写入）
    let before = branding_payload(&verify_admin(&state, &headers).await?);
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

    record_audit(
        &state,
        &super::common::admin_id_from_session(&state, &headers).await,
        "branding.update",
        "branding",
        Some(before),
        Some(branding_payload(&config)),
    );
    Ok(Json(json!({
        "success": true,
        "data": branding_payload(&config),
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

    #[test]
    fn defaults_render_without_config() {
        let cfg = crate::config::AppConfig::default();
        let payload = branding_payload(&cfg);
        assert_eq!(payload["panel_name"], "AIGX");
        assert_eq!(payload["logo_url"], "/logo.png");
        // favicon 未设置 → 回落 logo
        assert_eq!(payload["favicon_url"], "/logo.png");
        assert_eq!(payload["title"], "AIGX — AI 中转网关");
    }

    #[test]
    fn custom_branding_wins_and_favicon_separate() {
        let cfg = cfg_with(
            "My Gateway",
            "https://cdn.example.com/logo.png",
            Some("/favicon.ico"),
        );
        let payload = branding_payload(&cfg);
        assert_eq!(payload["panel_name"], "My Gateway");
        assert_eq!(payload["logo_url"], "https://cdn.example.com/logo.png");
        assert_eq!(payload["favicon_url"], "/favicon.ico");
        assert_eq!(payload["title"], "My Gateway — AI 中转网关");
    }

    /// 空串/纯空白的名称与 logo 回落到默认值（不把空串渲染到页面上）。
    #[test]
    fn blank_values_fall_back_to_defaults() {
        let cfg = cfg_with("   ", "  ", Some("  "));
        let payload = branding_payload(&cfg);
        assert_eq!(payload["panel_name"], "AIGX");
        assert_eq!(payload["logo_url"], "/logo.png");
        assert_eq!(payload["favicon_url"], "/logo.png");
    }
}
