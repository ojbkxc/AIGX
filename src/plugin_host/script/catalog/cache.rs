//! catalog.json 磁盘缓存：目录的持久态 + 各阶段 TTL / 重试闸门。
//!
//! ## 为什么 TTL 三个阶段不一样
//!
//! - **market 6h**：registry.json 是人工策展清单（magpie-community 仓库
//!   main 分支），变化频率是「有人提交 PR」的节奏——小时级足够新鲜；
//! - **github 10min**：topic:magpie-plugin 搜索结果随时可能冒出新仓，
//!   第三方发现是「越快看到越好」的软实时需求；但搜索 API 匿名配额只有
//!   10 次/分钟，不能更频繁；
//! - **stars 6h**：星数是排序参考不是事实数据，没必要跟着 github 阶段
//!   每次都拉——stars 单独成阶段（refresh.rs），失败不影响列表可用性。
//!
//! ## 重试闸门（market_retry / github_retry / stars_retry）
//!
//! network::Failure 返回 retry_at；这里按阶段存住，refresh 层在 now <
//! retry_at 时**跳过该阶段**（不空转打限流源）。三闸门独立：market 失败
//! 不阻塞 github 刷新。
//!
//! ## 文件形态与安全边界
//!
//! version==1（结构不兼容时整体丢弃重拉，不做迁移）；整文件 ≤16MB
//! （load 时拒绝超限文件并当作空缓存——被写坏的巨型文件不应阻塞网关启动；
//! 实测 registry 79KB + dist 3.5MB，正常状态离上限很远）。
//! 写盘走 tmp + rename 原子替换，网关被 kill 不会留下半截 JSON。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// pub(crate)：IgnoredRepo 定义在 types.rs（本模块转手引用），mod.rs 与
// github.rs 经 `cache::IgnoredRepo` 取用——私有 use 对它们不可见（E0603）。
pub(crate) use super::types::{Entry, IgnoredRepo};

/// 缓存结构版本。字段/语义不兼容时 +1，load 遇到不认识的版本直接丢弃。
pub const CACHE_VERSION: u32 = 1;

/// 缓存文件大小上限。源文件（registry 79KB / dist 3.5MB）正常离它很远；
/// 超限说明缓存已坏，按空缓存处理。
pub const MAX_CACHE_BYTES: usize = 16 << 20;

/// TTL（秒）：market 6h / github 10min / stars 6h。
pub const TTL_MARKET_SECS: i64 = 6 * 3600;
pub const TTL_GITHUB_SECS: i64 = 600;
pub const TTL_STARS_SECS: i64 = 6 * 3600;

/// 目录缓存（落盘形态；字段名 snake_case，js-host 侧按同名键读）。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Cache {
    /// 结构版本（==1，不认识即丢弃）。
    pub version: u32,
    /// registry 策展条目（official + recommended）。
    pub market: Vec<Entry>,
    /// GitHub topic 发现条目（third-party）。
    pub github: Vec<Entry>,
    /// market 阶段成功时刻（unix 秒）。
    pub market_at: i64,
    /// github 阶段成功时刻（unix 秒）。
    pub github_at: i64,
    /// stars 阶段成功时刻（unix 秒）。
    pub stars_at: i64,
    /// market 重试闸门（unix 秒；now < 该值则跳过 market 阶段）。
    pub market_retry: i64,
    /// github 重试闸门。
    pub github_retry: i64,
    /// stars 重试闸门。
    pub stars_retry: i64,
    /// market 最近一次失败原因（管理面展示）。
    pub market_error: Option<String>,
    /// github 最近一次失败原因。
    pub github_error: Option<String>,
    /// github 阶段「搜索成功但 0 结果」标记（与失败区分：0 结果是正常态）。
    pub github_missing: bool,
    /// 被忽略的仓（验证失败 / 6h 内无 push），不再深扫。
    pub ignored: Vec<IgnoredRepo>,
}

impl Cache {
    /// 空缓存（version 预填）。
    pub fn empty() -> Self {
        Cache {
            version: CACHE_VERSION,
            ..Default::default()
        }
    }

    /// market 是否过期（从未成功拉取也算过期）。
    pub fn market_stale(&self, now: i64) -> bool {
        self.market_at == 0 || now - self.market_at >= TTL_MARKET_SECS
    }

    /// github 是否过期。
    pub fn github_stale(&self, now: i64) -> bool {
        self.github_at == 0 || now - self.github_at >= TTL_GITHUB_SECS
    }

    /// stars 是否过期。
    pub fn stars_stale(&self, now: i64) -> bool {
        self.stars_at == 0 || now - self.stars_at >= TTL_STARS_SECS
    }

    /// 阶段是否被重试闸门拦住（失败退避期内不再打源）。
    pub fn market_blocked(&self, now: i64) -> bool {
        now < self.market_retry
    }
    pub fn github_blocked(&self, now: i64) -> bool {
        now < self.github_retry
    }
    pub fn stars_blocked(&self, now: i64) -> bool {
        now < self.stars_retry
    }

    /// 记一次阶段失败：写错误信息 + 推进重试闸门。
    pub fn record_failure(&mut self, stage: Stage, message: String, retry_at: i64) {
        match stage {
            Stage::Market => {
                self.market_error = Some(message);
                self.market_retry = retry_at;
            }
            Stage::Github => {
                self.github_error = Some(message);
                self.github_retry = retry_at;
            }
            Stage::Stars => {
                self.stars_retry = retry_at;
            }
        }
    }

    /// 记一次阶段成功：清错误 + 清闸门 + 盖时间戳。
    pub fn record_success(&mut self, stage: Stage, now: i64) {
        match stage {
            Stage::Market => {
                self.market_error = None;
                self.market_retry = 0;
                self.market_at = now;
            }
            Stage::Github => {
                self.github_error = None;
                self.github_retry = 0;
                self.github_at = now;
                self.github_missing = self.github.is_empty();
            }
            Stage::Stars => {
                self.stars_retry = 0;
                self.stars_at = now;
            }
        }
    }

    /// 缓存文件路径：`{script_plugins_root}/catalog.json`。
    ///
    /// runtime 把这个绝对路径塞进 ScriptInitPayload.catalog_path 交给
    /// js-host（Bun 侧市场查询直接读文件，不走网关 RPC）。
    pub fn cache_path(script_root: &Path) -> PathBuf {
        script_root.join("catalog.json")
    }

    /// 从磁盘加载；任何异常（不存在 / 超限 / 版本不认识 / 解析失败）都
    /// 返回空缓存——缓存永远只是加速，坏了就重拉，不让网关启动失败。
    pub fn load(path: &Path) -> Cache {
        let Ok(bytes) = std::fs::read(path) else {
            return Cache::empty();
        };
        if bytes.len() > MAX_CACHE_BYTES {
            tracing::warn!(
                size = bytes.len(),
                cap = MAX_CACHE_BYTES,
                "catalog.json 超过大小上限，按空缓存处理（会重新拉取）"
            );
            return Cache::empty();
        }
        match serde_json::from_slice::<Cache>(&bytes) {
            Ok(c) if c.version == CACHE_VERSION => c,
            Ok(c) => {
                tracing::warn!(
                    got = c.version,
                    want = CACHE_VERSION,
                    "catalog.json 版本不认识，丢弃重拉"
                );
                Cache::empty()
            }
            Err(e) => {
                tracing::warn!(error = %e, "catalog.json 解析失败，按空缓存处理");
                Cache::empty()
            }
        }
    }

    /// 原子写盘：先写 `catalog.json.tmp` 再 rename（同目录 rename 是原子操作；
    /// Windows 上 std::fs::rename 对已存在目标会覆盖）。
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, path)
    }
}

/// 刷新阶段（TTL / 闸门 / 错误字段都按阶段分键）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Market,
    Github,
    Stars,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            label: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn ttl_semantics() {
        let mut c = Cache::empty();
        assert!(c.market_stale(1000));
        c.record_success(Stage::Market, 10_000);
        assert!(!c.market_stale(10_000 + TTL_MARKET_SECS - 1));
        assert!(c.market_stale(10_000 + TTL_MARKET_SECS));
    }

    #[test]
    fn failure_gate_blocks_then_expires() {
        let mut c = Cache::empty();
        c.record_failure(Stage::Github, "HTTP 403".into(), 50_000);
        assert!(c.github_blocked(49_999));
        assert!(!c.github_blocked(50_000));
        assert_eq!(c.github_error.as_deref(), Some("HTTP 403"));
        c.record_success(Stage::Github, 50_001);
        assert!(c.github_error.is_none());
        assert_eq!(c.github_retry, 0);
    }

    #[test]
    fn load_rejects_bad_version_and_garbage() {
        let dir = std::env::temp_dir().join("aigx_catalog_cache_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("catalog.json");

        let mut c = Cache::empty();
        c.market.push(sample_entry("x"));
        c.save(&p).unwrap();
        let loaded = Cache::load(&p);
        assert_eq!(loaded.market.len(), 1);

        std::fs::write(&p, br#"{"version": 99}"#).unwrap();
        assert!(Cache::load(&p).market.is_empty());

        std::fs::write(&p, b"not json at all").unwrap();
        assert!(Cache::load(&p).market.is_empty());

        let _ = std::fs::remove_file(&p);
        assert!(Cache::load(&p).market.is_empty());
    }

    #[test]
    fn save_is_atomic_roundtrip() {
        let dir = std::env::temp_dir().join("aigx_catalog_cache_test2");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("catalog.json");
        let mut c = Cache::empty();
        c.github.push(sample_entry("r1"));
        c.record_success(Stage::Github, 123);
        // ignored 语义（retain/drain/解除条件）由 github.rs 与 refresh.rs
        // 的集成路径验证；这里只测 roundtrip
        c.ignored.push(IgnoredRepo {
            repo: "owner/bad".into(),
            reason: "no entry evidence".into(),
            at: 100,
            pushed_at: 456,
        });
        c.save(&p).unwrap();
        let loaded = Cache::load(&p);
        assert_eq!(loaded.github.len(), 1);
        assert_eq!(loaded.github_at, 123);
        assert_eq!(loaded.ignored.len(), 1);
        assert_eq!(loaded.ignored[0].repo, "owner/bad");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn cache_path_layout() {
        let p = Cache::cache_path(Path::new("/data/script-plugins"));
        assert_eq!(p, Path::new("/data/script-plugins/catalog.json"));
    }

    #[test]
    fn github_missing_flag_tracks_empty() {
        let mut c = Cache::empty();
        assert!(!c.github_missing);
        c.record_success(Stage::Github, 1);
        assert!(c.github_missing);
        c.github.push(sample_entry("r"));
        c.record_success(Stage::Github, 2);
        assert!(!c.github_missing);
    }
}
