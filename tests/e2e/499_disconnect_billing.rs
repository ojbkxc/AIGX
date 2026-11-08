//! #34/#47 断线 499 计费 CI 回归（方案 B：Rust 集成测试）
//!
//! 钉住的缺口（27 档清单）：
//! ① Drop 触发链：预留已建、上游请求已发出、首事件未到时客户端断开
//!    → handler future 在 await 点被 drop → AttemptStartGuard::Drop 落 499
//! ② 估算计费：settle_usage 按估算 prompt 扣款并入账（used_quota delta 对账）
//! ③ 无双计：handoff() 接管后守卫不再记账；断连请求无 200、对照请求无 499
//! ④ anthropic 面镜像（/v1/messages，anthropic.rs L493/L1183 守卫链）
//!
//! 方法学坑（全部继承自 scout harness，见 43-499-regression-plan.md §3）：
//! - response_cache 同体遮蔽（chat.rs L880）：非流式按请求体哈希缓存，
//!   同体请求第二次 0ms 命中不打上游 → 每条请求唯一消息体（`unique_content`）
//! - 双面渠道测试模型名隔离：openai 面 mock-model / anthropic 面 mock-model-anth，
//!   否则调度器随机选渠 → 499 归因漂移（scout 首轮实证 req-ca316511）
//! - quota 观测：/api/tokens 需 all=1 才见 admin 之外归属的 key
//! - mock hits=10 外部可核量：断连用例上游必须真实执行（防缓存遮蔽与白嫖双证），
//!   非自证桩
//!
//! 运行：cargo test --locked（并入 CI `test` job）。本机禁编译（无 MSVC 链接器），
//! 迭代走 CI：push → ci.py <sha> → 红了看 cilog.py 日志。

use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const KEY: &str = "sk-e147-0001";
const PASSWORD: &str = "Aigx#499Test2026";
const EMAIL: &str = "admin@gmail.com";
/// mock 上游延迟（断连窗口）：断连 1.5/2s 必须早于首字节 3s
const MOCK_DELAY: Duration = Duration::from_millis(3000);
/// 断连用例的客户端等待时长（等价 curl -m）
const DISCONNECT_AT_MS: u64 = 1500;
/// 判账等待：守卫 Drop 写日志/结算为异步落库
const SETTLE_WAIT: Duration = Duration::from_millis(2500);
/// 对照用例客户端总超时（mock 延迟 3s + 网关处理余量）
const CONTROL_TIMEOUT: Duration = Duration::from_secs(30);

// ─── mock 上游（双协议，移植自 C:\tmp\aigx-499-regress\mock-upstream.mjs）───

struct MockState {
    hits: AtomicU64,
}

fn json_response(v: Value) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(v.to_string()))
        .expect("json response")
}

fn sse_response(body: String) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .body(Body::from(body))
        .expect("sse response")
}

fn openai_sse() -> String {
    let mut out = String::new();
    for _ in 0..16 {
        let chunk = json!({
            "id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": 1700000000,
            "model": "mock-model",
            "choices": [{"index": 0, "delta": {"content": "y".repeat(16)}, "finish_reason": null}]
        });
        out.push_str(&format!("data: {chunk}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

fn anthropic_sse() -> String {
    let mut out = String::new();
    let send = |out: &mut String, event: &str, data: Value| {
        out.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    };
    send(
        &mut out,
        "message_start",
        json!({
            "type": "message_start",
            "message": {"id": "msg_mock", "type": "message", "role": "assistant", "model": "mock-model",
                        "content": [], "usage": {"input_tokens": 100, "output_tokens": 1}}
        }),
    );
    send(
        &mut out,
        "content_block_start",
        json!({
            "type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}
        }),
    );
    for _ in 0..16 {
        send(
            &mut out,
            "content_block_delta",
            json!({
                "type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": "y".repeat(16)}
            }),
        );
    }
    send(
        &mut out,
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": "end_turn", "stop_sequence": null},
            "usage": {"output_tokens": 64}
        }),
    );
    send(&mut out, "message_stop", json!({"type": "message_stop"}));
    out
}

async fn mock_handler(State(st): State<Arc<MockState>>, req: Request) -> Response {
    st.hits.fetch_add(1, Ordering::SeqCst);
    let path = req.uri().path().to_string();
    let body = axum::body::to_bytes(req.into_body(), 256 * 1024)
        .await
        .unwrap_or_default();
    let stream = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("stream").and_then(|s| s.as_bool()))
        .unwrap_or(false);
    // 延迟首字节（断连窗口）：与 node mock 的 setTimeout(finish, 3000) 同语义
    tokio::time::sleep(MOCK_DELAY).await;
    if path.ends_with("/models") {
        return json_response(
            json!({"object": "list", "data": [{"id": "mock-model", "object": "model"}]}),
        );
    }
    if path.ends_with("/messages") {
        return if stream {
            sse_response(anthropic_sse())
        } else {
            json_response(json!({
                "id": "msg_mock", "type": "message", "role": "assistant", "model": "mock-model",
                "content": [{"type": "text", "text": "x".repeat(256)}],
                "stop_reason": "end_turn", "stop_sequence": null,
                "usage": {"input_tokens": 100, "output_tokens": 64}
            }))
        };
    }
    if stream {
        return sse_response(openai_sse());
    }
    json_response(json!({
        "id": "chatcmpl-mock", "object": "chat.completion", "created": 1700000000, "model": "mock-model",
        "choices": [{"index": 0, "message": {"role": "assistant", "content": "x".repeat(256)}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 100, "completion_tokens": 64, "total_tokens": 164}
    }))
}

// ─── 网关实例（子进程）───

/// 起真实 aigx 二进制（env 配置，scout 实证口径）+ 就绪轮询。
/// Drop 时 kill 防泄漏。
struct ServerHandle {
    child: Child,
    base: String,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
}

fn server_log_tail(log_path: &std::path::Path) -> String {
    std::fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .rev()
        .take(20)
        .collect::<Vec<_>>()
        .join("\n")
}

async fn spawn_server(data_dir: &std::path::Path, log_path: &std::path::Path) -> ServerHandle {
    let port = free_port();
    let exe = env!("CARGO_BIN_EXE_aigx");
    let mut cmd = Command::new(exe);
    cmd.env("AIGX_SERVER__DATA_DIR", data_dir);
    cmd.env("AIGX_SERVER__PORT", port.to_string());
    cmd.env("AIGX_SERVER__HOST", "127.0.0.1");
    // 中间盒/代理 env 一律清掉（网关出站直连 mock；CI runner 无此问题，本机防御）
    for k in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        cmd.env_remove(k);
    }
    let out = std::fs::File::create(log_path).expect("create server log");
    cmd.stdout(out.try_clone().expect("clone log"));
    cmd.stderr(out);
    let child = cmd.spawn().expect("spawn aigx binary");
    let base = format!("http://127.0.0.1:{port}");

    let client = client();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() > deadline {
            panic!(
                "网关 60s 未就绪。server.log 尾部：\n{}",
                server_log_tail(log_path)
            );
        }
        if client
            .get(format!("{base}/api/auth/captcha"))
            .send()
            .await
            .is_ok()
        {
            return ServerHandle { child, base };
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

// ─── 管理面 bootstrap（移植自 C:\tmp\aigx-499-regress\bootstrap.mjs）───

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(CONTROL_TIMEOUT)
        .build()
        .expect("reqwest client")
}

async fn solve_captcha(c: &reqwest::Client, base: &str) -> String {
    let v: Value = c
        .get(format!("{base}/api/auth/captcha"))
        .send()
        .await
        .expect("captcha")
        .json()
        .await
        .expect("captcha json");
    let d = v.get("data").cloned().unwrap_or(v);
    let salt = d["salt"].as_str().unwrap_or_default().to_string();
    let challenge = d["challenge"].as_str().unwrap_or_default().to_string();
    let signature = d["signature"].as_str().unwrap_or_default().to_string();
    let max = d["maxnumber"].as_u64().unwrap_or(1_000_000);
    for n in 0..=max {
        let mut h = Sha256::new();
        h.update(format!("{salt}{n}"));
        if hex::encode(h.finalize()) == challenge {
            let payload = json!({
                "algorithm": "SHA-256", "challenge": challenge, "salt": salt,
                "signature": signature, "number": n
            });
            return B64.encode(payload.to_string());
        }
    }
    panic!("ALTCHA solve failed");
}

async fn login_once(c: &reqwest::Client, base: &str, password: &str) -> Option<String> {
    let captcha = solve_captcha(c, base).await;
    let r = c
        .post(format!("{base}/api/auth/login"))
        .json(&json!({"email": EMAIL, "password": password, "captcha": captcha}))
        .send()
        .await;
    match r {
        Ok(resp) if resp.status().is_success() => {
            let v: Value = resp.json().await.expect("login json");
            Some(
                v.get("data")
                    .and_then(|d| d.get("token"))
                    .or_else(|| v.get("token"))
                    .and_then(|t| t.as_str())
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
        Err(e) => panic!("login 请求失败：{e}"),
    }
}

async fn admin_token(c: &reqwest::Client, base: &str) -> String {
    let tok = match login_once(c, base, "123456").await {
        Some(t) => t,
        None => login_once(c, base, PASSWORD).await.expect("两个密码均被拒"),
    };
    // 新库默认 must_change_password：先改密再重登（旧库改密会「旧密码不正确」，忽略）
    if let Ok(resp) = c
        .post(format!("{base}/api/auth/change-password"))
        .bearer_auth(&tok)
        .json(&json!({"old_password": "123456", "new_password": PASSWORD}))
        .send()
        .await
    {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if !status.is_success() && !body.contains("旧密码不正确") {
            panic!("change-password 失败：{body}");
        }
    }
    login_once(c, base, PASSWORD).await.expect("改密后重登失败")
}

async fn ensure_channel(
    c: &reqwest::Client,
    base: &str,
    token: &str,
    name: &str,
    channel_type: &str,
    model: &str,
    mock_base: &str,
) {
    let list: Value = c
        .get(format!("{base}/api/channels"))
        .bearer_auth(token)
        .send()
        .await
        .expect("channels list")
        .json()
        .await
        .expect("channels json");
    let items = list
        .get("data")
        .and_then(|d| {
            d.as_array()
                .cloned()
                .or_else(|| d.get("items").and_then(|i| i.as_array()).cloned())
        })
        .unwrap_or_default();
    for item in items {
        if item.get("name").and_then(|n| n.as_str()) == Some(name) {
            if let Some(id) = item.get("id").map(ToString::to_string) {
                let _ = c
                    .delete(format!("{base}/api/channels/{id}"))
                    .bearer_auth(token)
                    .send()
                    .await;
            }
        }
    }
    c.post(format!("{base}/api/channels"))
        .bearer_auth(token)
        .json(&json!({
            "name": name, "channel_type": channel_type,
            "base_url": format!("{mock_base}/v1"),
            "api_key": "sk-e147", "priority": 100, "weight": 100,
            "status": "enabled", "models": [model]
        }))
        .send()
        .await
        .expect("create channel")
        .error_for_status()
        .expect("create channel ok");
}

// ─── 观测面 ───

async fn all_logs(c: &reqwest::Client, base: &str, token: &str) -> Vec<Value> {
    let v: Value = c
        .get(format!("{base}/api/logs/requests?size=300"))
        .bearer_auth(token)
        .send()
        .await
        .expect("logs")
        .json()
        .await
        .expect("logs json");
    v.get("data")
        .and_then(|d| {
            d.as_array()
                .cloned()
                .or_else(|| d.get("items").and_then(|i| i.as_array()).cloned())
        })
        .unwrap_or_default()
}

async fn key_used_quota(c: &reqwest::Client, base: &str, token: &str) -> u64 {
    let v: Value = c
        .get(format!("{base}/api/tokens?all=1"))
        .bearer_auth(token)
        .send()
        .await
        .expect("tokens")
        .json()
        .await
        .expect("tokens json");
    let arr = v
        .get("data")
        .and_then(|d| {
            d.as_array()
                .cloned()
                .or_else(|| d.get("items").and_then(|i| i.as_array()).cloned())
        })
        .unwrap_or_default();
    arr.iter()
        .find(|t| t.to_string().contains("e147"))
        .and_then(|t| t.get("used_quota").and_then(|q| q.as_u64()))
        .expect("key e147 不可见（all=1 口径）")
}

/// 唯一消息体（坑①：response_cache 同体遮蔽）
static UNIQ: AtomicU64 = AtomicU64::new(0);
fn unique_content(tag: &str) -> String {
    let n = UNIQ.fetch_add(1, Ordering::SeqCst);
    format!("hi {tag}-{n}")
}

// ─── 用例执行 ───

#[derive(Clone, Copy)]
enum Proto {
    Openai,    // /v1/chat/completions
    Anthropic, // /v1/messages
}

impl Proto {
    fn path(self) -> &'static str {
        match self {
            Proto::Openai => "/v1/chat/completions",
            Proto::Anthropic => "/v1/messages",
        }
    }
}

/// 断连用例：原始 TCP 写请求后等待 `at` 再 drop 连接——等价 curl -m
/// （curl 超时杀进程=socket 关闭；服务端 hyper 感知 FIN → drop handler
/// → 守卫 Drop 落 499）。不走 reqwest 是因为其 timeout 的连接中止语义
/// 不如裸 socket drop 与 curl -m 严格等价。
async fn disconnect_send(port: u16, path: &str, body: &str, at: Duration) {
    use tokio::io::AsyncWriteExt;
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect gateway");
    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {KEY}\r\nContent-Type: application/json\r\nanthropic-version: 2023-06-01\r\nContent-Length: {len}\r\n\r\n{body}",
        len = body.len()
    );
    stream
        .write_all(req.as_bytes())
        .await
        .expect("write request");
    stream.flush().await.expect("flush request");
    tokio::time::sleep(at).await;
    drop(stream); // 客户端断开
}

fn port_of(base: &str) -> u16 {
    base.rsplit(':')
        .next()
        .expect("port")
        .parse()
        .expect("port parse")
}

async fn complete_request(c: &reqwest::Client, base: &str, path: &str, body: Value) {
    let r = c
        .post(format!("{base}{path}"))
        .bearer_auth(KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .expect("control request");
    assert_eq!(r.status(), StatusCode::OK, "对照用例应 200");
    let _ = r.bytes().await; // 读完整响应体（流式对照含 SSE）
}

/// 单面 4 用例 + 断言族（A1/A1b/A1c/A1d/A3/A3b/A2/A4），移植 verify.mjs 口径。
/// 返回本面结束后的 used_quota（供跨面总账）。
#[allow(clippy::too_many_arguments)] // 断言族参数即上下文，拆结构体反而稀释可读性（billing.rs 同款先例）
async fn run_surface(
    c: &reqwest::Client,
    base: &str,
    admin: &str,
    port: u16,
    tag: &str,
    channel: &str,
    model: &str,
    proto: Proto,
    ids0: &std::collections::HashSet<String>,
    q_prev: u64,
    total_cost_sink: &mut u64,
) -> u64 {
    let path = proto.path();
    // 请求体构造（唯一体纪律：每次 unique_content 递增）
    let body = |stream: bool| {
        let mut o = json!({
            "model": model,
            "max_tokens": 256,
            "messages": [{"role": "user", "content": unique_content(tag)}]
        });
        if stream {
            o["stream"] = json!(true);
        }
        o
    };
    // 断连两例（E1/EA1 非流式 1.5s；E2/EA2 流式 2s）
    let e1 = body(false).to_string();
    disconnect_send(port, path, &e1, Duration::from_millis(DISCONNECT_AT_MS)).await;
    let e2 = body(true).to_string();
    disconnect_send(
        port,
        path,
        &e2,
        Duration::from_millis(DISCONNECT_AT_MS + 500),
    )
    .await;
    // 对照两例（C1/CA1 非流式；C2/CA2 流式）
    complete_request(c, base, path, body(false)).await;
    complete_request(c, base, path, body(true)).await;

    tokio::time::sleep(SETTLE_WAIT).await;
    let fresh: Vec<Value> = all_logs(c, base, admin)
        .await
        .into_iter()
        .filter(|l| {
            !ids0.contains(
                l.get("request_id")
                    .and_then(|r| r.as_str())
                    .unwrap_or_default(),
            ) && l.get("channel_name").and_then(|n| n.as_str()) == Some(channel)
        })
        .collect();
    let q_after = key_used_quota(c, base, admin).await;

    let n499: Vec<&Value> = fresh
        .iter()
        .filter(|l| l.get("status_code").and_then(|s| s.as_u64()) == Some(499))
        .collect();
    let n200: Vec<&Value> = fresh
        .iter()
        .filter(|l| l.get("status_code").and_then(|s| s.as_u64()) == Some(200))
        .collect();
    let cost_of = |l: &Value| l.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0) as i64;
    let in_of = |l: &Value| l.get("input_tokens").and_then(|t| t.as_u64()).unwrap_or(0);
    let out_of = |l: &Value| l.get("output_tokens").and_then(|t| t.as_u64()).unwrap_or(0);

    assert_eq!(
        fresh.len(),
        4,
        "{tag}：新日志应恰 4 条，实际 {}",
        fresh.len()
    );
    assert_eq!(
        n499.len(),
        2,
        "{tag} A1：499 条数（Drop 触发链）实际 {}",
        n499.len()
    );
    assert!(
        n499.iter().all(|l| l
            .get("error_msg")
            .and_then(|e| e.as_str())
            .is_some_and(|e| e.contains("client closed before first event"))),
        "{tag} A1b：error_msg 口径"
    );
    assert!(
        n499.iter()
            .all(|l| in_of(l) > 0 && out_of(l) == 0 && cost_of(l) > 0),
        "{tag} A1c：估算特征 in>0/out=0/cost>0"
    );
    assert!(
        n499.iter()
            .all(|l| l.get("channel_name").and_then(|n| n.as_str()) == Some(channel)),
        "{tag} A1d：499 归因本面渠道"
    );
    assert_eq!(n200.len(), 2, "{tag} A3：对照 200 条数实际 {}", n200.len());
    assert!(
        n200.iter()
            .any(|l| in_of(l) == 100 && out_of(l) == 64 && cost_of(l) > 0),
        "{tag} A3b：对照真实 usage in=100/out=64"
    );
    let sum_cost: i64 = fresh.iter().map(cost_of).sum();
    assert_eq!(
        (q_after - q_prev) as i64,
        sum_cost,
        "{tag} A2：used_quota delta 应等于 Σ新日志 cost（估算计费入账）"
    );
    assert!(
        n499.iter()
            .all(|l| l.get("status_code").and_then(|s| s.as_u64()) != Some(200))
            && n200
                .iter()
                .all(|l| l.get("status_code").and_then(|s| s.as_u64()) != Some(499)),
        "{tag} A4：断连无 200、对照无 499（无双计）"
    );
    *total_cost_sink += sum_cost as u64;
    q_after
}

// ─── 主测试 ───

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_499_billing_dual_surface() {
    // 1) mock 上游（双协议，hits 计数为外部可核量）
    let mock = Arc::new(MockState {
        hits: AtomicU64::new(0),
    });
    let mock_port = free_port();
    let mock_app = Router::new()
        .route("/v1/chat/completions", post(mock_handler))
        .route("/v1/messages", post(mock_handler))
        .with_state(mock.clone());
    let mock_listener = tokio::net::TcpListener::bind(("127.0.0.1", mock_port))
        .await
        .expect("bind mock");
    let mock_task = tokio::spawn(async move {
        axum::serve(mock_listener, mock_app)
            .await
            .expect("mock serve");
    });

    // 2) 网关实例（临时 data dir + 独立端口）
    let tmp = tempfile::tempdir().expect("tempdir");
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).expect("create data dir");
    let log_path = tmp.path().join("server.log");
    let server = spawn_server(&data_dir, &log_path).await;
    let base = server.base.clone();
    let port = port_of(&base);
    let mock_base = format!("http://127.0.0.1:{mock_port}");

    let c = client();

    // 3) 管理面 bootstrap
    let admin = admin_token(&c, &base).await;
    ensure_channel(
        &c,
        &base,
        &admin,
        "e147-mock",
        "openai_compatible",
        "mock-model",
        &mock_base,
    )
    .await;
    ensure_channel(
        &c,
        &base,
        &admin,
        "e147-mock-anth",
        "anthropic",
        "mock-model-anth",
        &mock_base,
    )
    .await;
    for m in ["mock-model", "mock-model-anth"] {
        c.post(format!("{base}/api/pricing"))
            .bearer_auth(&admin)
            .json(&json!({"model_name": m, "input_price": 50, "output_price": 100, "price_type": "token"}))
            .send()
            .await
            .expect("pricing")
            .error_for_status()
            .expect("pricing ok");
    }
    c.post(format!("{base}/api/tokens"))
        .bearer_auth(&admin)
        .json(&json!({"name": "e147-key", "custom_key": KEY}))
        .send()
        .await
        .expect("create key")
        .error_for_status()
        .expect("create key ok");

    // 4) 双面冒烟（非 200 即拓扑无效）
    let smoke = c
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(KEY)
        .json(&json!({"model": "mock-model", "messages": [{"role": "user", "content": unique_content("smoke-oai")}]}))
        .send()
        .await
        .expect("smoke openai");
    assert_eq!(smoke.status(), StatusCode::OK, "openai 冒烟应 200");
    let smoke = c
        .post(format!("{base}/v1/messages"))
        .bearer_auth(KEY)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({"model": "mock-model-anth", "max_tokens": 256, "messages": [{"role": "user", "content": unique_content("smoke-anth")}]}))
        .send()
        .await
        .expect("smoke anthropic");
    assert_eq!(smoke.status(), StatusCode::OK, "anthropic 冒烟应 200");

    // 5) 基线
    let ids0: std::collections::HashSet<String> = all_logs(&c, &base, &admin)
        .await
        .into_iter()
        .filter_map(|l| {
            l.get("request_id")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let q0 = key_used_quota(&c, &base, &admin).await;

    // 6) openai 面：E1/E2/C1/C2
    let mut total_cost: u64 = 0;
    let q1 = run_surface(
        &c,
        &base,
        &admin,
        port,
        "E",
        "e147-mock",
        "mock-model",
        Proto::Openai,
        &ids0,
        q0,
        &mut total_cost,
    )
    .await;

    // 7) anthropic 面：EA1/EA2/CA1/CA2
    run_surface(
        &c,
        &base,
        &admin,
        port,
        "EA",
        "e147-mock-anth",
        "mock-model-anth",
        Proto::Anthropic,
        &ids0,
        q1,
        &mut total_cost,
    )
    .await;

    // 8) 跨面总账 + mock hits 外部可核量
    let q_end = key_used_quota(&c, &base, &admin).await;
    assert_eq!(
        (q_end - q0) as i64,
        total_cost as i64,
        "X1：跨面总账 used_quota delta 应等于 Σ8 用例 cost"
    );
    let hits = mock.hits.load(Ordering::SeqCst);
    assert_eq!(
        hits, 10,
        "mock hits 应为 10（双面冒烟 2 + 用例 8，断连用例上游必须真实执行）"
    );

    let _ = mock_task.await;
    println!("499 回归双表面全绿：hits={hits} q_end={q_end}");
}
