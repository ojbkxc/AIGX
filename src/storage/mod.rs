use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

// rusqlite KV 存储模块 — 仅当启用 sqlite-kv feature 时编译。
//
// 注意：rusqlite（libsqlite3-sys 0.28）与 sea-orm 的 sqlx-sqlite（libsqlite3-sys 0.26）
// 存在原生库 links 冲突，不能同时启用。当启用 sea-orm + sqlite 时需用
// --no-default-features 禁用 sqlite-kv，此时 FileStore 降级为 JSON 文件后端。
#[cfg(feature = "sqlite-kv")]
pub mod sqlite;

// PostgreSQL KV 存储模块 — 仅当启用 postgres feature 时编译。
// 通过 sea-orm 的 sqlx 依赖（sea-orm 已 re-export sqlx）直连 PostgreSQL。
#[cfg(feature = "postgres")]
pub mod postgres;

/// 将任意 key 编码为文件名安全的形式：ASCII 字母数字及 `-_.` 保持原样，
/// 其余字符以 `%XX` 形式转义。可逆，避免 Windows 下 `:`/`/` 等非法字符问题。
fn encode_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn decode_key(enc: &str) -> String {
    let bytes = enc.as_bytes();
    let mut out = Vec::with_capacity(enc.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 前缀匹配的上界：`prefix + U+10FFFF`。
///
/// **三后端共用的唯一前缀判定口径**。历史上 SQLite 用 `key >= p AND key < p+MAX`、
/// JSON 后端用 `starts_with`、PostgreSQL 用 `LIKE 'p%'`（前缀未转义）——前两者
/// 是「字面前缀」，后者是「LIKE 模式」，前缀里只要含 `%` 或 `_` 就**过匹配**：
/// 例如 `list("plugin_")` 在 PG 上会连 `pluginX` 一起返回，同一份数据换后端
/// 结果不同。本函数把口径收敛为字面区间比较（PG 的 TEXT 比较是 bytewise，
/// 与 SQLite BINARY 排序一致）。
#[allow(dead_code)] // `--no-default-features`（纯 JSON 后端）构建下没有 SQL 调用方
pub(crate) fn prefix_upper(prefix: &str) -> String {
    format!("{prefix}\u{10FFFF}")
}

/// 键区间 `[from, to)` 的数量上限钳制（三后端共用）。
///
/// 各后端对 `limit` 的默认容忍度不同，同一调用在不同后端结果不同：
/// - SQLite 负 LIMIT 语义是「无限制」，故必须钳成正区间；
/// - PostgreSQL 传负值同理（`LIMIT -1` = 无限制）；
/// - JSON 后端 `truncate(0)` 返回空。
///
/// 统一为 `[0, 10_000_000]`：**`limit = 0` 返回空集**（三后端一致；SQLite 原先
/// clamp 到 1，会让「请求 0 条」返回 1 条）。生产调用点均传 ≥1，故此归一不改
/// 现有任何查询的结果。
#[allow(dead_code)] // `--no-default-features`（纯 JSON 后端）构建下没有 SQL 调用方
pub(crate) fn clamp_limit(limit: usize) -> i64 {
    if limit > 10_000_000 {
        10_000_000
    } else {
        limit as i64
    }
}

/// 基于 JSON 文件的 KV 存储（无 rusqlite 时的回退后端）。
/// 每个键存储为 `{dir}/{encoded_key}.json`。
///
/// 默认构建（sqlite-kv）下仅作为旧数据迁移的读取源；`--no-default-features`
/// 构建（配合 SeaORM 多数据库）时 `FileStore` 直接使用本实现。
pub struct JsonFileStore {
    dir: PathBuf,
    cache: RwLock<HashMap<String, String>>,
    /// 读-改-写事务锁（`put_if_absent` / `mutate` 的检查与落盘必须同处一段）。
    ///
    /// JSON 后端没有数据库事务，唯一的原子性来源就是这把锁：`put_if_absent`
    /// 若先 `get` 再 `put`，两个线程可以同时看到「不存在」并各写一次，
    /// 返回两个 `true`——靠它做幂等的调用方（`user/checkin.rs` 的签到 CAS）
    /// 就会双发奖励。锁是进程级的：跨进程只靠文件系统，「尽力而为」。
    write: parking_lot::Mutex<()>,
}

/// 敏感数据落盘：0600 权限写文件（Unix）。
///
/// `JsonFileStore` 存储里含敏感数据（渠道凭证、API Key 信封、用户密码
/// 哈希、SMTP/Telegram 令牌）——默认 umask（022）下 `fs::write` 产出的
/// 0644 会让同机其他账户读到。`OpenOptions::mode` 仅在**创建**时生效：
/// 已存在的文件不覆盖其权限（运维手工设的权限不会被每次写入冲掉）。
///
/// Windows 无 POSIX 权限位，忽略（NTFS ACL 由系统策略管理）。
#[cfg(unix)]
fn write_secret_file(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(content.as_bytes())
}

#[cfg(not(unix))]
fn write_secret_file(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)
}

/// 原子落盘：先写同目录临时文件再 `rename` 覆盖目标。
///
/// 直接 `truncate + write` 有一个**崩溃即丢值**的窗口：文件已经变成长度 0，
/// 新内容还没落完，进程被杀/掉电 → 磁盘上留下一个空文件。读取侧
/// `serde_json::from_str("")` 报解析错，`UserStore::load` 这类「读失败即
/// 中止」的路径会让**整个用户表加载失败**，而丢的那一条恰好是最新的
/// 用户/渠道快照。`rename` 在同目录内是原子的（POSIX 与 NTFS 均保证），
/// 所以要么读到旧值，要么读到新值，不会读到半个。
fn write_secret_file_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = match path.file_name().and_then(|s| s.to_str()) {
        Some(name) => path.with_file_name(format!(".{name}.tmp")),
        None => return write_secret_file(path, content),
    };
    write_secret_file(&tmp, content)?;
    // Windows 上「rename 覆盖一个正被别的句柄打开的文件」会短暂返回
    // ERROR_ACCESS_DENIED（读取侧刚 open 还没 close）。这属于瞬时竞争，
    // 不是失败：退让 1ms 重试几次即可。最终失败才把 tmp 清掉并报错。
    let mut last_err = None;
    for attempt in 0..5 {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                if attempt < 4 {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    Err(last_err.unwrap_or_else(|| std::io::Error::other("atomic rename failed")))
}

/// SQLite 数据库文件收紧到 0600（Unix）。
///
/// db 本体由 rusqlite 创建（不受本模块 write 路径控制，umask 默认值
/// 可能是 0644）；WAL/SHM 副档同理，且 WAL 含同样的明文页——只收紧
/// 本体等于留后门，三个档一起收。失败仅告警：权限收紧是加固不是功能，
/// 不能因 chmod 失败阻断启动。已存在文件显式 set_permissions（不同于
/// write_secret_file 的「仅创建时生效」——这里就是要改存量权限）。
#[cfg(unix)]
fn harden_file_perms(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        if perms.mode() & 0o777 != 0o600 {
            perms.set_mode(0o600);
            if let Err(e) = std::fs::set_permissions(path, perms) {
                tracing::warn!("failed to harden {} to 0600: {e}", path.display());
            }
        }
    }
}

#[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
impl JsonFileStore {
    /// 创建一个新的 JSON 文件存储，数据存储在 `dir` 目录下。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            dir,
            cache: RwLock::new(HashMap::new()),
            write: parking_lot::Mutex::new(()),
        }
    }

    /// 与 `SqliteStore::open` 同签名（无 rusqlite 构建下 `FileStore` 即本类型）。
    ///
    /// 存在的意义是**签名一致**：调用方（main.rs 的启动路径、proxy_pool 的
    /// 测试）对 `FileStore::open(dir)` 的使用在两种后端下都是合法的，不该
    /// 因为换了 feature 组合就编译不过。
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        Ok(Self::new(dir))
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{}.json", encode_key(key)))
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        // B19：优先从缓存读取——原先 get 只写缓存从不读，缓存形同虚设；
        // 命中时避免一次磁盘 IO 与文件存在性检查（clone 出内容后立即释放读锁）
        if let Some(content) = self.cache.read().get(key).cloned() {
            let value = serde_json::from_str(&content)?;
            return Ok(Some(value));
        }
        let path = self.path_of(key);
        if !path.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&path)?;
        let value = serde_json::from_str(&content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(Some(value))
    }

    /// 写入 JSON 值
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let _guard = self.write.lock();
        self.put_locked(key, value)
    }

    /// 写入（调用方已持 `write` 锁）。
    fn put_locked<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        let path = self.path_of(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(value)?;
        write_secret_file_atomic(&path, &content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(())
    }

    /// 删除键
    #[allow(dead_code)] // 预留/未接线（见 main.rs 债务清单）
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        let _guard = self.write.lock();
        let path = self.path_of(key);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        self.cache.write().remove(key);
        Ok(())
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        let mut keys = Vec::new();
        if !self.dir.exists() {
            return Ok(keys);
        }
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let key = decode_key(stem);
                    if key.starts_with(prefix) {
                        keys.push(key);
                    }
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// 倒序取前 `limit` 个键（与 SqliteStore::list_latest_keys 同语义降级实现）
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        let mut keys = self.list(prefix)?;
        keys.reverse();
        keys.truncate(limit);
        Ok(keys)
    }

    /// 按键区间 `[from, to)` 取键（**纯区间**，与 `SqliteStore::list_range` 同语义）。
    ///
    /// `prefix` 只为兼容签名保留：SQLite 后端一直忽略它，PostgreSQL 后端旧实现
    /// 把它拼进 `LIKE`，于是同一组参数在三后端给出三套结果。统一以 SQLite 的
    /// 「纯键区间」为准（生产调用方本来就把前缀算进 `from`/`to`）。
    pub fn list_range(&self, _prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        // list 已按字典序升序，filter 保序，无需再排。
        self.list("").map(|keys| {
            keys.into_iter()
                .filter(|k| k.as_str() >= from && k.as_str() < to)
                .collect()
        })
    }

    /// 键区间 `[from, to)` 升序取前 `limit` 个（JSON 后端 list + filter 降级，
    /// 与 `SqliteStore::list_range_asc` 同语义）。
    pub fn list_range_asc(
        &self,
        _prefix: &str,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .list("")?
            .into_iter()
            .filter(|k| k.as_str() >= from && k.as_str() < to)
            .collect();
        keys.truncate(limit);
        Ok(keys)
    }

    /// 键区间 `[from, to)` 倒序取前 `limit` 个（JSON 后端 list + filter 降级）。
    pub fn list_range_desc(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        let mut keys: Vec<String> = self
            .list("")?
            .into_iter()
            .filter(|k| k.as_str() >= from && k.as_str() < to)
            .collect();
        keys.reverse();
        keys.truncate(limit);
        Ok(keys)
    }

    /// 键区间 `[from, to)` 的键数量（JSON 后端降级为过滤后计数）。
    pub fn count_range(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        Ok(self
            .list("")?
            .into_iter()
            .filter(|k| k.as_str() >= from && k.as_str() < to)
            .count())
    }

    /// 前缀内的键数量（JSON 后端降级为 list().len()）。
    pub fn count_prefix(&self, prefix: &str) -> anyhow::Result<usize> {
        Ok(self.list(prefix)?.len())
    }

    /// 原子插入（key 已存在返回 false）。
    ///
    /// **检查与落盘必须在同一段 `write` 锁内**：旧实现先 `get`（无锁）再在
    /// 另一段窗口里判断 `cache.contains_key` / `path.exists()`，两个线程可以
    /// 同时通过全部检查、各写一次、各返回 `true`。靠它做幂等的唯一生产调用点
    /// 是 `user/checkin.rs` 的签到 CAS（`put_if_absent` 返回 true 才保留奖励），
    /// 因此这个窗口在 JSON 后端（`--no-default-features` 构建）意味着**同一用户
    /// 同一天可双领奖励**，而 SQLite/PG 后端的 `INSERT ... ON CONFLICT DO NOTHING`
    /// 不会。跨进程仍只靠文件系统（尽力而为），锁是进程级的。
    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        let _guard = self.write.lock();
        if self.cache.read().contains_key(key) {
            return Ok(false);
        }
        let path = self.path_of(key);
        if path.exists() {
            // 缓存未命中但文件在：把内容补进缓存再拒绝（与 get 的缓存写入
            // 同口径，避免下一次 get 又走一次磁盘）。
            if let Ok(content) = std::fs::read_to_string(&path) {
                self.cache.write().insert(key.to_string(), content);
            }
            return Ok(false);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(value)?;
        write_secret_file_atomic(&path, &content)?;
        self.cache.write().insert(key.to_string(), content);
        Ok(true)
    }

    /// 原子更新（读取-修改-写入）：读、闭包、写全程持 `write` 锁。
    /// 返回旧值（如果存在）。
    ///
    /// 旧实现是 `get` → 闭包 → `put` 三步、三步之间不持锁，两个并发的自增
    /// 会读同一个旧值、各自 +1，最终只 +1（丢一次更新）。与 SQLite/PG 的同名
    /// 方法一致地只在「同一个进程内」原子（跨进程无隔离），但至少不再自身
    /// 制造丢更新。
    #[allow(dead_code)]
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default,
        F: FnOnce(Option<T>) -> T,
    {
        let _guard = self.write.lock();
        let existing: Option<T> = self.get(key)?;
        let old = existing.clone();
        let new_value = f(existing);
        self.put_locked(key, &new_value)?;
        Ok(old)
    }

    /// 原子读-改-写（同 `SqliteStore::mutate` / `PgStore::mutate_raw` 的语义）。
    ///
    /// 闭包返回 `Err` 时**一个字节都不落盘**（校验失败不该留下半截修改）。
    pub fn mutate<T, F>(&self, key: &str, f: F) -> anyhow::Result<T>
    where
        T: DeserializeOwned + Serialize + Clone,
        F: FnOnce(Option<T>) -> anyhow::Result<T>,
    {
        let _guard = self.write.lock();
        let existing: Option<T> = self.get(key)?;
        let new_value = f(existing)?;
        self.put_locked(key, &new_value)?;
        Ok(new_value)
    }

    /// 异步写入（JSON 后端降级实现）。
    ///
    /// 与 `FileStore::put_async` 同签名同语义：JSON 后端本就没有连接池/事务，
    /// 直接同步写即可；失败仅记日志，不向调用方传播（调用方原本就是 error!）。
    /// 保留这个降级实现，让热路径调用点不必按后端 feature 分叉。
    pub fn put_async<T: Serialize + Send + 'static>(
        &self,
        key: impl Into<String> + Send + 'static,
        value: T,
    ) {
        let key = key.into();
        if let Err(e) = self.put(&key, &value) {
            tracing::error!("put_async {key} failed: {e:#}");
        }
    }

    /// 多键写入（JSON 后端逐条写，无事务）。
    /// 多键写入。
    ///
    /// 与 SQLite/PG 后端的 `write_batch` 不同：JSON 后端没有事务，**逐条写、
    /// 失败即停，已写的不回滚**。这是能力边界，不是可以忽略的差异——调用方
    /// （`log::writer::apply_ops`）拿到 `Err` 会整批回队重试，故这里的
    /// 半批落库会在重试时被同一批再写一遍（聚合行因此翻倍）。默认构建
    /// （sqlite-kv）不会走到这里；只有 `--no-default-features` 构建受影响。
    pub fn write_batch(&self, entries: &[(&str, serde_json::Value)]) -> anyhow::Result<()> {
        for (key, val) in entries {
            self.put(key, val)?;
        }
        Ok(())
    }
}

/// 统一 KV 存储入口。
///
/// - 默认构建（`sqlite-kv`）：基于 SQLite（WAL 模式）的持久化 KV，
///   首次打开时自动将旧版 `*.json` 文件数据迁移进 SQLite。
/// - `--no-default-features`（配合 SeaORM）：降级为 JSON 文件后端。
/// - 同时启用 `postgres` + `database.url`：FileStore 内部改用 PgStore。
#[cfg(feature = "sqlite-kv")]
pub struct FileStore {
    inner: FileStoreInner,
}

/// 统一后端枚举：SQLite（默认）或 PostgreSQL（database.url 配置后启用）。
#[cfg(feature = "sqlite-kv")]
enum FileStoreInner {
    Sqlite(sqlite::SqliteStore),
    #[cfg(feature = "postgres")]
    Postgres(postgres::PgStore),
}

#[cfg(feature = "sqlite-kv")]
impl FileStore {
    /// 打开（或创建）`dir` 下的 SQLite 数据库，并迁移旧版 JSON 数据。
    pub fn open(dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&dir)?;
        let db_path = dir.join("aigx.db");
        let inner = sqlite::SqliteStore::open(db_path)?;
        migrate_legacy_json(&dir, &inner)?;
        // P0：db 本体 + WAL/SHM 副档收紧到 0600（含敏感数据的主存储）
        #[cfg(unix)]
        for suffix in ["", "-wal", "-shm"] {
            harden_file_perms(&dir.join(format!("aigx.db{suffix}")));
        }
        Ok(Self {
            inner: FileStoreInner::Sqlite(inner),
        })
    }

    /// 兼容旧调用：无法失败时 panic。
    #[allow(dead_code)] // 预留/线表符号（见 main.rs 债务清单）
    pub fn new(dir: PathBuf) -> Self {
        Self::open(dir).expect("failed to open SQLite storage")
    }

    /// 以 PostgreSQL 为后端打开 KV 存储（保持同样的同步接口）。
    ///
    /// 仅在启用 `postgres` feature 时可用；`database.url` 非空时由 main.rs 调用。
    /// 打开后执行一次性文件→PG 历史数据迁移（幂等，已存在 key 跳过）：
    /// `sqlite_dirs` 里的 `aigx.db` **与**同目录下的旧版 `*.json` 键文件都被
    /// 搬运（后者是 mysql / `--no-default-features` 构建的全部数据所在）。
    #[cfg(feature = "postgres")]
    pub fn open_postgres(
        url: &str,
        max_connections: u32,
        sqlite_dirs: &[std::path::PathBuf],
    ) -> anyhow::Result<Self> {
        let inner = postgres::PgStore::new(url, max_connections)?;
        let store = Self {
            inner: FileStoreInner::Postgres(inner),
        };
        migrate_sqlite_to_pg(&store, sqlite_dirs);
        Ok(store)
    }

    /// 读取 JSON 值
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> anyhow::Result<Option<T>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.get(key),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.get(key),
        }
    }

    /// 写入 JSON 值
    pub fn put<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put(key, value),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put(key, value),
        }
    }

    /// 删除键
    pub fn delete(&self, key: &str) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.delete(key),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.delete(key),
        }
    }

    /// 列出所有键（支持前缀匹配）
    pub fn list(&self, prefix: &str) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list(prefix),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list(prefix),
        }
    }

    /// P0 性能：按字典序倒序取前 `limit` 个键（前缀过滤，索引范围扫描）。
    /// 供日志分页用——key 的时间戳前缀使字典序等价时间序，只取最新一页即可。
    pub fn list_latest_keys(&self, prefix: &str, limit: usize) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_latest_keys(prefix, limit),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list_latest_keys(prefix, limit),
        }
    }

    /// 列出前缀内、字典序在 `[from, to)` 区间的键（升序）。
    ///
    /// 供按天聚合的窗口类读用：定长时间戳/日期中段使字典序等价数值序，
    /// 键范围扫把「最近 N 分钟」类查询从全表降为近窗（吃主键索引）。
    pub fn list_range(&self, prefix: &str, from: &str, to: &str) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_range(prefix, from, to),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list_range(prefix, from, to),
        }
    }

    /// 键区间 `[from, to)` 倒序取前 `limit` 个键。
    ///
    /// 日志类读的「最新 N 条」专用：SQLite 后端用 `ORDER BY key DESC LIMIT`
    /// 在索引上倒走、凑够即停；Postgres 后端降级为取区间后反转截断。
    pub fn list_range_desc(
        &self,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_range_desc(from, to, limit),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => {
                let mut keys = p.list_range("", from, to)?;
                keys.reverse();
                keys.truncate(limit);
                Ok(keys)
            }
        }
    }

    /// 键区间 `[from, to)` **升序**取前 `limit` 个键。
    ///
    /// 容量裁剪专用：最旧的键天然排在最前（定宽时间戳前缀使字典序=时间序），
    /// 两个后端都是索引范围扫 + LIMIT，不物化整个前缀的键。
    pub fn list_range_asc(
        &self,
        _prefix: &str,
        from: &str,
        to: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.list_range_asc(from, to, limit),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.list_range_asc(_prefix, from, to, limit),
        }
    }

    /// 键区间 `[from, to)` 的键数量（SQLite 走 `COUNT(*)`，不物化键）。
    pub fn count_range(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.count_range(from, to),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => Ok(p.list_range("", from, to)?.len()),
        }
    }

    /// 前缀内的键数量（SQLite 走 `COUNT(*)`，不物化键）。
    pub fn count_prefix(&self, prefix: &str) -> anyhow::Result<usize> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.count_prefix(prefix),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => Ok(p.list(prefix)?.len()),
        }
    }

    /// 原子插入（key 已存在返回 false）——check-then-put 竞态的 CAS 原语。
    pub fn put_if_absent<T: Serialize>(&self, key: &str, value: &T) -> anyhow::Result<bool> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put_if_absent(key, value),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put_if_absent(key, value),
        }
    }

    /// 原子更新（读取-修改-写入）。
    ///
    /// 三后端语义一致：整段读-改-写在同一个临界区内（SQLite 持连接锁、PG 是
    /// actor 单命令、JSON 后端持写锁）。`status` 是「老 API 形」，新代码用
    /// [`Self::mutate`]（能中途报错，且不要求 `Default`）。
    ///
    /// `Send + 'static`：PG 分支把闭包搬进 actor 线程执行。
    #[allow(dead_code)]
    pub fn update<T, F>(&self, key: &str, f: F) -> anyhow::Result<Option<T>>
    where
        T: DeserializeOwned + Serialize + Clone + Default + 'static,
        F: FnOnce(Option<T>) -> T + Send + 'static,
    {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.update(key, f),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.update(key, f),
        }
    }

    /// 异步写入：序列化在调用线程完成，落盘丢进 blocking 线程池，
    /// 避免请求线程被磁盘 IO 阻塞。
    ///
    /// 供热路径（如 channel 落盘）在**锁外**调用；`put` 本体保持同步语义不变。
    /// 失败仅记日志（与调用方原本的 error! 行为一致），不向调用方传播错误。
    pub fn put_async<T: Serialize + Send + 'static>(
        &self,
        key: impl Into<String> + Send + 'static,
        value: T,
    ) {
        let key = key.into();
        let raw = match serde_json::to_string(&value) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("put_async serialize {key} failed: {e}");
                return;
            }
        };
        // PG 后端本身是 actor 队列（send 只入队，落盘在专用工作线程），
        // 直接同步 send 即已非阻塞，无需 spawn_blocking。
        #[cfg(feature = "postgres")]
        if let FileStoreInner::Postgres(p) = &self.inner {
            if let Err(e) = p.put_raw(&key, &raw) {
                tracing::error!("put_async {key} failed: {e:#}");
            }
            return;
        }
        let sqlite = match &self.inner {
            FileStoreInner::Sqlite(s) => s.cheap_clone(),
            // 上面已处理并 return；这里只为 match 穷举（启用 postgres 时才有该变体）。
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(_) => unreachable!("postgres 后端已在上面 return"),
        };
        // 无 tokio runtime（同步测试/启动早期）时直接同步写，语义不变
        if tokio::runtime::Handle::try_current().is_err() {
            if let Err(e) = sqlite.put_raw(&key, &raw) {
                tracing::error!("put_async {key} failed: {e:#}");
            }
            return;
        }
        let key_for_log = key.clone();
        tokio::task::spawn(async move {
            let result = tokio::task::spawn_blocking(move || sqlite.put_raw(&key, &raw)).await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => tracing::error!("put_async {key_for_log} failed: {e:#}"),
                Err(e) => tracing::error!("put_async join failed: {e}"),
            }
        });
    }

    /// 多键原子写入（单事务）：中途失败全部回滚，消除「写到一半进程挂了」的中间态。
    ///
    /// 三个后端现在**都是** all-or-nothing：
    /// - SQLite：`BEGIN IMMEDIATE ... COMMIT`；
    /// - PostgreSQL：actor 内一个 `BEGIN ... COMMIT`（旧实现是逐条 `self.put()`，
    ///   属于 N 次独立提交——第 k 条失败时前 k-1 条已落库，调用方却按「整批
    ///   没落」重试，聚合行因此被折叠两次，见 `PgStore::put_many_raw` 的注释）；
    /// - JSON（`--no-default-features`）：逐条写，失败即返回错误，**已写的
    ///   不回滚**——本后端没有事务，这是能力边界而不是可忽略的差异。
    pub fn write_batch(&self, entries: &[(&str, serde_json::Value)]) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(sqlite) => sqlite.transaction(|conn| {
                for (key, val) in entries {
                    sqlite::SqliteStore::put_conn(conn, key, val)?;
                }
                Ok(())
            }),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => {
                let mut raw: Vec<(String, String)> = Vec::with_capacity(entries.len());
                for (key, val) in entries {
                    raw.push(((*key).to_string(), serde_json::to_string(val)?));
                }
                p.put_many_raw(&raw)
            }
        }
    }

    /// 原子读-改-写：`f` 拿到当前值（不存在给 `None`），返回要写入的新值。
    ///
    /// 这是「先 `get` 改完再 `put`」的**正确替代**，三后端的临界区各自成立：
    /// - SQLite：整段持 `Mutex<Connection>`（`SqliteStore::mutate`）；
    /// - PostgreSQL：整段是 actor 里的**一条命令**（`PgStore::mutate_raw`），
    ///   别的命令插不进来；
    /// - JSON：整段持 `JsonFileStore` 的写锁。
    ///
    /// 反例（本仓真实踩过两次）：`get` → 改 → `put` 之间没有任何互斥，两个
    /// 并发调用各拿旧值、各写回，后写的把先写的抹掉。改不了调用方时至少别
    /// 把这种序列写在新代码里。
    pub fn mutate<T, F>(&self, key: &str, f: F) -> anyhow::Result<T>
    where
        T: DeserializeOwned + Serialize + Clone + 'static,
        F: FnOnce(Option<T>) -> anyhow::Result<T> + Send + 'static,
    {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.mutate(key, f),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => {
                let (_old_raw, new_raw) = p.mutate_raw(key, move |existing| {
                    let parsed: Option<T> = match existing {
                        Some(raw) => Some(serde_json::from_str(&raw)?),
                        None => None,
                    };
                    let value = f(parsed)?;
                    Ok(serde_json::to_string(&value)?)
                })?;
                Ok(serde_json::from_str(&new_raw)?)
            }
        }
    }

    /// 写入原始 JSON 字符串（`put_async` 内部用；SQLite/PG 走 `put_raw`，JSON 后端直接写文件）。
    ///
    /// 调用点在 `put_async` 里按后端各取所需，这里保留一个统一的便捷实现；
    /// 迁移/容错测试也会直接写原始 JSON 文本，因此跨模块可见。
    #[cfg(feature = "sqlite-kv")]
    #[allow(dead_code)] // 生产 put_async 已原地分派；当前仅测试写原始 JSON 时调用
    pub fn put_raw(&self, key: &str, raw: &str) -> anyhow::Result<()> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put_raw(key, raw),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put_raw(key, raw),
        }
    }

    /// 原子插入**原始 JSON 文本**：key 已存在时不写入，返回 `false`。
    ///
    /// 迁移专用（`migrate_sqlite_to_pg`）：必须搬原始字节，不能走
    /// `put_if_absent<T: Serialize>` —— 后者会先反序列化再序列化，形状与存量值
    /// 不完全一致的历史记录会被 `serde_json::Value` 往返改写成另一份语义
    /// （或直接跳过，静默丢数据）。与 `put_raw` 同形态按 backend 分派。
    #[cfg(feature = "sqlite-kv")]
    #[allow(dead_code)] // 仅 sqlite→postgres 迁移（feature-gated）调用；默认构建看不到调用方
    pub fn put_if_absent_raw(&self, key: &str, raw: &str) -> anyhow::Result<bool> {
        match &self.inner {
            FileStoreInner::Sqlite(s) => s.put_if_absent_raw(key, raw),
            #[cfg(feature = "postgres")]
            FileStoreInner::Postgres(p) => p.put_if_absent_raw(key, raw),
        }
    }
}

/// 无 rusqlite 时，`FileStore` 即 JSON 文件后端。
#[cfg(not(feature = "sqlite-kv"))]
pub type FileStore = JsonFileStore;

/// 将旧版 JSON 文件数据迁移进 SQLite（幂等）：
/// - 遍历 `dir` 下所有 `*.json`；
/// - 对 SQLite 中尚不存在的 key 写入原始 JSON 内容；
/// - 已存在的 key 跳过，避免覆盖 SQLite 中的新数据。
#[cfg(feature = "sqlite-kv")]
fn migrate_legacy_json(dir: &Path, store: &sqlite::SqliteStore) -> anyhow::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut migrated = 0usize;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let key = decode_key(stem);
        if store.contains(&key)? {
            continue;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        store.put_raw(&key, &content)?;
        migrated += 1;
    }
    if migrated > 0 {
        tracing::info!("Migrated {migrated} legacy JSON entries into SQLite storage");
    }
    Ok(())
}

/// SQLite→PG 一次性历史数据迁移（幂等）：
/// - 按顺序探测 `sqlite_dirs` 中的 `aigx.db`（新旧两种布局都覆盖）；
/// - **也读同一个 dir 下的旧版 `*.json` 键文件**：`--no-default-features`
///   构建（sea-orm + mysql 矩阵）里 `FileStore` 就是 JSON 文件后端，数据全
///   落在 `{data_dir}/data/*.json` 而**没有任何 aigx.db**。只探 `aigx.db`
///   等于从 MySQL 换到 PG 时把这些数据整块留在原地，新库看起来「用户/渠道
///   全没了」（同一份数据换后端行为不同的最极端形态）。
/// - 对 PG 中尚不存在的 key 用 put_if_absent 原子写入（并发/重复启动安全）；
/// - 已存在的 key 跳过，避免覆盖 PG 中的新数据。
///
/// 背景：`open_postgres` 此前直接返回空 PG 库，历史数据（请求日志/审计/
/// 会话等）全部留在文件里，切库后页面查不到历史——补上迁移。
#[cfg(all(feature = "sqlite-kv", feature = "postgres"))]
fn migrate_sqlite_to_pg(pg: &FileStore, sqlite_dirs: &[std::path::PathBuf]) {
    for dir in sqlite_dirs {
        let db_path = dir.join("aigx.db");
        let mut sources = 0usize;
        let mut migrated = 0usize;

        if db_path.exists() {
            sources += 1;
            match sqlite::SqliteStore::open(db_path.clone()) {
                Ok(sqlite) => match sqlite.raw_pairs() {
                    Ok(pairs) => {
                        for (key, raw) in pairs {
                            // put_raw 免二次序列化；put_if_absent 保证 PG 已有
                            // （更新）的 key 不被旧值覆盖。
                            match pg.put_if_absent_raw(&key, &raw) {
                                Ok(true) => migrated += 1,
                                Ok(false) => {}
                                Err(e) => {
                                    tracing::warn!("SQLite→PG migration: put {key} failed: {e:#}")
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "SQLite→PG migration: scan {} failed: {e}",
                            db_path.display()
                        )
                    }
                },
                Err(e) => {
                    tracing::warn!("SQLite→PG migration: skip {}: {e}", db_path.display());
                }
            }
        }

        // 旧版 JSON 键文件（mysql/JSON 构建下的全部数据都在这里）
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let mut json_source = 0usize;
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let key = decode_key(stem);
                    json_source += 1;
                    match pg.put_if_absent_raw(&key, &content) {
                        Ok(true) => migrated += 1,
                        Ok(false) => {}
                        Err(e) => tracing::warn!("JSON→PG migration: put {key} failed: {e:#}"),
                    }
                }
                if json_source > 0 {
                    sources += 1;
                }
            }
            Err(e) => tracing::warn!("JSON→PG migration: read_dir {} failed: {e}", dir.display()),
        }

        if sources > 0 {
            tracing::info!(
                "file→PG migration: {migrated} entries imported from {} ({sources} source(s))",
                dir.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_put_and_get() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        store.put("test_key", &"hello").unwrap();
        let val: Option<String> = store.get("test_key").unwrap();
        assert_eq!(val, Some("hello".to_string()));
    }

    #[test]
    fn test_get_nonexistent() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        let val: Option<String> = store.get("nonexistent").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_delete() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        store.put("to_delete", &"value").unwrap();
        store.delete("to_delete").unwrap();
        let val: Option<String> = store.get("to_delete").unwrap();
        assert_eq!(val, None);
    }

    #[test]
    fn test_list_with_prefix() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        store.put("user:alice", &"data1").unwrap();
        store.put("user:bob", &"data2").unwrap();
        store.put("admin:root", &"data3").unwrap();

        let user_keys = store.list("user:").unwrap();
        assert_eq!(user_keys.len(), 2);
        assert!(user_keys.contains(&"user:alice".to_string()));
        assert!(user_keys.contains(&"user:bob".to_string()));
    }

    #[test]
    fn test_update() {
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());

        // 初始写入
        store.put("counter", &0u64).unwrap();

        // 原子更新：递增
        let old: Option<u64> = store
            .update("counter", |val| match val {
                Some(n) => n + 1,
                None => 1,
            })
            .unwrap();
        assert_eq!(old, Some(0));

        let val: Option<u64> = store.get("counter").unwrap();
        assert_eq!(val, Some(1));
    }

    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn test_legacy_json_migration() {
        let dir = TempDir::new().unwrap();
        // 预置旧版 JSON 文件
        let legacy = JsonFileStore::new(dir.path().to_path_buf());
        legacy.put("user:old1", &"legacy-data").unwrap();
        legacy.put("user:old2", &42u64).unwrap();

        // 打开 SQLite 存储（触发迁移）
        let store = FileStore::new(dir.path().to_path_buf());

        let v1: Option<String> = store.get("user:old1").unwrap();
        assert_eq!(v1, Some("legacy-data".to_string()));
        let v2: Option<u64> = store.get("user:old2").unwrap();
        assert_eq!(v2, Some(42));
    }

    /// P0：JSON 后端落盘文件必须是 0600（存储含渠道凭证/API Key 信封等敏感数据）。
    #[cfg(unix)]
    #[test]
    fn json_store_writes_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        store.put("user:secret", &"sensitive").unwrap();
        let meta = std::fs::metadata(store.path_of("user:secret")).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "敏感数据落盘必须是 0600"
        );
        // put_if_absent 同口径
        store.put_if_absent("channel:token", &"tok").unwrap();
        let meta2 = std::fs::metadata(store.path_of("channel:token")).unwrap();
        assert_eq!(meta2.permissions().mode() & 0o777, 0o600);
    }

    /// SQLite 里的**原始字节**必须原样搬进目标后端（不经过类型往返）。
    ///
    /// 迁移若逐条 `get::<serde_json::Value>()` 再 `put`，形状与目标类型不完全
    /// 一致的历史值会被往返改写或直接跳过 —— 迁移静默丢数据。这里用一条
    /// 「看起来不像任何业务类型」的历史值来钉住字节级搬运。
    #[cfg(feature = "sqlite-kv")]
    #[test]
    fn raw_pairs_preserves_exact_bytes() {
        let dir = TempDir::new().unwrap();
        let store = sqlite::SqliteStore::open(dir.path().join("test.db")).unwrap();
        let odd = r#"{"status":"active","legacy_extra":{ "nested" : [1,2,3] },"":"weird"}"#;
        store.put_raw("user:odd", odd).unwrap();
        store.put_raw("user:plain", r#"{"id":"u1"}"#).unwrap();
        let pairs = store.raw_pairs().unwrap();
        let odd_again = pairs
            .iter()
            .find(|(k, _)| k == "user:odd")
            .map(|(_, v)| v.clone())
            .expect("必须读到 user:odd");
        assert_eq!(
            odd_again, odd,
            "迁移读出的必须是原始字节，不是往返后的规范化 JSON"
        );
    }

    /// P0：SQLite 主库文件（含 WAL/SHM 副档，含同样的明文页）必须是 0600。
    #[cfg(all(unix, feature = "sqlite-kv"))]
    #[test]
    fn sqlite_db_files_are_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = TempDir::new().unwrap();
        let store = FileStore::new(dir.path().to_path_buf());
        store.put("user:secret", &"sensitive").unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join("aigx.db"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "aigx.db 必须是 0600"
        );
        for name in ["aigx.db-wal", "aigx.db-shm"] {
            let p = dir.path().join(name);
            if p.exists() {
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{name} 必须是 0600");
            }
        }
    }

    /// 同一 key 的并发 `put_if_absent` 只能有一个赢家。
    ///
    /// 判据是**确定性**的：闭包/判据看的是「赢家数量必须恰好 1」。旧实现先
    /// 无锁 `get` 再另开一段窗口判断 `cache.contains_key` / `path.exists()`，
    /// 两个线程可以同时通过全部检查并各返回 `true`。这条在 16 线程下旧实现
    /// 会间歇性出现 `wins > 1`（`user/checkin.rs` 依赖它做「同日只能领一次」
    /// 的幂等，多出的赢家就是多发一份奖励）。
    #[test]
    fn json_put_if_absent_has_single_winner() {
        let dir = TempDir::new().unwrap();
        let store = std::sync::Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        for round in 0..8 {
            let key = format!("checkin:u{round}:2026-01-01");
            let wins = std::sync::atomic::AtomicU32::new(0);
            std::thread::scope(|scope| {
                for _ in 0..16 {
                    let store = store.clone();
                    let key = key.clone();
                    let wins = &wins;
                    scope.spawn(move || {
                        if store.put_if_absent(&key, &1u64).unwrap() {
                            wins.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                    });
                }
            });
            assert_eq!(
                wins.load(std::sync::atomic::Ordering::SeqCst),
                1,
                "round {round}: put_if_absent 必须恰好一个赢家"
            );
            let v: Option<u64> = store.get(&key).unwrap();
            assert_eq!(v, Some(1));
        }
    }

    /// 并发 `update` 不丢更新：N 个线程各自增一次，最终必须等于 N。
    ///
    /// 旧实现「get → 闭包 → put」三步不持锁，闭包里睡 2ms 把窗口撑开，16 线程
    /// 各 +1 后典型结果是远小于 16 的中间值（每次并发都能撞上）。
    #[test]
    fn json_update_does_not_lose_increments() {
        let dir = TempDir::new().unwrap();
        let store = std::sync::Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        store.put("counter", &0u64).unwrap();
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let store = store.clone();
                scope.spawn(move || {
                    store
                        .update("counter", |v| {
                            // 撑开读-改-写窗口（旧实现在这里被并发打断）
                            std::thread::sleep(std::time::Duration::from_millis(2));
                            v.unwrap_or(0) + 1
                        })
                        .unwrap();
                });
            }
        });
        let final_v: Option<u64> = store.get("counter").unwrap();
        assert_eq!(final_v, Some(16), "并发自增丢了更新：{final_v:?}");
    }

    /// 落盘是「写临时文件 + rename」：文件在磁盘上**永远不出现半截内容**。
    ///
    /// 旧实现 `truncate + write_all`：截断之后、写完之前磁盘上是**空文件**，
    /// 此刻崩溃/被杀就留下一个 0 字节的 JSON —— 读取侧解析失败，而
    /// `UserStore::load` 这类「读失败即中止」的路径会让整张表加载失败。
    /// 注意判据必须**直接读文件**（绕开 `get` 的内存缓存），否则缓存会掩盖
    /// 整个窗口、让这条测试恒真。
    #[test]
    fn json_put_is_atomic_on_disk() {
        let dir = TempDir::new().unwrap();
        let store = std::sync::Arc::new(JsonFileStore::new(dir.path().to_path_buf()));
        // 值足够大，让旧实现的「截断→写完」之间有可观察的空窗
        let big: Vec<String> = (0..20_000).map(|i| format!("payload-{i}")).collect();
        store.put("blob", &big).unwrap();
        let path = store.path_of("blob");
        let stop = std::sync::atomic::AtomicBool::new(false);
        let bad = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let path = path.clone();
                let stop = &stop;
                let bad = &bad;
                scope.spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        // 直接读文件：空文件 / 半截 JSON 都会在解析处暴露
                        match std::fs::read_to_string(&path) {
                            Ok(text) => {
                                if serde_json::from_str::<Vec<String>>(&text).is_err() {
                                    bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                }
                            }
                            // 目标文件在 rename 期间应该一直存在
                            Err(_) => {
                                bad.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
            {
                let store = store.clone();
                let big = big.clone();
                let stop = &stop;
                scope.spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        store.put("blob", &big).unwrap();
                    }
                });
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        assert_eq!(
            bad.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "并发写期间读到了空文件/半截 JSON（落盘不是原子的）"
        );
    }

    /// `list = 0` 在三后端都必须是「空集」（SQLite 原先 clamp 到 1）。
    #[test]
    fn limit_zero_means_empty() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        for i in 0..3 {
            store.put(&format!("k:{i}"), &i).unwrap();
        }
        assert!(store.list_latest_keys("k:", 0).unwrap().is_empty());
        assert!(store
            .list_range_asc("k:", "k:", "k:\u{10FFFF}", 0)
            .unwrap()
            .is_empty());
        assert!(store
            .list_range_desc("k:", "k:\u{10FFFF}", 0)
            .unwrap()
            .is_empty());
    }

    /// `list_range` 是**纯键区间**，与 `prefix` 参数无关（与 SQLite 同语义）。
    ///
    /// 旧实现先按 `prefix` 过滤：同一组 `(prefix, from, to)` 在 SQLite 上返回
    /// 区间全部键、在 JSON/PG 上只返回前缀内的键 —— 同一份数据换后端结果不同。
    #[test]
    fn list_range_is_prefix_agnostic() {
        let dir = TempDir::new().unwrap();
        let store = JsonFileStore::new(dir.path().to_path_buf());
        store.put("a:1", &1).unwrap();
        store.put("b:1", &1).unwrap();
        let keys = store.list_range("a:", "a:", "c").unwrap();
        assert_eq!(keys, vec!["a:1".to_string(), "b:1".to_string()]);
        assert_eq!(store.count_range("a:", "c").unwrap(), 2);
    }
}
