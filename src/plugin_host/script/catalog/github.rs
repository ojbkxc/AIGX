//! GitHub 发现源：topic:magpie-plugin 搜索 → 静态验证 → 第三方条目。
//!
//! ## 为什么 GitHub 发现是独立阶段（与 market 的边界）
//!
//! 第三方定义：**GitHub topic 搜到但不在 registry** 的仓。所以 github
//! 阶段的产出永远是「market 条目的补集」：与 market 同名的候选直接跳过。
//! 搜索分页 10 页 × 100 = 上限 1000 仓（topic 热度现实远低于此；到上限
//! 就够，不追分页无穷尽——见 `MAX_PAGES`）。
//!
//! ## 验证流水线（每仓 2+N 个请求，全部**不执行代码**）
//!
//! 1. 搜索结果自带 pushed_at / stars / description（不用再调 GET /repos）；
//!    **6h 内未 push 的仓直接进 ignored**——老仓大概率不是活跃 magpie
//!    插件，跳过深扫省配额；仓一旦重新 push（新搜索结果 pushed_at 变新）
//!    会自动退出 ignored 重新验证，见 cache::is_ignored；
//! 2. raw 拉 `package.json`：没有 → 非 npm 形态，ignored；
//! 3. `magpie` 键 / main / exports 定位入口候选，raw 拉入口源码，连同
//!    相对导入链（BFS ≤16 文件 / ≤6MB）写进**临时目录**，复用
//!    [`provider::scan_entry`] 的 tokenizer 找 `auth.provider` 字面量。
//!    没有证据 → ignored（这是「搜到但不是 magpie 插件」的绝大多数）。
//!
//! 有证据 → 第三方条目（publisher=ThirdParty）。selector=`github:{repo}`，
//! pkg-installer 走 zip/tarball 安装。npm 源升级判定（同名 npm 包且 repo
//! 匹配）留给后续波次——GitHub zip 直装已可安装，不阻塞目录可用性。
//!
//! ## stars（第三阶段独立刷）
//!
//! 搜索阶段抓到的 stars 随条目落缓存；此后 stars 阶段每 6h 对已收录的
//! 第三方仓逐个 GET /repos/{repo} 刷新。单仓失败保留旧值（星数缺新不删旧）。

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Deserialize;

use super::cache::IgnoredRepo;
use super::network::{self, Failure};
use super::provider;
use super::types::{Entry, ScriptPublisher, ScriptSource};

/// 搜索 API（topic 过滤 + 非 archived + 非 fork，按 stars 降序）。
pub const SEARCH_URL: &str = "https://api.github.com/search/repositories?q=topic:magpie-plugin+archived:false+fork:false&sort=stars&order=desc&per_page=100";
/// 最大分页数（10 页 ×100 = 1000 仓封顶）。
pub const MAX_PAGES: u32 = 10;
/// 每页条数（GitHub 搜索 API 单页上限就是 100）。
pub const PAGE_SIZE: u32 = 100;
/// 6h 内未 push 的仓进 ignored（活跃度闸门）。
pub const PUSH_IGNORE_WINDOW_SECS: i64 = 6 * 3600;
/// 搜索响应大小上限（100 条 repo JSON ~200KB，给 10 倍余量）。
const SEARCH_MAX_BYTES: usize = 2 << 20;
/// 单文件 raw 拉取上限（与 provider::MAX_FILE_BYTES 同口径）。
const RAW_MAX_BYTES: usize = 256 * 1024;
/// 深扫的并发上限（GitHub 匿名配额脆弱，保守并发）。
const SCAN_CONCURRENCY: usize = 4;
/// 入口探测最多尝试的候选路径数。
const MAX_ENTRY_CANDIDATES: usize = 8;
/// raw 拉取超时。
const RAW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// GitHub 搜索结果条目（裁剪形状，只留验证要用的字段）。
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct RepoItem {
    /// `owner/repo` 全名。
    #[serde(rename = "full_name")]
    pub repo: String,
    #[serde(rename = "html_url")]
    pub html_url: String,
    pub description: Option<String>,
    pub stargazers_count: u64,
    pub pushed_at: Option<String>,
}

/// GitHub 搜索响应。
#[derive(Deserialize, Debug, Default)]
pub struct SearchResponse {
    #[serde(default)]
    pub items: Vec<RepoItem>,
}

/// 仓 package.json 的裁剪形状（验证用）。
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RepoPackageJson {
    pub name: Option<String>,
    pub version: Option<String>,
    pub main: Option<String>,
    pub description: Option<String>,
    /// magpie 自报键：{provider: "./x.js"} / {agent: "./x.agent.js"} 等。
    pub magpie: Option<serde_json::Value>,
    pub repository: Option<serde_json::Value>,
    /// exports 的 "." 键可能是字符串或 {import/default/require: "..."}。
    pub exports: Option<serde_json::Value>,
}

/// GitHub 仓库 API 响应（stars 刷新阶段用）。
#[derive(Deserialize, Debug, Default)]
#[serde(default)]
pub struct RepoInfo {
    #[serde(rename = "full_name")]
    pub repo: String,
    pub stargazers_count: u64,
    pub pushed_at: Option<String>,
}

/// raw.githubusercontent.com 内容 URL。
fn raw_url(repo: &str, branch: &str, path: &str) -> String {
    format!("https://raw.githubusercontent.com/{repo}/{branch}/{path}")
}

/// 从 magpie 键 / main / exports 提取入口候选（相对包根的路径，去掉 ./）。
///
/// magpie 值可能是字符串或 `{provider: {path: "..."}}` 形态，宽容解析；
/// exports 的 "." 键可能是字符串或 `{import/default/require: "..."}`。
/// 兜底追加 index.js / index.ts（多数单文件插件入口长这样）。
fn entry_candidates(pj: &RepoPackageJson) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| {
        let t = s.trim_start_matches("./");
        if !t.is_empty() && !out.contains(&t.to_string()) {
            out.push(t.to_string());
        }
    };
    if let Some(serde_json::Value::Object(m)) = &pj.magpie {
        for (_, v) in m {
            match v {
                serde_json::Value::String(s) => push(s),
                serde_json::Value::Object(o) => {
                    for val in o.values() {
                        if let serde_json::Value::String(s) = val {
                            push(s);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(m) = &pj.main {
        push(m);
    }
    if let Some(serde_json::Value::Object(m)) = &pj.exports {
        if let Some(dot) = m.get(".") {
            match dot {
                serde_json::Value::String(s) => push(s),
                serde_json::Value::Object(o) => {
                    for key in ["import", "default", "require"] {
                        if let Some(serde_json::Value::String(s)) = o.get(key) {
                            push(s);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for guess in ["index.js", "index.ts"] {
        push(guess);
    }
    out.truncate(MAX_ENTRY_CANDIDATES);
    out
}

/// 一次验证的产出。
#[derive(Clone, Debug, Default)]
pub struct RepoVerdict {
    /// 仓全名（并发完成序≠输入序，忽略名单必须自带名字）。
    pub repo: String,
    /// 通过验证 → 第三方条目。
    pub entry: Option<Entry>,
    /// 未通过 → 进 ignored 的原因（entry 与 ignore_reason 互斥）。
    pub ignore_reason: Option<String>,
    /// 本仓当前 pushed_at（unix 秒；0 = 未知）。
    pub pushed_at: i64,
}

/// 验证一个仓：package.json → 入口扫描（临时目录 + 本地 BFS）。
async fn verify_repo(client: &reqwest::Client, item: &RepoItem) -> RepoVerdict {
    let mut verdict = RepoVerdict {
        repo: item.repo.clone(),
        ..Default::default()
    };
    let pushed = item
        .pushed_at
        .as_deref()
        .and_then(network::iso_to_unix)
        .unwrap_or(0);
    verdict.pushed_at = pushed;

    // 1) 活跃度闸门：6h 内无 push → ignored（省深扫配额）
    let now = network::now_unix();
    if pushed > 0 && now - pushed > PUSH_IGNORE_WINDOW_SECS {
        verdict.ignore_reason = Some(format!(
            "最近 push 距今超过 6h（{}天前），暂不验证",
            (now - pushed) / 86400
        ));
        return verdict;
    }

    // 2) package.json（main 404 时 master 兜底一次）
    let branch = "main";
    let pj_bytes = match fetch_raw(client, &item.repo, branch, "package.json").await {
        Ok(b) => b,
        Err(_) => match fetch_raw(client, &item.repo, "master", "package.json").await {
            Ok(b) => b,
            Err(_) => {
                verdict.ignore_reason = Some("仓库根没有 package.json（非 npm 形态）".into());
                return verdict;
            }
        },
    };
    let Ok(pj) = serde_json::from_slice::<RepoPackageJson>(&pj_bytes) else {
        verdict.ignore_reason = Some("package.json 解析失败".into());
        return verdict;
    };

    // 3) 入口源码
    let mut entry_src: Option<String> = None;
    let mut entry_rel: Option<String> = None;
    for cand in entry_candidates(&pj) {
        if let Ok(bytes) = fetch_raw(client, &item.repo, branch, &cand).await {
            if let Ok(s) = String::from_utf8(bytes) {
                entry_src = Some(s);
                entry_rel = Some(cand);
                break;
            }
        }
    }
    let (Some(src), Some(rel)) = (entry_src, entry_rel) else {
        verdict.ignore_reason = Some("找不到可读的入口文件".into());
        return verdict;
    };

    // 4) 写临时目录复用 provider::scan_entry 的本地 BFS（相对导入链）
    let tmp_root = std::env::temp_dir().join(format!(
        "aigx-magpie-scan-{}",
        item.repo.replace(['/', '\\'], "-")
    ));
    let scanned = if write_scan_tree(&tmp_root, &rel, &src, client, &item.repo, branch).await {
        let oc = provider::scan_entry(&tmp_root.join("pkg"), Path::new(&rel));
        let _ = std::fs::remove_dir_all(&tmp_root);
        oc
    } else {
        let _ = std::fs::remove_dir_all(&tmp_root);
        None
    };
    let Some(oc) = scanned else {
        verdict.ignore_reason = Some("入口扫描失败".into());
        return verdict;
    };
    let Some(pid) = oc.provider_id else {
        verdict.ignore_reason = Some("入口源码没有 auth.provider 证据".into());
        return verdict;
    };

    // 通过 → 第三方条目
    let (repo_short, repo_url) = super::market::normalize_repository(pj.repository.as_ref());
    let desc = pj.description.clone().or_else(|| item.description.clone());
    let mut summary = BTreeMap::new();
    if let Some(d) = &desc {
        summary.insert("en".into(), d.clone());
    }
    verdict.entry = Some(Entry {
        name: pj.name.clone().unwrap_or_else(|| item.repo.clone()),
        label: desc.clone().unwrap_or_else(|| pid.clone()),
        version: pj.version.clone(),
        providers: vec![pid],
        source: ScriptSource::Github,
        selector: format!("github:{}", item.repo),
        publisher: ScriptPublisher::ThirdParty,
        summary,
        repository: repo_short.or_else(|| Some(item.repo.clone())),
        repository_url: repo_url.or_else(|| Some(item.html_url.clone())),
        stars: Some(item.stargazers_count),
        stars_at: Some(network::now_unix()),
        icon_sources: vec![],
        kind: None,
        installable: true,
        unavailable_reason: None,
        // GitHub 直装没有 dist 分发，archive_url/integrity 留空
        //（pkg-installer 对 None 回退走 GitHub tarball 下载链路）
        archive_url: None,
        integrity: None,
        pushed_at: (pushed > 0).then_some(pushed),
        manifest_at: None,
        provider_verified: Some(true),
    });
    verdict
}

/// raw 拉取一个小文件（文本）。
async fn fetch_raw(
    client: &reqwest::Client,
    repo: &str,
    branch: &str,
    path: &str,
) -> Result<Vec<u8>, Failure> {
    network::fetch_bytes(
        client,
        &raw_url(repo, branch, path),
        RAW_MAX_BYTES,
        RAW_TIMEOUT,
        false,
    )
    .await
}

/// 相对导入说明符 → 仓库内的候选相对路径（原样 / .ext / index.ext）。
///
/// **必须做词法归一**：`Path::join("./c.js")` 只做拼接，产出
/// `a/./c.js`（`..` 同理变成 `a/../c.js`）——这种路径拿去拼
/// raw.githubusercontent.com 的 URL 会被 CDN 归一成另一条路径，而
/// 候选集的字符串比较也会失真。这里按 `/` 切段折叠 `.` 与 `..`，
/// 上溯越过包根即视为越界（相对链必须封闭在包内）。
///
/// 后缀策略（Node ESM 解析顺序）：
/// - 说明符**已带已知后缀**（`./c.js`）→ 只试替换成其后缀家族里的其它
///   扩展名（`a/c.ts` 等，覆盖「仓里存的是 TS 版」的情况）加上原路径；
/// - 说明符**无后缀**（`./util`）→ 追加各扩展名与 `index.*`。
///
/// 两者都保留原样（服务端可能直接有该文件）。
fn rel_candidates(pkg_root: &Path, base: &Path, spec: &str) -> Vec<String> {
    let Some(base_rel) = base.parent().and_then(|d| d.strip_prefix(pkg_root).ok()) else {
        return vec![];
    };
    let base_parts: Vec<String> = base_rel
        .to_string_lossy()
        .replace('\\', "/")
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let mut parts = base_parts;
    for seg in spec.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return vec![]; // 越界：../ 超出包根
                }
            }
            other => parts.push(other.to_string()),
        }
    }
    let rel_str = parts.join("/");
    if rel_str.is_empty() {
        return vec![];
    }
    const EXTS: [&str; 6] = ["js", "ts", "jsx", "tsx", "mjs", "cjs"];
    let mut cands = vec![rel_str.clone()];
    let has_known_ext = EXTS.iter().any(|e| rel_str.ends_with(&format!(".{e}")));
    if has_known_ext {
        // 换扩展：`a/c.js` → `a/c.ts` …（保留目录与文件名主干）
        let stem = rel_str.trim_end_matches(|c| c != '.');
        for e in EXTS {
            let c = format!("{stem}{e}");
            if !cands.contains(&c) {
                cands.push(c);
            }
        }
    } else {
        for e in EXTS {
            cands.push(format!("{rel_str}.{e}"));
        }
        for e in ["js", "ts", "mjs", "cjs"] {
            cands.push(format!("{rel_str}/index.{e}"));
        }
    }
    cands
}

/// 把入口源码 + 相对导入链（BFS ≤16 文件 / ≤6MB，与 provider.rs 同边界）
/// 逐个 raw 拉取写进临时目录，让 [`provider::scan_entry`] 在本地做 BFS。
async fn write_scan_tree(
    tmp_root: &Path,
    entry_rel: &str,
    entry_src: &str,
    client: &reqwest::Client,
    repo: &str,
    branch: &str,
) -> bool {
    let pkg_root = tmp_root.join("pkg");
    let entry_abs = pkg_root.join(entry_rel);
    if entry_abs
        .parent()
        .map(|p| std::fs::create_dir_all(p).is_err())
        .unwrap_or(true)
    {
        return false;
    }
    if std::fs::write(&entry_abs, entry_src).is_err() {
        return false;
    }
    let mut visited: HashSet<String> = HashSet::from([entry_rel.to_string()]);
    let mut queue: std::collections::VecDeque<(String, String)> =
        std::collections::VecDeque::from([(entry_rel.to_string(), entry_src.to_string())]);
    let mut total = entry_src.len();
    while let Some((rel, src)) = queue.pop_front() {
        if visited.len() >= provider::MAX_SCAN_FILES || total >= provider::MAX_TOTAL_BYTES {
            break;
        }
        let base_abs = pkg_root.join(&rel);
        for spec in provider::relative_imports(&src) {
            let mut done = false;
            for cand in rel_candidates(&pkg_root, &base_abs, &spec) {
                if visited.contains(&cand) {
                    continue;
                }
                let Ok(bytes) = fetch_raw(client, repo, branch, &cand).await else {
                    continue;
                };
                visited.insert(cand.clone());
                total += bytes.len();
                if total > provider::MAX_TOTAL_BYTES {
                    done = true;
                    break;
                }
                let Ok(child_src) = String::from_utf8(bytes) else {
                    continue;
                };
                let abs = pkg_root.join(&cand);
                if let Some(parent) = abs.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if std::fs::write(&abs, &child_src).is_ok() {
                    queue.push_back((cand, child_src));
                }
                done = true;
                break;
            }
            if done {
                continue;
            }
        }
        if total >= provider::MAX_TOTAL_BYTES {
            break;
        }
    }
    true
}

/// 搜索全部页（串行翻页：GitHub 搜索 API 对并发翻页也限流）。
pub async fn search_repos(client: &reqwest::Client) -> Result<Vec<RepoItem>, Failure> {
    let mut all = Vec::new();
    for page in 1..=MAX_PAGES {
        let url = format!("{SEARCH_URL}&page={page}");
        let resp: SearchResponse = network::fetch_json(
            client,
            &url,
            SEARCH_MAX_BYTES,
            std::time::Duration::from_secs(20),
            true,
        )
        .await?;
        let got = resp.items.len();
        all.extend(resp.items);
        // 不满页 → 后面没有了
        if (got as u32) < PAGE_SIZE {
            break;
        }
    }
    Ok(all)
}

/// github 阶段全流程：搜索 → 逐仓验证 → 产出（条目, 忽略仓）。
///
/// - 与 market 同名（小写包名集合 `market_names`）的候选直接跳过；
/// - 已在 `ignored` 且 push 时间没变新的候选跳过深扫（活跃度闸门的缓存面）。
pub async fn fetch_github(
    client: &reqwest::Client,
    market_names: &HashSet<String>,
    ignored: &[IgnoredRepo],
) -> Result<(Vec<Entry>, Vec<IgnoredRepo>), Failure> {
    let items = search_repos(client).await?;
    let candidates: Vec<RepoItem> = items
        .iter()
        .filter(|i| {
            // 自家组织仓不进第三方（官方/推荐已由 market 覆盖）
            // 先绑定 lower 再取 owner：to_lowercase() 临时值若与 split()
            // 链在同一 let 里，语句结束即 drop，owner 悬空（E0716）
            let lower = i.repo.to_lowercase();
            let owner = lower.split('/').next().unwrap_or("");
            if owner == "magpie-community" {
                return false;
            }
            if market_names.contains(&lower) {
                return false;
            }
            // 已忽略且没有新 push → 本轮跳过
            let pushed = i
                .pushed_at
                .as_deref()
                .and_then(network::iso_to_unix)
                .unwrap_or(0);
            let stale_ignored = ignored
                .iter()
                .any(|r| r.repo == i.repo && pushed <= r.pushed_at);
            !stale_ignored
        })
        .cloned()
        .collect();

    let client2 = std::sync::Arc::new(client.clone());
    let verdicts = network::map_limited(SCAN_CONCURRENCY, candidates, move |it| {
        // Arc 捕获：map_limited 的闭包要求 'static，借 &client2 活不过
        //（E0373）；每任务 clone 一个 reqwest::Client（内部连接池 Arc，
        // clone 廉价）。
        let client = client2.clone();
        async move { verify_repo(&client, &it).await }
    })
    .await;

    let mut entries = Vec::new();
    let mut new_ignored = Vec::new();
    for v in verdicts.into_iter().flatten() {
        if let Some(e) = v.entry {
            entries.push(e);
        } else if let Some(reason) = v.ignore_reason {
            // RepoVerdict 自带 repo 名（并发完成序≠输入序，名字必须随
            // verdict 走，不能靠回填索引对齐）
            new_ignored.push(IgnoredRepo {
                repo: v.repo,
                reason,
                at: network::now_unix(),
                pushed_at: v.pushed_at,
            });
        }
    }
    Ok((entries, new_ignored))
}

/// 对已收录的第三方条目刷新 stars（第三阶段；6h TTL）。
///
/// 逐仓 GET /repos/{repo}；单仓失败保留旧值（stars 缺新不删旧）。
pub async fn refresh_stars(client: &reqwest::Client, entries: &mut [Entry]) -> usize {
    let targets: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.publisher == ScriptPublisher::ThirdParty && e.repository.is_some())
        .map(|(i, e)| (i, e.repository.clone().unwrap_or_default()))
        .collect();
    let client2 = std::sync::Arc::new(client.clone());
    let infos = network::map_limited(SCAN_CONCURRENCY, targets, move |(i, repo)| {
        // Arc 捕获（同 fetch_github：闭包要求 'static）
        let client = client2.clone();
        async move {
            let url = format!("https://api.github.com/repos/{repo}");
            let info = network::fetch_json::<RepoInfo>(
                &client,
                &url,
                256 * 1024,
                std::time::Duration::from_secs(15),
                true,
            )
            .await
            .ok();
            // 显式 Option<(usize, RepoInfo)>：E0308 的来源是隐式推断
            // 在 async move 里绑歪；类型钉死后 (i, info) 按序回填
            info.map(|info| (i, info))
        }
    })
    .await;
    let mut updated = 0;
    // 双层 flatten：闭包产出 Option<(i, info)>，map_limited 又包一层
    // Option（Vec<Option<Option<...>>>）——一次 flatten 得到的还是 Option
    for (i, info) in infos.into_iter().flatten().flatten() {
        entries[i].stars = Some(info.stargazers_count);
        entries[i].stars_at = Some(network::now_unix());
        updated += 1;
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_candidates_prefer_magpie_key_and_dedupe() {
        let pj: RepoPackageJson = serde_json::from_str(
            r#"{"name":"x","main":"./lib/main.js","magpie":{"provider":"./x.provider.js"},
                "exports":{".":"./dist/idx.js"}}"#,
        )
        .unwrap();
        let c = entry_candidates(&pj);
        assert_eq!(c[0], "x.provider.js");
        assert!(c.contains(&"lib/main.js".to_string()));
        assert!(c.contains(&"dist/idx.js".to_string()));
        assert!(c.contains(&"index.js".to_string()));
        assert!(c.len() <= MAX_ENTRY_CANDIDATES);
        // 去重：main 与 exports 同路径只留一份
        let pj2: RepoPackageJson =
            serde_json::from_str(r#"{"main":"./index.js","exports":{".":"./index.js"}}"#).unwrap();
        let c2 = entry_candidates(&pj2);
        assert_eq!(c2.iter().filter(|s| *s == "index.js").count(), 1);
    }

    #[test]
    fn resolve_rel_bounds_to_pkg_root() {
        let root = Path::new("/tmp/pkg");
        let base = Path::new("/tmp/pkg/a/b.js");
        let c1 = rel_candidates(root, base, "./c.js");
        assert!(c1.contains(&"a/c.js".to_string()));
        assert!(c1.contains(&"a/c.ts".to_string()));
        let c2 = rel_candidates(root, base, "../c.js");
        assert!(c2.contains(&"c.js".to_string()));
        // 越界
        assert!(rel_candidates(root, base, "../../etc/passwd").is_empty());
    }

    #[test]
    fn raw_url_shape() {
        assert_eq!(
            raw_url("owner/repo", "main", "package.json"),
            "https://raw.githubusercontent.com/owner/repo/main/package.json"
        );
    }

    #[test]
    fn search_url_has_topic_and_sort() {
        assert!(SEARCH_URL.contains("topic:magpie-plugin"));
        assert!(SEARCH_URL.contains("archived:false"));
        assert!(SEARCH_URL.contains("fork:false"));
        assert!(SEARCH_URL.contains("sort=stars"));
    }

    #[test]
    fn search_response_deserializes() {
        let r: SearchResponse = serde_json::from_str(
            r#"{"total_count":1,"items":[{"full_name":"a/b","html_url":"https://github.com/a/b",
                "description":"d","stargazers_count":7,"pushed_at":"2026-11-14T00:00:00Z"}]}"#,
        )
        .unwrap();
        assert_eq!(r.items[0].repo, "a/b");
        assert_eq!(r.items[0].stargazers_count, 7);
    }
}
