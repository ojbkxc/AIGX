//! 预览计划、确认令牌、备份轮转与原子写入、恢复。
//!
//! ## 写入事务
//!
//! 1. 读取目标文件现状（大小上限 [`MAX_FILE_BYTES`]），`compose` 生成目标内容；
//! 2. 计算指纹 `sha256(记录投影 ‖ 每个文件 {路径, 现状哈希, 目标哈希})`，
//!    签发 10 分钟有效的确认令牌（落 KV，每目标只保留最近一次）；
//! 3. 应用时**重新计算计划**并与令牌指纹比对 —— 文件在这期间被客户端自己
//!    改过、或记录被编辑过，都会导致指纹变化并拒绝写入（TOCTOU 防护）；
//! 4. 逐文件备份 `<file>.bak.<时间戳>`（每个文件保留最近
//!    [`KEEP_BACKUPS`] 份）→ 写临时文件 → rename 原子替换；
//! 5. 写入后重新读取校验；失败则用本次备份回滚已写文件。
//!
//! ## 恢复
//!
//! 恢复 = 用备份内容生成一份计划，同样**先预览 → 再应用**，只是指纹基于
//! 备份内容而非记录。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use super::edit::{self, ConfigFormat};
use super::store::{self, PreviewToken, Record};
use super::targets::{self, CurrentFiles, FileKind, Target, TargetFile};
use super::{now_ms, KEEP_BACKUPS, MAX_FILE_BYTES, PREVIEW_TOKEN_TTL_MS};

/// 一个文件的计划（before/after 均为 UTF-8 文本）。
#[derive(Debug, Clone)]
pub struct FilePlan {
    pub kind: FileKind,
    pub path: PathBuf,
    pub format: ConfigFormat,
    pub before: String,
    /// `None` = 恢复时该文件原本不存在（删除）
    pub after: Option<String>,
    pub secrets: Vec<String>,
}

impl FilePlan {
    pub fn changed(&self) -> bool {
        match &self.after {
            Some(after) => *after != self.before,
            None => !self.before.is_empty() || Path::new(&self.path).exists(),
        }
    }
}

/// 预览计划。
#[derive(Debug, Clone)]
pub struct Plan {
    pub target: Target,
    pub record_id: String,
    pub files: Vec<FilePlan>,
    pub warnings: Vec<String>,
    /// 计划主体（记录或备份来源），参与指纹
    pub subject: Value,
}

/// 读取受限大小的 UTF-8 文本。不存在 → 空串；目录/符号链接/超大 → 报错。
fn read_text(path: &Path) -> Result<String, String> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if !meta.is_file() {
                return Err(format!("{} 不是普通文件", path.display()));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(format!(
                    "{} 超过大小上限（{} 字节）",
                    path.display(),
                    meta.len()
                ));
            }
            let bytes = std::fs::read(path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
            String::from_utf8(bytes).map_err(|_| format!("{} 不是 UTF-8 文本", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(format!("{} 是失效的链接，拒绝替换", path.display()));
            }
            Ok(String::new())
        }
        Err(e) => Err(format!("读取 {} 失败：{e}", path.display())),
    }
}

fn current_files(files: &[TargetFile]) -> Result<CurrentFiles, String> {
    let mut state = CurrentFiles::default();
    for file in files {
        match file.kind {
            FileKind::Config => state.config = read_text(&file.path)?,
            FileKind::Credentials => state.credentials = read_text(&file.path)?,
        }
    }
    Ok(state)
}

/// 目标文件的写入计划（不备份、不写盘）。
pub fn plan_for_record(target: Target, record: &Record) -> Result<Plan, String> {
    let files = target.files();
    let current = current_files(&files)?;
    let api_key = store::plaintext_key(record);
    let composed = targets::compose(target, record, &api_key, &current)?;
    let mut plans = Vec::new();
    for file in &files {
        match file.kind {
            FileKind::Config => plans.push(FilePlan {
                kind: file.kind,
                path: file.path.clone(),
                format: target.format(),
                before: current.config.clone(),
                after: Some(composed.config.clone()),
                secrets: if api_key.is_empty() {
                    Vec::new()
                } else {
                    vec![api_key.clone()]
                },
            }),
            FileKind::Credentials => plans.push(FilePlan {
                kind: file.kind,
                path: file.path.clone(),
                format: ConfigFormat::Yaml,
                before: current.credentials.clone(),
                after: composed.credentials.clone(),
                secrets: composed
                    .credentials
                    .as_ref()
                    .map(|_| vec![api_key.clone()])
                    .unwrap_or_default(),
            }),
        }
    }
    Ok(Plan {
        target,
        record_id: record.id.clone(),
        files: plans,
        warnings: composed.warnings,
        subject: json!({
            "kind": "record",
            "id": record.id,
            "name": record.name,
            "updatedAt": record.updated_at,
        }),
    })
}

/// 恢复计划：用备份文本覆盖目标文件。
pub fn plan_for_restore(
    target: Target,
    file: &TargetFile,
    backup_text: String,
    backup_id: &str,
) -> Result<Plan, String> {
    if file.kind == FileKind::Credentials {
        targets::validate_dsh_credentials(&backup_text)?;
    } else {
        // 备份内容必须能被目标格式解析（防止恢复一份坏备份把客户端配置写死）
        let parsed = edit::parse(
            if file.kind == FileKind::Config {
                target.format()
            } else {
                ConfigFormat::Yaml
            },
            &backup_text,
        )?;
        if !parsed.is_object() {
            return Err("备份内容根节点不是映射/对象，拒绝恢复".into());
        }
    }
    let before = read_text(&file.path)?;
    Ok(Plan {
        target,
        record_id: backup_id.to_string(),
        files: vec![FilePlan {
            kind: file.kind,
            path: file.path.clone(),
            format: if file.kind == FileKind::Config {
                target.format()
            } else {
                ConfigFormat::Yaml
            },
            before,
            after: Some(backup_text),
            secrets: Vec::new(),
        }],
        warnings: vec![format!(
            "恢复会用备份 {backup_id} 覆盖 {}；若该文件在备份之后又被其他程序修改，请先确认。",
            file.path.display()
        )],
        subject: json!({ "kind": "backup", "id": backup_id, "file": file.path.display().to_string() }),
    })
}

fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 计划指纹（记录/备份来源 + 每个文件的路径、现状哈希、目标哈希）。
pub fn fingerprint(plan: &Plan) -> String {
    let files = plan
        .files
        .iter()
        .map(|file| {
            json!({
                "kind": file.kind.as_str(),
                "path": file.path.display().to_string(),
                "before": hash_bytes(file.before.as_bytes()),
                "after": file.after.as_deref().map(|text| hash_bytes(text.as_bytes())),
            })
        })
        .collect::<Vec<_>>();
    let payload = json!({
        "target": plan.target.id(),
        "recordId": plan.record_id,
        "subject": plan.subject,
        "files": files,
    });
    hash_bytes(payload.to_string().as_bytes())
}

/// 计划投影（预览接口响应）：内容脱敏 + 行级 diff。
pub fn plan_view(plan: &Plan) -> Value {
    let mut files = Vec::new();
    for file in &plan.files {
        let secrets: Vec<&str> = file.secrets.iter().map(String::as_str).collect();
        let before = edit::redact(&file.before, &secrets);
        let after = match &file.after {
            Some(text) => edit::redact(text, &secrets),
            None => String::new(),
        };
        files.push(json!({
            "kind": file.kind.as_str(),
            "path": file.path.display().to_string(),
            "format": file.format.as_str(),
            "before": before,
            "after": after,
            "diff": line_diff(&edit::redact(&file.before, &secrets), &after),
            "changed": file.changed(),
            "created": file.before.is_empty() && file.after.is_some(),
            "deleted": file.after.is_none(),
        }));
    }
    json!({
        "target": plan.target.id(),
        "recordId": plan.record_id,
        "configPath": plan.target.config_path().display().to_string(),
        "files": files,
        "warnings": plan.warnings,
        "hasChanges": plan.files.iter().any(FilePlan::changed),
        "fingerprint": fingerprint(plan),
    })
}

/// 行级 diff（LCS；超大输入退化为整体删除+新增，防 O(n·m) 内存放大）。
pub fn line_diff(before: &str, after: &str) -> Vec<Value> {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    if a.len().saturating_mul(b.len()) > 2_000_000 {
        return a
            .iter()
            .map(|line| json!({"kind":"removed","text":line}))
            .chain(b.iter().map(|line| json!({"kind":"added","text":line})))
            .collect();
    }
    let width = b.len() + 1;
    let mut lcs = vec![0u32; (a.len() + 1) * width];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i * width + j] = if a[i] == b[j] {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(json!({"kind":"same","text":b[j],"oldLine":i+1,"newLine":j+1}));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i * width + j + 1] >= lcs[(i + 1) * width + j]) {
            out.push(json!({"kind":"added","text":b[j],"newLine":j+1}));
            j += 1;
        } else {
            out.push(json!({"kind":"removed","text":a[i],"oldLine":i+1}));
            i += 1;
        }
    }
    out
}

/// 签发预览令牌。
pub fn issue_token(
    store: &crate::storage::FileStore,
    plan: &Plan,
) -> Result<(Value, PreviewToken), String> {
    store::purge_expired_token(store, plan.target)?;
    let token = PreviewToken {
        target: plan.target.id().into(),
        record_id: plan.record_id.clone(),
        fingerprint: fingerprint(plan),
        expires_at: now_ms() + PREVIEW_TOKEN_TTL_MS,
    };
    store::save_token(store, plan.target, &token)?;
    let mut view = plan_view(plan);
    if let Some(object) = view.as_object_mut() {
        object.insert("token".into(), json!(token.fingerprint));
    }
    Ok((view, token))
}

/// 校验令牌：目标/记录/指纹/有效期全部命中才放行。
pub fn verify_token(
    store: &crate::storage::FileStore,
    plan: &Plan,
    token: &str,
) -> Result<(), String> {
    let saved = store::take_token(store, plan.target)?.ok_or_else(|| {
        "没有可用的预览确认：请先调用预览接口，确认后再应用".to_string()
    })?;
    // 令牌一次性：校验后无论成败都清掉，避免「重复点应用」打出重复写入
    let _ = store::clear_token(store, plan.target);
    if saved.target != plan.target.id()
        || saved.record_id != plan.record_id
        || saved.fingerprint != fingerprint(plan)
    {
        return Err("配置或目标文件已变化，请重新生成预览后再应用".into());
    }
    if token.trim().is_empty() || token.trim() != saved.fingerprint {
        return Err("预览确认令牌无效，请重新生成预览".into());
    }
    if saved.expires_at < now_ms() {
        return Err("预览已过期（10 分钟），请重新生成".into());
    }
    Ok(())
}

/// 一个配置文件的备份条目。
#[derive(Debug, Clone)]
pub struct BackupEntry {
    pub path: PathBuf,
    pub id: String,
    pub created_at: i64,
    pub size: u64,
}

const BAK_MARK: &str = ".bak.";

fn backup_id(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.rsplit_once(BAK_MARK))
        .map(|(_, id)| id.to_string())
        .unwrap_or_default()
}

/// 列出某文件的备份（新的在前）。
pub fn list_backups(file: &Path) -> Result<Vec<BackupEntry>, String> {
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return Ok(Vec::new());
    };
    let Some(parent) = file.parent() else {
        return Ok(Vec::new());
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("读取备份目录 {} 失败：{e}", parent.display())),
    };
    let prefix = format!("{name}{BAK_MARK}");
    let mut items = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("读取备份失败：{e}"))?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Some(timestamp) = file_name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(created_at) = timestamp.parse::<i64>() else {
            continue;
        };
        let path = entry.path();
        let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
        items.push(BackupEntry {
            path,
            id: timestamp.to_string(),
            created_at,
            size,
        });
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    Ok(items)
}

/// 备份当前内容并轮转（每个文件只留最近 `KEEP_BACKUPS` 份）。
///
/// 备份 ID 用毫秒时间戳；同一毫秒重复备份时递增直到不撞名。
fn backup_file(path: &Path, before: &str) -> Result<BackupEntry, String> {
    let mut created_at = now_ms();
    let mut target = PathBuf::new();
    for _ in 0..1000 {
        target = PathBuf::from(format!("{}{BAK_MARK}{created_at}", path.display()));
        if !target.exists() {
            break;
        }
        created_at += 1;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    }
    std::fs::write(&target, before.as_bytes())
        .map_err(|e| format!("写备份 {} 失败：{e}", target.display()))?;
    let entries = list_backups(path)?;
    for stale in entries.iter().skip(KEEP_BACKUPS) {
        let _ = std::fs::remove_file(&stale.path);
    }
    Ok(BackupEntry {
        path: target,
        id: created_at.to_string(),
        created_at,
        size: before.len() as u64,
    })
}

/// 原子替换：写同目录临时文件 → rename。
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} 没有父目录", path.display()));
    };
    std::fs::create_dir_all(parent).map_err(|e| format!("创建目录 {} 失败：{e}", parent.display()))?;
    let temp = parent.join(format!(
        ".aigx-client-config-{}-{}.tmp",
        std::process::id(),
        now_ms()
    ));
    let result = std::fs::write(&temp, text.as_bytes())
        .and_then(|_| {
            // 保留原文件权限（Unix）；Windows 无 POSIX 位，忽略。
            #[cfg(unix)]
            if let Ok(meta) = std::fs::metadata(path) {
                let _ = std::fs::set_permissions(&temp, meta.permissions());
            }
            std::fs::rename(&temp, path)
        });
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("写入 {} 失败：{error}", path.display()));
    }
    Ok(())
}

/// 应用计划：备份 → 写入 → 回读校验；失败回滚已写文件。
///
/// 调用方必须已持有 [`super::mutation_lock`]（本函数自己也加一次 ——
/// `std::sync::Mutex` 不可重入，因此这里**不**再加锁，由 REST 入口持锁后
/// 调用本函数）。
pub fn apply_plan(plan: &Plan) -> Result<Vec<BackupEntry>, String> {
    let changes: Vec<&FilePlan> = plan.files.iter().filter(|file| file.changed()).collect();
    if changes.is_empty() {
        return Ok(Vec::new());
    }
    // 写入前再次核对现状（预览后文件可能被动过）
    for file in &changes {
        let current = read_text(&file.path)?;
        if current != file.before {
            return Err(format!(
                "{} 在预览后被其他程序修改，请重新生成预览",
                file.path.display()
            ));
        }
    }
    let mut backups = Vec::new();
    for file in &changes {
        match backup_file(&file.path, &file.before) {
            Ok(entry) => backups.push(entry),
            Err(error) => {
                return Err(format!("{error}；本次未写入任何文件"));
            }
        }
    }
    let mut written: Vec<&FilePlan> = Vec::new();
    for file in &changes {
        let result = match &file.after {
            Some(text) => write_atomic(&file.path, text),
            None => std::fs::remove_file(&file.path).map_err(|e| {
                format!("删除 {} 失败：{e}", file.path.display())
            }),
        };
        if let Err(error) = result {
            let rollback = rollback_written(&written);
            return Err(format!(
                "{error}{}",
                rollback
                    .err()
                    .map(|e| format!("；自动回滚失败：{e}，请用备份手动恢复"))
                    .unwrap_or_else(|| "；已写入的部分已回滚".into())
            ));
        }
        written.push(file);
    }
    if let Err(error) = verify_written(&changes) {
        let rollback = rollback_written(&written);
        return Err(format!(
            "{error}{}",
            rollback
                .err()
                .map(|e| format!("；自动回滚失败：{e}，请用备份手动恢复"))
                .unwrap_or_else(|| "；已写入的部分已回滚".into())
        ));
    }
    Ok(backups)
}

fn verify_written(files: &[&FilePlan]) -> Result<(), String> {
    for file in files {
        let current = read_text(&file.path)?;
        let expected = file.after.clone().unwrap_or_default();
        if current != expected {
            return Err(format!("写入后校验 {} 失败", file.path.display()));
        }
    }
    Ok(())
}

fn rollback_written(files: &[&FilePlan]) -> Result<(), String> {
    let mut errors = Vec::new();
    for file in files.iter().rev() {
        let exists = std::fs::symlink_metadata(&file.path).is_ok();
        let result = if file.before.is_empty() {
            if exists {
                std::fs::remove_file(&file.path)
                    .map_err(|e| format!("移除 {} 失败：{e}", file.path.display()))
            } else {
                Ok(())
            }
        } else {
            write_atomic(&file.path, &file.before)
        };
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("；"))
    }
}

/// 备份内容 → 文本（恢复预览用）。
pub fn read_backup(file: &Path, id: &str) -> Result<String, String> {
    if id.is_empty() || !id.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("备份 ID 无效".into());
    }
    let path = PathBuf::from(format!("{}{BAK_MARK}{id}", file.display()));
    let meta = std::fs::metadata(&path).map_err(|_| "备份不存在".to_string())?;
    if !meta.is_file() {
        return Err("备份不是普通文件".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("备份超过大小上限".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("读取备份失败：{e}"))?;
    String::from_utf8(bytes).map_err(|_| "备份不是 UTF-8 文本".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "aigx-client-config-{tag}-{}-{seq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plan(files: Vec<FilePlan>) -> Plan {
        Plan {
            target: Target::Codex,
            record_id: "r1".into(),
            files,
            warnings: Vec::new(),
            subject: json!({"kind":"record","id":"r1"}),
        }
    }

    #[test]
    fn backup_rotation_keeps_latest_n() {
        let dir = temp_dir("bak");
        let path = dir.join("config.toml");
        std::fs::write(&path, "v0").unwrap();
        for i in 1..=15 {
            backup_file(&path, &format!("v{i}")).unwrap();
        }
        let entries = list_backups(&path).unwrap();
        assert_eq!(entries.len(), KEEP_BACKUPS);
        // 最新一份是最后一次写的 v15
        let newest = std::fs::read_to_string(&entries[0].path).unwrap();
        assert_eq!(newest, "v15");
        let oldest_kept = std::fs::read_to_string(&entries.last().unwrap().path).unwrap();
        assert_eq!(oldest_kept, "v6");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_writes_and_verifies_then_can_restore() {
        let dir = temp_dir("apply");
        let config = dir.join("config.toml");
        std::fs::write(&config, "model = \"old\"\n").unwrap();
        let plan = plan(vec![FilePlan {
            kind: FileKind::Config,
            path: config.clone(),
            format: ConfigFormat::Toml,
            before: "model = \"old\"\n".into(),
            after: Some("model = \"new\"\n".into()),
            secrets: Vec::new(),
        }]);
        let backups = apply_plan(&plan).unwrap();
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "model = \"new\"\n");
        let backup_text = std::fs::read_to_string(&backups[0].path).unwrap();
        assert_eq!(backup_text, "model = \"old\"\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apply_rejects_when_file_changed_after_preview() {
        let dir = temp_dir("toctou");
        let config = dir.join("config.toml");
        std::fs::write(&config, "model = \"old\"\n").unwrap();
        let plan = plan(vec![FilePlan {
            kind: FileKind::Config,
            path: config.clone(),
            format: ConfigFormat::Toml,
            before: "model = \"old\"\n".into(),
            after: Some("model = \"new\"\n".into()),
            secrets: Vec::new(),
        }]);
        std::fs::write(&config, "model = \"edited\"\n").unwrap();
        let error = apply_plan(&plan).unwrap_err();
        assert!(error.contains("预览后被其他程序修改"), "{error}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "model = \"edited\"\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_changes_with_content() {
        let base = plan(vec![FilePlan {
            kind: FileKind::Config,
            path: PathBuf::from("/tmp/x/config.toml"),
            format: ConfigFormat::Toml,
            before: "a = 1\n".into(),
            after: Some("a = 2\n".into()),
            secrets: Vec::new(),
        }]);
        let mut other = base.clone();
        other.files[0].after = Some("a = 3\n".into());
        assert_ne!(fingerprint(&base), fingerprint(&other));
    }

    #[test]
    fn line_diff_marks_added_and_removed() {
        let diff = line_diff("a\nb\n", "a\nc\n");
        let kinds: Vec<&str> = diff
            .iter()
            .map(|item| item.get("kind").and_then(Value::as_str).unwrap())
            .collect();
        assert_eq!(kinds, vec!["same", "removed", "added"]);
    }
}