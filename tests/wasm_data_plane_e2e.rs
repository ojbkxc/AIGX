//! Wasm 插件数据面端到端（真实二进制 + 真跑 wasmtime + 真实 HTTP）。
//!
//! 覆盖 `docs/a2a-alignment/README.md` §六.10 与 §六·六.1/.2：
//!
//! 1. 真实 `aigx` 二进制（临时 HOME + 临时 data dir，harness 与
//!    `tests/e2e/499_disconnect_billing.rs` 同款）；
//! 2. 管理面安装一个**真流式** Wasm 插件包（zip 由本地 HTTP 源供下载，
//!    `plugin.wasm` 是 `src/plugin_host/wasm/fixture.rs` 的编码器产物、
//!    由 `tests/e2e/fixtures/stream_plugin.wasm.b64` 固定；lib 单测
//!    `fixture::tests::streaming_module_matches_checked_in_e2e_artifact`
//!    保证两者不漂移）；
//! 3. 启用 → 建 `plugin` 渠道（`plugin_provider` 指向 wasm.json 的 provider）
//!    → 建 API Key → 打 `/v1/chat/completions`；
//! 4. 断言非流式聚合出 "hello"（两段 chunk "hel"+"lo"），流式 SSE 同时
//!    出现 "hel"/"lo"；未声明的模型拿不到 200（路由真实性的反向对照）。
//!
//! ## Windows 跳过说明
//!
//! `dirs::home_dir()` 在 Windows 走 `SHGetKnownFolderPath`、**忽略 HOME 环境
//! 变量**（与 499 harness 同一条已知限制，见其 `spawn_server` 注释），临时
//! HOME 无法隔离。本用例在 Windows 直接返回（打印跳过原因），以 CI(linux) 为准。

use std::io::{Cursor, Write};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// wasm 夹具的 provider id（必须与 `stream_plugin.wasm.b64` 内的 init 应答一致）。
const PROVIDER: &str = "wasm-e2e-provider";
/// 渠道声明的模型名（同时出现在夹具 SSE 帧里）。
const MODEL: &str = "wasm-e2e-model";
const PACK_ID: &str = "wasm-e2e-pack";
const KEY: &str = "sk-wasm-e2e-0001";
const EMAIL: &str = "admin@gmail.com";
const CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
/// 数据面（`/v1/*`）请求的超时。
///
/// 不能复用 [`CONTROL_TIMEOUT`]：控制面都是读配置、建密钥这类毫秒级操作，60s
/// 足够；而数据面第一次打 wasm 渠道会触发 wasmtime 的**首次编译 + init 握手**，
/// 在共享 CI runner 上实测会超过 60s，于是用例报一个毫不相关的 `TimedOut`，
/// 看不出是慢还是挂。给它自己的预算。
const DATA_PLANE_TIMEOUT: Duration = Duration::from_secs(180);

// ─── wasm 插件包（真实二进制，base64 制品由 lib 单测钉住） ───

fn plugin_wasm() -> Vec<u8> {
    let encoded = include_str!("e2e/fixtures/stream_plugin.wasm.b64");
    B64.decode(encoded.trim()).expect("decode wasm artifact")
}

fn pack_zip() -> Vec<u8> {
    let pack = json!({
        "id": PACK_ID,
        "name": "Wasm E2E",
        "version": "0.0.1",
        // 必须等于宿主 PROTOCOL_VERSION（install.rs 校验）
        "protocol": 1,
        "entry": { "wasm32-unknown-unknown": "plugin.wasm" },
        "providers": [{ "id": PROVIDER, "label": "Wasm E2E" }],
    });
    // chat.stream.v1：conversation 里的 stream.head/chunk；
    // accounts.cas.v1：init 应答 capabilities.chat=true 的前置声明。
    let wasm = json!({
        "abi": 1,
        "requires": ["chat.stream.v1", "accounts.cas.v1"],
        "origins": [],
        "memory_mib": 16,
        "concurrency": 2,
    });
    let mut out = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
        let options: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (name, data) in [
            ("pack.json", pack.to_string().into_bytes()),
            ("wasm.json", wasm.to_string().into_bytes()),
            ("plugin.wasm", plugin_wasm()),
        ] {
            writer.start_file(name, options).unwrap();
            writer.write_all(&data).unwrap();
        }
        writer.finish().unwrap();
    }
    out
}

// ─── 本地 zip 源（安装走生产 `install_from_url`） ───

fn spawn_zip_server(bytes: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind zip server");
    listener.set_nonblocking(true).expect("nonblocking");
    let addr = listener.local_addr().expect("zip server addr");
    let shared = Arc::new(bytes);
    let app = Router::new()
        .route(
            "/pack.zip",
            get(|State(bytes): State<Arc<Vec<u8>>>| async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/zip")
                    .body(Body::from(bytes.as_ref().clone()))
                    .expect("zip response")
            }),
        )
        .with_state(shared);
    let handle = tokio::spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).expect("adopt listener");
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), handle)
}

// ─── 网关实例（harness 与 499 同款） ───

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    listener.local_addr().expect("local_addr").port()
}

/// 失败诊断：把 server.log 尾部按**时间正序**交出来。
///
/// 此前是 `rev().take(30)`：倒序贴出来最难读，而且 30 行装不下
/// 「启动 → 安装/启用 → 数据面」这条链 —— CI 上两次红灯都只看到「已启用」
/// 那一行，看不出请求是没进来还是进来后挂住。一个用例生命周期内这份日志只有
/// 几十行，直接给最后 200 行正序。
fn server_log_tail(log_path: &std::path::Path) -> String {
    let text = std::fs::read_to_string(log_path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(200);
    lines[start..].join("\n")
}

/// 从 server.log 提取首启一次性管理员密码（随机化后仅打印到日志）。
fn initial_password_from_log(log_path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(log_path).ok()?;
    const B64URL: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rest: &str = &text;
    while let Some(index) = rest.find("Aigx-") {
        let tail = &rest[index + 5..];
        let len = tail
            .bytes()
            .take_while(|byte| B64URL.contains(byte))
            .count();
        if len == 24 {
            return Some(format!("Aigx-{}", &tail[..24]));
        }
        rest = &rest[index + 5..];
    }
    None
}

async fn spawn_server(home: &std::path::Path, log_path: &std::path::Path) -> (ChildGuard, String) {
    let port = free_port();
    let data_dir = home.join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let aigx_dir = home.join(".aigx");
    std::fs::create_dir_all(&aigx_dir).expect("create .aigx dir");
    let cfg_data_dir = data_dir.to_string_lossy().replace('\\', "/");
    std::fs::write(
        aigx_dir.join("config.toml"),
        format!("[server]\nhost = \"127.0.0.1\"\nport = {port}\ndata_dir = \"{cfg_data_dir}\"\n"),
    )
    .expect("write config.toml");

    let exe = env!("CARGO_BIN_EXE_aigx");
    let mut cmd = Command::new(exe);
    cmd.env("HOME", home);
    cmd.env("AIGX_SERVER__DATA_DIR", &data_dir);
    cmd.env("AIGX_SERVER__PORT", port.to_string());
    cmd.env("AIGX_SERVER__HOST", "127.0.0.1");
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env_remove(key);
    }
    let out = std::fs::File::create(log_path).expect("create server log");
    cmd.stdout(out.try_clone().expect("clone log"));
    cmd.stderr(out);
    let child = ChildGuard(cmd.spawn().expect("spawn aigx binary"));
    let base = format!("http://127.0.0.1:{port}");

    let client = client();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!(
                "网关 90s 未就绪。server.log 尾部：\n{}",
                server_log_tail(log_path)
            );
        }
        if client
            .get(format!("{base}/api/monitor/healthz"))
            .send()
            .await
            .is_ok()
        {
            return (child, base);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ─── 管理面 bootstrap（与 499 同款） ───

fn install_rustls_provider() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("install rustls ring provider");
    });
}

fn client() -> reqwest::Client {
    install_rustls_provider();
    reqwest::Client::builder()
        .no_proxy()
        .timeout(CONTROL_TIMEOUT)
        .build()
        .expect("reqwest client")
}

async fn solve_captcha(c: &reqwest::Client, base: &str) -> String {
    let value: Value = c
        .get(format!("{base}/api/auth/captcha"))
        .send()
        .await
        .expect("captcha")
        .json()
        .await
        .expect("captcha json");
    let data = value.get("data").cloned().unwrap_or(value);
    let salt = data["salt"].as_str().unwrap_or_default().to_string();
    let challenge = data["challenge"].as_str().unwrap_or_default().to_string();
    let signature = data["signature"].as_str().unwrap_or_default().to_string();
    let max = data["maxnumber"].as_u64().unwrap_or(1_000_000);
    for number in 0..=max {
        let mut hasher = Sha256::new();
        hasher.update(format!("{salt}{number}"));
        if hex::encode(hasher.finalize()) == challenge {
            let payload = json!({
                "algorithm": "SHA-256", "challenge": challenge, "salt": salt,
                "signature": signature, "number": number
            });
            return B64.encode(payload.to_string());
        }
    }
    panic!("ALTCHA solve failed");
}

async fn login_once(c: &reqwest::Client, base: &str, password: &str) -> Option<String> {
    let captcha = solve_captcha(c, base).await;
    let response = c
        .post(format!("{base}/api/auth/login"))
        .json(&json!({ "email": EMAIL, "password": password, "captcha": captcha }))
        .send()
        .await;
    match response {
        Ok(resp) if resp.status().is_success() => {
            let value: Value = resp.json().await.expect("login json");
            Some(
                value
                    .get("data")
                    .and_then(|data| data.get("token"))
                    .or_else(|| value.get("token"))
                    .and_then(|token| token.as_str())
                    .unwrap_or_default()
                    .to_string(),
            )
        }
        Ok(resp) => {
            let body = resp.text().await.unwrap_or_default();
            if body.contains("账号或密码错误") || body.contains("请先完成机器人校验")
            {
                None
            } else {
                panic!("login 异常响应：{body}");
            }
        }
        Err(error) => panic!("login 请求失败：{error}"),
    }
}

async fn admin_token(c: &reqwest::Client, base: &str, log_path: &std::path::Path) -> String {
    let initial = initial_password_from_log(log_path);
    let mut token = None;
    if let Some(password) = initial.as_deref() {
        token = login_once(c, base, password).await;
    }
    let mut token = token.unwrap_or_else(|| {
        panic!(
            "新实例初始密码登录失败（日志尾部：\n{}）",
            server_log_tail(log_path)
        )
    });
    // 新库默认 must_change_password：先改密再重登
    if let Some(password) = initial.as_deref() {
        let resp = c
            .post(format!("{base}/api/auth/change-password"))
            .bearer_auth(&token)
            .json(&json!({ "old_password": password, "new_password": "Aigx#WasmE2E2026" }))
            .send()
            .await
            .expect("change-password");
        if !resp.status().is_success() {
            panic!(
                "change-password 失败：{}",
                resp.text().await.unwrap_or_default()
            );
        }
        token = login_once(c, base, "Aigx#WasmE2E2026")
            .await
            .expect("改密后重登失败");
    }
    token
}

async fn post_expect_ok(c: &reqwest::Client, url: String, token: &str, body: Value) -> Value {
    let resp = c
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("post");
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert!(status.is_success(), "请求失败 {status}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

// ─── 用例 ───

#[tokio::test]
async fn wasm_pack_serves_v1_chat_completions() {
    if cfg!(windows) {
        eprintln!(
            "SKIP: Windows 下 dirs::home_dir 忽略 HOME（499 harness 同限制），本用例以 CI(linux) 为准"
        );
        return;
    }

    let (_zip_base, zip_server) = spawn_zip_server(pack_zip());
    let tmp = tempfile::tempdir().expect("tempdir");
    let log_path = tmp.path().join("server.log");
    let (child, base) = spawn_server(tmp.path(), &log_path).await;
    let c = client();
    let token = admin_token(&c, &base, &log_path).await;

    // 1) 安装 → 2) 启用（生产路径；启用会真跑 wasmtime 编译 + init 握手）
    let installed = post_expect_ok(
        &c,
        format!("{base}/api/plugins/install"),
        &token,
        json!({
            "url": format!("{_zip_base}/pack.zip")
        }),
    )
    .await;
    assert_eq!(installed["data"]["pack"]["id"], PACK_ID, "{installed}");
    let enabled = post_expect_ok(
        &c,
        format!("{base}/api/plugins/enable"),
        &token,
        json!({
            "id": PACK_ID, "enabled": true
        }),
    )
    .await;
    assert_eq!(enabled["data"]["enabled"], true, "{enabled}");

    // 3) Plugin 渠道（plugin_provider 指向 wasm.json 的 provider）
    let created = post_expect_ok(
        &c,
        format!("{base}/api/channels"),
        &token,
        json!({
            "name": "wasm-e2e-channel",
            "channel_type": "plugin",
            "base_url": "",
            "plugin_provider": PROVIDER,
            "priority": 100,
            "weight": 100,
            "status": "enabled",
            "models": [MODEL],
        }),
    )
    .await;
    assert!(created["success"].as_bool().unwrap_or(false), "{created}");

    // 模型定价（usage 有值，避免计费口径干扰）
    let priced = c
        .post(format!("{base}/api/pricing"))
        .bearer_auth(&token)
        .json(&json!({
            "model_name": MODEL, "input_price": 1, "output_price": 1, "price_type": "token"
        }))
        .send()
        .await
        .expect("pricing");
    assert!(
        priced.status().is_success(),
        "pricing: {}",
        priced.text().await.unwrap_or_default()
    );

    // 4) API Key
    let created_key = post_expect_ok(
        &c,
        format!("{base}/api/tokens"),
        &token,
        json!({
            "name": "wasm-e2e-key", "custom_key": KEY
        }),
    )
    .await;
    assert_eq!(created_key["success"], true, "{created_key}");

    // 5) 非流式：两段 chunk 聚合为 "hello"（resolve.rs 的 wasm 分支 → PluginBridge）
    // 耗时是分岔点：≈ DATA_PLANE_TIMEOUT（180s）= 请求挂住（handler 不返回）；
    // 亚秒级失败 = 传输层当场断开（服务进程已死 / 连接被重置）。两者根因不同，
    // 不报耗时的话 CI 只吐一个 `error sending request`，无法区分。
    let started = std::time::Instant::now();
    let resp = c
        .post(format!("{base}/v1/chat/completions"))
        .timeout(DATA_PLANE_TIMEOUT)
        .bearer_auth(KEY)
        .json(&json!({
            "model": MODEL,
            "messages": [{ "role": "user", "content": "ping-wasm-e2e" }],
            "stream": false,
        }))
        .send()
        .await
        .unwrap_or_else(|e| {
            panic!(
                "chat completions 失败（耗时 {:?}）: {e}\nserver.log 尾部：\n{}",
                started.elapsed(),
                server_log_tail(&log_path)
            )
        });
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    assert_eq!(status, StatusCode::OK, "wasm 渠道必须 200: {text}");
    let body: Value = serde_json::from_str(&text).expect("chat json");
    assert_eq!(
        body["choices"][0]["message"]["content"], "hello",
        "两段 wasm chunk 必须经 PluginBridge 聚合: {body}"
    );

    // 6) 流式：SSE 正文里两段 delta 都要出现（真流式通路）
    let stream_resp = c
        .post(format!("{base}/v1/chat/completions"))
        .timeout(DATA_PLANE_TIMEOUT)
        .bearer_auth(KEY)
        .json(&json!({
            "model": MODEL,
            "messages": [{ "role": "user", "content": "ping-wasm-e2e-stream" }],
            "stream": true,
        }))
        .send()
        .await
        .unwrap_or_else(|e| {
            panic!(
                "stream chat: {e}\nserver.log 尾部：\n{}",
                server_log_tail(&log_path)
            )
        });
    assert_eq!(stream_resp.status(), StatusCode::OK);
    let sse = stream_resp.text().await.unwrap_or_default();
    assert!(
        sse.contains("hel") && sse.contains("lo"),
        "流式正文缺 delta: {sse}"
    );

    // 反向对照：未声明的模型不得 200（证明请求真的走了渠道路由）
    let unknown = c
        .post(format!("{base}/v1/chat/completions"))
        .timeout(DATA_PLANE_TIMEOUT)
        .bearer_auth(KEY)
        .json(&json!({
            "model": "wasm-e2e-model-missing",
            "messages": [{ "role": "user", "content": "nope" }],
        }))
        .send()
        .await
        .unwrap_or_else(|e| {
            panic!(
                "unknown model: {e}\nserver.log 尾部：\n{}",
                server_log_tail(&log_path)
            )
        });
    assert_ne!(
        unknown.status(),
        StatusCode::OK,
        "未声明的模型不应被路由到 wasm 渠道"
    );

    zip_server.abort();
    drop(child);
}
