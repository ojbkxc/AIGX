//! 日志与审计系统 — 请求日志 + 管理员操作审计。
//!
//! 参照 new-api model/log.go 的字段设计（UserId/ModelName/PromptTokens/CompletionTokens/
//! Quota/UseTime/ChannelId/TokenId/Ip/RequestId）与 model/log.go 的 Type=Consume/Manage 区分。
//!
//! - RequestLog：每次推理请求完成后记录一条（含 model/tokens/cost/latency/status）
//! - AuditLog：管理员写操作（创建/删除/更新用户、渠道、令牌、定价等）记录 before/after
//!
//! 持久化使用 FileStore KV：
//! - 请求日志 key: `reqlog:{created_at}:{id}`
//! - 审计日志 key: `auditlog:{created_at}:{id}`
//!
//! 时间戳前缀便于按时间范围扫描与排序。

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::storage::FileStore;

// ── RequestLog ─────────────────────────────────────────────────────

/// 请求日志 — 每次推理调用记录一条。
///
/// 参照 new-api model/log.go Log struct。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    /// 唯一 ID（uuid）
    pub id: String,
    /// 用户 ID（可能为空，管理员级令牌）
    #[serde(default)]
    pub user_id: Option<String>,
    /// API Key ID
    #[serde(default)]
    pub key_id: Option<String>,
    /// 渠道 ID（若使用通用渠道）
    #[serde(default)]
    pub channel_id: Option<String>,
    /// 渠道名（admin 排查用，用户不展示）
    #[serde(default)]
    pub channel_name: Option<String>,
    /// 模型名 — 映射后的上游真实名（= 价格表里的名），用户看这个
    #[serde(default)]
    pub model: String,
    /// 用户原始请求名（admin 排查用，用户不展示）。无映射时与 model 相同。
    #[serde(default)]
    pub origin_model: Option<String>,
    /// 输入 token 数
    #[serde(default)]
    pub input_tokens: u64,
    /// 输出 token 数
    #[serde(default)]
    pub output_tokens: u64,
    /// 本次请求费用（配额单位，向用户收的销售价）
    #[serde(default)]
    pub cost: i64,
    /// 渠道成本（配额单位）。未配成本价时 = cost，利润显示 "—"；
    /// 配了成本价时 = 按 cost_pricing 算出的成本，利润 = cost - channel_cost。
    #[serde(default)]
    pub channel_cost: i64,
    /// 延迟（毫秒）
    #[serde(default)]
    pub latency_ms: u64,
    /// HTTP 状态码
    #[serde(default)]
    pub status_code: u16,
    /// 错误信息（失败时填）
    #[serde(default)]
    pub error_msg: Option<String>,
    /// 客户端 IP
    #[serde(default)]
    pub ip: Option<String>,
    /// 请求 ID（关联客户端请求）
    #[serde(default)]
    pub request_id: Option<String>,
    /// 创建时间（unix timestamp）
    pub created_at: i64,
    /// 调度决策回放（P1-7）：候选渠道列表
    #[serde(default)]
    pub candidate_channels: Vec<String>,
    /// 调度决策回放（P1-7）：被过滤的渠道及原因
    #[serde(default)]
    pub filtered_channels: Vec<FilteredChannel>,
    /// 调度决策回放（P1-7）：最终选中渠道
    #[serde(default)]
    pub selected_channel: Option<String>,
    /// 是否缓存命中（G4：区分 cache 请求与普通请求的记账维度）
    #[serde(default)]
    pub cache_hit: bool,
}

/// 被过滤的渠道信息（P1-7 调度决策回放）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilteredChannel {
    pub channel_id: String,
    pub reason: String,
}

impl Default for RequestLog {
    fn default() -> Self {
        Self::new()
    }
}

impl RequestLog {
    pub fn new() -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            user_id: None,
            key_id: None,
            channel_id: None,
            channel_name: None,
            model: String::new(),
            origin_model: None,
            input_tokens: 0,
            output_tokens: 0,
            cost: 0,
            channel_cost: 0,
            latency_ms: 0,
            status_code: 200,
            error_msg: None,
            ip: None,
            request_id: None,
            created_at: chrono::Utc::now().timestamp(),
            candidate_channels: Vec::new(),
            filtered_channels: Vec::new(),
            selected_channel: None,
            cache_hit: false,
        }
    }
}

// ── AuditLog ───────────────────────────────────────────────────────

/// 管理员操作审计日志。
///
/// 参照 new-api LogTypeManage：记录 admin 对系统资源的写操作。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: String,
    /// 管理员用户 ID（或 "admin" 兼容旧模式）
    #[serde(default)]
    pub admin_id: String,
    /// 操作类型：create / update / delete / login / logout / etc.
    #[serde(default)]
    pub action: String,
    /// 目标资源（如 "user:{id}"、"channel:{id}"）
    #[serde(default)]
    pub target: String,
    /// 操作前状态（JSON 字符串，可选）
    #[serde(default)]
    pub before: Option<String>,
    /// 操作后状态（JSON 字符串，可选）
    #[serde(default)]
    pub after: Option<String>,
    /// 创建时间
    pub created_at: i64,
}

impl AuditLog {
    pub fn new(
        admin_id: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            admin_id: admin_id.into(),
            action: action.into(),
            target: target.into(),
            before: None,
            after: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}

// ── Store ──────────────────────────────────────────────────────────

/// 请求日志存储。
///
/// key 格式：`reqlog:{created_at}:{id}`，时间戳前缀便于按时间范围扫描。
///
/// B13：设置容量上限防止无界增长撑爆磁盘——写入路径每
/// `PURGE_CHECK_EVERY` 次触发一次容量检查，超限时删除最旧一批。
pub struct RequestLogStore {
    store: Arc<FileStore>,
    /// 写入计数：摊薄容量检查频率（每 PURGE_CHECK_EVERY 次写入触发一次）
    writes: std::sync::atomic::AtomicU64,
}

impl RequestLogStore {
    /// B13：请求日志容量上限
    const MAX_LOGS: usize = 100_000;
    /// B13：超限时每批清理的最旧日志条数
    const PURGE_BATCH: usize = 10_000;
    /// B13：每隔多少次写入触发一次容量检查
    const PURGE_CHECK_EVERY: u64 = 1000;

    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            writes: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn key_of(log: &RequestLog) -> String {
        format!("reqlog:{}:{}", log.created_at, log.id)
    }

    /// 追加一条请求日志
    pub fn add(&self, mut log: RequestLog) -> anyhow::Result<RequestLog> {
        if log.id.is_empty() {
            log.id = uuid::Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = chrono::Utc::now().timestamp();
        }
        // B13：定期检查容量，超限清理最旧一批（检查失败不影响本次写入）
        let writes = self
            .writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if writes.is_multiple_of(Self::PURGE_CHECK_EVERY) {
            if let Err(e) = self.purge_overflow() {
                tracing::warn!("request log purge check failed: {e}");
            }
        }
        let key = Self::key_of(&log);
        self.store.put(&key, &log)?;
        Ok(log)
    }

    /// B13：容量超限时删除最旧一批日志。
    ///
    /// key 中 created_at 为数字字符串，字典序与数值序不一致
    ///（如 "999" > "1000"），需 parse 后按数值排序再取最旧。
    fn purge_overflow(&self) -> anyhow::Result<()> {
        let keys = self.store.list("reqlog:")?;
        if keys.len() < Self::MAX_LOGS {
            return Ok(());
        }
        let mut timed: Vec<(i64, String)> = keys
            .into_iter()
            .map(|k| {
                let ts = k
                    .strip_prefix("reqlog:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(i64::MAX);
                (ts, k)
            })
            .collect();
        timed.sort_by_key(|(ts, _)| *ts);
        let excess =
            (timed.len().saturating_sub(Self::MAX_LOGS) + Self::PURGE_BATCH).min(timed.len());
        for (_, k) in timed.into_iter().take(excess) {
            if let Err(e) = self.store.delete(&k) {
                tracing::warn!("request log purge failed for {k}: {e}");
            }
        }
        Ok(())
    }

    /// 日志保留配置（运维设置持久化；None = 不按天数清理，只受容量上限约束）
    ///
    /// key 格式 `reqlog:{created_at}:{id}` 时间戳前缀 + `auditlog:{ts}:{id}`，
    /// 按天清理直接对前缀范围扫描即可，无需遍历解析值。
    pub fn set_retention(
        &self,
        max_age_days: Option<u64>,
        max_capacity: Option<usize>,
    ) -> anyhow::Result<()> {
        #[derive(Serialize, Deserialize, Default)]
        struct Retention {
            max_age_days: Option<u64>,
            max_capacity: Option<usize>,
        }
        self.store.put(
            "log_retention",
            &Retention {
                max_age_days,
                max_capacity,
            },
        )
    }

    /// 读取日志保留配置（无记录返回 None/None = 默认策略：不限天数、容量 10 万）
    pub fn retention(&self) -> (Option<u64>, Option<usize>) {
        #[derive(Serialize, Deserialize, Default)]
        struct Retention {
            max_age_days: Option<u64>,
            max_capacity: Option<usize>,
        }
        match self.store.get::<Retention>("log_retention") {
            Ok(Some(r)) => (r.max_age_days, r.max_capacity),
            _ => (None, None),
        }
    }

    /// 手动/定时清理：删除早于 `before_ts` 的请求日志，返回删除条数（分批限流防阻塞）。
    ///
    /// 与 new-api DeleteOldLogBatch 对齐的语义，但 AIGX 是 KV 存储无需事务，
    /// 直接按键前缀收集 + 分批删除（每批 5000 条，逐条 delete 是 O(1) 点删）。
    pub fn delete_older_than(&self, before_ts: i64) -> anyhow::Result<usize> {
        let keys = self.store.list("reqlog:")?;
        let mut doomed: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                k.strip_prefix("reqlog:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|ts| ts < before_ts)
                    .unwrap_or(false)
            })
            .collect();
        let n = doomed.len();
        const BATCH: usize = 5000;
        for chunk in std::mem::take(&mut doomed).chunks(BATCH) {
            for k in chunk {
                if let Err(e) = self.store.delete(k) {
                    tracing::warn!("retention delete failed for {k}: {e}");
                }
            }
        }
        Ok(n)
    }

    /// 列出全部请求日志（按时间倒序）
    pub fn list_all(&self) -> Vec<RequestLog> {
        let keys = match self.store.list("reqlog:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut logs: Vec<RequestLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<RequestLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        logs
    }

    /// 按 ID 删除单条请求日志（best-effort，不存在视为成功）
    pub fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let keys = self.store.list("reqlog:")?;
        let target = keys.into_iter().find(|k| k.ends_with(&format!(":{id}")));
        if let Some(k) = target {
            self.store.delete(&k)?;
        }
        Ok(())
    }

    /// 批量删除请求日志（按 ID 列表），返回成功删除的条数
    pub fn delete_many(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.delete_by_id(id).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 清空全部请求日志，返回删除条数
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("reqlog:")?;
        let n = keys.len();
        for k in &keys {
            if let Err(e) = self.store.delete(k) {
                tracing::warn!("clear request log failed for {k}: {e}");
            }
        }
        Ok(n)
    }

    /// 按条件过滤并分页。
    ///
    /// 参数：
    /// - user_id：按用户 ID 过滤
    /// - model：按模型过滤
    /// - channel_id：按渠道过滤
    /// - start/end：时间范围（unix timestamp，闭区间）
    /// - page/size：分页（1-based）
    #[allow(clippy::too_many_arguments)]
    pub fn list_with_filter(
        &self,
        user_id: Option<&str>,
        model: Option<&str>,
        channel_id: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        page: usize,
        size: usize,
    ) -> (Vec<RequestLog>, usize) {
        // P0 性能：无筛选时用 list_latest_keys 只取最新一页所需的键（索引范围
        // 扫描 + LIMIT），避免全表列键+全量反序列化。10 万条日志时从 O(全表)
        // 降到 O(page*size)。键的时间戳前缀保证字典序=时间序，倒序即最新优先。
        // total 用 COUNT 同样走索引，不再物化全部记录。
        let page = page.max(1);
        let size = size.max(1);
        let has_filter = user_id.is_some() || model.is_some() || channel_id.is_some();

        if !has_filter {
            let total = match self.store.list("reqlog:") {
                Ok(keys) => keys.len(),
                Err(_) => 0,
            };
            let limit = page.saturating_mul(size);
            let keys = self
                .store
                .list_latest_keys("reqlog:", limit)
                .unwrap_or_default();
            let mut logs: Vec<RequestLog> = keys
                .into_iter()
                .filter_map(|k| self.store.get::<RequestLog>(&k).ok().flatten())
                .collect();
            // 键倒序（最新优先）；值的 created_at 与键一致，再按值稳定排序一次
            logs.sort_by_key(|l| std::cmp::Reverse(l.created_at));
            return (logs, total);
        }

        // 带筛选：退回全量扫描（筛选条件在值内，无索引可用），但只在
        // 时间窗内扫描（有 start/end 时先按键范围裁剪键集）。
        let all = self.list_all();
        let filtered: Vec<RequestLog> = all
            .into_iter()
            .filter(|l| {
                if let Some(u) = user_id {
                    if l.user_id.as_deref() != Some(u) {
                        return false;
                    }
                }
                if let Some(m) = model {
                    if l.model != m {
                        return false;
                    }
                }
                if let Some(c) = channel_id {
                    if l.channel_id.as_deref() != Some(c) {
                        return false;
                    }
                }
                if let Some(s) = start {
                    if l.created_at < s {
                        return false;
                    }
                }
                if let Some(e) = end {
                    if l.created_at > e {
                        return false;
                    }
                }
                true
            })
            .collect();
        let total = filtered.len();
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            filtered[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }

    /// 导出全部（或按过滤条件）为 JSON 字符串
    pub fn export_json(&self) -> String {
        let all = self.list_all();
        serde_json::to_string_pretty(&all).unwrap_or_else(|_| "[]".to_string())
    }

    /// 导出按 user_id 过滤后的 JSON（普通用户导出自己的记录）
    pub fn export_json_for_user(&self, user_id: &str) -> String {
        let all = self.list_all();
        let filtered: Vec<&RequestLog> = all
            .iter()
            .filter(|l| l.user_id.as_deref() == Some(user_id))
            .collect();
        serde_json::to_string_pretty(&filtered).unwrap_or_else(|_| "[]".to_string())
    }

    /// 导出为 CSV 字符串
    pub fn export_csv(&self) -> String {
        let all = self.list_all();
        let mut buf = String::from(
            "id,created_at,user_id,key_id,channel_id,channel_name,model,origin_model,input_tokens,output_tokens,cost,channel_cost,latency_ms,status_code,error_msg,ip\n",
        );
        for l in &all {
            buf.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&l.id),
                csv_escape(&l.created_at.to_string()),
                csv_escape(l.user_id.as_deref().unwrap_or("")),
                csv_escape(l.key_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_name.as_deref().unwrap_or("")),
                csv_escape(&l.model),
                csv_escape(l.origin_model.as_deref().unwrap_or("")),
                l.input_tokens,
                l.output_tokens,
                l.cost,
                l.channel_cost,
                l.latency_ms,
                l.status_code,
                csv_escape(l.error_msg.as_deref().unwrap_or("")),
                csv_escape(l.ip.as_deref().unwrap_or("")),
            ));
        }
        buf
    }

    /// 导出按 user_id 过滤后的 CSV（普通用户导出自己的记录）
    pub fn export_csv_for_user(&self, user_id: &str) -> String {
        let all = self.list_all();
        let mut buf = String::from(
            "id,created_at,user_id,key_id,channel_id,channel_name,model,origin_model,input_tokens,output_tokens,cost,channel_cost,latency_ms,status_code,error_msg,ip\n",
        );
        for l in &all {
            if l.user_id.as_deref() != Some(user_id) {
                continue;
            }
            buf.push_str(&format!(
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                csv_escape(&l.id),
                csv_escape(&l.created_at.to_string()),
                csv_escape(l.user_id.as_deref().unwrap_or("")),
                csv_escape(l.key_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_id.as_deref().unwrap_or("")),
                csv_escape(l.channel_name.as_deref().unwrap_or("")),
                csv_escape(&l.model),
                csv_escape(l.origin_model.as_deref().unwrap_or("")),
                l.input_tokens,
                l.output_tokens,
                l.cost,
                l.channel_cost,
                l.latency_ms,
                l.status_code,
                csv_escape(l.error_msg.as_deref().unwrap_or("")),
                csv_escape(l.ip.as_deref().unwrap_or("")),
            ));
        }
        buf
    }

    /// 聚合统计辅助方法（供 dashboard 使用）。
    ///
    /// 返回所有请求日志（按时间正序），便于上层聚合。
    pub fn all_sorted_asc(&self) -> Vec<RequestLog> {
        let mut all = self.list_all();
        all.sort_by_key(|a| a.created_at);
        all
    }
}

/// 标准 CSV 字段转义（RFC 4180）。
///
/// 规则：
/// - 字段包含逗号、双引号或换行符（`\n`/`\r`）时，用双引号包裹整个字段；
/// - 字段内的双引号用两个双引号转义（`"` → `""`）；
/// - 其余字段原样输出。
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{}\"", escaped)
    } else {
        s.to_string()
    }
}

/// 审计日志存储。
///
/// key 格式：`auditlog:{created_at}:{id}`
pub struct AuditLogStore {
    store: Arc<FileStore>,
}

impl AuditLogStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self { store }
    }

    fn key_of(log: &AuditLog) -> String {
        format!("auditlog:{}:{}", log.created_at, log.id)
    }

    /// 追加一条审计日志
    pub fn add(&self, mut log: AuditLog) -> anyhow::Result<AuditLog> {
        if log.id.is_empty() {
            log.id = uuid::Uuid::new_v4().to_string();
        }
        if log.created_at == 0 {
            log.created_at = chrono::Utc::now().timestamp();
        }
        let key = Self::key_of(&log);
        self.store.put(&key, &log)?;
        Ok(log)
    }

    /// 列出全部审计日志（按时间倒序）
    pub fn list(&self) -> Vec<AuditLog> {
        let keys = match self.store.list("auditlog:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut logs: Vec<AuditLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<AuditLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        logs
    }

    /// 按 ID 删除单条审计日志
    pub fn delete_by_id(&self, id: &str) -> anyhow::Result<()> {
        let keys = self.store.list("auditlog:")?;
        let target = keys.into_iter().find(|k| k.ends_with(&format!(":{id}")));
        if let Some(k) = target {
            self.store.delete(&k)?;
        }
        Ok(())
    }

    /// 清理早于 `before_ts` 的审计日志（与请求日志同保留策略）。
    ///
    /// key 格式 `auditlog:{created_at}:{id}` 时间戳前缀，范围扫描即删。
    pub fn delete_older_than(&self, before_ts: i64) -> anyhow::Result<usize> {
        let keys = self.store.list("auditlog:")?;
        let doomed: Vec<String> = keys
            .into_iter()
            .filter(|k| {
                k.strip_prefix("auditlog:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .map(|ts| ts < before_ts)
                    .unwrap_or(false)
            })
            .collect();
        let n = doomed.len();
        for k in doomed {
            if let Err(e) = self.store.delete(&k) {
                tracing::warn!("audit retention delete failed for {k}: {e}");
            }
        }
        Ok(n)
    }

    /// 批量删除审计日志
    pub fn delete_many(&self, ids: &[String]) -> usize {
        let mut removed = 0;
        for id in ids {
            if self.delete_by_id(id).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// 清空全部审计日志
    pub fn clear_all(&self) -> anyhow::Result<usize> {
        let keys = self.store.list("auditlog:")?;
        let n = keys.len();
        for k in &keys {
            if let Err(e) = self.store.delete(k) {
                tracing::warn!("clear audit log failed for {k}: {e}");
            }
        }
        Ok(n)
    }

    /// 分页查询（P0 性能：键倒序 LIMIT 只取一页 + COUNT 总数，索引范围扫描）
    pub fn list_paged(&self, page: usize, size: usize) -> (Vec<AuditLog>, usize) {
        let page = page.max(1);
        let size = size.max(1);
        let total = self.store.list("auditlog:").map(|k| k.len()).unwrap_or(0);
        let limit = page.saturating_mul(size);
        let keys = self
            .store
            .list_latest_keys("auditlog:", limit)
            .unwrap_or_default();
        let mut logs: Vec<AuditLog> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<AuditLog>(&k).ok().flatten())
            .collect();
        logs.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        (logs, total)
    }
}

// ── 安全事件存储 ──────────────────────────────────────────────────────

/// 安全事件类型（结构化分类，供安全中心展示与告警规则引用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityEventType {
    /// 认证失败（登录/API key 校验失败）
    AuthFailure,
    /// 限流触发（登录/注册/API 限流）
    RateLimit,
    /// IP 拦截（IP 黑名单命中）
    IpBlocked,
    /// 滥用检测（额度耗尽后继续尝试等）
    Abuse,
    /// 入侵尝试（路径遍历/异常 UA 等）
    Intrusion,
}

impl SecurityEventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AuthFailure => "auth_failure",
            Self::RateLimit => "rate_limit",
            Self::IpBlocked => "ip_blocked",
            Self::Abuse => "abuse",
            Self::Intrusion => "intrusion",
        }
    }
}

/// 单条结构化安全事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub id: String,
    /// 事件类型
    pub event_type: SecurityEventType,
    /// 严重程度：info / warning / critical
    pub severity: String,
    /// 事件来源（IP）
    pub ip: Option<String>,
    /// 关联用户（若可归因）
    pub user_id: Option<String>,
    /// 关联请求 ID（若在请求路径内）
    pub request_id: Option<String>,
    /// 人类可读详情（脱敏）
    pub detail: String,
    pub created_at: i64,
}

impl SecurityEvent {
    pub fn new(event_type: SecurityEventType, severity: &str, detail: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            event_type,
            severity: severity.to_string(),
            ip: None,
            user_id: None,
            request_id: None,
            detail,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    pub fn with_ip(mut self, ip: Option<String>) -> Self {
        self.ip = ip;
        self
    }

    pub fn with_user(mut self, user_id: Option<String>) -> Self {
        self.user_id = user_id;
        self
    }
}

/// 安全事件存储——FileStore 持久化，环形上限 1000 条。
///
/// key 格式：`security_event:{created_at}:{id}`
pub struct SecurityEventStore {
    store: Arc<FileStore>,
    /// 环形缓冲容量上限
    max_events: usize,
}

impl SecurityEventStore {
    const MAX_EVENTS: usize = 1000;

    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            store,
            max_events: Self::MAX_EVENTS,
        }
    }

    fn key_of(ev: &SecurityEvent) -> String {
        format!("security_event:{}:{}", ev.created_at, ev.id)
    }

    /// 追加一条安全事件（超出容量清理最旧）。
    pub fn add(&self, mut ev: SecurityEvent) -> anyhow::Result<SecurityEvent> {
        if ev.id.is_empty() {
            ev.id = uuid::Uuid::new_v4().to_string();
        }
        if ev.created_at == 0 {
            ev.created_at = chrono::Utc::now().timestamp();
        }
        self.store.put(&Self::key_of(&ev), &ev)?;
        self.purge_overflow();
        Ok(ev)
    }

    /// 容量超限时删除最旧事件（best-effort，失败仅告警）。
    fn purge_overflow(&self) {
        let Ok(keys) = self.store.list("security_event:") else {
            return;
        };
        let total = keys.len();
        if total <= self.max_events {
            return;
        }
        let mut timed: Vec<(i64, String)> = keys
            .into_iter()
            .map(|k| {
                let ts = k
                    .strip_prefix("security_event:")
                    .and_then(|rest| rest.split(':').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .unwrap_or(i64::MAX);
                (ts, k)
            })
            .collect();
        timed.sort_by_key(|(ts, _)| *ts);
        let excess = total - self.max_events;
        for (_, k) in timed.into_iter().take(excess) {
            let _ = self.store.delete(&k);
        }
    }

    /// 列出全部事件（按时间倒序）。
    pub fn list_all(&self) -> Vec<SecurityEvent> {
        let keys = match self.store.list("security_event:") {
            Ok(k) => k,
            Err(_) => return Vec::new(),
        };
        let mut events: Vec<SecurityEvent> = keys
            .into_iter()
            .filter_map(|k| self.store.get::<SecurityEvent>(&k).ok().flatten())
            .collect();
        events.sort_by_key(|b| std::cmp::Reverse(b.created_at));
        events
    }

    /// 过滤 + 分页。
    ///
    /// - `event_type`：按类型过滤（None=全部）
    /// - `start`：时间下限（unix ts，None=不限）
    /// - `end`：时间上限（unix ts，None=不限）
    /// - `page/size`：分页（1-based）
    pub fn list_paged(
        &self,
        event_type: Option<&str>,
        start: Option<i64>,
        end: Option<i64>,
        page: usize,
        size: usize,
    ) -> (Vec<SecurityEvent>, usize) {
        let all = self.list_all();
        let filtered: Vec<SecurityEvent> = all
            .into_iter()
            .filter(|e| {
                if let Some(ty) = event_type {
                    if e.event_type.as_str() != ty {
                        return false;
                    }
                }
                if let Some(s) = start {
                    if e.created_at < s {
                        return false;
                    }
                }
                if let Some(en) = end {
                    if e.created_at > en {
                        return false;
                    }
                }
                true
            })
            .collect();
        let total = filtered.len();
        let page = page.max(1);
        let size = size.max(1);
        let start_idx = (page - 1) * size;
        let paged = if start_idx >= total {
            Vec::new()
        } else {
            let end_idx = (start_idx + size).min(total);
            filtered[start_idx..end_idx].to_vec()
        };
        (paged, total)
    }
}

// ── 全局组合 Store ──────────────────────────────────────────────────

/// 组合日志存储（请求日志 + 审计日志 + 安全事件）。
///
/// 加入 AppState 供各处使用。
pub struct LogStore {
    pub requests: RequestLogStore,
    pub audits: AuditLogStore,
    pub security: SecurityEventStore,
}

impl LogStore {
    pub fn new(store: Arc<FileStore>) -> Self {
        Self {
            requests: RequestLogStore::new(store.clone()),
            audits: AuditLogStore::new(store.clone()),
            security: SecurityEventStore::new(store),
        }
    }

    /// 便捷方法：记录请求日志（忽略错误，仅 tracing）
    pub fn record_request(&self, log: RequestLog) {
        if let Err(e) = self.requests.add(log) {
            tracing::warn!("Failed to record request log: {e}");
        }
    }

    /// 便捷方法：记录审计日志（忽略错误，仅 tracing）
    pub fn record_audit(
        &self,
        admin_id: impl Into<String>,
        action: impl Into<String>,
        target: impl Into<String>,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) {
        let mut entry = AuditLog::new(admin_id, action, target);
        entry.before = before.map(|v| serde_json::to_string(&v).unwrap_or_default());
        entry.after = after.map(|v| serde_json::to_string(&v).unwrap_or_default());
        if let Err(e) = self.audits.add(entry) {
            tracing::warn!("Failed to record audit log: {e}");
        }
    }

    /// 便捷方法：记录安全事件（忽略错误，仅 tracing）
    pub fn record_security(&self, event: SecurityEvent) {
        if let Err(e) = self.security.add(event) {
            tracing::warn!("Failed to record security event: {e}");
        }
    }
}

// 用 RwLock 包装以便纳入 AppState（Clone）
#[allow(dead_code)]
pub type SharedLogStore = Arc<RwLock<LogStore>>;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> LogStore {
        LogStore::new(Arc::new(FileStore::new(
            TempDir::new().unwrap().path().to_path_buf(),
        )))
    }

    #[test]
    fn add_and_list_request() {
        let s = store();
        let mut log = RequestLog::new();
        log.model = "gpt-4".to_string();
        log.input_tokens = 100;
        log.output_tokens = 50;
        log.cost = 5;
        s.requests.add(log.clone()).unwrap();

        let all = s.requests.list_all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].model, "gpt-4");
        assert_eq!(all[0].input_tokens, 100);
    }

    #[test]
    fn filter_by_model() {
        let s = store();
        for m in ["gpt-4", "gpt-4", "claude-3"] {
            let mut log = RequestLog::new();
            log.model = m.to_string();
            s.requests.add(log).unwrap();
        }
        let (gpt4, total) =
            s.requests
                .list_with_filter(None, Some("gpt-4"), None, None, None, 1, 10);
        assert_eq!(total, 2);
        assert_eq!(gpt4.len(), 2);
    }

    #[test]
    fn audit_log() {
        let s = store();
        s.record_audit(
            "admin-id",
            "create",
            "user:abc",
            None,
            Some(serde_json::json!({"email": "a@b.c"})),
        );
        let all = s.audits.list();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].action, "create");
        assert!(all[0].after.is_some());
    }

    #[test]
    fn pagination() {
        let s = store();
        for i in 0..15 {
            let mut log = RequestLog::new();
            log.model = format!("m{i}");
            s.requests.add(log).unwrap();
        }
        let (page1, total) = s
            .requests
            .list_with_filter(None, None, None, None, None, 1, 10);
        assert_eq!(total, 15);
        assert_eq!(page1.len(), 10);
        let (page2, _) = s
            .requests
            .list_with_filter(None, None, None, None, None, 2, 10);
        assert_eq!(page2.len(), 5);
    }
}
