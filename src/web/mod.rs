use axum::{
    body::Body,
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use std::{convert::Infallible, path::PathBuf};
use tower_http::services::ServeDir;

/// 提供管理面板静态文件，支持 SPA fallback
pub fn serve_static_files() -> Router {
    let static_dir = find_static_dir().unwrap_or_else(|| {
        let mut path = PathBuf::from(".");
        path.push("static");
        path
    });

    tracing::info!("Serving static files from: {:?}", static_dir);

    let index_path = static_dir.join("index.html");

    Router::new().fallback_service(ServeDir::new(&static_dir).fallback(tower::service_fn(
        move |_req: Request<Body>| {
            let index_path = index_path.clone();
            async move {
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
}

/// 查找静态文件目录
fn find_static_dir() -> Option<PathBuf> {
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
