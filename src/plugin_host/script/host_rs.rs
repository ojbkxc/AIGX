//! 脚本宿主的 Rust 侧生命周期 —— 嵌入 JS 宿主、落盘、spawn Bun、init 握手、
//! 崩溃观察、优雅关闭（任务 #5）。
//!
//! ## 分工边界
//!
//! 协议层**完全复用** [`crate::plugin_host::rpc::RpcChannel`]（帧编解码 /
//! 奇偶 id / credit 背压 / pending 路由全部现成）——这里不做任何帧的
//! 编解码，只做「文件分发 + 进程管理 + init 载荷」。Bun 的取得与启动
//! 参数构造在 [`super::bun`]（`ensure` / `run_command`，含环境清理与
//! Windows 无窗）。
//!
//! ## 宿主文件的分发策略
//!
//! JS 宿主（`host/*.mjs`，AIGX 自研）用 `include_str!` 进二进制，运行前
//! 落盘到 `{data_dir}/script-plugins/host/{sha256 前 24 位}/`：
//!
//! - 目录名 = 全部文件「名字 + 长度 + 内容」的指纹 → 内容变则目录换，
//!   正在跑的旧 Bun 进程引用的文件永不被覆盖（Windows 上删运行中的文件
//!   会失败；Unix 上会让进程读到空文件）；
//! - 幂等：带 `.complete` 标记的目录直接跳过重写；
//! - 旧指纹目录不清理：体积只有几十 KB，堆积成本可忽略，运维按
//!   host/ 下非当前指纹目录手动清理。
//!
//! ## init 握手（60s）
//!
//! 脚本宿主要动态 import 全部 npm 包（远比原生插件的 10s 宽）。请求形状：
//!
//! ```json
//! {"id":1,"method":"init","params":{"protocol":1,"init":{…}}}
//! ```
//!
//! `init` 载荷是 [`ScriptInitPayload`]（snake_case 四键 packages / reserved /
//! claimed / catalog_path，wire 契约不可改）。应答取
//! `{"providers":[{id,label,upstream,models}], "identities":{pkgId:{声明id:实际id}}}`
//! （providers 兼容裸数组形状，与原生握手同款宽容；identities 缺失整段忽略）。
//!
//! ## 生命周期
//!
//! spawn（stderr 有界直连 tracing）→ init（60s）→ Running →
//! [`supervise`]（500ms 轮询 try_wait，异常退出标 Crashed）/
//! [`shutdown`]（先 RPC shutdown 5s 宽限，超时 kill）。

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::AsyncBufReadExt;
use tokio::process::{Child, ChildStderr};
use tokio::sync::{Mutex, RwLock};

use super::bun;
use super::types::{script_plugins_root, ScriptInitPayload, ScriptPackage, ScriptProviderInfo};
// 协议内核（RpcChannel/CallbackHandler/PROTOCOL_VERSION）在 plugin_host::rpc——
// 与原生插件同一套。注意 `super::rpc` 是 script 模块下另一个文件（#6 的
// provider 方法名常量），两者并存、含义不同，不能混用路径。
use crate::plugin_host::rpc::{CallbackHandler, RpcChannel, RpcError, PROTOCOL_VERSION};

/// init 握手超时：脚本宿主要动态 import 全部 npm 包（冷盘 + 首次编译），
/// 比原生插件的 10s 宽一个量级。
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// 优雅关闭：等宿主自行退出的超时（同原生插件）。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// stderr 单行字节上限（脚本刷屏时硬切，防撑爆内存）。
const STDERR_LINE_LIMIT: usize = 1 << 16;

/// 握手失败时随错误回报的 stderr 尾巴大小。
///
/// 正常路径 stderr 只进 tracing；但宿主起不来时（Bun 缺动态库、脚本顶层
/// 抛错、import 失败）根因几乎只在 stderr 里，而管理面的 `lastError` 是
/// 运维唯一能看到的字段——必须把尾巴带上去，否则只剩一句「启动失败」。
const STDERR_TAIL_BYTES: usize = 8 << 10;

/// supervise 的退出轮询间隔（同原生插件）。
const SUPERVISE_POLL: Duration = Duration::from_millis(500);

/// 嵌入二进制的宿主文件（AIGX 自研 JS，许可随本仓）。
///
/// 入口恒为 `main.mjs`；`wire`/`loader`/`provider`/`errors` 是它的同目录
/// 相对 import——落盘时五个文件必须在同一个 host_dir 里。
const HOST_FILES: &[(&str, &str)] = &[
    ("main.mjs", include_str!("host/main.mjs")),
    ("wire.mjs", include_str!("host/wire.mjs")),
    ("loader.mjs", include_str!("host/loader.mjs")),
    ("provider.mjs", include_str!("host/provider.mjs")),
    ("errors.mjs", include_str!("host/errors.mjs")),
];

/// spawn / 握手错误。
#[derive(Debug, thiserror::Error)]
pub enum ScriptHostError {
    /// Bun 运行时不可用（下载失败 / 探针不过）。
    #[error("Bun runtime unavailable: {0}")]
    Runtime(String),
    /// 宿主文件落盘或进程 spawn 的 IO 失败。
    #[error("script host io: {0}")]
    Io(#[from] std::io::Error),
    /// init 握手失败（超时 / 宿主报错 / 通道关闭）。
    #[error("script host init failed: {0}")]
    Handshake(#[from] RpcError),
    /// init 载荷序列化失败（几乎不可能：全无副作用的纯结构）。
    #[error("init payload serialize failed: {0}")]
    Serde(String),
}

/// 宿主进程状态（观察者写，管理面读）。
///
/// 没有 `Initializing`：[`spawn_host`] 只在握手成功后返回宿主，
/// 握手失败的进程当场 kill + 报错，外部观察不到中间态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptHostState {
    /// 正常运行（握手完成）。
    Running,
    /// 崩溃（观察者标记；重启策略由 runtime 层决定）。
    Crashed,
    /// 已关闭（网关 shutdown 或宿主自行退出）。
    Shutdown,
}

/// 一个脚本宿主进程（**一个** Bun 承载全部脚本包）。
pub struct ScriptHost {
    /// 子进程句柄（wait/kill 要 `&mut`，用 Mutex 支持跨任务共享）。
    child: Mutex<Child>,
    /// RPC 通道（复用原生插件的协议内核）。用 `Arc` 包住：数据面
    /// `channel_for_provider` 要把通道句柄交给 `PluginBridge`，桥的生命期
    /// 可以长于一次宿主热切换（旧宿主等在途流结束后才退出）。
    rpc: Arc<RpcChannel>,
    /// init 应答里脚本申报的 provider（冲突加缀后的实际接管 id）。
    providers: Vec<ScriptProviderInfo>,
    /// init 应答里的 identities：`{pkgId: {声明 id → 实际接管 id}}`
    /// （id 冲突加缀时网关路由要用它翻译；运行期为只读快照）。
    identities: serde_json::Map<String, Value>,
    state: RwLock<ScriptHostState>,
    shutdown_requested: AtomicBool,
}

impl ScriptHost {
    /// RPC 通道（数据面 conversation / 运维面方法都走它）。
    ///
    /// 返回 `Arc` 克隆：桥句柄可以持有通道而宿主句柄被热切换替换。
    pub fn rpc(&self) -> Arc<RpcChannel> {
        self.rpc.clone()
    }

    /// init 应答里申报的 provider 集合。
    pub fn providers(&self) -> &[ScriptProviderInfo] {
        &self.providers
    }

    /// 声明 id → 实际接管 id 的映射（`{pkgId: {alias → providerId}}`）。
    pub fn identities(&self) -> &serde_json::Map<String, Value> {
        &self.identities
    }

    /// 当前状态快照。
    pub async fn state(&self) -> ScriptHostState {
        self.state.read().await.clone()
    }

    /// 标记网关主动关闭（区分正常关闭 vs 崩溃）。
    pub fn mark_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
    }

    /// 是否由网关主动关闭。
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }
}

/// 宿主文件集合的内容指纹（sha256 前 24 位 hex）。
///
/// 哈希覆盖每个文件的「名字 + 0 分隔 + 长度 + 内容」——带边界防拼接歧义
/// （"ab"+"c" 与 "a"+"bc" 必须得到不同指纹）。目录名即指纹：内容变 →
/// 目录换 → 老进程的文件永不被覆盖。
fn host_content_hash() -> String {
    let mut hasher = Sha256::new();
    for (name, content) in HOST_FILES {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update((content.len() as u64).to_le_bytes());
        hasher.update(content.as_bytes());
    }
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..24].to_string()
}

/// 宿主目录：`{data_dir}/script-plugins/host/{sha256 前 24 位}`。
fn host_dir(data_dir: &Path) -> PathBuf {
    script_plugins_root(data_dir)
        .join("host")
        .join(host_content_hash())
}

/// 把宿主文件落到 host_dir（幂等：指纹目录 + `.complete` 标记）。
///
/// 先写临时名再 rename：中断后最终路径上不会出现半截文件；`.complete`
/// 最后写——见标记即五个文件全部就位。
fn materialize_host_files(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = host_dir(data_dir);
    if dir.join(".complete").is_file() {
        return Ok(dir);
    }
    std::fs::create_dir_all(&dir)?;
    for (name, content) in HOST_FILES {
        let final_path = dir.join(name);
        let tmp = dir.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &final_path)?;
    }
    std::fs::write(dir.join(".complete"), b"ok")?;
    Ok(dir)
}

/// 由已启用脚本包集合构造 init 载荷。
///
/// `packages` 取包版本目录的**绝对路径**——路径只能经
/// [`ScriptPackage::version_dir`] 产生（目录名是 `script-{hash}`，scoped
/// 包名的 `/` 不会进文件系统），不要自己拼 `packages/{包名}`。
/// `claimed` 取各包声明的 provider id 集合（与 reserved 的交集由网关侧
/// 让位逻辑处理，这里原样上报）。
pub fn build_init_payload(
    script_root: &Path,
    packages: &[ScriptPackage],
    reserved: Vec<String>,
    catalog_path: Option<String>,
) -> ScriptInitPayload {
    ScriptInitPayload {
        packages: packages
            .iter()
            .map(|p| p.version_dir(script_root).to_string_lossy().into_owned())
            .collect(),
        reserved,
        claimed: packages
            .iter()
            .flat_map(|p| p.providers.iter().map(|pr| pr.id.clone()))
            .collect(),
        catalog_path,
    }
}

/// spawn 脚本宿主并完成 init 握手。
///
/// 返回的 [`ScriptHost`] 处于 Running（providers / identities 已就位）；
/// 调用方随后 [`supervise`] 观察退出、用 [`shutdown`] 优雅关闭。
/// 握手失败的进程当场 kill + 关通道，外部观察不到中间态。
pub async fn spawn_host(
    data_dir: &Path,
    payload: ScriptInitPayload,
    callbacks: Arc<dyn CallbackHandler>,
) -> Result<Arc<ScriptHost>, ScriptHostError> {
    // Bun 运行时（已存在则探针校验，否则下载+校验+落盘——含网络，不占 init 超时）
    let bun_exe = bun::ensure(data_dir)
        .await
        .map_err(|e| ScriptHostError::Runtime(format!("{e:#}")))?;
    let host_dir = materialize_host_files(data_dir)?;

    // init 载荷先序列化再 spawn：反序列化不会失败也不能失败，但「spawn 后
    // 才可能出错的步骤」越少越好——早退路径上的子进程没人 kill。
    let init = serde_json::to_value(&payload)
        .map_err(|e| ScriptHostError::Serde(format!("init payload: {e}")))?;
    let params = serde_json::json!({
        "protocol": PROTOCOL_VERSION,
        "init": init,
    });

    let mut cmd = bun::run_command(&bun_exe, &host_dir);
    // spawn_host 的 future 被 drop（调用方取消 / 任务 abort）时兜底杀子进程：
    // 正常路径由 shutdown/kill 接管，这里防的是「spawn 成功但没人再管它」。
    cmd.kill_on_drop(true);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stdin = child.stdin.take().expect("stdin was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // stderr 有界直连 tracing + 留一份尾巴：宿主/脚本崩溃时能看到原因，
    // 刷屏会被硬切；握手失败时尾巴随错误一起上报（见 STDERR_TAIL_BYTES）。
    let stderr_tail: Arc<StdMutex<StderrTail>> = Arc::new(StdMutex::new(StderrTail::default()));
    tokio::spawn(pump_stderr(stderr, stderr_tail.clone()));

    let rpc = RpcChannel::new(Box::new(stdout), Box::new(stdin), callbacks);

    // init 握手（60s）：宿主要动态 import 全部 npm 包。失败路径当场
    // kill + 关读循环——不留半死进程。
    let handshake = tokio::time::timeout(INIT_TIMEOUT, rpc.call("init", params)).await;
    let result = match handshake {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => {
            rpc.shutdown();
            let _ = child.kill().await;
            return Err(ScriptHostError::Handshake(attach_stderr_tail(
                err,
                &stderr_tail,
            )));
        }
        Err(_) => {
            rpc.shutdown();
            let _ = child.kill().await;
            let err = RpcError {
                message: "script host init timeout (60s)".into(),
                code: Some("timeout".into()),
            };
            return Err(ScriptHostError::Handshake(attach_stderr_tail(
                err,
                &stderr_tail,
            )));
        }
    };

    let providers = parse_providers(&result);
    let identities = parse_identities(&result);
    if providers.is_empty() {
        tracing::info!("[Script] 宿主 init 完成，未申报任何 provider（空包集合或全部让位）");
    } else {
        let ids: Vec<&str> = providers.iter().map(|p| p.id.as_str()).collect();
        tracing::info!("[Script] 宿主 init 完成，providers: {ids:?}");
    }
    if !identities.is_empty() {
        tracing::debug!("[Script] 宿主 identities（声明 id → 实际接管 id）: {identities:?}");
    }

    Ok(Arc::new(ScriptHost {
        child: Mutex::new(child),
        rpc: Arc::new(rpc),
        providers,
        identities,
        state: RwLock::new(ScriptHostState::Running),
        shutdown_requested: AtomicBool::new(false),
    }))
}

/// 解析 init 应答的 identities：`{pkgId: {声明 id → 实际接管 id}}`。
///
/// 脚本导出的 provider id 与包声明可能不一致（id 冲突加缀 `-2`/`-3`），
/// 网关凭这份映射把「声明 id 的路由/账号」翻译到「实际接管 id」。形状由
/// js-host 定义（见 host/main.mjs），解析不出整段忽略（映射缺失只影响
/// 冲突包的路由，不值得为它拒绝宿主）。
fn parse_identities(result: &Value) -> serde_json::Map<String, Value> {
    result
        .get("identities")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default()
}

/// 解析 init 应答的 providers。
///
/// 主形状 `{"providers":[…]}`，兼容裸数组（与原生握手同款宽容）；
/// 单条解析失败静默丢弃（`ScriptProviderInfo` 全字段 default，正常不会失败）。
fn parse_providers(result: &Value) -> Vec<ScriptProviderInfo> {
    let arr = result
        .as_array()
        .cloned()
        .or_else(|| result.get("providers").and_then(|p| p.as_array()).cloned())
        .unwrap_or_default();
    arr.iter()
        .filter_map(|v| serde_json::from_value::<ScriptProviderInfo>(v.clone()).ok())
        .collect()
}

/// 观察宿主退出。
///
/// 用 `try_wait` 轮询（不长期持有 child 锁，避免与 shutdown 死锁）。
/// 网关主动关闭后的退出按正常路径记 debug；无征兆退出记 warn 并标
/// Crashed——重启/热切换策略由 runtime 层（script-core 的 #6）决定。
pub async fn supervise(host: Arc<ScriptHost>) {
    loop {
        let exited = {
            let mut child = host.child.lock().await;
            match child.try_wait() {
                Ok(Some(status)) => Some(status),
                Ok(None) => None,
                Err(e) => {
                    tracing::warn!("script host try_wait error: {e}");
                    return;
                }
            }
        };
        if let Some(status) = exited {
            if host.is_shutdown_requested() {
                tracing::debug!("script host exited after shutdown request: {status}");
            } else {
                tracing::warn!("script host exited unexpectedly: {status}");
                *host.state.write().await = ScriptHostState::Crashed;
            }
            return;
        }
        tokio::time::sleep(SUPERVISE_POLL).await;
    }
}

/// 优雅关闭宿主：先 RPC shutdown（宿主回帧后自行退出），5s 宽限后 kill
/// （kill 幂等：已退出是 no-op）。
pub async fn shutdown(host: &ScriptHost) {
    host.mark_shutdown();
    let rpc = host.rpc();
    let _ = tokio::time::timeout(SHUTDOWN_GRACE, rpc.call("shutdown", Value::Null)).await;
    rpc.shutdown();
    let mut child = host.child.lock().await;
    let _ = child.kill().await;
    *host.state.write().await = ScriptHostState::Shutdown;
}

/// 有界 stderr 尾巴（最近若干行，总量 ≤ [`STDERR_TAIL_BYTES`]）。
///
/// 握手失败时拼进错误消息——宿主起不来的根因（Bun 缺动态库 / 脚本抛错）
/// 只会出现在这里。超限时丢最旧的行：崩溃原因的末几行比开头的噪声重要。
#[derive(Default)]
struct StderrTail {
    lines: VecDeque<String>,
    bytes: usize,
}

impl StderrTail {
    fn push(&mut self, line: &str) {
        let line = if line.len() > STDERR_TAIL_BYTES {
            // 单行超限：留头部（Bun/JS 的报错签名通常在行首）
            format!("{}…(truncated)", utf8_head(line, STDERR_TAIL_BYTES))
        } else {
            line.to_string()
        };
        self.bytes += line.len() + 1;
        self.lines.push_back(line);
        while self.bytes > STDERR_TAIL_BYTES && self.lines.len() > 1 {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len() + 1;
            }
        }
    }

    fn render(&self) -> String {
        let mut out = String::with_capacity(self.bytes);
        for line in &self.lines {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
        }
        out
    }
}

/// 取不超过 `max` 字节的 UTF-8 前缀（不切断字符边界）。
fn utf8_head(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut cut = max;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    &s[..cut]
}

/// 把 stderr 尾巴拼进错误消息（无尾巴时原样返回）。
fn attach_stderr_tail(mut err: RpcError, tail: &StdMutex<StderrTail>) -> RpcError {
    let rendered = tail.lock().unwrap_or_else(|e| e.into_inner()).render();
    if !rendered.is_empty() {
        err.message = format!("{}\nscript-host stderr (tail):\n{rendered}", err.message);
    }
    err
}

/// stderr 有界行泵。
///
/// 按 `fill_buf` 分块攒行（`read_until` 对不换行的流是无界分配——脚本
/// 刷屏场景不可接受）；超过 [`STDERR_LINE_LIMIT`] 时硬切当行并继续。
/// 每行同时记进 `tail`（握手失败时随错误上报）。
async fn pump_stderr(stderr: ChildStderr, tail: Arc<StdMutex<StderrTail>>) {
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        loop {
            let available = match reader.fill_buf().await {
                Ok(a) => a,
                Err(_) => return,
            };
            if available.is_empty() {
                // EOF：把尾巴吐出去再收工
                if !buf.is_empty() {
                    emit_stderr_line(&buf, &tail);
                }
                return;
            }
            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..pos]);
                    reader.consume(pos + 1);
                    emit_stderr_line(&buf, &tail);
                    break;
                }
                None => {
                    buf.extend_from_slice(available);
                    let len = available.len();
                    reader.consume(len);
                    if buf.len() > STDERR_LINE_LIMIT {
                        // 行超限：硬切记录（防刷屏撑爆内存），下一轮从行中继续
                        emit_stderr_line(&buf, &tail);
                        buf.clear();
                        break;
                    }
                }
            }
        }
    }
}

fn emit_stderr_line(bytes: &[u8], tail: &StdMutex<StderrTail>) {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return;
    }
    tracing::warn!("[script-host stderr] {text}");
    tail.lock().unwrap_or_else(|e| e.into_inner()).push(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_hash_is_stable_and_24_hex() {
        let h1 = host_content_hash();
        let h2 = host_content_hash();
        assert_eq!(h1, h2, "同一份嵌入内容指纹必须稳定");
        assert_eq!(h1.len(), 24, "指纹取前 24 位");
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn host_files_all_embedded() {
        // 入口 + 全部相对 import 必须齐：少一个 = Bun 启动即 ModuleNotFound
        assert_eq!(HOST_FILES[0].0, "main.mjs", "入口必须排第一（文档约定）");
        for (name, content) in HOST_FILES {
            assert!(name.ends_with(".mjs"), "{name} 应是 .mjs");
            assert!(!content.trim().is_empty(), "{name} 不应为空");
        }
        for (name, _) in HOST_FILES.iter().skip(1) {
            let main = HOST_FILES.iter().find(|(n, _)| *n == "main.mjs").unwrap().1;
            assert!(
                main.contains(&format!("./{}", name.trim_end_matches(".mjs"))),
                "main.mjs 应相对 import {name}"
            );
        }
    }

    #[test]
    fn materialize_is_idempotent_and_complete() {
        let root = std::env::temp_dir().join(format!("aigx_host_rs_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = materialize_host_files(&root).expect("首次落盘应成功");
        assert!(dir.starts_with(script_plugins_root(&root)));
        assert!(dir.file_name().unwrap().to_string_lossy().len() == 24);
        for (name, content) in HOST_FILES {
            let written = std::fs::read(dir.join(name)).expect("文件应存在");
            assert_eq!(written, content.as_bytes(), "{name} 内容应与嵌入一致");
        }
        assert!(dir.join(".complete").is_file(), "完成标记应存在");
        // 再来一次：直接复用（不重写）
        let dir2 = materialize_host_files(&root).expect("幂等落盘应成功");
        assert_eq!(dir, dir2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_payload_serializes_snake_case_wire_keys() {
        // wire 契约：JS 侧按 packages/reserved/claimed/catalog_path 读
        let payload = ScriptInitPayload {
            packages: vec!["/data/x".into()],
            reserved: vec!["native".into()],
            claimed: vec!["kiro".into()],
            catalog_path: Some("/data/catalog.json".into()),
        };
        let v = serde_json::to_value(&payload).unwrap();
        for key in ["packages", "reserved", "claimed", "catalog_path"] {
            assert!(v.get(key).is_some(), "wire 键 {key} 必须存在（snake_case）");
        }
        assert!(
            v.get("catalogPath").is_none(),
            "camelCase 键是契约事故，不允许出现"
        );
    }

    #[test]
    fn parse_providers_accepts_object_and_bare_array() {
        let one =
            serde_json::json!({"id":"kiro","label":"Kiro","upstream":"anthropic","models":["m1"]});
        let from_object = parse_providers(&serde_json::json!({"providers":[one]}));
        assert_eq!(from_object.len(), 1);
        assert_eq!(from_object[0].id, "kiro");
        assert_eq!(from_object[0].upstream.as_deref(), Some("anthropic"));
        assert_eq!(from_object[0].models, vec!["m1".to_string()]);

        let from_array = parse_providers(&serde_json::json!([one]));
        assert_eq!(from_array.len(), 1);

        // 未知字段宽容（serde default），缺字段落默认值
        let minimal = parse_providers(&serde_json::json!({"providers":[{"id":"x"}]}));
        assert_eq!(minimal[0].label, "");
        assert!(minimal[0].upstream.is_none());

        // 空结果 = 空 providers（不报错：空包集合是合法状态）
        assert!(parse_providers(&Value::Null).is_empty());
    }

    #[test]
    fn build_init_payload_uses_hash_version_dir() {
        let pkg = ScriptPackage {
            id: "@magpie-community/kiro".into(),
            version: "1.2.0".into(),
            providers: vec![ScriptProviderInfo {
                id: "kiro".into(),
                label: "Kiro".into(),
                upstream: Some("anthropic".into()),
                models: vec![],
            }],
            ..Default::default()
        };
        let root = Path::new("/data/script-plugins");
        let payload = build_init_payload(
            root,
            std::slice::from_ref(&pkg),
            vec!["native".into()],
            Some("/c.json".into()),
        );
        // packages 必须是 version_dir 的绝对路径（hash 目录形态）
        assert_eq!(
            payload.packages,
            vec![pkg.version_dir(root).to_string_lossy().into_owned()]
        );
        assert!(
            payload.packages[0].contains("script-"),
            "目录名应是 hash 形态：{}",
            payload.packages[0]
        );
        assert!(!payload.packages[0].contains('@'), "scoped 包名不得进路径");
        // claimed 取包声明的 provider id
        assert_eq!(payload.claimed, vec!["kiro".to_string()]);
        assert_eq!(payload.reserved, vec!["native".to_string()]);
        assert_eq!(payload.catalog_path.as_deref(), Some("/c.json"));
    }

    #[test]
    fn parse_identities_extracts_map_and_tolerates_missing() {
        let result = serde_json::json!({
            "providers": [],
            "identities": { "@a/b": { "kiro": "kiro-2" } }
        });
        let m = parse_identities(&result);
        assert_eq!(
            m.get("@a/b")
                .and_then(|v| v.get("kiro"))
                .and_then(Value::as_str),
            Some("kiro-2")
        );
        // identities 缺失 / 形状不对 → 空映射（映射缺失只影响冲突包路由）
        assert!(parse_identities(&serde_json::json!({"providers": []})).is_empty());
        assert!(parse_identities(&serde_json::json!({"identities": "bad"})).is_empty());
        assert!(parse_identities(&Value::Null).is_empty());
    }

    #[test]
    fn stderr_tail_keeps_latest_lines_within_bound() {
        let mut tail = StderrTail::default();
        // 每行 1KiB，塞 20 行（20KiB > 8KiB）→ 只留尾巴，最旧的行被丢
        let line = "x".repeat(1024);
        for i in 0..20 {
            tail.push(&format!("{i:02}-{line}"));
        }
        assert!(
            tail.bytes <= STDERR_TAIL_BYTES,
            "总量必须受限: {}",
            tail.bytes
        );
        let rendered = tail.render();
        assert!(rendered.contains("19-"), "最新行必须保留");
        assert!(!rendered.contains("00-"), "最旧的行应被丢弃");
    }

    #[test]
    fn stderr_tail_truncates_single_oversized_line() {
        let mut tail = StderrTail::default();
        tail.push(&"y".repeat(STDERR_TAIL_BYTES * 2));
        assert_eq!(tail.lines.len(), 1);
        let rendered = tail.render();
        assert!(
            rendered.len() < STDERR_TAIL_BYTES * 2,
            "单行超限也要截断: {}",
            rendered.len()
        );
        assert!(rendered.contains("truncated"));
    }

    #[test]
    fn utf8_head_respects_char_boundary() {
        // 中文 3 字节/字：max 落在字符中间时必须回退，不 panic
        let s = "错误原因";
        assert_eq!(utf8_head(s, 4), "错");
        assert_eq!(utf8_head(s, 3), "错");
        assert_eq!(utf8_head(s, 100), s);
        assert_eq!(utf8_head(s, 0), "");
    }

    #[test]
    fn attach_stderr_tail_appends_only_when_present() {
        let tail = StdMutex::new(StderrTail::default());
        let err = RpcError {
            message: "init failed".into(),
            code: Some("closed".into()),
        };
        let untouched = attach_stderr_tail(err.clone(), &tail);
        assert_eq!(untouched.message, "init failed", "无 stderr 时消息不变");

        tail.lock()
            .unwrap()
            .push("Error relocating /lib/libstdc++.so.6: version GLIBCXX_3.4 not found");
        let with_tail = attach_stderr_tail(err, &tail);
        assert!(with_tail.message.starts_with("init failed"));
        assert!(
            with_tail.message.contains("Error relocating"),
            "根因必须进错误消息（管理面 lastError 的来源）"
        );
    }
}
