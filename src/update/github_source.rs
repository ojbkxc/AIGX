//! GitHub 加速源候选链（对齐 Agent2API `core/update/source.rs` 的候选语义）。
//!
//! ## 为什么需要
//!
//! 国内网络访问 `github.com` / `raw.githubusercontent.com` 经常超时或限流。
//! 加速站是「前缀式」的：`https://加速站/` + 原始 GitHub URL，因此同一个
//! 资源有多条可试的 URL —— 按 `github_mode` 决定先后顺序，失败再试下一个。
//!
//! ## 只接管「资源 URL」
//!
//! 加速站通常只代理 releases / raw，不代理 `api.github.com`；`supports_api`
//! 为假的镜像不参与 API 请求。本模块的 [`resource_candidates`] 只接受资源
//! URL（`github.com/<owner>/<repo>/releases/download/...` 与
//! `raw.githubusercontent.com`），API 请求（release 元数据）仍走
//! `UpdateConfig::api_base` 或官方 GitHub API。
//!
//! ## 与上游的差异
//!
//! 上游有「自动测速排序 + 逐候选限流冷却」两层状态（`source_state` /
//! `rate_limit`）；AIGX 一期只做**有序回退**（官方/加速按 mode 排好，逐个试
//! 到成功为止），不记冷却 —— 加速站失败一次不代表资源不可用，回退到下一个
//! 就会命中；记住失败状态反而可能把「偶发超时」固化成「永久跳过」。

use crate::config::{GithubAccessMode, UpdateConfig, DEFAULT_GITHUB_MIRROR};

/// 一个可尝试的下载地址。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResourceCandidate {
    pub url: String,
    /// 显示名（错误文案里区分「官方源」与「加速源 xxx」）。
    pub label: String,
}

/// 是否为 GitHub 资源 URL（releases 资产 / raw 文件）。
///
/// 只认 https + 默认端口 + 无凭证：加速源拼接的是完整原始 URL，带查询参数
/// 的资源（如 `?download=1`）也允许，但前缀里不能有凭证。
pub(crate) fn is_github_resource(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return false;
    }
    match url.host_str().unwrap_or("") {
        "raw.githubusercontent.com" | "codeload.github.com" => true,
        "github.com" | "www.github.com" => url.path().contains("/releases/download/"),
        _ => false,
    }
}

/// 按 `github_mode` 生成候选链：官方 URL 永远在列表里兜底。
///
/// - `official`：只有官方
/// - `accelerated`：`mirror_url` 首选 → 其余启用的镜像 → 官方
/// - `auto`：官方 → 启用的镜像（顺序即配置顺序）
///
/// 非资源 URL / `api_base` 指向自建源（此时镜像拼接无意义）时原样单条返回。
pub(crate) fn resource_candidates(config: &UpdateConfig, url: &str) -> Vec<ResourceCandidate> {
    let official = ResourceCandidate {
        url: url.to_string(),
        label: "官方源".to_string(),
    };
    // 自建更新源（`api_base` 非空）时下载地址可能完全不是 GitHub 域；
    // 加速前缀只对原始 GitHub URL 有意义。
    if !is_github_resource(url) {
        return vec![official];
    }
    let mirrors: Vec<&crate::config::GithubMirror> = config
        .github_mirrors
        .iter()
        .filter(|mirror| mirror.enabled && !mirror.url.trim().is_empty())
        .collect();
    let mirrored = |mirror: &crate::config::GithubMirror| ResourceCandidate {
        url: format!("{}{url}", mirror.url.trim()),
        label: format!("加速源 {}", mirror.url.trim()),
    };
    let mut candidates = Vec::new();
    match config.github_mode {
        GithubAccessMode::Official => {}
        GithubAccessMode::Auto => {
            candidates.push(official.clone());
            for mirror in mirrors.iter() {
                candidates.push(mirrored(mirror));
            }
        }
        GithubAccessMode::Accelerated => {
            // 首选镜像排最前（配置里同一个 URL 只出现一次 —— 归一化时去重）
            let preferred = config.github_mirror_url.trim();
            let preferred_entry = mirrors.iter().find(|mirror| mirror.url.trim() == preferred);
            for mirror in preferred_entry.into_iter() {
                candidates.push(mirrored(mirror));
            }
            for mirror in mirrors.iter() {
                if mirror.url.trim() == preferred {
                    continue;
                }
                candidates.push(mirrored(mirror));
            }
            candidates.push(official.clone());
        }
    }
    if config.github_mode == GithubAccessMode::Official || candidates.is_empty() {
        return vec![official];
    }
    candidates
}

/// 归一化一个加速前缀：补尾斜杠、校验 https / 无凭证 / 非官方域。
pub(crate) fn normalize_mirror_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(DEFAULT_GITHUB_MIRROR.to_string());
    }
    if value.len() > 1024 || value.contains('\\') || value.chars().any(char::is_whitespace) {
        return Err("GitHub 加速前缀不是合法地址".to_string());
    }
    let mut url = url::Url::parse(value).map_err(|_| "GitHub 加速前缀不是合法地址".to_string())?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err("GitHub 加速前缀必须是 HTTPS 地址".to_string());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path().contains("://")
        || url.path().to_ascii_lowercase().contains("%3a")
    {
        return Err("GitHub 加速前缀不能包含凭证、查询参数或原始 GitHub 地址".to_string());
    }
    if matches!(
        url.host_str(),
        Some("github.com" | "api.github.com" | "raw.githubusercontent.com")
    ) {
        return Err("GitHub 加速前缀应填写加速站地址，而不是 GitHub 官方地址".to_string());
    }
    let path = format!("{}/", url.path().trim_end_matches('/'));
    url.set_path(&path);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GithubMirror;

    fn config(mode: GithubAccessMode, mirrors: Vec<(&str, bool)>) -> UpdateConfig {
        UpdateConfig {
            github_mode: mode,
            github_mirrors: mirrors
                .into_iter()
                .map(|(url, supports_api)| GithubMirror {
                    url: url.to_string(),
                    enabled: true,
                    supports_api,
                })
                .collect(),
            ..UpdateConfig::default()
        }
    }

    const ASSET: &str =
        "https://github.com/ojbkxc/AIGX/releases/download/v1.0.65/AIGX-1.0.65-server-linux-x86_64.upd";

    /// 官方模式：只有一条，前缀一个都不拼。
    #[test]
    fn official_mode_returns_single_original_url() {
        let config = config(
            GithubAccessMode::Official,
            vec![("https://gh-proxy.com/", true)],
        );
        let candidates = resource_candidates(&config, ASSET);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].url, ASSET);
        assert_eq!(candidates[0].label, "官方源");
    }

    /// auto：官方在前，镜像按配置顺序在后。
    #[test]
    fn auto_mode_tries_official_first_then_mirrors() {
        let config = config(
            GithubAccessMode::Auto,
            vec![("https://a.example/", true), ("https://b.example/", true)],
        );
        let candidates = resource_candidates(&config, ASSET);
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].url, ASSET);
        assert_eq!(candidates[1].url, format!("https://a.example/{ASSET}"));
        assert_eq!(candidates[2].url, format!("https://b.example/{ASSET}"));
    }

    /// accelerated：首选镜像排最前，官方兜底在最后。
    #[test]
    fn accelerated_mode_prefers_configured_mirror() {
        let mut config = config(
            GithubAccessMode::Accelerated,
            vec![("https://a.example/", true), ("https://b.example/", true)],
        );
        config.github_mirror_url = "https://b.example/".to_string();
        let candidates = resource_candidates(&config, ASSET);
        assert_eq!(candidates[0].url, format!("https://b.example/{ASSET}"));
        assert_eq!(candidates[1].url, format!("https://a.example/{ASSET}"));
        assert_eq!(candidates.last().unwrap().url, ASSET);
    }

    /// 非 GitHub 资源（自建源）不拼前缀。
    #[test]
    fn non_github_url_is_never_prefixed() {
        let config = config(
            GithubAccessMode::Accelerated,
            vec![("https://a.example/", true)],
        );
        let url = "https://my-mirror.internal/releases/download/v1/x.upd";
        let candidates = resource_candidates(&config, url);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].url, url);
    }

    /// raw 文件也属于资源；API 地址不属于。
    #[test]
    fn raw_is_resource_but_api_is_not() {
        assert!(is_github_resource(
            "https://raw.githubusercontent.com/owner/repo/main/a.sha256"
        ));
        assert!(!is_github_resource(
            "https://api.github.com/repos/o/r/releases"
        ));
        assert!(!is_github_resource("https://github.com/owner/repo"));
    }

    /// 前缀归一化：补尾斜杠；拒绝官方域 / http / 带查询参数。
    #[test]
    fn mirror_url_normalization() {
        assert_eq!(
            normalize_mirror_url("https://gh-proxy.com").unwrap(),
            "https://gh-proxy.com/"
        );
        assert_eq!(
            normalize_mirror_url("https://gh-proxy.com/mirror").unwrap(),
            "https://gh-proxy.com/mirror/"
        );
        assert!(normalize_mirror_url("http://gh-proxy.com/").is_err());
        assert!(normalize_mirror_url("https://github.com/").is_err());
        assert!(normalize_mirror_url("https://gh-proxy.com/?x=1").is_err());
        assert_eq!(
            normalize_mirror_url("").unwrap(),
            DEFAULT_GITHUB_MIRROR.to_string()
        );
    }
}
