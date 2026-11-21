//! 分发索引（tgz + sha512 完整性）—— CDN 优先、legacy + 旧链回落。
//!
//! ## 为什么聚合目录之外还要一份分发索引
//!
//! A2A beta.4 的 CDN 聚合目录（`website::CATALOG`）只告诉你「有哪些
//! provider 插件、版本、commit」，**不含包体下载地址**。真正的 tgz 与
//! sha512 在分发索引里（beta.4 `distribution.rs`）。AIGX 的安装链路
//! （`install::entry_to_source`）对 Magpie 条目要求 `archive_url` +
//! `integrity`，所以刷新 CDN 目录后必须补一步分发索引，把这两项填进
//! [`Entry`]，否则官方包会「看得到装不了」。
//!
//! ## 与旧链的关系
//!
//! 分发索引与旧 `market::DIST_URL`（`raw.githubusercontent.com/.../dist/
//! index.json`）是**同一份 JSON 形状**（`{packages:{name:{lat/versions}}}`），
//! 只是官网 CDN 镜像了一份。取数顺序：
//!
//! 1. CDN（`website::DISTRIBUTION`）；
//! 2. legacy 官网（`website::LEGACY_DISTRIBUTION`）；
//! 3. 旧链 raw dist（`market::fetch_dist`）——保证 CDN 全挂时官方包仍可安装。
//!
//! 下载地址仍按 `market::DIST_TARBALL_BASE` 拼（raw dist 分支），与
//! `entry_to_source` 的属地校验一致；分发索引只提供相对 `file` 与 integrity。

use super::market::{self, DistIndex};
use super::network::Failure;
use super::types::{Entry, ScriptSource};
use super::website;

/// 分发索引大小上限（与旧链 dist 一致）。
const DIST_LIMIT: usize = 16 << 20;

/// 拉分发索引：CDN → legacy → 旧链 raw dist。
pub async fn fetch(client: &reqwest::Client) -> Result<DistIndex, Failure> {
    match website::bytes_with_fallback(
        client,
        website::DISTRIBUTION,
        website::LEGACY_DISTRIBUTION,
        DIST_LIMIT,
    )
    .await
    {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| Failure::delayed(format!("分发索引解析失败: {e}"), 600)),
        Err(error) => {
            tracing::warn!("官网分发索引不可用，回落旧链 raw dist：{}", error.message);
            market::fetch_dist(client).await
        }
    }
}

/// 用分发索引给 Magpie 条目补 `version` / `manifest_at` / `archive_url` /
/// `integrity`，并把补不到的 Magpie 条目标成「暂不可安装」（否则安装链路
/// 到 `entry_to_source` 才报错，前端会先显示成可装）。
///
/// 返回 dist 命中的条目数（日志用）。
pub fn attach_archives(entries: &mut [Entry], dist: &DistIndex) -> usize {
    let hits = market::apply_dist(entries, dist);
    mark_missing_archives(entries);
    hits
}

/// 把仍缺下载地址的 Magpie 条目标成不可安装。
///
/// 分发索引整段不可用时也会单独调用本函数——此时所有 Magpie 条目都缺
/// `archive_url`，如实标记比「显示可装、点了失败」更诚实。
pub fn mark_missing_archives(entries: &mut [Entry]) {
    for entry in entries.iter_mut() {
        let missing = entry
            .archive_url
            .as_deref()
            .map(str::is_empty)
            .unwrap_or(true);
        if entry.source == ScriptSource::Magpie && missing {
            entry.installable = false;
            if entry.unavailable_reason.is_none() {
                entry.unavailable_reason = Some("分发索引里找不到下载地址，暂不可安装".into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_host::script::catalog::market::DistIndex;

    fn magpie(name: &str) -> Entry {
        Entry::official_magpie(name, name, vec!["p".into()])
    }

    #[test]
    fn attach_fills_archive_and_marks_unavailable_when_missing() {
        let idx: DistIndex = serde_json::from_str(
            r#"{"packages":{"@magpie-community/hit":{"latest":"1.0.0","versions":{"1.0.0":{
                "file":"tgz/hit-1.0.0.tgz","integrity":"sha512-x"}}}}}"#,
        )
        .unwrap();
        let mut entries = vec![
            magpie("@magpie-community/hit"),
            magpie("@magpie-community/miss"),
        ];
        let hits = attach_archives(&mut entries, &idx);
        assert_eq!(hits, 1);
        // 命中：拿到 archive_url + integrity，仍可安装
        assert!(entries[0].installable);
        assert_eq!(
            entries[0].archive_url.as_deref(),
            Some(
                "https://raw.githubusercontent.com/magpie-community/plugins/dist/tgz/hit-1.0.0.tgz"
            )
        );
        assert_eq!(entries[0].integrity.as_deref(), Some("sha512-x"));
        // 未命中：标不可安装 + 原因
        assert!(!entries[1].installable);
        assert!(entries[1].unavailable_reason.is_some());
    }

    #[test]
    fn mark_missing_does_not_touch_npm_or_github() {
        let mut entries = vec![
            Entry {
                name: "npm-pkg".into(),
                source: ScriptSource::Npm,
                installable: true,
                ..Default::default()
            },
            Entry {
                name: "o/r".into(),
                source: ScriptSource::Github,
                installable: true,
                ..Default::default()
            },
        ];
        mark_missing_archives(&mut entries);
        assert!(entries[0].installable && entries[1].installable);
    }
}
