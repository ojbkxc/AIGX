//! 官网 CDN 目录端点 + 「CDN 优先、legacy 回落」的字节读取。
//!
//! ## 为什么要有这一层
//!
//! AIGX 早期（对齐 A2A beta.1）直接打 `raw.githubusercontent.com` 的
//! registry / dist 与 GitHub API（见 `market.rs` / `github.rs`）。A2A
//! **beta.4 起改为拉官网 CDN 的聚合目录**（`website::CATALOG`），旧
//! `script/catalog/{github,market,network}.rs` 在上游被删除。
//!
//! 本模块只做「地址 + 读取 + 回落」，解析在 [`super::cdn`]，
//! 分发索引在 [`super::distribution`]。对齐 beta.4：
//!
//! - `catalog`：聚合后的社区插件清单（schemaVersion==1 的 camelCase 条目）；
//! - `distribution`：分发索引（tgz 文件 + sha512 integrity），与旧
//!   `market::DIST_URL` 同一份 JSON 形状。
//!
//! 每个端点都有 **CDN 域名**（`cdn.agent2api.dev/...`）与 **legacy 官网**
//! （`agent2api.dev/api/plugins/community/...`）两份：先打 CDN，失败再打
//! legacy（[`bytes_with_fallback`]，对应 beta.4 `website.rs:93`）。两者都
//! 不通时把失败原样上抛，由刷新层回落到 AIGX 自带的旧链。

use std::time::Duration;

use super::network::{self, Failure};

/// 官网 CDN 聚合目录（beta.4 `CDN_COMMUNITY_CATALOG_URL`）。
pub const CATALOG: &str = "https://cdn.agent2api.dev/agent2api/feeds/community/catalog.json";
/// 聚合目录的 legacy 端点（beta.4 `LEGACY_CATALOG`）。
pub const LEGACY_CATALOG: &str = "https://agent2api.dev/api/plugins/community/catalog";

/// 官网 CDN 分发索引（beta.4 `CDN_COMMUNITY_DISTRIBUTION_URL`）。
pub const DISTRIBUTION: &str =
    "https://cdn.agent2api.dev/agent2api/feeds/community/distribution.json";
/// 分发索引的 legacy 端点（beta.4 `LEGACY_DISTRIBUTION`）。
pub const LEGACY_DISTRIBUTION: &str = "https://agent2api.dev/api/plugins/community/distribution";

/// 单次官网请求超时（与旧链 registry/dist 的 30s 一致）。
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// GET 一个官网端点并读满字节（有界）。走 `network::fetch_bytes`，因此
/// 享有限流头处理、大小上限、超时语义；`github_api=false`（官网端点不需要
/// GitHub 头）。
pub async fn bytes(client: &reqwest::Client, url: &str, limit: usize) -> Result<Vec<u8>, Failure> {
    network::fetch_bytes(client, url, limit, FETCH_TIMEOUT, false).await
}

/// 先打 `primary`（CDN），失败再打 `fallback`（legacy）。
///
/// 两者都失败时返回合成错误（保留两段原因，便于管理面/日志定位是
/// 「CDN 挂了」还是「官网整体不可用」）。合成错误的退避取较短值——
/// 刷新层随后还会回落旧链，不该因为官网两次失败就把整个阶段挂很久。
pub async fn bytes_with_fallback(
    client: &reqwest::Client,
    primary: &str,
    fallback: &str,
    limit: usize,
) -> Result<Vec<u8>, Failure> {
    match bytes(client, primary, limit).await {
        Ok(bytes) => Ok(bytes),
        Err(primary_err) => bytes(client, fallback, limit)
            .await
            .map_err(|fallback_err| {
                Failure::delayed(
                    format!(
                        "{}（{}）；回落 {} 也失败：{}",
                        primary_err.message, primary, fallback, fallback_err.message
                    ),
                    300,
                )
            }),
    }
}
