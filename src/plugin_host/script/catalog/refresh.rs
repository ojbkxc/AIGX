//! 后台刷新任务：Notify 唤醒 + 60s 空闲 tick 的刷新流水线。
//!
//! ## 调度模型（为什么是 Notify + 空闲 tick）
//!
//! 两个唤醒源：
//! - **启动即刷**：网关起来后第一轮全量刷新（force=true，无视 TTL）；
//! - **手动 wake**：管理面「立即刷新」按钮 / 安装失败后的补偿刷新
//!   （force 按调用方意愿）；
//! - **60s tick**：空闲轮询兜底——TTL 到期自然触发下一轮。tick 而不是
//!   长睡眠：目录是「打开插件页就要好看」的面子工程，60s 粒度足够；
//!   长睡眠（如睡 6h 等 TTL）会让手动 wake 白等很久。
//!
//! ## 刷新顺序：CDN 优先 → 旧链回落 → stars
//!
//! 1. **CDN 聚合目录**（[`super::cdn`]，对齐 A2A beta.4）：一次拉齐
//!    official / recommended / third-party 全部分组，再用分发索引补充
//!    Magpie 条目的下载地址（[`super::distribution`]）。成功即接管
//!    market + github 两个阶段。
//! 2. **旧链回落**：CDN 不可用（网络 / 格式 / 空目录保护）时，走 AIGX
//!    自带的 registry + dist join + npm 回退（[`super::market`]）与 GitHub
//!    topic 发现（[`super::github`]）——**保证不比没有 CDN 时更差**。
//!    `CDN 失败` 与 `旧链阶段失败` 的重试闸门都在旧链里推进，避免每轮
//!    反复打两个源。
//! 3. **stars**：第三方条目星数刷新（独立 12h TTL，失败不影响可用性）。
//!
//! ## 失败语义
//!
//! CDN 返回 [`Err`] 时**不**推进闸门，直接进入旧链；旧链阶段自己决定
//! 成功（清闸门、盖时间戳）还是失败（推进带抖动的重试闸门）。因此 CDN
//! 挂掉时刷新频率由旧链的 TTL / 闸门控制，不会热循环。

use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

use super::cache::{Cache, Stage};
use super::cdn;
use super::network;
use super::types::{merge, ScriptPublisher};

/// 空闲轮询间隔。
const IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(60);

/// 失败重试的随机抖动范围（秒）：避免同一批客户端在同一秒一起重试
///（对齐 A2A beta.3 的 RETRY_JITTER_MS）。
const RETRY_JITTER_SECS: i64 = 5 * 60;

/// 进程内稳定的随机值：每个客户端各错开一段，重试和刷新不会挤在同一时刻。
///
/// 用进程启动时刻做 seed（`OnceLock` 只算一次），后续调用稳定——保证
/// 「同一进程内多次抖动一致」，而不是每次都跳到新值。
fn noise(max: i64) -> i64 {
    use std::sync::OnceLock;
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEED: OnceLock<i64> = OnceLock::new();
    let seed = *SEED.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|value| value.as_nanos() as i64)
            .unwrap_or(0)
    });
    if max <= 0 {
        0
    } else {
        seed.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407)
            .rotate_left(17)
            .rem_euclid(max)
    }
}

/// 在 base 之上再加一段随时间变化的抖动，保证多客户端与多次重试都错开。
fn spread(base: i64, range: i64) -> i64 {
    if range <= 0 {
        return base;
    }
    base + (noise(range) + network::now_unix().rem_euclid(range)).rem_euclid(range)
}

/// 目录刷新器（对外 API 在 [`super::Catalog`]）。
pub struct Refresher {
    /// 共享缓存（内存态；每次变更即写盘）。
    pub cache: Arc<Mutex<Cache>>,
    /// 缓存文件绝对路径（runtime 取去填 ScriptInitPayload.catalog_path）。
    pub path: std::path::PathBuf,
    /// 唤醒信号（true = force 刷新）。
    wake: Notify,
    /// 最近一次唤醒是否带 force（wake() 与 force wake() 竞争时后到者赢）。
    force_flag: Arc<std::sync::atomic::AtomicBool>,
    /// HTTP 客户端（目录刷新专用，带 GitHub 头逻辑）。
    pub http: reqwest::Client,
}

impl Refresher {
    /// 创建刷新器（加载磁盘缓存；**不**启动后台任务——测试用）。
    pub fn new(script_root: &std::path::Path) -> Arc<Refresher> {
        let path = Cache::cache_path(script_root);
        let cache = Arc::new(Mutex::new(Cache::load(&path)));
        Arc::new(Refresher {
            cache,
            path,
            wake: Notify::new(),
            force_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            http: network::build_client(),
        })
    }

    /// 创建并**立即**启动后台任务（启动即全量刷一轮）。
    ///
    /// 这是网关启动路径：`Refresher::spawn(&script_plugins_root(data_dir))`。
    pub fn spawn(script_root: &std::path::Path) -> Arc<Refresher> {
        let r = Self::new(script_root);
        tokio::spawn(run_loop(r.clone()));
        r
    }

    /// 手动唤醒一轮刷新。force=true 时无视 TTL（但不动重试闸门）。
    pub fn wake(&self, force: bool) {
        if force {
            self.force_flag
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.wake.notify_one();
    }

    /// 状态快照（管理面 /api/script/catalog/status）。
    pub async fn status(&self) -> Status {
        let c = self.cache.lock().await;
        Status {
            market_at: c.market_at,
            github_at: c.github_at,
            stars_at: c.stars_at,
            market_error: c.market_error.clone(),
            github_error: c.github_error.clone(),
            github_missing: c.github_missing,
            market_stale: c.market_stale(network::now_unix()),
            github_stale: c.github_stale(network::now_unix()),
            stars_stale: c.stars_stale(network::now_unix()),
            official: c
                .market
                .iter()
                .filter(|e| e.publisher == ScriptPublisher::Official)
                .count(),
            recommended: c
                .market
                .iter()
                .filter(|e| e.publisher == ScriptPublisher::Recommended)
                .count(),
            third_party: c.github.len(),
            ignored: c.ignored.len(),
        }
    }
}

/// 状态快照（serde 序列化成管理面 JSON，snake_case）。
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "snake_case")]
pub struct Status {
    pub market_at: i64,
    pub github_at: i64,
    pub stars_at: i64,
    pub market_error: Option<String>,
    pub github_error: Option<String>,
    pub github_missing: bool,
    pub market_stale: bool,
    pub github_stale: bool,
    pub stars_stale: bool,
    /// 三分类计数（前端分组角标用）。
    pub official: usize,
    pub recommended: usize,
    pub third_party: usize,
    /// 被忽略的仓数量。
    pub ignored: usize,
}

/// 主循环：启动即刷 → 循环（tick / wake 二选一）→ 到期或被唤醒就刷。
async fn run_loop(r: Arc<Refresher>) {
    // 启动即全量刷（force：无视 TTL；首刷的重试闸门本来就全是 0）
    refresh_cycle(&r, true).await;
    loop {
        tokio::select! {
            _ = r.wake.notified() => {
                let force = r.force_flag.swap(false, std::sync::atomic::Ordering::Relaxed);
                refresh_cycle(&r, force).await;
            }
            _ = tokio::time::sleep(IDLE_TICK) => {
                // 空闲 tick：只刷过期阶段（refresh_cycle 内部按 TTL 判断）
                refresh_cycle(&r, false).await;
            }
        }
    }
}

/// 一轮刷新：CDN →（失败回落）旧链 market/github → stars。
async fn refresh_cycle(r: &Refresher, force: bool) {
    let client = &r.http;

    // market 是否到期（github 在旧链里再各自判 TTL）
    let do_market = force || {
        let c = r.cache.lock().await;
        c.market_stale(network::now_unix()) && !c.market_blocked(network::now_unix())
    };

    let mut cdn_done = false;
    if do_market {
        let cdn_result = cdn::fetch(client).await;
        // 「CDN 成功才接管」的判断单独抽出，便于单测（见 tests::cdn_usable）。
        if cdn_usable(&cdn_result) {
            let catalog = match cdn_result {
                Ok(catalog) => catalog,
                Err(_) => unreachable!("cdn_usable 已保证 Ok"),
            };
            let catalog = fill_distribution(client, catalog).await;
            let mut c = r.cache.lock().await;
            apply_cdn(&mut c, catalog, network::now_unix());
            let _ = c.save(&r.path);
            tracing::info!(
                market = c.market.len(),
                third_party = c.github.len(),
                "CDN 目录刷新成功"
            );
            cdn_done = true;
        } else if let Err(f) = cdn_result {
            tracing::warn!("CDN 目录不可用，回落旧链：{}", f.message);
        }
    }

    // CDN 未接管：旧链 market + github（各自按 TTL / 闸门决定跑不跑）。
    if !cdn_done {
        refresh_legacy_market_and_github(r, force).await;
    }

    // 阶段 3：stars（独立 TTL；只刷第三方条目）
    refresh_stars(r, force).await;
}

/// CDN 结果是否可用；不可用（任何 `Err`）即回落旧链。
///
/// 抽成函数而非内联 `is_ok()`：这是「CDN 失败回落旧链」契约的单一判据，
/// 单测直接钉住它。
fn cdn_usable(result: &Result<cdn::Catalog, network::Failure>) -> bool {
    result.is_ok()
}

/// 给 CDN 目录补分发数据（Magpie 下载地址 + integrity）。
///
/// 分发索引整段失败也不阻断：Magpie 条目会被标成「暂不可安装」，其余
/// 来源（npm / github）照常可用。
async fn fill_distribution(client: &reqwest::Client, mut catalog: cdn::Catalog) -> cdn::Catalog {
    match super::distribution::fetch(client).await {
        Ok(dist) => {
            let hits = super::distribution::attach_archives(&mut catalog.market, &dist);
            tracing::info!(hits, "CDN 条目补充分发数据完成");
        }
        Err(f) => {
            tracing::warn!("分发索引不可用，Magpie 条目暂标不可安装：{}", f.message);
            super::distribution::mark_missing_archives(&mut catalog.market);
        }
    }
    catalog
}

/// 把一次成功的 CDN 结果落进缓存：接管 market + github 两个阶段，清错误
/// 与闸门、盖时间戳；上游 warning 写进 `market_error` 供管理面展示。
fn apply_cdn(cache: &mut Cache, catalog: cdn::Catalog, now: i64) {
    cache.market = catalog.market;
    cache.github = catalog.github;
    cache.record_success(Stage::Market, now);
    cache.record_success(Stage::Github, now);
    // record_success 会清 market_error；上游告警要覆盖回去（None 时保持已清）。
    cache.market_error = catalog.warning;
}

/// 旧链阶段 1 + 2：market（registry + dist + npm）→ github（topic 发现）。
/// 逐阶段过 TTL / 闸门，成功/失败都落缓存。
async fn refresh_legacy_market_and_github(r: &Refresher, force: bool) {
    let now = network::now_unix();
    let client = &r.http;

    // ---------- 阶段 1：curated（market）----------
    let do_market = force || {
        let c = r.cache.lock().await;
        c.market_stale(now) && !c.market_blocked(now)
    };
    if do_market {
        match super::market::fetch_market(client).await {
            Ok(entries) => {
                let mut c = r.cache.lock().await;
                c.market = entries;
                c.record_success(Stage::Market, network::now_unix());
                let _ = c.save(&r.path);
                tracing::info!(count = c.market.len(), "market 目录刷新成功");
            }
            Err(f) => {
                let mut c = r.cache.lock().await;
                // 失败重试加抖动（对齐 A2A beta.3）：retry_at 至少 now+60s，
                // 再叠一段随机错开，避免重试挤在同一时刻。
                let retry_at = f
                    .retry_at
                    .max(spread(network::now_unix() + 60, RETRY_JITTER_SECS));
                c.record_failure(Stage::Market, f.message, retry_at);
                let _ = c.save(&r.path);
                tracing::warn!(retry_at, "market 刷新失败，进入重试退避");
            }
        }
    }

    // ---------- 阶段 2：tagged（github；依赖 market 条目名做补集）----------
    let now = network::now_unix();
    let (do_github, market_names, ignored, previous) = {
        let c = r.cache.lock().await;
        let names: std::collections::HashSet<String> =
            c.market.iter().map(|e| e.name.to_lowercase()).collect();
        (
            force || (c.github_stale(now) && !c.github_blocked(now)),
            names,
            c.ignored.clone(),
            c.github.clone(),
        )
    };
    if do_github {
        match super::github::fetch_github(client, &market_names, &ignored, &previous).await {
            Ok((entries, new_ignored)) => {
                let mut c = r.cache.lock().await;
                // 新忽略名单合并：旧记录保留（repo 可能本轮没搜到），新的覆盖同 repo
                for ni in new_ignored {
                    c.ignored.retain(|x| x.repo != ni.repo);
                    c.ignored.push(ni);
                }
                if c.ignored.len() > 200 {
                    // 先取长度再 drain：c.ignored.drain(0..c.ignored.len()-N)
                    // 的 range 参数与 drain 的 &mut 借用冲突（E0502）
                    let excess = c.ignored.len() - 200;
                    c.ignored.drain(0..excess);
                }
                c.github = entries;
                c.record_success(Stage::Github, network::now_unix());
                let _ = c.save(&r.path);
                tracing::info!(
                    count = c.github.len(),
                    ignored = c.ignored.len(),
                    "github 发现刷新成功"
                );
            }
            Err(f) => {
                let mut c = r.cache.lock().await;
                let retry_at = f
                    .retry_at
                    .max(spread(network::now_unix() + 60, RETRY_JITTER_SECS));
                c.record_failure(Stage::Github, f.message, retry_at);
                let _ = c.save(&r.path);
                tracing::warn!(retry_at, "github 刷新失败，进入重试退避");
            }
        }
    }
}

/// 阶段 3：stars（独立 TTL；只刷第三方条目）。
async fn refresh_stars(r: &Refresher, force: bool) {
    let now = network::now_unix();
    let client = &r.http;
    let do_stars = force || {
        let c = r.cache.lock().await;
        c.stars_stale(now) && !c.stars_blocked(now)
    };
    if do_stars {
        // 拷出再刷：锁绝不跨网络 await——持锁等 GitHub 会把 wake()/status()
        // 卡住几十秒（管理面「立即刷新」按钮转圈的直接来源）
        let mut working = {
            let c = r.cache.lock().await;
            c.github.clone()
        };
        // 每个客户端在 12~18 小时之间错开刷新（STARS_TTL + noise(6h)），
        // 避免同一时刻一起请求仓库信息。
        let threshold = super::cache::TTL_STARS_SECS + noise(6 * 3_600);
        let (updated, attempted) =
            super::github::refresh_stars(client, &mut working, threshold).await;
        let mut c = r.cache.lock().await;
        c.github = working;
        if attempted > updated {
            // 有仓读取失败：推进 stars 重试闸门（30min + 抖动），保留旧值。
            let retry_at = spread(network::now_unix() + 30 * 60, RETRY_JITTER_SECS);
            c.record_failure(
                Stage::Stars,
                format!("{attempted} 个仓库星数暂时未能刷新"),
                retry_at,
            );
        } else {
            c.record_success(Stage::Stars, network::now_unix());
        }
        let _ = c.save(&r.path);
        tracing::info!(updated, attempted, "stars 刷新完成");
    }
}

/// 合并当前目录（market + github → 排序列表）。
///
/// 从缓存快照合并，不触发刷新；调用方（API 层）按需调 [`super::Catalog::wake`]。
pub async fn snapshot(r: &Refresher) -> Vec<super::types::Entry> {
    let c = r.cache.lock().await;
    merge(&c.market, &c.github)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试辅助：只建 Refresher（加载/建空缓存），不起网络循环。
    async fn spawn_inner_for_test(dir: &std::path::Path) -> Arc<Refresher> {
        // Refresher::new 会建 reqwest Client；测试进程不经 main() 的 provider 安装。
        crate::install_rustls_provider_for_tests();
        let r = Refresher::new(dir);
        // 预写空缓存文件，避免 load 走 default 路径干扰断言
        let _ = r.cache.lock().await.save(&r.path);
        r
    }

    #[tokio::test]
    async fn status_reports_counts_and_staleness() {
        let dir = std::env::temp_dir().join("aigx_refresh_test");
        std::fs::create_dir_all(&dir).unwrap();
        let r = spawn_inner_for_test(&dir).await;
        let s = r.status().await;
        assert!(s.official == 0 && s.recommended == 0 && s.third_party == 0);
        assert!(s.market_stale, "空缓存 market 必然过期");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn snapshot_merges_from_cache() {
        // Entry 是目录内部富条目（catalog::types），不在共享契约 script::types
        // ——路径写错会 unresolved import（E0432），测试也编译不过。
        // 本文件是 catalog::refresh，测试模块在 refresh::tests：
        // `super::super::types` = catalog::types（`types` 是 catalog 私有子模块，
        // 祖先私有项对后代可见，故这条路径合法）。
        use super::super::types::{Entry, ScriptPublisher};
        let dir = std::env::temp_dir().join("aigx_refresh_test2");
        std::fs::create_dir_all(&dir).unwrap();
        let r = spawn_inner_for_test(&dir).await;
        {
            let mut c = r.cache.lock().await;
            c.market.push(Entry {
                name: "@magpie-community/a".into(),
                label: "A".into(),
                publisher: ScriptPublisher::Official,
                ..Default::default()
            });
            c.github.push(Entry {
                name: "x/y".into(),
                label: "X".into(),
                publisher: ScriptPublisher::ThirdParty,
                stars: Some(3),
                ..Default::default()
            });
        }
        let snap = snapshot(&r).await;
        assert_eq!(snap.len(), 2);
        // official 排在第三方之前
        assert_eq!(snap[0].publisher, ScriptPublisher::Official);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn wake_flag_reset_on_cycle_read() {
        let dir = std::env::temp_dir().join("aigx_refresh_test3");
        std::fs::create_dir_all(&dir).unwrap();
        let r = spawn_inner_for_test(&dir).await;
        r.wake(true);
        assert!(r.force_flag.load(std::sync::atomic::Ordering::Relaxed));
        r.force_flag
            .swap(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!r.force_flag.load(std::sync::atomic::Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// CDN 契约：只有 `Ok` 才接管；任何 `Err`（网络 / 坏 schema / 空目录
    /// 保护）都必须回落旧链。
    #[test]
    fn cdn_usable_only_on_ok() {
        let bad = super::super::cdn::parse_catalog(b"not json");
        assert!(!cdn_usable(&bad), "解析失败必须回落旧链");
        let bad_schema = super::super::cdn::parse_catalog(br#"{"schemaVersion":2,"entries":[]}"#);
        assert!(!cdn_usable(&bad_schema), "坏 schema 必须回落旧链");
        let good = super::super::cdn::parse_catalog(
            br#"{"schemaVersion":1,"entries":[
                {"name":"@magpie-community/a","providers":["a"],"source":"magpie:a",
                 "publisher":"official","kind":"provider","providerVerified":true}]}"#,
        );
        assert!(cdn_usable(&good), "合法目录应接管");
    }

    /// 一次成功的 CDN 结果接管 market+github 两桶，清错误、盖时间戳，
    /// 并把上游 warning 落进 market_error。
    #[test]
    fn apply_cdn_takes_over_and_records_success() {
        let mut cache = Cache::empty();
        // 先塞一条旧错误，验证被清 / 被 warning 覆盖
        cache.market_error = Some("old".into());
        let catalog = super::super::cdn::parse_catalog(
            r#"{"schemaVersion":1,"warning":"目录告警","entries":[
                {"name":"@magpie-community/a","providers":["a"],"source":"magpie:a",
                 "publisher":"official","kind":"provider","providerVerified":true}]}"#
                .as_bytes(),
        )
        .expect("合法目录");
        apply_cdn(&mut cache, catalog, 1_700_000_000);
        assert_eq!(cache.market.len(), 1);
        assert_eq!(cache.market_at, 1_700_000_000);
        assert_eq!(cache.github_at, 1_700_000_000);
        assert_eq!(cache.market_retry, 0);
        assert_eq!(cache.github_retry, 0);
        assert_eq!(cache.market_error.as_deref(), Some("目录告警"));
        assert!(cache.github.is_empty() && cache.github_missing);
    }
}
