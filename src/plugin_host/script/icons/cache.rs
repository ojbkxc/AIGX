// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/icons/cache.rs
//!
//! 图标磁盘缓存：`{script_root}/icons/{sha256}.{ext}` + `index.json`。
//!
//! - 文件名是**内容摘要**（同一张图多引用共享一份文件，`read` 侧可校验
//!   内容未篡改）；
//! - `index.json` 记 `images`（引用摘要 → 文件 + 时间）与 `cards`（包/目录
//!   卡片键 → 文件），落盘原子写；
//! - 容量有界：`MAX_ENTRIES` 条、`MAX_DISK_BYTES` 字节，超限按最旧
//!   `checked_at` 淘汰。
//!
//! ## 与 A2A 的差异
//!
//! A2A 的缓存根是 `{script_root}/icons`、索引读写在 `storage::atomic_write`；
//! AIGX 版复用 `std::fs` 的 tmp+rename（无 `atomic_write` 工具），根目录
//! 由 [`install`] 注入（`{script_root}/icons`）。

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::formats;

/// 下载失败后的重试间隔（1h）。
const RETRY_MS: i64 = 3_600_000;
/// 成功后的刷新间隔（7d）。
const REFRESH_MS: i64 = 7 * 86_400_000;
/// 索引条目上限。
const MAX_ENTRIES: usize = 512;
/// 磁盘总量上限（64MiB）。
const MAX_DISK_BYTES: usize = 64 * 1024 * 1024;

/// 一条引用摘要的缓存态。
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
struct Image {
    file: Option<String>,
    bytes: usize,
    checked_at: i64,
    retry_at: i64,
}

/// 缓存索引。
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
struct Index {
    images: BTreeMap<String, Image>,
    cards: BTreeMap<String, String>,
}

/// 缓存根（注入）。
static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 注入缓存根目录（main.rs 或 runtime 启动时调一次）。
pub fn install(root: PathBuf) {
    let _ = ROOT.set(root);
}

fn root() -> Option<PathBuf> {
    ROOT.get().cloned()
}

struct Cache {
    root: PathBuf,
    index: Index,
}

fn cache() -> &'static Mutex<Option<Cache>> {
    static CACHE: OnceLock<Mutex<Option<Cache>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// 内容摘要（hex）。
pub fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn read_limited(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > limit as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// 原子写（tmp + rename）。
fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// 惰性加载索引（根变了才重读）。
fn load(cache: &mut Option<Cache>) {
    let Some(root) = root() else { return };
    if cache.as_ref().is_some_and(|c| c.root == root) {
        return;
    }
    let index = read_limited(&root.join("index.json"), 1024 * 1024)
        .and_then(|bytes| serde_json::from_slice::<Index>(&bytes).ok())
        .unwrap_or_default();
    let mut loaded = Cache { root, index };
    loaded.index.images.retain(|key, image| {
        key.len() == 64
            && key.bytes().all(|byte| byte.is_ascii_hexdigit())
            && image.file.as_deref().is_none_or(formats::filename)
    });
    loaded.index.cards.retain(|_, file| formats::filename(file));
    *cache = Some(loaded);
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 淘汰超限条目 + 删除无引用文件 + 写索引。
fn persist(cache: &mut Cache) {
    while cache.index.images.len() > MAX_ENTRIES {
        let Some(key) = cache
            .index
            .images
            .iter()
            .min_by_key(|(_, image)| image.checked_at)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cache.index.images.remove(&key);
    }
    let mut files: HashMap<String, (usize, i64)> = HashMap::new();
    for image in cache.index.images.values() {
        if let Some(file) = &image.file {
            let entry = files.entry(file.clone()).or_default();
            entry.0 = image.bytes;
            entry.1 = entry.1.max(image.checked_at);
        }
    }
    let mut ordered: Vec<_> = files.into_iter().collect();
    ordered.sort_by_key(|(_, (_, at))| *at);
    let mut total: usize = ordered.iter().map(|(_, (bytes, _))| bytes).sum();
    for (file, (bytes, _)) in ordered {
        if total <= MAX_DISK_BYTES {
            break;
        }
        cache
            .index
            .images
            .retain(|_, image| image.file.as_deref() != Some(file.as_str()));
        cache.index.cards.retain(|_, value| value != &file);
        total = total.saturating_sub(bytes);
    }
    let referenced: std::collections::HashSet<_> = cache
        .index
        .images
        .values()
        .filter_map(|image| image.file.as_ref())
        .collect();
    cache
        .index
        .cards
        .retain(|_, file| referenced.contains(file));
    if let Ok(entries) = std::fs::read_dir(&cache.root) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if formats::filename(&name) && !referenced.contains(&name) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    if let Ok(bytes) = serde_json::to_vec(&cache.index) {
        let _ = atomic_write(&cache.root.join("index.json"), &bytes);
    }
}

/// 引用摘要 → 缓存文件 URL 片段（`/plugins/script-icons/{file}`）。
pub fn source(value: &str) -> Option<String> {
    let mut guard = cache().lock().ok()?;
    load(&mut guard);
    let cache = guard.as_ref()?;
    let file = cache
        .index
        .images
        .get(&hash(value.as_bytes()))?
        .file
        .as_deref()?;
    Some(format!("/plugins/script-icons/{file}"))
}

/// 卡片键 → 缓存文件 URL 片段。
pub fn card(key: &str) -> Option<String> {
    let mut guard = cache().lock().ok()?;
    load(&mut guard);
    let cache = guard.as_ref()?;
    let file = cache.index.cards.get(key)?;
    Some(format!("/plugins/script-icons/{file}"))
}

/// 绑定卡片键到文件（`None` = 解除绑定）。
pub fn bind(key: &str, file: Option<&str>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    load(&mut guard);
    let Some(cache) = guard.as_mut() else { return };
    let cached = match file {
        Some(file) if formats::filename(file) => Some(file),
        _ => None,
    };
    if cache.index.cards.get(key).map(String::as_str) == cached {
        return;
    }
    match cached {
        Some(file) => {
            cache.index.cards.insert(key.into(), file.into());
        }
        None => {
            cache.index.cards.remove(key);
        }
    }
    persist(cache);
}

/// 只保留给定卡片键（宿主重建后清理旧包卡片）。
pub fn retain_cards(keys: &std::collections::HashSet<String>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    load(&mut guard);
    let Some(cache) = guard.as_mut() else { return };
    let count = cache.index.cards.len();
    cache.index.cards.retain(|key, _| keys.contains(key));
    if cache.index.cards.len() != count {
        persist(cache);
    }
}

/// 解析一个引用（data URL 直接落盘；https 下载）→ 文件名。
///
/// `force=true` 忽略 TTL 重新拉。失败记 `retry_at`（1h 内不再试）。
pub async fn resolve(value: &str, force: bool) -> Option<String> {
    let key = hash(value.as_bytes());
    let inline = value
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"));
    let now = now_ms();
    let previous = {
        let mut guard = cache().lock().ok()?;
        load(&mut guard);
        let cache = guard.as_mut()?;
        let mut previous = cache.index.images.get(&key).cloned().unwrap_or_default();
        if previous
            .file
            .as_ref()
            .is_some_and(|file| !cache.root.join(file).is_file())
        {
            previous.file = None;
        }
        if inline && previous.file.is_some()
            || !force
                && (previous.retry_at > now
                    || previous.file.is_some()
                        && now.saturating_sub(previous.checked_at) < REFRESH_MS)
        {
            return previous.file;
        }
        previous
    };
    let result = if inline {
        formats::inline(value).ok_or_else(|| "插件内嵌图标无效".to_string())
    } else {
        super::download::bytes(value, formats::MAX_BYTES).await
    };
    let image = result.and_then(|bytes| {
        let extension = formats::extension(&bytes).ok_or("插件图标不是支持的图片格式")?;
        let file = format!("{}.{extension}", hash(&bytes));
        let path = root()
            .ok_or_else(|| "图标缓存根未就绪".to_string())?
            .join(&file);
        if !path.is_file() {
            atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
        }
        Ok(Image {
            file: Some(file),
            bytes: bytes.len(),
            checked_at: now,
            retry_at: 0,
        })
    });
    let image = match image {
        Ok(image) => image,
        Err(_) => Image {
            retry_at: now + RETRY_MS,
            ..previous
        },
    };
    let result = image.file.clone();
    let mut guard = cache().lock().ok()?;
    load(&mut guard);
    let cache = guard.as_mut()?;
    cache.index.images.insert(key, image);
    persist(cache);
    result
}

/// 读一个缓存文件（文件名 → 字节 + MIME）。
///
/// 三重校验：文件名格式、路径不越出缓存根、内容摘要与文件名一致。
pub fn read(file: &str) -> Option<(Vec<u8>, &'static str)> {
    if !formats::filename(file) {
        return None;
    }
    let root = root()?.canonicalize().ok()?;
    let path = root.join(file).canonicalize().ok()?;
    if !path.starts_with(&root) {
        return None;
    }
    let bytes = read_limited(&path, formats::MAX_BYTES)?;
    let (digest, extension) = file.split_once('.')?;
    if hash(&bytes) != digest || formats::extension(&bytes)? != extension {
        return None;
    }
    Some((bytes, formats::mime(extension)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn with_root<R>(f: impl FnOnce() -> R) -> R {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let dir = tempfile::TempDir::new().unwrap();
            let path = dir.path().join("icons");
            std::fs::create_dir_all(&path).unwrap();
            std::mem::forget(dir);
            install(path);
        });
        f()
    }

    /// 缓存根 + 索引是进程级单例，且 `persist` 会删除「无引用文件」——
    /// 并行跑时一个测试的失败清理会删掉另一个刚写的图标。测试串行化。
    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }

    #[test]
    fn read_rejects_bad_names_and_traversal() {
        with_root(|| {
            assert!(read("../../etc/passwd").is_none());
            assert!(read("not-a-hash.png").is_none());
            assert!(read(&format!("{}.bmp", "a".repeat(64))).is_none());
            assert!(
                read(&format!("{}.png", "a".repeat(64))).is_none(),
                "不存在的文件"
            );
        });
    }

    #[tokio::test]
    async fn resolve_inline_data_url_and_read_back() {
        let _serial = serial().await;
        with_root(|| ());
        let png = b"\x89PNG\r\n\x1a\nplugin-icon-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(png);
        let data_url = format!("data:image/png;base64,{b64}");
        let file = resolve(&data_url, false).await.expect("应落盘");
        assert!(formats::filename(&file));
        let (bytes, mime) = read(&file).expect("应能读回");
        assert_eq!(bytes, png);
        assert_eq!(mime, "image/png");
        // source() 映射到 URL 片段
        assert_eq!(
            source(&data_url).as_deref(),
            Some(format!("/plugins/script-icons/{file}").as_str())
        );
    }

    #[tokio::test]
    async fn resolve_invalid_inline_is_none() {
        let _serial = serial().await;
        with_root(|| ());
        assert!(resolve("data:text/plain,abc", false).await.is_none());
        assert!(resolve("data:image/png;base64,bm90IGFuIGltYWdl", false)
            .await
            .is_none());
    }
}
