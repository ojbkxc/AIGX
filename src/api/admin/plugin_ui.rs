// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件 UI 资源的托管：`GET /plugins/{pack}/ui/{*rest}`。
//!
//! ── 为什么由网关托管而不是客户端自己读包 ──────────────────────
//! 插件页面是包里的静态资源，而**只有网关知道包装在哪**（`{plugins_dir}/<id>/<version>/ui/`）。
//! 远程客户端连的是网关，它连那台机器的文件系统都看不到 —— 所以页面必须由网关出。
//!
//! ── 为什么这条路由**不挂 protected** ────────────────────────
//! 它是页面本身（HTML + JS + CSS），要能被 iframe 直接加载；而 iframe 里的
//! 请求带不上面板令牌头。页面拿到令牌靠的是宿主用 `postMessage` 传进去。
//! 真正的数据请求仍走 `/api/*`，那些全部挂 protected —— 页面本身不含任何
//! 数据，只是代码。这与「静态 `ui/` 目录」同一档暴露面。
//!
//! ── 安全 ────────────────────────────────────────────────────
//! 路径解析与边界检查全在 [`resolve_ui_file`]：只服务**已启用**包的
//! `ui/` 目录，`..` / 绝对路径 / 反斜杠 / 盘符一律拒绝，canonicalize 后
//! 再确认仍在 `ui/` 根内（纵深防御）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use std::path::PathBuf;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::Response;

use crate::api::openai::AppState;

/// 管理面路由。页面本身免鉴权（理由见模块头）。
pub fn router() -> axum::Router<AppState> {
    // axum 0.7 语法：`:pack` 命名参数 + `*rest` 通配（与 A2A 的 `{pack}` /
    // `{*rest}` 是同一路由形态的两种写法，后者是 axum 0.8 起的新语法）
    axum::Router::new().route("/plugins/:pack/ui/*rest", axum::routing::get(serve))
}

/// GET /plugins/{pack}/ui/{*rest}
pub async fn serve(
    State(state): State<AppState>,
    Path((pack, rest)): Path<(String, String)>,
) -> Response {
    let Some(path) = resolve_ui_file(&state, &pack, &rest) else {
        return not_found();
    };
    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(_) => return not_found(),
    };
    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_of(&path))
        // 与静态服务同一口径：插件升级后浏览器立刻拿到新页面，
        // 不会出现「页面引用了旧 js」的中间态
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(bytes));
    response.unwrap_or_else(|_| not_found())
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from("not found"))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

/// 把 `/plugins/{pack}/ui/{rest}` 解析成磁盘上的文件路径（`None` = 不存在 / 非法）。
///
/// ── 安全边界（三条，缺一条就是漏洞）──────────────────────────
///   1. **只服务已启用包的版本目录**：包 id 必须在注册表里且 `enabled`，
///      版本目录由注册表给（不是 URL 给的）—— 于是 URL 里的 `{pack}` 只是
///      一个查表的键，不能指向任意目录；
///   2. **路径逐段归一化**：`..` / 绝对路径 / 反斜杠 / 盘符一律拒绝，
///      不允许逃出 `ui/` 目录；
///   3. **只出 `ui/` 前缀下的文件**：包里的 `bin/`、`pack.json` 不对外。
pub fn resolve_ui_file(state: &AppState, pack_id: &str, rest: &str) -> Option<PathBuf> {
    if pack_id.is_empty() || rest.is_empty() {
        return None;
    }
    // ① 包必须已安装且启用（停用 = 页面也不该能访问）
    let meta = state.plugin_host.registry().get(pack_id)?;
    if !meta.enabled {
        return None;
    }
    let root = meta
        .version_dir(state.plugin_host.plugins_root())
        .join("ui");
    // ② 逐段归一化
    let mut segments: Vec<&str> = Vec::new();
    for segment in rest.split('/') {
        if segment.contains(':') {
            return None;
        }
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => {
                if other.contains('\\') {
                    return None;
                }
                segments.push(other);
            }
        }
    }
    if segments.is_empty() {
        return None;
    }
    let mut path = root.clone();
    for segment in &segments {
        path.push(segment);
    }
    // ③ 归一化后的路径必须仍在 `ui/` 下（双保险：`..` 已在上面消化）
    let canonical_root = root.canonicalize().ok()?;
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    canonical.is_file().then_some(canonical)
}

/// 扩展名 → content-type。插件页面是一组前端资源，常见类型够用。
fn mime_of(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "map" => "application/json",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_table_covers_frontend_assets() {
        assert_eq!(
            mime_of(std::path::Path::new("a/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_of(std::path::Path::new("a/app.mjs")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_of(std::path::Path::new("a/x.woff2")), "font/woff2");
        assert_eq!(
            mime_of(std::path::Path::new("a/x.unknown")),
            "application/octet-stream"
        );
    }

    /// 归一化规则：`..` 逐级弹出；弹空即拒绝；盘符与反斜杠直接拒绝。
    /// 这里只测纯字符串规则（不依赖 AppState / 磁盘）。
    fn normalize(rest: &str) -> Option<Vec<&str>> {
        let mut segments: Vec<&str> = Vec::new();
        for segment in rest.split('/') {
            if segment.contains(':') {
                return None;
            }
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop()?;
                }
                other => {
                    if other.contains('\\') {
                        return None;
                    }
                    segments.push(other);
                }
            }
        }
        if segments.is_empty() {
            return None;
        }
        Some(segments)
    }

    #[test]
    fn path_normalization_rejects_escapes() {
        assert_eq!(normalize("index.html"), Some(vec!["index.html"]));
        assert_eq!(normalize("./a/./b.js"), Some(vec!["a", "b.js"]));
        // 弹空 → 拒绝（逃出 ui/）
        assert_eq!(normalize("../x"), None);
        assert_eq!(normalize("a/../../x"), None);
        // 盘符 / 反斜杠
        assert_eq!(normalize("C:/x"), None);
        assert_eq!(normalize("a\\b"), None);
        // 全部弹掉 → 拒绝
        assert_eq!(normalize("a/.."), None);
    }
}
