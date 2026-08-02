use axum::Router;
use std::path::PathBuf;
use tower_http::services::ServeDir;

/// 提供管理面板静态文件
pub fn serve_static_files() -> Router {
    // 尝试从多个位置查找静态文件目录
    let static_dir = find_static_dir().unwrap_or_else(|| {
        // 默认路径
        let mut path = PathBuf::from(".");
        path.push("static");
        path
    });

    tracing::info!("Serving static files from: {:?}", static_dir);

    Router::new().fallback_service(
        ServeDir::new(&static_dir)
            .not_found_service(
                tower_http::services::ServeDir::new(&static_dir.join("index.html"))
                    .precompressed_br()
                    .precompressed_gzip()
                    .precompressed_zstd(),
            ),
    )
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

    for candidate in candidates.into_iter().flatten() {
        if candidate.exists() && candidate.is_dir() {
            return Some(candidate);
        }
    }

    None
}