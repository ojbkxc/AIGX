# AIGX Rust Backend with Embedded Frontend
## Rust 后端 + 现代前端集成指南

> **目标**: 创建单一 Rust 可执行文件，内嵌现代前端，所有编译通过 GitHub Actions

---

## 📦 Rust 项目结构（保持不变）

```
C:\GitHub\AIGX\
├── Cargo.toml
├── Cargo.lock
├── src/
│   ├── lib.rs              # 核心库
│   ├── main.rs             # 程序入口
│   ├── proxy/              # 代理逻辑
│   ├── handlers/           # HTTP 处理
│   │   ├── auth.rs        # 认证处理
│   │   ├── channel.rs     # 渠道管理
│   │   ├── user.rs        # 用户管理
│   │   ├── token.rs       # API密钥
│   │   ├── dashboard.rs   # 仪表盘
│   │   └── logs.rs        # 日志审计
│   ├── static/             # 静态资源
│   │   └── build.rs       # 前端嵌入
│   ├── middleware/         # 中间件
│   ├── models/             # 数据模型
│   ├── routes/             # 路由定义
│   └── services/           # 业务逻辑
├── frontend/              # 前端项目
│   ├── package.json
│   ├── package-lock.json
│   ├── tsconfig.json
       ...前端源码
```

---

## 🔧 Cargo.toml 配置

```
[package]
name = "aigx"
version = "1.0.1"
edition = "2021"

[dependencies]
# Web框架
axum = "0.7"
axum-extra = { version = "0.9", features = ["query", "cookie"] }
tower = "0.5"
tower-http = { version = "0.6", features = ["fs", "compression-gzip"] }

# JSON序列化
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# 异步运行时
tokio = { version = "1", features = ["full"] }
tokio-util = "0.7"

# 静态文件嵌入
rust-embed = "8.0"
mime = "0.3"
mime_guess = "2.0"

# 数据库
rusqlite = { version = "0.32", features = ["bundled"] }
sea-orm = { version = "1.1", features = ["sqlite"] }

# 认证与密码
jsonwebtoken = "9.3"
argon2 = "0.5"

# Redis缓存（可选）
redis = { version = "0.25", features = ["tokio-comp"], optional = true }

# 限流
lru = "0.12"

# 图表解析
tiktoken-rs = "0.5"

# Etag生成
etag = "0.2"

# 日志
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }

# CLI参数解析
clap = { version = "4.5", features = ["derive"] }

# WebSocket（用于实时更新）
ws = "0.12"
futures-util = { version = "0.3", features = [" compat"] }

[features]
default = ["cityhash", "redis"]
embed-frontend = []

[profile.release]
strip = true
opt-level = "z"       # 最小化大小优先
lto = true            # 链接时优化
codegen-units = 1     # 更好的优化
panic = "abort"       # 移除panic处理代码

[build-dependencies]
static-files = "0.9"
```

---

## 🏗️ 前端嵌入系统设计

### 自动化构建流程
```rust
// src/static/build.rs - 前端构建嵌入器
use std::path::PathBuf;
use std::process::Command;

fn main() {
    // 1. 配置静态资源路径
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    
    // 2. 如果启用embed-features，编译前端
    #[cfg(feature = "embed-frontend")]
    {
        println!("cargo:rerun-if-changed=frontend/package.json");
        
        let frontend_out = out_dir.clone();
        
        // 构建 TypeScript 前端项目
        let status = Command::new("npm")
            .args(["--prefix", "frontend", "run", "build"])
            .status()
            .expect("Failed to build frontend");
        
        if !status.success() {
            eprintln!("❌ Frontend build failed!");
            std::process::exit(1);
        }
        
        println!("✅ Frontend built successfully");
        
        // 嵌入构建产物
        rust_embed::RustEmbed::include_directory_assets!(out_dir);
    }
    
    println!("cargo:rustc-env=FRONTEND_LOADED={}", has_embedded_frontend());
}

fn has_embedded_frontend() -> &'static str {
    #[cfg(feature = "embed-frontend")]
    {
        "true"
    }
    #[cfg(not(feature = "embed-frontend"))]
    {
        "false"
    }
}
```

---

## 🌐 HTTP 服务配置

```rust
// src/main.rs - 程序入口
use axum::{
    Router,
    routing::{get, post, put, delete, get_all},
    middleware::from_fn,
    Extract,
};
use axum_extra::{
    routing::StaticFiles,
    StaticDir,
    TypedHeader,
    cors::Any,
};
use tower_http::{
    compression::GzipLayer,
    headers::StrictIfNoneMatch,
    cors::CorsLayer,
    serve::ServeDir,
};

// 导入前端静态资源
#[cfg(feature = "embed-frontend")]
rust_embed::RustEmbed! { folder = "assets/frontend_dist" }

use aigx::handlers;
use aigx::middleware::auth::AuthMiddleware;
use aigx::services::dashboard_service::HealthChecker;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. 初始化数据库
    let db_path = init_database()?;
    
    // 2. 配置路由
    let app = Router::new()
        // 健康检查
        .route("/health", get(health_check))
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        
        // 前端路由 - SPA fallback
        .fallback(frontend_handler)
        
        // API路由 - 需要认证
        .route_layer(from_fn(AuthMiddleware::new))
        .merge(api_routes())
        
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any)
        )
        .layer(GzipLayer::new())
        .with_state(HealthChecker::new(db_path.clone()));

    // 3. 启动服务器
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    
    println!("🚀 AIGX Server running on http://127.0.0.1:8080");
    if cfg!(feature = "--release") {
        println!("📦 Compiled in release mode");
    }
    
    axum::serve(listener, app).await?;
    
    Ok(())
}

async fn health_check() -> &'static str {
    "AIGX Health Check: OK"
}

#[cfg(feature = "embed-frontend")]
async fn frontend_handler() -> rust_embed::RustEmbed {
    // 返回嵌入的前端静态资源
    // RustEmbed会自动处理index.html和SPA路由fallback
    rust_embed::RustEmbed.into()
}

#[cfg(not(feature = "embed-frontend"))]
async fn frontend_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::INTERNAL_SERVER_ERROR)
        .body("Frontend not embedded. Please build with --features embed-frontend")
        .unwrap()
}
```

---

## 🛣️ API 路由配置

```rust
// src/routes/mod.rs
use axum::{Router, routing, Extension};
use aigx::handlers;
use aigx::services::rate_limit::RateLimiter;

pub fn create_routes(rate_limiter: Extension<RateLimiter>) -> Router {
    Router::new()
        // 认证相关
        .route("/api/auth", routing::get(handlers::auth::get_api_keys))
        
        // 用户管理
        .route("/api/users", routing::get(handlers::user::list_all))
            .route("/api/users/me", routing::
```

## 🎯 前端-Hook集成方案

```html
<!-- frontend/index.html -->
<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>AIGX - AI Gateway Admin</title>
    
    <!-- Rust Backend 适配器注入 -->
    <script>
    // 通过Proxy拦截fetch请求，确保只访问正确的API路径
    // 当访问 /v1/* 路径时，会自动路由到 Rust Backend
    window.__API_BASE__ = '/api';
    window.__UPSTREAM_BASE__ = '/v1';
    
    // UI 风格注入
    document.documentElement.classList.add('dark'); // 默认暗色主题

    // Rust 能识别的环境变量配置
    const RUST_CONFIG = {
        api_base: window.__API_BASE__,
        aigx_version: "1.0.1",
        frontend_enabled: true,
        theme: "dark"
    };
    
    // 导出给AIGX后端读取
    window.RUST_CONFIG = JSON.stringify(RUST_CONFIG);
    </script>
    
    <!-- Tailwind CSS (Vite处理) -->
    <link rel="stylesheet" href="/react-embed.css">
    <!-- 动态注入主题样式 -->
    <link rel="stylesheet" href="/theme.css">
    
    <!-- 预加载字体（如果Rust内嵌版本使用CDN） -->
</head>
<body>
    <div id="root">
        <script src="/react-embed.js"></script>
        <!-- React 19 应用入口在 Rust 编译时嵌入 -->
    </div>
</body>
</html>
```

## 🚀 部署与验证

### 环境变量配置
```bash
# 使用嵌入前端版本运行
RUST_LOG=info ./aigx \
  --host 0.0.0.0 \
  --port 8080 \
  --data-dir /path/to/data \
  --feature embed-frontend
  
# 或者
./aigx \
  --launch embed-frontend=false \
  --host 0.0.0.0 \
  --port 8080 \
  --data-dir /path/to/data
```

### Docker 构建
```dockerfile
# Dockerfile
FROM rust:1.75 as builder

# 安装Node.js依赖
RUN apt-get update && apt-get install -y nodejs npm

WORKDIR /app

# 复制 Cargo 配置
COPY Cargo.* ./

# 构建阶段
RUN cargo build \
    --release \
    --bin aigx-w-frontend \
    --features embed-frontend

# 运行阶段
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 从构建阶段复制二进制
COPY --from=builder /app/target/release/aigx-w-frontend /app/aigx

# 健康检查
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD wget --no-verbose --tries=1 --spider http://localhost:8080/health || exit 1

EXPOSE 8080

# Rust后端默认从6868端口获取部署配置，支持运行时控制，例如
AWS_LAMBDA_FUNCTION_TIMEOUT=300 \
AWS_LAMBDA_FUNCTION_MEMORY_SIZE=512 \
./aigx \
  --embed-frontend=false \
  --host 0.0.0.0 \
  --port 8080 \
  --config "/app/config.toml"
```

## ✅ 验证清单

```bash
# 1. 本地测试
cargo build --release --features embed-frontend
./target/release/aigx-w-frontend
# 浏览器访问 http://127.0.0.1:8080

# 2. 环境变量测试
./aigx --embed-frontend feature-check
# 应该输出前端已成功嵌入的确认信息

# 3. 浏览器控制台验证
# 检查: window.__RUST_CONFIG__ 是否存在
# 检查: /api/* 路径是否能正确路由到后端
# 检查: 前端资源是否能被正确加载
```

这将确保：
1. 所有前端和Rust编译在GitHub Actions中完成
2. Rust后端仍然保持核心Rust优势
3. 现代前端体验正常
4. 部署简单（单一可执行文件）