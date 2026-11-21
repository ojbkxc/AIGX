//! 消费 A2A beta.4 的官网 CDN 聚合目录（`website::CATALOG`）。
//!
//! ## 与旧三阶段的关系（CDN 优先、旧链回落）
//!
//! beta.4 把目录发现从「AIGX 自带的 registry + dist join + GitHub topic
//! 搜索」换成了「拉官网聚合好的 CDN 目录」。AIGX 保留旧链作回落：本模块
//! 的 [`fetch`] 成功即用 CDN 结果；返回 `Err` 时 [`super::refresh`] 会走
//! 旧三阶段（[`super::market`] / [`super::github`]）。因此解析层的错误
//! **必须显式变成 `Err`**，不能「解析失败当空目录」——那会让刷新层误以为
//! CDN 成功而丢掉旧链兜底。
//!
//! ## 线上格式（beta.4 `catalog/refresh.rs:127-165`）
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "warning": "可选，上游生成目录时的告警",
//!   "entries": [
//!     { "name": "…", "label": "…", "version": "…", "providers": ["…"],
//!       "source": "magpie:…|裸包名|github:owner/repo", "selector": "版本/ref",
//!       "publisher": "official|recommended|third-party",
//!       "summary": {"zh": "…"}, "repository": "owner/repo",
//!       "repositoryUrl": "…", "stars": 0, "starsAt": 0,
//!       "iconSources": ["…"], "kind": "provider",
//!       "installable": true, "unavailableReason": null,
//!       "pushedAt": "2026-…", "manifestAt": 0, "providerVerified": true,
//!       "commitSha": "40 位 hex 或 null" }
//!   ]
//! }
//! ```
//!
//! 字段是 **camelCase**（beta.4 `#[serde(rename_all = "camelCase")]`），与
//! AIGX 缓存里的 [`Entry`]（snake_case）不同，故用独立 wire 结构
//! [`WireEntry`] 承接、再逐字段映射——**不**把 camelCase rename 加到
//! [`Entry`] 上，否则会破坏旧缓存读取（见 types.rs 注释）。
//!
//! ## 过滤与分组（对齐 beta.4 `refresh.rs:151-165`）
//!
//! - **`is_provider` 过滤**：`kind` 为 provider 且（`providerVerified` 或
//!   `publisher∈{official,recommended}` 且 providers 非空）。AIGX 额外容忍
//!   `kind` 缺省（旧 registry 口径），显式非 provider 才排除——见
//!   [`WireEntry::is_provider`]。
//! - **按 `publisher=="third-party"` 分组**：第三方进 `github` 列表（对应
//!   旧链「GitHub topic 发现」桶，前端三分类里排最后），其余进 `market`。
//!
//! ## 空目录保护（「不能比现在更差」）
//!
//! 若 `entries` 非空但**全部被过滤掉**，几乎只可能是上游改了字段语义
//! （例如 `providerVerified` / `kind` 全缺）。此时返回 `Err` 让刷新层回落
//! 旧链，而不是把现有条目清空。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::network::{self, Failure};
use super::types::{Entry, ScriptPublisher, ScriptSource};
use super::website;

/// 聚合目录的大小上限（实测目录远小于 16MB；与分发索引同上限）。
const CATALOG_LIMIT: usize = 16 << 20;

/// CDN 目录的一次拉取结果（已按 publisher 分好桶）。
#[derive(Debug, Default)]
pub struct Catalog {
    /// 非第三方（official + recommended）。
    pub market: Vec<Entry>,
    /// 第三方（publisher=="third-party"）。
    pub github: Vec<Entry>,
    /// 上游生成目录时的告警（写进 market_error，供管理面展示）。
    pub warning: Option<String>,
}

/// 拉取官网 CDN 聚合目录（CDN → legacy 回落）。
pub async fn fetch(client: &reqwest::Client) -> Result<Catalog, Failure> {
    let bytes = website::bytes_with_fallback(
        client,
        website::CATALOG,
        website::LEGACY_CATALOG,
        CATALOG_LIMIT,
    )
    .await?;
    parse_catalog(&bytes)
}

/// 纯解析（无网络）：便于单测合法/坏 schema/过滤/分组/installable。
pub fn parse_catalog(bytes: &[u8]) -> Result<Catalog, Failure> {
    let doc: WireCatalog = serde_json::from_slice(bytes)
        .map_err(|e| Failure::delayed(format!("CDN 目录解析失败: {e}"), 600))?;
    if doc.schema_version != 1 {
        return Err(Failure::delayed(
            format!(
                "CDN 目录 schemaVersion={} 不是 1，格式不兼容",
                doc.schema_version
            ),
            3600,
        ));
    }
    let Some(raw_entries) = doc.entries else {
        return Err(Failure::delayed("CDN 目录缺少 entries 列表", 600));
    };

    let raw_count = raw_entries.len();
    let mut catalog = Catalog {
        warning: doc
            .warning
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty()),
        ..Catalog::default()
    };
    for raw in raw_entries {
        if !raw.is_provider() {
            continue;
        }
        let third_party = raw.publisher.trim() == "third-party";
        let entry = raw.into_entry();
        if third_party {
            catalog.github.push(entry);
        } else {
            catalog.market.push(entry);
        }
    }

    // 空目录保护：有条目但全被过滤 → 视为格式不兼容，回落旧链。
    if raw_count > 0 && catalog.market.is_empty() && catalog.github.is_empty() {
        return Err(Failure::delayed(
            "CDN 目录条目全部未通过 provider 过滤（疑似上游字段变化）",
            600,
        ));
    }
    Ok(catalog)
}

/// 线上聚合目录顶层（camelCase）。
#[derive(Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
struct WireCatalog {
    schema_version: u64,
    entries: Option<Vec<WireEntry>>,
    warning: Option<String>,
}

/// 线上聚合目录的一条条目（与 beta.4 `catalog/types.rs` `Entry` 同形）。
#[derive(Deserialize, Debug, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct WireEntry {
    name: String,
    label: String,
    version: String,
    providers: Vec<String>,
    source: String,
    #[allow(dead_code)] // beta.4 的版本/ref 选择器；AIGX 安装靠 source 前缀+dist，暂不消费
    selector: String,
    publisher: String,
    summary: BTreeMap<String, String>,
    repository: Option<String>,
    repository_url: Option<String>,
    stars: Option<u64>,
    stars_at: i64,
    icon_sources: Vec<String>,
    kind: String,
    installable: bool,
    unavailable_reason: Option<String>,
    pushed_at: String,
    manifest_at: i64,
    provider_verified: bool,
    commit_sha: Option<String>,
}

impl WireEntry {
    /// provider 过滤（对齐 beta.4 `Entry::is_provider`）。
    ///
    /// 与上游的差异只有一处：**`kind` 缺省视为 provider**（AIGX 旧 registry
    /// 口径），显式非 `provider` 才排除。上游若真用 `kind="provider"` 显式
    /// 标注，行为与 beta.4 完全一致；若上游某天漏标，AIGX 也不会把整张
    /// 目录丢空（空目录保护仍兜底）。
    fn is_provider(&self) -> bool {
        let kind = self.kind.trim();
        let kind_ok = kind.is_empty() || kind == "provider";
        let curated = matches!(self.publisher.trim(), "official" | "recommended");
        kind_ok && (self.provider_verified || (curated && !self.providers.is_empty()))
    }

    /// 映射成 AIGX 缓存条目（snake_case [`Entry`]）。
    fn into_entry(self) -> Entry {
        let source = classify_source(&self.source, &self.publisher, self.repository.as_deref());
        let publisher = classify_publisher(&self.publisher);
        let selector = build_selector(
            &source,
            &self.name,
            self.repository.as_deref(),
            self.commit_sha.as_deref(),
        );
        Entry {
            name: self.name,
            label: self.label,
            version: nonempty(self.version),
            providers: self.providers,
            source,
            selector,
            publisher,
            summary: self.summary,
            repository: self.repository,
            repository_url: self.repository_url,
            stars: self.stars,
            stars_at: positive(self.stars_at),
            icon_sources: self.icon_sources,
            kind: nonempty(self.kind),
            // 分发信息（archive_url / integrity）不在聚合目录里，由
            // distribution.rs 用分发索引补齐；这里先留空。
            archive_url: None,
            integrity: None,
            installable: self.installable,
            unavailable_reason: self.unavailable_reason,
            pushed_at: parse_time(&self.pushed_at),
            manifest_at: positive(self.manifest_at),
            provider_verified: Some(self.provider_verified),
            commit_sha: self
                .commit_sha
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }
}

/// CDN `source`（安装指示串）→ AIGX 来源枚举。
///
/// beta.4 的 `source` 是喂给 `InstallSource::parse(input, selector)` 的
/// 输入：official 走 `magpie:<name>`，recommended/npm 走裸包名，第三方
/// 走 `github:owner/repo`。`source` 为空时按 publisher 兜底推断。
fn classify_source(source: &str, publisher: &str, repository: Option<&str>) -> ScriptSource {
    let s = source.trim();
    if s.starts_with("magpie:") {
        ScriptSource::Magpie
    } else if s.starts_with("github:") || s.starts_with("https://github.com") {
        ScriptSource::Github
    } else if s.starts_with("file:") {
        ScriptSource::Local
    } else if !s.is_empty() {
        ScriptSource::Npm
    } else {
        match publisher.trim() {
            "official" => ScriptSource::Magpie,
            "third-party" if repository.is_some() => ScriptSource::Github,
            _ => ScriptSource::Npm,
        }
    }
}

/// CDN `publisher` 字符串 → AIGX 三分类。
fn classify_publisher(publisher: &str) -> ScriptPublisher {
    match publisher.trim() {
        "official" => ScriptPublisher::Official,
        "recommended" => ScriptPublisher::Recommended,
        _ => ScriptPublisher::ThirdParty,
    }
}

/// 生成 AIGX 风格的安装指示串（前端原样回传给 install）。
///
/// 口径与 `market.rs` 的 `official_magpie` / `recommended_npm`、
/// `github.rs` 的 `github:{repo}` 一致。**GitHub 条目额外带上 `commit_sha`**
/// （`github:owner/repo#<sha>`）：安装会钉到该提交（`source::parse` 支持
/// `#ref`），从而 `source_ref` 记下完整 SHA，`check_updates` 才能拿目录里的
/// `commit_sha` 做「有没有新提交」的比对（见 operations::github_update）。
fn build_selector(
    source: &ScriptSource,
    name: &str,
    repository: Option<&str>,
    commit_sha: Option<&str>,
) -> String {
    match source {
        ScriptSource::Magpie => format!("magpie:{name}"),
        ScriptSource::Npm => name.to_string(),
        ScriptSource::Github => {
            let repo = repository.unwrap_or(name);
            match commit_sha.map(str::trim) {
                Some(sha) if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    format!("github:{repo}#{sha}")
                }
                _ => format!("github:{repo}"),
            }
        }
        ScriptSource::Local => format!("file:{name}"),
    }
}

/// 去空白后非空才保留。
fn nonempty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// 正数（>0）才保留（0 = 「没有」）。
fn positive(value: i64) -> Option<i64> {
    (value > 0).then_some(value)
}

/// `pushedAt` 兼容两种形态：unix 秒整数串 / RFC3339（如 GitHub
/// `2026-11-11T08:19:01Z`）。空串 / 非法 → None。
fn parse_time(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(unix) = trimmed.parse::<i64>() {
        return (unix > 0).then_some(unix);
    }
    network::iso_to_unix(trimmed).filter(|unix| *unix > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(code: &str) -> Catalog {
        parse_catalog(code.as_bytes()).unwrap_or_else(|e| panic!("应解析成功：{}", e.message))
    }

    #[test]
    fn parses_valid_catalog_and_maps_fields() {
        let cat = one(r#"{
                "schemaVersion": 1,
                "warning": "目录生成略有延迟",
                "entries": [
                    {
                        "name": "@magpie-community/kiro",
                        "label": "Kiro",
                        "version": "1.2.0",
                        "providers": ["kiro"],
                        "source": "magpie:@magpie-community/kiro",
                        "selector": "latest",
                        "publisher": "official",
                        "summary": {"zh": "Kiro 接入"},
                        "repository": "magpie-community/plugins",
                        "repositoryUrl": "https://github.com/magpie-community/plugins",
                        "stars": 12,
                        "starsAt": 1700000000,
                        "iconSources": ["https://x/kiro.svg"],
                        "kind": "provider",
                        "installable": true,
                        "unavailableReason": null,
                        "pushedAt": "2026-11-11T08:19:01Z",
                        "manifestAt": 1700000100,
                        "providerVerified": true,
                        "commitSha": "0123456789abcdef0123456789abcdef01234567"
                    }
                ]
            }"#);
        assert_eq!(cat.market.len(), 1);
        assert!(cat.github.is_empty());
        assert_eq!(cat.warning.as_deref(), Some("目录生成略有延迟"));
        let e = &cat.market[0];
        assert_eq!(e.source, ScriptSource::Magpie);
        assert_eq!(e.publisher, ScriptPublisher::Official);
        assert_eq!(e.selector, "magpie:@magpie-community/kiro");
        assert_eq!(e.version.as_deref(), Some("1.2.0"));
        assert_eq!(e.kind.as_deref(), Some("provider"));
        assert_eq!(e.stars_at, Some(1700000000));
        assert_eq!(e.manifest_at, Some(1700000100));
        assert!(e.pushed_at.is_some(), "RFC3339 pushedAt 应转 unix 秒");
        assert_eq!(e.provider_verified, Some(true));
        assert_eq!(
            e.commit_sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
    }

    #[test]
    fn rejects_bad_schema_version() {
        let err = parse_catalog(br#"{"schemaVersion": 2, "entries": []}"#).unwrap_err();
        assert!(err.message.contains("schemaVersion"), "{}", err.message);
    }

    #[test]
    fn rejects_missing_entries() {
        let err = parse_catalog(br#"{"schemaVersion": 1}"#).unwrap_err();
        assert!(err.message.contains("entries"), "{}", err.message);
    }

    #[test]
    fn groups_third_party_into_github_bucket() {
        let cat = one(r#"{"schemaVersion": 1, "entries": [
                {"name":"@magpie-community/a","providers":["a"],"source":"magpie:@magpie-community/a",
                 "publisher":"official","kind":"provider","providerVerified":true},
                {"name":"@other/b","providers":["b"],"source":"@other/b",
                 "publisher":"recommended","kind":"provider","providerVerified":false},
                {"name":"owner/repo","providers":["c"],"source":"github:owner/repo",
                 "publisher":"third-party","repository":"owner/repo","kind":"provider","providerVerified":true}
            ]}"#);
        assert_eq!(cat.market.len(), 2, "official+recommended 进 market");
        assert_eq!(cat.github.len(), 1, "third-party 进 github");
        assert_eq!(cat.github[0].source, ScriptSource::Github);
        assert_eq!(cat.github[0].selector, "github:owner/repo");
    }

    #[test]
    fn github_selector_pins_commit_sha_for_update_detection() {
        let cat = one(r#"{"schemaVersion": 1, "entries": [
                {"name":"owner/repo","providers":["c"],"source":"github:owner/repo",
                 "publisher":"third-party","repository":"owner/repo","kind":"provider","providerVerified":true,
                 "commitSha":"0123456789abcdef0123456789abcdef01234567"}
            ]}"#);
        // selector 带上提交 SHA：安装钉到该提交，source_ref 记完整 SHA，
        // check_updates 才能据此判断是否有新提交。
        assert_eq!(
            cat.github[0].selector,
            "github:owner/repo#0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(
            cat.github[0].commit_sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
    }

    #[test]
    fn keeps_installable_false_and_reason() {
        let cat = one(r#"{"schemaVersion": 1, "entries": [
                {"name":"@magpie-community/a","providers":["a"],"source":"magpie:@magpie-community/a",
                 "publisher":"official","kind":"provider","providerVerified":true,
                 "installable": false, "unavailableReason": "分发暂不可用"}
            ]}"#);
        let e = &cat.market[0];
        assert!(!e.installable);
        assert_eq!(e.unavailable_reason.as_deref(), Some("分发暂不可用"));
    }

    #[test]
    fn filters_non_provider_entries() {
        // kind=middleware 明确非 provider → 丢弃；kind 缺省但 curated+providers → 保留
        let cat = one(r#"{"schemaVersion": 1, "entries": [
                {"name":"mw","kind":"middleware","providers":["x"],"publisher":"official","source":"mw"},
                {"name":"@magpie-community/a","providers":["a"],"publisher":"official","source":"magpie:a"}
            ]}"#);
        assert_eq!(cat.market.len(), 1);
        assert_eq!(cat.market[0].name, "@magpie-community/a");
    }

    #[test]
    fn all_filtered_out_is_a_fallback_error() {
        // 有条目但全部被过滤 → 视为格式不兼容，回落旧链（不能静默清空）
        let err = parse_catalog(
            br#"{"schemaVersion": 1, "entries": [
                {"name":"agent","kind":"agent","providers":[],"publisher":"official","source":"a"}
            ]}"#,
        )
        .unwrap_err();
        assert!(err.message.contains("过滤"), "{}", err.message);
    }

    #[test]
    fn empty_entries_is_ok() {
        // 上游明确给出空 entries（无 provider）不是错误
        let cat = one(r#"{"schemaVersion": 1, "entries": []}"#);
        assert!(cat.market.is_empty() && cat.github.is_empty());
    }

    #[test]
    fn classify_source_prefixes() {
        assert_eq!(
            classify_source("magpie:x", "official", None),
            ScriptSource::Magpie
        );
        assert_eq!(
            classify_source("github:o/r", "third-party", Some("o/r")),
            ScriptSource::Github
        );
        assert_eq!(
            classify_source("https://github.com/o/r", "third-party", Some("o/r")),
            ScriptSource::Github
        );
        assert_eq!(
            classify_source("bare-pkg", "recommended", None),
            ScriptSource::Npm
        );
        assert_eq!(
            classify_source("file:/tmp/x", "official", None),
            ScriptSource::Local
        );
        // 空 source 按 publisher 兜底
        assert_eq!(classify_source("", "official", None), ScriptSource::Magpie);
        assert_eq!(
            classify_source("", "third-party", Some("o/r")),
            ScriptSource::Github
        );
        assert_eq!(classify_source("", "recommended", None), ScriptSource::Npm);
    }
}
