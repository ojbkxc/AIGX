//! 服务器自动更新 —— GitHub Release 检测 + 无第三方依赖的自解包 + 原地替换 + exec 自重启。
//!
//! ── 为什么不用 tar.gz / zip ─────────────────────────────────────
//! 本机无法在本仓库重新生成 `Cargo.lock`（无 cargo 环境），因此不引入 `tar`
//! / `flate2` / `zip` 之类的新依赖。发布包改用本模块自定义的无压缩容器：
//!
//! ```text
//! AIGXUP1\n
//! 重复：[u32 LE 路径长度][路径 UTF-8][u64 LE 文件长度][文件内容]
//! ```
//!
//! 打包由 `.github/workflows/build.yml` 的 `create-release` 任务用 Python
//! 脚本生成，解包就是本文件的流式读取 —— 两边格式逐字一致，改名要一起改。
//!
//! ── 更新流程 ──────────────────────────────────────────────────
//! 1. 检查：`GET {api}/repos/{repo}/releases/latest`（或预发布列表），
//!    选出 `AIGX-{ver}-server-linux-{arch}.upd` 与它的 `.sha256`；
//! 2. 下载：流式落盘（进度可轮询），同时算 sha256；
//! 3. 校验：校验文件里的 sha256 必须完全一致，否则拒绝安装；
//! 4. 解包：只允许 `aigx` 与 `static/` 条目，拒绝绝对路径与 `..`；
//! 5. 替换：当前程序先改名为 `aigx.bak`，再写入新二进制（0755）；
//!    static 目录同理备份为 `static.bak`，失败自动回滚；
//! 6. 重启：延迟 2 秒后用 `exec` 原地替换进程镜像（同一 PID，systemd /
//!    Docker 都不需要额外重启策略）。
//!
//! ── 安全边界 ──────────────────────────────────────────────────
//! - 仅 Linux 支持原地更新；Windows/macOS 只允许检查版本；
//! - 下载地址默认只允许 GitHub 资产域名（https）；配置了 `api_base`
//!   时额外放行该源的主机（显式配置才生效）；
//! - `.sha256` 是**必需**的，缺少校验文件的发布一律拒绝；
//! - 程序目录不可写时在开始前就拒绝，不做半途而废的替换；
//! - 备份文件（`aigx.bak` / `static.bak`）保留，支持一键回滚。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::config::UpdateConfig;
use crate::storage::FileStore;

/// 当前二进制版本（编译期取自 Cargo.toml）。
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const DEFAULT_REPO: &str = "ojbkxc/AIGX";
const GITHUB_API: &str = "https://api.github.com";
const USER_AGENT: &str = "aigx-updater";
/// 更新包体积上限（明显异常的响应直接中止，避免写满磁盘）。
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
/// 自定义发布包格式的魔数。
const BUNDLE_MAGIC: &[u8; 7] = b"AIGXUP1";
const CHECK_KEY: &str = "update:last_check";
const APPLIED_KEY: &str = "update:applied";
/// Windows 回滚挂起标记（#28）：Windows 运行中的 .exe 被文件锁死，
/// 无法像 Linux 那样 rename 原地替换——回滚动作只写标记+校验备份，
/// 下次服务重启时由 [`UpdateManager::consume_rollback_pending`] 完成
/// 替换（备份 aigx.bak → aigx.exe，进程未运行时文件锁已释放）。
const ROLLBACK_PENDING_KEY: &str = "update:rollback_pending";
/// 重启请求计数（每次 +1；前端据此与 startedAt 一起判定「重启真的发生了」）。
const RESTART_EPOCH_KEY: &str = "update:restart_epoch";
/// 最近一次 exec 自重启的失败原因（成功重启后进程无记忆，此项重启即清）。
const RESTART_ERROR_KEY: &str = "update:restart_error";
/// 「已应用待重启」标记（持久化：exec 失败/被外部重启后状态不丢）。
const PENDING_RESTART_KEY: &str = "update:pending_restart";
/// 两次检查之间的最短间隔（秒）：手动按钮连点会重复消耗 GitHub 匿名限额。
const MIN_CHECK_GAP: i64 = 60;

/// 本进程的启动时间（unix 秒）。exec 自重启**不换 PID**，前端只能靠
/// startedAt 的变化判定「新进程起来了」——这是无缝更新的关键信号。
static PROCESS_STARTED_AT: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

/// 进程启动时间（`new()` 时初始化一次）。
pub fn process_started_at() -> i64 {
    *PROCESS_STARTED_AT.get_or_init(now_ts)
}

/// 是否跑在容器里（Docker 在 `/` 放 `.dockerenv`）。容器场景 exec 原地重启
/// 不可靠（发布二进制是 glibc，Alpine 运行时是 musl），UI 需要提示拉新镜像。
pub fn containerized() -> bool {
    std::path::Path::new("/.dockerenv").exists()
}

/// `static/version.json`（CI 构建时写入，随 .upd 打包）里的版本号。
fn static_version(exe: &Path) -> Option<String> {
    let path = static_dir_path(exe).join("version.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("version")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().trim_start_matches('v').to_string())
        .filter(|s| !s.is_empty())
}

/// Release 里的一个资产。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateAsset {
    pub name: String,
    pub size: u64,
    pub url: String,
}

/// 归一化后的 Release 信息（持久化到 KV，重启后无需重新请求）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseInfo {
    pub tag: String,
    pub name: String,
    pub notes: String,
    pub published_at: String,
    pub page_url: String,
    pub prerelease: bool,
    /// 本机架构的服务器更新包（Linux 之外为 None）。
    pub bundle: Option<UpdateAsset>,
    /// 更新包的 `.sha256` 伴随文件。
    pub checksum: Option<UpdateAsset>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedCheck {
    #[serde(default)]
    latest: Option<ReleaseInfo>,
    #[serde(default)]
    checked_at: i64,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    retry_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AppliedMarker {
    #[serde(default)]
    version: String,
    #[serde(default)]
    applied_at: i64,
    #[serde(default)]
    previous_version: String,
}

/// 下载进度（前端轮询展示）。
#[derive(Debug, Clone, Default, Serialize)]
struct DownloadProgress {
    active: bool,
    done: bool,
    received: u64,
    total: u64,
    percent: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

struct Inner {
    latest: Option<ReleaseInfo>,
    checked_at: i64,
    last_error: Option<String>,
    retry_at: i64,
    checking: bool,
    applying: bool,
    pending_restart: bool,
    applied: Option<AppliedMarker>,
    progress: DownloadProgress,
    /// 重启请求计数（持久化，跨进程存活）。
    restart_epoch: u64,
    /// 最近一次 exec 自重启的失败原因。
    restart_error: Option<String>,
}

/// 更新管理器（内部 Arc + Mutex，Clone 共享同一状态）。
#[derive(Clone)]
pub struct UpdateManager {
    inner: Arc<parking_lot::Mutex<Inner>>,
    store: Arc<FileStore>,
    data_dir: PathBuf,
    egress: Arc<crate::egress::EgressCache>,
    /// 启动时解析一次当前程序路径：更新替换后 `/proc/self/exe` 会带
    /// `(deleted)` 后缀，不能再用它来 exec 重启。
    exe: PathBuf,
    /// 程序目录是否可写（启动时探测一次，避免每次状态轮询都写探针文件）。
    writable: bool,
}

impl UpdateManager {
    pub fn new(store: Arc<FileStore>, data_dir: PathBuf) -> Self {
        let persisted = store
            .get::<PersistedCheck>(CHECK_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let applied = store.get::<AppliedMarker>(APPLIED_KEY).ok().flatten();
        // 重启相关状态跨进程恢复：exec 失败/被 systemd 重启后不丢
        let mut pending_restart = store
            .get::<bool>(PENDING_RESTART_KEY)
            .ok()
            .flatten()
            .unwrap_or(false);
        let restart_epoch = store
            .get::<u64>(RESTART_EPOCH_KEY)
            .ok()
            .flatten()
            .unwrap_or(0);
        let mut restart_error = store.get::<String>(RESTART_ERROR_KEY).ok().flatten();
        // 新进程已在跑「应用后的版本」= 更新已生效：清掉待重启与失败残留。
        // （exec 会带上旧参数与环境，这里靠版本对齐判定，而不是「新进程=成功」——
        //  systemd 外部重启也会走到这，同样成立。）
        if applied
            .as_ref()
            .is_some_and(|marker| marker.version == CURRENT_VERSION)
            && (pending_restart || restart_error.is_some())
        {
            pending_restart = false;
            restart_error = None;
            let _ = store.put(PENDING_RESTART_KEY, &false);
            let _ = store.delete(RESTART_ERROR_KEY);
        }
        // #28（Windows 回滚）：启动时消费回滚挂起标记——进程此时刚起，
        // .exe 文件锁已由上一个退出进程释放，可以安全替换。
        // Linux 走原有 rename 原地路径（行为不变），这里只服务 Windows；
        // 标记在任意平台写入都合法，消费端按平台分派。
        let consume_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aigx"));
        if store
            .get::<bool>(ROLLBACK_PENDING_KEY)
            .ok()
            .flatten()
            .unwrap_or(false)
        {
            match consume_rollback_pending(&consume_exe) {
                Ok(()) => {
                    let _ = store.delete(ROLLBACK_PENDING_KEY);
                    tracing::warn!(
                        "pending rollback applied at startup (binary restored from .bak)"
                    );
                }
                Err(e) => {
                    // 替换失败不阻塞启动：保留标记，下次重启再试；记录原因
                    // （store.put 泛型吃 &T——先绑定再引用，E0308）
                    let message = format!("rollback pending failed: {e}");
                    let _ = store.put(RESTART_ERROR_KEY, &message);
                    tracing::error!("pending rollback failed: {e}");
                }
            }
        }
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aigx"));
        let writable = writable_exe_dir(&exe);
        let _ = std::fs::create_dir_all(data_dir.join("updates"));
        let _ = PROCESS_STARTED_AT.set(now_ts());
        Self {
            inner: Arc::new(parking_lot::Mutex::new(Inner {
                latest: persisted.latest,
                checked_at: persisted.checked_at,
                last_error: persisted.last_error,
                retry_at: persisted.retry_at,
                checking: false,
                applying: false,
                pending_restart,
                applied,
                progress: DownloadProgress::default(),
                restart_epoch,
                restart_error,
            })),
            store,
            data_dir,
            egress: Arc::new(crate::egress::EgressCache::with_timeout(
                Duration::from_secs(600),
            )),
            exe,
            writable,
        }
    }

    fn lock(&self) -> parking_lot::MutexGuard<'_, Inner> {
        self.inner.lock()
    }

    /// 把内存中的检查结果落库（重启后不重复消耗 GitHub 限额）。
    fn persist(&self) {
        let snapshot = {
            let guard = self.lock();
            PersistedCheck {
                latest: guard.latest.clone(),
                checked_at: guard.checked_at,
                last_error: guard.last_error.clone(),
                retry_at: guard.retry_at,
            }
        };
        if let Err(e) = self.store.put(CHECK_KEY, &snapshot) {
            tracing::warn!("persist update check state failed: {e}");
        }
    }

    /// 给界面/API/AI 工具的状态快照（**不触发网络请求**）。
    pub fn status(&self, config: &UpdateConfig) -> Value {
        let guard = self.lock();
        let latest = guard.latest.as_ref();
        let latest_tag = latest.map(|release| release.tag.as_str()).unwrap_or("");
        let comparison = if latest.is_some() {
            compare_versions(latest_tag, CURRENT_VERSION)
        } else {
            None
        };
        let download =
            if guard.progress.active || guard.progress.done || guard.progress.error.is_some() {
                json!(guard.progress)
            } else {
                Value::Null
            };
        // 冷却期可见：刚查过就直说，别让按钮像坏了（前端据此显示倒计时）
        let now = now_ts();
        let since_check = guard.checked_at.max(0);
        let recently_checked =
            since_check > 0 && now - since_check < MIN_CHECK_GAP && !guard.checking;
        let retry_after_secs = if recently_checked {
            MIN_CHECK_GAP - (now - since_check)
        } else {
            0
        };
        let static_ver = static_version(&self.exe);
        let version_mismatch = static_ver
            .as_deref()
            .map(|v| v != CURRENT_VERSION)
            .unwrap_or(false);
        json!({
            "currentVersion": CURRENT_VERSION,
            "latestVersion": if latest_tag.is_empty() { Value::Null } else { json!(latest_tag) },
            "hasUpdate": comparison.map(|order| order > 0),
            "comparison": match comparison {
                Some(order) if order > 0 => json!("newer"),
                Some(order) if order < 0 => json!("older"),
                Some(_) => json!("same"),
                None => Value::Null,
            },
            "releaseName": latest.map(|release| release.name.clone()),
            "notes": latest.map(|release| release.notes.clone()),
            "publishedAt": latest.map(|release| release.published_at.clone()),
            "pageUrl": latest.map(|release| release.page_url.clone()),
            "prerelease": latest.map(|release| release.prerelease).unwrap_or(false),
            "bundle": latest.and_then(|release| release.bundle.clone()),
            "checksumAvailable": latest
                .map(|release| release.checksum.is_some())
                .unwrap_or(false),
            "checkedAt": guard.checked_at,
            "lastError": guard.last_error,
            "retryAt": guard.retry_at,
            "checking": guard.checking,
            "applying": guard.applying,
            "pendingRestart": guard.pending_restart,
            "download": download,
            "supported": supported(),
            "writable": self.writable,
            "backupAvailable": backup_path(&self.exe).is_file(),
            "downloadDir": self.data_dir.join("updates").to_string_lossy(),
            // #18-①：更新产物磁盘占用（.upd/.sha256/staging 合计字节）——
            // 面板显示回收效果；冷路径只在本端点算一次
            "downloadDirBytes": std::fs::read_dir(self.data_dir.join("updates"))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|e| e.metadata().ok())
                        .map(|m| m.len())
                        .sum::<u64>()
                })
                .unwrap_or(0),
            "repository": repo_of(config),
            "apiBase": api_root(config),
            "appliedVersion": guard.applied.as_ref().map(|marker| marker.version.clone()),
            "appliedAt": guard.applied.as_ref().map(|marker| marker.applied_at).unwrap_or(0),
            // ── 无缝更新判定用 ──
            // 本进程启动时间：exec 不换 PID，前端只能靠它的变化确认「新进程起来了」
            "startedAt": process_started_at(),
            "restartEpoch": guard.restart_epoch,
            "restartError": guard.restart_error,
            "staticVersion": static_ver,
            "versionMismatch": version_mismatch,
            "containerized": containerized(),
            "recentlyChecked": recently_checked,
            "retryAfterSecs": retry_after_secs,
        })
    }

    /// 检查更新（手动与定时共用）。失败时返回可读原因并落冷却。
    pub async fn check(&self, config: &UpdateConfig) -> Result<Value, String> {
        let now = now_ts();
        let recently_checked = {
            let guard = self.lock();
            if guard.checking {
                return Err("已有一次更新检查正在进行".to_string());
            }
            if guard.retry_at > now {
                return Err(format!(
                    "更新检查冷却中，请约 {} 秒后再试",
                    guard.retry_at - now
                ));
            }
            guard.checked_at > 0 && now - guard.checked_at < MIN_CHECK_GAP
        };
        // 刚查过：直接复用最近结果（前端连点不应把匿名限额连点掉）
        if recently_checked {
            return Ok(self.status(config));
        }
        self.lock().checking = true;
        let outcome = self.fetch_release(config).await;
        {
            let mut guard = self.lock();
            guard.checking = false;
            match &outcome {
                Ok(release) => {
                    guard.latest = release.clone();
                    guard.checked_at = now_ts();
                    guard.retry_at = 0;
                    guard.last_error = None;
                }
                Err((message, retry_at)) => {
                    guard.last_error = Some(message.clone());
                    guard.retry_at = *retry_at;
                }
            }
        }
        self.persist();
        match outcome {
            Ok(release) => {
                let tag = release
                    .as_ref()
                    .map(|item| item.tag.as_str())
                    .unwrap_or("(无)");
                tracing::info!("update check: current={CURRENT_VERSION} latest={tag}");
                Ok(self.status(config))
            }
            Err((message, _)) => Err(message),
        }
    }

    /// 启动后台更新（下载 → 校验 → 解包 → 替换；按配置自动重启）。
    ///
    /// 立即返回状态快照，进度与结果通过 [`Self::status`] 轮询。
    /// `force`（修复模式）：跳过「已是最新」与「待重启」拦截，用最新 `.upd`
    /// 重新安装一遍——修「二进制换了 static 没换」这类半更新。
    pub fn start_apply(&self, config: &UpdateConfig, force: bool) -> Result<Value, String> {
        if !supported() {
            return Err(
                "当前平台不支持原地更新（仅 Linux 服务器/容器）：请手动替换发布包".to_string(),
            );
        }
        let release = {
            let mut guard = self.lock();
            if guard.applying {
                return Err("更新正在执行中，请稍候".to_string());
            }
            if guard.pending_restart && !force {
                return Err("更新已应用，请先重启服务".to_string());
            }
            if !self.writable {
                return Err(
                    "程序目录不可写，无法原地更新（容器内请确认 /opt/aigx 可写）".to_string(),
                );
            }
            let Some(release) = guard.latest.clone() else {
                return Err("请先检查更新".to_string());
            };
            if !force && compare_versions(&release.tag, CURRENT_VERSION).unwrap_or(0) <= 0 {
                return Err("当前已是最新版本".to_string());
            }
            let Some(bundle) = release.bundle.clone() else {
                return Err("该版本没有提供服务器更新包（.upd）".to_string());
            };
            if release.checksum.is_none() {
                return Err("该版本没有提供 .sha256 校验文件，已拒绝更新".to_string());
            }
            guard.applying = true;
            guard.pending_restart = false;
            let _ = self.store.put(PENDING_RESTART_KEY, &false);
            guard.last_error = None;
            guard.progress = DownloadProgress {
                active: true,
                done: false,
                received: 0,
                total: bundle.size,
                percent: 0,
                error: None,
            };
            release
        };
        let manager = self.clone();
        let config_clone = config.clone();
        tokio::spawn(async move {
            manager.run_apply(config_clone, release).await;
        });
        Ok(self.status(config))
    }

    async fn run_apply(&self, config: UpdateConfig, release: ReleaseInfo) {
        let result = self.do_apply(&config, &release).await;
        let success = result.is_ok();
        {
            let mut guard = self.lock();
            guard.applying = false;
            if success {
                guard.pending_restart = true;
                guard.progress.active = false;
                guard.progress.done = true;
                guard.progress.percent = 100;
                guard.progress.error = None;
            } else {
                guard.progress.active = false;
                guard.progress.error = result.as_ref().err().cloned();
                guard.last_error = result.as_ref().err().cloned();
            }
        }
        if success {
            // 待重启标记落盘：exec 失败/进程被外部重启后状态不丢
            if let Err(e) = self.store.put(PENDING_RESTART_KEY, &true) {
                tracing::warn!("persist pending_restart failed: {e}");
            }
            let marker = AppliedMarker {
                version: release.tag.clone(),
                applied_at: now_ts(),
                previous_version: CURRENT_VERSION.to_string(),
            };
            self.lock().applied = Some(marker.clone());
            if let Err(e) = self.store.put(APPLIED_KEY, &marker) {
                tracing::warn!("persist applied marker failed: {e}");
            }
            tracing::info!(
                "update applied: {} (previous {})",
                release.tag,
                CURRENT_VERSION
            );
            // #18-①：回收更新产物（生产实证 39 个备份 527MB + 8 个 .upd
            // 253MB，更新机制不回收）。保留策略：
            // - 本轮下载的 .upd（回滚候选，rollback() 复用 force 模式）
            // - 旧版本 bundle（previous_version 对应的包，供再回滚）
            // 其余 .upd/.sha256 全删；staging 每轮已清。
            let updates_dir = self.data_dir.join("updates");
            // `host_arch()` 在未知架构返回 None —— 此时不保留旧 bundle 资产
            // （`and_then` 而非 `map`：map 会产出 `Option<Option<String>>`，
            // 且把 `Option<&str>` 直接喂给 `&str` 形参是 E0308）。
            let keep = [
                release.bundle.as_ref().map(|b| b.name.clone()),
                host_arch().and_then(|arch| server_bundle_name(&marker.previous_version, arch)),
            ];
            if let Ok(entries) = std::fs::read_dir(&updates_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_bundle_artifact = name.ends_with(".upd") || name.ends_with(".sha256");
                    let is_kept = keep
                        .iter()
                        .flatten()
                        .any(|k| name == *k || name == format!("{k}.sha256"));
                    if is_bundle_artifact && !is_kept {
                        let _ = std::fs::remove_file(entry.path());
                        tracing::debug!("update artifact reclaimed: {name}");
                    }
                }
            }
            self.persist();
            if config.restart_after_apply {
                if let Err(e) = self.request_restart() {
                    tracing::error!("auto restart after update failed: {e}");
                }
            }
        } else {
            tracing::error!(
                "update apply failed: {}",
                result.as_ref().err().map(String::as_str).unwrap_or("")
            );
            self.persist();
        }
    }

    async fn do_apply(&self, config: &UpdateConfig, release: &ReleaseInfo) -> Result<(), String> {
        let bundle = release
            .bundle
            .clone()
            .ok_or_else(|| "该版本没有提供服务器更新包".to_string())?;
        let checksum = release
            .checksum
            .clone()
            .ok_or_else(|| "该版本没有提供 .sha256 校验文件".to_string())?;
        let dir = self.data_dir.join("updates");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| format!("创建更新目录失败: {e}"))?;
        let archive = dir.join(safe_file_name(&bundle.name));
        let _ = tokio::fs::remove_file(&archive).await;
        // 先取校验值：下载完才发现没有校验文件等于白下
        let expected = download_text(self, config, &checksum.url).await?;
        let expected = parse_checksum(&expected)?;
        let actual = self.download_bundle(config, &bundle, &archive).await?;
        if actual != expected {
            tracing::error!("bundle sha256 mismatch: expected {expected} actual {actual}");
            let _ = tokio::fs::remove_file(&archive).await;
            return Err("更新包校验失败（sha256 不匹配），已放弃安装".to_string());
        }
        let staging = dir.join("staging");
        remove_dir(&staging);
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|e| format!("创建解包目录失败: {e}"))?;
        extract_bundle(&archive, &staging).await?;
        self.install_staged(&staging)?;
        remove_dir(&staging);
        Ok(())
    }

    /// 流式下载更新包并返回实际 sha256（hex）。
    async fn download_bundle(
        &self,
        config: &UpdateConfig,
        asset: &UpdateAsset,
        path: &Path,
    ) -> Result<String, String> {
        assert_downloadable(&asset.url, config)?;
        let client = self.client(config)?;
        let response = client
            .get(&asset.url)
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/octet-stream")
            .send()
            .await
            .map_err(|e| format!("下载更新包失败: {e}"))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(format!("下载更新包失败：更新源返回 HTTP {status}"));
        }
        let declared = response.content_length().unwrap_or(0);
        if declared > MAX_BUNDLE_BYTES {
            return Err(format!(
                "更新包体积异常（{} MB），已中止",
                declared / 1024 / 1024
            ));
        }
        {
            let mut guard = self.lock();
            guard.progress.total = if declared > 0 {
                declared
            } else {
                guard.progress.total
            };
        }
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|e| format!("创建更新包文件失败: {e}"))?;
        let mut hasher = Sha256::new();
        let mut received: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("下载中断: {e}"))?;
            received += chunk.len() as u64;
            if received > MAX_BUNDLE_BYTES {
                return Err("更新包超过体积上限，已中止".to_string());
            }
            hasher.update(&chunk);
            tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
                .await
                .map_err(|e| format!("写入更新包失败: {e}"))?;
            let mut guard = self.lock();
            guard.progress.received = received;
            if guard.progress.total == 0 {
                guard.progress.total = received;
            }
            let total = guard.progress.total;
            guard.progress.percent = percent_of(received, total);
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|e| format!("写入更新包失败: {e}"))?;
        drop(file);
        if declared > 0 && received != declared {
            return Err(format!(
                "更新包不完整（期望 {declared} 字节，实际 {received} 字节）"
            ));
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// 原地替换二进制与 static 目录（失败自动回滚）。
    fn install_staged(&self, staging: &Path) -> Result<(), String> {
        let new_exe = staging.join("aigx");
        let new_static = staging.join("static");
        if !new_exe.is_file() {
            return Err("更新包缺少 aigx 二进制".to_string());
        }
        if !new_static.join("index.html").is_file() {
            return Err("更新包缺少 static/index.html".to_string());
        }
        let static_dir = static_dir_path(&self.exe);
        let exe_bak = backup_path(&self.exe);
        let static_bak = backup_path(&static_dir);
        let _ = std::fs::remove_file(&exe_bak);
        remove_dir(&static_bak);
        // 1) 当前程序改名做备份（Linux 允许重命名运行中的可执行文件）
        std::fs::rename(&self.exe, &exe_bak).map_err(|e| format!("备份当前程序失败: {e}"))?;
        // 2) 写入新程序
        let installed = std::fs::copy(&new_exe, &self.exe)
            .map_err(|e| format!("写入新程序失败: {e}"))
            .and_then(|_| set_executable(&self.exe));
        if let Err(e) = installed {
            let _ = std::fs::rename(&exe_bak, &self.exe);
            return Err(e);
        }
        // 3) 替换 static 目录（失败时把程序与目录一起回滚）
        let static_result = (|| -> Result<(), String> {
            if static_dir.exists() {
                std::fs::rename(&static_dir, &static_bak)
                    .map_err(|e| format!("备份静态目录失败: {e}"))?;
            }
            copy_dir(&new_static, &static_dir).map_err(|e| format!("写入静态目录失败: {e}"))
        })();
        if let Err(e) = static_result {
            remove_dir(&static_dir);
            if static_bak.exists() {
                let _ = std::fs::rename(&static_bak, &static_dir);
            }
            let _ = std::fs::rename(&exe_bak, &self.exe);
            return Err(e);
        }
        Ok(())
    }

    /// 延迟 2 秒后 exec 自重启（先让 HTTP 响应/SSE 收尾）。
    ///
    /// 重启判定交给前端：`restartEpoch`（本次请求）+ `startedAt`（新进程
    /// 启动时间变化）——exec **不换 PID**，PID 判不出。exec 失败会把原因
    /// 写进 `update:restart_error` 并保留进程，前端能看到、不是假成功。
    pub fn request_restart(&self) -> Result<Value, String> {
        if !supported() {
            return Err("当前平台不支持网页端重启，请在服务器上重启 AIGX".to_string());
        }
        let exe = self.exe.clone();
        let epoch = {
            let mut guard = self.lock();
            guard.restart_epoch += 1;
            guard.restart_error = None;
            guard.restart_epoch
        };
        if let Err(e) = self.store.put(RESTART_EPOCH_KEY, &epoch) {
            tracing::warn!("persist restart epoch failed: {e}");
        }
        let _ = self.store.delete(RESTART_ERROR_KEY);
        let store = self.store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            tracing::warn!("restarting AIGX via exec: {}", exe.display());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
                // cwd 显式归位到 exe 目录：static 目录解析/相对路径才稳定
                if let Some(dir) = exe.parent() {
                    let _ = std::env::set_current_dir(dir);
                }
                let error = std::process::Command::new(&exe)
                    .args(args)
                    .envs(std::env::vars_os())
                    .exec();
                // exec 只在失败时返回：把原因留下，前端不再假报「已重启」
                let message = format!("exec 自重启失败：{error}");
                tracing::error!("{message}");
                let _ = store.put(RESTART_ERROR_KEY, &message);
            }
            #[cfg(not(unix))]
            {
                let _ = (exe, store);
                tracing::error!("self restart is only supported on unix");
            }
        });
        Ok(json!({ "restarting": true, "delaySecs": 2, "restartEpoch": epoch }))
    }

    /// 回滚到上一版本（备份由上一次成功安装留下）。
    ///
    /// 平台分派（#28）：Linux 沿用 rename 原地替换（运行中的可执行文件可
    /// 改名）；Windows 的 .exe 被运行锁锁死，只能写挂起标记、下次启动时
    /// 替换（[`consume_rollback_pending`]）。两端 HTTP 语义一致：
    /// Linux 返回 restored（重启即生效），Windows 返回 scheduled（用户
    /// 重启服务后生效）——前端 UpdatePanel 据此提示，不再「按钮在但报错」。
    pub fn rollback(&self, config: &UpdateConfig) -> Result<Value, String> {
        if !supported() {
            return Err("当前平台不支持原地回滚".to_string());
        }
        // Windows：写挂起标记 + 预检备份存在，替换推迟到下次启动
        if cfg!(not(target_os = "linux")) {
            let exe_bak = backup_path(&self.exe);
            if !exe_bak.is_file() {
                return Err("没有可回滚的程序备份（aigx.exe.bak）".to_string());
            }
            self.store
                .put(ROLLBACK_PENDING_KEY, &true)
                .map_err(|e| format!("写入回滚标记失败: {e}"))?;
            {
                let mut guard = self.lock();
                guard.pending_restart = true;
                guard.applied = None;
            }
            let _ = self.store.delete(APPLIED_KEY);
            self.persist();
            if config.restart_after_apply {
                // 尽力触发重启（Windows 上 exec 重启不支持时提示手动重启）
                let _ = self.request_restart();
            }
            tracing::warn!("rollback scheduled for next restart (windows)");
            return Ok(json!({
                "scheduled": true,
                "needRestart": true,
                "message": "回滚已安排：重启 AIGX 服务后生效（Windows 下运行中的程序无法原地替换）"
            }));
        }
        {
            let guard = self.lock();
            if guard.applying {
                return Err("更新正在执行，暂时无法回滚".to_string());
            }
        }
        let exe_bak = backup_path(&self.exe);
        if !exe_bak.is_file() {
            return Err("没有可回滚的程序备份（aigx.bak）".to_string());
        }
        let failed_exe = self.exe.with_file_name(format!(
            "{}.failed",
            file_name_of(&self.exe).unwrap_or("aigx")
        ));
        let _ = std::fs::remove_file(&failed_exe);
        std::fs::rename(&self.exe, &failed_exe).map_err(|e| format!("移走当前程序失败: {e}"))?;
        std::fs::rename(&exe_bak, &self.exe).map_err(|e| {
            // 回滚失败：尽量把当前程序换回去
            let _ = std::fs::rename(&failed_exe, &self.exe);
            format!("恢复备份程序失败: {e}")
        })?;
        let _ = std::fs::remove_file(&failed_exe);
        // static 目录回滚（有备份才动）
        let static_dir = static_dir_path(&self.exe);
        let static_bak = backup_path(&static_dir);
        if static_bak.exists() {
            let failed_static = static_dir.with_file_name(format!(
                "{}.failed",
                file_name_of(&static_dir).unwrap_or("static")
            ));
            remove_dir(&failed_static);
            if static_dir.exists() {
                let _ = std::fs::rename(&static_dir, &failed_static);
            }
            if let Err(e) = std::fs::rename(&static_bak, &static_dir) {
                tracing::error!("restore static dir failed: {e}");
                return Err(format!(
                    "程序已回滚，但 static 目录恢复失败: {e}，请手动检查"
                ));
            }
            remove_dir(&failed_static);
        }
        {
            let mut guard = self.lock();
            guard.pending_restart = true;
            guard.last_error = None;
            // 回滚后「已应用版本」不再成立：清掉标记，避免状态页显示误导信息
            guard.applied = None;
        }
        if let Err(e) = self.store.delete(APPLIED_KEY) {
            tracing::warn!("clear applied marker failed: {e}");
        }
        self.persist();
        tracing::warn!("update rolled back to previous version, restart to take effect");
        if config.restart_after_apply {
            let _ = self.request_restart();
        }
        Ok(json!({ "restored": true, "needRestart": true }))
    }

    /// 定时任务入口：按配置的间隔检查，必要时自动应用并重启。
    pub async fn tick(&self, config: &UpdateConfig) {
        if !config.enabled || !config.auto_check {
            return;
        }
        let interval = config.interval_hours.max(1) as i64 * 3600;
        if now_ts() - self.lock().checked_at < interval {
            return;
        }
        match self.check(config).await {
            Ok(status) => {
                if config.auto_apply && status["hasUpdate"].as_bool() == Some(true) {
                    if let Err(e) = self.start_apply(config, false) {
                        tracing::warn!("auto update apply failed: {e}");
                    }
                }
            }
            Err(e) => tracing::warn!("auto update check failed: {e}"),
        }
    }
}

impl UpdateManager {
    fn client(&self, config: &UpdateConfig) -> Result<Arc<reqwest::Client>, String> {
        self.egress
            .client(Some(config.proxy.trim()))
            .map_err(|e| format!("更新代理配置错误: {e}"))
    }

    /// 请求更新源并归一化结果；`Err` 的第二个元素是最早可重试时刻。
    async fn fetch_release(
        &self,
        config: &UpdateConfig,
    ) -> Result<Option<ReleaseInfo>, (String, i64)> {
        let repo = repo_of(config);
        let root = api_root(config);
        let url = if config.include_prerelease {
            format!("{root}/repos/{repo}/releases?per_page=20")
        } else {
            format!("{root}/repos/{repo}/releases/latest")
        };
        let client = self.client(config).map_err(|e| (e, 0))?;
        let mut request = client
            .get(&url)
            .header(reqwest::header::ACCEPT, "application/vnd.github+json")
            .header(reqwest::header::USER_AGENT, USER_AGENT);
        if !config.token.trim().is_empty() {
            request = request.bearer_auth(config.token.trim());
        }
        let response = request
            .send()
            .await
            .map_err(|e| (format!("请求更新源失败: {e}"), now_ts() + 300))?;
        let status = response.status().as_u16();
        let retry_at = retry_deadline(response.headers(), status);
        if status == 404 {
            // 仓库还没有任何 Release：不是错误，按「无更新」处理
            return Ok(None);
        }
        if status == 403 || status == 429 {
            let wait = (retry_at - now_ts()).max(60) / 60;
            return Err((
                format!(
                    "更新源访问受限（HTTP {status}），约 {wait} 分钟后可重试；可在更新设置里配置 GitHub 令牌提高限额"
                ),
                retry_at,
            ));
        }
        if !(200..300).contains(&status) {
            return Err((format!("更新源返回 HTTP {status}"), retry_at));
        }
        let payload: Value = response
            .json()
            .await
            .map_err(|e| (format!("解析更新源响应失败: {e}"), retry_at))?;
        let item = if config.include_prerelease {
            pick_listed_release(&payload, is_prerelease_version(CURRENT_VERSION)).cloned()
        } else {
            Some(payload)
        };
        Ok(item.map(|value| release_info(&value, &repo)))
    }
}

/// 下载一个小文本文件（.sha256）。
async fn download_text(
    manager: &UpdateManager,
    config: &UpdateConfig,
    url: &str,
) -> Result<String, String> {
    assert_downloadable(url, config)?;
    let client = manager.client(config)?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("下载校验文件失败: {e}"))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(format!("下载校验文件失败：HTTP {status}"));
    }
    let text = response
        .text()
        .await
        .map_err(|e| format!("读取校验文件失败: {e}"))?;
    if text.len() > 4096 {
        return Err("校验文件体积异常，已中止".to_string());
    }
    Ok(text)
}

// ─── 平台与路径 ────────────────────────────────────────────────

/// 是否支持原地更新（仅 Linux 服务器/容器）。
pub fn supported() -> bool {
    // #28：Windows 产物是头等发布目标（6 平台矩阵硬门禁），网页端
    // 更新/回滚语义在 Windows 上同样成立——更新沿用 .upd 链路，
    // 回滚走「挂起标记 + 下次启动替换」（运行中的 .exe 被文件锁锁死，
    // 只能推迟到进程退出后替换）。
    cfg!(any(target_os = "linux", target_os = "windows"))
}

/// 本机架构对应的发布包后缀。
pub fn host_arch() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("amd64"),
        "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// 服务器更新包资产名（与 CI 打包脚本、发布说明三方一致）。
pub fn server_bundle_name(version: &str, arch: &str) -> Option<String> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let version = version.trim();
    if version.is_empty() || arch.is_empty() {
        return None;
    }
    Some(format!("AIGX-{version}-server-linux-{arch}.upd"))
}

fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

fn repo_of(config: &UpdateConfig) -> String {
    let trimmed = config.repo.trim();
    if trimmed.is_empty() {
        DEFAULT_REPO.to_string()
    } else {
        trimmed.to_string()
    }
}

fn api_root(config: &UpdateConfig) -> String {
    let base = config.api_base.trim().trim_end_matches('/');
    if base.is_empty() {
        GITHUB_API.to_string()
    } else {
        base.to_string()
    }
}

fn percent_of(received: u64, total: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    (received.saturating_mul(100) / total).min(100)
}

fn writable_exe_dir(exe: &Path) -> bool {
    let Some(dir) = exe.parent() else {
        return false;
    };
    let probe = dir.join(".aigx-update-write-test");
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// static 目录定位：**exe 同级 `static/` 是权威**（更新替换的就是它）；
/// 仅当它不存在时才回落 web 层候选链（cwd 下的 ./static 等），避免 cwd
/// 里恰好有同名目录时替换错位置。
fn static_dir_path(exe: &Path) -> PathBuf {
    let adjacent = exe
        .parent()
        .map(|parent| parent.join("static"))
        .unwrap_or_else(|| PathBuf::from("static"));
    if adjacent.is_dir() {
        return adjacent;
    }
    crate::web::find_static_dir().unwrap_or(adjacent)
}

/// 消费 Windows 回滚挂起标记：aigx.bak → aigx（进程未运行，锁已释放）。
///
/// 仅替换二进制；static 目录的备份恢复沿用 [`UpdateManager::rollback`]
/// 在 Linux 上的同一逻辑——但 Windows 启动时 static 没有运行锁，直接
/// 按备份恢复（有备份才动，与 Linux 行为一致）。
fn consume_rollback_pending(exe: &Path) -> Result<(), String> {
    let exe_bak = backup_path(exe);
    if !exe_bak.is_file() {
        // 函数签名是 Result<(), String>（与 rollback()/request_restart 的
        // String 错误口径一致），不用 anyhow::bail!（类型不匹配 E0308）
        return Err("没有可回滚的程序备份".to_string());
    }
    let failed = exe.with_file_name(format!("{}.failed", file_name_of(exe).unwrap_or("aigx")));
    let _ = std::fs::remove_file(&failed);
    std::fs::rename(exe, &failed).map_err(|e| format!("移走当前程序失败: {e}"))?;
    std::fs::rename(&exe_bak, exe).map_err(|e| {
        let _ = std::fs::rename(&failed, exe);
        format!("恢复备份程序失败: {e}")
    })?;
    let _ = std::fs::remove_file(&failed);
    // static 目录恢复（有备份才动）
    let static_dir = static_dir_path(exe);
    let static_bak = backup_path(&static_dir);
    if static_bak.exists() {
        let failed_static = static_dir.with_file_name(format!(
            "{}.failed",
            file_name_of(&static_dir).unwrap_or("static")
        ));
        remove_dir(&failed_static);
        if static_dir.exists() {
            let _ = std::fs::rename(&static_dir, &failed_static);
        }
        if let Err(e) = std::fs::rename(&static_bak, &static_dir) {
            tracing::error!("restore static dir failed: {e}");
            return Err(format!("程序已回滚，但 static 目录恢复失败: {e}"));
        }
        remove_dir(&failed_static);
    }
    Ok(())
}

fn backup_path(path: &Path) -> PathBuf {
    let name = file_name_of(path).unwrap_or("aigx");
    path.with_file_name(format!("{name}.bak"))
}

fn file_name_of(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn remove_dir(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(|e| format!("读取程序权限失败: {e}"))?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).map_err(|e| format!("设置程序权限失败: {e}"))
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn copy_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|e| format!("创建目录失败: {e}"))?;
    let entries = std::fs::read_dir(source).map_err(|e| format!("读取目录失败: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取目录项失败: {e}"))?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &destination)?;
        } else {
            std::fs::copy(&path, &destination).map_err(|e| format!("复制文件失败: {e}"))?;
        }
    }
    Ok(())
}

// ─── 发布包解包（自定义格式，见模块头）──────────────────────────

fn safe_file_name(name: &str) -> String {
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    let cleaned: String = base
        .chars()
        .map(|ch| {
            if matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                ch
            }
        })
        .collect();
    if cleaned.is_empty() {
        "aigx-update.bundle".to_string()
    } else {
        cleaned
    }
}

/// 只允许 `aigx` 与 `static/` 下的相对路径（拒绝绝对路径、`..` 与反斜杠）。
fn safe_entry_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains("..") {
        return None;
    }
    if path == "aigx" {
        return Some(PathBuf::from("aigx"));
    }
    let rest = path.strip_prefix("static/")?;
    if rest.is_empty() {
        return None;
    }
    Some(PathBuf::from("static").join(rest))
}

async fn extract_bundle(archive: &Path, staging: &Path) -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(archive)
        .await
        .map_err(|e| format!("打开更新包失败: {e}"))?;
    let mut magic = [0u8; 7];
    file.read_exact(&mut magic)
        .await
        .map_err(|_| "更新包格式错误（缺少文件头）".to_string())?;
    if &magic != BUNDLE_MAGIC {
        return Err("更新包格式不受支持".to_string());
    }
    let mut total: u64 = 0;
    let mut entries: usize = 0;
    loop {
        let mut length_buf = [0u8; 4];
        match file.read_exact(&mut length_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(format!("读取更新包失败: {e}")),
        }
        entries += 1;
        if entries > 10_000 {
            return Err("更新包条目过多".to_string());
        }
        let path_len = u32::from_le_bytes(length_buf) as usize;
        if path_len == 0 || path_len > 4096 {
            return Err("更新包路径长度非法".to_string());
        }
        let mut path_buf = vec![0u8; path_len];
        file.read_exact(&mut path_buf)
            .await
            .map_err(|_| "更新包路径不完整".to_string())?;
        let path = String::from_utf8(path_buf).map_err(|_| "更新包路径编码非法".to_string())?;
        let relative =
            safe_entry_path(&path).ok_or_else(|| format!("更新包包含非法路径: {path}"))?;
        let mut size_buf = [0u8; 8];
        file.read_exact(&mut size_buf)
            .await
            .map_err(|_| "更新包文件长度不完整".to_string())?;
        let size = u64::from_le_bytes(size_buf);
        if size > MAX_BUNDLE_BYTES {
            return Err("更新包单文件过大".to_string());
        }
        total = total.saturating_add(size);
        if total > MAX_BUNDLE_BYTES {
            return Err("更新包解压后体积过大".to_string());
        }
        let target = staging.join(&relative);
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("创建解包目录失败: {e}"))?;
        }
        let mut output = tokio::fs::File::create(&target)
            .await
            .map_err(|e| format!("创建解包文件失败: {e}"))?;
        let mut remaining = size;
        while remaining > 0 {
            let want = remaining.min(1024 * 1024) as usize;
            let mut buffer = vec![0u8; want];
            file.read_exact(&mut buffer)
                .await
                .map_err(|_| "更新包内容不完整".to_string())?;
            tokio::io::AsyncWriteExt::write_all(&mut output, &buffer)
                .await
                .map_err(|e| format!("写入解包文件失败: {e}"))?;
            remaining -= want as u64;
        }
        tokio::io::AsyncWriteExt::flush(&mut output)
            .await
            .map_err(|e| format!("写入解包文件失败: {e}"))?;
    }
    if entries == 0 {
        return Err("更新包为空".to_string());
    }
    Ok(())
}

// ─── Release 归一化与资产挑选 ───────────────────────────────────

fn pick_asset(assets: Option<&Value>, wanted: &str) -> Option<UpdateAsset> {
    let list = assets?.as_array()?;
    for item in list {
        if item.get("name").and_then(Value::as_str) != Some(wanted) {
            continue;
        }
        let url = item
            .get("browser_download_url")
            .and_then(Value::as_str)
            .unwrap_or("");
        if url.is_empty() {
            return None;
        }
        return Some(UpdateAsset {
            name: wanted.to_string(),
            size: item.get("size").and_then(Value::as_u64).unwrap_or(0),
            url: url.to_string(),
        });
    }
    None
}

fn release_info(payload: &Value, repo: &str) -> ReleaseInfo {
    let text = |key: &str| -> String {
        payload
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let raw_tag = text("tag_name");
    let tag = raw_tag
        .strip_prefix('v')
        .or_else(|| raw_tag.strip_prefix('V'))
        .unwrap_or(&raw_tag)
        .to_string();
    let name = {
        let name = text("name");
        if name.is_empty() {
            raw_tag.clone()
        } else {
            name
        }
    };
    let page_url = {
        let url = text("html_url");
        if url.is_empty() {
            format!("https://github.com/{repo}/releases")
        } else {
            url
        }
    };
    let published_at = {
        let published = text("published_at");
        if published.is_empty() {
            text("created_at")
        } else {
            published
        }
    };
    let bundle_name = host_arch().and_then(|arch| server_bundle_name(&tag, arch));
    let bundle = bundle_name
        .as_deref()
        .and_then(|name| pick_asset(payload.get("assets"), name));
    let checksum = bundle_name
        .as_deref()
        .and_then(|name| pick_asset(payload.get("assets"), &format!("{name}.sha256")));
    ReleaseInfo {
        tag,
        name,
        notes: text("body").chars().take(4000).collect(),
        published_at,
        page_url,
        prerelease: payload
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        bundle,
        checksum,
    }
}

/// 下载地址白名单：默认只允许 GitHub 资产域名；配置了 `api_base` 时
/// 额外放行该源自身的主机（显式配置才生效）。
fn assert_downloadable(url: &str, config: &UpdateConfig) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|_| "下载地址不是合法 URL".to_string())?;
    let host = parsed.host_str().unwrap_or("").to_lowercase();
    if let Ok(base) = url::Url::parse(&api_root(config)) {
        let base_host = base.host_str().unwrap_or("").to_lowercase();
        let same_host = !base_host.is_empty()
            && (host == base_host || host.ends_with(&format!(".{base_host}")));
        let insecure = base.scheme() == "https" && parsed.scheme() != "https";
        if same_host && insecure {
            return Err("下载地址必须是 https".to_string());
        }
        if same_host {
            return Ok(());
        }
    }
    if parsed.scheme() != "https" {
        return Err("下载地址必须是 https".to_string());
    }
    let allowed = matches!(
        host.as_str(),
        "github.com" | "www.github.com" | "objects.githubusercontent.com" | "codeload.github.com"
    ) || host.ends_with(".githubusercontent.com");
    if !allowed {
        return Err(format!("下载地址域名不在允许列表内: {host}"));
    }
    Ok(())
}

/// 解析 `.sha256` 文件：接受 `sha256sum` 的 `<hex>  <name>` 或裸 hex。
fn parse_checksum(text: &str) -> Result<String, String> {
    let token = text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("校验文件格式错误（需要 64 位 sha256）".to_string());
    }
    Ok(token)
}

/// 从响应头算最早可再次请求的时刻（秒；0 = 上游没给限制）。
fn retry_deadline(headers: &reqwest::header::HeaderMap, status: u16) -> i64 {
    let now = now_ts();
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let retry = header("retry-after")
        .and_then(|value| {
            value
                .parse::<i64>()
                .ok()
                .map(|seconds| now.saturating_add(seconds.max(0)))
                .or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value)
                        .ok()
                        .map(|date| date.timestamp())
                })
        })
        .unwrap_or(0);
    let reset = if header("x-ratelimit-remaining") == Some("0") {
        header("x-ratelimit-reset")
            .and_then(|value| value.parse::<i64>().ok())
            .map(|seconds| seconds.saturating_add(1))
            .unwrap_or(0)
    } else {
        0
    };
    let minimum = if status == 403 || status == 429 {
        now + 60
    } else {
        0
    };
    retry.max(reset).max(minimum)
}

// ─── 版本比较（semver 核心三段 + 预发布段）──────────────────────

fn split_version(text: &str) -> Option<([u64; 3], Option<&str>)> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    let mut parts = [0u64; 3];
    let mut rest = body;
    for (index, slot) in parts.iter_mut().enumerate() {
        let digits_end = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits_end == 0 {
            return None;
        }
        *slot = rest[..digits_end].parse().ok()?;
        rest = &rest[digits_end..];
        if index < 2 {
            rest = rest.strip_prefix('.')?;
        }
    }
    let head = rest.split('+').next().unwrap_or(rest);
    let prerelease = head.strip_prefix('-').filter(|tail| !tail.is_empty());
    Some((parts, prerelease))
}

fn is_prerelease_version(text: &str) -> bool {
    split_version(text)
        .map(|(_, prerelease)| prerelease.is_some())
        .unwrap_or(false)
}

fn compare_prerelease(left: &str, right: &str) -> i32 {
    let mut left_parts = left.split('.');
    let mut right_parts = right.split('.');
    loop {
        match (left_parts.next(), right_parts.next()) {
            (None, None) => return 0,
            (None, Some(_)) => return -1,
            (Some(_), None) => return 1,
            (Some(a), Some(b)) => {
                let order = match (a.parse::<u64>(), b.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.cmp(b),
                };
                if order != std::cmp::Ordering::Equal {
                    if order == std::cmp::Ordering::Less {
                        return -1;
                    }
                    return 1;
                }
            }
        }
    }
}

/// a > b 返回 1，相等 0，a < b 返回 -1；无法解析返回 None。
fn compare_versions(a: &str, b: &str) -> Option<i32> {
    let (left, left_pre) = split_version(a)?;
    let (right, right_pre) = split_version(b)?;
    for (left_part, right_part) in left.iter().zip(right.iter()) {
        if left_part > right_part {
            return Some(1);
        }
        if left_part < right_part {
            return Some(-1);
        }
    }
    Some(match (left_pre, right_pre) {
        (None, None) => 0,
        (None, Some(_)) => 1,
        (Some(_), None) => -1,
        (Some(left), Some(right)) => compare_prerelease(left, right),
    })
}

/// 从 Release 列表里挑版本最高的一版（预发布通道用）。
fn pick_listed_release(payload: &Value, only_prerelease: bool) -> Option<&Value> {
    let list = payload.as_array()?;
    let mut best: Option<&Value> = None;
    let mut first: Option<&Value> = None;
    for item in list {
        if item.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if first.is_none() {
            first = Some(item);
        }
        let tag = item.get("tag_name").and_then(Value::as_str).unwrap_or("");
        if split_version(tag).is_none() {
            continue;
        }
        if only_prerelease && !is_prerelease_version(tag) {
            continue;
        }
        let take = match best {
            None => true,
            Some(current) => {
                let current_tag = current
                    .get("tag_name")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                compare_versions(tag, current_tag) == Some(1)
            }
        };
        if take {
            best = Some(item);
        }
    }
    if only_prerelease {
        best
    } else {
        best.or(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare_handles_prerelease() {
        assert_eq!(compare_versions("v1.2.3", "1.2.2"), Some(1));
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Some(0));
        assert_eq!(compare_versions("1.2.3-beta.1", "1.2.3"), Some(-1));
        assert_eq!(compare_versions("1.2.3-beta.2", "1.2.3-beta.1"), Some(1));
        assert_eq!(compare_versions("not-a-version", "1.2.3"), None);
    }

    #[test]
    fn checksum_parser_accepts_sha256sum_output() {
        let hash = "a".repeat(64);
        assert_eq!(parse_checksum(&hash), Ok(hash.clone()));
        assert_eq!(
            parse_checksum(&format!("{hash}  AIGX-1.2.3-server-linux-amd64.upd")),
            Ok(hash)
        );
        assert!(parse_checksum("deadbeef").is_err());
    }

    #[test]
    fn bundle_entry_paths_are_restricted() {
        assert_eq!(safe_entry_path("aigx"), Some(PathBuf::from("aigx")));
        assert!(safe_entry_path("static/index.html").is_some());
        assert!(safe_entry_path("../evil").is_none());
        assert!(safe_entry_path("/etc/passwd").is_none());
        assert!(safe_entry_path("aigx\\..\\evil").is_none());
        assert!(safe_entry_path("README.md").is_none());
    }

    #[test]
    fn server_bundle_name_matches_release_convention() {
        if cfg!(target_os = "linux") {
            assert_eq!(
                server_bundle_name("1.2.3", "amd64").as_deref(),
                Some("AIGX-1.2.3-server-linux-amd64.upd")
            );
            assert!(server_bundle_name("", "amd64").is_none());
        }
    }
}
