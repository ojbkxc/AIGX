//! 插件子进程管理 —— spawn / 握手 / 崩溃观察 / 优雅关闭。
//!
//! ## 子进程形态
//!
//! 插件是独立二进制，通过 stdin/stdout 与网关 RPC（见 `rpc.rs`）。
//! stderr 直连 `tracing`（插件崩溃时能看到原因）。
//!
//! ## M1 范围
//!
//! - `spawn()` —— `tokio::process::Command` 起子进程
//! - `handshake()` —— `init` 握手（发 `HostEnv`+`protocol`，收 `Vec<ProviderInfo>`）
//! - `shutdown()` —— 先 RPC `shutdown` 等 5s，超时 `kill`
//! - `supervise()` —— 观察子进程退出（崩溃重启在 M4 完善）

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock};

#[cfg(test)]
use super::rpc::NoopCallbacks;
use super::rpc::{CallbackHandler, RpcChannel, RpcError, PROTOCOL_VERSION};

/// 握手超时。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 优雅关闭：等子进程自行退出的超时。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// stderr 行的字节上限（防止插件疯狂刷日志撑爆内存）。
const STDERR_LINE_LIMIT: usize = 1 << 16;

/// 传给插件的宿主环境。
///
/// 字段名与上游 `agent2api-provider-protocol` 的 [`HostEnv`] 逐字一致
/// （serde 无 rename，JSON 键 = Rust 字段名的 snake_case）。握手时以
/// `"env"` 字段发出（不是 `hostEnv`）—— 字段名对不上会直接
/// 「init 的 env 无法解析: invalid type: null」。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct HostEnv {
    pub data_dir: String,
    pub host_version: String,
    pub target: String,
    pub proxy: String,
    /// 是否允许插件写调试报文（网关的调试模式开关）。上游为必填项。
    pub debug_traffic: bool,
    /// 网关监听的 HTTP 端口（`0` = 未知）。上游可选（`#[serde(default)]`）。
    pub loopback_port: u16,
}

/// 宿主的目标三元组（与上游 `hostEnv.target` 同口径）。
///
/// 优先用编译期 `TARGET`（`cargo --target` 会设），否则从 `(OS, ARCH)` 推导
/// —— 现有构建流程**并不导出** `TARGET`，只靠它会让目标恒为 `"unknown"`。
///
/// 同一口径也用于安装时挑 `pack.json` 的 `entry`：上游把它写成
/// `{目标三元组: 入口路径}` 的映射（AIGX 自建包仍写字符串，两种都收）。
pub fn host_target_triple() -> String {
    if let Some(t) = option_env!("TARGET") {
        if !t.is_empty() && t != "unknown" {
            return t.to_string();
        }
    }
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu".to_string(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_string(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_string(),
        ("macos", "aarch64") => "aarch64-apple-darwin".to_string(),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_string(),
        (os, arch) => format!("{arch}-{os}"),
    }
}

/// 插件申报的一家 provider。
///
/// `serde(default)` 让未知字段自然落到默认值——插件可能申报更多元数据
/// （能力位、模型列表等），网关不强制校验。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
}

/// 进程状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessState {
    /// 正在握手中。
    Handshaking,
    /// 正常运行（已完成握手）。
    Running,
    /// 崩溃（观察者标记，等 M4 实现重启）。
    Crashed { attempts: u32 },
    /// 已关闭（网关 shutdown 或插件自行退出）。
    Shutdown,
}

/// spawn 错误。
#[derive(Debug, thiserror::Error)]
pub enum SpawnError {
    /// `Command::spawn` 失败（二进制不存在、权限不足等）。
    #[error("failed to spawn plugin process: {0}")]
    Spawn(#[from] std::io::Error),
}

/// 一个插件子进程。
///
/// 持有 `Child`（用于 `wait`/`kill`）+ `RpcChannel`（用于 RPC）+ 状态。
/// 不是 `Clone`——调用方用 `Arc<PluginProcess>` 共享。
pub struct PluginProcess {
    /// 包 id（`pack.json` 的 id）。
    pub pack_id: String,
    /// 子进程句柄。用 `Mutex` 包住：`wait`/`kill`/`try_wait` 都要 `&mut`，
    /// 而调用方共享的是 `Arc<PluginProcess>`（取不到 `&mut`）。
    pub child: Mutex<Child>,
    /// RPC 通道。
    pub rpc: RpcChannel,
    /// 进程状态（观察者写，管理面读）。
    pub state: RwLock<ProcessState>,
    /// 是否由网关主动发起的关闭（区分正常关闭 vs 崩溃）。
    shutdown_requested: AtomicBool,
}

impl PluginProcess {
    /// 标记网关主动关闭。
    pub fn mark_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
    }

    /// 是否由网关主动关闭。
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }
}

/// 起一个插件子进程。
///
/// `entry` 是插件二进制路径，`host_env` 是传给插件的宿主环境，
/// `callbacks` 处理插件 → 网关的回调（读账号 / settings / ledger / 日志）。
/// 返回的 `PluginProcess` 已完成 spawn 但**尚未握手**——调用方需调
/// [`handshake`] 完成 init。
pub fn spawn(
    pack_id: &str,
    entry: &Path,
    host_env: &HostEnv,
    callbacks: Arc<dyn CallbackHandler>,
) -> Result<Arc<PluginProcess>, SpawnError> {
    let mut cmd = Command::new(entry);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // 插件环境：网关版本、目标平台等（插件可能读环境变量做自检）
    cmd.env("AIGX_PLUGIN_HOST_VERSION", &host_env.host_version);
    cmd.env("AIGX_PLUGIN_TARGET", &host_env.target);
    if !host_env.proxy.is_empty() {
        cmd.env("AIGX_PLUGIN_PROXY", &host_env.proxy);
    }

    let mut child = cmd.spawn().map_err(SpawnError::Spawn)?;

    let stdin: ChildStdin = child.stdin.take().expect("stdin was piped");
    let stdout: ChildStdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // stderr 直连 tracing（有界读，防止插件刷屏撑爆内存）
    let tag = pack_id.to_string();
    tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            // 用 take 限制单行长度：超过即截断当行读完
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&buf[..buf.len().min(STDERR_LINE_LIMIT)]);
                    let text = text.trim_end_matches(['\n', '\r']);
                    if !text.is_empty() {
                        tracing::warn!("[plugin {tag} stderr] {text}");
                    }
                    if buf.len() >= STDERR_LINE_LIMIT {
                        // 超长行：丢弃，继续读下一行
                        tracing::warn!(
                            "[plugin {tag} stderr] (line truncated at {STDERR_LINE_LIMIT} bytes)"
                        );
                    }
                }
                Err(_) => break,
            }
        }
    });

    let rpc = RpcChannel::new(Box::new(stdout), Box::new(stdin), callbacks);

    Ok(Arc::new(PluginProcess {
        pack_id: pack_id.to_string(),
        child: Mutex::new(child),
        rpc,
        state: RwLock::new(ProcessState::Handshaking),
        shutdown_requested: AtomicBool::new(false),
    }))
}

/// `init` 握手。
///
/// 发 `{"protocol":1,"env":{...}}`，插件回 `{providers:[{id,label}]}`。
/// 10s 超时判失败。
///
/// `env` 字段名与上游 SDK 一致（`sdk_serve.rs` 里 `params.get("env")`）；
/// HostEnv 的 JSON 键是 snake_case（`data_dir`/`host_version`/…），与上游
/// serde 结构逐字对应——直接序列化 `HostEnv` 即可，不要手写 camelCase 键。
pub async fn handshake(
    rpc: &RpcChannel,
    host_env: &HostEnv,
) -> Result<Vec<ProviderInfo>, RpcError> {
    let env = serde_json::to_value(host_env).map_err(|e| RpcError {
        message: format!("序列化 HostEnv 失败: {e}"),
        code: Some("serde".into()),
    })?;
    let params = serde_json::json!({
        "protocol": PROTOCOL_VERSION,
        "env": env,
    });
    let result = tokio::time::timeout(HANDSHAKE_TIMEOUT, rpc.call("init", params))
        .await
        .map_err(|_| RpcError {
            message: "handshake timeout".into(),
            code: Some("timeout".into()),
        })??;
    let providers = result
        .get("providers")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<ProviderInfo>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    Ok(providers)
}

/// 优雅关闭插件。
///
/// 先发 RPC `shutdown`（等插件自行退出），5s 超时后 `kill`（kill 幂等：
/// 插件已退出则是 no-op）。
pub async fn shutdown(process: &PluginProcess) {
    process.mark_shutdown();
    // 先尝试 RPC shutdown（best effort，插件可能已退出）
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, process.rpc.call("shutdown", Value::Null)).await;
    process.rpc.shutdown();
    // 兜底：直接 kill 子进程
    {
        let mut child = process.child.lock().await;
        let _ = child.kill().await;
    }
    *process.state.write().await = ProcessState::Shutdown;
}

/// 观察子进程退出。
///
/// 用 `try_wait` 轮询（不长期持有 `child` 锁，避免与 `shutdown` 死锁），
/// 每 500ms 探一次。崩溃重启（退避）在 M4 扩展。
pub async fn supervise(process: Arc<PluginProcess>) {
    loop {
        let exited = {
            let mut child = process.child.lock().await;
            match child.try_wait() {
                Ok(Some(status)) => Some(status),
                Ok(None) => None, // 仍在运行
                Err(e) => {
                    tracing::warn!("plugin {} try_wait error: {e}", process.pack_id);
                    return;
                }
            }
        };
        if let Some(status) = exited {
            if process.is_shutdown_requested() {
                tracing::debug!(
                    "plugin {} exited after shutdown request: {status}",
                    process.pack_id
                );
            } else {
                tracing::warn!("plugin {} exited unexpectedly: {status}", process.pack_id);
                *process.state.write().await = ProcessState::Crashed { attempts: 1 };
            }
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

    type BoxReader = Box<dyn AsyncRead + Send + Unpin>;
    type BoxWriter = Box<dyn AsyncWrite + Send + Unpin>;

    /// 建一对 duplex：返回 (client 读, client 写, plugin 读, plugin 写)。
    fn duplex_pair() -> (BoxReader, BoxWriter, BoxReader, BoxWriter) {
        let (client_out, plugin_in) = tokio::io::duplex(1 << 16);
        let (plugin_out, client_in) = tokio::io::duplex(1 << 16);
        (
            Box::new(client_in),
            Box::new(client_out),
            Box::new(plugin_in),
            Box::new(plugin_out),
        )
    }

    /// 读一帧（测试用）。
    async fn read_one_frame<R: tokio::io::AsyncBufRead + Unpin>(
        reader: &mut R,
    ) -> super::super::rpc::Frame {
        let mut buf = Vec::new();
        reader
            .read_until(b'\n', &mut buf)
            .await
            .expect("read frame");
        super::super::rpc::decode_frame(String::from_utf8_lossy(&buf).trim()).expect("decode")
    }

    /// 插件侧应答一个 init。
    async fn answer_init<W: AsyncWrite + Unpin>(writer: &mut W, request_id: i64, result: Value) {
        let response = super::super::rpc::Frame {
            id: request_id,
            result: Some(result),
            ..Default::default()
        };
        writer
            .write_all(serde_json::to_string(&response).unwrap().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
    }

    #[tokio::test]
    async fn handshake_returns_providers() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let rpc = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let plugin_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            assert_eq!(frame.method.as_deref(), Some("init"));
            answer_init(
                &mut plugin_write,
                frame.id,
                serde_json::json!({
                    "providers": [
                        {"id": "kiro", "label": "Kiro"},
                        {"id": "astudio", "label": "AStudio"}
                    ]
                }),
            )
            .await;
        });

        let env = HostEnv {
            data_dir: "/data".into(),
            host_version: "1.0.52".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            proxy: "".into(),
            debug_traffic: false,
            loopback_port: 0,
        };
        let providers = handshake(&rpc, &env).await.unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].id, "kiro");
        assert_eq!(providers[0].label, "Kiro");
        assert_eq!(providers[1].id, "astudio");
        plugin_task.await.unwrap();
        rpc.shutdown();
    }

    #[tokio::test]
    async fn handshake_empty_providers() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let rpc = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let plugin_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            answer_init(
                &mut plugin_write,
                frame.id,
                serde_json::json!({ "providers": [] }),
            )
            .await;
        });

        let providers = handshake(&rpc, &HostEnv::default()).await.unwrap();
        assert!(providers.is_empty());
        plugin_task.await.unwrap();
        rpc.shutdown();
    }

    #[tokio::test]
    async fn handshake_error_response() {
        let (client_read, client_write, plugin_read, mut plugin_write) = duplex_pair();
        let rpc = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        let plugin_task = tokio::spawn(async move {
            let mut reader = tokio::io::BufReader::new(plugin_read);
            let frame = read_one_frame(&mut reader).await;
            let response = super::super::rpc::Frame {
                id: frame.id,
                error: Some(RpcError {
                    message: "protocol mismatch".into(),
                    code: Some("proto".into()),
                }),
                ..Default::default()
            };
            plugin_write
                .write_all(serde_json::to_string(&response).unwrap().as_bytes())
                .await
                .unwrap();
            plugin_write.write_all(b"\n").await.unwrap();
            plugin_write.flush().await.unwrap();
        });

        let result = handshake(&rpc, &HostEnv::default()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().message, "protocol mismatch");
        plugin_task.await.unwrap();
        rpc.shutdown();
    }

    #[tokio::test]
    async fn handshake_plugin_exits() {
        let (client_read, client_write, plugin_read, plugin_write) = duplex_pair();
        let rpc = RpcChannel::new(client_read, client_write, Arc::new(NoopCallbacks));

        // 插件端直接关闭（drop）
        drop(plugin_read);
        drop(plugin_write);

        let result = handshake(&rpc, &HostEnv::default()).await;
        assert!(result.is_err(), "插件退出后握手应失败");
        rpc.shutdown();
    }

    #[test]
    fn provider_info_ignores_unknown_fields() {
        // 插件可能申报更多字段（能力位等），未知字段应自然忽略
        let v: ProviderInfo =
            serde_json::from_str(r#"{"id":"kiro","label":"Kiro","capabilities":{"chat":true}}"#)
                .unwrap();
        assert_eq!(v.id, "kiro");
        assert_eq!(v.label, "Kiro");
    }

    #[test]
    fn provider_info_defaults_missing_fields() {
        let v: ProviderInfo = serde_json::from_str(r#"{"id":"kiro"}"#).unwrap();
        assert_eq!(v.id, "kiro");
        assert_eq!(v.label, "");
    }
}
