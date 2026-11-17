// Ported from Agent2API v3.5.0-beta.1 (AGPL-3.0, Copyright (c) 2026 aimod-cc)
// Source: desktop-tauri/src-tauri/server/src/server/core/plugin_host/script/icons/download.rs
//!
//! 图标下载：仅 https、禁内网（SSRF 防护）、跳转有界、字节有界。
//!
//! 复用 AIGX 的 [`crate::net_guard::validate_outbound_url`] 做 SSRF 校验
//! （协议 / 主机名 / 私网地址），比 A2A 手写的 `public_ip` 判定更统一
//! （与渠道 `base_url` 同口径）。

use std::time::Duration;

/// 下载一个图标引用（https）→ 字节（≤ `limit`）。
pub async fn bytes(value: &str, limit: usize) -> Result<Vec<u8>, String> {
    tokio::time::timeout(Duration::from_secs(30), request(value, limit))
        .await
        .map_err(|_| "图标获取超时".to_string())?
}

async fn request(value: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut url = url::Url::parse(value).map_err(|_| "图标地址无效")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| "图标下载客户端不可用")?;
    for _ in 0..=4 {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err("图标仅支持不含登录凭据的 HTTPS 地址".into());
        }
        // SSRF：禁内网 / 本机 / 保留地址（与渠道 base_url 同口径）
        crate::net_guard::validate_outbound_url(url.as_str(), false)
            .map_err(|_| "图标地址不能指向本机、内网或保留地址".to_string())?;
        let mut response = client
            .get(url.clone())
            .header(
                "user-agent",
                "AIGX-PluginIcons/1.0 (+https://github.com/ojbkxc/iserver)",
            )
            .header("accept", "image/*, application/json;q=0.9, */*;q=0.5")
            .send()
            .await
            .map_err(|_| "图标下载失败")?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or("图标跳转缺少目标地址")?;
            url = url.join(location).map_err(|_| "图标跳转地址无效")?;
            continue;
        }
        if !response.status().is_success() {
            return Err(format!("图标下载返回 HTTP {}", response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|size| size > limit as u64)
        {
            return Err("图标资源超过大小限制".into());
        }
        let mut out = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "图标下载中断")? {
            if out.len().saturating_add(chunk.len()) > limit {
                return Err("图标资源超过大小限制".into());
            }
            out.extend_from_slice(&chunk);
        }
        if out.is_empty() {
            return Err("图标资源为空".into());
        }
        return Ok(out);
    }
    Err("图标地址跳转次数过多".into())
}
