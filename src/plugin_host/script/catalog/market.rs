//! market 源：registry.json（策展清单）+ dist/index.json（分发包索引）→ market 条目。
//!
//! ## 双源为什么要都拉
//!
//! registry.json 是**策展视图**：name / icon / providers / summary，但
//! **没有版本号**（version 字段是清单结构版本，不是包版本）——装的时候
//! 必须知道装哪个版本。dist/index.json 是**分发索引**：每个包的 versions
//! 映射（tgz 文件名 + sha512 integrity + 发布时刻 + 完整 manifest）。
//! 两者 join（按包名）才能得到「可安装」的条目。
//!
//! ## kind 缺省视为 provider + providers 非空（2026-11-15 实测口径，勿改）
//!
//! registry.json 121 条里 **50 条是「kind 缺省 + providers 非空」的
//! provider 形态条目**（43 official + 7 recommended，见 mod.rs 实测口径）；
//! 有 kind 的 71 条（middleware 24 / agent 25 / tools 8 / page 14）**全部
//! 没有 providers 字段**，被「providers 非空」天然过滤。如果按「只收
//! kind=="provider"」字面实现，43 个官方 provider 包会被全部过滤掉
//! （没有一条 kind 显式等于 "provider"）。正确口径：
//!
//! ```ignore
//! let keep = kind.as_deref().is_none_or(|k| k == "provider");
//! // 再要求 providers 非空
//! ```
//!
//! 即 kind 缺省（None）→ 视为 provider 保留；kind 明确给出且不是
//! "provider" → 排除（防上游未来给 agent 类条目补 providers 字段时
//! 误收进 provider 目录）。
//!
//! ## official / recommended 判定（实测口径）
//!
//! `package.starts_with("@magpie-community/") && community == true` →
//! official（providers 形态条目里实测 43 条全命中）。注意 registry 里有
//! community=true 但非官方 scope 的条目（magpie-providers-hub /
//! magpie-plugin-codar 等 7 条），它们是 recommended 而非 official——
//! community 标记 ≠ official。
//!
//! ## dist 大小纪律
//!
//! dist/index.json 实测 **3.5MB**（registry 79KB 的 45 倍）。下载层
//! 上限 16MB；解析层用 typed struct（[`DistIndex`]）让 serde 只留需要的
//! 字段——顶层 `packages.{name}.versions.{v}.{file,integrity,time,manifest}`
//! 里 manifest 只取 name/version/description/repository/main/exports。
//! 绝不做 serde_json::Value 全量展开后手工遍历（无界 map 会把 3.5MB
//! JSON 的全部字符串驻留内存）。
//!
//! ## npm 回退
//!
//! dist 只覆盖 `@magpie-community/*`（105 包全部官方 scope）；recommended
//! 条目（裸包名，如 magpie-muse-auth）按 registry.npmjs.org 的
//! dist-tags.latest 补版本（实测 4/4 抽查可达）。补不到（404/网络失败）
//! 的条目标 installable=false + unavailable_reason，**保留在列表里**——
//! 目录要如实反映市场有什么，而不是悄悄隐藏拉不到版本的包。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::network::{self, Failure, NpmDoc};
use super::types::Entry;

/// registry.json URL（main 分支；与上游「策展市场」同源）。
pub const REGISTRY_URL: &str =
    "https://raw.githubusercontent.com/magpie-community/plugins/main/registry.json";
/// dist 分发索引 URL（dist 分支）。
pub const DIST_URL: &str =
    "https://raw.githubusercontent.com/magpie-community/plugins/dist/index.json";
/// dist tgz 下载基址（file 字段相对此基址；2026-11-15 实测 200 可达，
/// integrity 与实际内容闭环 MATCH）。官方包在 npm 大多 404，这里是
/// pkg-installer 的权威下载源。
pub const DIST_TARBALL_BASE: &str =
    "https://raw.githubusercontent.com/magpie-community/plugins/dist";

/// registry.json 大小上限（实测 79KB；给 8 倍余量）。
const REGISTRY_MAX_BYTES: usize = 640 * 1024;
/// dist index 大小上限（实测 3.5MB；给 ~4.5 倍余量）。
const DIST_MAX_BYTES: usize = 16 << 20;
/// registry / dist 下载超时。
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// npm 回退的单包超时（一阶段最多 50 个待查包，由调用方并发）。
const NPM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// npm 回退的并发上限。
const NPM_CONCURRENCY: usize = 6;

/// registry.json 条目（**只声明观察到的字段**，未知字段 serde 忽略）。
#[derive(Deserialize, Debug, Clone, Default)]
pub struct RegistryPlugin {
    /// npm 包名（`@magpie-community/xxx` 或第三方名）。
    #[serde(default)]
    pub package: String,
    /// 展示名。
    #[serde(default)]
    pub name: String,
    /// 图标 URL（可空）。
    #[serde(default)]
    pub icon: Option<String>,
    /// 覆盖的 provider id（非空才收，见 [`fetch_market`]）。
    #[serde(default)]
    pub providers: Vec<String>,
    /// 官方标记（与 @magpie-community/ 前缀联合判定 official）。
    #[serde(default)]
    pub community: bool,
    /// kind 字段：**实测 71/121 条带 kind（全无 providers 字段）、50 条
    /// 缺省**；缺省视为 provider（模块注释）。
    #[serde(default)]
    pub kind: Option<String>,
    /// 多语言摘要 {en, zh}。
    #[serde(default)]
    pub summary: BTreeMap<String, String>,
    /// 仓库归属（部分条目带；没有则从 dist manifest 补）。
    #[serde(default)]
    pub repository: Option<String>,
}

/// registry.json 顶层（version 必须为 1）。
#[derive(Deserialize, Debug)]
pub struct RegistryDoc {
    pub version: u32,
    #[serde(default)]
    pub plugins: Vec<RegistryPlugin>,
}

/// dist manifest 的裁剪形状（只取网关需要的字段）。
///
/// main/exports 等入口字段不解析：目录的 providers 以 registry 为权威，
/// 入口探测只在 github.rs 的 RepoPackageJson（第三方仓验证）里做。
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DistManifest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub repository: Option<serde_json::Value>,
}

/// dist index 的一个版本条目。
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct DistVersion {
    /// tgz 相对路径（分发基 URL 下方可定位真包）。
    #[serde(default)]
    pub file: String,
    /// sha512 integrity（npm SRI 格式，pkg-installer 安装时校验）。
    #[serde(default)]
    pub integrity: Option<String>,
    /// 发布时刻 ISO8601。
    #[serde(default)]
    pub time: Option<String>,
    /// 该版本的 package.json（裁剪形状）。
    #[serde(default)]
    pub manifest: DistManifest,
}

/// dist index 顶层。
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct DistIndex {
    /// 包名 → {versions: {版本号: 条目}}。
    pub packages: BTreeMap<String, DistPackage>,
}

/// dist index 的单包形状。
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct DistPackage {
    #[serde(default)]
    pub versions: BTreeMap<String, DistVersion>,
    /// dist 自报的 latest（可空；缺省取 versions 字典序最大）。
    #[serde(default)]
    pub latest: Option<String>,
}

impl DistIndex {
    /// 单包的「当前版本」条目：latest 优先，否则字典序最大版本。
    pub fn current(&self, name: &str) -> Option<&DistVersion> {
        let p = self.packages.get(name)?;
        let v = p
            .latest
            .as_deref()
            .or_else(|| p.versions.keys().next_back().map(|s| s.as_str()))?;
        p.versions.get(v)
    }
}

/// repository 字段的三种 JSON 形态 → 归一 `owner/repo` 简写 + 完整 URL。
///
/// 见过：字符串（"git+https://github.com/o/r.git"）、
/// `{url: "..."}`、`{type, url, directory}`。公开给 github.rs 复用
/// （仓 package.json 的 repository 形态与 dist manifest 相同）。
pub fn normalize_repository(repo: Option<&serde_json::Value>) -> (Option<String>, Option<String>) {
    let Some(v) = repo else {
        return (None, None);
    };
    let url: Option<String> = match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(m) => m.get("url").and_then(|u| u.as_str()).map(String::from),
        _ => None,
    };
    let Some(url) = url else {
        return (None, None);
    };
    let clean = url.trim_start_matches("git+").trim_end_matches(".git");
    // github.com/{owner}/{repo} → 简写
    let short = clean
        .split(['/', ':'])
        .map(String::from)
        .collect::<Vec<_>>();
    let short = if short.len() >= 2 {
        short
            .windows(2)
            .rev()
            .find(|w| {
                // 找 github.com 后的两段；简化：取最后两段（owner/repo）
                w.iter().all(|s| !s.is_empty())
            })
            .map(|w| format!("{}/{}", w[0], w[1]))
    } else {
        None
    };
    (short, Some(clean.to_string()))
}

/// 拉 registry.json 并投影成 market 条目骨架（版本/manifest 由 dist/npm 补）。
///
/// 过滤规则（**kind 缺省视为 provider**，见模块注释）+ providers 非空。
/// version != 1 时返回失败（清单结构变化，硬拒绝避免解析语义漂移）。
pub async fn fetch_registry(client: &reqwest::Client) -> Result<Vec<Entry>, Failure> {
    let bytes = network::fetch_bytes(
        client,
        REGISTRY_URL,
        REGISTRY_MAX_BYTES,
        FETCH_TIMEOUT,
        false,
    )
    .await?;
    let doc: RegistryDoc = serde_json::from_slice(&bytes)
        .map_err(|e| Failure::delayed(format!("registry.json 解析失败: {e}"), 600))?;
    if doc.version != 1 {
        return Err(Failure::delayed(
            format!(
                "registry.json version={} 不是 1，清单结构已变化",
                doc.version
            ),
            3600,
        ));
    }
    let mut out = Vec::with_capacity(doc.plugins.len());
    for p in doc.plugins {
        // kind 缺省（None）→ 视为 provider；明确非 provider → 排除
        let keep = p.kind.as_deref().is_none_or(|k| k == "provider");
        if !keep || p.providers.is_empty() || p.package.is_empty() {
            continue;
        }
        let official = super::types::is_official(&p.package, p.community);
        let mut e = if official {
            Entry::official_magpie(&p.package, &p.name, p.providers.clone())
        } else {
            Entry::recommended_npm(&p.package, &p.name, p.providers.clone())
        };
        e.summary = p.summary.clone();
        // registry 的 icon 可能是完整 URL / data URI / **裸资源名**（如
        // `githubcopilot`，2026-11-20 实测 126 条里 26 条）。裸名字原样透给
        // 前端会解析成管理面自身的相对路径而 404 —— 这是商店图标不显示的
        // 根因。用 `icons::market_sources` 归一再落库（绝对 URL 原样保留，
        // 裸名字展开为上游 svg/png 两个候选）。
        e.icon_sources = p
            .icon
            .as_deref()
            .map(crate::plugin_host::script::icons::market_sources)
            .unwrap_or_default();
        e.kind = p.kind.clone();
        if let Some(r) = &p.repository {
            e.repository = Some(r.clone());
        }
        if e.label.is_empty() {
            e.label = p.package.clone();
        }
        out.push(e);
    }
    Ok(out)
}

/// 拉 dist/index.json（typed struct，见模块「dist 大小纪律」）。
pub async fn fetch_dist(client: &reqwest::Client) -> Result<DistIndex, Failure> {
    network::fetch_json(client, DIST_URL, DIST_MAX_BYTES, FETCH_TIMEOUT, false).await
}

/// 用 dist 索引补条目的版本 / manifest / repository。
///
/// dist 命中的条目回填：version（latest）、manifest_at（time）、
/// repository（manifest.repository 归一）。dist 未命中的保持原样返回
/// `None`（调用方决定走 npm 回退）。
pub fn apply_dist(entries: &mut [Entry], dist: &DistIndex) -> usize {
    let mut hits = 0;
    for e in entries.iter_mut() {
        if let Some(v) = dist.current(&e.name) {
            hits += 1;
            if !v.manifest.version.is_empty() {
                e.version = Some(v.manifest.version.clone());
            }
            if let Some(t) = &v.time {
                e.manifest_at = network::iso_to_unix(t);
            }
            let (short, url) = normalize_repository(v.manifest.repository.as_ref());
            if e.repository.is_none() {
                e.repository = short;
            }
            if e.repository_url.is_none() {
                e.repository_url = url;
            }
            if e.summary.is_empty() {
                if let Some(d) = &v.manifest.description {
                    e.summary.insert("en".into(), d.clone());
                }
            }
            if e.label.is_empty() {
                e.label = v.manifest.name.clone();
            }
            // dist tgz 下载定位 + SRI（pkg-installer 的权威下载源与校验依据；
            // 2026-11-15 实测：官方 scoped 包大多不在 npm，dist tgz 是唯一
            // 100% 可达源，且 sha512 与 tarball 实际内容闭环 MATCH）。
            if !v.file.is_empty() {
                e.archive_url = Some(format!("{DIST_TARBALL_BASE}/{}", v.file));
                e.integrity = v.integrity.clone();
            }
        }
    }
    hits
}

/// npm registry 回退：为 dist 未命中的条目补 version / manifest_at。
///
/// 并发 NPM_CONCURRENCY；先**克隆待查名单**（索引+包名）再发请求——闭包
/// 里不能借 `entries[i]`，否则与后续 `&mut entries` 冲突。单个包失败只
/// 影响它自己（标 installable=false + reason），不拖垮整个 market 阶段。
pub async fn npm_fallback(client: &reqwest::Client, entries: &mut [Entry]) {
    let targets: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.version.is_none() && e.source == super::types::ScriptSource::Npm)
        .map(|(i, e)| (i, e.name.clone()))
        .collect();
    let client2 = std::sync::Arc::new(client.clone());
    let docs = network::map_limited(NPM_CONCURRENCY, targets, move |(i, name)| {
        // Arc 捕获（闭包要求 'static，借 &client2 活不过）
        let client = client2.clone();
        async move {
            let url = network::npm_registry_url(&name);
            network::fetch_json::<NpmDoc>(&client, &url, 4 << 20, NPM_TIMEOUT, false)
                .await
                .ok()
                .map(|d| (i, d))
        }
    })
    .await;
    // 双层 flatten：闭包产出 Option<(i, doc)>，map_limited 又包一层
    // Option（Vec<Option<Option<...>>>）——一次 flatten 得到的还是 Option
    for (i, doc) in docs.into_iter().flatten().flatten() {
        let e = &mut entries[i];
        if let Some(v) = doc.latest_version() {
            e.version = Some(v.to_string());
            e.manifest_at = doc.latest_time();
            let (short, url) = normalize_repository(doc.latest_repository().as_ref());
            if e.repository.is_none() {
                e.repository = short;
            }
            if e.repository_url.is_none() {
                e.repository_url = url;
            }
            if e.summary.is_empty() {
                if let Some(d) = doc.latest_description() {
                    e.summary.insert("en".into(), d.to_string());
                }
            }
        }
    }
    // 仍然没有版本的条目：保留但标不可安装（目录如实反映）
    for e in entries.iter_mut() {
        if e.version.is_none() && e.source == super::types::ScriptSource::Npm {
            e.installable = false;
            e.unavailable_reason = Some("npm 上找不到该包（或网络失败），暂不可安装".into());
        }
    }
}

/// market 阶段全流程：registry → dist join → npm 回退。
pub async fn fetch_market(client: &reqwest::Client) -> Result<Vec<Entry>, Failure> {
    let mut entries = fetch_registry(client).await?;
    let dist = fetch_dist(client).await?;
    let hits = apply_dist(&mut entries, &dist);
    tracing::info!(
        registry = entries.len(),
        dist_hits = hits,
        "market 双源 join 完成"
    );
    npm_fallback(client, &mut entries).await;
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_bare_icon_names_become_asset_urls() {
        // 实测裸名字（如 kiro-color）必须展开成上游资源 URL，而不是原样
        // 透出（否则前端 <img src="kiro-color"> 404）。
        let v: RegistryPlugin = serde_json::from_str(
            r#"{"package":"@magpie-community/kiro","name":"Kiro","providers":["kiro"],
                "community":true,"icon":"kiro-color"}"#,
        )
        .unwrap();
        let sources = v
            .icon
            .as_deref()
            .map(crate::plugin_host::script::icons::market_sources)
            .unwrap_or_default();
        assert_eq!(sources.len(), 2);
        assert!(sources[0].contains("/assets/icons/kiro-color.svg"));
        assert!(sources[1].contains("/assets/icons/kiro-color.png"));
        // 完整 URL 原样保留
        let v: RegistryPlugin = serde_json::from_str(
            r#"{"package":"p","icon":"https://example.com/a.svg","providers":["x"]}"#,
        )
        .unwrap();
        assert_eq!(
            v.icon
                .as_deref()
                .map(crate::plugin_host::script::icons::market_sources)
                .unwrap_or_default(),
            vec!["https://example.com/a.svg".to_string()]
        );
    }

    #[test]
    fn registry_keep_rule_default_kind_is_provider() {
        // 2026-11-15 实测口径：kind 缺省 → provider；明确 middleware/agent/... → 排除
        let keep_none = None::<String>.is_none_or(|k| k == "provider");
        assert!(
            keep_none,
            "kind 缺省必须视为 provider（102 个官方包全靠它）"
        );
        let drop_middleware = Some("middleware".to_string())
            .as_deref()
            .is_none_or(|k| k == "provider");
        assert!(!drop_middleware);
    }

    #[test]
    fn real_registry_sample_reproduces_43_7() {
        // 2026-11-15 真实样本（D:\tmp\magpie_registry.json）投影断言：
        // 50 条 provider 形态 = 43 official + 7 recommended。
        // 该测试在 CI 无样本文件时自动跳过。
        let Ok(bytes) = std::fs::read("D:/tmp/magpie_registry.json") else {
            return;
        };
        let doc: RegistryDoc = serde_json::from_slice(&bytes).unwrap();
        let mut official = 0;
        let mut recommended = 0;
        for p in &doc.plugins {
            let keep = p.kind.as_deref().is_none_or(|k| k == "provider");
            if !keep || p.providers.is_empty() || p.package.is_empty() {
                continue;
            }
            if super::super::types::is_official(&p.package, p.community) {
                official += 1;
            } else {
                recommended += 1;
            }
        }
        assert_eq!(official, 43, "official provider 条目数");
        assert_eq!(recommended, 7, "recommended 条目数");
    }

    #[test]
    fn registry_json_samples_deserialize() {
        // 用实测样本的前几条形状（缺 kind / 带 kind / community=true 非官方）
        let doc: RegistryDoc = serde_json::from_str(
            r#"{
                "version": 1,
                "plugins": [
                    {"package":"@magpie-community/kiro","name":"Kiro","providers":["kiro"],"community":true,
                     "summary":{"en":"a","zh":"甲"}},
                    {"package":"@openviking/magpie-plugin","name":"OpenViking","kind":"middleware","community":true},
                    {"package":"some/agent-pkg","name":"A","kind":"agent","community":false,"providers":["x"]},
                    {"package":"no-providers-pkg","name":"B","kind":null,"community":false}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(doc.version, 1);
        assert_eq!(doc.plugins.len(), 4);
        // kind: null 反序列化成 None → 保留；providers 空 → 过滤
        let kept: Vec<&RegistryPlugin> = doc
            .plugins
            .iter()
            .filter(|p| {
                p.kind.as_deref().is_none_or(|k| k == "provider") && !p.providers.is_empty()
            })
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "只有官方(kiro)保留：kind=middleware/agent 明确非 provider 一律排除，空 providers 也排除"
        );
    }

    #[test]
    fn dist_index_join_fills_version_and_time() {
        let idx: DistIndex = serde_json::from_str(
            r#"{"packages":{"@magpie-community/kiro":{"latest":"1.2.0","versions":{"1.2.0":{
                "file":"tgz/kiro-1.2.0.tgz","integrity":"sha512-x","time":"2026-11-11T08:19:01.838Z",
                "manifest":{"name":"@magpie-community/kiro","version":"1.2.0",
                "repository":{"type":"git","url":"git+https://github.com/magpie-community/plugins.git"}}}}}}}"#,
        )
        .unwrap();
        assert!(idx.current("@magpie-community/kiro").is_some());
        assert!(idx.current("@magpie-community/missing").is_none());

        let mut entries = vec![Entry::official_magpie(
            "@magpie-community/kiro",
            "Kiro",
            vec!["kiro".into()],
        )];
        apply_dist(&mut entries, &idx);
        assert_eq!(entries[0].version.as_deref(), Some("1.2.0"));
        assert_eq!(entries[0].manifest_at, Some(1_794_385_141));
        assert_eq!(
            entries[0].repository.as_deref(),
            Some("magpie-community/plugins")
        );
        // dist tgz 下载定位 + SRI（pkg-installer 权威源）
        assert_eq!(
            entries[0].archive_url.as_deref(),
            Some("https://raw.githubusercontent.com/magpie-community/plugins/dist/tgz/kiro-1.2.0.tgz")
        );
        assert_eq!(entries[0].integrity.as_deref(), Some("sha512-x"));
        // public() 透出（前端/安装 API 消费）
        let p = entries[0].public();
        assert_eq!(
            p.archive_url.as_deref(),
            Some("https://raw.githubusercontent.com/magpie-community/plugins/dist/tgz/kiro-1.2.0.tgz")
        );
        assert_eq!(p.integrity.as_deref(), Some("sha512-x"));
    }

    #[test]
    fn repository_normalizer_handles_forms() {
        let (s, u) = normalize_repository(Some(&serde_json::json!(
            "git+https://github.com/owner/repo.git"
        )));
        assert_eq!(s.as_deref(), Some("owner/repo"));
        assert_eq!(u.as_deref(), Some("https://github.com/owner/repo"));
        let (s2, u2) =
            normalize_repository(Some(&serde_json::json!({"url":"https://github.com/o2/r2"})));
        assert_eq!(s2.as_deref(), Some("o2/r2"));
        assert_eq!(u2.as_deref(), Some("https://github.com/o2/r2"));
        assert_eq!(normalize_repository(None), (None, None));
    }

    #[test]
    fn summary_picked_zh_first() {
        use super::super::types::pick_summary;
        let mut m = BTreeMap::new();
        m.insert("en".into(), "e".into());
        m.insert("zh".into(), "中".into());
        assert_eq!(pick_summary(&m).as_deref(), Some("中"));
    }
}
