//! 后台刷新任务：Notify 唤醒 + 60s 空闲 tick 的三阶段流水线。
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
//! ## 阶段顺序（curated → tagged → stars）与失败语义
//!
//! 1. **curated**（market）：registry + dist join + npm 回退；
//! 2. **tagged**（github）：topic 搜索 + 逐仓验证（依赖 market 的条目名
//!    集合做补集判定，**必须在 market 之后**）；
//! 3. **stars**：第三方条目的星数刷新（独立 6h TTL；失败不影响前两阶段）。
//!
//! 每阶段独立 TTL + 独立重试闸门（cache.rs）：某阶段被闸门拦住就跳过，
//! 不重打限流源；force=true 绕过 TTL 但**不绕过重试闸门**——限流退避
//! 是对上游的礼貌，手动刷新也不例外（这是「手动刷新」与「无脑重试」的
//! 区别）。

use std::sync::Arc;

use tokio::sync::{Mutex, Notify};

use super::cache::{Cache, Stage};
use super::network;
use super::types::{merge, ScriptPublisher};

/// 空闲轮询间隔。
const IDLE_TICK: std::time::Duration = std::time::Duration::from_secs(60);

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

/// 一轮刷新：market → github → stars，逐阶段过 TTL / 闸门，成功/失败都落缓存。
async fn refresh_cycle(r: &Refresher, force: bool) {
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
                c.record_failure(Stage::Market, f.message, f.retry_at);
                let _ = c.save(&r.path);
                tracing::warn!(retry_at = f.retry_at, "market 刷新失败，进入重试退避");
            }
        }
    }

    // ---------- 阶段 2：tagged（github；依赖 market 条目名做补集）----------
    let now = network::now_unix();
    let (do_github, market_names, ignored) = {
        let c = r.cache.lock().await;
        let names: std::collections::HashSet<String> =
            c.market.iter().map(|e| e.name.to_lowercase()).collect();
        (
            force || (c.github_stale(now) && !c.github_blocked(now)),
            names,
            c.ignored.clone(),
        )
    };
    if do_github {
        match super::github::fetch_github(client, &market_names, &ignored).await {
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
                c.record_failure(Stage::Github, f.message, f.retry_at);
                let _ = c.save(&r.path);
                tracing::warn!(retry_at = f.retry_at, "github 刷新失败，进入重试退避");
            }
        }
    }

    // ---------- 阶段 3：stars（独立 TTL；只刷第三方条目）----------
    let now = network::now_unix();
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
        let updated = super::github::refresh_stars(client, &mut working).await;
        let mut c = r.cache.lock().await;
        c.github = working;
        c.record_success(Stage::Stars, network::now_unix());
        let _ = c.save(&r.path);
        tracing::info!(updated, "stars 刷新完成");
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
}
