//! 同步快照：**哪些数据进云同步、怎么构建、怎么合并回去**。
//!
//! ```text
//! build(store, versions, now)  KV → SyncSnapshot（含逐键 updated_at）
//! merge(local, remote, mode)  两份快照 → 合并结果 + 需要写回的条目
//! apply(store, writes)        合并结果 → KV（逐键写回）
//! safety_backup(snapshot, …)  拉取前把本机现状写到 backups/
//! ```
//!
//! ── 数据范围（唯一判定入口 [`is_synced_key`]）───────────────
//! 同步的是 FileStore KV 里**除日志与云同步自身状态之外**的全部键：
//!   · 排除请求日志 / 审计日志（`reqlog:` / `auditlog:`，体积大且是本机痕迹）；
//!   · 排除 `sync:config` / `sync:status` / `sync:key_versions`——配置里有远端
//!     凭据密文，传上云等于把钥匙放进云盘；
//!   · 排除 `backgroundTaskState`（后台任务的排期是本机运行状态，换机重建）。
//! 构建与合并两侧共用这一处判定，避免「构建时排除、恢复时又写回来」。
//!
//! ── 为什么快照值保持库里的原始形态（密文原样）────────────────
//! KV 里大量凭证本来就是 `enc1:` 信封（密钥是本机 `account.key`）。若在构建
//! 时解密，等于把全部凭据以明文写进要上传到第三方存储的文件；原样搬运的好处
//! 是快照不含明文、恢复时也不需要「解密再加密」。代价是换机恢复需要同一把
//! `account.key`——manifest 里带指纹，拉取时提示不一致。
//!
//! ── 内容哈希与 updated_at ──────────────────────────────────
//! `content_hash` 只哈希 **entries**（BTreeMap 顺序稳定），不含导出时刻 / 设备名，
//! 因此「内容没变」在两次构建间得到同一个哈希——上传前的跳过判断依赖它。
//!
//! KV 的每个值没有天然的修改时间。逐键版本表（`sync:key_versions`）记录
//! 「键 → 内容哈希 + 首次观察到该值的时刻」：构建时值哈希与表里一致 → 沿用
//! 旧时刻；不一致（新增 / 修改）→ 记当前时刻。合并时用它和远端快照自带的
//! `updated_at` 比较，实现「按条目 updated_at 新者胜」。
//!
//! ── 合并模式 ────────────────────────────────────────────────
//!   · `merge`：逐键比较 updated_at，新者胜；两边相同时保留本机；本机独有键
//!     保留（不删本机数据）；远端独有键导入。
//!   · `local`：本机强制覆盖——冲突键一律保留本机（远端独有键仍会导入，
//!     它不属于冲突）。
//!   · `remote`：远端强制覆盖——冲突键一律取远端。**仍不删除**远端没有的
//!     本机键：删除是不可逆操作，同步恢复的定位是「补齐数据」而不是「清库重放」。
//!
//! 恢复前一定先做安全备份（[`safety_backup`]），与整库导入的纪律一致。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::storage::FileStore;

use super::config::KeyVersion;

/// 快照格式标识（改这个字符串等于让所有已发布的远端数据「不被认识」）。
pub const SNAPSHOT_FORMAT: &str = "aigx-cloud-sync";
/// 快照协议版本。
pub const SNAPSHOT_VERSION: u32 = 1;
/// 远端快照文件名。
pub const REMOTE_SNAPSHOT: &str = "snapshot.json";
/// 远端 manifest 文件名。
pub const REMOTE_MANIFEST: &str = "manifest.json";
/// 快照体积上限（256 MiB）。
pub const MAX_SNAPSHOT_BYTES: usize = 256 * 1024 * 1024;
/// manifest 体积上限（1 MiB）。
pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// 本机运行状态键：不进快照（换机重建，带上只会误导）。
const MACHINE_LOCAL_KEYS: &[&str] = &["backgroundTaskState"];

/// 一个键的快照条目。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncEntry {
    /// 该值的首次观察时刻（毫秒）；合并时与远端条目比较。
    #[serde(default)]
    pub updated_at: i64,
    /// KV 里的原始 JSON 值（密文原样）。
    pub value: Value,
}

/// 一份同步快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSnapshot {
    /// 格式标识（[`SNAPSHOT_FORMAT`]）。
    pub format: String,
    /// 协议版本（[`SNAPSHOT_VERSION`]）。
    pub version: u32,
    /// 导出时刻（毫秒）。
    pub exported_at: i64,
    /// 导出设备名（展示 / 排障用）。
    #[serde(default)]
    pub device: String,
    /// 键 → 条目（BTreeMap 保证序列化顺序稳定）。
    pub entries: BTreeMap<String, SyncEntry>,
}

impl SyncSnapshot {
    /// 内容哈希：只哈希 entries，与导出时刻 / 设备名无关。
    pub fn content_hash(&self) -> String {
        hash_entries(&self.entries)
    }

    /// 结构校验（拉取远端数据时用）。
    pub fn validate(&self) -> Result<(), String> {
        if self.format != SNAPSHOT_FORMAT {
            return Err(format!(
                "远端文件不是 AIGX 云同步快照（format = {}）",
                self.format
            ));
        }
        if self.version != SNAPSHOT_VERSION {
            return Err(format!(
                "远端快照协议版本不兼容（v{}，本机 v{SNAPSHOT_VERSION}）",
                self.version
            ));
        }
        Ok(())
    }
}

/// 一个 JSON 值的内容哈希。
pub fn hash_value(value: &Value) -> String {
    match serde_json::to_vec(value) {
        Ok(bytes) => super::sha256_hex(&bytes),
        // Value 一定能序列化；真失败也不该让整次构建 panic
        Err(_) => String::new(),
    }
}

impl SyncEntry {
    /// 本条目在逐键版本表里的对应项。
    pub fn to_version(&self) -> KeyVersion {
        KeyVersion {
            hash: hash_value(&self.value),
            updated_at: self.updated_at,
        }
    }
}

/// 一组条目的内容哈希。
pub fn hash_entries(entries: &BTreeMap<String, SyncEntry>) -> String {
    match serde_json::to_vec(entries) {
        Ok(bytes) => super::sha256_hex(&bytes),
        Err(_) => String::new(),
    }
}

/// 这个 KV 键要不要进云同步。**唯一判定入口**。
pub fn is_synced_key(key: &str) -> bool {
    if key.is_empty() || key.len() > 512 {
        return false;
    }
    if crate::api::admin::backup::LOG_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
    {
        return false;
    }
    if key == super::config::KV_KEY_CONFIG
        || key == super::config::KV_KEY_STATUS
        || key == super::config::KV_KEY_VERSIONS
    {
        return false;
    }
    !MACHINE_LOCAL_KEYS.contains(&key)
}

/// 构建本机快照，并返回更新后的逐键版本表。
pub fn build(
    store: &FileStore,
    versions: &BTreeMap<String, KeyVersion>,
    now: i64,
) -> Result<(SyncSnapshot, BTreeMap<String, KeyVersion>), String> {
    let keys = store
        .list("")
        .map_err(|error| format!("列出本机数据键失败: {error}"))?;
    let mut entries = BTreeMap::new();
    let mut next_versions = BTreeMap::new();
    for key in keys {
        if !is_synced_key(&key) {
            continue;
        }
        let value: Value = match store.get(&key) {
            Ok(Some(value)) => value,
            // 竞态下键被删：跳过
            Ok(None) => continue,
            Err(error) => {
                // 单键读失败不阻断整份快照（键损坏 ≠ 整库不可同步）
                tracing::warn!("[CloudSync] 读取键 {key} 失败，已跳过: {error}");
                continue;
            }
        };
        let hash = hash_value(&value);
        let updated_at = versions
            .get(&key)
            .filter(|version| version.hash == hash)
            .map(|version| version.updated_at)
            .unwrap_or(now);
        entries.insert(key.clone(), SyncEntry { updated_at, value });
        next_versions.insert(key, KeyVersion { hash, updated_at });
    }
    Ok((
        SyncSnapshot {
            format: SNAPSHOT_FORMAT.to_string(),
            version: SNAPSHOT_VERSION,
            exported_at: now,
            device: super::device_name(),
            entries,
        },
        next_versions,
    ))
}

/// 合并模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMode {
    /// 逐键 updated_at 新者胜（默认）。
    Merge,
    /// 本机强制覆盖冲突条目。
    Local,
    /// 远端强制覆盖冲突条目。
    Remote,
}

impl MergeMode {
    /// 解析请求里的模式字符串。
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "merge" => Ok(Self::Merge),
            "local" => Ok(Self::Local),
            "remote" => Ok(Self::Remote),
            other => Err(format!(
                "未知的合并模式 {other}（可选 merge / local / remote）"
            )),
        }
    }

    /// 展示名。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Local => "local",
            Self::Remote => "remote",
        }
    }
}

/// 合并统计。
#[derive(Debug, Clone, Default)]
pub struct MergeStats {
    /// 远端条目数。
    pub remote_entries: usize,
    /// 本机条目数。
    pub local_entries: usize,
    /// 远端独有、导入的条目数。
    pub added: usize,
    /// 冲突且取远端（覆盖本机）的条目数。
    pub updated: usize,
    /// 冲突且保留本机的条目数。
    pub kept_local: usize,
    /// 远端快照里混入的「本机专属键」条数（已忽略）。
    pub ignored_remote_keys: usize,
}

impl MergeStats {
    /// 界面形态。
    pub fn to_json(&self) -> Value {
        json!({
            "remote_entries": self.remote_entries,
            "local_entries": self.local_entries,
            "added": self.added,
            "updated": self.updated,
            "kept_local": self.kept_local,
            "ignored_remote_keys": self.ignored_remote_keys,
        })
    }
}

/// 合并结果。
pub struct MergeOutcome {
    /// 合并后的完整条目视图（用于计算恢复后应有的本机内容哈希）。
    pub merged: BTreeMap<String, SyncEntry>,
    /// 需要写回本机的条目（只有取远端的那些）。
    pub writes: Vec<(String, SyncEntry)>,
    /// 统计。
    pub stats: MergeStats,
}

/// 合并两份快照。纯函数（不读库、不发网络），合并策略的单测入口。
pub fn merge(local: &SyncSnapshot, remote: &SyncSnapshot, mode: MergeMode) -> MergeOutcome {
    let mut merged: BTreeMap<String, SyncEntry> = BTreeMap::new();
    let mut writes: Vec<(String, SyncEntry)> = Vec::new();
    let mut stats = MergeStats {
        remote_entries: remote.entries.len(),
        local_entries: local.entries.len(),
        ..MergeStats::default()
    };

    for (key, remote_entry) in &remote.entries {
        // 远端快照可能被手改 / 来自更宽松的实现：本机专属键一律忽略
        if !is_synced_key(key) {
            stats.ignored_remote_keys += 1;
            continue;
        }
        match local.entries.get(key) {
            None => {
                merged.insert(key.clone(), remote_entry.clone());
                writes.push((key.clone(), remote_entry.clone()));
                stats.added += 1;
            }
            Some(local_entry) => {
                let take_remote = match mode {
                    // 严格「新者胜」：相等/无法比较时保留本机（尽量不覆盖本机改动）
                    MergeMode::Merge => remote_entry.updated_at > local_entry.updated_at,
                    MergeMode::Local => false,
                    MergeMode::Remote => true,
                };
                if take_remote {
                    merged.insert(key.clone(), remote_entry.clone());
                    writes.push((key.clone(), remote_entry.clone()));
                    stats.updated += 1;
                } else {
                    merged.insert(key.clone(), local_entry.clone());
                    stats.kept_local += 1;
                }
            }
        }
    }

    for (key, local_entry) in &local.entries {
        if merged.contains_key(key) {
            continue;
        }
        // 本机独有键：保留（同步恢复不删除本机数据）
        merged.insert(key.clone(), local_entry.clone());
        stats.kept_local += 1;
    }

    MergeOutcome {
        merged,
        writes,
        stats,
    }
}

/// 把合并结果写回 KV，返回实际写入条数。
pub fn apply(store: &FileStore, writes: &[(String, SyncEntry)]) -> Result<usize, String> {
    let mut written = 0usize;
    for (key, entry) in writes {
        if !is_synced_key(key) {
            continue;
        }
        store
            .put(key, &entry.value)
            .map_err(|error| format!("写入键 {key} 失败: {error}"))?;
        written += 1;
    }
    Ok(written)
}

/// 拉取（应用远端数据）之前，把本机当前快照写到 `{data_dir}/backups/`。
///
/// 与整库导入的 `pre-import-<ts>.json` 同一职责：恢复是破坏性的，用户后悔时
/// 要有一份可读的留影。先写临时文件再改名，中途失败不会留下半份 JSON 被误当
/// 成可用的备份。
pub fn safety_backup(snapshot: &SyncSnapshot, data_dir: &Path) -> Result<String, String> {
    let dir = data_dir.join("backups");
    std::fs::create_dir_all(&dir).map_err(|error| format!("创建备份目录失败: {error}"))?;
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let name = format!(
        "cloud-sync-{stamp}-{}.json",
        chrono::Utc::now().timestamp_millis() % 1000
    );
    let target = dir.join(&name);
    let tmp = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| format!("序列化安全备份失败: {error}"))?;
    std::fs::write(&tmp, &bytes).map_err(|error| format!("写入安全备份失败: {error}"))?;
    std::fs::rename(&tmp, &target).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        format!("保存安全备份失败: {error}")
    })?;
    Ok(name)
}

/// 远端 manifest：回答「远端这一份是什么」并携带完整性校验。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncManifest {
    /// 格式标识。
    pub format: String,
    /// 协议版本。
    pub version: u32,
    /// 生成该快照的程序版本（展示 / 排障）。
    #[serde(default)]
    pub app_version: String,
    /// 生成设备。
    #[serde(default)]
    pub device: String,
    /// 生成时刻（毫秒）。
    pub created_at: i64,
    /// 条目数。
    pub entry_count: usize,
    /// 快照文件字节数。
    pub snapshot_bytes: u64,
    /// 快照**条目集合**的内容哈希（上传跳过判断 / 跨机比较用）。
    pub content_hash: String,
    /// 快照**文件字节**的 sha256（下载完整性校验用）。
    pub file_sha256: String,
}

impl SyncManifest {
    /// 由快照及其序列化字节构造 manifest。
    pub fn build(snapshot: &SyncSnapshot, bytes: &[u8]) -> Self {
        Self {
            format: SNAPSHOT_FORMAT.to_string(),
            version: SNAPSHOT_VERSION,
            app_version: crate::update::CURRENT_VERSION.to_string(),
            device: snapshot.device.clone(),
            created_at: snapshot.exported_at,
            entry_count: snapshot.entries.len(),
            snapshot_bytes: bytes.len() as u64,
            content_hash: snapshot.content_hash(),
            file_sha256: super::sha256_hex(bytes),
        }
    }

    /// 校验下载回来的字节与 manifest 记录一致。
    pub fn verify(&self, bytes: &[u8]) -> Result<(), String> {
        if self.format != SNAPSHOT_FORMAT {
            return Err(format!(
                "远端 manifest 不是 AIGX 云同步数据（format = {}）",
                self.format
            ));
        }
        if self.version != SNAPSHOT_VERSION {
            return Err(format!(
                "远端 manifest 协议版本不兼容（v{}，本机 v{SNAPSHOT_VERSION}）",
                self.version
            ));
        }
        if self.snapshot_bytes != bytes.len() as u64 {
            return Err(format!(
                "远端快照大小不符（应为 {} 字节，实际 {} 字节），文件可能被截断，请重新上传",
                self.snapshot_bytes,
                bytes.len()
            ));
        }
        let actual = super::sha256_hex(bytes);
        if actual != self.file_sha256 {
            return Err(
                "远端快照校验失败（sha256 不一致），文件可能已损坏，请重新上传".to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(updated_at: i64, value: Value) -> SyncEntry {
        SyncEntry { updated_at, value }
    }

    fn snapshot(entries: &[(&str, SyncEntry)]) -> SyncSnapshot {
        SyncSnapshot {
            format: SNAPSHOT_FORMAT.to_string(),
            version: SNAPSHOT_VERSION,
            exported_at: 1_700_000_000_000,
            device: "test".to_string(),
            entries: entries
                .iter()
                .map(|(key, entry)| ((*key).to_string(), entry.clone()))
                .collect(),
        }
    }

    /// 内容哈希：与导出时刻 / 设备名无关，只随条目内容变化。
    #[test]
    fn content_hash_ignores_export_metadata() {
        let a = snapshot(&[("k", entry(1, json!({"v": 1})))]);
        let mut b = a.clone();
        b.exported_at += 999;
        b.device = "another-machine".to_string();
        assert_eq!(a.content_hash(), b.content_hash());

        let mut changed = a.clone();
        changed
            .entries
            .insert("k".to_string(), entry(1, json!({"v": 2})));
        assert_ne!(a.content_hash(), changed.content_hash());
    }

    /// 上传跳过判断：同哈希跳过、不同哈希上传。
    #[test]
    fn hash_skip_decision() {
        let a = snapshot(&[("k", entry(1, json!({"v": 1})))]);
        let b = snapshot(&[("k", entry(2, json!({"v": 1})))]);
        assert_eq!(
            a.content_hash(),
            b.content_hash(),
            "仅 updated_at 变化不影响内容哈希"
        );
        let c = snapshot(&[("k", entry(2, json!({"v": 9})))]);
        assert_ne!(a.content_hash(), c.content_hash());
    }

    /// merge：远端 updated_at 更新 → 取远端；本机更新 → 保留本机。
    #[test]
    fn merge_newer_wins() {
        let local = snapshot(&[
            ("old-remote", entry(200, json!({"v": "local"}))),
            ("old-local", entry(100, json!({"v": "local"}))),
            ("local-only", entry(50, json!({"v": "local"}))),
        ]);
        let remote = snapshot(&[
            ("old-remote", entry(100, json!({"v": "remote"}))),
            ("old-local", entry(300, json!({"v": "remote"}))),
            ("remote-only", entry(10, json!({"v": "remote"}))),
        ]);
        let outcome = merge(&local, &remote, MergeMode::Merge);
        // 冲突：本机 200 > 远端 100 → 保留本机
        assert_eq!(outcome.merged["old-remote"].value, json!({"v": "local"}));
        // 冲突：远端 300 > 本机 100 → 取远端
        assert_eq!(outcome.merged["old-local"].value, json!({"v": "remote"}));
        // 远端独有 → 导入；本机独有 → 保留
        assert_eq!(outcome.merged["remote-only"].value, json!({"v": "remote"}));
        assert_eq!(outcome.merged["local-only"].value, json!({"v": "local"}));
        assert_eq!(outcome.stats.added, 1);
        assert_eq!(outcome.stats.updated, 1);
        assert_eq!(outcome.stats.kept_local, 3);
        assert_eq!(outcome.writes.len(), 2);
    }

    /// merge：updated_at 相同 → 保留本机（不覆盖本机改动）。
    #[test]
    fn merge_tie_keeps_local() {
        let local = snapshot(&[("k", entry(100, json!({"v": "local"})))]);
        let remote = snapshot(&[("k", entry(100, json!({"v": "remote"})))]);
        let outcome = merge(&local, &remote, MergeMode::Merge);
        assert_eq!(outcome.merged["k"].value, json!({"v": "local"}));
        assert!(outcome.writes.is_empty());
        assert_eq!(outcome.stats.kept_local, 1);
    }

    /// 强制模式：local 全部保留本机冲突；remote 全部取远端。
    #[test]
    fn merge_force_modes() {
        let local = snapshot(&[
            ("k1", entry(900, json!({"v": "local"}))),
            ("k2", entry(100, json!({"v": "local"}))),
        ]);
        let remote = snapshot(&[
            ("k1", entry(100, json!({"v": "remote"}))),
            ("k2", entry(900, json!({"v": "remote"}))),
        ]);

        let forced_local = merge(&local, &remote, MergeMode::Local);
        assert_eq!(forced_local.merged["k1"].value, json!({"v": "local"}));
        assert_eq!(forced_local.merged["k2"].value, json!({"v": "local"}));
        assert!(forced_local.writes.is_empty());

        let forced_remote = merge(&local, &remote, MergeMode::Remote);
        assert_eq!(forced_remote.merged["k1"].value, json!({"v": "remote"}));
        assert_eq!(forced_remote.merged["k2"].value, json!({"v": "remote"}));
        assert_eq!(forced_remote.writes.len(), 2);
    }

    /// 远端混入本机专属键（sync:config 等）→ 忽略，绝不写回本机。
    #[test]
    fn merge_ignores_machine_local_remote_keys() {
        let local = snapshot(&[]);
        let remote = snapshot(&[
            (
                "sync:config",
                entry(1, json!({"webdav": {"password": "stolen"}})),
            ),
            ("reqlog:1", entry(1, json!({"msg": "log"}))),
            ("channel:ok", entry(1, json!({"name": "ok"}))),
        ]);
        let outcome = merge(&local, &remote, MergeMode::Remote);
        assert!(!outcome.merged.contains_key("sync:config"));
        assert!(!outcome.merged.contains_key("reqlog:1"));
        assert!(outcome.merged.contains_key("channel:ok"));
        assert_eq!(outcome.stats.ignored_remote_keys, 2);
        assert_eq!(outcome.writes.len(), 1);
    }

    /// 快照结构校验：格式 / 版本不符一律拒绝。
    #[test]
    fn validate_rejects_wrong_format_or_version() {
        let mut snapshot = snapshot(&[]);
        assert!(snapshot.validate().is_ok());
        snapshot.format = "someone-else".to_string();
        assert!(snapshot.validate().is_err());
        snapshot.format = SNAPSHOT_FORMAT.to_string();
        snapshot.version = SNAPSHOT_VERSION + 1;
        assert!(snapshot.validate().is_err());
    }

    /// manifest 校验：字节数 / sha256 不符即拒绝。
    #[test]
    fn manifest_verifies_bytes() {
        let snapshot = snapshot(&[("k", entry(1, json!({"v": 1})))]);
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let manifest = SyncManifest::build(&snapshot, &bytes);
        assert!(manifest.verify(&bytes).is_ok());

        let mut truncated = bytes.clone();
        truncated.truncate(bytes.len() - 1);
        assert!(manifest.verify(&truncated).is_err());

        let mut tampered = bytes.clone();
        let last = tampered.len() - 2;
        if tampered[last] == b'1' {
            tampered[last] = b'2';
        } else {
            tampered[last] = b'1';
        }
        assert!(manifest.verify(&tampered).is_err());
    }

    /// 同步范围：日志与云同步自身状态永不进快照。
    #[test]
    fn synced_key_filter() {
        assert!(is_synced_key("channel:abc"));
        assert!(is_synced_key("apikey_x"));
        assert!(is_synced_key("user:someone"));
        assert!(!is_synced_key("reqlog:1"));
        assert!(!is_synced_key("auditlog:1"));
        assert!(!is_synced_key("sync:config"));
        assert!(!is_synced_key("sync:status"));
        assert!(!is_synced_key("sync:key_versions"));
        assert!(!is_synced_key("backgroundTaskState"));
        assert!(!is_synced_key(""));
    }

    /// 合并模式的字符串解析。
    #[test]
    fn merge_mode_parse() {
        assert_eq!(MergeMode::parse("").unwrap(), MergeMode::Merge);
        assert_eq!(MergeMode::parse("MERGE").unwrap(), MergeMode::Merge);
        assert_eq!(MergeMode::parse("local").unwrap(), MergeMode::Local);
        assert_eq!(MergeMode::parse("remote").unwrap(), MergeMode::Remote);
        assert!(MergeMode::parse("overwrite").is_err());
    }
}
