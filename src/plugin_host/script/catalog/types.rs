//! Magpie 目录内部类型 —— 富条目 [`Entry`]、前端投影 [`PublicEntry`]、忽略仓 [`IgnoredRepo`]。
//!
//! ## 与共享契约（script-core 维护的 script/types.rs）的关系
//!
//! `crate::plugin_host::script::types::ScriptCatalogEntry` 是**下游契约**：
//! 安装链路（operations / pkg-installer）与 API 层按它消费目录。本模块的
//! [`Entry`] 是目录**内部**的富条目 —— 在契约字段之外多出 version / kind /
//! selector / stars_at / icon_sources / installable / unavailable_reason /
//! pushed_at / manifest_at / provider_verified 等目录特有字段，用于：
//!
//! - 缓存落盘（cache.rs 的 catalog.json 直接序列化 Entry）；
//! - 排序合并（[`merge`]：official 0 < recommended 1 < third-party 2，
//!   再 stars 降序、label 升序）；
//! - 前端展示（[`Entry::public`] → camelCase 的 [`PublicEntry`]，带
//!   `magpieOfficial` / `magpieRecommended` 两个布尔，前端分组徽标直接用）。
//!
//! 投影全部**单向只读**：Entry 是唯一事实，`to_catalog_entry()` / `public()`
//! 不回写缓存；契约缺字段时在目录内部保留完整信息，需要透出再谈契约。
//!
//! ## source 通道语义（为什么 official 与 recommended 的 source 不同）
//!
//! 实测（2026-11-15，registry.json 121 条 / dist index.json 105 包）：
//! 官方包 `@magpie-community/*` 走 magpie 社区 dist 分发（tgz 带 sha512
//! integrity，可校验）→ `ScriptSource::Magpie`；其余 registry 条目（7 条
//! recommended）是普通 npm 发布，dist 里没有 → `ScriptSource::Npm`
//! （裸包名直装）。GitHub 发现的仓：有同名 npm 包且 repo 匹配且有入口 →
//! Npm，否则 Github。
//! [`Entry::selector`] 是给 pkg-installer 的完整安装指示串
//! （`magpie:{name}` / 裸包名 / `github:{owner/repo}`）。

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

// pub(crate)：sibling 模块（github/market/refresh）经 `super::types::X`
// 引用这三个契约类型——私有 use 转手的名字对 sibling 不可见（E0603）。
pub(crate) use crate::plugin_host::script::types::{
    ScriptCatalogEntry, ScriptPublisher, ScriptSource,
};

/// 官方包的 npm scope 前缀（实测 registry 121 条中 102 条命中该前缀且
/// community=true；其中 provider 形态 43 条进目录）。
pub const OFFICIAL_SCOPE: &str = "@magpie-community/";

/// 官方判定：包名以 `@magpie-community/` 开头 **且** community=true。
///
/// 两个条件缺一不可：实测 7 条 recommended 里有 2 条 community=true 但
/// 非官方 scope（magpie-providers-hub / magpie-plugin-codar），也有官方
/// 前缀但 community 缺省的条目——它们都不是 official。
pub fn is_official(name: &str, community: bool) -> bool {
    community && name.starts_with(OFFICIAL_SCOPE)
}

/// 多语言摘要挑选：zh 优先（AIGX 管理面中文为主）→ en → 任意第一个。
pub fn pick_summary(summary: &BTreeMap<String, String>) -> Option<String> {
    if let Some(zh) = summary.get("zh") {
        if !zh.is_empty() {
            return Some(zh.clone());
        }
    }
    if let Some(en) = summary.get("en") {
        if !en.is_empty() {
            return Some(en.clone());
        }
    }
    summary.values().next().cloned()
}

/// 目录内部富条目（缓存与合并的事实载体）。
///
/// 字段语义见模块注释；`installable=false` 时 `unavailable_reason` 必填
/// （前端据此渲染「不可安装」态而不是隐藏条目）。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Entry {
    /// npm 包名（market 条目）或 GitHub 仓全名（无 npm 包的第三方发现）。
    pub name: String,
    /// 展示名（registry `name` / package.json description / 仓 description）。
    pub label: String,
    /// 版本（dist latest / npm dist-tags.latest / 仓 package.json version）。
    pub version: Option<String>,
    /// 包声明覆盖的 provider id（registry providers 数组或扫描到的 auth.provider）。
    pub providers: Vec<String>,
    /// 来源通道（magpie / npm / github），见模块注释「source 通道语义」。
    pub source: ScriptSource,
    /// 安装指示串（pkg-installer 消费）：`magpie:{name}` / 裸包名 / `github:{repo}`。
    pub selector: String,
    /// 发布者三分类（official / recommended / third-party）。
    pub publisher: ScriptPublisher,
    /// 多语言摘要（registry summary 的 {en, zh} 原样保留）。
    pub summary: BTreeMap<String, String>,
    /// 仓库归属（`owner/repo` 简写，stars 刷新与去重键）。
    pub repository: Option<String>,
    /// 仓库完整 URL（dist manifest repository.url / GitHub html_url）。
    pub repository_url: Option<String>,
    /// Star 数（GitHub）。
    pub stars: Option<u64>,
    /// Star 数抓取时刻（unix 秒；stars 阶段刷新）。
    pub stars_at: Option<i64>,
    /// 图标候选 URL（registry icon / 仓 owner avatar，前端按序回退）。
    pub icon_sources: Vec<String>,
    /// registry 条目的 kind 字段（实测多数缺省；缺省视为 provider，保留 None）。
    pub kind: Option<String>,
    /// dist 分发的 tgz 下载 URL（pkg-installer 权威下载源；官方包在 npm
    /// 大多 404，2026-11-15 实测）。npm 回退 / GitHub 发现条目为 None。
    pub archive_url: Option<String>,
    /// dist index 声明的 tgz SRI（`sha512-…` base64；pkg-installer 校验用，
    /// 与 npm 的 dist.integrity 同形态）。与 archive_url 配对出现。
    pub integrity: Option<String>,
    /// 是否可安装（dist/npm 命中且入口可解析）。
    pub installable: bool,
    /// 不可安装原因（installable=false 时必填）。
    pub unavailable_reason: Option<String>,
    /// 仓最近 push 时刻（unix 秒；GitHub 发现条目专用）。
    pub pushed_at: Option<i64>,
    /// 包清单发布时刻（unix 秒；dist time / npm time[latest]）。
    pub manifest_at: Option<i64>,
    /// 入口静态扫描是否验证到 auth.provider 证据（None=未扫描，策展条目默认不扫）。
    pub provider_verified: Option<bool>,
    /// 目录条目对应的上游提交 SHA（CDN 目录 `commitSha`）。
    ///
    /// 仅 CDN 聚合目录（beta.4 `Entry.commit_sha`）会带；旧链
    /// registry/dist/GitHub 发现条目为 None。用途：已装 GitHub 插件的
    /// 「有没有新提交」快速判定（`Catalog::github_commit`）——不必再打
    /// GitHub API 就能和目录里的 commit 比对。
    ///
    /// `#[serde(default)]` 让旧 `catalog.json` / 旧缓存条目（无此键）也能
    /// 反序列化成 None，见模块「兼容旧缓存读取」。
    pub commit_sha: Option<String>,
}

impl Entry {
    /// 投影成下游共享契约 [`ScriptCatalogEntry`]（安装链路 / API 消费）。
    ///
    /// source_ref 口径与 `ScriptPackage::source_ref` 一致：
    /// magpie/npm → 裸包名；github → `owner/repo`（未钉 ref，装默认分支）。
    pub fn to_catalog_entry(&self) -> ScriptCatalogEntry {
        let source_ref = match self.source {
            ScriptSource::Magpie | ScriptSource::Npm => self.name.clone(),
            ScriptSource::Github => self.repository.clone().unwrap_or_else(|| self.name.clone()),
            ScriptSource::Local => self.name.clone(),
        };
        ScriptCatalogEntry {
            name: self.name.clone(),
            label: self.label.clone(),
            providers: self.providers.clone(),
            publisher: self.publisher.clone(),
            source: self.source.clone(),
            source_ref,
            summary: pick_summary(&self.summary),
            repository: self.repository.clone(),
            stars: self.stars,
            archive_url: self.archive_url.clone(),
            integrity: self.integrity.clone(),
        }
    }

    /// 投影成前端 JSON（camelCase 字段，含 magpieOfficial / magpieRecommended）。
    pub fn public(&self) -> PublicEntry {
        PublicEntry {
            name: self.name.clone(),
            label: self.label.clone(),
            version: self.version.clone(),
            providers: self.providers.clone(),
            kind: self.kind.clone(),
            publisher: self.publisher.as_str().to_string(),
            magpie_official: self.publisher == ScriptPublisher::Official,
            magpie_recommended: self.publisher == ScriptPublisher::Recommended,
            source: self.source.as_str().to_string(),
            selector: self.selector.clone(),
            summary: pick_summary(&self.summary),
            repository: self.repository.clone(),
            repository_url: self.repository_url.clone(),
            stars: self.stars,
            stars_at: self.stars_at,
            icon_sources: self.icon_sources.clone(),
            installable: self.installable,
            unavailable_reason: self.unavailable_reason.clone(),
            archive_url: self.archive_url.clone(),
            integrity: self.integrity.clone(),
            pushed_at: self.pushed_at,
            manifest_at: self.manifest_at,
            provider_verified: self.provider_verified,
        }
    }

    /// 官方条目构造辅助：source/selector/installable 按官方通道（magpie dist）。
    pub fn official_magpie(name: &str, label: &str, providers: Vec<String>) -> Self {
        Entry {
            name: name.to_string(),
            label: label.to_string(),
            providers,
            source: ScriptSource::Magpie,
            selector: format!("magpie:{name}"),
            publisher: ScriptPublisher::Official,
            installable: true,
            ..Default::default()
        }
    }

    /// 推荐条目构造辅助：source/selector 按推荐通道（npm registry，裸包名）。
    pub fn recommended_npm(name: &str, label: &str, providers: Vec<String>) -> Self {
        Entry {
            name: name.to_string(),
            label: label.to_string(),
            providers,
            source: ScriptSource::Npm,
            selector: name.to_string(),
            publisher: ScriptPublisher::Recommended,
            installable: true,
            ..Default::default()
        }
    }
}

/// 双源合并 + 三分类排序。
///
/// market（registry 策展）优先：同名包 GitHub 发现条目让位（策展信息更全，
/// 且 GitHub 侧只对不在 registry 的仓做第三方补充——这是「第三方 = 搜到但
/// 不在 registry」定义的直接实现）。排序：publisher rank 升序 → stars 降序
/// → label 升序（同 label 再按 name 保证确定性）。
pub fn merge(market: &[Entry], github: &[Entry]) -> Vec<Entry> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<Entry> = Vec::with_capacity(market.len() + github.len());
    for e in market {
        if seen.insert(e.name.to_lowercase()) {
            out.push(e.clone());
        }
    }
    for e in github {
        if seen.insert(e.name.to_lowercase()) {
            out.push(e.clone());
        }
    }
    out.sort_by(|a, b| {
        a.publisher
            .rank()
            .cmp(&b.publisher.rank())
            .then_with(|| b.stars.unwrap_or(0).cmp(&a.stars.unwrap_or(0)))
            .then_with(|| a.label.cmp(&b.label).then_with(|| a.name.cmp(&b.name)))
    });
    out
}

/// 前端投影（camelCase JSON；分组徽标用 magpieOfficial / magpieRecommended）。
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PublicEntry {
    pub name: String,
    pub label: String,
    pub version: Option<String>,
    pub providers: Vec<String>,
    pub kind: Option<String>,
    /// `official` / `recommended` / `third-party`（前端 badge 字符串）。
    pub publisher: String,
    pub magpie_official: bool,
    pub magpie_recommended: bool,
    /// `magpie` / `npm` / `github`。
    pub source: String,
    /// 安装指示串（前端发起安装时原样回传）。
    pub selector: String,
    pub summary: Option<String>,
    pub repository: Option<String>,
    pub repository_url: Option<String>,
    pub stars: Option<u64>,
    pub stars_at: Option<i64>,
    pub icon_sources: Vec<String>,
    pub installable: bool,
    pub unavailable_reason: Option<String>,
    /// dist tgz 下载 URL（官方包权威下载源；npm/GitHub 条目为 None）。
    pub archive_url: Option<String>,
    /// dist 声明的 SRI（`sha512-…`），与 archive_url 配对；安装校验用。
    pub integrity: Option<String>,
    pub pushed_at: Option<i64>,
    pub manifest_at: Option<i64>,
    pub provider_verified: Option<bool>,
}

/// 被忽略的 GitHub 仓（验证失败或暂缓验证），防每轮反复深扫。
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(default)]
pub struct IgnoredRepo {
    /// 仓全名（`owner/repo`）。
    pub repo: String,
    /// 忽略原因（管理面可解释为什么第三方列表里没有它）。
    pub reason: String,
    /// 忽略判定时刻（unix 秒）。
    pub at: i64,
    /// 忽略时刻捕获的 pushed_at（unix 秒）；仓重新 push 即退出忽略。
    pub pushed_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn market_entry(name: &str, label: &str, stars: Option<u64>, official: bool) -> Entry {
        Entry {
            name: name.into(),
            label: label.into(),
            stars,
            publisher: if official {
                ScriptPublisher::Official
            } else {
                ScriptPublisher::Recommended
            },
            ..Default::default()
        }
    }

    #[test]
    fn official_rule_requires_scope_and_flag() {
        assert!(is_official("@magpie-community/kiro", true));
        // community=true 但非官方 scope → 推荐而非官方（实测 @openviking 的情况）
        assert!(!is_official("@openviking/magpie-plugin", true));
        assert!(!is_official("@magpie-community/kiro", false));
    }

    #[test]
    fn summary_prefers_zh_then_en_then_any() {
        let mut m = BTreeMap::new();
        m.insert("en".into(), "english".into());
        assert_eq!(pick_summary(&m).as_deref(), Some("english"));
        m.insert("zh".into(), "中文".into());
        assert_eq!(pick_summary(&m).as_deref(), Some("中文"));
    }

    #[test]
    fn merge_orders_publisher_stars_label_and_dedups() {
        let market = vec![
            market_entry("b-pkg", "Beta", Some(10), false),
            market_entry("a-pkg", "Alpha", None, true),
        ];
        let github = vec![
            // 与 market 同名 → 让位
            Entry {
                name: "a-pkg".into(),
                publisher: ScriptPublisher::ThirdParty,
                stars: Some(999),
                ..Default::default()
            },
            Entry {
                name: "c-repo".into(),
                label: "Gamma".into(),
                publisher: ScriptPublisher::ThirdParty,
                stars: Some(5),
                ..Default::default()
            },
            Entry {
                name: "d-repo".into(),
                label: "Gamma".into(),
                publisher: ScriptPublisher::ThirdParty,
                stars: Some(50),
                ..Default::default()
            },
        ];
        let merged = merge(&market, &github);
        let names: Vec<&str> = merged.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a-pkg", "b-pkg", "d-repo", "c-repo"]);
        // 顺序解释：rank 优先——official(a-pkg,0) < recommended(b-pkg,1)
        // < third-party(d-repo 50 > c-repo 5，stars 降序；同 stars 才看 label)。
        // a-pkg 的 GitHub 重名条目被 market 优先去重丢弃（999 stars 不参与）。
    }

    #[test]
    fn public_entry_has_magpie_flags_and_camel_case() {
        let e = market_entry("@magpie-community/x", "X", None, true);
        let v = serde_json::to_value(e.public()).unwrap();
        assert_eq!(v["magpieOfficial"], serde_json::json!(true));
        assert_eq!(v["magpieRecommended"], serde_json::json!(false));
        assert_eq!(v["publisher"], "official");
        assert_eq!(v["installable"], serde_json::json!(false));
        assert!(v.get("unavailableReason").is_some());
        assert!(v.get("providerVerified").is_some());
    }

    #[test]
    fn to_catalog_entry_maps_source_ref() {
        let mut e = Entry::official_magpie("@magpie-community/kiro", "Kiro", vec!["kiro".into()]);
        e.summary.insert("zh".into(), "Kiro 接入".into());
        let c = e.to_catalog_entry();
        assert_eq!(c.source_ref, "@magpie-community/kiro");
        assert_eq!(c.summary.as_deref(), Some("Kiro 接入"));
        assert_eq!(c.source, ScriptSource::Magpie);

        let g = Entry {
            name: "some-repo".into(),
            repository: Some("owner/some-repo".into()),
            source: ScriptSource::Github,
            ..Default::default()
        };
        assert_eq!(g.to_catalog_entry().source_ref, "owner/some-repo");
    }
}
