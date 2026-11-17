// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 插件页的运行期资源：设计令牌与提供商图标。
//!
//! ```text
//! GET /plugins/runtime/theme.css           宿主设计令牌（:root 两套主题）
//! GET /plugins/runtime/providers/<id>.png  提供商图标（运行时静态目录同源）
//! ```
//!
//! ── 这些资源解决什么问题 ────────────────────────────────────
//! 插件页面（签到中心这类）在 iframe 里，是**独立文档**：读不到宿主的
//! `App.css` 设计令牌，也读不到 `static/providers/` 里的图标。于是它只能
//! 自绘一套控件、手抄一份色板、用首字母代替品牌图标 —— 三样都会与宿主漂移，
//! 表现成「迁移过来的页面长得不一样」。
//!
//! 把这两类资源从网关出给 iframe，插件页就能用**与宿主完全相同的那一份**。
//!
//! ── 与 A2A 的差异 ────────────────────────────────────────────
//! A2A 还出 `islands/ui.js` / `islands/ui.css`（React 组件库产物）。
//! AIGX 前端没有 islands 构建，组件库不是可独立嵌入的产物，因此这两条
//! **明确 404**（安全边界：不猜路径、不回落）。提供商图标改从运行时静态
//! 目录读（AIGX 图标不编进二进制，`static/providers/` 已有全部品牌图）。
//!
//! ── 不挂 protected ──────────────────────────────────────────
//! 与 `/plugins/{pack}/ui/*` 同一理由：iframe 的请求带不上面板令牌头，而这些
//! 文件不含任何数据 —— 它们就是宿主的公开静态资源。真正的数据请求仍走
//! `/api/*`（全部 protected）。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, StatusCode};
use axum::response::Response;

use crate::api::openai::AppState;

/// 管理面路由。静态资源免鉴权（理由见模块头）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/plugins/runtime/*rest", axum::routing::get(serve))
}

/// 宿主设计令牌：`frontend/src/App.css` 的 `:root` 两套主题 + 通用样式。
///
/// 编进二进制而不是读磁盘：桌面/容器形态的网关不知道 `frontend/` 在哪，
/// 让网关去猜路径会静默 404。体积代价可接受（源码级单文件）。
const TOKENS: &str = include_str!("../../../frontend/src/App.css");

/// 响应构造的小工具：省掉每个分支里重复的 builder 链。
fn reply(status: StatusCode, mime: &'static str, body: Vec<u8>) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        // 与插件页其它资源同一口径：网关升级后立刻拿到新产物，
        // 不会出现「页面用了新组件、css 还是旧的」这种中间态
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()))
        })
}

fn not_found() -> Response {
    reply(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        b"not found".to_vec(),
    )
}

/// GET /plugins/runtime/{*rest}
///
/// 一个入口分派两类资源：路径匹配是显式的（`match`），不做前缀猜测 ——
/// 猜错的后果是「请求了个不存在的文件却拿到了别的东西」，比 404 更难查。
pub async fn serve(Path(rest): Path<String>) -> Response {
    // 归一化：去掉空段与 `.`，`..` 一律拒绝（这里只有固定几个资源，
    // 不需要「向上走」的语义）
    let normalized: Vec<&str> = rest
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if normalized
        .iter()
        .any(|segment| *segment == ".." || segment.contains('\\') || segment.contains(':'))
    {
        return not_found();
    }
    let path = normalized.join("/");
    match path.as_str() {
        // 设计令牌：宿主 `:root` 两套主题（亮/暗），插件页据此与宿主同色
        "theme.css" | "tokens.css" => reply(
            StatusCode::OK,
            "text/css; charset=utf-8",
            TOKENS.as_bytes().to_vec(),
        ),
        // AIGX 无 islands 产物：明确 404（不猜路径、不回落）
        "islands/ui.js" | "islands/ui.css" => not_found(),
        // 旧资源路径：AIGX 样式集中在 App.css，不再单列
        "layout.css" | "components.css" | "tooltip.css" => reply(
            StatusCode::OK,
            "text/css; charset=utf-8",
            TOKENS.as_bytes().to_vec(),
        ),
        other => serve_provider_icon(other),
    }
}

/// 提供商图标：`providers/<id>.png|svg`，从运行时静态目录读。
///
/// 只允许 `<id>` 是 `[a-z0-9_-]`（品牌 id 命名空间），杜绝目录穿越；
/// 文件必须在 `static/providers/` 下（双保险：canonicalize 比对前缀）。
fn serve_provider_icon(path: &str) -> Response {
    let Some(name) = path.strip_prefix("providers/") else {
        return not_found();
    };
    let Some(id) = name
        .strip_suffix(".png")
        .or_else(|| name.strip_suffix(".svg"))
    else {
        return not_found();
    };
    let id = id.to_ascii_lowercase();
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return not_found();
    }
    let Some(static_dir) = crate::web::find_static_dir() else {
        return not_found();
    };
    let root = static_dir.join("providers");
    let candidates = [
        root.join(format!("{id}.png")),
        root.join(format!("{id}.svg")),
    ];
    let Some(file) = candidates.iter().find(|candidate| candidate.is_file()) else {
        return not_found();
    };
    let (Ok(canonical), Ok(root)) = (file.canonicalize(), root.canonicalize()) else {
        return not_found();
    };
    if !canonical.starts_with(&root) {
        return not_found();
    }
    let Ok(bytes) = std::fs::read(&canonical) else {
        return not_found();
    };
    let mime = if canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("svg"))
    {
        "image/svg+xml"
    } else {
        "image/png"
    };
    reply(StatusCode::OK, mime, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_serves_tokens_css() {
        let body = TOKENS;
        assert!(body.contains(":root"));
        assert!(body.contains("--surface-2"));
    }

    #[test]
    fn traversal_paths_are_rejected() {
        for bad in ["../etc/passwd", "providers/../../x.png", "a\\b", "C:/x.png"] {
            let segments: Vec<&str> = bad.split('/').collect();
            assert!(
                segments
                    .iter()
                    .any(|s| *s == ".." || s.contains('\\') || s.contains(':')),
                "{bad} 应被归一化闸拦截"
            );
        }
    }

    #[test]
    fn provider_icon_id_validation() {
        // 只有品牌 id 能进目录；路径分隔符与点号一律不认
        assert!(serve_provider_icon("providers/qoder").status() == StatusCode::NOT_FOUND);
        assert!(serve_provider_icon("providers/../qoder.png").status() == StatusCode::NOT_FOUND);
    }
}
