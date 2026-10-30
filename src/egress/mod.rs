//! 出口网络层 —— 按账号代理配置缓存 `reqwest::Client`。
//!
//! 账号型渠道的凭证来自不同网络环境（不同出口 IP / 地区），需要按账号
//! 使用不同代理。`reqwest::Client` 的构造与连接池是重资产，按代理地址缓存
//! 复用：同一代理共享一个 Client（连接池/ TLS 会话都复用），未配置代理时
//! 走默认（环境变量 `HTTP_PROXY` / `HTTPS_PROXY` 仍然生效）。
//!
//! 支持 `http://` / `https://` / `socks5://` / `socks5h://`（reqwest 的
//! `socks` feature 已在 Cargo.toml 打开）。代理串非法时返回错误而不是
//! 静默直连——否则账号流量会从错误的出口 IP 发出。

use dashmap::DashMap;
use std::sync::Arc;
use std::time::Duration;

/// 默认请求超时：与全局共享 Client 一致（覆盖大多数上游推理时长）。
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// 按代理地址缓存的出口 Client 池。
pub struct EgressCache {
    clients: DashMap<String, Arc<reqwest::Client>>,
    timeout: Duration,
}

impl Default for EgressCache {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressCache {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            clients: DashMap::new(),
            timeout,
        }
    }

    /// 取（或构造）指定代理的 Client。
    ///
    /// `proxy` 为 `None` / 空白串 = 直连（沿用环境变量代理）。
    pub fn client(&self, proxy: Option<&str>) -> Result<Arc<reqwest::Client>, String> {
        let key = normalize_proxy(proxy);
        if let Some(cached) = self.clients.get(&key) {
            return Ok(cached.clone());
        }
        let built = Arc::new(build_client(&key, self.timeout)?);
        self.clients.insert(key, built.clone());
        Ok(built)
    }

    /// 使某个代理的缓存失效（代理地址变更或排查代理故障时调用）。
    pub fn invalidate(&self, proxy: Option<&str>) {
        self.clients.remove(&normalize_proxy(proxy));
    }
}

/// 归一化代理串：去首尾空白，空串代表直连（作为缓存键）。
fn normalize_proxy(proxy: Option<&str>) -> String {
    proxy.map(str::trim).unwrap_or_default().to_string()
}

/// 构造出口 Client：强制 HTTP/1.1（与全局共享 Client 同策略，防 h2 PING
/// 帧掩蔽读超时），按代理串配置 `Proxy::all`。
fn build_client(proxy: &str, timeout: Duration) -> Result<reqwest::Client, String> {
    let mut builder = reqwest::Client::builder().http1_only().timeout(timeout);
    if !proxy.is_empty() {
        let parsed =
            reqwest::Proxy::all(proxy).map_err(|e| format!("invalid proxy '{proxy}': {e}"))?;
        builder = builder.proxy(parsed);
    }
    builder
        .build()
        .map_err(|e| format!("failed to build egress client: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_proxy_trims_and_maps_empty_to_direct() {
        assert_eq!(normalize_proxy(None), "");
        assert_eq!(normalize_proxy(Some("   ")), "");
        assert_eq!(
            normalize_proxy(Some(" http://127.0.0.1:8080 ")),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn rejects_malformed_proxy_url() {
        let cache = EgressCache::new();
        assert!(cache.client(Some("://broken")).is_err());
    }

    #[test]
    fn accepts_socks5_proxy() {
        let cache = EgressCache::new();
        assert!(cache.client(Some("socks5://127.0.0.1:1080")).is_ok());
    }

    #[test]
    fn caches_client_per_proxy() {
        let cache = EgressCache::new();
        let a = cache.client(None).expect("direct client");
        let b = cache.client(Some("")).expect("direct client again");
        assert!(Arc::ptr_eq(&a, &b));
    }
}
