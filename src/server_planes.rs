//! 管理面 / 数据面端口分离（对标 Agent2API 2.7.0）。
//!
//! 默认（`server.admin_bind` 未设置）：单监听、单 Router，行为与既往完全一致。
//! 设置 `admin_bind`（如 `127.0.0.1:9528`）后：
//!
//! - **主端口（数据面）**：只放行 `/v1/*` 与无前缀兼容路由、健康探针
//!   （`/livez` `/readyz` `/health`）、`/metrics`，以及必须由外部第三方
//!   触达的**支付回调**（易支付 notify/return、Stripe webhook）与
//!   AutoClaw 浏览器 OAuth 回调（其回调地址由主端口拼出）。
//! - **管理端口（admin_bind）**：`/api/*`、`/api-docs/*`、`/swagger-ui`、
//!   前端静态资源（SPA / `/assets/*` / logo 等）。
//!
//! 实现方式：路由表**不拆**，继续复用同一个 `build_router`，只在两个监听器
//! 最外层各套一层「平面守卫」中间件——不属于该平面的路径直接 404 JSON。
//! 这样路由定义保持单处（避免 3000 行路由表复制/漂移），并且未配置
//! `admin_bind` 时连这一层都不会挂上，零行为变化。
//!
//! 注意：管理面鉴权（会话 / ALTCHA / 限流）原样保留在 `/api/*` handler 内，
//! 只是「哪些端口能到达它们」被本模块收窄；CORS / 压缩 / 请求体上限 /
//! in-flight 计数等中间件仍由 `build_router` 统一挂在两个平面上。

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;

use crate::config::ServerConfig;
use crate::health::LivezState;

/// 请求所属的监听平面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plane {
    /// 主端口：数据面 + 探针 + 外部回调。
    Data,
    /// `admin_bind`：管理 API + 管理前端静态资源。
    Admin,
}

/// 无前缀兼容路由（客户端 base_url 填根地址时使用，与 main.rs 双挂一致）。
const COMPAT_PATHS: &[&str] = &[
    "/chat/completions",
    "/responses",
    "/completions",
    "/embeddings",
    "/rerank",
    "/images/generations",
    "/images/edits",
    "/audio/transcriptions",
    "/audio/translations",
    "/audio/speech",
    "/models",
    "/messages",
    "/messages/count_tokens",
];

/// 必须留在数据面可达端口的外部回调（第三方服务器 / 浏览器直连）。
///
/// - 易支付异步通知与同步跳转：`notify_url` / `return_url` 由支付网关回呼；
/// - Stripe webhook：Stripe 服务器回呼；
/// - AutoClaw 浏览器 OAuth 回调：`callback_url` 由 `config.server.port` 拼出
///   （见 `api::admin::account_login::handle_browser_oauth_callback`），
///   故必须落在主端口。
const DATA_PLANE_CALLBACKS: &[&str] = &[
    "/api/user/epay/notify",
    "/api/user/epay/return",
    "/api/user/stripe/webhook",
    "/auth/callback-zai",
    "/auth/callback-google",
];

/// 健康探针与指标：两个平面都放行。
///
/// 主端口给负载均衡 / K8s 探针与 Prometheus；管理端口也允许，便于运维在
/// 隔离端口上直接做健康检查（只读端点，不扩大攻击面）。
const PROBES: &[&str] = &["/livez", "/readyz", "/health", "/metrics"];

fn path_matches(path: &str, exact_or_prefix: &str) -> bool {
    path == exact_or_prefix || (exact_or_prefix.ends_with('/') && path.starts_with(exact_or_prefix))
}

/// 该路径是否属于数据面（不含探针 / 外部回调）。
pub fn is_data_path(path: &str) -> bool {
    if path == "/v1" || path.starts_with("/v1/") {
        return true;
    }
    // 带参数的无前缀路由 `/models/:model`：前缀匹配
    COMPAT_PATHS
        .iter()
        .any(|p| path_matches(path, p) || path.starts_with(&format!("{p}/")))
}

/// 该路径是否必须由数据面承载（含探针与外部回调）。
pub fn is_data_plane(path: &str) -> bool {
    is_data_path(path)
        || PROBES.contains(&path)
        || DATA_PLANE_CALLBACKS.contains(&path)
        // 探针/回调整理出的带尾斜杠形态（个别反代会补 `/`）
        || PROBES.contains(&path.trim_end_matches('/'))
        || DATA_PLANE_CALLBACKS.contains(&path.trim_end_matches('/'))
}

/// 该请求路径是否允许在指定平面处理。
pub fn plane_allows(plane: Plane, path: &str) -> bool {
    match plane {
        Plane::Data => is_data_plane(path),
        // 管理面 = 除「数据面专属」之外的一切：/api/*、/api-docs/*、
        // /swagger-ui、SPA 路由、/assets/*、logo 等。外部回调与探针同样
        // 允许（它们在两个端口都能工作，不构成越权：无会话的公开端点或
        // 只读探针）。
        Plane::Admin => !is_data_path(path) || DATA_PLANE_CALLBACKS.contains(&path),
    }
}

/// 404 JSON（与 `web::serve_static_files` 对未匹配 `/api/*` 的口径一致）。
fn plane_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        [("content-type", "application/json")],
        r#"{"success":false,"error":"Not found"}"#,
    )
        .into_response()
}

async fn data_plane_guard(req: axum::extract::Request, next: Next) -> Response {
    if plane_allows(Plane::Data, req.uri().path()) {
        next.run(req).await
    } else {
        plane_not_found()
    }
}

async fn admin_plane_guard(req: axum::extract::Request, next: Next) -> Response {
    if plane_allows(Plane::Admin, req.uri().path()) {
        next.run(req).await
    } else {
        plane_not_found()
    }
}

/// 给路由套上平面守卫（最外层：先判平面，未命中直接 404，不进业务中间件）。
pub fn with_plane(app: Router, plane: Plane) -> Router {
    match plane {
        Plane::Data => app.layer(middleware::from_fn(data_plane_guard)),
        Plane::Admin => app.layer(middleware::from_fn(admin_plane_guard)),
    }
}

/// 校验 `admin_bind` 形如 `host:port`（支持 `[::1]:9528`），且不能与主监听相同。
pub fn validate_admin_bind(raw: &str, main_host: &str, main_port: u16) -> Result<String, String> {
    let bind = raw.trim();
    if bind.is_empty() {
        return Err("地址不能为空".to_string());
    }
    if bind.chars().any(char::is_whitespace) {
        return Err("地址不能包含空白字符".to_string());
    }
    let (host, port) = match bind.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => return Err("格式应为 host:port（如 127.0.0.1:9528）".to_string()),
    };
    if host.is_empty() {
        return Err("缺少主机部分".to_string());
    }
    let port: u16 = port
        .parse()
        .map_err(|_| "端口必须是 1-65535 的整数".to_string())?;
    if port == 0 {
        return Err("端口必须在 1-65535 之间".to_string());
    }
    // 与主监听完全相同：两个监听器会抢同一个端口，启动必然失败，提前给出
    // 可读错误；仅当两边都能解析成 SocketAddr 时才比较（主机名不做 DNS 解析）。
    if let (Ok(admin_addr), Ok(main_addr)) = (
        bind.parse::<SocketAddr>(),
        format!("{main_host}:{main_port}").parse::<SocketAddr>(),
    ) {
        if admin_addr == main_addr {
            return Err("admin_bind 不能与主监听地址相同".to_string());
        }
    }
    let main_text = format!("{main_host}:{main_port}");
    if bind == main_text {
        return Err("admin_bind 不能与主监听地址相同".to_string());
    }
    Ok(bind.to_string())
}

async fn wait_shutdown(mut rx: tokio::sync::broadcast::Receiver<()>) {
    let _ = rx.recv().await;
}

/// 双监听 serve（`admin_bind` 已配置时）。
///
/// - 先 bind 两个监听器：任一失败立即返回错误（配置错误 fail-fast）；
/// - 收到一次关停信号后：标记 draining → 广播给两个 serve → 各自 graceful
///   shutdown（等待在途请求），总等待上限 `drain_timeout`；
/// - 任一 serve 运行期出错也立即返回错误。
///
/// `shutdown` 由调用方传入（main.rs 的 `shutdown_signal()`），保证信号处理
/// 与单端口路径同源。
pub async fn serve_split(
    server: &ServerConfig,
    admin_bind: &str,
    app: Router,
    livez: Arc<LivezState>,
    shutdown: impl Future<Output = ()> + Send + 'static,
    drain_timeout: Duration,
) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let main_addr = format!("{}:{}", server.host, server.port);
    let admin_addr = validate_admin_bind(admin_bind, &server.host, server.port)
        .map_err(|e| anyhow::anyhow!("admin_bind 配置非法：{e}"))?;

    let data_listener = tokio::net::TcpListener::bind(&main_addr)
        .await
        .with_context(|| format!("无法监听数据面地址 {main_addr}"))?;
    let admin_listener = tokio::net::TcpListener::bind(&admin_addr)
        .await
        .with_context(|| format!("无法监听管理面地址 {admin_addr}（admin_bind）"))?;
    tracing::info!(
        "Data plane listening on {main_addr}; admin plane listening on {admin_addr} \
         (/v1 + probes + payment callbacks on {}; /api + SPA on {})",
        main_addr,
        admin_addr
    );

    let data_app = with_plane(app.clone(), Plane::Data);
    let admin_app = with_plane(app, Plane::Admin);

    // 先建全部接收端再 spawn 信号任务，避免「信号恰好落在订阅之前」被漏掉。
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(4);
    let data_rx = shutdown_tx.subscribe();
    let admin_rx = shutdown_tx.subscribe();
    let mut deadline_rx = shutdown_tx.subscribe();

    let signal_livez = livez;
    let signal_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        shutdown.await;
        tracing::info!("Shutdown signal received, marking draining...");
        signal_livez.mark_shutting_down();
        let _ = signal_tx.send(());
    });

    let data_serve = axum::serve(
        data_listener,
        data_app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(wait_shutdown(data_rx));
    let admin_serve = axum::serve(
        admin_listener,
        admin_app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(wait_shutdown(admin_rx));

    let mut data_fut = Box::pin(std::future::IntoFuture::into_future(data_serve));
    let mut admin_fut = Box::pin(std::future::IntoFuture::into_future(admin_serve));
    let drain = async move {
        // 信号未到前不启用超时；`Err` 表示信号任务在发送前已结束。
        if deadline_rx.recv().await.is_err() {
            std::future::pending::<()>().await;
        }
        tokio::time::sleep(drain_timeout).await;
    };

    tokio::select! {
        (data_res, admin_res) = async { tokio::join!(&mut data_fut, &mut admin_fut) } => {
            data_res?;
            admin_res?;
        }
        _ = drain => {
            tracing::warn!(
                "Graceful shutdown timed out after {}s of draining; forcing exit with in-flight requests pending",
                drain_timeout.as_secs()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_plane_carries_v1_and_compat_routes() {
        for path in [
            "/v1/chat/completions",
            "/v1/models",
            "/v1/models/gpt-4o",
            "/v1/messages/count_tokens",
            "/chat/completions",
            "/messages",
            "/messages/count_tokens",
            "/models",
            "/models/gpt-4o",
            "/embeddings",
            "/images/generations",
            "/audio/speech",
        ] {
            assert!(
                plane_allows(Plane::Data, path),
                "数据面应放行 {path}（含无前缀兼容路由）"
            );
        }
    }

    #[test]
    fn data_plane_carries_probes_and_external_callbacks() {
        for path in [
            "/livez",
            "/readyz",
            "/health",
            "/metrics",
            "/api/user/epay/notify",
            "/api/user/epay/return",
            "/api/user/stripe/webhook",
            "/auth/callback-zai",
            "/auth/callback-google",
        ] {
            assert!(plane_allows(Plane::Data, path), "数据面应放行 {path}");
        }
    }

    #[test]
    fn data_plane_rejects_admin_surface() {
        for path in [
            "/api/auth/login",
            "/api/tokens",
            "/api/settings",
            "/api/public/branding",
            "/api/agent/sessions",
            "/api-docs/openapi.json",
            "/swagger-ui",
            "/",
            "/login",
            "/assets/index-abc123.js",
            "/logo.png",
        ] {
            assert!(
                !plane_allows(Plane::Data, path),
                "数据面不得暴露管理面路径 {path}"
            );
        }
    }

    #[test]
    fn admin_plane_rejects_data_surface_but_serves_spa() {
        for path in ["/v1/chat/completions", "/chat/completions", "/v1/models"] {
            assert!(
                !plane_allows(Plane::Admin, path),
                "管理面不得暴露数据面路径 {path}"
            );
        }
        for path in [
            "/api/auth/login",
            "/api/tokens",
            "/api/public/branding",
            "/api-docs/openapi.json",
            "/swagger-ui",
            "/",
            "/login",
            "/assets/app.js",
            "/logo.png",
            "/api/user/epay/notify",
            "/metrics",
        ] {
            assert!(plane_allows(Plane::Admin, path), "管理面应放行 {path}");
        }
    }

    /// 前缀匹配不能误伤：`/models` 兼容路由不得把 `/model-settings` 当成数据面。
    #[test]
    fn compat_prefix_does_not_overmatch_siblings() {
        assert!(!plane_allows(Plane::Data, "/model-settings"));
        assert!(!plane_allows(Plane::Data, "/modelsx"));
        assert!(!plane_allows(Plane::Data, "/v1beta/models"));
        assert!(plane_allows(Plane::Data, "/v1/models"));
    }

    #[test]
    fn admin_bind_validation() {
        for ok in [
            "127.0.0.1:9528",
            "0.0.0.0:9528",
            "[::1]:9528",
            "localhost:9528",
        ] {
            assert!(
                validate_admin_bind(ok, "127.0.0.1", 8080).is_ok(),
                "应接受 {ok}"
            );
        }
        for bad in [
            "",
            "9528",
            ":9528",
            "127.0.0.1:",
            "127.0.0.1:0",
            "127.0.0.1:70000",
            "127.0.0.1:1x",
            "127.0.0.1:9528 extra",
        ] {
            assert!(
                validate_admin_bind(bad, "127.0.0.1", 8080).is_err(),
                "必须拒绝 {bad:?}"
            );
        }
        // 与主监听相同（含通配地址归一后的同一 SocketAddr）
        assert!(validate_admin_bind("127.0.0.1:8080", "127.0.0.1", 8080).is_err());
        assert!(validate_admin_bind("0.0.0.0:8080", "0.0.0.0", 8080).is_err());
        assert!(validate_admin_bind("127.0.0.1:8081", "127.0.0.1", 8080).is_ok());
    }
}
