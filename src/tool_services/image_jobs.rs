// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
//
//! 图片生成任务层（`tool_image_jobs` 的上游语义 → AIGX FileStore KV）。
//!
//! ── 与上游的存储差异 ────────────────────────────────────────
//! 上游用 rusqlite 任务表 + 文件表在同一事务里维护状态机；AIGX 的通用存储是
//! `FileStore` KV（SQLite / PG / JSON 三后端同一接口），本模块**不用数据库表**：
//!   · 任务记录：`image_job:{id}` → 序列化的 [`ImageJob`]；
//!   · 任务索引：`image_jobs` → 任务 id 列表（上限 10,000，与上游一致）；
//!   · 文件索引：`image_job_file:{file_id}` → 所属任务 id；
//!   · 图片字节：落数据目录 `{data_dir}/image_jobs/{file_id}`（不塞进 KV）。
//!
//! 进程内用一把互斥锁串行「读索引 → 判重 → 写记录」，等价于上游的
//! `BEGIN IMMEDIATE` 事务；AIGX 网关是单实例部署，跨进程原子性不在承诺内。
//! 崩溃窗口：finish 先写字节再更新任务状态，中途崩溃会留下 running 任务，
//! 由 `maintain` 在超时后转 `uncertain`（与上游「执行进程已停止」同语义）。
//!
//! ── 保留的上游语义 ──────────────────────────────────────────
//! · `request_id` 幂等：同一 owner + request_id 重试返回原任务，不重复生成；
//! · running 超过 deadline → `uncertain`（上游可能已消耗额度，不自动重提）；
//! · 过期：结果字节删除、状态 `expired`、结果清空；提交编号在保留期 + 7 天
//!   内不可复用（过期记录延后 7 天物理删除）；
//! · 并发上限 / 每分钟提交频率 / 记录数 / 存储上限四道闸门。
//!
//! 幂等键按 **owner（网关 Key id）+ request_id**，与上游「Key 哈希 + 提交编号」
//! 对应（AIGX 用 Key id 代替 Key 哈希，见 `image_runtime::owner_of`）。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::storage::FileStore;

use super::image_config::{ImageConfig, ImageTarget};

/// 单张图片上限（同时用于创建时的预留字节与 finish 校验）。
pub const MAX_IMAGE_BYTES: i64 = 32 * 1024 * 1024;

const JOB_PREFIX: &str = "image_job:";
const FILE_PREFIX: &str = "image_job_file:";
const INDEX_KEY: &str = "image_jobs";
const MAX_RECORDS: usize = 10_000;
/// 过期记录再保留七天，避免迟到重试重新扣费（上游同值）。
const DEDUP_GRACE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// maintain 的最小间隔（KV 后端每次全量扫描代价不低）。
const MAINTAIN_INTERVAL_MS: i64 = 30_000;

/// 任务层错误（status/code/message 与上游 GatewayError 的两字段一一对应）。
#[derive(Debug, Clone)]
pub struct ImageToolError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ImageToolError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_image_input", message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(404, "image_job_not_found", message)
    }

    pub fn storage(message: impl std::fmt::Display) -> Self {
        Self::new(
            503,
            "image_storage_unavailable",
            format!("图片任务存储不可用：{message}"),
        )
    }
}

fn storage(error: impl std::fmt::Display) -> ImageToolError {
    ImageToolError::storage(error)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 32 字节随机 id（64 位十六进制），与上游 `random_id` 同形。
pub fn random_id() -> Result<String, ImageToolError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(storage)?;
    Ok(hex::encode(bytes))
}

/// 64 位十六进制（文件 id 白名单）。
pub fn is_file_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn job_key(id: &str) -> String {
    format!("{JOB_PREFIX}{id}")
}

fn file_key(id: &str) -> String {
    format!("{FILE_PREFIX}{id}")
}

/// 任务记录（序列化进 KV；字段名与上游表同义）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageJob {
    pub id: String,
    pub owner: String,
    pub request_id: String,
    pub request_hash: String,
    pub operation: String,
    pub provider: String,
    pub model: String,
    pub account_id: String,
    /// `running` / `succeeded` / `failed` / `uncertain` / `expired`
    pub state: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    /// running 任务的执行截止（上游 `deadline_at` 列）。
    #[serde(default)]
    pub deadline_at: i64,
    /// 创建时按张数预留的字节数；结束后归零。
    #[serde(default)]
    pub reserved_bytes: i64,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

/// 单张图片的元信息（字节在磁盘上）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageFileMeta {
    pub id: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: i64,
}

/// 任务层内部返回的图片（带字节）。
pub struct ImageFile {
    pub id: String,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

impl ImageFile {
    pub fn meta(&self) -> ImageFileMeta {
        ImageFileMeta {
            id: self.id.clone(),
            mime: self.mime.clone(),
            width: self.width,
            height: self.height,
            bytes: self.bytes.len() as i64,
        }
    }
}

/// 新任务入参（与上游 `NewJob` 同形）。
pub struct NewJob<'a> {
    pub owner: &'a str,
    pub request_id: &'a str,
    pub request_hash: &'a str,
    pub operation: &'a str,
    pub target: &'a ImageTarget,
    pub count: u32,
    pub timeout: u64,
}

/// 进程级串行锁：`ImageJobs` 每次请求都重新构造，锁放模块级才能覆盖同进程
/// 的所有 handler（等价于上游 `BEGIN IMMEDIATE` 事务；单实例部署语义）。
fn job_lock() -> &'static parking_lot::Mutex<()> {
    static LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    &LOCK
}

/// FileStore KV + 文件目录上的任务存储。
#[derive(Clone)]
pub struct ImageJobs {
    store: Arc<FileStore>,
    dir: PathBuf,
}

impl ImageJobs {
    pub fn new(store: Arc<FileStore>, dir: PathBuf) -> Self {
        Self { store, dir }
    }

    /// 生产路径：任务 KV 用 ToolServices 的 FileStore，字节落数据目录。
    pub fn from_services(services: &super::ToolServices) -> Self {
        Self::new(services.store.clone(), Self::default_root_dir())
    }

    /// 默认文件目录：与凭证密钥同根的数据目录（启动时 `set_data_dir`）。
    pub fn default_root_dir() -> PathBuf {
        crate::account_session::secret::data_dir().join("image_jobs")
    }

    fn load_index(&self) -> Result<Vec<String>, ImageToolError> {
        self.store
            .get::<Vec<String>>(INDEX_KEY)
            .map(|value| value.unwrap_or_default())
            .map_err(storage)
    }

    fn save_index(&self, ids: &[String]) -> Result<(), ImageToolError> {
        self.store.put(INDEX_KEY, &ids.to_vec()).map_err(storage)
    }

    fn load_job(&self, id: &str) -> Result<Option<ImageJob>, ImageToolError> {
        self.store.get::<ImageJob>(&job_key(id)).map_err(storage)
    }

    fn save_job(&self, job: &ImageJob) -> Result<(), ImageToolError> {
        self.store.put(&job_key(&job.id), job).map_err(storage)
    }

    fn remove_job(&self, id: &str) -> Result<(), ImageToolError> {
        self.store.delete(&job_key(id)).map_err(storage)
    }

    fn file_path(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    fn remove_files(&self, job: &ImageJob) -> Result<(), ImageToolError> {
        let files: Vec<ImageFileMeta> = job
            .result
            .as_ref()
            .and_then(|result| result.get("images"))
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| serde_json::from_value::<ImageFileMeta>(item.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();
        for file in files {
            if !is_file_id(&file.id) {
                continue;
            }
            let _ = self.store.delete(&file_key(&file.id));
            let _ = std::fs::remove_file(self.file_path(&file.id));
        }
        Ok(())
    }

    /// 过期 / 超时维护（上游 `maintain` 的同语义实现）。
    ///
    /// 公开为 `pub(crate)` 供单测把时钟推快；生产路径经 [`Self::maintain`]
    /// 做 30 秒节流。
    pub(crate) fn maintain_at(&self, now: i64) -> Result<(), ImageToolError> {
        let ids = self.load_index()?;
        let total = ids.len();
        let mut kept: Vec<String> = Vec::with_capacity(total);
        for id in ids {
            let Some(mut job) = self.load_job(&id)? else {
                continue;
            };
            let mut changed = false;
            if job.state == "running" && job.deadline_at < now {
                job.state = "uncertain".into();
                job.reserved_bytes = 0;
                job.updated_at = now;
                job.error =
                    Some("生成任务已超时或执行进程已停止，上游可能已消耗额度，未重新提交".into());
                changed = true;
            }
            if job.expires_at <= now && job.state != "running" {
                if job.state != "expired" {
                    let _ = self.remove_files(&job);
                    job.state = "expired".into();
                    job.result = None;
                    job.reserved_bytes = 0;
                    job.updated_at = now;
                    job.error = Some("图片结果已过期，未重新生成".into());
                    changed = true;
                }
                if job.expires_at < now - DEDUP_GRACE_MS {
                    let _ = self.remove_files(&job);
                    if let Err(error) = self.remove_job(&id) {
                        tracing::warn!("[Tools] 图片任务 {id} 清理失败：{}", error.message);
                    }
                    continue;
                }
            }
            if changed {
                if let Err(error) = self.save_job(&job) {
                    tracing::warn!("[Tools] 图片任务 {id} 维护写入失败：{}", error.message);
                }
            }
            kept.push(id);
        }
        if kept.len() != total {
            self.save_index(&kept)?;
        }
        Ok(())
    }

    fn maintain(&self) -> Result<(), ImageToolError> {
        static LAST: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
        let now = now_ms();
        let last = LAST.load(std::sync::atomic::Ordering::Relaxed);
        if now.saturating_sub(last) < MAINTAIN_INTERVAL_MS {
            return Ok(());
        }
        LAST.store(now, std::sync::atomic::Ordering::Relaxed);
        self.maintain_at(now)
    }

    fn find_request_locked(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<ImageJob>, ImageToolError> {
        for id in self.load_index()? {
            if let Some(job) = self.load_job(&id)? {
                if job.owner == owner && job.request_id == request_id {
                    return Ok(Some(job));
                }
            }
        }
        Ok(None)
    }

    pub fn find_request(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<ImageJob>, ImageToolError> {
        let _guard = job_lock().lock();
        self.maintain()?;
        self.find_request_locked(owner, request_id)
    }

    /// 创建任务；同一 owner + request_id 已存在时返回 `(原任务, false)`。
    pub fn create(
        &self,
        config: &ImageConfig,
        input: NewJob<'_>,
    ) -> Result<(ImageJob, bool), ImageToolError> {
        let _guard = job_lock().lock();
        let now = now_ms();
        self.maintain()?;
        if let Some(job) = self.find_request_locked(input.owner, input.request_id)? {
            return Ok((job, false));
        }
        let mut ids = self.load_index()?;
        if ids.len() >= MAX_RECORDS {
            return Err(ImageToolError::new(
                507,
                "image_storage_full",
                "图片任务记录已满，请等待过期记录清理",
            ));
        }
        let mut running: u32 = 0;
        let mut recent: u32 = 0;
        let mut used: i64 = 0;
        for id in &ids {
            let Some(job) = self.load_job(id)? else {
                continue;
            };
            if job.state == "running" {
                running += 1;
                used = used.saturating_add(job.reserved_bytes);
            }
            if job.owner == input.owner && job.created_at > now - 60_000 {
                recent += 1;
            }
            used = used.saturating_add(file_bytes(&job));
        }
        if running >= config.max_concurrency.clamp(1, 8) {
            return Err(ImageToolError::new(
                429,
                "image_concurrency_limit",
                "图片生成已达到并发上限，请稍后使用同一提交编号重试",
            ));
        }
        if recent >= 20 {
            return Err(ImageToolError::new(
                429,
                "image_rate_limit",
                "图片任务提交过于频繁，请稍后重试",
            ));
        }
        let reserve = i64::from(input.count) * MAX_IMAGE_BYTES;
        if used.saturating_add(reserve) > i64::from(config.storage_mb.clamp(64, 4096)) * 1024 * 1024
        {
            return Err(ImageToolError::new(
                507,
                "image_storage_full",
                "图片存储剩余空间不足，尚未请求生图；请清理旧结果或增加存储上限",
            ));
        }
        let job = ImageJob {
            id: random_id()?,
            owner: input.owner.to_string(),
            request_id: input.request_id.to_string(),
            request_hash: input.request_hash.to_string(),
            operation: input.operation.to_string(),
            provider: input.target.provider.clone(),
            model: input.target.model.clone(),
            account_id: input.target.account_id.clone(),
            state: "running".into(),
            created_at: now,
            updated_at: now,
            expires_at: now + i64::from(config.retention_hours.clamp(1, 168)) * 60 * 60 * 1000,
            deadline_at: now + (input.timeout as i64 + 120) * 1000,
            reserved_bytes: reserve,
            result: None,
            error: None,
        };
        self.save_job(&job)?;
        ids.push(job.id.clone());
        self.save_index(&ids)?;
        Ok((job, true))
    }

    pub fn get(&self, id: &str) -> Result<ImageJob, ImageToolError> {
        let _guard = job_lock().lock();
        self.maintain()?;
        self.load_job(id)?
            .ok_or_else(|| ImageToolError::not_found("图片任务不存在或已过期"))
    }

    pub fn list(&self) -> Result<Vec<ImageJob>, ImageToolError> {
        let _guard = job_lock().lock();
        self.maintain()?;
        let mut jobs = Vec::new();
        for id in self.load_index()? {
            if let Some(job) = self.load_job(&id)? {
                jobs.push(job);
            }
        }
        jobs.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        jobs.truncate(50);
        Ok(jobs)
    }

    /// 保存结果：写字节到磁盘 + 文件索引，再把任务置 `succeeded`。
    pub fn finish(
        &self,
        id: &str,
        mut result: Value,
        files: Vec<ImageFile>,
    ) -> Result<(), ImageToolError> {
        let _guard = job_lock().lock();
        let now = now_ms();
        let mut job = self
            .load_job(id)?
            .ok_or_else(|| ImageToolError::not_found("图片任务不存在或已过期"))?;
        let total: i64 = files.iter().map(|file| file.bytes.len() as i64).sum();
        if job.state != "running" || total > job.reserved_bytes {
            return Err(ImageToolError::storage(
                "图片已生成，但任务已结束或图片超过预留空间，请勿重新生图",
            ));
        }
        if let Err(error) = std::fs::create_dir_all(&self.dir) {
            return Err(ImageToolError::storage(format!(
                "创建图片目录失败：{error}"
            )));
        }
        let mut written: Vec<String> = Vec::new();
        let mut metas: Vec<ImageFileMeta> = Vec::with_capacity(files.len());
        for file in &files {
            if !is_file_id(&file.id) {
                self.cleanup_written(&written);
                return Err(ImageToolError::storage("图片文件编号无效"));
            }
            let target = self.file_path(&file.id);
            let temporary = self.dir.join(format!(".{}.tmp", file.id));
            if let Err(error) = std::fs::write(&temporary, &file.bytes) {
                self.cleanup_written(&written);
                return Err(ImageToolError::storage(format!("写入图片失败：{error}")));
            }
            if let Err(error) = std::fs::rename(&temporary, &target) {
                let _ = std::fs::remove_file(&temporary);
                self.cleanup_written(&written);
                return Err(ImageToolError::storage(format!("落盘图片失败：{error}")));
            }
            written.push(file.id.clone());
            if let Err(error) = self.store.put(
                &file_key(&file.id),
                &json!({ "job_id": job.id, "mime": file.mime }),
            ) {
                self.cleanup_written(&written);
                return Err(storage(error));
            }
            metas.push(file.meta());
        }
        result["images"] = serde_json::to_value(&metas).map_err(storage)?;
        job.result = Some(result);
        job.state = "succeeded".into();
        job.reserved_bytes = 0;
        job.updated_at = now;
        job.error = None;
        self.save_job(&job)
    }

    fn cleanup_written(&self, ids: &[String]) {
        for id in ids {
            let _ = self.store.delete(&file_key(id));
            let _ = std::fs::remove_file(self.file_path(id));
        }
    }

    /// 结束 running 任务（`uncertain` = 上游可能已消耗额度）。
    pub fn fail(&self, id: &str, uncertain: bool, message: &str) -> Result<(), ImageToolError> {
        let _guard = job_lock().lock();
        let Some(mut job) = self.load_job(id)? else {
            return Ok(());
        };
        if job.state != "running" {
            return Ok(());
        }
        job.state = if uncertain { "uncertain" } else { "failed" }.into();
        job.reserved_bytes = 0;
        job.updated_at = now_ms();
        job.error = Some(message.chars().take(2000).collect());
        self.save_job(&job)
    }

    /// 读取一张生成图片（调用方负责 owner / 作用域鉴权）。
    pub fn read_file(&self, id: &str) -> Result<(ImageJob, ImageFile), ImageToolError> {
        if !is_file_id(id) {
            return Err(ImageToolError::not_found("生成图片不存在或已过期"));
        }
        let _guard = job_lock().lock();
        self.maintain()?;
        let Some(meta) = self.store.get::<Value>(&file_key(id)).map_err(storage)? else {
            return Err(ImageToolError::not_found("生成图片不存在或已过期"));
        };
        let job_id = meta
            .get("job_id")
            .and_then(Value::as_str)
            .ok_or_else(|| ImageToolError::not_found("生成图片不存在或已过期"))?;
        let job = self
            .load_job(job_id)?
            .ok_or_else(|| ImageToolError::not_found("生成图片不存在或已过期"))?;
        if job.state != "succeeded" || job.expires_at <= now_ms() {
            return Err(ImageToolError::not_found("生成图片不存在或已过期"));
        }
        let file_meta = job
            .result
            .as_ref()
            .and_then(|result| result.get("images"))
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    let parsed: ImageFileMeta = serde_json::from_value(item.clone()).ok()?;
                    (parsed.id == id).then_some(parsed)
                })
            })
            .ok_or_else(|| ImageToolError::not_found("生成图片不存在或已过期"))?;
        let bytes = std::fs::read(self.file_path(id))
            .map_err(|_| ImageToolError::not_found("生成图片不存在或已过期"))?;
        Ok((
            job,
            ImageFile {
                id: id.to_string(),
                mime: file_meta.mime,
                width: file_meta.width,
                height: file_meta.height,
                bytes,
            },
        ))
    }

    /// 管理面清理：running 拒绝；其余删字节并置 `expired`。
    pub fn delete(&self, id: &str) -> Result<(), ImageToolError> {
        let _guard = job_lock().lock();
        let Some(mut job) = self.load_job(id)? else {
            return Err(ImageToolError::not_found("图片任务不存在或已过期"));
        };
        if job.state == "running" {
            return Err(ImageToolError::new(
                409,
                "image_job_running",
                "生成中的任务不能清理，请等待结束",
            ));
        }
        self.remove_files(&job)?;
        job.state = "expired".into();
        job.result = None;
        job.reserved_bytes = 0;
        job.updated_at = now_ms();
        job.error = Some("图片结果已清理，未重新生成".into());
        self.save_job(&job)
    }

    /// 已用 / 预留字节（上游 `storage_usage` 同字段）。
    pub fn storage_usage(&self) -> Result<Value, ImageToolError> {
        let _guard = job_lock().lock();
        self.maintain()?;
        let mut used: i64 = 0;
        let mut reserved: i64 = 0;
        for id in self.load_index()? {
            if let Some(job) = self.load_job(&id)? {
                used = used.saturating_add(file_bytes(&job));
                if job.state == "running" {
                    reserved = reserved.saturating_add(job.reserved_bytes);
                }
            }
        }
        Ok(json!({ "used_bytes": used, "reserved_bytes": reserved }))
    }
}

fn file_bytes(job: &ImageJob) -> i64 {
    job.result
        .as_ref()
        .and_then(|result| result.get("images"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("bytes").and_then(Value::as_i64))
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn setup() -> (ImageJobs, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FileStore::new(dir.path().join("kv")));
        let jobs = ImageJobs::new(store, dir.path().join("files"));
        (jobs, dir)
    }

    fn config() -> ImageConfig {
        ImageConfig {
            generation: ImageTarget {
                provider: "workbuddy".into(),
                model: "glm-image".into(),
                account_id: String::new(),
            },
            editing: None,
            max_images: 2,
            max_concurrency: 2,
            retention_hours: 24,
            storage_mb: 512,
        }
    }

    fn target() -> ImageTarget {
        ImageTarget {
            provider: "workbuddy".into(),
            model: "glm-image".into(),
            account_id: String::new(),
        }
    }

    fn new_job<'a>(owner: &'a str, request_id: &'a str, target: &'a ImageTarget) -> NewJob<'a> {
        NewJob {
            owner,
            request_id,
            request_hash: "hash-a",
            operation: "generate",
            target,
            count: 1,
            timeout: 300,
        }
    }

    fn png_bytes() -> Vec<u8> {
        // 1x1 PNG（无外部依赖，image crate 可识别）
        const PNG: [u8; 67] = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        PNG.to_vec()
    }

    /// 幂等：同一 owner + request_id 第二次提交返回原任务，不新增记录。
    #[test]
    fn create_is_idempotent_per_request_id() {
        let (jobs, _dir) = setup();
        let target = target();
        let (first, created) = jobs
            .create(&config(), new_job("k1", "request-0001", &target))
            .unwrap();
        assert!(created);
        let (second, created) = jobs
            .create(&config(), new_job("k1", "request-0001", &target))
            .unwrap();
        assert!(!created);
        assert_eq!(first.id, second.id);
        assert_eq!(jobs.list().unwrap().len(), 1);
        // 不同 owner 的同名 request_id 是另一个任务
        let (third, created) = jobs
            .create(&config(), new_job("k2", "request-0001", &target))
            .unwrap();
        assert!(created);
        assert_ne!(third.id, first.id);
    }

    /// running 超过 deadline → uncertain，且预留字节归零。
    #[test]
    fn running_timeout_becomes_uncertain() {
        let (jobs, _dir) = setup();
        let target = target();
        let (job, _) = jobs
            .create(&config(), new_job("k1", "request-0002", &target))
            .unwrap();
        // 直接推快时钟：deadline 是 now + (timeout + 120)s
        jobs.maintain_at(job.created_at + 10 * 60 * 1000).unwrap();
        let after = jobs.get(&job.id).unwrap();
        assert_eq!(after.state, "uncertain");
        assert_eq!(after.reserved_bytes, 0);
        assert!(after.error.unwrap().contains("上游可能已消耗额度"));
    }

    /// 过期清理：结果字节与文件索引删除，状态 expired；七天后记录物理删除。
    #[test]
    fn expired_jobs_are_cleaned() {
        let (jobs, _dir) = setup();
        let target = target();
        let (job, _) = jobs
            .create(&config(), new_job("k1", "request-0003", &target))
            .unwrap();
        let file = ImageFile {
            id: random_id().unwrap(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: png_bytes(),
        };
        let file_id = file.id.clone();
        jobs.finish(&job.id, json!({ "created": 1 }), vec![file])
            .unwrap();
        let (_, read) = jobs.read_file(&file_id).unwrap();
        assert_eq!(read.bytes.len(), png_bytes().len());

        // 24h 后过期：状态 expired、结果清空、下载 404
        jobs.maintain_at(job.created_at + 25 * 60 * 60 * 1000)
            .unwrap();
        let expired = jobs.get(&job.id).unwrap();
        assert_eq!(expired.state, "expired");
        assert!(expired.result.is_none());
        assert!(jobs.read_file(&file_id).is_err());

        // 再过七天：记录从索引与 KV 删除
        jobs.maintain_at(job.created_at + 25 * 60 * 60 * 1000 + DEDUP_GRACE_MS + 1000)
            .unwrap();
        assert!(jobs.get(&job.id).is_err());
        assert!(jobs.list().unwrap().is_empty());
    }

    /// finish 后任务成功、图片元信息可读；running 任务不允许清理。
    #[test]
    fn finish_and_delete_state_machine() {
        let (jobs, _dir) = setup();
        let target = target();
        let (job, _) = jobs
            .create(&config(), new_job("k1", "request-0004", &target))
            .unwrap();
        assert!(jobs.delete(&job.id).is_err());
        let file = ImageFile {
            id: random_id().unwrap(),
            mime: "image/png".into(),
            width: 1,
            height: 1,
            bytes: png_bytes(),
        };
        let file_id = file.id.clone();
        jobs.finish(&job.id, json!({}), vec![file]).unwrap();
        let done = jobs.get(&job.id).unwrap();
        assert_eq!(done.state, "succeeded");
        assert!(done.result.unwrap()["images"].is_array());
        jobs.delete(&job.id).unwrap();
        assert_eq!(jobs.get(&job.id).unwrap().state, "expired");
        assert!(jobs.read_file(&file_id).is_err());
    }

    /// 文件 id 必须是 64 位十六进制（防路径穿越）。
    #[test]
    fn read_file_rejects_non_hex_id() {
        let (jobs, _dir) = setup();
        assert!(jobs.read_file("../../etc/passwd").is_err());
        assert!(jobs.read_file("not-a-file-id").is_err());
        assert!(is_file_id(&"a".repeat(64)));
        assert!(!is_file_id(&"g".repeat(64)));
    }
}
