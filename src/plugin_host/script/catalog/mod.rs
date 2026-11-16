//! Magpie 脚本插件目录 —— 双源拉取 + 三分类 + TTL 缓存的后台任务。
//!
//! ## 对齐范围（只对齐可观察契约，不拷上游 AGPL 代码）
//!
//! 上游 A2A 3.5.0-beta.1 插件页的「Magpie 插件」分组行为：
//! - **三分类**：官方（@magpie-community/*）/ 推荐（registry 其余）/
//!   第三方（GitHub topic 搜到但不在 registry）；
//! - **双源**：策展清单 registry.json（main 分支）+ 分发索引 dist/index.json
//!   （dist 分支）join 出「可安装」条目；dist 没有的走 npm registry 回退；
//! - **TTL 缓存**：market 6h / github 10min / stars 6h（节奏差异的理由见
//!   cache.rs）；catalog.json 落 `{data_dir}/script-plugins/catalog.json`，
//!   version==1，≤16MB；
//! - **排序**：official 0 < recommended 1 < third-party 2，再 stars 降序、
//!   label 升序。
//!
//! 上面的**数据契约**（URL、字段名、分类口径、TTL 数值）是可观察行为；
//! 本目录全部 Rust 代码为 AIGX 独立书写。
//!
//! ## 实测口径（2026-11-15，勿凭想象改）
//!
//! - registry.json 121 条：**50 条有 providers 字段且 kind 缺省**（这才是
//!   进目录的 provider 形态条目）；有 kind 的 71 条（middleware 24 / agent
//!   25 / tools 8 / page 14）**全部没有 providers 字段**，被「providers
//!   非空」规则天然过滤。kind 判定规则仍是「缺省视为 provider、明确非
//!   provider 才排除」——它防的是上游未来给 agent 类条目补 providers
//!   字段时目录把它们误收进来；
//! - **目录实测产出 50 条 = 43 official + 7 recommended**（不是 121 条里
//!   official 判定命中的 102——那 102 里 59 条是 agent/middleware/page/
//!   tools 形态包，providers 字段为空，不进 provider 目录）；
//! - official 判定 = `package.starts_with("@magpie-community/") &&
//!   community==true`。注意 7 条 recommended 里有 2 条 community=true
//!   但非官方 scope（magpie-providers-hub / magpie-plugin-codar），印证
//!   「community 标记 ≠ official」；
//! - dist/index.json 105 包全部是 `@magpie-community/*`，registry 43 个
//!   官方 provider 条目里 43 个全部在 dist 命中；7 条 recommended 不在
//!   dist（npm registry 回退实测 4/4 抽查可达）。dist 顶层
//!   `{packages: {name: {versions: {v: {file, integrity, time, manifest}}}}}`，
//!   全部带 latest 字段；dist 包的 magpie 键是 {icon, options, page, …}
//!   等自报元数据，**没有 provider 子键**——provider 声明在 registry 的
//!   providers 数组，不在包 manifest 里；
//! - dist/index.json 实测 3.5MB，typed struct 解析防无界展开。
//!
//! ## 模块布局
//!
//! - [`types`] —— 富条目 Entry / 前端投影 PublicEntry / merge 排序
//! - [`cache`] —— catalog.json 缓存 + 三阶段 TTL / 重试闸门
//! - [`network`] —— 统一超时/限流/重试语义 + 有界并发工具
//! - [`market`] —— registry.json + dist join + npm 回退
//! - [`github`] —— topic 搜索 + 静态验证（不执行代码）
//! - [`provider`] —— 入口源码 tokenizer 静态扫描 auth.provider
//! - [`refresh`] —— Notify + 60s tick 后台任务（三阶段流水线）
//!
//! ## 对外 API（[`Catalog`]）
//!
//! - [`Catalog::spawn`] —— 网关启动路径：建 Refresher 并启动后台循环；
//! - [`Catalog::wake`] —— 手动刷新（force 无视 TTL，不动限流闸门）；
//! - [`Catalog::status`] —— 阶段时间戳/错误/三分类计数（管理面展示）；
//! - [`Catalog::recommendations`] —— 合并排序列表（可投影成契约条目或
//!   前端 camelCase JSON）；
//! - [`Catalog::package_metadata`] —— 已装包名 → 目录条目回映射（安装页
//!   「已安装」徽标 / 版本提示）；
//! - [`Catalog::catalog_path`] —— 缓存文件绝对路径（runtime 塞进
//!   ScriptInitPayload.catalog_path 交给 js-host）。

mod cache;
mod github;
mod market;
mod network;
// pub(crate)：安装链路（operations.rs）在 npm/github/local 来源安装后复用
// 入口静态扫描回填 provider 声明（不执行代码，只读源码文本）。
pub(crate) mod provider;
mod refresh;
mod types;

// 私有模块的产出类型通过 re-export 变得可命名（否则 pub 方法返回它们
// 会触发 E0446「private type in public interface」）。IgnoredRepo 直接从
// types.rs（定义处，pub struct）转出——不能经 cache.rs 转出：那里的
// pub(crate) use 会把可见性降到 crate 级，pub use 再转出是 E0365。
pub use refresh::Status;
pub use types::{Entry, IgnoredRepo, PublicEntry};

use std::sync::Arc;

use crate::plugin_host::script::types::{ScriptCatalogEntry, ScriptPublisher};
use refresh::Refresher;

/// 目录门面：包住 Refresher，给 runtime / API 层一个稳定的调用面。
pub struct Catalog {
    inner: Arc<Refresher>,
}
impl Catalog {
    /// 创建目录并启动后台刷新（网关启动时调用一次）。
    ///
    /// `script_root` 传 `script_plugins_root(data_dir)`；缓存落其下的
    /// `catalog.json`。
    pub fn spawn(script_root: &std::path::Path) -> Arc<Catalog> {
        Arc::new(Catalog {
            inner: Refresher::spawn(script_root),
        })
    }

    /// 手动唤醒一轮刷新。force=true 无视 TTL（限流闸门仍然生效）。
    pub fn wake(&self, force: bool) {
        self.inner.wake(force);
    }

    /// 目录状态快照（阶段时间戳 / 错误 / 三分类计数）。
    pub async fn status(&self) -> Status {
        self.inner.status().await
    }

    /// 合并排序后的目录条目（market 优先去重 → official < recommended
    /// < third-party → stars 降序 → label 升序）。
    pub async fn recommendations(&self) -> Vec<Entry> {
        refresh::snapshot(&self.inner).await
    }

    /// 目录条目 → 下游共享契约投影（operations 的 install(magpie) 消费）。
    pub async fn catalog_entries(&self) -> Vec<ScriptCatalogEntry> {
        refresh::snapshot(&self.inner)
            .await
            .iter()
            .map(Entry::to_catalog_entry)
            .collect()
    }

    /// 前端投影列表（camelCase JSON，含 magpieOfficial / magpieRecommended）。
    pub async fn public_entries(&self) -> Vec<PublicEntry> {
        refresh::snapshot(&self.inner)
            .await
            .iter()
            .map(Entry::public)
            .collect()
    }

    /// 已装包回映射：包名（不区分大小写）→ 目录条目。
    ///
    /// 用途：管理面插件页对已安装包打「已安装」徽标、显示可升级版本
    /// （Entry.version 与已装 ScriptPackage.version 比较）。没有目录信息
    /// 的包（如本地直装）不在返回里——调用方按 None 处理即可。
    pub async fn package_metadata(&self, pkg: &str) -> Option<Entry> {
        let lower = pkg.to_lowercase();
        refresh::snapshot(&self.inner)
            .await
            .into_iter()
            .find(|e| e.name.to_lowercase() == lower)
    }

    /// 缓存文件绝对路径（runtime 塞进 ScriptInitPayload.catalog_path）。
    pub fn catalog_path(&self) -> &std::path::Path {
        &self.inner.path
    }

    /// 官方/推荐计数（status 的快捷读取，测试与日志用）。
    pub async fn publisher_counts(&self) -> (usize, usize, usize) {
        let s = self.status().await;
        (s.official, s.recommended, s.third_party)
    }
}

/// 三分类排序权重暴露（API 层做自定义排序时用；与 ScriptPublisher::rank 一致）。
pub fn publisher_rank(p: &ScriptPublisher) -> u8 {
    p.rank()
}

/// 忽略名单查询（管理面「为什么没有某个仓」的解释接口）。
pub async fn ignored_repos(catalog: &Catalog) -> Vec<IgnoredRepo> {
    catalog.inner.cache.lock().await.ignored.clone()
}

/// 便捷入口：从全局 data_dir 建目录（main.rs 启动路径）。
///
/// data_dir 由 `crate::account_session::secret::data_dir()` 提供。
pub fn spawn_from_data_dir() -> Arc<Catalog> {
    let root = crate::plugin_host::script::types::script_plugins_root(
        &crate::account_session::secret::data_dir(),
    );
    Catalog::spawn(&root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::script::types::{ScriptPublisher, ScriptSource};

    #[tokio::test]
    async fn catalog_api_shapes_without_network() {
        let dir = std::env::temp_dir().join("aigx_catalog_mod_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 不起网络循环（Refresher::new 只加载磁盘缓存）；
        // 网络路径由 CI 的集成测试覆盖，这里验证 API 形状。
        crate::install_rustls_provider_for_tests();
        let r = Refresher::new(&dir);
        let cat = Catalog { inner: r };
        let s = cat.status().await;
        // 空缓存：三分类计数为 0，market 必然过期
        assert_eq!(s.official + s.recommended + s.third_party, 0);
        assert!(s.market_stale);
        assert!(cat.catalog_path().ends_with("catalog.json"));
        assert!(cat.recommendations().await.is_empty());
        assert!(cat.public_entries().await.is_empty());
        assert!(cat.catalog_entries().await.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn package_metadata_case_insensitive() {
        let dir = std::env::temp_dir().join("aigx_catalog_mod_test2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::install_rustls_provider_for_tests();
        let r = Refresher::new(&dir);
        {
            let mut c = r.cache.lock().await;
            c.market.push(Entry {
                name: "@magpie-community/Kiro".into(),
                label: "Kiro".into(),
                version: Some("1.0.0".into()),
                publisher: ScriptPublisher::Official,
                source: ScriptSource::Magpie,
                ..Default::default()
            });
        }
        // 直接用 inner Refresher 验证回映射（不起网络）
        let cat = Catalog { inner: r };
        let m = cat.package_metadata("@magpie-community/kiro").await;
        assert!(m.is_some());
        assert_eq!(m.unwrap().version.as_deref(), Some("1.0.0"));
        assert!(cat
            .package_metadata("@magpie-community/none")
            .await
            .is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
