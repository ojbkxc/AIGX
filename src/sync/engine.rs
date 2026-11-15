//! 同步引擎：上传（含跳过 / 间隔 / 退避重试）、拉取（合并恢复）、测试连接、状态。
//!
//! ```text
//! upload   本机快照 → 内容哈希（没变就跳过）→ 传输 → 状态 / 逐键版本表
//! pull     远端 manifest → 快照（校验）→ 安全备份 → 合并 → 写回 → 重载缓存
//! test     当前（未保存的）表单拼一份临时设置，只做连通性探测
//! status   状态 KV + 远端路径
//! ```
//!
//! ── 上传的三种「不发数据」情形 ───────────────────────────────
//!   1. **内容没变**：本地哈希与 `status.last_hash` 相同 → 跳过（零请求）；
//!   2. **远端已经是这份内容**：本地哈希变了（例如刚拉取完另一台机器的数据）
//!      但远端 manifest 的 `content_hash` 与本地一致 → 只更新状态，不重传；
//!   3. **最小同步间隔未到**：与上次成功上传相距不足 `min_interval_secs` →
//!      跳过并说明原因（保护远端与线路，避免用户连点或调度抖动打爆配额）。
//!
//! ── 失败退避 ────────────────────────────────────────────────
//! 单次操作内做 [`MAX_UPLOAD_ATTEMPTS`] 次重试（指数退避 1s / 2s / 4s）；
//! 全部失败后把 `consecutive_failures + 1` 与 `retry_at`（间隔 × 2^n，上限
//! 一天）落进状态 KV——调度循环据此推迟下一轮，且**跨重启保留**。
//!
//! ── 拉取后的缓存重载 ────────────────────────────────────────
//! 合并直接改写 KV，而多数 store 是「启动时读一次、之后走内存」的形态。写回后
//! 必须让这些内存态重读，否则本次运行仍按旧数据服务（渠道 / 用户 / 凭据 / 定价
//! 全是这类）。[`reload_cached_stores`] 逐一重载；单项失败只告警不阻断——已落下
//! 的数据不会因为某个 store 重载失败而回滚，重启后自然一致。

use serde_json::{json, Value};

use crate::api::openai::AppState;

use super::config::{self, SyncConfig, SyncStatus};
use super::snapshot::{self, MergeMode, SyncManifest};
use super::transport::Transport;
use super::{now_ms, sync_lock};

/// 单次上传的最大尝试次数（含首次）。
const MAX_UPLOAD_ATTEMPTS: u32 = 3;
/// 失败退避的上限（一天）。
const MAX_BACKOFF_MS: i64 = 24 * 60 * 60_000;

fn store(state: &AppState) -> &std::sync::Arc<crate::storage::FileStore> {
    &state.scheduled_task_store
}

/// 同步操作的错误：`config=true` 表示用户要改设置（API 映射 400），
/// 否则是远端 / 运行时问题（API 映射 502）。
#[derive(Debug, Clone)]
pub struct SyncError {
    pub message: String,
    pub config: bool,
}

impl SyncError {
    /// 配置类错误（未启用 / 必填项缺失 / 地址非法 / 凭据解不开）。
    pub fn config(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            config: true,
        }
    }

    /// 远端 / 运行时错误。
    pub fn remote(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            config: false,
        }
    }
}

impl From<String> for SyncError {
    fn from(message: String) -> Self {
        Self::remote(message)
    }
}

/// 状态 JSON（GET /api/admin/sync/status）。
pub fn status_json(state: &AppState) -> Value {
    let config = config::load(store(state));
    let status = config::load_status(store(state));
    json!({
        "enabled": config.enabled,
        "backend": config.backend,
        "remote_path": config.remote_display(),
        "interval_secs": config.interval_secs,
        "min_interval_secs": config.min_interval_secs,
        "last_attempt_at": status.last_attempt_at,
        "last_success_at": status.last_success_at,
        "last_action": status.last_action,
        "last_hash": status.last_hash,
        "last_error": status.last_error,
        "consecutive_failures": status.consecutive_failures,
        "retry_at": status.retry_at,
        "last_bytes": status.last_bytes,
        "next_run_at": next_run_at(&config, &status),
    })
}

/// 调度视角的下次执行时刻（毫秒；0 = 不会自动执行）。
fn next_run_at(config: &SyncConfig, status: &SyncStatus) -> i64 {
    if !config.enabled {
        return 0;
    }
    let sched_state =
        crate::scheduled_tasks::task_state::read(super::scheduler::TASK_KEY).unwrap_or_default();
    let due = sched_state.due_at();
    // 排期条目缺失 = 还没跑过（调度循环下一拍会 `reschedule`），按「现在」给
    if due <= 0 {
        now_ms().max(status.retry_at)
    } else {
        due.max(status.retry_at)
    }
}

// ─── 测试连接 ─────────────────────────────────────────────────

/// 用**这批输入**拼一份临时设置做连通性探测（不落库）。
///
/// 用户改了密码但还没保存时，测的应该是新密码：密码 / Secret 非空则重新加密，
/// 空则沿用库里那份（与保存语义一致）。
pub async fn test_connection(
    state: &AppState,
    input: config::SaveInput,
) -> Result<Value, SyncError> {
    let config = config::load(store(state));
    let config = config::apply_input(config, input).map_err(SyncError::config)?;
    probe(&config).await
}

/// 对一份已拼好的设置做连通性探测（不落库）。
pub async fn probe(config: &SyncConfig) -> Result<Value, SyncError> {
    let transport = Transport::from_config(config).map_err(SyncError::config)?;
    transport.test_connection().await?;
    Ok(json!({
        "success": true,
        "backend": config.backend,
        "remote_path": transport.display_path(),
    }))
}

// ─── 上传 ─────────────────────────────────────────────────────

/// 手动 / 调度触发的上传。
///
/// `manual=false`（调度循环）与 `manual=true`（「立即同步」按钮）走同一条路径：
/// 跳过判断与最小间隔对两者一视同仁——按钮不该绕过「内容没变」的结论，但
/// 内容确实变了而间隔未到时，返回一条明确的 `skipped` 说明。`force=true`
/// （请求体显式指定）才越过「内容未变化 / 最小间隔」两道闸门。
pub async fn upload(state: &AppState, manual: bool, force: bool) -> Result<Value, SyncError> {
    let _guard = sync_lock().lock().await;
    let config = config::require_enabled(store(state)).map_err(SyncError::config)?;

    let now = now_ms();
    let status = config::load_status(store(state));
    let versions = config::load_versions(store(state));
    let (local_snapshot, next_versions) = snapshot::build(store(state), &versions, now)?;
    let local_hash = local_snapshot.content_hash();

    // 1) 内容没变：零请求跳过
    if !force && !status.last_hash.is_empty() && status.last_hash == local_hash {
        let mut status = status.clone();
        status.last_attempt_at = now;
        status.retry_at = 0;
        config::save_status(store(state), &status)?;
        return Ok(json!({
            "status": "skipped",
            "reason": "内容未变化，跳过上传",
            "content_hash": local_hash,
            "manual": manual,
        }));
    }

    // 2) 最小同步间隔（保护远端：两次成功上传之间的硬下限）
    let min_gap_ms = config.min_interval_secs.saturating_mul(1000);
    if !force
        && min_gap_ms > 0
        && status.last_success_at > 0
        && now - status.last_success_at < min_gap_ms
    {
        let remaining = (status.last_success_at + min_gap_ms - now) / 1000;
        return Ok(json!({
            "status": "skipped",
            "reason": format!("距上次成功同步不足 {} 秒（还需等待 {remaining} 秒）", config.min_interval_secs),
            "content_hash": local_hash,
            "manual": manual,
        }));
    }

    let transport = Transport::from_config(&config).map_err(SyncError::config)?;

    // 3) 远端已是这份内容（例如刚拉取完）：只更新状态，不重传
    if let Ok(Some((_, remote_manifest))) = transport.get_manifest().await {
        if remote_manifest.content_hash == local_hash {
            let mut status = status.clone();
            status.last_attempt_at = now;
            status.last_success_at = now;
            status.last_action = "upload".to_string();
            status.last_hash = local_hash.clone();
            status.last_error.clear();
            status.consecutive_failures = 0;
            status.retry_at = 0;
            config::save_status(store(state), &status)?;
            config::save_versions(store(state), &next_versions)?;
            return Ok(json!({
                "status": "skipped",
                "reason": "远端内容与本机一致，跳过上传",
                "content_hash": local_hash,
                "remote_path": transport.display_path(),
                "manual": manual,
            }));
        }
    }

    let snapshot_bytes = serde_json::to_vec(&local_snapshot)
        .map_err(|error| format!("序列化同步快照失败: {error}"))?;
    if snapshot_bytes.len() > snapshot::MAX_SNAPSHOT_BYTES {
        return Err(SyncError::remote(format!(
            "本机数据超过同步上限（{} 字节 > {} 字节），请先清理数据",
            snapshot_bytes.len(),
            snapshot::MAX_SNAPSHOT_BYTES
        )));
    }
    let manifest = SyncManifest::build(&local_snapshot, &snapshot_bytes);
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("序列化 manifest 失败: {error}"))?;

    // 4) 传输：数据先落、manifest 最后（断网时远端仍指向上一份完整快照）
    let mut last_error = String::new();
    for attempt in 1..=MAX_UPLOAD_ATTEMPTS {
        let result = async {
            transport.test_connection().await?;
            transport
                .put(
                    snapshot::REMOTE_SNAPSHOT,
                    snapshot_bytes.clone(),
                    "application/json",
                )
                .await?;
            transport
                .put(
                    snapshot::REMOTE_MANIFEST,
                    manifest_bytes.clone(),
                    "application/json",
                )
                .await
        }
        .await;
        match result {
            Ok(()) => {
                let mut status = status.clone();
                status.last_attempt_at = now;
                status.last_success_at = now_ms();
                status.last_action = "upload".to_string();
                status.last_hash = local_hash.clone();
                status.last_error.clear();
                status.consecutive_failures = 0;
                status.retry_at = 0;
                status.last_bytes = snapshot_bytes.len() as u64;
                config::save_status(store(state), &status)?;
                config::save_versions(store(state), &next_versions)?;
                tracing::info!(
                    "[CloudSync] 上传完成：{}（{} 键，{} 字节）",
                    transport.display_path(),
                    local_snapshot.entries.len(),
                    snapshot_bytes.len()
                );
                return Ok(json!({
                    "status": "uploaded",
                    "remote_path": transport.display_path(),
                    "bytes": snapshot_bytes.len(),
                    "entries": local_snapshot.entries.len(),
                    "content_hash": local_hash,
                    "attempts": attempt,
                    "manual": manual,
                }));
            }
            Err(error) => {
                last_error = error;
                if attempt < MAX_UPLOAD_ATTEMPTS {
                    let backoff = 500u64 << (attempt - 1);
                    tracing::warn!(
                        "[CloudSync] 上传第 {attempt} 次尝试失败（{backoff}ms 后重试）：{last_error}"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                }
            }
        }
    }

    record_failure(state, &config, &status, &last_error)?;
    Err(SyncError::remote(last_error))
}

/// 记一次失败：状态 KV + 指数退避 + 日志。
fn record_failure(
    state: &AppState,
    config: &SyncConfig,
    previous: &SyncStatus,
    error: &str,
) -> Result<(), String> {
    let failures = previous.consecutive_failures.saturating_add(1);
    let multiplier = 1_i64 << failures.saturating_sub(1).min(10);
    let base = config.interval_secs.max(60).saturating_mul(1000);
    let retry_at = now_ms().saturating_add(base.saturating_mul(multiplier).min(MAX_BACKOFF_MS));
    let mut status = previous.clone();
    status.last_attempt_at = now_ms();
    status.last_error = error.to_string();
    status.consecutive_failures = failures;
    status.retry_at = retry_at;
    config::save_status(store(state), &status)?;
    // 同步失败通常可重试（网络抖动、密码填错）：warn 级，不点亮错误徽标
    tracing::warn!("[CloudSync] 同步失败（连续 {failures} 次）：{error}");
    Ok(())
}

// ─── 拉取 ─────────────────────────────────────────────────────

/// 从远端拉取快照并合并到本机。
///
/// 顺序：读 manifest → 校验 → 读快照 → 校验 → **本机安全备份** → 合并 →
/// 写回 → 重载内存缓存 → 状态 / 版本表。`mode` 决定冲突条目的取舍：
/// `merge`（默认，updated_at 新者胜）/ `local`（本机强制覆盖）/ `remote`
/// （远端强制覆盖）。
pub async fn pull(state: &AppState, mode: MergeMode) -> Result<Value, SyncError> {
    let _guard = sync_lock().lock().await;
    let config = config::require_enabled(store(state)).map_err(SyncError::config)?;
    let transport = Transport::from_config(&config).map_err(SyncError::config)?;

    let Some((_, manifest)) = transport.get_manifest().await? else {
        return Err(SyncError::remote("远端还没有可拉取的同步数据"));
    };
    let remote_snapshot = transport.get_snapshot(&manifest).await?;

    let now = now_ms();
    let previous_status = config::load_status(store(state));
    let versions = config::load_versions(store(state));
    let (local_snapshot, _) = snapshot::build(store(state), &versions, now)?;
    let outcome = snapshot::merge(&local_snapshot, &remote_snapshot, mode);

    // 应用前安全备份（破坏性操作前留影；与整库导入同一纪律）
    let data_dir = crate::config::expand_path(&state.config_manager.get().await.server.data_dir);
    let safety = snapshot::safety_backup(&local_snapshot, &data_dir)
        .map_err(|error| format!("生成安全备份失败（未应用远端数据）: {error}"))?;

    let written = snapshot::apply(store(state), &outcome.writes)?;
    reload_cached_stores(state);

    // 版本表：合并视图即恢复后本机应有内容；新写入的条目沿用远端 updated_at
    let mut next_versions = std::collections::BTreeMap::new();
    for (key, entry) in &outcome.merged {
        next_versions.insert(key.clone(), entry.to_version());
    }
    config::save_versions(store(state), &next_versions)?;

    let merged_hash = snapshot::hash_entries(&outcome.merged);
    let mut status = previous_status;
    status.last_attempt_at = now_ms();
    status.last_success_at = now_ms();
    status.last_action = "pull".to_string();
    status.last_hash = merged_hash.clone();
    status.last_error.clear();
    status.consecutive_failures = 0;
    status.retry_at = 0;
    config::save_status(store(state), &status)?;

    tracing::info!(
        "[CloudSync] 拉取完成：来自 {}（{}），模式 {}，写入 {} 键，安全备份 {}",
        if manifest.device.is_empty() {
            "未知设备"
        } else {
            &manifest.device
        },
        if manifest.created_at > 0 {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(manifest.created_at)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default()
        } else {
            String::new()
        },
        mode.as_str(),
        written,
        safety
    );

    Ok(json!({
        "status": "pulled",
        "mode": mode.as_str(),
        "remote_path": transport.display_path(),
        "device": manifest.device,
        "created_at": manifest.created_at,
        "written": written,
        "merged": outcome.stats.to_json(),
        "content_hash": merged_hash,
        "safety_backup": safety,
    }))
}

/// 拉取后重载各内存 store（best-effort：单项失败只告警）。
fn reload_cached_stores(state: &AppState) {
    macro_rules! reload {
        ($label:expr, $result:expr) => {
            if let Err(error) = $result {
                tracing::warn!(
                    "[CloudSync] 拉取后重载{}失败（重启后自然一致）：{error}",
                    $label
                );
            }
        };
    }

    reload!("渠道", state.channel_store.load());
    reload!("API Key", state.api_key_store.load());
    reload!("用户", state.user_store.load());
    reload!("定价", state.pricing_store.load());
    reload!("兑换码", state.redemption_store.load());
    reload!("套餐", state.plan_store.load());
    reload!("订阅", state.subscription_store.load());
    reload!("用户分组", state.user_group_store.load());
    reload!("IP 名单", state.ip_filter.load());
    reload!("模型规则", state.model_rules.load());
    reload!("模型元信息", state.model_metadata.load());
    reload!("账号池", state.account_sessions.load());
    reload!("上游签到", state.upstream_checkin.load());
    reload!("工单", state.ticket_store.load());
    reload!("订单", state.order_store.load());
    // 配置型单例（内存里各有一份快照）：从 KV 读回后经 update() 发布
    let kv = store(state).clone();
    if let Ok(Some(cfg)) = kv.get::<crate::prompt::GatewayPrompt>(crate::prompt::GATEWAY_PROMPT_KEY)
    {
        reload!("系统提示词", state.gateway_prompt.update(cfg));
    }
    if let Ok(Some(cfg)) = kv.get::<crate::purge::PurgeSettings>(crate::purge::PURGE_SETTINGS_KEY) {
        reload!("破甲设置", state.purge.update(cfg));
    }
    if let Ok(Some(cfg)) = kv.get::<crate::channel::routing_policy::RoutingPolicy>(
        crate::channel::routing_policy::ROUTING_POLICY_KEY,
    ) {
        reload!("路由策略", state.routing_policy.update(cfg));
    }
    if let Ok(Some(cfg)) = kv.get::<crate::agent::user_tool_policy::UserToolPolicy>(
        crate::agent::user_tool_policy::USER_TOOL_POLICY_KEY,
    ) {
        reload!("工具策略", state.user_tool_policy.update(cfg));
    }

    // 同步自己的配置 / 状态 / 版本表不在快照里（is_synced_key 排除），
    // 但远端可能混入（手改 / 旧实现）：apply 侧已忽略，这里无需特殊处理。
    tracing::info!("[CloudSync] 拉取后已重载内存缓存（渠道/用户/凭据/定价等）");
}
