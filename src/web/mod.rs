use axum::{
    body::Body,
    extract::Request as ExtRequest,
    http::{header, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    Router,
};
use std::{convert::Infallible, path::PathBuf};
use tower_http::services::ServeDir;

/// 静态资源缓存头中间件。
///
/// 自动更新替换 `static/` 后，浏览器若缓存了旧的 `index.html`，会继续引用
/// 已被删除的旧哈希资源（404 → 白屏/样式错乱）。因此：
/// - `index.html` 与 SPA 回退：`no-cache`（每次都回源校验，更新后立即生效）；
/// - `/assets/*`（文件名带内容哈希）：长缓存 `immutable`，更新无需重新下载。
async fn cache_headers(req: ExtRequest, next: Next) -> Response {
    let is_asset = req.uri().path().starts_with("/assets/");
    let mut resp = next.run(req).await;
    if resp.status().is_success() {
        let value = if is_asset {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    resp
}

/// 提供管理面板静态文件，支持 SPA fallback
pub fn serve_static_files() -> Router {
    let static_dir = find_static_dir().unwrap_or_else(|| {
        let mut path = PathBuf::from(".");
        path.push("static");
        path
    });
    serve_static_files_from(static_dir)
}

/// 以指定目录构建静态服务（`serve_static_files` 的实现体，单独抽出便于测试注入目录）。
fn serve_static_files_from(static_dir: PathBuf) -> Router {
    tracing::info!("Serving static files from: {:?}", static_dir);

    let index_path = static_dir.join("index.html");

    Router::new()
        .fallback_service(ServeDir::new(&static_dir).fallback(tower::service_fn(
            move |req: Request<Body>| {
                let index_path = index_path.clone();
                async move {
                    // #17-⑥：未匹配的 /api/* 路径必须返回 404 JSON，不能落进
                    // SPA fallback 返回 200 HTML——管理面探测（未鉴权路径枚举）
                    // 会把「HTML 200」误读为「端点存在」，前端 fetch().json()
                    // 也会解析失败报出误导性错误。SPA 只服务页面路由。
                    if req.uri().path().starts_with("/api/") {
                        return Ok::<Response<Body>, Infallible>(
                            Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .header("content-type", "application/json")
                                .body(Body::from(r#"{"success":false,"error":"Not found"}"#))
                                .unwrap_or_else(|_| {
                                    Response::builder()
                                        .status(StatusCode::NOT_FOUND)
                                        .body(Body::from("Not Found"))
                                        .unwrap_or_else(|_| Response::new(Body::from("Not Found")))
                                }),
                        );
                    }
                    // L1：Response::builder()...unwrap() 在 header/body 构造理论上不会失败，
                    // 但 unwrap 在 panic=unwind 下会终止服务线程。改为降级返回 500，增强健壮性。
                    let result: Result<Response<Body>, Infallible> =
                        match tokio::fs::read_to_string(&index_path).await {
                            Ok(content) => Ok(Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", "text/html; charset=utf-8")
                                .body(Body::from(content))
                                .unwrap_or_else(|_| {
                                    Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(Body::from("Internal Server Error"))
                                        .unwrap_or_else(|_| {
                                            Response::new(Body::from("Internal Server Error"))
                                        })
                                })),
                            Err(_) => Ok(Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Body::from("Not Found"))
                                .unwrap_or_else(|_| Response::new(Body::from("Not Found")))),
                        };
                    result
                }
            },
        )))
        // 缓存策略：index.html/SPA 回退 no-cache，哈希资源 immutable
        .layer(middleware::from_fn(cache_headers))
}

/// 查找静态文件目录（启动时解析一次；自动更新也用它定位要替换的目录）
pub fn find_static_dir() -> Option<PathBuf> {
    let candidates: Vec<Option<PathBuf>> = vec![
        Some(PathBuf::from(".").join("static")),
        Some(PathBuf::from(".").join("frontend").join("dist")),
        Some(PathBuf::from(".").join("admin").join("dist")),
        Some(PathBuf::from(".").join("web").join("static")),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .map(|p| p.join("static")),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .map(|p| p.join("..").join("static")),
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|candidate| candidate.exists() && candidate.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use tower::ServiceExt; // oneshot

    /// 造一个最小 static 目录：index.html + assets/app.js。
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>spa</html>").unwrap();
        std::fs::create_dir_all(dir.path().join("assets")).unwrap();
        std::fs::write(dir.path().join("assets/app.js"), "console.log(1)").unwrap();
        dir
    }

    async fn get(app: Router, uri: &str) -> Response {
        app.oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// 未匹配的 `/api/*` 必须回 404 JSON —— 不能落进 SPA 回退返回 200 HTML，
    /// 否则管理面的路径探测会把「HTML 200」误读成端点存在。
    #[tokio::test]
    async fn unmatched_api_path_returns_json_404() {
        let dir = fixture();
        let resp = get(
            serve_static_files_from(dir.path().to_path_buf()),
            "/api/nope",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        // 404 不套缓存头（中间件只在 2xx 上加）
        assert!(resp.headers().get(header::CACHE_CONTROL).is_none());
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("\"success\":false"), "{text}");
    }

    /// 页面路由（非 /api、非静态文件）回退到 index.html，且是 no-cache。
    #[tokio::test]
    async fn spa_route_falls_back_to_index_with_no_cache() {
        let dir = fixture();
        let resp = get(serve_static_files_from(dir.path().to_path_buf()), "/logs").await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/html; charset=utf-8")
        );
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-cache")
        );
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "<html>spa</html>");
    }

    /// 哈希资源走长缓存 immutable（更新后无需重新下载）。
    #[tokio::test]
    async fn hashed_asset_gets_immutable_cache() {
        let dir = fixture();
        let resp = get(
            serve_static_files_from(dir.path().to_path_buf()),
            "/assets/app.js",
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("public, max-age=31536000, immutable")
        );
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert_eq!(std::str::from_utf8(&body).unwrap(), "console.log(1)");
    }

    /// 静态目录里缺 index.html 时，SPA 回退给 404 而不是 500。
    #[tokio::test]
    async fn missing_index_html_falls_back_to_404() {
        let dir = tempfile::tempdir().unwrap();
        let resp = get(serve_static_files_from(dir.path().to_path_buf()), "/logs").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
