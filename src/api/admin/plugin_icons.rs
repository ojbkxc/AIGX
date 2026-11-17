// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 脚本插件图标：`GET /plugins/script-icons/{file}`。
//!
//! 图标由脚本插件宿主的图标缓存产出（`plugin_host::script::icons`：把
//! registry 里的 `icon` URL 拉下来、按内容摘要落盘）。本文件只是 HTTP 出口：
//! 读缓存 → 加缓存头 → 交出去。
//!
//! ── 缓存口径 ────────────────────────────────────────────────
//! - `ETag` = `"<文件名>"`（文件名本身是内容摘要，内容变即文件名变）；
//! - `If-None-Match` 命中 → 304，不重发正文；
//! - `Cache-Control: public, max-age=31536000, immutable`（一年 + 不可变）：
//!   文件名带摘要，可安全长缓存；
//! - 安全头固定（`nosniff` / `no-referrer` / 沙箱 CSP）：图标是不可信来源的
//!   字节，即便被当成页面打开也不能跑脚本。
//!
//! ── 不挂 protected ──────────────────────────────────────────
//! 与 `/plugins/runtime/*` 同一理由：img 标签不带头，图标不含数据。
//!
//! ── 硬约束 ──────────────────────────────────────────────────
//! release 是 `panic=abort`：本文件零 unwrap/expect/panic。

use axum::body::Body;
use axum::extract::Path;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::api::openai::AppState;
use crate::plugin_host::script::icons;

/// 管理面路由。图标是公开静态资源（理由见模块头）。
pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/plugins/script-icons/:file", axum::routing::get(serve))
}

/// GET /plugins/script-icons/{file}
pub async fn serve(Path(file): Path<String>, headers: HeaderMap) -> Response {
    // 文件名已由路由段限定（不含 `/`）；`read` 侧再做一次摘要名与越界校验
    let name = file.clone();
    let image = tokio::task::spawn_blocking(move || icons::read(&name)).await;
    let Ok(Some((bytes, mime))) = image else {
        let mut response = StatusCode::NOT_FOUND.into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return response;
    };
    let etag = format!("\"{file}\"");
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|entry| entry.trim().trim_start_matches("W/") == etag)
        });
    let mut response = if matches {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Body::from(bytes).into_response()
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; img-src data:; sandbox; \
             base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    if let Ok(etag) = HeaderValue::from_str(&etag) {
        headers.insert(header::ETAG, etag);
    }
    response
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    /// ETag 匹配：裸标签、`W/` 弱校验、逗号列表里任一项命中都算。
    /// 与 handler 内的判定逐字同口径（抽出来单测，避免为构造 HeaderMap 起 router）。
    fn matches(header: &str, etag: &str) -> bool {
        HeaderValue::from_str(header)
            .ok()
            .and_then(|value| value.to_str().ok().map(str::to_string))
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|entry| entry.trim().trim_start_matches("W/") == etag)
            })
    }

    #[test]
    fn etag_matching_forms() {
        let etag = "\"abc123.png\"";
        assert!(matches("\"abc123.png\"", etag));
        assert!(matches("W/\"abc123.png\"", etag));
        assert!(matches("\"other\", W/\"abc123.png\"", etag));
        assert!(!matches("\"different\"", etag));
        assert!(!matches("", etag));
    }
}
